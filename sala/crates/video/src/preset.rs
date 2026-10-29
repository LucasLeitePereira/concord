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
    /// Picked by the user in the UI: 16:9 box of `height` lines (480, 720, 1080 or 1440) at
    /// 30 or 60 fps. Build it with [`SharePreset::custom`].
    Custom { height: u16, fps: u8 },
}

/// Largest size the "sharp text" preset hands to the encoder.
const SHARP_TEXT_MAX: (u32, u32) = (3840, 2160);

impl SharePreset {
    /// The fixed presets (`Custom` is open-ended).
    pub const ALL: [SharePreset; 4] = [Self::P720p30, Self::P1080p30, Self::P1080p60, Self::SharpText];
    /// Heights the UI offers.
    pub const CUSTOM_HEIGHTS: [u32; 4] = [480, 720, 1080, 1440];

    /// A user-picked preset; `None` unless `height` is 480/720/1080/1440 and `fps` 30 or 60.
    pub fn custom(height: u32, fps: u32) -> Option<Self> {
        if !Self::CUSTOM_HEIGHTS.contains(&height) || !matches!(fps, 30 | 60) {
            return None;
        }
        Some(Self::Custom { height: height as u16, fps: fps as u8 })
    }

    /// Frame rate the encoder is paced at.
    pub fn fps(self) -> u32 {
        match self {
            Self::P720p30 | Self::P1080p30 => 30,
            Self::P1080p60 => 60,
            Self::SharpText => 15,
            Self::Custom { fps, .. } => u32::from(fps),
        }
    }

    /// Bounding box of the output: the source is shrunk to fit, never enlarged.
    pub fn max_size(self) -> (u32, u32) {
        match self {
            Self::P720p30 => (1280, 720),
            Self::P1080p30 | Self::P1080p60 => (1920, 1080),
            Self::SharpText => SHARP_TEXT_MAX,
            Self::Custom { height, .. } => ((u32::from(height) * 16).div_ceil(9), u32::from(height)),
        }
    }

    /// Bitrate range (kbps) for `codec`: H.264 numbers, x0.7 for HEVC and x0.6 for AV1.
    pub fn bitrate(self, codec: VideoCodec) -> BitrateRange {
        let (initial, min, max) = match self {
            Self::P720p30 => (3_000, 800, 5_000),
            Self::P1080p30 => (6_000, 1_500, 10_000),
            Self::P1080p60 => (9_000, 2_500, 15_000),
            Self::SharpText => (5_000, 1_000, 10_000),
            Self::Custom { height, fps } => {
                let (i, lo, hi) = if height <= 480 {
                    (1_500, 500, 2_500)
                } else if height <= 720 {
                    (3_000, 800, 5_000)
                } else if height <= 1080 {
                    (6_000, 1_500, 10_000)
                } else {
                    (10_000, 2_500, 16_000)
                };
                // 60 fps needs half as many bits again.
                if fps > 30 { (i * 3 / 2, lo * 3 / 2, hi * 3 / 2) } else { (i, lo, hi) }
            }
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
    fn custom_presets_follow_the_ui_choice_and_never_upscale() {
        assert_eq!(SharePreset::custom(480, 30).unwrap().output_size(1920, 1080), (852, 480));
        assert_eq!(SharePreset::custom(1440, 60).unwrap().output_size(3840, 2160), (2560, 1440));
        // Above the source: limited to the source, not enlarged.
        assert_eq!(SharePreset::custom(1440, 30).unwrap().output_size(1366, 768), (1366, 768));
        assert_eq!(SharePreset::custom(720, 60).unwrap().fps(), 60);
        // Values the UI does not offer are refused.
        assert!(SharePreset::custom(900, 30).is_none());
        assert!(SharePreset::custom(720, 45).is_none());
        for h in SharePreset::CUSTOM_HEIGHTS {
            for fps in [30, 60] {
                for codec in [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1] {
                    let p = SharePreset::custom(h, fps).unwrap();
                    let b = p.bitrate(codec);
                    assert!(b.min < b.initial && b.initial < b.max, "{p:?} {codec:?} {b:?}");
                }
            }
        }
        let (a, b) = (SharePreset::custom(720, 30).unwrap(), SharePreset::custom(720, 60).unwrap());
        assert!(b.bitrate(VideoCodec::H264).initial > a.bitrate(VideoCodec::H264).initial);
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
