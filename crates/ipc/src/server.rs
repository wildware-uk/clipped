//! Serving the protocol: the accept loop, the handshake, and dispatch.
//!
//! This module knows how to *talk*; it does not know what a recording is. Every
//! command that survives the handshake and the parser is handed to a
//! [`CommandHandler`], which the recorder implements over its own subsystems.
//! That split is what keeps the application's rules out of a protocol crate
//! (AGENTS.md section 5), and it is what lets the whole conversation be tested
//! against a handler that does nothing at all.
//!
//! # What is refused here, and why it is refused here
//!
//! Three classes of message never reach a handler:
//!
//! - A **handshake this build cannot accept** — an unknown protocol version, an
//!   unknown connection role, an event stream this build does not produce. The
//!   connection is refused with a reason and closed.
//! - A **frame that is not a message** — bad JSON, or a length prefix over
//!   [`MAX_FRAME_BYTES`](crate::MAX_FRAME_BYTES). The connection is closed
//!   after the refusal rather than resynchronised: a peer that cannot frame
//!   correctly has no defined position in the stream to recover to.
//! - A **command whose subsystem this build does not contain**
//!   ([`UnbuiltCommand`]). Refused with the milestone and the issue that build
//!   it. There is deliberately no way for a handler to answer one, so no
//!   handler can accidentally answer "done" to a thing it did not do
//!   (AGENTS.md sections 27 and 54).
//!
//! One more never reaches a handler, and it is the opposite case: `shutdown` is
//! *performed* here. See [`ShutdownRequest`].
//!
//! # Threads
//!
//! [`Server::serve`] blocks on its caller's thread and gives every accepted
//! connection one of its own, capped at [`MAX_CONCURRENT_CONNECTIONS`]. The cap
//! exists because the endpoint is reachable by anything running as the user: a
//! loop that opened connections would otherwise be a loop that created threads
//! inside the process that must not fall over.
//!
//! Connection threads are **not joined** when the listener stops. A thread
//! blocked reading from a client that is still connected has nothing to
//! interrupt it, and waiting for one would mean a recorder that cannot be shut
//! down by a client that will not let go. Nothing a connection thread owns
//! needs finalising — the recording does, and it belongs to the application,
//! not to a connection — so the process exits and they go with it.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::command::{Command, Reply, Shutdown};
use crate::error::{ErrorCode, ErrorDetail, ProtocolError};
use crate::frame::{read_message, write_message, FrameError};
use crate::message::{
    features, ClientMessage, ConnectionRole, Event, EventStream, Hello, Outcome, PeerIdentity,
    Request, Response, ServerMessage, Welcome, SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::status::RecorderStatus;
use crate::transport::{Listener, ListenerStopper, TransportError};

/// How many connections the recorder will serve at once.
///
/// The desktop application needs two: one for commands and one for events. The
/// rest of the allowance is for a second window, a command-line client and a
/// diagnostic tool, with room to spare. It is a bound on what a runaway client
/// can cost, not a target.
pub const MAX_CONCURRENT_CONNECTIONS: usize = 8;

/// How many events a subscriber may fall behind by before events are dropped.
///
/// Bounded because the alternative is not "the UI catches up", it is the
/// recorder growing a queue on behalf of a window that is not being drawn. A
/// dropped event is logged and the next status event carries the whole state
/// anyway, so a subscriber that falls behind recovers rather than diverging.
const EVENT_QUEUE_DEPTH: usize = 64;

/// The application behind the protocol.
///
/// Implemented by the recorder. Called from several connection threads at once,
/// so it is `Sync` and must not assume it is alone.
pub trait CommandHandler: Send + Sync {
    /// Performs a command.
    ///
    /// [`Command::Unbuilt`] never arrives here: it is refused before dispatch,
    /// so that "not in this build" cannot be answered with a success.
    ///
    /// # Errors
    ///
    /// A [`ProtocolError`] the desktop application can render.
    fn call(&self, command: Command) -> Result<Reply, ProtocolError>;

    /// What the recorder is doing, for the snapshot every status subscription
    /// opens with.
    ///
    /// A subscriber that had to ask separately would race the first event, and
    /// a UI that starts from "unknown" and waits for something to change would
    /// stay wrong for as long as nothing does.
    fn status(&self) -> RecorderStatus;

    /// What this build can actually do, as [`crate::features`] names.
    ///
    /// Sent in every [`Welcome`], so that a newer client can decide whether to
    /// offer a control rather than offering it and having the command refused
    /// (AGENTS.md section 27).
    fn features(&self) -> Vec<String>;
}

/// The half of `shutdown` that is not the application's.
///
/// # Why this is here rather than behind [`CommandHandler`]
///
/// `docs/ipc.md` and
/// [issue #220](https://github.com/wildware-uk/clipped/issues/220) ask for one
/// thing: that asking the recorder to exit runs **the shutdown it already has**
/// rather than a second one. That shutdown begins when the accept loop ends —
/// Ctrl+C works by stopping the listener — and the accept loop belongs to
/// [`Server`]. A handler could not end it without being handed a way to, which
/// would be this type by another name and one more place for the two paths to
/// diverge.
///
/// So a `shutdown` request stops the listener, and [`Server::serve`] returns as
/// though Ctrl+C had been pressed.
///
/// # The contract this puts on whoever calls [`Server::serve`]
///
/// **`serve` returning means "stop serving", not "the process has ended".**
/// Finishing a recording and exiting are the caller's, because this crate has
/// no idea what a recording is. A caller that serves and then carries on
/// regardless would be advertising [`features::SHUTDOWN`] and not honouring it,
/// which is exactly the untruth AGENTS.md section 27 forbids.
/// `apps/recorder/src/serve.rs` is the caller that matters, and it does the
/// same three things after `serve` returns whichever way it ended: stops any
/// recording and waits for its file to be finalised, closes the event
/// publisher, and exits.
///
/// # What is guaranteed about the reply
///
/// The listener is stopped **after** the reply has been written, not before, so
/// a client learns its shutdown was accepted rather than seeing the connection
/// break and having to guess why (`serve_commands`).
#[derive(Debug, Clone, Default)]
pub struct ShutdownRequest {
    inner: Arc<ShutdownState>,
}

/// What a [`ShutdownRequest`]'s clones share.
#[derive(Debug, Default)]
struct ShutdownState {
    /// Set when the listener has been asked to stop, and never cleared.
    requested: AtomicBool,
    /// Set the moment a shutdown starts being *decided*, and cleared again if
    /// it is refused. See [`ShutdownRequest::begin`].
    ///
    /// Separate from `requested` because they answer different questions at
    /// different moments: this one is true for the few microseconds a refusal
    /// takes to be decided, and `requested` is the fact a caller logs.
    deciding: AtomicBool,
    /// [`None`] until [`Server::serve`] attaches the listener it is serving.
    stopper: Mutex<Option<ListenerStopper>>,
}

/// The lock is read and written through a poisoned mutex deliberately.
///
/// A panic on some other connection thread must not be what decides that a
/// shutdown is answered `shutting_down` and then never happens. The value is one
/// cloneable handle, so there is no half-written state a poisoned lock could be
/// hiding — and the alternative, silently doing nothing, is exactly the failure
/// [`ShutdownRequest`] exists to prevent.
fn through_poison<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ShutdownRequest {
    /// A request nothing has asked for yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a shutdown has been asked for over the protocol.
    ///
    /// For a caller that wants to tell "the listener stopped because somebody
    /// asked it to" from "the listener stopped because Ctrl+C was pressed".
    /// Nothing in this crate branches on it; it is a fact worth logging.
    #[must_use]
    pub fn was_requested(&self) -> bool {
        self.inner.requested.load(Ordering::SeqCst)
    }

    /// Closes the recorder to new recordings, before a shutdown is decided.
    ///
    /// The decision is "may exiting end a recording", and it is made by reading
    /// the recorder's status. Between that read and the reply being written, a
    /// `start_recording` on another connection could begin one the decision
    /// never covered — and the recorder would then exit, ending a recording
    /// nobody was asked about (AGENTS.md section 17).
    ///
    /// So the door is shut first and the status read afterwards. From here a
    /// `start_recording` is refused with [`ErrorCode::ShuttingDown`], which is
    /// what makes that status read the last word rather than a snapshot.
    /// [`abandon`](Self::abandon) opens it again if the shutdown turns out to be
    /// refused, so a recorder that said no is left exactly as it was.
    fn begin(&self) {
        self.inner.deciding.store(true, Ordering::SeqCst);
    }

    /// Opens the door again, for a shutdown that was refused.
    fn abandon(&self) {
        self.inner.deciding.store(false, Ordering::SeqCst);
    }

    /// Whether this recorder is on its way out and will start nothing new.
    fn is_shutting_down(&self) -> bool {
        self.inner.deciding.load(Ordering::SeqCst)
    }

    /// Points this at the listener whose accept loop is to be ended.
    fn attach(&self, stopper: ListenerStopper) {
        *through_poison(&self.inner.stopper) = Some(stopper);
    }

    /// Ends the accept loop.
    ///
    /// Idempotent, and called from a connection thread rather than from the one
    /// blocked in `accept`, which is why stopping a listener is a thing that
    /// can be done from elsewhere at all (`transport/windows.rs`).
    fn request(&self) {
        self.inner.requested.store(true, Ordering::SeqCst);
        // Belt and braces: `accept_shutdown` has already done this, and a
        // future caller of `request` that has not would otherwise leave the
        // door open on a recorder that is exiting.
        self.begin();

        let stopper = through_poison(&self.inner.stopper).clone();

        match stopper {
            Some(stopper) => stopper.stop(),
            // Only reachable when nothing is serving a listener, which outside
            // this crate's own tests cannot happen: `Server::serve` attaches one
            // before it accepts anything. Recorded rather than ignored, because
            // a shutdown that was answered and did not happen is the failure
            // this whole path exists to avoid.
            None => tracing::error!(
                "a shutdown was accepted with no listener attached, so nothing stopped"
            ),
        }
    }
}

/// Distributes events to the connections that asked for them.
///
/// Cheap to clone; every clone publishes to the same subscribers. Publishing
/// never blocks: the thread that noticed a recording had failed is quite
/// possibly the thread that was recording, and it may not wait on a window
/// (AGENTS.md section 20).
#[derive(Debug, Clone, Default)]
pub struct EventPublisher {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
}

/// One event connection's place in the publisher.
#[derive(Debug)]
struct Subscriber {
    streams: Vec<EventStream>,
    events: SyncSender<Event>,
}

impl EventPublisher {
    /// A publisher with nobody listening.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sends an event to every connection subscribed to its stream.
    ///
    /// Subscribers that have gone are forgotten, and a subscriber that is too
    /// far behind loses this event rather than making the publisher wait.
    pub fn publish(&self, event: &Event) {
        let Some(stream) = event.stream() else {
            // Only a *read* event can be one this build cannot place, and the
            // recorder publishes events it constructed. Reaching here would be
            // a bug in a caller rather than version skew.
            tracing::warn!("an event that belongs to no stream was not published");
            return;
        };
        let Ok(mut subscribers) = self.subscribers.lock() else {
            // A poisoned lock means a panic somewhere in this module. Events are
            // diagnostics for the UI, not the recording, so the right response
            // is to say so and carry on.
            tracing::warn!("the event publisher's subscriber list was poisoned; event dropped");
            return;
        };

        subscribers.retain(|subscriber| {
            if !subscriber.streams.contains(&stream) {
                return true;
            }
            match subscriber.events.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    tracing::warn!(
                        stream = stream.as_str(),
                        "a client is not keeping up with events; one was dropped"
                    );
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// How many connections are subscribed.
    ///
    /// For diagnostics — "is anybody attached?" is the first question when the
    /// UI says it is showing nothing — and for the tests, which have to know a
    /// subscription exists before they can end it.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .lock()
            .map(|subscribers| subscribers.len())
            .unwrap_or_default()
    }

    /// Registers a connection and returns what it should write out.
    fn subscribe(&self, streams: Vec<EventStream>) -> Receiver<Event> {
        let (events, receiver) = sync_channel(EVENT_QUEUE_DEPTH);
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(Subscriber { streams, events });
        }
        receiver
    }

    /// Ends every subscription, which is what lets the threads writing them
    /// finish.
    pub fn close(&self) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.clear();
        }
    }
}

