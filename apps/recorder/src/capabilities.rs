//! The `capabilities` subcommand: what this machine can encode.
//!
//! Detection lives in `clipped-encoder`; this module is the presentation of it.
//! The split matters because the desktop application will show the same
//! information in a window (milestone M5) and must not re-derive any of it —
//! what it should share is the report, not the layout.
//!
//! # What the output has to make obvious
//!
//! One thing above all: which answers were measured on this machine and which
//! were inferred from published limits. Every inferred value carries
//! [`INFERRED_MARKER`], the legend explains it in one sentence, and
//! [`claim_text`] is the only function that renders a claim, so there is one
//! place for that rule to be right or wrong rather than a dozen.
//!
//! The second thing is that no encoder is implemented yet. A report full of
//! green ticks, from a build that cannot record, would be worse than no report
//! (AGENTS.md sections 27 and 54), so the footer says so and names the issues.

use std::error::Error;
use std::fmt;

use clipped_encoder::{
    detect_cached, Adapter, CapabilityCache, CapabilityReport, Claim, Codec, CodecSupport,
    Detection, DetectionSource, EncoderKind, EncoderReport, ProbeError, Recommendation, Resolution,
    SystemProbe,
};

use crate::cli::CapabilitiesArgs;

/// What marks a value that was inferred rather than measured.
///
/// Short on purpose: it appears in every cell of a table that has to stay
/// readable in an 80-column console, and a value nobody can see past is a value
/// nobody reads.
pub const INFERRED_MARKER: &str = "(i)";

/// The resolution framerate ceilings are quoted at.
///
/// One resolution rather than a column per size: 1080p is what most recordings
/// are made at (SPEC.md section 10), and the ceiling at any other size is this
/// one scaled, which the documentation explains.
const FRAMERATE_REFERENCE: Resolution = Resolution::HD_1080P;

/// Why `capabilities` could not report.
#[derive(Debug)]
pub enum CapabilitiesError {
    /// The machine could not be asked.
    Probe(ProbeError),
}

impl fmt::Display for CapabilitiesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Probe(error) => write!(
                formatter,
                "the graphics adapters could not be enumerated, so no encoder could be \
                 detected: {error}"
            ),
        }
    }
}

impl Error for CapabilitiesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Probe(error) => Some(error),
        }
    }
}

impl From<ProbeError> for CapabilitiesError {
    fn from(error: ProbeError) -> Self {
        Self::Probe(error)
    }
}

/// Detects and prints what this machine can encode.
///
/// # Errors
///
/// [`CapabilitiesError::Probe`] when the platform APIs could not be called. A
/// machine with no encoder at all is not an error: it is a report that says so.
pub fn run(args: &CapabilitiesArgs) -> Result<(), CapabilitiesError> {
    let probe = system_probe();
    let cache = cache(args.refresh);
    let detection = detect_cached(probe.as_ref(), &cache)?;

    print!("{}", render(&detection, &cache));
    Ok(())
}

/// The cache to use, which `--refresh` turns into one that never answers.
///
/// Refreshing points the cache at nothing rather than deleting the file: the
/// run then probes, and stores its answer where the next run will find it.
/// `%LOCALAPPDATA%` being unset — which is not a state Windows produces, but is
/// the state of a stripped-down environment — has the same effect, and neither
/// is a failure.
fn cache(refresh: bool) -> CapabilityCache {
    match CapabilityCache::default_path() {
        Some(path) if refresh => CapabilityCache::at(path).ignoring_stored(),
        Some(path) => CapabilityCache::at(path),
        None => CapabilityCache::disabled(),
    }
}

/// The machine this process is running on.
#[cfg(windows)]
fn system_probe() -> Box<dyn SystemProbe> {
    Box::new(clipped_encoder::WindowsProbe::new())
}

