//! The codecs and encoders this project recognises, and the size of a picture.

use core::fmt;

use clipped_logging::VideoEncoder;
use serde::{Deserialize, Serialize};

/// A video codec Clipped can be asked to produce (SPEC.md section 9).
///
/// Ordered least to most efficient, which is the order
/// [`EFFICIENCY_ORDER`](Self::EFFICIENCY_ORDER) reverses and the recommendation
/// in [`crate::recommendation`] walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// H.264 / AVC. Universally supported, least efficient, and the only codec
    /// every encoder in this list can produce.
    H264,
    /// H.265 / HEVC.
    Hevc,
    /// AV1.
    Av1,
}

impl Codec {
    /// Every codec, most efficient first.
    ///
    /// "Most efficient" means fewest bits for the same picture, which is the
    /// axis that matters for a recorder writing hours of video to a local disk
    /// (SPEC.md section 10). It is not the order of compatibility: a clip
    /// encoded with AV1 will not play everywhere H.264 will, which is why the
    /// user can still pin a codec.
    pub const EFFICIENCY_ORDER: [Self; 3] = [Self::Av1, Self::Hevc, Self::H264];

    /// How this codec is written in a structured log field and in the cache
    /// file: stable, lowercase and machine-searchable.
    #[must_use]
    pub const fn log_value(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }
}

impl fmt::Display for Codec {
    /// The label shown to a user, as in `Codec: H.264`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Av1 => "AV1",
        })
    }
}

/// A way of encoding video: three hardware families and the CPU (SPEC.md
/// section 9).
///
/// This names a *family*, not an implementation. A variant existing here does
/// not mean this build can encode with it — nothing can yet, which
/// [`backend_issue`](Self::backend_issue) says in the only way that stays true
/// as the backends land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderKind {
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD Advanced Media Framework.
    Amf,
    /// Intel Quick Sync Video.
    QuickSync,
    /// The CPU. A fallback rather than an equal option: it takes frames from
    /// the game being recorded (SPEC.md section 9, AGENTS.md section 18).
    Software,
}

impl EncoderKind {
    /// Every encoder, in the order SPEC.md section 9 lists them.
    ///
    /// This is the published order — what the settings screen shows and what
    /// the capability report prints. It is not by itself the recommendation
    /// order, which also weighs whether an adapter is discrete; see
    /// [`crate::recommendation`].
    pub const ALL: [Self; 4] = [Self::Nvenc, Self::Amf, Self::QuickSync, Self::Software];

    /// The GPU vendor whose hardware this encoder lives on, or `None` for the
    /// software encoder, which needs no particular adapter.
    #[must_use]
    pub const fn vendor(self) -> Option<Vendor> {
        match self {
            Self::Nvenc => Some(Vendor::Nvidia),
            Self::Amf => Some(Vendor::Amd),
            Self::QuickSync => Some(Vendor::Intel),
            Self::Software => None,
        }
    }

    /// Whether this encoder runs on dedicated silicon rather than on the CPU.
    #[must_use]
    pub const fn is_hardware(self) -> bool {
        !matches!(self, Self::Software)
    }

    /// The issue that implements this encoder.
    ///
    /// None of them are implemented: this ticket detects encoders and reports
    /// them, and the backends follow. The report prints this so that "your GPU
    /// can do AV1, and Clipped cannot yet" is one sentence with a link in it,
    /// rather than a silence the reader has to interpret (AGENTS.md section
    /// 27).
    #[must_use]
    pub const fn backend_issue(self) -> u32 {
        match self {
            Self::Nvenc => 15,
            Self::Amf => 16,
            Self::QuickSync => 17,
            Self::Software => 18,
        }
    }

    /// How this encoder is written in the `encoder` field of a diagnostic
    /// (docs/logging.md, "Standard fields").
    ///
    /// The vocabulary distinguishes software encoders by codec and hardware
    /// encoders by family, so this takes the codec and is total for every pair
    /// the report can produce. `None` means the log format has no word for the
    /// combination — software AV1, which nothing in this project plans to
    /// build — and a field with no word is left off the line rather than
    /// invented, which is what docs/logging.md asks for.
    #[must_use]
    pub const fn log_encoder(self, codec: Codec) -> Option<VideoEncoder> {
        match (self, codec) {
            (Self::Nvenc, _) => Some(VideoEncoder::Nvenc),
            (Self::Amf, _) => Some(VideoEncoder::AmdAmf),
            (Self::QuickSync, _) => Some(VideoEncoder::IntelQuickSync),
            (Self::Software, Codec::H264) => Some(VideoEncoder::SoftwareH264),
            (Self::Software, Codec::Hevc) => Some(VideoEncoder::SoftwareH265),
            (Self::Software, Codec::Av1) => None,
        }
    }

