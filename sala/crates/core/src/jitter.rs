//! Adaptive jitter buffer for incoming voice frames.
//!
//! API skeleton fixed by the architect. Implement the `todo!()` bodies; do not
//! change public signatures, type names or variants. Private fields may change.

use std::collections::BTreeMap;

/// Duration of one voice frame.
pub const FRAME_MS: u32 = 20;

/// Samples per millisecond at 48 kHz (used to convert RTP-style timestamps).
const SAMPLES_PER_MS: i64 = 48;

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
///
/// Implementation notes: sequence numbers are unwrapped into a monotonic `i64`
/// ("extended" sequence). When the buffer runs dry after playout started
/// (underrun, e.g. DTX silence or a network stall) it re-buffers: `pop` returns
/// `Empty` without advancing and playout resumes once the target delay has
/// elapsed since the next arrival. Delay growth therefore happens at underruns;
/// shrinking happens only through the overflow cap.
#[derive(Debug)]
pub struct JitterBuffer {
    min_delay_ms: u32,
    max_delay_ms: u32,
    /// Buffered frames keyed by extended sequence number.
    frames: BTreeMap<i64, Vec<u8>>,
    /// Highest extended sequence seen (reference for unwrapping).
    highest: Option<i64>,
    /// Extended sequence of the next slot to play (valid once a start happened).
    next_play: i64,
    /// Whether playout is currently running.
    playing: bool,
    /// Whether playout ever started (so `next_play` is meaningful).
    ever_started: bool,
    /// Arrival time anchoring the current buffering phase.
    anchor_ms: Option<u64>,
    /// Previous (arrival_ms, timestamp) for the jitter estimator.
    prev: Option<(u64, u32)>,
    /// Smoothed interarrival jitter in ms (RFC 3550 style, gain 1/16).
    jitter_ms: f64,
    stats: JitterStats,
}

impl JitterBuffer {
    /// Panics if `min_delay_ms > max_delay_ms`.
    pub fn new(min_delay_ms: u32, max_delay_ms: u32) -> Self {
        assert!(min_delay_ms <= max_delay_ms);
        Self {
            min_delay_ms,
            max_delay_ms,
            frames: BTreeMap::new(),
            highest: None,
            next_play: 0,
            playing: false,
            ever_started: false,
            anchor_ms: None,
            prev: None,
            jitter_ms: 0.0,
            stats: JitterStats::default(),
        }
    }

    fn max_frames(&self) -> usize {
        (self.max_delay_ms / FRAME_MS) as usize + 1
    }

    /// Unwraps a `u16` sequence number relative to the highest one seen.
    fn extend(&self, seq: u16) -> i64 {
        match self.highest {
            None => (1 << 20) + i64::from(seq),
            Some(hi) => {
                let diff = seq.wrapping_sub(hi as u16) as i16;
                hi + i64::from(diff)
            }
        }
    }

    /// Stores a received frame. `timestamp` is in samples at 48 kHz (960 per frame).
    pub fn push(&mut self, seq: u16, timestamp: u32, payload: Vec<u8>, arrival_ms: u64) {
        self.stats.received += 1;
        let ext = self.extend(seq);

        if self.ever_started && ext < self.next_play {
            self.stats.late_dropped += 1;
            return;
        }
        if self.frames.contains_key(&ext) {
            self.stats.duplicates += 1;
            return;
        }

        // Jitter estimate: deviation between arrival spacing and timestamp spacing.
        if let Some((prev_arrival, prev_ts)) = self.prev {
            let arrival_delta = arrival_ms as i64 - prev_arrival as i64;
            let ts_delta = i64::from(timestamp.wrapping_sub(prev_ts) as i32) / SAMPLES_PER_MS;
            let d = (arrival_delta - ts_delta).abs() as f64;
            self.jitter_ms += (d - self.jitter_ms) / 16.0;
        }
        self.prev = Some((arrival_ms, timestamp));

        if self.anchor_ms.is_none() {
            self.anchor_ms = Some(arrival_ms);
        }
        if self.highest.is_none_or(|h| ext > h) {
            self.highest = Some(ext);
        }
        self.frames.insert(ext, payload);

        let mut overflowed = false;
        while self.frames.len() > self.max_frames() {
            self.frames.pop_first();
            self.stats.overflow_dropped += 1;
            overflowed = true;
        }
        if overflowed
            && self.playing
            && let Some((&first, _)) = self.frames.first_key_value()
        {
            self.next_play = self.next_play.max(first);
        }
    }

