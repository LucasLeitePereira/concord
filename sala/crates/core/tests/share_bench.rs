//! T-035c: where does video get lost between two sessions on loopback? Diagnostic harness, not a
//! regression test: `#[ignore]`, prints numbers. The source is synthetic but sized like the real
//! 1080p30 stream (delta ~28 KB, keyframe ~250 KB at 8 Mbps) and paced at 30 fps.
//!
//! ```text
//! BENCH_SECS=20 cargo test -p sala-core --release --test share_bench -- --ignored --nocapture
//! ```
//!
//! Knobs (env): `BENCH_SECS` (default 20), `BENCH_KBPS` (default 8000: size of the frames; the
//! ABR is ignored so runs are comparable), `BENCH_DECODE_MS` (simulated decode time per frame in
//! the viewer; default 0), `BENCH_BURN` (threads spinning at 100% CPU; default 0), `BENCH_REAL=1`
//! (real WGC capture of the primary monitor + hardware encoder instead of the synthetic source),
//! `BENCH_VIEWER=1` (the consumer decodes like the viewer: D3D11VA, gap = wait for a keyframe).

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sala_core::config::{Config, Friend, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::session::{AudioSource, Session};
use sala_core::share::{Pipe, PipeFactory, VideoPipe};
use sala_protocol::{MediaDatagram, VideoCodec, max_video_payload};
use sala_video::abr::ReceiverReport;
use sala_video::fragment::{DEFAULT_FEC_GROUP, fragment_fec};
use sala_video::preset::SharePreset;
use sala_video::sender::SenderStats;

const FPS: u64 = 30;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn temp_root() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sala-bench-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port()
}

fn friend(name: &str, port: u16) -> Friend {
    Friend { name: name.into(), address: "127.0.0.1".into(), port: Some(port), fingerprint: None }
}

fn start(root: &Path, name: &str, port: u16, friends: Vec<Friend>) -> Session {
    let profile = Profile::resolve_in(root, Some(name)).unwrap();
    let mut config = Config::load_or_create(&profile).unwrap();
    config.port = port;
    config.friends = friends;
    config.save(&profile).unwrap();
    let identity = LocalIdentity::load_or_create(&profile.data_dir).unwrap();
    Session::start(profile, config, &identity, SocketAddr::from((Ipv4Addr::LOCALHOST, port)), AudioSource::Synthetic)
        .unwrap()
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[derive(Default)]
struct Counters {
    frames: AtomicU64,
    keyframes: AtomicU64,
    dropped_deltas: AtomicU64,
    keyframe_requests: AtomicU32,
    reports: Mutex<Vec<ReceiverReport>>,
    released: AtomicBool,
}

struct BenchPipe {
    c: Arc<Counters>,
    key_flag: Arc<AtomicBool>,
    kbps: u32,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl VideoPipe for BenchPipe {
    fn request_keyframe(&self) {
        self.c.keyframe_requests.fetch_add(1, Ordering::Relaxed);
        self.key_flag.store(true, Ordering::Relaxed);
    }

    fn report(&self, report: ReceiverReport) -> u32 {
        self.c.reports.lock().unwrap().push(report);
        self.kbps
    }

    fn target_kbps(&self) -> u32 {
        self.kbps
    }

    fn encoder_stats(&self) -> Option<SenderStats> {
        Some(SenderStats {
            frames: self.c.frames.load(Ordering::Relaxed),
            keyframes: self.c.keyframes.load(Ordering::Relaxed),
            dropped_deltas: self.c.dropped_deltas.load(Ordering::Relaxed),
            ..Default::default()
        })
    }
}

impl Drop for BenchPipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
        self.c.released.store(true, Ordering::Relaxed);
    }
}

