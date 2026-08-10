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
//! # What is deliberately not done
//!
//! Nothing here creates an encoder session. Opening NVENC or AMF and asking it
//! directly would be the most truthful answer available, and it is also the one
//! that allocates GPU memory and takes a session slot on a machine that may be
//! in the middle of a match. The trade-off, and what it costs in accuracy, is
//! written up in `docs/encoder-capabilities.md`; the short version is that
//! codec *availability* is measured through the operating system instead, and
//! the numeric limits are inferred and labelled as inferred.

use core::fmt;
use std::error::Error;

use serde::{Deserialize, Serialize};

use crate::adapter::{Adapter, AdapterId};
use crate::codec::{Codec, EncoderKind, Vendor};

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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HardwareEncoder {
    vendor: Vendor,
    adapter: Option<AdapterId>,
    codec: Codec,
    name: String,
}

impl HardwareEncoder {
    /// Records one.
    ///
    /// `adapter` is the GPU the transform belongs to where the operating system
    /// reported it, which it does not on every Windows version; the vendor is
    /// always available and is what attribution falls back to.
    #[must_use]
    pub fn new(
        vendor: Vendor,
        adapter: Option<AdapterId>,
        codec: Codec,
        name: impl Into<String>,
    ) -> Self {
        Self {
            vendor,
            adapter,
            codec,
            name: name.into(),
        }
    }

    /// The vendor that registered it.
    #[must_use]
    pub const fn vendor(&self) -> Vendor {
        self.vendor
    }

    /// The adapter it runs on, where that was reported.
    #[must_use]
    pub const fn adapter(&self) -> Option<AdapterId> {
        self.adapter
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

/// Everything the expensive half of a probe found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncoderObservations {
    runtimes: Vec<RuntimeObservation>,
    hardware_encoders: Vec<HardwareEncoder>,
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
}

/// Runs both halves of a probe.
///
/// # Errors
///
/// Whatever the probe failed with.
pub fn probe(probe: &dyn SystemProbe) -> Result<SystemFacts, ProbeError> {
    let adapters = probe.adapters()?;
    let encoders = probe.encoders()?;
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
    }
}
