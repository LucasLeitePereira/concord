//! F2 end-to-end (T-013, D-012): two in-process `Session`s (synthetic audio) and the
//! game-mode `Controller` of side A with the real `fake_ui` example and a fake
//! platform. Checks the enter/exit sequence, the `Status` the friend sees, that
//! voice keeps flowing, that a chat message in game mode becomes a notification
//! with only the sender, and mute/deafen.
//!
//! Needs the `fake_ui` example binary; the helper builds it on demand.
#![cfg(windows)]

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use sala_core::app::{Controller, Flow, PlatformApi, Step, Trigger, UiHost};
use sala_core::config::{Config, Friend, Profile};
use sala_core::identity::LocalIdentity;
use sala_core::ipc::IpcServer;
use sala_core::platform::{PlatformEvent, TrayState};
use sala_core::session::{AudioSource, Session, SessionEvent};
use sala_core::ui_process::ShutdownOutcome;
use sala_protocol::UiToCore;
use tokio::sync::mpsc;

const TEST_TIMEOUT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------- helpers

fn unique(tag: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "sala-f2-{tag}-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    )
}

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(unique(tag));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port()
}

fn fake_ui_exe() -> PathBuf {
    let profile_dir = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    let exe = profile_dir.join("examples").join("fake_ui.exe");
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        // Always (incremental, cheap): a stale fake_ui built before a protocol
        // change would fail to decode new events and exit.
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = std::process::Command::new(cargo)
            .args(["build", "-p", "sala-core", "--example", "fake_ui"])
            .status()
            .unwrap();
        assert!(st.success());
    });
    assert!(exe.is_file(), "fake_ui not found at {exe:?}");
    exe
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !cond() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn with_timeout<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, f).await.expect("test timed out")
}

fn make_profile(root: &Path, name: &str, port: u16, other: &str, other_port: u16) -> (Profile, Config, LocalIdentity) {
    let profile = Profile::resolve_in(root, Some(name)).unwrap();
    let mut config = Config::load_or_create(&profile).unwrap();
    config.port = port;
    config.friends = vec![Friend {
        name: other.into(),
        address: "127.0.0.1".into(),
        port: Some(other_port),
        fingerprint: None,
    }];
    config.save(&profile).unwrap();
    let identity = LocalIdentity::load_or_create(&profile.data_dir).unwrap();
    (profile, config, identity)
}

fn start(profile: Profile, config: Config, identity: &LocalIdentity) -> Session {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
    Session::start(profile, config, identity, addr, AudioSource::Synthetic).unwrap()
}

/// Records everything the controller asks of the platform, as debug strings.
#[derive(Clone, Default)]
struct FakePlatform {
    notifications: Arc<Mutex<Vec<String>>>,
    trays: Arc<Mutex<Vec<TrayState>>>,
    /// Every argument ever received, formatted, to search for leaked text.
    all_args: Arc<Mutex<Vec<String>>>,
}

impl PlatformApi for FakePlatform {
    fn set_tray(&self, state: TrayState) {
        self.all_args.lock().unwrap().push(format!("{state:?}"));
        self.trays.lock().unwrap().push(state);
    }
    fn notify_message(&self, sender: &str) {
        self.all_args.lock().unwrap().push(sender.to_string());
        self.notifications.lock().unwrap().push(sender.to_string());
    }
    fn notify_call(&self, sender: &str) {
        self.all_args.lock().unwrap().push(sender.to_string());
    }
    fn set_hotkeys(&self, _hotkeys: Vec<(sala_core::platform::HotkeyAction, sala_core::platform::Hotkey)>) {}
    fn set_ptt(&self, _hotkey: Option<sala_core::platform::Hotkey>) {}
}

/// Feeds the controller the session events that arrive within `window`.
async fn pump(ctl: &mut Controller<FakePlatform>, rx: &mut mpsc::UnboundedReceiver<SessionEvent>, window: Duration) {
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        ctl.on_session_event(ev);
    }
}

