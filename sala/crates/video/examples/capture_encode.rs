//! End-to-end check without network: capture a monitor/window, run the real `Sender`
//! pipeline (preset, hardware encoder, FEC, adaptive bitrate), drop datagrams on purpose,
//! reassemble on the other side and print statistics. Keyframes are also reassembled
//! without their parity fragments to show what FEC buys.
//!
//! `cargo run -p sala-video --example capture_encode -- [--seconds 5]
//!  [--preset 720p30|1080p30|1080p60|text] [--codec h264|hevc|av1] [--loss 2.0]
//!  [--fec 8] [--window "title part"]`

use std::time::{Duration, Instant};

use sala_protocol::MediaDatagram;
use sala_video::abr::ReceiverReport;
use sala_video::capture::{CaptureSession, list_monitors, list_windows};
use sala_video::fragment::{Reassembler, data_count};
use sala_video::preset::SharePreset;
use sala_video::sender::{FrameSlot, RawFrame, Sender, SenderConfig};

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The datagram as it would look without FEC (parity dropped), or `None` for parity.
fn without_fec(dg: &MediaDatagram) -> Option<MediaDatagram> {
    let MediaDatagram::Video { frame_id, frag_idx, frag_count, keyframe, fec_group, timestamp, payload } = dg else {
        return None;
    };
    if *fec_group == 0 {
        return Some(dg.clone());
    }
    let n = data_count(*frag_count as usize, *fec_group as usize)?;
    if *frag_idx as usize >= n {
        return None;
    }
    // Data chunks are 2 bytes smaller with FEC; irrelevant to the loss statistics.
    Some(MediaDatagram::Video {
        frame_id: *frame_id,
        frag_idx: *frag_idx,
        frag_count: n as u16,
        keyframe: *keyframe,
        fec_group: 0,
        timestamp: *timestamp,
        payload: payload.clone(),
    })
}

fn main() {
    let seconds: u64 = arg("--seconds", 5);
    let loss: f64 = arg("--loss", 2.0);
    let fec: u8 = arg("--fec", 8);
    let window: String = arg("--window", String::new());
    let preset = match arg("--preset", "1080p30".to_string()).as_str() {
        "720p30" => SharePreset::P720p30,
        "1080p60" => SharePreset::P1080p60,
        "text" => SharePreset::SharpText,
        _ => SharePreset::P1080p30,
    };
    let codec = match arg("--codec", "h264".to_string()).as_str() {
        "hevc" => sala_protocol::VideoCodec::Hevc,
        "av1" => sala_protocol::VideoCodec::Av1,
        _ => sala_protocol::VideoCodec::H264,
    };

    let monitors = list_monitors().expect("list monitors");
    let windows = list_windows().expect("list windows");
    let (target, tw, th) = if window.is_empty() {
        let m = monitors.first().expect("no monitor");
        (m.target, m.width, m.height)
    } else {
        let w = windows.iter().find(|w| w.name.contains(&window)).expect("window not found");
        (w.target, w.width, w.height)
    };

    let slot = FrameSlot::new();
    let mut cfg = SenderConfig::new(preset, codec, tw, th);
    cfg.fec_group = fec;
    let (sender, rx) = Sender::start(cfg, slot.clone()).expect("open sender");
    let ec = sender.encoder_config().clone();
    println!(
        "{preset:?}: {} {}x{} -> {}x{} {} fps, start {} kbps, FEC 1/{fec}",
        sender.encoder_name(),
        tw,
        th,
        ec.width,
        ec.height,
        ec.fps,
        ec.bitrate_kbps
    );

    let capture_slot = slot.clone();
    let session = CaptureSession::start(target, preset.fps(), move |f| {
        capture_slot.publish(RawFrame { width: f.width, height: f.height, data: f.data.to_vec() });
    })
    .expect("start capture");

    let t0 = Instant::now();
    let (mut with_fec, mut plain) = (Reassembler::new(), Reassembler::new());
    let (mut datagrams, mut dropped, mut kf_requests) = (0u64, 0u64, 0u32);
    let (mut keys_fec, mut keys_plain, mut frames_fec, mut frames_plain) = (0u32, 0u32, 0u32, 0u32);
    let (mut win_total, mut win_lost, mut last_report) = (0u64, 0u64, Instant::now());
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut sent_bytes = 0u64;
    while t0.elapsed() < Duration::from_secs(seconds) {
        if last_report.elapsed() >= Duration::from_secs(1) {
            let pct = if win_total == 0 { 0.0 } else { win_lost as f32 * 100.0 / win_total as f32 };
            let t = sender.report(ReceiverReport { loss_pct: pct, dropped_frames: 0, rtt_ms: Some(30) });
            println!("  t={:>2}s loss {pct:.1}% -> target {t} kbps", t0.elapsed().as_secs());
            (win_total, win_lost, last_report) = (0, 0, Instant::now());
        }
        let Ok(dgs) = rx.recv_timeout(Duration::from_millis(50)) else { continue };
        for dg in dgs {
            datagrams += 1;
            win_total += 1;
            if let MediaDatagram::Video { payload, .. } = &dg {
                sent_bytes += payload.len() as u64;
            }
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            if (rng % 10_000) as f64 / 10_000.0 < loss / 100.0 {
                dropped += 1;
                win_lost += 1;
                continue;
            }
            let now = t0.elapsed().as_millis() as u64;
            let out = with_fec.push(now, &dg);
            if let Some(f) = out.frame {
                frames_fec += 1;
                keys_fec += u32::from(f.keyframe);
            }
            if out.request_keyframe {
                kf_requests += 1;
                sender.request_keyframe();
            }
            if let Some(plain_dg) = without_fec(&dg)
                && let Some(f) = plain.push(now, &plain_dg).frame
            {
                frames_plain += 1;
                keys_plain += u32::from(f.keyframe);
            }
        }
    }
    let _ = session.stop();
    let secs = t0.elapsed().as_secs_f64();
    let st = sender.stats();
    let target_kbps = sender.target_kbps();
    sender.stop();
    println!("--- {secs:.1} s, simulated loss {loss}% ---");
    println!(
        "encoded: {} frames ({:.1} fps), avg encode call {:.2} ms, reopens {}, dropped deltas {}",
        st.frames,
        st.frames as f64 / secs,
        st.encode_micros as f64 / 1000.0 / st.frames.max(1) as f64,
        st.reopens,
        st.dropped_deltas
    );
    println!(
        "bitrate: {:.0} kbps on the wire (payload), final target {target_kbps} kbps",
        sent_bytes as f64 * 8.0 / secs / 1000.0
    );
    println!("datagrams: {datagrams}, dropped on purpose: {dropped}");
    println!("keyframes sent: {} (receiver asked {kf_requests} times)", st.keyframes);
    println!("keyframes surviving the loss: with FEC {keys_fec}, without FEC {keys_plain}");
    println!("frames reassembled: with FEC {frames_fec}, without FEC {frames_plain}");
}
