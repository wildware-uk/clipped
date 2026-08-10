//! What "Automatic" should choose, and why.
//!
//! SPEC.md section 9 asks for hardware encoding to be preferred automatically,
//! and SPEC.md section 8 shows the shape the product reports such a choice in:
//! the setting the user picked, and the thing it resolved to. This module
//! produces the ranked list behind that, so that the recorder, the desktop
//! application and the capability report all order encoders the same way rather
//! than each inventing an order.
//!
//! # The order, and why it is that
//!
//! 1. **Hardware before software.** The recorder runs alongside a game and CPU
//!    time is the scarcest thing on the machine (AGENTS.md section 18). A
//!    software encoder takes frames away from the thing being recorded.
//! 2. **An adapter with video memory of its own before one without.** An
//!    adapter that shares system memory with the CPU shares its bandwidth too,
//!    and on a machine with both, the game is running on the other one — so
//!    encoding there avoids copying every frame across the bus.
//! 3. **Then the most video memory.** The tie-break between two adapters that
//!    both have some, and a measured number rather than a guess about which of
//!    them is "the graphics card": an integrated GPU with a memory carve-out
//!    and a card are indistinguishable to DXGI except by how much they have
//!    (see [`AdapterKind`]).
//! 4. **Then the order SPEC.md section 9 lists**: NVIDIA, AMD, Intel.
//!
//! Within an encoder, the codec is the most efficient one whose support was
//! **measured**. An inferred claim never wins a codec: that is the whole point
//! of [`Claim`](crate::Claim), and picking AV1 because a table said the vendor
//! supports it is the exact failure this crate is built to avoid. H.264 is the
//! fallback, and every encoder here can produce it.

use core::cmp::Reverse;
use core::fmt;

use crate::adapter::{Adapter, AdapterId, AdapterKind};
use crate::claim::Evidence;
use crate::codec::{Codec, EncoderKind};
use crate::detection::{CapabilityReport, CodecSupport, EncoderReport};

/// Why an encoder is where it is in the ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChoiceReason {
    /// Dedicated encoding silicon on an adapter with video memory of its own:
    /// the cheapest option for the game being recorded.
    HardwareWithOwnMemory,
    /// Dedicated encoding silicon on an adapter that shares system memory.
    HardwareWithSharedMemory,
    /// Dedicated encoding silicon on an adapter this detection could not
    /// attribute.
    UnattributedHardware,
    /// The CPU. Always available, always last.
    SoftwareFallback,
}

impl ChoiceReason {
    /// Where this comes in the ranking, `0` being first.
    const fn rank(self) -> u8 {
        match self {
            Self::HardwareWithOwnMemory => 0,
            Self::HardwareWithSharedMemory => 1,
            Self::UnattributedHardware => 2,
            Self::SoftwareFallback => 3,
        }
    }
}

impl fmt::Display for ChoiceReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::HardwareWithOwnMemory => {
                "hardware encoding on an adapter with video memory of its own"
            }
            Self::HardwareWithSharedMemory => {
                "hardware encoding on an adapter sharing system memory"
            }
            Self::UnattributedHardware => "hardware encoding",
            Self::SoftwareFallback => "CPU encoding, which costs the game frames",
        })
    }
}

/// One encoder and codec that could be used, with its place in the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recommendation {
    kind: EncoderKind,
    codec: Codec,
    codec_evidence: Evidence,
    adapter: Option<AdapterId>,
    reason: ChoiceReason,
}

impl Recommendation {
    /// The encoder.
    #[must_use]
    pub const fn encoder(&self) -> EncoderKind {
        self.kind
    }

    /// The codec to use with it.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// Whether that codec's support was measured or only inferred.
    ///
    /// Worth showing next to the choice: a recommendation resting on an
    /// inferred claim is a guess, and the user is entitled to know which of the
    /// two they have been given.
    #[must_use]
    pub const fn codec_evidence(&self) -> Evidence {
        self.codec_evidence
    }

    /// The adapter it runs on, for a hardware encoder.
    #[must_use]
    pub const fn adapter(&self) -> Option<AdapterId> {
        self.adapter
    }

    /// Why it ranks where it does.
    #[must_use]
    pub const fn reason(&self) -> ChoiceReason {
        self.reason
    }
}

impl fmt::Display for Recommendation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} with {}", self.kind, self.codec)?;
        if self.codec_evidence == Evidence::Inferred {
            formatter.write_str(" (codec support inferred, not measured)")?;
        }
        write!(formatter, " — {}", self.reason)
    }
}

