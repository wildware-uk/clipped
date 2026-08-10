//! Turning what the machine said into what it can do.
//!
//! [`detect`] is a pure function from [`SystemFacts`] to a
//! [`CapabilityReport`]: given the same answers it produces the same report, on
//! any machine, which is what makes the no-hardware path testable on a machine
//! that has hardware.
//!
//! It is not silent, though. Detection writes its results to the diagnostics
//! log as it goes, through the `encoder` field of the standard vocabulary
//! (docs/logging.md), because a capability report that only exists on somebody
//! else's screen is no use in a bug report — and leaving the logging to each
//! caller means the desktop application and the recorder would each have to
//! remember.

use core::fmt;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::adapter::{Adapter, AdapterId};
use crate::cache::{CacheState, CapabilityCache, HardwareSignature};
use crate::claim::Claim;
use crate::codec::{Codec, EncoderKind, Resolution};
use crate::probe::{
    HardwareEncoder, ProbeError, RuntimeObservation, RuntimeOutcome, SystemFacts, SystemProbe,
};
use crate::reference::{framerate_ceiling, limits};

/// Why an encoder cannot be used on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Unavailable {
    /// No adapter from that vendor is present, so the encoder cannot be.
    NoVendorAdapter,
    /// The adapter is there and its encoder runtime is not installed.
    RuntimeNotInstalled,
    /// The runtime is installed and would not load, which is a broken driver
    /// rather than an absent one.
    RuntimeFailedToLoad,
}

impl fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoVendorAdapter => "no adapter from this vendor is present",
            Self::RuntimeNotInstalled => "the adapter is present but its encoder runtime is not",
            Self::RuntimeFailedToLoad => {
                "the encoder runtime is installed but could not be loaded, which usually means \
                 a damaged or partly installed driver"
            }
        })
    }
}

/// Whether an encoder can be used, and why not when it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Present and usable, as far as anything short of opening a session can
    /// tell.
    Available,
    /// Not usable, for this reason.
    Unavailable(Unavailable),
}

impl Availability {
    /// Whether the encoder is available.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

impl fmt::Display for Availability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available => formatter.write_str("available"),
            Self::Unavailable(reason) => write!(formatter, "unavailable: {reason}"),
        }
    }
}

/// One measured fact that contributed to an encoder's availability.
///
/// The report prints these under each encoder, and they are the answer to "why
/// does it think that?" — the question a user with a working GPU and an
/// unavailable encoder needs answered, and the one a report of bare yes and no
/// cannot answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Signal {
    /// An adapter from this encoder's vendor is present.
    VendorAdapter(AdapterId),
    /// A vendor encoder runtime was loaded, or was not.
    Runtime(RuntimeObservation),
    /// Windows lists a hardware encoder for a codec.
    HardwareEncoder(HardwareEncoder),
    /// The software encoder needs no adapter and no runtime, so there was
    /// nothing to ask.
    NoHardwareRequired,
}

impl fmt::Display for Signal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VendorAdapter(adapter) => write!(formatter, "adapter {adapter} is this vendor's"),
            Self::Runtime(observation) => write!(formatter, "{observation}"),
            Self::HardwareEncoder(encoder) => {
                write!(formatter, "Windows lists \"{encoder}\"", encoder = encoder)
            }
            Self::NoHardwareRequired => {
                formatter.write_str("runs on the CPU, so needs no adapter or driver")
            }
        }
    }
}

/// What is known about one codec on one encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecSupport {
    codec: Codec,
    supported: Claim<bool>,
    max_resolution: Claim<Resolution>,
    max_luma_samples_per_second: Claim<u64>,
    b_frames: Claim<bool>,
    hdr: Claim<bool>,
}

impl CodecSupport {
    /// The codec.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// Whether the encoder can produce it.
    ///
    /// [`Claim::Measured`] means Windows reported a hardware encoder for it;
    /// [`Claim::Inferred`] means only the reference table says so;
    /// [`Claim::Unknown`] means nothing here knows, which is the honest answer
    /// for HEVC and AV1 on a driver that does not advertise them.
    #[must_use]
    pub const fn supported(&self) -> Claim<bool> {
        self.supported
    }

    /// The largest picture, where a limit is published.
    #[must_use]
    pub const fn max_resolution(&self) -> Claim<Resolution> {
        self.max_resolution
    }

