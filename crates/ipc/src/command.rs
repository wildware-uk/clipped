//! The commands the desktop application sends, and the replies it gets back.
//!
//! # Why a command is a name and a bag of parameters on the wire
//!
//! A [`Request`](crate::Request) carries `command` as a string and `params` as
//! an untyped object, and [`Command::from_request`] is what turns that into
//! something typed. The obvious alternative — one `serde` enum tagged by the
//! command name — was rejected because of what it does when it fails: a command
//! name the recorder has never heard of becomes a deserialisation error, which
//! is indistinguishable from a corrupt frame, and the useful part (which name)
//! survives only inside `serde`'s English. Parsing the envelope first means an
//! unknown command is [`ErrorCode::UnknownCommand`] *naming the command*, and a
//! command whose parameters are wrong is [`ErrorCode::InvalidParameters`]
//! carrying what was wrong with them — which is the difference between a UI
//! that can say what happened and one that says "protocol error".
//!
//! # Commands whose subsystem does not exist yet
//!
//! Four of the commands the protocol defines belong to subsystems that are not
//! built: the replay buffer, bookmarks, screenshots and the configuration API.
//! They are [`UnbuiltCommand`], they are refused with
//! [`ErrorCode::NotImplemented`] and the milestone and issue that build them,
//! and there is deliberately nowhere for them to be handled — a command that
//! could be wired to a handler which quietly did nothing is exactly what
//! AGENTS.md sections 27 and 54 forbid.
//!
//! Their *parameters* are left as an open object rather than given a schema.
//! Nobody knows yet what `save_replay` takes, because the thing it would ask
//! for does not exist; inventing a shape now would be a public API designed
//! against a guess, and one the milestone that builds it would have to break
//! (AGENTS.md section 43).

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, ProtocolError};
use crate::message::Request;
use crate::status::{RecorderStatus, RecordingSummary};

/// A command, parsed and known to be one this build understands.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Is the recorder alive, and how long does a round trip take.
    Ping,
    /// What is the recorder doing.
    GetStatus,
    /// Start recording something.
    StartRecording(StartRecording),
    /// Stop the recording that is running.
    StopRecording(StopRecording),
    /// A command whose subsystem this build does not contain.
    ///
    /// It is parsed — so that the refusal names the command rather than
    /// rejecting the name — and then refused. See
    /// [`UnbuiltCommand::refusal`].
    Unbuilt(UnbuiltCommand),
}

impl Command {
    /// The command's name on the wire.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::GetStatus => "get_status",
            Self::StartRecording(_) => "start_recording",
            Self::StopRecording(_) => "stop_recording",
            Self::Unbuilt(command) => command.name(),
        }
    }

    /// Parses a request into a command this build knows.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::UnknownCommand`] if no command has that name, naming the
    /// one that was asked for; [`ErrorCode::InvalidParameters`] if the
    /// parameters are not the shape the command takes, naming the command and
    /// carrying `serde`'s account of what was wrong with the value.
    pub fn from_request(request: &Request) -> Result<Self, ProtocolError> {
        match request.command.as_str() {
            "ping" => Ok(Self::Ping),
            "get_status" => Ok(Self::GetStatus),
            "start_recording" => Ok(Self::StartRecording(parse_params(request)?)),
            "stop_recording" => Ok(Self::StopRecording(parse_params(request)?)),
            name => match UnbuiltCommand::from_name(name) {
                Some(command) => Ok(Self::Unbuilt(command)),
                None => Err(ProtocolError::new(
                    ErrorCode::UnknownCommand,
                    format!("this recorder has no `{name}` command"),
                )),
            },
        }
    }

    /// Builds the request that carries this command, with the identifier the
    /// reply will quote.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] if the parameters cannot be
    /// represented, which for the types here means a `f64` that is not a
    /// number. Kept as an error rather than a panic because a client builds
    /// these from user input.
    pub fn to_request(&self, id: u64) -> Result<Request, ProtocolError> {
        let params = match self {
            Self::Ping | Self::GetStatus => Ok(serde_json::Value::Null),
            Self::StartRecording(start) => serde_json::to_value(start),
            Self::StopRecording(stop) => serde_json::to_value(stop),
            Self::Unbuilt(_) => Ok(serde_json::Value::Null),
        }
        .map_err(|error| {
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                format!("`{}` could not be represented: {error}", self.name()),
            )
        })?;

        Ok(Request {
            id,
            command: self.name().to_owned(),
            params,
        })
    }
}

