//! The socket a Valve game posts its state to.
//!
//! Game State Integration is an HTTP POST with a JSON body, sent by the game to
//! an address it was configured with. That is the whole protocol, and this
//! module is deliberately the whole of the HTTP that is written to meet it: one
//! request line, a handful of headers, a body of a declared length, and a reply
//! that says nothing (AGENTS.md section 10 — the functionality is small enough
//! to implement safely, and the alternative is a general-purpose HTTP server
//! and an async runtime inside a plugin that accepts one kind of request from
//! one program on the same machine).
//!
//! # What keeps it safe to listen at all
//!
//! `docs/privacy.md`: *"a listening socket bound to loopback is still reachable
//! by every other process on the machine, including a web page in a browser."*
//! Three rules follow, and all three are in this module rather than in a
//! comment somewhere:
//!
//! - **It binds the loopback address explicitly.** Never `0.0.0.0`, which
//!   privacy.md calls "outbound access wearing a disguise". The caller passes a
//!   [`SocketAddr`] and [`GameStateListener::bind`] refuses one that is not
//!   loopback.
//! - **It authenticates every payload.** Valve's mechanism puts a shared token
//!   inside the JSON body, written into the game's configuration file by
//!   whoever configured it (`super::config`). A payload without the token, or
//!   with the wrong one, is answered `403` and never reaches the game logic.
//!   A browser can open a socket to a local port; it cannot read a file out of
//!   the Dota installation, which is what makes the token worth having.
//! - **It bounds everything it reads.** A line, the number of lines, the body,
//!   and the time a connection may take. A local process that opens a
//!   connection and says nothing costs one thread for [`READ_TIMEOUT`] and
//!   nothing else.
//! - **It bounds what it writes, too.** A plugin's standard error is inherited
//!   by the host (`clipped_plugins::process`), so a line printed per refused
//!   request would hand the same local process a way to fill the *host's* log
//!   through a socket it can reach. [`Complaints`] is what makes that one line
//!   per run of the plugin rather than one per request.
//!
//! The token is removed from the payload before it is delivered, so it cannot
//! reach an event payload, a log line or a bug report.

use core::fmt;
use core::time::Duration;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::Instant;

use serde_json::{Map, Value};

use super::secret::AuthToken;

/// The most a request head may carry: one line, and this many of them.
///
/// Generous for what a game sends and small enough that a process which opens a
/// connection and prints rubbish is bounded rather than interesting.
const MAX_HEAD_LINE_BYTES: u64 = 8 * 1024;
/// See [`MAX_HEAD_LINE_BYTES`].
///
/// Every line of the head counts towards it, including the request line and the
/// blank line that ends the head — so a request may carry
/// `MAX_HEAD_LINES - 2` headers. Counting the request line is what makes the
/// number here the number of lines this reader will actually read, rather than
/// one less than it.
const MAX_HEAD_LINES: usize = 64;

/// The most a state payload may carry.
///
/// A Dota 2 payload with the components this plugin subscribes to is a few
/// kilobytes. This is two orders of magnitude above that, so it bounds a
/// program that lies about its content length without being a limit a game
/// could ever reach.
pub const MAX_BODY_BYTES: usize = 512 * 1024;

/// How long one connection may take to say what it came to say.
///
/// The game is on the same machine and writes its payload in one go, so this is
/// long enough to be invisible and short enough that a connection which stalls
/// is reclaimed rather than held. Applied to reading and to writing, because a
/// peer that never reads the reply would otherwise hold the thread just as
/// effectively.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How many payloads may be waiting before the game is made to wait.
///
/// The plugin drains this within microseconds of a payload arriving, and Valve
/// throttles its posting to a configured interval, so this is never reached in
/// practice. It is a bounded queue rather than an unbounded one because an
/// unbounded queue in front of a socket is a memory leak with a network
/// interface (AGENTS.md section 58).
const QUEUE_DEPTH: usize = 16;

/// A state payload as it arrived: the game's own JSON, and when it landed.
#[derive(Debug, Clone)]
pub struct Payload {
    state: Value,
    received: Instant,
}

impl Payload {
    /// The game's state, with the auth token removed.
    #[must_use]
    pub const fn state(&self) -> &Value {
        &self.state
    }

