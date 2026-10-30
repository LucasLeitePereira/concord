//! `Controller` and the screen-share viewer (`screen-share`, D-012 step 2): the window opens on
//! request, a window the user closed can be opened again, and entering game mode closes it.
//! The window itself is a fake (no GPU); the real one is checked by hand with
//! `cargo run -p sala-video --example view_synthetic`.
#![cfg(windows)]

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sala_core::app::{Controller, PlatformApi, Step, Trigger, UiHost, ViewerOpener, ViewerWindow};
use sala_core::config::{Config, Profile};
use sala_core::game_detector::ForegroundSample;
use sala_core::identity::LocalIdentity;
use sala_core::ipc::IpcServer;
use sala_core::platform::{Hotkey, HotkeyAction, TrayState};
use sala_core::session::{AudioSource, Session, SessionEvent, StatusFlags};

struct NoPlatform;

impl PlatformApi for NoPlatform {
    fn set_tray(&self, _state: TrayState) {}
    fn notify_message(&self, _sender: &str) {}
    fn notify_call(&self, _sender: &str) {}
    fn set_hotkeys(&self, _hotkeys: Vec<(HotkeyAction, Hotkey)>) {}
    fn set_ptt(&self, _hotkey: Option<Hotkey>) {}
}

/// Records the state of every fake window.
#[derive(Clone)]
struct FakeWindow {
    open: Arc<AtomicBool>,
}

impl ViewerWindow for FakeWindow {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }
    fn close(self: Box<Self>) {
        self.open.store(false, Ordering::SeqCst);
    }
}

type Windows = Arc<Mutex<Vec<Arc<AtomicBool>>>>;

fn fake_opener(windows: Windows, calls: Arc<AtomicUsize>) -> ViewerOpener {
    Box::new(move |_rx, _request| {
        calls.fetch_add(1, Ordering::SeqCst);
        let open = Arc::new(AtomicBool::new(true));
        windows.lock().unwrap().push(open.clone());
        Ok(Box::new(FakeWindow { open }) as Box<dyn ViewerWindow>)
    })
}

