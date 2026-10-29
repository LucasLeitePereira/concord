//! Screen share glue between `sala-video` and the session (`screen-share`, `receiver-report`).
//!
//! Sender side ([`ShareOut`]): owns a [`Pipe`] (encoder + capture behind the [`VideoPipe`]
//! trait), announces `ShareStart`/`ShareStop`, and runs one thread that drains the encoder's
//! datagram channel into the network. Receiver side ([`ShareIn`]): one per sharing friend, feeds
//! the `Reassembler`, measures fragment loss ([`LossMeter`]) and builds the periodic
//! `ReceiverReport`. The real pipe ([`screen_pipe`]) needs WGC and a hardware encoder; tests
//! plug a synthetic one in through [`PipeFactory`].

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use sala_protocol::{ControlMessage, MediaDatagram, VIDEO_PROTOCOL_VERSION, VideoCodec};
use sala_video::abr::ReceiverReport as AbrReport;
use sala_video::capture::{CaptureSession, Source, Target};
use sala_video::fragment::{Frame, Output, Reassembler};
use sala_video::preset::SharePreset;
use sala_video::sender::{FrameSlot, RawFrame, Sender, SenderConfig};
use tokio::sync::mpsc;

use crate::net::Net;

/// Spacing of the `ReceiverReport`s.
pub const REPORT_INTERVAL_MS: u64 = 1_000;

/// At most this many whole frames are counted as lost from one id gap (a long outage must not
/// inflate the window).
const MAX_GAP_FRAMES: u32 = 64;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether a peer that announced `protocol_version` takes part in screen sharing (D-024 gate).
pub fn peer_supports_video(protocol_version: u16) -> bool {
    protocol_version >= VIDEO_PROTOCOL_VERSION
}

/// What the factory needs to build a pipe.
#[derive(Debug, Clone, Copy)]
pub struct ShareParams {
    pub preset: SharePreset,
    /// Smallest datagram budget among the viewers (the real QUIC limit, not a constant).
    pub max_datagram: usize,
}

/// Control surface of a running encoder. Dropping it must stop everything it started.
pub trait VideoPipe: Send + Sync {
    /// The next frame will be a keyframe.
    fn request_keyframe(&self);
    /// Applies a viewer report to the adaptive bitrate; returns the new target in kbps.
    fn report(&self, report: AbrReport) -> u32;
    fn target_kbps(&self) -> u32;
}

/// A running video source: its control plus the channel of fragmented frames.
pub struct Pipe {
    pub control: Box<dyn VideoPipe>,
    pub datagrams: Receiver<Vec<MediaDatagram>>,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

pub type PipeFactory = Box<dyn FnOnce(&ShareParams) -> Result<Pipe, String> + Send>;

/// An encoded frame received from a friend (input of the future viewer, T-033d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub friend: String,
    pub codec: VideoCodec,
    pub frame: Frame,
}

/// Counters of the sender side, for the UI and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareStats {
    /// The drain thread is alive (it ends when the encoder stops).
    pub running: bool,
    pub target_kbps: u32,
    pub datagrams_sent: u64,
    pub send_errors: u64,
    pub reports: u64,
    pub viewers: usize,
}

// ---------------------------------------------------------------- loss measurement

/// Counts video fragments received against the ones the sender produced. The "expected" side
/// comes from the frame ids: a new frame adds its `frag_count`, a gap of N ids adds N frames of
/// the last known size.
#[derive(Debug, Default)]
pub struct LossMeter {
    highest: Option<u32>,
    last_count: u32,
    received: u32,
    expected: u32,
    dropped: u32,
}

impl LossMeter {
    pub fn on_fragment(&mut self, frame_id: u32, frag_count: u16) {
        self.received = self.received.saturating_add(1);
        let count = u32::from(frag_count);
        match self.highest {
            None => {
                self.expected += count;
                self.highest = Some(frame_id);
                self.last_count = count;
            }
            Some(h) => {
                let gap = frame_id.wrapping_sub(h) as i32;
                if gap > 0 {
                    let missing = (gap as u32 - 1).min(MAX_GAP_FRAMES);
                    self.expected = self.expected.saturating_add(missing * self.last_count + count);
                    self.highest = Some(frame_id);
                    self.last_count = count;
                }
            }
        }
    }

    /// A frame was discarded (incomplete or undecodable).
    pub fn on_dropped_frame(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }

    /// Closes the window: `None` when the sender produced nothing in it.
    pub fn take_report(&mut self, window_ms: u64) -> Option<ControlMessage> {
        let report = (self.expected > 0).then(|| ControlMessage::ReceiverReport {
            received: self.received.min(self.expected),
            expected: self.expected,
            dropped_frames: self.dropped,
            window_ms: window_ms.min(u64::from(u32::MAX)) as u32,
        });
        self.received = 0;
        self.expected = 0;
        self.dropped = 0;
        report
    }
}

/// Turns the wire report into the input of the adaptive bitrate.
pub fn abr_report(received: u32, expected: u32, dropped_frames: u32, rtt_ms: Option<u32>) -> AbrReport {
    let loss_pct = if expected == 0 {
        0.0
    } else {
        expected.saturating_sub(received) as f32 * 100.0 / expected as f32
    };
    AbrReport { loss_pct, dropped_frames, rtt_ms }
}

// ---------------------------------------------------------------- receiver side

/// Receive state of one friend's share.
pub struct ShareIn {
    reasm: Reassembler,
    meter: LossMeter,
    window_start_ms: u64,
    codec: VideoCodec,
}

impl ShareIn {
    /// `codec` is the one announced by `ShareStart`; it travels with every frame to the viewer.
    pub fn new(now_ms: u64, codec: VideoCodec) -> Self {
        ShareIn { reasm: Reassembler::new(), meter: LossMeter::default(), window_start_ms: now_ms, codec }
    }

    pub fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn note(&mut self, out: &Output) {
        if out.request_keyframe {
            self.meter.on_dropped_frame();
        }
    }

    pub fn on_datagram(&mut self, now_ms: u64, dg: &MediaDatagram) -> Output {
        if let MediaDatagram::Video { frame_id, frag_count, .. } = dg {
            self.meter.on_fragment(*frame_id, *frag_count);
        }
        let out = self.reasm.push(now_ms, dg);
        self.note(&out);
        out
    }

    /// Expires stalled frames and, once per [`REPORT_INTERVAL_MS`], returns the report to send.
    pub fn tick(&mut self, now_ms: u64) -> (Output, Option<ControlMessage>) {
        let out = self.reasm.expire(now_ms);
        self.note(&out);
        let window = now_ms.saturating_sub(self.window_start_ms);
        let report = if window >= REPORT_INTERVAL_MS {
            self.window_start_ms = now_ms;
            self.meter.take_report(window)
        } else {
            None
        };
        (out, report)
    }
}

/// Delivers what the reassembler produced: the frame to the viewer channel (dropped when it is
/// full: live video never queues) and the keyframe request to the sender.
pub fn handle_output(out: Output, friend: &str, codec: VideoCodec, net: &Net, frames: &mpsc::Sender<VideoFrame>) {
    if let Some(frame) = out.frame {
        let _ = frames.try_send(VideoFrame { friend: friend.to_string(), codec, frame });
    }
    if out.request_keyframe
        && let Err(e) = net.send_control(friend, ControlMessage::KeyframeRequest)
    {
        tracing::debug!(%friend, "cannot ask for a keyframe: {e}");
    }
}

// ---------------------------------------------------------------- sender side

/// State shared between the loop, the drain thread and `Session::share_stats`.
pub struct ShareShared {
    pipe: Box<dyn VideoPipe>,
    recipients: Mutex<HashSet<String>>,
    stop: AtomicBool,
    running: AtomicBool,
    sent: AtomicU64,
    send_errors: AtomicU64,
    reports: AtomicU64,
}

impl ShareShared {
    pub fn stats(&self) -> ShareStats {
        ShareStats {
            running: self.running.load(Ordering::Relaxed),
            target_kbps: self.pipe.target_kbps(),
            datagrams_sent: self.sent.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            reports: self.reports.load(Ordering::Relaxed),
            viewers: lock(&self.recipients).len(),
        }
    }
}

/// A share in progress, owned by the session loop.
pub struct ShareOut {
    shared: Arc<ShareShared>,
    thread: Option<JoinHandle<()>>,
    start_msg: ControlMessage,
}

impl ShareOut {
    /// Announces the share to `peers` (already filtered by [`peer_supports_video`]) and starts
    /// draining `pipe` into the network.
    pub fn start(net: &Net, pipe: Pipe, peers: &[String]) -> ShareOut {
        let Pipe { control, datagrams, codec, width, height, fps } = pipe;
        let start_msg = ControlMessage::ShareStart { codec, width, height, fps };
        let shared = Arc::new(ShareShared {
            pipe: control,
            recipients: Mutex::new(HashSet::new()),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(true),
            sent: AtomicU64::new(0),
            send_errors: AtomicU64::new(0),
            reports: AtomicU64::new(0),
        });
        let out = ShareOut { shared: shared.clone(), thread: None, start_msg };
        for p in peers {
            out.add_peer(net, p);
        }
        let net = net.clone();
        let thread = std::thread::Builder::new()
            .name("share-send".into())
            .spawn(move || drain(&shared, &datagrams, &net))
            .map_err(|e| tracing::error!("cannot start the share sender thread: {e}"))
            .ok();
        if thread.is_none() {
            out.shared.running.store(false, Ordering::Relaxed);
        }
        ShareOut { thread, ..out }
    }