    /// Playout tick (see type-level contract).
    pub fn pop(&mut self, now_ms: u64) -> Pop {
        if !self.playing {
            let Some((&first, _)) = self.frames.first_key_value() else {
                return Pop::Empty;
            };
            let Some(anchor) = self.anchor_ms else {
                return Pop::Empty;
            };
            if now_ms < anchor + u64::from(self.target_delay_ms()) {
                return Pop::Empty;
            }
            self.playing = true;
            // First start: lowest buffered. Restart after underrun: resume at the
            // next expected slot unless it is hopelessly behind.
            if !self.ever_started || first - self.next_play > self.max_frames() as i64 {
                self.next_play = first;
            }
            self.ever_started = true;
        }

        if self.frames.is_empty() {
            // Underrun: re-buffer without advancing.
            self.playing = false;
            self.anchor_ms = None;
            return Pop::Empty;
        }

        let slot = self.next_play;
        self.next_play += 1;
        match self.frames.remove(&slot) {
            Some(payload) => {
                self.stats.played += 1;
                Pop::Frame(payload)
            }
            None => {
                self.stats.lost += 1;
                Pop::Lost
            }
        }
    }

    /// Current adaptive target delay in milliseconds.
    pub fn target_delay_ms(&self) -> u32 {
        let extra = (2.0 * self.jitter_ms).round() as u32;
        self.min_delay_ms.saturating_add(extra).clamp(self.min_delay_ms, self.max_delay_ms)
    }

    /// Number of frames currently buffered.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }
}