/// Reads a command's parameters out of the request carrying them.
///
/// A missing or null `params` is the same as an empty object, so a command
/// whose fields are all optional can be sent without one.
fn parse_params<T: serde::de::DeserializeOwned>(request: &Request) -> Result<T, ProtocolError> {
    let params = if request.params.is_null() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        request.params.clone()
    };

    serde_json::from_value(params).map_err(|error| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "`{}` was not given usable parameters: {error}",
                request.command
            ),
        )
    })
}

/// A command the protocol defines and this build cannot perform.
///
/// Each one names the subsystem it needs, the milestone that builds it and the
/// issue that tracks it, so the refusal the UI renders is specific enough to
/// act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnbuiltCommand {
    /// `save_replay` — needs the replay buffer.
    SaveReplay,
    /// `add_bookmark` — needs the bookmark store.
    AddBookmark,
    /// `take_screenshot` — needs screenshot capture.
    TakeScreenshot,
    /// `apply_settings` — needs the configuration API.
    ApplySettings,
}

/// Every command the protocol defines and this build cannot perform.
///
/// A test walks this, so a command added to [`UnbuiltCommand`] and forgotten
/// here is a failure rather than a command that quietly stops being refused.
pub const UNBUILT_COMMANDS: &[UnbuiltCommand] = &[
    UnbuiltCommand::SaveReplay,
    UnbuiltCommand::AddBookmark,
    UnbuiltCommand::TakeScreenshot,
    UnbuiltCommand::ApplySettings,
];

impl UnbuiltCommand {
    /// The command's name on the wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SaveReplay => "save_replay",
            Self::AddBookmark => "add_bookmark",
            Self::TakeScreenshot => "take_screenshot",
            Self::ApplySettings => "apply_settings",
        }
    }

    /// The unbuilt command with that name, if it is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        UNBUILT_COMMANDS
            .iter()
            .copied()
            .find(|command| command.name() == name)
    }

    /// The subsystem it needs, in the words a person would use.
    #[must_use]
    pub const fn subsystem(self) -> &'static str {
        match self {
            Self::SaveReplay => "the replay buffer",
            Self::AddBookmark => "bookmarks",
            Self::TakeScreenshot => "screenshot capture",
            Self::ApplySettings => "the settings API",
        }
    }

    /// The milestone that builds it.
    #[must_use]
    pub const fn milestone(self) -> &'static str {
        match self {
            Self::SaveReplay => "M3",
            Self::AddBookmark | Self::TakeScreenshot => "M8",
            Self::ApplySettings => "M7",
        }
    }

    /// The issue that tracks it.
    #[must_use]
    pub const fn tracking_issue(self) -> u32 {
        match self {
            Self::SaveReplay => 37,
            Self::AddBookmark => 64,
            Self::TakeScreenshot => 67,
            Self::ApplySettings => 108,
        }
    }

    /// The refusal this command always gets in this build.
    #[must_use]
    pub fn refusal(self) -> ProtocolError {
        ProtocolError::not_implemented(self.subsystem(), self.milestone(), self.tracking_issue())
    }
}

/// What to record, and how.
///
/// The fields are the `clipped-recorder record` options under the names they
/// have on the command line, and the recorder validates them through the same
/// code path — so a value the command line rejects is rejected here, with the
/// same message. One set of rules, in one place (AGENTS.md section 55).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StartRecording {
    /// Record the window whose title contains this text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// Record the window belonging to this executable, such as `cs2.exe`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Record the window belonging to this process identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Where to write the recording. Absent means the recorder's own
    /// timestamped default under the user's videos directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Whether an existing file at `output` may be replaced. A recording is
    /// never overwritten silently.
    pub overwrite: bool,
    /// `source`, or `WIDTHxHEIGHT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Frames per second to encode at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framerate: Option<u32>,
    /// `auto`, `h264`, `hevc` or `av1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// `auto`, `nvenc`, `amf`, `quicksync` or `software`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    /// `default`, `none`, or part of a device name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub microphone: Option<String>,
    /// `default`, `none`, or part of a device name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_audio: Option<String>,
}