/// Why serving stopped.
#[derive(Debug)]
pub enum ServerError {
    /// The transport failed in a way the accept loop could not continue past.
    Transport(TransportError),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
        }
    }
}

/// The recorder's side of the protocol.
#[derive(Debug)]
pub struct Server<H: CommandHandler + 'static> {
    handler: Arc<H>,
    events: EventPublisher,
    identity: PeerIdentity,
    live_connections: Arc<AtomicUsize>,
    shutdown: ShutdownRequest,
}

impl<H: CommandHandler + 'static> Server<H> {
    /// A server over `handler`, announcing itself as `identity`.
    #[must_use]
    pub fn new(handler: Arc<H>, events: EventPublisher, identity: PeerIdentity) -> Self {
        Self {
            handler,
            events,
            identity,
            live_connections: Arc::new(AtomicUsize::new(0)),
            shutdown: ShutdownRequest::new(),
        }
    }

    /// Whether the accept loop ended because a client sent `shutdown`.
    ///
    /// False when it ended for any other reason, including Ctrl+C. The caller
    /// winds up the same way either way; this is for the log line that says
    /// which it was.
    #[must_use]
    pub fn shutdown_was_requested(&self) -> bool {
        self.shutdown.was_requested()
    }

    /// Accepts and serves connections until the listener is stopped.
    ///
    /// It stops for one of two reasons: something outside called
    /// [`ListenerStopper::stop`] — which is how Ctrl+C reaches it — or a client
    /// sent `shutdown`. **Both mean the same thing to the caller**, and
    /// [`ShutdownRequest`] sets out what the caller has to do about it: finish
    /// anything still being recorded, and exit. Returning from here does not
    /// end the process, and nothing in this crate can.
    ///
    /// # Errors
    ///
    /// [`ServerError::Transport`] if accepting fails. A failure on one
    /// *connection* is not an error here: it closes that connection and the
    /// loop carries on, because a client that misbehaved must not be able to
    /// stop the recorder serving anybody else.
    pub fn serve(&self, listener: &mut Listener) -> Result<(), ServerError> {
        // Before the first accept, so that a `shutdown` on the first connection
        // has something to stop.
        self.shutdown.attach(listener.stopper());

        loop {
            let connection = match listener.accept() {
                Ok(Some(connection)) => connection,
                Ok(None) => {
                    tracing::info!("the recorder stopped listening");
                    return Ok(());
                }
                Err(error) => return Err(ServerError::Transport(error)),
            };

            if self.live_connections.load(Ordering::SeqCst) >= MAX_CONCURRENT_CONNECTIONS {
                tracing::warn!(
                    limit = MAX_CONCURRENT_CONNECTIONS,
                    "refused a connection: the recorder is already serving as many as it will"
                );
                let mut connection = connection;
                let _ = write_message(
                    &mut connection,
                    &ServerMessage::Refused(ProtocolError::new(
                        ErrorCode::TooManyConnections,
                        format!(
                            "this recorder serves {MAX_CONCURRENT_CONNECTIONS} connections at a \
                             time and they are all in use"
                        ),
                    )),
                );
                continue;
            }

            let handler = Arc::clone(&self.handler);
            let events = self.events.clone();
            let identity = self.identity.clone();
            let shutdown = self.shutdown.clone();
            let live = Arc::clone(&self.live_connections);
            live.fetch_add(1, Ordering::SeqCst);

            let spawned = thread::Builder::new()
                .name("clipped-ipc-connection".to_owned())
                .spawn(move || {
                    let _counted = ConnectionCount(live);
                    let mut connection = connection;
                    serve_connection(&mut connection, &*handler, &events, &identity, &shutdown);
                });

            if let Err(error) = spawned {
                self.live_connections.fetch_sub(1, Ordering::SeqCst);
                tracing::error!(%error, "a connection could not be given a thread");
            }
        }
    }
}

