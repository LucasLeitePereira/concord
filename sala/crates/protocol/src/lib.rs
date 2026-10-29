//! Shared types for the network protocol (core <-> core) and IPC (ui <-> core).
//!
//! API skeleton fixed by the architect. Implement the `todo!()` bodies; do not
//! change public signatures, type names, variants or field names.

use serde::{Deserialize, Serialize};

pub mod pipe;
pub use pipe::{current_user_pipe_name, pipe_name};

/// Named pipe client and line reader (Windows only, feature `ipc-client`).
#[cfg(all(windows, feature = "ipc-client"))]
pub mod ipc_client;

/// Version of the wire protocol announced in `Hello`. 2 = understands `Ring*` (D-024):
/// an older peer drops the control stream on an unknown variant, so `Ring` is only sent
/// to peers that announced at least [`RING_PROTOCOL_VERSION`].
pub const PROTOCOL_VERSION: u16 = 2;

/// First protocol version that understands `ControlMessage::Ring*` (D-024).
pub const RING_PROTOCOL_VERSION: u16 = 2;

/// Datagram size limit to assume when the connection does not report one.
pub const DEFAULT_MAX_DATAGRAM: usize = 1200;

/// Upper bound for one control frame payload (defensive limit for the decoder).
pub const MAX_CONTROL_FRAME: usize = 64 * 1024;

/// Errors produced by encoding/decoding.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// Encoded message would exceed the allowed size.
    #[error("message too large: {size} > {max}")]
    TooLarge { size: usize, max: usize },
    /// Bytes could not be decoded into a message.
    #[error("decode error: {0}")]
    Decode(String),
    /// Value could not be encoded.
    #[error("encode error: {0}")]
    Encode(String),
}

/// Video codec identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    Av1,
    Hevc,
    H264,
}

/// Hardware video capabilities of one machine.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub hw_encode: Vec<VideoCodec>,
    pub hw_decode: Vec<VideoCodec>,
}

fn default_true() -> bool {
    true
}

/// For the bincode wire (no field names): a frame that ends before this trailing
/// `bool` comes from an older peer and means `true`.
fn de_bool_or_true<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(bool::deserialize(d).unwrap_or(true))
}

/// Messages sent on the reliable bidirectional control stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    Hello {
        name: String,
        app_version: String,
        protocol_version: u16,
        capabilities: Capabilities,
    },
    Status {
        muted: bool,
        deafened: bool,
        game_mode: bool,
        sharing: bool,
        /// The sender is in the room (D-015). Frames from older peers lack it: `true`.
        #[serde(default = "default_true", deserialize_with = "de_bool_or_true")]
        in_room: bool,
    },
    Chat {
        id: u64,
        text: String,
        /// Unix time in milliseconds.
        sent_at: u64,
    },
    ShareStart {
        codec: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
    },
    ShareStop,
    KeyframeRequest,
    /// Periodic liveness signal.
    Heartbeat,
    Bye,
    /// The sender rings the receiver (D-024). Added after `Bye`: new variants only go last.
    Ring { call_id: u64 },
    /// The caller gave up (hang-up or the 30 s deadline).
    RingCancel { call_id: u64 },
    /// The receiver refused the call.
    RingDecline { call_id: u64 },
}

impl ControlMessage {
    /// Encodes one control frame: 4-byte big-endian `u32` payload length,
    /// followed by the bincode payload.
    ///
    /// Errors with `TooLarge` if the payload exceeds `MAX_CONTROL_FRAME`.
    pub fn encode_frame(&self) -> Result<Vec<u8>, ProtocolError> {
        let payload = bincode::serialize(self).map_err(|e| ProtocolError::Encode(e.to_string()))?;
        if payload.len() > MAX_CONTROL_FRAME {
            return Err(ProtocolError::TooLarge {
                size: payload.len(),
                max: MAX_CONTROL_FRAME,
            });
        }
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }
}

