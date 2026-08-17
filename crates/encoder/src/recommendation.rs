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
//! 1. **An encoder this build can open before one it cannot.** Detection
//!    reports what the *machine* has, and
//!    [`EncoderKind::is_implemented`](crate::EncoderKind::is_implemented) says
//!    which of those Clipped has a backend proven to drive. A family with no
//!    proven backend is still reported and still ranked — a user is entitled to
//!    know their GPU has an encoder Clipped cannot yet use — but it ranks below
//!    the software fallback, which does work. Recording on the CPU costs the
//!    game frames; being handed an encoder that has never encoded one costs the
//!    user the recording ([#175](https://github.com/wildware-uk/clipped/issues/175)).
//!
//!    An encoder on an adapter frames are not captured on is unopenable for a
//!    second reason and ranks the same way, for the same trade: a vendor's
//!    encode runtime refuses a Direct3D device belonging to another vendor's
//!    adapter, so on a machine with two of them the encoder can be entirely real
//!    and entirely unusable ([#443](https://github.com/wildware-uk/clipped/issues/443)).
//! 2. **Hardware before software.** The recorder runs alongside a game and CPU
//!    time is the scarcest thing on the machine (AGENTS.md section 18). A
//!    software encoder takes frames away from the thing being recorded.
//! 3. **An adapter with video memory of its own before one without.** An
//!    adapter that shares system memory with the CPU shares its bandwidth too,
//!    and on a machine with both, the game is running on the other one — so
//!    encoding there avoids copying every frame across the bus.
//! 4. **Then the most video memory.** The tie-break between two adapters that
//!    both have some, and a measured number rather than a guess about which of
//!    them is "the graphics card": an integrated GPU with a memory carve-out
//!    and a card are indistinguishable to DXGI except by how much they have
//!    (see [`AdapterKind`]).
//! 5. **Then the order SPEC.md section 9 lists**: NVIDIA, AMD, Intel.
//!
//! Within an encoder, the codec is the most efficient one whose support was
//! **measured**. An inferred claim never wins a codec: that is the whole point
//! of [`Claim`](crate::Claim), and picking AV1 because a table said the vendor
//! supports it is the exact failure this crate is built to avoid. H.264 is the
//! fallback, and every encoder here can produce it.
//!
//! # Showing a choice and making one
//!
//! Two different questions, and rule 1 is why they are not the same function:
//!
//! - **What should this machine be told?** [`recommend`], which ranks every
//!   available encoder including the ones this build cannot open, each carrying
//!   [`Recommendation::is_openable`] and printing the fact when it is `false`.
//! - **Which encoder should be opened?** [`Recommendation::for_opening`], which
//!   can only return one this build has a proven backend for.

use core::cmp::Reverse;
use core::fmt;

use crate::adapter::{Adapter, AdapterId, AdapterKind};
use crate::claim::Evidence;
use crate::codec::{Codec, EncoderKind};
use crate::detection::{CapabilityReport, CodecSupport, EncoderReport};

/// Why an encoder is where it is in the ranking.
///
/// Declared in ranking order, which is the order [`rank`](Self::rank) states
/// and the derived [`Ord`] agrees with.
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
    /// The CPU. Available on every machine, and last of the encoders that work.
    SoftwareFallback,
    /// The machine has this encoder and no recording made here can hand it a
    /// frame: it is on an adapter that is not the one capture creates its
    /// Direct3D device on, and vendor encode runtimes refuse another vendor's
    /// device ([`crate::Availability::OnAnotherAdapter`], issue #443).
    ///
    /// Below the CPU, for the reason [`NoProvenBackend`](Self::NoProvenBackend)
    /// is: encoding on the CPU costs the game frames, and choosing an encoder
    /// that cannot open costs the recording. Still ranked and still printed,
    /// because "your AMD card has an encoder and Clipped cannot use it here" is
    /// a thing a user is entitled to be told rather than to deduce from a
    /// missing line.
    NotTheCaptureAdapter,
    /// The machine has this encoder and this build cannot drive it: detection
    /// found the hardware and the runtime, and
    /// [`EncoderKind::is_implemented`](crate::EncoderKind::is_implemented)
    /// reports no backend proven to encode with it.
    ///
    /// Last, below the CPU. The two costs are not comparable: encoding on the
    /// CPU takes frames from the game, and encoding with a backend nobody has
    /// watched produce a frame takes the recording. This is why the class is
    /// part of the ranking rather than a caveat printed beside it — a reason
    /// that only *described* the hardware would leave the entry sorted above
    /// the fallback that works.
    NoProvenBackend,
}