/// Keeps the live-connection count honest however a connection thread ends.
#[derive(Debug)]
struct ConnectionCount(Arc<AtomicUsize>);

impl Drop for ConnectionCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Runs one connection from its handshake to its close.
///
/// Generic over the stream so that every path through it — an unknown version,
/// a malformed frame, a client that stops reading — is testable against a byte
/// buffer, with no pipe and no second process involved.
pub(crate) fn serve_connection<S: Read + Write, H: CommandHandler + ?Sized>(
    stream: &mut S,
    handler: &H,
    events: &EventPublisher,
    identity: &PeerIdentity,
    shutdown: &ShutdownRequest,
) {
    let hello = match read_hello(stream) {
        Ok(hello) => hello,
        Err(refusal) => {
            refuse(stream, refusal);
            return;
        }
    };

    if !SUPPORTED_PROTOCOL_VERSIONS.contains(&hello.protocol_version) {
        tracing::info!(
            requested = hello.protocol_version,
            client = hello.client.name,
            client_version = hello.client.version,
            "refused a connection speaking a protocol version this build does not"
        );
        refuse(
            stream,
            ProtocolError::new(
                ErrorCode::UnsupportedProtocolVersion,
                format!(
                    "this recorder speaks protocol version {}, and {} was asked for",
                    describe_versions(),
                    hello.protocol_version
                ),
            )
            .with_detail(ErrorDetail::UnsupportedProtocolVersion {
                requested: hello.protocol_version,
                supported: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
                recorder_version: identity.version.clone(),
            }),
        );
        return;
    }

    match hello.role {
        ConnectionRole::Control => {
            welcome(
                stream,
                identity,
                handler,
                ConnectionRole::Control,
                Vec::new(),
            );
            serve_commands(stream, handler, shutdown);
        }
        ConnectionRole::Events => match accepted_streams(&hello.streams) {
            Ok(streams) => {
                let receiver = events.subscribe(streams.clone());
                welcome(
                    stream,
                    identity,
                    handler,
                    ConnectionRole::Events,
                    streams.clone(),
                );
                serve_events(stream, handler, &receiver, &streams);
            }
            Err(refusal) => refuse(stream, refusal),
        },
        ConnectionRole::Unknown => refuse(
            stream,
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                "a connection is either `control` or `events`, and this was neither",
            ),
        ),
    }
}

/// Reads the first frame and insists it is a handshake.
fn read_hello<S: Read>(stream: &mut S) -> Result<Hello, ProtocolError> {
    match read_message::<_, ClientMessage>(stream) {
        Ok(ClientMessage::Hello(hello)) => Ok(hello),
        Ok(ClientMessage::Request(request)) => Err(ProtocolError::new(
            ErrorCode::HandshakeRequired,
            format!(
                "`{}` arrived before the handshake; the first message on a connection states \
                 the protocol version",
                request.command
            ),
        )),
        Err(error) => Err(frame_refusal(&error)),
    }
}

/// Turns a framing failure into the refusal it deserves.
fn frame_refusal(error: &FrameError) -> ProtocolError {
    ProtocolError::new(ErrorCode::MalformedFrame, error.to_string())
}

/// Writes a refusal and gives up on the connection.
fn refuse<S: Write>(stream: &mut S, refusal: ProtocolError) {
    tracing::debug!(code = %refusal.code, message = refusal.message, "refused a connection");
    // Nothing to do about a refusal that cannot be delivered: the peer has
    // already gone, which is the same outcome by another route.
    let _ = write_message(stream, &ServerMessage::Refused(refusal));
}

/// Writes the acceptance.
fn welcome<S: Write, H: CommandHandler + ?Sized>(
    stream: &mut S,
    identity: &PeerIdentity,
    handler: &H,
    role: ConnectionRole,
    streams: Vec<EventStream>,
) {
    let welcome = Welcome {
        protocol_version: crate::message::PROTOCOL_VERSION,
        recorder: identity.clone(),
        role,
        features: announced_features(handler),
        streams,
    };
    let _ = write_message(stream, &ServerMessage::Welcome(welcome));
}

/// What a connection is told this recorder can do.
///
/// The handler's own list, plus [`features::SHUTDOWN`], which is this module's
/// to claim rather than the application's: the accept loop is what a `shutdown`
/// ends, and [`ShutdownRequest`] sets out the contract that makes the rest of it
/// true. A handler that named it as well would be claiming something it does
/// not implement, so the duplicate is removed rather than announced twice.
fn announced_features<H: CommandHandler + ?Sized>(handler: &H) -> Vec<String> {
    let mut features = handler.features();
    if !features.iter().any(|name| name == features::SHUTDOWN) {
        features.push(features::SHUTDOWN.to_owned());
    }
    features
}

