//! A machine-readable description of this protocol, derived from the types
//! that implement it.
//!
//! # Why this exists
//!
//! The desktop application's front end is TypeScript and needs a view of these
//! messages. Issue #209 weighed generating that view from the Rust types
//! against mirroring it by hand, and chose to mirror
//! (`docs/ipc.md`, "The TypeScript types", records the decision and the
//! reasons). A hand-written mirror is only worth having if something fails when
//! it stops matching, and this module is the Rust half of that something: it
//! emits [`ProtocolSchema`], a JSON description of the whole wire surface,
//! which `packages/shared/src/ipc/conformance.test.ts` checks the TypeScript
//! against.
//!
//! # Why it is derived rather than written
//!
//! A schema typed out by hand is a third thing to keep in step, and would fail
//! in the one way that matters least: agreeing with neither side. So nothing
//! here states a field name or a wire string as a literal.
//!
//! - **Field names and optionality** come from `serde` itself.
//!   [`structure_of`] serialises a fully populated value to see which fields
//!   exist, then removes each one in turn and asks whether the result still
//!   deserialises to the same variant. That is the deserialiser's own answer to
//!   "may this be left out", not a guess about what the attributes mean.
//! - **Wire strings** come from serialising real values.
//! - **Completeness** comes from the compiler, and where it can from `serde`
//!   too. Every `every_*` function below walks its enumeration through an
//!   exhaustive `match`, so a variant added to [`ErrorCode`], [`Event`],
//!   [`Reply`] or any of the others stops this module compiling until it is
//!   named here — and therefore until somebody has been made to look at the
//!   list it belongs in. Naming a variant and still leaving it out of the list
//!   is a hole the compiler cannot see, so for the closed enumerations it is
//!   closed a second way: `serde` publishes its own list of variants in the
//!   error it raises for a tag it does not know, and
//!   `a_closed_enumeration_lists_every_variant_the_deserialiser_has` holds this
//!   schema against it. The two enumerations with an untagged catch-all —
//!   [`Event`] and [`ErrorDetail`] — have no such list to be held against,
//!   because their catch-all is what stops the deserialiser ever raising that
//!   error; the `match` is what covers them. And a capability is not a variant
//!   at all, so [`features::ALL`] is generated from the constants that define
//!   it (`message.rs`).
//! - **What a sample means** comes from parsing it. Every entry in
//!   [`ProtocolSchema::samples`] records whether *this build* could read the
//!   frame and what it made of it, taken from running the frame through the
//!   real [`ServerMessage`] and [`ClientMessage`] deserialisers. The TypeScript
//!   side has to reach the same verdict on the same bytes.
//!
//! # The compatibility policy is part of the surface
//!
//! [`Enumeration::tolerates_unrecognised`] is the field this exists for.
//! `docs/ipc.md` says an unknown error code, end reason, error detail or event
//! must not make a frame unreadable, and that an unknown **recorder state**
//! must. That asymmetry is deliberate — an event a client cannot read is
//! information it does without, and a state it cannot read is a lie it would
//! otherwise tell — and it is the first thing a naive TypeScript mirror smooths
//! out. The samples carry the same asymmetry as evidence rather than as a
//! claim.

use std::collections::BTreeMap;
use std::mem;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::command::{
    Command, ExportRecording, Reply, Shutdown, StartRecording, StopRecording, UnbuiltCommand,
};
use crate::error::{ErrorCode, ErrorDetail, ProtocolError};
use crate::frame::{LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES};
use crate::hotkeys::{HotkeyBinding, HotkeyState};
use crate::library::{
    LibraryClip, LibraryGame, LibraryRecording, LibrarySession, LibrarySessionPage, LibrarySessions,
};
use crate::message::{
    features, ClientMessage, ConnectionRole, Event, EventStream, Hello, Outcome, PeerIdentity,
    Request, Response, ServerMessage, Welcome, PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::playback::{OpenPlayback, PlaybackStream, PlaybackTrack};
use crate::server::MAX_CONCURRENT_CONNECTIONS;
use crate::status::{
    ActiveRecording, EndReason, ExportSummary, RecorderStatus, RecordingSummary, SessionRecording,
    SessionSummary, Watching,
};

/// The shape of the schema document itself.
///
/// Incremented when the *description* changes shape — a field added to
/// [`Structure`], say — rather than when the protocol does. The TypeScript
/// check refuses a document it does not recognise, so that a schema written by
/// a newer build cannot be read as if it meant something else.
pub const SCHEMA_FORMAT: u32 = 1;

/// The command that writes the schema file, quoted in the file itself.
pub const REGENERATE_WITH: &str = "cargo run -p clipped-ipc --bin protocol-schema";

/// Where the schema is written, relative to the repository root.
pub const SCHEMA_PATH: &str = "packages/shared/src/ipc/protocol-schema.json";

/// Everything the TypeScript mirror has to agree with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolSchema {
    /// The shape of this document. See [`SCHEMA_FORMAT`].
    pub schema_format: u32,
    /// How to produce this file, for whoever opens it and wonders.
    pub regenerate_with: String,
    /// The protocol version this build speaks.
    pub protocol_version: u32,
    /// Every version this build accepts.
    pub supported_protocol_versions: Vec<u32>,
    /// The bytes around a message.
    pub framing: Framing,
    /// How many connections one recorder serves.
    pub max_concurrent_connections: usize,
    /// The `type` of each envelope, and the payload it carries.
    ///
    /// Keyed by direction: `client` is what the desktop application sends,
    /// `server` what the recorder does. The payload names either a key of
    /// [`ProtocolSchema::structures`] or the prefix of a family of them, such
    /// as `event`, whose members are `event.status_changed` and the rest.
    pub envelopes: BTreeMap<String, BTreeMap<String, String>>,
    /// The closed and open sets of wire strings.
    pub enumerations: BTreeMap<String, Enumeration>,
    /// The fields of every object on the wire.
    pub structures: BTreeMap<String, Structure>,
    /// The commands, and which of them this build performs.
    pub commands: Vec<CommandSchema>,
    /// Real frames, and what this build makes of each one.
    pub samples: Vec<Sample>,
}

/// Where one message ends and the next begins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Framing {
    /// The width of the length prefix.
    pub length_prefix_bytes: u32,
    /// The byte order of the length prefix.
    pub length_prefix_endianness: String,
    /// The largest frame either side will send or accept.
    pub max_frame_bytes: u32,
}

/// A set of wire strings, and what happens to one that is not in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enumeration {
    /// Every value this build knows, as it appears on the wire.
    pub values: Vec<String>,
    /// Whether a value outside `values` still leaves the message it is part of
    /// readable.
    ///
    /// `true` for the codes, reasons, details and events the compatibility
    /// policy says an older client must survive; `false` for the recorder's
    /// state, where guessing would be worse than failing, and for the tags that
    /// decide what a frame even is.
    pub tolerates_unrecognised: bool,
}

/// The fields of one object on the wire.
///
/// Both lists are alphabetical, because that is the order `serde_json`'s map
/// yields, and a stable order keeps the committed file free of noise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Structure {
    /// Fields whose absence makes the message unreadable.
    pub required: Vec<String>,
    /// Fields the deserialiser supplies a value for when they are missing.
    pub optional: Vec<String>,
}

/// One command, and whether this build can perform it.
///
/// Every field here is one the TypeScript check compares. The subsystem, the
/// milestone and the issue a refused command names are deliberately not among
/// them: this document exists to be checked rather than to be read, and those
/// three already cross the check inside the "a command this build cannot
/// perform" sample, which carries the whole `not_implemented` detail as the
/// recorder actually sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSchema {
    /// The name on the wire.
    pub name: String,
    /// The structure its parameters take, where this build defines one.
    pub params: Option<String>,
    /// The reply it produces, where this build produces one.
    pub reply: Option<String>,
    /// Whether this build performs it, or refuses it with `not_implemented`.
    pub available_in_this_build: bool,
}

/// A frame, and what this build made of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// What this sample is here to demonstrate.
    pub name: String,
    /// `client` for a frame the desktop application sends, `server` for one the
    /// recorder does.
    pub direction: String,
    /// The frame itself, as JSON.
    pub frame: Value,
    /// Whether this build could read it.
    pub parses: bool,
    /// What it turned out to be, as a dotted path through the tags that decide
    /// it: `response.ok.recording_stopped.stopped`. [`None`] when it did not
    /// parse.
    ///
    /// The path carries the wire strings rather than variant names, including
    /// ones this build does not recognise, so a mirror that quietly dropped an
    /// unknown error code or end reason produces a different path.
    pub discriminant: Option<String>,
}

/// The whole wire surface of this build.
#[must_use]
pub fn protocol_schema() -> ProtocolSchema {
    ProtocolSchema {
        schema_format: SCHEMA_FORMAT,
        regenerate_with: REGENERATE_WITH.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        supported_protocol_versions: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
        framing: Framing {
            length_prefix_bytes: LENGTH_PREFIX_BYTES as u32,
            length_prefix_endianness: "little".to_owned(),
            max_frame_bytes: MAX_FRAME_BYTES,
        },
        max_concurrent_connections: MAX_CONCURRENT_CONNECTIONS,
        envelopes: envelopes(),
        enumerations: enumerations(),
        structures: structures(),
        commands: commands(),
        samples: samples(),
    }
}

/// The `type` of each envelope and the payload it carries.
fn envelopes() -> BTreeMap<String, BTreeMap<String, String>> {
    let client = every_client_message()
        .iter()
        .map(|message| {
            let payload = match message {
                ClientMessage::Hello(_) => "hello",
                ClientMessage::Request(_) => "request",
            };
            (envelope_type(message), payload.to_owned())
        })
        .collect();

    let server = every_server_message()
        .iter()
        .map(|message| {
            let payload = match message {
                ServerMessage::Welcome(_) => "welcome",
                ServerMessage::Refused(_) => "protocol_error",
                ServerMessage::Response(_) => "response",
                ServerMessage::Event(_) => "event",
            };
            (envelope_type(message), payload.to_owned())
        })
        .collect();

    BTreeMap::from([("client".to_owned(), client), ("server".to_owned(), server)])
}

/// The closed and open sets of wire strings.
fn enumerations() -> BTreeMap<String, Enumeration> {
    BTreeMap::from([
        (
            "client_message_type".to_owned(),
            Enumeration {
                values: every_client_message().iter().map(envelope_type).collect(),
                tolerates_unrecognised: false,
            },
        ),
        (
            "server_message_type".to_owned(),
            Enumeration {
                values: every_server_message().iter().map(envelope_type).collect(),
                tolerates_unrecognised: false,
            },
        ),
        (
            "connection_role".to_owned(),
            Enumeration {
                // `ConnectionRole::Unknown` is not listed: it is what this build
                // turns a role it does not have into, not a role anything sends.
                // The recorder refuses such a connection by name rather than
                // guessing at it (`server.rs`).
                values: every_connection_role()
                    .iter()
                    .map(|role| role.as_str().to_owned())
                    .collect(),
                tolerates_unrecognised: true,
            },
        ),
        (
            "event_stream".to_owned(),
            Enumeration {
                values: every_event_stream()
                    .iter()
                    .map(|stream| stream.as_str().to_owned())
                    .collect(),
                // A stream name is kept rather than failing the handshake, and
                // is then refused at subscription time: a stream that was
                // accepted and never delivered would be a UI panel with no
                // explanation (`docs/ipc.md`, "Events").
                tolerates_unrecognised: true,
            },
        ),
        (
            "feature".to_owned(),
            Enumeration {
                values: every_feature(),
                tolerates_unrecognised: true,
            },
        ),
        (
            "command".to_owned(),
            Enumeration {
                values: commands().into_iter().map(|command| command.name).collect(),
                // A command name is a string on the envelope and is refused by
                // name after the frame has been read, which is what lets a UI
                // report version skew rather than "malformed frame".
                tolerates_unrecognised: true,
            },
        ),
        (
            "error_code".to_owned(),
            Enumeration {
                values: every_error_code()
                    .iter()
                    .map(|code| code.as_str().to_owned())
                    .collect(),
                tolerates_unrecognised: true,
            },
        ),
        (
            "error_detail".to_owned(),
            Enumeration {
                values: every_error_detail().iter().filter_map(detail_tag).collect(),
                tolerates_unrecognised: true,
            },
        ),
        (
            "end_reason".to_owned(),
            Enumeration {
                values: every_end_reason()
                    .iter()
                    .map(|reason| reason.as_str().to_owned())
                    .collect(),
                tolerates_unrecognised: true,
            },
        ),
        (
            "event".to_owned(),
            Enumeration {
                values: every_event().iter().filter_map(event_tag).collect(),
                tolerates_unrecognised: true,
            },
        ),
        (
            "outcome".to_owned(),
            Enumeration {
                values: every_outcome().iter().map(outcome_tag).collect(),
                tolerates_unrecognised: false,
            },
        ),
        (
            "reply".to_owned(),
            Enumeration {
                values: every_reply().iter().map(reply_tag).collect(),
                tolerates_unrecognised: false,
            },
        ),
        (
            "hotkey_state".to_owned(),
            Enumeration {
                values: every_hotkey_state().iter().map(hotkey_state_tag).collect(),
                // The same asymmetry as `recorder_state` below, and for the same
                // reason: a hotkey state this build cannot read would be drawn
                // as one it can, and every one it can says the key works.
                tolerates_unrecognised: false,
            },
        ),
        (
            "recorder_state".to_owned(),
            Enumeration {
                values: every_recorder_status().iter().map(state_tag).collect(),
                // The asymmetry the compatibility policy turns on. A state this
                // build has never heard of fails the message that carried it,
                // because a window showing "idle" for a recorder doing
                // something else is a lie, and a message it cannot read is not.
                tolerates_unrecognised: false,
            },
        ),
    ])
}

