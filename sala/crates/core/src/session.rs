//! Core event loop as a library object (`core-process`): net -> voice, Hello/Status,
//! TOFU pin persistence and the in-memory chat (`chat-history`).
//!
//! Chat is RAM only (`inv-chat-never-on-disk`): the history of a friend lives in
//! `State::history`, is dropped on `Disconnected` and on shutdown, and the text is
//! never logged (only id and byte length).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sala_protocol::{
    AudioStream, ChatEntry, ControlMessage, DEFAULT_MAX_DATAGRAM, MediaDatagram, PROTOCOL_VERSION,
    RING_PROTOCOL_VERSION, VideoCodec, max_audio_payload,
};
use sala_video::capture::Source;
use sala_video::preset::SharePreset;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};

use crate::audio_device::{AudioInput, AudioOutput, Sink};
use crate::calls::{CallBook, CallEnd, RING_TIMEOUT};
use crate::config::{AudioConfig, AudioMode, Config, Friend, Profile};
use crate::cues::Cue;
use crate::identity::LocalIdentity;
use crate::net::{Net, NetError, NetEvent, trust_store_from_config};
use crate::share::{
    Pipe, PipeFactory, ShareIn, ShareOut, ShareParams, ShareShared, ShareStats, VideoFrame, abr_report,
    handle_output, peer_supports_video, screen_pipe,
};
use crate::share_audio::{AudioCapture, AudioFactory};
use crate::voice::{Activation, SineSource, VoiceEngine};

/// Where captured audio comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    /// Sine wave generator and a null sink: no audio device is opened at all (tests).
    Synthetic,
    /// Sine wave instead of the microphone, but the configured output device is opened
    /// (`--synthetic-audio`).
    SyntheticMic,
    /// Configured input/output devices.
    Device,
}

impl AudioSource {
    /// The microphone is replaced by a sine wave.
    fn synthetic_mic(self) -> bool {
        matches!(self, AudioSource::Synthetic | AudioSource::SyntheticMic)
    }

    /// The device sides this source opens (the rest stays synthetic / null).
    fn device_parts(self) -> AudioParts {
        match self {
            AudioSource::Synthetic => AudioParts::default(),
            AudioSource::SyntheticMic => AudioParts { input: false, output: true },
            AudioSource::Device => AudioParts::BOTH,
        }
    }
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

/// The `Status` flags (local or of a friend).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusFlags {
    pub muted: bool,
    pub deafened: bool,
    pub game_mode: bool,
    pub sharing: bool,
    /// In the room (D-015); everybody starts outside it (D-024).
    pub in_room: bool,
}

impl StatusFlags {
    fn to_message(self) -> ControlMessage {
        ControlMessage::Status {
            muted: self.muted,
            deafened: self.deafened,
            game_mode: self.game_mode,
            sharing: self.sharing,
            in_room: self.in_room,
        }
    }
}

/// What [`Session::call`] did besides entering the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStart {
    /// The friend's PC is ringing (or already was).
    Ringing,
    /// The friend is already in the room: joining was enough.
    AlreadyInRoom,
    /// The friend is not connected: nothing to ring.
    Offline,
    /// The friend's Concord predates rings (`CallResult unsupported` was emitted).
    Unsupported,
}

/// The sound of one local status change (the room wins over deafen, deafen over mute:
/// deafening also mutes, and one click must make one sound).
fn local_change_cue(before: StatusFlags, after: StatusFlags) -> Option<Cue> {
    if before.in_room != after.in_room {
        Some(if after.in_room { Cue::SelfJoin } else { Cue::SelfLeave })
    } else if before.deafened != after.deafened {
        Some(if after.deafened { Cue::Deafen } else { Cue::Undeafen })
    } else if before.muted != after.muted {
        Some(if after.muted { Cue::Mute } else { Cue::Unmute })
    } else {
        None
    }
}

/// The sound of a friend entering/leaving the room, heard only while we are in it.
/// `before`: what we knew (`None` = no `Status` yet, i.e. outside).
fn peer_room_cue(local_in_room: bool, before: Option<bool>, now: bool) -> Option<Cue> {
    if !local_in_room {
        return None;
    }
    match (before.unwrap_or(false), now) {
        (false, true) => Some(Cue::PeerJoin),
        (true, false) => Some(Cue::PeerLeave),
        _ => None,
    }
}

/// What the layer above (UI/tray) must hear about, in order, per session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Connected { friend: String },
    Disconnected { friend: String },
    Reconnecting { friend: String, attempt: u32 },
    /// A friend announced a new `Status`.
    PeerStatus { friend: String, status: StatusFlags },
    /// Chat received (already stored in the history).
    ChatReceived { friend: String, entry: ChatEntry },
    FingerprintMismatch { friend: String },
    /// The friend's address is not a peer of this Tailscale (another account, not shared).
    NotInTailnet { friend: String },
    /// The session itself changed the config (a fingerprint was pinned, TOFU).
    ConfigChanged,
    /// Every 2 s for each connected friend.
    CallStats { friend: String, rtt_ms: u32, since_secs: u64 },
    /// Microphone level while the UI meter is on (~every 50 ms).
    MicLevel { db: f32 },
    /// The rings changed (D-024): snapshot of who rings us and whom we ring.
    Calls { incoming: Vec<String>, outgoing: Vec<String> },
    /// A ring ended without the friend joining.
    CallResult { friend: String, end: CallEnd },
}

