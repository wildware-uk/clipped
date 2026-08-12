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
//! Two of the commands the protocol defines belong to subsystems that are not
//! built: a recording with a replay buffer and the configuration API.
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
//!
//! # The command with no handler behind it
//!
//! [`Command::Shutdown`] is the one command a [`CommandHandler`](crate::CommandHandler)
//! never sees, and for the opposite reason to [`UnbuiltCommand`]: not because
//! nothing can perform it, but because the thing that performs it is the accept
//! loop rather than the application. `crates/ipc/src/server.rs` answers it.

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, ProtocolError};
use crate::message::Request;
use crate::status::{BookmarkSummary, RecorderStatus, RecordingSummary, ScreenshotSummary};

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
    /// Mark this moment in the recording that is running.
    AddBookmark(AddBookmark),
    /// Save a still image of what is being captured.
    TakeScreenshot(TakeScreenshot),
    /// Stop serving, finish anything still being recorded, and exit.
    ///
    /// The one command not performed by a [`CommandHandler`](crate::CommandHandler):
    /// it is answered by [`crate::server`], which owns the accept loop and is
    /// therefore the only thing that can end it. See
    /// [`ShutdownRequest`](crate::server::ShutdownRequest) for the contract that
    /// makes "and exit" true rather than aspirational.
    Shutdown(Shutdown),
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
            Self::AddBookmark(_) => "add_bookmark",
            Self::TakeScreenshot(_) => "take_screenshot",
            Self::Shutdown(_) => "shutdown",
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
            "add_bookmark" => Ok(Self::AddBookmark(parse_params(request)?)),
            "take_screenshot" => Ok(Self::TakeScreenshot(parse_params(request)?)),
            "shutdown" => Ok(Self::Shutdown(parse_params(request)?)),
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
            Self::AddBookmark(bookmark) => serde_json::to_value(bookmark),
            Self::TakeScreenshot(screenshot) => serde_json::to_value(screenshot),
            Self::Shutdown(shutdown) => serde_json::to_value(shutdown),
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
    /// `save_replay` — needs a recording that is running a replay buffer.
    ///
    /// Not the buffer itself, which exists: [issue
    /// #37](https://github.com/wildware-uk/clipped/issues/37) wrote
    /// `clipped_replay::save_clip` and `clipped_session::record_with_replay`
    /// fills a buffer while it records. What no build does is *start* such a
    /// recording or route a save request to one, and that is [issue
    /// #38](https://github.com/wildware-uk/clipped/issues/38) — so that is the
    /// issue this refusal names.
    SaveReplay,
    /// `apply_settings` — needs the configuration API.
    ApplySettings,
}

/// Every command the protocol defines and this build cannot perform.
///
/// A test walks this, so a command added to [`UnbuiltCommand`] and forgotten
/// here is a failure rather than a command that quietly stops being refused.
pub const UNBUILT_COMMANDS: &[UnbuiltCommand] =
    &[UnbuiltCommand::SaveReplay, UnbuiltCommand::ApplySettings];

