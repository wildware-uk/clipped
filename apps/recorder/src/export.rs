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

use std::path::Path;

use clipped_ipc::{ErrorCode, ExportRecording, ExportSummary, ProtocolError};
use clipped_logging::RedactedPath;
use clipped_muxer::{remux_to_mp4, MuxError, RemuxError, RemuxSummary};

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
pub fn export(request: &ExportRecording) -> Result<ExportSummary, ProtocolError> {
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

    remux_to_mp4(source, destination)
        .map(|summary| summarise(source, destination, &summary))
        .map_err(refusal)
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
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A directory of this test's own; several of these run at once.
    fn scratch(name: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let directory = std::env::temp_dir().join(format!(
            "clipped-export-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
        directory
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

        let refusal = export(&ExportRecording {
            source: source.to_string_lossy().into_owned(),
            destination: destination.to_string_lossy().into_owned(),
        })
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

        let _ = std::fs::remove_dir_all(&directory);
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
            let refusal = export(&request).expect_err("a request naming no file is refused");
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

        let refusal = export(&ExportRecording {
            source: source.to_string_lossy().into_owned(),
            destination: directory
                .join("not-a-recording.mp4")
                .to_string_lossy()
                .into_owned(),
        })
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

        let _ = std::fs::remove_dir_all(&directory);
    }
}
