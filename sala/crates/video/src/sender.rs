//! Asynchronous send pipeline: capture -> "latest frame" slot -> encode thread -> datagrams.
//!
//! The capture side only calls [`FrameSlot::publish`], which overwrites the previous frame
//! (live video never queues). The encode thread wakes at the preset's frame interval, encodes
//! the newest frame, repeats the last one when the screen is static (so the stream keeps a
//! minimum rate), reacts to keyframe requests and receiver reports, and hands fragmented
//! datagrams to a bounded channel. Deltas are dropped when the channel is full; keyframes
//! never are. No network, no clock injection needed beyond what tests can control through
//! [`SenderConfig::idle_resend`].

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sala_protocol::{MediaDatagram, VideoCodec, max_video_payload};

use crate::abr::{Abr, ReceiverReport};
use crate::encoder::{EncodeError, EncoderConfig, TIME_BASE_HZ, VideoEncoder};
use crate::fragment::{DEFAULT_FEC_GROUP, fragment_fec};
use crate::preset::SharePreset;

/// One captured BGRA frame (`width * height * 4` bytes, no row padding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Default)]
struct SlotState {
    frame: Option<RawFrame>,
    woken: bool,
    closed: bool,
}

/// Single-slot mailbox between the capture callback and the encode thread. Clone it to give
/// the capture side its handle.
#[derive(Clone, Default)]
pub struct FrameSlot(Arc<(Mutex<SlotState>, Condvar)>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FrameSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `frame`, replacing a frame the encoder has not taken yet.
    pub fn publish(&self, frame: RawFrame) {
        lock(&self.0.0).frame = Some(frame);
    }

    /// Takes the newest unconsumed frame, if any.
    pub fn take(&self) -> Option<RawFrame> {
        lock(&self.0.0).frame.take()
    }

    fn wake(&self) {
        lock(&self.0.0).woken = true;
        self.0.1.notify_all();
    }

    fn close(&self) {
        lock(&self.0.0).closed = true;
        self.0.1.notify_all();
    }

    /// Sleeps until `deadline`, a wake-up or close; returns the newest frame and whether the
    /// slot was closed.
    fn wait(&self, deadline: Instant) -> (Option<RawFrame>, bool) {
        let (m, cv) = &*self.0;
        let mut s = lock(m);
        while !s.closed && !s.woken {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            s = cv.wait_timeout(s, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
        s.woken = false;
        (s.frame.take(), s.closed)
    }
}

#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub preset: SharePreset,
    pub codec: VideoCodec,
    /// Size of the captured source (the encoder scales to the preset).
    pub src_width: u32,
    pub src_height: u32,
    /// Largest encoded datagram (QUIC datagram budget).
    pub max_datagram: usize,
    /// XOR FEC group size for keyframes (0 = off).
    pub fec_group: u8,
    /// Repeat the last frame after this long without a new one.
    pub idle_resend: Duration,
    /// Encoded frames the output channel holds before deltas are dropped.
    pub channel_frames: usize,
}

impl SenderConfig {
    pub fn new(preset: SharePreset, codec: VideoCodec, src_width: u32, src_height: u32) -> Self {
        SenderConfig {
            preset,
            codec,
            src_width,
            src_height,
            max_datagram: 1200,
            fec_group: DEFAULT_FEC_GROUP,
            idle_resend: Duration::from_secs(1),
            channel_frames: 16,
        }
    }
}

/// Counters since the sender started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SenderStats {
    pub frames: u64,
    pub keyframes: u64,
    /// Delta frames dropped because the output channel was full.
    pub dropped_deltas: u64,
    pub bytes: u64,
    /// Times the encoder was reopened for a new bitrate.
    pub reopens: u64,
    pub encode_micros: u64,
    /// The encode thread stopped on an encoder error.
    pub failed: bool,
}

#[derive(Default)]
struct Shared {
    key_requested: AtomicBool,
    target_kbps: AtomicU32,
    frames: AtomicU64,
    keyframes: AtomicU64,
    dropped_deltas: AtomicU64,
    bytes: AtomicU64,
    reopens: AtomicU64,
    encode_micros: AtomicU64,
    failed: AtomicBool,
}

pub struct Sender {
    shared: Arc<Shared>,
    slot: FrameSlot,
    abr: Mutex<Abr>,
    started: Instant,
    encoder_name: String,
    config: EncoderConfig,
    handle: Option<JoinHandle<()>>,
}

