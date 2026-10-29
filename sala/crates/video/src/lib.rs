//! Video codec support (`hw-encode-probe`, `codec-negotiation`): detects which hardware
//! encoders/decoders FFmpeg can really open on this machine and picks the shared codec.
//!
//! FFmpeg is the LGPL shared build from `scripts/fetch-ffmpeg.ps1` (no libx264/GPL).

use std::sync::OnceLock;

use sala_protocol::{Capabilities, VideoCodec};

/// Codecs in negotiation order: best first.
pub const PRIORITY: [VideoCodec; 3] = [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264];

/// Picks the best codec that the sender can encode AND the receiver can decode
/// (AV1 > HEVC > H.264). `None` means no common hardware codec (software fallback).
pub fn choose_codec(local_encode: &[VideoCodec], remote_decode: &[VideoCodec]) -> Option<VideoCodec> {
    PRIORITY
        .into_iter()
        .find(|c| local_encode.contains(c) && remote_decode.contains(c))
}

/// Result of trying to open one encoder/decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub codec: VideoCodec,
    /// FFmpeg encoder name (`h264_nvenc`) or `<decoder>+d3d11va`.
    pub name: String,
    pub ok: bool,
    /// Why it failed (empty when `ok`).
    pub detail: String,
}

/// Everything the probe found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeReport {
    pub encoders: Vec<Attempt>,
    pub decoders: Vec<Attempt>,
    pub capabilities: Capabilities,
}

impl ProbeReport {
    /// Human-readable multi-line summary (used by `concord --probe` and the startup log).
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for (title, list) in [("encoders", &self.encoders), ("decoders", &self.decoders)] {
            out.push_str(title);
            out.push_str(":\n");
            for a in list {
                if a.ok {
                    out.push_str(&format!("  [ok]   {}\n", a.name));
                } else {
                    out.push_str(&format!("  [fail] {} - {}\n", a.name, a.detail));
                }
            }
        }
        out.push_str(&format!(
            "hardware encode: {:?}\nhardware decode: {:?}\n",
            self.capabilities.hw_encode, self.capabilities.hw_decode
        ));
        out
    }
}

/// Folds attempts into `Capabilities`: a codec is supported when any backend opened,
/// listed in `PRIORITY` order without duplicates.
pub fn capabilities_from(encoders: &[Attempt], decoders: &[Attempt]) -> Capabilities {
    let supported = |list: &[Attempt]| -> Vec<VideoCodec> {
        PRIORITY.into_iter().filter(|c| list.iter().any(|a| a.ok && a.codec == *c)).collect()
    };
    Capabilities { hw_encode: supported(encoders), hw_decode: supported(decoders) }
}

fn short(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Av1 => "av1",
        VideoCodec::Hevc => "hevc",
        VideoCodec::H264 => "h264",
    }
}

/// Hardware encoder backends of FFmpeg, in the order they are tried.
const BACKENDS: [&str; 3] = ["nvenc", "amf", "qsv"];

/// Opens every hardware encoder/decoder for real (small context) and reports the result.
/// Takes from a few hundred ms to a couple of seconds: prefer [`capabilities`], which caches.
pub fn probe_report() -> ProbeReport {
    let mut encoders = Vec::new();
    for codec in PRIORITY {
        for backend in BACKENDS {
            let name = format!("{}_{backend}", short(codec));
            let (ok, detail) = match imp::try_open_encoder(&name) {
                Ok(()) => (true, String::new()),
                Err(e) => (false, e),
            };
            encoders.push(Attempt { codec, name, ok, detail });
        }
    }
    let mut decoders = Vec::new();
    for codec in PRIORITY {
        let name = format!("{}+d3d11va", short(codec));
        let (ok, detail) = match imp::try_open_hw_decoder(codec) {
            Ok(()) => (true, String::new()),
            Err(e) => (false, e),
        };
        decoders.push(Attempt { codec, name, ok, detail });
    }
    let capabilities = capabilities_from(&encoders, &decoders);
    ProbeReport { encoders, decoders, capabilities }
}

/// One-shot probe returning only the capabilities (not cached).
pub fn probe() -> Capabilities {
    probe_report().capabilities
}

static REPORT: OnceLock<ProbeReport> = OnceLock::new();

/// Cached probe report: the hardware is probed once per process.
pub fn report() -> &'static ProbeReport {
    REPORT.get_or_init(probe_report)
}

/// Cached capabilities of this machine (blocks on the first call while probing).
pub fn capabilities() -> Capabilities {
    report().capabilities.clone()
}

/// Capabilities if the probe already finished; never blocks (for async code / the `Hello`).
pub fn capabilities_if_ready() -> Option<Capabilities> {
    REPORT.get().map(|r| r.capabilities.clone())
}

/// Starts the probe on a background thread (no-op effect if already done or running).
pub fn warm_up() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("video-probe".into()).spawn(|| {
            report();
        });
    });
}

mod imp {
    use super::VideoCodec;
    use ffmpeg_next as ff;
    use ff::ffi;
    use std::sync::Once;

