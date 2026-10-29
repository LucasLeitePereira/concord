//! Frame fragmentation and reassembly over unreliable datagrams (`video-fragmentation`).
//!
//! Pure logic, no FFmpeg and no clock: time is injected as milliseconds so tests are
//! deterministic. The sender splits each encoded frame into datagram-sized fragments; the
//! receiver reassembles them (any order), drops frames that lost a fragment and asks for a
//! keyframe at most once every [`KEYFRAME_REQUEST_INTERVAL_MS`].

use std::collections::BTreeMap;

use sala_protocol::MediaDatagram;

/// Minimum spacing between two keyframe requests (plan section 5.5).
pub const KEYFRAME_REQUEST_INTERVAL_MS: u64 = 500;
/// An incomplete frame older than this is dropped.
pub const FRAME_TIMEOUT_MS: u64 = 250;
/// Sanity cap on fragments per frame (a 4096 * ~1.2 KB frame is ~5 MB).
pub const MAX_FRAGMENTS: u16 = 4096;
/// Frames being reassembled at the same time.
const MAX_PARTIAL: usize = 8;

/// Splits one encoded frame into `Video` datagrams whose payload is at most `max_payload`
/// bytes (use `sala_protocol::max_video_payload`). An empty frame or `max_payload == 0`
/// yields nothing; a frame needing more than [`MAX_FRAGMENTS`] fragments is refused too.
pub fn fragment(frame_id: u32, keyframe: bool, timestamp: u32, data: &[u8], max_payload: usize) -> Vec<MediaDatagram> {
    fragment_fec(frame_id, keyframe, timestamp, data, max_payload, 0)
}

/// Default XOR FEC group size: one parity fragment per 8 data fragments (12.5% overhead).
pub const DEFAULT_FEC_GROUP: u8 = 8;
/// Parity payload = `u16` length of the group's last data fragment + XOR of the group.
const PARITY_HEADER: usize = 2;

/// Like [`fragment`], plus one XOR parity fragment after every `fec_group` data fragments
/// (`0` = no FEC). Parity fragments follow all data fragments (`frag_idx >= n_data`) and
/// `frag_count` counts both. With FEC the data chunks are 2 bytes smaller so the parity
/// payload still fits in `max_payload`. The receiver recovers one loss per group.
pub fn fragment_fec(
    frame_id: u32,
    keyframe: bool,
    timestamp: u32,
    data: &[u8],
    max_payload: usize,
    fec_group: u8,
) -> Vec<MediaDatagram> {
    let chunk_size = if fec_group == 0 { max_payload } else { max_payload.saturating_sub(PARITY_HEADER) };
    if data.is_empty() || chunk_size == 0 {
        return Vec::new();
    }
    let chunks: Vec<&[u8]> = data.chunks(chunk_size).collect();
    let n = chunks.len();
    let k = fec_group as usize;
    let total = if k == 0 { n } else { n + n.div_ceil(k) };
    if total > MAX_FRAGMENTS as usize {
        return Vec::new();
    }
    let make = |idx: usize, payload: Vec<u8>| MediaDatagram::Video {
        frame_id,
        frag_idx: idx as u16,
        frag_count: total as u16,
        keyframe,
        fec_group,
        timestamp,
        payload,
    };
    let mut out: Vec<MediaDatagram> = chunks.iter().enumerate().map(|(i, c)| make(i, c.to_vec())).collect();
    if k > 0 {
        for (g, group) in chunks.chunks(k).enumerate() {
            let mut payload = vec![0u8; PARITY_HEADER + group[0].len()];
            let last_len = group[group.len() - 1].len().min(u16::MAX as usize) as u16;
            payload[..PARITY_HEADER].copy_from_slice(&last_len.to_be_bytes());
            for c in group {
                xor_into(&mut payload[PARITY_HEADER..], c);
            }
            out.push(make(n + g, payload));
        }
    }
    out
}

fn xor_into(acc: &mut [u8], other: &[u8]) {
    for (a, b) in acc.iter_mut().zip(other) {
        *a ^= b;
    }
}