/// Commands for the loop, which owns the `VoiceEngine`.
enum EngineCmd {
    /// One-shot notification sound.
    Cue(Cue),
    /// The looping ring sound (`None` = silence).
    CueLoop(Option<Cue>),
    Muted(bool),
    Deafened(bool),
    Ptt(bool),
    InRoom(bool),
    /// New `[audio]` section (live settings; devices are reopened when they changed).
    Audio(Box<AudioConfig>),
    MicMeter(bool),
    /// "Ouvir minha voz": the encoded-and-decoded microphone also plays locally.
    MicTest(bool),
    PeerVolume(String, f32),
    /// A screen share whose pipe is already running (the factory ran on the caller's thread).
    ShareStart(Box<Pipe>, Option<AudioCapture>),
    ShareStop,
    /// Volume of the sound of the shares we watch (0.0..=1.0).
    ShareAudioVolume(f32),
}

/// Receive counters of one friend's share audio (`Session::share_audio_counters`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ShareAudioCounters {
    /// `ShareAudio` datagrams received.
    pub received: u64,
    /// Frames the jitter buffer delivered to the mix.
    pub played: u64,
    /// Peak of the decoded sound in dBFS (-120 for none).
    pub peak_db: f32,
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
    in_room: bool,
    /// Rings in progress (D-024).
    calls: CallBook,
    /// `protocol_version` of each connected friend's `Hello` (rings need `RING_PROTOCOL_VERSION`).
    proto: HashMap<String, u16>,
    /// We are sharing the screen (announced in `Status`).
    sharing: bool,
}

