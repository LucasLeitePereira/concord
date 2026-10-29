//! WASAPI process loopback: the sound of the PC without the sound of Concord itself
//! (`screen-share`, T-033f1). Needs Windows 10 2004 or later.
//!
//! The virtual device `VAD\Process_Loopback` is activated in `EXCLUDE_TARGET_PROCESS_TREE` mode
//! with our own PID, so the friends' voices (played by this process) never go back to them. If
//! anything fails the caller shares without audio: the plain endpoint loopback is NOT a
//! fallback, it would echo the voices.

use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient,
    IAudioClient, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{BLOB, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, IAgileObject, IAgileObject_Impl};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, IUnknown, Interface, Ref, implement};

use crate::share_audio::{AudioCapture, CHANNELS, FRAME_VALUES};
use crate::voice::SAMPLE_RATE;

const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
/// Frames waiting for the encoder (1 s); older ones are dropped.
const QUEUE_FRAMES: usize = 50;
const FRAME: Duration = Duration::from_millis(20);
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Signals the capture thread when the activation ends. Agile: it is called from a COM thread.
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Handler {
    done: std_mpsc::SyncSender<()>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Handler_Impl {
    fn ActivateCompleted(&self, _operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.done.try_send(());
        Ok(())
    }
}

impl IAgileObject_Impl for Handler_Impl {}

/// The capture thread; dropping it stops the capture and waits for the thread.
struct Guard {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts capturing the PC's sound without Concord's own. `Err` carries the reason (the caller
/// logs it and shares without audio).
pub fn start() -> Result<AudioCapture, String> {
    let (frames_tx, frames_rx) = std_mpsc::sync_channel::<Vec<f32>>(QUEUE_FRAMES);
    let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("share-loopback".into())
            .spawn(move || run(&frames_tx, &stop, &ready_tx))
            .map_err(|e| format!("cannot start the capture thread: {e}"))?
    };
    let guard = Guard { stop, thread: Some(thread) };
    match ready_rx.recv_timeout(OPEN_TIMEOUT) {
        Ok(Ok(())) => Ok(AudioCapture { frames: frames_rx, guard: Box::new(guard) }),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            // The thread is stuck inside a driver call: do not wait for it.
            guard.stop.store(true, Ordering::Relaxed);
            let mut guard = guard;
            guard.thread = None;
            Err("timed out opening the process loopback".into())
        }
    }
}

fn run(frames: &std_mpsc::SyncSender<Vec<f32>>, stop: &AtomicBool, ready: &std_mpsc::Sender<Result<(), String>>) {
    // SAFETY: plain COM initialisation for this thread, undone at the end of `run`.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com.is_err() {
        let _ = ready.send(Err(format!("CoInitializeEx failed: {com:?}")));
        return;
    }
    match open() {
        Ok((client, capture, event)) => {
            let _ = ready.send(Ok(()));
            // SAFETY: the interfaces and the event belong to this thread until the end.
            unsafe { pump(&client, &capture, event, frames, stop) };
            // SAFETY: `Stop` on a started client; errors on shutdown are irrelevant.
            unsafe {
                let _ = client.Stop();
                let _ = CloseHandle(event);
            }
        }
        Err(e) => {
            let _ = ready.send(Err(e));
        }
    }
    // SAFETY: balances the successful `CoInitializeEx` above.
    unsafe { CoUninitialize() };
}

fn open() -> Result<(IAudioClient, IAudioCaptureClient, HANDLE), String> {
    let client = activate().map_err(|e| format!("process loopback unavailable (needs Windows 10 2004+): {e}"))?;
    let block_align = (CHANNELS * 4) as u16;
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    // SAFETY: `format` outlives the call; the event is closed by `run`.
    unsafe {
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 200_000, 0, &raw const format, None)
            .map_err(|e| format!("IAudioClient::Initialize failed: {e}"))?;
        let event = CreateEventW(None, false, false, None).map_err(|e| format!("CreateEventW failed: {e}"))?;
        let started = (|| -> windows::core::Result<IAudioCaptureClient> {
            client.SetEventHandle(event)?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(capture)
        })();
        match started {
            Ok(capture) => Ok((client, capture, event)),
            Err(e) => {
                let _ = CloseHandle(event);
                Err(format!("cannot start the loopback stream: {e}"))
            }
        }
    }
}