/// Number of data fragments of a frame with `total` fragments and group size `k` (> 0);
/// `None` when no layout matches (corrupt header).
pub fn data_count(total: usize, k: usize) -> Option<usize> {
    let guess = total * k / (k + 1);
    (guess.saturating_sub(1)..=guess + 2).find(|&n| n >= 1 && n + n.div_ceil(k) == total)
}

/// A fully reassembled encoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub frame_id: u32,
    pub keyframe: bool,
    pub timestamp: u32,
    pub data: Vec<u8>,
}

/// What a call to the reassembler produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// A frame that is ready to decode.
    pub frame: Option<Frame>,
    /// The caller should send a `KeyframeRequest` (already rate limited).
    pub request_keyframe: bool,
}

struct Partial {
    frag_count: u16,
    fec_group: u8,
    keyframe: bool,
    timestamp: u32,
    first_seen_ms: u64,
    /// Data fragments held (received or recovered).
    received: usize,
    parts: Vec<Option<Vec<u8>>>,
    /// One parity payload per FEC group (empty without FEC).
    parity: Vec<Option<Vec<u8>>>,
}

impl Partial {
    /// Rebuilds a data fragment from a group's parity when exactly one is missing.
    fn recover(&mut self) {
        let k = self.fec_group as usize;
        if k == 0 {
            return;
        }
        let n = self.parts.len();
        for g in 0..self.parity.len() {
            let (start, end) = (g * k, ((g + 1) * k).min(n));
            let Some(parity) = &self.parity[g] else { continue };
            let mut missing = (start..end).filter(|&i| self.parts[i].is_none());
            let (Some(m), None) = (missing.next(), missing.next()) else { continue };
            let body = &parity[PARITY_HEADER..];
            let last_len = u16::from_be_bytes([parity[0], parity[1]]) as usize;
            let len = if m == end - 1 { last_len } else { body.len() };
            if len > body.len() {
                continue; // corrupt parity
            }
            let mut buf = body.to_vec();
            for i in (start..end).filter(|&i| i != m) {
                if let Some(c) = &self.parts[i] {
                    xor_into(&mut buf, c);
                }
            }
            buf.truncate(len);
            self.parts[m] = Some(buf);
            self.received += 1;
        }
    }
}

/// `a` is newer than `b` (wrapping `u32` ids).
fn newer(a: u32, b: u32) -> bool {
    a != b && (a.wrapping_sub(b) as i32) > 0
}

/// Reassembles frames from fragments that may arrive out of order, duplicated or not at all.
#[derive(Default)]
pub struct Reassembler {
    partial: BTreeMap<u32, Partial>,
    last_delivered: Option<u32>,
    /// A loss happened (or nothing was received yet): deltas are useless until a keyframe.
    awaiting_keyframe: bool,
    last_request_ms: Option<u64>,
}

impl Reassembler {
    pub fn new() -> Self {
        Reassembler { awaiting_keyframe: true, ..Default::default() }
    }

