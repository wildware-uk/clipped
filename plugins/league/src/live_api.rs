//! The one part of this plugin that touches a socket: an HTTPS GET of
//! `https://127.0.0.1:2999/liveclientdata/allgamedata`, through WinHTTP.
//!
//! # What this is allowed to do, and what it is not
//!
//! `plugin.json` declares exactly one thing — a **loopback connect** to
//! `127.0.0.1:2999`, "reads the Live Client Data API the running game serves" —
//! and `docs/privacy.md` is what that declaration is held to. This module is
//! the whole of the implementation of it, which is why it is one file with one
//! endpoint in it and nothing outside it that can ask for another:
//! [`LiveClientApi::open`] takes no arguments, and the port and the resource
//! are constants.
//!
//! Two decisions here are what make the declaration true rather than nearly
//! true:
//!
//! - **No proxy.** The session is opened with `WINHTTP_ACCESS_TYPE_NO_PROXY`.
//!   Without it, a machine with a system proxy configured could have this
//!   request sent to the proxy instead — off the machine, to somebody else's
//!   server, under a declaration that said loopback (`docs/privacy.md`,
//!   "Loopback is not a loophole").
//! - **No redirects, ever.** WinHTTP follows them by default, and the default
//!   policy allows `https` to `https` — so a listener answering `302 Location:
//!   https://somewhere.else` would take this request off the machine, with the
//!   certificate exception below still in force. Every request therefore sets
//!   `WINHTTP_OPTION_REDIRECT_POLICY` to
//!   `WINHTTP_OPTION_REDIRECT_POLICY_NEVER`, which makes a `3xx` an answer this
//!   code reads the status line of rather than an address it goes to. Only 200
//!   carries a body; anything else is "there is no match".
//!   `a_listener_that_answers_with_a_redirect_is_not_followed` is what holds
//!   that, with two loopback listeners and an assertion that the second one is
//!   never connected to.
//!
//! # The certificate, and why it is excepted
//!
//! League serves this API over HTTPS with a certificate signed by Riot's own
//! authority, which is not in Windows' trust store, and issued to a name that
//! is not `127.0.0.1`. A client that validated it in the usual way would fail
//! every request.
//!
//! So the certificate errors are ignored — **on the request handle, for this
//! request, and nowhere else**. `WinHttpSetOption` is called with
//! `WINHTTP_OPTION_SECURITY_FLAGS` on the handle returned by
//! `WinHttpOpenRequest`; it is not set on the session, it is not a change to
//! any trust store, and it cannot affect another request in this process, let
//! alone another process. This plugin makes exactly one kind of request, so the
//! exception's blast radius is that one kind of request.
//!
//! What is left over is worth stating rather than glossing: because the
//! certificate is not checked, this code cannot prove that the thing answering
//! on port 2999 is League. Any process on the machine could be listening there
//! first. That is why the body is treated as hostile input — bounded before it
//! is read ([`MAX_BODY_BYTES`]), parsed leniently, and never used for anything
//! but producing marks on a timeline. There is no credential to steal here and
//! nothing is sent: the request has no body, no cookie and no authorisation
//! header. Ignoring a certificate would be a serious thing to do to an outbound
//! connection, and this is a connection to this machine that carries nothing —
//! which stays true only while the request cannot be *redirected* into being an
//! outbound one. That is why the redirect policy above is set on the same
//! handle, and why the two decisions are one decision rather than two.
//!
//! # Windows only
//!
//! WinHTTP is the operating system's own HTTP stack, which is why this plugin
//! needs no TLS crate at all (`Cargo.toml` says what that saves). Clipped is a
//! Windows application (SPEC.md section 3) and so is League of Legends, so
//! there is no second implementation for another platform — `src/main.rs`
//! refuses to run on one rather than pretending.

use core::ffi::c_void;
use core::fmt;
use core::ptr;
use core::time::Duration;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetOption, WinHttpSetTimeouts, SECURITY_FLAG_IGNORE_CERT_CN_INVALID,
    SECURITY_FLAG_IGNORE_CERT_DATE_INVALID, SECURITY_FLAG_IGNORE_UNKNOWN_CA,
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
    WINHTTP_OPTION_SECURITY_FLAGS, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Where the API is. The whole of the manifest's network declaration.