    pub fn shared(&self) -> Arc<ShareShared> {
        self.shared.clone()
    }

    /// Adds a viewer: `ShareStart` plus a keyframe so that it can join mid-stream. `false` when
    /// it was already a viewer or the control message could not be sent.
    pub fn add_peer(&self, net: &Net, friend: &str) -> bool {
        if !lock(&self.shared.recipients).insert(friend.to_string()) {
            return false;
        }
        if let Err(e) = net.send_control(friend, self.start_msg.clone()) {
            tracing::warn!(%friend, "cannot announce the share: {e}");
            lock(&self.shared.recipients).remove(friend);
            return false;
        }
        self.shared.pipe.request_keyframe();
        tracing::info!(%friend, "share: viewer added");
        true
    }

    pub fn remove_peer(&self, friend: &str) {
        lock(&self.shared.recipients).remove(friend);
    }

    pub fn request_keyframe(&self) {
        self.shared.pipe.request_keyframe();
    }

    /// A viewer's report reaches the adaptive bitrate; returns the new target in kbps.
    pub fn on_report(&self, report: AbrReport) -> u32 {
        self.shared.reports.fetch_add(1, Ordering::Relaxed);
        self.shared.pipe.report(report)
    }

    /// Tells the viewers, stops the drain thread and waits for it. The pipe is released when the
    /// last `Arc<ShareShared>` goes away.
    pub fn stop(mut self, net: &Net) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let viewers: Vec<String> = lock(&self.shared.recipients).drain().collect();
        for v in viewers {
            if let Err(e) = net.send_control(&v, ControlMessage::ShareStop) {
                tracing::debug!(friend = %v, "cannot announce the end of the share: {e}");
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Sends every fragmented frame to every viewer until stopped or the encoder ends.
fn drain(shared: &ShareShared, rx: &Receiver<Vec<MediaDatagram>>, net: &Net) {
    while !shared.stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(dgs) => {
                let viewers: Vec<String> = lock(&shared.recipients).iter().cloned().collect();
                for dg in &dgs {
                    for v in &viewers {
                        // `send_datagram` refuses anything above this connection's real limit.
                        match net.send_datagram(v, dg) {
                            Ok(()) => shared.sent.fetch_add(1, Ordering::Relaxed),
                            Err(_) => shared.send_errors.fetch_add(1, Ordering::Relaxed),
                        };
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    shared.running.store(false, Ordering::Relaxed);
}

// ---------------------------------------------------------------- real source

struct ScreenPipe {
    // Capture first: it stops feeding frames before the sender goes away.
    capture: Mutex<Option<CaptureSession>>,
    sender: Sender,
}

impl VideoPipe for ScreenPipe {
    fn request_keyframe(&self) {
        self.sender.request_keyframe();
    }

    fn report(&self, report: AbrReport) -> u32 {
        self.sender.report(report)
    }

    fn target_kbps(&self) -> u32 {
        self.sender.target_kbps()
    }
}

impl Drop for ScreenPipe {
    fn drop(&mut self) {
        if let Some(c) = lock(&self.capture).take()
            && let Err(e) = c.stop()
        {
            tracing::warn!("cannot stop the screen capture: {e}");
        }
    }
}

/// Builds the real pipe: WGC capture of `source` -> latest-frame slot -> hardware encoder.
pub fn screen_pipe(params: &ShareParams, codec: VideoCodec, source: &Source) -> Result<Pipe, String> {
    let slot = FrameSlot::new();
    let mut cfg = SenderConfig::new(params.preset, codec, source.width, source.height);
    cfg.max_datagram = params.max_datagram;
    let (sender, datagrams) = Sender::start(cfg, slot.clone()).map_err(|e| format!("cannot open the encoder: {e}"))?;
    let ec = sender.encoder_config().clone();
    tracing::info!(encoder = sender.encoder_name(), width = ec.width, height = ec.height, fps = ec.fps, "share encoder ready");
    let capture = CaptureSession::start(source.target, params.preset.fps(), move |f| {
        slot.publish(RawFrame { width: f.width, height: f.height, data: f.data.to_vec() });
    })
    .map_err(|e| format!("cannot capture the screen: {e}"))?;
    let (width, height, fps) = (ec.width, ec.height, ec.fps);
    let control = Box::new(ScreenPipe { capture: Mutex::new(Some(capture)), sender });
    Ok(Pipe { control, datagrams, codec, width, height, fps })
}

// ---------------------------------------------------------------- UI-facing planning

/// Id of a source in the IPC (`ShareSource.id`).
pub fn source_id(target: Target) -> String {
    match target {
        Target::Monitor(h) => format!("monitor:{h}"),
        Target::Window(h) => format!("window:{h}"),
    }
}

/// Inverse of [`source_id`].
pub fn parse_source_id(id: &str) -> Option<Target> {
    let (kind, handle) = id.split_once(':')?;
    let handle: usize = handle.parse().ok()?;
    match kind {
        "monitor" => Some(Target::Monitor(handle)),
        "window" => Some(Target::Window(handle)),
        _ => None,
    }
}

/// Codec to share with. The friends' decoders are not known here, so H.264 (decodable
/// everywhere, in hardware or software) wins when this PC can encode it.
pub fn pick_codec(caps: &sala_protocol::Capabilities) -> Option<VideoCodec> {
    if caps.hw_encode.contains(&VideoCodec::H264) { Some(VideoCodec::H264) } else { caps.hw_encode.first().copied() }
}

/// `"nvenc"` / `"amf"` / `"qsv"`: the first backend that opened for `codec`.
pub fn encoder_backend(report: &sala_video::ProbeReport, codec: VideoCodec) -> Option<String> {
    let a = report.encoders.iter().find(|a| a.ok && a.codec == codec)?;
    a.name.split_once('_').map(|(_, backend)| backend.to_string())
}

/// What the "choose what to share" dialog shows. Blocking (probe cache and window enumeration).
pub fn list_share_sources() -> (Option<String>, Vec<sala_protocol::ShareSource>, Vec<sala_protocol::ShareSource>) {
    let report = sala_video::report();
    let encoder = pick_codec(&report.capabilities).and_then(|c| encoder_backend(report, c));
    let convert = |list: Result<Vec<Source>, sala_video::capture::CaptureError>| -> Vec<sala_protocol::ShareSource> {
        list.unwrap_or_else(|e| {
            tracing::warn!("cannot list the capture sources: {e}");
            Vec::new()
        })
        .into_iter()
        .map(|s| sala_protocol::ShareSource {
            id: source_id(s.target),
            name: s.name,
            width: s.width,
            height: s.height,
            primary: s.primary,
        })
        .collect()
    };
    (encoder, convert(sala_video::capture::list_monitors()), convert(sala_video::capture::list_windows()))
}

/// A share ready to start: the preset, the factory that opens capture and encoder, and the
/// height the viewers will really get (the requested one capped at the source's).
pub struct ShareLaunch {
    pub preset: SharePreset,
    pub factory: PipeFactory,
    pub resolution: u32,
}

/// Turns a UI request into a [`ShareLaunch`]; injectable in the `Controller` (tests use a
/// synthetic source).
pub type SharePlanner = Box<dyn Fn(&str, u32, u32) -> Result<ShareLaunch, String> + Send>;

/// The real planner: finds the source again (handles are stable, the list is not), picks the
/// codec and builds the screen pipe.
pub fn real_share_planner() -> SharePlanner {
    Box::new(|source_id_str, resolution, fps| {
        let preset = SharePreset::custom(resolution, fps).ok_or_else(|| format!("invalid quality: {resolution}p{fps}"))?;
        let target = parse_source_id(source_id_str).ok_or_else(|| format!("invalid source {source_id_str:?}"))?;
        let list = match target {
            Target::Monitor(_) => sala_video::capture::list_monitors(),
            Target::Window(_) => sala_video::capture::list_windows(),
        }
        .map_err(|e| format!("cannot list the sources: {e}"))?;
        let source = list.into_iter().find(|s| s.target == target).ok_or("the source is gone")?;
        let caps = sala_video::capabilities_if_ready().ok_or("still probing the video hardware, try again")?;
        let codec = pick_codec(&caps).ok_or("this PC has no hardware video encoder")?;
        let height = if source.height == 0 { resolution } else { preset.output_size(source.width, source.height).1 };
        Ok(ShareLaunch {
            preset,
            factory: Box::new(move |params| screen_pipe(params, codec, &source)),
            resolution: height,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ids_round_trip_and_reject_garbage() {
        for t in [Target::Monitor(65537), Target::Window(394), Target::Window(0)] {
            assert_eq!(parse_source_id(&source_id(t)), Some(t));
        }
        assert_eq!(source_id(Target::Monitor(7)), "monitor:7");
        for bad in ["", "monitor", "monitor:", "monitor:x", "screen:1", ":1", "window:-1"] {
            assert_eq!(parse_source_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn codec_and_backend_choice() {
        use sala_protocol::Capabilities;
        let caps = |hw_encode: Vec<VideoCodec>| Capabilities { hw_encode, hw_decode: vec![] };
        assert_eq!(pick_codec(&caps(vec![])), None);
        assert_eq!(pick_codec(&caps(vec![VideoCodec::Av1, VideoCodec::H264])), Some(VideoCodec::H264));
        assert_eq!(pick_codec(&caps(vec![VideoCodec::Hevc])), Some(VideoCodec::Hevc));

        let attempt = |codec, name: &str, ok| sala_video::Attempt { codec, name: name.into(), ok, detail: String::new() };
        let report = sala_video::ProbeReport {
            encoders: vec![
                attempt(VideoCodec::H264, "h264_nvenc", false),
                attempt(VideoCodec::H264, "h264_amf", false),
                attempt(VideoCodec::H264, "h264_qsv", true),
            ],
            ..Default::default()
        };
        assert_eq!(encoder_backend(&report, VideoCodec::H264).as_deref(), Some("qsv"));
        assert_eq!(encoder_backend(&report, VideoCodec::Av1), None);
    }

    #[test]
    fn loss_meter_counts_fragments_and_gaps() {
        let mut m = LossMeter::default();
        // Frame 1: 4 fragments, all arrive. Frame 2: lost entirely. Frame 3: 4 fragments, 3 arrive.
        for _ in 0..4 {
            m.on_fragment(1, 4);
        }
        for _ in 0..3 {
            m.on_fragment(3, 4);
        }
        let Some(ControlMessage::ReceiverReport { received, expected, .. }) = m.take_report(1000) else {
            panic!("report expected")
        };
        assert_eq!((received, expected), (7, 12));
        // The window restarts; with nothing new there is nothing to report.
        assert_eq!(m.take_report(1000), None);
    }

    #[test]
    fn loss_meter_ignores_reordering_and_duplicates_in_expected() {
        let mut m = LossMeter::default();
        m.on_fragment(10, 2);
        m.on_fragment(12, 2); // 11 missing
        m.on_fragment(11, 2); // arrives late: received, but expected was already counted
        m.on_fragment(11, 2); // duplicate
        let Some(ControlMessage::ReceiverReport { received, expected, .. }) = m.take_report(500) else {
            panic!("report expected")
        };
        assert_eq!(expected, 6);
        assert_eq!(received, 4);
    }

    #[test]
    fn loss_meter_caps_a_huge_gap_and_handles_wraparound() {
        let mut m = LossMeter::default();
        m.on_fragment(u32::MAX, 1);
        m.on_fragment(1, 1); // wraps: one frame (0) missing
        let Some(ControlMessage::ReceiverReport { expected, .. }) = m.take_report(1000) else { panic!() };
        assert_eq!(expected, 3);
        m.on_fragment(1_000, 1);
        let Some(ControlMessage::ReceiverReport { expected, .. }) = m.take_report(1000) else { panic!() };
        assert_eq!(expected, MAX_GAP_FRAMES + 1);
    }

    #[test]
    fn wire_report_becomes_loss_percentage() {
        assert_eq!(abr_report(90, 100, 2, Some(30)), AbrReport { loss_pct: 10.0, dropped_frames: 2, rtt_ms: Some(30) });
        assert_eq!(abr_report(5, 0, 0, None).loss_pct, 0.0);
        assert_eq!(abr_report(100, 90, 0, None).loss_pct, 0.0); // never negative
    }

    #[test]
    fn video_needs_protocol_version_3() {
        assert!(!peer_supports_video(2));
        assert!(peer_supports_video(3));
    }

    #[test]
    fn share_in_reports_once_per_window() {
        let mut rx = ShareIn::new(0, VideoCodec::H264);
        let dgs = sala_video::fragment::fragment(1, true, 0, &[7u8; 3000], 1000);
        for dg in &dgs {
            rx.on_datagram(10, dg);
        }
        let (_, early) = rx.tick(REPORT_INTERVAL_MS - 1);
        assert_eq!(early, None);
        let (_, report) = rx.tick(REPORT_INTERVAL_MS);
        assert_eq!(
            report,
            Some(ControlMessage::ReceiverReport { received: 3, expected: 3, dropped_frames: 0, window_ms: 1000 })
        );
    }
}