/// Incremental decoder for the control stream (bytes may arrive split or
/// coalesced arbitrarily).
#[derive(Debug)]
pub struct FrameDecoder {
    max_frame_len: usize,
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// `max_frame_len` bounds the declared payload length; larger declared
    /// lengths are rejected with `TooLarge` before buffering the payload.
    pub fn new(max_frame_len: usize) -> Self {
        Self {
            max_frame_len,
            buf: Vec::new(),
        }
    }

    /// Appends received bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete message, `Ok(None)` if more bytes are needed.
    pub fn next_message(&mut self) -> Result<Option<ControlMessage>, ProtocolError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > self.max_frame_len {
            return Err(ProtocolError::TooLarge {
                size: len,
                max: self.max_frame_len,
            });
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let result = bincode::deserialize::<ControlMessage>(&self.buf[4..4 + len])
            .map_err(|e| ProtocolError::Decode(e.to_string()));
        // Consume the frame even on a decode error so the stream can resync.
        self.buf.drain(..4 + len);
        result.map(Some)
    }
}

/// Which audio stream an audio datagram belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioStream {
    Voice,
    ShareAudio,
}

/// Unreliable media datagram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaDatagram {
    Audio {
        stream: AudioStream,
        seq: u16,
        /// In samples at 48 kHz.
        timestamp: u32,
        payload: Vec<u8>,
    },
    Video {
        frame_id: u32,
        frag_idx: u16,
        frag_count: u16,
        keyframe: bool,
        /// XOR FEC: one parity fragment per this many data fragments (0 = no FEC).
        /// Parity fragments follow the data ones: `frag_count` counts both.
        fec_group: u8,
        timestamp: u32,
        payload: Vec<u8>,
    },
}

impl MediaDatagram {
    /// Encodes the datagram (bincode). Errors with `TooLarge` if the encoded
    /// size exceeds `max_size`.
    pub fn encode(&self, max_size: usize) -> Result<Vec<u8>, ProtocolError> {
        let bytes = bincode::serialize(self).map_err(|e| ProtocolError::Encode(e.to_string()))?;
        if bytes.len() > max_size {
            return Err(ProtocolError::TooLarge {
                size: bytes.len(),
                max: max_size,
            });
        }
        Ok(bytes)
    }

    /// Decodes a datagram. Never panics on malformed input.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        bincode::deserialize(bytes).map_err(|e| ProtocolError::Decode(e.to_string()))
    }
}

/// Fixed encoded overhead of `empty` (a datagram with an empty payload); the
/// payload length prefix has a fixed width, so the overhead does not grow.
fn max_payload_for(empty: &MediaDatagram, max_datagram: usize) -> usize {
    let overhead = bincode::serialized_size(empty).unwrap_or(u64::MAX) as usize;
    max_datagram.saturating_sub(overhead)
}

/// Largest audio payload such that the encoded `Audio` datagram fits in
/// `max_datagram` bytes.
pub fn max_audio_payload(max_datagram: usize) -> usize {
    let empty = MediaDatagram::Audio {
        stream: AudioStream::Voice,
        seq: 0,
        timestamp: 0,
        payload: Vec::new(),
    };
    max_payload_for(&empty, max_datagram)
}

/// Largest video payload such that the encoded `Video` datagram fits in
/// `max_datagram` bytes.
pub fn max_video_payload(max_datagram: usize) -> usize {
    let empty = MediaDatagram::Video {
        frame_id: 0,
        frag_idx: 0,
        frag_count: 0,
        keyframe: false,
        fec_group: 0,
        timestamp: 0,
        payload: Vec::new(),
    };
    max_payload_for(&empty, max_datagram)
}

/// One chat entry as exposed to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatEntry {
    pub id: u64,
    pub from_self: bool,
    pub text: String,
    pub sent_at: u64,
}

/// Voice transmission mode (`audio.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceMode {
    VoiceActivity,
    PushToTalk,
}

/// The `[audio]` section as exposed to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSettings {
    /// cpal device name or `"default"`.
    pub input_device: String,
    pub output_device: String,
    /// 32..=128.
    pub bitrate_kbps: u32,
    pub mode: VoiceMode,
    /// dBFS, -100..=0.
    pub vad_threshold: i32,
    /// Microphone gain in percent, 0..=200.
    pub input_volume: u32,
    pub noise_suppression: bool,
    /// Volume of the notification sounds in percent, 0..=100 (0 = off, D-024).
    #[serde(default = "default_cue_volume")]
    pub cue_volume: u32,
}

