//! T-037a/b: cost and effect of the real `VideoEncoder::set_bitrate` (live on QSV, reopen on the
//! other backends): time of the call, time of the next frame, keyframe, bitrate measured after the
//! change and the biggest frame in the first second after it (the VBV is fixed at the ceiling).
//!
//! ```text
//! cargo run --release -p sala-video --example bitrate_change_bench -- [--reps 5] [--burn 0]
//!  [--width 1920] [--height 1080] [--lo 3000] [--hi 12000] [--start 12000] [--secs 2]
//! ```
//! The encoder opens at `--start` with the ceiling at `--hi`, then alternates `--lo` and `--hi`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sala_protocol::VideoCodec;
use sala_video::encoder::{BitrateChange, EncodedPacket, EncoderConfig, VideoEncoder};

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default)
}

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

struct Feed {
    scenes: Vec<Vec<u8>>,
    w: u32,
    h: u32,
    n: i64,
}

impl Feed {
    /// Encodes one frame; returns (ms of the call, packets).
    fn frame(&mut self, enc: &mut VideoEncoder) -> (f64, Vec<EncodedPacket>) {
        let t = Instant::now();
        let out = enc.encode_bgra(&self.scenes[self.n as usize % 4], self.w, self.h, self.n * 3000).expect("encode");
        self.n += 1;
        (t.elapsed().as_secs_f64() * 1000.0, out)
    }

    /// Runs frames for `secs` of media time (as fast as the encoder goes); returns
    /// (kbps of output, biggest packet in bytes).
    fn run(&mut self, enc: &mut VideoEncoder, secs: f64) -> (f64, usize) {
        let frames = (secs * 30.0) as u32;
        let (mut bytes, mut biggest) = (0usize, 0usize);
        for _ in 0..frames {
            let (_, out) = self.frame(enc);
            for p in &out {
                bytes += p.data.len();
                biggest = biggest.max(p.data.len());
            }
        }
        (bytes as f64 * 8.0 / secs / 1000.0, biggest)
    }
}

fn med_max(v: &mut [f64]) -> (f64, f64) {
    if v.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[v.len() / 2], v[v.len() - 1])
}

fn main() {
    let (w, h): (u32, u32) = (arg("--width", 1920), arg("--height", 1080));
    let (reps, burn, secs): (usize, usize, f64) = (arg("--reps", 5), arg("--burn", 0), arg("--secs", 2.0));
    let (lo, hi): (u32, u32) = (arg("--lo", 3000), arg("--hi", 12000));
    let start: u32 = arg("--start", hi);
    let cfg = EncoderConfig { codec: VideoCodec::H264, width: w, height: h, fps: 30, bitrate_kbps: start, ceiling_kbps: hi };
    let mut enc = VideoEncoder::open(&cfg).unwrap_or_else(|e| panic!("no encoder: {e}"));
    let scenes: Vec<Vec<u8>> = (0..4)
        .map(|i| {
            let mut b = vec![0u8; (w * h * 4) as usize];
            pattern(&mut b, w as usize, h as usize, i);
            b
        })
        .collect();
    let mut feed = Feed { scenes, w, h, n: 0 };

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
    // `--log 1`: FFmpeg's own log (the QSV reset messages) on stderr.
    if arg("--log", 0u32) == 1 {
        ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Verbose);
    }
    for _ in 0..10 {
        feed.frame(&mut enc);
    }

    let (mut call, mut first, mut keys, mut kinds) = (vec![], vec![], 0, [0usize; 2]);
    let mut steady = vec![];
    // Per change: (target, measured kbps after, biggest packet in the second after, name of the kind).
    let mut effect: Vec<(u32, f64, usize)> = vec![];
    let mut clock = 0u64;
    let mut cur = start;
    let (before, _) = feed.run(&mut enc, secs);
    for _ in 0..reps * 2 {
        let target = if cur == hi { lo } else { hi };
        clock += 60_000;
        let t = Instant::now();
        let kind = enc.set_bitrate(target, clock).expect("set_bitrate");
        call.push(t.elapsed().as_secs_f64() * 1000.0);
        match kind {
            BitrateChange::Live => kinds[0] += 1,
            BitrateChange::Reopened => kinds[1] += 1,
            BitrateChange::None => panic!("set_bitrate held back {cur} -> {target}"),
        }
        cur = target;
        let mut ms_total = 0.0;
        loop {
            let (ms, out) = feed.frame(&mut enc);
            ms_total += ms;
            if let Some(p) = out.first() {
                keys += usize::from(p.keyframe);
                break;
            }
        }
        first.push(ms_total);
        let (kbps, biggest) = feed.run(&mut enc, secs);
        effect.push((target, kbps, biggest));
        for _ in 0..15 {
            let (ms, _) = feed.frame(&mut enc);
            steady.push(ms);
        }
    }
    stop.store(true, Ordering::Relaxed);
    for b in burners {
        let _ = b.join();
    }
    std::thread::sleep(Duration::from_millis(10));

    let changes = reps * 2;
    let (m, x) = med_max(&mut call);
    println!("{} {w}x{h} burn={burn} start={start} lo={lo} hi={hi} changes={changes} (ms: median / max)", enc.name());
    println!("changes live {} / reopened {}; live still on: {}", kinds[0], kinds[1], enc.live_changes_ok());
    println!("set_bitrate call               : {m:8.1} / {x:8.1}");
    let (m, x) = med_max(&mut first);
    println!("first frame with a packet      : {m:8.1} / {x:8.1}  keyframes {keys}/{changes}");
    let (m, x) = med_max(&mut steady);
    println!("steady frame after the change  : {m:8.1} / {x:8.1}");
    println!("steady rate before the first change: {before:.0} kbps measured");
    for (i, (target, kbps, biggest)) in effect.iter().enumerate() {
        println!("change {i}: -> {target} kbps; measured {kbps:.0} kbps; biggest frame in the next {secs} s: {biggest} B");
    }
}
