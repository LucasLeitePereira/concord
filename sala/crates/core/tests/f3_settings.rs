//! F3 core support for the UI (T-016, D-015): room presence on the wire, voice gated by
//! it, friend management, settings applied live and saved, meter and call numbers.
//!
//! Two parts: sessions over loopback (room, friends, `CallStats`) and a `Controller`
//! driven through a real IPC pipe with a fake platform (settings, errors, meter).
#![cfg(windows)]

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sala_core::app::{Controller, PlatformApi, Trigger, UiHost};
use sala_core::calls::CallEnd;
use sala_core::config::{AudioMode, Config, Friend, Profile};
use sala_core::game_detector::ForegroundSample;
use sala_core::identity::{FriendEntry, LocalIdentity, TrustStore};
use sala_core::ipc::{ClientId, IpcClient, IpcServer};
use sala_core::net::{Net, NetEvent};
use sala_core::platform::{Hotkey, HotkeyAction, MenuItem, PlatformEvent, TrayState};
use sala_core::session::{AudioSource, CallStart, Session, SessionEvent};
use sala_protocol::{
    AudioSettings, Capabilities, ControlMessage, CoreToUi, GameModeSettings, HotkeySettings, UiToCore, VoiceMode,
};
use tokio::sync::mpsc;

fn unique(tag: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "sala-f3-{tag}-{}-{}-{}",
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

/// A free port never handed out before in this process (tests run in parallel and a
/// released port can otherwise be picked twice).
fn free_udp_port() -> u16 {
    static GIVEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    loop {
        let port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        let mut given = GIVEN.lock().unwrap();
        if !given.contains(&port) {
            given.push(port);
            return port;
        }
    }
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !cond() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn friend(name: &str, port: u16) -> Friend {
    Friend { name: name.into(), address: "127.0.0.1".into(), port: Some(port), fingerprint: None }
}

/// A session of profile `name` listening on `port` with the given friends.
fn start(root: &Path, name: &str, port: u16, friends: Vec<Friend>) -> (Session, Profile) {
    let profile = Profile::resolve_in(root, Some(name)).unwrap();
    let mut config = Config::load_or_create(&profile).unwrap();
    config.port = port;
    config.friends = friends;
    config.save(&profile).unwrap();
    let identity = LocalIdentity::load_or_create(&profile.data_dir).unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let s = Session::start(profile.clone(), config, &identity, addr, AudioSource::Synthetic).unwrap();
    (s, profile)
}

// ---------------------------------------------------------------- sessions

/// (A sent, A received, B sent, B received) voice frames.
fn counters(a: &Session, b: &Session) -> (u64, u64, u64, u64) {
    (
        a.voice_counters("b").sent,
        a.voice_counters("b").received,
        b.voice_counters("a").sent,
        b.voice_counters("a").received,
    )
}

async fn assert_voice_stopped(a: &Session, b: &Session, why: &str) {
    tokio::time::sleep(Duration::from_millis(500)).await; // in-flight datagrams
    let before = counters(a, b);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(counters(a, b), before, "voice must be silent: {why}");
}

async fn assert_voice_flows(a: &Session, b: &Session, why: &str) {
    let before = counters(a, b);
    wait_until(why, || {
        let now = counters(a, b);
        now.0 > before.0 + 20 && now.1 > before.1 + 20 && now.2 > before.2 + 20 && now.3 > before.3 + 20
    })
    .await;
}

async fn session_run() {
    let root = temp_root("net");
    let (pa, pb) = (free_udp_port(), free_udp_port());
    // A starts with no friends; B knows A and keeps dialing.
    let (a, profile_a) = start(&root, "a", pa, vec![]);
    let (b, _profile_b) = start(&root, "b", pb, vec![friend("a", pa)]);
    let mut a_events = a.take_events().unwrap();
    assert!(!a.is_connected("b"));
    // Unknown address: A refuses B until it adds it.
    a.add_friend("b", &format!("127.0.0.1:{pb}")).unwrap();
    assert!(a.add_friend("b", "127.0.0.1").is_err(), "duplicate name");
    assert!(a.add_friend("x", "not-an-ip").is_err());
    wait_until("connected", || a.is_connected("b") && b.is_connected("a")).await;
    wait_until("statuses", || a.peer_status("b").is_some() && b.peer_status("a").is_some()).await;
    // D-024: connected but outside the room, so no voice until each side joins.
    assert!(!a.peer_status("b").unwrap().in_room, "everybody starts outside the room");
    assert!(!a.local_status().in_room && !b.local_status().in_room);
    assert_voice_stopped(&a, &b, "nobody joined yet").await;
    a.set_in_room(true);
    assert_voice_stopped(&a, &b, "only one side joined").await;
    b.set_in_room(true);
    assert_voice_flows(&a, &b, "initial voice").await;

    // CallStats arrives every 2 s for connected friends.
    let mut config_changed = false;
    let stats = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match a_events.recv().await {
                Some(SessionEvent::CallStats { friend, rtt_ms, since_secs }) => return (friend, rtt_ms, since_secs),
                Some(SessionEvent::ConfigChanged) => config_changed = true,
                _ => {}
            }
        }
    })
    .await
    .expect("CallStats within 8 s");
    assert!(config_changed, "pinning the fingerprint (TOFU) must announce ConfigChanged");
    assert_eq!(stats.0, "b");
    assert!(stats.1 < 1000 && stats.2 < 60, "{stats:?}");

    // TOFU pin of the added friend reached config.toml.
    wait_until("pin saved", || {
        Config::load_or_create(&profile_a).unwrap().friends.iter().any(|f| f.name == "b" && f.fingerprint.is_some())
    })
    .await;

    // ---- A leaves the room: nothing flows either way, B still sees A connected.
    a.set_in_room(false);
    wait_until("b sees a out", || b.peer_status("a").is_some_and(|s| !s.in_room)).await;
    assert!(a.is_connected("b") && b.is_connected("a"), "chat/status stay on");
    assert_voice_stopped(&a, &b, "local out of the room").await;
    // Back in.
    a.set_in_room(true);
    wait_until("b sees a in", || b.peer_status("a").is_some_and(|s| s.in_room)).await;
    assert_voice_flows(&a, &b, "voice resumes").await;

    // ---- B leaves: A (still in) stops sending to B and plays nothing from it.
    b.set_in_room(false);
    wait_until("a sees b out", || a.peer_status("b").is_some_and(|s| !s.in_room)).await;
    assert_voice_stopped(&a, &b, "friend out of the room").await;
    b.set_in_room(true);
    wait_until("a sees b in", || a.peer_status("b").is_some_and(|s| s.in_room)).await;
    assert_voice_flows(&a, &b, "voice resumes again").await;

    // ---- Removing the friend closes the connection and A refuses its redials.
    a.remove_friend("b").unwrap();
    wait_until("closed", || !a.is_connected("b")).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(!a.is_connected("b"), "removed friend must stay out");
    assert!(Config::load_or_create(&profile_a).unwrap().friends.is_empty(), "saved");
    assert!(a.remove_friend("b").is_err());

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn room_friends_and_call_stats_over_loopback() {
    tokio::time::timeout(Duration::from_secs(120), session_run()).await.expect("test timed out");
}

