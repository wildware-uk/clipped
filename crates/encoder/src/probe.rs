//! What the machine was asked, and what it answered.
//!
//! This module is the seam between "ask Windows" and "work out what that
//! means". Everything above it — [`crate::detection`], [`crate::recommendation`]
//! and [`crate::cache`] — consumes [`SystemFacts`] and never calls an API, so
//! the whole of the reasoning can be tested against machines this one is not:
//! a laptop with no discrete GPU, a virtual machine with only the Basic Render
//! Driver, a workstation with two vendors' cards in it.
//!
//! # Why detection is split in two
//!
//! [`SystemProbe`] has two methods rather than one because they cost very
//! different amounts. Enumerating adapters is a DXGI call and a handful of
//! struct reads; finding out which encoders exist means starting Media
//! Foundation and loading vendor runtimes. The cheap half produces the cache
//! key ([`crate::cache`]), so a run that hits the cache never pays for the
//! expensive half.
//!
//! # What costs a session, and when it is paid
//!
//! Creating an encoder session is the only way to learn the numeric limits —
//! how large a picture the encoder takes, how much it can encode per second,
//! whether it has B-frames, whether it will encode 10-bit. It is also the one
//! measurement that allocates GPU memory and takes an encode session slot from
//! a machine that may be in the middle of a match, so it is not something to do
//! behind a user's back.
//!
//! [`Probing`] is that choice, made by the caller rather than here. Every
//! ordinary run asks only the questions that cost nothing and reports the
//! numeric limits as inferred; a run the user asked for — `capabilities
//! --refresh` — opens one session per available hardware encoder, asks it, and
//! caches the answers so that the next run pays nothing for them
//! (`docs/encoder-capabilities.md`).

use core::fmt;
use std::error::Error;

use serde::{Deserialize, Serialize};

use crate::adapter::Adapter;
use crate::codec::{Codec, EncoderKind, Resolution, Vendor};

/// Why the machine could not be asked.
///
/// Distinct from "asked, and the answer was no": a report built on a failed
/// probe would say "no encoders" in exactly the words a machine with no GPU
/// deserves, and the two must not be confusable (AGENTS.md section 15).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProbeError {
    /// A Windows API call failed.
    Api {
        /// The call, named as the documentation names it.
        operation: &'static str,
        /// The `HRESULT` or error code it returned.
        code: i32,
        /// What the system said about it, where it said anything.
        message: String,
    },
    /// This build has no way of asking, because it is not for Windows.
    ///
    /// Clipped targets Windows (SPEC.md section 3); the crates still build and
    /// test elsewhere, and this is what the platform-specific half becomes
    /// there.
    UnsupportedPlatform,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api {
                operation,
                code,
                message,
            } => {
                write!(formatter, "{operation} failed with 0x{code:08X}")?;
                if !message.is_empty() {
                    write!(formatter, ": {message}")?;
                }
                Ok(())
            }
            Self::UnsupportedPlatform => formatter.write_str(
                "encoder detection needs the Windows display and media APIs, and this build \
                 is not for Windows",
            ),
        }
    }
}

impl Error for ProbeError {}

/// What happened when a vendor's encoder runtime was loaded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOutcome {
    /// It loaded. The runtime this project will encode through is present and
    /// working well enough to be brought into a process.
    Loaded,
    /// It is not installed. The usual reason: no card from that vendor, or a
    /// driver installed without its encoder component.
    NotFound,
    /// It is installed and would not load, which is a broken driver rather than
    /// an absent one, and is worth saying differently.
    FailedToLoad {
        /// The Windows error code from the load attempt.
        code: u32,
    },
}

impl fmt::Display for RuntimeOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loaded => formatter.write_str("loaded"),
            Self::NotFound => formatter.write_str("not installed"),
            Self::FailedToLoad { code } => {
                write!(formatter, "installed but failed to load (error {code})")
            }
        }
    }
}