// ---------------------------------------------------------------- test

async fn one_run(round: usize) {
    let root = temp_root("data");
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let (profile_a, config_a, id_a) = make_profile(&root, "a", pa, "b", pb);
    let (profile_b, config_b, id_b) = make_profile(&root, "b", pb, "a", pa);
    let game_cfg = config_a.game_mode.clone();
    let a = Arc::new(start(profile_a, config_a, &id_a));
    let b = start(profile_b, config_b, &id_b);
    let mut a_events = a.take_events().unwrap();

    // A's UI: real process (fake_ui) on A's pipe.
    let pipe = format!(r"\\.\pipe\{}", unique("pipe"));
    let (ipc, mut ipc_rx) = IpcServer::start(&pipe).unwrap();
    let platform = FakePlatform::default();
    let mut ctl =
        Controller::new(a.clone(), UiHost::new(ipc, pipe, fake_ui_exe()), platform.clone(), &game_cfg, false);
    assert!(ctl.host_mut().launch(), "fake_ui must start");
    // fake_ui announces itself with GetState once it is connected to the pipe.
    let (client, cmd) = tokio::time::timeout(Duration::from_secs(15), ipc_rx.recv()).await.unwrap().unwrap();
    assert_eq!(cmd, UiToCore::GetState);
    ctl.on_command(client, cmd).await;

    wait_until("a and b connected", || a.is_connected("b") && b.is_connected("a")).await;
    wait_until("b has a's initial status", || b.peer_status("a").is_some()).await;
    // Simultaneous dialing yields two Connected from Net: the session reports one.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut connected = 0;
    while let Ok(ev) = a_events.try_recv() {
        if matches!(ev, SessionEvent::Connected { .. }) {
            connected += 1;
        }
        ctl.on_session_event(ev);
    }
    assert_eq!(connected, 1, "exactly one SessionEvent::Connected");
    assert!(!b.peer_status("a").unwrap().game_mode);
    // D-024: the app opens outside the room; join so there is a call to keep in game mode.
    a.set_in_room(true);
    b.set_in_room(true);

    // ---- enter game mode (manual, as the hotkey would)
    let received_before = b.voice_counters("a").received;
    ctl.clear_steps();
    let flow = ctl.on_platform_event(PlatformEvent::Hotkey(sala_core::platform::HotkeyAction::ToggleGameMode)).await;
    assert_eq!(flow, Flow::Continue);
    assert!(ctl.in_game());
    assert!(!ctl.host_mut().is_running(), "the UI process must be gone");
    let steps = ctl.steps().to_vec();
    let pos = |s: &Step| steps.iter().position(|x| x == s).unwrap_or_else(|| panic!("missing {s:?} in {steps:?}"));
    let ui_down = pos(&Step::UiShutdown(ShutdownOutcome::Exited));
    assert_eq!(ui_down, 0, "UI shutdown is the first step: {steps:?}");
    assert!(ui_down < pos(&Step::CloseViewer));
    assert!(pos(&Step::CloseViewer) < pos(&Step::StopSharing));
    assert!(pos(&Step::StopSharing) < pos(&Step::ReleaseMemory));
    assert!(pos(&Step::ReleaseMemory) < pos(&Step::StatusSent { game_mode: true }));
    assert!(pos(&Step::StatusSent { game_mode: true }) < pos(&Step::Tray { game_mode: true }));
    assert_eq!(
        steps.last(),
        Some(&Step::Cause { entering: true, trigger: Trigger::Hotkey }),
        "origin of the entry"
    );
    assert!(a.local_status().game_mode);
    assert!(platform.trays.lock().unwrap().last().unwrap().game_mode, "tray is amber");

    // B learns it; voice A -> B keeps flowing after the entry.
    wait_until("b sees a in game mode", || b.peer_status("a").is_some_and(|s| s.game_mode)).await;
    wait_until("voice keeps flowing", || b.voice_counters("a").received > received_before + 50).await;
    let (sent_now, got_now) = (a.voice_counters("b").sent, b.voice_counters("a").received);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(a.voice_counters("b").sent > sent_now + 50, "A still transmits in game mode");
    assert!(b.voice_counters("a").received > got_now + 50, "B still receives in game mode");

    // ---- chat from B while A is in game mode: notification with the sender only
    let secret = format!("SEGREDO-{round}-{}", unique("txt"));
    b.send_chat("a", &secret).unwrap();
    wait_until("chat reached a", || a.chat_history("b").len() == 1).await;
    pump(&mut ctl, &mut a_events, Duration::from_millis(300)).await;
    assert_eq!(*platform.notifications.lock().unwrap(), vec!["b".to_string()]);
    for arg in platform.all_args.lock().unwrap().iter() {
        assert!(!arg.contains(&secret), "chat text reached the platform: {arg}");
    }
    assert_eq!(a.chat_history("b")[0].text, secret, "control: the text did arrive");

    // ---- clicking the notification leaves game mode manually: UI relaunched, Status
    ctl.clear_steps();
    ctl.on_platform_event(PlatformEvent::NotificationClicked).await;
    assert!(!ctl.in_game());
    assert!(ctl.host_mut().is_running(), "UI relaunched");
    assert!(ctl.steps().contains(&Step::UiLaunched(true)));
    assert_eq!(
        ctl.steps().last(),
        Some(&Step::Cause { entering: false, trigger: Trigger::Notification }),
        "origin of the exit"
    );
    assert!(!a.local_status().game_mode);
    wait_until("b sees a out of game mode", || b.peer_status("a").is_some_and(|s| !s.game_mode)).await;
    assert!(!platform.trays.lock().unwrap().last().unwrap().game_mode);

    // Control: outside game mode a chat message is NOT a notification.
    b.send_chat("a", "outra").unwrap();
    wait_until("second chat reached a", || a.chat_history("b").len() == 2).await;
    pump(&mut ctl, &mut a_events, Duration::from_millis(300)).await;
    assert_eq!(platform.notifications.lock().unwrap().len(), 1);

    // ---- mute / deafen through the controller (inv-deafen-implies-mute)
    ctl.set_muted(true);
    assert!(a.local_status().muted);
    ctl.set_deafened(true);
    wait_until("b sees a deafened and muted", || b.peer_status("a").is_some_and(|s| s.deafened && s.muted)).await;
    ctl.set_deafened(false);
    assert!(a.local_status().muted && !a.local_status().deafened, "restores the previous mute");
    wait_until("b sees a undeafened", || b.peer_status("a").is_some_and(|s| !s.deafened && s.muted)).await;
    ctl.set_muted(false);
    ctl.set_deafened(true);
    ctl.set_deafened(false);
    assert!(!a.local_status().muted, "restores 'not muted'");
    wait_until("b sees a unmuted", || b.peer_status("a").is_some_and(|s| !s.deafened && !s.muted)).await;

    // The exit sequence is repeatable: automatic-style toggle once more.
    ctl.clear_steps();
    ctl.toggle_game_mode(Trigger::Ipc).await;
    assert!(ctl.in_game() && !ctl.host_mut().is_running());
    ctl.toggle_game_mode(Trigger::Menu).await;
    assert!(!ctl.in_game() && ctl.host_mut().is_running());
    let causes: Vec<_> = ctl.steps().iter().filter(|s| matches!(s, Step::Cause { .. })).cloned().collect();
    assert_eq!(
        causes,
        vec![
            Step::Cause { entering: true, trigger: Trigger::Ipc },
            Step::Cause { entering: false, trigger: Trigger::Menu },
        ]
    );

    ctl.shutdown().await;
    b.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f2_game_mode_end_to_end() {
    // One round by default; set F2_ROUNDS=3 to hunt flakiness (requirement of T-013).
    let rounds: usize = std::env::var("F2_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    for round in 0..rounds {
        with_timeout(one_run(round)).await;
    }
}
