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
use std::sync::Arc;

use sala_protocol::{CoreToUi, UiToCore};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};

use crate::config::{AudioMode, Config, GameModeConfig, Profile};
use crate::game_detector::{ForegroundSample, GameDetector, POLL_INTERVAL, Transition};
use crate::identity::LocalIdentity;
use crate::ipc::{ClientId, IpcClient, IpcError, IpcServer, current_user_pipe_name};
use crate::platform::{self, Hotkey, HotkeyAction, MenuItem, Platform, PlatformEvent, TrayState};
use crate::session::{AudioSource, Session, SessionEvent, StatusFlags};
use crate::ui_process::{SHUTDOWN_TIMEOUT, ShutdownOutcome, UiProcess, resolve_ui_exe};

/// The part of [`Platform`] the controller needs (fakeable in tests).
pub trait PlatformApi {
    fn set_tray(&self, state: TrayState);
    /// Sender name only, never the text (D-011).
    fn notify_message(&self, sender: &str);
}

impl PlatformApi for Platform {
    fn set_tray(&self, state: TrayState) {
        Platform::set_tray(self, state);
    }
    fn notify_message(&self, sender: &str) {
        Platform::notify_message(self, sender);
    }
}

/// One step of the game-mode sequences (D-012), recorded in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Step 1 of entering: the UI was asked to quit.
    UiShutdown(ShutdownOutcome),
    /// Step 2 (extension point, F4).
    CloseViewer,
    /// Step 3 (extension point, F4); not recorded with `keep_sharing_in_game_mode`.
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

pub struct Controller<P: PlatformApi> {
    session: Arc<Session>,
    host: UiHost,
    platform: P,
    detector: GameDetector,
    keep_sharing: bool,
    ptt_mode: bool,
    steps: Vec<Step>,
}

