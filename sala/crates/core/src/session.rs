//! Core event loop as a library object (`core-process`): net -> voice, Hello/Status,
//! TOFU pin persistence and the in-memory chat (`chat-history`).
//!
//! Chat is RAM only (`inv-chat-never-on-disk`): the history of a friend lives in
//! `State::history`, is dropped on `Disconnected` and on shutdown, and the text is
//! never logged (only id and byte length).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sala_protocol::{
    AudioStream, Capabilities, ChatEntry, ControlMessage, MediaDatagram, PROTOCOL_VERSION,
    max_audio_payload,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};

use crate::audio_device::{AudioInput, Sink};
use crate::config::{AudioMode, Config, Profile};
use crate::identity::LocalIdentity;
use crate::net::{Net, NetError, NetEvent, trust_store_from_config};
use crate::voice::{Activation, SineSource, VoiceEngine};

/// Where captured audio comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    /// Sine wave generator, null sink (tests, `--synthetic-audio`).
    Synthetic,
    /// Configured input/output devices.
    Device,
}

/// Voice counters of one friend. Kept after `Disconnected` (they hold no content).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceCounters {
    /// Voice datagrams handed to the connection towards this friend.
    pub sent: u64,
    /// Voice frames received from this friend (jitter buffer input).
    pub received: u64,
    pub played: u64,
    pub lost: u64,
    pub late_dropped: u64,
}

/// The four `Status` flags (local or of a friend).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusFlags {
    pub muted: bool,
    pub deafened: bool,
    pub game_mode: bool,
    pub sharing: bool,
}

impl StatusFlags {
    fn to_message(self) -> ControlMessage {
        ControlMessage::Status {
            muted: self.muted,
            deafened: self.deafened,
            game_mode: self.game_mode,
            sharing: self.sharing,
        }
    }
}

/// What the layer above (UI/tray) must hear about, in order, per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Connected { friend: String },
    Disconnected { friend: String },
    Reconnecting { friend: String, attempt: u32 },
    /// A friend announced a new `Status`.
    PeerStatus { friend: String, status: StatusFlags },
    /// Chat received (already stored in the history).
    ChatReceived { friend: String, entry: ChatEntry },
    FingerprintMismatch { friend: String },
}

/// Commands for the loop, which owns the `VoiceEngine`.
enum EngineCmd {
    Muted(bool),
    Deafened(bool),
    Ptt(bool),
}

struct State {
    history: HashMap<String, Vec<ChatEntry>>,
    voice: HashMap<String, VoiceCounters>,
    peers: HashMap<String, StatusFlags>,
    /// Mirror of the engine Activation (same operations, same order), so
    /// that `local_status()` is exact right after a `set_*` call
    /// (`inv-deafen-implies-mute` lives in `Activation`).
    local: Activation,
    game_mode: bool,
}

impl State {
    fn local_status(&self) -> StatusFlags {
        StatusFlags {
            muted: self.local.muted(),
            deafened: self.local.deafened(),
            game_mode: self.game_mode,
            sharing: false,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    next_chat_id: AtomicU64,
    events: mpsc::UnboundedSender<SessionEvent>,
    friends: Vec<String>,
}

impl Shared {
    fn emit(&self, ev: SessionEvent) {
        let _ = self.events.send(ev);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A running core: owns the `Net` endpoint, the voice engine and the chat history.
/// Several can live in one process (tests).
pub struct Session {
    net: Net,
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
    commands: mpsc::UnboundedSender<EngineCmd>,
    events: Mutex<Option<mpsc::UnboundedReceiver<SessionEvent>>>,
}

impl Session {
    /// Binds `bind_addr`, dials every friend of `config` and starts the loop.
    /// Must be called inside a tokio runtime.
    pub fn start(
        profile: Profile,
        config: Config,
        identity: &LocalIdentity,
        bind_addr: SocketAddr,
        source: AudioSource,
    ) -> Result<Session, String> {
        let (net, events) = Net::bind(bind_addr, identity, trust_store_from_config(&config))
            .map_err(|e| format!("cannot listen on {bind_addr}: {e}"))?;
        for friend in &config.friends {
            if let Err(e) = net.dial(friend) {
                tracing::warn!(friend = %friend.name, "cannot dial: {e}");
            }
        }
        let engine = VoiceEngine::new(&config.audio).map_err(|e| format!("opus: {e}"))?;
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                history: HashMap::new(),
                voice: HashMap::new(),
                peers: HashMap::new(),
                local: Activation::new(AudioMode::VoiceActivity, 0),
                game_mode: false,
            }),
            next_chat_id: AtomicU64::new(unix_ms().saturating_mul(1000)),
            events: events_tx,
            friends: config.friends.iter().map(|f| f.name.clone()).collect(),
        });
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(run_loop(
            profile,
            config,
            net.clone(),
            events,
            engine,
            source,
            shared.clone(),
            cmd_rx,
            stop_rx,
        ));
        Ok(Session {
            net,
            shared,
            stop,
            task: Mutex::new(Some(task)),
            commands: cmd_tx,
            events: Mutex::new(Some(events_rx)),
        })
    }