/// One attempt to load a vendor's encoder runtime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuntimeObservation {
    kind: EncoderKind,
    library: String,
    outcome: RuntimeOutcome,
}

impl RuntimeObservation {
    /// Records an attempt.
    #[must_use]
    pub fn new(kind: EncoderKind, library: impl Into<String>, outcome: RuntimeOutcome) -> Self {
        Self {
            kind,
            library: library.into(),
            outcome,
        }
    }

    /// Which encoder family the library belongs to.
    #[must_use]
    pub const fn kind(&self) -> EncoderKind {
        self.kind
    }

    /// The library that was tried, by file name.
    #[must_use]
    pub fn library(&self) -> &str {
        &self.library
    }

    /// What happened.
    #[must_use]
    pub const fn outcome(&self) -> &RuntimeOutcome {
        &self.outcome
    }

    /// Whether this observation shows a usable runtime.
    #[must_use]
    pub const fn loaded(&self) -> bool {
        matches!(self.outcome, RuntimeOutcome::Loaded)
    }
}

impl fmt::Display for RuntimeObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.library, self.outcome)
    }
}

/// A hardware encoder the operating system says exists.
///
/// One of these is a Media Foundation transform that a display driver
/// registered, filtered to hardware and to video encoders, and asked for by
/// output codec. Windows answering "there is an AV1 encoder from vendor 0x10DE"
/// is a measurement of the driver actually installed, which is the property a
/// table keyed on a GPU model cannot have.
/// The vendor is the whole of the attribution. Media Foundation does not say
/// which adapter a transform belongs to — `MFT_ENUM_ADAPTER_LUID` is documented
/// as a filter for `MFTEnum2`, not as an attribute on an activation object —
/// so which GPU an encoder runs on is inferred from the adapters instead
/// (`crate::detection`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HardwareEncoder {
    vendor: Vendor,
    codec: Codec,
    name: String,
}

impl HardwareEncoder {
    /// Records one.
    #[must_use]
    pub fn new(vendor: Vendor, codec: Codec, name: impl Into<String>) -> Self {
        Self {
            vendor,
            codec,
            name: name.into(),
        }
    }

    /// The vendor that registered it.
    #[must_use]
    pub const fn vendor(&self) -> Vendor {
        self.vendor
    }

    /// The codec it produces.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// The name the driver published for it, such as
    /// "NVIDIA H.264 Encoder MFT". Driver text, not user content.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for HardwareEncoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.name, self.codec)
    }
}

/// What one encoder answered when it was asked about one codec.
///
/// Every field is [`None`] until something measured it, and a field that stays
/// [`None`] falls back to the published limit in [`crate::reference`] — which
/// is the whole discipline of this crate expressed in a type: a value nobody
/// measured does not become a measurement because the session it would have
/// come from happened to open.
///
/// The vendors answer different subsets of this, and no vendor answers all of
/// it. NVENC has a capability query per codec and a numeric answer for each of
/// the four; AMF describes its input size range and its throughput, has a
/// B-frame capability for H.264 only, and says what its highest profile is,
/// which is where 10-bit comes from for HEVC. What each backend does and does
/// not fill in is written up in `docs/encoder-capabilities.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderLimits {
    kind: EncoderKind,
    codec: Codec,
    supported: Option<bool>,
    max_resolution: Option<Resolution>,
    max_luma_samples_per_second: Option<u64>,
    b_frames: Option<bool>,
    hdr: Option<bool>,
}

impl EncoderLimits {
    /// An answer about `codec` on `kind`, with nothing measured yet.
    #[must_use]
    pub const fn new(kind: EncoderKind, codec: Codec) -> Self {
        Self {
            kind,
            codec,
            supported: None,
            max_resolution: None,
            max_luma_samples_per_second: None,
            b_frames: None,
            hdr: None,
        }
    }