impl State {
    fn local_status(&self) -> StatusFlags {
        StatusFlags {
            muted: self.local.muted(),
            deafened: self.local.deafened(),
            game_mode: self.game_mode,
            sharing: self.sharing,
            in_room: self.in_room,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    next_chat_id: AtomicU64,
    events: mpsc::UnboundedSender<SessionEvent>,
    /// The live configuration (friends, pins, settings). Lock order: `state` before `config`.
    config: Mutex<Config>,
    profile: Profile,
    /// Commands for the loop (also used to play cues from any thread).
    commands: mpsc::UnboundedSender<EngineCmd>,
    next_call_id: AtomicU64,
    /// How long we ring a friend (`RING_TIMEOUT`; shortened by tests).
    ring_timeout: Mutex<Duration>,
    /// The running screen share (set by the loop; read by `Session::share_stats`).
    share: Mutex<Option<Arc<ShareShared>>>,
    /// A share is being started or running (one at a time).
    share_claim: AtomicBool,
    /// Frames of friends' shares, for the viewer. Bounded: live video never queues.
    video: mpsc::Sender<VideoFrame>,
    /// Counters of the sound of friends' shares (published by the loop).
    share_audio: Mutex<HashMap<String, ShareAudioCounters>>,
}

impl Shared {
    fn emit(&self, ev: SessionEvent) {
        let _ = self.events.send(ev);
    }

    fn cue(&self, cue: Cue) {
        let _ = self.commands.send(EngineCmd::Cue(cue));
    }

    /// The rings in `st` changed: set the ring loop and tell the layer above.
    fn calls_changed(&self, st: &State) {
        let _ = self.commands.send(EngineCmd::CueLoop(st.calls.loop_cue()));
        self.emit(SessionEvent::Calls { incoming: st.calls.incoming(), outgoing: st.calls.outgoing() });
    }

    /// Configured friends, in config order.
    fn friend_names(&self) -> Vec<String> {
        lock(&self.config).friends.iter().map(|f| f.name.clone()).collect()
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
    video: Mutex<Option<mpsc::Receiver<VideoFrame>>>,
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
        let (video_tx, video_rx) = mpsc::channel(VIDEO_QUEUE);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                history: HashMap::new(),
                voice: HashMap::new(),
                peers: HashMap::new(),
                local: Activation::new(AudioMode::VoiceActivity, 0),
                game_mode: false,
                in_room: false,
                calls: CallBook::default(),
                proto: HashMap::new(),
                sharing: false,
            }),
            next_chat_id: AtomicU64::new(unix_ms().saturating_mul(1000)),
            events: events_tx,
            config: Mutex::new(config),
            profile,
            commands: cmd_tx.clone(),
            next_call_id: AtomicU64::new(unix_ms().saturating_mul(1000)),
            ring_timeout: Mutex::new(RING_TIMEOUT),
            share: Mutex::new(None),
            share_claim: AtomicBool::new(false),
            video: video_tx,
            share_audio: Mutex::new(HashMap::new()),
        });
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(run_loop(
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
            video: Mutex::new(Some(video_rx)),
        })
    }

    /// The frames reassembled from friends' screen shares; the first call gets the stream.
    pub fn take_video_frames(&self) -> Option<mpsc::Receiver<VideoFrame>> {
        lock(&self.video).take()
    }

    /// Asks `friend`'s share for a keyframe (the viewer's decoder needs one to start or recover).
    pub fn request_keyframe(&self, friend: &str) {
        if let Err(e) = self.net.send_control(friend, ControlMessage::KeyframeRequest) {
            tracing::debug!(%friend, "cannot ask for a keyframe: {e}");
        }
    }

    /// Starts sharing the screen with every connected friend that understands it
    /// (`VIDEO_PROTOCOL_VERSION`); friends that connect later join automatically. `factory`
    /// opens the capture and encoder on the caller's thread (it can take a while).
    pub fn start_share(&self, preset: SharePreset, factory: PipeFactory) -> Result<(), String> {
        self.start_share_with_audio(preset, factory, None).map(|_| ())
    }

    /// Like [`Session::start_share`], also sending the PC's sound when `audio` is given.
    /// `Ok(true)` when the audio capture opened; if it fails the share goes on without sound
    /// (the reason is logged) and the result is `Ok(false)`.
    pub fn start_share_with_audio(
        &self,
        preset: SharePreset,
        factory: PipeFactory,
        audio: Option<AudioFactory>,
    ) -> Result<bool, String> {
        // Claim it now so that a second call cannot start another encoder meanwhile.
        if self.shared.share_claim.swap(true, Ordering::AcqRel) {
            return Err("already sharing".into());
        }
        let max_datagram = self
            .connected_friends()
            .iter()
            .filter(|f| lock(&self.shared.state).proto.get(*f).is_some_and(|v| peer_supports_video(*v)))
            .filter_map(|f| self.net.max_datagram_size(f))
            .min()
            .unwrap_or(DEFAULT_MAX_DATAGRAM);
        match factory(&ShareParams { preset, max_datagram }) {
            Ok(pipe) => {
                let capture = audio.and_then(|open| match open() {
                    Ok(c) => Some(c),
                    Err(e) => {
                        tracing::warn!("sharing without audio: {e}");
                        None
                    }
                });
                let with_audio = capture.is_some();
                // Announce `sharing = true` to everybody, then hand the pipe to the loop.
                self.change_local(Some(EngineCmd::ShareStart(Box::new(pipe), capture)), |s| s.sharing = true);
                Ok(with_audio)
            }
            Err(e) => {
                self.shared.share_claim.store(false, Ordering::Release);
                Err(e)
            }
        }
    }

    /// Shares `source` (a monitor or window from `sala_video::capture`) with the given codec.
    pub fn start_screen_share(&self, preset: SharePreset, codec: VideoCodec, source: Source) -> Result<(), String> {
        self.start_share(preset, Box::new(move |params| screen_pipe(params, codec, &source)))
    }

    /// Stops the screen share (harmless when not sharing).
    pub fn stop_share(&self) {
        self.shared.share_claim.store(false, Ordering::Release);
        self.change_local(Some(EngineCmd::ShareStop), |s| s.sharing = false);
    }

    /// Volume (0.0..=1.0) of the sound of the shares we watch, apart from the voices.
    pub fn set_share_audio_volume(&self, volume: f32) {
        let _ = self.shared.commands.send(EngineCmd::ShareAudioVolume(volume));
    }

    /// What arrived of `friend`'s share audio (zeroes if none did).
    pub fn share_audio_counters(&self, friend: &str) -> ShareAudioCounters {
        lock(&self.shared.share_audio).get(friend).copied().unwrap_or_default()
    }

    /// Counters of the running share; `None` when not sharing.
    pub fn share_stats(&self) -> Option<ShareStats> {
        lock(&self.shared.share).as_ref().map(|s| s.stats())
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
            if let Some(cue) = local_change_cue(before, after) {
                self.shared.cue(cue);
            }
            let msg = after.to_message();
            for f in &self.shared.friend_names() {
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

    /// Enters/leaves the room (D-015), announced to the friends (`Status`). Entering answers
    /// every incoming ring; leaving cancels the rings we were making (D-024).
    pub fn set_in_room(&self, in_room: bool) {
        self.change_local(Some(EngineCmd::InRoom(in_room)), |s| s.in_room = in_room);
        let mut st = lock(&self.shared.state);
        let mut changed = false;
        if in_room {
            changed = st.calls.clear_in();
        } else {
            for (friend, call_id) in st.calls.take_all_out() {
                changed = true;
                if let Err(e) = self.net.send_control(&friend, ControlMessage::RingCancel { call_id }) {
                    tracing::debug!(%friend, "cannot cancel the ring: {e}");
                }
            }
        }
        if changed {
            self.shared.calls_changed(&st);
        }
    }

    /// Enters the room and rings `friend` (D-024). Nobody is rung when the friend is already
    /// in the room, is offline, or runs a Concord too old to understand rings.
    pub fn call(&self, friend: &str) -> CallStart {
        self.set_in_room(true);
        let call_id = {
            let mut st = lock(&self.shared.state);
            if st.peers.get(friend).is_some_and(|p| p.in_room) {
                return CallStart::AlreadyInRoom;
            }
            if !self.net.is_connected(friend) {
                return CallStart::Offline;
            }
            if st.proto.get(friend).copied().unwrap_or(0) < RING_PROTOCOL_VERSION {
                self.shared.emit(SessionEvent::CallResult { friend: friend.to_string(), end: CallEnd::Unsupported });
                return CallStart::Unsupported;
            }
            if st.calls.outgoing_id(friend).is_some() {
                return CallStart::Ringing;
            }
            let call_id = self.shared.next_call_id.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = self.net.send_control(friend, ControlMessage::Ring { call_id }) {
                tracing::warn!(%friend, "cannot ring: {e}");
                return CallStart::Offline;
            }
            st.calls.ring_out(friend, call_id);
            self.shared.calls_changed(&st);
            call_id
        };
        tracing::info!(%friend, call_id, "ringing");
        let (shared, net, friend) = (self.shared.clone(), self.net.clone(), friend.to_string());
        let timeout = *lock(&self.shared.ring_timeout);
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let mut st = lock(&shared.state);
            if st.calls.end_out(&friend, call_id) {
                tracing::info!(%friend, call_id, "ring timed out");
                let _ = net.send_control(&friend, ControlMessage::RingCancel { call_id });
                shared.cue(Cue::CallFailed);
                shared.calls_changed(&st);
                shared.emit(SessionEvent::CallResult { friend, end: CallEnd::Timeout });
            }
        });
        CallStart::Ringing
    }

    /// Answers a ring by entering the room (D-024).
    pub fn accept_call(&self, friend: &str) {
        tracing::info!(%friend, "call accepted");
        self.set_in_room(true);
    }

    /// Refuses the ring of `friend`; the caller is told (D-024).
    pub fn decline_call(&self, friend: &str) {
        let mut st = lock(&self.shared.state);
        if let Some(call_id) = st.calls.answer_in(friend) {
            tracing::info!(%friend, call_id, "call declined");
            if let Err(e) = self.net.send_control(friend, ControlMessage::RingDecline { call_id }) {
                tracing::debug!(%friend, "cannot send the decline: {e}");
            }
            self.shared.calls_changed(&st);
        }
    }

    /// `(incoming, outgoing)` rings now.
    pub fn calls(&self) -> (Vec<String>, Vec<String>) {
        let st = lock(&self.shared.state);
        (st.calls.incoming(), st.calls.outgoing())
    }

    /// Plays a notification sound (D-024).
    pub fn play_cue(&self, cue: Cue) {
        self.shared.cue(cue);
    }

    /// Shortens (tests) or restores the time a ring lasts; applies to the next ring.
    pub fn set_ring_timeout(&self, timeout: Duration) {
        *lock(&self.shared.ring_timeout) = timeout;
    }

    /// Snapshot of the live configuration.
    pub fn config(&self) -> Config {
        lock(&self.shared.config).clone()
    }

    /// Applies the settings of `new` (name, audio, hotkeys, game mode) live and saves the
    /// config. Friends are not touched (see `add_friend` and friends). The audio section
    /// is applied by the loop: threshold, mode, gain and bitrate at once, devices reopened.
    /// Nothing changes when the file cannot be written. The name goes out in the `Hello`
    /// of the next connection.
    pub fn apply_config(&self, new: &Config) -> Result<(), String> {
        let mut cfg = lock(&self.shared.config);
        let mut candidate = cfg.clone();
        candidate.name = new.name.clone();
        candidate.audio = new.audio.clone();
        candidate.hotkeys = new.hotkeys.clone();
        candidate.game_mode = new.game_mode.clone();
        candidate.save(&self.shared.profile).map_err(|e| format!("cannot save the config: {e}"))?;
        let audio_changed = candidate.audio != cfg.audio;
        *cfg = candidate;
        if audio_changed {
            let _ = self.commands.send(EngineCmd::Audio(Box::new(cfg.audio.clone())));
        }
        Ok(())
    }

    /// Adds a friend, saves and dials it. `address` is an IP (default port) or `ip:port`.
    pub fn add_friend(&self, name: &str, address: &str) -> Result<(), String> {
        let name = crate::settings::validate_name(name)?;
        let address = address.trim();
        let (ip, port) = match address.parse::<std::net::IpAddr>() {
            Ok(ip) => (ip, None),
            Err(_) => match address.parse::<SocketAddr>() {
                Ok(sa) => (sa.ip(), Some(sa.port())),
                Err(_) => return Err(format!("{address:?} is not an IP address such as 100.64.0.1")),
            },
        };
        let friend = Friend { name, address: ip.to_string(), port, fingerprint: None };
        let mut cfg = lock(&self.shared.config);
        if cfg.friends.iter().any(|f| f.name == friend.name) {
            return Err(format!("friend {:?} already exists", friend.name));
        }
        let mut candidate = cfg.clone();
        candidate.friends.push(friend.clone());
        candidate.save(&self.shared.profile).map_err(|e| format!("cannot save the config: {e}"))?;
        *cfg = candidate;
        self.net.add_friend(&friend).map_err(|e| e.to_string())
    }

    /// Removes a friend, saves and closes the connection (a `Disconnected` follows if it was up).
    pub fn remove_friend(&self, name: &str) -> Result<(), String> {
        let mut cfg = lock(&self.shared.config);
        if !cfg.friends.iter().any(|f| f.name == name) {
            return Err(format!("unknown friend {name:?}"));
        }
        let mut candidate = cfg.clone();
        candidate.friends.retain(|f| f.name != name);
        candidate.save(&self.shared.profile).map_err(|e| format!("cannot save the config: {e}"))?;
        *cfg = candidate;
        drop(cfg);
        self.net.remove_friend(name);
        let mut st = lock(&self.shared.state);
        st.history.remove(name);
        st.peers.remove(name);
        Ok(())
    }

    /// Forgets the pinned fingerprint of `name` (trust store and `config.toml`).
    pub fn reset_fingerprint(&self, name: &str) -> Result<(), String> {
        let mut cfg = lock(&self.shared.config);
        if !cfg.friends.iter().any(|f| f.name == name) {
            return Err(format!("unknown friend {name:?}"));
        }
        let mut candidate = cfg.clone();
        for f in candidate.friends.iter_mut().filter(|f| f.name == name) {
            f.fingerprint = None;
        }
        candidate.save(&self.shared.profile).map_err(|e| format!("cannot save the config: {e}"))?;
        *cfg = candidate;
        self.net.reset_fingerprint(name);
        Ok(())
    }

    /// Starts/stops the `MicLevel` events (~every 50 ms while on).
    pub fn set_mic_meter(&self, on: bool) {
        let _ = self.commands.send(EngineCmd::MicMeter(on));
    }

    /// Plays our own voice locally, after the gain, the voice detection and Opus (what the
    /// friends would hear). Nothing extra is sent.
    pub fn set_mic_test(&self, on: bool) {
        let _ = self.commands.send(EngineCmd::MicTest(on));
    }

    /// Playback volume of a friend, 0.0..=2.0 (not persisted; applies to the running call).
    pub fn set_peer_volume(&self, friend: &str, volume: f32) {
        let _ = self.commands.send(EngineCmd::PeerVolume(friend.to_string(), volume));
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.net.local_addr()
    }

    pub fn is_connected(&self, friend: &str) -> bool {
        self.net.is_connected(friend)
    }

    /// Configured friends that are connected now, in config order.
    pub fn connected_friends(&self) -> Vec<String> {
        self.shared.friend_names().into_iter().filter(|f| self.net.is_connected(f)).collect()
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

/// Emits `NotInTailnet` when `friend`'s Tailscale address is not among our peers (D-022).
fn check_tailnet_peer(shared: &Arc<Shared>, friend: &str) {
    let address = lock(&shared.config).friends.iter().find(|f| f.name == friend).map(|f| f.address.clone());
    let Some(ip) = address.and_then(|a| a.parse::<std::net::IpAddr>().ok()) else { return };
    if !crate::tailscale::in_tailnet_range(ip) {
        return;
    }
    let (shared, friend) = (shared.clone(), friend.to_string());
    tokio::spawn(async move {
        if crate::tailscale::is_peer(ip).await == Some(false) {
            tracing::warn!(%friend, %ip, reason = "not_in_tailnet", "friend address is not in this Tailscale network");
            shared.emit(SessionEvent::NotInTailnet { friend });
        }
    });
}

/// Which audio sides to (re)open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AudioParts {
    input: bool,
    output: bool,
}