/// A stand-in for the platforms Clipped does not target.
///
/// The workspace builds and tests on Linux so that a contributor's other
/// machine is not useless to them (docs/logging.md says the same about the log
/// directory). What it cannot do there is enumerate adapters, and saying so is
/// better than an empty report that reads like a machine with no GPU.
#[cfg(not(windows))]
fn system_probe() -> Box<dyn SystemProbe> {
    #[derive(Debug)]
    struct UnsupportedPlatform;

    impl SystemProbe for UnsupportedPlatform {
        fn adapters(&self) -> Result<Vec<Adapter>, ProbeError> {
            Err(ProbeError::UnsupportedPlatform)
        }

        fn encoders(&self) -> Result<clipped_encoder::EncoderObservations, ProbeError> {
            Err(ProbeError::UnsupportedPlatform)
        }
    }

    Box::new(UnsupportedPlatform)
}

/// Renders a detection as the text the command prints.
///
/// Built as a string rather than printed as it goes so that it can be tested
/// without running a process, and so that a half-written report cannot be left
/// on the screen by a failure part way through.
#[must_use]
pub fn render(detection: &Detection, cache: &CapabilityCache) -> String {
    let report = detection.report();
    let mut out = String::new();

    out.push_str("Adapters\n\n");
    if report.adapters().is_empty() {
        out.push_str("  none: this machine reports no display adapter at all\n");
    }
    for adapter in report.adapters() {
        out.push_str(&adapter_lines(adapter));
    }

    out.push_str("\nEncoders\n\n");
    for encoder in report.encoders() {
        out.push_str(&encoder_lines(encoder, report));
    }

    out.push_str(&automatic_lines(report));
    out.push_str(&footer(detection, cache));
    out
}

/// Two lines about one adapter.
fn adapter_lines(adapter: &Adapter) -> String {
    let memory = if adapter.dedicated_video_memory() == 0 {
        "no dedicated video memory".to_owned()
    } else {
        format!(
            "{:.1} GiB dedicated video memory",
            adapter.dedicated_video_memory() as f64 / (1024.0 * 1024.0 * 1024.0)
        )
    };

    format!(
        "  {adapter}\n    {vendor}, device 0x{device:04X}, {memory}\n",
        vendor = adapter.vendor(),
        device = adapter.device_id(),
    )
}

/// One encoder: its availability, why, and its codecs.
fn encoder_lines(encoder: &EncoderReport, report: &CapabilityReport) -> String {
    let mut out = format!(
        "  {kind} — {availability}\n",
        kind = encoder.kind(),
        availability = encoder.availability()
    );

    if let Some(adapter) = encoder.adapter().and_then(|id| report.adapter(id)) {
        out.push_str(&format!("    on {}\n", adapter.description()));
    }

    // One measured fact per line rather than a sentence of them: a machine with
    // three codecs registered produces six of these, and a single line long
    // enough to hold them all is a line nobody reads.
    for signal in encoder.signals() {
        out.push_str(&format!("    measured: {signal}\n"));
    }

    if !encoder.codecs().is_empty() {
        out.push('\n');
        out.push_str(&format!(
            "    {:<7}{:<12}{:<17}{:<22}{:<11}{}\n",
            "codec",
            "supported",
            "max size",
            format!("max fps at {FRAMERATE_REFERENCE}"),
            "B-frames",
            "10-bit"
        ));
        for support in encoder.codecs() {
            out.push_str(&codec_line(support));
        }
    }

    out.push('\n');
    out
}

/// One codec's row.
fn codec_line(support: &CodecSupport) -> String {
    format!(
        "    {:<7}{:<12}{:<17}{:<22}{:<11}{}\n",
        support.codec().to_string(),
        claim_text(support.supported(), yes_or_no),
        claim_text(support.max_resolution(), ToString::to_string),
        claim_text(
            support.max_framerate_at(FRAMERATE_REFERENCE),
            ToString::to_string
        ),
        claim_text(support.b_frames(), yes_or_no),
        claim_text(support.hdr(), yes_or_no),
    )
}