    /// The stream of [`SessionEvent`]s; the first call gets it, later calls `None`.
    pub fn take_events(&self) -> Option<mpsc::UnboundedReceiver<SessionEvent>> {
        lock(&self.events).take()
    }

    /// Local mute/deafen/game-mode flags (what the friends were told).
    pub fn local_status(&self) -> StatusFlags {
        lock(&self.shared.state).local_status()
    }

    /// Last `Status` announced by a friend; `None` while not connected.
    pub fn peer_status(&self, friend: &str) -> Option<StatusFlags> {
        lock(&self.shared.state).peers.get(friend).copied()
    }

    /// Applies `change` to the local state under the lock, forwards `cmd` to
    /// the loop (same order) and tells every connected friend.
    fn change_local(&self, cmd: Option<EngineCmd>, change: impl FnOnce(&mut State)) {
        let mut state = lock(&self.shared.state);
        let before = state.local_status();
        change(&mut state);
        if let Some(cmd) = cmd {
            let _ = self.commands.send(cmd);
        }
        let after = state.local_status();
        if after != before {
            let msg = after.to_message();
            for f in &self.shared.friends {
                if self.net.is_connected(f)
                    && let Err(e) = self.net.send_control(f, msg.clone())
                {
                    tracing::warn!(friend = %f, "cannot send status: {e}");
                }
            }
        }
    }

    /// While deafened the request is remembered and applies when deafen ends.
    pub fn set_muted(&self, muted: bool) {
        self.change_local(Some(EngineCmd::Muted(muted)), |s| s.local.set_muted(muted));
    }

    /// Deafening also mutes; undeafening restores the previous mute state.
    pub fn set_deafened(&self, deafened: bool) {
        self.change_local(Some(EngineCmd::Deafened(deafened)), |s| s.local.set_deafened(deafened));
    }

    /// Push-to-talk key state (only matters when `audio.mode = push_to_talk`).
    pub fn set_ptt(&self, pressed: bool) {
        let _ = self.commands.send(EngineCmd::Ptt(pressed));
    }

    /// Game mode flag, announced to the friends (`Status`).
    pub fn set_game_mode(&self, on: bool) {
        self.change_local(None, |s| s.game_mode = on);
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.net.local_addr()
    }

    pub fn is_connected(&self, friend: &str) -> bool {
        self.net.is_connected(friend)
    }

    /// Configured friends that are connected now, in config order.
    pub fn connected_friends(&self) -> Vec<String> {
        self.shared.friends.iter().filter(|f| self.net.is_connected(f)).cloned().collect()
    }

    /// Sends a chat message and appends it to the history (`from_self = true`).
    pub fn send_chat(&self, friend: &str, text: &str) -> Result<ChatEntry, NetError> {
        let mut state = lock(&self.shared.state);
        let id = self.shared.next_chat_id.fetch_add(1, Ordering::Relaxed);
        let sent_at = unix_ms();
        self.net.send_control(
            friend,
            ControlMessage::Chat {
                id,
                text: text.to_string(),
                sent_at,
            },
        )?;
        let entry = ChatEntry {
            id,
            from_self: true,
            text: text.to_string(),
            sent_at,
        };
        state
            .history
            .entry(friend.to_string())
            .or_default()
            .push(entry.clone());
        tracing::info!(%friend, id, len = text.len(), "chat sent");
        Ok(entry)
    }

    /// Chat history of the current call with `friend` (empty when not in a call).
    pub fn chat_history(&self, friend: &str) -> Vec<ChatEntry> {
        lock(&self.shared.state)
            .history
            .get(friend)
            .cloned()
            .unwrap_or_default()
    }

    /// Voice counters of `friend` (zeroes if never connected).
    pub fn voice_counters(&self, friend: &str) -> VoiceCounters {
        lock(&self.shared.state)
            .voice
            .get(friend)
            .copied()
            .unwrap_or_default()
    }

