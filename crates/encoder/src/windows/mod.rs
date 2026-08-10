//! Asking a Windows machine what it can encode.
//!
//! This is the only part of the crate that calls a platform API. Everything
//! above it works from [`SystemFacts`](crate::SystemFacts), which is what lets
//! the reasoning be tested against machines that are not this one.
//!
//! # What is measured here
//!
//! | Question | How | Module |
//! | --- | --- | --- |
//! | Which adapters are present, and on which driver? | DXGI enumeration | [`dxgi`] |
//! | Will each vendor's encoder runtime load? | `LoadLibraryEx` from System32 | [`runtime`] |
//! | Which codecs does the driver register a hardware encoder for? | Media Foundation transform enumeration | [`media_foundation`] |
//!
//! None of the three opens an encoder session, so none of them can disturb a
//! game that is running (`docs/encoder-capabilities.md`).
//!
//! # Threading
//!
//! A probe is a sequence of blocking calls and belongs on whatever thread the
//! caller can afford to block — not a capture thread (AGENTS.md section 20).
//! Media Foundation and COM are started and stopped inside the call that needs
//! them, so a probe leaves the process's apartment state as it found it.

mod dxgi;
mod media_foundation;
mod runtime;

use crate::adapter::Adapter;
use crate::probe::{EncoderObservations, ProbeError, SystemProbe};

/// The real machine.
///
/// ```no_run
/// use clipped_encoder::{detect_cached, CapabilityCache, WindowsProbe};
///
/// let cache = CapabilityCache::at(
///     CapabilityCache::default_path().expect("a per-user directory"),
/// );
/// let detection = detect_cached(&WindowsProbe::new(), &cache)?;
/// println!("{} adapters", detection.report().adapters().len());
/// # Ok::<(), clipped_encoder::ProbeError>(())
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsProbe;

impl WindowsProbe {
    /// A probe of the machine this process is running on.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl SystemProbe for WindowsProbe {
    fn adapters(&self) -> Result<Vec<Adapter>, ProbeError> {
        dxgi::adapters()
    }

    fn encoders(&self) -> Result<EncoderObservations, ProbeError> {
        let mut observations = EncoderObservations::none();
        for (kind, library) in runtime::LIBRARIES {
            observations = observations.with_runtime(runtime::observe(*kind, library));
        }

        // A machine whose Media Foundation cannot be started still has its
        // vendor runtimes, and half an answer is worth more than none: the
        // report degrades to "no codec was measured", which the reference table
        // renders as `Unknown` rather than as "not supported". Not silent,
        // though — the reason is in the log (AGENTS.md section 15).
        match media_foundation::hardware_encoders() {
            Ok(encoders) => {
                for encoder in encoders {
                    observations = observations.with_hardware_encoder(encoder);
                }
            }
            Err(error) => tracing::warn!(
                %error,
                "the hardware encoders Windows lists could not be enumerated, so no codec \
                 support was measured on this run"
            ),
        }

        Ok(observations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detection::detect;
    use crate::probe::probe;

    #[test]
    fn the_real_machine_can_be_probed_and_reported() {
        // An end-to-end check that the three measurements compose: whatever
        // this machine is, probing it produces facts and detection turns them
        // into a report with every encoder family in it. What that report
        // *says* depends on the hardware, so nothing here asserts a vendor.
        let facts = probe(&WindowsProbe::new()).expect("a Windows machine can be probed");
        let report = detect(&facts);

        assert_eq!(
            report.encoders().len(),
            crate::EncoderKind::ALL.len(),
            "every encoder family is reported, available or not"
        );
        assert!(
            report
                .encoder(crate::EncoderKind::Software)
                .is_some_and(|software| software.availability().is_available()),
            "the software encoder is available on every machine"
        );
    }
}
