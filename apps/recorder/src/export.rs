//! The `export_recording` command: a finished recording, copied into MP4.
//!
//! Clipped records Matroska because it survives an interrupted recording
//! ([ADR 0001](../../../docs/adr/0001-use-mkv-for-recording.md)), and ADR 0001
//! names the price: MKV is not accepted by every upload target, chat client or
//! editor. `clipped_muxer::remux_to_mp4` is the answer to that — it copies the
//! coded packets into an MP4 without decoding or re-encoding them
//! (`docs/muxing.md`) — and this module is the whole of what stands between it
//! and the protocol.
//!
//! # Why the recorder does this and not the window
//!
//! Because the window cannot. `tests/integration/tests/workspace_layering.rs`
//! permits `apps/desktop/src-tauri` exactly one member of this workspace,
//! `clipped-ipc`, so the process drawing the interface links no FFmpeg and no
//! muxer ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)). It
//! also has no file-system permission to write anywhere. So it asks, exactly as
//! it asks for a page of the library.
//!
//! # What this module adds, and what it deliberately does not
//!
//! It adds three things and no policy:
//!
//! - it refuses a request that names no file, saying which one was missing;
//! - it refuses a **destination that already exists**, before the muxer is
//!   called, with its own code so that the window can offer the one useful
//!   action — choose another name (AGENTS.md sections 45 and 56);
//! - it turns a [`RemuxError`] into a [`ProtocolError`] whose message is **the
//!   muxer's own sentence**, unchanged.
//!
//! That last one is the point. "`match.mkv` cannot be remuxed to MP4 without
//! losing part of the recording: audio track 1 (wavpack)" is something a person
//! can act on; "export failed" is not (AGENTS.md section 15). Nothing here
//! rewrites, summarises or truncates what the muxer said.
//!
//! # The recording is never touched
//!
//! On every path, including every failing one. `remux_to_mp4` opens the source
//! for reading and removes a partial destination it created itself; this module
//! creates nothing at all (AGENTS.md section 56).
//!
//! # Threads
//!
//! An export runs on the connection thread the command arrived on, like a
//! library read and unlike a recording. It shares nothing with capture — a
//! different file, a different FFmpeg context, no lock either of them holds —
//! and the reply is sent when the MP4's index has been written, so a window
//! that has been told the export finished is looking at a playable file
//! (AGENTS.md section 22).
//!
//! It is not, however, free: a copy of a long recording is bounded by the disk,
//! and the connection it arrived on is answering nothing else meanwhile. That
//! is a property of the protocol's one-request-at-a-time control connection
//! rather than of this module, and the desktop application opens a connection
//! per call.
//!
//! # Saying so while it happens
//!
//! Which is why this says how far it has got, on the `exports` event stream and
//! not in the reply ([issue #446](https://github.com/wildware-uk/clipped/issues/446)).
//! The reply arrives when the MP4's index has been written, which is the moment
//! there is nothing left to report; a copy of a 2.2 GB recording is not instant,
//! and a window with nothing on screen reads as a hang and invites somebody to
//! kill the recorder mid-write.
//!
//! Nothing about it is allowed to slow the copy down (AGENTS.md section 20).
//! `clipped_muxer` calls the callback on the copying thread, so [`Reporter`] does
//! exactly two things per call: compare one integer, and — rarely —
//! `EventPublisher::publish`, which is a `try_send` on a bounded queue that
//! drops rather than waiting for a window that has stopped reading. There is no
//! lock, no allocation on the common path and no clock read.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use clipped_ipc::{
    ErrorCode, Event, EventPublisher, ExportProgress, ExportRecording, ExportSummary, ProtocolError,
};
use clipped_logging::RedactedPath;
use clipped_muxer::{
    remux_to_mp4_with, MuxError, RemuxError, RemuxOptions, RemuxProgress, RemuxSummary,
};

