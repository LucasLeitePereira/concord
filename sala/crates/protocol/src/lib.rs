//! Shared types for the network protocol (core <-> core) and IPC (ui <-> core).
//!
//! API skeleton fixed by the architect. Implement the `todo!()` bodies; do not
//! change public signatures, type names, variants or field names.

use serde::{Deserialize, Serialize};

/// Version of the wire protocol announced in `Hello`.
pub const PROTOCOL_VERSION: u16 = 1;

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
}

impl ControlMessage {
    /// Encodes one control frame: 4-byte big-endian `u32` payload length,
    /// followed by the bincode payload.
    ///
    /// Errors with `TooLarge` if the payload exceeds `MAX_CONTROL_FRAME`.
    pub fn encode_frame(&self) -> Result<Vec<u8>, ProtocolError> {
        todo!()
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
        let _ = (bytes, &self.buf, self.max_frame_len);
        todo!()
    }

    /// Returns the next complete message, `Ok(None)` if more bytes are needed.
    pub fn next_message(&mut self) -> Result<Option<ControlMessage>, ProtocolError> {
        todo!()
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
        timestamp: u32,
        payload: Vec<u8>,
    },
}

impl MediaDatagram {
    /// Encodes the datagram (bincode). Errors with `TooLarge` if the encoded
    /// size exceeds `max_size`.
    pub fn encode(&self, max_size: usize) -> Result<Vec<u8>, ProtocolError> {
        let _ = max_size;
        todo!()
    }

    /// Decodes a datagram. Never panics on malformed input.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let _ = bytes;
        todo!()
    }
}

/// Largest audio payload such that the encoded `Audio` datagram fits in
/// `max_datagram` bytes.
pub fn max_audio_payload(max_datagram: usize) -> usize {
    let _ = max_datagram;
    todo!()
}

/// Largest video payload such that the encoded `Video` datagram fits in
/// `max_datagram` bytes.
pub fn max_video_payload(max_datagram: usize) -> usize {
    let _ = max_datagram;
    todo!()
}

/// One chat entry as exposed to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatEntry {
    pub id: u64,
    pub from_self: bool,
    pub text: String,
    pub sent_at: u64,
}

/// Commands from the UI to the core (IPC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UiToCore {
    GetState,
    GetChatHistory,
    SendChat { text: String },
    SetMuted { muted: bool },
    SetDeafened { deafened: bool },
    Call { friend: String },
    HangUp,
    EnterGameMode,
    OpenViewer,
}

/// Events/state from the core to the UI (IPC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CoreToUi {
    ChatHistory { entries: Vec<ChatEntry> },
    ChatReceived { entry: ChatEntry },
    Status { muted: bool, deafened: bool, game_mode: bool },
    Error { message: String },
    Shutdown,
}

/// Encodes an IPC message as exactly one JSON line terminated by `\n`.
pub fn encode_ipc_line<T: Serialize>(msg: &T) -> Result<String, ProtocolError> {
    let _ = msg;
    todo!()
}

/// Decodes one IPC JSON line (trailing `\n`/`\r\n` allowed).
pub fn decode_ipc_line<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, ProtocolError> {
    let _ = line;
    todo!()
}