/// Whether this tick should produce a frame.
fn should_encode(has_fresh: bool, has_last: bool, key_wanted: bool, since_last: Option<Duration>, idle: Duration) -> bool {
    has_last && (has_fresh || key_wanted || since_last.is_none_or(|d| d >= idle))
}

impl Sender {
    /// Opens the encoder and starts the encode thread. The returned receiver yields one
    /// `Vec<MediaDatagram>` per encoded frame (all fragments, FEC included); the first
    /// datagram's `keyframe` flag tells what kind of frame it is.
    pub fn start(cfg: SenderConfig, slot: FrameSlot) -> Result<(Sender, Receiver<Vec<MediaDatagram>>), EncodeError> {
        let range = cfg.preset.bitrate(cfg.codec);
        let ecfg = cfg.preset.encoder_config(cfg.codec, cfg.src_width, cfg.src_height);
        let enc = VideoEncoder::open(&ecfg)?;
        let shared = Arc::new(Shared::default());
        let abr = Abr::new(range);
        shared.target_kbps.store(abr.target_kbps(), Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(cfg.channel_frames.max(1));
        let started = Instant::now();
        let encoder_name = enc.name().to_string();
        let handle = {
            let (shared, slot) = (shared.clone(), slot.clone());
            std::thread::Builder::new()
                .name("video-encode".into())
                .spawn(move || run(enc, &cfg, &slot, &shared, &tx, started))
                .map_err(|e| EncodeError::Ffmpeg(e.to_string()))?
        };
        Ok((Sender { shared, slot, abr: Mutex::new(abr), started, encoder_name, config: ecfg, handle: Some(handle) }, rx))
    }

    /// The next frame will be a keyframe (answer to a `KeyframeRequest`).
    pub fn request_keyframe(&self) {
        self.shared.key_requested.store(true, Ordering::Relaxed);
        self.slot.wake();
    }

    /// Feeds a receiver report to the adaptive bitrate; returns the new target in kbps.
    pub fn report(&self, report: ReceiverReport) -> u32 {
        let now = self.started.elapsed().as_millis() as u64;
        let target = lock(&self.abr).on_report(now, &report);
        self.shared.target_kbps.store(target, Ordering::Relaxed);
        target
    }

    pub fn target_kbps(&self) -> u32 {
        self.shared.target_kbps.load(Ordering::Relaxed)
    }

    /// Encoder settings the stream started with (size, fps, codec).
    pub fn encoder_config(&self) -> &EncoderConfig {
        &self.config
    }

    /// FFmpeg encoder in use (`h264_qsv`).
    pub fn encoder_name(&self) -> &str {
        &self.encoder_name
    }

    pub fn stats(&self) -> SenderStats {
        let s = &self.shared;
        SenderStats {
            frames: s.frames.load(Ordering::Relaxed),
            keyframes: s.keyframes.load(Ordering::Relaxed),
            dropped_deltas: s.dropped_deltas.load(Ordering::Relaxed),
            bytes: s.bytes.load(Ordering::Relaxed),
            reopens: s.reopens.load(Ordering::Relaxed),
            encode_micros: s.encode_micros.load(Ordering::Relaxed),
            failed: s.failed.load(Ordering::Relaxed),
        }
    }

    /// Stops the encode thread and waits for it.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.slot.close();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(
    mut enc: VideoEncoder,
    cfg: &SenderConfig,
    slot: &FrameSlot,
    shared: &Shared,
    tx: &SyncSender<Vec<MediaDatagram>>,
    started: Instant,
) {
    let tick = Duration::from_micros(1_000_000 / u64::from(enc.config().fps.max(1)));
    let max_payload = max_video_payload(cfg.max_datagram);
    let mut next = Instant::now();
    let mut last: Option<RawFrame> = None;
    let mut last_encode: Option<Instant> = None;
    let mut frame_id = 0u32;
    // A frame arrived since the last encode (early wake-ups must not lose this fact).
    let mut has_fresh = false;
    loop {
        let (fresh, closed) = slot.wait(next);
        if closed {
            return;
        }
        let now = Instant::now();
        if fresh.is_some() {
            last = fresh;
            has_fresh = true;
        }
        let key_wanted = shared.key_requested.load(Ordering::Relaxed);
        let due = should_encode(has_fresh, last.is_some(), key_wanted, last_encode.map(|t| now - t), cfg.idle_resend);
        // A keyframe request wakes the loop early: encode right away, then restart the pacing.
        if now < next && !(key_wanted && due) {
            continue;
        }
        next = (next + tick).max(now);
        if !due {
            continue;
        }
        let Some(frame) = &last else { continue };
        if shared.key_requested.swap(false, Ordering::Relaxed) {
            enc.force_keyframe();
        }
        match enc.set_bitrate(shared.target_kbps.load(Ordering::Relaxed)) {
            Ok(true) => {
                shared.reopens.fetch_add(1, Ordering::Relaxed);
                tracing::info!(kbps = enc.config().bitrate_kbps, "video encoder reopened for a new bitrate");
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("cannot change the video bitrate: {e}"),
        }
        let pts = (started.elapsed().as_micros() * TIME_BASE_HZ as u128 / 1_000_000) as i64;
        let t0 = Instant::now();
        let packets = match enc.encode_bgra(&frame.data, frame.width, frame.height, pts) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("video encode failed, stopping the sender: {e}");
                shared.failed.store(true, Ordering::Relaxed);
                return;
            }
        };
        shared.encode_micros.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
        last_encode = Some(now);
        has_fresh = false;
        for p in packets {
            frame_id = frame_id.wrapping_add(1);
            let fec = if p.keyframe { cfg.fec_group } else { 0 };
            let dgs = fragment_fec(frame_id, p.keyframe, p.pts as u32, &p.data, max_payload, fec);
            if dgs.is_empty() {
                tracing::warn!(bytes = p.data.len(), "encoded frame cannot be fragmented, dropped");
                continue;
            }
            shared.frames.fetch_add(1, Ordering::Relaxed);
            shared.bytes.fetch_add(p.data.len() as u64, Ordering::Relaxed);
            if p.keyframe {
                shared.keyframes.fetch_add(1, Ordering::Relaxed);
            }
            match deliver(tx, dgs, p.keyframe, slot) {
                Delivery::Sent => {}
                Delivery::Gone => return,
                Delivery::Dropped => {
                    // Channel full: the picture is broken until the next keyframe.
                    shared.dropped_deltas.fetch_add(1, Ordering::Relaxed);
                    enc.force_keyframe();
                }
            }
        }
    }
}

