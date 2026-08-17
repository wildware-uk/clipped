//! The envelopes, the handshake and the compatibility policy they implement.
//!
//! Every frame on this protocol is one [`ClientMessage`] or one
//! [`ServerMessage`], both tagged by a `type` field so that a reader never has
//! to infer what it is holding from which fields happen to be present.
//!
//! # The handshake, and why there is one
//!
//! The recorder and the desktop application are separate processes with
//! separate lifetimes and separate update schedules ([ADR
//! 0002](../../../docs/adr/0002-separate-recorder-process.md)). A recorder that
//! started at login three weeks ago and a UI the user updated this morning is
//! not an edge case, it is Tuesday. So the first frame on every connection is a
//! [`Hello`] stating the version the client speaks, and the recorder either
//! accepts it with a [`Welcome`] or refuses it with a
//! [`ServerMessage::Refused`] that says what it speaks instead.
//!
//! The alternative — start sending commands and find out through a
//! deserialisation failure — was never on the table: it produces "malformed
//! frame" for what is actually "you need to update", and it produces it after
//! the UI has already told the user everything is fine.
//!
//! [`Hello`] and the refusal are therefore **frozen**. Whatever else changes,
//! those two shapes must stay readable by every version, in both directions,
//! or the mechanism that reports incompatibility becomes the thing that is
//! incompatible.
//!
//! # The compatibility policy
//!
//! Stated fully in `docs/ipc.md`; the part the types enforce:
//!
//! - An **unknown version** is refused. Not downgraded, not attempted. The
//!   refusal carries every version the recorder does speak, so the client can
//!   say which side is behind.
//! - An **unknown field** inside a known version is ignored. This is what makes
//!   a version bump rare: adding a field to an event or a reply does not break
//!   a client that has never heard of it.
//! - An **unknown command** is refused by name
//!   ([`ErrorCode::UnknownCommand`](crate::ErrorCode::UnknownCommand)), not
//!   treated as a corrupt frame, so a newer UI asking an older recorder for
//!   something learns exactly that.
//! - Because unknown fields are ignored, a **behavioural change to an existing
//!   command may never arrive as a new optional field**. An older recorder
//!   would drop it and report success for something it did not do. Such a
//!   change is either a new command name or a new [`Welcome::features`] entry
//!   the client checks first.
//!
//! # Roles
//!
//! A connection declares in its [`Hello`] whether it carries commands
//! ([`ConnectionRole::Control`]) or events ([`ConnectionRole::Events`]), and it
//! never carries both. A control connection is strictly request then response;
//! an event connection is written to by the recorder and never read from again.
//!
//! That is a transport decision showing through: a synchronous Windows file
//! handle serialises the operations issued against it, so a recorder that
//! wanted to push an event down a connection while a read was outstanding on
//! the same handle would need overlapped I/O and a completion loop to do it
//! safely. Two connections, each used in one direction at a time, buy the same
//! thing for a fraction of the machinery. `docs/ipc.md` records the trade.

use serde::{Deserialize, Serialize};

use crate::command::Reply;
use crate::error::ProtocolError;
use crate::status::RecorderStatus;

/// The protocol version this build speaks.
///
/// Incremented **only** for a change that an existing client could not survive:
/// a field removed or given a new meaning, a command's parameters changed, a
/// reply restructured. Adding a command, a field, an event, an error code or a
/// feature does not touch it (AGENTS.md section 43).
pub const PROTOCOL_VERSION: u32 = 1;

/// Every version this build will accept from a client.
///
/// A recorder may speak more than one version at once, which is how a breaking
/// change is deployed without requiring the two processes to be updated in the
/// same instant. Today there has only ever been one version, so there is one
/// entry.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[PROTOCOL_VERSION];