    /// The reading of this process's monotonic clock taken when the payload was
    /// read off the socket.
    ///
    /// This is what an event's position on the recording's timeline is
    /// eventually measured from (`super::cadence`), so it is taken as early as
    /// possible — before the JSON is parsed, not after.
    #[must_use]
    pub const fn received(&self) -> Instant {
        self.received
    }
}

/// A bound loopback socket a Valve game can post state to.
#[derive(Debug)]
pub struct GameStateListener {
    listener: TcpListener,
    token: AuthToken,
}

impl GameStateListener {
    /// Binds `address`, which must be a loopback address.
    ///
    /// # Errors
    ///
    /// [`io::Error`] if the address is already in use — the ordinary reason
    /// being a second copy of this plugin, or the user's own tooling on the
    /// same port — and [`io::ErrorKind::InvalidInput`] if `address` is not
    /// loopback. The second is a programming error rather than a run-time
    /// condition, and it is a refusal rather than a debug assertion because the
    /// cost of getting it wrong is a socket exposed to the local network
    /// (`docs/privacy.md`).
    pub fn bind(address: SocketAddr, token: AuthToken) -> io::Result<Self> {
        if !address.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{address} is not a loopback address, and a game state listener binds nothing \
                     else: a wildcard bind exposes the socket to the local network"
                ),
            ));
        }
        Ok(Self {
            listener: TcpListener::bind(address)?,
            token,
        })
    }

    /// The address it is actually listening on.
    ///
    /// Not the same as the address it was asked for when that named port `0`,
    /// which is what a test binds.
    ///
    /// # Errors
    ///
    /// [`io::Error`] if the socket cannot report its own address.
    pub fn address(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accepts payloads on a thread of its own, and hands them over one at a
    /// time.
    ///
    /// The thread ends when the receiver is dropped or the socket fails, and it
    /// is not joined: a plugin's exit is the process exiting, and a thread
    /// blocked in `accept` cannot be woken without a second socket to wake it
    /// with. That is sound *here* and would not be in the recorder — the whole
    /// reason a plugin is a separate process (`docs/plugin-api.md`) is that its
    /// resources are reclaimed by it ending.
    ///
    /// Connections are served one at a time, deliberately. The game opens one
    /// connection per payload and waits for the reply, so concurrency would buy
    /// nothing; serving sequentially with a read timeout is what bounds the
    /// cost of a local process that connects and then says nothing.
    ///
    /// `reported_as` is what a line of diagnostics on standard error is
    /// attributed to — `"dota 2 plugin"`. It is passed in rather than known
    /// here because nothing in this module knows which game it is serving, and
    /// that is the property that lets the module move to a crate two plugin
    /// binaries link (`super`).
    #[must_use]
    pub fn serve(self, reported_as: &'static str) -> Receiver<Payload> {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_DEPTH);
        thread::spawn(move || self.accept_until_closed(&sender, reported_as));
        receiver
    }

    fn accept_until_closed(self, sender: &SyncSender<Payload>, reported_as: &str) {
        let mut complaints = Complaints::default();
        for connection in self.listener.incoming() {
            let stream = match connection {
                Ok(stream) => stream,
                // One refused connection is not a reason to stop listening: the
                // peer may simply have gone away between the SYN and the
                // accept. A socket that has genuinely failed fails again
                // immediately, and `incoming` ends.
                Err(error) => {
                    complain(
                        reported_as,
                        complaints
                            .unacceptable(&format!("a connection could not be accepted: {error}")),
                    );
                    continue;
                }
            };
            match self.serve_one(stream) {
                Ok(Ok(payload)) => {
                    if sender.send(payload).is_err() {
                        // Nobody is draining any more, which means the plugin
                        // is finishing.
                        return;
                    }
                }
                Ok(Err(refusal)) => complain(
                    reported_as,
                    complaints.refused(&format!("a payload was refused: {refusal}")),
                ),
                Err(error) => complain(
                    reported_as,
                    complaints.unreadable(&format!("a connection ended early: {error}")),
                ),
            }
        }
    }

    /// Reads one connection, answers it, and reports the payload it carried.
    ///
    /// `Ok(Err(refusal))` is a request that was answered and carried nothing
    /// this plugin wants — a refused token, a request that was not a POST. The
    /// distinction between that and `Err` is deliberate: an `Err` is a
    /// connection that could not be *spoken to*, which says nothing about what
    /// it was trying to say.
    fn serve_one(&self, stream: TcpStream) -> io::Result<Result<Payload, Refusal>> {
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(READ_TIMEOUT))?;
        let mut reader = BufReader::new(stream);

        let outcome = read_body(&mut reader).and_then(|body| accept(&body, &self.token));
        // Taken here rather than after the reply is written: the moment the
        // payload describes is the moment it was read, not the moment this
        // plugin finished being polite about it.
        let received = Instant::now();

        let status = match &outcome {
            Ok(_) => 200,
            Err(refusal) => refusal.status(),
        };
        let mut stream = reader.into_inner();
        write_reply(&mut stream, status)?;
        // Both directions are finished with. Shutting down rather than dropping
        // makes that explicit, and a failure to do so is not worth reporting:
        // the peer may already have gone.
        let _ = stream.shutdown(std::net::Shutdown::Both);

        Ok(outcome.map(|state| Payload { state, received }))
    }
}