/// The fields of every object on the wire.
fn structures() -> BTreeMap<String, Structure> {
    let mut structures = BTreeMap::from([
        (
            "peer_identity".to_owned(),
            structure_of(&exemplar_peer(), &[]),
        ),
        ("hello".to_owned(), structure_of(&exemplar_hello(), &[])),
        ("welcome".to_owned(), structure_of(&exemplar_welcome(), &[])),
        ("request".to_owned(), structure_of(&exemplar_request(), &[])),
        (
            "response".to_owned(),
            structure_of(&exemplar_response(), &[]),
        ),
        (
            "protocol_error".to_owned(),
            structure_of(&exemplar_error(), &[]),
        ),
        (
            "start_recording".to_owned(),
            structure_of(&exemplar_start_recording(), &[]),
        ),
        (
            "stop_recording".to_owned(),
            structure_of(
                &StopRecording {
                    recording_id: Some("r-1".to_owned()),
                },
                &[],
            ),
        ),
        (
            "shutdown".to_owned(),
            structure_of(
                &Shutdown {
                    finalise_recording: true,
                },
                &[],
            ),
        ),
        (
            "add_bookmark".to_owned(),
            structure_of(&exemplar_add_bookmark(), &[]),
        ),
        (
            "take_screenshot".to_owned(),
            structure_of(&exemplar_take_screenshot(), &[]),
        ),
        (
            "save_replay".to_owned(),
            structure_of(&exemplar_save_replay(), &[]),
        ),
        (
            "recording_summary".to_owned(),
            structure_of(&exemplar_summary(), &[]),
        ),
        (
            "bookmark_summary".to_owned(),
            structure_of(&exemplar_bookmark(), &[]),
        ),
        (
            "screenshot_summary".to_owned(),
            structure_of(&exemplar_screenshot(), &[]),
        ),
        (
            "replay_summary".to_owned(),
            structure_of(&exemplar_replay(), &[]),
        ),
        (
            "active_recording".to_owned(),
            structure_of(&exemplar_active_recording(), &[]),
        ),
        (
            "session_summary".to_owned(),
            structure_of(&exemplar_ended_session(), &[]),
        ),
        (
            "session_recording".to_owned(),
            structure_of(&exemplar_session_recording(), &[]),
        ),
        (
            "library_sessions".to_owned(),
            structure_of(&exemplar_library_sessions(), &[]),
        ),
        (
            "library_session_page".to_owned(),
            structure_of(&exemplar_library_page(), &[]),
        ),
        (
            "library_session".to_owned(),
            structure_of(&exemplar_library_session(), &[]),
        ),
        (
            "library_recording".to_owned(),
            structure_of(&exemplar_library_recording(), &[]),
        ),
        (
            "library_clip".to_owned(),
            structure_of(&exemplar_library_clip(), &[]),
        ),
        (
            "library_game".to_owned(),
            structure_of(&exemplar_library_game(), &[]),
        ),
        (
            "export_recording".to_owned(),
            structure_of(&exemplar_export_recording(), &[]),
        ),
        (
            "export_summary".to_owned(),
            structure_of(&exemplar_export(), &[]),
        ),
        (
            "open_playback".to_owned(),
            structure_of(&exemplar_open_playback(), &[]),
        ),
        (
            "playback_stream".to_owned(),
            structure_of(&exemplar_playback(), &[]),
        ),
        (
            "playback_track".to_owned(),
            structure_of(&exemplar_playback_track(), &[]),
        ),
    ]);

    for outcome in every_outcome() {
        structures.insert(
            format!("outcome.{}", outcome_tag(&outcome)),
            structure_of(&outcome, &[]),
        );
    }
    for reply in every_reply() {
        structures.insert(
            format!("reply.{}", reply_tag(&reply)),
            structure_of(&reply, &["reply"]),
        );
    }
    structures.insert(
        "hotkey_binding".to_owned(),
        structure_of(&exemplar_hotkey_binding(), &[]),
    );
    for state in every_hotkey_state() {
        structures.insert(
            format!("hotkey_state.{}", hotkey_state_tag(&state)),
            structure_of(&state, &["state"]),
        );
    }
    for status in every_recorder_status() {
        structures.insert(
            format!("recorder_status.{}", state_tag(&status)),
            structure_of(&status, &["state"]),
        );
    }
    for event in every_event() {
        if let Some(tag) = event_tag(&event) {
            structures.insert(format!("event.{tag}"), structure_of(&event, &["event"]));
        }
    }
    for detail in every_error_detail() {
        if let Some(tag) = detail_tag(&detail) {
            structures.insert(
                format!("error_detail.{tag}"),
                structure_of(&detail, &["detail"]),
            );
        }
    }

    structures
}

/// The commands, in the order `docs/ipc.md` tables them.
fn commands() -> Vec<CommandSchema> {
    let mut commands: Vec<CommandSchema> = every_built_command()
        .iter()
        .map(|command| CommandSchema {
            name: command.name().to_owned(),
            params: match command {
                Command::StartRecording(_) => Some("start_recording".to_owned()),
                Command::StopRecording(_) => Some("stop_recording".to_owned()),
                Command::AddBookmark(_) => Some("add_bookmark".to_owned()),
                Command::TakeScreenshot(_) => Some("take_screenshot".to_owned()),
                Command::SaveReplay(_) => Some("save_replay".to_owned()),
                Command::LibrarySessions(_) => Some("library_sessions".to_owned()),
                Command::LibraryEvents(_) => Some("library_events".to_owned()),
                Command::LibraryTrash(_) => Some("library_trash".to_owned()),
                Command::RestoreFromTrash(_) => Some("restore_from_trash".to_owned()),
                Command::EmptyTrash(_) => Some("empty_trash".to_owned()),
                Command::SetFavourite(_) => Some("set_favourite".to_owned()),
                Command::SetLock(_) => Some("set_lock".to_owned()),
                Command::ExportRecording(_) => Some("export_recording".to_owned()),
                Command::OpenPlayback(_) => Some("open_playback".to_owned()),
                Command::Shutdown(_) => Some("shutdown".to_owned()),
                Command::Ping
                | Command::GetStatus
                | Command::LibraryGames
                | Command::Plugins
                | Command::GetHotkeys
                | Command::Unbuilt(_) => None,
            },
            reply: match command {
                Command::Ping => Some("reply.pong".to_owned()),
                Command::GetStatus => Some("reply.status".to_owned()),
                Command::StartRecording(_) => Some("reply.recording_started".to_owned()),
                Command::StopRecording(_) => Some("reply.recording_stopped".to_owned()),
                Command::AddBookmark(_) => Some("reply.bookmark_added".to_owned()),
                Command::TakeScreenshot(_) => Some("reply.screenshot_taken".to_owned()),
                Command::SaveReplay(_) => Some("reply.replay_saved".to_owned()),
                Command::LibrarySessions(_) => Some("reply.library_sessions".to_owned()),
                Command::LibraryGames => Some("reply.library_games".to_owned()),
                Command::LibraryEvents(_) => Some("reply.library_events".to_owned()),
                Command::LibraryTrash(_) => Some("reply.library_trash".to_owned()),
                Command::RestoreFromTrash(_) => Some("reply.restored".to_owned()),
                Command::EmptyTrash(_) => Some("reply.trash_emptied".to_owned()),
                Command::SetFavourite(_) => Some("reply.favourited".to_owned()),
                Command::SetLock(_) => Some("reply.locked".to_owned()),
                Command::Plugins => Some("reply.plugins".to_owned()),
                Command::ExportRecording(_) => Some("reply.recording_exported".to_owned()),
                Command::OpenPlayback(_) => Some("reply.playback_opened".to_owned()),
                Command::GetHotkeys => Some("reply.hotkeys".to_owned()),
                Command::Shutdown(_) => Some("reply.shutting_down".to_owned()),
                Command::Unbuilt(_) => None,
            },
            available_in_this_build: true,
        })
        .collect();

    commands.extend(crate::command::UNBUILT_COMMANDS.iter().map(|unbuilt| {
        CommandSchema {
            name: unbuilt.name().to_owned(),
            // Deliberately no parameter schema: nobody knows yet what
            // `save_replay` takes, and inventing a shape now would be a public
            // API designed against a guess (AGENTS.md section 43).
            params: None,
            reply: None,
            available_in_this_build: false,
        }
    }));

    commands
}