/// The ranked list "Automatic" resolves through.
fn automatic_lines(report: &CapabilityReport) -> String {
    let mut out = String::from("Automatic would choose\n\n");
    for (position, recommendation) in clipped_encoder::recommend(report).iter().enumerate() {
        out.push_str(&format!("  {}. {recommendation}\n", position + 1));
    }
    out.push('\n');
    out
}

/// The legend, the standing caveat, and where the answer came from.
fn footer(detection: &Detection, cache: &CapabilityCache) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "{INFERRED_MARKER} inferred from published limits, not measured on this machine. \
         A value without it\n    was measured here. Codec support is measured when Windows \
         reports a hardware\n    encoder for that codec; the framerate ceiling is what the \
         codec's level allows,\n    not what the silicon can sustain. See \
         docs/encoder-capabilities.md.\n\n"
    ));

    out.push_str(&format!(
        "No encoder is implemented in this build, so nothing can be recorded yet: \
         NVENC is\n    issue #{nvenc}, AMF #{amf}, Quick Sync #{quicksync} and the software \
         fallback #{software},\n    at https://github.com/wildware-uk/clipped/issues.\n\n",
        nvenc = EncoderKind::Nvenc.backend_issue(),
        amf = EncoderKind::Amf.backend_issue(),
        quicksync = EncoderKind::QuickSync.backend_issue(),
        software = EncoderKind::Software.backend_issue(),
    ));

    let source = match (detection.source(), cache.path()) {
        (DetectionSource::Cached { .. }, Some(path)) => format!("read from {}", path.display()),
        (DetectionSource::Cached { .. }, None) => "read from the cache".to_owned(),
        (DetectionSource::Probed, Some(path)) => {
            format!("probed just now and stored in {}", path.display())
        }
        (DetectionSource::Probed, None) => "probed just now".to_owned(),
    };
    out.push_str(&format!(
        "Detection took {} ms, {source}.\n",
        detection.elapsed().as_millis()
    ));
    out
}

/// Renders a claim, marking it when it was inferred.
///
/// The single place that decides how evidence is shown. A caller cannot print a
/// value without going through this, which is what stops an inferred capability
/// from reaching a user's screen looking like a measured one — the failure this
/// whole feature is shaped around.
fn claim_text<T>(claim: Claim<T>, render: impl FnOnce(&T) -> String) -> String {
    match claim {
        Claim::Measured(value) => render(&value),
        Claim::Inferred(value) => format!("{} {INFERRED_MARKER}", render(&value)),
        Claim::Unknown => "unknown".to_owned(),
    }
}

/// `yes` or `no`, which reads better in a table than `true` and `false`.
fn yes_or_no(value: &bool) -> String {
    if *value { "yes" } else { "no" }.to_owned()
}

/// The codecs the report measured, for a one-line summary elsewhere.
#[must_use]
pub fn measured_codec_names(encoder: &EncoderReport) -> Vec<&'static str> {
    clipped_encoder::measured_codecs(encoder)
        .into_iter()
        .map(Codec::log_value)
        .collect()
}

