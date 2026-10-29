//! Notification sounds (D-024). Each [`Cue`] is a WAV embedded in the binary
//! (`assets/cues/`, Kenney "Interface Sounds", CC0), already at 48 kHz mono 16-bit with
//! faded edges, the same rate as the voice pipeline. [`CuePlayer`] mixes them into the
//! playout frame after the "deafened" silencing, so they are heard even while silenced.

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
        let (wav, _, pad) = self.recipe();
        let n = pcm16(wav).len() as u32;
        ((n * 1000).div_ceil(SAMPLE_RATE)).max(pad)
    }

    /// Embedded WAV, amplitude and the minimum total length (silence is appended up to it;
    /// for the loops it is the period).
    fn recipe(self) -> (&'static [u8], f32, u32) {
        macro_rules! wav {
            ($name:literal) => {
                include_bytes!(concat!("../assets/cues/", $name, ".wav")).as_slice()
            };
        }
        match self {
            Cue::SelfJoin => (wav!("self_join"), 0.6, 0),
            Cue::SelfLeave => (wav!("self_leave"), 0.6, 0),
            Cue::PeerJoin => (wav!("peer_join"), 0.4, 0),
            Cue::PeerLeave => (wav!("peer_leave"), 0.4, 0),
            Cue::Mute => (wav!("mute"), 0.5, 0),
            Cue::Unmute => (wav!("unmute"), 0.5, 0),
            Cue::Deafen => (wav!("deafen"), 0.5, 0),
            Cue::Undeafen => (wav!("undeafen"), 0.5, 0),
            Cue::Message => (wav!("message"), 0.45, 0),
            // The sound, then a pause: 2 s period.
            Cue::RingIncoming => (wav!("ring_incoming"), 0.6, 2000),
            Cue::RingOutgoing => (wav!("ring_outgoing"), 0.4, 3000),
            Cue::CallFailed => (wav!("call_failed"), 0.5, 0),
        }
    }

    /// The whole sound at 48 kHz mono, samples in [-1, 1].
    pub fn synth(self) -> Vec<f32> {
        let (wav, amp, min_ms) = self.recipe();
        let mut out: Vec<f32> = pcm16(wav).iter().map(|&s| f32::from(s) / 32768.0 * amp).collect();
        let min = ms_to_samples(min_ms);
        if out.len() < min {
            out.resize(min, 0.0);
        }
        out
    }
}

fn ms_to_samples(ms: u32) -> usize {
    (SAMPLE_RATE as usize * ms as usize) / 1000
}

/// Samples of the `data` chunk of a 16-bit PCM mono WAV (the format of `assets/cues/`;
/// anything else yields no samples, which the tests catch).
fn pcm16(wav: &[u8]) -> Vec<i16> {
    let mut i = 12;
    while i + 8 <= wav.len() {
        let id = &wav[i..i + 4];
        let len = u32::from_le_bytes([wav[i + 4], wav[i + 5], wav[i + 6], wav[i + 7]]) as usize;
        let body = &wav[i + 8..(i + 8 + len).min(wav.len())];
        if id == b"data" {
            return body.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        }
        i += 8 + len + (len & 1);
    }
    Vec::new()
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
            assert!(s.len().abs_diff(ms_to_samples(cue.duration_ms())) < 48, "{cue:?} length");
            // The embedded file is 48 kHz mono 16-bit (fmt chunk right after the header).
            let (wav, _, _) = cue.recipe();
            assert_eq!(&wav[12..16], b"fmt ", "{cue:?} fmt");
            assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "{cue:?} mono");
            assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), SAMPLE_RATE, "{cue:?} rate");
            assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "{cue:?} bits");
            assert!(s.len() >= ms_to_samples(80), "{cue:?} too short");
            assert!(rms(&s) > 0.01, "{cue:?} is silent");
            assert!(peak(&s) <= 1.0, "{cue:?} exceeds full scale");
            assert!(s.iter().all(|v| v.is_finite()));
            // No click at the edges: it starts and ends at zero.
            assert!(s[0].abs() < 1e-3 && s[s.len() - 1].abs() < 1e-3, "{cue:?} edge");
        }
    }

    #[test]
    fn pcm16_reads_only_the_data_chunk() {
        let mut w = b"RIFF\0\0\0\0WAVE".to_vec();
        w.extend_from_slice(b"LIST\x03\0\0\0abc\0"); // odd chunk + pad byte
        w.extend_from_slice(b"data\x04\0\0\0\x01\0\xff\xff");
        assert_eq!(pcm16(&w), [1, -1]);
        // Control: no data chunk, no samples.
        assert!(pcm16(&w[..w.len() - 12]).is_empty());
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
            let mut f = vec![0.0f32; FRAME * 25];
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