    /// The `encoder` field word for a diagnostic about the family as a whole,
    /// rather than about one codec.
    ///
    /// The logging vocabulary distinguishes software encoders by codec and
    /// hardware encoders by family, so a line about "the software encoder" has
    /// to pick one of its two words. It picks H.264, which is the software
    /// fallback SPEC.md section 9 names and the only codec the software
    /// encoder is expected to produce (see [`crate::reference`]). A build that
    /// gains a software HEVC encoder should log that one per codec rather than
    /// widen this.
    #[must_use]
    pub const fn log_encoder_family(self) -> VideoEncoder {
        match self {
            Self::Nvenc => VideoEncoder::Nvenc,
            Self::Amf => VideoEncoder::AmdAmf,
            Self::QuickSync => VideoEncoder::IntelQuickSync,
            Self::Software => VideoEncoder::SoftwareH264,
        }
    }
}

impl fmt::Display for EncoderKind {
    /// The label shown to a user, as in `Encoder: NVIDIA NVENC`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Nvenc => "NVIDIA NVENC",
            Self::Amf => "AMD AMF",
            Self::QuickSync => "Intel Quick Sync",
            Self::Software => "Software (CPU)",
        })
    }
}

/// The PCI vendor of a display adapter.
///
/// Only the three vendors with a hardware encoder get a name, plus Microsoft,
/// whose identifier belongs to the Basic Render Driver rather than to any
/// silicon. Everything else is [`Other`](Self::Other) with its identifier
/// intact, so an unrecognised adapter is still reported rather than dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    /// NVIDIA, PCI vendor `0x10DE`.
    Nvidia,
    /// AMD, PCI vendor `0x1002`.
    Amd,
    /// Intel, PCI vendor `0x8086`.
    Intel,
    /// Microsoft, PCI vendor `0x1414`: the software rasteriser, not a GPU.
    Microsoft,
    /// Anything else, by its PCI vendor identifier.
    Other(u32),
}

impl Vendor {
    /// PCI vendor identifier for NVIDIA.
    pub const NVIDIA: u32 = 0x10DE;
    /// PCI vendor identifier for AMD.
    pub const AMD: u32 = 0x1002;
    /// PCI vendor identifier for Intel.
    pub const INTEL: u32 = 0x8086;
    /// PCI vendor identifier Microsoft uses for the Basic Render Driver.
    pub const MICROSOFT: u32 = 0x1414;

    /// Recognises a PCI vendor identifier, as DXGI and Media Foundation both
    /// report it.
    #[must_use]
    pub const fn from_pci_id(id: u32) -> Self {
        match id {
            Self::NVIDIA => Self::Nvidia,
            Self::AMD => Self::Amd,
            Self::INTEL => Self::Intel,
            Self::MICROSOFT => Self::Microsoft,
            other => Self::Other(other),
        }
    }

    /// The PCI vendor identifier this was recognised from.
    #[must_use]
    pub const fn pci_id(self) -> u32 {
        match self {
            Self::Nvidia => Self::NVIDIA,
            Self::Amd => Self::AMD,
            Self::Intel => Self::INTEL,
            Self::Microsoft => Self::MICROSOFT,
            Self::Other(id) => id,
        }
    }
}

impl fmt::Display for Vendor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nvidia => formatter.write_str("NVIDIA"),
            Self::Amd => formatter.write_str("AMD"),
            Self::Intel => formatter.write_str("Intel"),
            Self::Microsoft => formatter.write_str("Microsoft"),
            Self::Other(id) => write!(formatter, "PCI vendor 0x{id:04X}"),
        }
    }
}

