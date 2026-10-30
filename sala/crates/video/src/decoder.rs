//! Video decoder of the viewer (`screen-share`): D3D11VA hardware decoding through FFmpeg with a
//! fallback to FFmpeg's own software decoders (LGPL, dynamic link; never libx264). Output is RGBA
//! (hardware frames are copied to system memory and converted with swscale).
//!
//! Input is what the `Reassembler` delivers: one Annex B access unit per [`Frame`]. The decoder
//! never shows garbage: deltas are skipped until a keyframe arrives, and any error drops the
//! reference chain and asks for a new keyframe (`DecodeOutput::request_keyframe`).

use std::sync::atomic::{AtomicU32, Ordering};

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

/// D3D11 adapters tried for hardware decoding, by DXGI index. On a laptop with an iGPU and a dGPU
/// the default adapter (0) may not decode what the iGPU encoder produces, while another does.
const MAX_ADAPTERS: u32 = 4;

/// Adapter that decoded a picture last in this process (`u32::MAX`: none yet).
static GOOD_ADAPTER: AtomicU32 = AtomicU32::new(u32::MAX);

/// Order in which adapters are tried: the one that worked before, then the others by index,
/// skipping the ones in the `tried` bit mask.
fn adapter_order(good: Option<u32>, tried: u32) -> Vec<u32> {
    let skip = |a: u32| tried & (1 << a) != 0;
    good.filter(|&g| g < MAX_ADAPTERS && !skip(g))
        .into_iter()
        .chain((0..MAX_ADAPTERS).filter(|&a| Some(a) != good && !skip(a)))
        .collect()
}

fn good_adapter() -> Option<u32> {
    Some(GOOD_ADAPTER.load(Ordering::Relaxed)).filter(|&a| a != u32::MAX)
}

/// Worker threads for the RGBA conversion (swscale slices the picture).
fn convert_threads() -> i64 {
    std::thread::available_parallelism().map_or(2, |n| n.get().min(4)) as i64
}

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
    fn d3d11va(adapter: u32) -> Result<Self, String> {
        let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
        let name = std::ffi::CString::new(adapter.to_string()).expect("digits only");
        // SAFETY: plain FFmpeg call; `device` is only used when it returns success.
        let r = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut device,
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        if r < 0 { Err(format!("cannot create a D3D11VA device on adapter {adapter} (error {r})")) } else { Ok(HwDevice(device)) }
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
    /// D3D11 adapter of the hardware decoder.
    adapter: Option<u32>,
    /// Bit mask of the adapters that already failed to decode in this decoder's lifetime.
    tried: u32,
    /// A keyframe was fed since the decoder was opened or last failed.
    synced: bool,
    produced: u64,
    scaler: Option<Scaler>,
    sw: ff::frame::Video,
}

/// swscale context converting a decoded picture to tightly packed RGBA straight into a `Vec`
/// (no intermediate frame and no row-by-row copy), sliced over several threads.
struct Scaler {
    ctx: *mut ffi::SwsContext,
    format: ffi::AVPixelFormat,
    width: u32,
    height: u32,
}

impl Scaler {
    fn new(format: ffi::AVPixelFormat, width: u32, height: u32) -> Result<Self, String> {
        // SAFETY: the context is allocated, configured through its AVOptions and initialised
        // before any use; it is freed on every error path and in `Drop`.
        unsafe {
            let ctx = ffi::sws_alloc_context();
            if ctx.is_null() {
                return Err("cannot allocate the scaler".into());
            }
            let set = |name: &std::ffi::CStr, v: i64| ffi::av_opt_set_int(ctx.cast(), name.as_ptr(), v, 0);
            set(c"srcw", i64::from(width));
            set(c"srch", i64::from(height));
            set(c"src_format", format as i64);
            set(c"dstw", i64::from(width));
            set(c"dsth", i64::from(height));
            set(c"dst_format", ffi::AVPixelFormat::AV_PIX_FMT_RGBA as i64);
            set(c"sws_flags", i64::from(ffi::SWS_BILINEAR));
            set(c"threads", convert_threads());
            let r = ffi::sws_init_context(ctx, std::ptr::null_mut(), std::ptr::null_mut());
            if r < 0 {
                ffi::sws_freeContext(ctx);
                return Err(format!("cannot initialise the scaler (error {r})"));
            }
            Ok(Scaler { ctx, format, width, height })
        }
    }