/// Default of `audio.cue_volume` (percent).
pub const DEFAULT_CUE_VOLUME: u32 = 60;

fn default_cue_volume() -> u32 {
    DEFAULT_CUE_VOLUME
}

/// The `[game_mode]` section as exposed to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameModeSettings {
    pub auto_detect: bool,
    pub games: Vec<String>,
    pub exclude: Vec<String>,
    pub keep_sharing_in_game_mode: bool,
}

/// The `[hotkeys]` section as exposed to the UI (strings such as `"Ctrl+Shift+M"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeySettings {
    pub push_to_talk: String,
    pub toggle_mute: String,
    pub toggle_deafen: String,
    pub toggle_game_mode: String,
}

/// One configured friend as exposed to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendInfo {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub port: Option<u16>,
    /// A fingerprint is pinned for this friend.
    pub verified: bool,
}

/// Commands from the UI to the core (IPC). The protocol only grows by addition (D-012).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UiToCore {
    GetState,
    GetChatHistory,
    SendChat { text: String },
    SetMuted { muted: bool },
    SetDeafened { deafened: bool },
    /// Enters the room and rings `friend` (D-024); `friend` must be a configured friend.
    Call { friend: String },
    /// Leaves the room (D-015) and cancels the pending rings: no voice either way; chat and
    /// status go on.
    HangUp,
    /// Enters the room without ringing anybody (D-024).
    JoinRoom,
    /// Answers a ring by entering the room (D-024).
    AcceptCall { friend: String },
    /// Refuses a ring (D-024); the caller is told.
    DeclineCall { friend: String },
    /// The UI window gained/lost focus (chat sound only plays while it is unfocused).
    SetUiFocus { focused: bool },
    EnterGameMode,
    OpenViewer,
    /// Show the UI (a second `sala-core` instance sends it; leaves game mode if needed).
    OpenUi,
    /// Manual exit from game mode (the UI is relaunched).
    ExitGameMode,
    /// Answered with `CoreToUi::Config`.
    GetConfig,
    /// Partial update: only the sections present are replaced (each whole). Applied
    /// live, saved to `config.toml`, answered by a `Config` broadcast or an `Error`.
    UpdateConfig {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        audio: Option<AudioSettings>,
        #[serde(default)]
        game_mode: Option<GameModeSettings>,
        #[serde(default)]
        hotkeys: Option<HotkeySettings>,
    },
    /// Adds (and dials) a friend; `address` is an IP.
    AddFriend { name: String, address: String },
    /// Removes a friend and closes the connection.
    RemoveFriend { name: String },
    /// Forgets the pinned fingerprint (the next connection pins again).
    ResetFingerprint { name: String },
    /// Answered with `CoreToUi::AudioDevices`.
    ListAudioDevices,
    /// While on, `CoreToUi::MicLevel` arrives every ~50 ms.
    SetMicMeter { on: bool },
    /// Playback volume of one friend, 0.0..=2.0 (not persisted).
    SetPeerVolume { friend: String, volume: f32 },
    /// While on, the core unregisters every global shortcut and the push-to-talk key (so the
    /// UI can capture a combination); turning it off, or the client disconnecting, restores them.
    SetHotkeyCapture { on: bool },
    /// Answered with `CoreToUi::TailnetPeers` (devices of this PC's Tailscale network).
    ListTailnetPeers,
}

/// One device of the Tailscale network (own tailnet or shared with us).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TailnetPeer {
    /// Device name (Tailscale `HostName`).
    pub name: String,
    /// Tailscale IPv4 address.
    pub address: String,
    pub online: bool,
    /// Owner's display name, when Tailscale knows it.
    #[serde(default)]
    pub owner: Option<String>,
    /// Same Tailscale user as this PC.
    #[serde(default)]
    pub mine: bool,
    /// `windows`, `linux`, `android`...
    #[serde(default)]
    pub os: String,
}

