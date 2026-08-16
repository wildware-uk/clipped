//! The protocol the desktop application drives the recorder through.
//!
//! Clipped is two processes. The recorder owns every recording and is expected
//! to run for days; the desktop application is a window the user opens, closes
//! and crashes, and it must be able to do all three without touching a
//! recording ([ADR
//! 0002](../../../docs/adr/0002-separate-recorder-process.md)). This crate is
//! the seam between them: the message definitions, the framing, the handshake
//! that decides whether two builds can talk at all, and the Windows named pipe
//! they talk over.
//!
//! `docs/ipc.md` is the specification. This documentation is the map of the
//! implementation.
//!
//! # Responsibilities
//!
//! - The wire format: messages, framing, error vocabulary, versioning.
//! - The transport: a per-user named pipe, and the rules for naming it.
//! - The mechanics of serving connections and of being a client.
//! - A machine-readable description of all of that, in [`schema`], which the
//!   desktop application's TypeScript mirror of these messages is checked
//!   against. The types here are the authority for both ends, and that module
//!   is how the other end finds out when they change.
//! - **Whether there is a recorder to talk to at all**: starting one, noticing
//!   when it has gone, and keeping each side of the conversation to one process
//!   ([`supervisor`]).
//!
//! [`schema`] is public, which for what is a build-time tool wants a reason
//! (AGENTS.md section 44). The emitter that writes the description is a
//! separate Cargo target — `src/bin/protocol-schema.rs` — and a binary reaches
//! the library beside it only through that library's public API, so the
//! alternative to exposing the module is not a smaller surface but a second
//! copy of the protocol's description living in the binary. A `cargo` feature
//! was weighed and not taken: off by default it would take
//! `the_committed_schema_is_the_one_this_build_produces` out of `cargo test
//! --workspace`, and with it the half of the conformance check that runs on
//! this side; on by default it would be a flag nothing ever turns off
//! (AGENTS.md section 38). What is exposed is inert — some `serde` data types,
//! one function that fills them in, and the path the file is written to.
//! Nothing in it takes part in holding a conversation, and nothing else in this
//! crate depends on it.
//!
//! [`supervisor`] is a deliberate widening of what this crate was when it held
//! only the wire format, and
//! [ADR 0006](../../../docs/adr/0006-recorder-lifetime-and-supervision.md)
//! records why it belongs here: supervision is expressed entirely in terms of
//! the endpoint, the client and the events — everything above — and both ends of
//! the boundary need it, which is exactly the property that put the protocol
//! here. It still names nothing from the recording engine, and nothing in it is
//! a message: [`schema`] does not describe it, because no recorder ever sends
//! one.
//!
//! # Not responsible for
//!
//! Doing anything a command asks for. This crate has no idea what a recording
//! is. [`CommandHandler`] is the hole the recorder plugs its own subsystems
//! into, which is what keeps a protocol crate from becoming a second place
//! where the application's rules live (AGENTS.md section 5). [`supervisor`] is
//! held to the same rule: it starts an executable it is handed the path to, and
//! could not name `clipped-recorder` if it wanted to.
//!
//! # Position in the architecture
//!
//! A leaf crate. It depends on no other `clipped-*` crate — deliberately, since
//! both ends of the protocol have to be able to use it, and the desktop
//! application's end must not drag the recording engine in with it.
//!
//! # Shape of a conversation
//!
//! ```text
//!  desktop application                     recorder
//!  ───────────────────                     ────────
//!  connect  ──────────────────────────▶    accept
//!  hello { protocol_version, role } ──▶    check the version
//!           ◀────────────────────────      welcome { features } | refused
//!  request { id, command, params } ───▶    dispatch
//!           ◀────────────────────────      response { id, outcome }
//! ```
//!
//! A second connection, opened with [`ConnectionRole::Events`], carries events
//! the other way and is never written to by the client after its handshake. The
//! module documentation on [`message`] explains why the two directions are not
//! on one connection.
//!
//! # Threading
//!
//! [`Server::serve`] blocks on the thread that called it and gives every
//! accepted connection a thread of its own, up to
//! [`MAX_CONCURRENT_CONNECTIONS`]. A [`CommandHandler`] is therefore called
//! from several threads at once and must be `Sync`; nothing here serialises
//! calls into it, because a `get_status` that had to queue behind a
//! `start_recording` would make the UI feel like the recorder had hung.
//!
//! Events are published from whichever thread noticed something — the thread
//! running a recording, most often — through [`EventPublisher`], which never
//! blocks the publisher on a slow client.
//!
//! # Example
//!
//! ```no_run
//! use std::time::Duration;
//!
//! use clipped_ipc::{Client, Command, Endpoint, Reply};
//!
//! let endpoint = Endpoint::for_this_session()?;
//! let mut client = Client::connect(&endpoint, "example", "0.1.0", Duration::from_secs(2))?;
//!
//! match client.call(&Command::GetStatus)? {
//!     Reply::Status { status } => println!("{status:?}"),
//!     other => println!("unexpected reply: {other:?}"),
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod client;
pub mod command;
pub mod error;
pub mod frame;
pub mod hotkeys;
pub mod library;
pub mod message;
mod plugins;
pub mod schema;
pub mod server;
pub mod status;
pub mod supervisor;
pub mod transport;

pub use client::{Client, ClientError, EventClient};
pub use command::{
    AddBookmark, Command, ExportRecording, Reply, SaveReplay, Shutdown, StartRecording,
    StopRecording, TakeScreenshot, UnbuiltCommand, UNBUILT_COMMANDS,
};
pub use error::{ErrorCode, ErrorDetail, ProtocolError};
pub use frame::{FrameError, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES};
pub use hotkeys::{HotkeyBinding, HotkeyState};
pub use library::{
    EmptyTrash, FavouriteMark, LibraryClip, LibraryEventLane, LibraryEventMark, LibraryEvents,
    LibraryGame, LibraryRecording, LibrarySession, LibrarySessionPage, LibrarySessions,
    LibraryTrash, LockMark, RestoreFromTrash, RestoredItem, SetFavourite, SetLock, TrashEmptied,
    TrashListing, TrashedItem,
};
pub use message::{
    features, ClientMessage, ConnectionRole, Event, EventStream, Hello, Outcome, PeerIdentity,
    Request, Response, ServerMessage, Welcome, PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
pub use plugins::{PluginDeclaration, PluginState, RefusedPlugin};
pub use server::{
    CommandHandler, EventPublisher, Server, ServerError, ShutdownRequest,
    MAX_CONCURRENT_CONNECTIONS,
};
pub use status::{
    ActiveRecording, BookmarkSummary, EndReason, ExportSummary, RecorderStatus, RecordingSummary,
    ReplaySummary, ScreenshotSummary,
};
pub use supervisor::{
    ensure_recorder, wait_for_recorder_to_exit, Attachment, AttachmentOrigin, RecorderCallError,
    RecorderLink, RecorderLinkEvent, RecorderLinkState, RestartPolicy, ShutdownOutcome,
    SupervisorError, SupervisorSettings,
};
pub use transport::{Endpoint, TransportError};
