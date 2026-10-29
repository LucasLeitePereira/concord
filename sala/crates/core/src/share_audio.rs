//! Sound of the PC inside a screen share (`screen-share`, `media-datagram` `ShareAudio`,
//! T-033f1).
//!
//! Sender: an [`AudioCapture`] (WASAPI process loopback, or a synthetic one in tests) hands
//! 20 ms stereo frames to [`ShareAudioEncoder`] (Opus, audio application, ~128 kbps); the
//! packets go out as `MediaDatagram::Audio { stream: ShareAudio }` to the video viewers.
//! Receiver: [`ShareAudioIn`] = `JitterBuffer` + stereo Opus decoder; the engine mixes it with
//! the voices under its own volume (`VoiceEngine::playout_tick`).

use std::sync::mpsc as std_mpsc;

use opus::{Application, Bitrate, Channels, Decoder, Encoder};

use crate::jitter::{JitterBuffer, JitterStats, Pop};
use crate::voice::{EncodedFrame, FRAME_SAMPLES, SAMPLE_RATE};

pub const CHANNELS: usize = 2;
/// Interleaved values of one 20 ms stereo frame.
pub const FRAME_VALUES: usize = FRAME_SAMPLES * CHANNELS;
pub const BITRATE_KBPS: u32 = 128;
const MAX_PACKET_BYTES: usize = 1275;

/// A running capture of the PC's sound: `frames` yields 20 ms stereo 48 kHz interleaved frames
/// (`FRAME_VALUES` values); dropping `guard` stops the capture.
pub struct AudioCapture {
    pub frames: std_mpsc::Receiver<Vec<f32>>,
    pub guard: Box<dyn Send>,
}

/// Opens an [`AudioCapture`]; runs on the thread that starts the share (it can take a while).
pub type AudioFactory = Box<dyn FnOnce() -> Result<AudioCapture, String> + Send>;

/// Turns a `StartShare { audio: true }` into a factory (tests inject a synthetic one).
pub type AudioPlanner = Box<dyn Fn() -> AudioFactory + Send>;

/// The real capture: WASAPI process loopback excluding this process. Never falls back to the
/// plain loopback, which would send the friends' voices back to them.
pub fn real_audio_planner() -> AudioPlanner {
    Box::new(|| {
        Box::new(|| {
            #[cfg(windows)]
            {
                crate::loopback::start()
            }
            #[cfg(not(windows))]
            {
                Err("system audio capture needs Windows".to_string())
            }
        })
    })
}

/// Opus stereo encoder with `seq` / `timestamp` numbering. Every frame is sent (no DTX): the
/// receiver's jitter buffer needs a steady cadence.
pub struct ShareAudioEncoder {
    enc: Encoder,
    seq: u16,
    timestamp: u32,
    buf: [u8; MAX_PACKET_BYTES],
}

impl ShareAudioEncoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self, opus::Error> {
        let mut enc = Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Audio)?;
        enc.set_bitrate(Bitrate::Bits((bitrate_kbps * 1000) as i32))?;
        Ok(Self { enc, seq: 0, timestamp: 0, buf: [0; MAX_PACKET_BYTES] })
    }

    /// Encodes one frame of exactly `FRAME_VALUES` interleaved samples.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<EncodedFrame, String> {
        if pcm.len() != FRAME_VALUES {
            return Err(format!("wrong frame size {}", pcm.len()));
        }
        let n = self.enc.encode_float(pcm, &mut self.buf).map_err(|e| e.to_string())?;
        let frame = EncodedFrame { seq: self.seq, timestamp: self.timestamp, payload: self.buf[..n].to_vec() };
        self.seq = self.seq.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(FRAME_SAMPLES as u32);
        Ok(frame)
    }
}

/// Averages the two channels of an interleaved stereo frame into mono (the output is mono).
pub fn stereo_to_mono(stereo: &[f32]) -> Vec<f32> {
    stereo.as_chunks::<CHANNELS>().0.iter().map(|p| (p[0] + p[1]) * 0.5).collect()
}