impl AudioParts {
    const BOTH: Self = Self { input: true, output: true };

    fn any(self) -> bool {
        self.input || self.output
    }

    fn union(self, other: Self) -> Self {
        Self { input: self.input || other.input, output: self.output || other.output }
    }
}

/// Result of one background open; `None` = that side was not requested.
type OpenedAudio = (Option<Option<AudioInput>>, Option<Sink>);

/// How long the session waits before trying a missing device again.
const AUDIO_RETRY: Duration = Duration::from_secs(5);

/// Reassembled frames the viewer channel holds before the oldest news is refused.
const VIDEO_QUEUE: usize = 32;

/// Opens the requested sides of `audio` on a blocking thread: WASAPI can take long or hang,
/// and the session loop must keep running meanwhile (T-029). `quiet` logs failures at debug
/// (periodic retries of a device that is still missing).
fn spawn_open_audio(
    audio: AudioConfig,
    parts: AudioParts,
    cap_tx: mpsc::Sender<Vec<f32>>,
    quiet: bool,
) -> oneshot::Receiver<OpenedAudio> {
    let (tx, rx) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let input = parts.input.then(|| match AudioInput::start(&audio.input_device, cap_tx) {
            Ok(i) => Some(i),
            Err(e) if quiet => {
                tracing::debug!("audio input still unavailable ({e})");
                None
            }
            Err(e) => {
                tracing::warn!("no audio input ({e}); not transmitting voice, will retry");
                None
            }
        });
        let output = parts.output.then(|| match AudioOutput::start(&audio.output_device) {
            Ok(o) => Sink::Device(o),
            Err(e) if quiet => {
                tracing::debug!("audio output still unavailable ({e})");
                Sink::Null
            }
            Err(e) => {
                tracing::warn!("no audio output ({e}); voice will not be played, will retry");
                Sink::Null
            }
        });
        // If the session is gone the streams are dropped here, off the runtime.
        let _ = tx.send((input, output));
    });
    rx
}

