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
use sala_core::config::{AudioMode, Config, Friend, Profile};
use sala_core::game_detector::ForegroundSample;
use sala_core::identity::LocalIdentity;
use sala_core::ipc::{ClientId, IpcClient, IpcServer};
use sala_core::platform::{Hotkey, HotkeyAction, TrayState};
use sala_core::session::{AudioSource, Session, SessionEvent};
use sala_protocol::{AudioSettings, CoreToUi, GameModeSettings, HotkeySettings, UiToCore, VoiceMode};
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

fn free_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port()
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
    assert!(a.peer_status("b").unwrap().in_room, "everybody starts in the room");
    assert_voice_flows(&a, &b, "initial voice").await;

    // CallStats arrives every 2 s for connected friends.
    let stats = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(SessionEvent::CallStats { friend, rtt_ms, since_secs }) = a_events.recv().await {
                return (friend, rtt_ms, since_secs);
            }
        }
    })
    .await
    .expect("CallStats within 8 s");
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
}

impl PlatformApi for FakePlatform {
    fn set_tray(&self, state: TrayState) {
        self.trays.lock().unwrap().push(state);
    }
    fn notify_message(&self, _sender: &str) {}
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
    ui: IpcClient,
    ipc_rx: mpsc::Receiver<(ClientId, UiToCore)>,
    events: mpsc::UnboundedReceiver<SessionEvent>,
    profile: Profile,
}

impl Rig {
    async fn new(friends: Vec<Friend>) -> Rig {
        let root = temp_root("ctl");
        let (session, profile) = start(&root, "a", free_udp_port(), friends);
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
        Rig { ctl, platform, ui, ipc_rx, events, profile }
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

    // GetState carries in_room; HangUp / Call flip it and tell the UI.
    let got = rig.ask(UiToCore::GetState).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: true }), "{got:?}");
    let got = rig.ask(UiToCore::HangUp).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: false }), "{got:?}");
    assert!(!rig.ctl.session().local_status().in_room);
    let got = rig.ask(UiToCore::Call { friend: "b".into() }).await;
    assert!(got.contains(&CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: true }), "{got:?}");
    let got = rig.ask(UiToCore::Call { friend: "nobody".into() }).await;
    assert!(error_of(&got).is_some());
    assert!(rig.ctl.session().local_status().in_room, "a failed Call changes nothing");

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