// ---------------------------------------------------------------- controller

#[derive(Clone, Default)]
struct FakePlatform {
    hotkeys: Arc<Mutex<Vec<Vec<String>>>>,
    ptt: Arc<Mutex<Vec<Option<String>>>>,
    trays: Arc<Mutex<Vec<TrayState>>>,
    /// Senders of the "está te chamando" balloons.
    call_balloons: Arc<Mutex<Vec<String>>>,
}

impl PlatformApi for FakePlatform {
    fn set_tray(&self, state: TrayState) {
        self.trays.lock().unwrap().push(state);
    }
    fn notify_message(&self, _sender: &str) {}
    fn notify_call(&self, sender: &str) {
        self.call_balloons.lock().unwrap().push(sender.to_string());
    }
    fn set_hotkeys(&self, hotkeys: Vec<(HotkeyAction, Hotkey)>) {
        self.hotkeys.lock().unwrap().push(hotkeys.iter().map(|(a, h)| format!("{a}={h}")).collect());
    }
    fn set_ptt(&self, hotkey: Option<Hotkey>) {
        self.ptt.lock().unwrap().push(hotkey.map(|h| h.to_string()));
    }
}

struct Rig {
    ctl: Controller<FakePlatform>,
    platform: FakePlatform,
    pipe: String,
    ui: IpcClient,
    ipc_rx: mpsc::Receiver<(ClientId, UiToCore)>,
    events: mpsc::UnboundedReceiver<SessionEvent>,
    profile: Profile,
}

impl Rig {
    async fn new(friends: Vec<Friend>) -> Rig {
        Rig::on_port(free_udp_port(), friends).await
    }

    /// Like `new`, with the session listening on `port` (so a peer can be pointed at it).
    async fn on_port(port: u16, friends: Vec<Friend>) -> Rig {
        let root = temp_root("ctl");
        let (session, profile) = start(&root, "a", port, friends);
        let game_cfg = session.config().game_mode;
        let events = session.take_events().unwrap();
        let pipe = format!(r"\\.\pipe\{}", unique("pipe"));
        let (ipc, ipc_rx) = IpcServer::start(&pipe).unwrap();
        let platform = FakePlatform::default();
        let ctl = Controller::new(
            Arc::new(session),
            UiHost::new(ipc, pipe.clone(), PathBuf::from("does-not-exist.exe")),
            platform.clone(),
            &game_cfg,
            false,
        );
        let ui = IpcClient::connect(&pipe).await.unwrap();
        Rig { ctl, platform, pipe, ui, ipc_rx, events, profile }
    }

    /// Sends `cmd` as the UI would, lets the controller handle it, and returns what the UI got.
    async fn ask(&mut self, cmd: UiToCore) -> Vec<CoreToUi> {
        self.ui.send(&cmd).await.unwrap();
        let (id, got) = tokio::time::timeout(Duration::from_secs(5), self.ipc_rx.recv()).await.unwrap().unwrap();
        assert_eq!(got, cmd);
        self.ctl.on_command(id, got).await;
        self.drain().await
    }