enum Delivery {
    Sent,
    /// A delta did not fit the full channel.
    Dropped,
    /// The receiver is gone or the sender was stopped: end the thread.
    Gone,
}

/// Sends one frame; a delta is dropped when the channel is full, a keyframe waits for room
/// (or for the sender to be stopped).
fn deliver(tx: &SyncSender<Vec<MediaDatagram>>, dgs: Vec<MediaDatagram>, keyframe: bool, slot: &FrameSlot) -> Delivery {
    let mut pending = dgs;
    loop {
        match tx.try_send(pending) {
            Ok(()) => return Delivery::Sent,
            Err(TrySendError::Disconnected(_)) => return Delivery::Gone,
            Err(TrySendError::Full(back)) => {
                if !keyframe {
                    return Delivery::Dropped;
                }
                if lock(&slot.0.0).closed {
                    return Delivery::Gone;
                }
                pending = back;
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::Reassembler;

    fn frame(w: u32, h: u32, shade: u8) -> RawFrame {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for (n, px) in data.chunks_mut(4).enumerate() {
            px.copy_from_slice(&[(n as u32 % w) as u8 ^ shade, (n as u32 / w) as u8, shade, 255]);
        }
        RawFrame { width: w, height: h, data }
    }

    #[test]
    fn slot_keeps_only_the_latest_frame() {
        let slot = FrameSlot::new();
        assert!(slot.take().is_none());
        for shade in 0..5 {
            slot.publish(frame(4, 2, shade));
        }
        assert_eq!(slot.take().unwrap(), frame(4, 2, 4));
        assert!(slot.take().is_none());
    }

    #[test]
    fn pacing_decision() {
        let idle = Duration::from_secs(1);
        let ms = |n| Some(Duration::from_millis(n));
        assert!(!should_encode(true, false, true, None, idle)); // nothing captured yet
        assert!(should_encode(true, true, false, ms(10), idle)); // new frame
        assert!(!should_encode(false, true, false, ms(999), idle)); // static screen, not yet
        assert!(should_encode(false, true, false, ms(1000), idle)); // static screen, resend
        assert!(should_encode(false, true, true, ms(10), idle)); // keyframe asked
        assert!(should_encode(false, true, false, None, idle)); // never encoded yet
    }

    #[test]
    fn wait_returns_at_the_deadline_and_wakes_early() {
        let slot = FrameSlot::new();
        let t0 = Instant::now();
        let (f, closed) = slot.wait(t0 + Duration::from_millis(60));
        assert!(f.is_none() && !closed);
        assert!(t0.elapsed() >= Duration::from_millis(55));
        let s2 = slot.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            s2.close();
        });
        let t0 = Instant::now();
        let (_, closed) = slot.wait(t0 + Duration::from_secs(10));
        assert!(closed && t0.elapsed() < Duration::from_secs(2));
        h.join().unwrap();
    }

    struct Run {
        frames: Vec<(bool, u32)>, // (keyframe, frame_id) as reassembled
    }

    /// Drives a real `Sender` with a synthetic source; returns what the receiver got.
    fn drive(
        idle: Duration,
        publish_for: Duration,
        total: Duration,
        ask_key_at: Option<Duration>,
        capacity: usize,
        drain: bool,
    ) -> Option<(Run, SenderStats)> {
        let slot = FrameSlot::new();
        let mut cfg = SenderConfig::new(SharePreset::P720p30, VideoCodec::H264, 640, 360);
        cfg.idle_resend = idle;
        cfg.channel_frames = capacity;
        let (sender, rx) = Sender::start(cfg, slot.clone()).ok()?;
        let src = {
            let slot = slot.clone();
            std::thread::spawn(move || {
                let t0 = Instant::now();
                let mut i = 0u8;
                while t0.elapsed() < publish_for {
                    slot.publish(frame(640, 360, i));
                    i = i.wrapping_add(9);
                    std::thread::sleep(Duration::from_millis(15));
                }
            })
        };
        let t0 = Instant::now();
        let mut reasm = Reassembler::new();
        let mut run = Run { frames: Vec::new() };
        let mut asked = false;
        while t0.elapsed() < total {
            if let Some(at) = ask_key_at
                && !asked
                && t0.elapsed() >= at
            {
                sender.request_keyframe();
                asked = true;
            }
            if !drain {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            if let Ok(dgs) = rx.recv_timeout(Duration::from_millis(20)) {
                for d in &dgs {
                    if let Some(f) = reasm.push(t0.elapsed().as_millis() as u64, d).frame {
                        run.frames.push((f.keyframe, f.frame_id));
                    }
                }
            }
        }
        src.join().unwrap();
        let stats = sender.stats();
        sender.stop();
        Some((run, stats))
    }

    /// Hardware-dependent: skipped (passes) when this machine has no encoder.
    #[test]
    fn streams_frames_and_answers_keyframe_requests() {
        let Some((run, stats)) =
            drive(Duration::from_secs(1), Duration::from_millis(1500), Duration::from_millis(1800), Some(Duration::from_millis(800)), 16, true)
        else {
            return;
        };
        assert!(!stats.failed);
        assert!(run.frames.len() >= 25, "only {} frames", run.frames.len());
        assert!(run.frames[0].0, "stream must open with a keyframe");
        let keys = run.frames.iter().filter(|f| f.0).count();
        assert!(keys >= 2, "requested keyframe never arrived ({keys})");
        // Ids are consecutive and nothing was lost on the lossless channel.
        for w in run.frames.windows(2) {
            assert_eq!(w[1].1, w[0].1.wrapping_add(1));
        }
    }

    /// Hardware-dependent. A static screen (no new frames) is still sent at the idle rate.
    #[test]
    fn static_screen_is_resent() {
        let Some((run, _)) =
            drive(Duration::from_millis(100), Duration::from_millis(60), Duration::from_millis(1200), None, 16, true)
        else {
            return;
        };
        // ~4 live frames, then roughly one repeat every 100 ms for the rest of the second.
        assert!(run.frames.len() >= 8, "only {} frames on a static screen", run.frames.len());
        assert!(run.frames.len() <= 30, "{} frames: static screen sent too fast", run.frames.len());
    }

    /// Hardware-dependent. Nobody reads the channel: deltas are dropped, the thread never
    /// blocks on them, and stop() still returns.
    #[test]
    fn full_channel_drops_deltas_and_stop_returns() {
        let t0 = Instant::now();
        let Some((_, stats)) =
            drive(Duration::from_secs(1), Duration::from_millis(1200), Duration::from_millis(1300), None, 2, false)
        else {
            return;
        };
        assert!(stats.dropped_deltas > 0, "nothing dropped: {stats:?}");
        assert!(stats.keyframes >= 1);
        assert!(t0.elapsed() < Duration::from_secs(10));
    }
}