fn factory(c: &Arc<Counters>, kbps: u32) -> PipeFactory {
    let c = c.clone();
    Box::new(move |params| {
        let max_payload = max_video_payload(params.max_datagram);
        let (tx, rx) = mpsc::sync_channel::<Vec<MediaDatagram>>(16);
        let (key_flag, stop) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let thread = {
            let (key_flag, stop, c) = (key_flag.clone(), stop.clone(), c.clone());
            std::thread::spawn(move || {
                // Sizes: the average delta is bitrate / fps * 0.85; a keyframe is 9x that.
                let delta_avg = u64::from(kbps) * 1000 / 8 / FPS * 85 / 100;
                let (mut id, mut key_next, mut rng) = (0u32, true, 0x9e37_79b9_u32);
                let tick = Duration::from_micros(1_000_000 / FPS);
                let mut next = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    // Sleep until ~1.5 ms before the deadline, then spin (Windows timers are coarse).
                    while let Some(left) = next.checked_duration_since(Instant::now()) {
                        if left > Duration::from_millis(3) {
                            std::thread::sleep(left - Duration::from_millis(2));
                        } else {
                            std::hint::spin_loop();
                        }
                    }
                    next += tick;
                    id += 1;
                    let key = key_next || key_flag.swap(false, Ordering::Relaxed);
                    key_next = false;
                    rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let jitter = 60 + u64::from(rng >> 16) % 80; // 60..140 %
                    let size = if key { delta_avg * 9 } else { delta_avg * jitter / 100 } as usize;
                    let data = vec![(id % 251) as u8; size];
                    let fec = if key { DEFAULT_FEC_GROUP } else { 0 };
                    let dgs = fragment_fec(id, key, id.wrapping_mul(3000), &data, max_payload, fec);
                    match tx.try_send(dgs) {
                        Err(TrySendError::Disconnected(_)) => break,
                        Err(TrySendError::Full(_)) => {
                            c.dropped_deltas.fetch_add(1, Ordering::Relaxed);
                            key_next = true;
                        }
                        Ok(()) => {
                            c.frames.fetch_add(1, Ordering::Relaxed);
                            c.keyframes.fetch_add(u64::from(key), Ordering::Relaxed);
                        }
                    }
                }
            })
        };
        let control = Box::new(BenchPipe { c, key_flag, kbps, stop, thread: Mutex::new(Some(thread)) });
        Ok(Pipe { control, datagrams: rx, codec: VideoCodec::H264, width: 1920, height: 1080, fps: FPS as u32 })
    })
}

#[test]
#[ignore = "diagnostic: prints numbers, takes BENCH_SECS"]
fn loopback_1080p30_loss_between_two_sessions() {
    let _ = tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).with_target(false).try_init();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    rt.block_on(run());
}

