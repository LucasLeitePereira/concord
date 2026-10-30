//! The single orchestrator of the core process (D-012): wires `Session`, `IpcServer`,
//! the UI process, `Platform` and `GameDetector`. Hotkeys, tray menu and IPC
//! commands all end up in the same [`Controller`] methods.
//!
//! The decisions live in [`Controller`], which only talks to its dependencies
//! (`Session`, [`UiHost`], a [`PlatformApi`]); tests run it with a fake platform
//! and the real `fake_ui` example. [`run`] adds the real Win32 platform, the
//! hotkey set-up and the event loop.
//!
//! Chat text is never logged (`inv-chat-never-on-disk`).

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sala_protocol::{AudioSettings, CoreToUi, GameModeSettings, HotkeySettings, UiToCore};
use sala_video::viewer::{self, Incoming, KeyframeRequester, ViewerHandle};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};

use crate::audio_device::list_devices;
use crate::config::{AudioMode, Config, GameModeConfig, Profile};
use crate::cues::Cue;
use crate::game_detector::{ForegroundSample, GameDetector, POLL_INTERVAL, Transition};
use crate::identity::LocalIdentity;
use crate::ipc::{ClientId, IpcClient, IpcError, IpcServer, current_user_pipe_name};
use crate::platform::{self, Hotkey, HotkeyAction, MenuItem, Platform, PlatformEvent, TrayState};
use crate::session::{AudioSource, Session, SessionEvent, StatusFlags};
use crate::share::{SharePlanner, VideoFrame, list_share_sources, real_share_planner};
use crate::share_audio::{AudioPlanner, real_audio_planner};
use crate::settings;
use crate::ui_process::{SHUTDOWN_TIMEOUT, ShutdownOutcome, UiProcess, resolve_ui_exe};
use crate::voice::clamp_volume;

/// How long "Quit" waits for the session (Bye to friends) before leaving anyway.
const SESSION_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The part of [`Platform`] the controller needs (fakeable in tests).
pub trait PlatformApi {
    fn set_tray(&self, state: TrayState);
    /// Sender name only, never the text (D-011).
    fn notify_message(&self, sender: &str);
    /// "X está te chamando" balloon (game mode, D-024).
    fn notify_call(&self, sender: &str);
    /// Replaces the global shortcuts.
    fn set_hotkeys(&self, hotkeys: Vec<(HotkeyAction, Hotkey)>);
    /// Replaces the push-to-talk key (`None` = none).
    fn set_ptt(&self, hotkey: Option<Hotkey>);
}

impl PlatformApi for Platform {
    fn set_tray(&self, state: TrayState) {
        Platform::set_tray(self, state);
    }
    fn notify_message(&self, sender: &str) {
        Platform::notify_message(self, sender);
    }
    fn notify_call(&self, sender: &str) {
        Platform::notify_call(self, sender);
    }
    fn set_hotkeys(&self, hotkeys: Vec<(HotkeyAction, Hotkey)>) {
        Platform::set_hotkeys(self, hotkeys);
    }
    fn set_ptt(&self, hotkey: Option<Hotkey>) {
        Platform::set_ptt(self, hotkey);
    }
}

/// What caused a game-mode transition; logged on every enter/exit so the log
/// tells automatic from manual changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// The detector; carries the file name of the sample that fired it.
    Auto { exe: String },
    Hotkey,
    /// Tray menu entry "Entrar/Sair do modo jogo".
    Menu,
    /// Tray menu entry "Abrir o Concord".
    TrayClick,
    /// Click on the chat notification.
    Notification,
    /// `EnterGameMode` / `ExitGameMode` from the UI.
    Ipc,
    /// `OpenUi` (a second `sala-core` was started).
    SecondInstance,
}

impl Trigger {
    /// Automatic trigger for the sample of this tick (`desconhecido` when there is no name).
    pub fn auto(sample: Option<&ForegroundSample>) -> Trigger {
        let name = sample
            .and_then(|s| s.exe_path.rsplit(['\\', '/']).next())
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("desconhecido");
        Trigger::Auto { exe: name.to_string() }
    }
}

impl std::fmt::Display for Trigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Trigger::Auto { exe } => write!(f, "auto ({exe})"),
            Trigger::Hotkey => f.write_str("hotkey"),
            Trigger::Menu => f.write_str("menu"),
            Trigger::TrayClick => f.write_str("tray_click"),
            Trigger::Notification => f.write_str("notification"),
            Trigger::Ipc => f.write_str("ipc"),
            Trigger::SecondInstance => f.write_str("second_instance"),
        }
    }
}

/// One step of the game-mode sequences (D-012), recorded in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Last step of a sequence: why it ran (also logged at its start).
    Cause { entering: bool, trigger: Trigger },
    /// Step 1 of entering: the UI was asked to quit.
    UiShutdown(ShutdownOutcome),
    /// Step 2 (extension point, F4).
    CloseViewer,
    /// Step 3: the screen share was stopped; not run with `keep_sharing_in_game_mode`.
    StopSharing,
    /// Step 4: working set handed back to the OS.
    ReleaseMemory,
    /// `Status` sent to the friends (`game_mode` is the new value).
    StatusSent { game_mode: bool },
    /// Tray icon updated (`true` = amber).
    Tray { game_mode: bool },
    /// Exiting: the UI was launched again (`true` = a process was started).
    UiLaunched(bool),
}

/// The UI process and the IPC server it talks to.
pub struct UiHost {
    ipc: IpcServer,
    pipe: String,
    exe: PathBuf,
    ui: Option<UiProcess>,
}

impl UiHost {
    pub fn new(ipc: IpcServer, pipe: String, exe: PathBuf) -> Self {
        UiHost { ipc, pipe, exe, ui: None }
    }

    pub fn ipc(&self) -> &IpcServer {
        &self.ipc
    }

