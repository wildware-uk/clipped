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
//! The second thing is the distance between "your machine can do this" and
//! "Clipped can do this". A report full of green ticks, from a build that can
//! only use some of them, would be worse than no report (AGENTS.md sections 27
//! and 54), so the footer names the encoders this build has a *proven* backend
//! for, the ones it only detects (which includes a backend nobody has watched
//! encode a real frame — Quick Sync, currently), and what a recording made with
//! any of them does and does not contain. Which encoders those are is asked of
//! [`EncoderKind::is_implemented`] rather than written out here: the previous
//! footer was a hand-written sentence, and it went on saying no encoder was
//! implemented through two of them landing
//! ([#167](https://github.com/wildware-uk/clipped/issues/167)). The sentence
//! that replaced it then went on saying nothing recorded, through the session
//! landing ([#126](https://github.com/wildware-uk/clipped/issues/126)) — which
//! is why the test below now asserts against both of the claims this report has
//! made and outlived.

use std::error::Error;
use std::fmt;

use clipped_encoder::{
    detect_cached, Adapter, CapabilityCache, CapabilityReport, Claim, CodecSupport, Detection,
    DetectionSource, EncoderKind, EncoderReport, ProbeError, Probing, Recommendation, Resolution,
    Signal, SystemProbe,
};

use crate::cli::CapabilitiesArgs;

/// What marks a value that was inferred rather than measured.
///
/// Short on purpose: it appears in every cell of a table that has to stay
/// readable in an 80-column console, and a value nobody can see past is a value
/// nobody reads.
pub const INFERRED_MARKER: &str = "(i)";

/// What stands in a cell the report declines to fill.
///
/// Used for the limits beside a codec whose support is unknown: they are
/// inferred from the encoder family's documentation, and a limit for a codec
/// that may not be there reads as a promise that it is.
const UNSTATED: &str = "—";

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
    let detection = detect_cached(probe.as_ref(), &cache, probing(args.refresh))?;

    print!("{}", render(&detection, &cache));
    Ok(())
}