/// The names of the capabilities a recorder can advertise in
/// [`Welcome::features`].
///
/// A feature is the honest answer to "can this build actually do the thing",
/// which a version number is not: two recorders speaking protocol 1 can differ
/// in what is compiled into them. A UI decides whether to *offer* a control by
/// asking here, so that it never presents a button whose command will be
/// refused (AGENTS.md section 27).
pub mod features {
    /// Names the capabilities, and the list of all of them.
    ///
    /// Unlike the rest of the protocol's vocabularies a feature is a constant
    /// rather than a variant, so there is no `match` a new one can fail to
    /// compile against. This macro is what stands in for that: [`ALL`] is
    /// generated from the same lines that define the constants, so a capability
    /// added here reaches `schema.rs` — and through it the TypeScript mirror
    /// and its conformance check — without anybody remembering to list it
    /// twice.
    macro_rules! capabilities {
        ($($(#[$description:meta])* $name:ident = $wire:literal;)+) => {
            $($(#[$description])* pub const $name: &str = $wire;)+

            /// Every capability name the protocol defines, in the order above.
            pub const ALL: &[&str] = &[$($name),+];
        };
    }

    capabilities! {
        /// The recorder can start and stop a recording.
        RECORDING = "recording";
        /// The recorder can report its status, and push status events.
        STATUS_EVENTS = "status_events";
        /// The recorder can mark a moment in the recording it is making.
        ///
        /// A UI asks for this before offering an "Add Bookmark" control,
        /// because a recorder built before
        /// [issue #64](https://github.com/wildware-uk/clipped/issues/64) has no
        /// `add_bookmark` command at all and would refuse the request with
        /// [`ErrorCode::UnknownCommand`](crate::ErrorCode::UnknownCommand) —
        /// which is the version skew this list exists to make visible before
        /// the button is drawn rather than after it is pressed.
        BOOKMARKS = "bookmarks";
        /// The recorder can take a screenshot.
        ///
        /// A UI asks for this before offering a "Take Screenshot" control, for
        /// the reason the line above gives: a recorder built before
        /// [issue #67](https://github.com/wildware-uk/clipped/issues/67) has no
        /// `take_screenshot` command it will perform, and the version skew is
        /// worth seeing before the button is drawn rather than after it is
        /// pressed.
        SCREENSHOTS = "screenshots";
        /// The recorder can be asked to finish and exit over the protocol.
        ///
        /// Unlike the two above, this one is not the application's to claim:
        /// it is [`Server`](crate::Server) that ends the accept loop, so
        /// [`Server::serve`](crate::Server::serve) is what adds this to a
        /// [`Welcome`], and a [`CommandHandler`](crate::CommandHandler) neither
        /// advertises nor performs it. A recorder built before the command
        /// existed does not advertise it and refuses it with
        /// [`ErrorCode::UnknownCommand`](crate::ErrorCode::UnknownCommand),
        /// which is what a client checks for before offering an "Exit" that
        /// would do nothing.
        SHUTDOWN = "shutdown";
        /// The recorder can read the recording library: `library_sessions` and
        /// `library_games`.
        ///
        /// A UI asks for this before drawing a library screen at all, for the
        /// reason the two above give: a recorder built before
        /// [issue #301](https://github.com/wildware-uk/clipped/issues/301) has
        /// no command that reads a row and would refuse the request with
        /// [`ErrorCode::UnknownCommand`](crate::ErrorCode::UnknownCommand).
        /// Without the check the window would have no way to tell that from an
        /// empty library, which is exactly the confusion the library commands
        /// are shaped to avoid.
        LIBRARY = "library";
        /// The recorder can copy a finished recording into MP4:
        /// `export_recording`.
        ///
        /// A UI asks for this before drawing an Export control against a
        /// recording, for the reason the four above give: a recorder built
        /// before [issue #399](https://github.com/wildware-uk/clipped/issues/399)
        /// has no `export_recording` command and would refuse the request with
        /// [`ErrorCode::UnknownCommand`](crate::ErrorCode::UnknownCommand) —
        /// after the user had chosen a file name for a file that was never
        /// going to be written.
        EXPORT = "export";
        /// The recorder registers the global hotkeys and can report where each
        /// one stands: `get_hotkeys`.
        ///
        /// A UI asks for this before drawing a hotkey list, for the reason the
        /// five above give: a recorder built before
        /// [issue #232](https://github.com/wildware-uk/clipped/issues/232)
        /// registers nothing at all and has no `get_hotkeys` command, so it
        /// would refuse the request with
        /// [`ErrorCode::UnknownCommand`](crate::ErrorCode::UnknownCommand). The
        /// difference matters more here than elsewhere: "this recorder does not
        /// register hotkeys" and "every hotkey registered cleanly" are opposite
        /// answers, and an empty list would be drawn as the second.
        HOTKEYS = "hotkeys";
        /// The recorder can save the last few seconds out of a recording's
        /// replay buffer: `save_replay`.
        ///
        /// A UI asks for this before offering a "Save Replay" control, for the
        /// reason the six above give: a recorder built before
        /// [issue #38](https://github.com/wildware-uk/clipped/issues/38)
        /// *parses* `save_replay` and always refuses it with
        /// [`ErrorCode::NotImplemented`](crate::ErrorCode::NotImplemented), so
        /// the check is what tells a window with an unusable button from one
        /// with a working one before somebody presses it.
        ///
        /// The feature says the command exists, not that a buffer is running.
        /// Whether *this* recording has one to save from is
        /// [`ActiveRecording::replay_seconds`](crate::ActiveRecording), because
        /// it is a property of the recording rather than of the build.
        REPLAY = "replay";
        /// The recorder can read and change the settings, and can list this
        /// machine's microphones: `get_settings`, `apply_settings` and
        /// `get_audio_devices`.
        ///
        /// A UI asks for this before drawing a settings form, for the reason
        /// the seven above give — but with a sharper failure than most. A
        /// recorder built before
        /// [issue #51](https://github.com/wildware-uk/clipped/issues/51) *has*
        /// an `apply_settings` command and refuses every call to it with
        /// [`ErrorCode::NotImplemented`](crate::ErrorCode::NotImplemented), so
        /// a window that did not check would draw a full form of controls and
        /// discover that none of them save only when somebody pressed Save.
        SETTINGS = "settings";
    }
}

/// Who is at the other end of a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerIdentity {
    /// The program's name, such as `clipped-recorder`.
    pub name: String,
    /// Its build version, as its own `Cargo.toml` states it.
    ///
    /// For diagnosis and for telling the user which side to update. Nothing
    /// branches on it: what a peer can *do* is [`Welcome::features`], and what
    /// it can *say* is the protocol version.
    pub version: String,
}

/// What a connection is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ConnectionRole {
    /// Commands and their replies, in strict alternation. The default.
    #[default]
    Control,
    /// Events, pushed by the recorder. Nothing is sent back after the
    /// handshake.
    Events,
    /// A role this build does not have. Refused, by name.
    Unknown,
}

impl ConnectionRole {
    /// The wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Events => "events",
            Self::Unknown => "unknown",
        }
    }
}