    pub fn is_running(&mut self) -> bool {
        self.ui.as_mut().is_some_and(UiProcess::is_running)
    }

    /// Starts the UI unless it is already running. True if a process was started.
    pub fn launch(&mut self) -> bool {
        if self.is_running() {
            return false;
        }
        match UiProcess::spawn(&self.exe, &self.pipe) {
            Ok(p) => {
                self.ui = Some(p);
                true
            }
            Err(e) => {
                warn!("cannot launch the UI: {e}");
                false
            }
        }
    }

    pub async fn shutdown_ui(&mut self) -> ShutdownOutcome {
        match self.ui.take() {
            Some(mut ui) => ui.shutdown(&self.ipc, SHUTDOWN_TIMEOUT).await,
            None => ShutdownOutcome::NotRunning,
        }
    }
}

/// What the event loop must do after a handled event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// The screen-share viewer window as the controller sees it (`screen-share`).
pub trait ViewerWindow: Send {
    /// Still on screen (turns `false` when the user closes it).
    fn is_open(&self) -> bool;
    /// Closes the window and frees the GPU.
    fn close(self: Box<Self>);
}

/// Opens a viewer on the given frame channel; `request_keyframe(friend)` is for its decoder.
pub type ViewerOpener = Box<
    dyn FnMut(std::sync::mpsc::Receiver<Incoming>, KeyframeRequester) -> Result<Box<dyn ViewerWindow>, String> + Send,
>;

struct RealViewer(ViewerHandle);

impl ViewerWindow for RealViewer {
    fn is_open(&self) -> bool {
        self.0.is_open()
    }
    fn close(self: Box<Self>) {
        let _ = self.0.close();
    }
}

fn real_viewer_opener() -> ViewerOpener {
    Box::new(|rx, req| {
        let handle = viewer::open(rx, req)?;
        Ok(Box::new(RealViewer(handle)) as Box<dyn ViewerWindow>)
    })
}

/// Where the forwarder thread delivers friends' frames: the open viewer, or nowhere.
type ViewerSink = Arc<Mutex<Option<std::sync::mpsc::SyncSender<Incoming>>>>;

/// Frames queued between the forwarder and the viewer's decoder; more than this are dropped
/// (the decoder then sees a hole and asks for a keyframe).
const VIEWER_QUEUE: usize = 16;

/// Moves `Session::take_video_frames()` into whichever viewer is open; frames that arrive while
/// none is open are dropped (live video never waits). Ends when the session stops.
fn spawn_video_forwarder(mut rx: tokio::sync::mpsc::Receiver<VideoFrame>, sink: ViewerSink) {
    let spawned = std::thread::Builder::new().name("video-forward".into()).spawn(move || {
        while let Some(VideoFrame { friend, codec, frame }) = rx.blocking_recv() {
            let guard = sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(tx) = guard.as_ref() {
                let _ = tx.try_send(Incoming { friend, codec, frame });
            }
        }
    });
    if let Err(e) = spawned {
        warn!("cannot start the video forwarder: {e}");
    }
}

pub struct Controller<P: PlatformApi> {
    viewer: Option<Box<dyn ViewerWindow>>,
    viewer_opener: ViewerOpener,
    viewer_sink: ViewerSink,
    /// Friends whose last `Status` said they are sharing their screen.
    sharing_peers: Vec<String>,
    /// Somebody was sharing while the open viewer was up (so the end of every share closes it).
    viewer_saw_share: bool,
    /// `take_video_frames` was already handed to the forwarder thread.
    video_forwarded: bool,
    /// Turns a `StartShare` into a pipe factory (tests use a synthetic source).
    share_planner: SharePlanner,
    /// Turns `StartShare { audio: true }` into a capture of the PC's sound (tests inject one).
    audio_planner: AudioPlanner,
    /// Height and fps of the running share (what `ShareState` reports); `None` = not sharing.
    share: Option<(u32, u32)>,
    /// The running share also sends the PC's sound (the capture really opened).
    share_audio: bool,
    session: Arc<Session>,
    host: UiHost,
    platform: P,
    detector: GameDetector,
    keep_sharing: bool,
    auto_detect: bool,
    ptt_mode: bool,
    /// The UI client that is capturing a shortcut (`SetHotkeyCapture`).
    capture: Option<ClientId>,
    /// The UI client that enabled the microphone level meter.
    mic_meter_client: Option<ClientId>,
    /// The UI client that enabled the voice test (`SetMicTest`, "Ouvir minha voz").
    mic_test_client: Option<ClientId>,
    /// The UI client whose window is focused (`SetUiFocus`); chat sounds play when none is.
    focused_client: Option<ClientId>,
    /// Friends that were ringing at the last `Calls`, to notice the new ones.
    ringing: Vec<String>,
    steps: Vec<Step>,
}

impl<P: PlatformApi> Controller<P> {
    /// `ptt_mode`: `audio.mode = push_to_talk` (then PTT events reach `Session::set_ptt`).
    pub fn new(session: Arc<Session>, host: UiHost, platform: P, game_cfg: &GameModeConfig, ptt_mode: bool) -> Self {
        Controller {
            viewer: None,
            viewer_opener: real_viewer_opener(),
            viewer_sink: Arc::new(Mutex::new(None)),
            sharing_peers: Vec::new(),
            viewer_saw_share: false,
            video_forwarded: false,
            share_planner: real_share_planner(),
            audio_planner: real_audio_planner(),
            share: None,
            share_audio: false,
            session,
            host,
            platform,
            detector: GameDetector::new(game_cfg),
            keep_sharing: game_cfg.keep_sharing_in_game_mode,
            auto_detect: game_cfg.auto_detect,
            ptt_mode,
            capture: None,
            mic_meter_client: None,
            mic_test_client: None,
            focused_client: None,
            ringing: Vec::new(),
            steps: Vec::new(),
        }
    }

