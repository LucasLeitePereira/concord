//! Video decoder of the viewer (`screen-share`): D3D11VA hardware decoding through FFmpeg with a
//! fallback to FFmpeg's own software decoders (LGPL, dynamic link; never libx264). Output is RGBA
//! (hardware frames are copied to system memory and converted with swscale).
//!
//! Input is what the `Reassembler` delivers: one Annex B access unit per [`Frame`]. The decoder
//! never shows garbage: deltas are skipped until a keyframe arrives, and any error drops the
//! reference chain and asks for a new keyframe (`DecodeOutput::request_keyframe`).

use ffmpeg_next as ff;
use ff::ffi;
use sala_protocol::VideoCodec;

use crate::fragment::Frame;

/// A decoded picture, tightly packed RGBA (`width * height * 4` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Result of feeding one frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DecodeOutput {
    pub frame: Option<RgbaFrame>,
    /// The decoder cannot continue without a keyframe: the caller should send a `KeyframeRequest`
    /// (rate limited by the caller).
    pub request_keyframe: bool,
}

#[derive(Debug)]
pub enum DecodeError {
    /// No decoder (hardware or software) could be opened.
    NoDecoder(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::NoDecoder(s) => write!(f, "no video decoder could be opened: {s}"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let _ = ff::init();
        ff::log::set_level(ff::log::Level::Quiet);
    });
}

fn decoder_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Av1 => "av1",
        VideoCodec::Hevc => "hevc",
        VideoCodec::H264 => "h264",
    }
}

/// `get_format` callback: take D3D11 when the decoder offers it, otherwise fail (which makes the
/// caller fall back to the software decoder).
unsafe extern "C" fn pick_d3d11(_ctx: *mut ffi::AVCodecContext, fmts: *const ffi::AVPixelFormat) -> ffi::AVPixelFormat {
    // SAFETY: FFmpeg passes a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        let mut p = fmts;
        while *p != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *p == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 {
                return *p;
            }
            p = p.add(1);
        }
    }
    ffi::AVPixelFormat::AV_PIX_FMT_NONE
}

/// Owns an `AVBufferRef` device reference.
struct HwDevice(*mut ffi::AVBufferRef);

impl HwDevice {
    fn d3d11va() -> Result<Self, String> {
        let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
        // SAFETY: plain FFmpeg call; `device` is only used when it returns success.
        let r = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut device,
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            )
        };
        if r < 0 { Err(format!("cannot create a D3D11VA device (error {r})")) } else { Ok(HwDevice(device)) }
    }
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        // SAFETY: we own this reference.
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}

pub struct VideoDecoder {
    dec: ff::decoder::Opened,
    codec: VideoCodec,
    hardware: bool,
    name: String,
    /// Keeps the D3D11 device alive as long as the decoder (the codec context holds its own ref).
    _device: Option<HwDevice>,
    /// A keyframe was fed since the decoder was opened or last failed.
    synced: bool,
    produced: u64,
    scaler: Option<Scaler>,
    sw: ff::frame::Video,
}

struct Scaler {
    ctx: ff::software::scaling::Context,
    format: ff::format::Pixel,
    width: u32,
    height: u32,
}

// SAFETY: FFmpeg codec/swscale contexts have no thread affinity; `VideoDecoder` is not `Sync` and
// every method takes `&mut self`, so it only ever moves to the decode thread.
unsafe impl Send for VideoDecoder {}

impl VideoDecoder {
    /// Opens D3D11VA decoding for `codec`, falling back to FFmpeg's software decoder.
    pub fn open(codec: VideoCodec) -> Result<Self, DecodeError> {
        init();
        let hw = Self::open_inner(codec, true);
        match hw {
            Ok(d) => Ok(d),
            Err(hw_err) => {
                tracing::info!(?codec, "hardware decoder unavailable ({hw_err}); using software");
                Self::open_inner(codec, false).map_err(|sw_err| DecodeError::NoDecoder(format!("hardware: {hw_err}; software: {sw_err}")))
            }
        }
    }

