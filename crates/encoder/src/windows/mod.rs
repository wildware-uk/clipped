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
//! | What are the encoder's own limits? | An encoder session, asked directly | [`device`], [`nvenc`], [`amf`] |
//!
//! The first three open no encoder session, so none of them can disturb a game
//! that is running. The fourth does, which is why it happens only when the
//! caller asks for [`Probing::WithSessions`] (`docs/encoder-capabilities.md`).
//!
//! # Threading
//!
//! A probe is a sequence of blocking calls and belongs on whatever thread the
//! caller can afford to block — not a capture thread (AGENTS.md section 20).
//! Media Foundation and COM are started and stopped inside the call that needs
//! them, so a probe leaves the process's apartment state as it found it.

mod amf;
mod bridge;
mod device;
mod dxgi;
#[cfg(test)]
pub(in crate::windows) mod hardware_test;
mod media_foundation;
mod nvenc;
mod quicksync;
mod runtime;

pub use amf::AmfEncoder;
pub use bridge::open_across_adapters;
pub use nvenc::NvencEncoder;
pub use quicksync::QuickSyncEncoder;

use crate::adapter::{encoding_adapter, Adapter};
use crate::codec::{EncoderKind, Vendor};
use crate::frame::GraphicsDevice;
use crate::probe::{EncoderLimits, EncoderObservations, ProbeError, SystemProbe};

/// The real machine.
///
/// ```no_run
/// use clipped_encoder::{detect_cached, CapabilityCache, Probing, WindowsProbe};
///
/// let cache = CapabilityCache::at(
///     CapabilityCache::default_path().expect("a per-user directory"),
/// );
/// // `WithoutSessions` opens no encoder, so this costs a game nothing.
/// let detection = detect_cached(&WindowsProbe::new(), &cache, Probing::WithoutSessions)?;
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
        for (kind, libraries) in runtime::LIBRARIES {
            for library in *libraries {
                observations = observations.with_runtime(runtime::observe(*kind, library));
            }
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

    fn encoder_limits(&self, adapters: &[Adapter]) -> Result<Vec<EncoderLimits>, ProbeError> {
        Ok(MEASURABLE
            .iter()
            .filter_map(|(kind, vendor, measure)| {
                let adapter = encoding_adapter(adapters, *vendor)?;
                // An adapter from the right vendor is not an encoder that can
                // be asked. Checking the runtime first is what makes "one
                // session per *available* hardware encoder" true of the code as
                // well as of the sentence: creating a Direct3D device on an
                // adapter is not free — on a laptop it can wake a discrete GPU
                // that was powered down — and this is the one command whose
                // whole argument is about not spending a user's GPU on
                // something they did not ask for.
                runtime_loads(*kind).then_some((*kind, adapter, measure))
            })
            .flat_map(|(kind, adapter, measure)| measure_on(kind, adapter, *measure))
            .collect())
    }
}

/// Whether an encoder family's runtime is installed and will load.
///
/// The same libraries [`WindowsProbe::encoders`] observes, loaded and released
/// the same way from the same function, so a family this reports on cannot
/// disagree with the availability the report prints. The measurement would go
/// through that runtime, so a family whose runtime is absent has nothing to
/// answer and nothing to open a device for.
fn runtime_loads(kind: EncoderKind) -> bool {
    runtime::LIBRARIES
        .iter()
        .filter(|(family, _)| *family == kind)
        .flat_map(|(_, libraries)| libraries.iter())
        .any(|library| runtime::observe(kind, library).loaded())
}

/// The encoder families whose runtime can be asked for its own limits, and the
/// adapter vendor each one needs to be asked on.
///
/// Quick Sync is absent, and its absence is the honest answer rather than an
/// omission: oneVPL does describe an implementation's limits, and there is no
/// Intel GPU on the machine this was written on, so an implementation of it
/// here could never be run — and a measurement path nobody has ever seen return
/// a value is not a measurement, it is a claim
/// ([#14](https://github.com/wildware-uk/clipped/issues/14) says the same of
/// the rest of the Quick Sync path). Its limits therefore stay inferred, and
/// the report says so.
type Measure = fn(&GraphicsDevice) -> Vec<EncoderLimits>;
const MEASURABLE: &[(EncoderKind, Vendor, Measure)] = &[
    (EncoderKind::Nvenc, Vendor::Nvidia, nvenc::measure_limits),
    (EncoderKind::Amf, Vendor::Amd, amf::measure_limits),
];

