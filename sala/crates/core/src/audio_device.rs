//! Audio devices through `cpal` (WASAPI on Windows).
//!
//! The cpal callbacks run on their own threads and never block: capture hands
//! 20 ms mono 48 kHz frames to tokio through `tokio::sync::mpsc::try_send`, and
//! playback receives frames from tokio through a bounded `std::sync::mpsc`
//! channel read with `try_recv` (D-002). Each stream lives on its own thread
//! (some backends' streams are not `Send`); dropping the handle stops it.
//!
//! Native formats (any rate, any channel count, f32/i16/i32/u16/f64) are
//! converted to/from 48 kHz mono with an average downmix and the linear
//! `Resampler` of `voice.rs`.

use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use tokio::sync::mpsc::Sender;

use crate::voice::{FRAME_SAMPLES, Resampler, SAMPLE_RATE, downmix_to_mono};

/// Frames queued towards the output device (1 s) before new ones are dropped.
const OUTPUT_QUEUE_FRAMES: usize = 50;
/// Device-side backlog (in 48 kHz mono samples) kept at most; older audio is discarded.
const OUTPUT_MAX_BACKLOG: usize = FRAME_SAMPLES * 10;

fn is_default(name: &str) -> bool {
    let n = name.trim();
    n.is_empty() || n.eq_ignore_ascii_case("default")
}

fn find_device(name: &str, input: bool) -> Result<Device, String> {
    let host = cpal::default_host();
    let default = if input { host.default_input_device() } else { host.default_output_device() };
    if is_default(name) {
        return default.ok_or_else(|| "no default audio device".to_string());
    }
    let wanted = name.trim().to_lowercase();
    if let Ok(devices) = host.devices() {
        for d in devices {
            let usable = if input { d.supports_input() } else { d.supports_output() };
            let matches = d
                .description()
                .map(|desc| desc.name().to_lowercase() == wanted)
                .unwrap_or(false);
            if usable && matches {
                return Ok(d);
            }
        }
    }
    tracing::warn!(device = name, "audio device not found, using the default one");
    default.ok_or_else(|| "no default audio device".to_string())
}

/// Names of the available devices (for the settings UI).
pub fn list_devices(input: bool) -> Vec<String> {
    let host = cpal::default_host();
    let Ok(devices) = host.devices() else { return Vec::new() };
    devices
        .filter(|d| if input { d.supports_input() } else { d.supports_output() })
        .filter_map(|d| d.description().ok().map(|x| x.name().to_string()))
        .collect()
}