/// The first choice "Automatic" would make, if there is one.
#[must_use]
pub fn automatic_choice(report: &CapabilityReport) -> Option<Recommendation> {
    clipped_encoder::recommend(report).first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipped_encoder::{
        detect, EncoderObservations, HardwareEncoder, RuntimeObservation, RuntimeOutcome,
        SystemFacts, Vendor,
    };

    fn nvidia_card() -> Adapter {
        Adapter::new(
            clipped_encoder::AdapterId::from_luid(1, 0),
            "NVIDIA GeForce RTX 4090",
            Vendor::Nvidia,
            0x2684,
            24 * 1024 * 1024 * 1024,
            false,
        )
    }

    /// Renders a report the way `run` would, without probing anything.
    fn rendered(facts: &SystemFacts) -> String {
        let report = detect(facts);
        let mut out = String::from("Adapters\n\n");
        for adapter in report.adapters() {
            out.push_str(&adapter_lines(adapter));
        }
        for encoder in report.encoders() {
            out.push_str(&encoder_lines(encoder, &report));
        }
        out.push_str(&automatic_lines(&report));
        out
    }

    #[test]
    fn an_inferred_value_is_marked_and_a_measured_one_is_not() {
        assert_eq!(claim_text(Claim::Measured(true), yes_or_no), "yes");
        assert_eq!(
            claim_text(Claim::Inferred(true), yes_or_no),
            format!("yes {INFERRED_MARKER}")
        );
        assert_eq!(claim_text(Claim::<bool>::Unknown, yes_or_no), "unknown");
    }

    #[test]
    fn a_measured_codec_is_printed_without_the_inferred_marker() {
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(RuntimeObservation::new(
                    EncoderKind::Nvenc,
                    "nvEncodeAPI64.dll",
                    RuntimeOutcome::Loaded,
                ))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Nvidia,
                    None,
                    Codec::Av1,
                    "NVIDIA AV1 Encoder MFT",
                )),
        );
        let output = rendered(&facts);

        let av1 = output
            .lines()
            .find(|line| line.trim_start().starts_with("AV1"))
            .expect("AV1 has a row");

        // The row reads `AV1  yes  8192x8192 (i)  …`, so the marker for the
        // support column — if there were one — would be the word after `yes`.
        let words: Vec<&str> = av1.split_whitespace().collect();
        assert_eq!(words[1], "yes", "AV1 support was measured: {av1}");
        assert_ne!(
            words[2], INFERRED_MARKER,
            "measured support must not be marked as inferred: {av1}"
        );
        // And the limits beside it must still be marked, because they were.
        assert!(
            av1.contains(INFERRED_MARKER),
            "the inferred limits must keep their marker: {av1}"
        );
    }

    #[test]
    fn a_machine_with_no_hardware_says_so_and_still_recommends_something() {
        let output = rendered(&SystemFacts::new(Vec::new(), EncoderObservations::none()));

        assert!(
            output.contains("no adapter from this vendor is present"),
            "an unavailable encoder must say why: {output}"
        );
        assert!(
            output.contains("Software (CPU)"),
            "the software encoder must still be offered: {output}"
        );
        assert!(
            output.contains("CPU encoding, which costs the game frames"),
            "the ranking must explain what falling back to the CPU means: {output}"
        );
    }

    #[test]
    fn every_encoder_family_appears_whether_or_not_it_is_available() {
        let output = rendered(&SystemFacts::new(Vec::new(), EncoderObservations::none()));

        for kind in EncoderKind::ALL {
            assert!(
                output.contains(&kind.to_string()),
                "{kind} is missing from the report: {output}"
            );
        }
    }

    #[test]
    fn the_footer_says_no_encoder_is_implemented_and_names_the_issues() {
        let detection = detect_cached(
            &{
                #[derive(Debug)]
                struct NoMachine;
                impl SystemProbe for NoMachine {
                    fn adapters(&self) -> Result<Vec<Adapter>, ProbeError> {
                        Ok(Vec::new())
                    }
                    fn encoders(&self) -> Result<EncoderObservations, ProbeError> {
                        Ok(EncoderObservations::none())
                    }
                }
                NoMachine
            },
            &CapabilityCache::disabled(),
        )
        .expect("a machine with no adapters detects successfully");

        let footer = footer(&detection, &CapabilityCache::disabled());
        assert!(footer.contains("No encoder is implemented in this build"));
        for issue in ["#15", "#16", "#17", "#18"] {
            assert!(footer.contains(issue), "the footer should name {issue}");
        }
        assert!(
            footer.contains(INFERRED_MARKER),
            "the legend must explain the marker: {footer}"
        );
    }
}