const HOST: PCWSTR = windows::core::w!("127.0.0.1");
/// The port League serves it on.
const PORT: u16 = 2999;
/// The one resource this plugin asks for.
const RESOURCE: PCWSTR = windows::core::w!("/liveclientdata/allgamedata");
/// What this plugin calls itself to the local server.
const USER_AGENT: PCWSTR = windows::core::w!("clipped-league-plugin");

/// The most of an answer this will read.
///
/// A ten-player payload is a few tens of kilobytes. This is far above that and
/// far below anything that matters to a machine running a game, and it exists
/// because the certificate is not checked: whatever is listening on port 2999
/// is untrusted input, and untrusted input that decides how much memory to
/// allocate is a denial of service with extra steps.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// How long any one stage of a request may take.
///
/// Bounded so that a server which accepts a connection and then says nothing
/// cannot stop this plugin's loop — which would stop its heartbeat, and get it
/// killed as hung (`docs/plugin-api.md`, "Supervision and restart"). Two
/// seconds is generous for a request to this machine and short enough that the
/// loop keeps its cadence.
const STAGE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the whole of a response body may take to arrive.
///
/// [`STAGE_TIMEOUT`] bounds each call WinHTTP is asked to make and nothing else,
/// which is no bound on a *read* at all: a listener that sends a byte every
/// half-second answers every call promptly and never finishes, and the poll that
/// is reading it never returns. The plugin then stops saying `alive` and the
/// host kills it as hung (`docs/plugin-api.md`, "Supervision and restart"),
/// which costs the rest of the match.
///
/// So the loop is bounded as well as the calls inside it. A second is several
/// orders of magnitude above what a body from this machine takes — the real one
/// is tens of kilobytes over loopback — and comfortably below the host's
/// patience, and it is the whole answer to "how long may a stranger on port 2999
/// keep this plugin busy".
const BODY_DEADLINE: Duration = Duration::from_secs(1);

/// What one request produced.
#[derive(Debug)]
pub enum Answer {
    /// It answered `200`, with a body and the round trip it took.
    Body {
        /// What it said.
        body: String,
        /// How long the whole request took. The match clock inside the body was
        /// read somewhere inside that window, so it is the honest width of the
        /// uncertainty about when — measured across the whole request rather
        /// than to the first byte, because over-stating precision is the
        /// direction that puts a mark in the wrong place and says it is sure.
        round_trip: Duration,
    },
    /// It answered something other than `200`: there is no match in progress.
    NoMatch,
    /// It did not answer.
    Unreachable {
        /// Why, for the log. Normal before a match has loaded and after it has
        /// ended, so this is not on its own a fault.
        because: LiveApiError,
    },
}

/// An open WinHTTP session pointed at League's local API.
///
/// One of these lasts as long as the plugin does. Neither handle it holds is a
/// socket — `WinHttpConnect` records a host and a port rather than connecting —
/// so keeping them costs nothing while a game is loading, and lets WinHTTP
/// reuse a connection between polls once one is up.
pub struct LiveClientApi {
    /// The host and port, which is `127.0.0.1:2999` and can be nothing else.
    ///
    /// Declared before the session it belongs to, because Rust drops fields in
    /// declaration order and a WinHTTP handle is closed after the handles under
    /// it (AGENTS.md section 58: ownership is explicit, including the order).
    connection: Handle,
    /// What every request off this connection is opened with.
    ///
    /// [`WINHTTP_FLAG_SECURE`] in the one configuration this plugin ships, and
    /// a field rather than a constant only so that this module's own tests can
    /// stand a plain listener on a loopback port and prove what the request
    /// handle's options do — see [`Self::open_at`].
    request_flags: WINHTTP_OPEN_REQUEST_FLAGS,
    /// The session the connection was made from.
    ///
    /// Never read: WinHTTP keeps no reference to it that Rust can see, so it is
    /// held here to give it the lifetime it has to have, and closed when this
    /// is dropped.
    _session: Handle,
}

