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
//!
//! # Why a sitting is here at all
//!
//! What the product does is record games by itself and group the files into
//! sittings (SPEC.md section 2, `docs/sessions.md`). A protocol that could only
//! say "recording, `process 4242`, 03:12" could not describe that, so a window
//! could not show it ([issue
//! #241](https://github.com/wildware-uk/clipped/issues/241)). [`SessionSummary`]
//! is the sitting as the recorder currently holds it: which game, which files so
//! far, and — once it is over — when and why it ended.
//!
//! It is the live counterpart of [`LibrarySession`](crate::LibrarySession) and
//! deliberately carries the same field names for the same facts, because they
//! are the same facts about the same sitting a few seconds apart. What it leaves
//! out is everything the library adds afterwards: a row identifier, a
//! favourite, a tag, a size on disk. None of those is known while the recording
//! is still being written, and inventing a place for them here would be
//! inventing a second answer to a question the library already answers.

use serde::{Deserialize, Serialize};

/// What the recorder is doing right now.
///
/// Three states rather than two: **watching and idle are different answers.** A
/// recorder watching for games with none running will record the next one that
/// starts; a recorder that is idle will not record anything until it is asked.
/// Reporting both as `idle` made those indistinguishable, and a window cannot
/// say what it does not know (AGENTS.md section 27).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecorderStatus {
    /// Nothing is being recorded, and nothing will be until something asks.
    Idle,
    /// Nothing is being recorded, and the next game to start will be.
    Watching(Watching),
    /// A recording is in progress.
    Recording(ActiveRecording),
}

impl RecorderStatus {
    /// Whether a recording is in progress.
    #[must_use]
    pub const fn is_recording(&self) -> bool {
        matches!(self, Self::Recording(_))
    }

    /// The sitting the recorder is in, if it is in one.
    ///
    /// A sitting outlives the recording it is made of — one that is being
    /// recorded is on [`Self::Recording`], and one waiting out its restart grace
    /// with no game running is on [`Self::Watching`] — so a window that wants to
    /// keep showing the same game across both asks here rather than matching on
    /// the state twice.
    #[must_use]
    pub fn session(&self) -> Option<&SessionSummary> {
        match self {
            Self::Idle => None,
            Self::Watching(watching) => watching.session.as_deref(),
            Self::Recording(recording) => recording.session.as_deref(),
        }
    }
}

/// The recorder is watching for a game to start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watching {
    /// The sitting that is still open, when one is.
    ///
    /// A game that exits keeps its sitting open for a grace period, so that the
    /// same game launching again rejoins it rather than fragmenting one sitting
    /// into two (`docs/sessions.md`). During that period the recorder is
    /// watching *and* in a sitting, and a window that dropped the game's name
    /// for those few seconds would flicker between "Counter-Strike 2" and
    /// "watching for games" and back.
    ///
    /// Absent when the recorder is watching for anything at all rather than for
    /// the return of something in particular.
    ///
    /// Behind a pointer for the reason [`ActiveRecording::session`] is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Box<SessionSummary>>,
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
    /// How much history this recording's replay buffer keeps, when it has one.
    ///
    /// Absent — and absent from the wire — for a recording with no buffer,
    /// which is every recording started without one asked for. A UI reads it
    /// before offering "Save Replay" *for this recording*: the `replay` feature
    /// says the build has the command, and this says there is something for it
    /// to save from. It also bounds what may be asked for, since a save cannot
    /// reach back further than the window (`docs/replay-buffer.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_seconds: Option<u32>,
    /// The sitting this recording belongs to, when it belongs to one.
    ///
    /// This is where the **game** is. [`Self::target`] is a capture selector —
    /// `process 4242` — and a window cannot turn one into "Counter-Strike 2"
    /// without the catalogue, which lives in the recorder. The sitting already
    /// knows, because being able to name the game is what made it a sitting.
    ///
    /// It is also how the second file of one sitting stops looking like an
    /// unrelated recording: [`SessionSummary::recordings`] holds the ones before
    /// this one.
    ///
    /// Absent for a recording that is not part of a sitting. Nothing the
    /// recorder makes today is — every recording opens one, automatic or asked
    /// for (`clipped_session::automatic`) — so this is the field's honest answer
    /// for a recorder that records without one rather than a state the
    /// application produces.
    ///
    /// Behind a pointer, which is not a wire decision — a [`Box`] is invisible
    /// to `serde` — but a size one. A sitting is larger than the recording it
    /// hangs off and is absent as often as not, and [`RecorderStatus`] is
    /// carried by value inside
    /// [`RecorderLinkState`](crate::RecorderLinkState) and cloned for every
    /// subscriber of every status change. Inline it would make the idle case
    /// pay for the recording one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Box<SessionSummary>>,
}

