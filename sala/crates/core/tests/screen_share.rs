//! T-033c2: screen share through the session (`screen-share`, `receiver-report`) with a
//! synthetic video source: no capture, no hardware encoder.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sala_core::config::{Config, Friend, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::session::{AudioSource, Session};
use sala_core::share::{Pipe, PipeFactory, VideoFrame, VideoPipe};
use sala_protocol::{DEFAULT_MAX_DATAGRAM, MediaDatagram, VideoCodec, max_video_payload};
use sala_video::abr::{Abr, ReceiverReport};
use sala_video::fragment::{DEFAULT_FEC_GROUP, fragment_fec};
use sala_video::preset::SharePreset;

const PRESET: SharePreset = SharePreset::P720p30;
const CODEC: VideoCodec = VideoCodec::H264;

fn temp_root() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sala-share-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A free port never handed out before in this process.
fn free_udp_port() -> u16 {
    static GIVEN: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    loop {
        let port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        let mut given = GIVEN.lock().unwrap();
        if !given.contains(&port) {
            given.push(port);
            return port;
        }
    }
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

/// Both sides dial at start; the duplicate connection is replaced about 1 s later (see
/// `settle_connection` in f3_settings.rs).
async fn connected_pair() -> (Session, Session) {
    let root = temp_root();
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let a = start(&root, "a", pa, vec![friend("b", pb)]);
    let b = start(&root, "b", pb, vec![friend("a", pa)]);
    wait_until("connected", || a.peer_status("b").is_some() && b.peer_status("a").is_some()).await;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    (a, b)
}

// ---------------------------------------------------------------- synthetic source

/// What the test can see of the fake encoder.
#[derive(Clone, Default)]
struct Probe {
    keyframe_requests: Arc<AtomicU32>,
    reports: Arc<Mutex<Vec<ReceiverReport>>>,
    /// Set when the pipe is dropped (after its producer thread ended).
    released: Arc<AtomicBool>,
    max_datagram: Arc<AtomicUsize>,
}

struct FakePipe {
    abr: Mutex<Abr>,
    started: Instant,
    probe: Probe,
    key_flag: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl VideoPipe for FakePipe {
    fn request_keyframe(&self) {
        self.probe.keyframe_requests.fetch_add(1, Ordering::Relaxed);
        self.key_flag.store(true, Ordering::Relaxed);
    }

    fn report(&self, report: ReceiverReport) -> u32 {
        self.probe.reports.lock().unwrap().push(report);
        self.abr.lock().unwrap().on_report(self.started.elapsed().as_millis() as u64, &report)
    }

    fn target_kbps(&self) -> u32 {
        self.abr.lock().unwrap().target_kbps()
    }
}

impl Drop for FakePipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
        self.probe.released.store(true, Ordering::Relaxed);
    }
}