    /// Whether B-frames are available.
    #[must_use]
    pub const fn b_frames(&self) -> Claim<bool> {
        self.b_frames
    }

    /// Whether 10-bit encoding — the necessary condition for HDR — is
    /// available.
    #[must_use]
    pub const fn hdr(&self) -> Claim<bool> {
        self.hdr
    }

    /// The framerate the codec's level limit permits at `resolution`.
    ///
    /// A ceiling from the *codec*, not from the silicon: see
    /// [`crate::reference`]. Nothing in this crate measures how fast an encoder
    /// actually is, and a number here is an upper bound on what the format
    /// allows rather than a promise about the hardware.
    #[must_use]
    pub fn max_framerate_at(&self, resolution: Resolution) -> Claim<u32> {
        self.max_luma_samples_per_second
            .map(|rate| framerate_ceiling(rate, resolution))
    }
}

/// What is known about one encoder family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderReport {
    kind: EncoderKind,
    availability: Availability,
    adapter: Option<AdapterId>,
    signals: Vec<Signal>,
    codecs: Vec<CodecSupport>,
}

impl EncoderReport {
    /// Which encoder family this is.
    #[must_use]
    pub const fn kind(&self) -> EncoderKind {
        self.kind
    }

    /// Whether it can be used, and why not when it cannot.
    #[must_use]
    pub const fn availability(&self) -> Availability {
        self.availability
    }

    /// The adapter it runs on, for a hardware encoder that is available.
    #[must_use]
    pub const fn adapter(&self) -> Option<AdapterId> {
        self.adapter
    }

    /// The measured facts behind [`availability`](Self::availability).
    #[must_use]
    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }

    /// What is known about each codec, most efficient first.
    ///
    /// Empty when the encoder is unavailable: an encoder that is not there has
    /// no codecs, and printing a table of limits under it would suggest
    /// otherwise.
    #[must_use]
    pub fn codecs(&self) -> &[CodecSupport] {
        &self.codecs
    }

    /// What is known about one codec, if anything.
    #[must_use]
    pub fn codec(&self, codec: Codec) -> Option<&CodecSupport> {
        self.codecs.iter().find(|support| support.codec == codec)
    }
}

/// Everything detection found: the adapters, and the encoders on them.
///
/// This is what the capability cache stores and what
/// [`recommend`](crate::recommend) ranks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    adapters: Vec<Adapter>,
    encoders: Vec<EncoderReport>,
}

impl CapabilityReport {
    /// The adapters DXGI enumerated, in its order.
    #[must_use]
    pub fn adapters(&self) -> &[Adapter] {
        &self.adapters
    }

    /// Every encoder family, in the order SPEC.md section 9 lists them —
    /// including the ones that are not available, because "Clipped did not find
    /// your NVIDIA card" is the report a user with a problem needs.
    #[must_use]
    pub fn encoders(&self) -> &[EncoderReport] {
        &self.encoders
    }

    /// One encoder family's report.
    #[must_use]
    pub fn encoder(&self, kind: EncoderKind) -> Option<&EncoderReport> {
        self.encoders.iter().find(|report| report.kind == kind)
    }

    /// One adapter by identifier.
    #[must_use]
    pub fn adapter(&self, id: AdapterId) -> Option<&Adapter> {
        self.adapters.iter().find(|adapter| adapter.id() == id)
    }

    /// Whether any hardware encoder is available.
    ///
    /// `false` is an ordinary answer: the software encoder is still there, and
    /// [`recommend`](crate::recommend) still has something to return.
    #[must_use]
    pub fn has_hardware_encoder(&self) -> bool {
        self.encoders
            .iter()
            .any(|report| report.kind.is_hardware() && report.availability.is_available())
    }
}

/// Where a report came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionSource {
    /// The machine was asked, in this run.
    Probed,
    /// The cache still matched the hardware, and this is what was stored.
    Cached {
        /// When the cached answer was originally measured.
        detected_at: SystemTime,
    },
}

impl fmt::Display for DetectionSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Probed => formatter.write_str("probed"),
            Self::Cached { .. } => formatter.write_str("cached"),
        }
    }
}

/// A report, plus how it was obtained and what that cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    report: CapabilityReport,
    source: DetectionSource,
    elapsed: Duration,
}

impl Detection {
    /// What was found.
    #[must_use]
    pub const fn report(&self) -> &CapabilityReport {
        &self.report
    }