    /// Feeds one `Video` datagram. Other datagram kinds are ignored.
    pub fn push(&mut self, now_ms: u64, dg: &MediaDatagram) -> Output {
        let MediaDatagram::Video { frame_id, frag_idx, frag_count, keyframe, fec_group, timestamp, payload } = dg else {
            return Output::default();
        };
        let (frame_id, frag_idx, frag_count, fec_group) = (*frame_id, *frag_idx, *frag_count, *fec_group);
        let mut lost = self.expire_inner(now_ms);
        if frag_count == 0 || frag_count > MAX_FRAGMENTS || frag_idx >= frag_count {
            return self.finish(now_ms, None, lost);
        }
        let n_data = if fec_group == 0 {
            Some(frag_count as usize)
        } else {
            data_count(frag_count as usize, fec_group as usize)
        };
        let Some(n_data) = n_data else {
            return self.finish(now_ms, None, lost);
        };
        let is_parity = frag_idx as usize >= n_data;
        if is_parity && payload.len() < PARITY_HEADER {
            return self.finish(now_ms, None, lost);
        }
        // Late frame (already delivered or older): ignore.
        if let Some(last) = self.last_delivered
            && !newer(frame_id, last)
        {
            return self.finish(now_ms, None, lost);
        }
        // A newer frame started: older incomplete ones will never complete.
        if !self.partial.contains_key(&frame_id) {
            let older: Vec<u32> = self.partial.keys().copied().filter(|id| newer(frame_id, *id)).collect();
            for id in older {
                self.partial.remove(&id);
                lost = true;
            }
            if self.partial.keys().any(|id| newer(*id, frame_id)) {
                // We already hold a newer frame; this one is stale.
                return self.finish(now_ms, None, lost);
            }
            if self.partial.len() >= MAX_PARTIAL {
                return self.finish(now_ms, None, lost);
            }
        }
        let p = self.partial.entry(frame_id).or_insert_with(|| Partial {
            frag_count,
            fec_group,
            keyframe: *keyframe,
            timestamp: *timestamp,
            first_seen_ms: now_ms,
            received: 0,
            parts: vec![None; n_data],
            parity: vec![None; frag_count as usize - n_data],
        });
        let inconsistent = p.frag_count != frag_count || p.fec_group != fec_group;
        let slot = if is_parity { p.parity.get_mut(frag_idx as usize - n_data) } else { p.parts.get_mut(frag_idx as usize) };
        let Some(slot) = slot.filter(|s| !inconsistent && s.is_none()) else {
            // Inconsistent layout or duplicate fragment.
            return self.finish(now_ms, None, lost);
        };
        *slot = Some(payload.clone());
        if !is_parity {
            p.received += 1;
        }
        p.recover();
        if p.received < p.parts.len() {
            return self.finish(now_ms, None, lost);
        }
        let p = self.partial.remove(&frame_id).expect("present");
        // Anything older than the completed frame is gone for good.
        let stale: Vec<u32> = self.partial.keys().copied().filter(|id| newer(frame_id, *id)).collect();
        for id in stale {
            self.partial.remove(&id);
            lost = true;
        }
        let gap = self.last_delivered.is_some_and(|last| frame_id != last.wrapping_add(1));
        self.last_delivered = Some(frame_id);
        if gap {
            lost = true;
        }
        let data: Vec<u8> = p.parts.into_iter().flatten().flatten().collect();
        let frame = Frame { frame_id, keyframe: p.keyframe, timestamp: p.timestamp, data };
        self.finish(now_ms, Some(frame), lost)
    }

    /// Drops incomplete frames older than [`FRAME_TIMEOUT_MS`]; call it periodically so that
    /// a stalled stream still asks for a keyframe.
    pub fn expire(&mut self, now_ms: u64) -> Output {
        let lost = self.expire_inner(now_ms);
        self.finish(now_ms, None, lost)
    }

    fn expire_inner(&mut self, now_ms: u64) -> bool {
        let before = self.partial.len();
        self.partial.retain(|_, p| now_ms.saturating_sub(p.first_seen_ms) < FRAME_TIMEOUT_MS);
        self.partial.len() != before
    }