/// Whether this run may open an encoder session to measure the numeric limits.
///
/// Only `--refresh`, and that is the whole rule. Opening a session is how the
/// maximum resolution, the throughput, B-frames and 10-bit stop being inferred,
/// and it is also how a game mid-match loses an encode session slot to a
/// process the user did not ask to run — so the two are tied together: a user
/// who asks for a fresh look gets the measurements, and a user who asks what
/// their machine can do gets the published limits, marked, in a few
/// milliseconds.
///
/// Nothing else in this build reaches [`Probing::WithSessions`], which is what
/// keeps the promise that probing never happens during a recording: there is
/// one caller, and it is a subcommand that records nothing.
const fn probing(refresh: bool) -> Probing {
    if refresh {
        Probing::WithSessions
    } else {
        Probing::WithoutSessions
    }
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

        fn encoder_limits(
            &self,
            _adapters: &[Adapter],
        ) -> Result<Vec<clipped_encoder::EncoderLimits>, ProbeError> {
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
    out.push_str(&ffmpeg_lines());
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

    // One fact per line rather than a sentence of them: a machine with three
    // codecs registered produces six of these, and a single line long enough to
    // hold them all is a line nobody reads.
    for signal in encoder.signals() {
        // `NoHardwareRequired` is a definition, not a measurement: the software
        // encoder needs no adapter and no runtime, so nothing was asked. In a
        // report whose whole premise is that distinction, calling it "measured"
        // would spend the word on the one line that did not earn it.
        out.push_str(&match signal {
            Signal::NoHardwareRequired => format!("    {signal}\n"),
            measured => format!("    measured: {measured}\n"),
        });
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
///
/// A codec this encoder will not produce prints no limits, whether that is
/// because nothing knows — `unknown` — or because the encoder itself said no.
/// The limits come from published documentation for the encoder family, so
/// printing `unknown  8192x4352 (i)  …  yes (i)` puts a 10-bit ceiling beside a
/// codec that may not exist on this machine at all, and the eye reads the row
/// as a promise. The support column is the one the rest depends on.
fn codec_line(support: &CodecSupport) -> String {
    let unavailable = match support.supported() {
        Claim::Unknown => Some("unknown".to_owned()),
        Claim::Measured(false) => Some("no".to_owned()),
        Claim::Inferred(false) => Some(format!("no {INFERRED_MARKER}")),
        Claim::Measured(true) | Claim::Inferred(true) => None,
    };
    if let Some(supported) = unavailable {
        return format!(
            "    {:<7}{:<12}{:<17}{:<22}{:<11}{}\n",
            support.codec().to_string(),
            supported,
            UNSTATED,
            UNSTATED,
            UNSTATED,
            UNSTATED
        );
    }

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

/// The ranked list "Automatic" resolves through, and what it will not resolve
/// to.
///
/// Every available encoder is printed, including one the machine has that this
/// build cannot open — quietly omitting an encoder a reader can see in the
/// table above would answer a question nobody asked. But it is printed in a
/// group of its own rather than as a numbered entry under "Automatic would
/// choose": that heading asserts every line beneath it is a choice, and an
/// entry reading "so it is not chosen" denies it. The two groups are
/// [`Recommendation::is_openable`] either way round, so a family becoming
/// implemented moves it between them with no edit here
/// (`crates/encoder/src/recommendation.rs`).
fn automatic_lines(report: &CapabilityReport) -> String {
    let (openable, unopenable): (Vec<_>, Vec<_>) = clipped_encoder::recommend(report)
        .into_iter()
        .partition(Recommendation::is_openable);

    let mut out = String::from("Automatic would choose\n\n");
    for (position, recommendation) in openable.iter().enumerate() {
        out.push_str(&format!("  {}. {recommendation}\n", position + 1));
    }
    out.push('\n');

    if !unopenable.is_empty() {
        out.push_str("Detected on this machine, and not available to choose\n\n");
        for recommendation in &unopenable {
            out.push_str(&format!("  - {recommendation}\n"));
        }
        out.push('\n');
    }
    out
}

/// The legend, the standing caveat, and where the answer came from.
/// Which FFmpeg this process is running against.
///
/// Reported here because this command exists to answer "what can this machine
/// do, and what is it doing it with", and the FFmpeg is half that answer: it
/// muxes every recording, remuxes every MP4 export, and decodes every thumbnail
/// and waveform. A bug report that does not name it is missing the variable
/// most likely to explain it
/// ([issue #256](https://github.com/wildware-uk/clipped/issues/256)).
///
/// Read out of the libraries this process **loaded**, not from the pin
/// `scripts/fetch-ffmpeg.ps1` recorded. Those are usually the same and
/// deliberately need not be: `.cargo/config.toml` says an environment variable
/// of the same name still wins, so somebody building against an FFmpeg of their
/// own is a supported situation, and this is where they find out which one they
/// got. It is also the answer the corresponding-source obligation turns on
/// (`docs/licensing.md`, issue #123).
///
/// The identifier, the licence and the three library versions, and not the
/// `configure` arguments: those run to about two thousand characters, which is
/// not a line anybody reads in a terminal report. They are where somebody
/// checking the licence position needs them — in the notices an installed copy
/// carries, which `scripts/collect-notices.ps1` reads out of the same build.
fn ffmpeg_lines() -> String {
    let build = clipped_muxer::linkage::linked_build();
    format!(
        "
FFmpeg

  {build}
"
    )
}

fn footer(detection: &Detection, cache: &CapabilityCache) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "{INFERRED_MARKER} inferred from published limits, not measured on this machine. \
         A value without it\n    was measured here. Codec support is measured when Windows \
         reports a hardware\n    encoder for that codec, or when the encoder itself was \
         asked. A measured\n    framerate ceiling is the encoder's own throughput; an \
         inferred one is what the\n    codec's level allows, which no silicon reaches. A \
         cell reading {UNSTATED} is a limit\n    left unstated, because a codec this encoder \
         will not produce has no limits\n    worth quoting. See docs/encoder-capabilities.md.\n\n"
    ));

    out.push_str(&measurement_lines(detection.report()));
    out.push_str(&implementation_lines());

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

/// How to turn published limits into measured ones, for a machine where that
/// would still change something.
///
/// The limits an encoder can be asked for are only asked for on `--refresh`,
/// because asking costs an encode session slot on a machine that may be in the
/// middle of a match (`docs/encoder-capabilities.md`). A reader looking at a row
/// full of `(i)` deserves to know that a better answer is one command away.
///
/// The condition is "has any encoder not been asked", not "is anything
/// inferred", and the difference matters: some limits stay inferred whatever
/// happens — NVENC's framerate ceiling is deliberately never published from the
/// driver's figure — so the second question would go on advertising a refresh
/// to somebody who had just done one, and cost them a session slot for nothing.
fn measurement_lines(report: &CapabilityReport) -> String {
    if !report.has_unasked_encoder() {
        return String::new();
    }

    format!(
        "Some limits above are {INFERRED_MARKER} because no encoder has been asked. \
         `clipped-recorder\n    capabilities --refresh` asks them, which opens one session per \
         available\n    hardware encoder for a few hundred milliseconds and stores what they \
         say. It\n    is not done automatically: that session slot may belong to a game.\n\n"
    )
}

/// What this build can encode with, what it only detects, and what it still
/// cannot do at all.
///
/// Three separate facts, and a reader needs all three. The table above lists
/// what the *machine* has; whether Clipped has a backend proven to encode with
/// it is a different question, answered by [`EncoderKind::is_implemented`] so
/// that this copy cannot drift from the code again. "Not here" does not mean
/// no code exists — Quick Sync has a real backend that nothing has ever seen
/// encode a frame on real hardware — so the second line says "not proven"
/// rather than "no backend", which would be false for that one
/// ([`EncoderKind::is_implemented`]'s doc explains why it still counts as not
/// implemented). And whichever encoder is chosen, a recording made today has a
/// video track and no audio track, which a report listing working encoders
/// would otherwise let a reader assume the opposite of.
fn implementation_lines() -> String {
    let (implemented, detected_only): (Vec<EncoderKind>, Vec<EncoderKind>) = EncoderKind::ALL
        .iter()
        .partition(|kind| kind.is_implemented());

    // Each list ends its own line rather than sitting mid-sentence, so that
    // the wrapping does not depend on how many encoders are in it.
    let mut out = String::new();
    if !implemented.is_empty() {
        out.push_str(&format!(
            "Encoding in this build: {}.\n",
            names(&implemented, false)
        ));
    }
    if !detected_only.is_empty() {
        out.push_str(&format!(
            "Detection only, not proven to encode: {}.\n    A machine whose best encoder is \
             one of those would encode on the CPU\n    instead.\n",
            names(&detected_only, true)
        ));
    }
    out.push_str(
        "`record` uses the first of these that will open a session on the device the\n    \
         frames are captured on, and falls back to the next when one refuses. A\n    \
         recording has a video track and no audio track: capturing audio into a\n    \
         session is not written yet (issue #180).\n\n",
    );
    out
}

/// Encoder names as a sentence-ready list, with the issue numbers a reader
/// would want to follow and not the ones they would not.
fn names(kinds: &[EncoderKind], with_issue: bool) -> String {
    kinds
        .iter()
        .map(|kind| {
            if with_issue {
                format!("{kind} (#{})", kind.backend_issue())
            } else {
                kind.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use clipped_encoder::{
        detect, Codec, EncoderObservations, HardwareEncoder, RuntimeObservation, RuntimeOutcome,
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
        rendered_report(&detect(facts))
    }

    /// The same, for a report that has already been detected.
    fn rendered_report(report: &CapabilityReport) -> String {
        let mut out = String::from("Adapters\n\n");
        for adapter in report.adapters() {
            out.push_str(&adapter_lines(adapter));
        }
        for encoder in report.encoders() {
            out.push_str(&encoder_lines(encoder, report));
        }
        out.push_str(&automatic_lines(report));
        out
    }

    /// The report names the FFmpeg this process actually loaded, which is the
    /// half of #256 a person reads rather than greps out of a log.
    ///
    /// Driven through the real [`render`] rather than through `ffmpeg_lines`
    /// directly, and that is the whole point of it: a first version of this test
    /// called the helper, and deleting the line that puts the section **into**
    /// the report did not fail it. `rendered_report` above composes a report of
    /// its own from the same private functions, so nothing else here asserts
    /// what `render` actually assembles.
    ///
    /// Asserted against `linked_build()` rather than a version written down
    /// here: a literal would need editing every time the pin moves, and it would
    /// still pass on a build reporting a constant compiled in rather than what
    /// was loaded — which is the failure this capability exists to prevent.
    #[test]
    fn the_report_names_the_ffmpeg_this_process_loaded() {
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
                    fn encoder_limits(
                        &self,
                        _adapters: &[Adapter],
                    ) -> Result<Vec<clipped_encoder::EncoderLimits>, ProbeError>
                    {
                        Ok(Vec::new())
                    }
                }
                NoMachine
            },
            &CapabilityCache::disabled(),
            Probing::WithoutSessions,
        )
        .expect("a machine with no adapters detects successfully");

        let build = clipped_muxer::linkage::linked_build();
        let report = render(&detection, &CapabilityCache::disabled());

        assert!(
            report.contains(build.identifier.as_ref()),
            "the report should name the loaded build `{}`: {report}",
            build.identifier
        );
        assert!(
            report.contains(build.licence.as_ref()),
            "the report should name the licence the build declares, which is what the              corresponding-source obligation turns on: {report}"
        );
        assert!(
            !report.contains("--prefix="),
            "the configure arguments are two thousand characters and belong in the notices,              not in a terminal report: {report}"
        );
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
    fn a_limit_the_encoder_itself_answered_prints_without_the_marker() {
        // Issue #133, at the only place a user meets it. The same row is
        // rendered twice — once from a report nobody measured the limits of,
        // once from a report an encoder session answered — and the difference
        // has to be visible, in the direction that the measured one loses the
        // marker rather than the inferred one gaining a number.
        let runtime = RuntimeObservation::new(
            EncoderKind::Nvenc,
            "nvEncodeAPI64.dll",
            RuntimeOutcome::Loaded,
        );
        let transform =
            HardwareEncoder::new(Vendor::Nvidia, Codec::Hevc, "NVIDIA HEVC Encoder MFT");

        let published = rendered(&SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(runtime.clone())
                .with_hardware_encoder(transform.clone()),
        ));
        let answered = clipped_encoder::EncoderLimits::new(EncoderKind::Nvenc, Codec::Hevc)
            .with_max_resolution(clipped_encoder::Resolution::new(8192, 8192))
            .with_b_frames(true)
            .with_hdr(true);
        let partly_measured = rendered(&SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(runtime.clone())
                .with_hardware_encoder(transform.clone())
                .with_limits(answered),
        ));
        let fully_measured = rendered(&SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(runtime)
                .with_hardware_encoder(transform)
                .with_limits(answered.with_max_luma_samples_per_second(500_000_000)),
        ));

        let hevc = |output: &str| {
            output
                .lines()
                .find(|line| line.trim_start().starts_with("HEVC"))
                .expect("HEVC has a row")
                .to_owned()
        };

        // Two published values and two the table declines to state: NVENC's
        // HEVC B-frames and 10-bit arrived with a generation, so the reference
        // table leaves both `Unknown` rather than guessing.
        assert_eq!(
            hevc(&published).matches(INFERRED_MARKER).count(),
            2,
            "every published limit in this row must be marked: {}",
            hevc(&published)
        );
        assert_eq!(
            hevc(&published).matches("unknown").count(),
            2,
            "the table states no B-frame or 10-bit claim for NVENC HEVC: {}",
            hevc(&published)
        );

        // The NVENC-shaped answer: the size, B-frames and 10-bit come from the
        // session and the framerate ceiling deliberately does not, so exactly
        // one marker survives and the size is not the value carrying it.
        assert!(
            hevc(&partly_measured).contains("8192x8192"),
            "the measured size has to be the one the encoder gave: {}",
            hevc(&partly_measured)
        );
        assert_eq!(
            hevc(&partly_measured).matches(INFERRED_MARKER).count(),
            1,
            "only the framerate ceiling was left unmeasured: {}",
            hevc(&partly_measured)
        );
        assert!(
            !hevc(&partly_measured).contains(&format!("8192x8192 {INFERRED_MARKER}")),
            "the marker that is left belongs to the framerate, not the size: {}",
            hevc(&partly_measured)
        );

        assert!(
            !hevc(&fully_measured).contains(INFERRED_MARKER),
            "every limit in this row was measured and none of it may be marked: {}",
            hevc(&fully_measured)
        );
    }

    #[test]
    fn a_codec_the_encoder_says_it_cannot_produce_is_given_no_limits_either() {
        // A measured "no" is not the same cell as `unknown`, and neither of
        // them may carry a maximum resolution: a row that reads
        // `no  8192x4352 (i)` invites the reading that the limit applies to
        // something, when the codec is not there at all.
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none()
                .with_runtime(RuntimeObservation::new(
                    EncoderKind::Nvenc,
                    "nvEncodeAPI64.dll",
                    RuntimeOutcome::Loaded,
                ))
                .with_limits(
                    clipped_encoder::EncoderLimits::new(EncoderKind::Nvenc, Codec::Av1)
                        .with_supported(false),
                ),
        );
        let output = rendered(&facts);
        let av1 = output
            .lines()
            .find(|line| line.trim_start().starts_with("AV1"))
            .expect("AV1 has a row");

        let columns: Vec<&str> = av1.split_whitespace().collect();
        assert_eq!(columns[1], "no", "the encoder was asked and said no: {av1}");
        assert!(
            !av1.contains(INFERRED_MARKER),
            "a codec the encoder will not produce must not be given limits: {av1}"
        );
        assert!(av1.contains(UNSTATED), "{av1}");
    }

    #[test]
    fn a_codec_whose_support_is_unknown_is_not_given_limits() {
        // The AMD AV1 row on the development machine: the driver registers no
        // AV1 transform, so support is unknown. Printing `8192x4352 (i)` and
        // `yes (i)` for 10-bit beside that word invites the reading that
        // 10-bit AV1 is available here, which is exactly the promise this
        // report exists not to make.
        let facts = SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none().with_runtime(RuntimeObservation::new(
                EncoderKind::Nvenc,
                "nvEncodeAPI64.dll",
                RuntimeOutcome::Loaded,
            )),
        );
        let output = rendered(&facts);
        let av1 = output
            .lines()
            .find(|line| line.trim_start().starts_with("AV1"))
            .expect("AV1 has a row");

        assert!(av1.contains("unknown"), "AV1 support is unknown: {av1}");
        assert!(
            !av1.contains(INFERRED_MARKER),
            "a codec that may not exist must not be given limits: {av1}"
        );
        assert!(
            av1.contains(UNSTATED),
            "the unstated limits need a mark of their own: {av1}"
        );
    }

    #[test]
    fn the_refresh_hint_appears_only_where_asking_would_change_the_report() {
        // A report nobody has measured should say how to do better; a report of
        // a machine that has been asked should not tell its reader to ask again,
        // even though limits stay `(i)` there — NVENC's framerate ceiling always
        // does. Neither should a machine with no hardware encoder to ask.
        let runtime = RuntimeObservation::new(
            EncoderKind::Nvenc,
            "nvEncodeAPI64.dll",
            RuntimeOutcome::Loaded,
        );
        let unasked = detect(&SystemFacts::new(
            vec![nvidia_card()],
            EncoderObservations::none().with_runtime(runtime.clone()),
        ));
        assert!(
            measurement_lines(&unasked).contains("--refresh"),
            "a report of published limits should say how to measure them"
        );

        // Asked, and answered about the size alone: the framerate ceiling is
        // still inferred and always will be, so the report must stop
        // advertising a command that would produce this same page again.
        let asked = detect(&SystemFacts::new(
            vec![nvidia_card()],
            [Codec::Av1, Codec::Hevc, Codec::H264].into_iter().fold(
                EncoderObservations::none().with_runtime(runtime),
                |observations, codec| {
                    observations.with_limits(
                        clipped_encoder::EncoderLimits::new(EncoderKind::Nvenc, codec)
                            .with_supported(true)
                            .with_max_resolution(clipped_encoder::Resolution::new(8192, 8192)),
                    )
                },
            ),
        ));
        assert!(
            rendered_report(&asked).contains(INFERRED_MARKER),
            "this report should still have inferred values in it, or it tests nothing"
        );
        assert!(
            measurement_lines(&asked).is_empty(),
            "the encoder has been asked and the report is still advertising --refresh"
        );

        let no_hardware = detect(&SystemFacts::new(Vec::new(), EncoderObservations::none()));
        assert!(
            measurement_lines(&no_hardware).is_empty(),
            "there is no encoder to open a session on, so there is nothing to suggest"
        );
    }

    #[test]
    fn the_software_encoder_does_not_call_its_definition_a_measurement() {
        // "measured: runs on the CPU, so needs no adapter or driver" spends
        // the word this whole report is built to keep precise on the one line
        // where nothing was asked.
        let output = rendered(&SystemFacts::new(Vec::new(), EncoderObservations::none()));
        let line = output
            .lines()
            .find(|line| line.contains("runs on the CPU"))
            .expect("the software encoder says why it needs nothing");

        assert!(
            !line.contains("measured:"),
            "nothing was measured to arrive at this: {line}"
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
    fn an_encoder_this_build_cannot_open_is_shown_and_not_chosen() {
        // An Intel-only machine, which is the one the two halves of this report
        // could contradict each other on: the table above must still show the
        // Quick Sync hardware it has, and "Automatic would choose" must put the
        // software fallback first, because no backend has ever encoded a frame
        // with Quick Sync (#175). The machine is injected rather than found —
        // there is no Intel GPU here.
        let facts = SystemFacts::new(
            vec![Adapter::new(
                clipped_encoder::AdapterId::from_luid(6, 0),
                "Intel(R) UHD Graphics 770",
                Vendor::Intel,
                0x4680,
                0,
                false,
            )],
            EncoderObservations::none()
                .with_runtime(RuntimeObservation::new(
                    EncoderKind::QuickSync,
                    "libmfxhw64.dll",
                    RuntimeOutcome::Loaded,
                ))
                .with_hardware_encoder(HardwareEncoder::new(
                    Vendor::Intel,
                    Codec::Hevc,
                    "Intel® Hardware HEVC Encoder MFT",
                )),
        );
        let output = rendered(&facts);

        // The encoder table still reports the hardware, measured.
        let hevc = output
            .lines()
            .find(|line| line.trim_start().starts_with("HEVC"))
            .expect("the detected HEVC encoder has a row");
        assert_eq!(
            hevc.split_whitespace().nth(1),
            Some("yes"),
            "the machine's own capability must still be reported: {hevc}"
        );

        // And the ranking chooses the encoder that works. Only the numbered
        // entries under "Automatic would choose" count as choices, and Quick
        // Sync must not be among them.
        let ranked: Vec<&str> = output
            .lines()
            .skip_while(|line| !line.starts_with("Automatic would choose"))
            .take_while(|line| !line.starts_with("Detected on this machine"))
            .filter(|line| line.trim_start().starts_with(|c: char| c.is_ascii_digit()))
            .collect();
        assert!(
            ranked[0].contains("Software (CPU)"),
            "a machine whose only encoder has no proven backend must be told to use the \
             CPU: {output}"
        );
        assert!(
            !ranked.iter().any(|line| line.contains("Intel Quick Sync")),
            "nothing under a heading that says it would be chosen may be an encoder that \
             is not: {output}"
        );

        // The encoder that cannot be opened is still shown, in the group that
        // says what it is.
        let quick_sync = output
            .lines()
            .skip_while(|line| !line.starts_with("Detected on this machine"))
            .find(|line| line.contains("Intel Quick Sync"))
            .expect("a detected encoder is still listed");
        assert!(
            quick_sync.contains("no backend proven")
                && quick_sync.contains(&format!("#{}", EncoderKind::QuickSync.backend_issue())),
            "the entry that cannot be opened must say so and name its issue: {quick_sync}"
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
    fn the_footer_names_the_backends_this_build_has_and_the_ones_it_lacks() {
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
                    fn encoder_limits(
                        &self,
                        _adapters: &[Adapter],
                    ) -> Result<Vec<clipped_encoder::EncoderLimits>, ProbeError>
                    {
                        // No adapters, so no encoder to open a session on.
                        Ok(Vec::new())
                    }
                }
                NoMachine
            },
            &CapabilityCache::disabled(),
            Probing::WithoutSessions,
        )
        .expect("a machine with no adapters detects successfully");

        let footer = footer(&detection, &CapabilityCache::disabled());

        // The two halves have to be told apart by name, whichever way round
        // they are: a family proven to encode must not be listed as one
        // Clipped only detects, and vice versa. Checked against
        // `EncoderKind::is_implemented` rather than against a list of names
        // written here, because the failure this replaces was exactly a
        // hand-maintained list that stopped being true.
        let line = |lead: &str| {
            footer
                .lines()
                .find(|line| line.starts_with(lead))
                .unwrap_or_default()
                .to_owned()
        };
        let encoding = line("Encoding in this build:");
        let detected = line("Detection only, not proven to encode:");

        for kind in EncoderKind::ALL {
            let name = kind.to_string();
            if kind.is_implemented() {
                assert!(
                    encoding.contains(&name),
                    "{kind} is proven to encode and the footer does not say so: {footer}"
                );
                assert!(
                    !detected.contains(&name),
                    "{kind} is proven to encode and must not be listed as detection only: \
                     {footer}"
                );
            } else {
                assert!(
                    detected.contains(&name)
                        && detected.contains(&format!("#{}", kind.backend_issue())),
                    "{kind} is not proven to encode, and the footer should say so and name \
                     its issue: {footer}"
                );
                assert!(
                    !encoding.contains(&name),
                    "{kind} is not proven to encode and must not be listed as one: {footer}"
                );
            }
        }

        // The two claims this footer has made and outlived. The first was true
        // when it was written and stayed in the shipped binary through NVENC
        // and the software fallback landing (#167); the second was true until
        // the session landed (#126). A report that says either again is worse
        // than one that says nothing.
        assert!(
            !footer.contains("No encoder is implemented"),
            "two backends exist, so the footer must not deny it: {footer}"
        );
        assert!(
            !footer.contains("Nothing records yet"),
            "`record` writes a file now, so the footer must not deny it: {footer}"
        );
        // What is genuinely still missing has to be said, or the corrected
        // footer reads as "this build records everything".
        assert!(
            footer.contains("no audio track"),
            "the footer must say a recording has no sound in it yet: {footer}"
        );
        assert!(
            footer.contains(INFERRED_MARKER),
            "the legend must explain the marker: {footer}"
        );
    }
}
