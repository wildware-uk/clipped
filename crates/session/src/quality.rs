//! The quality preset a user picks, and what it resolves to on this machine.
//!
//! SPEC.md section 10 asks for Performance, Balanced, High and Ultra, and for
//! each to mean *concrete settings* rather than a label. The difficulty is that
//! the concrete settings are not the same on two machines: a preset that named
//! AV1 would be wrong on every encoder that does not produce it, and this
//! project's whole discipline about capability detection exists because a table
//! keyed on a GPU model is wrong the moment a driver changes
//! (`docs/encoder-capabilities.md`).
//!
//! So a preset here is not a row in a table of numbers. It is a **position on
//! three axes**, and where that position lands is read off
//! [`CapabilityReport`] at the moment a recording opens its encoder.
//!
//! # The three axes, and why there are three
//!
//! | Axis | What the preset moves | Who else could set it |
//! | --- | --- | --- |
//! | Bits per pixel per frame | The rate control the encoder is configured with | nobody — [issue #181](https://github.com/wildware-uk/clipped/issues/181) is the numeric bitrate, and it is not built |
//! | [`EncodePreset`] | Where to sit on the vendor's quality-for-speed curve | nobody — every backend drives it and nothing chose it until now |
//! | Codec, when the codec setting is `auto` | Which codec `auto` means | the `codec` setting, when it names one |
//!
//! Three rather than ten, and the ten are named in `docs/configuration.md`
//! along with what each would need. A preset that also set the frame rate or
//! the resolution would be a second answer to a question those settings already
//! answer (AGENTS.md section 55), and a preset that claimed to set HDR or the
//! container would be a control that silently does nothing (AGENTS.md section
//! 27), because neither is built.
//!
//! # What a preset never does
//!
//! **It never names a codec this machine was not reported to produce.** That is
//! the whole of "unsupported combinations cannot be selected silently" for this
//! feature: the preset cannot express an unsupported combination, because the
//! only codec it can choose *instead of* the automatic ranking is H.264 — the
//! one codec every encoder in the workspace can produce
//! ([`Codec::H264`]'s own documentation) — and it declines even that when the
//! encoder reports a `no` for it.
//!
//! **It never overrides a setting somebody chose.** A user who sets `codec` by
//! hand keeps that codec on every preset. See `docs/configuration.md`,
//! "Custom is the absence of a preset", for why that is not a lie the screen
//! tells.

use core::fmt;

use clipped_encoder::{CapabilityReport, Codec, EncodePreset, EncoderKind};

/// How much of the machine a recording is allowed to spend on itself.
///
/// Four values and no `Custom`. SPEC.md section 10 lists `Custom` beside the
/// four, and then lists under it the settings a user would set by hand —
/// resolution, framerate, codec, encoder — every one of which is already its
/// own setting here and is always available. So `Custom` is not a fifth thing
/// to choose; it is the name of what is already true whenever one of those
/// settings has been set.
///
/// Storing it would make it worse rather than clearer. A value in the settings
/// file is a value [`Resolved::is_overridden`](crate::config::Resolved::is_overridden)
/// reports as chosen, which is what enables the settings screen's Reset — and a
/// Reset that returns a user to "no preset" would be a control that undoes
/// nothing, because there is no such state to return to (issue #286, AGENTS.md
/// section 27). Worse, if setting a codec by hand silently rewrote the preset
/// to `Custom`, one edit would throw away the user's choice of preset *and*
/// stop the parts they did not edit from following the machine — which is the
/// "fixed numbers that are wrong on half of machines" failure this whole
/// module exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum QualityPreset {
    /// The least the machine spends: the fewest bits, the vendor's fastest
    /// point, and H.264.
    Performance,
    /// The default, and what every recording made before this setting existed
    /// was given.
    #[default]
    Balanced,
    /// More bits and the vendor's quality point, on the codec `auto` chooses.
    High,
    /// The most bits this build will spend without being told a number.
    Ultra,
}

impl QualityPreset {
    /// Every preset, from the cheapest to the most expensive.
    pub const ALL: [Self; 4] = [Self::Performance, Self::Balanced, Self::High, Self::Ultra];