/// Copies the recording the request names into the MP4 the request names.
///
/// # Errors
///
/// - [`ErrorCode::InvalidParameters`] when the request names no source or no
///   destination, saying which.
/// - [`ErrorCode::DestinationExists`] when there is already a file where the
///   MP4 would go. Nothing is written and the file that is there is not read,
///   opened or altered.
/// - [`ErrorCode::ExportFailed`] for everything the muxer refuses, carrying its
///   own words: a recording that cannot be read, and a recording holding a
///   picture or sound track MP4 has no way to store.
/// - [`ErrorCode::Internal`] for a failure that is this process's fault rather
///   than the request's — the linked FFmpeg having no MP4 muxer in it, which
///   means the DLLs beside the executable are not the pinned build
///   (`docs/ffmpeg.md`).
pub fn export(
    request: &ExportRecording,
    events: &EventPublisher,
) -> Result<ExportSummary, ProtocolError> {
    let source = named(&request.source, "source")?;
    let destination = named(&request.destination, "destination")?;

    // Before the muxer, so that the refusal is this one rather than the
    // muxer's `output` variant wrapped in a sentence about writing. `remux_to_mp4`
    // checks it too and must: this process is not the only thing that can
    // create a file, and the gap between this check and its own is real. What
    // this buys is the *code* — `destination_exists` rather than
    // `export_failed` — which is what the window branches on to offer another
    // name (AGENTS.md section 45).
    if matches!(destination.try_exists(), Ok(true)) {
        return Err(already_there(destination));
    }

    tracing::info!(
        source = %RedactedPath::new(source),
        destination = %RedactedPath::new(destination),
        "copying a recording into MP4 because the desktop application asked for one"
    );

    let reporter = Reporter::new(events, &request.source, &request.destination);
    let report = |progress: RemuxProgress| reporter.report(progress);

    remux_to_mp4_with(
        source,
        destination,
        &RemuxOptions::new()
            .reporting_to(&report)
            .every(REPORT_EVERY_MEDIA),
    )
    .map(|summary| summarise(source, destination, &summary))
    .map_err(refusal)
}

/// How much of the recording the muxer copies between calls to [`Reporter`].
///
/// One second of the *recording*, not of wall clock. It is the muxer's floor
/// and not the event rate: [`Reporter`] thins it further, and this only bounds
/// how often the thinning has to run. A whole second of media is thousands of
/// packets, so the branch is nowhere near the copy's cost.
const REPORT_EVERY_MEDIA: std::time::Duration = std::time::Duration::from_secs(1);

/// Turns the muxer's reports into events, and throws most of them away.
///
/// Most of them, because the two rates are wrong for each other. The muxer
/// reports per second of recording copied, which for the two-hour recording
/// this exists for is 7,200 reports — down a bounded 64-deep queue whose other
/// traffic is the window's view of whether anything is recording. A window
/// needs a bar that moves, not one that is repainted seven thousand times.
///
/// So an event is published only when what a person would *see* changes:
///
/// - the whole percentage, when the recording said how long it was. That is at
///   most 101 events for a copy of any length, and every one of them moves the
///   bar by a visible amount.
/// - every ten seconds of recording copied, when it did not. There is no
///   percentage to draw then and [`ExportProgress::bytes`] is what advances, so
///   the rate is chosen rather than derived — at most 720 events for two hours.
///
/// # Threads
///
/// [`Self::report`] runs on the copying thread, so it holds no lock and
/// allocates nothing except the two paths inside an event it has already
/// decided to publish. `last_step` is one relaxed atomic because it is read and
/// written only from that one thread — an `AtomicU64` rather than a `Cell` so
/// that the closure can be `Sync`, which is what `RemuxOptions::reporting_to`
/// requires of it.
struct Reporter<'a> {
    events: &'a EventPublisher,
    source: &'a str,
    destination: &'a str,
    /// The step last published, or [`u64::MAX`] for "nothing yet".
    ///
    /// `MAX` rather than zero because zero is a real step: the first report of
    /// a copy is at 0 %, and it is the one that tells a window the copy has
    /// begun.
    last_step: AtomicU64,
}

/// How much recording is copied between reports when there is no total to
/// measure against.
const UNMEASURED_STEP_MS: u64 = 10_000;

impl<'a> Reporter<'a> {
    fn new(events: &'a EventPublisher, source: &'a str, destination: &'a str) -> Self {
        Self {
            events,
            source,
            destination,
            last_step: AtomicU64::new(u64::MAX),
        }
    }

    /// Publishes this report, if it says anything the last one did not.
    fn report(&self, progress: RemuxProgress) {
        if let Some(export) = self.considered(progress) {
            self.events.publish(&Event::ExportProgress { export });
        }
    }