    fn matches(&self, format: ffi::AVPixelFormat, width: u32, height: u32) -> bool {
        self.format == format && self.width == width && self.height == height
    }

    /// Converts `src` (of the size and format this scaler was made for) to RGBA.
    fn convert(&mut self, src: &ff::frame::Video) -> Result<Vec<u8>, String> {
        let (w, h) = (self.width as usize, self.height as usize);
        // A few spare bytes: SIMD tails must never touch memory that is not ours.
        let mut data = vec![0u8; w * h * 4 + 64];
        let dst: [*mut u8; 4] = [data.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()];
        let dst_stride: [i32; 4] = [(w * 4) as i32, 0, 0, 0];
        // SAFETY: `src` is a valid AVFrame of the size and format given to `new`; `dst` points
        // to a buffer of `h` rows of `w * 4` bytes.
        let rows = unsafe {
            let f = &*src.as_ptr();
            ffi::sws_scale(
                self.ctx,
                f.data.as_ptr().cast(),
                f.linesize.as_ptr(),
                0,
                h as i32,
                dst.as_ptr(),
                dst_stride.as_ptr(),
            )
        };
        if rows != h as i32 {
            return Err(format!("the scaler produced {rows} of {h} rows"));
        }
        data.truncate(w * h * 4);
        Ok(data)
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        // SAFETY: we own the context.
        unsafe { ffi::sws_freeContext(self.ctx) };
    }
}

// SAFETY: FFmpeg codec/swscale contexts have no thread affinity; `VideoDecoder` is not `Sync` and
// every method takes `&mut self`, so it only ever moves to the decode thread.
unsafe impl Send for VideoDecoder {}

impl VideoDecoder {
    /// Opens D3D11VA decoding for `codec`, falling back to FFmpeg's software decoder.
    pub fn open(codec: VideoCodec) -> Result<Self, DecodeError> {
        init();
        match Self::open_hardware(codec, 0) {
            Ok(d) => Ok(d),
            Err(hw_err) => {
                tracing::info!(?codec, "hardware decoder unavailable ({hw_err}); using software");
                Self::open_inner(codec, None).map_err(|sw_err| DecodeError::NoDecoder(format!("hardware: {hw_err}; software: {sw_err}")))
            }
        }
    }

    /// Opens the software decoder only (tests, and the fallback after a hardware failure).
    pub fn open_software(codec: VideoCodec) -> Result<Self, DecodeError> {
        init();
        Self::open_inner(codec, None).map_err(DecodeError::NoDecoder)
    }

    /// The first D3D11VA adapter (see [`adapter_order`]) on which a decoder opens.
    fn open_hardware(codec: VideoCodec, tried: u32) -> Result<Self, String> {
        let mut errors = Vec::new();
        for adapter in adapter_order(good_adapter(), tried) {
            match Self::open_inner(codec, Some(adapter)) {
                Ok(mut d) => {
                    d.tried = tried;
                    return Ok(d);
                }
                Err(e) => errors.push(e),
            }
        }
        Err(if errors.is_empty() { "every adapter failed".to_string() } else { errors.join("; ") })
    }

