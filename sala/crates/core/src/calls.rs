//! Rings in progress (D-024): who is ringing us and whom we are ringing. Pure state, no
//! I/O: the session feeds it with network events and local commands and acts on the answers.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::cues::Cue;

/// How long a caller keeps ringing before giving up (counted by the caller).
pub const RING_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a ring ended without the friend joining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEnd {
    Declined,
    Timeout,
    /// The caller gave up (seen by the receiver).
    Cancelled,
    /// The friend's Concord is too old to ring.
    Unsupported,
}

impl CallEnd {
    /// Value of `CoreToUi::CallResult::result`.
    pub fn as_str(self) -> &'static str {
        match self {
            CallEnd::Declined => "declined",
            CallEnd::Timeout => "timeout",
            CallEnd::Cancelled => "cancelled",
            CallEnd::Unsupported => "unsupported",
        }
    }
}

/// Incoming and outgoing rings, one per friend, each with the `call_id` that tells a stale
/// message (a `RingCancel` of an older ring) from the current one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CallBook {
    incoming: BTreeMap<String, u64>,
    outgoing: BTreeMap<String, u64>,
}

impl CallBook {
    /// `friend` rings us. False if we were already ringing from that same id.
    pub fn ring_in(&mut self, friend: &str, call_id: u64) -> bool {
        self.incoming.insert(friend.to_string(), call_id) != Some(call_id)
    }

    /// The caller gave up: true when it was the ring we had.
    pub fn cancel_in(&mut self, friend: &str, call_id: u64) -> bool {
        remove_if(&mut self.incoming, friend, call_id)
    }

    /// We answered or refused: true when there was a ring to answer.
    pub fn answer_in(&mut self, friend: &str) -> Option<u64> {
        self.incoming.remove(friend)
    }

    /// We ring `friend` with `call_id` (replaces an older ring of ours).
    pub fn ring_out(&mut self, friend: &str, call_id: u64) {
        self.outgoing.insert(friend.to_string(), call_id);
    }

    /// `friend` declined / the deadline passed: true when it was the current ring.
    pub fn end_out(&mut self, friend: &str, call_id: u64) -> bool {
        remove_if(&mut self.outgoing, friend, call_id)
    }

    /// `friend` joined the room (answer without a message) or went away: drops our ring.
    pub fn drop_out(&mut self, friend: &str) -> Option<u64> {
        self.outgoing.remove(friend)
    }

    /// We left the room or entered it: every ring we were making ends.
    pub fn take_all_out(&mut self) -> Vec<(String, u64)> {
        std::mem::take(&mut self.outgoing).into_iter().collect()
    }

    /// We entered the room: the incoming rings are answered.
    pub fn clear_in(&mut self) -> bool {
        let had = !self.incoming.is_empty();
        self.incoming.clear();
        had
    }

    /// A friend disconnected: forget both directions. True if anything changed.
    pub fn forget(&mut self, friend: &str) -> bool {
        let a = self.incoming.remove(friend).is_some();
        let b = self.outgoing.remove(friend).is_some();
        a || b
    }

    pub fn incoming(&self) -> Vec<String> {
        self.incoming.keys().cloned().collect()
    }

    pub fn outgoing(&self) -> Vec<String> {
        self.outgoing.keys().cloned().collect()
    }

    pub fn outgoing_id(&self, friend: &str) -> Option<u64> {
        self.outgoing.get(friend).copied()
    }

    pub fn incoming_id(&self, friend: &str) -> Option<u64> {
        self.incoming.get(friend).copied()
    }

    /// The loop that must be playing: an incoming ring wins over our own "calling".
    pub fn loop_cue(&self) -> Option<Cue> {
        if !self.incoming.is_empty() {
            Some(Cue::RingIncoming)
        } else if !self.outgoing.is_empty() {
            Some(Cue::RingOutgoing)
        } else {
            None
        }
    }
}

fn remove_if(map: &mut BTreeMap<String, u64>, friend: &str, call_id: u64) -> bool {
    if map.get(friend) == Some(&call_id) {
        map.remove(friend);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_ring_lifecycle_and_stale_cancel() {
        let mut b = CallBook::default();
        assert!(b.ring_in("ana", 5));
        assert!(!b.ring_in("ana", 5), "same id again is not news");
        assert_eq!(b.incoming(), ["ana"]);
        assert_eq!(b.loop_cue(), Some(Cue::RingIncoming));
        // A cancel for an older ring does not end the current one.
        assert!(!b.cancel_in("ana", 4));
        assert_eq!(b.incoming(), ["ana"]);
        assert!(b.cancel_in("ana", 5));
        assert!(b.incoming().is_empty());
        assert_eq!(b.loop_cue(), None);
        assert_eq!(b.answer_in("ana"), None);
        b.ring_in("ana", 9);
        assert_eq!(b.answer_in("ana"), Some(9));
    }

    #[test]
    fn outgoing_ring_ends_only_for_its_own_id_and_incoming_wins_the_loop() {
        let mut b = CallBook::default();
        b.ring_out("bia", 1);
        assert_eq!(b.loop_cue(), Some(Cue::RingOutgoing));
        assert!(!b.end_out("bia", 2), "a late answer to an older ring is ignored");
        assert_eq!(b.outgoing_id("bia"), Some(1));
        b.ring_in("ana", 3);
        assert_eq!(b.loop_cue(), Some(Cue::RingIncoming));
        assert!(b.end_out("bia", 1));
        assert_eq!(b.outgoing_id("bia"), None);
    }

    #[test]
    fn leaving_clears_everything_that_depends_on_it() {
        let mut b = CallBook::default();
        b.ring_out("bia", 1);
        b.ring_out("cai", 2);
        b.ring_in("ana", 3);
        assert_eq!(b.take_all_out(), vec![("bia".to_string(), 1), ("cai".to_string(), 2)]);
        assert!(b.outgoing().is_empty());
        assert!(b.clear_in());
        assert!(!b.clear_in());
        b.ring_in("ana", 3);
        b.ring_out("ana", 4);
        assert!(b.forget("ana"));
        assert!(!b.forget("ana"));
        assert_eq!((b.incoming(), b.outgoing()), (vec![], vec![]));
    }

    #[test]
    fn call_end_names_match_the_ipc_contract() {
        let names: Vec<&str> =
            [CallEnd::Declined, CallEnd::Timeout, CallEnd::Cancelled, CallEnd::Unsupported].map(CallEnd::as_str).to_vec();
        assert_eq!(names, ["declined", "timeout", "cancelled", "unsupported"]);
    }
}
