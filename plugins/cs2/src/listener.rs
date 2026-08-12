//! The loopback endpoint Counter-Strike 2 posts to.
//!
//! One socket, bound to `127.0.0.1`, accepting `POST` requests whose body is a
//! Game State Integration payload. It is declared in `plugin.json`, rendered as
//! the sentence a user is meant to read before enabling the plugin
//! (`clipped_plugins::NetworkAccess`; no screen shows it yet,
//! [issue #281](https://github.com/wildware-uk/clipped/issues/281)), and
//! written down in `docs/privacy.md`.
//!
//! # Loopback is not the same as safe
//!
//! `docs/privacy.md` is explicit about this, and it is the reason this module
//! has an authentication step at all:
//!
//! > A **listening** socket bound to loopback is still reachable by every other
//! > process on the machine, including a web page in a browser. Anything
//! > Clipped listens on must therefore require a shared secret or token that
//! > the local producer was configured with … and must reject unauthenticated
//! > payloads rather than trusting whatever arrives.
//!
//! So the token in the configuration file `crate::integration` wrote is checked
//! on **every** payload, before it reaches the derivation and before anything
//! about it is believed. A payload without the token is not a payload from
//! Counter-Strike, whatever it says about itself.
//!
//! Two further rules follow from the same sentence:
//!
//! - The socket binds `127.0.0.1` explicitly and never `0.0.0.0`. Binding the
//!   wildcard address would expose it to the local network, which
//!   `docs/privacy.md` calls "an outbound-class change wearing a disguise".
//! - Everything about a request is bounded before it is read: the header block
//!   ([`MAX_HEADER_BYTES`]), the body ([`MAX_BODY_BYTES`]), how long one read
//!   may wait ([`READ_TIMEOUT`]) and how long the whole connection may last
//!   ([`CONNECTION_DEADLINE`]). A local port that anything can connect to is a
//!   local port anything can hold open.
//!
//! # One connection at a time, so none of them may last
//!
//! Payloads are accepted on one thread, one connection at a time, because
//! Counter-Strike posts one at a time and a queue of one is the whole of the
//! concurrency this needs. The cost of that choice is that a connection which
//! never finishes is not one slow request — it is the endpoint not working, and
//! the game's payloads sitting in the accept backlog behind it.
//!
//! [`READ_TIMEOUT`] does not bound that on its own. It bounds a client that
//! goes *silent*; a client that sends one byte before each timeout expires is
//! talking, and can hold the socket for as long as it likes. So the connection
//! as a whole is given an allowance, checked before every read, after which it
//! is dropped. The worst one connection can cost is therefore
//! [`CONNECTION_DEADLINE`] plus one [`READ_TIMEOUT`], for a read already in
//! flight when the allowance ran out.
//!
//! # Why there is no HTTP dependency
//!
//! AGENTS.md section 10 asks whether the functionality is small enough to
//! implement safely, and the answer here is unusually clear: this speaks to one
//! client, which sends `POST` with a `Content-Length` and reads a status line.
//! It serves no files, has no routes, runs no middleware and answers no method
//! but `POST`. A general HTTP server would be several thousand lines of
//! somebody else's code — and its own security surface — inside a process whose
//! entire job is to receive a JSON blob from a game.

use core::fmt;
use core::time::Duration;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::time::Instant;

/// The most a request's header block may weigh.
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// The most a payload may weigh.
///
/// A Game State Integration payload subscribing to everything is a few
/// kilobytes; this one subscribes to six blocks. The bound is generous and it
/// is still a bound, because the process on the other end of a loopback port is
/// not necessarily the game.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// How long one read may wait.
///
/// This bounds a connection that goes quiet, and only that. A connection that
/// keeps talking without ever finishing resets it every time, which is what
/// [`CONNECTION_DEADLINE`] is for.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The whole of the time one connection may take.
///
/// Counter-Strike posts a few kilobytes over loopback and is gone, so this is
/// enormously generous to the game and short against anything else on the
/// machine that has found the port. Once it is spent the connection is dropped
/// mid-request, because the alternative — waiting to be told the request is
/// over — is waiting on a stranger.
const CONNECTION_DEADLINE: Duration = Duration::from_secs(10);