    /// This report as an event, or [`None`] if it says nothing new.
    ///
    /// Separated from the publishing so that the thinning can be measured over
    /// a copy far longer than any test may write a file for: the decision is
    /// arithmetic on two integers, and driving it with a two-hour recording's
    /// figures is the same decision the recorder makes (AGENTS.md section 25).
    fn considered(&self, progress: RemuxProgress) -> Option<ExportProgress> {
        let written_ms = progress.written_nanos / 1_000_000;
        // Clamped away from zero because it is about to be divided by. A total
        // under a millisecond is a recording that copies before anything could
        // be drawn, and one report of it is right.
        let total_ms = progress.total_nanos.map(|total| (total / 1_000_000).max(1));

        // Whole percent where there is a total, whole ten seconds of recording
        // where there is not. Integer arithmetic on purpose: this comparison is
        // what decides whether anything is sent at all, and a float would make
        // "the same percentage" depend on rounding.
        let step = match total_ms {
            Some(total) => written_ms.saturating_mul(100) / total,
            None => written_ms / UNMEASURED_STEP_MS,
        };
        if self.last_step.swap(step, Ordering::Relaxed) == step {
            return None;
        }

        Some(ExportProgress {
            source: self.source.to_owned(),
            destination: self.destination.to_owned(),
            written_ms,
            total_ms,
            packets: progress.packets,
            bytes: progress.bytes,
        })
    }
}

/// One of the request's two paths, or a refusal naming the one that was missing.
///
/// The protocol carries both as strings with a `serde` default, so that a
/// request missing one is a command this process refuses in its own words
/// rather than a frame that would not deserialise (`clipped_ipc::ExportRecording`).
/// This is where "in its own words" happens.
fn named<'request>(value: &'request str, field: &str) -> Result<&'request Path, ProtocolError> {
    if value.trim().is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!("`export_recording` needs a `{field}`, and was given none"),
        ));
    }
    Ok(Path::new(value))
}

/// The refusal for a destination that is already a file.
///
/// It names the file and says the rule, because "already exists" on its own
/// leaves somebody wondering whether Clipped has just overwritten something.
/// Only the file name, not the directories above it: an error message reaches
/// the log files and the path to somebody's recordings carries their account
/// name (`docs/logging.md`).
fn already_there(destination: &Path) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::DestinationExists,
        format!(
            "there is already a file at {}, and Clipped does not overwrite one; choose another \
             name",
            RedactedPath::new(destination)
        ),
    )
}

/// A refusal from the muxer, in the muxer's own words.
///
/// The code is what the desktop application branches on and the message is what
/// it shows, so the two are chosen separately:
///
/// - [`ErrorCode::DestinationExists`] for the muxer's own check, which catches
///   a file created between this module's check and the open. Reaching the
///   window as the same code as the check above means one sentence in the
///   interface rather than two, for one thing that happened.
/// - [`ErrorCode::Internal`] for a linked FFmpeg with no MP4 muxer, which is
///   the installation being wrong rather than the request.
/// - [`ErrorCode::ExportFailed`] for everything else — an unreadable recording,
///   a track MP4 cannot carry, a disk that filled up.
///
/// The message is `error.to_string()` on every path, which is the muxer's
/// sentence including the tracks it names. Nothing here paraphrases it: the
/// muxer is the only thing that knows why it refused, and a generic failure is
/// what AGENTS.md section 15 exists to prevent.
fn refusal(error: RemuxError) -> ProtocolError {
    let code = match &error {
        RemuxError::Output {
            source: MuxError::OutputExists { .. },
            ..
        } => ErrorCode::DestinationExists,
        RemuxError::Output {
            source: MuxError::ContainerUnsupported,
            ..
        } => ErrorCode::Internal,
        _ => ErrorCode::ExportFailed,
    };

    ProtocolError::new(code, error.to_string())
}

