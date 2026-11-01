//! T-041b: the `Controller`'s part of the self-update (D-038): `UpdateState` broadcasts,
//! `InstallUpdate` rules and the transitions driven by `UpdateEvent`. No network and no
//! installer: the worker is a fake that only counts how many times it was started.
#![cfg(windows)]

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sala_core::app::{Controller, PlatformApi, UiHost};
use sala_core::config::{Config, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::ipc::{ClientId, IpcClient, IpcServer};
use sala_core::platform::{Hotkey, HotkeyAction, TrayState};
use sala_core::session::{AudioSource, Session};
use sala_core::update::{Manifest, UpdateEvent};
use sala_protocol::{CoreToUi, UiToCore, UpdateState};

struct NoPlatform;

impl PlatformApi for NoPlatform {
    fn set_tray(&self, _state: TrayState) {}
    fn notify_message(&self, _sender: &str) {}
    fn notify_call(&self, _sender: &str) {}
    fn set_hotkeys(&self, _hotkeys: Vec<(HotkeyAction, Hotkey)>) {}
    fn set_ptt(&self, _hotkey: Option<Hotkey>) {}
}

fn temp_root() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sala-upd-{}-{}-{}",
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
    started: Arc<AtomicUsize>,
}

async fn rig() -> Rig {
    let root = temp_root();
    let port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
    let profile = Profile::resolve_in(&root, Some("u")).unwrap();
    let mut config = Config::load_or_create(&profile).unwrap();
    config.port = port;
    config.save(&profile).unwrap();
    let identity = LocalIdentity::load_or_create(&profile.data_dir).unwrap();
    let game_cfg = config.game_mode.clone();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let session = Arc::new(Session::start(profile, config, &identity, addr, AudioSource::Synthetic).unwrap());
    let pipe = format!(r"\\.\pipe\sala-upd-{}-{port}", std::process::id());
    let (ipc, rx) = IpcServer::start(&pipe).unwrap();
    let client = IpcClient::connect(&pipe).await.unwrap();
    let host = UiHost::new(ipc, pipe, PathBuf::from("does-not-exist.exe"));
    let mut ctl = Controller::new(session, host, NoPlatform, &game_cfg, false);
    let started = Arc::new(AtomicUsize::new(0));
    let counter = started.clone();
    ctl.set_update_installer(Arc::new(move |_manifest, _tx| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let mut rig = Rig { ctl, rx, client, started };
    // The server registers the client asynchronously: a broadcast sent before that would be
    // lost. A GetState round trip (answered with `Status`) proves the client is registered.
    rig.send(UiToCore::GetState).await;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), rig.client.recv()).await.unwrap().unwrap().unwrap();
        if matches!(msg, CoreToUi::Status { .. }) {
            break;
        }
    }
    rig
}

impl Rig {
    async fn send(&mut self, cmd: UiToCore) {
        self.client.send(&cmd).await.unwrap();
        let (id, got) = tokio::time::timeout(Duration::from_secs(10), self.rx.recv()).await.unwrap().unwrap();
        assert_eq!(got, cmd);
        self.ctl.on_command(id, got).await;
    }

    /// Next `UpdateState` message (others are skipped).
    async fn update_state(&mut self) -> UpdateState {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(left, self.client.recv())
                .await
                .expect("timed out waiting for UpdateState")
                .unwrap()
                .expect("ipc closed");
            if let CoreToUi::UpdateState { state } = msg {
                return state;
            }
        }
    }

    fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }
}

fn manifest(version: &str) -> Manifest {
    Manifest {
        version: version.into(),
        url: format!("https://github.com/LucasLeitePereira/concord/releases/download/v{version}/Concord-Setup.exe"),
        sha256: "0".repeat(64),
        notes: "novidades".into(),
    }
}

