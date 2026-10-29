//! Low-latency hardware video encoder (`hw-video-encoder`) through FFmpeg (LGPL, dynamic).
//! Never libx264 (GPL): without a working hardware encoder `open` fails.
//!
//! Input is BGRA (converted to NV12 with swscale, which also scales to the target size);
//! output is Annex B packets (parameter sets in-band on every keyframe).

use ffmpeg_next as ff;
use sala_protocol::VideoCodec;

/// Backends in the same order the probe uses.
const BACKENDS: [&str; 3] = ["nvenc", "amf", "qsv"];

/// Presentation clock of `pts` and of the fragment `timestamp`: 90 kHz.
pub const TIME_BASE_HZ: i32 = 90_000;

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub codec: VideoCodec,
    /// Output size; must be even.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
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

pub struct VideoEncoder {
    enc: ff::encoder::video::Encoder,
    name: String,
    cfg: EncoderConfig,
    scaler: Option<(ff::software::scaling::Context, u32, u32)>,
    src: Option<ff::frame::Video>,
    dst: ff::frame::Video,
    force_key: bool,
    last_pts: i64,
}

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
            match Self::open_named(&name, backend, cfg) {
                Ok(e) => return Ok(e),
                Err(e) => log.push(format!("{name}: {e}")),
            }
        }
        Err(EncodeError::NoEncoder(log.join("; ")))
    }

    fn open_named(name: &str, backend: &str, cfg: &EncoderConfig) -> Result<Self, String> {
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
        let bps = cfg.bitrate_kbps as usize * 1000;
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
        Ok(VideoEncoder {
            enc,
            name: name.to_string(),
            cfg: cfg.clone(),
            scaler: None,
            src: None,
            dst,
            force_key: false,
            last_pts: i64::MIN,
        })
    }

    /// FFmpeg encoder name in use (`h264_qsv`).
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
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

        if self.src.as_ref().is_none_or(|f| f.width() != width || f.height() != height) {
            self.src = Some(ff::frame::Video::new(ff::format::Pixel::BGRA, width, height));
        }
        if self.scaler.as_ref().is_none_or(|(_, w, h)| *w != width || *h != height) {
            let sws = ff::software::scaling::Context::get(
                ff::format::Pixel::BGRA,
                width,
                height,
                ff::format::Pixel::NV12,
                self.cfg.width,
                self.cfg.height,
                ff::software::scaling::Flags::BILINEAR,
            )
            .map_err(err)?;
            self.scaler = Some((sws, width, height));
        }
        let src = self.src.as_mut().expect("set above");
        let stride = src.stride(0);
        let row = width as usize * 4;
        let plane = src.data_mut(0);
        for y in 0..height as usize {
            plane[y * stride..y * stride + row].copy_from_slice(&bgra[y * row..(y + 1) * row]);
        }
        let (sws, _, _) = self.scaler.as_mut().expect("set above");
        sws.run(src, &mut self.dst).map_err(err)?;

        self.dst.set_pts(Some(pts));
        self.dst.set_kind(if std::mem::take(&mut self.force_key) {
            ff::picture::Type::I
        } else {
            ff::picture::Type::None
        });
        self.enc.send_frame(&self.dst).map_err(err)?;
        self.drain()
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
    fn rejects_odd_sizes() {
        let cfg = EncoderConfig { codec: VideoCodec::H264, width: 1281, height: 720, fps: 30, bitrate_kbps: 1000 };
        assert!(matches!(VideoEncoder::open(&cfg), Err(EncodeError::Invalid(_))));
    }

    /// Hardware-dependent: skipped (passes) when this machine has no encoder.
    #[test]
    fn encodes_and_forces_keyframes() {
        let cfg = EncoderConfig { codec: VideoCodec::H264, width: 640, height: 360, fps: 30, bitrate_kbps: 2000 };
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
        for p in enc.finish().unwrap() {
            total += 1;
            keys += usize::from(p.keyframe);
        }
        assert!(total >= 38, "got {total} packets");
        assert!(keys >= 2, "forced keyframe missing ({keys})");
    }
}