/// One sitting, as the recorder currently holds it.
///
/// The live counterpart of [`LibrarySession`](crate::LibrarySession), carrying
/// the same field names for the same facts — see the module documentation for
/// what it leaves out and why.
///
/// **Whether the sitting is over is [`Self::ended_at`].** There is no separate
/// "finished" shape: a sitting on a [`RecorderStatus`] is one the recorder is
/// still in, and the one on a
/// [`Event::SessionEnded`](crate::Event::SessionEnded) is the same object a
/// moment later with the two fields that only an ended sitting has. Two types
/// would have been two things to keep in step for a difference the presence of
/// a field already states, which is the shape `LibrarySession` settled on for
/// exactly this question.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// The identifier the recorder generated, shared by the sidecar, the files
    /// and — once it has been indexed —
    /// [`LibrarySession::session_id`](crate::LibrarySession::session_id).
    ///
    /// It is what makes two `status` events about one sitting recognisable as
    /// one sitting, and what lets a window that saw a sitting end find it again
    /// in the library.
    pub session_id: String,
    /// The catalogue's identifier for the game.
    ///
    /// Absent for a sitting the catalogue would not attribute — it reported a
    /// tie, or claimed nothing, and the sitting is filed under no game rather
    /// than under a guess (`docs/sessions.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_id: Option<String>,
    /// The game's name as the catalogue knows it.
    ///
    /// Absent for the same reason. What to call an unattributed sitting on
    /// screen is the screen's decision, not the protocol's: the protocol's job
    /// is to say that there is no name rather than to invent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_name: Option<String>,
    /// When the sitting started, RFC 3339 with the offset it was recorded in.
    ///
    /// A wall-clock reading where [`ActiveRecording::elapsed_ms`] is a duration,
    /// and the difference is deliberate. A running recording's elapsed time is
    /// something a window redraws every second and must not have to agree with
    /// the recorder about the time of day to show; a sitting's start is a fixed
    /// point that the library also records, and the two must be the same point.
    pub started_at: String,
    /// When it ended, RFC 3339 with an offset. Absent while it is still open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Why it ended: `game-exited`, `system-resumed`, `recorder-stopping` or
    /// `recording-ended`.
    ///
    /// The vocabulary of
    /// [`LibrarySession::end_reason`](crate::LibrarySession::end_reason), and a
    /// string for the same reason that one is: it is open, a reason this build
    /// has never heard of is kept and shown rather than failing the frame that
    /// carried it, and there is nothing here that branches on it.
    ///
    /// Absent while the sitting is still open. Why a *recording* ended is
    /// [`SessionRecording::outcome`] and [`EndReason`], which is a different
    /// question with a different vocabulary: a sitting can outlive several
    /// recordings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// The files it has produced, in the order they were recorded.
    ///
    /// Includes the one being written, which is what makes "the second file of
    /// this sitting" sayable while it is still being recorded. Empty for a
    /// sitting that has not started its first recording yet, and for one that
    /// never managed to.
    pub recordings: Vec<SessionRecording>,
}