/// Events/state from the core to the UI (IPC). Unknown variants must be ignored by the UI (D-012).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)] // `Config` is rare and short-lived; boxing would only complicate the UI
pub enum CoreToUi {
    ChatHistory { entries: Vec<ChatEntry> },
    ChatReceived { entry: ChatEntry },
    Status {
        muted: bool,
        deafened: bool,
        game_mode: bool,
        #[serde(default = "default_true")]
        in_room: bool,
    },
    Error { message: String },
    Shutdown,
    /// A friend's connection became active.
    PeerConnected { friend: String },
    PeerDisconnected { friend: String },
    PeerReconnecting { friend: String, attempt: u32 },
    /// The `Status` a friend announced (also sent on `GetState`).
    PeerStatus {
        friend: String,
        muted: bool,
        deafened: bool,
        game_mode: bool,
        sharing: bool,
        #[serde(default = "default_true")]
        in_room: bool,
    },
    /// A friend presented a different certificate than the pinned one.
    FingerprintMismatch { friend: String },
    /// Current configuration (reply to `GetConfig`; broadcast after every change).
    Config {
        name: String,
        audio: AudioSettings,
        game_mode: GameModeSettings,
        hotkeys: HotkeySettings,
        friends: Vec<FriendInfo>,
        /// This PC's Tailscale `ip[:port]` (port only when not the default); `None` when
        /// Tailscale was not found.
        #[serde(default)]
        my_address: Option<String>,
    },
    /// Device names; `"default"` first.
    AudioDevices { inputs: Vec<String>, outputs: Vec<String> },
    /// Microphone level (RMS of the last ~50 ms, dBFS, floor -120) while the meter is on.
    MicLevel { db: f32 },
    /// Every 2 s per connected friend.
    CallStats { friend: String, rtt_ms: u32, since_secs: u64 },
    /// The friend's address is not reachable through this PC's Tailscale (another account
    /// without sharing). Cleared by `PeerConnected`.
    PeerNotInTailnet { friend: String },
    /// Reply to `ListTailnetPeers`; `running` false = Tailscale off or not found (empty list).
    TailnetPeers { running: bool, peers: Vec<TailnetPeer> },
    /// Rings in progress (D-024): `incoming` = friends ringing us, `outgoing` = friends we
    /// are ringing. A snapshot, sent at every change and on `GetState`.
    Calls { incoming: Vec<String>, outgoing: Vec<String> },
    /// How a ring ended without a join: `"declined"`, `"timeout"`, `"cancelled"` (the caller
    /// gave up) or `"unsupported"` (the friend's Concord is too old to ring).
    CallResult { friend: String, result: String },
}

/// Encodes an IPC message as exactly one JSON line terminated by `\n`.
pub fn encode_ipc_line<T: Serialize>(msg: &T) -> Result<String, ProtocolError> {
    let mut line = serde_json::to_string(msg).map_err(|e| ProtocolError::Encode(e.to_string()))?;
    line.push('\n');
    Ok(line)
}

