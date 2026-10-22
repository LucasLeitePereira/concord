//! Adaptive jitter buffer for incoming voice frames.
//!
//! API skeleton fixed by the architect. Implement the `todo!()` bodies; do not
//! change public signatures, type names or variants. Private fields may change.

/// Duration of one voice frame.
pub const FRAME_MS: u32 = 20;

/// Result of one playout tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pop {
    /// Next frame in sequence order.
    Frame(Vec<u8>),
    /// The frame for this slot is missing: the decoder must run packet loss concealment.
    Lost,
    /// Still buffering (nothing to play yet).
    Empty,
}

/// Counters for diagnostics and the stats overlay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub received: u64,
    pub played: u64,
    pub lost: u64,
    pub late_dropped: u64,
    pub duplicates: u64,
    pub overflow_dropped: u64,
}

/// Adaptive jitter buffer.
///
/// Contract:
/// - `push` is called for every received frame with its local arrival time.
/// - `pop` is called once per `FRAME_MS` by the playout clock with the current
///   local time.
/// - Playout starts on the first `pop` where `now_ms >= first_arrival_ms + target_delay_ms()`,
///   beginning at the lowest buffered sequence number. Before that, `pop` returns `Empty`.
/// - After playout started, `pop` returns `Frame` or `Lost` for the next sequence
///   slot and advances one slot. To grow the delay toward a higher target it may
///   instead return `Empty` without advancing; to shrink it, it may discard
///   buffered frames (counted in `overflow_dropped`).
/// - The target delay starts at `min_delay_ms`, adapts to measured arrival jitter
///   and stays at `min_delay_ms` when arrivals are perfectly regular.
/// - Sequence numbers are `u16` and wrap around.
/// - At most `max_delay_ms / FRAME_MS + 1` frames are kept; when exceeded, the
///   oldest frames are discarded (`overflow_dropped`) and playout skips ahead.
#[derive(Debug)]
pub struct JitterBuffer {
    min_delay_ms: u32,
    max_delay_ms: u32,
}

impl JitterBuffer {
    /// Panics if `min_delay_ms > max_delay_ms`.
    pub fn new(min_delay_ms: u32, max_delay_ms: u32) -> Self {
        assert!(min_delay_ms <= max_delay_ms);
        let _ = (min_delay_ms, max_delay_ms);
        todo!()
    }

    /// Stores a received frame. `timestamp` is in samples at 48 kHz (960 per frame).
    pub fn push(&mut self, seq: u16, timestamp: u32, payload: Vec<u8>, arrival_ms: u64) {
        let _ = (seq, timestamp, payload, arrival_ms, self.min_delay_ms, self.max_delay_ms);
        todo!()
    }

    /// Playout tick (see type-level contract).
    pub fn pop(&mut self, now_ms: u64) -> Pop {
        let _ = now_ms;
        todo!()
    }

    /// Current adaptive target delay in milliseconds.
    pub fn target_delay_ms(&self) -> u32 {
        todo!()
    }

    /// Number of frames currently buffered.
    pub fn len(&self) -> usize {
        todo!()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> JitterStats {
        todo!()
    }
}

impl Default for JitterBuffer {
    /// 40..=120 ms.
    fn default() -> Self {
        Self::new(40, 120)
    }
}