    /// Replaces how the viewer window is created (tests use a fake).
    pub fn set_viewer_opener(&mut self, opener: ViewerOpener) {
        self.viewer_opener = opener;
    }

    /// Replaces how a `StartShare` is turned into a video pipe (tests use a synthetic source).
    pub fn set_share_planner(&mut self, planner: SharePlanner) {
        self.share_planner = planner;
    }

    /// Replaces how the PC's sound is captured for a share (tests use a synthetic source).
    pub fn set_audio_planner(&mut self, planner: AudioPlanner) {
        self.audio_planner = planner;
    }

    /// Whether this PC is sharing its screen.
    pub fn sharing(&self) -> bool {
        self.share.is_some()
    }

    /// Starts sharing `source_id` at `resolution` (height) and `fps` (`screen-share`). Opening
    /// capture and encoder can take a while, so it runs off the async threads.
    /// With `audio`, the PC's sound goes along (without Concord's own); if its capture cannot
    /// open the share starts without it and `ShareState.audio` is false.
    pub async fn start_share(&mut self, source_id: &str, resolution: u32, fps: u32, audio: bool) -> Result<(), String> {
        if self.share.is_some() {
            return Err("já está transmitindo a tela".into());
        }
        let launch = (self.share_planner)(source_id, resolution, fps)?;
        let session = self.session.clone();
        let (preset, factory) = (launch.preset, launch.factory);
        let audio_factory = audio.then(|| (self.audio_planner)());
        let with_audio = tokio::task::spawn_blocking(move || session.start_share_with_audio(preset, factory, audio_factory))
            .await
            .map_err(|e| format!("falha ao iniciar a transmissão: {e}"))??;
        self.share = Some((launch.resolution, preset.fps()));
        self.share_audio = with_audio;
        info!(resolution = launch.resolution, fps = preset.fps(), audio = with_audio, "screen share started");
        self.publish_share();
        Ok(())
    }

    /// Stops the screen share (D-012 step 3 and `StopShare`); harmless when not sharing.
    pub fn stop_share(&mut self) {
        let was = self.share.take().is_some();
        self.share_audio = false;
        self.session.stop_share();
        if was {
            info!("screen share stopped");
            self.publish_share();
        }
    }

    fn share_state_msg(&self) -> CoreToUi {
        let (resolution, fps) = self.share.unwrap_or((0, 0));
        CoreToUi::ShareState { sharing: self.share.is_some(), resolution, fps, audio: self.share_audio }
    }

    fn publish_share(&self) {
        self.host.ipc.broadcast(&self.share_state_msg());
    }

    /// The viewer window is on screen.
    pub fn viewer_open(&self) -> bool {
        self.viewer.as_ref().is_some_and(|v| v.is_open())
    }

    /// Opens the viewer on friends' screen shares (no-op while one is already open). Its decoder
    /// asks the sharing friend for a keyframe as soon as it needs one, which covers the start.
    pub fn open_viewer(&mut self) -> Result<(), String> {
        if self.viewer_open() {
            return Ok(());
        }
        self.close_viewer(); // a window the user already closed
        if !self.video_forwarded {
            let rx = self.session.take_video_frames().ok_or("the video stream is already in use")?;
            spawn_video_forwarder(rx, self.viewer_sink.clone());
            self.video_forwarded = true;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(VIEWER_QUEUE);
        let session = self.session.clone();
        let request: KeyframeRequester = Arc::new(move |friend| session.request_keyframe(friend));
        let window = (self.viewer_opener)(rx, request)?;
        *self.viewer_sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx);
        self.viewer = Some(window);
        self.viewer_saw_share = !self.sharing_peers.is_empty();
        info!("viewer opened");
        Ok(())
    }

    /// Records who is sharing and closes the viewer once the last watched share is over
    /// (T-035a). A viewer opened while nobody shared stays up until somebody does.
    fn track_shares(&mut self, friend: &str, sharing: bool) {
        self.sharing_peers.retain(|f| f != friend);
        if sharing {
            self.sharing_peers.push(friend.to_string());
            self.viewer_saw_share |= self.viewer_open();
        } else if self.viewer_saw_share && self.sharing_peers.is_empty() && self.viewer_open() {
            info!(%friend, "no one is sharing any more: closing the viewer");
            self.close_viewer();
        }
    }

    /// Closes the viewer (D-012 step 2, also when the user asks); harmless when none is open.
    pub fn close_viewer(&mut self) {
        *self.viewer_sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        if let Some(v) = self.viewer.take() {
            v.close();
            info!("viewer closed");
        }
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    pub fn host_mut(&mut self) -> &mut UiHost {
        &mut self.host
    }

    pub fn platform(&self) -> &P {
        &self.platform
    }

    pub fn in_game(&self) -> bool {
        self.detector.in_game()
    }

    /// Whether the detector should be fed samples (`game_mode.auto_detect`, live).
    pub fn auto_detect(&self) -> bool {
        self.auto_detect
    }

    /// Steps of the sequences run so far, oldest first.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn clear_steps(&mut self) {
        self.steps.clear();
    }

    // ------------------------------------------------------------ game mode

    /// Manual toggle (hotkey, tray, UI).
    pub async fn toggle_game_mode(&mut self, trigger: Trigger) {
        let want = !self.detector.in_game();
        self.set_game_mode_manual(want, trigger).await;
    }

    pub async fn set_game_mode_manual(&mut self, on: bool, trigger: Trigger) {
        if let Some(t) = self.detector.set_manual(on) {
            self.apply(t, trigger).await;
        }
    }

