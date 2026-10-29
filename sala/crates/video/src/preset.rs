//! Screen-share quality presets (`share-preset`): size, frame rate and bitrate range.
//!
//! Pure data, no FFmpeg. Bitrates are tuned for low-latency hardware H.264 and scaled for
//! the more efficient codecs.

use sala_protocol::VideoCodec;

use crate::encoder::EncoderConfig;

/// Bitrate range of a preset for one codec, in kbps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitrateRange {
    pub initial: u32,
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SharePreset {
    /// 1280x720 at 30 fps.
    P720p30,
    /// 1920x1080 at 30 fps.
    P1080p30,
    /// 1920x1080 at 60 fps.
    P1080p60,
    /// Native resolution (up to 4K), at most 15 fps: documents and code stay sharp.
    SharpText,
}

/// Largest size the "sharp text" preset hands to the encoder.
const SHARP_TEXT_MAX: (u32, u32) = (3840, 2160);

impl SharePreset {
    pub const ALL: [SharePreset; 4] = [Self::P720p30, Self::P1080p30, Self::P1080p60, Self::SharpText];

    /// Frame rate the encoder is paced at.
    pub fn fps(self) -> u32 {
        match self {
            Self::P720p30 | Self::P1080p30 => 30,
            Self::P1080p60 => 60,
            Self::SharpText => 15,
        }
    }

    /// Bounding box of the output: the source is shrunk to fit, never enlarged.
    pub fn max_size(self) -> (u32, u32) {
        match self {
            Self::P720p30 => (1280, 720),
            Self::P1080p30 | Self::P1080p60 => (1920, 1080),
            Self::SharpText => SHARP_TEXT_MAX,
        }
    }

    /// Bitrate range (kbps) for `codec`: H.264 numbers, x0.7 for HEVC and x0.6 for AV1.
    pub fn bitrate(self, codec: VideoCodec) -> BitrateRange {
        let (initial, min, max) = match self {
            Self::P720p30 => (3_000, 800, 5_000),
            Self::P1080p30 => (6_000, 1_500, 10_000),
            Self::P1080p60 => (9_000, 2_500, 15_000),
            Self::SharpText => (5_000, 1_000, 10_000),
        };
        let (num, den) = match codec {
            VideoCodec::H264 => (10, 10),
            VideoCodec::Hevc => (7, 10),
            VideoCodec::Av1 => (6, 10),
        };
        let s = |v: u32| v * num / den;
        BitrateRange { initial: s(initial), min: s(min), max: s(max) }
    }

    /// Output size for a `src_w` x `src_h` source: fits the preset's box keeping the aspect
    /// ratio, never upscales, and is even in both directions (encoder requirement).
    pub fn output_size(self, src_w: u32, src_h: u32) -> (u32, u32) {
        let (mw, mh) = self.max_size();
        let (sw, sh) = (u64::from(src_w.max(1)), u64::from(src_h.max(1)));
        let (mw, mh) = (u64::from(mw), u64::from(mh));
        // Integer math: floats could land on 1919.999.
        let (w, h) = if sw <= mw && sh <= mh {
            (sw, sh)
        } else if sw * mh >= sh * mw {
            (mw, sh * mw / sw) // width is the limit
        } else {
            (sw * mh / sh, mh)
        };
        let even = |v: u64| ((v as u32) & !1).max(2);
        (even(w), even(h))
    }

    /// Encoder configuration for this preset and a source of `src_w` x `src_h` pixels.
    pub fn encoder_config(self, codec: VideoCodec, src_w: u32, src_h: u32) -> EncoderConfig {
        let (width, height) = self.output_size(src_w, src_h);
        EncoderConfig { codec, width, height, fps: self.fps(), bitrate_kbps: self.bitrate(codec).initial }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_upscales_and_stays_even() {
        // A 1366x768 laptop in 1080p30 stays native (rounded to even), not 1920x1080.
        assert_eq!(SharePreset::P1080p30.output_size(1366, 768), (1366, 768));
        assert_eq!(SharePreset::P1080p30.output_size(1281, 721), (1280, 720));
        // 4K shrinks keeping 16:9.
        assert_eq!(SharePreset::P1080p60.output_size(3840, 2160), (1920, 1080));
        assert_eq!(SharePreset::P720p30.output_size(1920, 1080), (1280, 720));
        // Ultrawide: width is the limit.
        assert_eq!(SharePreset::P1080p30.output_size(3440, 1440), (1920, 802));
        // Tiny and degenerate sources never reach zero.
        assert_eq!(SharePreset::P720p30.output_size(1, 1), (2, 2));
        assert_eq!(SharePreset::P720p30.output_size(0, 0), (2, 2));
    }

    #[test]
    fn sharp_text_is_native_and_slow() {
        let c = SharePreset::SharpText.encoder_config(VideoCodec::H264, 2560, 1440);
        assert_eq!((c.width, c.height, c.fps), (2560, 1440, 15));
        let c = SharePreset::SharpText.encoder_config(VideoCodec::H264, 7680, 4320);
        assert_eq!((c.width, c.height), (3840, 2160));
    }

    #[test]
    fn every_preset_is_consistent() {
        for p in SharePreset::ALL {
            for codec in [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1] {
                let b = p.bitrate(codec);
                assert!(b.min < b.initial && b.initial < b.max, "{p:?} {codec:?} {b:?}");
                let c = p.encoder_config(codec, 1920, 1080);
                assert_eq!(c.bitrate_kbps, b.initial);
                assert_eq!(c.width % 2 + c.height % 2, 0);
                assert!(c.fps > 0 && c.fps <= 60);
            }
        }
        // Better codecs need fewer bits.
        let h264 = SharePreset::P1080p30.bitrate(VideoCodec::H264);
        assert!(SharePreset::P1080p30.bitrate(VideoCodec::Hevc).max < h264.max);
    }
}