impl fmt::Debug for LiveClientApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveClientApi")
            .field("endpoint", &"127.0.0.1:2999")
            .finish_non_exhaustive()
    }
}

impl LiveClientApi {
    /// Opens the session.
    ///
    /// # Errors
    ///
    /// [`LiveApiError`] when WinHTTP will not give a session or a connection
    /// handle. Both are local operations that do not touch the network, so a
    /// failure here is a machine-level problem rather than League not running.
    pub fn open() -> Result<Self, LiveApiError> {
        Self::open_at(PORT, WINHTTP_FLAG_SECURE)
    }

    /// The session, pointed at a port on this machine.
    ///
    /// [`Self::open`] is the only way in from outside this module, and it
    /// passes the one endpoint `plugin.json` declares: nothing a caller can
    /// reach chooses a port or drops [`WINHTTP_FLAG_SECURE`]. This exists
    /// because the request handle's options — the redirect policy above all —
    /// are the part of this file most worth a test, and testing them over HTTPS
    /// would need a TLS server standing on a machine somebody is also using
    /// (AGENTS.md section 25). The options are set on the request whatever it
    /// is opened with, so a plain listener on an ephemeral port proves them.
    fn open_at(port: u16, request_flags: WINHTTP_OPEN_REQUEST_FLAGS) -> Result<Self, LiveApiError> {
        // SAFETY: every argument is either a null-terminated wide literal that
        // outlives the call or a value type. The returned handle is null on
        // failure, which is checked immediately, and is owned by the `Handle`
        // that takes it.
        let session = unsafe {
            WinHttpOpen(
                USER_AGENT,
                // The declaration in `plugin.json` says loopback. A configured
                // system proxy must not be able to make that untrue.
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        };
        let session = Handle::new(session).ok_or_else(|| LiveApiError::open("session"))?;

        let milliseconds = i32::try_from(STAGE_TIMEOUT.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `session` is a live session handle owned by this function.
        unsafe {
            WinHttpSetTimeouts(
                session.0,
                milliseconds,
                milliseconds,
                milliseconds,
                milliseconds,
            )
        }
        .map_err(LiveApiError::Windows)?;

        // SAFETY: `session` is live, and `HOST` is a null-terminated wide
        // literal with static lifetime. The handle is checked for null.
        let connection = unsafe { WinHttpConnect(session.0, HOST, port, 0) };
        let connection = Handle::new(connection).ok_or_else(|| LiveApiError::open("connection"))?;

        Ok(Self {
            connection,
            request_flags,
            _session: session,
        })
    }

    /// Asks the API for everything it knows about the match in progress.
    ///
    /// Never fails: an API that is not there is [`Answer::Unreachable`], which
    /// is what a game that has not finished loading looks like and is not an
    /// error (AGENTS.md section 16).
    #[must_use]
    pub fn snapshot(&self) -> Answer {
        let started = Instant::now();
        match self.get() {
            Ok(Some(body)) => Answer::Body {
                body,
                round_trip: started.elapsed(),
            },
            Ok(None) => Answer::NoMatch,
            Err(because) => Answer::Unreachable { because },
        }
    }

    /// One GET: `Some(body)` for a 200, `None` for anything else it answered.
    fn get(&self) -> Result<Option<String>, LiveApiError> {
        // SAFETY: `self.connection` is live for as long as `self` is, and the
        // two literals are null-terminated wide strings with static lifetime.
        // A null `ppwszaccepttypes` is documented as "no types", and the handle
        // is checked for null.
        let request = unsafe {
            WinHttpOpenRequest(
                self.connection.0,
                windows::core::w!("GET"),
                RESOURCE,
                PCWSTR::null(),
                PCWSTR::null(),
                ptr::null(),
                self.request_flags,
            )
        };
        let request = Handle::new(request).ok_or_else(|| LiveApiError::open("request"))?;

        // Where this request may go, before anything about what it will accept
        // from wherever it ends up. WinHTTP follows redirects by default, and
        // its default policy permits `https` to `https` — so without this, a
        // listener on port 2999 that answered `302 Location:
        // https://somewhere.else` would send this request off the machine, with
        // the certificate exception below still in force, under a manifest that
        // declares loopback and nothing else. `NEVER` makes a `3xx` a status
        // code this code reads rather than an address it goes to.
        let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        // SAFETY: `request` is live, and the buffer is a `u32`'s worth of bytes
        // borrowed for the duration of the call, which is what this option
        // expects.
        unsafe {
            WinHttpSetOption(
                Some(request.0.cast_const()),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&never.to_ne_bytes()),
            )
        }
        .map_err(LiveApiError::Windows)?;

        // The certificate exception, on this handle and nothing else. See the
        // module documentation for why each of the three is needed: Riot's
        // authority is not in the trust store, the certificate is not issued to
        // `127.0.0.1`, and one that has expired must not stop a local game's
        // events being read.
        //
        // Set only on a secure request, which every request this plugin makes
        // is: there is no certificate to except on a request that has no
        // handshake, and an exception that is set where it means nothing is an
        // exception nobody can reason about the reach of.
        if self.request_flags & WINHTTP_FLAG_SECURE == WINHTTP_FLAG_SECURE {
            let ignored = SECURITY_FLAG_IGNORE_UNKNOWN_CA
                | SECURITY_FLAG_IGNORE_CERT_CN_INVALID
                | SECURITY_FLAG_IGNORE_CERT_DATE_INVALID;
            // SAFETY: `request` is live, and the buffer is a `u32`'s worth of
            // bytes borrowed for the duration of the call, which is what this
            // option expects.
            unsafe {
                WinHttpSetOption(
                    Some(request.0.cast_const()),
                    WINHTTP_OPTION_SECURITY_FLAGS,
                    Some(&ignored.to_ne_bytes()),
                )
            }
            .map_err(LiveApiError::Windows)?;
        }

        // SAFETY: `request` is live. No headers and no body are sent: this is a
        // GET of one fixed resource, and it carries nothing about the machine
        // it is running on.
        unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
            .map_err(LiveApiError::Windows)?;
        // SAFETY: `request` is live and the reserved argument is null, as the
        // API requires.
        unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) }
            .map_err(LiveApiError::Windows)?;