/// Which recording to stop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StopRecording {
    /// The recording to stop, as
    /// [`ActiveRecording::recording_id`](crate::ActiveRecording::recording_id)
    /// reported it.
    ///
    /// Absent means "whatever is running", which is what a tray menu wants.
    /// Naming it is what a UI does when the stop was aimed at a recording it
    /// had on screen, so that a recording which ended by itself in the meantime
    /// cannot have its successor stopped instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<String>,
}

/// What a command produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// The recorder is alive.
    Pong,
    /// What the recorder is doing.
    Status {
        /// The state, as of the moment the recorder answered.
        status: RecorderStatus,
    },
    /// A recording started.
    RecordingStarted {
        /// Identifies it to `stop_recording`.
        recording_id: String,
        /// The file it is writing.
        output: String,
    },
    /// A recording stopped, and its file is finished.
    RecordingStopped {
        /// What it turned out to be.
        summary: RecordingSummary,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: &str, params: serde_json::Value) -> Request {
        Request {
            id: 1,
            command: command.to_owned(),
            params,
        }
    }

    #[test]
    fn a_command_with_no_parameters_parses_from_a_null_a_missing_field_and_an_empty_object() {
        for params in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"ignored": true}),
        ] {
            assert_eq!(
                Command::from_request(&request("ping", params.clone())).expect("ping parses"),
                Command::Ping,
                "ping should not care about {params}"
            );
        }
    }

    #[test]
    fn an_unknown_command_is_refused_by_name_rather_than_as_a_broken_frame() {
        // The distinction matters to the UI: a name it sent that this recorder
        // does not have is a version-skew problem it can report; a broken frame
        // is a bug. They must not look the same.
        let error = Command::from_request(&request("delete_everything", serde_json::json!({})))
            .expect_err("no such command");

        assert_eq!(error.code, ErrorCode::UnknownCommand);
        assert!(
            error.message.contains("delete_everything"),
            "the refusal should name the command: {}",
            error.message
        );
    }

    #[test]
    fn parameters_of_the_wrong_shape_are_refused_with_what_was_wrong_with_them() {
        let error = Command::from_request(&request(
            "start_recording",
            serde_json::json!({"pid": "not a number"}),
        ))
        .expect_err("a pid is a number");

        assert_eq!(error.code, ErrorCode::InvalidParameters);
        assert!(
            error.message.contains("start_recording") && error.message.contains("expected u32"),
            "the refusal should name the command and say what was wrong: {}",
            error.message
        );
    }

    #[test]
    fn every_unbuilt_command_parses_and_then_refuses_with_somewhere_to_look() {
        for unbuilt in UNBUILT_COMMANDS {
            let command =
                Command::from_request(&request(unbuilt.name(), serde_json::json!({"any": 1})))
                    .expect("an unbuilt command still parses, so that it can be refused by name");
            assert_eq!(command, Command::Unbuilt(*unbuilt));

            let refusal = unbuilt.refusal();
            assert_eq!(refusal.code, ErrorCode::NotImplemented);
            assert!(
                refusal.detail.is_some(),
                "{} must say which milestone builds it",
                unbuilt.name()
            );
            assert!(
                unbuilt.tracking_issue() > 0,
                "{} must name a tracking issue",
                unbuilt.name()
            );
        }
    }

    #[test]
    fn a_command_round_trips_through_the_request_it_is_carried_in() {
        let start = Command::StartRecording(StartRecording {
            process: Some("cs2.exe".to_owned()),
            framerate: Some(144),
            ..StartRecording::default()
        });

        let request = start.to_request(7).expect("it can be represented");
        assert_eq!(request.id, 7);
        assert_eq!(request.command, "start_recording");

        let json = serde_json::to_string(&request).expect("it serialises");
        assert!(
            !json.contains("\"window\""),
            "an absent option should not reach the wire as a null: {json}"
        );

        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(Command::from_request(&back).expect("it parses"), start);
    }

    #[test]
    fn a_reply_round_trips() {
        let reply = Reply::Status {
            status: RecorderStatus::Idle,
        };
        let json = serde_json::to_string(&reply).expect("it serialises");
        assert_eq!(json, r#"{"reply":"status","status":{"state":"idle"}}"#);
        assert_eq!(
            serde_json::from_str::<Reply>(&json).expect("and deserialises"),
            reply
        );
    }
}
