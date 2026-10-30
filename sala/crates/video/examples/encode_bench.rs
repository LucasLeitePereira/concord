//! T-035f: where does the time of one encoded frame go? Encodes synthetic BGRA frames with the
//! real hardware encoder as fast as it goes and prints the average per part (input copy,
//! swscale, encoder proper) and the fps it could sustain.
//!
//! ```text
//! cargo run --release -p sala-video --example encode_bench -- [--width 1920] [--height 1080]
//!  [--frames 150] [--kbps 8000] [--burn 0] [--prio below|normal]
//! ```
//!
//! `--burn N` spins N threads at normal priority (the machine under load, like capture, decode
//! and render in the real run); `--prio` is the priority of the encode thread (the sender runs
//! it at below-normal).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sala_protocol::VideoCodec;
use sala_video::encoder::{EncoderConfig, VideoEncoder};

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThread() -> isize;
    fn SetThreadPriority(thread: isize, priority: i32) -> i32;
}

fn set_priority(level: i32) {
    // SAFETY: plain Win32 calls on the current thread's pseudo-handle.
    unsafe {
        SetThreadPriority(GetCurrentThread(), level);
    }
}

/// Gradient background, scrolling stripes and a noisy quarter of the screen (like a game scene).
fn pattern(buf: &mut [u8], w: usize, h: usize, n: u32) {
    let mut s = 0x9E37_79B9u32 ^ n.wrapping_mul(2_654_435_761);
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let noisy = x < w / 2 && y < h / 2;
            let px = if noisy {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                [(s & 0xFF) as u8, ((s >> 8) & 0xFF) as u8, ((s >> 16) & 0xFF) as u8, 255]
            } else {
                let stripe = ((x + n as usize * 9) / 48).is_multiple_of(2);
                let v = if stripe { 200 } else { 60 };
                [v, (y * 255 / h) as u8, (x * 255 / w) as u8, 255]
            };
            buf[o..o + 4].copy_from_slice(&px);
        }
    }
}

fn main() {
    let (w, h, frames): (u32, u32, u32) = (arg("--width", 1920), arg("--height", 1080), arg("--frames", 150));
    let burn: usize = arg("--burn", 0);
    let prio = arg("--prio", "below".to_string());
    let cfg = EncoderConfig { codec: VideoCodec::H264, width: w, height: h, fps: 30, bitrate_kbps: arg("--kbps", 8000) };
    let mut enc = VideoEncoder::open(&cfg).unwrap_or_else(|e| panic!("no encoder: {e}"));
    let scenes: Vec<Vec<u8>> = (0..4)
        .map(|i| {
            let mut b = vec![0u8; (w * h * 4) as usize];
            pattern(&mut b, w as usize, h as usize, i);
            b
        })
        .collect();

    let stop = Arc::new(AtomicBool::new(false));
    let burners: Vec<_> = (0..burn)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut x = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..10_000 {
                        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                }
            })
        })
        .collect();
    set_priority(if prio == "normal" { 0 } else { -1 });

    // Warm-up: the first frames open the swscale context and the encoder pipeline.
    for i in 0..5i64 {
        enc.encode_bgra(&scenes[i as usize % 4], w, h, i * 3000).expect("encode");
    }
    let (mut copy, mut sws, mut enc_t, mut packets, mut bytes) = (0u64, 0u64, 0u64, 0usize, 0usize);
    let t0 = Instant::now();
    for i in 0..i64::from(frames) {
        let out = enc.encode_bgra(&scenes[i as usize % 4], w, h, (i + 5) * 3000).expect("encode");
        let t = enc.last_timings();
        (copy, sws, enc_t) = (copy + t.copy_micros, sws + t.sws_micros, enc_t + t.enc_micros);
        packets += out.len();
        bytes += out.iter().map(|p| p.data.len()).sum::<usize>();
    }
    let total = t0.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    for b in burners {
        let _ = b.join();
    }
    let n = f64::from(frames);
    let ms = |us: u64| us as f64 / 1000.0 / n;
    println!(
        "{} {w}x{h} burn={burn} prio={prio}: {:.1} fps | per frame: total {:.1} ms = copy {:.1} + sws {:.1} + enc {:.1} | {packets} packets, {} KB avg",
        enc.name(),
        n / total,
        total * 1000.0 / n,
        ms(copy),
        ms(sws),
        ms(enc_t),
        bytes / 1024 / packets.max(1)
    );
}