        if status_of(&request)? != 200 {
            return Ok(None);
        }
        Ok(Some(read_body(&request)?))
    }
}

/// The status code of a response that has arrived.
fn status_of(request: &Handle) -> Result<u32, LiveApiError> {
    let mut status: u32 = 0;
    let mut length = u32::try_from(size_of::<u32>()).unwrap_or(4);
    // SAFETY: `request` is live and has a response. `WINHTTP_QUERY_FLAG_NUMBER`
    // is what makes the out parameter a `u32` rather than text, and `length`
    // says how big the buffer behind that pointer is. The header index is null
    // because there is one status code.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(ptr::from_mut(&mut status).cast::<c_void>()),
            &mut length,
            ptr::null_mut(),
        )
    }
    .map_err(LiveApiError::Windows)?;
    Ok(status)
}

/// Everything the response carries, up to [`MAX_BODY_BYTES`] and
/// [`BODY_DEADLINE`].
///
/// Each turn asks how much has arrived and then reads exactly that, rather than
/// asking `WinHttpReadData` for a bufferful. The difference is the whole of the
/// deadline: `WinHttpReadData` blocks until it can fill the buffer it was given
/// or the response ends, so a listener dripping a byte at a time keeps one call
/// inside WinHTTP for as long as it likes and never reaches the check below.
/// `WinHttpQueryDataAvailable` returns as soon as there is anything, which is
/// what puts this loop back in charge of its own clock.
fn read_body(request: &Handle) -> Result<String, LiveApiError> {
    let mut body: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 8192];
    let started = Instant::now();
    loop {
        if started.elapsed() > BODY_DEADLINE {
            return Err(LiveApiError::TooSlow {
                after: body.len(),
                took: started.elapsed(),
            });
        }

        let mut available: u32 = 0;
        // SAFETY: `request` is live and has a response, and `available` is a
        // `u32` this function owns for the duration of the call.
        unsafe { WinHttpQueryDataAvailable(request.0, &mut available) }
            .map_err(LiveApiError::Windows)?;
        if available == 0 {
            break;
        }

        let mut read: u32 = 0;
        let capacity = u32::try_from(chunk.len())
            .unwrap_or(u32::MAX)
            .min(available);
        // SAFETY: `request` is live and `chunk` is a buffer of at least
        // `capacity` bytes owned by this function; WinHTTP writes at most that
        // many and reports how many in `read`.
        unsafe {
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast::<c_void>(),
                capacity,
                &mut read,
            )
        }
        .map_err(LiveApiError::Windows)?;

        let read = read as usize;
        if read == 0 {
            break;
        }
        if body.len() + read > MAX_BODY_BYTES {
            return Err(LiveApiError::TooMuch {
                after: body.len() + read,
            });
        }
        body.extend_from_slice(&chunk[..read]);
    }

    // Lossy rather than a refusal: a body that is not text is something that is
    // not League answering on League's port, and the caller's job is to notice
    // that it cannot read what it is being sent (`crate::watch`), which it does
    // by failing to parse it. Refusing here would report that as the API being
    // unreachable, which is a different thing with a different answer.
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// A WinHTTP handle, closed when it goes out of scope.
///
/// Explicit ownership rather than a raw pointer with a `close` somebody has to
/// remember (AGENTS.md section 58).
struct Handle(*mut c_void);

