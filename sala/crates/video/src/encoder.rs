//! Low-latency hardware video encoder (`hw-video-encoder`) through FFmpeg (LGPL, dynamic).
//! Never libx264 (GPL): without a working hardware encoder `open` fails.
//!
//! Input is BGRA (converted to NV12 with swscale, which also scales to the target size);
//! output is Annex B packets (parameter sets in-band on every keyframe).

use ffmpeg_next as ff;
use sala_protocol::VideoCodec;

use crate::scale::BgraToNv12;

/// Backends in the same order the probe uses.
const BACKENDS: [&str; 3] = ["nvenc", "amf", "qsv"];

/// Presentation clock of `pts` and of the fragment `timestamp`: 90 kHz.
pub const TIME_BASE_HZ: i32 = 90_000;

/// A bitrate change smaller than this share of the applied rate is not worth a reopen: every
/// reopen forces a keyframe, a burst that the network answers with loss (and the ABR with a cut).
pub const REOPEN_THRESHOLD_PERCENT: u64 = 25;
/// Minimum spacing between two reopens.
pub const REOPEN_MIN_INTERVAL_MS: u64 = 10_000;
/// A cut (the network is losing packets) may reopen sooner than that.
pub const REOPEN_CUT_INTERVAL_MS: u64 = 2_000;

/// Whether moving from the applied `current` to `target` kbps justifies reopening the encoder
/// now. `since_reopen_ms` is the time since the last reopen (`None`: never). Pure: the caller
/// owns the clock.
pub fn bitrate_change_needs_reopen(current: u32, target: u32, since_reopen_ms: Option<u64>) -> bool {
    if target == 0 {
        return false;
    }
    let spacing = if target < current { REOPEN_CUT_INTERVAL_MS } else { REOPEN_MIN_INTERVAL_MS };
    u64::from(current.abs_diff(target)) * 100 > u64::from(current) * REOPEN_THRESHOLD_PERCENT
        && since_reopen_ms.is_none_or(|s| s >= spacing)
}

/// Whether a bitrate change on this backend is written on the open context (QSV, while it has
/// not shown that it cannot) instead of reopening the encoder. Pure: testable without hardware.
pub fn change_is_live(backend: &str, live_ok: bool) -> bool {
    backend == "qsv" && live_ok
}

/// The rate actually applied for `target`: a live change cannot go above the ceiling the context
/// was opened with (QSV refuses it with EINVAL), so it is capped there; a reopen takes it as is.
pub fn effective_target(live: bool, target: u32, ceiling_kbps: u32) -> u32 {
    if live { target.min(ceiling_kbps) } else { target }
}

/// Result of [`VideoEncoder::set_bitrate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateChange {
    /// Held back (inside the threshold or the spacing).
    None,
    /// Written on the open context; costs about one frame, with a keyframe.
    Live,
    /// The encoder was reopened (seconds), with a keyframe.
    Reopened,
}

impl BitrateChange {
    /// Either way the stream got a keyframe.
    pub fn changed(self) -> bool {
        self != BitrateChange::None
    }
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub codec: VideoCodec,
    /// Output size; must be even.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Rate to start at (and the one applied, once changed).
    pub bitrate_kbps: u32,
    /// Highest rate the stream will ask for. QSV opens the context there so that later changes can
    /// be live (a value below `bitrate_kbps` counts as `bitrate_kbps`).
    pub ceiling_kbps: u32,
}

/// Where the time of one `encode_bgra` call went, in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodeTimings {
    /// Filling the swscale input frame from the caller's buffer.
    pub copy_micros: u64,
    /// BGRA to NV12 conversion (and scaling).
    pub sws_micros: u64,
    /// `send_frame` plus draining the packets.
    pub enc_micros: u64,
}

/// One encoded access unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub keyframe: bool,
    /// In [`TIME_BASE_HZ`] ticks.
    pub pts: i64,
}

