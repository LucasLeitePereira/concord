//! BGRA to NV12 conversion (swscale) that reads straight from the caller's buffer and runs on
//! several threads. T-035f: the wrapper in `ffmpeg-next` needs an owned, padded input frame
//! (an extra 8 MB copy per 1080p frame) and its context is single-threaded.

use std::ffi::{CString, c_void};

use ffmpeg_next as ff;
use ff::ffi;

/// Pixels per input row for which swscale's SIMD never reads past the end of the row (64 bytes
/// of BGRA). With other widths the last row of the caller's buffer could be over-read, so those
/// frames go through a padded copy.
const SAFE_WIDTH_MULTIPLE: u32 = 16;

/// Number of swscale worker threads for this machine: half the logical cores, 1 to 4.
pub fn default_threads() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).clamp(1, 4) as u32)
}

/// A swscale context BGRA (`src_w` x `src_h`) to NV12 (`dst_w` x `dst_h`, bilinear).
pub struct BgraToNv12 {
    ctx: *mut ffi::SwsContext,
    src_w: u32,
    src_h: u32,
    /// Padded copy of the input, only for widths that are not a multiple of
    /// [`SAFE_WIDTH_MULTIPLE`].
    padded: Option<ff::frame::Video>,
}

// SAFETY: the swscale context is a plain heap object with no thread affinity (its own worker
// threads are owned by FFmpeg); every method takes `&mut self`, so one thread uses it at a time.
unsafe impl Send for BgraToNv12 {}

impl Drop for BgraToNv12 {
    fn drop(&mut self) {
        // SAFETY: `ctx` came from `sws_alloc_context` and is freed exactly once, here.
        unsafe { ffi::sws_freeContext(self.ctx) };
    }
}

fn set_int(ctx: *mut ffi::SwsContext, name: &str, value: i64) -> Result<(), String> {
    let name = CString::new(name).expect("static option name");
    // SAFETY: `ctx` is a live SwsContext (an AVClass-first object, as `av_opt_set_int` needs)
    // and `name` outlives the call.
    let r = unsafe { ffi::av_opt_set_int(ctx.cast::<c_void>(), name.as_ptr(), value, 0) };
    if r < 0 { Err(format!("swscale option {name:?} refused (error {r})")) } else { Ok(()) }
}

/// Free callback of the borrowed input buffer: the memory belongs to the caller.
unsafe extern "C" fn keep_data(_opaque: *mut c_void, _data: *mut u8) {}

impl BgraToNv12 {
    pub fn new(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32, threads: u32) -> Result<Self, String> {
        // SAFETY: plain FFmpeg calls; the context is owned by `Self` from here on (freed by Drop,
        // including on the error paths below).
        let ctx = unsafe { ffi::sws_alloc_context() };
        if ctx.is_null() {
            return Err("cannot allocate the swscale context".into());
        }
        let mut me = BgraToNv12 { ctx, src_w, src_h, padded: None };
        let bgra = ffi::AVPixelFormat::AV_PIX_FMT_BGRA as i64;
        let nv12 = ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i64;
        set_int(ctx, "srcw", i64::from(src_w))?;
        set_int(ctx, "srch", i64::from(src_h))?;
        set_int(ctx, "src_format", bgra)?;
        set_int(ctx, "dstw", i64::from(dst_w))?;
        set_int(ctx, "dsth", i64::from(dst_h))?;
        set_int(ctx, "dst_format", nv12)?;
        set_int(ctx, "sws_flags", i64::from(ffi::SWS_BILINEAR))?;
        set_int(ctx, "threads", i64::from(threads.max(1)))?;
        // SAFETY: fully configured context, no filters.
        let r = unsafe { ffi::sws_init_context(ctx, std::ptr::null_mut(), std::ptr::null_mut()) };
        if r < 0 {
            return Err(format!("cannot initialise swscale (error {r})"));
        }
        if !src_w.is_multiple_of(SAFE_WIDTH_MULTIPLE) {
            me.padded = Some(ff::frame::Video::new(ff::format::Pixel::BGRA, src_w, src_h));
        }
        Ok(me)
    }

    pub fn src_size(&self) -> (u32, u32) {
        (self.src_w, self.src_h)
    }

    /// Whether the input is first copied into a padded frame (see [`SAFE_WIDTH_MULTIPLE`]).
    pub fn copies_input(&self) -> bool {
        self.padded.is_some()
    }

