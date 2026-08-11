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
//! build (M3, issue #37)" is telling the truth; one that shows a greyed-out
//! button with no explanation is not (AGENTS.md section 27).
//!
//! # Unknown codes
//!
//! [`ErrorCode`] deliberately has an [`ErrorCode::Other`] variant and converts
//! from any string, so a build that meets a code added after it was compiled
//! keeps the message and degrades to "something the recorder refused" rather
//! than failing to parse the frame. That is the whole of the additive half of
//! the compatibility policy in `docs/ipc.md`.

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
    /// `recording_failed` — capture, encoding or muxing refused. Whatever had
    /// been written before the failure is still a finished file.
    RecordingFailed,
    /// `too_many_connections` — the recorder is already serving as many
    /// connections as it will. The endpoint is reachable by anything running as
    /// the user, so the cap is what stops a loop opening connections from
    /// costing the recorder a thread each.
    TooManyConnections,
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
            Self::RecordingFailed => "recording_failed",
            Self::TooManyConnections => "too_many_connections",
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
            "recording_failed" => Self::RecordingFailed,
            "too_many_connections" => Self::TooManyConnections,
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
/// Tagged by `detail`, so a client that meets a variant it does not know fails
/// to parse only the detail — which is why [`ProtocolError::detail`] is
/// optional and the message is not.
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
        /// The subsystem in the user's words: "the replay buffer".
        subsystem: String,
        /// The milestone that builds it, such as `M3`.
        milestone: String,
        /// The issue that tracks it.
        tracking_issue: u32,
    },
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
            ErrorCode::RecordingFailed,
            ErrorCode::TooManyConnections,
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
        let error = ProtocolError::not_implemented("the replay buffer", "M3", 37);

        assert_eq!(error.code, ErrorCode::NotImplemented);
        assert_eq!(
            error.detail,
            Some(ErrorDetail::NotImplemented {
                subsystem: "the replay buffer".to_owned(),
                milestone: "M3".to_owned(),
                tracking_issue: 37,
            }),
            "the UI has to be able to say which build this arrives in"
        );
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
