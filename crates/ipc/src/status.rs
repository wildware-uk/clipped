//! What the recorder is doing, and what a finished recording turned out to be.
//!
//! These are the payloads of `get_status` and of the `status` event stream, and
//! they are deliberately a *copy* of the recorder's state rather than a view of
//! it. Everything the desktop application displays crossed a process boundary
//! and can be stale by the time it is drawn ([ADR
//! 0002](../../../docs/adr/0002-separate-recorder-process.md)), so nothing here
//! is derived on the UI side from something that might have moved on.
//!
//! # Why the figures are the ones they are
//!
//! [`RecordingSummary`] carries `clipped-session`'s own counters, kept separate
//! rather than added together. A frame skipped to hold the requested rate is
//! the recorder doing what it was asked; a frame dropped because the disk fell
//! behind is the recorder failing. One "frames dropped" number would mean
//! nothing, and a UI cannot re-separate what the protocol has already mixed
//! (AGENTS.md section 19).

use serde::{Deserialize, Serialize};

/// What the recorder is doing right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecorderStatus {
    /// Nothing is being recorded.
    Idle,
    /// A recording is in progress.
    Recording(ActiveRecording),
}

impl RecorderStatus {
    /// Whether a recording is in progress.
    #[must_use]
    pub const fn is_recording(&self) -> bool {
        matches!(self, Self::Recording(_))
    }
}

/// The recording that is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRecording {
    /// Identifies this recording for the length of the recorder's life.
    ///
    /// `stop_recording` names it, so that a stop meant for a recording that has
    /// already ended cannot stop the next one — which is a real race when the
    /// window closed at the same moment the user pressed the button.
    pub recording_id: String,
    /// The file being written.
    pub output: String,
    /// What is being recorded, as the user asked for it: `process cs2.exe`.
    ///
    /// Not the window title. A title is user content and the most reliable way
    /// to put somebody's document name into a log or a screenshot of a bug
    /// report (AGENTS.md section 13); the selector is what they typed.
    pub target: String,
    /// Milliseconds the recording has been running, as the recorder measures
    /// it.
    ///
    /// Elapsed time rather than a wall-clock start, so that a UI showing a
    /// duration does not have to agree with the recorder about the time of day
    /// or the time zone.
    pub elapsed_ms: u64,
}

/// What a finished recording turned out to be.
///
/// The reply to `stop_recording`. Every field is measured; none is estimated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingSummary {
    /// The file that was written, and which is playable whatever ended the
    /// recording.
    pub output: String,
    /// How long the recording covers.
    pub duration_ms: u64,
    /// Why it ended.
    pub end_reason: EndReason,
    /// Frames that reached the file.
    pub frames_encoded: u64,
    /// Frames skipped to hold the requested frame rate. Expected, not a fault.
    pub frames_skipped_for_rate: u64,
    /// Frames dropped because the writer could not keep up. A fault, and the
    /// one figure here that means something went wrong.
    pub frames_dropped_writer_behind: u64,
    /// The frame rate actually sustained, where the recording was long enough
    /// to measure one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sustained_framerate: Option<f64>,
    /// The encoder that produced the file, as `nvenc`, `amf`, `quicksync` or
    /// `software`.
    pub encoder: String,
    /// The codec in the file, as `h264`, `hevc` or `av1`.
    pub codec: String,
    /// The encoded picture size.
    pub width: u32,
    /// The encoded picture size.
    pub height: u32,
}

/// A bookmark that was taken, as the recorder placed it.
///
/// The reply to `add_bookmark`. It carries where the bookmark *landed* rather
/// than only confirming it was taken, because where it landed is not where the
/// key was pressed: a bookmark is stamped [`Self::lead_seconds`] earlier, to
/// allow for the fact that a person presses the key after the thing they wanted
/// to mark. A UI that showed the press instead would be showing a moment that is
/// not the one in the file (`docs/bookmarks.md`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookmarkSummary {
    /// The recording it is in, as
    /// [`ActiveRecording::recording_id`] reported it.
    pub recording_id: String,
    /// How far into that recording the marked moment is.
    pub at_seconds: f64,
    /// Where the recording was when the key was pressed.
    ///
    /// [`Self::at_seconds`] plus [`Self::lead_seconds`], except at the very
    /// start of a recording, where the offset is clamped at zero and this is
    /// the only record of where the press actually was.
    pub pressed_at_seconds: f64,
    /// How far before the press the bookmark was stamped.
    pub lead_seconds: f64,
    /// What it is called, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The colour it was given, exactly as the caller wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    /// How long the marked moment lasts, if that was said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// The file the bookmarks of this recording are kept in.
    ///
    /// Named so that a user who wants their marks without Clipped can find
    /// them, and so that a support request can say which file to look at
    /// (AGENTS.md section 32).
    pub bookmarks_file: String,
    /// How many bookmarks this recording now has, including this one.
    pub bookmarks_in_recording: u32,
}