    fn open_inner(codec: VideoCodec, adapter: Option<u32>) -> Result<Self, String> {
        let hardware = adapter.is_some();
        let name = decoder_name(codec);
        let found = ff::decoder::find_by_name(name).ok_or("decoder not built into this FFmpeg")?;
        let mut ctx = ff::codec::context::Context::new_with_codec(found);
        let mut device = None;
        if let Some(adapter) = adapter {
            let dev = HwDevice::d3d11va(adapter)?;
            // SAFETY: plain field writes on the context we own, before `open`.
            unsafe {
                let raw = ctx.as_mut_ptr();
                (*raw).hw_device_ctx = ffi::av_buffer_ref(dev.0);
                (*raw).get_format = Some(pick_d3d11);
            }
            device = Some(dev);
        }
        // SAFETY: plain field writes on the context we own, before `open`.
        unsafe {
            let raw = ctx.as_mut_ptr();
            if hardware {
                (*raw).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            } else {
                // LOW_DELAY switches frame threading off, which leaves the single-slice streams
                // of the hardware encoders on one core. Two frame threads (one frame of delay)
                // decode 1080p about 1.5x faster; more threads gained nothing in the bench.
                (*raw).thread_count = 2;
                (*raw).thread_type = ffi::FF_THREAD_FRAME;
            }
        }
        let dec = ctx.decoder().open().map_err(|e| e.to_string())?;
        let label = match adapter {
            Some(_) => format!("{name}+d3d11va"),
            None => format!("{name} (software)"),
        };
        tracing::info!(decoder = %label, ?adapter, "video decoder opened");
        Ok(VideoDecoder {
            dec,
            codec,
            hardware,
            name: label,
            _device: device,
            adapter,
            tried: 0,
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
                tracing::debug!(decoder = %self.name, "decode failed: {e}");
                if self.switch_decoder() {
                    // Same keyframe, new decoder: no round trip to the sender.
                    return self.decode(frame);
                }
                self.synced = false;
                self.dec.flush();
                DecodeOutput { frame: None, request_keyframe: true }
            }
        }
    }

    /// A hardware decoder that never produced a picture is replaced by the next D3D11 adapter and,
    /// when none is left, by the software one (the GPU may lack the codec profile although the
    /// probe opened it; on a laptop another GPU often has it). `true` when it was replaced.
    fn switch_decoder(&mut self) -> bool {
        let Some(adapter) = self.adapter.filter(|_| self.hardware && self.produced == 0) else {
            return false;
        };
        let tried = self.tried | (1 << adapter);
        match Self::open_hardware(self.codec, tried) {
            Ok(next) => {
                tracing::warn!(codec = ?self.codec, from = adapter, to = ?next.adapter, "hardware decode produced nothing; trying another adapter");
                *self = next;
                return true;
            }
            Err(e) => tracing::debug!("no other hardware decoder: {e}"),
        }
        match Self::open_inner(self.codec, None) {
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
        if let Some(a) = self.adapter.filter(|_| self.produced == 1) {
            GOOD_ADAPTER.store(a, Ordering::Relaxed);
        }
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
        let (w, h) = (src.width(), src.height());
        let fmt: ffi::AVPixelFormat = src.format().into();
        if w == 0 || h == 0 {
            return Err("empty picture".into());
        }
        if self.scaler.as_ref().is_none_or(|s| !s.matches(fmt, w, h)) {
            self.scaler = Some(Scaler::new(fmt, w, h)?);
        }
        let data = self.scaler.as_mut().expect("set above").convert(src)?;
        Ok(RgbaFrame { width: w, height: h, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{EncoderConfig, VideoEncoder};

    #[test]
    fn adapters_are_tried_good_first_then_by_index_skipping_failures() {
        assert_eq!(adapter_order(None, 0), vec![0, 1, 2, 3]);
        assert_eq!(adapter_order(Some(1), 0), vec![1, 0, 2, 3]);
        // The one that failed in this decoder's lifetime is not tried again, even when it was good.
        assert_eq!(adapter_order(Some(1), 0b0010), vec![0, 2, 3]);
        assert_eq!(adapter_order(None, 0b0001), vec![1, 2, 3]);
        assert!(adapter_order(None, 0b1111).is_empty());
        // A stale value outside the range is ignored.
        assert_eq!(adapter_order(Some(9), 0), vec![0, 1, 2, 3]);
    }

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
        // Frame threading delays the output by a frame: the keyframe's picture comes with the next packet.
        let delta = stream.iter().find(|f| !f.keyframe).unwrap();
        dec.decode(&stream[0]);
        assert!(dec.decode(delta).frame.is_some());
        let garbage = Frame { frame_id: 99, keyframe: false, timestamp: 1, data: vec![0xAB; 300] };
        let mut asked = false;
        for _ in 0..3 {
            asked |= dec.decode(&garbage).request_keyframe;
        }
        assert!(asked, "garbage never asked for a keyframe");
        // After the garbage, a delta must not be fed to a broken chain.
        assert!(dec.decode(delta).request_keyframe);
        dec.decode(&stream[0]);
        let again = dec.decode(delta);
        assert!(again.frame.is_some() && !again.request_keyframe);
    }
}