impl UnbuiltCommand {
    /// The command's name on the wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SaveReplay => "save_replay",
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
            Self::SaveReplay => "a recording with a replay buffer",
            Self::ApplySettings => "the settings API",
        }
    }

    /// The milestone that builds it.
    #[must_use]
    pub const fn milestone(self) -> &'static str {
        match self {
            Self::SaveReplay => "M3",
            Self::ApplySettings => "M7",
        }
    }

    /// The issue that tracks it.
    #[must_use]
    pub const fn tracking_issue(self) -> u32 {
        match self {
            Self::SaveReplay => 38,
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

/// What to mark, and how far before the press to mark it.
///
/// Every field is optional, because the command a hotkey sends carries none of
/// them: pressing the key is the whole interaction, and it must not stop to ask
/// for a label while somebody is playing (SPEC.md section 25).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AddBookmark {
    /// Which recording to mark, as
    /// [`ActiveRecording::recording_id`](crate::ActiveRecording::recording_id)
    /// reported it.
    ///
    /// Absent means "whatever is being recorded", which is what a hotkey and a
    /// tray menu want. Naming it is what a window that had a particular
    /// recording on screen does, so that a mark meant for a recording which
    /// ended in the meantime does not land in its successor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<String>,
    /// What to call the bookmark.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A colour, in whatever notation the interface writes; the recorder stores
    /// it and does not interpret it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    /// How long the marked moment lasts. Absent means it is a moment rather
    /// than a span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// How far *before* this request the bookmark should be stamped.
    ///
    /// Absent means the recorder's own default, which is not zero: a person
    /// presses the key after the thing they wanted to mark, so a bookmark
    /// stamped at the press is reliably late. `docs/bookmarks.md` gives the
    /// figure and the reasoning. A caller with no human reaction to allow for —
    /// a plugin marking an event it detected itself — sends `0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_seconds: Option<f64>,
}

/// What to photograph, and how to save it.
///
/// Every field is optional, because the shape a hotkey and a tray menu send is
/// no fields at all: photograph whatever is being recorded, in the configured
/// format, and put it where screenshots go (SPEC.md section 26).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TakeScreenshot {
    /// Which recording to take the picture from, as
    /// [`ActiveRecording::recording_id`](crate::ActiveRecording::recording_id)
    /// reported it.
    ///
    /// Absent means "whatever is being recorded", exactly as it does for
    /// [`AddBookmark`]. Naming it is what a window that had a particular
    /// recording on screen does, so that a screenshot meant for a recording
    /// which ended in the meantime is refused rather than taken of its
    /// successor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<String>,
    /// Photograph the window whose title contains this text.
    ///
    /// The three target fields — this, [`Self::process`] and [`Self::pid`] —
    /// are only consulted when **nothing is being recorded**. A screenshot
    /// taken during a recording comes from a frame that recording already
    /// captured, which is both far cheaper and the only way to be sure the
    /// picture is of what is being recorded.
    ///
    /// They are the same three [`StartRecording`] takes, under the same names,
    /// so a caller that can start a recording of something can photograph it
    /// (AGENTS.md section 43).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// Photograph the window belonging to this executable, such as `cs2.exe`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Photograph the window belonging to this process identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// `png`, `jpeg` or `webp`. Absent means the recorder's own default.
    ///
    /// A string rather than an enumeration for the reason
    /// [`StartRecording::codec`] is one: the recorder validates it through the
    /// same code path its own settings go through, so a value it would refuse
    /// on the command line is refused here with the same message (AGENTS.md
    /// section 55).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

/// How far a shutdown may go.
///
/// The parameter exists because the two answers to "the user asked to exit and a
/// recording is running" are both wrong as a default. Exiting regardless would
/// let anything running as this user end somebody's recording by sending four
/// words down a pipe; refusing outright would leave a recorder that can never be
/// stopped while it is recording, which is
/// [issue #220](https://github.com/wildware-uk/clipped/issues/220) again in a
/// smaller shape.
///
/// So the recorder refuses by default and performs it when asked in as many
/// words. A caller that has not thought about the recording cannot end one by
/// accident, and one that has — a tray menu whose item reads "Stop recording and
/// exit" — says so in the request. Either way the file is finished and playable:
/// the recording is stopped through the recorder's ordinary shutdown path, not
/// abandoned (AGENTS.md section 17).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shutdown {
    /// Whether a recording in progress may be stopped and its file finished
    /// first.
    ///
    /// `false` — the default, and what a request that omits it means — refuses
    /// the shutdown with [`ErrorCode::AlreadyRecording`] while something is
    /// being recorded, naming the recording so the caller can put the question
    /// to the user.
    pub finalise_recording: bool,
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
    /// A moment was marked, and the mark is on disk.
    BookmarkAdded {
        /// Where it landed, which is not where the key was pressed.
        bookmark: BookmarkSummary,
    },
    /// A screenshot was taken, and the file is on disk.
    ScreenshotTaken {
        /// The file, and what is in it.
        screenshot: ScreenshotSummary,
    },
    /// The recorder has stopped listening and is winding up.
    ///
    /// Sent **before** the recorder exits, because a reply written after the
    /// process ended would never arrive. What it promises is that the shutdown
    /// was accepted and the endpoint is closing; the observable proof that it
    /// finished is the endpoint going away, which
    /// [`supervisor::wait_for_recorder_to_exit`](crate::supervisor::wait_for_recorder_to_exit)
    /// waits for.
    ShuttingDown {
        /// The recording that will be stopped and finished before the recorder
        /// exits, if there was one.
        ///
        /// Absent when nothing was being recorded. Present only for a shutdown
        /// that asked for [`Shutdown::finalise_recording`], because one that did
        /// not is refused rather than answered while a recording is running —
        /// so this names a file the caller can tell the user about rather than a
        /// file it has just silently ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finalising: Option<crate::status::ActiveRecording>,
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
    fn a_bookmark_is_a_command_this_build_performs_rather_than_one_it_refuses() {
        // Issue #64 built the bookmark store, so `add_bookmark` stopped being
        // an `UnbuiltCommand`. Leaving it in that list would mean the recorder
        // refused every bookmark with "bookmarks arrive in M8" while the
        // subsystem sat there working, and the refusal reads plausibly enough
        // that nobody would question it.
        assert_eq!(UnbuiltCommand::from_name("add_bookmark"), None);
        assert!(
            !UNBUILT_COMMANDS
                .iter()
                .any(|unbuilt| unbuilt.name() == "add_bookmark"),
            "add_bookmark is still listed as unbuilt: {UNBUILT_COMMANDS:?}"
        );
        assert!(matches!(
            Command::from_request(&request("add_bookmark", serde_json::Value::Null)),
            Ok(Command::AddBookmark(_))
        ));
    }

    #[test]
    fn a_bare_bookmark_asks_for_nothing_and_gets_the_recorders_own_defaults() {
        // What the hotkey and the tray item send. Every field absent has to
        // mean "you decide", including the lead: a `lead_seconds` that
        // defaulted to zero here would silently undo the reaction-time
        // allowance for every caller that did not name one.
        for params in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"invented_later": true}),
        ] {
            assert_eq!(
                Command::from_request(&request("add_bookmark", params.clone())).expect("it parses"),
                Command::AddBookmark(AddBookmark::default()),
                "a bookmark carrying {params} must ask for nothing in particular"
            );
        }

        let bare = AddBookmark::default();
        assert_eq!(bare.lead_seconds, None);
        assert_eq!(bare.label, None);
        assert_eq!(bare.colour, None);
        assert_eq!(bare.duration_seconds, None);
        assert_eq!(bare.recording_id, None);
    }

    #[test]
    fn every_bookmark_parameter_survives_the_request_it_is_carried_in() {
        // Distinct values on purpose: a round trip whose fields could be
        // confused with one another passes while the code swaps two of them.
        let bookmark = Command::AddBookmark(AddBookmark {
            recording_id: Some("r-7".to_owned()),
            label: Some("triple kill".to_owned()),
            colour: Some("#ffcc00".to_owned()),
            duration_seconds: Some(12.5),
            lead_seconds: Some(3.25),
        });

        let request = bookmark.to_request(11).expect("it can be represented");
        assert_eq!(request.command, "add_bookmark");

        let json = serde_json::to_string(&request).expect("it serialises");
        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        let Command::AddBookmark(parsed) = Command::from_request(&back).expect("it parses") else {
            panic!("an add_bookmark request parsed as something else");
        };

        assert_eq!(parsed.recording_id.as_deref(), Some("r-7"));
        assert_eq!(parsed.label.as_deref(), Some("triple kill"));
        assert_eq!(parsed.colour.as_deref(), Some("#ffcc00"));
        assert_eq!(parsed.duration_seconds, Some(12.5));
        assert_eq!(parsed.lead_seconds, Some(3.25));
    }

    #[test]
    fn a_bookmark_reply_says_where_it_landed_and_not_only_that_it_was_taken() {
        // The lead means the mark is not where the key was pressed. A reply
        // that dropped either figure would leave an interface unable to say
        // what it had just done.
        let reply = Reply::BookmarkAdded {
            bookmark: BookmarkSummary {
                recording_id: "r-1".to_owned(),
                at_seconds: 115.0,
                pressed_at_seconds: 120.0,
                lead_seconds: 5.0,
                label: None,
                colour: None,
                duration_seconds: None,
                bookmarks_file: r"D:\clips\session.bookmarks.json".to_owned(),
                bookmarks_in_recording: 3,
            },
        };

        let json = serde_json::to_string(&reply).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<Reply>(&json).expect("and deserialises"),
            reply
        );
        assert!(
            json.contains("\"at_seconds\":115.0") && json.contains("\"pressed_at_seconds\":120.0"),
            "both the mark and the press have to reach the wire: {json}"
        );
        assert!(
            !json.contains("\"label\""),
            "an unlabelled bookmark should not reach the wire as a null: {json}"
        );
    }

    #[test]
    fn a_shutdown_that_says_nothing_is_the_one_that_will_not_end_a_recording() {
        // The default is the whole of the safety property: a caller that has
        // not thought about the recording cannot end one, because saying
        // nothing means "no". A `#[serde(default)]` that flipped to `true`
        // would turn every bare `{"command":"shutdown"}` on this machine into a
        // way to stop somebody's recording.
        for params in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"invented_later": true}),
        ] {
            assert_eq!(
                Command::from_request(&request("shutdown", params.clone())).expect("it parses"),
                Command::Shutdown(Shutdown {
                    finalise_recording: false
                }),
                "a shutdown carrying {params} must not be allowed to finalise a recording"
            );
        }

        assert_eq!(
            Command::from_request(&request(
                "shutdown",
                serde_json::json!({"finalise_recording": true})
            ))
            .expect("it parses"),
            Command::Shutdown(Shutdown {
                finalise_recording: true
            }),
        );
    }

    #[test]
    fn a_shutdown_round_trips_through_the_request_it_is_carried_in() {
        let shutdown = Command::Shutdown(Shutdown {
            finalise_recording: true,
        });
        let request = shutdown.to_request(3).expect("it can be represented");
        assert_eq!(request.command, "shutdown");

        let json = serde_json::to_string(&request).expect("it serialises");
        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(
            Command::from_request(&back).expect("it parses"),
            shutdown,
            "the parameter has to survive the wire, or every shutdown becomes the refusing one"
        );
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
