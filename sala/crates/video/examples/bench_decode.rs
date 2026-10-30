//! Decode benchmark: encodes a moving test pattern with the real hardware encoder, then decodes
//! the whole stream and prints the decoded frames per second and the time per frame.
//! Run it in release: `cargo run --release -p sala-video --example bench_decode -- [--codec h264]
//!  [--width 1920] [--height 1080] [--fps 60] [--kbps 8000] [--frames 240] [--software]`
//! `--software` skips D3D11VA and measures only the FFmpeg software decoder.

use std::time::Instant;

use sala_protocol::VideoCodec;
use sala_video::decoder::VideoDecoder;
use sala_video::encoder::{EncoderConfig, VideoEncoder};
use sala_video::fragment::Frame;

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default)
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
    tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).init();
    let codec = match arg("--codec", "h264".to_string()).as_str() {
        "hevc" => VideoCodec::Hevc,
        "av1" => VideoCodec::Av1,
        _ => VideoCodec::H264,
    };
    let (w, h, fps, frames): (u32, u32, u32, u32) = (arg("--width", 1920), arg("--height", 1080), arg("--fps", 60), arg("--frames", 240));
    let software = std::env::args().any(|a| a == "--software");
    let kbps = arg("--kbps", 8000);
    let cfg = EncoderConfig { codec, width: w, height: h, fps, bitrate_kbps: kbps, ceiling_kbps: kbps };
    let mut enc = VideoEncoder::open(&cfg).unwrap_or_else(|e| panic!("no encoder: {e}"));
    println!("encoder: {}", enc.name());
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    let mut stream = Vec::new();
    let t0 = Instant::now();
    for i in 0..frames {
        pattern(&mut bgra, w as usize, h as usize, i);
        for p in enc.encode_bgra(&bgra, w, h, i64::from(i) * 90_000 / i64::from(fps)).expect("encode") {
            stream.push(Frame { frame_id: stream.len() as u32, keyframe: p.keyframe, timestamp: p.pts as u32, data: p.data });
        }
    }
    println!("encoded {} frames in {:.1}s (incl. pattern generation)", stream.len(), t0.elapsed().as_secs_f32());
    let bytes: usize = stream.iter().map(|f| f.data.len()).sum();
    println!("stream: {:.1} Mbps at {fps} fps", bytes as f32 * 8.0 / 1e6 / (stream.len() as f32 / fps as f32));

    let mut dec = if software { VideoDecoder::open_software(codec) } else { VideoDecoder::open(codec) }.expect("decoder");
    // The hardware decoder may fail on the first keyframe and fall back: feed it twice if so.
    for _ in 0..6 {
        if !dec.decode(&stream[0]).request_keyframe {
            break;
        }
    }
    println!("decoder: {}", dec.name());
    let mut pics = 0u32;
    let t = Instant::now();
    for f in &stream[1..] {
        pics += u32::from(dec.decode(f).frame.is_some());
    }
    let secs = t.elapsed().as_secs_f32();
    println!("decoded {pics} pictures in {secs:.2}s: {:.1} fps, {:.2} ms/frame (need {:.1} ms for {fps} fps)", pics as f32 / secs, secs * 1000.0 / pics.max(1) as f32, 1000.0 / fps as f32);
}