impl From<String> for ConnectionRole {
    fn from(role: String) -> Self {
        match role.as_str() {
            "control" => Self::Control,
            "events" => Self::Events,
            _ => Self::Unknown,
        }
    }
}

impl From<ConnectionRole> for String {
    fn from(role: ConnectionRole) -> Self {
        role.as_str().to_owned()
    }
}

/// A stream of events a connection can ask for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum EventStream {
    /// `status` — what the recorder is doing, whenever it changes. The
    /// subscription opens with the current state, so a UI that has just
    /// attached does not have to ask separately and race with the answer.
    Status,
    /// `errors` — failures that were nobody's request, such as a recording that
    /// stopped because the encoder did.
    Errors,
    /// `metrics` — live throughput, dropped frames and encoder load.
    ///
    /// Defined here and **refused** by this build: nothing measures those
    /// figures during a recording yet, and a stream that silently never
    /// delivers is a control that does nothing
    /// ([issue #100](https://github.com/wildware-uk/clipped/issues/100),
    /// AGENTS.md section 27).
    Metrics,
    /// A stream this build does not have. Refused, by name.
    Other(String),
}

impl EventStream {
    /// The wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Status => "status",
            Self::Errors => "errors",
            Self::Metrics => "metrics",
            Self::Other(name) => name,
        }
    }
}

impl From<String> for EventStream {
    fn from(stream: String) -> Self {
        match stream.as_str() {
            "status" => Self::Status,
            "errors" => Self::Errors,
            "metrics" => Self::Metrics,
            _ => Self::Other(stream),
        }
    }
}

impl From<EventStream> for String {
    fn from(stream: EventStream) -> Self {
        match stream {
            EventStream::Other(name) => name,
            ref known => known.as_str().to_owned(),
        }
    }
}

/// The first frame on every connection.
///
/// **Frozen.** See the module documentation: this shape and the refusal it can
/// produce are how two builds that agree on nothing else still manage to say
/// so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The version the client speaks. One version, not a range: a client that
    /// can speak two connects twice or retries, rather than leaving the
    /// recorder to guess which it meant.
    pub protocol_version: u32,
    /// Who is connecting, for the recorder's log and for the user's benefit
    /// when versions do not match.
    pub client: PeerIdentity,
    /// What the connection is for.
    #[serde(default)]
    pub role: ConnectionRole,
    /// Which event streams to deliver. Only read for
    /// [`ConnectionRole::Events`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<EventStream>,
}

/// The recorder accepting a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The version now in force, which is the one the client asked for.
    pub protocol_version: u32,
    /// Which recorder answered.
    pub recorder: PeerIdentity,
    /// What the connection was accepted as.
    pub role: ConnectionRole,
    /// What this build can actually do. See [`features`].
    pub features: Vec<String>,
    /// The event streams this connection will receive, for
    /// [`ConnectionRole::Events`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<EventStream>,
}