fn available(version: &str) -> UpdateState {
    UpdateState::Available { version: version.into(), notes: "novidades".into() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn found_update_is_broadcast_and_reported_by_get_state() {
    let mut r = rig().await;
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.12")));
    assert_eq!(r.update_state().await, available("0.1.12"));
    // A UI that connects later gets it from GetState.
    r.send(UiToCore::GetState).await;
    assert_eq!(r.update_state().await, available("0.1.12"));
    // The same check again changes nothing and is not broadcast again: the next message
    // is GetState's answer, not a duplicate broadcast.
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.12")));
    r.send(UiToCore::GetState).await;
    assert_eq!(r.update_state().await, available("0.1.12"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_state_without_update_sends_no_update_state() {
    let mut r = rig().await;
    r.send(UiToCore::GetState).await;
    // Flush marker: a later broadcast must be the first UpdateState the UI sees.
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.12")));
    assert_eq!(r.update_state().await, available("0.1.12"), "nothing may precede the real one");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_update_starts_once_and_duplicates_are_ignored() {
    let mut r = rig().await;
    // No manifest yet: ignored.
    r.send(UiToCore::InstallUpdate).await;
    assert_eq!(r.started(), 0);
    assert_eq!(r.ctl.update_state(), None);

    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.12")));
    assert_eq!(r.update_state().await, available("0.1.12"));
    r.send(UiToCore::InstallUpdate).await;
    assert_eq!(r.update_state().await, UpdateState::Downloading { percent: 0 });
    assert_eq!(r.started(), 1);

    // Double click while downloading, then while installing.
    r.send(UiToCore::InstallUpdate).await;
    r.ctl.on_update_event(UpdateEvent::State(UpdateState::Installing));
    assert_eq!(r.update_state().await, UpdateState::Installing);
    r.send(UiToCore::InstallUpdate).await;
    assert_eq!(r.started(), 1, "a second worker must not start");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declining_uac_goes_back_to_available_and_can_be_retried() {
    let mut r = rig().await;
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.12")));
    r.send(UiToCore::InstallUpdate).await;
    r.ctl.on_update_event(UpdateEvent::State(UpdateState::Downloading { percent: 100 }));
    r.ctl.on_update_event(UpdateEvent::State(UpdateState::Installing));
    r.ctl.on_update_event(UpdateEvent::Cancelled);
    assert_eq!(r.ctl.update_state(), Some(&available("0.1.12")));
    r.send(UiToCore::InstallUpdate).await;
    assert_eq!(r.started(), 2, "after declining, the button works again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_outside_installing_is_ignored() {
    let mut r = rig().await;
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.12")));
    r.send(UiToCore::InstallUpdate).await;
    r.ctl.on_update_event(UpdateEvent::Cancelled);
    assert_eq!(r.ctl.update_state(), Some(&UpdateState::Downloading { percent: 0 }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_check_never_overwrites_a_download_or_install() {
    let mut r = rig().await;
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.12")));
    r.send(UiToCore::InstallUpdate).await;
    r.ctl.on_update_event(UpdateEvent::State(UpdateState::Downloading { percent: 50 }));
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.13")));
    assert_eq!(r.ctl.update_state(), Some(&UpdateState::Downloading { percent: 50 }));
    r.ctl.on_update_event(UpdateEvent::State(UpdateState::Installing));
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.13")));
    assert_eq!(r.ctl.update_state(), Some(&UpdateState::Installing));
    // Control: once the install failed, a check may offer the newer release.
    r.ctl.on_update_event(UpdateEvent::State(UpdateState::Failed {
        version: "0.1.12".into(),
        notes: "n".into(),
        message: "falhou".into(),
    }));
    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.13")));
    assert_eq!(r.ctl.update_state(), Some(&available("0.1.13")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_install_can_be_retried_and_late_progress_is_dropped() {
    let mut r = rig().await;
    // Progress with nothing running is dropped.
    r.ctl.on_update_event(UpdateEvent::State(UpdateState::Downloading { percent: 9 }));
    assert_eq!(r.ctl.update_state(), None);

    r.ctl.on_update_event(UpdateEvent::Found(manifest("0.1.12")));
    r.send(UiToCore::InstallUpdate).await;
    let failed = UpdateState::Failed { version: "0.1.12".into(), notes: "novidades".into(), message: "x".into() };
    r.ctl.on_update_event(UpdateEvent::State(failed.clone()));
    assert_eq!(r.ctl.update_state(), Some(&failed));
    // A progress message arriving after the failure does not revive the download.
    r.ctl.on_update_event(UpdateEvent::State(UpdateState::Downloading { percent: 99 }));
    assert_eq!(r.ctl.update_state(), Some(&failed));
    r.send(UiToCore::InstallUpdate).await;
    assert_eq!(r.started(), 2);
    assert_eq!(r.ctl.update_state(), Some(&UpdateState::Downloading { percent: 0 }));
}