/// Real frames, and what this build makes of each one.
///
/// Nothing here states an expected outcome: every sample is run through the
/// real deserialiser and records what happened. A sample that stops parsing
/// because the protocol changed changes this file, and the TypeScript check
/// then has to reach the same new verdict.
fn samples() -> Vec<Sample> {
    let mut samples = Vec::new();

    for (name, message) in [
        (
            "a control handshake",
            ClientMessage::Hello(exemplar_hello()),
        ),
        (
            "an events handshake, naming its streams",
            ClientMessage::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                client: exemplar_peer(),
                role: ConnectionRole::Events,
                streams: vec![EventStream::Status, EventStream::Errors],
            }),
        ),
        (
            "a command with no parameters",
            ClientMessage::Request(Request {
                id: 7,
                command: "get_status".to_owned(),
                params: Value::Null,
            }),
        ),
        (
            "a command with parameters",
            ClientMessage::Request(Request {
                id: 1,
                command: "start_recording".to_owned(),
                params: serde_json::to_value(exemplar_start_recording())
                    .expect("the start options serialise"),
            }),
        ),
        (
            "asking for a page of the library",
            ClientMessage::Request(Request {
                id: 9,
                command: "library_sessions".to_owned(),
                params: serde_json::to_value(exemplar_library_sessions())
                    .expect("the listing options serialise"),
            }),
        ),
        (
            "asking what the library holds per game",
            ClientMessage::Request(Request {
                id: 10,
                command: "library_games".to_owned(),
                params: Value::Null,
            }),
        ),
        (
            "asking for a recording to be copied into MP4",
            ClientMessage::Request(Request {
                id: 11,
                command: "export_recording".to_owned(),
                params: serde_json::to_value(exemplar_export_recording())
                    .expect("the export options serialise"),
            }),
        ),
        (
            "asking for a recording to be opened for playback, on one of its tracks",
            ClientMessage::Request(Request {
                id: 13,
                command: "open_playback".to_owned(),
                params: serde_json::to_value(exemplar_open_playback())
                    .expect("the playback options serialise"),
            }),
        ),
        (
            "asking the recorder to exit",
            ClientMessage::Request(Request {
                id: 8,
                command: "shutdown".to_owned(),
                params: serde_json::to_value(Shutdown::default())
                    .expect("the shutdown options serialise"),
            }),
        ),
        (
            "asking the recorder to finish a recording and exit",
            ClientMessage::Request(Request {
                id: 8,
                command: "shutdown".to_owned(),
                params: serde_json::to_value(Shutdown {
                    finalise_recording: true,
                })
                .expect("the shutdown options serialise"),
            }),
        ),
    ] {
        samples.push(client_sample(
            name,
            serde_json::to_value(&message).expect("a client message serialises"),
        ));
    }

    for (name, message) in [
        (
            "a control connection accepted",
            ServerMessage::Welcome(exemplar_welcome()),
        ),
        (
            "a connection refused for its protocol version",
            ServerMessage::Refused(
                ProtocolError::new(
                    ErrorCode::UnsupportedProtocolVersion,
                    "this recorder speaks protocol version 2, and 3 was asked for",
                )
                .with_detail(ErrorDetail::UnsupportedProtocolVersion {
                    requested: 3,
                    supported: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
                    recorder_version: "0.1.0".to_owned(),
                }),
            ),
        ),
        (
            "a reply with nothing in it",
            ServerMessage::Response(Response {
                id: 7,
                outcome: Outcome::Ok(Reply::Pong),
            }),
        ),
        (
            "the status of a recorder doing nothing",
            ServerMessage::Response(Response {
                id: 7,
                outcome: Outcome::Ok(Reply::Status {
                    status: RecorderStatus::Idle,
                }),
            }),
        ),
        (
            "the status of a recorder watching for a game to start",
            ServerMessage::Response(Response {
                id: 7,
                outcome: Outcome::Ok(Reply::Status {
                    status: RecorderStatus::Watching(Watching::default()),
                }),
            }),
        ),
        (
            "the status of a recorder watching for a game that just exited to come back",
            ServerMessage::Response(Response {
                id: 7,
                outcome: Outcome::Ok(Reply::Status {
                    status: exemplar_watching(),
                }),
            }),
        ),
        (
            "the status of a recorder that is recording",
            ServerMessage::Response(Response {
                id: 7,
                outcome: Outcome::Ok(Reply::Status {
                    status: exemplar_recording(),
                }),
            }),
        ),
        (
            "the status of a recording that belongs to no sitting",
            ServerMessage::Response(Response {
                id: 7,
                outcome: Outcome::Ok(Reply::Status {
                    status: RecorderStatus::Recording(ActiveRecording {
                        session: None,
                        ..exemplar_active_recording()
                    }),
                }),
            }),
        ),
        (
            "a recording started",
            ServerMessage::Response(Response {
                id: 1,
                outcome: Outcome::Ok(Reply::RecordingStarted {
                    recording_id: "r-1".to_owned(),
                    output: r"D:\clips\session.mkv".to_owned(),
                }),
            }),
        ),
        (
            "a recording stopped, and what it turned out to be",
            ServerMessage::Response(Response {
                id: 2,
                outcome: Outcome::Ok(Reply::RecordingStopped {
                    summary: exemplar_summary(),
                }),
            }),
        ),
        (
            "a recording too short to measure a frame rate over",
            ServerMessage::Response(Response {
                id: 2,
                outcome: Outcome::Ok(Reply::RecordingStopped {
                    summary: RecordingSummary {
                        sustained_framerate: None,
                        ..exemplar_summary()
                    },
                }),
            }),
        ),
        (
            "a moment marked in the recording that is running",
            ServerMessage::Response(Response {
                id: 5,
                outcome: Outcome::Ok(Reply::BookmarkAdded {
                    bookmark: exemplar_bookmark(),
                }),
            }),
        ),
        (
            "a screenshot written to disk",
            ServerMessage::Response(Response {
                id: 6,
                outcome: Outcome::Ok(Reply::ScreenshotTaken {
                    screenshot: exemplar_screenshot(),
                }),
            }),
        ),
        (
            "a replay clip saved out of the buffer",
            ServerMessage::Response(Response {
                id: 12,
                outcome: Outcome::Ok(Reply::ReplaySaved {
                    clip: exemplar_replay(),
                }),
            }),
        ),
        (
            "a page of the library, with more after it",
            ServerMessage::Response(Response {
                id: 9,
                outcome: Outcome::Ok(Reply::LibrarySessions {
                    page: exemplar_library_page(),
                }),
            }),
        ),
        (
            "a library with nothing in it, which is not a library that could not be read",
            ServerMessage::Response(Response {
                id: 9,
                outcome: Outcome::Ok(Reply::LibrarySessions {
                    page: LibrarySessionPage::default(),
                }),
            }),
        ),
        (
            "a recording whose file has gone",
            ServerMessage::Response(Response {
                id: 9,
                outcome: Outcome::Ok(Reply::LibrarySessions {
                    page: LibrarySessionPage {
                        sessions: vec![LibrarySession {
                            recordings: vec![LibraryRecording {
                                missing_since: Some("2026-08-12T09:00:00+01:00".to_owned()),
                                ..exemplar_library_recording()
                            }],
                            clips: Vec::new(),
                            ..exemplar_library_session()
                        }],
                        next_cursor: None,
                    },
                }),
            }),
        ),
        (
            "the marks on one recording's timeline",
            ServerMessage::Response(Response {
                id: 11,
                outcome: Outcome::Ok(Reply::LibraryEvents {
                    lane: crate::library::LibraryEventLane {
                        marks: vec![
                            crate::library::LibraryEventMark {
                                recording: "1".to_owned(),
                                at: 4_000_000_000,
                                kind: "kill".to_owned(),
                                source: "counter-strike-2".to_owned(),
                            },
                            // A kind this build has never met, carried through
                            // and drawn like any other. A mirror that validated
                            // `kind` against a list would delete exactly the
                            // marks that have to survive
                            // (`crate::library::LibraryEventMark`).
                            crate::library::LibraryEventMark {
                                recording: "1".to_owned(),
                                at: 9_500_000_000,
                                kind: "acme-cs2.flashbang_blinded_five".to_owned(),
                                source: "acme-cs2".to_owned(),
                            },
                        ],
                    },
                }),
            }),
        ),
        (
            "what is waiting in the trash, and what emptying it would take",
            ServerMessage::Response(Response {
                id: 14,
                outcome: Outcome::Ok(Reply::LibraryTrash {
                    trash: crate::library::TrashListing {
                        items: vec![crate::library::TrashedItem {
                            kind: "recording".to_owned(),
                            id: 1,
                            path: r"D:\\Clips.trash\\clipped-cs2-20260814-201500.mkv".to_owned(),
                            original_path: r"D:\\Clips\\clipped-cs2-20260814-201500.mkv"
                                .to_owned(),
                            deleted_at: "2026-08-15T09:00:00+01:00".to_owned(),
                            expires_at: Some("2026-09-14T09:00:00+01:00".to_owned()),
                            size_bytes: Some(2_147_483_648),
                            // Two clips were cut from it, and they point at a
                            // file that is not where they think it is. A screen
                            // says so before somebody empties the trash,
                            // because that is when they stop being
                            // recoverable.
                            dependent_clips: 2,
                        }],
                        total_items: 1,
                        total_bytes: 2_147_483_648,
                        directory: r"D:\\Clips.trash".to_owned(),
                    },
                }),
            }),
        ),
        (
            "one recording put back where it was",
            ServerMessage::Response(Response {
                id: 15,
                outcome: Outcome::Ok(Reply::Restored {
                    restored: crate::library::RestoredItem {
                        kind: "recording".to_owned(),
                        id: 1,
                        path: r"D:\\Clips\\clipped-cs2-20260814-201500.mkv".to_owned(),
                        file_restored: true,
                        renamed: false,
                    },
                }),
            }),
        ),
        (
            "the trash emptied, with one file that would not go",
            ServerMessage::Response(Response {
                id: 16,
                outcome: Outcome::Ok(Reply::TrashEmptied {
                    emptied: crate::library::TrashEmptied {
                        removed: 1,
                        reclaimed_bytes: 2_147_483_648,
                        refused: vec![
                            r"D:\\Clips.trash\\locked.mkv: the file is open in another program"
                                .to_owned(),
                        ],
                    },
                }),
            }),
        ),
        (
            "a sitting marked a favourite, which is what cleanup then protects",
            ServerMessage::Response(Response {
                id: 17,
                outcome: Outcome::Ok(Reply::Favourited {
                    mark: crate::library::FavouriteMark {
                        // The kind that is addressed by text rather than by an
                        // integer, so a sample carries the awkward half of the
                        // target rather than only the easy one.
                        kind: "session".to_owned(),
                        session_id: "counter-strike-2-20260814-201500".to_owned(),
                        id: 0,
                        favourite: true,
                        changed: true,
                    },
                }),
            }),
        ),
        (
            "a recording locked against automatic cleanup, and nothing else",
            ServerMessage::Response(Response {
                id: 18,
                outcome: Outcome::Ok(Reply::Locked {
                    lock: crate::library::LockMark {
                        kind: "recording".to_owned(),
                        session_id: String::new(),
                        id: 1,
                        // Its own lock, so both are true. The pair exists for
                        // the other case: a recording inside a locked sitting
                        // is protected without having one.
                        locked: true,
                        protected: true,
                        changed: true,
                    },
                }),
            }),
        ),
        (
            "what is installed, and what each plugin asks for",
            ServerMessage::Response(Response {
                id: 12,
                outcome: Outcome::Ok(Reply::Plugins {
                    installed: vec![
                        exemplar_plugin(),
                        // One whose declaration has changed since it was
                        // allowed. Both texts travel, because "here is what
                        // changed" cannot be asked with one of them.
                        crate::plugins::PluginDeclaration {
                            id: "acme-dota2".to_owned(),
                            name: "Dota 2 highlights".to_owned(),
                            version: "0.2.0".to_owned(),
                            description: "Reports kills and objectives.".to_owned(),
                            network: vec![
                                "Listens on 127.0.0.1:3213 (this machine only) — receives Dota 2                                  game state"
                                    .to_owned(),
                            ],
                            enforcement: "Clipped shows what a plugin declares and refuses to start one whose declaration has \n             changed since you allowed it. It cannot yet stop a plugin from using the \n             network in ways it did not declare."
                .to_owned(),
                            state: crate::plugins::PluginState::NeedsConsentAgain {
                                agreed_to: "loopback listen 127.0.0.1:3213".to_owned(),
                                now_declares: "loopback listen 127.0.0.1:3213; outbound connect                                                telemetry.example:443"
                                    .to_owned(),
                            },
                        },
                    ],
                    refused: Vec::new(),
                }),
            }),
        ),
        (
            "what the library holds per game",
            ServerMessage::Response(Response {
                id: 10,
                outcome: Outcome::Ok(Reply::LibraryGames {
                    games: vec![
                        exemplar_library_game(),
                        // The sittings the catalogue would not attribute. It has
                        // no identifier and no name, and a mirror that required
                        // either would fail to read it.
                        LibraryGame {
                            game_id: None,
                            name: None,
                            first_seen_at: None,
                            last_played_at: None,
                            sessions: 2,
                            recordings: 2,
                            clips: 0,
                            favourites: 0,
                            bytes: 1_204_889,
                            missing: 0,
                        },
                    ],
                }),
            }),
        ),
        (
            "a recording copied into MP4 whole",
            ServerMessage::Response(Response {
                id: 11,
                outcome: Outcome::Ok(Reply::RecordingExported {
                    export: ExportSummary {
                        lossless: true,
                        losses: Vec::new(),
                        ..exemplar_export()
                    },
                }),
            }),
        ),
        (
            "a recording copied into MP4 without everything it held",
            ServerMessage::Response(Response {
                id: 11,
                outcome: Outcome::Ok(Reply::RecordingExported {
                    export: exemplar_export(),
                }),
            }),
        ),
        (
            "a recording opened for playback on the track a player would have taken anyway",
            ServerMessage::Response(Response {
                id: 13,
                outcome: Outcome::Ok(Reply::PlaybackOpened {
                    playback: PlaybackStream {
                        path: r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
                        audio_track: Some(1),
                        prepared: false,
                        ..exemplar_playback()
                    },
                }),
            }),
        ),
        (
            "a recording opened for playback on a track that needed a copy of its own",
            ServerMessage::Response(Response {
                id: 13,
                outcome: Outcome::Ok(Reply::PlaybackOpened {
                    playback: exemplar_playback(),
                }),
            }),
        ),
        (
            "playback refused because the recording's file has gone",
            ServerMessage::Response(Response {
                id: 13,
                outcome: Outcome::Error(ProtocolError::new(
                    ErrorCode::PlaybackFailed,
                    "cs2-20260811-201400-1.mkv is not there any more. It may have been moved or \
                     deleted, or the drive it is on may not be connected",
                )),
            }),
        ),
        (
            "an export refused because the file it would write already exists",
            ServerMessage::Response(Response {
                id: 11,
                outcome: Outcome::Error(ProtocolError::new(
                    ErrorCode::DestinationExists,
                    "there is already a file at cs2-20260811-201400-1.mp4, and Clipped does not                      overwrite one; choose another name",
                )),
            }),
        ),
        (
            "an export refused because MP4 cannot carry one of the recording's tracks",
            ServerMessage::Response(Response {
                id: 11,
                outcome: Outcome::Error(ProtocolError::new(
                    ErrorCode::ExportFailed,
                    "cs2-20260811-201400-1.mkv cannot be remuxed to MP4 without losing part of                      the recording: audio track 1 (wavpack). Nothing was written; the recording                      is unchanged and still playable as it is",
                )),
            }),
        ),
        (
            "a library that could not be read",
            ServerMessage::Response(Response {
                id: 9,
                outcome: Outcome::Error(ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    "the recording library could not be opened: the database is from a newer \
                     build of Clipped",
                )),
            }),
        ),
        (
            "a search that does not parse",
            ServerMessage::Response(Response {
                id: 9,
                outcome: Outcome::Error(ProtocolError::new(
                    ErrorCode::InvalidParameters,
                    "`library_sessions` was not given a usable query: expected a term after `OR` \
                     at position 8",
                )),
            }),
        ),
        (
            "the hotkeys, one of which another application already owns",
            ServerMessage::Response(Response {
                id: 12,
                outcome: Outcome::Ok(Reply::Hotkeys {
                    hotkeys: vec![
                        // The row this reply exists for: a combination the user
                        // cannot have, carrying the sentence a window shows.
                        // The recorder performs this one, so `handled` is true
                        // and `unavailable` absent — which is exactly why the
                        // two questions are separate fields.
                        HotkeyBinding {
                            action: "save_replay".to_owned(),
                            label: "Save replay".to_owned(),
                            hotkey: Some("Ctrl+F10".to_owned()),
                            state: HotkeyState::Conflict {
                                reason: "Ctrl+F10 could not be Clipped's shortcut for Save                                          replay: another application already uses it. Choose a                                          different combination, or close the application that                                          has this one and try again"
                                    .to_owned(),
                            },
                            handled: true,
                            unavailable: None,
                        },
                        // A hotkey that works: registered, and something behind
                        // it.
                        HotkeyBinding {
                            action: "add_bookmark".to_owned(),
                            label: "Add bookmark".to_owned(),
                            hotkey: Some("Ctrl+F9".to_owned()),
                            state: HotkeyState::Registered,
                            handled: true,
                            unavailable: None,
                        },
                        // An action nobody has bound, which is most of them.
                        HotkeyBinding {
                            action: "take_screenshot".to_owned(),
                            label: "Take screenshot".to_owned(),
                            hotkey: None,
                            state: HotkeyState::Unbound,
                            handled: true,
                            unavailable: None,
                        },
                        // And one with nothing behind it, carrying the sentence
                        // a settings screen shows in place of a working row.
                        exemplar_hotkey_binding(),
                    ],
                }),
            }),
        ),
        (
            "a shutdown accepted, with nothing being recorded",
            ServerMessage::Response(Response {
                id: 8,
                outcome: Outcome::Ok(Reply::ShuttingDown { finalising: None }),
            }),
        ),
        (
            "a shutdown accepted, naming the recording it will finish",
            ServerMessage::Response(Response {
                id: 8,
                outcome: Outcome::Ok(Reply::ShuttingDown {
                    finalising: Some(exemplar_active_recording()),
                }),
            }),
        ),
        (
            "a shutdown refused because a recording is running",
            ServerMessage::Response(Response {
                id: 8,
                outcome: Outcome::Error(ProtocolError::new(
                    ErrorCode::AlreadyRecording,
                    "`process `cs2.exe`` is being recorded to D:\\clips\\session.mkv; ask again \
                     with `finalise_recording` to finish that file and exit",
                )),
            }),
        ),
        (
            "a command this build cannot perform",
            ServerMessage::Response(Response {
                id: 3,
                outcome: Outcome::Error(UnbuiltCommand::ApplySettings.refusal()),
            }),
        ),
        (
            "a refusal with no detail",
            ServerMessage::Response(Response {
                id: 4,
                outcome: Outcome::Error(ProtocolError::new(
                    ErrorCode::NotRecording,
                    "there is nothing to stop",
                )),
            }),
        ),
        (
            "the state an events connection opens with",
            ServerMessage::Event(Event::StatusChanged {
                status: RecorderStatus::Idle,
            }),
        ),
        (
            "a recording appearing on the status stream",
            ServerMessage::Event(Event::StatusChanged {
                status: exemplar_recording(),
            }),
        ),
        (
            "a recorder settling back to watching after a game exited",
            ServerMessage::Event(Event::StatusChanged {
                status: exemplar_watching(),
            }),
        ),
        (
            "a sitting ending, with the files it produced",
            ServerMessage::Event(Event::SessionEnded {
                session: exemplar_ended_session(),
            }),
        ),
        (
            "a sitting of a game the catalogue would not attribute ending",
            ServerMessage::Event(Event::SessionEnded {
                session: SessionSummary {
                    session_id: "unattributed-20260811-201400".to_owned(),
                    game_id: None,
                    game_name: None,
                    end_reason: Some("recorder-stopping".to_owned()),
                    ..exemplar_ended_session()
                },
            }),
        ),
        (
            "a recording that ended by itself",
            ServerMessage::Event(Event::RecordingFailed {
                recording_id: "r-1".to_owned(),
                error: ProtocolError::new(
                    ErrorCode::RecordingFailed,
                    "the encoder stopped accepting frames",
                ),
            }),
        ),
    ] {
        samples.push(server_sample(
            name,
            serde_json::to_value(&message).expect("a server message serialises"),
        ));
    }

    // Everything below is a frame this build was not compiled against: what a
    // client meets when the recorder is newer than it is. The verdicts are
    // whatever the real deserialiser produces, and the TypeScript side has to
    // produce the same ones. This is the half of the protocol that cannot be
    // checked by reading the types.
    samples.push(server_sample(
        "an error code invented later",
        json!({"type": "response", "id": 5, "outcome": {"error": {
            "code": "gpu_on_fire", "message": "the graphics card is on fire"}}}),
    ));
    samples.push(server_sample(
        "an error detail invented later",
        json!({"type": "response", "id": 5, "outcome": {"error": {
            "code": "recording_failed",
            "message": "the disk the recording was being written to is full",
            "detail": {"detail": "disk_full", "free_bytes": 0}}}}),
    ));
    samples.push(server_sample(
        "an end reason invented later",
        json!({"type": "response", "id": 2, "outcome": {"ok": {
            "reply": "recording_stopped",
            "summary": {"output": "D:\\clips\\session.mkv", "duration_ms": 3800,
                        "end_reason": "display_disconnected", "frames_encoded": 115,
                        "frames_skipped_for_rate": 0, "frames_dropped_writer_behind": 0,
                        "encoder": "nvenc", "codec": "av1", "width": 1280, "height": 720}}}}),
    ));
    samples.push(server_sample(
        "a field invented later",
        json!({"type": "event", "event": "status_changed",
               "status": {"state": "recording", "recording_id": "r-1",
                          "output": "D:\\clips\\session.mkv", "target": "process `cs2.exe`",
                          "elapsed_ms": 4200, "gpu_temperature_c": 71}}),
    ));
    samples.push(server_sample(
        "an event invented later",
        json!({"type": "event", "event": "disk_filling_up", "free_bytes": 1024}),
    ));
    samples.push(server_sample(
        "a reason for a sitting to end invented later",
        json!({"type": "event", "event": "session_ended",
               "session": {"session_id": "cs2-20260811-201400", "game_id": "cs2",
                           "game_name": "Counter-Strike 2",
                           "started_at": "2026-08-11T20:14:00+01:00",
                           "ended_at": "2026-08-11T22:03:00+01:00",
                           "end_reason": "disk_full",
                           "recordings": [{"session_index": 1,
                                           "output": "D:\\clips\\cs2-20260811-201400-01.mkv",
                                           "outcome": "recorded",
                                           "duration_ms": 1800000}]}}),
    ));
    samples.push(server_sample(
        "a feature invented later",
        json!({"type": "welcome", "protocol_version": 1,
               "recorder": {"name": "clipped-recorder", "version": "9.0.0"},
               "role": "control", "features": ["recording", "status_events", "telepathy"]}),
    ));
    samples.push(client_sample(
        "an event stream invented later",
        json!({"type": "hello", "protocol_version": 1,
               "client": {"name": "clipped-desktop", "version": "9.0.0"},
               "role": "events", "streams": ["status", "telemetry"]}),
    ));
    samples.push(client_sample(
        "a command invented later",
        json!({"type": "request", "id": 9, "command": "defrost_the_gpu"}),
    ));

    // And the other half of the policy: what must *not* be read, whatever the
    // cost. A state this build does not know is the one unknown value that is
    // worse to guess at than to fail on.
    samples.push(server_sample(
        "a recorder state invented later, inside a reply",
        json!({"type": "response", "id": 7, "outcome": {"ok": {
            "reply": "status", "status": {"state": "paused"}}}}),
    ));
    samples.push(server_sample(
        "a recorder state invented later, inside an event",
        json!({"type": "event", "event": "status_changed", "status": {"state": "paused"}}),
    ));
    samples.push(server_sample(
        "a reply invented later",
        json!({"type": "response", "id": 7, "outcome": {"ok": {"reply": "teleported"}}}),
    ));
    samples.push(server_sample(
        "an outcome that is neither ok nor error",
        json!({"type": "response", "id": 7, "outcome": {"maybe": {}}}),
    ));
    samples.push(server_sample(
        "an envelope type invented later",
        json!({"type": "telemetry", "watts": 400}),
    ));
    samples.push(server_sample(
        "a frame that is not an object at all",
        json!("hello?"),
    ));

    samples
}