/// One command, and the identifier its reply will quote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Chosen by the client, unique among the requests in flight on this
    /// connection.
    pub id: u64,
    /// The command's name. See [`Command`](crate::Command).
    pub command: String,
    /// The command's parameters, kept untyped at this level so that an unknown
    /// command is refused by name rather than as a broken frame.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
}

/// One reply, quoting the request it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The [`Request::id`] this answers.
    pub id: u64,
    /// What happened.
    pub outcome: Outcome,
}

/// What a command produced, or why it did not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It worked.
    Ok(Reply),
    /// It did not, and this is what to tell the user.
    Error(ProtocolError),
}

/// Something the recorder decided to say without being asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The recorder started, stopped or changed what it is doing.
    StatusChanged {
        /// The new state.
        status: RecorderStatus,
    },
    /// A recording ended because something failed, rather than because it was
    /// asked to.
    ///
    /// The file is still finished and playable — `clipped-session` closes the
    /// container on every path out — and this says what stopped it.
    RecordingFailed {
        /// Which recording.
        recording_id: String,
        /// What failed.
        error: ProtocolError,
    },
    /// An event this build has never heard of, kept exactly as it arrived.
    ///
    /// Only ever produced by *reading*: a newer recorder may send an event that
    /// did not exist when the client was compiled, and `docs/ipc.md` says
    /// adding one costs no version bump. Without somewhere for it to go the
    /// whole frame fails to parse, and a client that treats an unreadable frame
    /// as a broken connection would lose its subscription over an event it did
    /// not need — the outcome the compatibility policy exists to prevent.
    ///
    /// The catch-all is last, and the variants above it are matched on their
    /// `event` tag first, so a known event never falls into it.
    #[serde(untagged)]
    Other(serde_json::Value),
}

impl Event {
    /// Which stream this event belongs to, for routing.
    ///
    /// [`None`] for [`Event::Other`], which is the only honest answer: an event
    /// this build has never heard of belongs to a stream only the build that
    /// invented it can name. Nothing publishes one — the recorder publishes
    /// events it constructed — so this is a reader's question, and a reader
    /// that cannot place an event ignores it.
    #[must_use]
    pub const fn stream(&self) -> Option<EventStream> {
        match self {
            Self::StatusChanged { .. } => Some(EventStream::Status),
            Self::RecordingFailed { .. } => Some(EventStream::Errors),
            Self::Other(_) => None,
        }
    }
}

/// Anything the desktop application sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// The handshake. Always first, exactly once.
    Hello(Hello),
    /// A command.
    Request(Request),
}