    /// Applies the keyframe policy to a completed frame and decides on the request.
    fn finish(&mut self, now_ms: u64, frame: Option<Frame>, lost: bool) -> Output {
        if lost {
            self.awaiting_keyframe = true;
        }
        let mut frame = frame;
        let mut dropped_delta = false;
        if let Some(f) = &frame {
            if f.keyframe {
                self.awaiting_keyframe = false;
            } else if self.awaiting_keyframe {
                frame = None; // undecodable delta
                dropped_delta = true;
            }
        }
        let mut request = false;
        if self.awaiting_keyframe && (lost || dropped_delta) {
            let due = self.last_request_ms.is_none_or(|t| now_ms.saturating_sub(t) >= KEYFRAME_REQUEST_INTERVAL_MS);
            if due {
                self.last_request_ms = Some(now_ms);
                request = true;
            }
        }
        Output { frame, request_keyframe: request }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    fn push_all(r: &mut Reassembler, now: u64, dgs: &[MediaDatagram]) -> Vec<Output> {
        dgs.iter().map(|d| r.push(now, d)).collect()
    }

    fn frames(outs: &[Output]) -> Vec<&Frame> {
        outs.iter().filter_map(|o| o.frame.as_ref()).collect()
    }

    #[test]
    fn single_fragment_frame() {
        let d = data(100);
        let f = fragment(1, true, 90, &d, 1000);
        assert_eq!(f.len(), 1);
        let mut r = Reassembler::new();
        let out = r.push(0, &f[0]);
        let fr = out.frame.unwrap();
        assert_eq!((fr.frame_id, fr.keyframe, fr.timestamp), (1, true, 90));
        assert_eq!(fr.data, d);
        assert!(!out.request_keyframe);
    }

    #[test]
    fn fragment_edges() {
        assert!(fragment(1, true, 0, &[], 100).is_empty());
        assert!(fragment(1, true, 0, &[1], 0).is_empty());
        let f = fragment(1, true, 0, &data(300), 100);
        assert_eq!(f.len(), 3);
        // Exact multiple does not add an empty fragment.
        assert_eq!(fragment(1, true, 0, &data(200), 100).len(), 2);
        // Too many fragments are refused.
        assert!(fragment(1, true, 0, &data(MAX_FRAGMENTS as usize + 1), 1).is_empty());
    }

    #[test]
    fn in_order_and_large_frame() {
        let d = data(2_000_000);
        let f = fragment(7, true, 5, &d, 1100);
        assert!(f.len() > 1000);
        let mut r = Reassembler::new();
        let outs = push_all(&mut r, 0, &f);
        let got = frames(&outs);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, d);
        assert!(outs.iter().all(|o| !o.request_keyframe));
    }