impl Handle {
    /// Takes ownership of a handle WinHTTP returned, or `None` if it failed.
    fn new(handle: *mut c_void) -> Option<Self> {
        (!handle.is_null()).then_some(Self(handle))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null handle this type owns, and nothing
        // else holds a copy. A close that fails leaves nothing a caller could
        // do about it.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// Why a request did not produce a body.
#[derive(Debug)]
pub enum LiveApiError {
    /// WinHTTP would not give a handle.
    NoHandle {
        /// Which one: the session, the connection or the request.
        which: &'static str,
        /// What Windows said.
        source: windows::core::Error,
    },
    /// A WinHTTP call failed.
    Windows(windows::core::Error),
    /// Whatever is listening sent more than [`MAX_BODY_BYTES`].
    TooMuch {
        /// How much had arrived when it was stopped.
        after: usize,
    },
    /// Whatever is listening took longer than [`BODY_DEADLINE`] over a body.
    TooSlow {
        /// How much had arrived when it was stopped.
        after: usize,
        /// How long the read had been going on.
        took: Duration,
    },
}

impl LiveApiError {
    /// A handle failure, with the reason Windows gave for it.
    fn open(which: &'static str) -> Self {
        Self::NoHandle {
            which,
            source: windows::core::Error::from_thread(),
        }
    }
}

impl fmt::Display for LiveApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHandle { which, source } => write!(
                formatter,
                "WinHTTP would not open a {which} for 127.0.0.1:{PORT}: {source}"
            ),
            Self::Windows(source) => write!(
                formatter,
                "the Live Client Data API on 127.0.0.1:{PORT} could not be read: {source}"
            ),
            Self::TooMuch { after } => write!(
                formatter,
                "whatever is listening on 127.0.0.1:{PORT} sent more than {MAX_BODY_BYTES} bytes \
                 ({after} and counting), which League's Live Client Data API does not"
            ),
            Self::TooSlow { after, took } => write!(
                formatter,
                "whatever is listening on 127.0.0.1:{PORT} had sent {after} bytes of an answer \
                 after {took:?} and had not finished, which League's Live Client Data API does not"
            ),
        }
    }
}