/// A screenshot that was taken and written to disk.
///
/// The reply to `take_screenshot`. It carries the file rather than only
/// confirming the picture was taken, because the useful next action — showing
/// it, revealing it in Explorer, attaching it to a message — needs the path,
/// and because a user is entitled to find their own screenshots without Clipped
/// (AGENTS.md section 32).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotSummary {
    /// The file that was written.
    pub path: String,
    /// What it was written as: `png`, `jpeg` or `webp`.
    pub format: String,
    /// The picture's width in pixels.
    pub width: u32,
    /// The picture's height in pixels.
    pub height: u32,
    /// How large the file is.
    pub bytes: u64,
    /// The recording it was taken during, if one was running.
    ///
    /// Absent for a screenshot taken with nothing recording, which is a
    /// supported thing to do rather than an error — see
    /// `clipped_session::screenshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<String>,
    /// How far into that recording the picture was taken.
    ///
    /// Absent for the same reason [`Self::recording_id`] is, and also when a
    /// recording had not yet put a frame in its file. It is the recording's own
    /// media clock, so a timeline can put a marker exactly where the picture
    /// came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_seconds: Option<f64>,
}

/// A recording copied into another container, and what the copy turned out to
/// be.
///
/// The reply to `export_recording`. Clipped records Matroska because it survives
/// an interrupted recording (ADR 0001), and MP4 is what the rest of the world
/// accepts — so an export is a **stream copy**: the coded packets of the
/// recording, in a different box (`clipped_muxer::remux`, `docs/muxing.md`).
///
/// The figures are here so that a window can say what happened rather than only
/// that something did. [`Self::elapsed_ms`] in particular is the whole argument
/// for remuxing instead of re-encoding, and a claim about it should be a
/// measurement (AGENTS.md section 18).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportSummary {
    /// The recording that was copied, unchanged by the copy.
    pub source: String,
    /// The file that was written.
    pub destination: String,
    /// How much media the result holds, from its earliest packet to the end of
    /// its latest one.
    pub duration_ms: u64,
    /// How many coded packets were copied, across every track.
    pub packets: u64,
    /// How many bytes of coded media were copied, before the container's own
    /// overhead.
    pub bytes: u64,
    /// How long the copy took, measured.
    pub elapsed_ms: u64,
    /// Whether the destination holds everything the source did.
    ///
    /// `false` means something was left out — chapter marks, an attached font —
    /// and [`Self::losses`] says what. It is never a picture or a sound track:
    /// a container that cannot carry one of those is a refusal rather than a
    /// quiet loss, because a file missing one of its audio tracks looks exactly
    /// like a file that never had it (`clipped_muxer::remux`, AGENTS.md
    /// section 15).
    pub lossless: bool,
    /// Everything the destination does not contain, phrased for somebody to
    /// read. Empty when [`Self::lossless`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub losses: Vec<String>,
}

/// Why a recording ended.
///
/// Mirrors `clipped_session::EndReason`. It is restated here rather than
/// re-exported because this crate is the protocol and must not depend on the
/// recording engine — the wire format is a contract with the desktop
/// application, and it should not change because an internal enumeration was
/// renamed (AGENTS.md sections 43 and 44).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum EndReason {
    /// `stopped` — something asked it to stop.
    Stopped,
    /// `target_lost` — the recorded window closed.
    TargetLost,
    /// `target_resized` — the recorded window changed size, which one file
    /// cannot follow.
    TargetResized,
    /// A reason this build has never heard of, kept verbatim.
    Other(String),
}

impl EndReason {
    /// The wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stopped => "stopped",
            Self::TargetLost => "target_lost",
            Self::TargetResized => "target_resized",
            Self::Other(reason) => reason,
        }
    }
}

impl From<String> for EndReason {
    fn from(reason: String) -> Self {
        match reason.as_str() {
            "stopped" => Self::Stopped,
            "target_lost" => Self::TargetLost,
            "target_resized" => Self::TargetResized,
            _ => Self::Other(reason),
        }
    }
}

impl From<EndReason> for String {
    fn from(reason: EndReason) -> Self {
        match reason {
            EndReason::Other(reason) => reason,
            known => known.as_str().to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_recorder_serialises_to_a_state_and_nothing_else() {
        let json = serde_json::to_string(&RecorderStatus::Idle).expect("it serialises");
        assert_eq!(json, r#"{"state":"idle"}"#);
    }

    #[test]
    fn an_active_recording_round_trips() {
        let status = RecorderStatus::Recording(ActiveRecording {
            recording_id: "r-1".to_owned(),
            output: r"D:\clips\session.mkv".to_owned(),
            target: "process cs2.exe".to_owned(),
            elapsed_ms: 4_200,
        });

        let json = serde_json::to_string(&status).expect("it serialises");
        let back: RecorderStatus = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, status);
        assert!(back.is_recording());
    }

    #[test]
    fn an_unknown_end_reason_keeps_its_name() {
        let reason: EndReason =
            serde_json::from_str("\"display_disconnected\"").expect("it still parses");
        assert_eq!(reason, EndReason::Other("display_disconnected".to_owned()));
    }

    #[test]
    fn a_field_added_after_this_build_was_compiled_is_ignored_rather_than_fatal() {
        // The additive half of the compatibility policy in docs/ipc.md: a newer
        // recorder may add a field to a status payload, and an older UI has to
        // keep working with the fields it does understand.
        let status: RecorderStatus = serde_json::from_str(
            r#"{"state":"recording","recording_id":"r-1","output":"a.mkv","target":"t",
                "elapsed_ms":1,"gpu_temperature_c":71}"#,
        )
        .expect("an unknown field must not break a known message");
        assert!(status.is_recording());
    }
}