    /// Records whether the encoder itself lists this codec.
    ///
    /// A vendor runtime saying "no" here is a measurement and not an absence of
    /// one: unlike the Media Foundation transforms, which a driver may simply
    /// not register, this is the encoder Clipped would record through
    /// enumerating its own codecs.
    #[must_use]
    pub const fn with_supported(mut self, supported: bool) -> Self {
        self.supported = Some(supported);
        self
    }

    /// Records the largest picture the encoder says it takes.
    #[must_use]
    pub const fn with_max_resolution(mut self, resolution: Resolution) -> Self {
        self.max_resolution = Some(resolution);
        self
    }

    /// Records how many luma samples a second the encoder says it can process.
    ///
    /// Both vendors report this as macroblocks — 16x16 samples — per second,
    /// and it is a statement about the *silicon* rather than about the codec's
    /// level, which is what makes it worth more than the number it replaces.
    #[must_use]
    pub const fn with_max_luma_samples_per_second(mut self, rate: u64) -> Self {
        self.max_luma_samples_per_second = Some(rate);
        self
    }

    /// Records whether the encoder offers B-frames.
    #[must_use]
    pub const fn with_b_frames(mut self, b_frames: bool) -> Self {
        self.b_frames = Some(b_frames);
        self
    }

    /// Records whether the encoder offers 10-bit encoding.
    #[must_use]
    pub const fn with_hdr(mut self, hdr: bool) -> Self {
        self.hdr = Some(hdr);
        self
    }

    /// Which encoder family answered.
    #[must_use]
    pub const fn kind(&self) -> EncoderKind {
        self.kind
    }

    /// Which codec it was asked about.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// Whether the encoder lists the codec, where it said.
    #[must_use]
    pub const fn supported(&self) -> Option<bool> {
        self.supported
    }

    /// The largest picture, where the encoder said.
    #[must_use]
    pub const fn max_resolution(&self) -> Option<Resolution> {
        self.max_resolution
    }

    /// The luma sample rate, where the encoder said.
    #[must_use]
    pub const fn max_luma_samples_per_second(&self) -> Option<u64> {
        self.max_luma_samples_per_second
    }

    /// Whether B-frames are offered, where the encoder said.
    #[must_use]
    pub const fn b_frames(&self) -> Option<bool> {
        self.b_frames
    }

    /// Whether 10-bit encoding is offered, where the encoder said.
    #[must_use]
    pub const fn hdr(&self) -> Option<bool> {
        self.hdr
    }

    /// Whether anything at all was measured.
    ///
    /// An answer with nothing in it is what a session that opened and would not
    /// describe itself produces, and it is worth telling apart from one that
    /// filled a field in: the first changes no claim in the report.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.supported.is_none()
            && self.max_resolution.is_none()
            && self.max_luma_samples_per_second.is_none()
            && self.b_frames.is_none()
            && self.hdr.is_none()
    }
}

/// How much a probe may spend on an answer.
///
/// The distinction is one thing: whether an encoder session may be opened. It
/// is the caller's to make, because the caller is the only one that knows
/// whether the user asked for this or whether it is happening underneath a
/// game (`docs/encoder-capabilities.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Probing {
    /// Ask only what costs no encoder session: the adapters, the runtimes, and
    /// the codecs the driver registers a transform for. What every ordinary run
    /// does, and what runs at start-up.
    #[default]
    WithoutSessions,
    /// Also open one encoder session per available hardware encoder and ask it
    /// for its own limits.
    ///
    /// Costs GPU memory and an encode session slot for as long as the query
    /// takes, so it belongs to a user who asked — `capabilities --refresh` —
    /// and never to a recording in progress.
    WithSessions,
}

impl Probing {
    /// Whether this probe may open an encoder session.
    #[must_use]
    pub const fn opens_sessions(self) -> bool {
        matches!(self, Self::WithSessions)
    }
}

impl fmt::Display for Probing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WithoutSessions => "without opening an encoder session",
            Self::WithSessions => "opening one encoder session per hardware encoder",
        })
    }
}