/// A frame the desktop application sends, and what this build makes of it.
fn client_sample(name: &str, frame: Value) -> Sample {
    let parsed = serde_json::from_value::<ClientMessage>(frame.clone());
    Sample {
        name: name.to_owned(),
        direction: "client".to_owned(),
        frame,
        parses: parsed.is_ok(),
        discriminant: parsed.ok().as_ref().map(client_discriminant),
    }
}

/// A frame the recorder sends, and what this build makes of it.
fn server_sample(name: &str, frame: Value) -> Sample {
    let parsed = serde_json::from_value::<ServerMessage>(frame.clone());
    Sample {
        name: name.to_owned(),
        direction: "server".to_owned(),
        frame,
        parses: parsed.is_ok(),
        discriminant: parsed.ok().as_ref().map(server_discriminant),
    }
}

/// What a client message turned out to be, as a dotted path of wire strings.
fn client_discriminant(message: &ClientMessage) -> String {
    match message {
        // The streams a handshake asked for are part of the path, and carry the
        // names this build does not recognise. Without them the sample of a
        // stream invented later would prove nothing: a mirror that quietly
        // dropped the unfamiliar name — rather than keeping it, which is what
        // the recorder does so it can refuse it *by* name — would reach exactly
        // the same discriminant as one that kept it.
        ClientMessage::Hello(hello) => {
            let role = hello.role.as_str();
            let streams: Vec<&str> = hello.streams.iter().map(EventStream::as_str).collect();
            if streams.is_empty() {
                format!("hello.{role}")
            } else {
                format!("hello.{role}.{}", streams.join("+"))
            }
        }
        ClientMessage::Request(request) => format!("request.{}", request.command),
    }
}

