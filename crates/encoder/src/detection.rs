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
    EncoderLimits, HardwareEncoder, ProbeError, Probing, RuntimeObservation, RuntimeOutcome,
    SystemFacts, SystemProbe,
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
    /// An encoder session was opened for a codec and described its own limits.
    ///
    /// The evidence behind every unmarked number in that codec's row, and the
    /// answer to "why does this machine's report have measurements where the
    /// documentation says these are inferred?" — because somebody ran
    /// `capabilities --refresh` and paid for a session.
    SessionLimits(Codec),
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
            Self::SessionLimits(codec) => write!(
                formatter,
                "an encoder session reported its own {codec} limits"
            ),
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
    /// [`Claim::Measured`] means either that Windows reported a hardware
    /// encoder for it, or that the vendor runtime itself was asked and answered
    /// — which is the only way this becomes a measured `false`, because a
    /// missing transform proves nothing and an encoder that does not enumerate
    /// a codec proves everything. [`Claim::Inferred`] means only the reference
    /// table says so; [`Claim::Unknown`] means nothing here knows, which is the
    /// honest answer for HEVC and AV1 on a driver that does not advertise them
    /// and has not been asked.
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

    /// The framerate ceiling at `resolution`, and what kind of ceiling it is.
    ///
    /// The two evidence values mean different things here, and the difference
    /// is the point of the qualifier:
    ///
    /// - [`Claim::Measured`] is the encoder's own answer to how many samples a
    ///   second it can process — a statement about this silicon, from
    ///   `NV_ENC_CAPS_MB_PER_SEC_MAX` or AMF's `MaxThroughput`.
    /// - [`Claim::Inferred`] is the *codec's* level limit and nothing about the
    ///   hardware at all: HEVC Level 6.2 permits over two thousand frames a
    ///   second at 1080p and no encoder does that (see [`crate::reference`]).
    ///
    /// Neither is a promise that a recording at that rate will keep up, because
    /// a ceiling on the encoder is not a ceiling on the capture, the disk or
    /// the machine.
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

    /// Whether an encoder session was opened and asked about this encoder.
    ///
    /// The difference between "these limits are published because nobody has
    /// asked" and "these limits are published because the encoder would not
    /// say", which is the difference between a refresh being worth suggesting
    /// and being a waste of the user's session slot.
    #[must_use]
    pub fn was_asked(&self) -> bool {
        self.signals
            .iter()
            .any(|signal| matches!(signal, Signal::SessionLimits(_)))
    }

    /// Whether any codec here still carries a limit from the published table.
    #[must_use]
    pub fn has_inferred_limit(&self) -> bool {
        self.codecs.iter().any(|support| {
            [
                support.max_resolution.evidence(),
                support.max_luma_samples_per_second.evidence(),
                support.b_frames.evidence(),
                support.hdr.evidence(),
            ]
            .into_iter()
            .any(|evidence| evidence == Some(crate::claim::Evidence::Inferred))
        })
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

    /// Whether an available hardware encoder is still quoting published limits
    /// without ever having been asked for its own.
    ///
    /// What the report uses to decide whether telling the reader about
    /// `--refresh` would gain them anything, and the reason it is this question
    /// rather than "is anything inferred?": some limits stay inferred however
    /// many times a machine is asked — NVENC's framerate ceiling is deliberately
    /// not published (`crate::windows::nvenc`) — so a report that advertised a
    /// refresh whenever it saw an `(i)` would go on advertising it to somebody
    /// who had just done one.
    #[must_use]
    pub fn has_unasked_encoder(&self) -> bool {
        self.encoders
            .iter()
            .filter(|report| report.kind.is_hardware() && report.availability.is_available())
            .any(|report| !report.was_asked() && report.has_inferred_limit())
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
            codecs: codec_support(kind, &[], &[]),
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

    let measured: Vec<&EncoderLimits> = facts
        .encoders()
        .limits()
        .iter()
        .filter(|limits| limits.kind() == kind)
        .collect();
    signals.extend(
        measured
            .iter()
            .filter(|limits| !limits.is_empty())
            .map(|limits| Signal::SessionLimits((*limits).codec())),
    );

    let availability = availability(&adapters, &runtimes, &hardware_encoders);
    let codecs = if availability.is_available() {
        codec_support(kind, &hardware_encoders, &measured)
    } else {
        Vec::new()
    };

    EncoderReport {
        kind,
        availability,
        // The same rule the capability probe follows when it chooses which
        // adapter to open a session on, from the same function.
        adapter: crate::adapter::encoding_adapter(facts.adapters(), vendor).map(Adapter::id),
        signals,
        codecs,
    }
}

/// Decides whether a hardware encoder is usable.
///
/// **The vendor runtime decides.** Issues #15 to #17 encode through
/// `nvEncodeAPI64.dll`, `amfrt64.dll` and Intel's media runtime, not through
/// the Media Foundation transforms, so a library that will not load is an
/// encoder that will not work whatever else the system says about it — which is
/// the rule `crate::windows::runtime` states and this function applies.
///
/// A driver that registers its media transforms without installing its encode
/// SDK runtime is therefore reported as unavailable, with the reason. That is
/// the deliberate choice: the transforms are real, and a recording made through
/// them is not something this project can offer, so calling the encoder
/// available would promise a capability the recording would not have.
///
/// The transforms are still a measurement, of a different question — which
/// codecs the installed driver registers an encoder for — and they are kept as
/// [`Signal`]s and used for [`codec_support`] whether or not the runtime loads.
fn availability(
    adapters: &[&Adapter],
    runtimes: &[&RuntimeObservation],
    hardware_encoders: &[&HardwareEncoder],
) -> Availability {
    if adapters.is_empty() && hardware_encoders.is_empty() {
        return Availability::Unavailable(Unavailable::NoVendorAdapter);
    }
    if runtimes.iter().any(|observation| observation.loaded()) {
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

/// Builds the per-codec table: measured where something answered, from the
/// reference table where nothing did.
///
/// Two kinds of measurement arrive here and they are not interchangeable.
/// `hardware_encoders` is what Windows lists, which costs no session and
/// answers only "is there an encoder for this codec". `measured` is what an
/// encoder session said when one was opened, which answers the numeric limits
/// as well — and only exists when the caller asked for
/// [`Probing::WithSessions`].
///
/// The rule for combining them is the same in every field: **a measurement
/// replaces the table entry and the absence of one leaves it alone.** A field
/// nobody measured keeps the published limit, still labelled inferred, rather
/// than becoming a measurement because the session it might have come from
/// happened to open.
fn codec_support(
    kind: EncoderKind,
    hardware_encoders: &[&HardwareEncoder],
    measured: &[&EncoderLimits],
) -> Vec<CodecSupport> {
    Codec::EFFICIENCY_ORDER
        .into_iter()
        .map(|codec| {
            let entry = limits(kind, codec);
            let session = measured
                .iter()
                .copied()
                .find(|limits| limits.codec() == codec);
            let registered = hardware_encoders
                .iter()
                .any(|encoder| encoder.codec() == codec);

            CodecSupport {
                codec,
                // Windows not listing an AV1 transform is not proof that the
                // encoder cannot produce AV1 — a driver may simply not register
                // one — so a missing transform falls through to what the table
                // says, which for AV1 is `Unknown`. The vendor runtime's own
                // codec list is a stronger answer than either, in both
                // directions: an encoder that does not enumerate a codec cannot
                // encode it, whatever Windows registered.
                supported: measured_or(
                    session.and_then(EncoderLimits::supported),
                    if registered {
                        Claim::Measured(true)
                    } else {
                        entry.supported
                    },
                ),
                max_resolution: measured_or(
                    session.and_then(EncoderLimits::max_resolution),
                    entry.max_resolution,
                ),
                max_luma_samples_per_second: measured_or(
                    session.and_then(EncoderLimits::max_luma_samples_per_second),
                    entry.max_luma_samples_per_second,
                ),
                b_frames: measured_or(session.and_then(EncoderLimits::b_frames), entry.b_frames),
                hdr: measured_or(session.and_then(EncoderLimits::hdr), entry.hdr),
            }
        })
        .collect()
}

/// A measurement if there is one, and the published limit if there is not.
///
/// One function so that the rule is written once. Five fields each deciding for
/// themselves is how one of them ends up promoting a table entry to
/// [`Claim::Measured`].
fn measured_or<T>(measurement: Option<T>, published: Claim<T>) -> Claim<T> {
    measurement.map_or(published, Claim::Measured)
}

/// Detects, using the cache when the hardware has not changed under it.
///
/// The cheap half of the probe runs every time, because its answer is the cache
/// key: a new GPU or a driver update has to invalidate the stored report, and
/// nothing else on the machine can be trusted to say that it happened. The
/// expensive half — starting Media Foundation, loading vendor runtimes, and
/// with [`Probing::WithSessions`] opening an encoder — runs only on a miss.
///
/// `probing` decides whether an encoder session may be opened. It applies only
/// to a miss: a cache hit opens nothing whatever it says, because the answers
/// are already stored. So a run that measures the limits is a run that was
/// going to probe anyway, and the machine pays for a session once per
/// `--refresh` rather than once per question.
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
    probing: Probing,
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
            tracing::debug!(%reason, %probing, "probing encoder capabilities");
            let mut observations = probe.encoders()?;

            if probing.opens_sessions() {
                // Timed separately from the rest, because this is the half the
                // whole design of the cache is an argument about: a number
                // nobody measures is how that argument survives being wrong
                // (AGENTS.md section 18).
                let session_probe = Instant::now();
                let measured = probe.encoder_limits(&adapters)?;
                let codecs = measured.len();
                for limits in measured {
                    observations = observations.with_limits(limits);
                }
                tracing::info!(
                    elapsed_ms = session_probe.elapsed().as_millis(),
                    codecs,
                    "asked the encoder sessions for their own limits"
                );
            }

            let facts = SystemFacts::new(adapters, observations);
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
        // The same list a settings screen would offer, from the same function,
        // so that a log line and a menu cannot disagree about what was
        // measured.
        let measured: Vec<&str> = crate::recommendation::measured_codecs(encoder)
            .into_iter()
            .map(Codec::log_value)
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
    fn a_driver_with_media_transforms_but_no_runtime_is_not_available() {
        // The rule this crate exists to enforce, in its least obvious case.
        // Windows lists an HEVC encoder, so something on this machine can
        // encode HEVC — but issues #15 to #17 encode through the vendor
        // runtime, and that is not installed, so Clipped cannot. Reporting
        // "available" here would promise a capability the recording would not
        // have, which is the failure the whole crate is shaped around.
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::NotFound))
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

        assert_eq!(
            nvenc.availability(),
            Availability::Unavailable(Unavailable::RuntimeNotInstalled)
        );
        // The transform is still evidence, and is still shown: the report has
        // to be able to explain why a machine with a visible HEVC encoder is
        // being told it has none.
        assert!(
            nvenc.signals().iter().any(|signal| matches!(
                signal,
                Signal::HardwareEncoder(encoder) if encoder.codec() == Codec::Hevc
            )),
            "the transform Windows listed must still be reported: {:?}",
            nvenc.signals()
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
    fn two_cards_from_one_vendor_are_attributed_to_the_larger_one_because_nothing_says_which() {
        // Attribution is a guess and this is its rule. Nothing measured says
        // which of two NVIDIA cards holds the encoder — Media Foundation's
        // `MFT_ENUM_ADAPTER_LUID` is an input filter for `MFTEnum2`, not an
        // attribute on the transforms, and no transform on the development
        // machine carries it — so the card with the most video memory of its
        // own is named, and the report shows which one it picked.
        let smaller_card = Adapter::new(
            AdapterId::from_luid(9, 0),
            "NVIDIA GeForce RTX 3060",
            Vendor::Nvidia,
            0x2503,
            12 * 1024 * 1024 * 1024,
            false,
        );
        let facts = SystemFacts::new(
            // The smaller card first, so that a rule which merely kept DXGI's
            // order would fail here.
            vec![smaller_card, nvidia_card()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::Loaded))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::H264,
                    "NVIDIA H.264 Encoder MFT",
                )),
        );

        assert_eq!(
            detect(&facts)
                .encoder(EncoderKind::Nvenc)
                .and_then(EncoderReport::adapter),
            Some(nvidia_card().id())
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
    fn a_limit_an_encoder_session_answered_is_reported_as_measured() {
        // The whole of issue #133 in one assertion: a number the encoder itself
        // gave has to arrive as `Measured`, so that the report prints it
        // without the marker that says "we looked this up".
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::Loaded))
                .with_limits(
                    EncoderLimits::new(EncoderKind::Nvenc, Codec::Hevc)
                        .with_supported(true)
                        .with_max_resolution(Resolution::new(8192, 8192))
                        .with_b_frames(true)
                        .with_hdr(true),
                ),
        );
        let report = detect(&facts);
        let hevc = report
            .encoder(EncoderKind::Nvenc)
            .and_then(|nvenc| nvenc.codec(Codec::Hevc))
            .copied()
            .expect("HEVC is reported");

        assert_eq!(hevc.supported(), Claim::Measured(true));
        assert_eq!(
            hevc.max_resolution(),
            Claim::Measured(Resolution::new(8192, 8192))
        );
        assert_eq!(hevc.b_frames(), Claim::Measured(true));
        assert_eq!(hevc.hdr(), Claim::Measured(true));
    }

    #[test]
    fn a_field_the_session_did_not_answer_keeps_the_published_limit() {
        // The rule that keeps the distinction worth anything. A session that
        // opened and answered two questions must not turn the other three into
        // measurements, and the table's own `Unknown` must survive as well: the
        // NVENC HEVC row publishes no B-frame claim, and a session that did not
        // mention them leaves it that way.
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::Loaded))
                .with_limits(
                    EncoderLimits::new(EncoderKind::Nvenc, Codec::Hevc)
                        .with_supported(true)
                        .with_max_resolution(Resolution::new(4096, 4096)),
                ),
        );
        let report = detect(&facts);
        let hevc = report
            .encoder(EncoderKind::Nvenc)
            .and_then(|nvenc| nvenc.codec(Codec::Hevc))
            .copied()
            .expect("HEVC is reported");

        assert_eq!(
            hevc.max_resolution(),
            Claim::Measured(Resolution::new(4096, 4096))
        );
        assert!(
            !hevc.max_framerate_at(Resolution::HD_1080P).is_measured(),
            "the framerate ceiling was not measured and must not say it was: {}",
            hevc.max_framerate_at(Resolution::HD_1080P)
        );
        assert_eq!(
            hevc.b_frames(),
            Claim::Unknown,
            "a session that said nothing about B-frames cannot have settled them"
        );
    }

    #[test]
    fn an_encoder_that_says_it_cannot_produce_a_codec_is_believed_over_the_transforms() {
        // Windows lists an AV1 transform and the vendor runtime does not
        // enumerate AV1. Clipped records through the runtime, so the runtime is
        // the answer — and this is a measured "no", which is different from the
        // table's `Unknown` and different again from the `Measured(true)` the
        // transform alone would have produced.
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::Loaded))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    Codec::Av1,
                    "NVIDIA AV1 Encoder MFT",
                ))
                .with_limits(
                    EncoderLimits::new(EncoderKind::Nvenc, Codec::Av1).with_supported(false),
                ),
        );
        let report = detect(&facts);

        assert_eq!(
            report
                .encoder(EncoderKind::Nvenc)
                .and_then(|nvenc| nvenc.codec(Codec::Av1))
                .map(CodecSupport::supported),
            Some(Claim::Measured(false))
        );
    }

    #[test]
    fn one_encoder_s_measurements_do_not_reach_another_s_row() {
        // Two vendors, one of them measured. Attributing NVENC's maximum size
        // to AMD would be a measurement of hardware that never gave it, and
        // both are 4096-wide often enough that a wrong answer would look right.
        let facts = SystemFacts::new(
            vec![nvidia_card(), integrated_amd()],
            EncoderObservations::none()
                .with_runtime(nvenc_runtime(RuntimeOutcome::Loaded))
                .with_runtime(RuntimeObservation::new(
                    EncoderKind::Amf,
                    "amfrt64.dll",
                    RuntimeOutcome::Loaded,
                ))
                .with_limits(
                    EncoderLimits::new(EncoderKind::Nvenc, Codec::H264)
                        .with_max_resolution(Resolution::new(8192, 8192)),
                ),
        );
        let report = detect(&facts);

        assert_eq!(
            report
                .encoder(EncoderKind::Amf)
                .and_then(|amf| amf.codec(Codec::H264))
                .map(CodecSupport::max_resolution),
            Some(Claim::Inferred(Resolution::new(4096, 2160))),
            "AMF's row must keep the published limit it had before NVENC was asked"
        );
    }

    #[test]
    fn a_report_says_whether_anything_is_still_worth_asking_for() {
        // What the printed report uses to decide whether to mention
        // `--refresh`. An encoder nobody has asked has something to gain from
        // one; an encoder that was asked has not, *even though* some of its
        // limits are still published — asking again would produce the same
        // report, and suggesting it would cost a session slot for nothing.
        let unasked = detect(&SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none().with_runtime(nvenc_runtime(RuntimeOutcome::Loaded)),
        ));
        assert!(unasked.has_unasked_encoder());

        // Answered about one field only, which is the NVENC shape: the
        // framerate ceiling stays published for ever, so a rule that looked for
        // an inferred value rather than for an unasked encoder would advertise
        // a refresh to somebody who had just done one.
        let asked = detect(&SystemFacts::new(
            vec![nvidia_card()],
            Codec::EFFICIENCY_ORDER.into_iter().fold(
                EncoderObservations::none().with_runtime(nvenc_runtime(RuntimeOutcome::Loaded)),
                |observations, codec| {
                    observations.with_limits(
                        EncoderLimits::new(EncoderKind::Nvenc, codec)
                            .with_supported(true)
                            .with_max_resolution(Resolution::new(4096, 4096)),
                    )
                },
            ),
        ));
        assert!(
            asked
                .encoder(EncoderKind::Nvenc)
                .is_some_and(EncoderReport::has_inferred_limit),
            "this report should still have published limits in it, or it tests nothing"
        );
        assert!(
            !asked.has_unasked_encoder(),
            "the only hardware encoder has been asked: {asked:?}"
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
