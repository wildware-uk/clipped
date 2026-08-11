//! The other end: connecting to a recorder and driving it.
//!
//! This is what the desktop application's Rust side uses, and what the
//! recorder's own tests drive a real process with. It exists in this crate
//! rather than being written twice because a second implementation of a
//! handshake is a second set of bugs, and because the client is the only honest
//! test of the server (AGENTS.md section 55).
//!
//! # Two clients, because there are two kinds of connection
//!
//! [`Client`] sends commands and waits for their replies. [`EventClient`]
//! subscribes and reads events until the recorder stops or the subscription
//! ends. A caller that wants both opens both, which is the shape the transport
//! wants — see the [`message`](crate::message) module documentation.
//!
//! # Blocking
//!
//! Both block. [`Client::call`] blocks until the reply arrives, and
//! [`EventClient::next_event`] blocks until an event does. Neither belongs on a
//! thread that is drawing a window; the desktop application will run them on a
//! thread of their own and hand the results to its UI, which is the ordinary
//! shape for a Tauri host.

use std::time::Duration;

use crate::command::{Command, Reply};
use crate::error::ProtocolError;
use crate::frame::{read_message, write_message, FrameError};
use crate::message::{
    ClientMessage, ConnectionRole, Event, EventStream, Hello, Outcome, PeerIdentity, Request,
    ServerMessage, Welcome, PROTOCOL_VERSION,
};
use crate::transport::{connect, Connection, Endpoint, TransportError};

/// Why talking to the recorder did not work.
#[derive(Debug)]
pub enum ClientError {
    /// The connection could not be made.
    Transport(TransportError),
    /// The connection failed, or the recorder went away.
    Frame(FrameError),
    /// The recorder said no. For a handshake that is a version or a role it
    /// will not accept; for a command it is the command's own refusal, which
    /// the desktop application renders.
    Refused(ProtocolError),
    /// The recorder sent something that does not belong here.
    ///
    /// A protocol bug on one side or the other, and worth telling apart from a
    /// refusal: a refusal is the recorder working correctly.
    Unexpected(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "{error}"),
            Self::Frame(error) => write!(formatter, "{error}"),
            Self::Refused(error) => write!(formatter, "{error}"),
            Self::Unexpected(what) => write!(formatter, "the recorder sent {what}"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Refused(error) => Some(error),
            Self::Unexpected(_) => None,
        }
    }
}

impl From<TransportError> for ClientError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<FrameError> for ClientError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

/// A control connection: commands out, replies back.
#[derive(Debug)]
pub struct Client {
    connection: Connection,
    welcome: Welcome,
    next_id: u64,
}

impl Client {
    /// Connects and completes the handshake.
    ///
    /// `name` and `version` identify the caller in the recorder's log and in
    /// the message a user sees when the two builds disagree.
    ///
    /// # Errors
    ///
    /// [`ClientError::Transport`] if no recorder is listening,
    /// [`ClientError::Refused`] if it will not accept this protocol version —
    /// carrying the versions it does speak — and [`ClientError::Frame`] if the
    /// connection failed part way through.
    pub fn connect(
        endpoint: &Endpoint,
        name: &str,
        version: &str,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let mut connection = connect(endpoint, timeout)?;
        let welcome = handshake(
            &mut connection,
            name,
            version,
            ConnectionRole::Control,
            Vec::new(),
        )?;

        Ok(Self {
            connection,
            welcome,
            next_id: 1,
        })
    }

    /// What the recorder said about itself when it accepted the connection.
    ///
    /// [`Welcome::features`] is what a client checks before offering a control,
    /// rather than inferring from a version number what a build contains.
    #[must_use]
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Which process is serving this connection.
    ///
    /// Asked of the pipe rather than of anything this process remembers, so it
    /// names the recorder actually on the other end. A supervisor uses it to
    /// tell a recorder it started from one that was already there
    /// ([`crate::supervisor`]); it is not authentication, and
    /// [`Connection::server_process_id`] says why.
    ///
    /// # Errors
    ///
    /// Whatever Windows said, most plausibly because the recorder has closed
    /// the connection.
    pub fn recorder_process_id(&self) -> std::io::Result<u32> {
        self.connection.server_process_id()
    }

    /// Sends a command and waits for its reply.
    ///
    /// # Errors
    ///
    /// [`ClientError::Refused`] carries the recorder's own refusal, which is
    /// the normal outcome for a command whose subsystem is not in that build.
    /// [`ClientError::Frame`] means the recorder went away;
    /// [`ClientError::Unexpected`] means it answered something else.
    pub fn call(&mut self, command: &Command) -> Result<Reply, ClientError> {
        let id = self.next_id;
        self.next_id += 1;

        let request = command.to_request(id).map_err(ClientError::Refused)?;
        self.exchange(&request)
    }