/// A payload that arrived and proved it came from the game.
#[derive(Debug)]
pub struct ReceivedPayload {
    /// The body, as posted.
    pub body: Vec<u8>,
    /// When this process took delivery of it. The derivation places events
    /// between two of these (`crate::derive`).
    pub received: Instant,
}

/// The socket Counter-Strike 2 posts to.
#[derive(Debug)]
pub struct GsiListener {
    socket: TcpListener,
    /// What each connection is allowed, in total. [`CONNECTION_DEADLINE`] in
    /// anything that ships; shorter only in the test that would otherwise have
    /// to wait it out.
    allowance: Duration,
    /// What one read of a connection is allowed, which is always
    /// [`READ_TIMEOUT`]. It is a field rather than a constant read inside
    /// [`handle`] so that the value the plugin ships with is something a test
    /// can assert, and so that the two bounds a connection gets arrive at
    /// `handle` the same way.
    read_timeout: Duration,
}

impl GsiListener {
    /// Binds the loopback address, and only the loopback address.
    ///
    /// Port `0` asks the operating system for a free one, which is what the
    /// tests use; the plugin proper binds the port its configuration file told
    /// the game about.
    ///
    /// # Errors
    ///
    /// The `io::Error`, which for a port another program already holds is
    /// exactly what the user needs to be told.
    pub fn bind(port: u16) -> io::Result<Self> {
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
        TcpListener::bind(address).map(|socket| Self {
            socket,
            allowance: CONNECTION_DEADLINE,
            read_timeout: READ_TIMEOUT,
        })
    }

    /// The same socket, with a shorter allowance per connection.
    ///
    /// Test-only, and deliberately not part of the API: the plugin gets
    /// [`CONNECTION_DEADLINE`] and nothing chooses otherwise. It exists so that
    /// the test which proves a stalled connection is dropped can prove it in
    /// under a second instead of waiting ten out.
    #[cfg(test)]
    fn allowing(self, allowance: Duration) -> Self {
        Self { allowance, ..self }
    }

    /// The port actually bound.
    ///
    /// # Errors
    ///
    /// The `io::Error` from asking the socket.
    pub fn port(&self) -> io::Result<u16> {
        self.socket.local_addr().map(|address| address.port())
    }

    /// Accepts payloads until the channel's receiver is gone.
    ///
    /// Runs on a thread of its own, one connection at a time. It never blocks
    /// the plugin's main loop and the main loop never waits for it: a game that
    /// stops posting is a channel that goes quiet, which is what the heartbeat
    /// in `crate::main` is for. No connection can hold this loop for longer
    /// than the module documentation's bound, which is what stops a stranger on
    /// the port from being the same thing as a game that stopped posting.
    ///
    /// `on_refusal` is called with each refused request so that the caller can
    /// count and report it; it is deliberately not a log line written here,
    /// because this module has no opinion about where diagnostics go.
    pub fn serve(
        &self,
        token: &str,
        payloads: &Sender<ReceivedPayload>,
        mut on_refusal: impl FnMut(&Refusal),
    ) {
        for connection in self.socket.incoming() {
            let Ok(stream) = connection else {
                // A connection that failed before it existed is not worth
                // stopping for; the next one may be the game.
                continue;
            };
            match handle(stream, token, self.allowance, self.read_timeout) {
                Ok(payload) => {
                    if payloads.send(payload).is_err() {
                        // Nobody is reading any more: the session has ended.
                        return;
                    }
                }
                Err(refusal) => on_refusal(&refusal),
            }
        }
    }
}