/// Whether something that went wrong on this socket is still worth a line.
///
/// The first of each kind is: "something on this machine is posting to this
/// port and being refused" is exactly the sentence somebody debugging a
/// misconfigured game wants, and it costs one line. The thousandth is the same
/// sentence for the thousandth time, and a plugin's standard error is inherited
/// by the host — so printing it per request would let any local process fill
/// the host's log by connecting to a port it can reach.
///
/// Kept as three flags rather than one so that a genuine refusal is not
/// silenced by an unrelated accept failure having happened first.
#[derive(Debug, Default)]
pub struct Complaints {
    refused: bool,
    unreadable: bool,
    unacceptable: bool,
}

impl Complaints {
    /// `line` if no payload has been refused yet, and nothing afterwards.
    pub fn refused(&mut self, line: &str) -> Option<String> {
        Self::once(&mut self.refused, line)
    }

    /// `line` if no connection has failed mid-request yet.
    pub fn unreadable(&mut self, line: &str) -> Option<String> {
        Self::once(&mut self.unreadable, line)
    }

    /// `line` if no connection has failed to be accepted yet.
    pub fn unacceptable(&mut self, line: &str) -> Option<String> {
        Self::once(&mut self.unacceptable, line)
    }

    fn once(said: &mut bool, line: &str) -> Option<String> {
        if core::mem::replace(said, true) {
            return None;
        }
        Some(format!("{line}{ONLY_ONCE}"))
    }
}

/// What the one line says about the lines that will not follow it.
///
/// Said in the line itself so that somebody reading a log knows the silence
/// after it is a policy rather than the problem having gone away.
const ONLY_ONCE: &str = " (further occurrences of this on this socket are not reported)";

/// Prints one of [`Complaints`]' lines, if there is one to print.
fn complain(reported_as: &str, line: Option<String>) {
    if let Some(line) = line {
        eprintln!("{reported_as}: {line}");
    }
}

/// Why a request was not accepted.
///
/// Every one of these is answered on the socket and counted nowhere. At most
/// one of them is ever printed, and [`Complaints`] is why: a plugin that logged
/// one line per refused request would hand any local process a way to fill the
/// host's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The request line was not `POST <target> HTTP/1.1`.
    NotAPost,
    /// A line of the head was longer than [`MAX_HEAD_LINE_BYTES`], or there
    /// were more than [`MAX_HEAD_LINES`] of them.
    HeadTooLong,
    /// No `content-length`, or one that is not a number.
    ///
    /// Valve's client always sends one. A body of an undeclared length would
    /// have to be read until the peer closed, which is an unbounded read.
    NoContentLength,
    /// A `transfer-encoding` this reader does not implement.
    Chunked,
    /// A declared body larger than [`MAX_BODY_BYTES`].
    BodyTooLarge {
        /// What the request said it was about to send.
        declared: usize,
    },
    /// The connection ended before the declared body arrived.
    Truncated,
    /// The body was not a JSON object.
    NotAState,
    /// The body carried no `auth.token`, or one that is not the configured
    /// token.
    ///
    /// Reported as one case rather than two on purpose: telling a caller which
    /// half of the credential it got right is a courtesy owed to a game, not to
    /// whatever else on this machine has found the port.
    Unauthenticated,
}