    /// One detector tick (`POLL_INTERVAL`).
    ///
    /// D-033: while this PC shares its screen or the viewer is open, the detector is not fed, so
    /// automatic detection never enters game mode (a full-screen window there is the stream
    /// itself). Leaving game mode stays automatic. When the stream ends the detector counts from
    /// zero again: a game still in the foreground enters after `Hysteresis::ENTER_AFTER` ticks.
    pub async fn tick(&mut self, sample: Option<&ForegroundSample>) {
        if !self.detector.in_game() && (self.sharing() || self.viewer_open()) {
            return;
        }
        if let Some(t) = self.detector.tick(sample) {
            self.apply(t, Trigger::auto(sample)).await;
        }
    }

    async fn apply(&mut self, t: Transition, trigger: Trigger) {
        match t {
            Transition::Enter => self.enter_game_mode(trigger).await,
            Transition::Exit => self.exit_game_mode(trigger),
        }
    }

    /// D-012: (1) UI shutdown confirmed, (2) viewer, (3) sharing, (4) memory,
    /// (5) `Status`, (6) amber tray.
    async fn enter_game_mode(&mut self, trigger: Trigger) {
        info!(%trigger, "entering game mode");
        // The UI is going away: nobody is watching the meter.
        self.stop_mic_meter();
        self.stop_mic_test();
        let outcome = self.host.shutdown_ui().await;
        self.steps.push(Step::UiShutdown(outcome));
        self.close_viewer();
        self.steps.push(Step::CloseViewer);
        if !self.keep_sharing {
            self.stop_share();
            self.steps.push(Step::StopSharing);
        }
        release_working_set();
        self.steps.push(Step::ReleaseMemory);
        self.session.set_game_mode(true);
        self.steps.push(Step::StatusSent { game_mode: true });
        self.publish_status();
        self.platform.set_tray(self.tray_state());
        self.steps.push(Step::Tray { game_mode: true });
        self.steps.push(Step::Cause { entering: true, trigger });
    }

    /// D-012 on leaving: launch the UI, `Status`, normal tray.
    fn exit_game_mode(&mut self, trigger: Trigger) {
        info!(%trigger, "leaving game mode");
        let launched = self.host.launch();
        self.steps.push(Step::UiLaunched(launched));
        self.session.set_game_mode(false);
        self.steps.push(Step::StatusSent { game_mode: false });
        self.publish_status();
        self.platform.set_tray(self.tray_state());
        self.steps.push(Step::Tray { game_mode: false });
        self.steps.push(Step::Cause { entering: false, trigger });
    }

    /// Shows the UI; leaves game mode first when needed (`trigger` is then the cause).
    pub async fn open_ui(&mut self, trigger: Trigger) {
        if self.detector.in_game() {
            self.set_game_mode_manual(false, trigger).await;
        } else {
            self.host.launch();
        }
    }

    // ------------------------------------------------------------ voice state

    pub fn set_muted(&mut self, muted: bool) {
        self.session.set_muted(muted);
        self.after_local_change();
    }

    /// Deafening mutes; undeafening restores the previous mute (`inv-deafen-implies-mute`).
    pub fn set_deafened(&mut self, deafened: bool) {
        self.session.set_deafened(deafened);
        self.after_local_change();
    }

    /// Enters/leaves the room (D-015): friends and the UI hear about it.
    /// Leaving the room also ends our screen share (T-035a): nobody is left to watch it.
    pub fn set_in_room(&mut self, in_room: bool) {
        if !in_room {
            self.stop_share();
        }
        self.session.set_in_room(in_room);
        self.after_local_change();
    }

    /// Enters the room and rings `friend` (D-024).
    pub fn call(&mut self, friend: &str) {
        let start = self.session.call(friend);
        info!(%friend, ?start, "call");
        self.after_local_change();
    }

    /// Whether the UI window is in front of the user (so a chat sound would be redundant).
    pub fn ui_focused(&self) -> bool {
        !self.detector.in_game() && self.focused_client.is_some_and(|c| self.host.ipc.has_client(c))
    }

    fn reply_error(&self, client: ClientId, message: String) {
        self.host.ipc.send_to(client, &CoreToUi::Error { message });
    }

    fn broadcast_config(&self) {
        self.host.ipc.broadcast(&settings::config_message_with_address(&self.session.config()));
    }

    /// Validates everything first (nothing is applied on an error), then saves and applies
    /// live: audio and friends in the session, game lists in the detector, shortcuts in the platform.
    pub fn update_config(
        &mut self,
        name: Option<String>,
        audio: Option<AudioSettings>,
        game_mode: Option<GameModeSettings>,
        hotkeys: Option<HotkeySettings>,
    ) -> Result<(), String> {
        let new = settings::merged(&self.session.config(), name.as_deref(), audio.as_ref(), game_mode.as_ref(), hotkeys.as_ref())?;
        let (list, ptt) = hotkey_plan(&new)?;
        self.session.apply_config(&new)?;
        self.detector.set_config(&new.game_mode);
        self.keep_sharing = new.game_mode.keep_sharing_in_game_mode;
        self.auto_detect = new.game_mode.auto_detect;
        self.ptt_mode = new.audio.mode == AudioMode::PushToTalk;
        // While the UI is capturing a shortcut the platform stays empty; leaving
        // capture applies the saved config.
        if self.capture.is_none() {
            self.platform.set_hotkeys(list);
            self.platform.set_ptt(if self.ptt_mode { ptt } else { None });
        }
        info!("settings updated and saved");
        Ok(())
    }

    /// `SetHotkeyCapture{on: true}`: unregister every shortcut and the PTT key.
    fn start_capture(&mut self, client: ClientId) {
        self.capture = Some(client);
        self.platform.set_hotkeys(Vec::new());
        self.platform.set_ptt(None);
    }

    /// Restores the shortcuts of the saved config.
    fn end_capture(&mut self) {
        if self.capture.take().is_none() {
            return;
        }
        match hotkey_plan(&self.session.config()) {
            Ok((list, ptt)) => {
                self.platform.set_hotkeys(list);
                self.platform.set_ptt(if self.ptt_mode { ptt } else { None });
            }
            Err(e) => warn!("cannot restore shortcuts: {e}"),
        }
    }