async fn run() {
    let secs = env_u64("BENCH_SECS", 20);
    let kbps = env_u64("BENCH_KBPS", 8000) as u32;
    let decode_ms = env_u64("BENCH_DECODE_MS", 0);
    // CPU contention like the real run (capture + encode + decode + render on the same 4 cores).
    let burn_stop = Arc::new(AtomicBool::new(false));
    let burners: Vec<_> = (0..env_u64("BENCH_BURN", 0))
        .map(|_| {
            let stop = burn_stop.clone();
            std::thread::spawn(move || {
                if env_u64("BENCH_BURN_PRIO", 0) != 0 {
                    // SAFETY: plain Win32 call on the current thread.
                    unsafe {
                        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
                        SetThreadPriority(GetCurrentThread(), env_u64("BENCH_BURN_PRIO", 0) as i32);
                    }
                }
                let mut x = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..10_000 {
                        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                }
            })
        })
        .collect();
    let root = temp_root();
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let a = start(&root, "a", pa, vec![friend("b", pb)]);
    let b = start(&root, "b", pb, vec![friend("a", pa)]);
    wait_until("connected", || a.peer_status("b").is_some() && b.peer_status("a").is_some()).await;
    tokio::time::sleep(Duration::from_millis(1600)).await;

    let mut frames = b.take_video_frames().expect("video stream");
    let viewer_frames = Arc::new(AtomicU64::new(0));
    let viewer_keys = Arc::new(AtomicU64::new(0));
    let viewer_gaps = Arc::new(AtomicU64::new(0));
    let viewer_asks = Arc::new(AtomicU64::new(0));
    let real_viewer = env_u64("BENCH_VIEWER", 0) != 0;
    let (ask_tx, mut ask_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let consumer = {
        let (n, k, g, asks) = (viewer_frames.clone(), viewer_keys.clone(), viewer_gaps.clone(), viewer_asks.clone());
        tokio::task::spawn_blocking(move || {
            // Like `viewer::decode_thread`: gap in the numbering = wait for a keyframe; the real
            // decoder (D3D11VA + RGBA conversion) when asked to.
            let mut decoder = None;
            let mut last_id: Option<u32> = None;
            let mut last_ask: Option<Instant> = None;
            while let Some(f) = frames.blocking_recv() {
                if real_viewer {
                    let gap = last_id.is_some_and(|l| f.frame.frame_id != l.wrapping_add(1));
                    last_id = Some(f.frame.frame_id);
                    let ask = |last_ask: &mut Option<Instant>| {
                        if last_ask.is_none_or(|t| t.elapsed() >= Duration::from_millis(500)) {
                            *last_ask = Some(Instant::now());
                            asks.fetch_add(1, Ordering::Relaxed);
                            let _ = ask_tx.send(());
                        }
                    };
                    if gap && !f.frame.keyframe {
                        g.fetch_add(1, Ordering::Relaxed);
                        ask(&mut last_ask);
                        continue;
                    }
                    let dec = decoder.get_or_insert_with(|| {
                        let d = sala_video::decoder::VideoDecoder::open(f.codec).expect("decoder");
                        println!("viewer decoder: {} hardware={}", d.name(), d.is_hardware());
                        d
                    });
                    let out = dec.decode(&f.frame);
                    if out.request_keyframe {
                        ask(&mut last_ask);
                    }
                    if out.frame.is_none() {
                        continue;
                    }
                }
                n.fetch_add(1, Ordering::Relaxed);
                k.fetch_add(u64::from(f.frame.keyframe), Ordering::Relaxed);
                if decode_ms > 0 {
                    std::thread::sleep(Duration::from_millis(decode_ms));
                }
            }
        })
    };

    let c = Arc::new(Counters::default());
    let real = env_u64("BENCH_REAL", 0) != 0;
    let factory: PipeFactory = if real {
        let source = sala_video::capture::list_monitors().unwrap().into_iter().find(|s| s.primary).expect("primary monitor");
        Box::new(move |params| sala_core::share::screen_pipe(params, VideoCodec::H264, &source))
    } else {
        factory(&c, kbps)
    };
    let t0 = Instant::now();
    a.start_share(SharePreset::P1080p30, factory).expect("start share");
    while t0.elapsed() < Duration::from_secs(secs) {
        if ask_rx.try_recv().is_ok() {
            b.request_keyframe("a"); // what the viewer's requester does
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let elapsed = t0.elapsed().as_secs_f32();
    let stats = a.share_stats().expect("share stats");
    a.stop_share();
    if real {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        c.released.store(true, Ordering::Relaxed);
    }
    wait_until("pipe released", || c.released.load(Ordering::Relaxed)).await;
    println!("viewer gaps {} keyframe asks {}", viewer_gaps.load(Ordering::Relaxed), viewer_asks.load(Ordering::Relaxed));

    let reports = c.reports.lock().unwrap().clone();
    let losses: Vec<f32> = reports.iter().map(|r| (r.loss_pct * 10.0).round() / 10.0).collect();
    let (sent, produced) = (c.frames.load(Ordering::Relaxed), viewer_frames.load(Ordering::Relaxed));
    println!("=== RESULT ({secs} s, {kbps} kbps, decode {decode_ms} ms) ===");
    println!(
        "encoded frames {sent} ({} keyframes, {} dropped by the full channel); delivered to the viewer {produced} ({} keyframes) = {:.1} fps of {FPS}",
        c.keyframes.load(Ordering::Relaxed),
        c.dropped_deltas.load(Ordering::Relaxed),
        viewer_keys.load(Ordering::Relaxed),
        produced as f32 / elapsed
    );
    println!(
        "datagrams sent ok {} / errors {}; keyframe requests {}; reports {} (loss % each second: {losses:?})",
        stats.datagrams_sent,
        stats.send_errors,
        c.keyframe_requests.load(Ordering::Relaxed),
        reports.len()
    );
    consumer.abort();
    a.shutdown().await;
    b.shutdown().await;
    burn_stop.store(true, Ordering::Relaxed);
    for t in burners {
        let _ = t.join();
    }
}
