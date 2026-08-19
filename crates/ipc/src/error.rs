//! What the recorder says when it will not do what it was asked.
//!
//! Every refusal on this protocol is one of these, and every one of them is
//! meant to be rendered by the desktop application rather than only logged. Two
//! consequences follow, and they are the reason this type is not simply a
//! string:
//!
//! - [`ErrorCode`] is stable and machine-readable, so the UI can decide what to
//!   *do* — offer a retry, open the settings screen, say the feature is not in
//!   this build — without matching on English.
//! - [`ProtocolError::message`] is the sentence a person reads, written to
//!   AGENTS.md section 28: short, specific, and about the thing that went wrong
//!   rather than about the protocol.
//!
//! [`ErrorDetail`] carries the machine-readable particulars for the two codes
//! that have any: which versions the recorder speaks, and which milestone a
//! missing subsystem belongs to. A UI that shows "Save replay — not in this
//! build (M3, issue #38)" is telling the truth; one that shows a greyed-out
//! button with no explanation is not (AGENTS.md section 27).
//!
//! # Unknown codes and unknown details
//!
//! [`ErrorCode`] deliberately has an [`ErrorCode::Other`] variant and converts
//! from any string, and [`ErrorDetail`] has an [`ErrorDetail::Other`] variant
//! that keeps a tag this build does not know as the JSON it arrived as. A build
//! that meets a refusal invented after it was compiled therefore keeps the code
//! and the message and degrades to "something the recorder refused" rather than
//! failing to parse the frame. That is the whole of the additive half of the
//! compatibility policy in `docs/ipc.md`, and it takes both halves: a `detail`
//! serde cannot read fails the enclosing [`ProtocolError`] rather than only the
//! field, so an unreadable detail would otherwise cost the message as well.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A refusal, in the form the desktop application renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    /// What kind of refusal this is. Stable across versions.
    pub code: ErrorCode,
    /// One sentence for a person, in the style of AGENTS.md section 28.
    pub message: String,
    /// The particulars, for the codes that have any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ErrorDetail>,
}

