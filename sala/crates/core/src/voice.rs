//! Voice pipeline without any audio device (`voice-frame`, `voice-activation`, `call`).
//!
//! Send path: capture frame -> `FrameProcessor` hook -> activation -> Opus -> `EncodedFrame`.
//! Receive path: `JitterBuffer` -> Opus (PLC / in-band FEC) -> volume -> mix (clamped).
//! Everything here is pure and testable; devices live in `audio_device.rs`.
//!
//! Level units: dBFS of the frame RMS (a full-scale square wave is 0 dBFS, a
//! full-scale sine is about -3 dBFS).

use std::collections::{HashMap, VecDeque};

use opus::{Application, Bitrate, Channels, Decoder, Encoder};

use crate::config::{AudioConfig, AudioMode};
use crate::jitter::{JitterBuffer, JitterStats, Pop};

pub const SAMPLE_RATE: u32 = 48_000;
/// Samples per 20 ms frame (mono).
pub const FRAME_SAMPLES: usize = 960;
/// Opus packets of at most this many bytes are DTX comfort-noise updates: not sent (D-006).
pub const DTX_MAX_BYTES: usize = 2;
/// Voice-activity hangover: keeps transmitting after the level drops (300 ms).
pub const HANGOVER_FRAMES: u32 = 15;
/// Expected packet loss told to the encoder so in-band FEC is worth its bits.
const PACKET_LOSS_PERC: i32 = 10;
const MAX_PACKET_BYTES: usize = 1275;
pub const MIN_BITRATE_KBPS: u32 = 32;
pub const MAX_BITRATE_KBPS: u32 = 128;

/// Frame level in dBFS (RMS). Empty or silent input is `f32::NEG_INFINITY`.
pub fn level_dbfs(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return f32::NEG_INFINITY;
    }
    let mean_sq = pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32;
    10.0 * mean_sq.log10()
}

/// Volume per friend: 0.0..=2.0 (`call.peer_volume`). NaN falls back to 1.0.
pub fn clamp_volume(volume: f32) -> f32 {
    if volume.is_nan() { 1.0 } else { volume.clamp(0.0, 2.0) }
}

/// Applies `volume` and clamps every sample to [-1, 1] (`inv-mix-no-clipping`).
pub fn apply_volume(samples: &mut [f32], volume: f32) {
    let v = clamp_volume(volume);
    for s in samples {
        *s = (*s * v).clamp(-1.0, 1.0);
    }
}

/// Synthetic microphone: a sine tone (`--synthetic-audio`).
pub struct SineSource {
    phase: f64,
    step: f64,
    amplitude: f32,
}

impl SineSource {
    pub fn new(freq_hz: f32, dbfs: f32) -> Self {
        Self {
            phase: 0.0,
            step: f64::from(freq_hz) / f64::from(SAMPLE_RATE),
            amplitude: 10f32.powf(dbfs / 20.0),
        }
    }

    /// 440 Hz at -12 dBFS peak.
    pub fn standard() -> Self {
        Self::new(440.0, -12.0)
    }

    pub fn next_frame(&mut self) -> Vec<f32> {
        (0..FRAME_SAMPLES)
            .map(|_| {
                let v = (self.phase * std::f64::consts::TAU).sin() as f32 * self.amplitude;
                self.phase = (self.phase + self.step).fract();
                v
            })
            .collect()
    }
}

/// Averages interleaved channels into mono.
pub fn downmix_to_mono(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    out.extend(
        interleaved
            .chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32),
    );
}

/// Streaming mono resampler with linear interpolation.
///
/// Linear interpolation is cheap and good enough for speech; it has no
/// anti-alias filter, so downsampling from a much higher rate adds some
/// aliasing (only 44.1/48/96 kHz devices are expected). State is carried
/// between calls so chunk boundaries are seamless.
pub struct Resampler {
    step: f64,
    pos: f64,
    carry: Vec<f32>,
}

