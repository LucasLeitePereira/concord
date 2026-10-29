//! End-to-end check without network: capture the primary monitor, encode with the hardware
//! encoder, fragment, (optionally drop datagrams), reassemble, and print statistics.
//!
//! `cargo run -p sala-video --example capture_encode -- [--seconds 5] [--fps 30] [--kbps 8000]
//!  [--codec h264|hevc|av1] [--loss 0.0] [--window "title part"]`

use std::sync::mpsc;
use std::time::{Duration, Instant};

use sala_protocol::max_video_payload;
use sala_video::capture::{CaptureSession, list_monitors, list_windows};
use sala_video::encoder::{EncoderConfig, TIME_BASE_HZ, VideoEncoder};
use sala_video::fragment::{Reassembler, fragment};

struct Raw {
    w: u32,
    h: u32,
    ts: Duration,
    data: Vec<u8>,
}

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let seconds: u64 = arg("--seconds", 5);
    let fps: u32 = arg("--fps", 30);
    let kbps: u32 = arg("--kbps", 8000);
    let loss: f64 = arg("--loss", 0.0);
    let window: String = arg("--window", String::new());
    let codec = match arg("--codec", "h264".to_string()).as_str() {
        "hevc" => sala_protocol::VideoCodec::Hevc,
        "av1" => sala_protocol::VideoCodec::Av1,
        _ => sala_protocol::VideoCodec::H264,
    };

    let monitors = list_monitors().expect("list monitors");
    println!("monitors:");
    for m in &monitors {
        println!("  {:?} {} {}x{}{}", m.target, m.name, m.width, m.height, if m.primary { " (primary)" } else { "" });
    }
    let windows = list_windows().expect("list windows");
    println!("windows: {} visible", windows.len());
    for w in windows.iter().take(5) {
        println!("  {:?} {}", w.target, w.name);
    }
    let (target, tw, th) = if window.is_empty() {
        let m = monitors.first().expect("no monitor");
        (m.target, m.width, m.height)
    } else {
        let w = windows.iter().find(|w| w.name.contains(&window)).expect("window not found");
        (w.target, w.width, w.height)
    };
    // Cap at 1080p keeping the aspect ratio; dimensions must be even.
    let scale = (1920.0 / f64::from(tw)).min(1080.0 / f64::from(th)).min(1.0);
    let ow = ((f64::from(tw) * scale) as u32) & !1;
    let oh = ((f64::from(th) * scale) as u32) & !1;
    let mut enc = VideoEncoder::open(&EncoderConfig { codec, width: ow, height: oh, fps, bitrate_kbps: kbps })
        .expect("open encoder");
    println!("encoder: {} {ow}x{oh} {fps} fps target {kbps} kbps", enc.name());

    let (tx, rx) = mpsc::sync_channel::<Raw>(2);
    let session = CaptureSession::start(target, fps, move |f| {
        // Drop the frame when the encoder is behind (live capture never queues).
        let _ = tx.try_send(Raw { w: f.width, h: f.height, ts: f.timestamp, data: f.data.to_vec() });
    })
    .expect("start capture");

    let max_payload = max_video_payload(1200);
    let mut reasm = Reassembler::new();
    let t0 = Instant::now();
    let (mut captured, mut encoded, mut delivered, mut keyframe_reqs) = (0u64, 0u64, 0u64, 0u32);
    let (mut bytes, mut datagrams, mut dropped) = (0u64, 0u64, 0u64);
    let (mut frame_id, mut first_ts) = (0u32, None);
    let mut last: Option<Raw> = None;
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut sum_ms = 0.0;

    while t0.elapsed() < Duration::from_secs(seconds) {
        let raw = match rx.recv_timeout(Duration::from_millis(1000 / u64::from(fps))) {
            Ok(r) => {
                captured += 1;
                last = Some(Raw { w: r.w, h: r.h, ts: r.ts, data: r.data.clone() });
                r
            }
            // Static screen: WGC sends nothing, re-encode the last frame to keep the cadence.
            Err(_) => match &last {
                Some(l) => Raw { w: l.w, h: l.h, ts: l.ts + Duration::from_millis(1), data: l.data.clone() },
                None => continue,
            },
        };
        let first = *first_ts.get_or_insert(raw.ts);
        let ticks = (raw.ts.saturating_sub(first).as_secs_f64() * f64::from(TIME_BASE_HZ)) as i64;
        let started = Instant::now();
        let packets = enc.encode_bgra(&raw.data, raw.w, raw.h, ticks).expect("encode");
        sum_ms += started.elapsed().as_secs_f64() * 1000.0;
        for p in packets {
            encoded += 1;
            bytes += p.data.len() as u64;
            frame_id += 1;
            for dg in fragment(frame_id, p.keyframe, p.pts as u32, &p.data, max_payload) {
                datagrams += 1;
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                if (rng % 10_000) as f64 / 10_000.0 < loss / 100.0 {
                    dropped += 1;
                    continue;
                }
                let now = t0.elapsed().as_millis() as u64;
                let out = reasm.push(now, &dg);
                if out.frame.is_some() {
                    delivered += 1;
                }
                if out.request_keyframe {
                    keyframe_reqs += 1;
                    enc.force_keyframe();
                }
            }
        }
    }
    let _ = session.stop();
    let secs = t0.elapsed().as_secs_f64();
    println!("--- {secs:.1} s ---");
    println!("captured (new screen frames): {captured} ({:.1}/s)", captured as f64 / secs);
    println!("encoded: {encoded} ({:.1} fps), avg encode call {:.2} ms", encoded as f64 / secs, sum_ms / encoded.max(1) as f64);
    println!("bitrate: {:.0} kbps", bytes as f64 * 8.0 / secs / 1000.0);
    println!("datagrams: {datagrams} (payload <= {max_payload} B), dropped on purpose: {dropped}");
    println!("reassembled: {delivered}, keyframe requests: {keyframe_reqs}");
}