impl ChoiceReason {
    /// Where this comes in the ranking, `0` being first.
    const fn rank(self) -> u8 {
        match self {
            Self::HardwareWithOwnMemory => 0,
            Self::HardwareWithSharedMemory => 1,
            Self::UnattributedHardware => 2,
            Self::SoftwareFallback => 3,
            Self::NotTheCaptureAdapter => 4,
            Self::NoProvenBackend => 5,
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
            Self::NotTheCaptureAdapter => {
                // Says which way round the mismatch is without naming either
                // adapter: `Availability` carries the vendors, and the report
                // prints that line directly above this one.
                "it is on an adapter frames are not captured on, so no recording here can open it"
            }
            Self::NoProvenBackend => {
                // Not "detected on this machine, and …": an entry is only in
                // this list because detection found it, and a caller that
                // groups the unopenable entries under a heading of their own —
                // as the capability report does — would say it twice in two
                // lines.
                "this build has no backend proven to encode with it, so it is not chosen"
            }
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

    /// Whether this encoder can actually be opened here.
    ///
    /// `false` for a family the machine has and Clipped has no proven backend
    /// for, and for one that is on an adapter frames are not captured on — the
    /// two independent ways a real encoder can still be no use. Such an entry
    /// is still returned by [`recommend`], because a
    /// capability report that hid it would be answering a question nobody
    /// asked — but it is ranked below the software fallback and no caller
    /// should open it. A settings screen offering the ranked list should
    /// present it as unavailable rather than as a choice.
    #[must_use]
    pub const fn is_openable(&self) -> bool {
        // Two independent ways to be unopenable, and both have to be asked.
        // The first is about this build; the second is about this machine, and
        // a backend that is proven everywhere is still one that cannot bind a
        // texture from another vendor's adapter (issue #443).
        self.kind.is_implemented() && !matches!(self.reason, ChoiceReason::NotTheCaptureAdapter)
    }

    /// Which encoder to *open* for "Automatic".
    ///
    /// The counterpart of [`recommend`], and the one to call when the answer
    /// will be handed to a backend rather than printed: it can only return an
    /// encoder [`is_openable`](Self::is_openable) accepts, so a caller cannot
    /// forget to ask. Ranking alone would very nearly do — rule 1 puts every
    /// openable encoder above every unopenable one, so the head of the list is
    /// already safe — and "very nearly" is not a property to rest a recording
    /// on when it costs one function to make it exact.
    ///
    /// `None` means no encoder in the report can be opened at all. Detection
    /// never produces such a report: the software fallback needs no hardware,
    /// is available on every machine, and has a proven backend. It is an
    /// `Option` rather than an infallible answer because a [`CapabilityReport`]
    /// can also be deserialised from the capability cache, and a caller that
    /// has to handle "nothing to open" is one that cannot be handed a
    /// fabricated encoder instead.
    #[must_use]
    pub fn for_opening(report: &CapabilityReport) -> Option<Self> {
        recommend(report).into_iter().find(Self::is_openable)
    }
}

impl fmt::Display for Recommendation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} with {}", self.kind, self.codec)?;
        if self.codec_evidence == Evidence::Inferred {
            formatter.write_str(" (codec support inferred, not measured)")?;
        }
        write!(formatter, " — {}", self.reason)?;
        if !self.is_openable() {
            // The issue number, because "no backend proven" without somewhere
            // to follow it reads as an apology rather than as a status
            // (AGENTS.md section 27). It is the same number the capability
            // report's footer prints, from the same function.
            write!(formatter, " (#{})", self.kind.backend_issue())?;
        }
        Ok(())
    }
}