    /// Ends the capture when the client that started it is gone (called periodically).
    pub fn check_capture_client(&mut self) {
        if let Some(c) = self.capture
            && !self.host.ipc.has_client(c)
        {
            info!("capturing client disconnected; restoring shortcuts");
            self.end_capture();
        }
        if let Some(c) = self.mic_meter_client
            && !self.host.ipc.has_client(c)
        {
            info!("metering client disconnected; stopping microphone meter");
            self.stop_mic_meter();
        }
        if let Some(c) = self.mic_test_client
            && !self.host.ipc.has_client(c)
        {
            info!("voice test client disconnected; stopping the voice test");
            self.stop_mic_test();
        }
        if let Some(c) = self.focused_client
            && !self.host.ipc.has_client(c)
        {
            self.focused_client = None;
        }
    }

    fn stop_mic_meter(&mut self) {
        self.mic_meter_client = None;
        self.session.set_mic_meter(false);
    }

    fn stop_mic_test(&mut self) {
        if self.mic_test_client.take().is_some() {
            self.session.set_mic_test(false);
            self.host.ipc.broadcast(&CoreToUi::MicTest { on: false });
        }
    }

    /// Sends the `Config` (with the Tailscale address) to every UI.
    pub fn refresh_config(&self) {
        self.broadcast_config();
    }

    fn after_local_change(&mut self) {
        self.publish_status();
        self.platform.set_tray(self.tray_state());
    }

    fn publish_status(&self) {
        self.host.ipc.broadcast(&status_msg(self.session.local_status()));
    }

    pub fn tray_state(&self) -> TrayState {
        let s = self.session.local_status();
        let call = if s.in_room {
            self.session.connected_friends().first().map(|f| format!("Em chamada com {f}"))
        } else {
            None
        };
        TrayState {
            game_mode: s.game_mode,
            muted: s.muted,
            deafened: s.deafened,
            call,
            incoming_calls: self.session.calls().0,
        }
    }

    // ------------------------------------------------------------ inputs

    pub async fn on_platform_event(&mut self, ev: PlatformEvent) -> Flow {
        match ev {
            PlatformEvent::Hotkey(HotkeyAction::ToggleMute) | PlatformEvent::Menu(MenuItem::ToggleMute) => {
                self.toggle_mute();
            }
            PlatformEvent::Hotkey(HotkeyAction::ToggleDeafen) | PlatformEvent::Menu(MenuItem::ToggleDeafen) => {
                let d = self.session.local_status().deafened;
                self.set_deafened(!d);
            }
            PlatformEvent::Hotkey(HotkeyAction::ToggleGameMode) => self.toggle_game_mode(Trigger::Hotkey).await,
            PlatformEvent::Menu(MenuItem::ToggleGameMode) => self.toggle_game_mode(Trigger::Menu).await,
            // TODO(F3): soundboard.
            PlatformEvent::Hotkey(HotkeyAction::PlaySound(_)) => {}
            PlatformEvent::PttPressed => {
                if self.ptt_mode {
                    self.session.set_ptt(true);
                }
            }
            PlatformEvent::PttReleased => {
                if self.ptt_mode {
                    self.session.set_ptt(false);
                }
            }
            PlatformEvent::Menu(MenuItem::OpenSala) => self.open_ui(Trigger::TrayClick).await,
            PlatformEvent::Menu(MenuItem::QuitSala) => return Flow::Quit,
            // D-024: answer a ring from the tray menu or the balloon, staying in game mode.
            PlatformEvent::Menu(MenuItem::AcceptCall(friend))
            | PlatformEvent::CallNotificationClicked { friend } => self.accept_call(&friend),
            PlatformEvent::Menu(MenuItem::DeclineCall(friend)) => self.session.decline_call(&friend),
            PlatformEvent::NotificationClicked => {
                // D-008: clicking the notification is a manual exit.
                if self.detector.in_game() {
                    self.set_game_mode_manual(false, Trigger::Notification).await;
                }
            }
            // The only warn for a failed registration (the platform thread stays silent).
            PlatformEvent::HotkeyRegistrationFailed { action, hotkey, error_code } => {
                warn!(%action, %hotkey, error_code, "hotkey could not be registered");
                self.host.ipc.broadcast(&CoreToUi::Error { message: hotkey_in_use_message(&hotkey) });
            }
        }
        Flow::Continue
    }

    /// Joins the room, answering the rings (a stale friend name is harmless).
    fn accept_call(&mut self, friend: &str) {
        self.session.accept_call(friend);
        self.after_local_change();
    }

    fn toggle_mute(&mut self) {
        let s = self.session.local_status();
        // While deafened the microphone stays muted.
        if !s.deafened {
            self.set_muted(!s.muted);
        }
    }