/// What a server message turned out to be, as a dotted path of wire strings.
fn server_discriminant(message: &ServerMessage) -> String {
    match message {
        ServerMessage::Welcome(welcome) => format!("welcome.{}", welcome.role.as_str()),
        ServerMessage::Refused(error) => format!("refused.{}", error_discriminant(error)),
        ServerMessage::Response(response) => match &response.outcome {
            Outcome::Ok(reply) => format!("response.ok.{}", reply_discriminant(reply)),
            Outcome::Error(error) => format!("response.error.{}", error_discriminant(error)),
        },
        ServerMessage::Event(event) => format!("event.{}", event_discriminant(event)),
    }
}

/// What a refusal turned out to be: its code, and the detail under it.
///
/// The detail is part of the path so that a mirror which quietly dropped one it
/// did not recognise — rather than keeping it as the catch-all — says something
/// different here.
fn error_discriminant(error: &ProtocolError) -> String {
    match &error.detail {
        None => error.code.as_str().to_owned(),
        Some(detail) => format!(
            "{}.{}",
            error.code.as_str(),
            detail_tag(detail).unwrap_or_else(|| "unrecognised".to_owned())
        ),
    }
}

/// What a reply turned out to be, including the wire strings underneath it.
fn reply_discriminant(reply: &Reply) -> String {
    match reply {
        Reply::Pong => "pong".to_owned(),
        Reply::Status { status } => format!("status.{}", state_tag(status)),
        Reply::RecordingStarted { .. } => "recording_started".to_owned(),
        Reply::RecordingStopped { summary } => {
            format!("recording_stopped.{}", summary.end_reason.as_str())
        }
        Reply::BookmarkAdded { .. } => "bookmark_added".to_owned(),
        Reply::ScreenshotTaken { .. } => "screenshot_taken".to_owned(),
        Reply::ReplaySaved { .. } => "replay_saved".to_owned(),
        // One discriminant, unlike `library_sessions` below: an empty lane is
        // not a different *shape*, it is the same shape carrying nothing. What
        // tells "none" from "not asked" is that `marks` is always present, and
        // that is a property of the type rather than of the path.
        Reply::LibraryEvents { .. } => "library_events".to_owned(),
        Reply::LibraryTrash { .. } => "library_trash".to_owned(),
        Reply::Favourited { .. } => "favourited".to_owned(),
        Reply::Locked { .. } => "locked".to_owned(),
        Reply::Restored { .. } => "restored".to_owned(),
        Reply::TrashEmptied { .. } => "trash_emptied".to_owned(),
        Reply::Plugins { .. } => "plugins".to_owned(),
        // Whether a copy had to be made is part of the path: it is the
        // difference between an answer that cost nothing and one that read the
        // whole recording, and a mirror that dropped `prepared` would otherwise
        // reach the same discriminant for both.
        Reply::PlaybackOpened { playback } => format!(
            "playback_opened.{}",
            if playback.prepared {
                "prepared"
            } else {
                "as_recorded"
            }
        ),
        // Whether the page ends the library is part of the path, for the reason
        // `shutting_down`'s finalising is: a mirror that dropped the cursor
        // would otherwise reach the same discriminant for a page that continues
        // and a page that does not, and paging is the whole of what this reply
        // is for.
        Reply::LibrarySessions { page } => match &page.next_cursor {
            None => "library_sessions".to_owned(),
            Some(_) => "library_sessions.more".to_owned(),
        },
        Reply::LibraryGames { .. } => "library_games".to_owned(),
        Reply::Hotkeys { .. } => "hotkeys".to_owned(),
        // Whether the copy is complete is part of the path, because it is the
        // one thing a window has to say differently: a mirror that dropped
        // `lossless` would reach the same discriminant for an MP4 that holds
        // the whole recording and one that quietly does not.
        Reply::RecordingExported { export } => match export.lossless {
            true => "recording_exported".to_owned(),
            false => "recording_exported.lossy".to_owned(),
        },
        // Whether a recording is being finished is the whole of what this reply
        // says, so it is part of the path: a mirror that dropped the field would
        // otherwise reach the same discriminant either way.
        Reply::ShuttingDown { finalising: None } => "shutting_down".to_owned(),
        Reply::ShuttingDown {
            finalising: Some(_),
        } => "shutting_down.finalising".to_owned(),
    }
}

/// What an event turned out to be.
fn event_discriminant(event: &Event) -> String {
    match event {
        Event::StatusChanged { status } => format!("status_changed.{}", state_tag(status)),
        // The reason a sitting ended is part of the path, because it is the one
        // thing this event says that a window shows differently — and a mirror
        // that dropped a reason it did not recognise would otherwise reach the
        // same answer as one that kept it.
        Event::SessionEnded { session } => format!(
            "session_ended.{}",
            session.end_reason.as_deref().unwrap_or("unstated")
        ),
        Event::RecordingFailed { .. } => "recording_failed".to_owned(),
        // Deliberately not the tag it arrived with: this build cannot know
        // whether two events it has never heard of are the same kind of thing.
        Event::Other(_) => "unrecognised".to_owned(),
    }
}

/// The fields of one object on the wire, and which of them may be left out.
///
/// The optionality is `serde`'s own answer rather than a reading of the
/// attributes: each field is removed in turn and the result offered back to the
/// deserialiser. A field whose absence still produces the same variant is
/// optional; one whose absence fails, or lands somewhere else, is required.
///
/// `tags` names the fields that decide which variant this is. They are required
/// by construction and are never probed, because removing one from a type with
/// an untagged catch-all — [`Event`], [`ErrorDetail`] — does not fail: it lands
/// in the catch-all, which would report the tag as optional.
fn structure_of<T>(value: &T, tags: &[&str]) -> Structure
where
    T: Serialize + DeserializeOwned,
{
    let serialised = serde_json::to_value(value).expect("a protocol type serialises");
    let object = serialised
        .as_object()
        .expect("a protocol structure is a JSON object")
        .clone();
    let variant = mem::discriminant(value);

    let mut required = Vec::new();
    let mut optional = Vec::new();
    for field in object.keys() {
        if tags.contains(&field.as_str()) {
            required.push(field.clone());
            continue;
        }

        let mut without = object.clone();
        without.remove(field);
        let survives = serde_json::from_value::<T>(Value::Object(without))
            .is_ok_and(|parsed| mem::discriminant(&parsed) == variant);

        if survives {
            optional.push(field.clone());
        } else {
            required.push(field.clone());
        }
    }

    Structure { required, optional }
}

/// The `type` an envelope carries.
fn envelope_type<T: Serialize>(message: &T) -> String {
    tag_of(message, "type")
}

/// The value of one tag field of a serialised value.
fn tag_of<T: Serialize>(value: &T, tag: &str) -> String {
    serde_json::to_value(value)
        .expect("a protocol type serialises")
        .get(tag)
        .and_then(Value::as_str)
        .expect("a tagged protocol type serialises its tag as a string")
        .to_owned()
}

/// The `state` of a recorder status.
fn state_tag(status: &RecorderStatus) -> String {
    tag_of(status, "state")
}

/// The `reply` of a reply.
fn reply_tag(reply: &Reply) -> String {
    tag_of(reply, "reply")
}

/// The single key an outcome is carried under.
fn outcome_tag(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ok(_) => "ok".to_owned(),
        Outcome::Error(_) => "error".to_owned(),
    }
}

/// The `event` of an event, or [`None`] for one this build did not recognise.
fn event_tag(event: &Event) -> Option<String> {
    match event {
        Event::StatusChanged { .. }
        | Event::SessionEnded { .. }
        | Event::RecordingFailed { .. } => Some(tag_of(event, "event")),
        Event::Other(_) => None,
    }
}

/// The `detail` of an error detail, or [`None`] for one this build did not
/// recognise.
fn detail_tag(detail: &ErrorDetail) -> Option<String> {
    match detail {
        ErrorDetail::UnsupportedProtocolVersion { .. } | ErrorDetail::NotImplemented { .. } => {
            Some(tag_of(detail, "detail"))
        }
        ErrorDetail::Other(_) => None,
    }
}

/// Who is connecting, in the schema's samples.
fn exemplar_peer() -> PeerIdentity {
    PeerIdentity {
        name: "clipped-desktop".to_owned(),
        version: "0.1.0".to_owned(),
    }
}

/// A control handshake.
fn exemplar_hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        client: exemplar_peer(),
        // Non-empty, or the field is skipped and the schema would not see it.
        streams: vec![EventStream::Status],
        role: ConnectionRole::Control,
    }
}

/// A control handshake accepted.
fn exemplar_welcome() -> Welcome {
    Welcome {
        protocol_version: PROTOCOL_VERSION,
        recorder: PeerIdentity {
            name: "clipped-recorder".to_owned(),
            version: "0.1.0".to_owned(),
        },
        role: ConnectionRole::Control,
        features: every_feature(),
        streams: vec![EventStream::Status],
    }
}

/// A request with parameters.
fn exemplar_request() -> Request {
    Request {
        id: 1,
        command: "stop_recording".to_owned(),
        params: json!({"recording_id": "r-1"}),
    }
}

/// A response.
fn exemplar_response() -> Response {
    Response {
        id: 1,
        outcome: Outcome::Ok(Reply::Pong),
    }
}

/// A refusal carrying a detail.
///
/// Taken from [`UnbuiltCommand`] rather than typed out, so the example in the
/// schema is a refusal the recorder really sends and cannot drift from one as
/// the subsystems behind these commands are built.
fn exemplar_error() -> ProtocolError {
    UnbuiltCommand::ApplySettings.refusal()
}

/// Every `start_recording` option at once.
fn exemplar_start_recording() -> StartRecording {
    StartRecording {
        window: Some("Counter-Strike".to_owned()),
        process: Some("cs2.exe".to_owned()),
        pid: Some(4242),
        output: Some(r"D:\clips\session.mkv".to_owned()),
        overwrite: false,
        resolution: Some("source".to_owned()),
        framerate: Some(60),
        codec: Some("auto".to_owned()),
        encoder: Some("auto".to_owned()),
        microphone: Some("none".to_owned()),
        system_audio: Some("none".to_owned()),
        replay_seconds: Some(60),
    }
}

/// A recording in progress.
fn exemplar_recording() -> RecorderStatus {
    RecorderStatus::Recording(exemplar_active_recording())
}

/// A recorder watching for a game, in a sitting waiting out its restart grace.
///
/// The sitting is present so that [`structure_of`] sees the field at all: an
/// exemplar with nothing in it would describe a `watching` status as having no
/// fields, and the TypeScript mirror would be checked against that.
fn exemplar_watching() -> RecorderStatus {
    RecorderStatus::Watching(Watching {
        session: Some(Box::new(exemplar_session())),
    })
}

/// The recording itself, without the state tag around it.
fn exemplar_active_recording() -> ActiveRecording {
    ActiveRecording {
        recording_id: "r-1".to_owned(),
        output: r"D:\clips\cs2-20260811-201400-02.mkv".to_owned(),
        target: "process `cs2.exe`".to_owned(),
        elapsed_ms: 4_200,
        replay_seconds: Some(60),
        session: Some(Box::new(exemplar_session())),
    }
}

/// A sitting that is still going, with one file finished and one being written.
fn exemplar_session() -> SessionSummary {
    SessionSummary {
        session_id: "cs2-20260811-201400".to_owned(),
        game_id: Some("cs2".to_owned()),
        game_name: Some("Counter-Strike 2".to_owned()),
        started_at: "2026-08-11T20:14:00+01:00".to_owned(),
        ended_at: None,
        end_reason: None,
        recordings: vec![
            exemplar_session_recording(),
            SessionRecording {
                session_index: 2,
                output: r"D:\clips\cs2-20260811-201400-02.mkv".to_owned(),
                outcome: None,
                duration_ms: None,
            },
        ],
    }
}

