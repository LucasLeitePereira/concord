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
use nnnoiseless::DenoiseState;

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
pub use crate::config::MAX_INPUT_VOLUME_PERCENT;

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

/// Level reported for digital silence instead of `-inf`.
pub const SILENCE_DB: f32 = -120.0;

/// Software gain of `audio.input_volume` (D-014): percent (clamped to 0..=200) to a factor.
pub fn input_gain(percent: u32) -> f32 {
    percent.min(MAX_INPUT_VOLUME_PERCENT) as f32 / 100.0
}

/// Applies the input gain and clamps every sample to [-1, 1]. Gain 1.0 is the identity.
pub fn apply_input_gain(samples: &mut [f32], gain: f32) {
    if gain == 1.0 {
        return;
    }
    for s in samples {
        *s = (*s * gain).clamp(-1.0, 1.0);
    }
}

/// Microphone level over a reporting interval, in dBFS (floor [`SILENCE_DB`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicLevels {
    /// Mean power of all captured frames.
    pub avg_db: f32,
    /// Largest absolute sample.
    pub peak_db: f32,
}

/// Accumulates the microphone level between two log lines.
#[derive(Debug, Clone, Copy, Default)]
struct LevelMeter {
    sum_sq: f64,
    samples: u64,
    peak: f32,
}

impl LevelMeter {
    fn add(&mut self, pcm: &[f32]) {
        self.sum_sq += pcm.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>();
        self.samples += pcm.len() as u64;
        self.peak = pcm.iter().fold(self.peak, |m, s| m.max(s.abs()));
    }