/// Reads one request and answers it.
///
/// `allowance` is the whole of the time this connection may take and
/// `read_timeout` is the most one read of it may wait; see the module
/// documentation for why the second is not the first.
fn handle(
    mut stream: TcpStream,
    token: &str,
    allowance: Duration,
    read_timeout: Duration,
) -> Result<ReceivedPayload, Refusal> {
    // Both directions are bounded before a byte is read, and a socket that will
    // not take a timeout is one this endpoint declines to read from: the
    // allowance below is checked between reads and cannot interrupt one that is
    // already blocked, so without a timeout underneath it there is no bound at
    // all.
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|_| Refusal::Unreadable)?;
    stream
        .set_write_timeout(Some(read_timeout))
        .map_err(|_| Refusal::Unreadable)?;

    let reading = stream.try_clone().map_err(|_| Refusal::Unreadable)?;
    let outcome = read_post(&mut BufReader::new(Deadline::starting_now(
        reading, allowance,
    )));
    let received = Instant::now();

    let answer = match &outcome {
        Ok(body) if carries(body, token) => "HTTP/1.1 200 OK",
        Ok(_) => "HTTP/1.1 403 Forbidden",
        Err(RequestError::TooLarge) => "HTTP/1.1 413 Payload Too Large",
        Err(RequestError::Stalled) => "HTTP/1.1 408 Request Timeout",
        Err(_) => "HTTP/1.1 400 Bad Request",
    };
    // Answered whatever happened, and the connection closed: Counter-Strike
    // opens a new one for every post, and a reply it never gets is a `timeout`
    // it waits out before sending the next.
    let _ = stream.write_all(
        format!("{answer}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
    );
    let _ = stream.flush();

    match outcome {
        Ok(body) if carries(&body, token) => Ok(ReceivedPayload { body, received }),
        Ok(_) => Err(Refusal::Unauthenticated),
        Err(error) => Err(Refusal::Malformed { error }),
    }
}

/// A reader that stops once the connection has spent its allowance.
///
/// The check happens *before* each read rather than interrupting one in
/// flight, which is why the socket underneath it must also have a read timeout:
/// together they bound the connection at the allowance plus one
/// [`READ_TIMEOUT`]. Nothing here cancels anything — the caller drops the
/// connection when this refuses, and dropping it is what ends the conversation.
struct Deadline<R> {
    inner: R,
    expires: Instant,
}

impl<R> Deadline<R> {
    /// Gives `inner` `allowance` from this moment.
    fn starting_now(inner: R, allowance: Duration) -> Self {
        Self {
            inner,
            expires: Instant::now() + allowance,
        }
    }
}

impl<R: Read> Read for Deadline<R> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if Instant::now() >= self.expires {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the connection took longer than this endpoint allows one to take",
            ));
        }
        self.inner.read(into)
    }
}

