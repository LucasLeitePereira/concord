//! Adaptive bitrate (`adaptive-bitrate`): pure control logic with injected time (ms).
//!
//! The receiver sends a [`ReceiverReport`] about once per second; [`Abr::on_report`] turns it
//! into the bitrate target for the encoder. Heavy loss cuts the target fast, a clean network
//! raises it slowly, and the zone in between holds (hysteresis) so the rate does not oscillate.

use crate::preset::BitrateRange;

/// Loss above this multiplies the target by [`DECREASE_FACTOR`].
pub const LOSS_DECREASE_PCT: f32 = 5.0;
/// Loss below this (with no dropped frames) allows raising the target.
pub const LOSS_INCREASE_PCT: f32 = 1.0;
/// Decrease factor 0.7 and increase factor 1.03 as percentages (integer math: no float drift).
/// The climb is slow on purpose: the rate that fits the network is found from above after a cut,
/// and an encoder that has to reopen can only follow a change of 25% or more.
pub const DECREASE_PERCENT: u32 = 70;
pub const INCREASE_PERCENT: u32 = 103;
/// Spacing between two increases.
pub const INCREASE_INTERVAL_MS: u64 = 1_000;
/// Spacing between two decreases: reports that arrive right after a cut still describe the
/// old, higher rate (and the keyframe burst of the encoder reopening for it) and must not cut again.
pub const DECREASE_COOLDOWN_MS: u64 = 3_000;
/// After a decrease the target stays put this long before probing upward again.
pub const HOLD_AFTER_DECREASE_MS: u64 = 8_000;
/// A cut within this long after the previous one doubles the next hold (probing upward failed).
pub const BACKOFF_WINDOW_MS: u64 = 60_000;
/// A round trip above this (ms) blocks increases: the queue is already building.
pub const RTT_HOLD_MS: u32 = 400;
/// Reports this soon after the encoder reopened say nothing about the network: they carry the loss
/// of the keyframe burst the reopen itself caused, and cutting for it would start the next reopen.
pub const REOPEN_BLACKOUT_MS: u64 = 2_500;

/// Periodic feedback from the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ReceiverReport {
    /// Share of video fragments lost since the last report, 0-100.
    pub loss_pct: f32,
    /// Frames the receiver had to discard (incomplete or undecodable) since the last report.
    pub dropped_frames: u32,
    pub rtt_ms: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Abr {
    range: BitrateRange,
    current: u32,
    last_decrease_ms: Option<u64>,
    last_increase_ms: Option<u64>,
    last_reopen_ms: Option<u64>,
    /// Cuts that followed another one within [`BACKOFF_WINDOW_MS`] (a failed probe upward).
    decrease_streak: u32,
}

impl Abr {
    pub fn new(range: BitrateRange) -> Self {
        Abr {
            range,
            current: range.initial.clamp(range.min, range.max),
            last_decrease_ms: None,
            last_increase_ms: None,
            last_reopen_ms: None,
            decrease_streak: 0,
        }
    }

    /// How long the target stays put after a cut: doubled for each recent cut, up to 8x.
    fn hold_ms(&self) -> u64 {
        HOLD_AFTER_DECREASE_MS << self.decrease_streak.min(3)
    }

    /// The encoder was reopened at `now_ms` (it sent a keyframe burst): the reports of the next
    /// [`REOPEN_BLACKOUT_MS`] are ignored.
    pub fn note_encoder_reopen(&mut self, now_ms: u64) {
        self.last_reopen_ms = Some(now_ms);
    }

    /// Current bitrate target in kbps.
    pub fn target_kbps(&self) -> u32 {
        self.current
    }