    pub async fn on_command(&mut self, client: ClientId, cmd: UiToCore) {
        match cmd {
            UiToCore::GetState => self.send_state(client),
            UiToCore::GetChatHistory => {
                let entries = match self.current_friend() {
                    Some(f) => self.session.chat_history(&f),
                    None => Vec::new(),
                };
                self.host.ipc.send_to(client, &CoreToUi::ChatHistory { entries });
            }
            UiToCore::SendChat { text } => {
                let result = match self.current_friend() {
                    Some(f) => self.session.send_chat(&f, &text).map_err(|e| e.to_string()),
                    None => Err("not in a call".to_string()),
                };
                let reply = match result {
                    // Echo of the stored entry (`from_self = true`).
                    Ok(entry) => CoreToUi::ChatReceived { entry },
                    Err(message) => CoreToUi::Error { message },
                };
                self.host.ipc.send_to(client, &reply);
            }
            UiToCore::SetMuted { muted } => self.set_muted(muted),
            UiToCore::SetDeafened { deafened } => self.set_deafened(deafened),
            UiToCore::EnterGameMode => self.set_game_mode_manual(true, Trigger::Ipc).await,
            UiToCore::ExitGameMode => self.set_game_mode_manual(false, Trigger::Ipc).await,
            UiToCore::OpenUi => self.open_ui(Trigger::SecondInstance).await,
            // D-024: the app opens outside the room; "Call" enters it and rings the friend.
            UiToCore::Call { friend } => {
                if self.session.config().friends.iter().any(|f| f.name == friend) {
                    self.call(&friend);
                } else {
                    self.reply_error(client, format!("unknown friend {friend:?}"));
                }
            }
            UiToCore::HangUp => self.set_in_room(false),
            UiToCore::JoinRoom => self.set_in_room(true),
            UiToCore::AcceptCall { friend } => self.accept_call(&friend),
            UiToCore::DeclineCall { friend } => self.session.decline_call(&friend),
            UiToCore::SetUiFocus { focused } => {
                if focused {
                    self.focused_client = Some(client);
                } else if self.focused_client == Some(client) {
                    self.focused_client = None;
                }
            }
            UiToCore::GetConfig => {
                self.host.ipc.send_to(client, &settings::config_message_with_address(&self.session.config()));
            }
            UiToCore::UpdateConfig { name, audio, game_mode, hotkeys } => {
                match self.update_config(name, audio, game_mode, hotkeys) {
                    Ok(()) => self.broadcast_config(),
                    Err(message) => self.reply_error(client, message),
                }
            }
            UiToCore::AddFriend { name, address } => match self.session.add_friend(&name, &address) {
                Ok(()) => self.broadcast_config(),
                Err(message) => self.reply_error(client, message),
            },
            UiToCore::RemoveFriend { name } => match self.session.remove_friend(&name) {
                Ok(()) => self.broadcast_config(),
                Err(message) => self.reply_error(client, message),
            },
            UiToCore::ResetFingerprint { name } => match self.session.reset_fingerprint(&name) {
                Ok(()) => self.broadcast_config(),
                Err(message) => self.reply_error(client, message),
            },
            UiToCore::ListAudioDevices => {
                let (inputs, outputs) = tokio::task::spawn_blocking(|| {
                    (list_devices(true), list_devices(false))
                })
                .await
                .unwrap_or_default();
                let reply = CoreToUi::AudioDevices {
                    inputs: settings::with_default_first(inputs),
                    outputs: settings::with_default_first(outputs),
                };
                self.host.ipc.send_to(client, &reply);
            }
            UiToCore::ListTailnetPeers => {
                let (running, peers) =
                    tokio::task::spawn_blocking(crate::tailscale::list_peers_blocking).await.unwrap_or_default();
                self.host.ipc.send_to(client, &CoreToUi::TailnetPeers { running, peers });
            }
            UiToCore::SetMicMeter { on } => {
                if on {
                    self.mic_meter_client = Some(client);
                    self.session.set_mic_meter(true);
                } else {
                    self.stop_mic_meter();
                }
            }
            UiToCore::SetMicTest { on } => {
                if on && !self.session.local_status().game_mode {
                    self.mic_test_client = Some(client);
                    self.session.set_mic_test(true);
                    self.host.ipc.broadcast(&CoreToUi::MicTest { on: true });
                } else if self.mic_test_client.is_some() {
                    self.stop_mic_test();
                } else {
                    // Refused (game mode) or already off: tell the asking UI where things stand.
                    self.host.ipc.send_to(client, &CoreToUi::MicTest { on: false });
                }
            }
            UiToCore::SetHotkeyCapture { on } => {
                if on {
                    self.start_capture(client);
                } else {
                    self.end_capture();
                }
            }
            UiToCore::SetPeerVolume { friend, volume } => {
                if self.session.config().friends.iter().any(|f| f.name == friend) {
                    self.session.set_peer_volume(&friend, clamp_volume(volume));
                } else {
                    self.reply_error(client, format!("unknown friend {friend:?}"));
                }
            }
            UiToCore::OpenViewer => {
                if self.in_game() {
                    self.reply_error(client, "o visualizador não abre no modo jogo".into());
                } else if let Err(e) = self.open_viewer() {
                    warn!("cannot open the viewer: {e}");
                    self.reply_error(client, format!("não foi possível abrir o visualizador: {e}"));
                }
            }
            UiToCore::ListShareSources => {
                let (encoder, monitors, windows) =
                    tokio::task::spawn_blocking(list_share_sources).await.unwrap_or_default();
                self.host.ipc.send_to(client, &CoreToUi::ShareSources { encoder, monitors, windows });
            }
            UiToCore::StartShare { source_id, resolution, fps, audio } => {
                if let Err(e) = self.start_share(&source_id, resolution, fps, audio).await {
                    warn!("cannot start the screen share: {e}");
                    self.reply_error(client, format!("não foi possível transmitir a tela: {e}"));
                }
            }
            UiToCore::StopShare => self.stop_share(),
            UiToCore::SetShareAudioVolume { volume } => self.session.set_share_audio_volume(volume),
        }
    }

    /// The current call is the first connected friend.
    fn current_friend(&self) -> Option<String> {
        self.session.connected_friends().into_iter().next()
    }

    fn send_state(&self, client: ClientId) {
        let ipc = &self.host.ipc;
        ipc.send_to(client, &status_msg(self.session.local_status()));
        let (incoming, outgoing) = self.session.calls();
        ipc.send_to(client, &CoreToUi::Calls { incoming, outgoing });
        ipc.send_to(client, &self.share_state_msg());
        ipc.send_to(client, &CoreToUi::MicTest { on: self.mic_test_client.is_some() });
        for friend in self.session.connected_friends() {
            ipc.send_to(client, &CoreToUi::PeerConnected { friend: friend.clone() });
            if let Some(p) = self.session.peer_status(&friend) {
                ipc.send_to(client, &peer_status_msg(&friend, p));
            }
        }
    }