/// `ActivateAudioInterfaceAsync` on the process-loopback virtual device, excluding our PID.
fn activate() -> windows::core::Result<IAudioClient> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: std::process::id(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let mut variant = PROPVARIANT::default();
    variant.Anonymous = PROPVARIANT_0 {
        Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
            vt: VT_BLOB,
            wReserved1: 0,
            wReserved2: 0,
            wReserved3: 0,
            Anonymous: PROPVARIANT_0_0_0 {
                blob: BLOB { cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, pBlobData: (&raw mut params).cast() },
            },
        }),
    };
    // The blob points to the stack: the variant must never be cleared (that would free it).
    let variant = ManuallyDrop::new(variant);

    let (done_tx, done_rx) = std_mpsc::sync_channel::<()>(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Handler { done: done_tx }.into();
    // SAFETY: `params` and `variant` live until the operation completed (waited for below).
    let operation = unsafe {
        ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&raw const *variant), &handler)?
    };
    done_rx
        .recv_timeout(OPEN_TIMEOUT)
        .map_err(|_| windows::core::Error::new(HRESULT(0x8000_4005u32 as i32), "activation timed out"))?;
    let mut result = HRESULT(0);
    let mut unknown: Option<IUnknown> = None;
    // SAFETY: out parameters are valid locals.
    unsafe { operation.GetActivateResult(&mut result, &mut unknown)? };
    result.ok()?;
    unknown
        .ok_or_else(|| windows::core::Error::new(HRESULT(0x8000_4005u32 as i32), "no audio client"))?
        .cast::<IAudioClient>()
}

/// Reads packets into 20 ms frames until `stop`. When the PC plays nothing (no packets) a
/// silent frame is made every 20 ms so that the receiver's jitter buffer keeps its cadence.
///
/// # Safety
/// The interfaces and the event must be valid and used only from this thread.
unsafe fn pump(
    _client: &IAudioClient,
    capture: &IAudioCaptureClient,
    event: HANDLE,
    frames: &std_mpsc::SyncSender<Vec<f32>>,
    stop: &AtomicBool,
) {
    let mut pending: Vec<f32> = Vec::with_capacity(FRAME_VALUES * 4);
    let mut last_emit = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: valid event handle owned by this thread.
        unsafe { WaitForSingleObject(event, 20) };
        loop {
            // SAFETY: the buffer from `GetBuffer` is read before `ReleaseBuffer`.
            unsafe {
                let n = match capture.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!("share audio: GetNextPacketSize failed: {e}");
                        return;
                    }
                };
                if n == 0 {
                    break;
                }
                let (mut data, mut got, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
                if let Err(e) = capture.GetBuffer(&mut data, &mut got, &mut flags, None, None) {
                    tracing::warn!("share audio: GetBuffer failed: {e}");
                    return;
                }
                let values = got as usize * CHANNELS;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    pending.resize(pending.len() + values, 0.0);
                } else {
                    pending.extend_from_slice(std::slice::from_raw_parts(data.cast::<f32>(), values));
                }
                let _ = capture.ReleaseBuffer(got);
            }
        }
        while pending.len() >= FRAME_VALUES {
            let frame: Vec<f32> = pending.drain(..FRAME_VALUES).collect();
            let _ = frames.try_send(frame);
            last_emit = Instant::now();
        }
        if last_emit.elapsed() >= FRAME * 2 {
            let _ = frames.try_send(vec![0.0; FRAME_VALUES]);
            last_emit += FRAME;
            if last_emit.elapsed() > FRAME * 5 {
                last_emit = Instant::now();
            }
        }
    }
}