    /// Converts `bgra` (`src_w * src_h * 4` bytes, no row padding) into `dst`, an NV12 frame of
    /// the output size. Returns the microseconds spent copying the input (0 on the direct path).
    pub fn convert(&mut self, bgra: &[u8], dst: &mut ff::frame::Video) -> Result<u64, String> {
        let (w, h) = (self.src_w as usize, self.src_h as usize);
        if bgra.len() != w * h * 4 {
            return Err("input does not match the converter size".into());
        }
        if let Some(frame) = &mut self.padded {
            let t0 = std::time::Instant::now();
            let stride = frame.stride(0);
            let plane = frame.data_mut(0);
            for y in 0..h {
                plane[y * stride..y * stride + w * 4].copy_from_slice(&bgra[y * w * 4..(y + 1) * w * 4]);
            }
            let copy_micros = t0.elapsed().as_micros() as u64;
            // SAFETY: both frames are live, refcounted and of the sizes the context was set up with.
            let r = unsafe { ffi::sws_scale_frame(self.ctx, dst.as_mut_ptr(), frame.as_ptr()) };
            return if r < 0 { Err(format!("swscale failed (error {r})")) } else { Ok(copy_micros) };
        }
        // SAFETY: `src` is a frame header of our own whose only buffer is a borrowed view of
        // `bgra` (READONLY, free callback that frees nothing). `sws_scale_frame` finishes with
        // the frame (`sws_frame_end` drops its references) before returning, and `av_frame_free`
        // drops ours, so nothing outlives this call and `bgra` is never written. The row length
        // is a multiple of 64 bytes (`SAFE_WIDTH_MULTIPLE`), so swscale's vector loads stay
        // inside the buffer.
        unsafe {
            let mut src = ffi::av_frame_alloc();
            if src.is_null() {
                return Err("cannot allocate a frame".into());
            }
            (*src).format = ffi::AVPixelFormat::AV_PIX_FMT_BGRA as i32;
            (*src).width = w as i32;
            (*src).height = h as i32;
            (*src).data[0] = bgra.as_ptr().cast_mut();
            (*src).linesize[0] = (w * 4) as i32;
            let buf = ffi::av_buffer_create(
                bgra.as_ptr().cast_mut(),
                bgra.len(),
                Some(keep_data),
                std::ptr::null_mut(),
                ffi::AV_BUFFER_FLAG_READONLY,
            );
            if buf.is_null() {
                ffi::av_frame_free(&mut src);
                return Err("cannot wrap the input buffer".into());
            }
            (*src).buf[0] = buf;
            let r = ffi::sws_scale_frame(self.ctx, dst.as_mut_ptr(), src);
            ffi::av_frame_free(&mut src);
            if r < 0 { Err(format!("swscale failed (error {r})")) } else { Ok(0) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Colourful, non-periodic test image (BGRA, no row padding).
    fn image(w: u32, h: u32, shift: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let x = x + shift;
                v.extend_from_slice(&[(x * 255 / (w + 8)) as u8, (y * 255 / h) as u8, ((x * 7 + y * 13) % 256) as u8, 255]);
            }
        }
        v
    }

    /// The conversion the encoder used before T-035f: owned padded frame + `ffmpeg-next` context.
    fn reference(bgra: &[u8], w: u32, h: u32, dw: u32, dh: u32) -> ff::frame::Video {
        let mut src = ff::frame::Video::new(ff::format::Pixel::BGRA, w, h);
        let stride = src.stride(0);
        for y in 0..h as usize {
            src.data_mut(0)[y * stride..y * stride + w as usize * 4]
                .copy_from_slice(&bgra[y * w as usize * 4..(y + 1) * w as usize * 4]);
        }
        let mut sws = ff::software::scaling::Context::get(
            ff::format::Pixel::BGRA,
            w,
            h,
            ff::format::Pixel::NV12,
            dw,
            dh,
            ff::software::scaling::Flags::BILINEAR,
        )
        .unwrap();
        let mut dst = ff::frame::Video::new(ff::format::Pixel::NV12, dw, dh);
        sws.run(&src, &mut dst).unwrap();
        dst
    }

    /// Visible bytes of both NV12 planes (luma, then interleaved chroma).
    fn planes(f: &ff::frame::Video, w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for (plane, rows) in [(0usize, h as usize), (1, h as usize / 2)] {
            let stride = f.stride(plane);
            for y in 0..rows {
                out.extend_from_slice(&f.data(plane)[y * stride..y * stride + w as usize]);
            }
        }
        out
    }

    fn convert(bgra: &[u8], w: u32, h: u32, dw: u32, dh: u32, threads: u32) -> (Vec<u8>, bool) {
        let mut sws = BgraToNv12::new(w, h, dw, dh, threads).unwrap();
        let mut dst = ff::frame::Video::new(ff::format::Pixel::NV12, dw, dh);
        let copied = sws.copies_input();
        sws.convert(bgra, &mut dst).unwrap();
        (planes(&dst, dw, dh), copied)
    }

    #[test]
    fn matches_the_previous_conversion_byte_for_byte() {
        // (source size, output size): direct path, padded-copy path (66 is not a multiple of 16),
        // and a downscale.
        for ((w, h), (dw, dh)) in [((64, 36), (64, 36)), ((66, 36), (66, 36)), ((128, 72), (64, 36))] {
            let img = image(w, h, 0);
            let want = planes(&reference(&img, w, h, dw, dh), dw, dh);
            for threads in [1, 3] {
                let (got, copied) = convert(&img, w, h, dw, dh, threads);
                assert_eq!(copied, !w.is_multiple_of(16));
                assert_eq!(got, want, "{w}x{h} -> {dw}x{dh}, {threads} threads");
            }
        }
    }

    /// Control: the comparison above is sensitive to the pixels, so a wrong stride or format in
    /// the new path (which shifts or scrambles them) cannot go unnoticed.
    #[test]
    fn the_comparison_notices_different_pixels() {
        let (w, h) = (64, 36);
        let want = planes(&reference(&image(w, h, 0), w, h, w, h), w, h);
        assert_ne!(convert(&image(w, h, 1), w, h, w, h, 1).0, want);
        // And the planes are really in use: not an all-zero buffer on both sides.
        assert!(want.iter().any(|&b| b > 16));
    }

    #[test]
    fn rejects_a_buffer_of_the_wrong_size() {
        let mut sws = BgraToNv12::new(64, 36, 64, 36, 1).unwrap();
        let mut dst = ff::frame::Video::new(ff::format::Pixel::NV12, 64, 36);
        assert!(sws.convert(&image(64, 35, 0), &mut dst).is_err());
    }
}