/// Drops audio streams off the runtime threads (their `Drop` may wait briefly).
fn release_audio<T: Send + 'static>(x: T) {
    tokio::task::spawn_blocking(move || drop(x));
}

async fn run_loop(
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
    let mut cur_audio = lock(&shared.config).audio.clone();
    if source.synthetic_mic() {
        spawn_synthetic_source(cap_tx.clone());
    }
    // Only the sides this source opens as real devices are (re)opened, ever.
    let device_parts = source.device_parts();
    // Devices open in the background; until then the session runs without them.
    let mut input: Option<AudioInput> = None;
    let mut sink = Sink::Null;
    let mut audio_job = device_parts
        .any()
        .then(|| spawn_open_audio(cur_audio.clone(), device_parts, cap_tx.clone(), false));
    // Sides whose device changed while a job was running: reopened when it finishes.
    let mut audio_pending = AudioParts::default();
    let mut audio_retry: Option<tokio::time::Instant> = None;
    tracing::info!(synthetic_mic = source.synthetic_mic(), real_output = device_parts.output, "voice ready");
    let started = Instant::now();
    let mut playout = interval(Duration::from_millis(20));
    playout.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut stats_tick = interval(Duration::from_secs(2));
    let mut meter_tick = interval(Duration::from_millis(50));
    meter_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut meter_on = false;
    let mut send_errors: u64 = 0;
    // Screen share: ours (sender side) and the friends' (one receiver each).
    let mut share_out: Option<ShareOut> = None;
    let mut shares_in: HashMap<String, ShareIn> = HashMap::new();
    let mut share_tick = interval(Duration::from_millis(250));
    share_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        let event = tokio::select! {
            e = events.recv() => match e { Some(e) => e, None => break },
            Some(mut pcm) = cap_rx.recv() => {
                match engine.process_capture(&mut pcm) {
                    Ok(Some(f)) => {
                        // Both sides in the room (D-015).
                        for peer in engine.send_targets() {
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
                publish_share_audio(&engine, &shared);
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
                for friend in shared.friend_names() {
                    if let (Some(rtt), Some(up)) = (net.rtt(&friend), net.connected_for(&friend)) {
                        shared.emit(SessionEvent::CallStats {
                            friend,
                            rtt_ms: rtt.as_millis().min(u128::from(u32::MAX)) as u32,
                            since_secs: up.as_secs(),
                        });
                    }
                }
                continue;
            }
            opened = async { audio_job.as_mut().expect("guarded").await }, if audio_job.is_some() => {
                audio_job = None;
                let Ok((new_input, new_output)) = opened else { continue };
                if let Some(i) = new_input {
                    release_audio(std::mem::replace(&mut input, i));
                }
                if let Some(s) = new_output {
                    release_audio(std::mem::replace(&mut sink, s));
                }
                tracing::info!(input = input.is_some(), output = !sink.is_null(), "audio devices opened");
                let pending = std::mem::take(&mut audio_pending);
                if pending.input {
                    release_audio(input.take());
                }
                if pending.output {
                    release_audio(std::mem::replace(&mut sink, Sink::Null));
                }
                let missing = AudioParts {
                    input: input.is_none() && device_parts.input,
                    output: sink.is_null() && device_parts.output,
                };
                if pending.any() {
                    audio_job = Some(spawn_open_audio(cur_audio.clone(), missing, cap_tx.clone(), false));
                } else if missing.any() {
                    audio_retry = Some(tokio::time::Instant::now() + AUDIO_RETRY);
                }
                continue;
            }
            _ = tokio::time::sleep_until(audio_retry.unwrap_or_else(tokio::time::Instant::now)),
                if audio_retry.is_some() && audio_job.is_none() =>
            {
                audio_retry = None;
                let missing = AudioParts {
                    input: input.is_none() && device_parts.input,
                    output: sink.is_null() && device_parts.output,
                };
                if missing.any() {
                    audio_job = Some(spawn_open_audio(cur_audio.clone(), missing, cap_tx.clone(), true));
                }
                continue;
            }
            _ = meter_tick.tick(), if meter_on => {
                shared.emit(SessionEvent::MicLevel { db: engine.take_ui_level() });
                continue;
            }
            _ = share_tick.tick(), if !shares_in.is_empty() => {
                let now_ms = started.elapsed().as_millis() as u64;
                for (friend, rx) in shares_in.iter_mut() {
                    let (out, report) = rx.tick(now_ms);
                    handle_output(out, friend, rx, &net, &shared.video);
                    rx.log_stats(now_ms, friend, &net);
                    let Some(report) = report else { continue };
                    // The sender announced the share, so it is new enough; check anyway (D-024).
                    if lock(&shared.state).proto.get(friend).is_some_and(|v| peer_supports_video(*v))
                        && let Err(e) = net.send_control(friend, report)
                    {
                        tracing::debug!(%friend, "cannot send the receiver report: {e}");
                    }
                }
                continue;
            }
            Some(cmd) = commands.recv() => {
                match cmd {
                    EngineCmd::Muted(m) => engine.activation_mut().set_muted(m),
                    EngineCmd::Deafened(d) => engine.activation_mut().set_deafened(d),
                    EngineCmd::Ptt(p) => engine.activation_mut().set_ptt(p),
                    EngineCmd::InRoom(r) => {
                        tracing::info!(in_room = r, "room presence changed");
                        engine.set_in_room(r);
                    }
                    EngineCmd::Audio(audio) => {
                        if let Err(e) = engine.apply_audio(&audio) {
                            tracing::warn!("cannot apply the audio settings: {e}");
                        }
                        let changed = AudioParts {
                            input: audio.input_device != cur_audio.input_device && device_parts.input,
                            output: audio.output_device != cur_audio.output_device && device_parts.output,
                        };
                        cur_audio = *audio;
                        if changed.any() {
                            tracing::info!(input = %cur_audio.input_device, output = %cur_audio.output_device, "reopening the audio devices");
                            if audio_job.is_some() {
                                audio_pending = audio_pending.union(changed);
                            } else {
                                // Close the old streams before opening the new ones.
                                if changed.input {
                                    release_audio(input.take());
                                }
                                if changed.output {
                                    release_audio(std::mem::replace(&mut sink, Sink::Null));
                                }
                                let missing = AudioParts {
                                    input: input.is_none() && device_parts.input,
                                    output: sink.is_null() && device_parts.output,
                                };
                                audio_retry = None;
                                audio_job = Some(spawn_open_audio(cur_audio.clone(), changed.union(missing), cap_tx.clone(), false));
                            }
                        }
                    }
                    EngineCmd::Cue(c) => engine.play_cue(c),
                    EngineCmd::CueLoop(c) => engine.set_cue_loop(c),
                    EngineCmd::MicMeter(on) => {
                        engine.set_ui_meter(on);
                        if on && !meter_on {
                            meter_tick.reset();
                        }
                        meter_on = on;
                    }
                    EngineCmd::MicTest(on) => {
                        if let Err(e) = engine.set_loopback(on) {
                            tracing::warn!("cannot start the voice test: {e}");
                        }
                    }
                    EngineCmd::PeerVolume(friend, v) => engine.set_peer_volume(&friend, v),
                    EngineCmd::ShareAudioVolume(v) => engine.set_share_audio_volume(v),
                    EngineCmd::ShareStart(pipe, audio) => {
                        if let Some(old) = share_out.take() {
                            *lock(&shared.share) = None;
                            old.stop(&net);
                        }
                        let peers: Vec<String> = {
                            let st = lock(&shared.state);
                            st.proto
                                .iter()
                                .filter(|(f, v)| peer_supports_video(**v) && net.is_connected(f))
                                .map(|(f, _)| f.clone())
                                .collect()
                        };
                        tracing::info!(viewers = peers.len(), "share started");
                        let mut out = ShareOut::start(&net, *pipe, &peers);
                        if let Some(capture) = audio {
                            out.start_audio(&net, capture);
                        }
                        *lock(&shared.share) = Some(out.shared());
                        share_out = Some(out);
                    }
                    EngineCmd::ShareStop => {
                        if let Some(old) = share_out.take() {
                            *lock(&shared.share) = None;
                            old.stop(&net);
                            tracing::info!("share stopped");
                        }
                    }
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
                    name: lock(&shared.config).name.clone(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                    protocol_version: PROTOCOL_VERSION,
                    // Never block the runtime on the probe: use the cached result if
                    // ready (main() warms it at startup; library users/tests send empty).
                    capabilities: sala_video::capabilities_if_ready().unwrap_or_default(),
                };
                let status = local.to_message();
                for msg in [hello, status] {
                    if let Err(e) = net.send_control(&friend, msg) {
                        tracing::warn!(%friend, "cannot send: {e}");
                    }
                }
                if newly_pinned {
                    let mut pinned = false;
                    {
                        let mut config = lock(&shared.config);
                        if let Some(f) = config.friends.iter_mut().find(|f| f.name == friend) {
                            f.fingerprint = Some(fingerprint.to_hex());
                            pinned = true;
                            match config.save(&shared.profile) {
                                Ok(()) => tracing::info!(%friend, "fingerprint pinned (TOFU) and saved"),
                                Err(e) => tracing::error!(%friend, "cannot save pinned fingerprint: {e}"),
                            }
                        }
                    }
                    if pinned {
                        shared.emit(SessionEvent::ConfigChanged);
                    }
                }
                if first {
                    shared.emit(SessionEvent::Connected { friend });
                }
            }
            NetEvent::Control { friend, msg } => match msg {
                ControlMessage::Hello { name, app_version, protocol_version, .. } => {
                    tracing::info!(%friend, %name, %app_version, protocol_version, "event: Hello");
                    lock(&shared.state).proto.insert(friend.clone(), protocol_version);
                    // A friend who connects while we share joins the stream.
                    if let Some(out) = &share_out
                        && peer_supports_video(protocol_version)
                    {
                        out.add_peer(&net, &friend);
                    }
                }
                ControlMessage::ShareStart { codec, width, height, fps } => {
                    tracing::info!(%friend, ?codec, width, height, fps, "event: ShareStart");
                    let now_ms = started.elapsed().as_millis() as u64;
                    engine.drop_share_audio(&friend);
                    shares_in.insert(friend, ShareIn::new(now_ms, codec));
                }
                ControlMessage::ShareStop => {
                    tracing::info!(%friend, "event: ShareStop");
                    shares_in.remove(&friend);
                    engine.drop_share_audio(&friend);
                }
                ControlMessage::KeyframeRequest => {
                    if let Some(out) = &share_out {
                        tracing::debug!(%friend, "event: KeyframeRequest");
                        out.request_keyframe();
                    }
                }
                ControlMessage::ReceiverReport { received, expected, dropped_frames, window_ms } => {
                    if let Some(out) = &share_out {
                        let rtt = net.rtt(&friend).map(|d| d.as_millis().min(u128::from(u32::MAX)) as u32);
                        let target = out.on_report(abr_report(received, expected, dropped_frames, rtt));
                        tracing::debug!(%friend, received, expected, dropped_frames, window_ms, target, "event: ReceiverReport");
                    }
                }
                ControlMessage::Status { muted, deafened, game_mode, sharing, in_room } => {
                    tracing::info!(%friend, muted, deafened, game_mode, sharing, in_room, "event: Status");
                    let status = StatusFlags { muted, deafened, game_mode, sharing, in_room };
                    {
                        let mut st = lock(&shared.state);
                        let before = st.peers.insert(friend.clone(), status).map(|p| p.in_room);
                        if let Some(cue) = peer_room_cue(st.in_room, before, in_room) {
                            shared.cue(cue);
                        }
                        // The friend joined: our ring was answered (no message needed).
                        if in_room && st.calls.drop_out(&friend).is_some() {
                            shared.calls_changed(&st);
                        }
                    }
                    engine.set_peer_in_room(&friend, in_room);
                    shared.emit(SessionEvent::PeerStatus { friend, status });
                }
                ControlMessage::Ring { call_id } => {
                    let mut st = lock(&shared.state);
                    // Already in the room: the friend's `Status` is the whole news.
                    if !st.in_room && st.calls.ring_in(&friend, call_id) {
                        tracing::info!(%friend, call_id, "event: ringing us");
                        shared.calls_changed(&st);
                    }
                }
                ControlMessage::RingCancel { call_id } => {
                    let mut st = lock(&shared.state);
                    if st.calls.cancel_in(&friend, call_id) {
                        tracing::info!(%friend, call_id, "event: ring cancelled");
                        shared.calls_changed(&st);
                        shared.emit(SessionEvent::CallResult { friend, end: CallEnd::Cancelled });
                    }
                }
                ControlMessage::RingDecline { call_id } => {
                    let mut st = lock(&shared.state);
                    if st.calls.end_out(&friend, call_id) {
                        tracing::info!(%friend, call_id, "event: ring declined");
                        shared.cue(Cue::CallFailed);
                        shared.calls_changed(&st);
                        shared.emit(SessionEvent::CallResult { friend, end: CallEnd::Declined });
                    }
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
                MediaDatagram::Audio { stream: AudioStream::ShareAudio, seq, timestamp, payload } => {
                    // Only while the friend's share is announced (late datagrams after a
                    // `ShareStop` must not bring the decoder back).
                    if shares_in.contains_key(&friend) {
                        let now_ms = started.elapsed().as_millis() as u64;
                        engine.push_share_audio(&friend, seq, timestamp, payload, now_ms);
                    }
                }
                MediaDatagram::Video { .. } => {
                    if let Some(rx) = shares_in.get_mut(&friend) {
                        let now_ms = started.elapsed().as_millis() as u64;
                        let out = rx.on_datagram(now_ms, &dg);
                        handle_output(out, &friend, rx, &net, &shared.video);
                    }
                }
            },
            NetEvent::Disconnected { friend } => {
                shares_in.remove(&friend);
                engine.drop_share_audio(&friend);
                if let Some(out) = &share_out {
                    out.remove_peer(&friend);
                }
                publish_voice_counters(&engine, &shared);
                engine.remove_peer(&friend);
                // End of call: the chat history is discarded (inv-chat-never-on-disk).
                {
                    let mut st = lock(&shared.state);
                    st.history.remove(&friend);
                    let was_in_room = st.peers.remove(&friend).is_some_and(|p| p.in_room);
                    st.proto.remove(&friend);
                    if let Some(cue) = peer_room_cue(st.in_room, Some(was_in_room), false) {
                        shared.cue(cue);
                    }
                    if st.calls.forget(&friend) {
                        shared.calls_changed(&st);
                    }
                }
                tracing::info!(%friend, "event: disconnected");
                shared.emit(SessionEvent::Disconnected { friend });
            }
            NetEvent::Reconnecting { friend, attempt, delay } => {
                tracing::info!(%friend, attempt, ?delay, "event: reconnecting");
                // After a couple of failures (and every ~5 min) check whether the address is
                // reachable through Tailscale at all; a friend on another account is not.
                if attempt % 20 == 2 {
                    check_tailnet_peer(&shared, &friend);
                }
                shared.emit(SessionEvent::Reconnecting { friend, attempt });
            }
            NetEvent::Rejected { .. } => {}
            NetEvent::FingerprintMismatch { friend, .. } => {
                shared.emit(SessionEvent::FingerprintMismatch { friend });
            }
        }
    }
    if let Some(out) = share_out.take() {
        *lock(&shared.share) = None;
        out.stop(&net);
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

/// Mirrors the engine's share-audio counters for `Session::share_audio_counters`. A friend
/// whose share ended keeps its last counters (like the voice ones).
fn publish_share_audio(engine: &VoiceEngine, shared: &Shared) {
    let names = engine.share_audio_names();
    if names.is_empty() {
        return;
    }
    let mut map = lock(&shared.share_audio);
    for n in names {
        if let Some((stats, peak)) = engine.share_audio_stats(&n) {
            let peak_db = if peak > 0.0 { 20.0 * peak.log10() } else { -120.0 };
            map.insert(n, ShareAudioCounters { received: stats.received, played: stats.played, peak_db });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(muted: bool, deafened: bool, in_room: bool) -> StatusFlags {
        StatusFlags { muted, deafened, in_room, ..StatusFlags::default() }
    }

    #[test]
    fn everybody_starts_outside_the_room() {
        assert!(!StatusFlags::default().in_room);
    }

    #[test]
    fn one_local_change_makes_one_sound() {
        let base = flags(false, false, true);
        assert_eq!(local_change_cue(flags(false, false, false), base), Some(Cue::SelfJoin));
        assert_eq!(local_change_cue(base, flags(false, false, false)), Some(Cue::SelfLeave));
        assert_eq!(local_change_cue(base, flags(true, false, true)), Some(Cue::Mute));
        assert_eq!(local_change_cue(flags(true, false, true), base), Some(Cue::Unmute));
        // Deafening also mutes, but only the deafen sound plays.
        assert_eq!(local_change_cue(base, flags(true, true, true)), Some(Cue::Deafen));
        assert_eq!(local_change_cue(flags(true, true, true), flags(true, false, true)), Some(Cue::Undeafen));
        // Control: nothing changed that has a sound.
        assert_eq!(local_change_cue(base, base), None);
        let game = StatusFlags { game_mode: true, ..base };
        assert_eq!(local_change_cue(base, game), None);
    }

    #[test]
    fn peer_room_sounds_need_us_in_the_room() {
        assert_eq!(peer_room_cue(true, Some(false), true), Some(Cue::PeerJoin));
        assert_eq!(peer_room_cue(true, None, true), Some(Cue::PeerJoin));
        assert_eq!(peer_room_cue(true, Some(true), false), Some(Cue::PeerLeave));
        // Control: outside the room we hear nothing about it; no change, no sound.
        assert_eq!(peer_room_cue(false, Some(false), true), None);
        assert_eq!(peer_room_cue(false, Some(true), false), None);
        assert_eq!(peer_room_cue(true, Some(true), true), None);
        assert_eq!(peer_room_cue(true, None, false), None);
    }

    #[test]
    fn only_the_device_source_opens_the_microphone() {
        assert_eq!(AudioSource::Device.device_parts(), AudioParts::BOTH);
        // `--synthetic-audio`: tone for the mic, real output.
        assert_eq!(AudioSource::SyntheticMic.device_parts(), AudioParts { input: false, output: true });
        assert!(AudioSource::SyntheticMic.synthetic_mic());
        // Tests: no device at all.
        assert!(!AudioSource::Synthetic.device_parts().any());
        assert!(!AudioSource::Device.synthetic_mic());
    }
}