    /// Whether the machine was asked or the cache answered.
    #[must_use]
    pub const fn source(&self) -> DetectionSource {
        self.source
    }

    /// How long this took, cache lookup included.
    ///
    /// Reported rather than assumed: the whole argument for caching is that
    /// asking is slow, and a number nobody measures is how that argument
    /// survives being wrong.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

/// Works out what a machine can encode from what it said.
///
/// Pure, apart from the diagnostics it writes: the same facts always produce
/// the same report. Tests build [`SystemFacts`] for machines that do not exist
/// here — no GPU at all, an integrated GPU only, two vendors at once — and get
/// the report those machines would produce.
#[must_use]
pub fn detect(facts: &SystemFacts) -> CapabilityReport {
    let encoders = EncoderKind::ALL
        .into_iter()
        .map(|kind| detect_encoder(kind, facts))
        .collect();

    let report = CapabilityReport {
        adapters: facts.adapters().to_vec(),
        encoders,
    };
    log_report(&report);
    report
}

/// Detects one encoder family.
fn detect_encoder(kind: EncoderKind, facts: &SystemFacts) -> EncoderReport {
    let mut signals = Vec::new();

    let Some(vendor) = kind.vendor() else {
        // The software encoder. Nothing to ask: it is available on any machine
        // that can run this process, which is what makes it the fallback the
        // recommendation can always reach.
        signals.push(Signal::NoHardwareRequired);
        return EncoderReport {
            kind,
            availability: Availability::Available,
            adapter: None,
            signals,
            codecs: codec_support(kind, &[]),
        };
    };

    let adapters: Vec<&Adapter> = facts
        .adapters()
        .iter()
        .filter(|adapter| adapter.vendor() == vendor && adapter.can_host_hardware_encoder())
        .collect();
    signals.extend(
        adapters
            .iter()
            .map(|adapter| Signal::VendorAdapter(adapter.id())),
    );

    let runtimes: Vec<&RuntimeObservation> = facts
        .encoders()
        .runtimes()
        .iter()
        .filter(|observation| observation.kind() == kind)
        .collect();
    signals.extend(
        runtimes
            .iter()
            .map(|observation| Signal::Runtime((*observation).clone())),
    );

    let hardware_encoders: Vec<&HardwareEncoder> = facts
        .encoders()
        .hardware_encoders()
        .iter()
        .filter(|encoder| encoder.vendor() == vendor)
        .collect();
    signals.extend(
        hardware_encoders
            .iter()
            .map(|encoder| Signal::HardwareEncoder((*encoder).clone())),
    );

    let availability = availability(&adapters, &runtimes, &hardware_encoders);
    let codecs = if availability.is_available() {
        codec_support(kind, &hardware_encoders)
    } else {
        Vec::new()
    };

    EncoderReport {
        kind,
        availability,
        adapter: choose_adapter(&adapters, &hardware_encoders),
        signals,
        codecs,
    }
}

/// Decides whether a hardware encoder is usable.
///
/// Two independent measurements can say yes, and either is enough. The vendor
/// runtime loading says the library this project will encode through is
/// installed and works; Windows listing a hardware encoder says the display
/// driver registered one. A machine can have the second without the first — a
/// driver that ships its media transforms but not its encode SDK runtime — and
/// reporting the encoder as absent in that case would be wrong.
fn availability(
    adapters: &[&Adapter],
    runtimes: &[&RuntimeObservation],
    hardware_encoders: &[&HardwareEncoder],
) -> Availability {
    if adapters.is_empty() && hardware_encoders.is_empty() {
        return Availability::Unavailable(Unavailable::NoVendorAdapter);
    }
    if runtimes.iter().any(|observation| observation.loaded()) || !hardware_encoders.is_empty() {
        return Availability::Available;
    }
    if runtimes
        .iter()
        .any(|observation| matches!(observation.outcome(), RuntimeOutcome::FailedToLoad { .. }))
    {
        return Availability::Unavailable(Unavailable::RuntimeFailedToLoad);
    }
    Availability::Unavailable(Unavailable::RuntimeNotInstalled)
}

/// Picks the adapter an encoder runs on.
///
/// Windows reports the adapter behind a hardware encoder on recent versions,
/// and that is the authoritative answer; where it does not, the vendor's own
/// adapter is used, preferring the one with the most video memory of its own,
/// which is right whenever a machine has one card per vendor. A machine with
/// two cards from one vendor and only one of them encoding is beyond what this
/// can tell, and the report says which adapter it picked rather than implying
/// it knew.
fn choose_adapter(
    adapters: &[&Adapter],
    hardware_encoders: &[&HardwareEncoder],
) -> Option<AdapterId> {
    hardware_encoders
        .iter()
        .find_map(|encoder| encoder.adapter())
        .filter(|id| adapters.iter().any(|adapter| adapter.id() == *id))
        .or_else(|| {
            adapters
                .iter()
                .max_by_key(|adapter| adapter.dedicated_video_memory())
                .map(|adapter| adapter.id())
        })
}

/// Builds the per-codec table: measured where Windows answered, from the
/// reference table where it did not.
fn codec_support(kind: EncoderKind, hardware_encoders: &[&HardwareEncoder]) -> Vec<CodecSupport> {
    Codec::EFFICIENCY_ORDER
        .into_iter()
        .map(|codec| {
            let entry = limits(kind, codec);
            let measured = hardware_encoders
                .iter()
                .any(|encoder| encoder.codec() == codec);

            CodecSupport {
                codec,
                // A measurement replaces the table entry; the absence of one
                // does not. Windows not listing an AV1 transform is not proof
                // that the encoder cannot produce AV1 — it may simply not
                // expose one — so the fallback is what the table says, which
                // for AV1 is `Unknown`.
                supported: if measured {
                    Claim::Measured(true)
                } else {
                    entry.supported
                },
                max_resolution: entry.max_resolution,
                max_luma_samples_per_second: entry.max_luma_samples_per_second,
                b_frames: entry.b_frames,
                hdr: entry.hdr,
            }
        })
        .collect()
}

/// Detects, using the cache when the hardware has not changed under it.
///
/// The cheap half of the probe runs every time, because its answer is the cache
/// key: a new GPU or a driver update has to invalidate the stored report, and
/// nothing else on the machine can be trusted to say that it happened. The
/// expensive half — starting Media Foundation, loading vendor runtimes — runs
/// only on a miss.
///
/// A cache that cannot be read or cannot be written never fails a detection.
/// The worst a broken cache can do is make this slow, and refusing to report
/// capabilities because a file in `%LOCALAPPDATA%` is corrupt would be choosing
/// the cache over the user (AGENTS.md section 17).
///
/// # Errors
///
/// [`ProbeError`] when the machine could not be asked at all.
pub fn detect_cached(
    probe: &dyn SystemProbe,
    cache: &CapabilityCache,
) -> Result<Detection, ProbeError> {
    let started = Instant::now();
    let adapters = probe.adapters()?;
    let signature = HardwareSignature::of(&adapters);

    match cache.load(&signature) {
        CacheState::Fresh {
            report,
            detected_at,
        } => {
            log_report(&report);
            tracing::debug!(
                // Redacted because the path runs through the user's account
                // name (AGENTS.md section 14, docs/logging.md).
                cache = cache
                    .path()
                    .map(|path| clipped_logging::RedactedPath::new(path).to_string()),
                "capability report read from the cache; the hardware signature still matches"
            );
            Ok(Detection {
                report,
                source: DetectionSource::Cached { detected_at },
                elapsed: started.elapsed(),
            })
        }
        CacheState::Stale(reason) => {
            tracing::debug!(%reason, "probing encoder capabilities");
            let facts = SystemFacts::new(adapters, probe.encoders()?);
            let report = detect(&facts);
            if let Err(error) = cache.store(&signature, &report) {
                // Not fatal, and not silent either (AGENTS.md section 15): the
                // detection stands, and the next run will simply do the work
                // again.
                tracing::warn!(
                    %error,
                    "the encoder capability cache could not be written, so the next run \
                     will probe again"
                );
            }
            Ok(Detection {
                report,
                source: DetectionSource::Probed,
                elapsed: started.elapsed(),
            })
        }
    }
}

/// Writes the report to the diagnostics log.
///
/// One line per adapter and one per encoder at `info`, one per codec at
/// `debug`. The `encoder` field carries the standard vocabulary word so that a
/// search for `encoder=nvenc` finds these lines and a recording session's
/// alike (docs/logging.md, "Standard fields").
fn log_report(report: &CapabilityReport) {
    for adapter in report.adapters() {
        tracing::info!(
            adapter = %adapter.id(),
            vendor = %adapter.vendor(),
            adapter_kind = adapter.kind().log_value(),
            device_id = format_args!("0x{:04X}", adapter.device_id()),
            driver_version = adapter.driver_version().map(|version| version.to_string()),
            dedicated_video_memory_bytes = adapter.dedicated_video_memory(),
            description = adapter.description(),
            "display adapter"
        );
    }

    for encoder in report.encoders() {
        let kind = encoder.kind();
        let measured: Vec<&str> = encoder
            .codecs()
            .iter()
            .filter(|support| support.supported().is_measured_true())
            .map(|support| support.codec().log_value())
            .collect();

        tracing::info!(
            encoder = %kind.log_encoder_family(),
            available = encoder.availability().is_available(),
            availability = %encoder.availability(),
            adapter = encoder.adapter().map(|id| id.to_string()),
            measured_codecs = measured.join(","),
            "encoder detected"
        );

        for support in encoder.codecs() {
            tracing::debug!(
                encoder = %kind.log_encoder_family(),
                codec = support.codec().log_value(),
                supported = %support.supported(),
                max_resolution = %support.max_resolution(),
                max_framerate_1080p = %support.max_framerate_at(Resolution::HD_1080P),
                b_frames = %support.b_frames(),
                hdr = %support.hdr(),
                "encoder codec capability"
            );
        }
    }

    tracing::info!(
        adapters = report.adapters().len(),
        hardware_encoder_available = report.has_hardware_encoder(),
        "encoder capability detection complete"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterKind;
    use crate::codec::Vendor;
    use crate::probe::EncoderObservations;

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

    fn basic_render_driver() -> Adapter {
        Adapter::new(
            AdapterId::from_luid(3, 0),
            "Microsoft Basic Render Driver",
            Vendor::Microsoft,
            0x008C,
            0,
            true,
        )
    }

    fn nvenc_runtime(outcome: RuntimeOutcome) -> RuntimeObservation {
        RuntimeObservation::new(EncoderKind::Nvenc, "nvEncodeAPI64.dll", outcome)
    }

    #[test]
    fn a_machine_with_no_display_adapter_still_reports_the_software_encoder() {
        // The no-hardware path. It cannot be produced on the machine this was
        // written on — the GPUs are soldered in — so it is produced by handing
        // detection the facts such a machine would report.
        let report = detect(&SystemFacts::new(Vec::new(), EncoderObservations::none()));

        assert!(!report.has_hardware_encoder());
        for kind in [EncoderKind::Nvenc, EncoderKind::Amf, EncoderKind::QuickSync] {
            let encoder = report.encoder(kind).expect("every encoder is reported");
            assert_eq!(
                encoder.availability(),
                Availability::Unavailable(Unavailable::NoVendorAdapter),
                "{kind} should be unavailable with a reason"
            );
            assert!(
                encoder.codecs().is_empty(),
                "{kind} is not there, so it has no codecs"
            );
        }

        let software = report
            .encoder(EncoderKind::Software)
            .expect("the software encoder is always reported");
        assert!(software.availability().is_available());
        assert_eq!(
            software.codec(Codec::H264).map(CodecSupport::supported),
            Some(Claim::Inferred(true))
        );
    }

    #[test]
    fn a_machine_with_only_a_software_rasteriser_has_no_hardware_encoder() {
        let report = detect(&SystemFacts::new(
            vec![basic_render_driver()],
            EncoderObservations::none(),
        ));

        assert!(!report.has_hardware_encoder());
        assert_eq!(
            report
                .encoder(EncoderKind::Nvenc)
                .map(EncoderReport::availability),
            Some(Availability::Unavailable(Unavailable::NoVendorAdapter))
        );
    }

    #[test]
    fn a_card_whose_runtime_is_missing_is_unavailable_for_a_different_reason() {
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none().with_runtime(nvenc_runtime(RuntimeOutcome::NotFound)),
        );
        let report = detect(&facts);

        assert_eq!(
            report
                .encoder(EncoderKind::Nvenc)
                .map(EncoderReport::availability),
            Some(Availability::Unavailable(Unavailable::RuntimeNotInstalled))
        );
    }

    #[test]
    fn a_runtime_that_is_present_and_broken_is_reported_as_broken() {
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::FailedToLoad { code: 126 })),
        );
        let report = detect(&facts);

        let encoder = report
            .encoder(EncoderKind::Nvenc)
            .expect("nvenc is reported");
        assert_eq!(
            encoder.availability(),
            Availability::Unavailable(Unavailable::RuntimeFailedToLoad),
            "a damaged driver and an absent one are different problems"
        );
    }

    #[test]
    fn a_codec_windows_reports_is_measured_and_one_it_does_not_is_not() {
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::Loaded))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    None,
                    Codec::H264,
                    "NVIDIA H.264 Encoder MFT",
                )),
        );
        let report = detect(&facts);
        let nvenc = report
            .encoder(EncoderKind::Nvenc)
            .expect("nvenc is reported");

        assert_eq!(
            nvenc.codec(Codec::H264).map(CodecSupport::supported),
            Some(Claim::Measured(true))
        );
        // Nothing measured AV1, and the table refuses to guess: this is the
        // claim that must never become `Inferred(true)`.
        assert_eq!(
            nvenc.codec(Codec::Av1).map(CodecSupport::supported),
            Some(Claim::Unknown)
        );
    }

    #[test]
    fn a_driver_with_media_transforms_but_no_runtime_is_still_available() {
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::NotFound))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    None,
                    Codec::Hevc,
                    "NVIDIA HEVC Encoder MFT",
                )),
        );

        assert_eq!(
            detect(&facts)
                .encoder(EncoderKind::Nvenc)
                .map(EncoderReport::availability),
            Some(Availability::Available)
        );
    }

    #[test]
    fn two_vendors_are_reported_separately_and_each_gets_its_own_adapter() {
        let facts = SystemFacts::new(
            vec![nvidia_card(), integrated_amd()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::Loaded))
                .with_runtime(RuntimeObservation::new(
                    EncoderKind::Amf,
                    "amfrt64.dll",
                    RuntimeOutcome::Loaded,
                )),
        );
        let report = detect(&facts);

        assert_eq!(
            report
                .encoder(EncoderKind::Nvenc)
                .and_then(EncoderReport::adapter),
            Some(nvidia_card().id())
        );
        assert_eq!(
            report
                .encoder(EncoderKind::Amf)
                .and_then(EncoderReport::adapter),
            Some(integrated_amd().id())
        );
        assert_eq!(
            report
                .encoder(EncoderKind::QuickSync)
                .map(EncoderReport::availability),
            Some(Availability::Unavailable(Unavailable::NoVendorAdapter)),
            "there is no Intel adapter, so Quick Sync is not available"
        );
    }

    #[test]
    fn the_adapter_windows_names_wins_over_the_guess() {
        let second_card = Adapter::new(
            AdapterId::from_luid(9, 0),
            "NVIDIA GeForce RTX 3060",
            Vendor::Nvidia,
            0x2503,
            12 * 1024 * 1024 * 1024,
            false,
        );
        let facts = SystemFacts::new(
            vec![nvidia_card(), second_card.clone()],
            EncoderObservations::none().with_hardware_encoder(HardwareEncoder::new(
                Vendor::Nvidia,
                Some(second_card.id()),
                Codec::H264,
                "NVIDIA H.264 Encoder MFT",
            )),
        );

        assert_eq!(
            detect(&facts)
                .encoder(EncoderKind::Nvenc)
                .and_then(EncoderReport::adapter),
            Some(second_card.id())
        );
    }

    #[test]
    fn the_report_keeps_every_adapter_including_the_ones_that_cannot_encode() {
        let facts = SystemFacts::new(
            vec![nvidia_card(), basic_render_driver()],
            EncoderObservations::none(),
        );
        let report = detect(&facts);

        assert_eq!(report.adapters().len(), 2);
        assert_eq!(
            report
                .adapter(basic_render_driver().id())
                .map(Adapter::kind),
            Some(AdapterKind::Software)
        );
    }

    #[test]
    fn a_codec_ceiling_is_reported_per_resolution() {
        let facts = SystemFacts::new(vec![], EncoderObservations::none());
        let report = detect(&facts);
        let software = report
            .encoder(EncoderKind::Software)
            .expect("the software encoder is always reported");
        let h264 = software.codec(Codec::H264).expect("H.264 is reported");

        // The software encoder's ceiling is the CPU, which nothing here
        // measures, so there is no number to give.
        assert_eq!(h264.max_framerate_at(Resolution::HD_1080P), Claim::Unknown);
    }
}