/// Which of the requested streams this build will deliver.
///
/// A stream that cannot be delivered refuses the whole connection rather than
/// being quietly dropped from the subscription. A UI that asked for metrics and
/// was silently given none would show an empty graph and no explanation, which
/// is precisely the failure AGENTS.md section 27 describes.
fn accepted_streams(requested: &[EventStream]) -> Result<Vec<EventStream>, ProtocolError> {
    if requested.is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            "an events connection has to say which streams it wants",
        ));
    }

    let mut accepted = Vec::with_capacity(requested.len());
    for stream in requested {
        match stream {
            EventStream::Status | EventStream::Errors => {
                if !accepted.contains(stream) {
                    accepted.push(stream.clone());
                }
            }
            EventStream::Metrics => {
                return Err(ProtocolError::not_implemented(
                    "live recording metrics",
                    "M14",
                    100,
                ))
            }
            EventStream::Other(name) => {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidParameters,
                    format!("this recorder has no `{name}` event stream"),
                ))
            }
        }
    }

    Ok(accepted)
}

/// Reads requests and writes replies until the client goes away.
fn serve_commands<S: Read + Write, H: CommandHandler + ?Sized>(
    stream: &mut S,
    handler: &H,
    shutdown: &ShutdownRequest,
) {
    loop {
        let message = match read_message::<_, ClientMessage>(stream) {
            Ok(message) => message,
            Err(error) if error.is_disconnect() => {
                tracing::debug!("a client disconnected");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "closing a connection that sent something unreadable");
                refuse(stream, frame_refusal(&error));
                return;
            }
        };

        let request = match message {
            ClientMessage::Request(request) => request,
            ClientMessage::Hello(_) => {
                refuse(
                    stream,
                    ProtocolError::new(
                        ErrorCode::MalformedFrame,
                        "the handshake happens once, at the start of a connection",
                    ),
                );
                return;
            }
        };

        let (outcome, after) = dispatch(handler, shutdown, &request);
        let response = Response {
            id: request.id,
            outcome,
        };

        if let Err(error) = write_message(stream, &ServerMessage::Response(response)) {
            // The client that asked has gone before the answer arrived. Normal
            // when a window is closed mid-request, and nothing to recover.
            tracing::debug!(%error, "a reply could not be delivered");
            return;
        }

        if after == AfterReply::StopServing {
            // Deliberately after the write. Stopping the listener starts the
            // caller winding the process up, and a reply written into a process
            // that has exited is a client left guessing whether its shutdown was
            // accepted or the pipe simply broke.
            tracing::info!("a client asked the recorder to finish and exit");
            shutdown.request();
            return;
        }
    }
}

/// What has to happen once a reply has been written.
///
/// Exists because the one thing that cannot be done *before* writing a reply is
/// ending the process that would write it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterReply {
    /// Read the next request, as usual.
    KeepServing,
    /// End the accept loop; the caller finishes any recording and exits.
    StopServing,
}

/// Performs one request, or explains why not.
fn dispatch<H: CommandHandler + ?Sized>(
    handler: &H,
    shutdown: &ShutdownRequest,
    request: &Request,
) -> (Outcome, AfterReply) {
    match Command::from_request(request) {
        Err(refusal) => (Outcome::Error(refusal), AfterReply::KeepServing),
        // A recorder that has accepted a shutdown is winding up, and a recording
        // started now would be one it stopped again a moment later — without
        // anybody having been asked whether it might. Refused here rather than
        // by the handler because the shutdown is this module's
        // ([`ShutdownRequest::begin`]).
        Ok(Command::StartRecording(_)) if shutdown.is_shutting_down() => (
            Outcome::Error(ProtocolError::new(
                ErrorCode::ShuttingDown,
                "this recorder has been asked to exit and will not start a recording",
            )),
            AfterReply::KeepServing,
        ),
        // Refused here, and never handed to a handler: there is no
        // implementation to reach, and a handler that could be asked is a
        // handler that could answer wrongly (AGENTS.md section 54).
        Ok(Command::Unbuilt(command)) => {
            tracing::debug!(
                command = command.name(),
                issue = command.tracking_issue(),
                "refused a command whose subsystem is not in this build"
            );
            (Outcome::Error(command.refusal()), AfterReply::KeepServing)
        }
        // Also never handed to a handler, for the opposite reason: what a
        // shutdown ends is the accept loop, which belongs to this module.
        Ok(Command::Shutdown(request)) => accept_shutdown(handler, shutdown, request),
        Ok(command) => {
            let outcome = match handler.call(command) {
                Ok(reply) => Outcome::Ok(reply),
                Err(refusal) => Outcome::Error(refusal),
            };
            (outcome, AfterReply::KeepServing)
        }
    }
}

/// Decides whether a shutdown may go ahead, and says what it will cost.
///
/// The recording is the whole of the decision. A recorder asked to exit while it
/// is recording is being asked to end something the user may not know is
/// running, so it refuses unless the request said in as many words that it may
/// (AGENTS.md section 17, [`Shutdown::finalise_recording`]). When it may, the
/// recording is named in the reply, because the file it leaves is a real file
/// somebody should be told about.
///
/// # Why the door is shut before the status is read
///
/// The status is read on one connection thread while seven others may be
/// serving commands. Reading it first and closing afterwards would leave a
/// window — the read, the decision, and the write of the reply — in which a
/// `start_recording` could begin a recording this decision never covered, and
/// which the caller's own shutdown would then end. [`ShutdownRequest::begin`]
/// closes that window; [`ShutdownRequest::abandon`] reopens it for a shutdown
/// that turns out to be refused, so a `start_recording` after a *refused*
/// shutdown is served exactly as before.
fn accept_shutdown<H: CommandHandler + ?Sized>(
    handler: &H,
    shutdown: &ShutdownRequest,
    request: Shutdown,
) -> (Outcome, AfterReply) {
    shutdown.begin();

    let recording = match handler.status() {
        // A watching recorder has nothing to finalise: it is between recordings,
        // which is what makes it watching rather than recording.
        RecorderStatus::Idle | RecorderStatus::Watching(_) => None,
        RecorderStatus::Recording(active) => Some(active),
    };

    match (&recording, request.finalise_recording) {
        (Some(active), false) => {
            shutdown.abandon();
            (
                Outcome::Error(ProtocolError::new(
                    ErrorCode::AlreadyRecording,
                    format!(
                        "`{}` is being recorded to {}; ask again with `finalise_recording` to \
                         finish that file and exit",
                        active.target, active.output
                    ),
                )),
                AfterReply::KeepServing,
            )
        }
        _ => (
            Outcome::Ok(Reply::ShuttingDown {
                finalising: recording,
            }),
            AfterReply::StopServing,
        ),
    }
}