    /// Stops the loop, closes the connections and drops every chat history.
    /// Calling it again is harmless.
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        let task = lock(&self.task).take();
        if let Some(t) = task {
            let _ = t.await;
        }
        lock(&self.shared.state).history.clear();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

fn spawn_synthetic_source(tx: mpsc::Sender<Vec<f32>>) {
    tokio::spawn(async move {
        let mut src = SineSource::standard();
        let mut tick = interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if tx.try_send(src.next_frame()).is_err() && tx.is_closed() {
                break;
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    profile: Profile,
    mut config: Config,
    net: Net,
    mut events: mpsc::UnboundedReceiver<NetEvent>,
    mut engine: VoiceEngine,
    source: AudioSource,
    shared: Arc<Shared>,
    mut commands: mpsc::UnboundedReceiver<EngineCmd>,
    mut stop: watch::Receiver<bool>,
) {
    // Voice: microphone (device or synthetic) -> engine -> Net; Net -> engine -> sink.
    let (cap_tx, mut cap_rx) = mpsc::channel::<Vec<f32>>(16);
    let _input = match source {
        AudioSource::Synthetic => {
            spawn_synthetic_source(cap_tx.clone());
            None
        }
        AudioSource::Device => match AudioInput::start(&config.audio.input_device, cap_tx.clone()) {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::warn!("no audio input ({e}); not transmitting voice");
                None
            }
        },
    };
    let sink = match source {
        AudioSource::Synthetic => Sink::Null,
        AudioSource::Device => Sink::open(&config.audio.output_device),
    };
    tracing::info!(synthetic = source == AudioSource::Synthetic, null_sink = sink.is_null(), "voice ready");
    let started = Instant::now();
    let mut playout = interval(Duration::from_millis(20));
    playout.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut stats_tick = interval(Duration::from_secs(2));
    let mut send_errors: u64 = 0;

    loop {
        let event = tokio::select! {
            e = events.recv() => match e { Some(e) => e, None => break },
            Some(mut pcm) = cap_rx.recv() => {
                match engine.process_capture(&mut pcm) {
                    Ok(Some(f)) => {
                        let peers: Vec<String> = engine.peer_names().map(str::to_string).collect();
                        for peer in peers {
                            let Some(max) = net.max_datagram_size(&peer) else { continue };
                            if f.payload.len() > max_audio_payload(max) {
                                send_errors += 1;
                                continue;
                            }
                            let dg = MediaDatagram::Audio {
                                stream: AudioStream::Voice,
                                seq: f.seq,
                                timestamp: f.timestamp,
                                payload: f.payload.clone(),
                            };
                            if net.send_datagram(&peer, &dg).is_err() {
                                send_errors += 1;
                            } else {
                                lock(&shared.state).voice.entry(peer).or_default().sent += 1;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("opus encode failed: {e}"),
                }
                continue;
            }
            _ = playout.tick() => {
                let now_ms = started.elapsed().as_millis() as u64;
                sink.play(engine.playout_tick(now_ms));
                publish_voice_counters(&engine, &shared);
                continue;
            }
            _ = stats_tick.tick() => {
                let s = engine.stats();
                let mic = engine.take_mic_levels();
                tracing::info!(
                    sent = s.sent,
                    skipped = s.skipped,
                    send_errors,
                    mic_avg_db = format_args!("{:.1}", mic.avg_db),
                    mic_peak_db = format_args!("{:.1}", mic.peak_db),
                    "voice tx"
                );
                let names: Vec<String> = engine.peer_names().map(str::to_string).collect();
                for n in names {
                    if let Some(p) = engine.peer_stats(&n) {
                        tracing::info!(
                            friend = %n,
                            received = p.jitter.received,
                            played = p.jitter.played,
                            lost = p.jitter.lost,
                            late = p.jitter.late_dropped,
                            plc = p.plc_frames,
                            fec = p.fec_frames,
                            "voice rx"
                        );
                    }
                }
                continue;
            }
            Some(cmd) = commands.recv() => {
                let act = engine.activation_mut();
                match cmd {
                    EngineCmd::Muted(m) => act.set_muted(m),
                    EngineCmd::Deafened(d) => act.set_deafened(d),
                    EngineCmd::Ptt(p) => act.set_ptt(p),
                }
                continue;
            }
            _ = stop.changed() => break,
        };
        match event {
            NetEvent::Connected { friend, fingerprint, newly_pinned } => {
                tracing::info!(%friend, fingerprint = %fingerprint.to_hex(), newly_pinned, "event: connected");
                // A repeated Connected (replacement connection) keeps history and voice.
                let (local, first) = {
                    let mut st = lock(&shared.state);
                    let first = !st.history.contains_key(&friend);
                    if first {
                        st.history.insert(friend.clone(), Vec::new());
                    }
                    (st.local_status(), first)
                };
                if let Err(e) = engine.add_peer(&friend) {
                    tracing::warn!(%friend, "cannot start voice: {e}");
                }
                let hello = ControlMessage::Hello {
                    name: config.name.clone(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: Capabilities::default(),
                };
                let status = local.to_message();
                for msg in [hello, status] {
                    if let Err(e) = net.send_control(&friend, msg) {
                        tracing::warn!(%friend, "cannot send: {e}");
                    }
                }
                if newly_pinned
                    && let Some(f) = config.friends.iter_mut().find(|f| f.name == friend)
                {
                    f.fingerprint = Some(fingerprint.to_hex());
                    match config.save(&profile) {
                        Ok(()) => tracing::info!(%friend, "fingerprint pinned (TOFU) and saved"),
                        Err(e) => tracing::error!(%friend, "cannot save pinned fingerprint: {e}"),
                    }
                }
                if first {
                    shared.emit(SessionEvent::Connected { friend });
                }
            }
            NetEvent::Control { friend, msg } => match msg {
                ControlMessage::Hello { name, app_version, protocol_version, .. } => {
                    tracing::info!(%friend, %name, %app_version, protocol_version, "event: Hello");
                }
                ControlMessage::Status { muted, deafened, game_mode, sharing } => {
                    tracing::info!(%friend, muted, deafened, game_mode, sharing, "event: Status");
                    let status = StatusFlags { muted, deafened, game_mode, sharing };
                    lock(&shared.state).peers.insert(friend.clone(), status);
                    shared.emit(SessionEvent::PeerStatus { friend, status });
                }
                // Never log chat text: id and size only.
                ControlMessage::Chat { id, text, sent_at } => {
                    tracing::info!(%friend, id, len = text.len(), "event: Chat");
                    let entry = ChatEntry { id, from_self: false, text, sent_at };
                    lock(&shared.state)
                        .history
                        .entry(friend.clone())
                        .or_default()
                        .push(entry.clone());
                    shared.emit(SessionEvent::ChatReceived { friend, entry });
                }
                ControlMessage::Bye => tracing::info!(%friend, "event: Bye"),
                other => tracing::info!(%friend, "event: control {other:?}"),
            },
            NetEvent::Datagram { friend, dg } => match dg {
                MediaDatagram::Audio { stream: AudioStream::Voice, seq, timestamp, payload } => {
                    let now_ms = started.elapsed().as_millis() as u64;
                    engine.push_datagram(&friend, seq, timestamp, payload, now_ms);
                }
                _ => tracing::trace!(%friend, "event: datagram"),
            },
            NetEvent::Disconnected { friend } => {
                publish_voice_counters(&engine, &shared);
                engine.remove_peer(&friend);
                // End of call: the chat history is discarded (inv-chat-never-on-disk).
                {
                    let mut st = lock(&shared.state);
                    st.history.remove(&friend);
                    st.peers.remove(&friend);
                }
                tracing::info!(%friend, "event: disconnected");
                shared.emit(SessionEvent::Disconnected { friend });
            }
            NetEvent::Reconnecting { friend, attempt, delay } => {
                tracing::info!(%friend, attempt, ?delay, "event: reconnecting");
                shared.emit(SessionEvent::Reconnecting { friend, attempt });
            }
            NetEvent::Rejected { addr, reason } => {
                tracing::warn!(%addr, %reason, "event: rejected");
            }
            NetEvent::FingerprintMismatch { friend, expected, presented } => {
                tracing::warn!(
                    %friend,
                    expected = %expected.to_hex(),
                    presented = %presented.to_hex(),
                    "event: FINGERPRINT MISMATCH, connection refused"
                );
                shared.emit(SessionEvent::FingerprintMismatch { friend });
            }
        }
    }
    net.shutdown().await;
    lock(&shared.state).history.clear();
}

/// Copies the receive-side counters of every active peer into the shared state
/// (`sent` is maintained where datagrams are sent).
fn publish_voice_counters(engine: &VoiceEngine, shared: &Shared) {
    let names: Vec<String> = engine.peer_names().map(str::to_string).collect();
    if names.is_empty() {
        return;
    }
    let mut state = lock(&shared.state);
    for n in names {
        if let Some(p) = engine.peer_stats(&n) {
            let c = state.voice.entry(n).or_default();
            c.received = p.jitter.received;
            c.played = p.jitter.played;
            c.lost = p.jitter.lost;
            c.late_dropped = p.jitter.late_dropped;
        }
    }
}
