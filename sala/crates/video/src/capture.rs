//! Screen/window capture with Windows Graphics Capture (`screen-capture`), through the
//! `windows-capture` crate. Delivers tightly packed BGRA frames to a callback.
//!
//! TODO(zero-copy D3D11): each frame is copied GPU -> CPU staging texture -> `Vec` and later
//! converted to NV12 by swscale. The zero-copy path would hand the captured `ID3D11Texture2D`
//! straight to the hardware encoder (FFmpeg `d3d11` hw frames context) and skip both copies.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
    SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

/// What can be captured. The id is the native handle value (`HMONITOR` / `HWND`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Monitor(usize),
    Window(usize),
}

/// One entry of the "what to share" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub target: Target,
    pub name: String,
    /// Pixel size (monitors only; windows report 0 x 0 when unknown).
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

pub type CaptureError = Box<dyn std::error::Error + Send + Sync>;

/// A captured frame, valid only inside the callback.
pub struct BgraFrame<'a> {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, no row padding.
    pub data: &'a [u8],
    /// Capture clock (WGC `SystemRelativeTime`), monotonic.
    pub timestamp: Duration,
}

/// Monitors, primary first.
pub fn list_monitors() -> Result<Vec<Source>, CaptureError> {
    let primary = Monitor::primary().ok().map(|m| m.as_raw_hmonitor() as usize);
    let mut out: Vec<Source> = Monitor::enumerate()?
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let id = m.as_raw_hmonitor() as usize;
            let name = m.name().or_else(|_| m.device_name()).unwrap_or_else(|_| format!("Monitor {}", i + 1));
            Source {
                target: Target::Monitor(id),
                name,
                width: m.width().unwrap_or(0),
                height: m.height().unwrap_or(0),
                primary: Some(id) == primary,
            }
        })
        .collect();
    out.sort_by_key(|s| !s.primary);
    Ok(out)
}

/// Visible top-level windows that have a title.
pub fn list_windows() -> Result<Vec<Source>, CaptureError> {
    Ok(Window::enumerate()?
        .into_iter()
        .filter_map(|w| {
            let title = w.title().ok()?;
            if title.trim().is_empty() {
                return None;
            }
            Some(Source {
                target: Target::Window(w.as_raw_hwnd() as usize),
                name: title,
                width: w.width().unwrap_or(0).max(0) as u32,
                height: w.height().unwrap_or(0).max(0) as u32,
                primary: false,
            })
        })
        .collect())
}

type FrameCallback = Box<dyn FnMut(&BgraFrame<'_>) + Send>;

/// What the capture callback measures (shared with the owner of the session).
#[derive(Default)]
struct CaptureCounters {
    frames: AtomicU64,
    read_micros: AtomicU64,
    deliver_micros: AtomicU64,
    /// Longest read since the last [`CaptureSession::stats`] call.
    read_micros_max: AtomicU64,
}

/// Capture callback counters. Totals count since the start; `read_micros_max` is the longest
/// read since the previous snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureStats {
    /// Frames WGC delivered to the callback.
    pub frames: u64,
    /// Time spent reading the frame (`frame.buffer()` + `as_nopadding_buffer`: GPU -> CPU copy).
    pub read_micros: u64,
    /// Time spent in the `on_frame` callback (handing the frame off).
    pub deliver_micros: u64,
    pub read_micros_max: u64,
}

struct Handler {
    on_frame: FrameCallback,
    counters: Arc<CaptureCounters>,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = (FrameCallback, Arc<CaptureCounters>);
    type Error = CaptureError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let (on_frame, counters) = ctx.flags;
        Ok(Handler { on_frame, counters, scratch: Vec::new() })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, _control: InternalCaptureControl) -> Result<(), Self::Error> {
        let (width, height) = (frame.width(), frame.height());
        // 100 ns ticks.
        let ticks = frame.timestamp()?.Duration.max(0) as u64;
        let timestamp = Duration::from_nanos(ticks.saturating_mul(100));
        let t0 = Instant::now();
        let buffer = frame.buffer()?;
        let data = buffer.as_nopadding_buffer(&mut self.scratch);
        let t1 = Instant::now();
        (self.on_frame)(&BgraFrame { width, height, data, timestamp });
        let c = &self.counters;
        let read = (t1 - t0).as_micros() as u64;
        c.frames.fetch_add(1, Ordering::Relaxed);
        c.read_micros.fetch_add(read, Ordering::Relaxed);
        c.read_micros_max.fetch_max(read, Ordering::Relaxed);
        c.deliver_micros.fetch_add(t1.elapsed().as_micros() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        tracing::info!("capture item closed");
        Ok(())
    }
}

/// A running capture; dropping it (or `stop`) ends it.
pub struct CaptureSession {
    control: Option<windows_capture::capture::CaptureControl<Handler, CaptureError>>,
    counters: Arc<CaptureCounters>,
}

impl CaptureSession {
    /// Starts capturing `target` on a background thread. `on_frame` runs on that thread for
    /// every frame; keep it quick (hand the data off). `max_fps` throttles WGC itself.
    pub fn start(
        target: Target,
        max_fps: u32,
        on_frame: impl FnMut(&BgraFrame<'_>) + Send + 'static,
    ) -> Result<Self, CaptureError> {
        let counters = Arc::new(CaptureCounters::default());
        let cb: (FrameCallback, Arc<CaptureCounters>) = (Box::new(on_frame), counters.clone());
        let interval = if max_fps == 0 {
            MinimumUpdateIntervalSettings::Default
        } else {
            MinimumUpdateIntervalSettings::Custom(Duration::from_micros(1_000_000 / u64::from(max_fps)))
        };
        let control = match target {
            Target::Monitor(id) => Handler::start_free_threaded(Settings::new(
                Monitor::from_raw_hmonitor(id as *mut std::ffi::c_void),
                CursorCaptureSettings::WithCursor,
                DrawBorderSettings::WithoutBorder,
                SecondaryWindowSettings::Default,
                interval,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                cb,
            ))?,
            Target::Window(id) => Handler::start_free_threaded(Settings::new(
                Window::from_raw_hwnd(id as *mut std::ffi::c_void),
                CursorCaptureSettings::WithCursor,
                DrawBorderSettings::WithoutBorder,
                SecondaryWindowSettings::Default,
                interval,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                cb,
            ))?,
        };
        Ok(CaptureSession { control: Some(control), counters })
    }

    /// Counters of the capture callback; resets the `read_micros_max` window.
    pub fn stats(&self) -> CaptureStats {
        let c = &self.counters;
        CaptureStats {
            frames: c.frames.load(Ordering::Relaxed),
            read_micros: c.read_micros.load(Ordering::Relaxed),
            deliver_micros: c.deliver_micros.load(Ordering::Relaxed),
            read_micros_max: c.read_micros_max.swap(0, Ordering::Relaxed),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.control.as_ref().is_none_or(|c| c.is_finished())
    }

    pub fn stop(mut self) -> Result<(), CaptureError> {
        match self.control.take() {
            Some(c) => c.stop().map_err(|e| -> CaptureError { e.to_string().into() }),
            None => Ok(()),
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        if let Some(c) = self.control.take() {
            let _ = c.stop();
        }
    }
}