/// Runs `build` on a new thread, waits for its result, then parks the stream
/// there until `stop` is signalled (the sender being dropped counts).
struct StreamThread {
    stop: Option<std_mpsc::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl StreamThread {
    fn spawn<F>(build: F) -> Result<Self, String>
    where
        F: FnOnce() -> Result<cpal::Stream, String> + Send + 'static,
    {
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let join = std::thread::Builder::new()
            .name("audio-stream".into())
            .spawn(move || match build() {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stop_rx.recv(); // returns on signal or when the handle is dropped
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop: Some(stop_tx), join: Some(join) }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => Err("audio thread died while starting".into()),
        }
    }
}

impl Drop for StreamThread {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Running microphone capture. Frames of `FRAME_SAMPLES` mono samples at 48 kHz.
pub struct AudioInput {
    _thread: StreamThread,
}

impl AudioInput {
    /// `device`: name from `AudioConfig::input_device`; empty / "default" = system default.
    pub fn start(device: &str, frames: Sender<Vec<f32>>) -> Result<Self, String> {
        let device = device.to_string();
        let thread = StreamThread::spawn(move || {
            let dev = find_device(&device, true)?;
            let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
            tracing::info!(
                device = dev.description().map(|d| d.name().to_string()).unwrap_or_default(),
                rate = cfg.sample_rate(),
                channels = cfg.channels(),
                format = ?cfg.sample_format(),
                "audio input"
            );
            let fmt = cfg.sample_format();
            let sc: StreamConfig = cfg.into();
            let stream = match fmt {
                SampleFormat::F32 => build_input::<f32>(&dev, sc, frames),
                SampleFormat::I16 => build_input::<i16>(&dev, sc, frames),
                SampleFormat::I32 => build_input::<i32>(&dev, sc, frames),
                SampleFormat::U16 => build_input::<u16>(&dev, sc, frames),
                SampleFormat::F64 => build_input::<f64>(&dev, sc, frames),
                other => Err(format!("unsupported input sample format {other}")),
            }?;
            stream.play().map_err(|e| e.to_string())?;
            Ok(stream)
        })?;
        Ok(Self { _thread: thread })
    }
}

fn build_input<T>(dev: &Device, sc: StreamConfig, frames: Sender<Vec<f32>>) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(sc.channels);
    let mut resampler = Resampler::new(sc.sample_rate, SAMPLE_RATE);
    let mut floats: Vec<f32> = Vec::new();
    let mut mono: Vec<f32> = Vec::new();
    let mut pending: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 2);
    dev.build_input_stream(
        sc,
        move |data: &[T], _: &_| {
            floats.clear();
            floats.extend(data.iter().map(|s| f32::from_sample(*s)));
            mono.clear();
            downmix_to_mono(&floats, channels, &mut mono);
            resampler.process(&mono, &mut pending);
            while pending.len() >= FRAME_SAMPLES {
                let frame: Vec<f32> = pending.drain(..FRAME_SAMPLES).collect();
                // Full channel = consumer is behind: drop the frame, never block.
                let _ = frames.try_send(frame);
            }
        },
        |e| tracing::warn!("audio input stream error: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

/// Running playback. Push 20 ms mono 48 kHz frames with `play`.
pub struct AudioOutput {
    tx: std_mpsc::SyncSender<Vec<f32>>,
    _thread: StreamThread,
}

impl AudioOutput {
    pub fn start(device: &str) -> Result<Self, String> {
        let device = device.to_string();
        let (tx, rx) = std_mpsc::sync_channel::<Vec<f32>>(OUTPUT_QUEUE_FRAMES);
        let thread = StreamThread::spawn(move || {
            let dev = find_device(&device, false)?;
            let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
            tracing::info!(
                device = dev.description().map(|d| d.name().to_string()).unwrap_or_default(),
                rate = cfg.sample_rate(),
                channels = cfg.channels(),
                format = ?cfg.sample_format(),
                "audio output"
            );
            let fmt = cfg.sample_format();
            let sc: StreamConfig = cfg.into();
            let stream = match fmt {
                SampleFormat::F32 => build_output::<f32>(&dev, sc, rx),
                SampleFormat::I16 => build_output::<i16>(&dev, sc, rx),
                SampleFormat::I32 => build_output::<i32>(&dev, sc, rx),
                SampleFormat::U16 => build_output::<u16>(&dev, sc, rx),
                SampleFormat::F64 => build_output::<f64>(&dev, sc, rx),
                other => Err(format!("unsupported output sample format {other}")),
            }?;
            stream.play().map_err(|e| e.to_string())?;
            Ok(stream)
        })?;
        Ok(Self { tx, _thread: thread })
    }

    /// Queues one frame; drops it when the device is far behind. Never blocks.
    pub fn play(&self, frame: Vec<f32>) {
        let _ = self.tx.try_send(frame);
    }
}

fn build_output<T>(
    dev: &Device,
    sc: StreamConfig,
    rx: std_mpsc::Receiver<Vec<f32>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(sc.channels);
    let mut resampler = Resampler::new(SAMPLE_RATE, sc.sample_rate);
    // Mono samples at the device rate, waiting to be written.
    let mut queue: VecDeque<f32> = VecDeque::new();
    let mut scratch: Vec<f32> = Vec::new();
    dev.build_output_stream(
        sc,
        move |data: &mut [T], _: &_| {
            while let Ok(frame) = rx.try_recv() {
                scratch.clear();
                resampler.process(&frame, &mut scratch);
                queue.extend(scratch.iter().copied());
            }
            let max_backlog = OUTPUT_MAX_BACKLOG * 2; // device rate may exceed 48 kHz a bit
            if queue.len() > max_backlog {
                let excess = queue.len() - max_backlog;
                queue.drain(..excess);
            }
            for out in data.chunks_mut(channels) {
                let v: T = T::from_sample(queue.pop_front().unwrap_or(0.0));
                out.fill(v);
            }
        },
        |e| tracing::warn!("audio output stream error: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

/// Where decoded voice goes. `Null` swallows frames (no device, `--synthetic-audio`).
pub enum Sink {
    Null,
    Device(AudioOutput),
}

impl Sink {
    /// Opens the output device; any failure degrades to the null sink (never panics).
    pub fn open(device: &str) -> Sink {
        match AudioOutput::start(device) {
            Ok(o) => Sink::Device(o),
            Err(e) => {
                tracing::warn!("no audio output ({e}); voice will not be played");
                Sink::Null
            }
        }
    }

    pub fn play(&self, frame: Vec<f32>) {
        if let Sink::Device(o) = self {
            o.play(frame);
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Sink::Null)
    }
}
