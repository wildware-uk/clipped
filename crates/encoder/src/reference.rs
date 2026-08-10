//! The table of published limits, and the rule that keeps it honest.
//!
//! Nothing in this file is measured. It exists because some questions a user
//! asks — "how large a picture will this encoder take?" — have no cheap
//! measurement behind them, and answering "we do not know" to all of them would
//! be useless as well as true.
//!
//! # The rule
//!
//! **A capability whose absence would break a recording is never inferred as
//! present.** Concretely: this table says [`Claim::Inferred(true)`] for
//! *codec support* only where it holds for every generation of silicon the
//! vendor's runtime will load on, which in practice means H.264 and nothing
//! else. HEVC and AV1 are [`Claim::Unknown`] here, and only ever become
//! [`Claim::Measured`] when the operating system reports a hardware encoder for
//! them ([`crate::probe::HardwareEncoder`]).
//!
//! That is the whole answer to the failure mode this table would otherwise
//! have. A user cannot pick AV1 on the strength of a table entry, because there
//! is no table entry that says yes to AV1.
//!
//! # What the numbers are
//!
//! | Field | Where it comes from |
//! | --- | --- |
//! | `max_resolution` | The vendor's own documentation, taking the narrowest figure across the generations their current runtime supports. |
//! | `max_luma_samples_per_second` | The codec standard's level limit — H.264 Level 6.2 (ITU-T H.264 Table A-1), HEVC Level 6.2 High tier (ITU-T H.265 Table A.8), AV1 Level 6.3 (AOMedia AV1 Annex A). |
//! | `b_frames`, `hdr` | Vendor documentation, again narrowed to what holds across generations; `Unknown` wherever it varies. |
//!
//! Two things follow from that and are worth saying in the open, because the
//! report repeats them:
//!
//! - A framerate ceiling here is what the **codec** permits, not what the
//!   silicon can sustain. HEVC Level 6.2 permits over two thousand frames a
//!   second at 1080p; no encoder does that. It is an upper bound, and the
//!   binding constraint is the hardware, which is not measured.
//! - A resolution ceiling is an upper bound too. A picture inside it is not
//!   guaranteed to be accepted; only opening a session settles that, and
//!   sessions are issues #15 to #18.
//!
//! Issue #14 asked for these four — maximum resolution, framerate, B-frames and
//! HDR — to be *detected* per encoder, and this table infers them instead.
//! Measuring them needs a live encoder session, which needs a backend, so
//! [issue #133](https://github.com/wildware-uk/clipped/issues/133) tracks
//! replacing the inferred values here with queried ones once one exists. Until
//! then every value from this module is labelled inferred wherever it is shown.
//!
//! HDR here means 10-bit encoding, which is the necessary condition for it. The
//! colour signalling that makes a 10-bit stream an HDR one belongs to the muxer
//! and is not written yet
//! ([issue #146](https://github.com/wildware-uk/clipped/issues/146)).

use crate::claim::Claim;
use crate::codec::{Codec, EncoderKind, Resolution};

/// Luma samples per second permitted by H.264 Level 6.2.
///
/// ITU-T H.264 Table A-1 gives Level 6.2 a `MaxMBPS` of 4,233,600 macroblocks
/// per second, and a macroblock is 16x16 = 256 luma samples.
pub(crate) const H264_LEVEL_6_2_LUMA_SAMPLE_RATE: u64 = 4_233_600 * 256;

/// Luma samples per second permitted by HEVC Level 6.2, High tier
/// (ITU-T H.265 Table A.8, `MaxLumaSr`).
pub(crate) const HEVC_LEVEL_6_2_LUMA_SAMPLE_RATE: u64 = 4_278_190_080;

/// Luma samples per second permitted by AV1 Level 6.3
/// (AOMedia AV1 specification, Annex A, `MaxLumaSampleRate`).
pub(crate) const AV1_LEVEL_6_3_LUMA_SAMPLE_RATE: u64 = 4_706_009_088;

/// The published limits for one encoder family and one codec.
///
/// Every field is a [`Claim`], so a caller that prints one prints its evidence
/// with it, and a caller that acts on one can ask whether it was measured —
/// which none of these were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReferenceLimits {
    /// Whether the family supports the codec at all, where that holds for every
    /// generation. `Unknown` wherever it depends on the chip.
    pub(crate) supported: Claim<bool>,
    /// The largest picture the vendor documents.
    pub(crate) max_resolution: Claim<Resolution>,
    /// The codec level's ceiling on luma samples per second.
    pub(crate) max_luma_samples_per_second: Claim<u64>,
    /// Whether B-frames are available.
    pub(crate) b_frames: Claim<bool>,
    /// Whether 10-bit encoding is available.
    pub(crate) hdr: Claim<bool>,
}

impl ReferenceLimits {
    /// Everything unknown: the answer for a combination the table does not
    /// cover.
    const fn unknown() -> Self {
        Self {
            supported: Claim::Unknown,
            max_resolution: Claim::Unknown,
            max_luma_samples_per_second: Claim::Unknown,
            b_frames: Claim::Unknown,
            hdr: Claim::Unknown,
        }
    }
}