/// Ranks every available encoder, best first.
///
/// The first entry is what "Automatic" resolves to. The list is never empty:
/// the software encoder is available on every machine, which is what makes
/// "Automatic" a setting that always has an answer rather than one that can
/// fail.
///
/// "Available" is what the *machine* has, so the list can include an encoder
/// this build cannot open — marked by [`Recommendation::is_openable`], ranked
/// below the software fallback, and printed with the reason. That is the list
/// to show. To choose an encoder to open, call
/// [`Recommendation::for_opening`], which cannot return one.
#[must_use]
pub fn recommend(report: &CapabilityReport) -> Vec<Recommendation> {
    let mut recommendations: Vec<Recommendation> = report
        .encoders()
        .iter()
        .filter(|encoder| encoder.availability().is_present())
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
    // Asked before anything about the hardware, because it outranks all of it:
    // how good an adapter is at encoding decides nothing if this build cannot
    // ask it to. The question is put to `EncoderKind::is_implemented`, which
    // lives beside the backends, so that a backend becoming proven moves the
    // ranking with it rather than leaving a list here to be updated by hand
    // (the failure #167 was raised for).
    if !encoder.kind().is_implemented() {
        return ChoiceReason::NoProvenBackend;
    }

    if !encoder.kind().is_hardware() {
        return ChoiceReason::SoftwareFallback;
    }

    // After the backend question and before every question about how good the
    // adapter is, for the same reason: how much video memory an encoder has
    // decides nothing if no recording here can hand it a frame.
    if matches!(
        encoder.availability(),
        crate::Availability::OnAnotherAdapter { .. }
    ) {
        return ChoiceReason::NotTheCaptureAdapter;
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
        // An NVIDIA part reporting no dedicated video memory of its own — the
        // shape DXGI reports for a virtualised adapter — beside an AMD card
        // with 24 GiB. SPEC.md section 9 lists NVIDIA before AMD, so what this
        // separates is the memory rule from the published order: the AMD entry
        // has to come first, and for the right reason.
        //
        // It is that pair rather than the Intel one this test used to use,
        // because with rule 1 in place an Intel entry's position no longer
        // says anything about memory: it is last whatever adapter it is on.
        // Both families here have a proven backend, so nothing but the memory
        // rule decides the order.
        let discrete_amd = Adapter::new(
            AdapterId::from_luid(5, 0),
            "AMD Radeon RX 7900 XTX",
            Vendor::Amd,
            0x744C,
            24 * 1024 * 1024 * 1024,
            false,
        );
        let shared_nvidia = Adapter::new(
            AdapterId::from_luid(6, 0),
            "NVIDIA GRID vGPU",
            Vendor::Nvidia,
            0x1BB3,
            0,
            false,
        );
        let facts = SystemFacts::new(
            vec![shared_nvidia, discrete_amd],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Amf, "amfrt64.dll"))
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll")),
        );

        let recommendations = recommend(&detect(&facts));
        assert_eq!(recommendations[0].encoder(), EncoderKind::Amf);
        assert_eq!(
            recommendations[0].reason(),
            ChoiceReason::HardwareWithOwnMemory
        );
        assert_eq!(recommendations[1].encoder(), EncoderKind::Nvenc);
        assert_eq!(
            recommendations[1].reason(),
            ChoiceReason::HardwareWithSharedMemory
        );
    }

    #[test]
    fn a_machine_whose_only_hardware_encoder_is_unproven_records_on_the_cpu() {
        // The machine this whole change is about: an Intel GPU and nothing
        // else. Quick Sync's hardware is there, its runtime loads, and its
        // backend has never encoded a frame — so ranking it above the software
        // fallback would hand a session layer an encoder that cannot record
        // (#175). Every fact here is one an Intel machine would produce,
        // injected through `SystemFacts` because there is no Intel GPU on the
        // machine this was written on.
        let integrated_intel = Adapter::new(
            AdapterId::from_luid(6, 0),
            "Intel(R) UHD Graphics 770",
            Vendor::Intel,
            0x4680,
            0,
            false,
        );
        let facts = SystemFacts::new(
            vec![integrated_intel],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::QuickSync, "libmfxhw64.dll"))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Intel,
                    Codec::Hevc,
                    "Intel® Hardware HEVC Encoder MFT",
                )),
        );
        let report = detect(&facts);

        // What a caller that will open an encoder is given: the CPU.
        let choice = Recommendation::for_opening(&report)
            .expect("the software fallback is available on every machine");
        assert_eq!(
            choice.encoder(),
            EncoderKind::Software,
            "an encoder with no proven backend was chosen to open: {choice}"
        );
        assert!(choice.is_openable());

        // And the ranking says the same thing, so a caller that reads the head
        // of the list without asking is safe too.
        let recommendations = recommend(&report);
        assert_eq!(recommendations[0].encoder(), EncoderKind::Software);

        // Quick Sync is still reported — the machine has it, and a user whose
        // GPU can encode HEVC is entitled to know Clipped cannot yet use it —
        // but below the fallback, saying why, and naming the issue.
        let quick_sync = recommendations
            .iter()
            .find(|recommendation| recommendation.encoder() == EncoderKind::QuickSync)
            .expect("a detected encoder stays in the report");
        assert!(
            !quick_sync.is_openable(),
            "Quick Sync has no proven backend, so nothing may open it: {quick_sync}"
        );
        assert_eq!(quick_sync.reason(), ChoiceReason::NoProvenBackend);
        let position = recommendations
            .iter()
            .position(|recommendation| recommendation.encoder() == EncoderKind::QuickSync)
            .expect("a detected encoder stays in the report");
        assert!(
            position > 0,
            "an encoder this build cannot open must not head the ranking: {recommendations:?}"
        );
        let printed = quick_sync.to_string();
        assert!(
            printed.contains("no backend proven"),
            "an entry nothing can open must say so: {printed}"
        );
        assert!(
            printed.contains(&format!("#{}", EncoderKind::QuickSync.backend_issue())),
            "the reader needs somewhere to follow it: {printed}"
        );
    }

    #[test]
    fn nothing_this_build_cannot_open_is_ever_offered_to_open() {
        // The property rather than one machine's answer: for every machine
        // these facts can describe, what `for_opening` returns is a family
        // `is_implemented` accepts.
        //
        // Only a machine that *has* an unopenable encoder can break it, so both
        // machines with hardware here have an Intel adapter, and in each of
        // them the Intel adapter is the one that wins every hardware rule —
        // dedicated memory, and more of it than anything else present. Those
        // are the cases that fail if `for_opening` stops filtering and the
        // ranking rule goes with it. The machine with no adapters cannot fail
        // and is not pretending to: it is here so the property is stated over
        // the machine that has nothing to rank, which is the one a caller is
        // likeliest to forget.
        let arc = |luid| {
            Adapter::new(
                AdapterId::from_luid(luid, 0),
                "Intel(R) Arc(TM) A770",
                Vendor::Intel,
                0x56A0,
                32 * 1024 * 1024 * 1024,
                false,
            )
        };
        let machines = [
            SystemFacts::new(Vec::new(), EncoderObservations::none()),
            SystemFacts::new(
                vec![arc(6)],
                EncoderObservations::none()
                    .with_runtime(loaded(EncoderKind::QuickSync, "libmfxhw64.dll")),
            ),
            SystemFacts::new(
                vec![nvidia_card(), integrated_amd(), arc(6)],
                EncoderObservations::none()
                    .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll"))
                    .with_runtime(loaded(EncoderKind::Amf, "amfrt64.dll"))
                    .with_runtime(loaded(EncoderKind::QuickSync, "libmfxhw64.dll")),
            ),
        ];

        for (machine, facts) in machines.into_iter().enumerate() {
            let report = detect(&facts);
            let choice = Recommendation::for_opening(&report)
                .expect("the software fallback is available on every machine");
            assert!(
                choice.encoder().is_implemented(),
                "machine {machine}: {choice} has no backend proven to encode with it"
            );
        }
    }

    #[test]
    fn an_intel_arc_card_does_not_outrank_the_cpu_on_its_own_memory() {
        // The strongest form of the failure: a discrete Intel card, which wins
        // every hardware rule in the ranking — dedicated memory, most of it,
        // and a runtime that loads. None of that matters while no backend has
        // encoded a frame on one.
        let arc = Adapter::new(
            AdapterId::from_luid(9, 0),
            "Intel(R) Arc(TM) A770 Graphics",
            Vendor::Intel,
            0x56A0,
            16 * 1024 * 1024 * 1024,
            false,
        );
        let facts = SystemFacts::new(
            vec![arc],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::QuickSync, "libmfxhw64.dll"))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Intel,
                    Codec::Av1,
                    "Intel® Hardware AV1 Encoder MFT",
                )),
        );

        let recommendations = recommend(&detect(&facts));
        let order: Vec<EncoderKind> = recommendations
            .iter()
            .map(Recommendation::encoder)
            .collect();
        assert_eq!(
            order,
            vec![EncoderKind::Software, EncoderKind::QuickSync],
            "measured AV1 on a card with 16 GiB is still an encoder this build cannot open"
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

    #[test]
    fn an_encoder_on_an_adapter_frames_are_not_captured_on_is_shown_and_never_chosen() {
        // Issue #443's machine: the NVIDIA card is enumerated first, so capture
        // lands there and the AMD encoder cannot be opened for a recording.
        //
        // Two things have to be true at once, and they pull in opposite
        // directions. `for_opening` must never return it — handing it to
        // `AmfEncoder::open` spends a recording's start-up on a refusal that was
        // already known. And `recommend` must still list it, because a user with
        // an AMD encoder Clipped will not use is entitled to be told that rather
        // than to notice a missing line.
        let facts = SystemFacts::new(
            vec![nvidia_card(), integrated_amd()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll"))
                .with_runtime(loaded(EncoderKind::Amf, "amfrt64.dll"))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Amd,
                    Codec::Hevc,
                    "AMDh265Encoder",
                )),
        );
        let report = detect(&facts);
        let ranked = recommend(&report);

        let amf = ranked
            .iter()
            .find(|recommendation| recommendation.encoder() == EncoderKind::Amf)
            .expect("an encoder this machine has must still be listed");
        assert_eq!(amf.reason(), ChoiceReason::NotTheCaptureAdapter);
        assert!(
            !amf.is_openable(),
            "nothing may open an encoder that refuses the device it would be handed"
        );
        assert!(
            amf.to_string().contains("captured"),
            "the entry has to say why it is not chosen: {amf}"
        );

        assert_eq!(
            Recommendation::for_opening(&report).map(|chosen| chosen.encoder()),
            Some(EncoderKind::Nvenc),
            "the encoder on the capture adapter is the one to open"
        );

        // Ranked below the software fallback, which is the difference between a
        // caveat and a class: a reason that only described the entry would leave
        // it sorted above an encoder that works.
        let position = |kind| {
            ranked
                .iter()
                .position(|recommendation| recommendation.encoder() == kind)
        };
        assert!(
            position(EncoderKind::Amf) > position(EncoderKind::Software),
            "an encoder that cannot be opened must rank below the CPU that can: {ranked:?}"
        );
    }

    #[test]
    fn reversing_the_adapter_order_moves_which_encoder_is_chosen() {
        // The same two cards the other way round, so the integrated part is the
        // default adapter — the laptop configuration issue #443 says would break
        // NVENC. Nothing about the hardware changed; what changed is where the
        // frames will be, and that alone has to change the choice.
        let machine = |adapters: Vec<Adapter>| {
            detect(&SystemFacts::new(
                adapters,
                EncoderObservations::none()
                    .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll"))
                    .with_runtime(loaded(EncoderKind::Amf, "amfrt64.dll"))
                    .with_hardware_encoder(HardwareEncoder::new(
                        Vendor::Amd,
                        Codec::Hevc,
                        "AMDh265Encoder",
                    ))
                    .with_hardware_encoder(HardwareEncoder::new(
                        Vendor::Nvidia,
                        Codec::Hevc,
                        "NVIDIA HEVC Encoder MFT",
                    )),
            ))
        };

        assert_eq!(
            Recommendation::for_opening(&machine(vec![nvidia_card(), integrated_amd()]))
                .map(|chosen| chosen.encoder()),
            Some(EncoderKind::Nvenc)
        );
        assert_eq!(
            Recommendation::for_opening(&machine(vec![integrated_amd(), nvidia_card()]))
                .map(|chosen| chosen.encoder()),
            Some(EncoderKind::Amf),
            "with capture on the AMD adapter, NVENC is the one that cannot be opened — even \
             though its card has all the video memory and ranks first everywhere else"
        );
    }
}