/// Anything the recorder sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The handshake was accepted.
    Welcome(Welcome),
    /// The connection was refused, and is about to be closed. Carries the
    /// reason, which for a version mismatch names every version the recorder
    /// speaks.
    Refused(ProtocolError),
    /// A reply to a request.
    Response(Response),
    /// An event on a subscribed stream.
    Event(Event),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorCode, ErrorDetail};

    #[test]
    fn the_handshake_shape_is_the_one_documented_and_frozen() {
        // This assertion is deliberately a literal. `Hello` is the one message
        // that has to stay readable by every build that will ever exist, so a
        // change to its field names should fail here and make somebody explain
        // themselves in review (AGENTS.md section 43).
        let hello = Hello {
            protocol_version: 1,
            client: PeerIdentity {
                name: "clipped-desktop".to_owned(),
                version: "0.1.0".to_owned(),
            },
            role: ConnectionRole::Control,
            streams: Vec::new(),
        };

        assert_eq!(
            serde_json::to_string(&ClientMessage::Hello(hello)).expect("it serialises"),
            r#"{"type":"hello","protocol_version":1,"client":{"name":"clipped-desktop","version":"0.1.0"},"role":"control"}"#
        );
    }

    #[test]
    fn a_hello_without_a_role_is_a_control_connection() {
        let hello: Hello =
            serde_json::from_str(r#"{"protocol_version":1,"client":{"name":"x","version":"0"}}"#)
                .expect("role is optional");
        assert_eq!(hello.role, ConnectionRole::Control);
        assert!(hello.streams.is_empty());
    }

    #[test]
    fn a_role_this_build_does_not_have_parses_as_unknown_rather_than_failing() {
        // So that the recorder can refuse it with a sentence, rather than the
        // client seeing "malformed frame" and having to guess.
        let hello: Hello = serde_json::from_str(
            r#"{"protocol_version":2,"client":{"name":"x","version":"0"},"role":"preview_frames"}"#,
        )
        .expect("an unknown role still parses");
        assert_eq!(hello.role, ConnectionRole::Unknown);
    }

    #[test]
    fn a_refusal_says_what_the_recorder_speaks_instead() {
        let refused = ServerMessage::Refused(
            ProtocolError::new(
                ErrorCode::UnsupportedProtocolVersion,
                "this recorder speaks protocol 1",
            )
            .with_detail(ErrorDetail::UnsupportedProtocolVersion {
                requested: 9,
                supported: vec![1],
                recorder_version: "0.1.0".to_owned(),
            }),
        );

        let json = serde_json::to_string(&refused).expect("it serialises");
        let back: ServerMessage = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, refused);
        assert!(
            json.contains("\"supported\":[1]"),
            "a refusal that does not say what is supported leaves nowhere to go: {json}"
        );
    }

    #[test]
    fn a_response_carries_either_a_reply_or_a_refusal_and_says_which() {
        let ok = ServerMessage::Response(Response {
            id: 4,
            outcome: Outcome::Ok(Reply::Pong),
        });
        assert_eq!(
            serde_json::to_string(&ok).expect("it serialises"),
            r#"{"type":"response","id":4,"outcome":{"ok":{"reply":"pong"}}}"#
        );

        let failed = ServerMessage::Response(Response {
            id: 4,
            outcome: Outcome::Error(ProtocolError::new(
                ErrorCode::NotRecording,
                "nothing to stop",
            )),
        });
        let json = serde_json::to_string(&failed).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).expect("and deserialises"),
            failed
        );
    }

    #[test]
    fn every_event_belongs_to_exactly_one_stream() {
        assert_eq!(
            Event::StatusChanged {
                status: RecorderStatus::Idle
            }
            .stream(),
            Some(EventStream::Status)
        );
        assert_eq!(
            Event::RecordingFailed {
                recording_id: "r-1".to_owned(),
                error: ProtocolError::new(ErrorCode::RecordingFailed, "the encoder went away"),
            }
            .stream(),
            Some(EventStream::Errors)
        );
    }

    #[test]
    fn an_event_this_build_has_never_heard_of_is_read_rather_than_breaking_the_connection() {
        // `docs/ipc.md` says adding an event does not increment the protocol
        // version, so a client compiled against protocol 1 has to survive one it
        // has never seen. Before the catch-all existed this frame failed to
        // deserialise, and a subscription died over an event nobody needed.
        let json = r#"{"type":"event","event":"disk_filling_up","free_bytes":1024}"#;

        let message: ServerMessage =
            serde_json::from_str(json).expect("an unknown event must still be a readable frame");

        match &message {
            ServerMessage::Event(event) => {
                assert_eq!(
                    event.stream(),
                    None,
                    "a build cannot know which stream an event it has never heard of belongs to"
                );
                match event {
                    Event::Other(raw) => assert_eq!(raw["event"], "disk_filling_up"),
                    other => panic!("an unknown event should be kept verbatim, got {other:?}"),
                }
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn a_known_event_is_still_read_as_itself_rather_than_as_an_unknown_one() {
        // The catch-all must not swallow the events this build does know. It is
        // matched after the tagged variants, so an event carrying a field added
        // later is still a `status_changed` — a `status_changed` that arrived as
        // an opaque object would leave the UI showing nothing and reporting no
        // error.
        let event: Event = serde_json::from_str(
            r#"{"event":"status_changed","status":{"state":"idle"},"invented_later":true}"#,
        )
        .expect("a known event with an unknown field parses");

        assert_eq!(
            event,
            Event::StatusChanged {
                status: RecorderStatus::Idle
            },
            "an unknown *field* is ignored, not a reason to fall back to the catch-all"
        );
    }

    #[test]
    fn an_event_round_trips_inside_its_envelope() {
        let event = ServerMessage::Event(Event::StatusChanged {
            status: RecorderStatus::Idle,
        });
        let json = serde_json::to_string(&event).expect("it serialises");
        assert_eq!(
            json,
            r#"{"type":"event","event":"status_changed","status":{"state":"idle"}}"#
        );
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).expect("and deserialises"),
            event
        );
    }

    #[test]
    fn the_supported_versions_include_the_one_this_build_speaks() {
        assert!(SUPPORTED_PROTOCOL_VERSIONS.contains(&PROTOCOL_VERSION));
    }
}