    /// Feeds one report and returns the (possibly unchanged) target in kbps.
    pub fn on_report(&mut self, now_ms: u64, r: &ReceiverReport) -> u32 {
        let since = |t: Option<u64>| t.map(|t| now_ms.saturating_sub(t));
        if r.loss_pct.is_nan() || since(self.last_reopen_ms).is_some_and(|d| d < REOPEN_BLACKOUT_MS) {
            return self.current;
        }
        if r.loss_pct > LOSS_DECREASE_PCT {
            if since(self.last_decrease_ms).is_none_or(|d| d >= DECREASE_COOLDOWN_MS) {
                let cut = (u64::from(self.current) * u64::from(DECREASE_PERCENT) / 100) as u32;
                self.current = cut.clamp(self.range.min, self.range.max);
                let recent = since(self.last_decrease_ms).is_some_and(|d| d < BACKOFF_WINDOW_MS);
                self.decrease_streak = if recent { self.decrease_streak.saturating_add(1) } else { 0 };
                self.last_decrease_ms = Some(now_ms);
            }
        } else if r.loss_pct < LOSS_INCREASE_PCT
            && r.dropped_frames == 0
            && r.rtt_ms.is_none_or(|rtt| rtt <= RTT_HOLD_MS)
            && since(self.last_decrease_ms).is_none_or(|d| d >= self.hold_ms())
            && since(self.last_increase_ms).is_none_or(|d| d >= INCREASE_INTERVAL_MS)
        {
            let up = (u64::from(self.current) * u64::from(INCREASE_PERCENT)).div_ceil(100) as u32;
            self.current = up.clamp(self.range.min, self.range.max);
            self.last_increase_ms = Some(now_ms);
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RANGE: BitrateRange = BitrateRange { initial: 6_000, min: 1_500, max: 10_000 };

    fn rep(loss: f32) -> ReceiverReport {
        ReceiverReport { loss_pct: loss, ..Default::default() }
    }

    #[test]
    fn heavy_loss_cuts_by_thirty_percent_down_to_the_minimum() {
        let mut a = Abr::new(RANGE);
        let step = DECREASE_COOLDOWN_MS;
        assert_eq!(a.on_report(0, &rep(8.0)), 4_200);
        assert_eq!(a.on_report(step, &rep(8.0)), 2_940);
        assert_eq!(a.on_report(2 * step, &rep(8.0)), 2_058);
        assert_eq!(a.on_report(3 * step, &rep(8.0)), 1_500); // clamped
        assert_eq!(a.on_report(4 * step, &rep(50.0)), 1_500);
    }

    #[test]
    fn repeated_reports_inside_the_cooldown_cut_only_once() {
        let mut a = Abr::new(RANGE);
        assert_eq!(a.on_report(0, &rep(10.0)), 4_200);
        assert_eq!(a.on_report(300, &rep(10.0)), 4_200);
        assert_eq!(a.on_report(1_000, &rep(10.0)), 4_200);
        assert_eq!(a.on_report(DECREASE_COOLDOWN_MS - 1, &rep(10.0)), 4_200);
        assert_eq!(a.on_report(DECREASE_COOLDOWN_MS, &rep(10.0)), 2_940);
    }

    #[test]
    fn clean_network_climbs_three_percent_per_second_up_to_the_maximum() {
        let mut a = Abr::new(RANGE);
        assert_eq!(a.on_report(0, &rep(0.0)), 6_180);
        // Less than 1 s later: no second step.
        assert_eq!(a.on_report(500, &rep(0.0)), 6_180);
        assert_eq!(a.on_report(1_000, &rep(0.5)), 6_366);
        let mut t = 1_000;
        while a.target_kbps() < RANGE.max {
            t += 1_000;
            let before = a.target_kbps();
            let after = a.on_report(t, &rep(0.0));
            assert!(after > before && after <= RANGE.max);
            assert!(t < 60_000, "never reaches the max");
        }
        assert_eq!(a.on_report(t + 1_000, &rep(0.0)), RANGE.max);
    }

    #[test]
    fn middle_zone_holds_and_does_not_oscillate() {
        let mut a = Abr::new(RANGE);
        for (i, loss) in [1.0, 2.0, 3.0, 5.0, 1.5].into_iter().enumerate() {
            assert_eq!(a.on_report(i as u64 * 1_000, &rep(loss)), 6_000, "loss {loss}");
        }
        // Dropped frames or a slow round trip also block increases, without cutting.
        let dropped = ReceiverReport { loss_pct: 0.0, dropped_frames: 2, rtt_ms: None };
        assert_eq!(a.on_report(10_000, &dropped), 6_000);
        let slow = ReceiverReport { loss_pct: 0.0, dropped_frames: 0, rtt_ms: Some(RTT_HOLD_MS + 1) };
        assert_eq!(a.on_report(11_000, &slow), 6_000);
        let ok = ReceiverReport { loss_pct: 0.0, dropped_frames: 0, rtt_ms: Some(RTT_HOLD_MS) };
        assert_eq!(a.on_report(12_000, &ok), 6_180);
    }

    #[test]
    fn no_climb_right_after_a_cut() {
        let mut a = Abr::new(RANGE);
        assert_eq!(a.on_report(0, &rep(9.0)), 4_200);
        // Clean reports during the hold period change nothing.
        for t in [1_000, 2_000, 5_000, HOLD_AFTER_DECREASE_MS - 1] {
            assert_eq!(a.on_report(t, &rep(0.0)), 4_200, "t={t}");
        }
        assert_eq!(a.on_report(HOLD_AFTER_DECREASE_MS, &rep(0.0)), 4_326);
        // Alternating loss/clean can never push the rate above where it started.
        let mut b = Abr::new(RANGE);
        let mut t = 0;
        for i in 0..40 {
            t += 1_000;
            b.on_report(t, &rep(if i % 2 == 0 { 9.0 } else { 0.0 }));
            assert!(b.target_kbps() <= RANGE.initial);
        }
    }

    #[test]
    fn the_loss_of_a_reopen_burst_does_not_cut_again() {
        let mut a = Abr::new(RANGE);
        a.note_encoder_reopen(10_000);
        assert_eq!(a.on_report(11_000, &rep(30.0)), 6_000);
        assert_eq!(a.on_report(10_000 + REOPEN_BLACKOUT_MS - 1, &rep(30.0)), 6_000);
        assert_eq!(a.on_report(10_000 + REOPEN_BLACKOUT_MS, &rep(30.0)), 4_200);
        // Control: without the note the same report cuts at once.
        let mut b = Abr::new(RANGE);
        assert_eq!(b.on_report(11_000, &rep(30.0)), 4_200);
    }

    #[test]
    fn stays_inside_the_range_and_survives_garbage() {
        let mut a = Abr::new(BitrateRange { initial: 50_000, min: 1_000, max: 2_000 });
        assert_eq!(a.target_kbps(), 2_000); // initial clamped
        assert_eq!(a.on_report(0, &rep(f32::NAN)), 2_000);
        assert_eq!(a.on_report(1, &rep(f32::INFINITY)), 1_400);
        assert_eq!(a.on_report(10_000, &rep(-3.0)), 1_442);
    }
}