    fn take(&mut self) -> MicLevels {
        let to_db = |power: f64| if power > 0.0 { (10.0 * power.log10()) as f32 } else { SILENCE_DB };
        let avg = if self.samples > 0 { self.sum_sq / self.samples as f64 } else { 0.0 };
        let levels = MicLevels {
            avg_db: to_db(avg).max(SILENCE_DB),
            peak_db: to_db(f64::from(self.peak) * f64::from(self.peak)).max(SILENCE_DB),
        };
        *self = LevelMeter::default();
        levels
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

/// Extension point for noise suppression (`nnnoiseless`, off by default).
/// Runs on every captured 20 ms frame before activation and encoding.
pub trait FrameProcessor: Send {
    fn process(&mut self, frame: &mut [f32]);
}

/// RNNoise frame processor. The model expects 16-bit-scaled samples at 48 kHz.
pub struct RnnDenoiser {
    state: Box<DenoiseState<'static>>,
}

impl RnnDenoiser {
    pub fn new() -> Self {
        Self { state: DenoiseState::new() }
    }
}

impl Default for RnnDenoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameProcessor for RnnDenoiser {
    fn process(&mut self, frame: &mut [f32]) {
        if frame.len() != FRAME_SAMPLES {
            return;
        }
        let mut input = [0.0; DenoiseState::FRAME_SIZE];
        let mut output = [0.0; DenoiseState::FRAME_SIZE];
        for chunk in frame.as_chunks_mut::<{ DenoiseState::FRAME_SIZE }>().0 {
            for (dst, src) in input.iter_mut().zip(chunk.iter()) {
                *dst = (*src * 32768.0).clamp(-32768.0, 32767.0);
            }
            self.state.process_frame(&mut output, &input);
            for (dst, src) in chunk.iter_mut().zip(output.iter()) {
                *dst = (*src / 32768.0).clamp(-1.0, 1.0);
            }
        }
    }
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

    /// Changes the target bitrate of the running encoder (clamped to 32..=128 kbps).
    pub fn set_bitrate(&mut self, bitrate_kbps: u32) -> Result<(), opus::Error> {
        let kbps = bitrate_kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS);
        self.enc.set_bitrate(Bitrate::Bits((kbps * 1000) as i32))
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

    /// Switches voice-activity / push-to-talk; the key state and the hangover restart.
    pub fn set_mode(&mut self, mode: AudioMode) {
        if mode != self.mode {
            self.mode = mode;
            self.ptt = false;
            self.hangover = 0;
        }
    }

    /// Voice-activity threshold in dBFS.
    pub fn set_threshold_db(&mut self, threshold_db: i32) {
        self.threshold_db = threshold_db as f32;
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

/// Room rule (D-015): voice flows with a friend only when both sides are in the room.
pub fn voice_allowed(local_in_room: bool, peer_in_room: bool) -> bool {
    local_in_room && peer_in_room
}

/// Per-friend receive state.
struct Peer {
    jitter: JitterBuffer,
    decoder: VoiceDecoder,
    volume: f32,
    /// The friend announced it is in the room (assumed until a `Status` says otherwise).
    in_room: bool,
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
    /// `audio.input_volume` as a factor (D-014).
    input_gain: f32,
    meter: LevelMeter,
    /// Separate accumulator for the UI meter (does not disturb the 2 s log), only fed while on.
    ui_meter: LevelMeter,
    ui_meter_on: bool,
    /// The local user is in the room (D-015).
    in_room: bool,
}

impl VoiceEngine {
    pub fn new(cfg: &AudioConfig) -> Result<Self, opus::Error> {
        Ok(Self {
            activation: Activation::new(cfg.mode, cfg.vad_threshold),
            encoder: VoiceEncoder::new(cfg.bitrate_kbps)?,
            denoiser: cfg.noise_suppression.then(|| Box::new(RnnDenoiser::new()) as Box<dyn FrameProcessor>),
            peers: HashMap::new(),
            stats: VoiceStats::default(),
            loopback: None,
            input_gain: input_gain(cfg.input_volume),
            meter: LevelMeter::default(),
            ui_meter: LevelMeter::default(),
            ui_meter_on: false,
            in_room: true,
        })
    }

    /// Applies a changed `[audio]` section live (devices are the caller's business).
    pub fn apply_audio(&mut self, cfg: &AudioConfig) -> Result<(), opus::Error> {
        self.activation.set_mode(cfg.mode);
        self.activation.set_threshold_db(cfg.vad_threshold);
        self.input_gain = input_gain(cfg.input_volume);
        if cfg.noise_suppression {
            if self.denoiser.is_none() {
                self.denoiser = Some(Box::new(RnnDenoiser::new()));
            }
        } else {
            self.denoiser = None;
        }
        self.encoder.set_bitrate(cfg.bitrate_kbps)
    }

    /// Local presence in the room (D-015).
    pub fn set_in_room(&mut self, in_room: bool) {
        self.in_room = in_room;
    }

    /// What `friend` announced about its presence in the room.
    pub fn set_peer_in_room(&mut self, friend: &str, in_room: bool) {
        if let Some(p) = self.peers.get_mut(friend) {
            p.in_room = in_room;
        }
    }

    /// Friends that must receive our voice now (both sides in the room).
    pub fn send_targets(&self) -> Vec<String> {
        self.peers
            .iter()
            .filter(|(_, p)| voice_allowed(self.in_room, p.in_room))
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Turns the UI meter on/off; turning it on starts from an empty interval.
    pub fn set_ui_meter(&mut self, on: bool) {
        if on && !self.ui_meter_on {
            self.ui_meter = LevelMeter::default();
        }
        self.ui_meter_on = on;
    }

    /// RMS level (dBFS) of the frames since the last call; starts a new interval.
    pub fn take_ui_level(&mut self) -> f32 {
        self.ui_meter.take().avg_db
    }

    /// Mic level since the last call (for the periodic log); starts a new interval.
    pub fn take_mic_levels(&mut self) -> MicLevels {
        self.meter.take()
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

    /// Starts the receive side of a call with `friend`. An existing peer is kept
    /// (jitter buffer and volume survive a replacement connection).
    pub fn add_peer(&mut self, friend: &str) -> Result<(), opus::Error> {
        if self.peers.contains_key(friend) {
            return Ok(());
        }
        let peer = Peer {
            jitter: JitterBuffer::default(),
            decoder: VoiceDecoder::new()?,
            volume: 1.0,
            in_room: true,
        };
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
        apply_input_gain(pcm, self.input_gain);
        if let Some(d) = self.denoiser.as_mut() {
            d.process(pcm);
        }
        self.meter.add(pcm);
        if self.ui_meter_on {
            self.ui_meter.add(pcm);
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
            // Out of the room (either side): the buffer is drained but nothing is heard.
            if let Some(mut frame) = decoded
                && voice_allowed(self.in_room, p.in_room)
            {
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
    fn rnnoise_reduces_low_level_white_noise() {
        let mut denoiser = RnnDenoiser::new();
        let mut state = 0x1234_5678u32;
        let mut input_energy = 0.0;
        let mut output_energy = 0.0;
        for frame_index in 0..50 {
            let mut input: Vec<f32> = (0..FRAME_SAMPLES)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((state >> 8) as f32 / 16_777_215.0 * 2.0 - 1.0) * 0.03
                })
                .collect();
            let original = input.clone();
            denoiser.process(&mut input);
            assert_eq!(input.len(), FRAME_SAMPLES);
            assert!(input.iter().all(|sample| (-1.0..=1.0).contains(sample)));
            if frame_index >= 40 {
                input_energy += rms(&original).powi(2);
                output_energy += rms(&input).powi(2);
            }
        }
        let reduction_db = 10.0 * (output_energy / input_energy).log10();
        let input_dbfs = 10.0 * (input_energy / 10.0).log10();
        let output_dbfs = 10.0 * (output_energy / 10.0).log10();
        println!("white noise: {input_dbfs:.1} dBFS -> {output_dbfs:.1} dBFS ({reduction_db:.1} dB reduction)");
        assert!(reduction_db <= -10.0, "noise reduction was {reduction_db:.1} dB");
    }

    #[test]
    fn rnnoise_keeps_a_440_hz_tone_nonzero() {
        let mut denoiser = RnnDenoiser::new();
        let mut source = SineSource::new(440.0, -20.0);
        let mut final_rms = 0.0;
        for _ in 0..50 {
            let mut frame = source.next_frame();
            denoiser.process(&mut frame);
            assert_eq!(frame.len(), FRAME_SAMPLES);
            assert!(frame.iter().all(|sample| (-1.0..=1.0).contains(sample)));
            final_rms = rms(&frame);
        }
        println!("440 Hz tone output: {:.1} dBFS", 20.0 * final_rms.log10());
        assert!(final_rms > 1.0e-5, "RNNoise removed the tone: RMS {final_rms}");
    }

    #[test]
    fn audio_config_toggles_rnnoise_without_resetting_an_enabled_state() {
        let mut cfg = AudioConfig { noise_suppression: true, ..AudioConfig::default() };
        let mut engine = VoiceEngine::new(&cfg).unwrap();
        assert!(engine.denoiser.is_some(), "enabled at engine creation");
        let state_ptr = engine.denoiser.as_deref().unwrap() as *const dyn FrameProcessor;
        engine.apply_audio(&cfg).unwrap();
        assert_eq!(state_ptr, engine.denoiser.as_deref().unwrap() as *const dyn FrameProcessor);

        cfg.noise_suppression = false;
        engine.apply_audio(&cfg).unwrap();
        assert!(engine.denoiser.is_none(), "disabled live");
        cfg.noise_suppression = true;
        engine.apply_audio(&cfg).unwrap();
        assert!(engine.denoiser.is_some(), "re-enabled live");
    }

    #[test]
    fn rnnoise_ignores_unexpected_frame_sizes() {
        let mut denoiser = RnnDenoiser::new();
        let mut frame = vec![2.0; FRAME_SAMPLES - 1];
        denoiser.process(&mut frame);
        assert_eq!(frame, vec![2.0; FRAME_SAMPLES - 1]);
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
    fn input_gain_scales_and_clips() {
        assert_eq!(input_gain(100), 1.0);
        assert_eq!(input_gain(200), 2.0);
        assert_eq!(input_gain(0), 0.0);
        assert_eq!(input_gain(9999), 2.0, "above the ceiling is clamped");
        let mut s = vec![0.25, -0.25, 0.9, -0.9];
        apply_input_gain(&mut s, 2.0);
        assert_eq!(s, vec![0.5, -0.5, 1.0, -1.0], "doubled, with clipping at full scale");
        let mut half = vec![0.5, -0.5];
        apply_input_gain(&mut half, 0.5);
        assert_eq!(half, vec![0.25, -0.25]);
    }

    #[test]
    fn hundred_percent_is_identity() {
        let orig = vec![0.123, -0.987, 0.0, 1.0, -1.0];
        let mut s = orig.clone();
        apply_input_gain(&mut s, input_gain(100));
        assert_eq!(s, orig);
    }

    #[test]
    fn engine_applies_gain_before_activation_and_meter() {
        // Sine with RMS about -51 dBFS: under the default -50 dB threshold at 100 %,
        // about -45 dB (over it) at 200 %.
        let quiet: Vec<f32> = (0..FRAME_SAMPLES)
            .map(|i| 0.004 * (i as f32 * std::f32::consts::TAU * 440.0 / SAMPLE_RATE as f32).sin())
            .collect();
        let run = |percent: u32| {
            let cfg = AudioConfig { input_volume: percent, ..AudioConfig::default() };
            let mut e = VoiceEngine::new(&cfg).unwrap();
            let mut f = quiet.clone();
            let sent = e.process_capture(&mut f).unwrap().is_some();
            (sent, e.take_mic_levels(), f[10])
        };
        let (sent_100, lv_100, s_100) = run(100);
        let (sent_200, lv_200, s_200) = run(200);
        assert!(!sent_100, "control: quiet frame is gated at 100 %");
        assert!(sent_200, "gain lifts it over the threshold");
        assert_eq!(s_100, quiet[10]);
        assert!((s_200 - 2.0 * quiet[10]).abs() < 1e-7);
        assert!((lv_200.avg_db - lv_100.avg_db - 6.02).abs() < 0.1, "{lv_100:?} {lv_200:?}");
        assert!((lv_200.peak_db - lv_100.peak_db - 6.02).abs() < 0.1);
    }

    #[test]
    fn mic_levels_report_avg_peak_floor_and_reset() {
        let mut e = engine();
        let silent = e.take_mic_levels();
        assert_eq!(silent, MicLevels { avg_db: SILENCE_DB, peak_db: SILENCE_DB }, "no frames: floor, not -inf");
        e.process_capture(&mut vec![0.0f32; FRAME_SAMPLES]).unwrap();
        assert_eq!(e.take_mic_levels().peak_db, SILENCE_DB, "digital silence: floor");
        let mut f = vec![0.0f32; FRAME_SAMPLES];
        f[0] = 0.5; // one spike
        e.process_capture(&mut f).unwrap();
        let lv = e.take_mic_levels();
        assert!((lv.peak_db - -6.02).abs() < 0.05, "{lv:?}");
        assert!(lv.avg_db < lv.peak_db - 25.0, "average well under the peak: {lv:?}");
        assert_eq!(e.take_mic_levels().peak_db, SILENCE_DB, "interval was reset");
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

    /// Energy heard from one peer over 30 frames, with the given room presences.
    fn heard_from_peer(local_in_room: bool, peer_in_room: bool) -> (f32, usize) {
        let mut e = engine();
        e.add_peer("a").unwrap();
        e.set_in_room(local_in_room);
        e.set_peer_in_room("a", peer_in_room);
        let mut enc = VoiceEncoder::new(96).unwrap();
        let mut src = SineSource::standard();
        let mut heard = 0.0f32;
        for i in 0..30u64 {
            let f = enc.encode(&src.next_frame()).unwrap().unwrap();
            e.push_datagram("a", f.seq, f.timestamp, f.payload, i * 20);
            heard += rms(&e.playout_tick(i * 20 + 100));
        }
        (heard, e.send_targets().len())
    }

    #[test]
    fn voice_flows_only_when_both_are_in_the_room() {
        let (heard, targets) = heard_from_peer(true, true);
        assert!(heard > 1.0, "control: both in the room is audible ({heard})");
        assert_eq!(targets, 1);
        for (local, peer) in [(true, false), (false, true), (false, false)] {
            let (heard, targets) = heard_from_peer(local, peer);
            assert_eq!(heard, 0.0, "local {local} peer {peer}: nothing may be played");
            assert_eq!(targets, 0, "local {local} peer {peer}: nothing may be sent");
        }
        assert!(voice_allowed(true, true));
        assert!(!voice_allowed(true, false) && !voice_allowed(false, true) && !voice_allowed(false, false));
    }

    #[test]
    fn leaving_the_room_still_drains_the_jitter_buffer() {
        let mut e = engine();
        e.add_peer("a").unwrap();
        e.set_in_room(false);
        let mut enc = VoiceEncoder::new(96).unwrap();
        let mut src = SineSource::standard();
        for i in 0..30u64 {
            let f = enc.encode(&src.next_frame()).unwrap().unwrap();
            e.push_datagram("a", f.seq, f.timestamp, f.payload, i * 20);
            e.playout_tick(i * 20 + 100);
        }
        assert!(e.peer_stats("a").unwrap().jitter.played > 20, "frames are consumed while out of the room");
    }

    #[test]
    fn ui_meter_runs_only_when_on_and_is_independent_of_the_log_meter() {
        let mut e = engine();
        let frame = || vec![0.1f32; FRAME_SAMPLES]; // -20 dBFS
        e.process_capture(&mut frame()).unwrap();
        assert_eq!(e.take_ui_level(), SILENCE_DB, "off: nothing accumulated");
        e.set_ui_meter(true);
        e.process_capture(&mut frame()).unwrap();
        let first = e.take_ui_level();
        assert!((first - -20.0).abs() < 0.1, "{first}");
        assert_eq!(e.take_ui_level(), SILENCE_DB, "the interval was reset");
        // The 2 s log meter saw both frames and is not reset by the UI meter.
        e.process_capture(&mut frame()).unwrap();
        e.take_ui_level();
        let log = e.take_mic_levels();
        assert!((log.avg_db - -20.0).abs() < 0.1, "{log:?}");
        // And the other way round.
        e.process_capture(&mut frame()).unwrap();
        e.take_mic_levels();
        assert!((e.take_ui_level() - -20.0).abs() < 0.1);
        e.set_ui_meter(false);
        e.process_capture(&mut frame()).unwrap();
        assert_eq!(e.take_ui_level(), SILENCE_DB);
    }

    #[test]
    fn apply_audio_changes_threshold_mode_gain_and_bitrate_live() {
        let quiet: Vec<f32> = (0..FRAME_SAMPLES)
            .map(|i| 0.004 * (i as f32 * std::f32::consts::TAU * 440.0 / SAMPLE_RATE as f32).sin())
            .collect(); // about -51 dBFS
        let mut e = engine();
        assert!(e.process_capture(&mut quiet.clone()).unwrap().is_none(), "control: gated at -50");
        let mut cfg = AudioConfig { vad_threshold: -60, ..AudioConfig::default() };
        e.apply_audio(&cfg).unwrap();
        assert!(e.process_capture(&mut quiet.clone()).unwrap().is_some(), "lower threshold lets it through");
        cfg.mode = AudioMode::PushToTalk;
        e.apply_audio(&cfg).unwrap();
        assert!(e.process_capture(&mut vec![0.5f32; FRAME_SAMPLES]).unwrap().is_none(), "PTT without the key");
        e.activation_mut().set_ptt(true);
        assert!(e.process_capture(&mut quiet.clone()).unwrap().is_some());
        cfg.mode = AudioMode::VoiceActivity;
        cfg.vad_threshold = -50;
        cfg.input_volume = 200;
        e.apply_audio(&cfg).unwrap();
        assert!(e.process_capture(&mut quiet.clone()).unwrap().is_some(), "gain 200 % lifts it over -50");
        cfg.bitrate_kbps = 40;
        e.apply_audio(&cfg).unwrap();
    }

    #[test]
    fn add_peer_twice_keeps_the_existing_peer() {
        let mut e = VoiceEngine::new(&AudioConfig::default()).unwrap();
        e.add_peer("a").unwrap();
        e.push_datagram("a", 1, 0, vec![0u8; 3], 0);
        e.set_peer_volume("a", 0.5);
        e.add_peer("a").unwrap();
        assert_eq!(e.peer_stats("a").unwrap().jitter.received, 1, "buffer survives");
        assert_eq!(e.peers["a"].volume, 0.5);
    }
}