fn temp_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sala-viewer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn controller() -> (Controller<NoPlatform>, Windows, Arc<AtomicUsize>) {
    let root = temp_root();
    let port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
    let profile = Profile::resolve_in(&root, Some("v")).unwrap();
    let mut config = Config::load_or_create(&profile).unwrap();
    config.port = port;
    config.save(&profile).unwrap();
    let identity = LocalIdentity::load_or_create(&profile.data_dir).unwrap();
    let game_cfg = config.game_mode.clone();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let session = Arc::new(Session::start(profile, config, &identity, addr, AudioSource::Synthetic).unwrap());
    let pipe = format!(r"\\.\pipe\sala-viewer-{}-{port}", std::process::id());
    let (ipc, _rx) = IpcServer::start(&pipe).unwrap();
    // The UI executable is never launched here.
    let host = UiHost::new(ipc, pipe, PathBuf::from("does-not-exist.exe"));
    let mut ctl = Controller::new(session, host, NoPlatform, &game_cfg, false);
    let windows: Windows = Arc::default();
    let calls = Arc::new(AtomicUsize::new(0));
    ctl.set_viewer_opener(fake_opener(windows.clone(), calls.clone()));
    (ctl, windows, calls)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn game_mode_closes_the_viewer_at_step_two() {
    let (mut ctl, windows, calls) = controller();
    ctl.open_viewer().unwrap();
    assert!(ctl.viewer_open());
    // A second request while open does not open a second window.
    ctl.open_viewer().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Control: the window stays open when nothing asks for game mode.
    assert!(windows.lock().unwrap()[0].load(Ordering::SeqCst));

    ctl.clear_steps();
    ctl.set_game_mode_manual(true, Trigger::Hotkey).await;
    assert!(ctl.in_game());
    assert!(!ctl.viewer_open(), "entering game mode must close the viewer");
    assert!(!windows.lock().unwrap()[0].load(Ordering::SeqCst), "the window itself must be told to close");
    let steps = ctl.steps().to_vec();
    let pos = |s: &Step| steps.iter().position(|x| x == s).unwrap_or_else(|| panic!("missing {s:?} in {steps:?}"));
    assert!(pos(&Step::CloseViewer) > 0, "the UI shutdown stays the first step: {steps:?}");
    assert!(pos(&Step::CloseViewer) < pos(&Step::StopSharing));
}

fn status(friend: &str, sharing: bool) -> SessionEvent {
    SessionEvent::PeerStatus {
        friend: friend.to_string(),
        status: StatusFlags { sharing, in_room: true, ..StatusFlags::default() },
    }
}

/// T-035a: the viewer follows the friends' shares.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_viewer_closes_when_the_last_watched_share_ends() {
    let (mut ctl, windows, _calls) = controller();
    ctl.on_session_event(status("b", true));
    ctl.open_viewer().unwrap();
    // Control: other news about the sharing friend leave the window alone.
    ctl.on_session_event(status("b", true));
    assert!(ctl.viewer_open());

    // Two friends share; the first stopping is not the end.
    ctl.on_session_event(status("c", true));
    ctl.on_session_event(status("b", false));
    assert!(ctl.viewer_open(), "c still shares");

    ctl.on_session_event(status("c", false));
    assert!(!ctl.viewer_open(), "no one shares any more: the viewer must close");
    assert!(!windows.lock().unwrap()[0].load(Ordering::SeqCst), "the window itself must be told to close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_friend_leaving_while_sharing_closes_the_viewer() {
    let (mut ctl, _windows, _calls) = controller();
    ctl.on_session_event(status("b", true));
    ctl.open_viewer().unwrap();
    ctl.on_session_event(SessionEvent::Disconnected { friend: "b".into() });
    assert!(!ctl.viewer_open());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_opened_before_any_share_stays_until_one_comes_and_goes() {
    let (mut ctl, _windows, _calls) = controller();
    ctl.open_viewer().unwrap();
    // Nobody ever shared: a plain mute change must not close it.
    ctl.on_session_event(status("b", false));
    assert!(ctl.viewer_open());
    ctl.on_session_event(status("b", true));
    assert!(ctl.viewer_open());
    ctl.on_session_event(status("b", false));
    assert!(!ctl.viewer_open());
}

/// D-033: a window covering the monitor while watching is the stream, not a game.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_detection_does_not_enter_game_mode_while_watching() {
    let covering = ForegroundSample { exe_path: r"C:\x\player.exe".into(), covers_monitor: true };

    // Control: without a stream the same samples enter game mode.
    let (mut ctl, _windows, _calls) = controller();
    ctl.tick(Some(&covering)).await;
    ctl.tick(Some(&covering)).await;
    assert!(ctl.in_game(), "control: no stream, detector must enter");

    // Watching: never enters, however long the window covers the monitor.
    let (mut ctl, _windows, _calls) = controller();
    ctl.open_viewer().unwrap();
    for _ in 0..6 {
        ctl.tick(Some(&covering)).await;
    }
    assert!(!ctl.in_game(), "no automatic game mode while the viewer is open");
    assert!(ctl.viewer_open());

    // The stream ends: the same window now counts, after the usual two readings.
    ctl.close_viewer();
    ctl.tick(Some(&covering)).await;
    assert!(!ctl.in_game());
    ctl.tick(Some(&covering)).await;
    assert!(ctl.in_game(), "detection resumes once the viewer is closed");

    // Manual entry works while watching (and runs the D-012 sequence, closing the viewer).
    let (mut ctl, _windows, _calls) = controller();
    ctl.open_viewer().unwrap();
    ctl.set_game_mode_manual(true, Trigger::Hotkey).await;
    assert!(ctl.in_game());
    assert!(!ctl.viewer_open());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_window_closed_by_the_user_can_be_reopened() {
    let (mut ctl, windows, calls) = controller();
    ctl.open_viewer().unwrap();
    // The user presses Esc: the window is gone but the controller still holds the handle.
    windows.lock().unwrap()[0].store(false, Ordering::SeqCst);
    assert!(!ctl.viewer_open());
    ctl.open_viewer().unwrap();
    assert!(ctl.viewer_open());
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a new window was opened");
    ctl.close_viewer();
    assert!(!ctl.viewer_open());
    // Closing again is harmless.
    ctl.close_viewer();
}