#[derive(Debug)]
pub enum EncodeError {
    /// No hardware encoder opened; the string lists each attempt.
    NoEncoder(String),
    Invalid(&'static str),
    Ffmpeg(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::NoEncoder(s) => write!(f, "no hardware encoder could be opened: {s}"),
            EncodeError::Invalid(s) => write!(f, "invalid encoder input: {s}"),
            EncodeError::Ffmpeg(s) => write!(f, "ffmpeg: {s}"),
        }
    }
}

impl std::error::Error for EncodeError {}

fn short(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Av1 => "av1",
        VideoCodec::Hevc => "hevc",
        VideoCodec::H264 => "h264",
    }
}

fn init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let _ = ff::init();
        ff::log::set_level(ff::log::Level::Quiet);
    });
}

/// Per-backend low-latency options (unknown ones are ignored by FFmpeg).
fn backend_options(backend: &str, keyframe_gop: u32) -> ff::Dictionary<'static> {
    let mut o = ff::Dictionary::new();
    match backend {
        "nvenc" => {
            o.set("preset", "p1");
            o.set("tune", "ull");
            o.set("rc", "cbr");
            o.set("zerolatency", "1");
            o.set("delay", "0");
            o.set("forced-idr", "1");
            o.set("rc-lookahead", "0");
        }
        "amf" => {
            o.set("usage", "ultralowlatency");
            o.set("quality", "speed");
            o.set("rc", "cbr");
        }
        _ => {
            o.set("preset", "veryfast");
            o.set("async_depth", "1");
            o.set("look_ahead", "0");
            o.set("forced_idr", "1");
            o.set("low_delay_brc", "1");
            o.set("scenario", "displayremoting");
        }
    }
    o.set("g", &keyframe_gop.to_string());
    o
}

/// Writes the rate on an open context without reopening it. Only `bit_rate` and `rc_max_rate`:
/// QSV resets with the new rate only while `rc_buffer_size` keeps the value it opened with.
fn write_rate(enc: &mut ff::encoder::video::Encoder, kbps: u32) {
    let bps = i64::from(kbps) * 1000;
    // SAFETY: plain field writes on the context we own, between two `send_frame` calls.
    unsafe {
        let ctx = enc.as_mut_ptr();
        (*ctx).bit_rate = bps;
        (*ctx).rc_max_rate = bps;
    }
}

pub struct VideoEncoder {
    enc: ff::encoder::video::Encoder,
    name: String,
    backend: String,
    cfg: EncoderConfig,
    scaler: Option<BgraToNv12>,
    dst: ff::frame::Video,
    force_key: bool,
    last_pts: i64,
    /// Time of the last rate change (live or reopen) on the caller's clock.
    last_reopen_ms: Option<u64>,
    last_timings: EncodeTimings,
    /// Live changes still allowed (cleared once the backend rejected one).
    live_ok: bool,
    /// A live change was written and no frame went through since.
    live_unverified: bool,
}

// SAFETY: the FFmpeg codec and swscale contexts are plain heap objects with no thread
// affinity; `VideoEncoder` is not `Sync` and every method takes `&mut self`, so it is only
// ever used by one thread at a time (moved into the encode thread, never shared).
unsafe impl Send for VideoEncoder {}

impl VideoEncoder {
    /// Opens the first hardware backend that works for `cfg.codec` (nvenc, amf, qsv).
    pub fn open(cfg: &EncoderConfig) -> Result<Self, EncodeError> {
        init();
        if cfg.width == 0 || cfg.height == 0 || !cfg.width.is_multiple_of(2) || !cfg.height.is_multiple_of(2) || cfg.fps == 0 {
            return Err(EncodeError::Invalid("size must be even and fps > 0"));
        }
        let mut log = Vec::new();
        for backend in BACKENDS {
            let name = format!("{}_{backend}", short(cfg.codec));
            match Self::open_named(&name, backend, cfg, true) {
                Ok(e) => return Ok(e),
                Err(e) => log.push(format!("{name}: {e}")),
            }
        }
        Err(EncodeError::NoEncoder(log.join("; ")))
    }

