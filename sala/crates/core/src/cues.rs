//! Notification sounds synthesized in code (D-024): no audio files. Each [`Cue`] is a few
//! sine tones with a raised-cosine envelope (no clicks), rendered at 48 kHz mono, the same
//! rate and frame size as the voice pipeline. [`CuePlayer`] mixes them into the playout
//! frame after the "deafened" silencing, so they are heard even while the app is silenced.

use crate::voice::SAMPLE_RATE;

/// One notification sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// You entered the room (two tones going up).
    SelfJoin,
    /// You left the room (two tones going down).
    SelfLeave,
    /// A friend entered the room while you are in it (softer than `SelfJoin`).
    PeerJoin,
    /// A friend left the room while you were in it.
    PeerLeave,
    Mute,
    Unmute,
    Deafen,
    Undeafen,
    /// Chat message while the UI is out of focus or closed.
    Message,
    /// Incoming ring (loops until stopped).
    RingIncoming,
    /// "Calling..." tone for the caller (loops until stopped).
    RingOutgoing,
    /// The ring was declined or went unanswered.
    CallFailed,
}

/// `(frequency 1, frequency 2, milliseconds)`; frequency 0 = silence / no second tone.
type Seg = (f32, f32, u32);

const fn tone(hz: f32, ms: u32) -> Seg {
    (hz, 0.0, ms)
}

const fn rest(ms: u32) -> Seg {
    (0.0, 0.0, ms)
}

/// Length of the fade in/out of each tone, in milliseconds.
const FADE_MS: u32 = 6;
/// Longest queue of one-shot cues (older ones are dropped).
const MAX_SHOTS: usize = 8;

impl Cue {
    pub const ALL: [Cue; 12] = [
        Cue::SelfJoin,
        Cue::SelfLeave,
        Cue::PeerJoin,
        Cue::PeerLeave,
        Cue::Mute,
        Cue::Unmute,
        Cue::Deafen,
        Cue::Undeafen,
        Cue::Message,
        Cue::RingIncoming,
        Cue::RingOutgoing,
        Cue::CallFailed,
    ];

    /// Loops until stopped (rings); the others play once.
    pub fn is_loop(self) -> bool {
        matches!(self, Cue::RingIncoming | Cue::RingOutgoing)
    }

    /// Length in milliseconds of one play (one period for the loops).
    pub fn duration_ms(self) -> u32 {
        let (segs, _, pad) = self.recipe();
        let sum: u32 = segs.iter().map(|s| s.2).sum();
        sum.max(pad)
    }

    /// Segments, amplitude and the minimum total length (silence is appended up to it).
    fn recipe(self) -> (Vec<Seg>, f32, u32) {
        match self {
            Cue::SelfJoin => (vec![tone(523.25, 90), tone(783.99, 140)], 0.55, 0),
            Cue::SelfLeave => (vec![tone(783.99, 90), tone(523.25, 140)], 0.55, 0),
            Cue::PeerJoin => (vec![tone(659.25, 70), tone(880.0, 110)], 0.32, 0),
            Cue::PeerLeave => (vec![tone(880.0, 70), tone(659.25, 110)], 0.32, 0),
            Cue::Mute => (vec![tone(520.0, 40), tone(340.0, 60)], 0.4, 0),
            Cue::Unmute => (vec![tone(340.0, 40), tone(520.0, 60)], 0.4, 0),
            Cue::Deafen => (vec![tone(420.0, 45), tone(240.0, 80)], 0.4, 0),
            Cue::Undeafen => (vec![tone(240.0, 45), tone(420.0, 80)], 0.4, 0),
            Cue::Message => (vec![tone(880.0, 60), tone(1174.66, 100)], 0.38, 0),
            // Ring, ring, then a pause: 2 s period.
            Cue::RingIncoming => (
                vec![tone(659.25, 140), tone(783.99, 140), rest(60), tone(659.25, 140), tone(783.99, 140)],
                0.5,
                2000,
            ),
            // Classic ringback: 1 s of two tones, 2 s of silence.
            Cue::RingOutgoing => (vec![(440.0, 480.0, 1000)], 0.3, 3000),
            Cue::CallFailed => (vec![tone(440.0, 110), tone(349.23, 110), tone(261.63, 220)], 0.45, 0),
        }
    }

    /// The whole sound at 48 kHz mono, samples in [-1, 1].
    pub fn synth(self) -> Vec<f32> {
        let (segs, amp, min_ms) = self.recipe();
        render(&segs, amp, min_ms)
    }
}