/// One recording within a sitting.
///
/// Deliberately smaller than
/// [`LibraryRecording`](crate::LibraryRecording): this describes a file the
/// recorder has just written, which has no row identifier, no tags and no
/// measured size, because nothing has indexed it yet. The library's own view of
/// the same file arrives later and is the one to ask for any of that.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecording {
    /// Which recording of the sitting this is, counting from one, as
    /// [`LibraryRecording::session_index`](crate::LibraryRecording::session_index)
    /// will also record it.
    pub session_index: u32,
    /// The file that was written, or is being written.
    pub output: String,
    /// What became of it: `recorded`, `no-window` or `failed`.
    ///
    /// Absent while it is still running, which is the answer for the last entry
    /// of a sitting that is still open. `no-window` and `failed` are entries
    /// that produced no playable file and are listed anyway: a sitting whose
    /// recording failed is not a sitting with one fewer recording, and a window
    /// that could not tell those apart would quietly lose the failure
    /// (AGENTS.md section 27).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Why it ended: `stopped`, `target-lost`, `target-resized`,
    /// `disk-space-low` or `output-unavailable`.
    ///
    /// The vocabulary of
    /// [`LibraryRecording::end_reason`](crate::LibraryRecording::end_reason),
    /// spelled the way the sidecar writes it and the index stores it, and a
    /// string for the same reason that one is: it is open, and a reason this
    /// build has never heard of is kept and shown rather than failing the frame
    /// that carried it.
    ///
    /// **Why a live sitting needs it at all.** A recording that somebody stopped
    /// answers this in the reply to that stop
    /// ([`RecordingSummary::end_reason`]). A recording that ended *by itself*
    /// has no reply to carry one, and the only thing the recorder sends is
    /// [`Event::SessionEnded`](crate::Event::SessionEnded) — so without this
    /// field a window watching a recording end can name the file and cannot say
    /// why it stopped, and a recording finished by a window being dragged looks
    /// exactly like one that ran to the end
    /// ([issue #625](https://github.com/wildware-uk/clipped/issues/625),
    /// [ADR 0012](../../../docs/adr/0012-a-session-follows-a-resize-with-a-new-file.md)).
    /// The indexed view of the same file has carried the word all along; this is
    /// the announcement catching up with it, minutes earlier.
    ///
    /// Absent while the recording is still being written, and for one that
    /// produced no file — a `no-window` or `failed` entry never reached an
    /// ending to have a reason for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// How long it runs for. Absent while it is still running, and for one that
    /// produced no file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
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

/// A clip saved out of a recording's replay buffer.
///
/// The reply to `save_replay`. It carries what the clip turned out to be rather
/// than only that one was written, because **what comes out is not exactly what
/// was asked for and a window has to be able to say so**
/// (`docs/replay-buffer.md`):
///
/// - A clip can only begin on a keyframe, so it is up to one keyframe interval
///   *longer* at the front than the request ([`Self::leading_slack_seconds`]).
/// - A buffer that has not filled yet gives less than was asked for — a hotkey
///   pressed ten seconds into a recording asking for the last thirty produces
///   the ten seconds there are ([`Self::complete`],
///   [`Self::shortfall_seconds`]). That is a clip worth having and worth
///   labelling, not a failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplaySummary {
    /// The file that was written.
    pub path: String,
    /// The recording it was saved out of, as
    /// [`ActiveRecording::recording_id`] reported it.
    pub recording_id: String,
    /// How much video was asked for.
    pub requested_seconds: f64,
    /// How long the clip is.
    pub duration_seconds: f64,
    /// Where in the recording the clip begins, on the recording's own timeline.
    pub source_start_seconds: f64,
    /// Where in the recording the clip ends.
    pub source_end_seconds: f64,
    /// Video kept before the requested start, because a clip has to begin on a
    /// keyframe.
    pub leading_slack_seconds: f64,
    /// Whether the buffer held the whole of what was asked for.
    pub complete: bool,
    /// How much of the request the buffer did not hold. Zero when
    /// [`Self::complete`].
    pub shortfall_seconds: f64,
    /// How many bytes of coded video were written, before the container's own
    /// overhead.
    pub bytes: u64,
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