    /// Opens the software decoder only (tests, and the fallback after a hardware failure).
    pub fn open_software(codec: VideoCodec) -> Result<Self, DecodeError> {
        init();
        Self::open_inner(codec, false).map_err(DecodeError::NoDecoder)
    }

    fn open_inner(codec: VideoCodec, hardware: bool) -> Result<Self, String> {
        let name = decoder_name(codec);
        let found = ff::decoder::find_by_name(name).ok_or("decoder not built into this FFmpeg")?;
        let mut ctx = ff::codec::context::Context::new_with_codec(found);
        let mut device = None;
        if hardware {
            let dev = HwDevice::d3d11va()?;
            // SAFETY: plain field writes on the context we own, before `open`.
            unsafe {
                let raw = ctx.as_mut_ptr();
                (*raw).hw_device_ctx = ffi::av_buffer_ref(dev.0);
                (*raw).get_format = Some(pick_d3d11);
            }
            device = Some(dev);
        }
        // SAFETY: low-latency flags on the context we own, before `open`.
        unsafe {
            let raw = ctx.as_mut_ptr();
            (*raw).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            if !hardware {
                (*raw).thread_count = 2;
            }
        }
        let dec = ctx.decoder().open().map_err(|e| e.to_string())?;
        let label = if hardware { format!("{name}+d3d11va") } else { format!("{name} (software)") };
        tracing::info!(decoder = %label, "video decoder opened");
        Ok(VideoDecoder {
            dec,
            codec,
            hardware,
            name: label,
            _device: device,
            synced: false,
            produced: 0,
            scaler: None,
            sw: ff::frame::Video::empty(),
        })
    }