/// Everything the expensive half of a probe found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncoderObservations {
    runtimes: Vec<RuntimeObservation>,
    hardware_encoders: Vec<HardwareEncoder>,
    limits: Vec<EncoderLimits>,
}

impl EncoderObservations {
    /// Nothing found: no runtime loaded and no hardware encoder registered.
    ///
    /// This is what a machine with no GPU at all produces, and it is a
    /// perfectly ordinary result rather than an error.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Adds a runtime load attempt.
    #[must_use]
    pub fn with_runtime(mut self, observation: RuntimeObservation) -> Self {
        self.runtimes.push(observation);
        self
    }

    /// Adds a hardware encoder the operating system reported.
    #[must_use]
    pub fn with_hardware_encoder(mut self, encoder: HardwareEncoder) -> Self {
        self.hardware_encoders.push(encoder);
        self
    }

    /// Adds what an encoder session answered about one codec.
    #[must_use]
    pub fn with_limits(mut self, limits: EncoderLimits) -> Self {
        self.limits.push(limits);
        self
    }

    /// Every runtime that was tried.
    #[must_use]
    pub fn runtimes(&self) -> &[RuntimeObservation] {
        &self.runtimes
    }

    /// Every hardware encoder that was found.
    #[must_use]
    pub fn hardware_encoders(&self) -> &[HardwareEncoder] {
        &self.hardware_encoders
    }

    /// What the encoder sessions answered, which is empty unless one was
    /// opened.
    #[must_use]
    pub fn limits(&self) -> &[EncoderLimits] {
        &self.limits
    }

    /// What one encoder answered about one codec, if it was asked.
    #[must_use]
    pub fn limits_for(&self, kind: EncoderKind, codec: Codec) -> Option<&EncoderLimits> {
        self.limits
            .iter()
            .find(|limits| limits.kind() == kind && limits.codec() == codec)
    }
}

/// The adapters and the encoder observations together: one machine, as it was
/// when it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemFacts {
    adapters: Vec<Adapter>,
    encoders: EncoderObservations,
}

impl SystemFacts {
    /// Assembles the two halves of a probe.
    #[must_use]
    pub fn new(adapters: Vec<Adapter>, encoders: EncoderObservations) -> Self {
        Self { adapters, encoders }
    }

    /// The adapters DXGI enumerated, in its order.
    #[must_use]
    pub fn adapters(&self) -> &[Adapter] {
        &self.adapters
    }

    /// What was found out about encoders.
    #[must_use]
    pub const fn encoders(&self) -> &EncoderObservations {
        &self.encoders
    }
}

/// A machine that can be asked about its adapters and encoders.
///
/// Implemented for real by `crate::windows::WindowsProbe`, and by tests with
/// whatever machine the test is about. Detection takes the answers, not the
/// probe, so most tests need no implementation of this at all — they build a
/// [`SystemFacts`] and call [`crate::detect`].
pub trait SystemProbe: fmt::Debug {
    /// Enumerates display adapters. The cheap half.
    ///
    /// # Errors
    ///
    /// [`ProbeError`] when the platform APIs could not be called at all. An
    /// empty list is a successful answer, not an error.
    fn adapters(&self) -> Result<Vec<Adapter>, ProbeError>;

    /// Finds the encoders. The expensive half.
    ///
    /// # Errors
    ///
    /// [`ProbeError`] when the platform APIs could not be called at all.
    fn encoders(&self) -> Result<EncoderObservations, ProbeError>;

    /// Opens one encoder session per available hardware encoder and asks each
    /// for its own limits. The half that costs a session.
    ///
    /// `adapters` is what [`adapters`](Self::adapters) already returned, so
    /// that a session is opened on the adapter the report will attribute the
    /// encoder to rather than on whichever one the runtime picks for itself.
    ///
    /// An encoder that will not open, or will not describe itself, contributes
    /// nothing and is not an error: the limits then stay inferred, which is
    /// where they were before this was called. An empty answer is therefore
    /// ordinary.
    ///
    /// # Errors
    ///
    /// [`ProbeError`] only when the platform APIs could not be called at all.
    fn encoder_limits(&self, adapters: &[Adapter]) -> Result<Vec<EncoderLimits>, ProbeError>;
}

