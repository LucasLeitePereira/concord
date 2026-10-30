//! Manual check of the viewer without network: encodes a moving test pattern (colour bars, a
//! sweeping square and a frame counter in binary blocks) with the real hardware encoder and
//! feeds the viewer window exactly like the core does. The viewer's keyframe requests are
//! answered, so closing nothing but waiting also shows recovery. Keys: F11, F3, Esc.
//!
//! `cargo run -p sala-video --example view_synthetic -- [--codec h264|hevc|av1] [--width 1280]
//!  [--height 720] [--fps 30] [--kbps 4000] [--drop-every N]`
//! `--drop-every N` loses one delta frame in N to show the gap handling and the keyframe request.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use sala_protocol::VideoCodec;
use sala_video::encoder::{EncoderConfig, VideoEncoder};
use sala_video::fragment::Frame;
use sala_video::viewer::{self, Incoming};

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn pattern(buf: &mut [u8], w: usize, h: usize, n: u32) {
    const BARS: [[u8; 3]; 7] =
        [[255, 255, 255], [255, 255, 0], [0, 255, 255], [0, 255, 0], [255, 0, 255], [255, 0, 0], [0, 0, 255]];
    let sq = h / 5;
    let sx = (n as usize * 6) % (w - sq);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = if y < h * 2 / 3 {
                let c = BARS[x * BARS.len() / w];
                if y > h / 4 { [c[0] / 2, c[1] / 2, c[2] / 2] } else { c }
            } else {
                let v = (x * 255 / w) as u8;
                [v, v, v]
            };
            let o = (y * w + x) * 4;
            buf[o..o + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
    // Sweeping black square with a white border.
    for y in 0..sq {
        for x in 0..sq {
            let edge = x < 4 || y < 4 || x >= sq - 4 || y >= sq - 4;
            let v = if edge { 255 } else { 0 };
            let o = ((h / 3 + y) * w + sx + x) * 4;
            buf[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    // Frame counter: 16 binary blocks in the top-left corner.
    for bit in 0..16usize {
        let on = (n >> (15 - bit)) & 1 == 1;
        let v = if on { 255 } else { 20 };
        for y in 8..28 {
            for x in 0..20 {
                let o = (y * w + 8 + bit * 24 + x) * 4;
                buf[o..o + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
}

fn main() {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let codec = match arg("--codec", "h264".to_string()).as_str() {
        "hevc" => VideoCodec::Hevc,
        "av1" => VideoCodec::Av1,
        _ => VideoCodec::H264,
    };
    let (w, h, fps): (u32, u32, u32) = (arg("--width", 1280), arg("--height", 720), arg("--fps", 30));
    let drop_every: u32 = arg("--drop-every", 0);
    let reopen: u32 = arg("--reopen", 0);
    if reopen > 0 {
        // No encoder needed: open, show for a moment, close and open again, in the same process.
        let mut rx = {
            let (_tx, rx) = std::sync::mpsc::channel::<Incoming>();
            rx
        };
        for round in 1..=reopen {
            let handle = viewer::open(rx, Arc::new(|_| {})).unwrap_or_else(|e| panic!("round {round}: {e}"));
            println!("round {round}: viewer open = {}", handle.is_open());
            std::thread::sleep(Duration::from_millis(1000));
            rx = handle.close().expect("the input channel comes back");
            println!("round {round}: closed");
        }
        println!("reopened {reopen} times");
        return;
    }
    let kbps = arg("--kbps", 4000);
    let cfg = EncoderConfig { codec, width: w, height: h, fps, bitrate_kbps: kbps, ceiling_kbps: kbps };
    let mut enc = match VideoEncoder::open(&cfg) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("no hardware encoder for {codec:?}: {e}");
            std::process::exit(1);
        }
    };
    println!("encoder: {}", enc.name());

    let (tx, rx) = std::sync::mpsc::channel::<Incoming>();
    let want_key = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(AtomicU32::new(0));
    let handle = {
        let (want_key, requests) = (want_key.clone(), requests.clone());
        viewer::open(
            rx,
            Arc::new(move |friend| {
                println!("viewer asked {friend} for a keyframe");
                requests.fetch_add(1, Ordering::Relaxed);
                want_key.store(true, Ordering::Release);
            }),
        )
        .expect("viewer")
    };

    let mut bgra = vec![0u8; (w * h * 4) as usize];
    let period = Duration::from_micros(1_000_000 / u64::from(fps));
    let started = Instant::now();
    let (mut n, mut frame_id, mut sent) = (0u32, 0u32, 0u32);
    while handle.is_open() {
        let due = started + period * n;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if want_key.swap(false, Ordering::AcqRel) {
            enc.force_keyframe();
        }
        pattern(&mut bgra, w as usize, h as usize, n);
        let packets = match enc.encode_bgra(&bgra, w, h, i64::from(n) * 90_000 / i64::from(fps)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("encode failed: {e}");
                break;
            }
        };
        for p in packets {
            frame_id = frame_id.wrapping_add(1);
            if drop_every > 0 && !p.keyframe && frame_id % drop_every == 0 {
                continue; // simulated loss: the viewer sees a hole in the numbering
            }
            sent += 1;
            let frame = Frame { frame_id, keyframe: p.keyframe, timestamp: p.pts as u32, data: p.data };
            let _ = tx.send(Incoming { friend: "teste".into(), codec, frame });
        }
        n += 1;
    }
    println!("viewer closed after {sent} frames sent, {} keyframe requests", requests.load(Ordering::Relaxed));
    let back = handle.close();
    println!("input channel handed back: {}", back.is_some());
}