impl<P: PlatformApi> Controller<P> {
    /// `ptt_mode`: `audio.mode = push_to_talk` (then PTT events reach `Session::set_ptt`).
    pub fn new(session: Arc<Session>, host: UiHost, platform: P, game_cfg: &GameModeConfig, ptt_mode: bool) -> Self {
        Controller {
            session,
            host,
            platform,
            detector: GameDetector::new(game_cfg),
            keep_sharing: game_cfg.keep_sharing_in_game_mode,
            ptt_mode,
            steps: Vec::new(),
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

    /// Steps of the sequences run so far, oldest first.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn clear_steps(&mut self) {
        self.steps.clear();
    }

    // ------------------------------------------------------------ game mode

    /// Manual toggle (hotkey, tray, UI).
    pub async fn toggle_game_mode(&mut self) {
        let want = !self.detector.in_game();
        self.set_game_mode_manual(want).await;
    }

    pub async fn set_game_mode_manual(&mut self, on: bool) {
        if let Some(t) = self.detector.set_manual(on) {
            self.apply(t).await;
        }
    }

    /// One detector tick (`POLL_INTERVAL`).
    pub async fn tick(&mut self, sample: Option<&ForegroundSample>) {
        if let Some(t) = self.detector.tick(sample) {
            self.apply(t).await;
        }
    }

    async fn apply(&mut self, t: Transition) {
        match t {
            Transition::Enter => self.enter_game_mode().await,
            Transition::Exit => self.exit_game_mode(),
        }
    }

    /// D-012: (1) UI shutdown confirmed, (2) viewer, (3) sharing, (4) memory,
    /// (5) `Status`, (6) amber tray.
    async fn enter_game_mode(&mut self) {
        info!("entering game mode");
        let outcome = self.host.shutdown_ui().await;
        self.steps.push(Step::UiShutdown(outcome));
        // TODO(F4): close the video viewer.
        self.steps.push(Step::CloseViewer);
        if !self.keep_sharing {
            // TODO(F4): stop the screen share.
            self.steps.push(Step::StopSharing);
        }
        release_working_set();
        self.steps.push(Step::ReleaseMemory);
        self.session.set_game_mode(true);
        self.steps.push(Step::StatusSent { game_mode: true });
        self.publish_status();
        self.platform.set_tray(self.tray_state());
        self.steps.push(Step::Tray { game_mode: true });
    }

    /// D-012 on leaving: launch the UI, `Status`, normal tray.
    fn exit_game_mode(&mut self) {
        info!("leaving game mode");
        let launched = self.host.launch();
        self.steps.push(Step::UiLaunched(launched));
        self.session.set_game_mode(false);
        self.steps.push(Step::StatusSent { game_mode: false });
        self.publish_status();
        self.platform.set_tray(self.tray_state());
        self.steps.push(Step::Tray { game_mode: false });
    }

    /// Shows the UI; leaves game mode first when needed.
    pub async fn open_ui(&mut self) {
        if self.detector.in_game() {
            self.set_game_mode_manual(false).await;
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

    fn after_local_change(&mut self) {
        self.publish_status();
        self.platform.set_tray(self.tray_state());
    }

    fn publish_status(&self) {
        let s = self.session.local_status();
        self.host.ipc.broadcast(&CoreToUi::Status { muted: s.muted, deafened: s.deafened, game_mode: s.game_mode });
    }

    pub fn tray_state(&self) -> TrayState {
        let s = self.session.local_status();
        let call = self.session.connected_friends().first().map(|f| format!("Em chamada com {f}"));
        TrayState { game_mode: s.game_mode, muted: s.muted, deafened: s.deafened, call }
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
            PlatformEvent::Hotkey(HotkeyAction::ToggleGameMode) | PlatformEvent::Menu(MenuItem::ToggleGameMode) => {
                self.toggle_game_mode().await;
            }
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
            PlatformEvent::Menu(MenuItem::OpenSala) => self.open_ui().await,
            PlatformEvent::Menu(MenuItem::QuitSala) => return Flow::Quit,
            PlatformEvent::NotificationClicked => {
                // D-008: clicking the notification is a manual exit.
                if self.detector.in_game() {
                    self.set_game_mode_manual(false).await;
                }
            }
            PlatformEvent::HotkeyRegistrationFailed { action, hotkey, error_code } => {
                warn!(?action, ?hotkey, error_code, "hotkey could not be registered");
            }
        }
        Flow::Continue
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
            UiToCore::EnterGameMode => self.set_game_mode_manual(true).await,
            UiToCore::ExitGameMode => self.set_game_mode_manual(false).await,
            UiToCore::OpenUi => self.open_ui().await,
            // TODO(F3): explicit call/hang-up; calls are automatic with friends for now.
            UiToCore::Call { .. } | UiToCore::HangUp => {
                self.host.ipc.send_to(client, &CoreToUi::Error { message: "not supported yet".into() });
            }
            // TODO(F4): video viewer.
            UiToCore::OpenViewer => {
                self.host.ipc.send_to(client, &CoreToUi::Error { message: "viewer not available yet".into() });
            }
        }
    }

    /// The current call is the first connected friend.
    fn current_friend(&self) -> Option<String> {
        self.session.connected_friends().into_iter().next()
    }

    fn send_state(&self, client: ClientId) {
        let s = self.session.local_status();
        let ipc = &self.host.ipc;
        ipc.send_to(client, &CoreToUi::Status { muted: s.muted, deafened: s.deafened, game_mode: s.game_mode });
        for friend in self.session.connected_friends() {
            ipc.send_to(client, &CoreToUi::PeerConnected { friend: friend.clone() });
            if let Some(p) = self.session.peer_status(&friend) {
                ipc.send_to(client, &peer_status_msg(&friend, p));
            }
        }
    }

    pub fn on_session_event(&mut self, ev: SessionEvent) {
        let ipc = &self.host.ipc;
        match ev {
            SessionEvent::Connected { friend } => {
                ipc.broadcast(&CoreToUi::PeerConnected { friend });
                self.platform.set_tray(self.tray_state());
            }
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
            }
            SessionEvent::FingerprintMismatch { friend } => {
                ipc.broadcast(&CoreToUi::FingerprintMismatch { friend });
            }
        }
    }

    /// "Close Sala": UI (D-010), session (Bye/close) and IPC. The platform goes
    /// away when the controller is dropped.
    pub async fn shutdown(mut self) {
        self.host.shutdown_ui().await;
        self.session.shutdown().await;
        self.host.ipc.shutdown().await;
    }
}

fn peer_status_msg(friend: &str, s: StatusFlags) -> CoreToUi {
    CoreToUi::PeerStatus {
        friend: friend.to_string(),
        muted: s.muted,
        deafened: s.deafened,
        game_mode: s.game_mode,
        sharing: s.sharing,
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

/// Shortcuts of `config` for the platform thread.
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
            _ = poll.tick(), if game_cfg.auto_detect => {
                let sample = platform::probe();
                ctl.tick(sample.as_ref()).await;
            }
            _ = &mut ctrl_c => break,
        }
    }
    ctl.shutdown().await;
    Ok(())
}