    /// The token this is written as in the settings file and on the command
    /// line.
    ///
    /// One vocabulary for both, like every other setting
    /// (`crate::config::document`).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::High => "high",
            Self::Ultra => "ultra",
        }
    }

    /// The preset a token names, if it names one.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.token() == token)
    }

    /// Bits spent per pixel per frame, before the clamps
    /// `crate::encoding` applies.
    ///
    /// [`Balanced`](Self::Balanced) is 0.15, which is the number every
    /// recording this project has ever made was given
    /// (`crates/session/src/encoding.rs` before this setting existed), so the
    /// default preset changes nothing for anybody. The other three are steps of
    /// roughly half again in each direction from it.
    ///
    /// It is a rate per pixel per frame rather than a number of megabits
    /// because a fixed number is wrong at every resolution but one — the
    /// reasoning `BITS_PER_PIXEL_PER_FRAME` carried before this module took it
    /// over, and the reason [issue
    /// #181](https://github.com/wildware-uk/clipped/issues/181) is a separate
    /// piece of work: choosing a *number* is a different act from choosing how
    /// generous to be, and this is the second.
    #[must_use]
    pub const fn bits_per_pixel_per_frame(self) -> f64 {
        match self {
            Self::Performance => 0.10,
            Self::Balanced => 0.15,
            Self::High => 0.22,
            Self::Ultra => 0.30,
        }
    }

    /// Where on the vendor's quality-for-speed curve to sit.
    ///
    /// [`High`](Self::High) and [`Ultra`](Self::Ultra) both ask for
    /// [`EncodePreset::Quality`], and that is not an oversight: that type has
    /// three points on purpose, because "the difference between adjacent points
    /// on a vendor scale is not something a user can judge" and because the
    /// points "have to mean the same thing on four different encoders"
    /// (`crates/encoder/src/config.rs`). There is no point above the best one,
    /// so the two presets differ in what they spend rather than in what they
    /// ask the encoder to try.
    #[must_use]
    pub const fn effort(self) -> EncodePreset {
        match self {
            Self::Performance => EncodePreset::Speed,
            Self::Balanced => EncodePreset::Balanced,
            Self::High | Self::Ultra => EncodePreset::Quality,
        }
    }

    /// Which codec this preset asks `kind` for, given the codec the automatic
    /// ranking would have chosen.
    ///
    /// `automatic` is what `crate::encoding` already resolves — the most
    /// efficient codec the encoder was *measured* to support — and three of the
    /// four presets take it unchanged. [`Performance`](Self::Performance) asks
    /// for H.264 instead.
    ///
    /// The claim being made for H.264 is a narrow one, and deliberately so.
    /// **Not** that it encodes faster: nothing in this repository has measured
    /// that, and on recent silicon it may well not. What it is is the codec the
    /// rest of the pipeline is cheapest at — every decode this build does for
    /// playback, thumbnails, waveforms, the editor and export — and the only
    /// codec every encoder in the workspace can produce, which is
    /// [`Codec`]'s own statement about it.
    ///
    /// An encoder that reports a `no` for H.264 keeps `automatic`, so this can
    /// never name a codec the machine was reported not to have.
    #[must_use]
    pub fn codec_for(
        self,
        kind: EncoderKind,
        automatic: Codec,
        report: &CapabilityReport,
    ) -> Codec {
        if self != Self::Performance {
            return automatic;
        }

        let refused = report
            .encoder(kind)
            .and_then(|encoder| encoder.codec(Codec::H264))
            .is_some_and(|support| support.supported().value() == Some(&false));

        if refused {
            automatic
        } else {
            Codec::H264
        }
    }

    /// Everything this preset means for one encoder on this machine.
    ///
    /// The one place the three axes are read together, so that what a recording
    /// is configured with and what `clipped-recorder capabilities` prints
    /// cannot disagree (AGENTS.md section 55).
    #[must_use]
    pub fn resolve(
        self,
        kind: EncoderKind,
        automatic: Codec,
        report: &CapabilityReport,
    ) -> ResolvedQuality {
        ResolvedQuality {
            preset: self,
            codec: self.codec_for(kind, automatic, report),
            effort: self.effort(),
            bits_per_pixel_per_frame: self.bits_per_pixel_per_frame(),
        }
    }
}

impl fmt::Display for QualityPreset {
    /// Through [`pad`](fmt::Formatter::pad) rather than
    /// [`write_str`](fmt::Formatter::write_str), so that a caller laying these
    /// out in a column — `clipped-recorder capabilities` does — gets the width
    /// it asked for instead of silently getting none.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(self.token())
    }
}

/// What one preset resolved to, on one encoder, on this machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedQuality {
    preset: QualityPreset,
    codec: Codec,
    effort: EncodePreset,
    bits_per_pixel_per_frame: f64,
}

