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
use sala_core::identity::LocalIdentity;
use sala_core::ipc::IpcServer;
use sala_core::platform::{Hotkey, HotkeyAction, TrayState};
use sala_core::session::{AudioSource, Session};

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