/// Runs a probe, opening encoder sessions only if `probing` allows it.
///
/// # Errors
///
/// Whatever the probe failed with.
pub fn probe(probe: &dyn SystemProbe, probing: Probing) -> Result<SystemFacts, ProbeError> {
    let adapters = probe.adapters()?;
    let mut encoders = probe.encoders()?;

    if probing.opens_sessions() {
        for limits in probe.encoder_limits(&adapters)? {
            encoders = encoders.with_limits(limits);
        }
    }

    Ok(SystemFacts::new(adapters, encoders))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_probe_names_the_call_and_the_code() {
        let error = ProbeError::Api {
            operation: "CreateDXGIFactory1",
            code: 0x887A_0004_u32 as i32,
            message: "The specified device interface or feature level is not supported".to_owned(),
        };
        let message = error.to_string();
        assert!(
            message.contains("CreateDXGIFactory1") && message.contains("0x887A0004"),
            "the error must name the call and the code: {message}"
        );
    }

    #[test]
    fn a_runtime_that_is_installed_but_broken_reads_differently_from_an_absent_one() {
        let absent = RuntimeObservation::new(
            EncoderKind::Nvenc,
            "nvEncodeAPI64.dll",
            RuntimeOutcome::NotFound,
        );
        let broken = RuntimeObservation::new(
            EncoderKind::Nvenc,
            "nvEncodeAPI64.dll",
            RuntimeOutcome::FailedToLoad { code: 126 },
        );

        assert!(!absent.loaded() && !broken.loaded());
        assert_ne!(absent.to_string(), broken.to_string());
        assert!(broken.to_string().contains("126"));
    }

    #[test]
    fn no_observations_is_an_ordinary_answer_rather_than_an_error() {
        let observations = EncoderObservations::none();
        assert!(observations.runtimes().is_empty());
        assert!(observations.hardware_encoders().is_empty());
        assert!(observations.limits().is_empty());
    }

    #[test]
    fn an_ordinary_probe_opens_no_encoder_session() {
        // The default is the one that runs at start-up and underneath a game,
        // so it is the one whose cost has to be nothing. A build that made
        // `WithSessions` the default would take a session slot from every
        // machine that ever asked what its GPU could do.
        assert!(!Probing::default().opens_sessions());
        assert!(Probing::WithSessions.opens_sessions());
    }

    #[test]
    fn a_session_that_would_not_describe_itself_measures_nothing() {
        let nothing = EncoderLimits::new(EncoderKind::Nvenc, Codec::Av1);
        assert!(nothing.is_empty());
        assert!(!nothing.with_b_frames(false).is_empty());
    }

    #[test]
    fn limits_are_found_by_the_encoder_and_the_codec_together() {
        // Two encoders answer about the same codec on a two-vendor machine, and
        // handing NVENC's maximum resolution to AMD would be a measurement
        // attributed to hardware that never gave it.
        let observations = EncoderObservations::none()
            .with_limits(
                EncoderLimits::new(EncoderKind::Nvenc, Codec::H264)
                    .with_max_resolution(Resolution::new(4096, 4096)),
            )
            .with_limits(
                EncoderLimits::new(EncoderKind::Amf, Codec::H264)
                    .with_max_resolution(Resolution::new(4096, 2160)),
            );

        assert_eq!(
            observations
                .limits_for(EncoderKind::Amf, Codec::H264)
                .and_then(EncoderLimits::max_resolution),
            Some(Resolution::new(4096, 2160))
        );
        assert_eq!(observations.limits_for(EncoderKind::Amf, Codec::Av1), None);
    }
}