/// The size of an encoded picture, in pixels.
///
/// `clipped-capture` has a `FrameSize` of its own for the same pair of numbers.
/// They are not shared because the two crates sit in the same layer and neither
/// may depend on the other (README, "Dependency direction"), and because they
/// answer different questions: that one is the size a frame arrived at, this
/// one is a limit an encoder documents. `clipped-session` converts, being the
/// crate above both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Resolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Resolution {
    /// 1920x1080, the resolution the report quotes framerate ceilings at
    /// because it is the one most recordings are made at (SPEC.md section 10).
    pub const HD_1080P: Self = Self::new(1920, 1080);

    /// 3840x2160.
    pub const UHD_4K: Self = Self::new(3840, 2160);

    /// A size.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Luma samples in one picture, which is what codec level limits are
    /// expressed against.
    #[must_use]
    pub const fn luma_samples(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// Whether a picture this size fits inside `limit` in both dimensions.
    #[must_use]
    pub const fn fits_within(self, limit: Self) -> bool {
        self.width <= limit.width && self.height <= limit.height
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}x{}", self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_are_ordered_by_efficiency_rather_than_by_age() {
        assert_eq!(
            Codec::EFFICIENCY_ORDER,
            [Codec::Av1, Codec::Hevc, Codec::H264]
        );
    }

    #[test]
    fn encoders_are_listed_in_the_order_the_specification_lists_them() {
        // SPEC.md section 9: NVIDIA, AMD, Intel, then the software fallback.
        assert_eq!(
            EncoderKind::ALL,
            [
                EncoderKind::Nvenc,
                EncoderKind::Amf,
                EncoderKind::QuickSync,
                EncoderKind::Software,
            ]
        );
    }

    #[test]
    fn log_values_match_the_logging_field_vocabulary() {
        // Compared against the real enumeration rather than against strings
        // copied out of it: a log search for `encoder=nvenc` has to find both
        // this crate's detection lines and a session's, and two
        // hand-maintained lists of the same words drift apart
        // (docs/logging.md, "Standard fields").
        for kind in EncoderKind::ALL {
            for codec in Codec::EFFICIENCY_ORDER {
                // Exhaustive on purpose: a new pair has to state its
                // counterpart here, or say why it has none.
                let expected = match (kind, codec) {
                    (EncoderKind::Nvenc, _) => Some("nvenc"),
                    (EncoderKind::Amf, _) => Some("amd_amf"),
                    (EncoderKind::QuickSync, _) => Some("intel_quicksync"),
                    (EncoderKind::Software, Codec::H264) => Some("software_h264"),
                    (EncoderKind::Software, Codec::Hevc) => Some("software_h265"),
                    // The log format has no word for a software AV1 encoder
                    // because nothing plans to build one, and a word in the
                    // format promising an encoder that does not exist is worse
                    // than a missing field.
                    (EncoderKind::Software, Codec::Av1) => None,
                };

                assert_eq!(
                    kind.log_encoder(codec).map(|encoder| encoder.to_string()),
                    expected.map(str::to_owned),
                    "{kind} with {codec} is logged as one word here and another by \
                     clipped-logging, so no log search finds both"
                );
            }
        }
    }

    #[test]
    fn vendor_identifiers_survive_a_round_trip() {
        for vendor in [
            Vendor::Nvidia,
            Vendor::Amd,
            Vendor::Intel,
            Vendor::Microsoft,
            Vendor::Other(0x1234),
        ] {
            assert_eq!(Vendor::from_pci_id(vendor.pci_id()), vendor);
        }
    }

    #[test]
    fn an_unrecognised_adapter_keeps_its_identifier_rather_than_disappearing() {
        let vendor = Vendor::from_pci_id(0x1AF4);
        assert_eq!(vendor, Vendor::Other(0x1AF4));
        assert_eq!(vendor.to_string(), "PCI vendor 0x1AF4");
    }

    #[test]
    fn a_resolution_knows_its_luma_samples_and_whether_it_fits_a_limit() {
        assert_eq!(Resolution::HD_1080P.luma_samples(), 1920 * 1080);
        assert!(Resolution::HD_1080P.fits_within(Resolution::UHD_4K));
        assert!(!Resolution::UHD_4K.fits_within(Resolution::HD_1080P));
        // Neither dimension may exceed the limit, even when the area does fit.
        assert!(!Resolution::new(4096, 100).fits_within(Resolution::UHD_4K));
    }
}