    async fn drain(&mut self) -> Vec<CoreToUi> {
        let mut out = Vec::new();
        while let Ok(Ok(Some(m))) = tokio::time::timeout(Duration::from_millis(200), self.ui.recv()).await {
            out.push(m);
        }
        out
    }

    /// Feeds the controller the session events of the next `window`.
    async fn pump(&mut self, window: Duration) -> Vec<CoreToUi> {
        let deadline = tokio::time::Instant::now() + window;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, self.events.recv()).await {
            self.ctl.on_session_event(ev);
        }
        self.drain().await
    }

    fn saved(&self) -> Config {
        Config::load_or_create(&self.profile).unwrap()
    }
}

fn error_of(msgs: &[CoreToUi]) -> Option<&str> {
    msgs.iter().find_map(|m| match m {
        CoreToUi::Error { message } => Some(message.as_str()),
        _ => None,
    })
}

fn config_of(msgs: &[CoreToUi]) -> Option<&CoreToUi> {
    msgs.iter().find(|m| matches!(m, CoreToUi::Config { .. }))
}

fn audio_settings(cfg: &Config) -> AudioSettings {
    let CoreToUi::Config { audio, .. } = sala_core::settings::config_message(cfg) else { unreachable!() };
    audio
}

fn hotkey_settings(cfg: &Config) -> HotkeySettings {
    let CoreToUi::Config { hotkeys, .. } = sala_core::settings::config_message(cfg) else { unreachable!() };
    hotkeys
}