/// Ranks every usable encoder, best first.
///
/// The first entry is what "Automatic" resolves to. The list is never empty:
/// the software encoder is available on every machine, which is what makes
/// "Automatic" a setting that always has an answer rather than one that can
/// fail.
#[must_use]
pub fn recommend(report: &CapabilityReport) -> Vec<Recommendation> {
    let mut recommendations: Vec<Recommendation> = report
        .encoders()
        .iter()
        .filter(|encoder| encoder.availability().is_available())
        .filter_map(|encoder| recommendation_for(encoder, report))
        .collect();

    // Sorted by class, then by video memory, then — because the sort is stable
    // and `report.encoders()` is already in the order `EncoderKind::ALL`
    // declares — by the order SPEC.md section 9 lists encoders in.
    recommendations.sort_by_key(|recommendation| {
        let memory = recommendation
            .adapter
            .and_then(|id| report.adapter(id))
            .map_or(0, Adapter::dedicated_video_memory);
        (recommendation.reason.rank(), Reverse(memory))
    });
    recommendations
}

/// Builds the recommendation for one available encoder, if it has a codec.
fn recommendation_for(
    encoder: &EncoderReport,
    report: &CapabilityReport,
) -> Option<Recommendation> {
    let (codec, codec_evidence) = best_codec(encoder)?;
    Some(Recommendation {
        kind: encoder.kind(),
        codec,
        codec_evidence,
        adapter: encoder.adapter(),
        reason: reason_for(encoder, report),
    })
}

/// The most efficient codec worth choosing.
///
/// Measured support wins outright, however inefficient the codec: a measured
/// H.264 encoder records, and an inferred AV1 one may not. Only when nothing
/// was measured does an inferred claim get a turn.
fn best_codec(encoder: &EncoderReport) -> Option<(Codec, Evidence)> {
    let measured = encoder
        .codecs()
        .iter()
        .find(|support| support.supported().is_measured_true());
    if let Some(support) = measured {
        return Some((support.codec(), Evidence::Measured));
    }

    encoder
        .codecs()
        .iter()
        .find(|support| support.supported().value() == Some(&true))
        .map(|support| (support.codec(), Evidence::Inferred))
}

/// Which class an available encoder belongs to.
fn reason_for(encoder: &EncoderReport, report: &CapabilityReport) -> ChoiceReason {
    if !encoder.kind().is_hardware() {
        return ChoiceReason::SoftwareFallback;
    }

    match encoder
        .adapter()
        .and_then(|id| report.adapter(id))
        .map(|adapter| adapter.kind())
    {
        Some(AdapterKind::OwnVideoMemory) => ChoiceReason::HardwareWithOwnMemory,
        Some(AdapterKind::SharedVideoMemory) => ChoiceReason::HardwareWithSharedMemory,
        // A hardware encoder attributed to a software rasteriser is a
        // contradiction detection should never produce, and it is treated as
        // unattributed rather than trusted.
        Some(AdapterKind::Software) | None => ChoiceReason::UnattributedHardware,
    }
}

