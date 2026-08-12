//! The loopback endpoint Counter-Strike 2 posts to.
//!
//! One socket, bound to `127.0.0.1`, accepting `POST` requests whose body is a
//! Game State Integration payload. It is declared in `plugin.json`, rendered
//! for the user as a sentence before the plugin may be enabled
//! (`clipped_plugins::NetworkAccess`) and written down in `docs/privacy.md`.
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
//! - Everything about a request is bounded before it is read: the header block,
//!   the body, and how long a connection may sit there. A local port that
//!   anything can connect to is a local port anything can hold open.
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

/// How long a connection may take to say anything.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

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
        TcpListener::bind(address).map(|socket| Self { socket })
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
    /// Runs on a thread of its own. It never blocks the plugin's main loop and
    /// the main loop never waits for it: a game that stops posting is a channel
    /// that goes quiet, which is what the heartbeat in `crate::main` is for.
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
            match handle(stream, token) {
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
fn handle(mut stream: TcpStream, token: &str) -> Result<ReceivedPayload, Refusal> {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));

    let outcome = read_post(&mut BufReader::new(
        stream.try_clone().map_err(|_| Refusal::Unreadable)?,
    ));
    let received = Instant::now();

    let answer = match &outcome {
        Ok(body) if carries(body, token) => "HTTP/1.1 200 OK",
        Ok(_) => "HTTP/1.1 403 Forbidden",
        Err(RequestError::TooLarge) => "HTTP/1.1 413 Payload Too Large",
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
    source
        .read_exact(&mut body)
        .map_err(|_| RequestError::Truncated)?;
    Ok(body)
}

/// Reads one line, bounded, and answers how many bytes it took.
fn read_line(source: &mut impl BufRead, into: &mut String) -> Result<usize, RequestError> {
    // `take` bounds the read itself rather than checking afterwards: a client
    // that never sends a newline would otherwise be a growing allocation.
    let mut bounded = source.take(MAX_HEADER_BYTES as u64);
    let mut bytes = Vec::new();
    bounded
        .read_until(b'\n', &mut bytes)
        .map_err(|_| RequestError::Unreadable)?;
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
}