/// The same sitting, after it ended.
///
/// This is the one the `session_summary` structure is derived from, because it
/// is the one carrying every field: `ended_at` and `end_reason` are absent from
/// a sitting that is still going, and a structure derived from that one would
/// not mention them.
fn exemplar_ended_session() -> SessionSummary {
    SessionSummary {
        ended_at: Some("2026-08-11T22:03:00+01:00".to_owned()),
        end_reason: Some("game-exited".to_owned()),
        recordings: vec![
            exemplar_session_recording(),
            SessionRecording {
                session_index: 2,
                output: r"D:\clips\cs2-20260811-201400-02.mkv".to_owned(),
                outcome: Some("recorded".to_owned()),
                duration_ms: Some(1_140_000),
            },
        ],
        ..exemplar_session()
    }
}

/// One finished file of a sitting, with everything one can carry.
fn exemplar_session_recording() -> SessionRecording {
    SessionRecording {
        session_index: 1,
        output: r"D:\clips\cs2-20260811-201400-01.mkv".to_owned(),
        outcome: Some("recorded".to_owned()),
        duration_ms: Some(1_800_000),
    }
}

/// A finished recording.
fn exemplar_summary() -> RecordingSummary {
    RecordingSummary {
        output: r"D:\clips\session.mkv".to_owned(),
        duration_ms: 3_800,
        end_reason: EndReason::Stopped,
        frames_encoded: 115,
        frames_skipped_for_rate: 0,
        frames_dropped_writer_behind: 0,
        sustained_framerate: Some(30.0),
        encoder: "nvenc".to_owned(),
        codec: "av1".to_owned(),
        width: 1280,
        height: 720,
    }
}

/// Every `add_bookmark` parameter at once.
fn exemplar_add_bookmark() -> crate::command::AddBookmark {
    crate::command::AddBookmark {
        recording_id: Some("r-1".to_owned()),
        label: Some("triple kill on mid".to_owned()),
        colour: Some("#ffcc00".to_owned()),
        duration_seconds: Some(12.5),
        lead_seconds: Some(5.0),
    }
}

/// Every `take_screenshot` parameter at once.
fn exemplar_take_screenshot() -> crate::command::TakeScreenshot {
    crate::command::TakeScreenshot {
        recording_id: Some("r-1".to_owned()),
        window: Some("Counter-Strike".to_owned()),
        process: Some("cs2.exe".to_owned()),
        pid: Some(4242),
        format: Some("png".to_owned()),
    }
}

/// Every `save_replay` parameter at once.
fn exemplar_save_replay() -> crate::command::SaveReplay {
    crate::command::SaveReplay {
        recording_id: Some("r-1".to_owned()),
        duration_seconds: Some(30.0),
        output: Some(r"D:\clipsce on mirage.mkv".to_owned()),
    }
}

/// A saved replay clip, short of what was asked for so that every field of the
/// shortfall is in the schema.
fn exemplar_replay() -> crate::status::ReplaySummary {
    crate::status::ReplaySummary {
        path: r"D:\clips\clipped-cs2-20260813-201400-replay-1.mkv".to_owned(),
        recording_id: "r-1".to_owned(),
        requested_seconds: 30.0,
        duration_seconds: 21.983,
        source_start_seconds: 8.017,
        source_end_seconds: 30.0,
        leading_slack_seconds: 1.983,
        complete: false,
        shortfall_seconds: 10.0,
        bytes: 51_204_112,
    }
}

/// A screenshot, with every optional field present so the schema sees them.
fn exemplar_screenshot() -> crate::status::ScreenshotSummary {
    crate::status::ScreenshotSummary {
        path: r"C:\Users\player\Pictures\Clipped\clipped-cs2-20260811-143205.png".to_owned(),
        format: "png".to_owned(),
        width: 2560,
        height: 1440,
        bytes: 4_812_009,
        recording_id: Some("r-1".to_owned()),
        at_seconds: Some(115.0),
    }
}

/// Every `library_sessions` parameter at once.
fn exemplar_library_sessions() -> LibrarySessions {
    LibrarySessions {
        limit: Some(25),
        after: Some("2026-08-11T20:14:00+01:00|cs2-20260811-201400".to_owned()),
        query: Some("game:cs2 tag:clutch".to_owned()),
    }
}

/// A page of the library, with every optional field present so the schema sees
/// them.
fn exemplar_library_page() -> LibrarySessionPage {
    LibrarySessionPage {
        sessions: vec![exemplar_library_session()],
        next_cursor: Some("2026-08-11T20:14:00+01:00|cs2-20260811-201400".to_owned()),
    }
}

/// A sitting, with every optional field present so the schema sees them.
fn exemplar_library_session() -> LibrarySession {
    LibrarySession {
        session_id: "cs2-20260811-201400".to_owned(),
        game_id: Some("cs2".to_owned()),
        game_name: Some("Counter-Strike 2".to_owned()),
        started_at: "2026-08-11T20:14:00+01:00".to_owned(),
        ended_at: Some("2026-08-11T22:03:00+01:00".to_owned()),
        end_reason: Some("game-exited".to_owned()),
        favourite: true,
        locked: true,
        recordings: vec![exemplar_library_recording()],
        clips: vec![exemplar_library_clip()],
    }
}

/// A recording in the library, with every optional field present.
///
/// `missing_since` among them: the field the whole read exists for has to be in
/// the schema, or a TypeScript mirror could leave it out and the check would
/// still pass.
fn exemplar_library_recording() -> LibraryRecording {
    LibraryRecording {
        recording_id: 12,
        session_index: 1,
        path: r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
        started_at: "2026-08-11T20:14:00+01:00".to_owned(),
        ended_at: Some("2026-08-11T22:03:00+01:00".to_owned()),
        outcome: Some("recorded".to_owned()),
        end_reason: Some("target-lost".to_owned()),
        duration_seconds: Some(6_540.0),
        width: Some(2560),
        height: Some(1440),
        size_bytes: Some(9_812_009_112),
        missing_since: Some("2026-08-12T09:00:00+01:00".to_owned()),
        favourite: false,
        // Not locked itself, and protected anyway: the exemplar carries the
        // case that separates the two fields, because a pair that always
        // agreed would not need to be a pair.
        locked: false,
        protected: true,
        tags: vec!["clutch".to_owned()],
    }
}

/// A clip in the library, with every optional field present.
fn exemplar_library_clip() -> LibraryClip {
    LibraryClip {
        clip_id: 3,
        path: r"D:\clips\ace-on-mirage.mkv".to_owned(),
        title: Some("Ace on Mirage".to_owned()),
        created_at: "2026-08-11T21:02:00+01:00".to_owned(),
        duration_seconds: Some(30.0),
        size_bytes: Some(48_120_091),
        missing_since: Some("2026-08-12T09:00:00+01:00".to_owned()),
        favourite: true,
        tags: vec!["clutch".to_owned()],
    }
}

/// What the library holds for one game, with every optional field present.
fn exemplar_library_game() -> LibraryGame {
    LibraryGame {
        game_id: Some("cs2".to_owned()),
        name: Some("Counter-Strike 2".to_owned()),
        first_seen_at: Some("2026-01-04T19:30:00+00:00".to_owned()),
        last_played_at: Some("2026-08-11T20:14:00+01:00".to_owned()),
        sessions: 214,
        recordings: 265,
        clips: 31,
        favourites: 12,
        bytes: 411_204_889_112,
        missing: 3,
    }
}

/// Every `export_recording` parameter at once.
fn exemplar_export_recording() -> ExportRecording {
    ExportRecording {
        source: r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
        destination: r"D:\clips\cs2-20260811-201400-1.mp4".to_owned(),
    }
}

/// Every `open_playback` parameter at once.
fn exemplar_open_playback() -> OpenPlayback {
    OpenPlayback {
        source: r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
        audio_track: Some(3),
    }
}

/// A recording opened for playback, with every optional field present so the
/// schema sees them.
fn exemplar_playback() -> PlaybackStream {
    PlaybackStream {
        path: r"C:\Users\sam\AppData\Local\Clipped\playback\cs2-20260811-201400-1-3.mp4".to_owned(),
        audio_track: Some(3),
        audio_tracks: vec![exemplar_playback_track()],
        prepared: true,
    }
}

/// One sound track a window can offer, with every optional field present.
fn exemplar_playback_track() -> PlaybackTrack {
    PlaybackTrack {
        index: 3,
        name: Some("Microphone".to_owned()),
        language: Some("eng".to_owned()),
        default: false,
    }
}

/// A finished export, with every optional field present so the schema sees
/// them.
fn exemplar_export() -> ExportSummary {
    ExportSummary {
        source: r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
        destination: r"D:\clips\cs2-20260811-201400-1.mp4".to_owned(),
        duration_ms: 6_540_000,
        packets: 588_120,
        bytes: 9_811_204_112,
        elapsed_ms: 4_182,
        lossless: false,
        losses: vec!["4 chapter marks are not carried into MP4".to_owned()],
    }
}

/// A bookmark, with every optional field present so the schema sees them.
fn exemplar_bookmark() -> crate::status::BookmarkSummary {
    crate::status::BookmarkSummary {
        recording_id: "r-1".to_owned(),
        at_seconds: 115.0,
        pressed_at_seconds: 120.0,
        lead_seconds: 5.0,
        label: Some("triple kill on mid".to_owned()),
        colour: Some("#ffcc00".to_owned()),
        duration_seconds: Some(12.5),
        bookmarks_file: r"D:\clips\session.bookmarks.json".to_owned(),
        bookmarks_in_recording: 3,
    }
}

// Every `every_*` function below exists to make its list exhaustive. The `match`
// in each one is a no-op at run time and a compile error the moment a variant is
// added and not named, which is what keeps this schema — and through it the
// TypeScript mirror — from silently describing a protocol that has moved on.
// Each `match` sits with the list it belongs to, because the arm the compiler
// demands and the entry the schema needs are the same decision; the tests below
// then ask `serde` for its own list wherever there is one to ask for.

/// Every envelope the desktop application sends.
fn every_client_message() -> Vec<ClientMessage> {
    let messages = vec![
        ClientMessage::Hello(exemplar_hello()),
        ClientMessage::Request(exemplar_request()),
    ];
    for message in &messages {
        match message {
            ClientMessage::Hello(_) | ClientMessage::Request(_) => {}
        }
    }
    messages
}

/// Every envelope the recorder sends.
fn every_server_message() -> Vec<ServerMessage> {
    let messages = vec![
        ServerMessage::Welcome(exemplar_welcome()),
        ServerMessage::Refused(exemplar_error()),
        ServerMessage::Response(exemplar_response()),
        ServerMessage::Event(Event::StatusChanged {
            status: RecorderStatus::Idle,
        }),
    ];
    for message in &messages {
        match message {
            ServerMessage::Welcome(_)
            | ServerMessage::Refused(_)
            | ServerMessage::Response(_)
            | ServerMessage::Event(_) => {}
        }
    }
    messages
}

/// Every role a connection can declare, less the placeholder this build turns
/// one it does not have into.
fn every_connection_role() -> Vec<ConnectionRole> {
    for role in [
        ConnectionRole::Control,
        ConnectionRole::Events,
        ConnectionRole::Unknown,
    ] {
        match role {
            ConnectionRole::Control | ConnectionRole::Events | ConnectionRole::Unknown => {}
        }
    }
    vec![ConnectionRole::Control, ConnectionRole::Events]
}

/// Every event stream the protocol defines, including the one this build
/// refuses.
fn every_event_stream() -> Vec<EventStream> {
    let streams = vec![
        EventStream::Status,
        EventStream::Errors,
        EventStream::Metrics,
    ];
    for stream in &streams {
        match stream {
            EventStream::Status
            | EventStream::Errors
            | EventStream::Metrics
            | EventStream::Other(_) => {}
        }
    }
    streams
}