/// Creates a device on `adapter` and asks that vendor's encoder about itself.
///
/// A device that cannot be created is a measurement that did not happen, and
/// not a failure of the detection: the report keeps the published limits it
/// would have had anyway. That is the rule everywhere in this path — the point
/// of measuring is to improve an answer that already exists (AGENTS.md section
/// 16).
fn measure_on(kind: EncoderKind, adapter: &Adapter, measure: Measure) -> Vec<EncoderLimits> {
    let device = match device::ProbeDevice::on(adapter.id()) {
        Ok(device) => device,
        Err(error) => {
            tracing::warn!(
                encoder = %kind.log_encoder_family(),
                adapter = %adapter.id(),
                %error,
                "no Direct3D device could be created on this adapter, so the encoder's own \
                 limits were not measured"
            );
            return Vec::new();
        }
    };

    let measured = measure(&device.as_graphics_device());
    tracing::debug!(
        encoder = %kind.log_encoder_family(),
        adapter = %adapter.id(),
        codecs = measured.len(),
        "asked an encoder session for its own limits"
    );
    // The device is released here, after the session that was opened against it
    // (AGENTS.md section 58).
    drop(device);
    measured
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::{Mutex, MutexGuard};

    use super::*;
    use crate::codec::Codec;
    use crate::detection::{detect, EncoderReport};
    use crate::probe::{probe, Probing};

    /// The environment variable that turns "this machine has no hardware
    /// encoder" from a pass into a failure, as the backend tests use it.
    const REQUIRE_ENCODER: &str = "CLIPPED_REQUIRE_ENCODER";

    /// Holds the hardware for as long as a probe that opens sessions is
    /// running.
    ///
    /// [`WindowsProbe::encoder_limits`] takes a real NVENC session and a real
    /// AMF component, which is exactly what the backends' own tests compete
    /// for — one of them fills the NVENC session table on purpose
    /// (`nvenc::tests::a_full_session_table_is_reported_and_leaves_nothing_behind`).
    /// So these tests take the same two mutexes those tests take rather than a
    /// third one of their own, which would serialise them against nothing that
    /// matters (AGENTS.md sections 25 and 55).
    ///
    /// Always in this order, and nothing else ever holds both, so the pair
    /// cannot deadlock against a test holding one.
    fn hardware() -> (MutexGuard<'static, ()>, MutexGuard<'static, ()>) {
        (hold(&nvenc::tests::SESSIONS), hold(&amf::tests::SESSIONS))
    }

    /// One of those mutexes, poisoned or not.
    ///
    /// A test that panicked while encoding poisoned the lock; the hardware is
    /// no worse for it, and refusing to run every later test because an earlier
    /// one failed would hide the rest of the suite behind the first failure —
    /// which is the rule both backends' test modules already follow.
    fn hold(sessions: &'static Mutex<()>) -> MutexGuard<'static, ()> {
        sessions.lock().unwrap_or_else(|held| held.into_inner())
    }

    /// Reports a test the machine cannot run, and fails under
    /// [`REQUIRE_ENCODER`].
    ///
    /// Skipping silently would make "the probe tests passed" and "the probe
    /// tests ran" the same sentence on a runner with no GPU.
    fn skipped(reason: &str) -> bool {
        assert!(
            !std::env::var_os(REQUIRE_ENCODER).is_some_and(|value| !value.is_empty()),
            "{REQUIRE_ENCODER} is set, so this must not be skipped: {reason}"
        );
        let _ = writeln!(std::io::stderr(), "SKIPPED (encoder): {reason}");
        true
    }

    #[test]
    fn the_real_machine_can_be_probed_and_reported() {
        // An end-to-end check that the measurements compose: whatever this
        // machine is, probing it produces facts and detection turns them into a
        // report with every encoder family in it. What that report *says*
        // depends on the hardware, so nothing here asserts a vendor.
        let facts = probe(&WindowsProbe::new(), Probing::WithoutSessions)
            .expect("a Windows machine can be probed");
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

    #[test]
    fn an_ordinary_probe_opens_no_encoder_session() {
        // The promise the whole design rests on: a run nobody asked to measure
        // must not take an encode session slot from a game. Asserted on the
        // real machine, because the thing being checked is that the Windows
        // probe does not reach for a session on its own.
        let facts = probe(&WindowsProbe::new(), Probing::WithoutSessions)
            .expect("a Windows machine can be probed");

        assert!(
            facts.encoders().limits().is_empty(),
            "an ordinary probe measured limits, which means it opened a session: {:?}",
            facts.encoders().limits()
        );
    }

    #[test]
    fn only_an_encoder_whose_runtime_loads_is_asked_for_its_limits() {
        // The two answers to "is this encoder there?" have to agree, because
        // they are used for different things and a disagreement is invisible in
        // both: the cheap probe's observation decides what the report says, and
        // this one decides whether a Direct3D device is created on the adapter
        // at all. Nothing here opens a session, so it takes no lock.
        let facts = probe(&WindowsProbe::new(), Probing::WithoutSessions)
            .expect("a Windows machine can be probed");

        for (kind, _, _) in MEASURABLE {
            let observed = facts
                .encoders()
                .runtimes()
                .iter()
                .any(|observation| observation.kind() == *kind && observation.loaded());
            assert_eq!(
                runtime_loads(*kind),
                observed,
                "{kind} would be asked for its limits on a machine the report calls \
                 unavailable, or the reverse"
            );
        }
    }

    #[test]
    fn asking_the_encoders_directly_measures_this_machine_s_own_limits() {
        // What issue #133 is for, checked against whatever hardware is here.
        // Nothing asserts a vendor or a number — the RTX 4090 this was written
        // against is not every machine — but every encoder that answers has to
        // answer something usable, and a measured limit has to reach the
        // report as a measurement rather than as another inferred value.
        let _hardware = hardware();
        let facts = probe(&WindowsProbe::new(), Probing::WithSessions)
            .expect("a Windows machine can be probed");
        let report = detect(&facts);

        if !report.has_hardware_encoder() {
            assert!(skipped("this machine has no hardware encoder to ask"));
            return;
        }

        let measured: Vec<_> = facts
            .encoders()
            .limits()
            .iter()
            .filter(|limits| !limits.is_empty())
            .collect();
        assert!(
            !measured.is_empty(),
            "a machine with a hardware encoder answered nothing at all: {:?}",
            facts.encoders().limits()
        );

        for limits in &measured {
            if let Some(resolution) = limits.max_resolution() {
                assert!(
                    resolution.width >= 1920 && resolution.height >= 1080,
                    "{} reported a maximum of {resolution} for {}, which no hardware encoder \
                     this project supports would say",
                    limits.kind(),
                    limits.codec()
                );
            }
            if let Some(rate) = limits.max_luma_samples_per_second() {
                assert!(
                    rate >= crate::codec::Resolution::HD_1080P.luma_samples(),
                    "{} reported {rate} luma samples a second for {}, which is less than one \
                     frame of 1080p",
                    limits.kind(),
                    limits.codec()
                );
            }
        }

        // And the report has to show it. A measurement that does not survive
        // the trip into a `Claim` changes nothing a user sees, which is the
        // failure that would be easiest to miss.
        let measured_in_report = report
            .encoders()
            .iter()
            .flat_map(EncoderReport::codecs)
            .any(|support| support.max_resolution().is_measured());
        assert!(
            measured_in_report,
            "nothing measured reached the report: {report:?}"
        );
    }

    #[test]
    fn a_measured_limit_replaces_the_published_one_rather_than_sitting_beside_it() {
        // The two probes on the same machine, compared. Every codec a session
        // measured a size for must be reported as measured, and every codec it
        // did not must keep exactly the claim the cheap probe produced — which
        // is what stops a session that opened from promoting the table entries
        // beside the values it actually answered.
        let cheap = detect(
            &probe(&WindowsProbe::new(), Probing::WithoutSessions).expect("the machine answers"),
        );
        let _hardware = hardware();
        let facts =
            probe(&WindowsProbe::new(), Probing::WithSessions).expect("the machine answers");
        let measured = detect(&facts);

        if !measured.has_hardware_encoder() {
            assert!(skipped("this machine has no hardware encoder to ask"));
            return;
        }

        for kind in crate::EncoderKind::ALL {
            let (Some(before), Some(after)) = (cheap.encoder(kind), measured.encoder(kind)) else {
                continue;
            };
            for codec in Codec::EFFICIENCY_ORDER {
                let (Some(before), Some(after)) = (before.codec(codec), after.codec(codec)) else {
                    continue;
                };
                let answered = facts
                    .encoders()
                    .limits_for(kind, codec)
                    .and_then(|limits| limits.max_resolution());

                match answered {
                    Some(size) => assert_eq!(
                        after.max_resolution(),
                        crate::Claim::Measured(size),
                        "{kind}/{codec} was measured and the report does not say so"
                    ),
                    None => assert_eq!(
                        after.max_resolution(),
                        before.max_resolution(),
                        "{kind}/{codec} was not measured, so its claim must not have changed"
                    ),
                }
            }
        }
    }
}