impl ProtocolError {
    /// A refusal with a code and a message and nothing else.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    /// The same, with the machine-readable particulars attached.
    #[must_use]
    pub fn with_detail(mut self, detail: ErrorDetail) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The refusal a command whose subsystem this build does not contain gets.
    ///
    /// Never silence and never a success it did not perform: the UI is told
    /// which subsystem is missing, which milestone builds it and which issue
    /// tracks it, so that it can say so (AGENTS.md sections 27 and 54).
    #[must_use]
    pub fn not_implemented(subsystem: &str, milestone: &str, tracking_issue: u32) -> Self {
        Self::new(
            ErrorCode::NotImplemented,
            format!("{subsystem} is not in this build"),
        )
        .with_detail(ErrorDetail::NotImplemented {
            subsystem: subsystem.to_owned(),
            milestone: milestone.to_owned(),
            tracking_issue,
        })
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for ProtocolError {}

/// The stable vocabulary of refusals.
///
/// Serialised as the `snake_case` string in each variant's documentation. A
/// code is never renamed or reused for a different meaning; a new kind of
/// refusal is a new code, which older clients read as
/// [`Other`](ErrorCode::Other) (AGENTS.md section 43).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ErrorCode {
    /// `unsupported_protocol_version` — the version the client asked for is not
    /// one this recorder speaks. Carries
    /// [`ErrorDetail::UnsupportedProtocolVersion`].
    UnsupportedProtocolVersion,
    /// `handshake_required` — a request arrived before the handshake.
    HandshakeRequired,
    /// `malformed_frame` — the bytes on the connection were not a message. The
    /// connection is closed after this one; a peer that cannot frame correctly
    /// cannot be trusted to resynchronise.
    MalformedFrame,
    /// `unknown_command` — no command by that name exists in this build.
    UnknownCommand,
    /// `invalid_parameters` — the command exists and its parameters do not
    /// describe something that could be done.
    InvalidParameters,
    /// `not_implemented` — the command exists in the protocol and the subsystem
    /// behind it is not in this build. Carries
    /// [`ErrorDetail::NotImplemented`].
    NotImplemented,
    /// `already_recording` — a recording is already running, and this recorder
    /// runs one at a time.
    AlreadyRecording,
    /// `not_recording` — there is nothing to stop.
    NotRecording,
    /// `target_not_found` — no window matched what was asked for.
    TargetNotFound,
    /// `target_not_capturable` — a window matched and cannot be recorded as it
    /// is.
    ///
    /// Deliberately not [`TargetNotFound`](Self::TargetNotFound): the window was
    /// found, it is named in the message, and the thing to do about it is to
    /// change the window rather than to choose a different one. A minimised
    /// window is the case this was added for — Windows draws it for nobody, so a
    /// recording of it would be an empty file
    /// ([issue #383](https://github.com/wildware-uk/clipped/issues/383)).
    TargetNotCapturable,
    /// `recording_failed` — capture, encoding or muxing refused. Whatever had
    /// been written before the failure is still a finished file.
    RecordingFailed,
    /// `too_many_connections` — the recorder is already serving as many
    /// connections as it will. The endpoint is reachable by anything running as
    /// the user, so the cap is what stops a loop opening connections from
    /// costing the recorder a thread each.
    TooManyConnections,
    /// `shutting_down` — the recorder has accepted a `shutdown` and will not
    /// start anything new.
    ///
    /// It exists so that the permission a `shutdown` carries stays true: the
    /// decision about whether exiting may end a recording is made by reading
    /// the status, and without this a `start_recording` arriving between that
    /// read and the reply would begin a recording the decision never covered
    /// (`crate::server::accept_shutdown`, AGENTS.md section 17).
    ShuttingDown,
    /// `destination_exists` — there is already a file where the caller asked
    /// for one to be written, and Clipped does not overwrite.
    ///
    /// A code of its own rather than [`InvalidParameters`](Self::InvalidParameters)
    /// because the useful action is specific and the UI can offer it: pick
    /// another name. It is the protocol's expression of AGENTS.md section 56 —
    /// the file that is already there is somebody's footage, and the one
    /// certain way to destroy it is to write over it.
    DestinationExists,
    /// `export_failed` — a recording could not be written into the container
    /// that was asked for, and the message says which part of it stopped it.
    ///
    /// Distinct from [`RecordingFailed`](Self::RecordingFailed), which is about
    /// a recording in progress. This is about a finished file being copied into
    /// a different container, and the recording it was made from is untouched
    /// on every path that produces it (`clipped_muxer::remux`).
    ExportFailed,
    /// `playback_failed` — a recording could not be opened for playback, and
    /// the message says why.
    ///
    /// Distinct from [`ExportFailed`](Self::ExportFailed), which is about a
    /// copy somebody asked to keep. This is about the file the window was
    /// about to draw a player over, and the two most common causes are ones a
    /// person can act on: the recording's file has gone, or the sound track
    /// that was asked for is not in it. A window branches on this to say so
    /// instead of drawing a player that does nothing (AGENTS.md section 27,
    /// [issue #304](https://github.com/wildware-uk/clipped/issues/304)).
    PlaybackFailed,
    /// `library_unavailable` — the recording library could not be read, and the
    /// message says why.
    ///
    /// It exists so that "there is nothing in your library" and "your library
    /// could not be opened" cannot be shown as the same thing. A read that
    /// failed answered with an empty page would have a window drawing an empty
    /// library over a database that is locked, corrupt, from a newer build or on
    /// a drive that is not plugged in — the fabricated state AGENTS.md section
    /// 27 forbids.
    LibraryUnavailable,
    /// `edit_unreadable` — a clip's edit document is there and this build
    /// cannot read it, and the message says what to do about it.
    ///
    /// Separate from [`Self::InvalidParameters`] because nothing is wrong with
    /// the request: the clip exists and was asked for correctly. Separate from
    /// [`Self::LibraryUnavailable`] because the library opened fine — it is one
    /// clip that will not, and every other clip in the library still will.
    ///
    /// The case it is really for is the one that has to be survivable: a
    /// document saved by a **newer** Clipped, opened on a machine the user has
    /// not updated yet. That is not a fault, it is a version skew, and the only
    /// correct response is to say so and change nothing — an older build that
    /// rewrote a document it did not understand is precisely how somebody loses
    /// the edit they made on their other machine (AGENTS.md sections 43 and
    /// 56). A window that could not tell this from a corrupt library would
    /// offer the wrong remedy for it.
    EditUnreadable,
    /// `internal` — the recorder is at fault and cannot say more usefully.
    Internal,
    /// A code this build has never heard of, kept verbatim.
    ///
    /// Not an error in itself: it is how a client compiled against an older
    /// protocol keeps a newer recorder's message readable instead of failing to
    /// parse it.
    Other(String),
}

impl ErrorCode {
    /// The wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::UnsupportedProtocolVersion => "unsupported_protocol_version",
            Self::HandshakeRequired => "handshake_required",
            Self::MalformedFrame => "malformed_frame",
            Self::UnknownCommand => "unknown_command",
            Self::InvalidParameters => "invalid_parameters",
            Self::NotImplemented => "not_implemented",
            Self::AlreadyRecording => "already_recording",
            Self::NotRecording => "not_recording",
            Self::TargetNotFound => "target_not_found",
            Self::TargetNotCapturable => "target_not_capturable",
            Self::RecordingFailed => "recording_failed",
            Self::TooManyConnections => "too_many_connections",
            Self::ShuttingDown => "shutting_down",
            Self::DestinationExists => "destination_exists",
            Self::ExportFailed => "export_failed",
            Self::PlaybackFailed => "playback_failed",
            Self::LibraryUnavailable => "library_unavailable",
            Self::EditUnreadable => "edit_unreadable",
            Self::Internal => "internal",
            Self::Other(code) => code,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for ErrorCode {
    fn from(code: String) -> Self {
        match code.as_str() {
            "unsupported_protocol_version" => Self::UnsupportedProtocolVersion,
            "handshake_required" => Self::HandshakeRequired,
            "malformed_frame" => Self::MalformedFrame,
            "unknown_command" => Self::UnknownCommand,
            "invalid_parameters" => Self::InvalidParameters,
            "not_implemented" => Self::NotImplemented,
            "already_recording" => Self::AlreadyRecording,
            "not_recording" => Self::NotRecording,
            "target_not_found" => Self::TargetNotFound,
            "target_not_capturable" => Self::TargetNotCapturable,
            "recording_failed" => Self::RecordingFailed,
            "too_many_connections" => Self::TooManyConnections,
            "shutting_down" => Self::ShuttingDown,
            "destination_exists" => Self::DestinationExists,
            "export_failed" => Self::ExportFailed,
            "playback_failed" => Self::PlaybackFailed,
            "library_unavailable" => Self::LibraryUnavailable,
            "edit_unreadable" => Self::EditUnreadable,
            "internal" => Self::Internal,
            _ => Self::Other(code),
        }
    }
}

impl From<ErrorCode> for String {
    fn from(code: ErrorCode) -> Self {
        match code {
            ErrorCode::Other(code) => code,
            known => known.as_str().to_owned(),
        }
    }
}

/// The machine-readable particulars of the refusals that have any.
///
/// Tagged by `detail`. A tag this build has never heard of is kept as
/// [`Other`](ErrorDetail::Other) rather than failing to parse, because the
/// alternative is worse than losing the detail: serde fails the *whole*
/// [`ProtocolError`], so a refusal carrying a detail added after this build was
/// compiled would arrive as a broken frame and take its code and its message
/// with it. "A refusal you have not seen before" would become "the connection
/// is broken", which is exactly what the additive half of the compatibility
/// policy in `docs/ipc.md` promises will not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "detail", rename_all = "snake_case")]
pub enum ErrorDetail {
    /// Which versions this recorder actually speaks.
    ///
    /// The point of the handshake: a refusal that does not say what the other
    /// side speaks leaves the user with "it did not connect" and no next step
    /// (AGENTS.md section 45).
    UnsupportedProtocolVersion {
        /// What the client asked for.
        requested: u32,
        /// Every version the recorder would have accepted.
        supported: Vec<u32>,
        /// The recorder's own build version, so the user can be told which of
        /// the two to update.
        recorder_version: String,
    },
    /// Which subsystem is missing, and where it is being built.
    NotImplemented {
        /// The subsystem in the user's words: "a recording with a replay
        /// buffer".
        subsystem: String,
        /// The milestone that builds it, such as `M3`.
        milestone: String,
        /// The issue that tracks it.
        tracking_issue: u32,
    },
    /// A detail this build has never heard of, kept exactly as it arrived.
    ///
    /// Not an error in itself, and not something to construct: it is how a
    /// client compiled against an older protocol keeps a newer recorder's
    /// refusal readable. The raw object is kept rather than discarded so that a
    /// diagnostic can show what was not understood — the desktop application
    /// has nothing to render from it, and renders the message instead.
    #[serde(untagged)]
    Other(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_code_survives_a_round_trip_through_its_wire_string() {
        let codes = [
            ErrorCode::UnsupportedProtocolVersion,
            ErrorCode::HandshakeRequired,
            ErrorCode::MalformedFrame,
            ErrorCode::UnknownCommand,
            ErrorCode::InvalidParameters,
            ErrorCode::NotImplemented,
            ErrorCode::AlreadyRecording,
            ErrorCode::NotRecording,
            ErrorCode::TargetNotFound,
            ErrorCode::TargetNotCapturable,
            ErrorCode::RecordingFailed,
            ErrorCode::TooManyConnections,
            ErrorCode::ShuttingDown,
            ErrorCode::DestinationExists,
            ErrorCode::ExportFailed,
            ErrorCode::PlaybackFailed,
            ErrorCode::LibraryUnavailable,
            ErrorCode::Internal,
        ];

        for code in codes {
            let wire = serde_json::to_string(&code).expect("a code serialises");
            let back: ErrorCode = serde_json::from_str(&wire).expect("and deserialises");
            assert_eq!(back, code, "{wire} did not round-trip");
        }
    }

    #[test]
    fn a_code_this_build_has_never_heard_of_keeps_its_name_rather_than_failing_to_parse() {
        // The additive half of the compatibility policy: a client built against
        // protocol 1 must still be able to show the message a later recorder
        // sent it, whatever the code was called.
        let code: ErrorCode =
            serde_json::from_str("\"gpu_on_fire\"").expect("an unknown code still parses");
        assert_eq!(code, ErrorCode::Other("gpu_on_fire".to_owned()));
        assert_eq!(
            serde_json::to_string(&code).expect("and serialises back unchanged"),
            "\"gpu_on_fire\""
        );
    }

    #[test]
    fn a_missing_subsystem_names_the_milestone_and_the_issue_that_builds_it() {
        let error = ProtocolError::not_implemented("a recording with a replay buffer", "M3", 38);

        assert_eq!(error.code, ErrorCode::NotImplemented);
        assert_eq!(
            error.detail,
            Some(ErrorDetail::NotImplemented {
                subsystem: "a recording with a replay buffer".to_owned(),
                milestone: "M3".to_owned(),
                tracking_issue: 38,
            }),
            "the UI has to be able to say which build this arrives in"
        );
    }

    #[test]
    fn a_refusal_carrying_a_detail_this_build_has_never_heard_of_is_still_a_usable_refusal() {
        // The whole point of the additive half of the policy. Before this
        // parsed, `serde_json::from_str::<ProtocolError>` on exactly this frame
        // failed with "unknown variant `disk_full`", which cost the client the
        // code, the message and the frame — a refusal the UI had never seen
        // arriving as a broken connection.
        let json = r#"{"code":"recording_failed","message":"the disk the recording was being written to is full","detail":{"detail":"disk_full","free_bytes":0}}"#;

        let error: ProtocolError =
            serde_json::from_str(json).expect("an unknown detail must not cost the whole refusal");

        assert_eq!(error.code, ErrorCode::RecordingFailed);
        assert_eq!(
            error.message, "the disk the recording was being written to is full",
            "the message is what the user is shown, so it is the part that must survive"
        );
        match &error.detail {
            Some(ErrorDetail::Other(raw)) => assert_eq!(
                raw["detail"], "disk_full",
                "the detail is kept for diagnostics rather than discarded: {raw}"
            ),
            other => panic!("an unknown detail should be kept verbatim, got {other:?}"),
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &serde_json::to_string(&error).expect("it serialises back")
            )
            .expect("into JSON"),
            serde_json::from_str::<serde_json::Value>(json).expect("the same JSON"),
            "a detail this build cannot read must still be passed on unchanged"
        );
    }

    #[test]
    fn a_refusal_whose_code_and_detail_are_both_new_still_says_what_went_wrong() {
        // Both halves at once, which is what a client two versions behind
        // actually meets.
        let error: ProtocolError = serde_json::from_str(
            r#"{"code":"gpu_on_fire","message":"the graphics card is on fire","detail":{"detail":"gpu_on_fire","celsius":900}}"#,
        )
        .expect("a wholly unfamiliar refusal still parses");

        assert_eq!(error.code, ErrorCode::Other("gpu_on_fire".to_owned()));
        assert_eq!(error.message, "the graphics card is on fire");
    }

    #[test]
    fn an_error_with_no_detail_does_not_carry_a_null_on_the_wire() {
        let json = serde_json::to_string(&ProtocolError::new(ErrorCode::NotRecording, "no"))
            .expect("it serialises");
        assert!(
            !json.contains("detail"),
            "an absent detail should be absent, not null: {json}"
        );
    }
}