impl ResolvedQuality {
    /// The same answer, for a recording whose `codec` setting names one.
    ///
    /// A preset decides what `auto` means; it is never a second opinion about a
    /// codec somebody chose. So the two settings compose rather than compete —
    /// Performance on a recording pinned to AV1 is AV1 at Performance's bits
    /// and Performance's effort — and the settings screen already says which of
    /// the two the user set, through
    /// [`Resolved::is_overridden`](crate::config::Resolved::is_overridden).
    #[must_use]
    pub const fn with_codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// The preset this came from.
    #[must_use]
    pub const fn preset(&self) -> QualityPreset {
        self.preset
    }

    /// The codec to ask the encoder for, when the codec setting is `auto`.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// Where on the vendor's curve to sit.
    #[must_use]
    pub const fn effort(&self) -> EncodePreset {
        self.effort
    }

    /// Bits per pixel per frame, before clamping.
    #[must_use]
    pub const fn bits_per_pixel_per_frame(&self) -> f64 {
        self.bits_per_pixel_per_frame
    }
}

impl fmt::Display for ResolvedQuality {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{preset}: {codec}, {effort} preset, {bits} bits per pixel per frame",
            preset = self.preset,
            codec = self.codec.log_value(),
            effort = self.effort,
            bits = self.bits_per_pixel_per_frame,
        )
    }
}

#[cfg(test)]
mod tests {
    use clipped_encoder::{
        detect, measured_codecs, Adapter, AdapterId, EncoderLimits, EncoderObservations,
        HardwareEncoder, RuntimeObservation, RuntimeOutcome, SystemFacts, Vendor,
    };

    use super::*;

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