/// A finished remux, as the protocol reports it.
fn summarise(source: &Path, destination: &Path, summary: &RemuxSummary) -> ExportSummary {
    ExportSummary {
        source: source.to_string_lossy().into_owned(),
        destination: destination.to_string_lossy().into_owned(),
        duration_ms: u64::try_from(summary.duration().as_millis()).unwrap_or(u64::MAX),
        packets: summary.packets(),
        bytes: summary.byte_len(),
        elapsed_ms: u64::try_from(summary.elapsed().as_millis()).unwrap_or(u64::MAX),
        lossless: summary.plan().is_lossless(),
        losses: summary.plan().losses(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::test_support::Scratch;

    /// A directory of this test's own, removed again when the test that made it
    /// passes; several of these run at once.
    ///
    /// This used to return a bare path and nothing ever removed it
    /// ([issue #598](https://github.com/wildware-uk/clipped/issues/598)). See
    /// [`Scratch`] for what the returned value does and how to hold it.
    fn scratch(name: &str) -> Scratch {
        Scratch::new(&format!("export-{name}"))
    }

    #[test]
    fn a_destination_that_is_already_a_file_is_refused_and_the_file_is_left_alone() {
        // AGENTS.md section 56 in one test. The bytes are read back afterwards
        // rather than only the length: a check that only counted bytes would
        // pass over a file that had been truncated and rewritten to the same
        // size, and an export writing an MP4 header over somebody's footage is
        // exactly the failure this refuses.
        let directory = scratch("taken");
        let source = directory.join("match.mkv");
        let destination = directory.join("match.mp4");
        std::fs::write(&source, b"not really a recording").expect("the source is written");
        std::fs::write(&destination, b"somebody else's footage").expect("the file is written");

        let refusal = export(
            &ExportRecording {
                source: source.to_string_lossy().into_owned(),
                destination: destination.to_string_lossy().into_owned(),
            },
            &EventPublisher::new(),
        )
        .expect_err("a destination that exists is refused");

        assert_eq!(
            refusal.code,
            ErrorCode::DestinationExists,
            "the window offers `choose another name` on this code and nothing else: {refusal:?}"
        );
        assert!(
            refusal.message.contains("match.mp4")
                && refusal.message.contains("choose another name"),
            "the refusal has to name the file and say the one thing to do about it — which is              what the check before the muxer buys, because the muxer's own is worded for a              recorder writing a recording: {}",
            refusal.message
        );
        assert_eq!(
            std::fs::read(&destination).expect("the file is still there"),
            b"somebody else's footage",
            "a refused export must not have touched what was already there"
        );
    }

    #[test]
    fn a_request_that_names_no_file_says_which_one_it_was_given_none_of() {
        // The two fields are strings with a `serde` default, so a request
        // missing one arrives here rather than failing to parse. Without this
        // it would reach the muxer as an empty path and come back as "the
        // recording  could not be read", which names nothing.
        for (request, expected) in [
            (
                ExportRecording {
                    source: String::new(),
                    destination: r"D:\clips\match.mp4".to_owned(),
                },
                "source",
            ),
            (
                ExportRecording {
                    source: r"D:\clips\match.mkv".to_owned(),
                    destination: "   ".to_owned(),
                },
                "destination",
            ),
        ] {
            let refusal = export(&request, &EventPublisher::new())
                .expect_err("a request naming no file is refused");
            assert_eq!(refusal.code, ErrorCode::InvalidParameters, "{request:?}");
            assert!(
                refusal.message.contains(expected),
                "the refusal has to name the field that was missing: {}",
                refusal.message
            );
        }
    }

    #[test]
    fn a_recording_that_cannot_be_read_is_refused_in_the_muxers_own_words() {
        // The wording is the acceptance criterion, not the code. A mapping that
        // replaced the muxer's sentence with one of its own would leave the
        // window saying "export failed" over a file that had been moved,
        // renamed or is on a drive that is not plugged in — three different
        // problems with three different answers (AGENTS.md sections 15 and 45).
        let directory = scratch("unreadable");
        let source = directory.join("not-a-recording.mkv");
        std::fs::write(&source, b"this is not media").expect("the source is written");

        let refusal = export(
            &ExportRecording {
                source: source.to_string_lossy().into_owned(),
                destination: directory
                    .join("not-a-recording.mp4")
                    .to_string_lossy()
                    .into_owned(),
            },
            &EventPublisher::new(),
        )
        .expect_err("a file that is not media cannot be remuxed");

        assert_eq!(refusal.code, ErrorCode::ExportFailed);
        assert!(
            refusal.message.contains("not-a-recording.mkv")
                && refusal.message.contains("could not be read"),
            "the muxer's own sentence is the one worth showing: {}",
            refusal.message
        );
        assert!(
            !directory.join("not-a-recording.mp4").exists(),
            "a refused export must not leave a stub behind"
        );
    }

    /// Every report a copy of `total` would make, at the muxer's own rate.
    ///
    /// The rate `export` asks the muxer for — one report per second of the
    /// recording copied — replayed over a recording of any length without
    /// writing one. What comes back is what a window would have been sent.
    fn thinned(total: Option<Duration>, of: Duration) -> Vec<ExportProgress> {
        let events = EventPublisher::new();
        let reporter = Reporter::new(&events, r"D:\clips\match.mkv", r"D:\clips\match.mp4");

        let mut published = Vec::new();
        let mut written = Duration::ZERO;
        let mut packets = 0;
        while written <= of {
            packets += 1;
            if let Some(export) = reporter.considered(RemuxProgress {
                written_nanos: u64::try_from(written.as_nanos()).expect("a plausible recording"),
                total_nanos: total
                    .map(|total| u64::try_from(total.as_nanos()).expect("a plausible recording")),
                packets,
                // A rate in the region of a 2.2 GB recording of two hours.
                bytes: packets * 300_000,
            }) {
                published.push(export);
            }
            written = written.saturating_add(REPORT_EVERY_MEDIA);
        }
        published
    }

    #[test]
    fn a_two_hour_export_reports_often_enough_to_watch_and_rarely_enough_not_to_flood() {
        // The recording issue #446 is about: the two-hour sitting recorded
        // while verifying issue #47, which is 2.2 GB. Driven through the
        // thinning rather than written to disk — the decision is the same one
        // either way, and it is the decision that has to hold at this length.
        let two_hours = Duration::from_secs(2 * 60 * 60);
        let published = thinned(Some(two_hours), two_hours);

        // The muxer offered 7,200 reports over that copy. Every one of them
        // published would be a bounded 64-deep queue, shared with the window's
        // view of whether anything is recording, taking two orders of magnitude
        // more traffic than a bar can show.
        assert!(
            published.len() <= 101,
            "a two-hour export published {} events; the queue it shares with `status_changed` is \
             64 deep, so this is how a window loses track of a recording",
            published.len()
        );
        // And it has to be a bar somebody can watch. Ten events over two hours
        // is a bar that moves once every twelve minutes, which is not far from
        // the silence this replaced.
        assert!(
            published.len() >= 90,
            "a two-hour export published {} events, so the bar would sit still for minutes at a \
             time",
            published.len()
        );

        // Every one of them moves it, and moves it forwards.
        for pair in published.windows(2) {
            assert!(
                pair[1].written_ms > pair[0].written_ms,
                "two published events carried the same position, so one of them repainted \
                 nothing: {:?} then {:?}",
                pair[0],
                pair[1]
            );
            let (before, after) = (
                pair[0]
                    .fraction()
                    .expect("a measured export has a fraction"),
                pair[1]
                    .fraction()
                    .expect("a measured export has a fraction"),
            );
            assert!(
                after > before,
                "the fraction did not advance between two published events: {before} then {after}"
            );
        }

        let first = published.first().expect("a two-hour export reports");
        let last = published.last().expect("a two-hour export reports");
        assert_eq!(
            first.fraction().map(|fraction| (fraction * 100.0) as u32),
            Some(0),
            "the first event is what tells a window the copy has begun: {first:?}"
        );
        assert_eq!(
            last.fraction().map(|fraction| (fraction * 100.0) as u32),
            Some(100),
            "the last event of a finished copy did not read as finished: {last:?}"
        );
    }

    #[test]
    fn a_recording_that_never_said_how_long_it_was_still_reports_something_that_advances() {
        // An interrupted recording keeps every packet it wrote and no total,
        // which is the property ADR 0001 chose Matroska for. There is no
        // percentage to draw for one, and a denominator invented here would be
        // a bar that lied — so the events carry no total, and what advances is
        // the bytes.
        let two_hours = Duration::from_secs(2 * 60 * 60);
        let published = thinned(None, two_hours);

        assert!(
            (100..=800).contains(&published.len()),
            "a two-hour export of a recording with no declared length published {} events",
            published.len()
        );
        for progress in &published {
            assert_eq!(
                progress.total_ms, None,
                "a total was invented for a recording that declared none: {progress:?}"
            );
            assert_eq!(
                progress.fraction(),
                None,
                "a fraction was offered with nothing to divide by: {progress:?}"
            );
        }
        for pair in published.windows(2) {
            assert!(
                pair[1].bytes > pair[0].bytes && pair[1].written_ms > pair[0].written_ms,
                "the one figure an unbounded indication can show did not advance: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn a_four_second_export_is_not_drowned_in_events_either() {
        // The case that exists today and needs none of this. It must not
        // suddenly produce a hundred events for a copy that finishes in
        // milliseconds: the muxer offers four reports over four seconds of
        // recording, and thinning cannot manufacture more than it was given.
        let published = thinned(Some(Duration::from_secs(4)), Duration::from_secs(4));

        assert!(
            published.len() <= 5,
            "a four-second export published {} events for a copy that finishes in milliseconds",
            published.len()
        );
        assert!(
            !published.is_empty(),
            "a four-second export published nothing at all, so a window could not tell a fast \
             copy from a stalled one"
        );
    }
}
