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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::command::{Command, Reply};
use crate::error::{ErrorCode, ErrorDetail, ProtocolError};
use crate::frame::{read_message, write_message, FrameError};
use crate::message::{
    ClientMessage, ConnectionRole, Event, EventStream, Hello, Outcome, PeerIdentity, Request,
    Response, ServerMessage, Welcome, SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::status::RecorderStatus;
use crate::transport::{Listener, TransportError};

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
        }
    }

    /// Accepts and serves connections until the listener is stopped.
    ///
    /// # Errors
    ///
    /// [`ServerError::Transport`] if accepting fails. A failure on one
    /// *connection* is not an error here: it closes that connection and the
    /// loop carries on, because a client that misbehaved must not be able to
    /// stop the recorder serving anybody else.
    pub fn serve(&self, listener: &mut Listener) -> Result<(), ServerError> {
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
            let live = Arc::clone(&self.live_connections);
            live.fetch_add(1, Ordering::SeqCst);

            let spawned = thread::Builder::new()
                .name("clipped-ipc-connection".to_owned())
                .spawn(move || {
                    let _counted = ConnectionCount(live);
                    let mut connection = connection;
                    serve_connection(&mut connection, &*handler, &events, &identity);
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
            serve_commands(stream, handler);
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
        features: handler.features(),
        streams,
    };
    let _ = write_message(stream, &ServerMessage::Welcome(welcome));
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
fn serve_commands<S: Read + Write, H: CommandHandler + ?Sized>(stream: &mut S, handler: &H) {
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

        let response = Response {
            id: request.id,
            outcome: dispatch(handler, &request),
        };

        if let Err(error) = write_message(stream, &ServerMessage::Response(response)) {
            // The client that asked has gone before the answer arrived. Normal
            // when a window is closed mid-request, and nothing to recover.
            tracing::debug!(%error, "a reply could not be delivered");
            return;
        }
    }
}

/// Performs one request, or explains why not.
fn dispatch<H: CommandHandler + ?Sized>(handler: &H, request: &Request) -> Outcome {
    match Command::from_request(request) {
        Err(refusal) => Outcome::Error(refusal),
        // Refused here, and never handed to a handler: there is no
        // implementation to reach, and a handler that could be asked is a
        // handler that could answer wrongly (AGENTS.md section 54).
        Ok(Command::Unbuilt(command)) => {
            tracing::debug!(
                command = command.name(),
                issue = command.tracking_issue(),
                "refused a command whose subsystem is not in this build"
            );
            Outcome::Error(command.refusal())
        }
        Ok(command) => match handler.call(command) {
            Ok(reply) => Outcome::Ok(reply),
            Err(refusal) => Outcome::Error(refusal),
        },
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
    #[derive(Debug)]
    struct Stub;

    impl CommandHandler for Stub {
        fn call(&self, command: Command) -> Result<Reply, ProtocolError> {
            match command {
                Command::Ping => Ok(Reply::Pong),
                Command::GetStatus => Ok(Reply::Status {
                    status: RecorderStatus::Idle,
                }),
                other => Err(ProtocolError::new(
                    ErrorCode::Internal,
                    format!("this stub does not do `{}`", other.name()),
                )),
            }
        }

        fn status(&self) -> RecorderStatus {
            RecorderStatus::Idle
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
        serve_connection(script, &Stub, &EventPublisher::new(), &identity());
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
        serve_connection(&mut script, &Stub, &publisher, &identity());
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
        serve_connection(&mut script, &Stub, &publisher, &identity());
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