fn game_settings(cfg: &Config) -> GameModeSettings {
    let CoreToUi::Config { game_mode, .. } = sala_core::settings::config_message(cfg) else { unreachable!() };
    game_mode
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_config_applies_live_saves_and_rejects_atomically() {
    let mut rig = Rig::new(vec![friend("b", free_udp_port())]).await;
    let before = rig.saved();

    // GetConfig mirrors the file.
    let got = rig.ask(UiToCore::GetConfig).await;
    let Some(CoreToUi::Config { name, friends, .. }) = config_of(&got) else { panic!("no Config in {got:?}") };
    assert_eq!((name.as_str(), friends.len()), (before.name.as_str(), 1));

    // Game list: x.exe is not a game yet (control), becomes one after the update.
    // Windowed: only the games list can make it positive.
    let x = ForegroundSample { exe_path: r"C:\games\x.exe".into(), covers_monitor: false };
    for _ in 0..5 {
        rig.ctl.tick(Some(&x)).await;
    }
    assert!(!rig.ctl.in_game(), "control: x.exe is not in the list");

    let mut audio = audio_settings(&before);
    audio.vad_threshold = -40;
    audio.input_volume = 150;
    audio.bitrate_kbps = 64;
    audio.mode = VoiceMode::PushToTalk;
    let mut hk = hotkey_settings(&before);
    hk.toggle_mute = "Ctrl+Alt+M".into();
    hk.push_to_talk = "Mouse5".into();
    let mut game = game_settings(&before);
    game.games.push("x.exe".into());
    game.keep_sharing_in_game_mode = true;
    let got = rig
        .ask(UiToCore::UpdateConfig { name: Some("Zé".into()), audio: Some(audio.clone()), game_mode: Some(game), hotkeys: Some(hk) })
        .await;
    assert!(error_of(&got).is_none(), "{got:?}");
    let Some(CoreToUi::Config { name, audio: a2, hotkeys, game_mode, .. }) = config_of(&got) else { panic!("{got:?}") };
    assert_eq!(name, "Zé");
    assert_eq!(a2, &audio);
    assert_eq!(hotkeys.toggle_mute, "Ctrl+Alt+M");
    assert!(game_mode.games.contains(&"x.exe".to_string()));

    // Saved in the profile's config.toml (commented format).
    let saved = rig.saved();
    assert_eq!(saved.name, "Zé");
    assert_eq!((saved.audio.vad_threshold, saved.audio.input_volume, saved.audio.bitrate_kbps), (-40, 150, 64));
    assert_eq!(saved.audio.mode, AudioMode::PushToTalk);
    assert_eq!(saved.hotkeys.push_to_talk, "Mouse5");
    assert!(saved.game_mode.keep_sharing_in_game_mode);
    assert_eq!(saved.friends, before.friends, "friends untouched");
    let text = std::fs::read_to_string(rig.profile.config_path()).unwrap();
    assert!(text.contains("# "), "commented template format");

    // Applied live: platform got the shortcuts and the PTT key; the detector the new list.
    let hotkeys = rig.platform.hotkeys.lock().unwrap().last().cloned().unwrap();
    assert_eq!(hotkeys.len(), 3);
    assert!(hotkeys.iter().any(|h| h.starts_with("toggle_mute=") && h.contains("Alt")), "{hotkeys:?}");
    assert_eq!(rig.platform.ptt.lock().unwrap().last().cloned().unwrap(), Some("Mouse5".to_string()));
    for _ in 0..5 {
        rig.ctl.tick(Some(&x)).await;
    }
    assert!(rig.ctl.in_game(), "x.exe is now a game");
    rig.ctl.set_game_mode_manual(false, Trigger::Ipc).await;

    // Switching back to voice activity clears the PTT key.
    let mut audio_va = audio.clone();
    audio_va.mode = VoiceMode::VoiceActivity;
    rig.ask(UiToCore::UpdateConfig { name: None, audio: Some(audio_va), game_mode: None, hotkeys: None }).await;
    assert_eq!(rig.platform.ptt.lock().unwrap().last().cloned().unwrap(), None);

    // ---- invalid updates change nothing (file, platform) and answer Error.
    let calls = rig.platform.hotkeys.lock().unwrap().len();
    let file_before = std::fs::read_to_string(rig.profile.config_path()).unwrap();
    let mut bad_audio = audio.clone();
    bad_audio.bitrate_kbps = 500;
    let got = rig.ask(UiToCore::UpdateConfig { name: Some("Outro".into()), audio: Some(bad_audio), game_mode: None, hotkeys: None }).await;
    assert!(error_of(&got).is_some_and(|m| m.contains("bitrate")), "{got:?}");
    assert!(config_of(&got).is_none(), "no Config broadcast on error");
    let mut bad_hk = hotkey_settings(&rig.saved());
    bad_hk.toggle_deafen = "Ctrl+NotAKey".into();
    let got = rig
        .ask(UiToCore::UpdateConfig { name: Some("Outro".into()), audio: None, game_mode: None, hotkeys: Some(bad_hk) })
        .await;
    assert!(error_of(&got).is_some_and(|m| m.contains("toggle_deafen")), "{got:?}");
    assert_eq!(std::fs::read_to_string(rig.profile.config_path()).unwrap(), file_before, "file untouched");
    assert_eq!(rig.saved().name, "Zé", "the valid part of a rejected update is not applied");
    assert_eq!(rig.platform.hotkeys.lock().unwrap().len(), calls, "platform untouched");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn friend_management_and_reset_fingerprint_persist() {
    let mut pinned = friend("b", free_udp_port());
    pinned.fingerprint = Some("ab".repeat(32));
    let mut rig = Rig::new(vec![pinned]).await;

    let got = rig.ask(UiToCore::GetConfig).await;
    let Some(CoreToUi::Config { friends, .. }) = config_of(&got) else { panic!() };
    assert!(friends[0].verified);

    // Reset: unverified in the broadcast and in the file; the address stays.
    let got = rig.ask(UiToCore::ResetFingerprint { name: "b".into() }).await;
    let Some(CoreToUi::Config { friends, .. }) = config_of(&got) else { panic!("{got:?}") };
    assert!(!friends[0].verified);
    assert_eq!(rig.saved().friends[0].fingerprint, None);
    assert_eq!(rig.saved().friends[0].address, "127.0.0.1");
    let got = rig.ask(UiToCore::ResetFingerprint { name: "nobody".into() }).await;
    assert!(error_of(&got).is_some());

    // Add: broadcast + file; duplicates and bad addresses are errors.
    let got = rig.ask(UiToCore::AddFriend { name: "cris".into(), address: "100.64.0.9".into() }).await;
    let Some(CoreToUi::Config { friends, .. }) = config_of(&got) else { panic!("{got:?}") };
    assert_eq!(friends.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["b", "cris"]);
    assert!(!friends[1].verified);
    assert_eq!(rig.saved().friends.len(), 2);
    assert_eq!(rig.saved().friends[1].address, "100.64.0.9");
    for (n, a) in [("cris", "100.64.0.10"), ("dani", "100.x.y.z"), ("", "100.64.0.11"), ("eva", "")] {
        let got = rig.ask(UiToCore::AddFriend { name: n.into(), address: a.into() }).await;
        assert!(error_of(&got).is_some(), "{n:?} {a:?}: {got:?}");
    }
    assert_eq!(rig.saved().friends.len(), 2, "rejected adds are not saved");

    // Remove.
    let got = rig.ask(UiToCore::RemoveFriend { name: "cris".into() }).await;
    let Some(CoreToUi::Config { friends, .. }) = config_of(&got) else { panic!("{got:?}") };
    assert_eq!(friends.len(), 1);
    assert_eq!(rig.saved().friends.len(), 1);
    let got = rig.ask(UiToCore::RemoveFriend { name: "cris".into() }).await;
    assert!(error_of(&got).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn room_commands_status_devices_volume_and_meter() {
    let mut rig = Rig::new(vec![friend("b", free_udp_port())]).await;

    // GetState carries in_room (false at start, D-024); JoinRoom / HangUp / Call flip it and
    // tell the UI.
    let got = rig.ask(UiToCore::GetState).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: false }), "{got:?}");
    assert!(got.contains(&CoreToUi::Calls { incoming: vec![], outgoing: vec![] }), "{got:?}");
    let got = rig.ask(UiToCore::JoinRoom).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: true }), "{got:?}");
    assert!(rig.ctl.session().local_status().in_room);
    let got = rig.ask(UiToCore::HangUp).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: false }), "{got:?}");
    assert!(!rig.ctl.session().local_status().in_room);
    let got = rig.ask(UiToCore::Call { friend: "b".into() }).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: true }), "{got:?}");
    let got = rig.ask(UiToCore::Call { friend: "nobody".into() }).await;
    assert!(error_of(&got).is_some());
    assert!(rig.ctl.session().local_status().in_room, "a failed Call changes nothing");
    // Control: an unknown friend is not rung either.
    assert!(rig.ctl.session().calls().1.is_empty());

    // Devices: "default" first in both lists.
    let got = rig.ask(UiToCore::ListAudioDevices).await;
    let Some(CoreToUi::AudioDevices { inputs, outputs }) = got.first() else { panic!("{got:?}") };
    assert_eq!(inputs.first().map(String::as_str), Some("default"));
    assert_eq!(outputs.first().map(String::as_str), Some("default"));

    // Volume: known friends only.
    assert!(error_of(&rig.ask(UiToCore::SetPeerVolume { friend: "b".into(), volume: 1.5 }).await).is_none());
    assert!(error_of(&rig.ask(UiToCore::SetPeerVolume { friend: "zzz".into(), volume: 1.5 }).await).is_some());

    // Meter: ~20 events/s only while on; synthetic mic is a -12 dBFS-peak sine (RMS about -15).
    let quiet = rig.pump(Duration::from_millis(400)).await;
    assert!(!quiet.iter().any(|m| matches!(m, CoreToUi::MicLevel { .. })), "off by default");
    assert!(rig.ask(UiToCore::SetMicMeter { on: true }).await.is_empty());
    let levels: Vec<f32> = rig
        .pump(Duration::from_millis(1000))
        .await
        .iter()
        .filter_map(|m| if let CoreToUi::MicLevel { db } = m { Some(*db) } else { None })
        .collect();
    assert!((8..=24).contains(&levels.len()), "about 20 events in 1 s, got {}", levels.len());
    assert!(levels.iter().skip(1).all(|db| (-19.0..=-11.0).contains(db)), "{levels:?}");
    rig.ask(UiToCore::SetMicMeter { on: false }).await;
    rig.pump(Duration::from_millis(150)).await; // in-flight events
    let after = rig.pump(Duration::from_millis(500)).await;
    assert!(!after.iter().any(|m| matches!(m, CoreToUi::MicLevel { .. })), "off stops the events");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mic_test_state_follows_requests_game_mode_and_client() {
    let mut rig = Rig::new(vec![]).await;
    let on = CoreToUi::MicTest { on: true };
    let off = CoreToUi::MicTest { on: false };
    // GetState reports it off (control), then on after SetMicTest.
    assert!(rig.ask(UiToCore::GetState).await.contains(&off));
    assert!(rig.ask(UiToCore::SetMicTest { on: true }).await.contains(&on));
    assert!(rig.ask(UiToCore::GetState).await.contains(&on));
    assert!(rig.ask(UiToCore::SetMicTest { on: false }).await.contains(&off));
    // Game mode turns it off and refuses it.
    assert!(rig.ask(UiToCore::SetMicTest { on: true }).await.contains(&on));
    let got = rig.ask(UiToCore::EnterGameMode).await;
    assert!(got.contains(&off), "{got:?}");
    assert!(rig.ask(UiToCore::GetState).await.contains(&off));
    let got = rig.ask(UiToCore::SetMicTest { on: true }).await;
    assert!(!got.contains(&on), "refused in game mode: {got:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mic_test_stops_when_the_ui_client_disconnects() {
    let mut rig = Rig::new(vec![]).await;
    let mut ui = IpcClient::connect(&rig.pipe).await.unwrap();
    ui.send(&UiToCore::SetMicTest { on: true }).await.unwrap();
    let (client, command) = tokio::time::timeout(Duration::from_secs(5), rig.ipc_rx.recv()).await.unwrap().unwrap();
    rig.ctl.on_command(client, command).await;
    drop(ui);
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.ctl.check_capture_client();
    let got = rig.pump(Duration::from_millis(200)).await;
    assert!(got.contains(&CoreToUi::MicTest { on: false }), "disconnect must turn the test off: {got:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mic_meter_stops_when_the_ui_client_disconnects() {
    let mut rig = Rig::new(vec![]).await;
    let mut ui = IpcClient::connect(&rig.pipe).await.unwrap();
    ui.send(&UiToCore::SetMicMeter { on: true }).await.unwrap();
    let (client, command) = tokio::time::timeout(Duration::from_secs(5), rig.ipc_rx.recv()).await.unwrap().unwrap();
    assert_eq!(command, UiToCore::SetMicMeter { on: true });
    rig.ctl.on_command(client, command).await;

    let started = rig.pump(Duration::from_millis(400)).await;
    assert!(started.iter().any(|m| matches!(m, CoreToUi::MicLevel { .. })), "meter should run while its UI client is connected");

    drop(ui);
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.ctl.check_capture_client();
    rig.pump(Duration::from_millis(150)).await; // discard in-flight level events
    let after = rig.pump(Duration::from_millis(300)).await;
    assert!(!after.iter().any(|m| matches!(m, CoreToUi::MicLevel { .. })), "disconnect must stop the mic meter");
}

// ---------------------------------------------------------------- calls (T-032, D-024)

/// Collects the events of a session in the background.
#[derive(Clone)]
struct EventLog(Arc<Mutex<Vec<SessionEvent>>>);

impl EventLog {
    fn watch(s: &Session) -> EventLog {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut rx = s.take_events().unwrap();
        let sink = log.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                sink.lock().unwrap().push(ev);
            }
        });
        EventLog(log)
    }

    /// `(friend, how it ended)` of every `CallResult` so far.
    fn results(&self) -> Vec<(String, CallEnd)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::CallResult { friend, end } => Some((friend.clone(), *end)),
                _ => None,
            })
            .collect()
    }

    fn has(&self, friend: &str, end: CallEnd) -> bool {
        self.results().contains(&(friend.to_string(), end))
    }
}