    pub fn on_session_event(&mut self, ev: SessionEvent) {
        match &ev {
            SessionEvent::PeerStatus { friend, status } => self.track_shares(friend, status.sharing),
            SessionEvent::Disconnected { friend } => self.track_shares(friend, false),
            _ => {}
        }
        let ipc = &self.host.ipc;
        match ev {
            SessionEvent::Connected { friend } => {
                ipc.broadcast(&CoreToUi::PeerConnected { friend });
                self.platform.set_tray(self.tray_state());
            }
            SessionEvent::ConfigChanged => self.broadcast_config(),
            SessionEvent::Disconnected { friend } => {
                ipc.broadcast(&CoreToUi::PeerDisconnected { friend });
                self.platform.set_tray(self.tray_state());
            }
            SessionEvent::Reconnecting { friend, attempt } => {
                ipc.broadcast(&CoreToUi::PeerReconnecting { friend, attempt });
            }
            SessionEvent::PeerStatus { friend, status } => {
                ipc.broadcast(&peer_status_msg(&friend, status));
            }
            SessionEvent::ChatReceived { friend, entry } => {
                if self.session.local_status().game_mode {
                    // Only the sender leaves the process (D-011).
                    self.platform.notify_message(&friend);
                } else {
                    ipc.broadcast(&CoreToUi::ChatReceived { entry });
                }
                // A sound when the window is not in front of the user (D-024).
                if !self.ui_focused() {
                    self.session.play_cue(Cue::Message);
                }
            }
            SessionEvent::Calls { incoming, outgoing } => {
                // In game mode nobody looks at a window: a balloon says who is calling.
                if self.session.local_status().game_mode {
                    for f in incoming.iter().filter(|f| !self.ringing.contains(f)) {
                        self.platform.notify_call(f);
                    }
                }
                self.ringing = incoming.clone();
                ipc.broadcast(&CoreToUi::Calls { incoming, outgoing });
                self.platform.set_tray(self.tray_state());
            }
            SessionEvent::CallResult { friend, end } => {
                ipc.broadcast(&CoreToUi::CallResult { friend, result: end.as_str().to_string() });
            }
            SessionEvent::FingerprintMismatch { friend } => {
                ipc.broadcast(&CoreToUi::FingerprintMismatch { friend });
            }
            SessionEvent::NotInTailnet { friend } => {
                ipc.broadcast(&CoreToUi::PeerNotInTailnet { friend });
            }
            SessionEvent::CallStats { friend, rtt_ms, since_secs } => {
                ipc.broadcast(&CoreToUi::CallStats { friend, rtt_ms, since_secs });
            }
            SessionEvent::MicLevel { db } => {
                ipc.broadcast(&CoreToUi::MicLevel { db });
            }
        }
    }

    /// "Close Sala": UI (D-010), session (Bye/close) and IPC. The platform goes
    /// away when the controller is dropped.
    pub async fn shutdown(mut self) {
        self.host.shutdown_ui().await;
        // "Quit" must always quit, even if the session loop is stuck (T-029).
        if tokio::time::timeout(SESSION_STOP_TIMEOUT, self.session.shutdown()).await.is_err() {
            tracing::warn!("session did not stop in time; quitting anyway");
        }
        self.host.ipc.shutdown().await;
    }
}

/// Readable error for the UI toast (the log line keeps the technical details).
fn hotkey_in_use_message(hotkey: &Hotkey) -> String {
    format!("{hotkey} já está em uso por outro programa")
}

fn status_msg(s: StatusFlags) -> CoreToUi {
    CoreToUi::Status { muted: s.muted, deafened: s.deafened, game_mode: s.game_mode, in_room: s.in_room }
}

fn peer_status_msg(friend: &str, s: StatusFlags) -> CoreToUi {
    CoreToUi::PeerStatus {
        friend: friend.to_string(),
        muted: s.muted,
        deafened: s.deafened,
        game_mode: s.game_mode,
        sharing: s.sharing,
        in_room: s.in_room,
    }
}