    fn init() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = ff::init();
            ff::log::set_level(ff::log::Level::Quiet);
        });
    }

    pub fn try_open_encoder(name: &str) -> Result<(), String> {
        init();
        let codec = ff::encoder::find_by_name(name).ok_or("not built into this FFmpeg")?;
        let mut enc = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| e.to_string())?;
        enc.set_width(1280);
        enc.set_height(720);
        enc.set_format(ff::format::Pixel::NV12);
        enc.set_time_base((1, 30));
        enc.set_frame_rate(Some((30, 1)));
        enc.set_bit_rate(3_000_000);
        enc.set_max_b_frames(0);
        enc.open().map(|_| ()).map_err(|e| e.to_string())
    }

    /// Native FFmpeg decoder names: `find(Id::AV1)` would return libdav1d (software, no
    /// hwaccel), so the hwaccel-capable native decoders are looked up by name.
    fn decoder_name(codec: VideoCodec) -> &'static str {
        match codec {
            VideoCodec::Av1 => "av1",
            VideoCodec::Hevc => "hevc",
            VideoCodec::H264 => "h264",
        }
    }

    /// Opens the decoder with a D3D11VA device attached. This proves the decoder is
    /// D3D11VA-capable and a device exists; it does not feed a bitstream (so a GPU
    /// that lacks the codec profile may still be reported: see report notes).
    pub fn try_open_hw_decoder(codec: VideoCodec) -> Result<(), String> {
        init();
        let decoder = ff::decoder::find_by_name(decoder_name(codec)).ok_or("decoder not built into this FFmpeg")?;
        // SAFETY: plain FFmpeg C calls on pointers we own; everything allocated is freed below.
        unsafe {
            let mut has_config = false;
            let mut i = 0;
            loop {
                let cfg = ffi::avcodec_get_hw_config(decoder.as_ptr(), i);
                if cfg.is_null() {
                    break;
                }
                if (*cfg).device_type == ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA
                    && (*cfg).methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
                {
                    has_config = true;
                }
                i += 1;
            }
            if !has_config {
                return Err("decoder has no D3D11VA support".into());
            }
            let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
            let r = ffi::av_hwdevice_ctx_create(
                &mut device,
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            );
            if r < 0 {
                return Err(format!("cannot create a D3D11VA device (error {r})"));
            }
            let mut ctx = ffi::avcodec_alloc_context3(decoder.as_ptr());
            if ctx.is_null() {
                ffi::av_buffer_unref(&mut device);
                return Err("cannot allocate the decoder context".into());
            }
            (*ctx).hw_device_ctx = ffi::av_buffer_ref(device);
            let r = ffi::avcodec_open2(ctx, decoder.as_ptr(), std::ptr::null_mut());
            ffi::avcodec_free_context(&mut ctx);
            ffi::av_buffer_unref(&mut device);
            if r < 0 {
                return Err(format!("cannot open the decoder (error {r})"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use VideoCodec::{Av1, H264, Hevc};

    #[test]
    fn prefers_av1_then_hevc_then_h264() {
        assert_eq!(choose_codec(&[H264, Hevc, Av1], &[Av1, Hevc, H264]), Some(Av1));
        assert_eq!(choose_codec(&[H264, Hevc, Av1], &[Hevc, H264]), Some(Hevc));
        assert_eq!(choose_codec(&[H264, Hevc], &[Av1, H264, Hevc]), Some(Hevc));
        assert_eq!(choose_codec(&[H264], &[Av1, Hevc, H264]), Some(H264));
    }

    #[test]
    fn needs_both_sides() {
        // Sender encodes only AV1, receiver decodes only H.264: nothing in common.
        assert_eq!(choose_codec(&[Av1], &[H264]), None);
        assert_eq!(choose_codec(&[], &[Av1, Hevc, H264]), None);
        assert_eq!(choose_codec(&[Av1, Hevc, H264], &[]), None);
        // Direction matters: encode list is the local one.
        assert_eq!(choose_codec(&[Av1, H264], &[Hevc, H264]), Some(H264));
    }

    fn attempt(codec: VideoCodec, name: &str, ok: bool) -> Attempt {
        Attempt { codec, name: name.into(), ok, detail: String::new() }
    }

    #[test]
    fn capabilities_fold_any_backend_in_priority_order() {
        let enc = [
            attempt(Av1, "av1_nvenc", false),
            attempt(Av1, "av1_amf", false),
            attempt(Hevc, "hevc_nvenc", false),
            attempt(Hevc, "hevc_qsv", true),
            attempt(H264, "h264_nvenc", true),
            attempt(H264, "h264_qsv", true),
        ];
        let dec = [attempt(H264, "h264+d3d11va", true), attempt(Av1, "av1+d3d11va", true)];
        let caps = capabilities_from(&enc, &dec);
        assert_eq!(caps.hw_encode, vec![Hevc, H264]);
        assert_eq!(caps.hw_decode, vec![Av1, H264]);
    }

    #[test]
    fn nothing_opened_means_empty_capabilities() {
        let enc = [attempt(H264, "h264_nvenc", false)];
        assert_eq!(capabilities_from(&enc, &[]), Capabilities::default());
    }

    /// Runs the real probe: must not crash on any machine, and the report must agree
    /// with the capabilities it produced.
    #[test]
    fn real_probe_is_consistent() {
        let r = probe_report();
        assert_eq!(r.encoders.len(), 9);
        assert_eq!(r.decoders.len(), 3);
        assert_eq!(r.capabilities, capabilities_from(&r.encoders, &r.decoders));
        assert!(!r.summary().is_empty());
    }
}