/// The published limits for `kind` encoding `codec`.
///
/// The `match` is exhaustive over both enumerations, so a new codec or a new
/// encoder family does not compile until somebody has decided what this table
/// says about it — which is the point, because the alternative is a default
/// that silently claims something.
pub(crate) const fn limits(kind: EncoderKind, codec: Codec) -> ReferenceLimits {
    match (kind, codec) {
        // NVENC. H.264 is 8-bit only on every generation, which is why HDR is
        // a confident no rather than an unknown. HEVC B-frames arrived with
        // Turing and AV1 with Ada, so both are left to measurement.
        (EncoderKind::Nvenc, Codec::H264) => ReferenceLimits {
            supported: Claim::Inferred(true),
            max_resolution: Claim::Inferred(Resolution::new(4096, 4096)),
            max_luma_samples_per_second: Claim::Inferred(H264_LEVEL_6_2_LUMA_SAMPLE_RATE),
            b_frames: Claim::Inferred(true),
            hdr: Claim::Inferred(false),
        },
        (EncoderKind::Nvenc, Codec::Hevc) => ReferenceLimits {
            supported: Claim::Unknown,
            max_resolution: Claim::Inferred(Resolution::new(8192, 8192)),
            max_luma_samples_per_second: Claim::Inferred(HEVC_LEVEL_6_2_LUMA_SAMPLE_RATE),
            b_frames: Claim::Unknown,
            hdr: Claim::Unknown,
        },
        // NVENC's AV1 encoder exists only on Ada and later, so where it exists
        // at all, 10-bit does too and B-frames do not.
        (EncoderKind::Nvenc, Codec::Av1) => ReferenceLimits {
            supported: Claim::Unknown,
            max_resolution: Claim::Inferred(Resolution::new(8192, 8192)),
            max_luma_samples_per_second: Claim::Inferred(AV1_LEVEL_6_3_LUMA_SAMPLE_RATE),
            b_frames: Claim::Inferred(false),
            hdr: Claim::Inferred(true),
        },

        // AMF. H.264 B-frames are supported by some video engines and not
        // others, so they are unknown rather than assumed either way.
        (EncoderKind::Amf, Codec::H264) => ReferenceLimits {
            supported: Claim::Inferred(true),
            max_resolution: Claim::Inferred(Resolution::new(4096, 2160)),
            max_luma_samples_per_second: Claim::Inferred(H264_LEVEL_6_2_LUMA_SAMPLE_RATE),
            b_frames: Claim::Unknown,
            hdr: Claim::Inferred(false),
        },
        (EncoderKind::Amf, Codec::Hevc) => ReferenceLimits {
            supported: Claim::Unknown,
            max_resolution: Claim::Inferred(Resolution::new(8192, 4352)),
            max_luma_samples_per_second: Claim::Inferred(HEVC_LEVEL_6_2_LUMA_SAMPLE_RATE),
            b_frames: Claim::Inferred(false),
            hdr: Claim::Unknown,
        },
        (EncoderKind::Amf, Codec::Av1) => ReferenceLimits {
            supported: Claim::Unknown,
            max_resolution: Claim::Inferred(Resolution::new(8192, 4352)),
            max_luma_samples_per_second: Claim::Inferred(AV1_LEVEL_6_3_LUMA_SAMPLE_RATE),
            b_frames: Claim::Inferred(false),
            hdr: Claim::Inferred(true),
        },

        // Quick Sync. Nothing in this row has been checked against real
        // hardware by this project: there is no Intel GPU on the machine these
        // were written on, which is recorded here rather than only in a pull
        // request nobody will read again (issue #14).
        (EncoderKind::QuickSync, Codec::H264) => ReferenceLimits {
            supported: Claim::Inferred(true),
            max_resolution: Claim::Inferred(Resolution::new(4096, 2304)),
            max_luma_samples_per_second: Claim::Inferred(H264_LEVEL_6_2_LUMA_SAMPLE_RATE),
            b_frames: Claim::Inferred(true),
            hdr: Claim::Inferred(false),
        },
        (EncoderKind::QuickSync, Codec::Hevc) => ReferenceLimits {
            supported: Claim::Unknown,
            max_resolution: Claim::Inferred(Resolution::new(8192, 4320)),
            max_luma_samples_per_second: Claim::Inferred(HEVC_LEVEL_6_2_LUMA_SAMPLE_RATE),
            b_frames: Claim::Unknown,
            hdr: Claim::Unknown,
        },
        (EncoderKind::QuickSync, Codec::Av1) => ReferenceLimits {
            supported: Claim::Unknown,
            max_resolution: Claim::Inferred(Resolution::new(8192, 4352)),
            max_luma_samples_per_second: Claim::Inferred(AV1_LEVEL_6_3_LUMA_SAMPLE_RATE),
            b_frames: Claim::Unknown,
            hdr: Claim::Inferred(true),
        },

        // The software encoder. This row is a claim about what this project
        // will build rather than about the machine, which is why H.264 is a
        // confident yes and the other two are unknown: issue #18 has not
        // chosen, and the obvious software HEVC encoder is GPL-2.0, which
        // deny.toml forbids in an MPL-2.0 application.
        //
        // Everything else here is `Unknown`, and that is the discipline rather
        // than an omission. Every other inferred number in this table cites a
        // vendor's published limit; this row has no vendor and no library yet,
        // so there is nothing to infer from — a maximum resolution or a
        // B-frame claim here would be a guess wearing the word "inferred".
        // There is no luma rate for the same reason the CPU has no level: the
        // ceiling is the machine, and this table measures nothing.
        (EncoderKind::Software, Codec::H264) => ReferenceLimits {
            supported: Claim::Inferred(true),
            max_resolution: Claim::Unknown,
            max_luma_samples_per_second: Claim::Unknown,
            b_frames: Claim::Unknown,
            hdr: Claim::Unknown,
        },
        (EncoderKind::Software, Codec::Hevc | Codec::Av1) => ReferenceLimits::unknown(),
    }
}