/// How far an export has got, while it is still going.
///
/// The payload of [`Event::ExportProgress`](crate::Event::ExportProgress), and
/// the answer to a copy of a two-hour recording looking like a hang
/// ([issue #446](https://github.com/wildware-uk/clipped/issues/446)). It has to
/// be an event and not a field on the reply, because the reply arrives when the
/// copy has finished, which is the moment there is nothing left to report.
///
/// # What identifies it
///
/// [`Self::destination`], which the client chose and named in the request that
/// started this. There is no request identifier on the event path — a
/// `CommandHandler` is never shown the [`Request`](crate::Request) — and
/// inventing one for this would be a change to what `export_recording` means
/// rather than an addition to the protocol. The destination is enough: a
/// destination that already exists is refused, so two exports cannot be writing
/// the same file at once.
///
/// # Measurements, not a percentage
///
/// A window that wants a percentage divides, and gets to decide what to draw
/// when there is nothing to divide by. [`Self::total_ms`] is absent for a
/// recording whose container declares no duration — an interrupted one keeps
/// every packet it wrote and no total, which is the property ADR 0001 chose
/// Matroska for — and a single `percent` field could only have spelled that as
/// zero. Drawing "0 %" for "no idea" is the sort of control that does nothing
/// AGENTS.md section 27 forbids; [`Self::bytes`] is what still advances, and is
/// what an unbounded indication should be showing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportProgress {
    /// The recording being copied, unchanged by the copy.
    pub source: String,
    /// The file being written. This is what identifies the export.
    pub destination: String,
    /// How much of the recording's own timeline has been copied so far.
    ///
    /// The same measurement [`ExportSummary::duration_ms`] carries, so the last
    /// progress event of a copy and the reply that follows it agree rather than
    /// disagreeing by whatever the reporting interval was.
    pub written_ms: u64,
    /// How long the recording says it is, where it says at all.
    ///
    /// Absent rather than zero when the container declares no duration. See the
    /// type's documentation: the two are different things to draw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u64>,
    /// How many coded packets have been copied, across every carried track.
    pub packets: u64,
    /// How many bytes of coded media have been copied, before the container's
    /// own overhead.
    pub bytes: u64,
}