/// Both sides dial at start; the net layer replaces the duplicate connection about 1 s
/// later, and that reconnect forgets pending rings (as it should). Let it happen first.
async fn settle_connection() {
    tokio::time::sleep(Duration::from_millis(1600)).await;
}

/// Two connected sessions, both outside the room, with their event logs.
async fn connected_pair() -> (Session, Session, EventLog, EventLog) {
    let root = temp_root("calls");
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let (a, _) = start(&root, "a", pa, vec![friend("b", pb)]);
    let (b, _) = start(&root, "b", pb, vec![friend("a", pa)]);
    let (la, lb) = (EventLog::watch(&a), EventLog::watch(&b));
    wait_until("connected", || a.peer_status("b").is_some() && b.peer_status("a").is_some()).await;
    settle_connection().await;
    (a, b, la, lb)
}

async fn close(a: Session, b: Session) {
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ring_then_accept_puts_both_in_the_room_with_voice() {
    let (a, b, la, lb) = connected_pair().await;
    assert!(!a.local_status().in_room && !b.local_status().in_room);

    assert_eq!(a.call("b"), CallStart::Ringing);
    assert!(a.local_status().in_room, "calling enters the room");
    assert_eq!(a.calls(), (vec![], vec!["b".to_string()]));
    wait_until("b is rung", || b.calls().0 == ["a"]).await;
    assert!(!b.local_status().in_room, "ringing does not pull b into the room");
    assert_voice_stopped(&a, &b, "b has not answered yet").await;

    b.accept_call("a");
    assert!(b.local_status().in_room && b.calls().0.is_empty());
    wait_until("a's ring is answered", || a.calls().1.is_empty()).await;
    assert_voice_flows(&a, &b, "voice after accepting").await;
    assert!(la.results().is_empty() && lb.results().is_empty(), "an answered ring is not a failure");
    close(a, b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declined_ring_tells_the_caller_who_stays_in_the_room() {
    let (a, b, la, lb) = connected_pair().await;
    assert_eq!(a.call("b"), CallStart::Ringing);
    wait_until("b is rung", || b.calls().0 == ["a"]).await;

    b.decline_call("a");
    wait_until("a hears the decline", || la.has("b", CallEnd::Declined)).await;
    assert!(a.calls().1.is_empty() && b.calls().0.is_empty());
    assert!(a.local_status().in_room && !b.local_status().in_room);
    assert!(lb.results().is_empty(), "the one who declined gets no result");
    // Declining twice (or a ring that is gone) is harmless.
    b.decline_call("a");
    close(a, b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unanswered_ring_times_out_for_both_sides() {
    let (a, b, la, lb) = connected_pair().await;
    a.set_ring_timeout(Duration::from_millis(900));
    assert_eq!(a.call("b"), CallStart::Ringing);
    wait_until("b is rung", || b.calls().0 == ["a"]).await;
    // Control: it is still ringing before the deadline.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.calls().1, ["b"]);
    assert!(la.results().is_empty());

    wait_until("a times out", || la.has("b", CallEnd::Timeout)).await;
    wait_until("b sees the cancel", || lb.has("a", CallEnd::Cancelled)).await;
    assert!(a.calls().1.is_empty() && b.calls().0.is_empty());
    assert!(a.local_status().in_room, "the caller stays in the room");
    close(a, b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hanging_up_while_ringing_cancels_it() {
    let (a, b, la, lb) = connected_pair().await;
    assert_eq!(a.call("b"), CallStart::Ringing);
    wait_until("b is rung", || b.calls().0 == ["a"]).await;

    a.set_in_room(false);
    wait_until("b sees the cancel", || lb.has("a", CallEnd::Cancelled)).await;
    assert!(a.calls().1.is_empty() && b.calls().0.is_empty());
    assert!(la.results().is_empty(), "giving up is silent for the caller");
    // The timer of the cancelled ring must not fire later.
    a.set_ring_timeout(Duration::from_millis(50));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!la.has("b", CallEnd::Timeout));
    close(a, b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn joining_the_room_rings_nobody_and_calling_a_friend_already_there_does_not_ring() {
    let (a, b, la, _lb) = connected_pair().await;
    a.set_in_room(true);
    wait_until("b sees a in", || b.peer_status("a").is_some_and(|s| s.in_room)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(b.calls().0.is_empty() && a.calls().1.is_empty(), "JoinRoom rings nobody");

    // b joins by itself; a "call" now has nobody to ring.
    b.set_in_room(true);
    wait_until("a sees b in", || a.peer_status("b").is_some_and(|s| s.in_room)).await;
    assert_eq!(a.call("b"), CallStart::AlreadyInRoom);
    assert!(a.calls().1.is_empty() && la.results().is_empty());
    assert_voice_flows(&a, &b, "both joined without a ring").await;
    close(a, b).await;
}

/// A bare network peer that announces `protocol_version` in its `Hello`; returns what
/// `Session::call` did and whether the `Ring` frame reached the peer.
async fn call_a_bare_peer(protocol_version: u16) -> (CallStart, bool) {
    let root = temp_root("proto");
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let identity = LocalIdentity::load_or_create(&root.join("bare")).unwrap();
    let trust = TrustStore::new(vec![FriendEntry { name: "a".into(), address: Ipv4Addr::LOCALHOST.into(), fingerprint: None }]);
    // Bind the bare peer first, so a's first dial finds it.
    let (net, mut rx) = Net::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, pb)), &identity, trust).unwrap();
    let (a, _) = start(&root, "a", pa, vec![friend("b", pb)]);
    // Wait for a's dial to land, then speak like a peer of that version.
    tokio::time::timeout(Duration::from_secs(15), async {
        while !matches!(rx.recv().await, Some(NetEvent::Connected { .. })) {}
    })
    .await
    .expect("a connects to the bare peer");
    let hello = ControlMessage::Hello {
        name: "b".into(),
        app_version: "test".into(),
        protocol_version,
        capabilities: Capabilities::default(),
    };
    let status = ControlMessage::Status { muted: false, deafened: false, game_mode: false, sharing: false, in_room: false };
    net.send_control("a", hello).unwrap();
    net.send_control("a", status).unwrap();
    wait_until("a knows the peer", || a.peer_status("b").is_some()).await;

    let started = a.call("b");
    let mut ring_seen = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        ring_seen |= matches!(ev, NetEvent::Control { msg: ControlMessage::Ring { .. }, .. });
    }
    net.shutdown().await;
    a.shutdown().await;
    (started, ring_seen)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rings_are_only_sent_to_peers_that_understand_them() {
    // An old peer drops its control stream on an unknown variant: never send it a Ring.
    let (started, ring_seen) = call_a_bare_peer(1).await;
    assert_eq!(started, CallStart::Unsupported);
    assert!(!ring_seen, "a protocol-1 peer must not receive a Ring");
    // Control: a peer that announces version 2 is rung.
    let (started, ring_seen) = call_a_bare_peer(2).await;
    assert_eq!(started, CallStart::Ringing);
    assert!(ring_seen, "a protocol-2 peer receives the Ring");
}

fn calls_of(msgs: &[CoreToUi]) -> Vec<(Vec<String>, Vec<String>)> {
    msgs.iter()
        .filter_map(|m| match m {
            CoreToUi::Calls { incoming, outgoing } => Some((incoming.clone(), outgoing.clone())),
            _ => None,
        })
        .collect()
}

fn results_of(msgs: &[CoreToUi]) -> Vec<(String, String)> {
    msgs.iter()
        .filter_map(|m| match m {
            CoreToUi::CallResult { friend, result } => Some((friend.clone(), result.clone())),
            _ => None,
        })
        .collect()
}

/// A controller rig for `a` plus a plain session `b` that knows it.
async fn rig_with_peer() -> (Rig, Session, EventLog) {
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let rig = Rig::on_port(pa, vec![friend("b", pb)]).await;
    let (b, _) = start(&temp_root("peer"), "b", pb, vec![friend("a", pa)]);
    let log = EventLog::watch(&b);
    wait_until("connected", || rig.ctl.session().peer_status("b").is_some() && b.peer_status("a").is_some()).await;
    settle_connection().await;
    (rig, b, log)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rings_reach_the_ui_and_its_answers_go_back() {
    let (mut rig, b, b_log) = rig_with_peer().await;

    // b rings: the UI gets the snapshot, the tray lists the caller, no balloon (not in game mode).
    assert_eq!(b.call("a"), CallStart::Ringing);
    let got = rig.pump(Duration::from_millis(800)).await;
    assert_eq!(calls_of(&got).last(), Some(&(vec!["b".to_string()], vec![])), "{got:?}");
    assert_eq!(rig.platform.trays.lock().unwrap().last().unwrap().incoming_calls, ["b"]);
    assert!(rig.platform.call_balloons.lock().unwrap().is_empty());
    // A UI that connects later learns it from GetState.
    let got = rig.ask(UiToCore::GetState).await;
    assert!(calls_of(&got).contains(&(vec!["b".to_string()], vec![])), "{got:?}");

    // Decline: b is told, the snapshot empties, a stays outside.
    rig.ask(UiToCore::DeclineCall { friend: "b".into() }).await;
    let got = rig.pump(Duration::from_millis(500)).await;
    assert_eq!(calls_of(&got).last(), Some(&(vec![], vec![])), "{got:?}");
    wait_until("b hears the decline", || b_log.has("a", CallEnd::Declined)).await;
    assert!(!rig.ctl.session().local_status().in_room);

    // Accept: a joins, the status goes out, the ring is over on both sides.
    b.call("a");
    rig.pump(Duration::from_millis(500)).await;
    let got = rig.ask(UiToCore::AcceptCall { friend: "b".into() }).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: true }), "{got:?}");
    let got = rig.pump(Duration::from_millis(500)).await;
    assert_eq!(calls_of(&got).last(), Some(&(vec![], vec![])), "{got:?}");
    wait_until("b's ring is answered", || b.calls().1.is_empty()).await;

    // The caller gives up while ringing: the UI gets `cancelled`.
    rig.ask(UiToCore::HangUp).await;
    b.set_in_room(false);
    wait_until("both out", || rig.ctl.session().peer_status("b").is_some_and(|s| !s.in_room)).await;
    b.call("a");
    rig.pump(Duration::from_millis(500)).await;
    b.set_in_room(false);
    let got = rig.pump(Duration::from_millis(800)).await;
    assert_eq!(results_of(&got), [("b".to_string(), "cancelled".to_string())], "{got:?}");

    // `Call` over IPC enters the room and rings b.
    let got = rig.ask(UiToCore::Call { friend: "b".into() }).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: true }), "{got:?}");
    wait_until("b is rung by the UI", || b.calls().0 == ["a"]).await;
    let got = rig.pump(Duration::from_millis(400)).await;
    assert_eq!(calls_of(&got).last(), Some(&(vec![], vec!["b".to_string()])), "{got:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn game_mode_shows_a_balloon_and_the_tray_answers_the_ring() {
    let (mut rig, b, b_log) = rig_with_peer().await;
    rig.ctl.set_game_mode_manual(true, Trigger::Ipc).await;
    assert!(rig.ctl.in_game());

    b.call("a");
    rig.pump(Duration::from_millis(800)).await;
    assert_eq!(*rig.platform.call_balloons.lock().unwrap(), ["b"]);
    assert_eq!(rig.platform.trays.lock().unwrap().last().unwrap().incoming_calls, ["b"]);

    // Clicking the balloon joins the room and stays in game mode.
    rig.ctl.on_platform_event(PlatformEvent::CallNotificationClicked { friend: "b".into() }).await;
    assert!(rig.ctl.session().local_status().in_room);
    assert!(rig.ctl.in_game(), "answering does not leave game mode");
    rig.pump(Duration::from_millis(500)).await;
    assert!(rig.platform.trays.lock().unwrap().last().unwrap().incoming_calls.is_empty());

    // The tray's "Recusar" refuses the next ring.
    rig.ctl.set_in_room(false);
    b.set_in_room(false);
    wait_until("both out", || b.peer_status("a").is_some_and(|s| !s.in_room) && b.calls().1.is_empty()).await;
    b.call("a");
    rig.pump(Duration::from_millis(800)).await;
    assert_eq!(rig.platform.call_balloons.lock().unwrap().len(), 2, "a new ring, a new balloon");
    rig.ctl.on_platform_event(PlatformEvent::Menu(MenuItem::DeclineCall("b".into()))).await;
    wait_until("b hears the decline", || b_log.has("a", CallEnd::Declined)).await;
    assert!(!rig.ctl.session().local_status().in_room);
    assert!(rig.ctl.in_game());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ui_focus_follows_the_client_and_is_off_in_game_mode() {
    let mut rig = Rig::new(vec![]).await;
    assert!(!rig.ctl.ui_focused(), "no focus report yet: sounds may play");
    rig.ask(UiToCore::SetUiFocus { focused: true }).await;
    assert!(rig.ctl.ui_focused());
    rig.ask(UiToCore::SetUiFocus { focused: false }).await;
    assert!(!rig.ctl.ui_focused());

    // A UI that dies while focused must not mute the chat sound for good.
    let mut ui = IpcClient::connect(&rig.pipe).await.unwrap();
    ui.send(&UiToCore::SetUiFocus { focused: true }).await.unwrap();
    let (client, command) = tokio::time::timeout(Duration::from_secs(5), rig.ipc_rx.recv()).await.unwrap().unwrap();
    rig.ctl.on_command(client, command).await;
    assert!(rig.ctl.ui_focused());
    drop(ui);
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.ctl.check_capture_client();
    assert!(!rig.ctl.ui_focused());

    // Game mode: the UI is closed, so it is never "in focus", whatever it said before.
    rig.ask(UiToCore::SetUiFocus { focused: true }).await;
    assert!(rig.ctl.ui_focused());
    rig.ctl.set_game_mode_manual(true, Trigger::Ipc).await;
    assert!(!rig.ctl.ui_focused());
}