    /// Sends a request built by hand.
    ///
    /// For a caller that has to send something [`Command`] cannot express — a
    /// command name this build has never heard of, or parameters of the wrong
    /// shape. That is a test's job rather than an application's, and it is
    /// public because the rejection paths have to be exercised against a real
    /// recorder rather than against a mock of one.
    ///
    /// # Errors
    ///
    /// As [`call`](Self::call).
    pub fn call_raw(
        &mut self,
        command: &str,
        params: serde_json::Value,
    ) -> Result<Reply, ClientError> {
        let id = self.next_id;
        self.next_id += 1;

        self.exchange(&Request {
            id,
            command: command.to_owned(),
            params,
        })
    }

    /// Writes a request and reads the reply that quotes it.
    fn exchange(&mut self, request: &Request) -> Result<Reply, ClientError> {
        write_message(
            &mut self.connection,
            &ClientMessage::Request(request.clone()),
        )?;

        match read_message::<_, ServerMessage>(&mut self.connection)? {
            ServerMessage::Response(response) if response.id == request.id => {
                match response.outcome {
                    Outcome::Ok(reply) => Ok(reply),
                    Outcome::Error(error) => Err(ClientError::Refused(error)),
                }
            }
            ServerMessage::Response(response) => Err(ClientError::Unexpected(format!(
                "a reply to request {} while {} was outstanding",
                response.id, request.id
            ))),
            ServerMessage::Refused(error) => Err(ClientError::Refused(error)),
            ServerMessage::Welcome(_) => {
                Err(ClientError::Unexpected("a second handshake".to_owned()))
            }
            ServerMessage::Event(_) => Err(ClientError::Unexpected(
                "an event on a control connection".to_owned(),
            )),
        }
    }
}

/// An event connection: the recorder writes, this reads.
#[derive(Debug)]
pub struct EventClient {
    connection: Connection,
    welcome: Welcome,
}

impl EventClient {
    /// Connects, subscribes, and is ready to read events.
    ///
    /// A `status` subscription's first event is the recorder's current state,
    /// so there is no window in which a freshly attached client knows nothing.
    ///
    /// # Errors
    ///
    /// [`ClientError::Refused`] if the recorder will not deliver one of the
    /// streams asked for — which is what a request for `metrics` gets in this
    /// build — as well as the failures [`Client::connect`] can produce.
    pub fn subscribe(
        endpoint: &Endpoint,
        name: &str,
        version: &str,
        streams: Vec<EventStream>,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let mut connection = connect(endpoint, timeout)?;
        let welcome = handshake(
            &mut connection,
            name,
            version,
            ConnectionRole::Events,
            streams,
        )?;

        Ok(Self {
            connection,
            welcome,
        })
    }

    /// The streams the recorder agreed to deliver.
    #[must_use]
    pub fn streams(&self) -> &[EventStream] {
        &self.welcome.streams
    }

    /// Blocks until the next event arrives.
    ///
    /// # Errors
    ///
    /// [`ClientError::Frame`] when the recorder closes the connection, which is
    /// how a subscription ends; [`FrameError::is_disconnect`] tells that from a
    /// failure.
    pub fn next_event(&mut self) -> Result<Event, ClientError> {
        match read_message::<_, ServerMessage>(&mut self.connection)? {
            ServerMessage::Event(event) => Ok(event),
            ServerMessage::Refused(error) => Err(ClientError::Refused(error)),
            other => Err(ClientError::Unexpected(format!(
                "{other:?} on an event connection"
            ))),
        }
    }
}

/// Sends the handshake and reads the answer.
fn handshake(
    connection: &mut Connection,
    name: &str,
    version: &str,
    role: ConnectionRole,
    streams: Vec<EventStream>,
) -> Result<Welcome, ClientError> {
    let hello = ClientMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        client: PeerIdentity {
            name: name.to_owned(),
            version: version.to_owned(),
        },
        role,
        streams,
    });
    write_message(connection, &hello)?;

    match read_message::<_, ServerMessage>(connection)? {
        ServerMessage::Welcome(welcome) => Ok(welcome),
        ServerMessage::Refused(error) => Err(ClientError::Refused(error)),
        other => Err(ClientError::Unexpected(format!(
            "{other:?} in answer to a handshake"
        ))),
    }
}