/// Step 4 of entering game mode: drop what can be dropped and hand the working
/// set back to the OS. (Chat history must survive: it is the call state.)
fn release_working_set() {
    // SAFETY: documented call on the current-process pseudo handle; failure only
    // means nothing was trimmed.
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(path: &str) -> ForegroundSample {
        ForegroundSample { exe_path: path.into(), covers_monitor: true }
    }

    #[test]
    fn auto_trigger_names_the_exe_file_only() {
        let t = Trigger::auto(Some(&sample(r"C:\Riot Games\VALORANT\VALORANT.exe")));
        assert_eq!(t, Trigger::Auto { exe: "VALORANT.exe".into() });
        assert_eq!(t.to_string(), "auto (VALORANT.exe)");
        assert_eq!(Trigger::auto(Some(&sample("/x/cs2.exe"))), Trigger::Auto { exe: "cs2.exe".into() });
        // Control: no sample / empty path is "desconhecido", never an empty name.
        assert_eq!(Trigger::auto(None), Trigger::Auto { exe: "desconhecido".into() });
        assert_eq!(Trigger::auto(Some(&sample(""))), Trigger::Auto { exe: "desconhecido".into() });
        assert_eq!(Trigger::auto(Some(&sample(r"C:\dir\"))), Trigger::Auto { exe: "desconhecido".into() });
    }

    #[test]
    fn hotkey_in_use_message_is_readable() {
        let hk: Hotkey = "ctrl+shift+m".parse().unwrap();
        assert_eq!(hotkey_in_use_message(&hk), "Ctrl+Shift+M já está em uso por outro programa");
    }

    #[test]
    fn manual_triggers_have_stable_log_names() {
        let names: Vec<String> = [
            Trigger::Hotkey,
            Trigger::Menu,
            Trigger::TrayClick,
            Trigger::Notification,
            Trigger::Ipc,
            Trigger::SecondInstance,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(names, ["hotkey", "menu", "tray_click", "notification", "ipc", "second_instance"]);
    }
}

// ------------------------------------------------------------------ run

#[derive(Debug)]
pub enum AppError {
    /// Another core owns the pipe: the caller should send it `OpenUi` and exit 0.
    AlreadyRunning,
    Other(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::AlreadyRunning => write!(f, "another core is already running"),
            AppError::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Tells the running core (same user and profile) to show its UI.
pub async fn notify_running_instance(profile_name: Option<&str>) -> std::io::Result<()> {
    let mut client = IpcClient::connect(&current_user_pipe_name(profile_name)).await?;
    client.send(&UiToCore::OpenUi).await
}

fn parse_hotkey(what: &str, text: &str) -> Option<Hotkey> {
    match text.parse::<Hotkey>() {
        Ok(h) => Some(h),
        Err(e) => {
            warn!(%what, "invalid hotkey in config: {e}");
            None
        }
    }
}

/// Shortcuts of `config` for the platform thread (an invalid one is skipped with a warning).
fn configure_hotkeys(platform: &Platform, config: &Config) {
    let h = &config.hotkeys;
    let list: Vec<(HotkeyAction, Hotkey)> = [
        (HotkeyAction::ToggleMute, "toggle_mute", &h.toggle_mute),
        (HotkeyAction::ToggleDeafen, "toggle_deafen", &h.toggle_deafen),
        (HotkeyAction::ToggleGameMode, "toggle_game_mode", &h.toggle_game_mode),
    ]
    .into_iter()
    .filter_map(|(a, name, text)| parse_hotkey(name, text).map(|hk| (a, hk)))
    .collect();
    platform.set_hotkeys(list);
    if config.audio.mode == AudioMode::PushToTalk {
        platform.set_ptt(parse_hotkey("push_to_talk", &h.push_to_talk));
    }
}

type HotkeyPlan = (Vec<(HotkeyAction, Hotkey)>, Option<Hotkey>);

/// Shortcuts of `config` from a UI update: every one must parse. The list is for
/// `set_hotkeys`, the last one is the push-to-talk key.
fn hotkey_plan(config: &Config) -> Result<HotkeyPlan, String> {
    let h = &config.hotkeys;
    let parse = |what: &str, text: &str| text.parse::<Hotkey>().map_err(|e| format!("invalid shortcut {what} ({text:?}): {e}"));
    let list = vec![
        (HotkeyAction::ToggleMute, parse("toggle_mute", &h.toggle_mute)?),
        (HotkeyAction::ToggleDeafen, parse("toggle_deafen", &h.toggle_deafen)?),
        (HotkeyAction::ToggleGameMode, parse("toggle_game_mode", &h.toggle_game_mode)?),
    ];
    Ok((list, Some(parse("push_to_talk", &h.push_to_talk)?)))
}

/// Runs the core until "Quit" or Ctrl+C.
pub async fn run(
    profile_name: Option<&str>,
    profile: Profile,
    config: Config,
    identity: &LocalIdentity,
    source: AudioSource,
) -> Result<(), AppError> {
    let pipe = current_user_pipe_name(profile_name);
    // The pipe is the single-instance guard: take it before anything else.
    let (ipc, mut ipc_rx) = IpcServer::start(&pipe).map_err(|e| match e {
        IpcError::AlreadyRunning => AppError::AlreadyRunning,
        other => AppError::Other(format!("ipc: {other}")),
    })?;
    let (platform, mut platform_rx) =
        Platform::start().map_err(|e| AppError::Other(format!("platform: {e}")))?;
    let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port));
    let game_cfg = config.game_mode.clone();
    let ptt_mode = config.audio.mode == AudioMode::PushToTalk;
    configure_hotkeys(&platform, &config);
    let session = Arc::new(Session::start(profile, config, identity, bind_addr, source).map_err(AppError::Other)?);
    let mut session_rx = session.take_events().ok_or_else(|| AppError::Other("session events taken".into()))?;

    let host = UiHost::new(ipc, pipe, resolve_ui_exe());
    let mut ctl = Controller::new(session, host, platform, &game_cfg, ptt_mode);
    ctl.platform().set_tray(ctl.tray_state());
    ctl.host_mut().launch();

    // Tailscale is brought up in the background; the UI gets the address once it runs.
    // Test profiles (`--profile`) leave Tailscale alone.
    let manage_tailscale = profile_name.is_none();
    let (ts_tx, mut ts_rx) = tokio::sync::mpsc::channel::<()>(1);
    if manage_tailscale {
        tokio::spawn(async move {
            if crate::tailscale::ensure_up().await {
                let _ = ts_tx.send(()).await;
            }
        });
    }
    let mut capture_watch = interval(std::time::Duration::from_secs(1));
    capture_watch.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut poll = interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    loop {
        tokio::select! {
            cmd = ipc_rx.recv() => match cmd {
                Some((client, cmd)) => ctl.on_command(client, cmd).await,
                None => break,
            },
            ev = platform_rx.recv() => match ev {
                Some(ev) => {
                    if ctl.on_platform_event(ev).await == Flow::Quit {
                        break;
                    }
                }
                None => break,
            },
            ev = session_rx.recv() => match ev {
                Some(ev) => ctl.on_session_event(ev),
                None => break,
            },
            _ = poll.tick(), if ctl.auto_detect() => {
                let sample = platform::probe();
                ctl.tick(sample.as_ref()).await;
            }
            Some(()) = ts_rx.recv() => ctl.refresh_config(),
            _ = capture_watch.tick() => ctl.check_capture_client(),
            _ = &mut ctrl_c => break,
        }
    }
    ctl.shutdown().await;
    if manage_tailscale {
        crate::tailscale::shut_down().await;
    }
    Ok(())
}