/// The framerate ceiling `limit` luma samples per second allows at
/// `resolution`.
///
/// Saturates rather than overflowing, and is zero for a picture too large to
/// encode even once a second — which is a truthful answer rather than a
/// division to guard against.
pub(crate) fn framerate_ceiling(luma_samples_per_second: u64, resolution: Resolution) -> u32 {
    let samples = resolution.luma_samples();
    if samples == 0 {
        return 0;
    }
    u32::try_from(luma_samples_per_second / samples).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_table_entry_claims_hevc_or_av1_support() {
        // This is the rule the module documentation states, asserted rather
        // than trusted. A future contributor who fills in "AV1: yes" for a
        // vendor fails here, and the failure explains why that is not allowed.
        for kind in EncoderKind::ALL {
            for codec in [Codec::Hevc, Codec::Av1] {
                assert_eq!(
                    limits(kind, codec).supported,
                    Claim::Unknown,
                    "{kind} claims {codec} support without measuring it; a user who selects \
                     {codec} on that claim loses a recording when the driver disagrees"
                );
            }
        }
    }

    #[test]
    fn every_encoder_infers_h264_support_because_every_generation_has_it() {
        for kind in EncoderKind::ALL {
            assert_eq!(
                limits(kind, Codec::H264).supported,
                Claim::Inferred(true),
                "{kind} should fall back to H.264 when nothing measured it"
            );
        }
    }

    #[test]
    fn the_software_row_infers_nothing_it_has_no_source_for() {
        // Every inferred number in this table cites a vendor's published
        // limit. The software encoder has no vendor and, until issue #18
        // chooses a library, no published anything — so the only claim it may
        // make is that whatever gets built will encode H.264.
        for codec in Codec::EFFICIENCY_ORDER {
            let entry = limits(EncoderKind::Software, codec);
            for (field, stated) in [
                ("max_resolution", entry.max_resolution.value().is_some()),
                ("b_frames", entry.b_frames.value().is_some()),
                ("hdr", entry.hdr.value().is_some()),
                (
                    "max_luma_samples_per_second",
                    entry.max_luma_samples_per_second.value().is_some(),
                ),
            ] {
                assert!(
                    !stated,
                    "the software {codec} row states a {field} that no vendor documents"
                );
            }
        }
    }

    #[test]
    fn nothing_in_the_table_is_measured() {
        for kind in EncoderKind::ALL {
            for codec in Codec::EFFICIENCY_ORDER {
                let entry = limits(kind, codec);
                for measured in [
                    entry.supported.is_measured(),
                    entry.max_resolution.is_measured(),
                    entry.max_luma_samples_per_second.is_measured(),
                    entry.b_frames.is_measured(),
                    entry.hdr.is_measured(),
                ] {
                    assert!(
                        !measured,
                        "{kind}/{codec} presents a table entry as a measurement"
                    );
                }
            }
        }
    }

    #[test]
    fn a_codec_ceiling_at_1080p_is_the_level_limit_divided_by_the_picture() {
        // H.264 Level 6.2 permits 1,083,801,600 luma samples a second, and a
        // 1080p picture is 2,073,600 of them.
        assert_eq!(
            framerate_ceiling(H264_LEVEL_6_2_LUMA_SAMPLE_RATE, Resolution::HD_1080P),
            522
        );
        // The same ceiling buys a quarter as many frames at 4K.
        assert_eq!(
            framerate_ceiling(H264_LEVEL_6_2_LUMA_SAMPLE_RATE, Resolution::UHD_4K),
            130
        );
    }

    #[test]
    fn a_ceiling_for_a_picture_of_no_size_is_zero_rather_than_a_division_by_zero() {
        assert_eq!(
            framerate_ceiling(H264_LEVEL_6_2_LUMA_SAMPLE_RATE, Resolution::new(0, 0)),
            0
        );
    }
}