fn ms_to_samples(ms: u32) -> usize {
    (SAMPLE_RATE as usize * ms as usize) / 1000
}

fn render(segs: &[Seg], amp: f32, min_ms: u32) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for &(f1, f2, ms) in segs {
        let n = ms_to_samples(ms);
        if f1 <= 0.0 {
            out.resize(out.len() + n, 0.0);
            continue;
        }
        let fade = ms_to_samples(FADE_MS).min(n / 2).max(1);
        let tones = if f2 > 0.0 { 2.0 } else { 1.0 };
        for i in 0..n {
            let t = i as f64 / f64::from(SAMPLE_RATE);
            let mut v = (t * f64::from(f1) * std::f64::consts::TAU).sin();
            if f2 > 0.0 {
                v += (t * f64::from(f2) * std::f64::consts::TAU).sin();
            }
            // Raised-cosine edges: the segment starts and ends at exactly 0.
            let edge = i.min(n - 1 - i);
            let env = if edge >= fade {
                1.0
            } else {
                0.5 - 0.5 * (std::f64::consts::PI * edge as f64 / fade as f64).cos()
            };
            out.push((v / tones * env) as f32 * amp);
        }
    }
    let min = ms_to_samples(min_ms);
    if out.len() < min {
        out.resize(min, 0.0);
    }
    out
}

struct Shot {
    samples: Vec<f32>,
    pos: usize,
}

struct Looped {
    cue: Cue,
    samples: Vec<f32>,
    pos: usize,
}

/// Queue of one-shot cues plus at most one looping cue, with a volume. Not thread-safe:
/// it lives inside the `VoiceEngine`, driven by the session loop.
pub struct CuePlayer {
    shots: Vec<Shot>,
    looped: Option<Looped>,
    gain: f32,
}

impl Default for CuePlayer {
    fn default() -> Self {
        CuePlayer::new(60)
    }
}

impl CuePlayer {
    /// `volume_percent`: 0..=100 (0 = off).
    pub fn new(volume_percent: u32) -> Self {
        CuePlayer { shots: Vec::new(), looped: None, gain: gain_of(volume_percent) }
    }

    /// Changes the volume of what is playing and of what comes next. 0 drops the queue.
    pub fn set_volume(&mut self, volume_percent: u32) {
        self.gain = gain_of(volume_percent);
        if self.gain == 0.0 {
            self.shots.clear();
        }
    }

    /// Queues a one-shot cue (loop cues must use [`CuePlayer::set_loop`]). Ignored at volume 0.
    pub fn play(&mut self, cue: Cue) {
        if self.gain == 0.0 || cue.is_loop() {
            return;
        }
        if self.shots.len() >= MAX_SHOTS {
            self.shots.remove(0);
        }
        self.shots.push(Shot { samples: cue.synth(), pos: 0 });
    }

    /// Starts (or keeps, when it is already the one playing) the looping cue; `None` stops it.
    pub fn set_loop(&mut self, cue: Option<Cue>) {
        match cue {
            Some(c) if c.is_loop() => {
                if self.looped.as_ref().is_some_and(|l| l.cue == c) {
                    return;
                }
                self.looped = Some(Looped { cue: c, samples: c.synth(), pos: 0 });
            }
            _ => self.looped = None,
        }
    }

    pub fn current_loop(&self) -> Option<Cue> {
        self.looped.as_ref().map(|l| l.cue)
    }

    /// Nothing queued and no loop.
    pub fn is_idle(&self) -> bool {
        self.shots.is_empty() && self.looped.is_none()
    }

    /// Adds the next `mix.len()` samples (scaled by the volume) to `mix`. No clamping: the
    /// caller does it after summing everything.
    pub fn mix_into(&mut self, mix: &mut [f32]) {
        for shot in &mut self.shots {
            for m in mix.iter_mut() {
                let Some(s) = shot.samples.get(shot.pos) else { break };
                *m += s * self.gain;
                shot.pos += 1;
            }
        }
        self.shots.retain(|s| s.pos < s.samples.len());
        if let Some(l) = self.looped.as_mut() {
            for m in mix.iter_mut() {
                *m += l.samples[l.pos] * self.gain;
                l.pos = (l.pos + 1) % l.samples.len();
            }
        }
    }
}