impl Resampler {
    pub fn new(from_hz: u32, to_hz: u32) -> Self {
        Self { step: f64::from(from_hz) / f64::from(to_hz), pos: 0.0, carry: Vec::new() }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.step - 1.0).abs() < f64::EPSILON {
            out.extend_from_slice(input);
            return;
        }
        let mut m = std::mem::take(&mut self.carry);
        m.extend_from_slice(input);
        while (self.pos as usize) + 1 < m.len() {
            let i = self.pos as usize;
            let f = (self.pos - i as f64) as f32;
            out.push(m[i] * (1.0 - f) + m[i + 1] * f);
            self.pos += self.step;
        }
        let consumed = (self.pos as usize).min(m.len());
        self.pos -= consumed as f64;
        self.carry = m.split_off(consumed);
    }
}

/// Extension point for noise suppression (`nnnoiseless`, off by default, not wired yet).
/// Runs on every captured 20 ms frame before activation and encoding.
pub trait FrameProcessor: Send {
    fn process(&mut self, frame: &mut [f32]);
}

/// One Opus packet ready for a `MediaDatagram::Audio { stream: Voice, .. }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub seq: u16,
    pub timestamp: u32,
    pub payload: Vec<u8>,
}

/// Opus encoder + `voice-frame` numbering.
///
/// `seq` counts packets actually produced (DTX / gated frames leave no hole,
/// D-006). `timestamp` counts captured samples, so it jumps over silence.
pub struct VoiceEncoder {
    enc: Encoder,
    seq: u16,
    timestamp: u32,
    buf: [u8; MAX_PACKET_BYTES],
}

impl VoiceEncoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self, opus::Error> {
        let mut enc = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
        let kbps = bitrate_kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS);
        enc.set_bitrate(Bitrate::Bits((kbps * 1000) as i32))?;
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(PACKET_LOSS_PERC)?;
        enc.set_dtx(true)?;
        Ok(Self { enc, seq: 0, timestamp: 0, buf: [0; MAX_PACKET_BYTES] })
    }

    /// Encodes one frame of exactly `FRAME_SAMPLES`. `None` for a DTX packet (not sent, no seq used).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Option<EncodedFrame>, opus::Error> {
        let ts = self.timestamp;
        self.timestamp = self.timestamp.wrapping_add(FRAME_SAMPLES as u32);
        let n = self.enc.encode_float(pcm, &mut self.buf)?;
        if n <= DTX_MAX_BYTES {
            return Ok(None);
        }
        let frame = EncodedFrame { seq: self.seq, timestamp: ts, payload: self.buf[..n].to_vec() };
        self.seq = self.seq.wrapping_add(1);
        Ok(Some(frame))
    }

    /// A captured frame that is not transmitted (muted / no voice): time passes, seq does not.
    pub fn skip_frame(&mut self) {
        self.timestamp = self.timestamp.wrapping_add(FRAME_SAMPLES as u32);
    }
}

/// Opus decoder with loss concealment.
pub struct VoiceDecoder {
    dec: Decoder,
    pub plc_frames: u64,
    pub fec_frames: u64,
}

impl VoiceDecoder {
    pub fn new() -> Result<Self, opus::Error> {
        Ok(Self { dec: Decoder::new(SAMPLE_RATE, Channels::Mono)?, plc_frames: 0, fec_frames: 0 })
    }

    /// Decodes a received packet. `None` when the packet is invalid.
    pub fn decode(&mut self, payload: &[u8]) -> Option<Vec<f32>> {
        self.run(payload, false)
    }

    /// Fills a lost slot: FEC from `next_payload` (the packet right after the
    /// hole) when available, otherwise PLC (empty decode).
    pub fn conceal(&mut self, next_payload: Option<&[u8]>) -> Option<Vec<f32>> {
        match next_payload {
            Some(p) if !p.is_empty() => {
                self.fec_frames += 1;
                self.run(p, true)
            }
            _ => {
                self.plc_frames += 1;
                self.run(&[], false)
            }
        }
    }

    fn run(&mut self, payload: &[u8], fec: bool) -> Option<Vec<f32>> {
        let mut out = vec![0.0f32; FRAME_SAMPLES];
        match self.dec.decode_float(payload, &mut out, fec) {
            Ok(n) if n > 0 => {
                out.truncate(n);
                Some(out)
            }
            _ => None,
        }
    }
}