/// Decodes one IPC JSON line (trailing `\n`/`\r\n` allowed).
pub fn decode_ipc_line<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, ProtocolError> {
    serde_json::from_str(line.trim_end_matches(['\n', '\r']))
        .map_err(|e| ProtocolError::Decode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_messages() -> Vec<ControlMessage> {
        vec![
            ControlMessage::Hello {
                name: "ana".into(),
                app_version: "0.1.0".into(),
                protocol_version: PROTOCOL_VERSION,
                capabilities: Capabilities {
                    hw_encode: vec![VideoCodec::Av1, VideoCodec::H264],
                    hw_decode: vec![VideoCodec::Hevc],
                },
            },
            ControlMessage::Status {
                muted: true,
                deafened: false,
                game_mode: true,
                sharing: false,
                in_room: false,
            },
            ControlMessage::Chat {
                id: u64::MAX,
                text: "olá, mundo".into(),
                sent_at: 1_700_000_000_000,
            },
            ControlMessage::ShareStart {
                codec: VideoCodec::Hevc,
                width: 1920,
                height: 1080,
                fps: 60,
            },
            ControlMessage::ShareStop,
            ControlMessage::KeyframeRequest,
            ControlMessage::Heartbeat,
            ControlMessage::Bye,
            ControlMessage::Ring { call_id: u64::MAX },
            ControlMessage::RingCancel { call_id: 7 },
            ControlMessage::RingDecline { call_id: 0 },
        ]
    }

    /// Bincode index of the variant: the first 4 bytes (LE) of the payload.
    fn variant_index(m: &ControlMessage) -> u32 {
        let f = m.encode_frame().unwrap();
        u32::from_le_bytes([f[4], f[5], f[6], f[7]])
    }

    #[test]
    fn ring_variants_were_added_at_the_end() {
        // Old peers know indices 0..=7 (up to `Bye`); anything else breaks their stream, so
        // the existing indices must never move and the new ones come after (D-024).
        assert_eq!(variant_index(&ControlMessage::Bye), 7);
        assert_eq!(variant_index(&ControlMessage::Heartbeat), 6);
        assert_eq!(variant_index(&ControlMessage::Ring { call_id: 1 }), 8);
        assert_eq!(variant_index(&ControlMessage::RingCancel { call_id: 1 }), 9);
        assert_eq!(variant_index(&ControlMessage::RingDecline { call_id: 1 }), 10);
    }

    #[test]
    fn unknown_variant_is_a_decode_error_and_the_decoder_resyncs() {
        // What an old peer sees of a `Ring`: a variant index it does not know.
        let mut payload = 99u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&1u64.to_le_bytes());
        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        d.push(&(payload.len() as u32).to_be_bytes());
        d.push(&payload);
        d.push(&ControlMessage::Heartbeat.encode_frame().unwrap());
        assert!(matches!(d.next_message(), Err(ProtocolError::Decode(_))));
        assert_eq!(d.next_message().unwrap(), Some(ControlMessage::Heartbeat));
    }

    #[test]
    fn ring_needs_protocol_version_2() {
        const { assert!(PROTOCOL_VERSION >= RING_PROTOCOL_VERSION) };
        assert_eq!(RING_PROTOCOL_VERSION, 2);
    }

    #[test]
    fn control_roundtrip_all() {
        for m in all_messages() {
            let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
            d.push(&m.encode_frame().unwrap());
            assert_eq!(d.next_message().unwrap(), Some(m));
            assert_eq!(d.next_message().unwrap(), None);
        }
    }

    #[test]
    fn decoder_handles_byte_by_byte_and_coalesced() {
        let msgs = all_messages();
        let bytes: Vec<u8> = msgs.iter().flat_map(|m| m.encode_frame().unwrap()).collect();
        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        let mut got = Vec::new();
        for b in &bytes {
            d.push(&[*b]);
            while let Some(m) = d.next_message().unwrap() {
                got.push(m);
            }
        }
        assert_eq!(got, msgs);

        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        d.push(&bytes);
        let mut got = Vec::new();
        while let Some(m) = d.next_message().unwrap() {
            got.push(m);
        }
        assert_eq!(got, msgs);
    }

    #[test]
    fn decoder_partial_returns_none() {
        let f = ControlMessage::Bye.encode_frame().unwrap();
        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        d.push(&f[..f.len() - 1]);
        assert_eq!(d.next_message().unwrap(), None);
        d.push(&f[f.len() - 1..]);
        assert_eq!(d.next_message().unwrap(), Some(ControlMessage::Bye));
    }

    #[test]
    fn encode_frame_too_large() {
        let m = ControlMessage::Chat {
            id: 1,
            text: "x".repeat(MAX_CONTROL_FRAME),
            sent_at: 0,
        };
        assert!(matches!(m.encode_frame(), Err(ProtocolError::TooLarge { .. })));
        // control: a small chat is fine
        let ok = ControlMessage::Chat {
            id: 1,
            text: "x".repeat(1000),
            sent_at: 0,
        };
        assert!(ok.encode_frame().is_ok());
    }

    #[test]
    fn decoder_rejects_oversized_declared_length_without_payload() {
        let mut d = FrameDecoder::new(100);
        d.push(&1000u32.to_be_bytes());
        assert_eq!(
            d.next_message(),
            Err(ProtocolError::TooLarge { size: 1000, max: 100 })
        );
        // control: within the limit it just waits for more bytes
        let mut d = FrameDecoder::new(2000);
        d.push(&1000u32.to_be_bytes());
        assert_eq!(d.next_message(), Ok(None));
    }

    #[test]
    fn decoder_garbage_is_error_not_panic_and_resyncs() {
        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        d.push(&3u32.to_be_bytes());
        d.push(&[0xff, 0xff, 0xff]);
        assert!(matches!(d.next_message(), Err(ProtocolError::Decode(_))));
        d.push(&ControlMessage::Bye.encode_frame().unwrap());
        assert_eq!(d.next_message().unwrap(), Some(ControlMessage::Bye));
    }

    fn audio(n: usize, stream: AudioStream) -> MediaDatagram {
        MediaDatagram::Audio {
            stream,
            seq: 65535,
            timestamp: u32::MAX,
            payload: vec![7; n],
        }
    }

    fn video(n: usize) -> MediaDatagram {
        MediaDatagram::Video {
            frame_id: 9,
            frag_idx: 2,
            frag_count: 5,
            keyframe: true,
            fec_group: 8,
            timestamp: 123,
            payload: vec![1; n],
        }
    }

    #[test]
    fn datagram_roundtrip() {
        for d in [
            audio(100, AudioStream::Voice),
            audio(0, AudioStream::ShareAudio),
            video(500),
            video(0),
        ] {
            let b = d.encode(DEFAULT_MAX_DATAGRAM).unwrap();
            assert_eq!(MediaDatagram::decode(&b).unwrap(), d);
        }
    }

    #[test]
    fn datagram_size_limit() {
        let d = audio(DEFAULT_MAX_DATAGRAM, AudioStream::Voice);
        assert!(matches!(
            d.encode(DEFAULT_MAX_DATAGRAM),
            Err(ProtocolError::TooLarge { .. })
        ));
    }

    #[test]
    fn max_payload_is_exact() {
        for max in [DEFAULT_MAX_DATAGRAM, 1000, 1350] {
            let a = max_audio_payload(max);
            assert!(audio(a, AudioStream::Voice).encode(max).is_ok());
            assert!(audio(a + 1, AudioStream::Voice).encode(max).is_err());
            let v = max_video_payload(max);
            assert!(video(v).encode(max).is_ok());
            assert!(video(v + 1).encode(max).is_err());
        }
        assert_eq!(max_audio_payload(0), 0);
        assert_eq!(max_video_payload(3), 0);
    }

    #[test]
    fn datagram_decode_malformed_never_panics() {
        assert!(MediaDatagram::decode(&[]).is_err());
        assert!(MediaDatagram::decode(&[0xff; 7]).is_err());
        let b = audio(50, AudioStream::Voice).encode(2000).unwrap();
        assert!(MediaDatagram::decode(&b[..b.len() - 10]).is_err());
        // huge declared vec length (length prefix is the last 8 bytes before payload)
        let mut bad = b.clone();
        let at = b.len() - 50 - 8;
        bad[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(MediaDatagram::decode(&bad).is_err());
    }

    #[test]
    fn ipc_roundtrip_and_format() {
        let cmds = [
            UiToCore::GetState,
            UiToCore::GetChatHistory,
            UiToCore::SendChat { text: "oi\nlinha".into() },
            UiToCore::SetMuted { muted: true },
            UiToCore::SetDeafened { deafened: false },
            UiToCore::Call { friend: "bia".into() },
            UiToCore::HangUp,
            UiToCore::JoinRoom,
            UiToCore::AcceptCall { friend: "bia".into() },
            UiToCore::DeclineCall { friend: "bia".into() },
            UiToCore::SetUiFocus { focused: true },
            UiToCore::EnterGameMode,
            UiToCore::OpenViewer,
            UiToCore::OpenUi,
            UiToCore::ExitGameMode,
            UiToCore::GetConfig,
            UiToCore::UpdateConfig { name: Some("ana".into()), audio: Some(audio_settings()), game_mode: None, hotkeys: None },
            UiToCore::AddFriend { name: "bia".into(), address: "100.64.0.2".into() },
            UiToCore::RemoveFriend { name: "bia".into() },
            UiToCore::ResetFingerprint { name: "bia".into() },
            UiToCore::ListAudioDevices,
            UiToCore::SetMicMeter { on: true },
            UiToCore::SetHotkeyCapture { on: true },
            UiToCore::ListTailnetPeers,
            UiToCore::SetPeerVolume { friend: "bia".into(), volume: 1.5 },
        ];
        for c in cmds {
            let l = encode_ipc_line(&c).unwrap();
            assert!(l.ends_with('\n'));
            assert_eq!(l.matches('\n').count(), 1, "embedded newline must be escaped");
            assert_eq!(decode_ipc_line::<UiToCore>(&l).unwrap(), c);
        }
        let e = ChatEntry {
            id: 1,
            from_self: false,
            text: "t".into(),
            sent_at: 5,
        };
        let evs = [
            CoreToUi::ChatHistory { entries: vec![e.clone()] },
            CoreToUi::ChatReceived { entry: e },
            CoreToUi::Status { muted: true, deafened: true, game_mode: false, in_room: false },
            CoreToUi::Error { message: "x".into() },
            CoreToUi::Shutdown,
            CoreToUi::PeerConnected { friend: "bia".into() },
            CoreToUi::PeerDisconnected { friend: "bia".into() },
            CoreToUi::PeerReconnecting { friend: "bia".into(), attempt: 3 },
            CoreToUi::PeerStatus { friend: "bia".into(), muted: true, deafened: false, game_mode: true, sharing: false, in_room: false },
            CoreToUi::FingerprintMismatch { friend: "bia".into() },
            CoreToUi::PeerNotInTailnet { friend: "bia".into() },
            CoreToUi::TailnetPeers {
                running: true,
                peers: vec![TailnetPeer {
                    name: "pc-bia".into(),
                    address: "100.64.0.2".into(),
                    online: true,
                    owner: Some("Bia".into()),
                    mine: false,
                    os: "windows".into(),
                }],
            },
            CoreToUi::Config {
                name: "ana".into(),
                audio: audio_settings(),
                game_mode: GameModeSettings {
                    auto_detect: true,
                    games: vec!["cs2.exe".into()],
                    exclude: vec![],
                    keep_sharing_in_game_mode: false,
                },
                hotkeys: HotkeySettings {
                    push_to_talk: "Mouse4".into(),
                    toggle_mute: "Ctrl+Shift+M".into(),
                    toggle_deafen: "Ctrl+Shift+D".into(),
                    toggle_game_mode: "Ctrl+Shift+G".into(),
                },
                friends: vec![FriendInfo { name: "bia".into(), address: "100.64.0.2".into(), port: Some(1), verified: true }],
                my_address: Some("100.64.0.9".into()),
            },
            CoreToUi::AudioDevices { inputs: vec!["default".into(), "Mic".into()], outputs: vec!["default".into()] },
            CoreToUi::MicLevel { db: -42.5 },
            CoreToUi::CallStats { friend: "bia".into(), rtt_ms: 23, since_secs: 61 },
            CoreToUi::Calls { incoming: vec!["bia".into()], outgoing: vec![] },
            CoreToUi::CallResult { friend: "bia".into(), result: "declined".into() },
        ];
        for ev in evs {
            let l = encode_ipc_line(&ev).unwrap();
            assert_eq!(decode_ipc_line::<CoreToUi>(&l).unwrap(), ev);
            let crlf = l.replace('\n', "\r\n");
            assert_eq!(decode_ipc_line::<CoreToUi>(&crlf).unwrap(), ev);
        }
        assert_eq!(
            encode_ipc_line(&UiToCore::HangUp).unwrap(),
            "{\"type\":\"HangUp\"}\n"
        );
    }

    fn audio_settings() -> AudioSettings {
        AudioSettings {
            input_device: "default".into(),
            output_device: "Fones".into(),
            bitrate_kbps: 96,
            mode: VoiceMode::PushToTalk,
            vad_threshold: -50,
            input_volume: 100,
            noise_suppression: false,
            cue_volume: 40,
        }
    }

    #[test]
    fn audio_settings_without_cue_volume_use_the_default() {
        // An older UI/config message has no `cue_volume`: 60, not an error.
        let json = "{\"input_device\":\"default\",\"output_device\":\"default\",\"bitrate_kbps\":96,\"mode\":\"voice_activity\",\"vad_threshold\":-50,\"input_volume\":100,\"noise_suppression\":false}";
        let a: AudioSettings = serde_json::from_str(json).unwrap();
        assert_eq!(a.cue_volume, DEFAULT_CUE_VOLUME);
        // Control: an explicit value is kept.
        let b: AudioSettings = serde_json::from_str(&json.replace("}", ",\"cue_volume\":0}")).unwrap();
        assert_eq!(b.cue_volume, 0);
    }

    #[test]
    fn ring_ipc_json_shapes() {
        assert_eq!(
            encode_ipc_line(&UiToCore::AcceptCall { friend: "bia".into() }).unwrap(),
            "{\"type\":\"AcceptCall\",\"friend\":\"bia\"}\n"
        );
        assert_eq!(encode_ipc_line(&UiToCore::JoinRoom).unwrap(), "{\"type\":\"JoinRoom\"}\n");
        assert_eq!(
            encode_ipc_line(&CoreToUi::Calls { incoming: vec!["a".into()], outgoing: vec![] }).unwrap(),
            "{\"type\":\"Calls\",\"incoming\":[\"a\"],\"outgoing\":[]}\n"
        );
    }

    #[test]
    fn status_without_in_room_means_in_room() {
        // Wire (bincode): an old peer's frame ends right before `in_room`.
        let new = ControlMessage::Status { muted: true, deafened: false, game_mode: false, sharing: true, in_room: false };
        let frame = new.encode_frame().unwrap();
        let payload = &frame[4..frame.len() - 1]; // drop the trailing bool byte
        let mut old = (payload.len() as u32).to_be_bytes().to_vec();
        old.extend_from_slice(payload);
        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        d.push(&old);
        assert_eq!(
            d.next_message().unwrap(),
            Some(ControlMessage::Status { muted: true, deafened: false, game_mode: false, sharing: true, in_room: true })
        );
        // Control: the new frame keeps an explicit `false`.
        let mut d = FrameDecoder::new(MAX_CONTROL_FRAME);
        d.push(&frame);
        assert_eq!(d.next_message().unwrap(), Some(new));
        // IPC (JSON): field absent.
        let st: CoreToUi = decode_ipc_line("{\"type\":\"Status\",\"muted\":false,\"deafened\":false,\"game_mode\":false}").unwrap();
        assert_eq!(st, CoreToUi::Status { muted: false, deafened: false, game_mode: false, in_room: true });
        let ps: CoreToUi = decode_ipc_line(
            "{\"type\":\"PeerStatus\",\"friend\":\"b\",\"muted\":false,\"deafened\":false,\"game_mode\":false,\"sharing\":false}",
        )
        .unwrap();
        assert!(matches!(ps, CoreToUi::PeerStatus { in_room: true, .. }));
    }

    #[test]
    fn update_config_sections_are_optional() {
        let c: UiToCore = decode_ipc_line("{\"type\":\"UpdateConfig\",\"name\":\"x\"}").unwrap();
        assert_eq!(c, UiToCore::UpdateConfig { name: Some("x".into()), audio: None, game_mode: None, hotkeys: None });
    }

    #[test]
    fn ipc_decode_errors() {
        assert!(matches!(
            decode_ipc_line::<UiToCore>("not json\n"),
            Err(ProtocolError::Decode(_))
        ));
        assert!(matches!(
            decode_ipc_line::<UiToCore>("{\"type\":\"Nope\"}"),
            Err(ProtocolError::Decode(_))
        ));
        assert!(decode_ipc_line::<UiToCore>("").is_err());
    }
}