fn gain_of(volume_percent: u32) -> f32 {
    volume_percent.min(100) as f32 / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 960;

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn every_cue_has_its_duration_sound_and_stays_in_range() {
        for cue in Cue::ALL {
            let s = cue.synth();
            assert_eq!(s.len(), ms_to_samples(cue.duration_ms()), "{cue:?} length");
            assert!(s.len() >= ms_to_samples(80), "{cue:?} too short");
            assert!(rms(&s) > 0.01, "{cue:?} is silent");
            assert!(peak(&s) <= 1.0, "{cue:?} exceeds full scale");
            assert!(s.iter().all(|v| v.is_finite()));
            // No click at the edges: it starts and ends at zero.
            assert!(s[0].abs() < 1e-3 && s[s.len() - 1].abs() < 1e-3, "{cue:?} edge");
        }
    }

    #[test]
    fn loops_are_the_rings_only_and_have_their_period() {
        let loops: Vec<Cue> = Cue::ALL.into_iter().filter(|c| c.is_loop()).collect();
        assert_eq!(loops, [Cue::RingIncoming, Cue::RingOutgoing]);
        assert_eq!(Cue::RingIncoming.duration_ms(), 2000);
        assert_eq!(Cue::RingOutgoing.duration_ms(), 3000);
        // Joining goes up in pitch, leaving goes down: the two are different sounds.
        assert_ne!(Cue::SelfJoin.synth(), Cue::SelfLeave.synth());
        assert_ne!(Cue::PeerJoin.synth(), Cue::SelfJoin.synth());
        assert_ne!(Cue::Mute.synth(), Cue::Unmute.synth());
        assert_ne!(Cue::Deafen.synth(), Cue::Undeafen.synth());
    }

    #[test]
    fn one_shot_plays_once_then_the_player_is_idle() {
        let mut p = CuePlayer::new(100);
        p.play(Cue::Message);
        assert!(!p.is_idle());
        let mut heard = 0.0f32;
        for _ in 0..30 {
            let mut f = vec![0.0f32; FRAME];
            p.mix_into(&mut f);
            heard = heard.max(peak(&f));
        }
        assert!(heard > 0.1);
        assert!(p.is_idle(), "the cue finished");
        let mut f = vec![0.0f32; FRAME];
        p.mix_into(&mut f);
        assert_eq!(peak(&f), 0.0);
    }

    #[test]
    fn volume_zero_is_silence_and_scales_linearly() {
        let mut off = CuePlayer::new(0);
        off.play(Cue::SelfJoin);
        assert!(off.is_idle(), "volume 0 queues nothing");
        off.set_loop(Some(Cue::RingIncoming));
        let mut f = vec![0.0f32; FRAME];
        for _ in 0..10 {
            off.mix_into(&mut f);
        }
        assert_eq!(peak(&f), 0.0);

        let level = |vol: u32| {
            let mut p = CuePlayer::new(vol);
            p.play(Cue::SelfJoin);
            let mut f = vec![0.0f32; FRAME * 5];
            p.mix_into(&mut f);
            peak(&f)
        };
        let (full, half) = (level(100), level(50));
        assert!(full > 0.4);
        assert!((half - full / 2.0).abs() < 0.01, "{half} vs {full}");
    }

    #[test]
    fn loop_repeats_until_stopped_and_set_loop_keeps_the_phase() {
        let mut p = CuePlayer::new(100);
        p.set_loop(Some(Cue::RingOutgoing));
        let period = ms_to_samples(3000);
        let mut f = vec![0.0f32; FRAME];
        let mut first_sound = None;
        let mut sounds_after_period = 0.0f32;
        for tick in 0..(period / FRAME) * 2 {
            f.fill(0.0);
            p.mix_into(&mut f);
            if first_sound.is_none() && peak(&f) > 0.05 {
                first_sound = Some(tick);
            }
            if tick >= period / FRAME && peak(&f) > 0.05 {
                sounds_after_period = peak(&f);
            }
            // Asking for the same loop again must not restart it.
            p.set_loop(Some(Cue::RingOutgoing));
        }
        assert_eq!(first_sound, Some(0));
        assert!(sounds_after_period > 0.05, "the second period sounds again");
        p.set_loop(None);
        assert!(p.is_idle());
        f.fill(0.0);
        p.mix_into(&mut f);
        assert_eq!(peak(&f), 0.0);
        // A one-shot cue is not a loop.
        p.set_loop(Some(Cue::Message));
        assert!(p.current_loop().is_none());
    }

    #[test]
    fn queue_is_bounded() {
        let mut p = CuePlayer::new(100);
        for _ in 0..50 {
            p.play(Cue::Mute);
        }
        assert_eq!(p.shots.len(), MAX_SHOTS);
    }
}