/// The codecs an encoder was measured to support, most efficient first.
///
/// The list a settings screen should offer without a warning next to it.
#[must_use]
pub fn measured_codecs(encoder: &EncoderReport) -> Vec<Codec> {
    encoder
        .codecs()
        .iter()
        .filter(|support| support.supported().is_measured_true())
        .map(CodecSupport::codec)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Adapter;
    use crate::codec::Vendor;
    use crate::detect;
    use crate::probe::{
        EncoderObservations, HardwareEncoder, RuntimeObservation, RuntimeOutcome, SystemFacts,
    };

    fn nvidia_card() -> Adapter {
        Adapter::new(
            AdapterId::from_luid(1, 0),
            "NVIDIA GeForce RTX 4090",
            Vendor::Nvidia,
            0x2684,
            24 * 1024 * 1024 * 1024,
            false,
        )
    }

    fn integrated_amd() -> Adapter {
        Adapter::new(
            AdapterId::from_luid(2, 0),
            "AMD Radeon(TM) Graphics",
            Vendor::Amd,
            0x164E,
            0,
            false,
        )
    }

    fn loaded(kind: EncoderKind, library: &str) -> RuntimeObservation {
        RuntimeObservation::new(kind, library, RuntimeOutcome::Loaded)
    }

    #[test]
    fn a_machine_with_no_hardware_encoder_still_gets_a_recommendation() {
        let report = detect(&SystemFacts::new(Vec::new(), EncoderObservations::none()));
        let recommendations = recommend(&report);

        assert_eq!(
            recommendations.len(),
            1,
            "only the software encoder is left"
        );
        let first = recommendations[0];
        assert_eq!(first.encoder(), EncoderKind::Software);
        assert_eq!(first.codec(), Codec::H264);
        assert_eq!(first.reason(), ChoiceReason::SoftwareFallback);
    }

    #[test]
    fn an_adapter_with_its_own_memory_outranks_one_that_shares() {
        // The AMD part here shares system memory and the NVIDIA one does not,
        // so the published NVIDIA-then-AMD order and the memory rule agree;
        // the test that they are not the same rule is below.
        let facts = SystemFacts::new(
            vec![nvidia_card(), integrated_amd()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll"))
                .with_runtime(loaded(EncoderKind::Amf, "amfrt64.dll")),
        );
        let recommendations = recommend(&detect(&facts));

        let order: Vec<EncoderKind> = recommendations
            .iter()
            .map(Recommendation::encoder)
            .collect();
        assert_eq!(
            order,
            vec![EncoderKind::Nvenc, EncoderKind::Amf, EncoderKind::Software]
        );
        assert_eq!(
            recommendations[0].reason(),
            ChoiceReason::HardwareWithOwnMemory
        );
        assert_eq!(
            recommendations[1].reason(),
            ChoiceReason::HardwareWithSharedMemory
        );
    }

    #[test]
    fn a_card_with_its_own_memory_wins_over_a_vendor_the_specification_lists_first() {
        // An Intel part sharing system memory and an AMD card with its own:
        // SPEC.md section 9 lists AMD before Intel, so what this separates is
        // the memory rule from the published order — the AMD entry has to come
        // first for the right reason.
        let discrete_amd = Adapter::new(
            AdapterId::from_luid(5, 0),
            "AMD Radeon RX 7900 XTX",
            Vendor::Amd,
            0x744C,
            24 * 1024 * 1024 * 1024,
            false,
        );
        let integrated_intel = Adapter::new(
            AdapterId::from_luid(6, 0),
            "Intel(R) UHD Graphics",
            Vendor::Intel,
            0x9BC8,
            0,
            false,
        );
        let facts = SystemFacts::new(
            vec![integrated_intel, discrete_amd],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Amf, "amfrt64.dll"))
                .with_runtime(loaded(EncoderKind::QuickSync, "libmfxhw64.dll")),
        );

        let recommendations = recommend(&detect(&facts));
        assert_eq!(recommendations[0].encoder(), EncoderKind::Amf);
        assert_eq!(recommendations[1].encoder(), EncoderKind::QuickSync);
        assert_eq!(
            recommendations[1].reason(),
            ChoiceReason::HardwareWithSharedMemory
        );
    }

    #[test]
    fn between_two_adapters_with_their_own_memory_the_larger_one_wins() {
        // The case the machine this was written on produces: an NVIDIA card
        // with 24 GiB and an integrated AMD part with a 2 GiB carve-out. DXGI
        // cannot say which of them is a graphics card, so the ranking uses the
        // number it can see rather than a word it would have to guess.
        let carve_out = Adapter::new(
            AdapterId::from_luid(7, 0),
            "AMD Radeon(TM) Graphics",
            Vendor::Amd,
            0x13C0,
            2 * 1024 * 1024 * 1024,
            false,
        );
        let facts = SystemFacts::new(
            // Enumerated with the smaller adapter first, so that a ranking that
            // merely kept DXGI's order would fail here.
            vec![carve_out, nvidia_card()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Amf, "amfrt64.dll"))
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll")),
        );

        let recommendations = recommend(&detect(&facts));
        assert_eq!(recommendations[0].encoder(), EncoderKind::Nvenc);
        assert_eq!(recommendations[1].encoder(), EncoderKind::Amf);
        assert_eq!(
            recommendations[0].reason(),
            ChoiceReason::HardwareWithOwnMemory
        );
    }

    #[test]
    fn the_codec_chosen_is_the_most_efficient_one_that_was_measured() {
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll"))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::H264,
                    "NVIDIA H.264 Encoder MFT",
                ))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::Av1,
                    "NVIDIA AV1 Encoder MFT",
                )),
        );

        let first = recommend(&detect(&facts))[0];
        assert_eq!(first.codec(), Codec::Av1);
        assert_eq!(first.codec_evidence(), Evidence::Measured);
    }

    #[test]
    fn an_unmeasured_codec_never_wins_the_recommendation() {
        // The runtime loaded, so NVENC is available, but nothing measured a
        // codec. The fallback is H.264 on an inferred claim — never AV1, which
        // is the failure this ranking exists to prevent.
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll")),
        );

        let first = recommend(&detect(&facts))[0];
        assert_eq!(first.encoder(), EncoderKind::Nvenc);
        assert_eq!(first.codec(), Codec::H264);
        assert_eq!(first.codec_evidence(), Evidence::Inferred);
        assert!(
            first.to_string().contains("inferred"),
            "a guess must say so when it is printed: {first}"
        );
    }

    #[test]
    fn an_unavailable_encoder_is_not_recommended() {
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none().with_runtime(RuntimeObservation::new(
                EncoderKind::Nvenc,
                "nvEncodeAPI64.dll",
                RuntimeOutcome::NotFound,
            )),
        );

        let recommendations = recommend(&detect(&facts));
        assert!(recommendations
            .iter()
            .all(|recommendation| recommendation.encoder() != EncoderKind::Nvenc));
        assert_eq!(recommendations[0].encoder(), EncoderKind::Software);
    }

    #[test]
    fn measured_codecs_lists_only_what_was_measured() {
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll"))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::Hevc,
                    "NVIDIA HEVC Encoder MFT",
                )),
        );
        let report = detect(&facts);
        let nvenc = report
            .encoder(EncoderKind::Nvenc)
            .expect("nvenc is reported");

        assert_eq!(measured_codecs(nvenc), vec![Codec::Hevc]);
    }
}