    /// `live`: whether later rate changes may be live; if so on QSV the context opens at the
    /// ceiling (rate and VBV) and `cfg.bitrate_kbps` is written right after.
    fn open_named(name: &str, backend: &str, cfg: &EncoderConfig, live: bool) -> Result<Self, String> {
        let live = change_is_live(backend, live);
        let mut cfg = cfg.clone();
        cfg.ceiling_kbps = cfg.ceiling_kbps.max(cfg.bitrate_kbps);
        let open_kbps = if live { cfg.ceiling_kbps } else { cfg.bitrate_kbps };
        let codec = ff::encoder::find_by_name(name).ok_or("not built into this FFmpeg")?;
        let mut enc = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| e.to_string())?;
        enc.set_width(cfg.width);
        enc.set_height(cfg.height);
        enc.set_format(ff::format::Pixel::NV12);
        enc.set_time_base((1, TIME_BASE_HZ));
        enc.set_frame_rate(Some((cfg.fps as i32, 1)));
        let bps = open_kbps as usize * 1000;
        enc.set_bit_rate(bps);
        enc.set_max_bit_rate(bps);
        enc.set_max_b_frames(0);
        // Long GOP: keyframes only on demand (and a safety one every 20 s).
        let gop = cfg.fps * 20;
        enc.set_gop(gop);
        // SAFETY: plain field writes on the context we own, before `open`. A VBV of ~125 ms
        // keeps the bitrate spikes (and so the latency) small.
        unsafe {
            let ctx = enc.as_mut_ptr();
            (*ctx).rc_buffer_size = (bps / 8).min(i32::MAX as usize) as i32;
            (*ctx).rc_initial_buffer_occupancy = (*ctx).rc_buffer_size;
            (*ctx).delay = 0;
            (*ctx).has_b_frames = 0;
            (*ctx).thread_count = 1;
        }
        let enc = enc.open_with(backend_options(backend, gop)).map_err(|e| e.to_string())?;
        let dst = ff::frame::Video::new(ff::format::Pixel::NV12, cfg.width, cfg.height);
        let mut enc = enc;
        let below_ceiling = live && cfg.bitrate_kbps < open_kbps;
        if below_ceiling {
            write_rate(&mut enc, cfg.bitrate_kbps);
        }
        Ok(VideoEncoder {
            enc,
            name: name.to_string(),
            backend: backend.to_string(),
            cfg,
            scaler: None,
            dst,
            force_key: false,
            last_pts: i64::MIN,
            last_reopen_ms: None,
            last_timings: EncodeTimings::default(),
            live_ok: live,
            live_unverified: below_ceiling,
        })
    }

    /// Time split of the last `encode_bgra` call.
    pub fn last_timings(&self) -> EncodeTimings {
        self.last_timings
    }

    /// FFmpeg encoder name in use (`h264_qsv`).
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// Moves the bitrate target (`now_ms`: a monotonic clock owned by the caller). Either way a
    /// rate change makes a keyframe, a burst that the network answers with loss, so the target
    /// is applied only when the change exceeds [`REOPEN_THRESHOLD_PERCENT`] of the applied rate
    /// and [`REOPEN_MIN_INTERVAL_MS`] passed since the last change (a cut waits only
    /// [`REOPEN_CUT_INTERVAL_MS`]); a target held back stays pending and is judged again on the
    /// next call.
    ///
    /// How it is applied depends on the backend: QSV (opened at `ceiling_kbps`) takes the new
    /// `bit_rate`/`rc_max_rate` on the open context, about one frame of cost ([`BitrateChange::Live`]),
    /// and caps the target at the ceiling; if the next frame is rejected, `encode_bgra` falls back
    /// to reopening for the rest of the session. NVENC and AMF reopen the context, which takes
    /// seconds on this thread ([`BitrateChange::Reopened`]).
    pub fn set_bitrate(&mut self, kbps: u32, now_ms: u64) -> Result<BitrateChange, EncodeError> {
        let live = change_is_live(&self.backend, self.live_ok);
        let target = effective_target(live, kbps, self.cfg.ceiling_kbps);
        let since = self.last_reopen_ms.map(|t| now_ms.saturating_sub(t));
        if !bitrate_change_needs_reopen(self.cfg.bitrate_kbps, target, since) {
            return Ok(BitrateChange::None);
        }
        if live {
            write_rate(&mut self.enc, target);
            self.cfg.bitrate_kbps = target;
            self.live_unverified = true;
            self.force_key = true;
            self.last_reopen_ms = Some(now_ms);
            return Ok(BitrateChange::Live);
        }
        let mut cfg = self.cfg.clone();
        cfg.bitrate_kbps = target;
        let fresh = Self::open_named(&self.name, &self.backend, &cfg, false).map_err(EncodeError::Ffmpeg)?;
        self.enc = fresh.enc;
        self.dst = fresh.dst;
        self.cfg = cfg;
        self.force_key = true;
        self.last_reopen_ms = Some(now_ms);
        Ok(BitrateChange::Reopened)
    }

    /// The backend rejected a live change: reopen at the applied rate and never go live again.
    fn fall_back_to_reopen(&mut self) -> Result<(), String> {
        let fresh = Self::open_named(&self.name, &self.backend, &self.cfg, false)?;
        self.enc = fresh.enc;
        self.live_ok = false;
        self.live_unverified = false;
        self.force_key = true;
        Ok(())
    }

    /// Whether rate changes are still written live (false after the backend rejected one).
    pub fn live_changes_ok(&self) -> bool {
        change_is_live(&self.backend, self.live_ok)
    }

    /// The next frame is encoded as an IDR keyframe (answer to a `KeyframeRequest`).
    pub fn force_keyframe(&mut self) {
        self.force_key = true;
    }

    /// Encodes one BGRA frame (`width * height * 4` bytes, no padding); `pts` in 90 kHz
    /// ticks. Returns the packets that are ready (usually one, sometimes none at start).
    pub fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        pts: i64,
    ) -> Result<Vec<EncodedPacket>, EncodeError> {
        if width == 0 || height == 0 || bgra.len() != width as usize * height as usize * 4 {
            return Err(EncodeError::Invalid("buffer does not match width * height * 4"));
        }
        let err = |e: ff::Error| EncodeError::Ffmpeg(e.to_string());
        // The encoder needs strictly increasing pts.
        let pts = if pts <= self.last_pts { self.last_pts + 1 } else { pts };
        self.last_pts = pts;

        if self.scaler.as_ref().is_none_or(|s| s.src_size() != (width, height)) {
            let threads = crate::scale::default_threads();
            self.scaler = Some(
                BgraToNv12::new(width, height, self.cfg.width, self.cfg.height, threads).map_err(EncodeError::Ffmpeg)?,
            );
        }
        let t0 = std::time::Instant::now();
        let sws = self.scaler.as_mut().expect("set above");
        let copy_micros = sws.convert(bgra, &mut self.dst).map_err(EncodeError::Ffmpeg)?;
        let t2 = std::time::Instant::now();

        self.dst.set_pts(Some(pts));
        self.dst.set_kind(if std::mem::take(&mut self.force_key) {
            ff::picture::Type::I
        } else {
            ff::picture::Type::None
        });
        if let Err(e) = self.enc.send_frame(&self.dst) {
            if !self.live_unverified {
                return Err(err(e));
            }
            // The backend refused the live rate change (EINVAL): reopen at the applied rate,
            // give up on live changes for this encoder and send the same frame again.
            tracing::warn!("video encoder rejected a live bitrate change ({e}); reopening, live changes off");
            self.fall_back_to_reopen().map_err(EncodeError::Ffmpeg)?;
            self.dst.set_kind(ff::picture::Type::I);
            self.force_key = false;
            self.enc.send_frame(&self.dst).map_err(err)?;
        }
        self.live_unverified = false;
        let packets = self.drain();
        self.last_timings = EncodeTimings {
            copy_micros,
            sws_micros: (t2 - t0).as_micros() as u64 - copy_micros,
            enc_micros: t2.elapsed().as_micros() as u64,
        };
        packets
    }

    /// Flushes the encoder at the end of the stream.
    pub fn finish(&mut self) -> Result<Vec<EncodedPacket>, EncodeError> {
        self.enc.send_eof().map_err(|e| EncodeError::Ffmpeg(e.to_string()))?;
        self.drain()
    }

    fn drain(&mut self) -> Result<Vec<EncodedPacket>, EncodeError> {
        let mut out = Vec::new();
        loop {
            let mut pkt = ff::Packet::empty();
            match self.enc.receive_packet(&mut pkt) {
                Ok(()) => out.push(EncodedPacket {
                    data: pkt.data().map(<[u8]>::to_vec).unwrap_or_default(),
                    keyframe: pkt.is_key(),
                    pts: pkt.pts().unwrap_or(0),
                }),
                Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => break,
                Err(ff::Error::Eof) => break,
                Err(e) => return Err(EncodeError::Ffmpeg(e.to_string())),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopen_needs_a_big_change_and_spacing_but_cuts_may_come_sooner() {
        assert!(!bitrate_change_needs_reopen(6000, 6000, None));
        assert!(!bitrate_change_needs_reopen(6000, 7500, None)); // +25% exactly: no
        assert!(bitrate_change_needs_reopen(6000, 7501, None));
        assert!(!bitrate_change_needs_reopen(6000, 4500, None)); // -25% exactly: no
        assert!(bitrate_change_needs_reopen(6000, 4200, None)); // the ABR cut
        assert!(!bitrate_change_needs_reopen(6000, 0, None));
        // Spacing: a raise waits the full interval, a cut only the short one.
        assert!(!bitrate_change_needs_reopen(6000, 9000, Some(9_999)));
        assert!(bitrate_change_needs_reopen(6000, 9000, Some(10_000)));
        assert!(!bitrate_change_needs_reopen(6000, 3000, Some(1_999)));
        assert!(bitrate_change_needs_reopen(6000, 3000, Some(2_000)));
    }

    /// The feedback loop seen in the field: the network holds ~6 Mbps, a keyframe burst from a
    /// reopen makes the next report lossy, the ABR cuts, then climbs, and the encoder reopens
    /// again. Runs the real ABR with a fake clock for two minutes of reports (one per second)
    /// and returns the reopens `policy(applied, target, ms since the last reopen)` allowed.
    fn simulate_reopens(policy: impl Fn(u32, u32, Option<u64>) -> bool) -> u32 {
        use crate::abr::{Abr, ReceiverReport};
        use crate::preset::BitrateRange;
        let mut abr = Abr::new(BitrateRange { initial: 8_000, min: 2_000, max: 10_000 });
        let (mut applied, mut last_reopen, mut reopens) = (8_000u32, None::<u64>, 0u32);
        for second in 0..120u64 {
            let now = second * 1_000;
            let burst = last_reopen.is_some_and(|t| now - t <= 1_000);
            let loss_pct = if burst || applied > 6_000 { 8.0 } else { 0.0 };
            let target = abr.on_report(now, &ReceiverReport { loss_pct, ..Default::default() });
            if policy(applied, target, last_reopen.map(|t| now - t)) {
                applied = target;
                last_reopen = Some(now);
                reopens += 1;
                abr.note_encoder_reopen(now);
            }
        }
        reopens
    }

    #[test]
    fn oscillating_loss_cannot_cause_a_reopen_storm() {
        let reopens = simulate_reopens(bitrate_change_needs_reopen);
        assert!(reopens <= 120 / 10 + 2, "{reopens} reopens in 120 s");
        assert!(reopens >= 1, "the ABR never got the encoder to move");
        // Control: with no threshold and no spacing the same reports reopen far more, so this
        // simulation does catch a storm.
        let ungated = simulate_reopens(|current, target, _| target > 0 && current != target);
        assert!(ungated >= 3 * reopens, "ungated {ungated} vs gated {reopens}");
    }

    /// Hardware-dependent: skipped (passes) when this machine has no encoder.
    #[test]
    fn set_bitrate_reopens_forces_a_keyframe_and_respects_the_spacing() {
        let cfg = EncoderConfig { codec: VideoCodec::H264, width: 640, height: 360, fps: 30, bitrate_kbps: 4000, ceiling_kbps: 4000 };
        let Ok(mut enc) = VideoEncoder::open(&cfg) else { return };
        let frame = vec![90u8; 640 * 360 * 4];
        enc.encode_bgra(&frame, 640, 360, 0).unwrap();
        assert!(!enc.set_bitrate(4300, 0).unwrap().changed());
        assert!(!enc.set_bitrate(3300, 0).unwrap().changed()); // -17.5%: inside the threshold
        assert_eq!(enc.config().bitrate_kbps, 4000);
        assert!(enc.set_bitrate(2000, 0).unwrap().changed());
        assert_eq!(enc.config().bitrate_kbps, 2000);
        let mut first_key = None;
        for i in 1..10i64 {
            for p in enc.encode_bgra(&frame, 640, 360, i * 3000).unwrap() {
                first_key.get_or_insert(p.keyframe);
            }
        }
        assert_eq!(first_key, Some(true), "no keyframe after reopening");
        // Right after a reopen, another large raise waits for the spacing.
        assert!(!enc.set_bitrate(4000, 1_000).unwrap().changed());
        assert!(enc.set_bitrate(4000, REOPEN_MIN_INTERVAL_MS).unwrap().changed());
    }

    #[test]
    fn live_changes_are_only_for_qsv_and_never_above_the_ceiling() {
        assert!(change_is_live("qsv", true));
        assert!(!change_is_live("qsv", false), "after a rejected change it must reopen");
        assert!(!change_is_live("nvenc", true));
        assert!(!change_is_live("amf", true));
        assert_eq!(effective_target(true, 12_000, 8_000), 8_000);
        assert_eq!(effective_target(true, 3_000, 8_000), 3_000);
        assert_eq!(effective_target(false, 12_000, 8_000), 12_000);
        assert!(!BitrateChange::None.changed());
        assert!(BitrateChange::Live.changed() && BitrateChange::Reopened.changed());
    }

    /// Hardware-dependent: skipped (passes) when this machine has no QSV. The context opens at the
    /// ceiling, the first target is applied live, a raise goes back up to (and is capped at) the
    /// ceiling, and each change makes a keyframe without a reopen.
    #[test]
    fn qsv_changes_the_bitrate_live_within_the_ceiling() {
        let cfg = EncoderConfig { codec: VideoCodec::H264, width: 640, height: 360, fps: 30, bitrate_kbps: 2000, ceiling_kbps: 6000 };
        let Ok(mut enc) = VideoEncoder::open(&cfg) else { return };
        if enc.name() != "h264_qsv" {
            return;
        }
        let mut frame = vec![0u8; 640 * 360 * 4];
        let mut pts = 0i64;
        let mut run = |enc: &mut VideoEncoder, n: usize| -> Vec<bool> {
            let mut keys = Vec::new();
            for _ in 0..n {
                for (i, px) in frame.chunks_mut(4).enumerate() {
                    px.copy_from_slice(&[(i as i64 + pts * 5) as u8, (i / 640) as u8, 90, 255]);
                }
                pts += 3000;
                keys.extend(enc.encode_bgra(&frame, 640, 360, pts).unwrap().iter().map(|p| p.keyframe));
            }
            keys
        };
        assert_eq!(enc.config().bitrate_kbps, 2000);
        run(&mut enc, 5);
        // 2000 -> 6000 (+200%), and 9000 is capped at the ceiling.
        assert_eq!(enc.set_bitrate(9000, 0).unwrap(), BitrateChange::Live);
        assert_eq!(enc.config().bitrate_kbps, 6000);
        assert_eq!(run(&mut enc, 3).first(), Some(&true), "no keyframe after the live change");
        // A cut after the short spacing is live as well.
        assert_eq!(enc.set_bitrate(2000, REOPEN_CUT_INTERVAL_MS).unwrap(), BitrateChange::Live);
        assert_eq!(run(&mut enc, 3).first(), Some(&true));
        assert!(enc.live_changes_ok(), "the live change was rejected and the fallback ran");
    }

    #[test]
    fn rejects_odd_sizes() {
        let cfg = EncoderConfig { codec: VideoCodec::H264, width: 1281, height: 720, fps: 30, bitrate_kbps: 1000, ceiling_kbps: 1000 };
        assert!(matches!(VideoEncoder::open(&cfg), Err(EncodeError::Invalid(_))));
    }

    /// Hardware-dependent: skipped (passes) when this machine has no encoder. A source whose
    /// width is not a multiple of 16 (1366x768 monitors) is converted through a padded copy.
    #[test]
    fn encodes_a_source_that_needs_the_padded_copy() {
        let cfg = EncoderConfig { codec: VideoCodec::H264, width: 640, height: 360, fps: 30, bitrate_kbps: 2000, ceiling_kbps: 2000 };
        let Ok(mut enc) = VideoEncoder::open(&cfg) else { return };
        let frame = vec![120u8; 650 * 360 * 4];
        enc.encode_bgra(&frame, 650, 360, 0).unwrap();
        assert!(enc.last_timings().copy_micros > 0);
        // Back to a width that needs no copy: the converter is rebuilt for the new size.
        enc.encode_bgra(&vec![120u8; 640 * 360 * 4], 640, 360, 3000).unwrap();
        assert_eq!(enc.last_timings().copy_micros, 0);
    }

    /// Hardware-dependent: skipped (passes) when this machine has no encoder.
    #[test]
    fn encodes_and_forces_keyframes() {
        let cfg = EncoderConfig { codec: VideoCodec::H264, width: 640, height: 360, fps: 30, bitrate_kbps: 2000, ceiling_kbps: 2000 };
        let Ok(mut enc) = VideoEncoder::open(&cfg) else { return };
        let mut frame = vec![0u8; 640 * 360 * 4];
        let (mut keys, mut total) = (0, 0);
        for i in 0..40i64 {
            for (n, px) in frame.chunks_mut(4).enumerate() {
                px.copy_from_slice(&[(n as i64 + i * 7) as u8, (n / 640) as u8, 90, 255]);
            }
            if i == 20 {
                enc.force_keyframe();
            }
            for p in enc.encode_bgra(&frame, 640, 360, i * 3000).unwrap() {
                assert!(!p.data.is_empty());
                total += 1;
                if p.keyframe {
                    keys += 1;
                }
            }
        }
        // The time split of the last call (what `share stats tx` reports) is filled in.
        let parts = enc.last_timings();
        assert!(parts.sws_micros > 0 && parts.enc_micros > 0, "{parts:?}");
        assert_eq!(parts.copy_micros, 0, "640 is a multiple of 16: no input copy expected");
        for p in enc.finish().unwrap() {
            total += 1;
            keys += usize::from(p.keyframe);
        }
        assert!(total >= 38, "got {total} packets");
        assert!(keys >= 2, "forced keyframe missing ({keys})");
    }
}