/// Receive side of one friend's share audio.
pub struct ShareAudioIn {
    jitter: JitterBuffer,
    dec: Decoder,
    /// Highest absolute sample of the decoded frames (a level meter for tests / diagnostics).
    peak: f32,
}

impl ShareAudioIn {
    pub fn new() -> Result<Self, opus::Error> {
        Ok(Self { jitter: JitterBuffer::default(), dec: Decoder::new(SAMPLE_RATE, Channels::Stereo)?, peak: 0.0 })
    }

    pub fn push(&mut self, seq: u16, timestamp: u32, payload: Vec<u8>, now_ms: u64) {
        self.jitter.push(seq, timestamp, payload, now_ms);
    }

    /// One 20 ms tick: the next frame as mono (`FRAME_SAMPLES`), `None` while buffering.
    pub fn pop_mono(&mut self, now_ms: u64) -> Option<Vec<f32>> {
        let stereo = match self.jitter.pop(now_ms) {
            Pop::Frame(payload) => self.decode(&payload, false),
            Pop::Lost => {
                let next = self.jitter.peek_next().map(<[u8]>::to_vec);
                match next {
                    Some(p) if !p.is_empty() => self.decode(&p, true),
                    _ => self.decode(&[], false),
                }
            }
            Pop::Empty => None,
        }?;
        self.peak = stereo.iter().fold(self.peak, |m, s| m.max(s.abs()));
        Some(stereo_to_mono(&stereo))
    }

    fn decode(&mut self, payload: &[u8], fec: bool) -> Option<Vec<f32>> {
        let mut out = vec![0.0f32; FRAME_VALUES];
        match self.dec.decode_float(payload, &mut out, fec) {
            Ok(n) if n > 0 => {
                out.truncate(n * CHANNELS);
                Some(out)
            }
            _ => None,
        }
    }

    pub fn stats(&self) -> JitterStats {
        self.jitter.stats()
    }

    /// Peak (linear, 0..1) of everything decoded so far.
    pub fn peak(&self) -> f32 {
        self.peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Left 440 Hz at `amp`, right silent.
    fn left_sine(frame_index: usize, amp: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(FRAME_VALUES);
        for i in 0..FRAME_SAMPLES {
            let t = (frame_index * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
            v.push((t * 440.0 * std::f32::consts::TAU).sin() * amp);
            v.push(0.0);
        }
        v
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn stereo_round_trip_keeps_the_energy_and_numbers_the_packets() {
        let mut enc = ShareAudioEncoder::new(BITRATE_KBPS).unwrap();
        let mut rx = ShareAudioIn::new().unwrap();
        let mut heard = Vec::new();
        // Real time: frame k arrives and tick k plays at 20 * k ms.
        for k in 0..60usize {
            let now = (k * 20) as u64;
            if k < 40 {
                let f = enc.encode(&left_sine(k, 0.5)).unwrap();
                assert_eq!(f.seq, k as u16);
                assert_eq!(f.timestamp, (k * FRAME_SAMPLES) as u32);
                assert!(f.payload.len() > 2 && f.payload.len() <= 1100, "len {}", f.payload.len());
                rx.push(f.seq, f.timestamp, f.payload, now);
            }
            if let Some(m) = rx.pop_mono(now) {
                assert_eq!(m.len(), FRAME_SAMPLES);
                heard.push(m);
            }
        }
        assert!(heard.len() >= 30, "only {} frames came out", heard.len());
        // Mono of (sine, 0) is a sine at half amplitude: rms ~ 0.5 * 0.707 / 2.
        let r = rms(&heard[10]);
        assert!((0.12..0.22).contains(&r), "rms {r}");
        assert!(rx.peak() > 0.4, "peak {}", rx.peak());
    }

    #[test]
    fn wrong_frame_size_is_refused() {
        let mut enc = ShareAudioEncoder::new(BITRATE_KBPS).unwrap();
        assert!(enc.encode(&[0.0; FRAME_SAMPLES]).is_err());
    }

    #[test]
    fn downmix_averages_the_channels() {
        assert_eq!(stereo_to_mono(&[1.0, 0.0, 0.5, 0.5]), vec![0.5, 0.5]);
    }
}