/// Writes events until the subscription ends or the client stops reading.
fn serve_events<S: Write, H: CommandHandler + ?Sized>(
    stream: &mut S,
    handler: &H,
    events: &Receiver<Event>,
    streams: &[EventStream],
) {
    // The state as it is now, before anything changes. Without it a client that
    // attaches to a recorder which then does nothing for an hour has nothing to
    // display for an hour.
    if streams.contains(&EventStream::Status) {
        let snapshot = Event::StatusChanged {
            status: handler.status(),
        };
        if write_message(stream, &ServerMessage::Event(snapshot)).is_err() {
            return;
        }
    }

    while let Ok(event) = events.recv() {
        if let Err(error) = write_message(stream, &ServerMessage::Event(event)) {
            tracing::debug!(%error, "an event subscriber went away");
            return;
        }
    }
}

/// The supported versions, as a sentence fragment.
fn describe_versions() -> String {
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::message::PROTOCOL_VERSION;
    use crate::status::RecorderStatus;

    /// A connection whose input is a script and whose output is collected.
    ///
    /// Enough for the handshake and for request/response, both of which read
    /// and then write in strict alternation.
    #[derive(Debug)]
    struct Scripted {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Scripted {
        fn new(messages: &[ClientMessage]) -> Self {
            let mut input = Vec::new();
            for message in messages {
                write_message(&mut input, message).expect("the script is writable");
            }
            Self {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }

        fn raw(bytes: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(bytes),
                output: Vec::new(),
            }
        }

        fn replies(&self) -> Vec<ServerMessage> {
            let mut reader = self.output.as_slice();
            let mut replies = Vec::new();
            while let Ok(message) = read_message::<_, ServerMessage>(&mut reader) {
                replies.push(message);
            }
            replies
        }
    }

    impl Read for Scripted {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.output.write(buffer)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A handler that answers the two commands with no subsystem behind them.
    ///
    /// The state it reports is a field rather than a constant, because the one
    /// decision `shutdown` turns on is whether something is being recorded.
    #[derive(Debug)]
    struct Stub {
        status: RecorderStatus,
    }

    impl Stub {
        /// A recorder doing nothing.
        fn idle() -> Self {
            Self {
                status: RecorderStatus::Idle,
            }
        }

        /// A recorder part way through a recording.
        fn recording() -> Self {
            Self {
                status: RecorderStatus::Recording(crate::status::ActiveRecording {
                    recording_id: "r-1".to_owned(),
                    output: r"D:\clips\session.mkv".to_owned(),
                    target: "process `cs2.exe`".to_owned(),
                    elapsed_ms: 4_200,
                    replay_seconds: None,
                    session: None,
                }),
            }
        }
    }

    impl CommandHandler for Stub {
        fn call(&self, command: Command) -> Result<Reply, ProtocolError> {
            match command {
                Command::Ping => Ok(Reply::Pong),
                Command::GetStatus => Ok(Reply::Status {
                    status: self.status.clone(),
                }),
                other => Err(ProtocolError::new(
                    ErrorCode::Internal,
                    format!("this stub does not do `{}`", other.name()),
                )),
            }
        }

        fn status(&self) -> RecorderStatus {
            self.status.clone()
        }

        fn features(&self) -> Vec<String> {
            vec![crate::message::features::STATUS_EVENTS.to_owned()]
        }
    }

    fn identity() -> PeerIdentity {
        PeerIdentity {
            name: "clipped-recorder".to_owned(),
            version: "0.1.0".to_owned(),
        }
    }

    fn hello(version: u32, role: ConnectionRole, streams: Vec<EventStream>) -> ClientMessage {
        ClientMessage::Hello(Hello {
            protocol_version: version,
            client: PeerIdentity {
                name: "test".to_owned(),
                version: "0".to_owned(),
            },
            role,
            streams,
        })
    }

    fn run(script: &mut Scripted) {
        serve_connection(
            script,
            &Stub::idle(),
            &EventPublisher::new(),
            &identity(),
            &ShutdownRequest::new(),
        );
    }

    #[test]
    fn a_supported_version_is_welcomed_and_told_what_the_build_can_do() {
        let mut script =
            Scripted::new(&[hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new())]);
        run(&mut script);

        match script.replies().first() {
            Some(ServerMessage::Welcome(welcome)) => {
                assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
                assert_eq!(welcome.role, ConnectionRole::Control);
                assert!(
                    welcome
                        .features
                        .contains(&crate::message::features::STATUS_EVENTS.to_owned()),
                    "the welcome should carry what the build can do: {welcome:?}"
                );
            }
            other => panic!("expected a welcome, got {other:?}"),
        }
    }

    #[test]
    fn a_version_this_build_does_not_speak_is_refused_with_the_ones_it_does() {
        // The acceptance criterion: not undefined behaviour, not a
        // deserialisation failure several messages later, and not silence.
        let mut script = Scripted::new(&[hello(99, ConnectionRole::Control, Vec::new())]);
        run(&mut script);

        let replies = script.replies();
        assert_eq!(
            replies.len(),
            1,
            "the connection should end there: {replies:?}"
        );
        match &replies[0] {
            ServerMessage::Refused(error) => {
                assert_eq!(error.code, ErrorCode::UnsupportedProtocolVersion);
                assert_eq!(
                    error.detail,
                    Some(ErrorDetail::UnsupportedProtocolVersion {
                        requested: 99,
                        supported: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
                        recorder_version: "0.1.0".to_owned(),
                    })
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_client_from_before_the_watching_state_is_refused_rather_than_half_understood() {
        // Protocol 1 has no `watching` state and `RecorderStatus` has no
        // catch-all to put one in, so a client speaking it would fail to read
        // the first status this recorder sent it — several messages after being
        // told everything was fine (issue #241, `docs/ipc.md`). It is refused at
        // the handshake instead, and told which side is behind.
        let mut script = Scripted::new(&[hello(1, ConnectionRole::Control, Vec::new())]);
        run(&mut script);

        let replies = script.replies();
        assert_eq!(
            replies.len(),
            1,
            "the connection should end there: {replies:?}"
        );
        match &replies[0] {
            ServerMessage::Refused(error) => {
                assert_eq!(error.code, ErrorCode::UnsupportedProtocolVersion);
                assert_eq!(
                    error.detail,
                    Some(ErrorDetail::UnsupportedProtocolVersion {
                        requested: 1,
                        supported: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
                        recorder_version: "0.1.0".to_owned(),
                    }),
                    "a refusal that does not name what this recorder speaks leaves the client \
                     unable to say which side needs updating"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_request_before_the_handshake_is_refused_for_that_reason() {
        let mut script = Scripted::new(&[ClientMessage::Request(Request {
            id: 1,
            command: "ping".to_owned(),
            params: serde_json::Value::Null,
        })]);
        run(&mut script);

        match script.replies().first() {
            Some(ServerMessage::Refused(error)) => {
                assert_eq!(error.code, ErrorCode::HandshakeRequired);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_that_is_not_a_message_is_refused_and_the_connection_ends() {
        let payload = br#"{"type":"hello","protocol_version":"#;
        let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(payload);

        let mut script = Scripted::raw(bytes);
        run(&mut script);

        match script.replies().first() {
            Some(ServerMessage::Refused(error)) => {
                assert_eq!(error.code, ErrorCode::MalformedFrame);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_oversized_length_prefix_is_refused_without_the_payload_being_believed() {
        let mut bytes = u32::MAX.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"and now four gigabytes of nothing");

        let mut script = Scripted::raw(bytes);
        run(&mut script);

        match script.replies().first() {
            Some(ServerMessage::Refused(error)) => {
                assert_eq!(error.code, ErrorCode::MalformedFrame);
                assert!(
                    error.message.contains("4294967295"),
                    "the refusal should say what was announced: {}",
                    error.message
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn commands_are_answered_in_order_and_each_reply_quotes_its_request() {
        let mut script = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            ClientMessage::Request(Request {
                id: 10,
                command: "ping".to_owned(),
                params: serde_json::Value::Null,
            }),
            ClientMessage::Request(Request {
                id: 11,
                command: "get_status".to_owned(),
                params: serde_json::Value::Null,
            }),
        ]);
        run(&mut script);

        let replies = script.replies();
        assert!(matches!(replies[0], ServerMessage::Welcome(_)));
        match (&replies[1], &replies[2]) {
            (ServerMessage::Response(first), ServerMessage::Response(second)) => {
                assert_eq!(first.id, 10);
                assert_eq!(first.outcome, Outcome::Ok(Reply::Pong));
                assert_eq!(second.id, 11);
            }
            other => panic!("expected two responses, got {other:?}"),
        }
    }

    #[test]
    fn a_command_whose_subsystem_is_missing_is_refused_without_reaching_the_handler() {
        // The stub answers everything it is given with an `internal` error, so
        // a `not_implemented` code here proves the refusal happened before
        // dispatch — which is what stops a handler answering "saved" to a
        // replay it did not save.
        for unbuilt in crate::command::UNBUILT_COMMANDS {
            let mut script = Scripted::new(&[
                hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
                ClientMessage::Request(Request {
                    id: 1,
                    command: unbuilt.name().to_owned(),
                    params: serde_json::Value::Null,
                }),
            ]);
            run(&mut script);

            match &script.replies()[1] {
                ServerMessage::Response(response) => match &response.outcome {
                    Outcome::Error(error) => {
                        assert_eq!(error.code, ErrorCode::NotImplemented, "{}", unbuilt.name());
                        assert!(error.detail.is_some(), "{}", unbuilt.name());
                    }
                    other => panic!("{} should be refused, got {other:?}", unbuilt.name()),
                },
                other => panic!("expected a response, got {other:?}"),
            }
        }
    }

    /// Runs one control connection against a handler and a shutdown request of
    /// the caller's choosing.
    fn run_against<H: CommandHandler + ?Sized>(
        script: &mut Scripted,
        handler: &H,
        shutdown: &ShutdownRequest,
    ) {
        serve_connection(
            script,
            handler,
            &EventPublisher::new(),
            &identity(),
            shutdown,
        );
    }

    /// A `shutdown` request, with or without permission to end a recording.
    fn shutdown_request(finalise: bool) -> ClientMessage {
        ClientMessage::Request(Request {
            id: 1,
            command: "shutdown".to_owned(),
            params: serde_json::json!({ "finalise_recording": finalise }),
        })
    }

    #[test]
    fn every_connection_is_told_this_recorder_can_be_asked_to_exit() {
        // The stub's own feature list does not name it, because a handler does
        // not implement it — this module does. A client that read only the
        // handler's list would refuse to offer an Exit that works.
        let mut script =
            Scripted::new(&[hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new())]);
        run(&mut script);

        match script.replies().first() {
            Some(ServerMessage::Welcome(welcome)) => assert!(
                welcome.features.contains(&features::SHUTDOWN.to_owned()),
                "a recorder that will accept `shutdown` has to say so: {:?}",
                welcome.features
            ),
            other => panic!("expected a welcome, got {other:?}"),
        }
    }

    #[test]
    fn a_shutdown_of_an_idle_recorder_is_answered_and_then_ends_the_accept_loop() {
        let shutdown = ShutdownRequest::new();
        let mut script = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            shutdown_request(false),
        ]);
        run_against(&mut script, &Stub::idle(), &shutdown);

        match &script.replies()[1] {
            ServerMessage::Response(response) => assert_eq!(
                response.outcome,
                Outcome::Ok(Reply::ShuttingDown { finalising: None }),
                "nothing was being recorded, so nothing is named as being finished"
            ),
            other => panic!("expected a response, got {other:?}"),
        }
        assert!(
            shutdown.was_requested(),
            "answering a shutdown and not asking the listener to stop would be a control that \
             does nothing"
        );
    }

    #[test]
    fn a_shutdown_that_was_not_asked_to_end_a_recording_is_refused_and_the_recorder_keeps_serving()
    {
        // The safety property. Anything running as this user can reach the
        // endpoint, so a bare `shutdown` must not be able to end a recording
        // (AGENTS.md section 17).
        let shutdown = ShutdownRequest::new();
        let mut script = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            shutdown_request(false),
            ClientMessage::Request(Request {
                id: 2,
                command: "ping".to_owned(),
                params: serde_json::Value::Null,
            }),
        ]);
        run_against(&mut script, &Stub::recording(), &shutdown);

        let replies = script.replies();
        match &replies[1] {
            ServerMessage::Response(response) => match &response.outcome {
                Outcome::Error(error) => {
                    assert_eq!(error.code, ErrorCode::AlreadyRecording);
                    assert!(
                        error.message.contains("cs2.exe")
                            && error.message.contains("finalise_recording"),
                        "the refusal has to name what is being recorded and the way to proceed: {}",
                        error.message
                    );
                }
                other => panic!("a bare shutdown during a recording must be refused: {other:?}"),
            },
            other => panic!("expected a response, got {other:?}"),
        }
        assert!(
            !shutdown.was_requested(),
            "a refused shutdown must not have stopped the listener anyway"
        );
        assert_eq!(
            replies.len(),
            3,
            "the connection should still be serving after a refusal: {replies:?}"
        );
    }

    #[test]
    fn a_shutdown_that_may_finish_the_recording_names_the_file_it_will_leave() {
        let shutdown = ShutdownRequest::new();
        let mut script = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            shutdown_request(true),
        ]);
        run_against(&mut script, &Stub::recording(), &shutdown);

        match &script.replies()[1] {
            ServerMessage::Response(response) => match &response.outcome {
                Outcome::Ok(Reply::ShuttingDown {
                    finalising: Some(active),
                }) => {
                    assert_eq!(active.recording_id, "r-1");
                    assert_eq!(active.output, r"D:\clips\session.mkv");
                }
                other => panic!(
                    "a shutdown that ends a recording has to say which file it leaves: {other:?}"
                ),
            },
            other => panic!("expected a response, got {other:?}"),
        }
        assert!(shutdown.was_requested());
    }

    #[test]
    fn the_door_is_shut_before_the_recording_the_decision_turns_on_is_read() {
        // The ordering, which is the whole of the fix and is not visible from
        // outside: shutting the door *after* the status has been read leaves
        // exactly the window a `start_recording` could arrive in. So the
        // handler answers `status` by recording what it found, which is the
        // only moment the two can be compared.
        #[derive(Debug)]
        struct Watched {
            shutdown: ShutdownRequest,
            shut_when_asked: Mutex<Option<bool>>,
        }

        impl CommandHandler for Watched {
            fn call(&self, _command: Command) -> Result<Reply, ProtocolError> {
                Err(ProtocolError::new(ErrorCode::Internal, "not used"))
            }

            fn status(&self) -> RecorderStatus {
                *through_poison(&self.shut_when_asked) = Some(self.shutdown.is_shutting_down());
                RecorderStatus::Idle
            }

            fn features(&self) -> Vec<String> {
                Vec::new()
            }
        }

        let shutdown = ShutdownRequest::new();
        let handler = Watched {
            shutdown: shutdown.clone(),
            shut_when_asked: Mutex::new(None),
        };

        let mut script = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            shutdown_request(false),
        ]);
        run_against(&mut script, &handler, &shutdown);

        assert_eq!(
            *through_poison(&handler.shut_when_asked),
            Some(true),
            "the recorder has to stop accepting recordings before it reads whether one is \
             running, or the answer is out of date before the reply is written"
        );
    }

    #[test]
    fn a_recording_cannot_be_started_on_another_connection_once_a_shutdown_is_accepted() {
        // The permission a `shutdown` carries is decided by reading the status,
        // and the reply is written afterwards. Without a door shut before that
        // read, a `start_recording` on one of the other seven connections could
        // begin a recording the decision never covered — which the caller's own
        // shutdown would then end, having asked nobody (AGENTS.md section 17).
        let shutdown = ShutdownRequest::new();

        let mut exiting = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            shutdown_request(false),
        ]);
        run_against(&mut exiting, &Stub::idle(), &shutdown);

        let mut other = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            ClientMessage::Request(Request {
                id: 7,
                command: "start_recording".to_owned(),
                params: serde_json::json!({}),
            }),
        ]);
        run_against(&mut other, &Stub::idle(), &shutdown);

        match &other.replies()[1] {
            ServerMessage::Response(response) => match &response.outcome {
                Outcome::Error(error) => assert_eq!(
                    error.code,
                    ErrorCode::ShuttingDown,
                    "a recorder on its way out must not start a recording: {}",
                    error.message
                ),
                other => panic!("a recording started while exiting: {other:?}"),
            },
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_shutdown_leaves_the_recorder_able_to_start_a_recording() {
        // The other half of the door, and the reason it is opened again rather
        // than latched: a `shutdown` that was refused changed nothing, so a
        // `start_recording` afterwards is served exactly as it was before.
        let shutdown = ShutdownRequest::new();

        let mut refused = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            shutdown_request(false),
        ]);
        run_against(&mut refused, &Stub::recording(), &shutdown);

        let mut other = Scripted::new(&[
            hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
            ClientMessage::Request(Request {
                id: 7,
                command: "start_recording".to_owned(),
                params: serde_json::json!({}),
            }),
        ]);
        run_against(&mut other, &Stub::idle(), &shutdown);

        match &other.replies()[1] {
            ServerMessage::Response(response) => assert!(
                !matches!(
                    &response.outcome,
                    Outcome::Error(error) if error.code == ErrorCode::ShuttingDown
                ),
                "the shutdown was refused, so nothing is shutting down: {:?}",
                response.outcome
            ),
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn a_poisoned_lock_does_not_swallow_an_accepted_shutdown() {
        // The failure this replaced: `attach` and `request` both took the lock
        // with `if let Ok(..)`, so a panic anywhere in this module left an
        // accepted shutdown answered `shutting_down` with nothing ever stopping
        // the listener — the exact "a control that did nothing" this path
        // exists to avoid.
        let shutdown = ShutdownRequest::new();
        let poisoning = shutdown.clone();
        let _ = thread::spawn(move || {
            let _held = through_poison(&poisoning.inner.stopper);
            panic!("poisoning the lock on purpose");
        })
        .join();
        assert!(
            shutdown.inner.stopper.is_poisoned(),
            "this test is only meaningful against a poisoned lock"
        );

        let mut listener = crate::transport::Listener::bind(
            &crate::transport::Endpoint::named(&format!(
                "clipped-poison-test.{}",
                std::process::id()
            ))
            .expect("the generated name is valid"),
        )
        .expect("nothing else has this name");
        shutdown.attach(listener.stopper());

        shutdown.request();
        assert!(
            shutdown.was_requested(),
            "a shutdown that was accepted has to have been recorded"
        );
        assert!(
            listener.accept().expect("accepting works").is_none(),
            "the listener should have been stopped rather than left accepting"
        );
    }

    #[test]
    fn the_reply_to_a_shutdown_is_written_before_the_listener_is_asked_to_stop() {
        // Order matters and is not observable from the outside: the caller
        // winds the process up as soon as the accept loop ends, so a listener
        // stopped first is a client left with a broken pipe and no idea whether
        // its shutdown was accepted. The scripted stream records the order.
        #[derive(Debug, Default)]
        struct Order {
            replied_before_stopping: Mutex<Option<bool>>,
        }

        let shutdown = ShutdownRequest::new();
        let order = Arc::new(Order::default());

        /// A stream that notes, on the first write, whether a stop had already
        /// been asked for.
        #[derive(Debug)]
        struct Watched {
            inner: Scripted,
            shutdown: ShutdownRequest,
            order: Arc<Order>,
        }

        impl Read for Watched {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.inner.read(buffer)
            }
        }

        impl Write for Watched {
            fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
                let mut seen = self
                    .order
                    .replied_before_stopping
                    .lock()
                    .expect("the test's own lock");
                *seen = Some(seen.unwrap_or(true) && !self.shutdown.was_requested());
                drop(seen);
                self.inner.write(buffer)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }

        let mut stream = Watched {
            inner: Scripted::new(&[
                hello(PROTOCOL_VERSION, ConnectionRole::Control, Vec::new()),
                shutdown_request(false),
            ]),
            shutdown: shutdown.clone(),
            order: Arc::clone(&order),
        };
        serve_connection(
            &mut stream,
            &Stub::idle(),
            &EventPublisher::new(),
            &identity(),
            &shutdown,
        );

        assert_eq!(
            *order
                .replied_before_stopping
                .lock()
                .expect("the test's own lock"),
            Some(true),
            "every byte of the reply has to be on the wire before the listener stops"
        );
        assert!(shutdown.was_requested());
    }

    #[cfg(windows)]
    #[test]
    fn a_shutdown_over_a_real_pipe_ends_the_accept_loop_the_caller_is_blocked_in() {
        // The scripted tests above prove the reply and the request; this one
        // proves the thing they cannot, which is that the request reaches the
        // *listener*. `Server::serve` blocks in `accept` on another thread, and
        // ending it is what starts the recorder winding up — so a shutdown that
        // was answered and left the loop running would be the whole feature
        // quietly not working (issue #220).
        use std::time::{Duration, Instant};

        use crate::client::Client;
        use crate::transport::{Endpoint, Listener};

        let endpoint = Endpoint::named(&format!("clipped-shutdown-test.{}", std::process::id()))
            .expect("the generated name is valid");
        let mut listener = Listener::bind(&endpoint).expect("nothing else has this name");

        let server = Server::new(Arc::new(Stub::idle()), EventPublisher::new(), identity());
        let (finished, served) = std::sync::mpsc::channel();
        let serving = thread::spawn(move || {
            let outcome = server.serve(&mut listener);
            let _ = finished.send((outcome.is_ok(), server.shutdown_was_requested()));
        });

        let mut client = Client::connect(&endpoint, "test", "0", Duration::from_secs(5))
            .expect("the server is listening");
        let reply = client
            .call(&Command::Shutdown(Shutdown::default()))
            .expect("an idle recorder accepts a shutdown");
        assert_eq!(reply, Reply::ShuttingDown { finalising: None });

        let (ended_cleanly, requested) = served
            .recv_timeout(Duration::from_secs(5))
            .expect("the accept loop has to end, or nothing would ever stop the recorder");
        assert!(
            ended_cleanly,
            "the loop ended as a failure rather than a stop"
        );
        assert!(
            requested,
            "the loop ended without recording that a client asked it to"
        );

        let joined = Instant::now();
        serving.join().expect("the serving thread finished");
        assert!(joined.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_status_subscription_opens_with_the_state_the_recorder_is_in_and_then_waits() {
        // An event connection blocks on its subscription rather than reading,
        // which is the whole reason it is a separate connection. Closing the
        // publisher is what ends it — the same thing the recorder does when it
        // shuts down — so this covers that path as well.
        let publisher = EventPublisher::new();
        let closer = publisher.clone();
        let ending = thread::spawn(move || {
            while closer.subscriber_count() == 0 {
                thread::sleep(std::time::Duration::from_millis(1));
            }
            closer.close();
        });

        let mut script = Scripted::new(&[hello(
            PROTOCOL_VERSION,
            ConnectionRole::Events,
            vec![EventStream::Status],
        )]);
        serve_connection(
            &mut script,
            &Stub::idle(),
            &publisher,
            &identity(),
            &ShutdownRequest::new(),
        );
        ending.join().expect("the closing thread finished");

        let replies = script.replies();
        assert!(matches!(replies[0], ServerMessage::Welcome(_)));
        match &replies[1] {
            ServerMessage::Event(Event::StatusChanged { status }) => {
                assert_eq!(*status, RecorderStatus::Idle);
            }
            other => panic!("expected an opening status event, got {other:?}"),
        }
    }

    #[test]
    fn an_event_published_while_a_subscriber_is_connected_reaches_it() {
        let publisher = EventPublisher::new();
        let sending = publisher.clone();
        let ending = thread::spawn(move || {
            while sending.subscriber_count() == 0 {
                thread::sleep(std::time::Duration::from_millis(1));
            }
            sending.publish(&Event::RecordingFailed {
                recording_id: "r-1".to_owned(),
                error: ProtocolError::new(ErrorCode::RecordingFailed, "the encoder went away"),
            });
            sending.close();
        });

        let mut script = Scripted::new(&[hello(
            PROTOCOL_VERSION,
            ConnectionRole::Events,
            vec![EventStream::Errors],
        )]);
        serve_connection(
            &mut script,
            &Stub::idle(),
            &publisher,
            &identity(),
            &ShutdownRequest::new(),
        );
        ending.join().expect("the publishing thread finished");

        let replies = script.replies();
        assert!(
            replies
                .iter()
                .any(|reply| matches!(reply, ServerMessage::Event(Event::RecordingFailed { .. }))),
            "the published event should have been written out: {replies:?}"
        );
    }

    #[test]
    fn the_metrics_stream_is_refused_rather_than_accepted_and_never_delivered() {
        // A subscription that is accepted and then silent is a control that
        // does nothing (AGENTS.md section 27). Nothing measures live metrics
        // yet, so the subscription fails and says where they are being built.
        let mut script = Scripted::new(&[hello(
            PROTOCOL_VERSION,
            ConnectionRole::Events,
            vec![EventStream::Status, EventStream::Metrics],
        )]);
        run(&mut script);

        match script.replies().first() {
            Some(ServerMessage::Refused(error)) => {
                assert_eq!(error.code, ErrorCode::NotImplemented);
                assert_eq!(
                    error.detail,
                    Some(ErrorDetail::NotImplemented {
                        subsystem: "live recording metrics".to_owned(),
                        milestone: "M14".to_owned(),
                        tracking_issue: 100,
                    })
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_events_connection_that_asks_for_nothing_is_refused() {
        let mut script =
            Scripted::new(&[hello(PROTOCOL_VERSION, ConnectionRole::Events, Vec::new())]);
        run(&mut script);

        match script.replies().first() {
            Some(ServerMessage::Refused(error)) => {
                assert_eq!(error.code, ErrorCode::InvalidParameters);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn events_reach_the_subscribers_of_their_own_stream_and_nobody_else() {
        let publisher = EventPublisher::new();
        let status = publisher.subscribe(vec![EventStream::Status]);
        let errors = publisher.subscribe(vec![EventStream::Errors]);

        let failure = Event::RecordingFailed {
            recording_id: "r-1".to_owned(),
            error: ProtocolError::new(ErrorCode::RecordingFailed, "the encoder went away"),
        };
        publisher.publish(&failure);

        assert_eq!(
            errors.try_recv().expect("the errors stream gets it"),
            failure
        );
        assert!(
            status.try_recv().is_err(),
            "a status subscriber should not receive an error event"
        );
    }

    #[test]
    fn closing_the_publisher_ends_every_subscription() {
        let publisher = EventPublisher::new();
        let events = publisher.subscribe(vec![EventStream::Status]);
        publisher.close();

        assert!(
            events.recv().is_err(),
            "a closed publisher must let the threads writing its events finish"
        );
    }

    #[test]
    fn a_subscriber_that_stops_reading_loses_events_rather_than_stalling_the_publisher() {
        // The recording thread is a plausible publisher, and it may not wait on
        // a window (AGENTS.md section 20).
        let publisher = EventPublisher::new();
        let _events = publisher.subscribe(vec![EventStream::Status]);

        for _ in 0..EVENT_QUEUE_DEPTH * 2 {
            publisher.publish(&Event::StatusChanged {
                status: RecorderStatus::Idle,
            });
        }
    }
}