impl Default for JitterBuffer {
    /// 40..=120 ms.
    fn default() -> Self {
        Self::new(40, 120)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(seq: u16) -> Vec<u8> {
        vec![(seq & 0xff) as u8, (seq >> 8) as u8]
    }
    fn ts(seq: u16) -> u32 {
        // Continuous across the u16 wrap, as a real sender's timestamps are.
        u32::from(seq.wrapping_sub(65400)).wrapping_mul(960)
    }

    /// Pushes `seqs[i]` at `t0 + arrivals`, ticks every 20 ms; returns pops.
    fn run(jb: &mut JitterBuffer, sched: &[(u64, u16)], ticks: std::ops::Range<u64>) -> Vec<Pop> {
        let mut out = Vec::new();
        let mut i = 0;
        for t in ticks {
            let now = t * 20;
            while i < sched.len() && sched[i].0 <= now {
                jb.push(sched[i].1, ts(sched[i].1), pl(sched[i].1), sched[i].0);
                i += 1;
            }
            out.push(jb.pop(now));
        }
        out
    }

    fn frames(p: &[Pop]) -> Vec<Vec<u8>> {
        p.iter().filter_map(|x| if let Pop::Frame(f) = x { Some(f.clone()) } else { None }).collect()
    }

    #[test]
    fn regular_stream_plays_in_order_after_min_delay() {
        let mut jb = JitterBuffer::default();
        let sched: Vec<_> = (0..10u16).map(|s| (u64::from(s) * 20, s)).collect();
        let out = run(&mut jb, &sched, 0..12);
        assert_eq!(jb.target_delay_ms(), 40);
        assert_eq!(out[0], Pop::Empty);
        assert_eq!(out[1], Pop::Empty);
        assert_eq!(out[2], Pop::Frame(pl(0)));
        assert_eq!(frames(&out), (0..10).map(pl).collect::<Vec<_>>());
        let s = jb.stats();
        assert_eq!((s.received, s.played, s.lost), (10, 10, 0));
    }

    #[test]
    fn empty_before_target_delay_negative_control() {
        let mut jb = JitterBuffer::new(40, 120);
        jb.push(0, 0, pl(0), 100);
        assert_eq!(jb.pop(139), Pop::Empty);
        assert_eq!(jb.pop(140), Pop::Frame(pl(0)));
    }

    #[test]
    fn loss_yields_lost_slot() {
        let mut jb = JitterBuffer::default();
        let sched: Vec<_> = (0..8u16).filter(|s| *s != 3).map(|s| (u64::from(s) * 20, s)).collect();
        let out = run(&mut jb, &sched, 0..10);
        assert_eq!(out[2 + 3], Pop::Lost);
        assert_eq!(frames(&out).len(), 7);
        assert_eq!(jb.stats().lost, 1);
        // Control: without loss there is no Lost.
        let mut jb2 = JitterBuffer::default();
        let all: Vec<_> = (0..8u16).map(|s| (u64::from(s) * 20, s)).collect();
        assert!(!run(&mut jb2, &all, 0..10).contains(&Pop::Lost));
    }

    #[test]
    fn reordering_is_fixed() {
        let mut jb = JitterBuffer::default();
        // 1 arrives after 2.
        let sched = [(0, 0), (20, 2), (30, 1), (60, 3), (80, 4), (100, 5)];
        let arrival_order: Vec<u16> = sched.iter().map(|s| s.1).collect();
        assert_ne!(arrival_order, vec![0, 1, 2, 3, 4, 5]); // control: input is disordered
        let out = run(&mut jb, &sched, 0..14);
        assert_eq!(frames(&out), (0..6).map(pl).collect::<Vec<_>>());
        assert_eq!(jb.stats().lost, 0);
    }

    #[test]
    fn late_frame_after_its_slot_is_dropped() {
        let mut jb = JitterBuffer::default();
        let mut sched: Vec<_> = (0..8u16).filter(|s| *s != 3).map(|s| (u64::from(s) * 20, s)).collect();
        sched.push((500, 3)); // far too late
        sched.sort();
        let out = run(&mut jb, &sched, 0..30);
        assert!(out.contains(&Pop::Lost));
        assert!(!frames(&out).contains(&pl(3)));
        assert_eq!(jb.stats().late_dropped, 1);
    }

    #[test]
    fn duplicates_are_ignored() {
        let mut jb = JitterBuffer::default();
        jb.push(5, ts(5), pl(5), 0);
        jb.push(5, ts(5), pl(5), 1);
        jb.push(6, ts(6), pl(6), 20);
        assert_eq!(jb.len(), 2);
        assert_eq!(jb.stats().duplicates, 1);
        assert_eq!(jb.pop(40), Pop::Frame(pl(5)));
        assert_eq!(jb.pop(60), Pop::Frame(pl(6)));
        // Duplicate of an already played frame counts as late.
        jb.push(5, ts(5), pl(5), 61);
        assert_eq!(jb.stats().late_dropped, 1);
    }

    #[test]
    fn sequence_wraparound() {
        let mut jb = JitterBuffer::default();
        let seqs: Vec<u16> = (0..8u16).map(|i| 65530u16.wrapping_add(i)).collect();
        let sched: Vec<_> = seqs.iter().enumerate().map(|(i, s)| (i as u64 * 20, *s)).collect();
        let out = run(&mut jb, &sched, 0..10);
        assert_eq!(frames(&out), seqs.iter().map(|s| pl(*s)).collect::<Vec<_>>());
        assert_eq!(jb.stats().lost, 0);
    }

    #[test]
    fn wraparound_with_reordering_across_boundary() {
        let mut jb = JitterBuffer::default();
        let sched = [(0, 65534), (20, 0), (30, 65535), (60, 1), (80, 2), (100, 3)];
        let out = run(&mut jb, &sched, 0..14);
        assert_eq!(frames(&out), [65534u16, 65535, 0, 1, 2, 3].iter().map(|s| pl(*s)).collect::<Vec<_>>());
    }

    #[test]
    fn jitter_raises_target_and_is_bounded() {
        let mut jb = JitterBuffer::default();
        let mut t = 0u64;
        for s in 0..200u16 {
            // Alternate 0 / 60 ms of extra delay.
            let extra = if s % 2 == 0 { 0 } else { 60 };
            jb.push(s, ts(s), pl(s), u64::from(s) * 20 + extra);
            t += 1;
        }
        assert!(t > 0);
        let target = jb.target_delay_ms();
        assert!(target > 40, "target {target}");
        assert!(target <= 120);
        // Control: regular arrivals stay at min.
        let mut reg = JitterBuffer::default();
        for s in 0..200u16 {
            reg.push(s, ts(s), pl(s), u64::from(s) * 20);
        }
        assert_eq!(reg.target_delay_ms(), 40);
    }

    #[test]
    fn overflow_discards_oldest_and_skips_ahead() {
        let mut jb = JitterBuffer::new(40, 120); // max 7 frames
        for s in 0..20u16 {
            jb.push(s, ts(s), pl(s), 0);
        }
        assert_eq!(jb.len(), 7);
        assert_eq!(jb.stats().overflow_dropped, 13);
        assert_eq!(jb.pop(1000), Pop::Frame(pl(13)));
    }

    #[test]
    fn underrun_rebuffers_and_late_frame_still_plays() {
        let mut jb = JitterBuffer::default();
        jb.push(0, ts(0), pl(0), 0);
        jb.push(1, ts(1), pl(1), 20);
        assert_eq!(jb.pop(40), Pop::Frame(pl(0)));
        assert_eq!(jb.pop(60), Pop::Frame(pl(1)));
        assert_eq!(jb.pop(80), Pop::Empty); // dry
        jb.push(2, ts(2), pl(2), 90);
        assert_eq!(jb.pop(100), Pop::Empty); // re-buffering
        assert_eq!(jb.pop(140), Pop::Frame(pl(2)));
    }

    #[test]
    fn simulated_lossy_jittery_network_keeps_invariants() {
        let mut jb = JitterBuffer::default();
        let mut sched = Vec::new();
        let mut x = 12345u32;
        for s in 0..500u16 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            if (x >> 24).is_multiple_of(10) {
                continue; // ~4% loss
            }
            let delay = u64::from((x >> 16) % 50);
            sched.push((u64::from(s) * 20 + delay, 65400u16.wrapping_add(s)));
        }
        sched.sort();
        let out = run(&mut jb, &sched, 0..540);
        let s = jb.stats();
        assert_eq!(s.played as usize, frames(&out).len());
        assert!(s.played > 400);
        // Played frames are strictly increasing in (unwrapped) sequence.
        let seqs: Vec<u16> = frames(&out).iter().map(|f| u16::from(f[0]) | u16::from(f[1]) << 8).collect();
        for w in seqs.windows(2) {
            assert!((w[1].wrapping_sub(w[0]) as i16) > 0);
        }
        assert!(jb.len() <= 7);
    }

    #[test]
    #[should_panic]
    fn new_rejects_min_above_max() {
        let _ = JitterBuffer::new(100, 50);
    }
}