/// Decides whether the microphone transmits (`voice-activation`) and owns the
/// mute / deafen state (`inv-deafen-implies-mute`).
#[derive(Debug, Clone)]
pub struct Activation {
    mode: AudioMode,
    threshold_db: f32,
    ptt: bool,
    muted: bool,
    deafened: bool,
    muted_before_deafen: bool,
    hangover: u32,
}

impl Activation {
    pub fn new(mode: AudioMode, threshold_db: i32) -> Self {
        Self {
            mode,
            threshold_db: threshold_db as f32,
            ptt: false,
            muted: false,
            deafened: false,
            muted_before_deafen: false,
            hangover: 0,
        }
    }

    /// Push-to-talk key state (the global hotkey itself arrives in F2).
    pub fn set_ptt(&mut self, pressed: bool) {
        self.ptt = pressed;
    }

    /// While deafened the microphone stays muted: the request is remembered and
    /// applies when deafen ends.
    pub fn set_muted(&mut self, muted: bool) {
        if self.deafened {
            self.muted_before_deafen = muted;
        } else {
            self.muted = muted;
        }
    }

    /// Deafening mutes the microphone; undeafening restores the previous mute state.
    pub fn set_deafened(&mut self, deafened: bool) {
        if deafened == self.deafened {
            return;
        }
        if deafened {
            self.muted_before_deafen = self.muted;
            self.muted = true;
        } else {
            self.muted = self.muted_before_deafen;
        }
        self.deafened = deafened;
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn deafened(&self) -> bool {
        self.deafened
    }

    /// True when this captured frame must be transmitted.
    pub fn should_transmit(&mut self, pcm: &[f32]) -> bool {
        if self.muted {
            self.hangover = 0;
            return false;
        }
        match self.mode {
            AudioMode::PushToTalk => self.ptt,
            AudioMode::VoiceActivity => {
                if level_dbfs(pcm) >= self.threshold_db {
                    self.hangover = HANGOVER_FRAMES;
                    true
                } else if self.hangover > 0 {
                    self.hangover -= 1;
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// Per-friend receive state.
struct Peer {
    jitter: JitterBuffer,
    decoder: VoiceDecoder,
    volume: f32,
}

/// Counters for the periodic log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceStats {
    /// Packets produced and handed to the network.
    pub sent: u64,
    /// Frames not transmitted (muted, no voice, DTX).
    pub skipped: u64,
}

/// Receive-side counters of one friend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerStats {
    pub jitter: JitterStats,
    pub plc_frames: u64,
    pub fec_frames: u64,
}

/// The whole voice pipeline of one `sala-core` (all calls share one microphone).
pub struct VoiceEngine {
    activation: Activation,
    encoder: VoiceEncoder,
    denoiser: Option<Box<dyn FrameProcessor>>,
    peers: HashMap<String, Peer>,
    stats: VoiceStats,
    loopback: Option<(VoiceDecoder, VecDeque<Vec<f32>>)>,
}

impl VoiceEngine {
    pub fn new(cfg: &AudioConfig) -> Result<Self, opus::Error> {
        Ok(Self {
            activation: Activation::new(cfg.mode, cfg.vad_threshold),
            encoder: VoiceEncoder::new(cfg.bitrate_kbps)?,
            denoiser: None,
            peers: HashMap::new(),
            stats: VoiceStats::default(),
            loopback: None,
        })
    }

    pub fn activation(&self) -> &Activation {
        &self.activation
    }

    pub fn activation_mut(&mut self) -> &mut Activation {
        &mut self.activation
    }

    /// Plugs a noise suppressor in front of activation / encoding.
    pub fn set_denoiser(&mut self, p: Option<Box<dyn FrameProcessor>>) {
        self.denoiser = p;
    }

    /// Audio test (spec 5.1): the microphone is also encoded, decoded and played locally.
    pub fn set_loopback(&mut self, on: bool) -> Result<(), opus::Error> {
        self.loopback = if on { Some((VoiceDecoder::new()?, VecDeque::new())) } else { None };
        Ok(())
    }

    pub fn stats(&self) -> VoiceStats {
        self.stats
    }

    /// Starts (or restarts) the receive side of a call with `friend`.
    pub fn add_peer(&mut self, friend: &str) -> Result<(), opus::Error> {
        let peer =
            Peer { jitter: JitterBuffer::default(), decoder: VoiceDecoder::new()?, volume: 1.0 };
        self.peers.insert(friend.to_string(), peer);
        Ok(())
    }

    pub fn remove_peer(&mut self, friend: &str) {
        self.peers.remove(friend);
    }

    pub fn set_peer_volume(&mut self, friend: &str, volume: f32) {
        if let Some(p) = self.peers.get_mut(friend) {
            p.volume = clamp_volume(volume);
        }
    }

    pub fn peer_stats(&self, friend: &str) -> Option<PeerStats> {
        self.peers.get(friend).map(|p| PeerStats {
            jitter: p.jitter.stats(),
            plc_frames: p.decoder.plc_frames,
            fec_frames: p.decoder.fec_frames,
        })
    }

    pub fn peer_names(&self) -> impl Iterator<Item = &str> {
        self.peers.keys().map(String::as_str)
    }

    /// Send path for one captured 20 ms frame (`FRAME_SAMPLES` mono samples).
    pub fn process_capture(&mut self, pcm: &mut [f32]) -> Result<Option<EncodedFrame>, opus::Error> {
        if pcm.len() != FRAME_SAMPLES {
            return Ok(None);
        }
        if let Some(d) = self.denoiser.as_mut() {
            d.process(pcm);
        }
        if !self.activation.should_transmit(pcm) {
            self.encoder.skip_frame();
            self.stats.skipped += 1;
            return Ok(None);
        }
        let frame = self.encoder.encode(pcm)?;
        match &frame {
            Some(f) => {
                self.stats.sent += 1;
                if let Some((dec, queue)) = self.loopback.as_mut()
                    && let Some(out) = dec.decode(&f.payload)
                {
                    queue.push_back(out);
                }
            }
            None => self.stats.skipped += 1,
        }
        Ok(frame)
    }

    /// A voice datagram from `friend` arrived.
    pub fn push_datagram(&mut self, friend: &str, seq: u16, timestamp: u32, payload: Vec<u8>, now_ms: u64) {
        if let Some(p) = self.peers.get_mut(friend) {
            p.jitter.push(seq, timestamp, payload, now_ms);
        }
    }

    /// Playout tick (every 20 ms): one mixed frame ready for the sink. Always
    /// `FRAME_SAMPLES` long; silence while deafened (buffers are still drained).
    pub fn playout_tick(&mut self, now_ms: u64) -> Vec<f32> {
        let mut mix = vec![0.0f32; FRAME_SAMPLES];
        for p in self.peers.values_mut() {
            let decoded = match p.jitter.pop(now_ms) {
                Pop::Frame(payload) => p.decoder.decode(&payload),
                Pop::Lost => {
                    let next = p.jitter.peek_next();
                    p.decoder.conceal(next)
                }
                Pop::Empty => None,
            };
            if let Some(mut frame) = decoded {
                apply_volume(&mut frame, p.volume);
                for (m, s) in mix.iter_mut().zip(&frame) {
                    *m += s;
                }
            }
        }
        if let Some((_, queue)) = self.loopback.as_mut() {
            if let Some(frame) = queue.pop_front() {
                for (m, s) in mix.iter_mut().zip(&frame) {
                    *m += s;
                }
            }
            while queue.len() > 5 {
                queue.pop_front();
            }
        }
        if self.activation.deafened() {
            mix.fill(0.0);
        }
        // Sum of several friends can exceed full scale: final clamp.
        apply_volume(&mut mix, 1.0);
        mix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn engine() -> VoiceEngine {
        VoiceEngine::new(&AudioConfig::default()).unwrap()
    }

    #[test]
    fn opus_round_trip_preserves_energy() {
        let mut enc = VoiceEncoder::new(96).unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let mut src = SineSource::standard();
        let mut in_e = 0.0;
        let mut out_e = 0.0;
        for i in 0..50 {
            let f = src.next_frame();
            let packet = enc.encode(&f).unwrap().expect("tone is not DTX");
            let out = dec.decode(&packet.payload).unwrap();
            assert_eq!(out.len(), FRAME_SAMPLES);
            if i >= 10 {
                in_e += rms(&f);
                out_e += rms(&out);
            }
        }
        let ratio = out_e / in_e;
        assert!((0.7..1.3).contains(&ratio), "energy ratio {ratio}");
    }

    #[test]
    fn dtx_does_not_consume_seq() {
        let mut enc = VoiceEncoder::new(96).unwrap();
        let mut src = SineSource::standard();
        let silence = vec![0.0f32; FRAME_SAMPLES];
        let mut seqs = Vec::new();
        let mut dtx_skips = 0;
        for round in 0..3 {
            for _ in 0..5 {
                seqs.push(enc.encode(&src.next_frame()).unwrap().unwrap().seq);
            }
            for _ in 0..60 {
                match enc.encode(&silence).unwrap() {
                    Some(f) => seqs.push(f.seq),
                    None => dtx_skips += 1,
                }
            }
            let _ = round;
        }
        assert!(dtx_skips > 0, "DTX never kicked in");
        for w in seqs.windows(2) {
            assert_eq!(w[1], w[0].wrapping_add(1), "hole in seq: {seqs:?}");
        }
    }

    #[test]
    fn timestamp_advances_over_dtx_and_skipped_frames() {
        let mut enc = VoiceEncoder::new(96).unwrap();
        let mut src = SineSource::standard();
        let a = enc.encode(&src.next_frame()).unwrap().unwrap();
        enc.skip_frame();
        enc.skip_frame();
        let b = enc.encode(&src.next_frame()).unwrap().unwrap();
        assert_eq!(b.seq, a.seq + 1);
        assert_eq!(b.timestamp, a.timestamp + 3 * FRAME_SAMPLES as u32);
    }

    #[test]
    fn bitrate_is_clamped_and_changes_size() {
        let mut lo = VoiceEncoder::new(1).unwrap();
        let mut hi = VoiceEncoder::new(999).unwrap();
        let mut a = SineSource::new(300.0, -6.0);
        let mut b = SineSource::new(300.0, -6.0);
        let (mut nl, mut nh) = (0, 0);
        for _ in 0..50 {
            nl += lo.encode(&a.next_frame()).unwrap().unwrap().payload.len();
            nh += hi.encode(&b.next_frame()).unwrap().unwrap().payload.len();
        }
        assert!(nh > nl, "128 kbps ({nh}) should use more bytes than 32 kbps ({nl})");
    }

    #[test]
    fn vad_with_hangover() {
        let mut a = Activation::new(AudioMode::VoiceActivity, -45);
        let loud = vec![0.1f32; FRAME_SAMPLES]; // -20 dBFS
        let quiet = vec![0.0001f32; FRAME_SAMPLES]; // -80 dBFS
        assert!(!a.should_transmit(&quiet));
        assert!(a.should_transmit(&loud));
        for i in 0..HANGOVER_FRAMES {
            assert!(a.should_transmit(&quiet), "hangover frame {i}");
        }
        assert!(!a.should_transmit(&quiet), "hangover must end");
    }

    #[test]
    fn vad_without_hangover_would_cut_tail() {
        // Negative control: right after speech the very next quiet frame is still sent.
        let mut a = Activation::new(AudioMode::VoiceActivity, -45);
        a.should_transmit(&vec![0.1f32; FRAME_SAMPLES]);
        assert!(a.should_transmit(&vec![0.0f32; FRAME_SAMPLES]));
    }

    #[test]
    fn ptt_follows_key_state_and_ignores_level() {
        let mut a = Activation::new(AudioMode::PushToTalk, -45);
        let loud = vec![0.5f32; FRAME_SAMPLES];
        let quiet = vec![0.0f32; FRAME_SAMPLES];
        assert!(!a.should_transmit(&loud));
        a.set_ptt(true);
        assert!(a.should_transmit(&quiet));
        a.set_ptt(false);
        assert!(!a.should_transmit(&loud));
    }

    #[test]
    fn mute_wins_over_voice_and_ptt() {
        let loud = vec![0.5f32; FRAME_SAMPLES];
        let mut v = Activation::new(AudioMode::VoiceActivity, -45);
        v.set_muted(true);
        assert!(!v.should_transmit(&loud));
        let mut p = Activation::new(AudioMode::PushToTalk, -45);
        p.set_ptt(true);
        p.set_muted(true);
        assert!(!p.should_transmit(&loud));
    }

    #[test]
    fn deafen_mutes_and_undeafen_restores_previous_mute() {
        let mut a = Activation::new(AudioMode::VoiceActivity, -45);
        // Not muted before: restored to unmuted.
        a.set_deafened(true);
        assert!(a.muted() && a.deafened());
        a.set_deafened(false);
        assert!(!a.muted() && !a.deafened());
        // Muted before: stays muted afterwards.
        a.set_muted(true);
        a.set_deafened(true);
        a.set_deafened(false);
        assert!(a.muted());
        // Unmute request while deafened applies on undeafen, mic stays muted meanwhile.
        a.set_deafened(true);
        a.set_muted(false);
        assert!(a.muted());
        a.set_deafened(false);
        assert!(!a.muted());
    }

    #[test]
    fn volume_is_clamped_and_output_never_clips() {
        assert_eq!(clamp_volume(5.0), 2.0);
        assert_eq!(clamp_volume(-1.0), 0.0);
        assert_eq!(clamp_volume(f32::NAN), 1.0);
        let mut s = vec![0.9, -0.9, 0.3];
        apply_volume(&mut s, 2.0);
        assert_eq!(s, vec![1.0, -1.0, 0.6]);
        let mut z = vec![0.5];
        apply_volume(&mut z, 0.0);
        assert_eq!(z, vec![0.0]);
    }

    #[test]
    fn mix_of_peers_is_clamped() {
        let mut e = engine();
        e.add_peer("a").unwrap();
        e.add_peer("b").unwrap();
        e.set_peer_volume("a", 2.0);
        e.set_peer_volume("b", 2.0);
        let mut src = SineSource::new(440.0, -1.0);
        let mut enc = VoiceEncoder::new(96).unwrap();
        let mut t = 0u64;
        let mut peak = 0.0f32;
        for _ in 0..40 {
            let f = enc.encode(&src.next_frame()).unwrap().unwrap();
            for n in ["a", "b"] {
                e.push_datagram(n, f.seq, f.timestamp, f.payload.clone(), t);
            }
            t += 20;
            peak = e.playout_tick(t + 100).iter().fold(peak, |m, s| m.max(s.abs()));
        }
        assert!(peak > 0.9, "mix should be loud, peak {peak}");
        assert!(peak <= 1.0);
    }

    #[test]
    fn resample_44100_to_48000_length_and_continuity() {
        let mut r = Resampler::new(44_100, 48_000);
        let input: Vec<f32> = (0..44_100).map(|i| i as f32 * 1e-3).collect(); // ramp
        let mut out = Vec::new();
        // Feed in odd-sized chunks: state must carry across them.
        for c in input.chunks(441 + 7) {
            r.process(c, &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 2, "len {}", out.len());
        // A ramp resampled linearly stays a ramp with slope 44100/48000 (scaled by 1e-3).
        for w in out.windows(2) {
            assert!(((w[1] - w[0]) - 0.000_918_75).abs() < 2e-5, "{} {}", w[0], w[1]);
        }
    }

    #[test]
    fn resample_same_rate_is_identity_and_48_to_44_shrinks() {
        let x: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut out = Vec::new();
        Resampler::new(48_000, 48_000).process(&x, &mut out);
        assert_eq!(out, x);
        let mut down = Vec::new();
        Resampler::new(48_000, 44_100).process(&vec![0.0; 4800], &mut down);
        assert!((down.len() as i64 - 4410).abs() <= 2);
    }

    #[test]
    fn downmix_averages_channels() {
        let mut out = Vec::new();
        downmix_to_mono(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
        let mut mono = Vec::new();
        downmix_to_mono(&[0.1, 0.2], 1, &mut mono);
        assert_eq!(mono, vec![0.1, 0.2]);
    }

    #[test]
    fn synthetic_source_level_matches_spec() {
        let f = SineSource::standard().next_frame();
        let peak = f.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((20.0 * peak.log10() - -12.0).abs() < 0.2);
        assert_eq!(f.len(), FRAME_SAMPLES);
    }

    #[test]
    fn deafened_playout_is_silent_but_drains() {
        let mut e = engine();
        e.add_peer("a").unwrap();
        let mut enc = VoiceEncoder::new(96).unwrap();
        let mut src = SineSource::standard();
        let mut heard = 0.0f32;
        for deaf in [false, true] {
            e.activation_mut().set_deafened(deaf);
            for i in 0..30u64 {
                let f = enc.encode(&src.next_frame()).unwrap().unwrap();
                e.push_datagram("a", f.seq, f.timestamp, f.payload, i * 20);
                let out = e.playout_tick(i * 20 + 100);
                if deaf {
                    assert!(out.iter().all(|s| *s == 0.0));
                } else {
                    heard = heard.max(rms(&out));
                }
            }
        }
        assert!(heard > 0.05, "control: not deafened must be audible");
    }

    #[test]
    fn loopback_returns_own_voice_only_when_enabled() {
        let run = |loopback: bool| {
            let mut e = engine();
            e.set_loopback(loopback).unwrap();
            let mut src = SineSource::standard();
            let mut e_sum = 0.0;
            for i in 0..30u64 {
                let mut f = src.next_frame();
                e.process_capture(&mut f).unwrap();
                e_sum += rms(&e.playout_tick(i * 20));
            }
            e_sum
        };
        assert!(run(true) > 1.0);
        assert_eq!(run(false), 0.0);
    }

    #[test]
    fn gated_frames_leave_no_seq_hole() {
        let mut e = engine();
        e.activation_mut().set_muted(true);
        let mut src = SineSource::standard();
        assert!(e.process_capture(&mut src.next_frame()).unwrap().is_none());
        e.activation_mut().set_muted(false);
        let f = e.process_capture(&mut src.next_frame()).unwrap().unwrap();
        assert_eq!(f.seq, 0);
        assert_eq!(f.timestamp, FRAME_SAMPLES as u32);
        assert_eq!(e.stats().sent, 1);
        assert_eq!(e.stats().skipped, 1);
    }

    /// Encoder -> 5% loss + reordering -> jitter -> decoder, no device.
    #[test]
    fn pipeline_with_loss_and_reorder_conceals_without_panic() {
        let mut tx = engine();
        let mut rx = engine();
        rx.add_peer("p").unwrap();
        let mut src = SineSource::standard();
        let mut rng = 12345u32; // deterministic LCG
        let mut next = move || {
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            rng >> 8
        };
        let mut held: Option<EncodedFrame> = None;
        let mut out_energy = 0.0f32;
        let mut dropped = 0;
        for i in 0..500u64 {
            let now = i * 20;
            let mut f = src.next_frame();
            let frame = tx.process_capture(&mut f).unwrap().unwrap();
            if next() % 100 < 5 {
                dropped += 1; // lost on the wire
            } else if next() % 100 < 5 && held.is_none() {
                held = Some(frame); // delayed by one frame (reordering)
            } else {
                rx.push_datagram("p", frame.seq, frame.timestamp, frame.payload, now);
                if let Some(h) = held.take() {
                    rx.push_datagram("p", h.seq, h.timestamp, h.payload, now);
                }
            }
            let out = rx.playout_tick(now);
            assert_eq!(out.len(), FRAME_SAMPLES);
            assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
            out_energy += rms(&out);
        }
        let st = rx.peer_stats("p").unwrap();
        assert!(dropped > 5, "simulation must drop packets");
        assert!(st.jitter.lost > 0, "losses must be signalled");
        assert!(st.plc_frames + st.fec_frames >= st.jitter.lost.saturating_sub(3 * 2));
        assert!(st.fec_frames > 0, "isolated holes must use FEC");
        assert!(out_energy > 50.0, "audio must keep flowing: {out_energy}");
    }
}
