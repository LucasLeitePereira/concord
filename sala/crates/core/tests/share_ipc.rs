//! T-033e1: `StartShare` / `StopShare` / `ShareState` through the IPC and the `Controller`
//! (`screen-share`, `ipc-message`, D-012 step 3). The video source is synthetic: no capture,
//! no encoder (the planner is injected).
#![cfg(windows)]

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sala_core::app::{Controller, PlatformApi, Step, Trigger, UiHost};
use sala_core::config::{Config, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::ipc::{ClientId, IpcClient, IpcServer};
use sala_core::platform::{Hotkey, HotkeyAction, TrayState};
use sala_core::session::{AudioSource, Session};
use sala_core::share::{Pipe, ShareLaunch, SharePlanner, VideoPipe};
use sala_core::share_audio::{AudioCapture, AudioPlanner};
use sala_protocol::{CoreToUi, MediaDatagram, UiToCore, VideoCodec};
use sala_video::abr::ReceiverReport;
use sala_video::preset::SharePreset;

struct NoPlatform;

impl PlatformApi for NoPlatform {
    fn set_tray(&self, _state: TrayState) {}
    fn notify_message(&self, _sender: &str) {}
    fn notify_call(&self, _sender: &str) {}
    fn set_hotkeys(&self, _hotkeys: Vec<(HotkeyAction, Hotkey)>) {}
    fn set_ptt(&self, _hotkey: Option<Hotkey>) {}
}

/// Raises `released` when dropped, i.e. when the session let go of the share.
struct FakePipe {
    _tx: SyncSender<Vec<MediaDatagram>>,
    released: Arc<AtomicBool>,
}

impl VideoPipe for FakePipe {
    fn request_keyframe(&self) {}
    fn report(&self, _report: ReceiverReport) -> u32 {
        1000
    }
    fn target_kbps(&self) -> u32 {
        1000
    }
}

impl Drop for FakePipe {
    fn drop(&mut self) {
        self.released.store(true, Ordering::SeqCst);
    }
}

type Calls = Arc<Mutex<Vec<(String, u32, u32)>>>;

/// Accepts `monitor:*` ids (the source is "1920x1080" so the height is capped at 1080);
/// anything else is an unknown source.
fn planner(calls: Calls, released: Arc<AtomicBool>) -> SharePlanner {
    Box::new(move |id, resolution, fps| {
        calls.lock().unwrap().push((id.to_string(), resolution, fps));
        if !id.starts_with("monitor:") {
            return Err("the source is gone".into());
        }
        let preset = SharePreset::custom(resolution, fps).ok_or("invalid quality")?;
        let released = released.clone();
        Ok(ShareLaunch {
            preset,
            resolution: resolution.min(1080),
            factory: Box::new(move |_params| {
                let (tx, rx) = mpsc::sync_channel(4);
                Ok(Pipe {
                    control: Box::new(FakePipe { _tx: tx, released }),
                    datagrams: rx,
                    codec: VideoCodec::H264,
                    width: 1280,
                    height: 720,
                    fps,
                })
            }),
        })
    })
}

fn temp_root() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sala-shipc-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Rig {
    ctl: Controller<NoPlatform>,
    rx: tokio::sync::mpsc::Receiver<(ClientId, UiToCore)>,
    client: IpcClient,
    calls: Calls,
    released: Arc<AtomicBool>,
}

async fn rig(keep_sharing: bool) -> Rig {
    let root = temp_root();
    let port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
    let profile = Profile::resolve_in(&root, Some("s")).unwrap();
    let mut config = Config::load_or_create(&profile).unwrap();
    config.port = port;
    config.game_mode.keep_sharing_in_game_mode = keep_sharing;
    config.save(&profile).unwrap();
    let identity = LocalIdentity::load_or_create(&profile.data_dir).unwrap();
    let game_cfg = config.game_mode.clone();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let session = Arc::new(Session::start(profile, config, &identity, addr, AudioSource::Synthetic).unwrap());
    let pipe = format!(r"\\.\pipe\sala-shipc-{}-{port}", std::process::id());
    let (ipc, rx) = IpcServer::start(&pipe).unwrap();
    let client = IpcClient::connect(&pipe).await.unwrap();
    let host = UiHost::new(ipc, pipe, PathBuf::from("does-not-exist.exe"));
    let mut ctl = Controller::new(session, host, NoPlatform, &game_cfg, false);
    let calls = Calls::default();
    let released = Arc::new(AtomicBool::new(false));
    ctl.set_share_planner(planner(calls.clone(), released.clone()));
    Rig { ctl, rx, client, calls, released }
}

impl Rig {
    /// The UI sends `cmd`; the controller handles it.
    async fn send(&mut self, cmd: UiToCore) {
        self.client.send(&cmd).await.unwrap();
        let (id, got) = tokio::time::timeout(Duration::from_secs(10), self.rx.recv()).await.unwrap().unwrap();
        assert_eq!(got, cmd);
        self.ctl.on_command(id, got).await;
    }

    /// Next message accepted by `pick` (others are skipped).
    async fn next<T>(&mut self, what: &str, pick: impl Fn(CoreToUi) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(left, self.client.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for: {what}"))
                .unwrap()
                .expect("ipc closed");
            if let Some(v) = pick(msg) {
                return v;
            }
        }
    }

    async fn share_state(&mut self) -> (bool, u32, u32) {
        self.next("ShareState", |m| match m {
            CoreToUi::ShareState { sharing, resolution, fps, .. } => Some((sharing, resolution, fps)),
            _ => None,
        })
        .await
    }

    async fn released(&self) -> bool {
        for _ in 0..100 {
            if self.released.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }
}

fn start_cmd(res: u32, fps: u32) -> UiToCore {
    UiToCore::StartShare { source_id: "monitor:1".into(), resolution: res, fps, audio: false }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_and_stop_share_are_reported_to_the_ui() {
    let mut r = rig(false).await;
    r.send(UiToCore::GetState).await;
    assert_eq!(r.share_state().await, (false, 0, 0), "GetState reports that nothing is shared");

    // The requested 1440 is capped by the (1080p) source: the UI is told the real height.
    r.send(start_cmd(1440, 60)).await;
    assert_eq!(r.share_state().await, (true, 1080, 60));
    assert!(r.ctl.sharing());
    assert!(r.ctl.session().local_status().sharing, "friends are told through Status");
    assert_eq!(r.calls.lock().unwrap().as_slice(), [("monitor:1".to_string(), 1440, 60)]);
    assert!(!r.released.load(Ordering::SeqCst));

    // A late-joining UI sees the share on GetState.
    r.send(UiToCore::GetState).await;
    assert_eq!(r.share_state().await, (true, 1080, 60));

    // A second share is refused and the first keeps running.
    r.send(start_cmd(720, 30)).await;
    let err = r.next("Error", |m| if let CoreToUi::Error { message } = m { Some(message) } else { None }).await;
    assert!(err.contains("transmitindo"), "{err}");
    assert!(r.ctl.sharing());

    r.send(UiToCore::StopShare).await;
    assert_eq!(r.share_state().await, (false, 0, 0));
    assert!(!r.ctl.session().local_status().sharing);
    assert!(r.released().await, "stopping must release capture and encoder");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_that_cannot_start_becomes_an_error() {
    let mut r = rig(false).await;
    r.send(UiToCore::StartShare { source_id: "window:9".into(), resolution: 720, fps: 30, audio: false }).await;
    let err = r.next("Error", |m| if let CoreToUi::Error { message } = m { Some(message) } else { None }).await;
    assert!(err.contains("the source is gone"), "{err}");
    assert!(!r.ctl.sharing());
    assert!(!r.ctl.session().local_status().sharing);
    // StopShare with nothing running is harmless.
    r.send(UiToCore::StopShare).await;
    assert!(!r.ctl.sharing());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn game_mode_stops_the_share_at_step_three() {
    let mut r = rig(false).await;
    r.send(start_cmd(720, 30)).await;
    assert_eq!(r.share_state().await, (true, 720, 30));
    r.ctl.clear_steps();
    r.ctl.set_game_mode_manual(true, Trigger::Hotkey).await;
    assert!(r.ctl.steps().contains(&Step::StopSharing));
    assert!(!r.ctl.sharing());
    assert!(!r.ctl.session().local_status().sharing);
    assert!(r.released().await, "entering game mode must release the video pipe");
    assert_eq!(r.share_state().await, (false, 0, 0));
}

/// T-033f1: what the fake audio planner did.
#[derive(Default)]
struct AudioProbe {
    opened: AtomicUsize,
    /// The capture guard was dropped (the capture stopped).
    released: AtomicBool,
}

/// Keeps the frame channel open (a closed one means "the capture ended") and flags the drop.
struct AudioGuard(Arc<AudioProbe>, #[allow(dead_code)] mpsc::Sender<Vec<f32>>);

impl Drop for AudioGuard {
    fn drop(&mut self) {
        self.0.released.store(true, Ordering::SeqCst);
    }
}

/// `fail`: the capture cannot open (old Windows, driver error...).
fn audio_planner(probe: Arc<AudioProbe>, fail: bool) -> AudioPlanner {
    Box::new(move || {
        let probe = probe.clone();
        Box::new(move || {
            if fail {
                return Err("process loopback unavailable".to_string());
            }
            probe.opened.fetch_add(1, Ordering::SeqCst);
            let (tx, frames) = mpsc::channel::<Vec<f32>>();
            Ok(AudioCapture { frames, guard: Box::new(AudioGuard(probe, tx)) })
        })
    })
}

fn start_audio_cmd() -> UiToCore {
    UiToCore::StartShare { source_id: "monitor:1".into(), resolution: 720, fps: 30, audio: true }
}

/// `ShareStats.audio` once the loop has taken the share over (it does so asynchronously).
async fn session_audio(r: &Rig) -> bool {
    for _ in 0..100 {
        if let Some(s) = r.ctl.session().share_stats() {
            return s.audio;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the session never reported the share");
}

async fn share_audio_flag(r: &mut Rig) -> bool {
    r.next("ShareState", |m| match m {
        CoreToUi::ShareState { audio, .. } => Some(audio),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_audio_is_reported_only_when_the_capture_opened_and_stops_with_the_share() {
    let mut r = rig(false).await;
    let probe = Arc::new(AudioProbe::default());
    r.ctl.set_audio_planner(audio_planner(probe.clone(), false));
    r.send(start_audio_cmd()).await;
    assert!(share_audio_flag(&mut r).await, "ShareState.audio must be true");
    assert_eq!(probe.opened.load(Ordering::SeqCst), 1);
    assert!(session_audio(&r).await, "the sender thread must flag the audio");
    // GetState tells late UIs too.
    r.send(UiToCore::GetState).await;
    assert!(share_audio_flag(&mut r).await);
    // The volume command is accepted at any time.
    r.send(UiToCore::SetShareAudioVolume { volume: 0.3 }).await;

    r.send(UiToCore::StopShare).await;
    assert!(!share_audio_flag(&mut r).await);
    for _ in 0..100 {
        if probe.released.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(probe.released.load(Ordering::SeqCst), "stopping the share must stop the capture");
}

/// Negative controls: no audio asked = the planner is never used; a capture that fails to open
/// leaves the video share running with `audio = false`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_without_audio_when_not_asked_or_when_the_capture_fails() {
    let mut r = rig(false).await;
    let probe = Arc::new(AudioProbe::default());
    r.ctl.set_audio_planner(audio_planner(probe.clone(), false));
    r.send(start_cmd(720, 30)).await;
    assert!(!share_audio_flag(&mut r).await);
    assert_eq!(probe.opened.load(Ordering::SeqCst), 0, "audio:false must not open the capture");
    r.send(UiToCore::StopShare).await;

    r.ctl.set_audio_planner(audio_planner(probe.clone(), true));
    r.send(start_audio_cmd()).await;
    assert!(!share_audio_flag(&mut r).await, "failed capture: audio must be false");
    assert!(r.ctl.sharing(), "the video share goes on without sound");
    assert!(!session_audio(&r).await);
}

/// Negative control: with `keep_sharing_in_game_mode` the share survives game mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn game_mode_keeps_the_share_when_configured() {
    let mut r = rig(true).await;
    r.send(start_cmd(720, 30)).await;
    assert_eq!(r.share_state().await, (true, 720, 30));
    r.ctl.clear_steps();
    r.ctl.set_game_mode_manual(true, Trigger::Hotkey).await;
    assert!(!r.ctl.steps().contains(&Step::StopSharing));
    assert!(r.ctl.sharing());
    assert!(r.ctl.session().local_status().sharing);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!r.released.load(Ordering::SeqCst));
}