impl Refusal {
    /// The HTTP status this is answered with.
    #[must_use]
    pub const fn status(&self) -> u16 {
        match self {
            Self::NotAPost => 405,
            Self::HeadTooLong | Self::NoContentLength | Self::Truncated | Self::NotAState => 400,
            Self::Chunked => 501,
            Self::BodyTooLarge { .. } => 413,
            Self::Unauthenticated => 403,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAPost => formatter.write_str("it was not a POST"),
            Self::HeadTooLong => {
                formatter.write_str("its head was longer than this reader accepts")
            }
            Self::NoContentLength => {
                formatter.write_str("it declared no content-length, so its body has no end")
            }
            Self::Chunked => formatter.write_str("it was chunked, which this reader does not read"),
            Self::BodyTooLarge { declared } => write!(
                formatter,
                "it declared {declared} bytes, and this reader accepts {MAX_BODY_BYTES}"
            ),
            Self::Truncated => formatter.write_str("it ended before the body it declared"),
            Self::NotAState => formatter.write_str("its body was not a JSON object"),
            Self::Unauthenticated => {
                formatter.write_str("it did not carry the token this listener was configured with")
            }
        }
    }
}

impl core::error::Error for Refusal {}

/// Reads a request and returns its body.
///
/// Separated from the socket so that every shape of a malformed request is a
/// test over a `Cursor` rather than something that needs a peer.
///
/// # Errors
///
/// [`Refusal`], which the caller answers with [`Refusal::status`].
pub fn read_body(reader: &mut impl BufRead) -> Result<Vec<u8>, Refusal> {
    let request_line = read_head_line(reader)?;
    if !request_line
        .split(' ')
        .next()
        .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
    {
        return Err(Refusal::NotAPost);
    }

    let mut length = None;
    // From one, because the request line above was the first of them.
    for _ in 1..MAX_HEAD_LINES {
        let line = read_head_line(reader)?;
        if line.is_empty() {
            let length = length.ok_or(Refusal::NoContentLength)?;
            return read_exactly(reader, length);
        }
        let Some((name, value)) = line.split_once(':') else {
            // A header this reader cannot even split is not a header. Ignoring
            // it rather than refusing keeps the reader from being stricter than
            // the thing it exists to read.
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let declared: usize = value.parse().map_err(|_| Refusal::NoContentLength)?;
            if declared > MAX_BODY_BYTES {
                return Err(Refusal::BodyTooLarge { declared });
            }
            length = Some(declared);
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && !value.eq_ignore_ascii_case("identity")
        {
            return Err(Refusal::Chunked);
        }
    }
    Err(Refusal::HeadTooLong)
}

/// One line of the request head, without its line ending.
fn read_head_line(reader: &mut impl BufRead) -> Result<String, Refusal> {
    let mut line = Vec::new();
    let read = reader
        .by_ref()
        .take(MAX_HEAD_LINE_BYTES)
        .read_until(b'\n', &mut line)
        .map_err(|_| Refusal::Truncated)?;
    if read == 0 {
        return Err(Refusal::Truncated);
    }
    if line.last() != Some(&b'\n') {
        // Either the peer stopped mid-line or the line is longer than this
        // reader accepts. Both are answered the same way, and neither is worth
        // reading more of to distinguish.
        return Err(Refusal::HeadTooLong);
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| Refusal::HeadTooLong)
}

/// Exactly `length` bytes, or [`Refusal::Truncated`].
fn read_exactly(reader: &mut impl BufRead, length: usize) -> Result<Vec<u8>, Refusal> {
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| Refusal::Truncated)?;
    Ok(body)
}

/// Checks a body's token and returns the state with the token removed.
///
/// # Errors
///
/// [`Refusal::NotAState`] for a body that is not a JSON object, and
/// [`Refusal::Unauthenticated`] for one whose `auth.token` is absent or wrong.
pub fn accept(body: &[u8], token: &AuthToken) -> Result<Value, Refusal> {
    let mut state: Map<String, Value> =
        serde_json::from_slice(body).map_err(|_| Refusal::NotAState)?;

    // Removed rather than read, so that the credential cannot reach an event
    // payload, a log line or a bug report from here. Everything downstream sees
    // the state and no token at all.
    let presented = state
        .remove("auth")
        .and_then(|auth| match auth {
            Value::Object(mut auth) => auth.remove("token"),
            _ => None,
        })
        .and_then(|presented| match presented {
            Value::String(presented) => Some(presented),
            _ => None,
        })
        .ok_or(Refusal::Unauthenticated)?;

    if !token.matches(&presented) {
        return Err(Refusal::Unauthenticated);
    }
    Ok(Value::Object(state))
}