/// Every capability the protocol defines.
///
/// [`features::ALL`] is generated from the constants themselves, so this is the
/// definitions rather than a copy of them: a capability added to [`features`]
/// changes this schema, and the TypeScript check then has something to fail
/// against.
fn every_feature() -> Vec<String> {
    features::ALL
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

/// Every command this build performs.
fn every_built_command() -> Vec<Command> {
    let commands = vec![
        Command::Ping,
        Command::GetStatus,
        Command::StartRecording(exemplar_start_recording()),
        Command::StopRecording(StopRecording::default()),
        Command::AddBookmark(exemplar_add_bookmark()),
        Command::TakeScreenshot(exemplar_take_screenshot()),
        Command::SaveReplay(exemplar_save_replay()),
        Command::LibrarySessions(exemplar_library_sessions()),
        Command::LibraryGames,
        Command::LibraryEvents(crate::library::LibraryEvents {
            recording: "1".to_owned(),
        }),
        Command::LibraryTrash(crate::library::LibraryTrash {}),
        Command::RestoreFromTrash(crate::library::RestoreFromTrash {
            kind: "recording".to_owned(),
            id: 1,
        }),
        Command::EmptyTrash(crate::library::EmptyTrash {
            items: 1,
            bytes: 2_147_483_648,
        }),
        Command::SetFavourite(crate::library::SetFavourite {
            kind: "recording".to_owned(),
            session_id: String::new(),
            id: 1,
            favourite: true,
        }),
        Command::SetLock(crate::library::SetLock {
            kind: "recording".to_owned(),
            session_id: String::new(),
            id: 1,
            locked: true,
        }),
        Command::Plugins,
        Command::ExportRecording(exemplar_export_recording()),
        Command::OpenPlayback(exemplar_open_playback()),
        Command::GetHotkeys,
        Command::Shutdown(Shutdown::default()),
    ];
    for command in &commands {
        match command {
            Command::Ping
            | Command::GetStatus
            | Command::StartRecording(_)
            | Command::StopRecording(_)
            | Command::AddBookmark(_)
            | Command::TakeScreenshot(_)
            | Command::SaveReplay(_)
            | Command::LibrarySessions(_)
            | Command::LibraryGames
            | Command::LibraryEvents(_)
            | Command::LibraryTrash(_)
            | Command::RestoreFromTrash(_)
            | Command::EmptyTrash(_)
            | Command::SetFavourite(_)
            | Command::SetLock(_)
            | Command::Plugins
            | Command::ExportRecording(_)
            | Command::OpenPlayback(_)
            | Command::GetHotkeys
            | Command::Shutdown(_)
            | Command::Unbuilt(_) => {}
        }
    }
    commands
}

/// Every refusal code this build knows.
fn every_error_code() -> Vec<ErrorCode> {
    let codes = vec![
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
    for code in &codes {
        match code {
            ErrorCode::UnsupportedProtocolVersion
            | ErrorCode::HandshakeRequired
            | ErrorCode::MalformedFrame
            | ErrorCode::UnknownCommand
            | ErrorCode::InvalidParameters
            | ErrorCode::NotImplemented
            | ErrorCode::AlreadyRecording
            | ErrorCode::NotRecording
            | ErrorCode::TargetNotFound
            | ErrorCode::TargetNotCapturable
            | ErrorCode::RecordingFailed
            | ErrorCode::TooManyConnections
            | ErrorCode::ShuttingDown
            | ErrorCode::DestinationExists
            | ErrorCode::ExportFailed
            | ErrorCode::PlaybackFailed
            | ErrorCode::LibraryUnavailable
            | ErrorCode::Internal
            | ErrorCode::Other(_) => {}
        }
    }
    codes
}

/// Every machine-readable detail this build knows.
fn every_error_detail() -> Vec<ErrorDetail> {
    let details = vec![
        ErrorDetail::UnsupportedProtocolVersion {
            requested: 2,
            supported: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
            recorder_version: "0.1.0".to_owned(),
        },
        ErrorDetail::NotImplemented {
            subsystem: UnbuiltCommand::ApplySettings.subsystem().to_owned(),
            milestone: UnbuiltCommand::ApplySettings.milestone().to_owned(),
            tracking_issue: UnbuiltCommand::ApplySettings.tracking_issue(),
        },
    ];
    for detail in &details {
        match detail {
            ErrorDetail::UnsupportedProtocolVersion { .. }
            | ErrorDetail::NotImplemented { .. }
            | ErrorDetail::Other(_) => {}
        }
    }
    details
}

/// Every reason a recording can end that this build knows.
fn every_end_reason() -> Vec<EndReason> {
    let reasons = vec![
        EndReason::Stopped,
        EndReason::TargetLost,
        EndReason::TargetResized,
    ];
    for reason in &reasons {
        match reason {
            EndReason::Stopped
            | EndReason::TargetLost
            | EndReason::TargetResized
            | EndReason::Other(_) => {}
        }
    }
    reasons
}

/// Every event this build knows.
fn every_event() -> Vec<Event> {
    let events = vec![
        Event::StatusChanged {
            status: RecorderStatus::Idle,
        },
        Event::SessionEnded {
            session: exemplar_ended_session(),
        },
        Event::RecordingFailed {
            recording_id: "r-1".to_owned(),
            error: ProtocolError::new(
                ErrorCode::RecordingFailed,
                "the encoder stopped accepting frames",
            ),
        },
    ];
    for event in &events {
        match event {
            Event::StatusChanged { .. }
            | Event::SessionEnded { .. }
            | Event::RecordingFailed { .. }
            | Event::Other(_) => {}
        }
    }
    events
}

/// Both halves of a response.
fn every_outcome() -> Vec<Outcome> {
    let outcomes = vec![
        Outcome::Ok(Reply::Pong),
        Outcome::Error(ProtocolError::new(ErrorCode::Internal, "something")),
    ];
    for outcome in &outcomes {
        match outcome {
            Outcome::Ok(_) | Outcome::Error(_) => {}
        }
    }
    outcomes
}

/// Every reply this build produces.
fn every_reply() -> Vec<Reply> {
    let replies = vec![
        Reply::Pong,
        Reply::Status {
            status: RecorderStatus::Idle,
        },
        Reply::RecordingStarted {
            recording_id: "r-1".to_owned(),
            output: r"D:\clips\session.mkv".to_owned(),
        },
        Reply::RecordingStopped {
            summary: exemplar_summary(),
        },
        Reply::BookmarkAdded {
            bookmark: exemplar_bookmark(),
        },
        Reply::ScreenshotTaken {
            screenshot: exemplar_screenshot(),
        },
        Reply::ReplaySaved {
            clip: exemplar_replay(),
        },
        Reply::LibrarySessions {
            page: exemplar_library_page(),
        },
        Reply::LibraryGames {
            games: vec![exemplar_library_game()],
        },
        Reply::RecordingExported {
            export: exemplar_export(),
        },
        Reply::PlaybackOpened {
            playback: exemplar_playback(),
        },
        Reply::Hotkeys {
            hotkeys: every_hotkey_state()
                .into_iter()
                .map(|state| HotkeyBinding {
                    state,
                    ..exemplar_hotkey_binding()
                })
                .collect(),
        },
        Reply::ShuttingDown {
            // `Some`, or the field is skipped and the schema would not see it.
            finalising: Some(exemplar_active_recording()),
        },
        Reply::LibraryEvents {
            lane: crate::library::LibraryEventLane {
                marks: vec![crate::library::LibraryEventMark {
                    recording: "1".to_owned(),
                    at: 4_000_000_000,
                    kind: "kill".to_owned(),
                    source: "counter-strike-2".to_owned(),
                }],
            },
        },
        Reply::LibraryTrash {
            trash: crate::library::TrashListing {
                items: vec![crate::library::TrashedItem {
                    kind: "recording".to_owned(),
                    id: 1,
                    path: r"D:\Clips.trash\clipped-cs2-20260814-201500.mkv".to_owned(),
                    original_path: r"D:\Clips\clipped-cs2-20260814-201500.mkv".to_owned(),
                    deleted_at: "2026-08-15T09:00:00+01:00".to_owned(),
                    // `Some`, or the field is skipped and the schema would not
                    // see it.
                    expires_at: Some("2026-09-14T09:00:00+01:00".to_owned()),
                    size_bytes: Some(2_147_483_648),
                    dependent_clips: 2,
                }],
                total_items: 1,
                total_bytes: 2_147_483_648,
                directory: r"D:\Clips.trash".to_owned(),
            },
        },
        Reply::Restored {
            restored: crate::library::RestoredItem {
                kind: "recording".to_owned(),
                id: 1,
                path: r"D:\\Clips\\clipped-cs2-20260814-201500.mkv".to_owned(),
                file_restored: true,
                renamed: false,
            },
        },
        Reply::TrashEmptied {
            emptied: crate::library::TrashEmptied {
                removed: 1,
                reclaimed_bytes: 2_147_483_648,
                // Non-empty, or the field is a list nothing proves can carry
                // anything.
                refused: vec![
                    r"D:\\Clips.trash\\locked.mkv: the file is open in another program".to_owned(),
                ],
            },
        },
        Reply::Favourited {
            mark: crate::library::FavouriteMark {
                kind: "recording".to_owned(),
                session_id: String::new(),
                id: 1,
                favourite: true,
                changed: true,
            },
        },
        Reply::Locked {
            lock: crate::library::LockMark {
                kind: "recording".to_owned(),
                session_id: String::new(),
                id: 1,
                locked: true,
                protected: true,
                changed: true,
            },
        },
        Reply::Plugins {
            installed: vec![exemplar_plugin()],
            refused: vec![crate::plugins::RefusedPlugin {
                directory: r"C:\ProgramData\Clipped\plugins\half-installed".to_owned(),
                reason: "its manifest is not readable JSON".to_owned(),
            }],
        },
    ];
    for reply in &replies {
        match reply {
            Reply::Pong
            | Reply::Status { .. }
            | Reply::RecordingStarted { .. }
            | Reply::RecordingStopped { .. }
            | Reply::BookmarkAdded { .. }
            | Reply::ScreenshotTaken { .. }
            | Reply::ReplaySaved { .. }
            | Reply::LibrarySessions { .. }
            | Reply::LibraryGames { .. }
            | Reply::LibraryEvents { .. }
            | Reply::LibraryTrash { .. }
            | Reply::Restored { .. }
            | Reply::TrashEmptied { .. }
            | Reply::Favourited { .. }
            | Reply::Locked { .. }
            | Reply::Plugins { .. }
            | Reply::Hotkeys { .. }
            | Reply::RecordingExported { .. }
            | Reply::PlaybackOpened { .. }
            | Reply::ShuttingDown { .. } => {}
        }
    }
    replies
}

/// One installed plugin, for the schema and the samples.
fn exemplar_plugin() -> crate::plugins::PluginDeclaration {
    crate::plugins::PluginDeclaration {
        id: "acme-cs2".to_owned(),
        name: "Counter-Strike 2 highlights".to_owned(),
        version: "0.1.0".to_owned(),
        description: "Reports kills, deaths and rounds from Game State Integration.".to_owned(),
        network: vec![
            "Listens on 127.0.0.1:3212 (this machine only) — receives Counter-Strike 2 game state"
                .to_owned(),
        ],
        enforcement: "Clipped shows what a plugin declares and refuses to start one whose declaration has \n             changed since you allowed it. It cannot yet stop a plugin from using the \n             network in ways it did not declare."
                .to_owned(),
        state: crate::plugins::PluginState::NotEnabled,
    }
}

/// Every state a binding can be in.
fn every_hotkey_state() -> Vec<HotkeyState> {
    let states = vec![
        HotkeyState::Unbound,
        HotkeyState::Registered,
        HotkeyState::Conflict {
            reason: "another application already uses it".to_owned(),
        },
    ];
    for state in &states {
        match state {
            HotkeyState::Unbound | HotkeyState::Registered | HotkeyState::Conflict { .. } => {}
        }
    }
    states
}

/// One row of the hotkey list, with every optional field present so that the
/// schema sees it.
///
/// An action nothing performs, because `unavailable` is only ever sent for one
/// and it is the field the schema would otherwise never see. It is
/// `open_overlay` and no longer `save_replay`: [issue
/// #38](https://github.com/wildware-uk/clipped/issues/38) built the replay
/// buffer, the command and the key, so a schema still shipping that row would
/// describe a recorder nobody builds (AGENTS.md sections 27 and 54).
fn exemplar_hotkey_binding() -> HotkeyBinding {
    HotkeyBinding {
        action: "open_overlay".to_owned(),
        label: "Open overlay".to_owned(),
        hotkey: Some("Ctrl+F12".to_owned()),
        state: HotkeyState::Registered,
        handled: false,
        unavailable: Some(
            "Open overlay is not in this build: the overlay arrives in M5 (issue #53)".to_owned(),
        ),
    }
}

/// The `state` tag of one hotkey state.
fn hotkey_state_tag(state: &HotkeyState) -> String {
    tag_of(state, "state")
}

/// Every state a recorder reports.
fn every_recorder_status() -> Vec<RecorderStatus> {
    let states = vec![
        RecorderStatus::Idle,
        exemplar_watching(),
        exemplar_recording(),
    ];
    for state in &states {
        match state {
            RecorderStatus::Idle | RecorderStatus::Watching(_) | RecorderStatus::Recording(_) => {}
        }
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_describes_every_message_the_protocol_has() {
        let schema = protocol_schema();

        for envelope in ["hello", "request"] {
            assert!(
                schema.envelopes["client"].contains_key(envelope),
                "the client sends a `{envelope}` and the schema does not describe it"
            );
        }
        for envelope in ["welcome", "refused", "response", "event"] {
            assert!(
                schema.envelopes["server"].contains_key(envelope),
                "the recorder sends a `{envelope}` and the schema does not describe it"
            );
        }
        for structure in [
            "hello",
            "welcome",
            "request",
            "response",
            "protocol_error",
            "peer_identity",
            "start_recording",
            "stop_recording",
            "shutdown",
            "recording_summary",
            "active_recording",
            "reply.pong",
            "reply.status",
            "reply.recording_started",
            "reply.recording_stopped",
            "reply.shutting_down",
            "event.status_changed",
            "event.session_ended",
            "event.recording_failed",
            "recorder_status.idle",
            "recorder_status.watching",
            "recorder_status.recording",
            "session_summary",
            "session_recording",
            "library_sessions",
            "library_session_page",
            "library_session",
            "library_recording",
            "library_clip",
            "library_game",
            "reply.library_sessions",
            "reply.library_games",
            "error_detail.unsupported_protocol_version",
            "error_detail.not_implemented",
            "outcome.ok",
            "outcome.error",
        ] {
            assert!(
                schema.structures.contains_key(structure),
                "`{structure}` is on the wire and not in the schema"
            );
        }
    }

    /// A tag no build will ever have, used to make a deserialiser say what it
    /// does have.
    const PROBE: &str = "a-tag-no-build-has";

    /// The variants `serde` itself knows for a closed, tagged enumeration.
    ///
    /// A tagged enumeration with no catch-all answers an unrecognised tag with
    /// the list of the ones it has — "unknown variant `x`, expected one of
    /// `a`, `b`" — and that list is written by the derive macro from the type
    /// definition. So it is the definition's own answer to "which variants are
    /// there", available to a test at run time, which no `match` can give:
    /// exhaustiveness makes the compiler demand an *arm* for a new variant, and
    /// an arm can be added without the exemplar beside it. This is what makes
    /// that omission fail.
    fn variants_the_deserialiser_has<T: DeserializeOwned>(probe: Value) -> Vec<String> {
        let refusal = serde_json::from_value::<T>(probe)
            .err()
            .expect("a tag no build has is not a variant")
            .to_string();

        // Everything `serde` quoted: the tag it was given, and then the
        // variants it expected instead.
        let mut quoted = refusal.split('`').skip(1).step_by(2);
        assert_eq!(
            quoted.next(),
            Some(PROBE),
            "`serde` no longer names the unrecognised tag first in `{refusal}`; this test reads \
             its list of variants out of that sentence and needs re-reading against it"
        );

        let variants: Vec<String> = quoted.map(ToOwned::to_owned).collect();
        assert!(
            !variants.is_empty(),
            "`serde` listed no variants in `{refusal}`"
        );
        variants
    }

    #[test]
    fn a_closed_enumeration_lists_every_variant_the_deserialiser_has() {
        // The `match` in each `every_*` function stops a new variant compiling.
        // This is the other half: that the variant was not merely named in the
        // `match` and left out of the list, which the compiler cannot see and
        // which would leave this schema — and the TypeScript checked against it
        // — describing a protocol the recorder no longer speaks.
        //
        // Only the closed enumerations can be asked. `Event` and `ErrorDetail`
        // have untagged catch-alls, so an unrecognised tag lands in one instead
        // of producing the error that carries the list; they are held by their
        // `match` alone.
        let schema = protocol_schema();
        let listed = |name: &str| {
            let mut values = schema.enumerations[name].values.clone();
            values.sort();
            values
        };
        let sorted = |mut variants: Vec<String>| {
            variants.sort();
            variants
        };

        assert_eq!(
            listed("reply"),
            sorted(variants_the_deserialiser_has::<Reply>(
                json!({"reply": PROBE})
            )),
            "a reply the recorder can produce and this schema does not describe"
        );
        assert_eq!(
            listed("outcome"),
            sorted(variants_the_deserialiser_has::<Outcome>(json!({PROBE: {}}))),
            "a response can end in a way this schema does not describe"
        );
        assert_eq!(
            listed("recorder_state"),
            sorted(variants_the_deserialiser_has::<RecorderStatus>(
                json!({"state": PROBE})
            )),
            "a state the recorder can report and this schema does not describe"
        );
        assert_eq!(
            listed("client_message_type"),
            sorted(variants_the_deserialiser_has::<ClientMessage>(
                json!({"type": PROBE})
            ))
        );
        assert_eq!(
            listed("server_message_type"),
            sorted(variants_the_deserialiser_has::<ServerMessage>(
                json!({"type": PROBE})
            ))
        );
    }

    #[test]
    fn optionality_is_the_deserialisers_answer_and_not_a_reading_of_the_attributes() {
        let schema = protocol_schema();

        let hello = &schema.structures["hello"];
        assert_eq!(hello.required, ["client", "protocol_version"]);
        assert_eq!(
            hello.optional,
            ["role", "streams"],
            "a handshake without a role is a control connection, and without streams asks for none"
        );

        let error = &schema.structures["protocol_error"];
        assert_eq!(error.required, ["code", "message"]);
        assert_eq!(
            error.optional,
            ["detail"],
            "the two fields a refusal must carry are the ones the user is shown"
        );

        assert_eq!(
            schema.structures["start_recording"].required,
            Vec::<String>::new(),
            "every start option has a default, the same ones the command line has"
        );
    }

    #[test]
    fn a_tag_is_never_reported_as_optional_because_a_catch_all_absorbed_it() {
        let schema = protocol_schema();

        // Removing `event` from a `status_changed` does not fail: it lands in
        // `Event::Other`, which would make the tag look optional and let a
        // TypeScript mirror leave it out of the type.
        assert_eq!(
            schema.structures["event.status_changed"].required,
            ["event", "status"]
        );
        assert!(schema.structures["event.status_changed"]
            .optional
            .is_empty());
        assert_eq!(
            schema.structures["error_detail.not_implemented"].required,
            ["detail", "milestone", "subsystem", "tracking_issue"]
        );
    }

    #[test]
    fn the_recorder_state_is_the_one_enumeration_that_does_not_tolerate_a_stranger() {
        let schema = protocol_schema();

        assert!(
            !schema.enumerations["recorder_state"].tolerates_unrecognised,
            "a window showing `idle` for a recorder doing something else is a lie"
        );
        for open in ["error_code", "end_reason", "event", "error_detail"] {
            assert!(
                schema.enumerations[open].tolerates_unrecognised,
                "`{open}` is what an older client meets a newer recorder through"
            );
        }
    }

    #[test]
    fn every_sample_records_what_this_build_actually_did_with_it() {
        let schema = protocol_schema();

        let named = |name: &str| {
            schema
                .samples
                .iter()
                .find(|sample| sample.name == name)
                .unwrap_or_else(|| panic!("the schema has a `{name}` sample"))
        };

        // The compatibility policy, as evidence rather than as a claim.
        let unknown_code = named("an error code invented later");
        assert!(unknown_code.parses);
        assert_eq!(
            unknown_code.discriminant.as_deref(),
            Some("response.error.gpu_on_fire"),
            "an unfamiliar code is kept verbatim, or the message it carried is lost with it"
        );

        let unknown_reason = named("an end reason invented later");
        assert!(unknown_reason.parses);
        assert_eq!(
            unknown_reason.discriminant.as_deref(),
            Some("response.ok.recording_stopped.display_disconnected")
        );

        let unknown_event = named("an event invented later");
        assert!(unknown_event.parses);
        assert_eq!(
            unknown_event.discriminant.as_deref(),
            Some("event.unrecognised")
        );

        // And the asymmetry.
        assert!(
            !named("a recorder state invented later, inside a reply").parses,
            "a state this build cannot read must fail the reply rather than be guessed at"
        );
        assert_eq!(
            named("a recorder state invented later, inside an event")
                .discriminant
                .as_deref(),
            Some("event.unrecognised"),
            "inside an event there is somewhere for it to go, and it is not `idle`"
        );
    }

    #[test]
    fn every_reply_the_recorder_can_send_has_a_sample_carrying_it() {
        // The hole this closes, and it was a real one. `bookmark_added` and
        // `screenshot_taken` were in every list this schema publishes and in no
        // sample, so no frame carrying either ever crossed the conformance
        // check — and `packages/shared/src/ipc/parse.ts` could not read either
        // of them. Both sides agreed about a reply neither could handle.
        //
        // A list is not enough on its own: the samples are what make the
        // TypeScript actually parse a frame. So a reply added without one fails
        // here, and the TypeScript half asserts the same thing from its side.
        let schema = protocol_schema();
        let discriminants: Vec<&str> = schema
            .samples
            .iter()
            .filter_map(|sample| sample.discriminant.as_deref())
            .collect();

        for reply in every_reply() {
            let tag = reply_tag(&reply);
            let prefix = format!("response.ok.{tag}");
            assert!(
                discriminants
                    .iter()
                    .any(|discriminant| *discriminant == prefix
                        || discriminant.starts_with(&format!("{prefix}."))),
                "`{tag}` is a reply this recorder sends and no sample carries one, so nothing \
                 checks that the other end can read it"
            );
        }
    }

    #[test]
    fn every_sample_frame_is_reproducible_from_the_schema_alone() {
        // The TypeScript check reads nothing but this document, so a sample
        // that needed something else to interpret it would not be checkable.
        for sample in protocol_schema().samples {
            assert!(
                ["client", "server"].contains(&sample.direction.as_str()),
                "`{}` has no direction to parse it in",
                sample.name
            );
            assert_eq!(
                sample.parses,
                sample.discriminant.is_some(),
                "`{}` says it parsed and did not say what to",
                sample.name
            );
        }
    }

    #[test]
    fn the_committed_schema_is_the_one_this_build_produces() {
        // The half of the conformance check that runs on this side. The
        // TypeScript check reads the committed file; this one insists the
        // committed file is still what the Rust types say, so that a field
        // added here and not regenerated fails in CI rather than leaving the
        // TypeScript check agreeing with a document nobody updated.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(SCHEMA_PATH);
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));

        let committed: ProtocolSchema = serde_json::from_str(&committed).unwrap_or_else(|error| {
            panic!("{SCHEMA_PATH} is not a schema document: {error}. Run `{REGENERATE_WITH}`.")
        });

        assert_eq!(
            committed,
            protocol_schema(),
            "{SCHEMA_PATH} no longer describes this build's protocol. Run `{REGENERATE_WITH}`, \
             then look at what changed in the TypeScript mirror it is checked against."
        );
    }

    #[test]
    fn the_schema_round_trips_through_json() {
        let schema = protocol_schema();
        let json = serde_json::to_string(&schema).expect("the schema serialises");
        assert_eq!(
            serde_json::from_str::<ProtocolSchema>(&json).expect("and deserialises"),
            schema
        );
    }
}
