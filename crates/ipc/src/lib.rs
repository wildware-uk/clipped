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
//! - **Whether there is a recorder to talk to at all**: starting one, noticing
//!   when it has gone, and keeping each side of the conversation to one process
//!   ([`supervisor`]).
//!
//! That last responsibility is a deliberate widening of what this crate was
//! when it held only the wire format, and
//! [ADR 0006](../../../docs/adr/0006-recorder-lifetime-and-supervision.md)
//! records why it belongs here: supervision is expressed entirely in terms of
//! the endpoint, the client and the events — everything above — and both ends of
//! the boundary need it, which is exactly the property that put the protocol
//! here. It still names nothing from the recording engine.
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
pub mod message;
pub mod server;
pub mod status;
pub mod supervisor;
pub mod transport;

pub use client::{Client, ClientError, EventClient};
pub use command::{
    Command, Reply, StartRecording, StopRecording, UnbuiltCommand, UNBUILT_COMMANDS,
};
pub use error::{ErrorCode, ErrorDetail, ProtocolError};
pub use frame::{FrameError, MAX_FRAME_BYTES};
pub use message::{
    features, ClientMessage, ConnectionRole, Event, EventStream, Hello, Outcome, PeerIdentity,
    Request, Response, ServerMessage, Welcome, PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
pub use server::{CommandHandler, EventPublisher, Server, ServerError, MAX_CONCURRENT_CONNECTIONS};
pub use status::{ActiveRecording, EndReason, RecorderStatus, RecordingSummary};
pub use supervisor::{
    ensure_recorder, Attachment, AttachmentOrigin, RecorderLink, RecorderLinkEvent,
    RecorderLinkState, RestartPolicy, SupervisorError, SupervisorSettings,
};
pub use transport::{Endpoint, TransportError};