/// Answers a request. The game ignores the body, so there is not one.
fn write_reply(stream: &mut TcpStream, status: u16) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Not Implemented",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::net::{Ipv4Addr, SocketAddr};

    use serde_json::json;

    use super::*;

    fn token() -> AuthToken {
        AuthToken::parse("abcdefghijklmnopqrstuvwx").expect("a well-formed token")
    }

    fn request(body: &str) -> Cursor<Vec<u8>> {
        Cursor::new(
            format!(
                "POST / HTTP/1.1\r\nhost: 127.0.0.1:3213\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
        )
    }

    #[test]
    fn a_post_with_the_right_token_delivers_the_state_without_it() {
        let body = json!({"auth": {"token": "abcdefghijklmnopqrstuvwx"}, "map": {"matchid": "1"}})
            .to_string();
        let read = read_body(&mut request(&body)).expect("the request reads");
        let state = accept(&read, &token()).expect("the token matches");

        assert_eq!(state, json!({"map": {"matchid": "1"}}));
        assert!(
            !state.to_string().contains("abcdefghijklmnopqrstuvwx"),
            "the credential must not survive into anything downstream can see: {state}"
        );
    }

    #[test]
    fn a_payload_without_the_token_is_refused_rather_than_trusted() {
        // The rule `docs/privacy.md` states: a loopback listener is reachable
        // by every process on this machine, so what it accepts is what it can
        // authenticate. Each of these is something a web page could send.
        for body in [
            json!({"map": {"matchid": "1"}}),
            json!({"auth": {}, "map": {}}),
            json!({"auth": {"token": "not-the-token"}, "map": {}}),
            json!({"auth": "abcdefghijklmnopqrstuvwx", "map": {}}),
        ] {
            let read = read_body(&mut request(&body.to_string())).expect("the request reads");
            assert_eq!(
                accept(&read, &token()),
                Err(Refusal::Unauthenticated),
                "this should not have been accepted: {body}"
            );
        }
        assert_eq!(Refusal::Unauthenticated.status(), 403);
    }

    #[test]
    fn a_request_that_is_not_a_state_post_is_answered_and_dropped() {
        let mut get = Cursor::new(b"GET / HTTP/1.1\r\nhost: x\r\n\r\n".to_vec());
        assert_eq!(read_body(&mut get), Err(Refusal::NotAPost));

        let mut no_length = Cursor::new(b"POST / HTTP/1.1\r\nhost: x\r\n\r\n{}".to_vec());
        assert_eq!(read_body(&mut no_length), Err(Refusal::NoContentLength));

        let mut chunked = Cursor::new(
            b"POST / HTTP/1.1\r\ntransfer-encoding: chunked\r\ncontent-length: 2\r\n\r\n{}"
                .to_vec(),
        );
        assert_eq!(read_body(&mut chunked), Err(Refusal::Chunked));

        let mut short = Cursor::new(b"POST / HTTP/1.1\r\ncontent-length: 64\r\n\r\n{}".to_vec());
        assert_eq!(read_body(&mut short), Err(Refusal::Truncated));

        let not_json = read_body(&mut request("not json at all")).expect("the request reads");
        assert_eq!(accept(&not_json, &token()), Err(Refusal::NotAState));
    }

    #[test]
    fn nothing_a_peer_declares_makes_this_reader_allocate_without_bound() {
        // A body larger than the limit is refused on the strength of the
        // header, before a byte of it is read — and before `vec![0; length]`
        // has been asked for a gigabyte.
        let declared = usize::try_from(u32::MAX).expect("a 32-bit length fits a usize here");
        let mut enormous = Cursor::new(
            format!("POST / HTTP/1.1\r\ncontent-length: {declared}\r\n\r\n").into_bytes(),
        );
        assert_eq!(
            read_body(&mut enormous),
            Err(Refusal::BodyTooLarge { declared })
        );

        // The numbers below are the documented bounds written out, rather than
        // the constants that set them. A test that reads the constant it is
        // checking moves with it: this one agreed just as happily with a head
        // of four thousand lines until the mutation that raised the limit was
        // tried against it and nothing failed.
        assert_eq!(MAX_HEAD_LINE_BYTES, 8 * 1024);
        assert_eq!(MAX_HEAD_LINES, 64);
        assert_eq!(MAX_BODY_BYTES, 512 * 1024);

        let mut unterminated = Cursor::new(vec![b'x'; 16 * 1024]);
        assert_eq!(read_body(&mut unterminated), Err(Refusal::HeadTooLong));

        // The bound is the number of lines this reader will read, and the
        // request line is one of them. Asserted at the boundary rather than
        // well past it, because a head of sixty-five lines is refused by a
        // reader that stops at sixty-four and by one that stops at sixty-five,
        // and only one of those matches what the constant says.
        //
        //   request line + `filler` fillers + content-length + blank line
        let head = |filler: usize| {
            Cursor::new(
                ("POST / HTTP/1.1\r\n".to_owned()
                    + &"x: y\r\n".repeat(filler)
                    + "content-length: 0\r\n\r\n")
                    .into_bytes(),
            )
        };
        assert_eq!(
            read_body(&mut head(61)),
            Ok(Vec::new()),
            "sixty-four lines of head is the most, and this is exactly that"
        );
        assert_eq!(
            read_body(&mut head(62)),
            Err(Refusal::HeadTooLong),
            "sixty-five is one too many"
        );
    }

    #[test]
    fn a_local_process_cannot_fill_the_hosts_log_by_being_refused() {
        // `Refusal`'s own documentation is what this holds to: a refusal is
        // answered on the socket and counted nowhere. A plugin's standard error
        // is inherited by the host (`clipped_plugins::process`), so one line
        // per refused request would be a log file any process on this machine
        // could fill through a port it is allowed to reach.
        let mut complaints = Complaints::default();
        let lines: Vec<String> = (0..10_000)
            .filter_map(|_| complaints.refused("a payload was refused: it was not a POST"))
            .collect();

        assert_eq!(
            lines.len(),
            1,
            "ten thousand refusals are worth one line, and this was worth {}",
            lines.len()
        );
        assert!(
            lines[0].contains("it was not a POST"),
            "the one line still says what happened: {}",
            lines[0]
        );
        assert!(
            lines[0].contains("not reported"),
            "and says that the silence after it is a policy: {}",
            lines[0]
        );

        // One flag per kind, so a connection that could not be accepted does
        // not use up the line a refused payload would have had.
        let mut complaints = Complaints::default();
        assert!(complaints.unacceptable("could not accept").is_some());
        assert!(complaints.refused("refused").is_some());
        assert!(complaints.unreadable("ended early").is_some());
        assert!(complaints.unacceptable("could not accept").is_none());
        assert!(complaints.refused("refused").is_none());
        assert!(complaints.unreadable("ended early").is_none());
    }

    #[test]
    fn a_listener_binds_loopback_and_refuses_anything_else() {
        // The rule that keeps the socket off the local network. It is a refusal
        // rather than a comment because `docs/privacy.md` treats a wildcard
        // bind as an outbound-class change.
        let wildcard = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        let refused = GameStateListener::bind(wildcard, token()).expect_err("0.0.0.0 is refused");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);

        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let listener = GameStateListener::bind(loopback, token()).expect("loopback binds");
        assert!(listener.address().expect("an address").ip().is_loopback());
    }

    #[test]
    fn a_state_posted_to_the_socket_arrives_on_the_channel() {
        // The one test that uses a real socket, on an ephemeral port so that it
        // cannot collide with anything else on this machine. Everything else
        // about the reader is exercised above without one.
        let listener = GameStateListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), token())
            .expect("loopback binds");
        let address = listener.address().expect("an address");
        let payloads = listener.serve("game state listener test");

        post(
            address,
            &json!({"auth": {"token": "not-the-token"}, "map": {}}).to_string(),
        );
        post(
            address,
            &json!({"auth": {"token": "abcdefghijklmnopqrstuvwx"}, "map": {"matchid": "77"}})
                .to_string(),
        );

        let payload = payloads
            .recv_timeout(Duration::from_secs(10))
            .expect("the authenticated payload arrives");
        assert_eq!(
            payload.state(),
            &json!({"map": {"matchid": "77"}}),
            "the refused payload must not have been delivered, and the token must not have \
             survived the one that was"
        );
    }

    fn post(address: SocketAddr, body: &str) {
        let mut stream = TcpStream::connect(address).expect("the listener accepts a connection");
        write!(
            stream,
            "POST / HTTP/1.1\r\nhost: {address}\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .expect("the request is written");
        stream.flush().expect("the request is flushed");
        let mut reply = String::new();
        stream
            .read_to_string(&mut reply)
            .expect("the listener replies");
        assert!(reply.starts_with("HTTP/1.1 "), "unexpected reply: {reply}");
    }
}