/// Every frame is filled with `frame_id % 251`, so the receiver can check the reassembly.
/// With `lossy`, one fragment of every second delta frame is never sent.
fn factory(probe: &Probe, lossy: bool) -> PipeFactory {
    let probe = probe.clone();
    Box::new(move |params| {
        probe.max_datagram.store(params.max_datagram, Ordering::Relaxed);
        let max_payload = max_video_payload(params.max_datagram);
        let (tx, rx) = mpsc::sync_channel::<Vec<MediaDatagram>>(16);
        let (key_flag, stop) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let thread = {
            let (key_flag, stop) = (key_flag.clone(), stop.clone());
            std::thread::spawn(move || {
                let (mut id, mut key_next) = (0u32, true);
                while !stop.load(Ordering::Relaxed) {
                    id += 1;
                    let key = key_next || key_flag.swap(false, Ordering::Relaxed);
                    key_next = false;
                    let data = vec![(id % 251) as u8; if key { 20_000 } else { 3_000 }];
                    let fec = if key { DEFAULT_FEC_GROUP } else { 0 };
                    let mut dgs = fragment_fec(id, key, id.wrapping_mul(90), &data, max_payload, fec);
                    if lossy && !key && id % 2 == 0 {
                        dgs.retain(|d| !matches!(d, MediaDatagram::Video { frag_idx: 1, .. }));
                    }
                    match tx.try_send(dgs) {
                        Err(TrySendError::Disconnected(_)) => break,
                        Err(TrySendError::Full(_)) => key_next = true,
                        Ok(()) => {}
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
            })
        };
        let range = PRESET.bitrate(CODEC);
        let control = Box::new(FakePipe {
            abr: Mutex::new(Abr::new(range)),
            started: Instant::now(),
            probe: probe.clone(),
            key_flag,
            stop,
            thread: Mutex::new(Some(thread)),
        });
        Ok(Pipe { control, datagrams: rx, codec: CODEC, width: 1280, height: 720, fps: 30 })
    })
}

async fn recv_frame(rx: &mut tokio::sync::mpsc::Receiver<VideoFrame>, what: &str) -> VideoFrame {
    tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for: {what}"))
        .expect("video channel closed")
}

// ---------------------------------------------------------------- tests

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn share_start_reaches_the_viewer_who_reports_and_share_stop_releases_everything() {
    let (a, b) = connected_pair().await;
    let mut frames = b.take_video_frames().expect("video stream");
    let probe = Probe::default();
    a.start_share(PRESET, factory(&probe, false)).expect("start share");
    assert!(a.local_status().sharing);
    wait_until("B sees A sharing", || b.peer_status("a").is_some_and(|s| s.sharing)).await;
    let max = probe.max_datagram.load(Ordering::Relaxed);
    assert!(max > 0 && max <= DEFAULT_MAX_DATAGRAM, "max_datagram from the real connection: {max}");
    assert!(a.start_share(PRESET, factory(&Probe::default(), false)).is_err(), "one share at a time");

    // The first frame the viewer gets is a keyframe; every frame is reassembled intact.
    let first = recv_frame(&mut frames, "first frame").await;
    assert_eq!(first.friend, "a");
    assert!(first.frame.keyframe);
    let mut seen = 1;
    while seen < 40 {
        let f = recv_frame(&mut frames, "stream of frames").await.frame;
        assert!(f.data.iter().all(|b| *b == (f.frame_id % 251) as u8), "frame {} corrupted", f.frame_id);
        assert_eq!(f.data.len(), if f.keyframe { 20_000 } else { 3_000 });
        seen += 1;
    }

    // Clean link: reports arrive, show (almost) no loss and do not cut the bitrate.
    wait_until("receiver reports", || a.share_stats().is_some_and(|s| s.reports >= 2)).await;
    let stats = a.share_stats().unwrap();
    assert!(stats.running && stats.viewers == 1 && stats.datagrams_sent > 40, "{stats:?}");
    assert_eq!(stats.send_errors, 0);
    assert!(stats.target_kbps >= PRESET.bitrate(CODEC).initial, "no loss, no cut: {stats:?}");

    // Stop: everything is released, the viewer is told, no more frames arrive.
    a.stop_share();
    wait_until("pipe released", || probe.released.load(Ordering::Relaxed)).await;
    assert!(a.share_stats().is_none());
    assert!(!a.local_status().sharing);
    wait_until("B sees A stop", || b.peer_status("a").is_some_and(|s| !s.sharing)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    while frames.try_recv().is_ok() {}
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(frames.try_recv().is_err(), "frames kept arriving after ShareStop");

    // The slot is free again.
    let again = Probe::default();
    a.start_share(PRESET, factory(&again, false)).expect("second share");
    recv_frame(&mut frames, "frame of the second share").await;
    a.stop_share();
    wait_until("second pipe released", || again.released.load(Ordering::Relaxed)).await;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loss_at_the_viewer_cuts_the_bitrate_and_asks_for_keyframes() {
    let (a, b) = connected_pair().await;
    let mut frames = b.take_video_frames().expect("video stream");
    let probe = Probe::default();
    a.start_share(PRESET, factory(&probe, true)).expect("start share");
    recv_frame(&mut frames, "first frame").await;
    let initial = PRESET.bitrate(CODEC).initial;
    // The drain loop of the test keeps the bounded channel from filling up.
    let drain = tokio::spawn(async move { while frames.recv().await.is_some() {} });

    wait_until("bitrate cut by the reports", || a.share_stats().is_some_and(|s| s.target_kbps < initial)).await;
    let reports = probe.reports.lock().unwrap().clone();
    assert!(reports.iter().any(|r| r.loss_pct > 5.0), "reports: {reports:?}");
    // 1 for the viewer joining, more because of the gaps the viewer saw.
    assert!(probe.keyframe_requests.load(Ordering::Relaxed) >= 2, "viewer never asked for a keyframe");

    a.stop_share();
    wait_until("pipe released", || probe.released.load(Ordering::Relaxed)).await;
    drain.abort();
    a.shutdown().await;
    b.shutdown().await;
}