    /// The NVIDIA half of the machine this was developed on: an RTX 4090 whose
    /// driver registers all three transforms and whose encoder answered for all
    /// three codecs.
    fn nvidia_machine() -> CapabilityReport {
        detect(&SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll"))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::Av1,
                    "NVIDIA AV1 Encoder MFT",
                ))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::Hevc,
                    "NVIDIA HEVC Encoder MFT",
                ))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::H264,
                    "NVIDIA H.264 Encoder MFT",
                )),
        ))
    }

    /// The AMD half of the same machine: integrated Radeon graphics whose
    /// driver registers H.264 and HEVC and nothing for AV1, which is what
    /// `clipped-recorder capabilities --refresh` reports here.
    fn amd_machine() -> CapabilityReport {
        detect(&SystemFacts::new(
            vec![integrated_amd()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Amf, "amfrt64.dll"))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Amd,
                    Codec::Hevc,
                    "AMDh265Encoder",
                ))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Amd,
                    Codec::H264,
                    "AMDh264Encoder",
                )),
        ))
    }

    /// The codec the automatic ranking picks for `kind`, which is what
    /// `crate::encoding` hands to `resolve`.
    fn automatic_for(kind: EncoderKind, report: &CapabilityReport) -> Codec {
        let measured = report
            .encoder(kind)
            .map(measured_codecs)
            .unwrap_or_default();
        Codec::EFFICIENCY_ORDER
            .into_iter()
            .find(|codec| measured.contains(codec))
            .unwrap_or(Codec::H264)
    }

    #[test]
    fn the_same_preset_resolves_to_different_codecs_on_two_gpu_classes() {
        // Acceptance criterion 1, as a unit test over the two machines the
        // measurement in the pull request was taken on. Ultra asks for the most
        // efficient codec the encoder was *measured* to support, so it is AV1
        // on the NVIDIA card and HEVC on the integrated Radeon — whose driver
        // registers no AV1 transform, so AV1 is `unknown` there rather than a
        // measured no. A preset that named AV1 from a table would ask the AMD
        // encoder for a codec nothing said it has.
        let nvidia = nvidia_machine();
        let amd = amd_machine();

        let on_nvidia = QualityPreset::Ultra.resolve(
            EncoderKind::Nvenc,
            automatic_for(EncoderKind::Nvenc, &nvidia),
            &nvidia,
        );
        let on_amd = QualityPreset::Ultra.resolve(
            EncoderKind::Amf,
            automatic_for(EncoderKind::Amf, &amd),
            &amd,
        );

        assert_eq!(on_nvidia.codec(), Codec::Av1);
        assert_eq!(on_amd.codec(), Codec::Hevc);
        assert_ne!(
            on_nvidia.codec(),
            on_amd.codec(),
            "one preset resolving to one codec on both machines would mean it was read off a \
             table rather than off what each encoder reported"
        );
    }

    #[test]
    fn no_preset_ever_names_a_codec_the_machine_did_not_report() {
        // Acceptance criterion 2 for the preset itself: it cannot express an
        // unsupported combination, so there is none to select silently. Every
        // preset, on every encoder of two machines, must land on a codec that
        // encoder reported — measured, or H.264, which every encoder in the
        // workspace produces and which the reference table publishes for all of
        // them.
        for report in [nvidia_machine(), amd_machine()] {
            for encoder in report.encoders() {
                let kind = encoder.kind();
                let automatic = automatic_for(kind, &report);
                for preset in QualityPreset::ALL {
                    let resolved = preset.resolve(kind, automatic, &report);
                    let claim = encoder
                        .codec(resolved.codec())
                        .map(|support| support.supported().value().copied());
                    assert_ne!(
                        claim,
                        Some(Some(false)),
                        "{preset} on {kind:?} resolved to {}, which this encoder reported it \
                         does not produce",
                        resolved.codec().log_value(),
                    );
                }
            }
        }
    }

    #[test]
    fn performance_asks_for_h264_and_the_others_take_what_auto_chose() {
        // The one axis where a preset overrules the automatic ranking, and the
        // three where it does not. Balanced taking `automatic` unchanged is
        // what keeps this setting from changing any existing recording.
        let report = nvidia_machine();
        let automatic = automatic_for(EncoderKind::Nvenc, &report);
        assert_eq!(automatic, Codec::Av1);

        assert_eq!(
            QualityPreset::Performance.codec_for(EncoderKind::Nvenc, automatic, &report),
            Codec::H264
        );
        for preset in [
            QualityPreset::Balanced,
            QualityPreset::High,
            QualityPreset::Ultra,
        ] {
            assert_eq!(
                preset.codec_for(EncoderKind::Nvenc, automatic, &report),
                Codec::Av1,
                "{preset} must take the codec the ranking measured, not one of its own"
            );
        }
    }

    #[test]
    fn performance_keeps_the_automatic_codec_on_an_encoder_that_refuses_h264() {
        // A measured no for H.264 is a thing NVENC can answer, and it is the
        // one case where Performance's preference would name a codec the
        // machine does not have. It declines instead.
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(loaded(EncoderKind::Nvenc, "nvEncodeAPI64.dll"))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::Hevc,
                    "NVIDIA HEVC Encoder MFT",
                ))
                .with_limits(
                    EncoderLimits::new(EncoderKind::Nvenc, Codec::H264).with_supported(false),
                )
                .with_limits(
                    EncoderLimits::new(EncoderKind::Nvenc, Codec::Hevc).with_supported(true),
                ),
        );
        let report = detect(&facts);
        let automatic = automatic_for(EncoderKind::Nvenc, &report);

        assert_eq!(
            QualityPreset::Performance.codec_for(EncoderKind::Nvenc, automatic, &report),
            automatic,
            "an encoder that answered `no` for H.264 must not be asked for it"
        );
    }

    #[test]
    fn the_default_preset_spends_what_every_recording_before_it_was_given() {
        // 0.15 bits per pixel per frame is the constant `crate::encoding` held
        // before this module took it over. A change here changes the size and
        // the quality of every recording made by somebody who has configured
        // nothing, which is not something a preset setting is allowed to do on
        // its way in.
        assert_eq!(QualityPreset::default(), QualityPreset::Balanced);
        assert!((QualityPreset::Balanced.bits_per_pixel_per_frame() - 0.15).abs() < f64::EPSILON);
        assert_eq!(QualityPreset::Balanced.effort(), EncodePreset::Balanced);
    }

    #[test]
    fn the_presets_are_ordered_by_what_they_spend() {
        // The list a settings screen draws is in this order, so a preset that
        // spent less than the one before it would read as a mistake.
        for pair in QualityPreset::ALL.windows(2) {
            let (cheaper, dearer) = (pair[0], pair[1]);
            assert!(
                cheaper.bits_per_pixel_per_frame() < dearer.bits_per_pixel_per_frame(),
                "{cheaper} must spend fewer bits than {dearer}"
            );
            assert!(
                cheaper.effort() <= dearer.effort(),
                "{cheaper} must not ask the encoder for more effort than {dearer}"
            );
        }
    }

    #[test]
    fn every_preset_survives_the_round_trip_the_settings_file_makes() {
        for preset in QualityPreset::ALL {
            assert_eq!(QualityPreset::from_token(preset.token()), Some(preset));
        }
        assert_eq!(QualityPreset::from_token("custom"), None);
        assert_eq!(QualityPreset::from_token("Ultra"), None);
    }
}