    #[test]
    fn out_of_order() {
        let d = data(5000);
        let mut f = fragment(3, true, 0, &d, 1000);
        f.reverse();
        f.swap(1, 3);
        let mut r = Reassembler::new();
        let outs = push_all(&mut r, 0, &f);
        let got = frames(&outs);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, d);
    }

    #[test]
    fn duplicate_and_invalid_are_ignored() {
        let d = data(3000);
        let f = fragment(1, true, 0, &d, 1000);
        let mut r = Reassembler::new();
        assert!(r.push(0, &f[0]).frame.is_none());
        assert!(r.push(0, &f[0]).frame.is_none()); // duplicate does not count twice
        // Invalid indices.
        let bad = |idx, count| MediaDatagram::Video {
            frame_id: 1,
            frag_idx: idx,
            frag_count: count,
            keyframe: true,
            fec_group: 0,
            timestamp: 0,
            payload: vec![0; 10],
        };
        assert!(r.push(0, &bad(5, 3)).frame.is_none());
        assert!(r.push(0, &bad(0, 0)).frame.is_none());
        assert!(r.push(0, &bad(0, MAX_FRAGMENTS + 1)).frame.is_none());
        assert!(r.push(0, &bad(1, 7)).frame.is_none()); // count differs from the first fragment
        assert!(r.push(0, &f[1]).frame.is_none());
        let out = r.push(0, &f[2]);
        assert_eq!(out.frame.unwrap().data, d);
        // The delivered frame arriving again is ignored.
        assert!(r.push(1, &f[0]).frame.is_none());
    }

    #[test]
    fn loss_drops_frame_and_requests_keyframe_once_per_interval() {
        let mut r = Reassembler::new();
        // Frame 1 (keyframe) fine.
        let f1 = fragment(1, true, 0, &data(2500), 1000);
        assert!(frames(&push_all(&mut r, 0, &f1)).len() == 1);
        // Frame 2 loses fragment 1; frame 3 (delta) starts -> frame 2 dropped, keyframe requested.
        let f2 = fragment(2, false, 1, &data(2500), 1000);
        r.push(10, &f2[0]);
        r.push(10, &f2[2]);
        let f3 = fragment(3, false, 2, &data(2500), 1000);
        let out = r.push(20, &f3[0]);
        assert!(out.request_keyframe);
        // The rest of frame 3 completes but is an undecodable delta: dropped.
        let o = push_all(&mut r, 21, &f3[1..]);
        assert!(frames(&o).is_empty());
        // Rate limit: another loss within 500 ms does not request again.
        let f4 = fragment(4, false, 3, &data(2500), 1000);
        let f5 = fragment(5, false, 4, &data(2500), 1000);
        r.push(100, &f4[0]);
        let out = r.push(110, &f5[0]);
        assert!(!out.request_keyframe);
        // After the interval it asks again.
        let f6 = fragment(6, false, 5, &data(2500), 1000);
        let out = r.push(20 + KEYFRAME_REQUEST_INTERVAL_MS, &f6[0]);
        assert!(out.request_keyframe);
        // A keyframe resumes delivery.
        let k = fragment(7, true, 6, &data(100), 1000);
        let out = r.push(2000, &k[0]);
        assert!(out.frame.is_some());
        let d8 = fragment(8, false, 7, &data(100), 1000);
        assert!(r.push(2001, &d8[0]).frame.is_some());
    }

    #[test]
    fn timeout_expires_incomplete_frame() {
        let mut r = Reassembler::new();
        let f1 = fragment(1, true, 0, &data(100), 1000);
        r.push(0, &f1[0]);
        let f2 = fragment(2, false, 1, &data(3000), 1000);
        r.push(10, &f2[0]);
        assert_eq!(r.expire(10 + FRAME_TIMEOUT_MS - 1), Output::default());
        let out = r.expire(10 + FRAME_TIMEOUT_MS);
        assert!(out.request_keyframe);
        // Late remaining fragments of the expired frame never complete it.
        assert!(r.push(400, &f2[1]).frame.is_none());
        assert!(r.push(400, &f2[2]).frame.is_none());
    }

    #[test]
    fn whole_frame_missing_is_detected() {
        let mut r = Reassembler::new();
        r.push(0, &fragment(1, true, 0, &data(10), 1000)[0]);
        // Frame 2 never arrives at all.
        let out = r.push(5, &fragment(3, false, 2, &data(10), 1000)[0]);
        assert!(out.frame.is_none());
        assert!(out.request_keyframe);
    }

    #[test]
    fn frame_id_wraps() {
        let mut r = Reassembler::new();
        let a = r.push(0, &fragment(u32::MAX, true, 0, &data(10), 1000)[0]);
        assert!(a.frame.is_some());
        let b = r.push(1, &fragment(0, false, 1, &data(10), 1000)[0]);
        assert!(b.frame.is_some());
    }

    fn video_idx(d: &MediaDatagram) -> u16 {
        match d {
            MediaDatagram::Video { frag_idx, .. } => *frag_idx,
            _ => unreachable!(),
        }
    }

    /// Feeds `dgs` minus the fragments whose index is in `lost`.
    fn push_without(r: &mut Reassembler, now: u64, dgs: &[MediaDatagram], lost: &[u16]) -> Vec<Output> {
        dgs.iter().filter(|d| !lost.contains(&video_idx(d))).map(|d| r.push(now, d)).collect()
    }

    #[test]
    fn fec_layout_and_data_count() {
        // 20 data chunks of 98 bytes (+2 header = 100) -> 3 groups -> 3 parity.
        let d = data(98 * 19 + 40);
        let f = fragment_fec(1, true, 0, &d, 100, 8);
        assert_eq!(f.len(), 23);
        for (i, dg) in f.iter().enumerate() {
            let MediaDatagram::Video { frag_idx, frag_count, fec_group, payload, .. } = dg else { unreachable!() };
            assert_eq!((*frag_idx as usize, *frag_count, *fec_group), (i, 23, 8));
            assert!(payload.len() <= 100);
        }
        for k in 1..=16usize {
            for n in 1..=300usize {
                assert_eq!(data_count(n + n.div_ceil(k), k), Some(n), "n={n} k={k}");
            }
        }
        // fec_group 0 keeps the legacy layout.
        assert_eq!(fragment_fec(1, true, 0, &data(300), 100, 0), fragment(1, true, 0, &data(300), 100));
    }

    #[test]
    fn fec_recovers_one_loss_per_group() {
        let d = data(98 * 19 + 40); // last data fragment is short (40 bytes)
        let f = fragment_fec(1, true, 0, &d, 100, 8);
        // One data loss in each group, the last one being the short final fragment.
        for lost in [vec![3u16, 10, 19], vec![0, 8, 16], vec![7, 15, 18]] {
            let mut r = Reassembler::new();
            let outs = push_without(&mut r, 0, &f, &lost);
            let got = frames(&outs);
            assert_eq!(got.len(), 1, "lost {lost:?}");
            assert_eq!(got[0].data, d, "lost {lost:?}");
            assert!(outs.iter().all(|o| !o.request_keyframe));
        }
        // A lost parity fragment costs nothing.
        let mut r = Reassembler::new();
        let outs = push_without(&mut r, 0, &f, &[20, 21, 22]);
        assert_eq!(frames(&outs)[0].data, d);
        // Order does not matter: parity first, then the data in reverse.
        let mut shuffled = f.clone();
        shuffled.reverse();
        let mut r = Reassembler::new();
        let outs = push_without(&mut r, 0, &shuffled, &[5]);
        assert_eq!(frames(&outs)[0].data, d);
    }

    #[test]
    fn fec_two_losses_in_one_group_drop_the_frame_and_request_keyframe() {
        let d = data(98 * 19 + 40);
        let f = fragment_fec(1, true, 0, &d, 100, 8);
        let mut r = Reassembler::new();
        let outs = push_without(&mut r, 0, &f, &[2, 5]);
        assert!(frames(&outs).is_empty());
        // The next frame shows the gap and asks for a keyframe, as without FEC.
        let next = fragment_fec(2, false, 1, &data(50), 100, 0);
        let out = r.push(10, &next[0]);
        assert!(out.frame.is_none());
        assert!(out.request_keyframe);
        // Control: the same single loss without FEC drops the frame.
        let plain = fragment(1, true, 0, &d, 100);
        let mut r = Reassembler::new();
        assert!(frames(&push_without(&mut r, 0, &plain, &[3])).is_empty());
    }

    #[test]
    fn fec_ignores_corrupt_layout() {
        let mut r = Reassembler::new();
        let bad = |idx: u16, count: u16, k: u8, len: usize| MediaDatagram::Video {
            frame_id: 1,
            frag_idx: idx,
            frag_count: count,
            keyframe: true,
            fec_group: k,
            timestamp: 0,
            payload: vec![0; len],
        };
        // frag_count 10 with k=8 has no valid (data, parity) split (9 data -> 11 total).
        assert!(r.push(0, &bad(0, 10, 8, 10)).frame.is_none());
        // Parity shorter than its header.
        assert!(r.push(0, &bad(1, 2, 1, 1)).frame.is_none());
        // Parity claiming a longer last fragment than its body: never panics.
        let mut p = vec![0u8; 12];
        p[..2].copy_from_slice(&500u16.to_be_bytes());
        let mut bogus = bad(2, 3, 2, 0);
        if let MediaDatagram::Video { payload, .. } = &mut bogus {
            *payload = p;
        }
        assert!(r.push(0, &bad(0, 3, 2, 10)).frame.is_none());
        assert!(r.push(0, &bogus).frame.is_none());
    }

    #[test]
    fn fec_saves_most_keyframes_under_two_percent_loss() {
        let d = data(60_000); // ~55 fragments at 1100 B
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut survive = [0u32; 2];
        for (slot, k) in [0u8, 8].into_iter().enumerate() {
            for id in 1..=200u32 {
                let mut r = Reassembler::new();
                let mut ok = false;
                for dg in fragment_fec(id, true, 0, &d, 1100, k) {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    if rng % 100 < 2 {
                        continue;
                    }
                    ok |= r.push(0, &dg).frame.is_some();
                }
                survive[slot] += u32::from(ok);
            }
        }
        // Without FEC ~ (0.98^55) = 33% survive; with FEC it should be far higher.
        assert!(survive[1] > survive[0] + 60, "no fec {} / fec {}", survive[0], survive[1]);
        assert!(survive[1] >= 150, "fec only saved {}", survive[1]);
    }

    #[test]
    fn late_joiner_waits_for_keyframe() {
        let mut r = Reassembler::new();
        let out = r.push(0, &fragment(40, false, 0, &data(10), 1000)[0]);
        assert!(out.frame.is_none());
        assert!(out.request_keyframe);
    }
}