impl core::error::Error for LiveApiError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::NoHandle { source, .. } | Self::Windows(source) => Some(source),
            Self::TooMuch { .. } | Self::TooSlow { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    use super::*;

    /// A body the derivation would happily read, so that a test which fails
    /// fails because a redirect was followed rather than because what was at
    /// the other end was rubbish.
    const A_MATCH: &str = r#"{"gameData":{"gameTime":12.0},"events":{"Events":[]}}"#;

    /// A listener on a port the operating system chose.
    ///
    /// Port zero rather than a number written down: two of these run at once in
    /// the test below, tests in this crate run in parallel, and a hard-coded
    /// port is a test that fails when somebody is playing League on the machine
    /// running it.
    fn listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is available");
        let port = listener
            .local_addr()
            .expect("a bound listener has an address")
            .port();
        (listener, port)
    }

    /// Reads a request off `stream` up to the blank line that ends its headers.
    ///
    /// The body is never read because nothing this plugin sends has one; what
    /// matters is that the request is consumed before an answer is written, so
    /// that the answer is not racing the client's own send.
    fn read_request(stream: &mut TcpStream) {
        let mut seen = Vec::new();
        let mut byte = [0_u8; 1];
        while !seen.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(0) | Err(_) => return,
                Ok(_) => seen.push(byte[0]),
            }
        }
    }

    #[test]
    fn a_listener_that_answers_with_a_redirect_is_not_followed() {
        // The declaration in `plugin.json` is loopback, and the certificate on
        // this request is not checked — so the one thing that must be true of
        // whatever answers on the port is that it cannot send this request
        // anywhere else. WinHTTP follows redirects by default, which is what
        // makes this a test of a setting rather than of a default.
        //
        // Two listeners: one that redirects, and one that must never be
        // connected to. It is deliberately the *second* assertion that is the
        // security property; the first would also pass if the request had
        // failed for an unrelated reason.
        let (elsewhere, elsewhere_port) = listener();
        let reached = Arc::new(AtomicBool::new(false));
        let answered_there = Arc::clone(&reached);
        thread::spawn(move || {
            for stream in elsewhere.incoming() {
                let Ok(mut stream) = stream else { break };
                answered_there.store(true, Ordering::SeqCst);
                read_request(&mut stream);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{A_MATCH}",
                    A_MATCH.len()
                );
            }
        });

        let (redirector, port) = listener();
        thread::spawn(move || {
            for stream in redirector.incoming() {
                let Ok(mut stream) = stream else { break };
                read_request(&mut stream);
                let _ = write!(
                    stream,
                    "HTTP/1.1 302 Found\r\n\
                     Location: http://127.0.0.1:{elsewhere_port}/liveclientdata/allgamedata\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });

        let api = LiveClientApi::open_at(port, WINHTTP_OPEN_REQUEST_FLAGS(0))
            .expect("WinHTTP opens a session for a loopback port");
        let answer = api.snapshot();

        assert!(
            matches!(answer, Answer::NoMatch),
            "a redirect is not a match, and its body is not read: {answer:?}"
        );
        assert!(
            !reached.load(Ordering::SeqCst),
            "the plugin followed a redirect off the one endpoint its manifest declares"
        );
    }

    #[test]
    fn a_listener_that_drips_bytes_cannot_hold_the_poll_loop() {
        // `WinHttpSetTimeouts` bounds one `WinHttpReadData` call, which is no
        // bound on the read at all: every call here returns promptly, and the
        // body never ends. Without a deadline over the whole loop this poll
        // never returns, the plugin stops saying `alive`, and the host kills it
        // as hung for the rest of the match.
        let (dripping, port) = listener();
        thread::spawn(move || {
            for stream in dripping.incoming() {
                let Ok(mut stream) = stream else { break };
                read_request(&mut stream);
                if write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n"
                )
                .is_err()
                {
                    continue;
                }
                // One byte at a time, for longer than the deadline, until the
                // client gives up and the write fails.
                while stream.write_all(b" ").and_then(|()| stream.flush()).is_ok() {
                    thread::sleep(Duration::from_millis(50));
                }
            }
        });

        let api = LiveClientApi::open_at(port, WINHTTP_OPEN_REQUEST_FLAGS(0))
            .expect("WinHTTP opens a session for a loopback port");
        let started = Instant::now();
        let answer = api.snapshot();
        let took = started.elapsed();

        assert!(
            matches!(
                answer,
                Answer::Unreachable {
                    because: LiveApiError::TooSlow { .. }
                }
            ),
            "a body that never ends is the endpoint failing to answer: {answer:?}"
        );
        assert!(
            took < STAGE_TIMEOUT * 3,
            "the poll took {took:?}: the {BODY_DEADLINE:?} deadline did not bound it"
        );
    }
}