    /// `h264+d3d11va` or `h264 (software)`.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn codec(&self) -> VideoCodec {
        self.codec
    }

    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// Decodes one access unit.
    pub fn decode(&mut self, frame: &Frame) -> DecodeOutput {
        if !self.synced {
            if !frame.keyframe {
                return DecodeOutput { frame: None, request_keyframe: true };
            }
            self.synced = true;
        }
        match self.try_decode(frame) {
            Ok(frame) => DecodeOutput { frame, request_keyframe: false },
            Err(e) => {
                tracing::debug!(decoder = %self.name, "decode failed: {e}");                if self.switch_to_software() {
                    // Same keyframe, new decoder: no round trip to the sender.
                    return self.decode(frame);
                }
                self.synced = false;
                self.dec.flush();
                DecodeOutput { frame: None, request_keyframe: true }
            }
        }
    }

    /// A hardware decoder that never produced a picture is replaced by the software one (the GPU
    /// may lack the codec profile although the probe opened it). `true` when it was replaced.
    fn switch_to_software(&mut self) -> bool {
        if !self.hardware || self.produced > 0 {
            return false;
        }
        match Self::open_inner(self.codec, false) {
            Ok(sw) => {
                tracing::warn!(codec = ?self.codec, "hardware decode produced nothing; switching to software");
                *self = sw;
                true
            }
            Err(e) => {
                tracing::warn!("software fallback unavailable: {e}");
                false
            }
        }
    }

    fn try_decode(&mut self, frame: &Frame) -> Result<Option<RgbaFrame>, String> {
        let mut pkt = ff::Packet::copy(&frame.data);
        pkt.set_pts(Some(i64::from(frame.timestamp)));
        if frame.keyframe {
            pkt.set_flags(ff::packet::Flags::KEY);
        }
        self.dec.send_packet(&pkt).map_err(|e| e.to_string())?;
        // With no B-frames one packet yields at most one picture; keep the newest anyway.
        let mut latest = None;
        loop {
            let mut decoded = ff::frame::Video::empty();
            match self.dec.receive_frame(&mut decoded) {
                Ok(()) => latest = Some(decoded),
                Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => break,
                Err(ff::Error::Eof) => break,
                Err(e) => return Err(e.to_string()),
            }
        }
        let Some(decoded) = latest else { return Ok(None) };
        let rgba = self.convert(decoded)?;
        self.produced += 1;
        Ok(Some(rgba))
    }

    fn convert(&mut self, decoded: ff::frame::Video) -> Result<RgbaFrame, String> {
        let on_gpu = decoded.format() == ff::format::Pixel::D3D11;
        if on_gpu {
            // SAFETY: both frames are valid AVFrames owned by us; the call allocates `self.sw`.
            let r = unsafe { ffi::av_hwframe_transfer_data(self.sw.as_mut_ptr(), decoded.as_ptr(), 0) };
            if r < 0 {
                return Err(format!("cannot copy the GPU frame (error {r})"));
            }
        }
        let src: &ff::frame::Video = if on_gpu { &self.sw } else { &decoded };
        let (w, h, fmt) = (src.width(), src.height(), src.format());
        if w == 0 || h == 0 {
            return Err("empty picture".into());
        }
        if self.scaler.as_ref().is_none_or(|s| s.format != fmt || s.width != w || s.height != h) {
            let ctx = ff::software::scaling::Context::get(
                fmt,
                w,
                h,
                ff::format::Pixel::RGBA,
                w,
                h,
                ff::software::scaling::Flags::BILINEAR,
            )
            .map_err(|e| e.to_string())?;
            self.scaler = Some(Scaler { ctx, format: fmt, width: w, height: h });
        }
        let mut out = ff::frame::Video::new(ff::format::Pixel::RGBA, w, h);
        self.scaler.as_mut().expect("set above").ctx.run(src, &mut out).map_err(|e| e.to_string())?;
        let stride = out.stride(0);
        let row = w as usize * 4;
        let plane = out.data(0);
        let mut data = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            data.extend_from_slice(&plane[y * stride..y * stride + row]);
        }
        Ok(RgbaFrame { width: w, height: h, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{EncoderConfig, VideoEncoder};

    const W: u32 = 640;
    const H: u32 = 360;

    /// A frame with a solid colour block that moves, on a gradient.
    fn pattern(i: u32) -> Vec<u8> {
        let mut px = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let o = ((y * W + x) * 4) as usize;
                let inside = (x / 80) % 2 == (i % 2) && y > 100 && y < 260;
                let (b, g, r) = if inside { (30, 30, 230) } else { (60, (y / 2) as u8, 60) };
                px[o..o + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        px
    }

    fn encode_stream(codec: VideoCodec, n: u32) -> Option<Vec<Frame>> {
        let cfg = EncoderConfig { codec, width: W, height: H, fps: 30, bitrate_kbps: 3000 };
        let mut enc = VideoEncoder::open(&cfg).ok()?;
        let mut frames = Vec::new();
        for i in 0..n {
            for p in enc.encode_bgra(&pattern(i), W, H, i64::from(i) * 3000).unwrap() {
                frames.push(Frame { frame_id: frames.len() as u32, keyframe: p.keyframe, timestamp: p.pts as u32, data: p.data });
            }
        }
        frames.extend(enc.finish().unwrap().into_iter().map(|p| Frame {
            frame_id: 0,
            keyframe: p.keyframe,
            timestamp: p.pts as u32,
            data: p.data,
        }));
        Some(frames)
    }

    /// Mean RGB of a block of the RGBA picture.
    fn mean_rgb(f: &RgbaFrame, x0: u32, y0: u32, x1: u32, y1: u32) -> [u32; 3] {
        let (mut s, mut n) = ([0u32; 3], 0);
        for y in y0..y1 {
            for x in x0..x1 {
                let o = ((y * f.width + x) * 4) as usize;
                for (c, sum) in s.iter_mut().enumerate() {
                    *sum += u32::from(f.data[o + c]);
                }
                n += 1;
            }
        }
        s.map(|v| v / n)
    }

    /// Feeds the opening keyframe. A GPU whose D3D11VA rejects the stream (the probe cannot tell)
    /// fails here once, falls back to software and asks again: the retry is that second ask.
    fn prime(dec: &mut VideoDecoder, key: &Frame) -> DecodeOutput {
        let out = dec.decode(key);
        if out.request_keyframe { dec.decode(key) } else { out }
    }

    /// Hardware-dependent: passes in silence when this machine has no encoder.
    #[test]
    fn decodes_what_the_encoder_produced() {
        let Some(stream) = encode_stream(VideoCodec::H264, 20) else { return };
        assert!(stream[0].keyframe, "the stream must start with a keyframe");
        for software in [false, true] {
            let mut dec = if software {
                VideoDecoder::open_software(VideoCodec::H264).unwrap()
            } else {
                VideoDecoder::open(VideoCodec::H264).unwrap()
            };
            let mut pics = Vec::new();
            pics.extend(prime(&mut dec, &stream[0]).frame);
            for f in &stream[1..] {
                let out = dec.decode(f);
                assert!(!out.request_keyframe, "{}: unexpected keyframe request", dec.name());
                pics.extend(out.frame);
            }
            assert!(pics.len() >= 15, "{}: only {} pictures", dec.name(), pics.len());
            let last = pics.last().unwrap();
            assert_eq!((last.width, last.height), (W, H), "cropping is wrong on {}", dec.name());
            assert_eq!(last.data.len(), (W * H * 4) as usize);
            // The red blocks sit on even or odd 80 px columns depending on the frame; colours must
            // come out right (R high, G and B low; exact values drift with the lossy codec), so
            // one of the two probes is red and the other is the dark green gradient.
            let a = mean_rgb(last, 10, 150, 70, 200);
            let b = mean_rgb(last, 90, 150, 150, 200);
            let is_red = |c: [u32; 3]| c[0] > 150 && c[1] < 100 && c[2] < 100;
            assert!(is_red(a) != is_red(b), "{}: probes are {a:?} and {b:?}", dec.name());
        }
    }

    /// Deltas before the first keyframe are not decoded and ask for a keyframe; after a keyframe
    /// everything is normal (control: the same delta decodes once a keyframe preceded it).
    #[test]
    fn waits_for_a_keyframe() {
        let Some(stream) = encode_stream(VideoCodec::H264, 6) else { return };
        let mut dec = VideoDecoder::open(VideoCodec::H264).unwrap();
        let delta = stream.iter().find(|f| !f.keyframe).expect("a delta frame").clone();
        let out = dec.decode(&delta);
        assert!(out.frame.is_none() && out.request_keyframe);
        let mut got = usize::from(prime(&mut dec, &stream[0]).frame.is_some());
        for f in &stream[1..] {
            let out = dec.decode(f);
            assert!(!out.request_keyframe);
            got += usize::from(out.frame.is_some());
        }
        assert!(got >= 5, "got {got}");
    }

    /// Garbage after sync: no picture, a keyframe request, and the decoder recovers on the next keyframe.
    #[test]
    fn corrupt_data_requests_a_keyframe_and_recovers() {
        let Some(stream) = encode_stream(VideoCodec::H264, 6) else { return };
        let mut dec = VideoDecoder::open_software(VideoCodec::H264).unwrap();
        assert!(dec.decode(&stream[0]).frame.is_some());
        let garbage = Frame { frame_id: 99, keyframe: false, timestamp: 1, data: vec![0xAB; 300] };
        let out = dec.decode(&garbage);
        assert!(out.frame.is_none());
        // After the garbage, a delta must not be fed to a broken chain.
        let delta = stream.iter().find(|f| !f.keyframe).unwrap();
        if out.request_keyframe {
            assert!(dec.decode(delta).request_keyframe);
        }
        let again = dec.decode(&stream[0]);
        assert!(again.frame.is_some() && !again.request_keyframe);
    }
}