impl ExportProgress {
    /// How far through, between zero and one, or [`None`] if the recording never
    /// said how long it was.
    ///
    /// Here rather than in the window so that both ends of the protocol agree
    /// about what "no total" means, including the clamp: a source's declared
    /// duration and the end of its last packet need not agree to the
    /// millisecond, and a progress bar that reads 101 % is a bug report.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total_ms?;
        if total == 0 {
            return None;
        }
        Some((self.written_ms as f64 / total as f64).clamp(0.0, 1.0))
    }
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

    /// A sitting of one game with one file recorded and a second being written.
    fn a_sitting() -> SessionSummary {
        SessionSummary {
            session_id: "cs2-20260811-201400".to_owned(),
            game_id: Some("cs2".to_owned()),
            game_name: Some("Counter-Strike 2".to_owned()),
            started_at: "2026-08-11T20:14:00+01:00".to_owned(),
            ended_at: None,
            end_reason: None,
            recordings: vec![
                SessionRecording {
                    session_index: 1,
                    output: r"D:\clips\cs2-20260811-201400-01.mkv".to_owned(),
                    outcome: Some("recorded".to_owned()),
                    end_reason: Some("stopped".to_owned()),
                    duration_ms: Some(1_800_000),
                },
                SessionRecording {
                    session_index: 2,
                    output: r"D:\clips\cs2-20260811-201400-02.mkv".to_owned(),
                    outcome: None,
                    end_reason: None,
                    duration_ms: None,
                },
            ],
        }
    }

    #[test]
    fn an_active_recording_round_trips() {
        let status = RecorderStatus::Recording(ActiveRecording {
            recording_id: "r-1".to_owned(),
            output: r"D:\clips\session.mkv".to_owned(),
            target: "process cs2.exe".to_owned(),
            elapsed_ms: 4_200,
            replay_seconds: None,
            session: None,
        });

        let json = serde_json::to_string(&status).expect("it serialises");
        let back: RecorderStatus = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, status);
        assert!(back.is_recording());
    }

    #[test]
    fn a_recording_names_the_game_and_the_sitting_rather_than_only_the_capture_selector() {
        // The whole of issue #241 in one assertion: "process 4242" is what the
        // recorder was pointed at, and it is not something a window can show
        // somebody. The game and the place in the sitting travel beside it.
        let status = RecorderStatus::Recording(ActiveRecording {
            recording_id: "r-2".to_owned(),
            output: r"D:\clips\cs2-20260811-201400-02.mkv".to_owned(),
            target: "process 4242".to_owned(),
            elapsed_ms: 192_000,
            replay_seconds: None,
            session: Some(Box::new(a_sitting())),
        });

        let json = serde_json::to_string(&status).expect("it serialises");
        let back: RecorderStatus = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, status);

        let session = back.session().expect("a recording in a sitting has one");
        assert_eq!(session.game_name.as_deref(), Some("Counter-Strike 2"));
        assert_eq!(
            session.recordings.len(),
            2,
            "the file being written is the second of this sitting, and the protocol has to be \
             able to say so"
        );
    }

    #[test]
    fn a_watching_recorder_is_not_an_idle_one() {
        // A recorder in `watch` mode with no game running used to report `idle`,
        // which is the same answer as a recorder that will never record. The two
        // must not serialise to the same frame.
        let watching = serde_json::to_string(&RecorderStatus::Watching(Watching::default()))
            .expect("it serialises");
        let idle = serde_json::to_string(&RecorderStatus::Idle).expect("it serialises");

        assert_eq!(watching, r#"{"state":"watching"}"#);
        assert_ne!(watching, idle);
        assert_eq!(
            serde_json::from_str::<RecorderStatus>(&watching).expect("and deserialises"),
            RecorderStatus::Watching(Watching::default())
        );
    }

    #[test]
    fn a_sitting_waiting_out_its_restart_grace_keeps_its_game_while_nothing_is_recording() {
        // The game exited, the sitting is still open for a few seconds in case it
        // comes back, and nothing is being recorded. A window that could only read
        // a game off a *recording* would blank the name for those seconds.
        let status = RecorderStatus::Watching(Watching {
            session: Some(Box::new(a_sitting())),
        });

        let json = serde_json::to_string(&status).expect("it serialises");
        let back: RecorderStatus = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, status);
        assert!(!back.is_recording());
        assert_eq!(
            back.session()
                .and_then(|session| session.game_name.as_deref()),
            Some("Counter-Strike 2")
        );
    }

    #[test]
    fn a_sitting_that_is_still_open_says_nothing_about_having_ended() {
        // The absence is the state, exactly as `LibrarySession` has it: a null
        // `ended_at` would make every open sitting look like it carried the
        // field, and a window reading truthiness would call it ended.
        //
        // Asked of the sitting's **own** keys rather than of the whole frame as
        // a string. `SessionRecording` has an `end_reason` of its own — why one
        // *file* ended, which a finished file of an open sitting has and the
        // sitting itself does not — so a substring search now finds the wrong
        // one and would pass for a sitting that really had ended.
        let json: serde_json::Value = serde_json::to_value(a_sitting()).expect("it serialises");
        let sitting = json.as_object().expect("a sitting is an object");
        assert!(!sitting.contains_key("ended_at"), "{json}");
        assert!(!sitting.contains_key("end_reason"), "{json}");
    }

    #[test]
    fn an_ended_sitting_carries_when_and_why_it_ended_and_the_files_it_produced() {
        let mut ended = a_sitting();
        ended.ended_at = Some("2026-08-11T22:03:00+01:00".to_owned());
        ended.end_reason = Some("game-exited".to_owned());
        ended.recordings[1].outcome = Some("recorded".to_owned());
        ended.recordings[1].end_reason = Some("target-resized".to_owned());
        ended.recordings[1].duration_ms = Some(1_140_000);

        let json = serde_json::to_string(&ended).expect("it serialises");
        let back: SessionSummary = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, ended);
        assert_eq!(
            back.recordings
                .iter()
                .map(|recording| recording.output.as_str())
                .collect::<Vec<_>>(),
            [
                r"D:\clips\cs2-20260811-201400-01.mkv",
                r"D:\clips\cs2-20260811-201400-02.mkv"
            ],
            "a sitting that ended without naming its files leaves a window with nothing to offer"
        );
        assert_eq!(
            back.recordings
                .iter()
                .map(|recording| recording.end_reason.as_deref())
                .collect::<Vec<_>>(),
            [Some("stopped"), Some("target-resized")],
            "and a sitting whose files do not say why they ended leaves a window unable to tell              a recording somebody stopped from one a size change finished (issue #625)"
        );
    }

    #[test]
    fn a_sitting_the_catalogue_would_not_attribute_carries_no_game_rather_than_an_empty_one() {
        // The catalogue refused to guess which game it was, and so does this: an
        // empty string here would be a game called "".
        let session = SessionSummary {
            session_id: "unattributed-20260811-201400".to_owned(),
            started_at: "2026-08-11T20:14:00+01:00".to_owned(),
            ..SessionSummary::default()
        };

        let json = serde_json::to_string(&session).expect("it serialises");
        assert!(!json.contains("game_id"), "{json}");
        assert!(!json.contains("game_name"), "{json}");
    }

    #[test]
    fn a_session_end_reason_this_build_has_never_heard_of_is_kept() {
        // The same policy as an unknown recording end reason: kept verbatim and
        // shown, rather than failing the frame that carried it.
        let session: SessionSummary = serde_json::from_str(
            r#"{"session_id":"s-1","started_at":"2026-08-11T20:14:00+01:00",
                "ended_at":"2026-08-11T22:03:00+01:00","end_reason":"disk_full",
                "recordings":[]}"#,
        )
        .expect("an end reason invented later must still parse");
        assert_eq!(session.end_reason.as_deref(), Some("disk_full"));
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

    #[test]
    fn an_export_fraction_is_none_when_there_is_nothing_to_divide_by() {
        let progress = |written_ms, total_ms| {
            ExportProgress {
                source: r"D:\clips\match.mkv".to_owned(),
                destination: r"D:\clips\match.mp4".to_owned(),
                written_ms,
                total_ms,
                packets: 1,
                bytes: 1,
            }
            .fraction()
        };

        // An interrupted recording declares no duration. `None` rather than
        // zero, because a window draws an unbounded indication for one and a bar
        // at nought for the other, and the second is a control that does nothing
        // (AGENTS.md section 27).
        assert_eq!(progress(1_308_000, None), None);
        // And a total of nought, which would otherwise be a division by zero
        // producing infinity or a NaN — either of which reaches a `<meter>` as
        // an attribute nobody can draw.
        assert_eq!(progress(0, Some(0)), None);

        assert_eq!(progress(0, Some(6_540_000)), Some(0.0));
        assert_eq!(progress(1_635_000, Some(6_540_000)), Some(0.25));
        assert_eq!(progress(6_540_000, Some(6_540_000)), Some(1.0));
        // A recording's declared duration and the end of its last packet need
        // not agree to the millisecond, and a progress bar reading 101 % is a
        // bug report.
        assert_eq!(progress(6_600_000, Some(6_540_000)), Some(1.0));
    }
}