/// Whether an `io::Error` is a read that ran out of time.
///
/// Either the socket's own [`READ_TIMEOUT`] or [`Deadline`]: both mean the
/// connection stopped being worth waiting for, and neither is a socket that
/// failed. Windows reports the first as `TimedOut`; `WouldBlock` is here
/// because a timed-out read is permitted to report either.
fn timed_out(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

/// Whether a body carries the token the configuration file gave the game.
///
/// The comparison is over the whole token and does not stop at the first
/// differing byte. A token is short and this is not a serious side channel, but
/// writing the loop that returns early would be writing a worse comparison for
/// no gain.
fn carries(body: &[u8], token: &str) -> bool {
    let Ok(payload) = crate::payload::GsiPayload::parse(body) else {
        return false;
    };
    let Some(presented) = payload.token() else {
        return false;
    };
    let presented = presented.as_bytes();
    let expected = token.as_bytes();
    if presented.len() != expected.len() {
        return false;
    }
    presented
        .iter()
        .zip(expected)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

/// Reads the body of a `POST`, or says why it did not.
///
/// Separated from the socket so that every case below is a test over a byte
/// slice rather than something that needs a network.
///
/// # Errors
///
/// [`RequestError`], naming what was wrong with the request.
pub fn read_post(source: &mut impl BufRead) -> Result<Vec<u8>, RequestError> {
    let mut request_line = String::new();
    read_line(source, &mut request_line)?;
    if !request_line.starts_with("POST ") {
        return Err(RequestError::NotAPost);
    }

    let mut length: Option<usize> = None;
    let mut header_bytes = request_line.len();
    loop {
        let mut header = String::new();
        header_bytes += read_line(source, &mut header)?;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(RequestError::TooLarge);
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(RequestError::Malformed);
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = Some(value.trim().parse().map_err(|_| RequestError::Malformed)?);
        }
    }

    // Counter-Strike sends a `Content-Length`. Anything that does not is not
    // Counter-Strike, and guessing where a body ends is how a reader waits
    // forever.
    let length = length.ok_or(RequestError::NoLength)?;
    if length > MAX_BODY_BYTES {
        return Err(RequestError::TooLarge);
    }

    let mut body = vec![0; length];
    source.read_exact(&mut body).map_err(|error| {
        if timed_out(&error) {
            RequestError::Stalled
        } else {
            RequestError::Truncated
        }
    })?;
    Ok(body)
}

/// Reads one line, bounded, and answers how many bytes it took.
fn read_line(source: &mut impl BufRead, into: &mut String) -> Result<usize, RequestError> {
    // `take` bounds the read itself rather than checking afterwards: a client
    // that never sends a newline would otherwise be a growing allocation.
    let mut bounded = source.take(MAX_HEADER_BYTES as u64);
    let mut bytes = Vec::new();
    bounded.read_until(b'\n', &mut bytes).map_err(|error| {
        if timed_out(&error) {
            RequestError::Stalled
        } else {
            RequestError::Unreadable
        }
    })?;
    if bytes.is_empty() {
        return Err(RequestError::Truncated);
    }
    if bytes.len() >= MAX_HEADER_BYTES {
        return Err(RequestError::TooLarge);
    }
    into.push_str(&String::from_utf8_lossy(&bytes));
    Ok(bytes.len())
}

/// Why a request was not a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// Something other than a `POST`. A browser visiting the port lands here.
    NotAPost,
    /// No `Content-Length`.
    NoLength,
    /// Bigger than this module will read.
    TooLarge,
    /// The connection ended in the middle.
    Truncated,
    /// The connection ran out of time: silent for [`READ_TIMEOUT`], or talking
    /// without finishing for longer than the whole connection is allowed.
    Stalled,
    /// A header this module could not read.
    Malformed,
    /// The socket failed.
    Unreadable,
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotAPost => "not a POST",
            Self::NoLength => "no Content-Length",
            Self::TooLarge => "larger than this endpoint reads",
            Self::Truncated => "the connection ended mid-request",
            Self::Stalled => "the connection took longer than this endpoint waits",
            Self::Malformed => "a header could not be read",
            Self::Unreadable => "the connection failed",
        })
    }
}

impl core::error::Error for RequestError {}

/// Why a connection produced no payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The request was fine and the token was not the one in the configuration
    /// file. This is the case `docs/privacy.md` requires: something on this
    /// machine that is not the game.
    Unauthenticated,
    /// The request was not one this endpoint reads.
    Malformed {
        /// What was wrong with it.
        error: RequestError,
    },
    /// The connection could not be used at all.
    Unreadable,
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthenticated => formatter.write_str(
                "a request arrived without the token from the game's configuration file, and was \
                 refused",
            ),
            Self::Malformed { error } => write!(formatter, "a request was refused: {error}"),
            Self::Unreadable => formatter.write_str("a connection could not be read"),
        }
    }
}

impl core::error::Error for Refusal {}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    const PAYLOAD: &str = include_str!("../tests/payloads/live_round.json");
    const TOKEN: &str = "fixture-token-not-a-secret";

    fn post(body: &str) -> String {
        format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn a_post_from_the_game_reads_as_its_body() {
        let request = post(PAYLOAD);
        let body = read_post(&mut Cursor::new(request.as_bytes())).expect("a well-formed post");
        assert_eq!(body, PAYLOAD.as_bytes());
    }

    #[test]
    fn anything_that_is_not_a_post_with_a_length_is_refused_by_name() {
        let refuse = |request: &str| {
            read_post(&mut Cursor::new(request.as_bytes())).expect_err("refused: {request}")
        };

        assert_eq!(
            refuse("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
            RequestError::NotAPost,
            "a browser opening the port is the obvious visitor, and it is not the game"
        );
        assert_eq!(
            refuse("POST / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
            RequestError::NoLength
        );
        assert_eq!(
            refuse("POST / HTTP/1.1\r\nContent-Length: banana\r\n\r\n"),
            RequestError::Malformed
        );
        assert_eq!(
            refuse("POST / HTTP/1.1\r\nContent-Length: 100\r\n\r\nshort"),
            RequestError::Truncated
        );
        assert_eq!(refuse(""), RequestError::Truncated);
    }

    #[test]
    fn a_body_larger_than_this_endpoint_reads_is_refused_before_it_is_allocated() {
        let request = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert_eq!(
            read_post(&mut Cursor::new(request.as_bytes())).expect_err("too large"),
            RequestError::TooLarge
        );
    }

    #[test]
    fn a_header_block_that_never_ends_is_refused_rather_than_read_forever() {
        let mut request = String::from("POST / HTTP/1.1\r\n");
        while request.len() < MAX_HEADER_BYTES * 2 {
            request.push_str("X-Padding: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
        }
        assert_eq!(
            read_post(&mut Cursor::new(request.as_bytes())).expect_err("too large"),
            RequestError::TooLarge
        );
    }

    #[test]
    fn only_a_payload_carrying_the_configured_token_is_believed() {
        assert!(carries(PAYLOAD.as_bytes(), TOKEN));
        assert!(
            !carries(PAYLOAD.as_bytes(), "a-different-token"),
            "a payload with somebody else's token is not from this game"
        );
        assert!(
            !carries(br#"{"player":{"steamid":"1"}}"#, TOKEN),
            "a payload with no token at all is not from this game either"
        );
        assert!(!carries(b"not json", TOKEN));
        assert!(
            !carries(PAYLOAD.as_bytes(), "fixture-token-not-a-secre"),
            "a prefix of the token is not the token"
        );
    }

    /// A client that keeps talking and takes its time about finishing.
    ///
    /// A little at a time with a wait in front of each read, which is the shape
    /// a per-read timeout cannot catch: every read succeeds, so nothing ever
    /// times out, and the request still never ends.
    struct Dribble {
        remaining: Vec<u8>,
        per_read: Duration,
    }

    impl Dribble {
        /// Thirty-two bytes at a time, so that a fixture-sized request takes
        /// tens of reads rather than one.
        const CHUNK: usize = 32;
    }

    impl Read for Dribble {
        fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
            if self.remaining.is_empty() || into.is_empty() {
                return Ok(0);
            }
            thread::sleep(self.per_read);
            let taken = Self::CHUNK.min(into.len()).min(self.remaining.len());
            into[..taken].copy_from_slice(&self.remaining[..taken]);
            self.remaining.drain(..taken);
            Ok(taken)
        }
    }

    #[test]
    fn a_request_that_never_finishes_is_refused_when_its_allowance_runs_out() {
        // Every read here succeeds, so READ_TIMEOUT never fires: what is
        // measured is the connection as a whole. The request is a perfectly
        // well-formed post, and the only thing wrong with it is how long it is
        // taking — which is exactly what a process holding the port open would
        // look like.
        let request = post(PAYLOAD);
        let dribbling = || Dribble {
            remaining: request.as_bytes().to_vec(),
            per_read: Duration::from_millis(5),
        };

        assert_eq!(
            read_post(&mut BufReader::new(Deadline::starting_now(
                dribbling(),
                Duration::from_millis(10)
            )))
            .expect_err("a request that outstays its allowance is refused"),
            RequestError::Stalled
        );

        // The same client, given time it does not need, is the same well-formed
        // post: the bound refuses connections that take too long, not slow ones.
        assert_eq!(
            read_post(&mut BufReader::new(Deadline::starting_now(
                dribbling(),
                Duration::from_secs(60)
            )))
            .expect("time enough for it"),
            PAYLOAD.as_bytes()
        );
    }

    /// The socket, for real, on a port the operating system picks.
    ///
    /// Loopback only, one connection at a time, and nothing outside this
    /// process can reach it. It is here because everything above tests the
    /// reader and none of it proves a socket ever gets bound to the right
    /// address or that an unauthenticated payload is actually dropped.
    #[test]
    fn a_real_loopback_socket_accepts_the_game_and_refuses_everything_else() {
        let listener = GsiListener::bind(0).expect("an ephemeral loopback port");
        let port = listener.port().expect("the port it got");
        let (sender, receiver) = mpsc::channel();

        let serving = thread::spawn(move || {
            let mut refusals = Vec::new();
            listener.serve(TOKEN, &sender, |refusal| refusals.push(*refusal));
            refusals
        });

        let send = |request: String| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("loopback connects");
            stream
                .write_all(request.as_bytes())
                .expect("the request goes");
            let mut answer = String::new();
            stream.read_to_string(&mut answer).expect("an answer comes");
            answer
        };

        assert!(send(post(PAYLOAD)).starts_with("HTTP/1.1 200"));

        let wrong_token = PAYLOAD.replace(TOKEN, "not-the-token");
        assert!(
            send(post(&wrong_token)).starts_with("HTTP/1.1 403"),
            "a payload without the token must be refused, not answered with 200"
        );
        assert!(send("GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_owned()).starts_with("HTTP/1.1 400"));

        // Dropping the receiver ends `serve` on its next delivery, so one more
        // good request is what closes the thread down.
        drop(receiver);
        let _ = send(post(PAYLOAD));
        let refusals = serving.join().expect("the serving thread");

        assert_eq!(
            refusals,
            vec![
                Refusal::Unauthenticated,
                Refusal::Malformed {
                    error: RequestError::NotAPost
                }
            ],
            "exactly one payload should have got through, and both refusals named"
        );
    }

    /// A socket that will not take a read timeout is declined, not read.
    ///
    /// The allowance in [`Deadline`] is checked *between* reads and cannot
    /// interrupt one already blocked, so a socket with no timeout underneath it
    /// has no bound at all — a single silent client would hold the endpoint,
    /// and with it the game's payloads, for as long as it liked. Declining such
    /// a socket is therefore behaviour rather than tidiness, which means it
    /// needs a test that bites and not a sentence claiming it does.
    ///
    /// `Duration::ZERO` is how this gets a **real** `TcpStream` to refuse a
    /// bound: `set_read_timeout` rejects it, so the endpoint takes the same
    /// path a socket failing for any other reason would take. The request is a
    /// perfectly good authenticated post either way, so the only thing that can
    /// account for the difference between the two halves below is the bound
    /// that could not be set.
    #[test]
    fn a_socket_that_will_not_take_a_read_timeout_is_declined_rather_than_read_unbounded() {
        let handled = |read_timeout: Duration| {
            let socket =
                TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
                    .expect("an ephemeral loopback port");
            let port = socket.local_addr().expect("the port it got").port();

            let posting = thread::spawn(move || {
                let mut stream =
                    TcpStream::connect(("127.0.0.1", port)).expect("loopback connects");
                let _ = stream.write_all(post(PAYLOAD).as_bytes());
                let mut answer = String::new();
                let _ = stream.read_to_string(&mut answer);
                answer
            });

            let (stream, _) = socket.accept().expect("the connection it just made");
            let outcome = handle(stream, TOKEN, CONNECTION_DEADLINE, read_timeout);
            let answer = posting.join().expect("the posting thread");
            (outcome, answer)
        };

        let (declined, answer) = handled(Duration::ZERO);
        assert_eq!(
            declined.expect_err("a socket that cannot be bounded is not read from"),
            Refusal::Unreadable,
            "a connection whose read timeout the operating system refused was read anyway"
        );
        assert!(
            answer.is_empty(),
            "nothing is answered to a connection that was never read: {answer:?}"
        );

        // The identical request over a socket that does take the bound. This is
        // the half that makes the half above mean something: what differs
        // between them is one `Duration`.
        let (accepted, answer) = handled(READ_TIMEOUT);
        assert_eq!(
            accepted
                .expect("the same post, over a socket that took the bound")
                .body,
            PAYLOAD.as_bytes()
        );
        assert!(answer.starts_with("HTTP/1.1 200"));
    }

    /// The read timeout the plugin actually uses is the constant, not a test's.
    #[test]
    fn a_bound_listener_reads_with_the_timeout_this_module_documents() {
        let listener = GsiListener::bind(0).expect("an ephemeral loopback port");
        assert_eq!(
            (listener.read_timeout, listener.allowance),
            (READ_TIMEOUT, CONNECTION_DEADLINE),
            "a bound that only exists in a test is not a bound"
        );
    }

    /// The failure the connection allowance exists for, over a real socket.
    ///
    /// Payloads are accepted one connection at a time, so a connection that
    /// never finishes is the game's payloads never being read. The client below
    /// is not silent — a silent one `READ_TIMEOUT` would catch — it keeps
    /// sending, one byte at a time, forever.
    #[test]
    fn a_connection_that_will_not_finish_does_not_hold_the_game_out() {
        // Short enough that this test does not wait CONNECTION_DEADLINE out.
        // What ships is the constant, asserted below.
        const ALLOWANCE: Duration = Duration::from_millis(300);
        // Longer than the endpoint may take to get past the stalling client,
        // and shorter than that client keeps going for.
        const PATIENCE: Duration = Duration::from_secs(3);

        let listener = GsiListener::bind(0).expect("an ephemeral loopback port");
        assert_eq!(
            listener.allowance, CONNECTION_DEADLINE,
            "a bound bound only in tests is not a bound"
        );
        let listener = listener.allowing(ALLOWANCE);
        let port = listener.port().expect("the port it got");
        let (sender, receiver) = mpsc::channel();
        let serving = thread::spawn(move || {
            let mut refusals = Vec::new();
            listener.serve(TOKEN, &sender, |refusal| refusals.push(*refusal));
            refusals
        });

        // A request that begins and does not end, for far longer than the
        // endpoint is prepared to wait.
        let stalling = thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("loopback connects");
            stream
                .write_all(b"POST / HTTP/1.1\r\n")
                .expect("the first line goes");
            for _ in 0..200 {
                // Never a newline, so the header block never ends. It stops
                // early only when the endpoint has hung up on it, which is the
                // outcome this test is about.
                if stream.write_all(b"X").is_err() || stream.flush().is_err() {
                    return true;
                }
                thread::sleep(Duration::from_millis(50));
            }
            false
        });

        // Give the stalling client the connection the endpoint is working on,
        // then post as the game does.
        thread::sleep(Duration::from_millis(100));
        let mut game = TcpStream::connect(("127.0.0.1", port)).expect("loopback connects");
        game.write_all(post(PAYLOAD).as_bytes())
            .expect("the payload goes");

        let delivered = receiver
            .recv_timeout(PATIENCE)
            .expect("the game's payload was still waiting behind a connection that never finished");
        assert_eq!(delivered.body, PAYLOAD.as_bytes());
        assert!(
            stalling.join().expect("the stalling thread"),
            "the endpoint waited the stalling client out instead of dropping it"
        );

        // End the serving thread, and read back what it made of the stall.
        drop(receiver);
        let mut closing = TcpStream::connect(("127.0.0.1", port)).expect("loopback connects");
        let _ = closing.write_all(post(PAYLOAD).as_bytes());
        let refusals = serving.join().expect("the serving thread");
        assert!(
            refusals.contains(&Refusal::Malformed {
                error: RequestError::Stalled
            }),
            "the stalled connection should have been refused by name: {refusals:?}"
        );
    }
}
