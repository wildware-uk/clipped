//! The named pipe itself.
//!
//! Every Windows call this crate makes is in this file (AGENTS.md section 5).
//! What leaves it is a [`Listener`], a [`Connection`] that is a `Read + Write`,
//! and errors from the platform-independent vocabulary in the parent module.
//!
//! # The three properties that make this a local channel
//!
//! 1. **The access-control list.** The pipe is created with an explicit DACL
//!    granting the account that created it and nobody else. Without one,
//!    Windows applies a default that gives `Everyone` read access, and a
//!    process running as another signed-in user could open the pipe and read
//!    the recorder's replies. `security_descriptor` builds it from the
//!    process token's own user SID rather than from a well-known name, so it is
//!    right on a domain account, a Microsoft account and a local one alike.
//!
//!    What it cannot promise: `SYSTEM` and members of the Administrators group
//!    can take ownership of any object and rewrite its DACL. This keeps other
//!    *users* out. It does not keep an administrator out, and nothing on
//!    Windows would.
//!
//! 2. **`PIPE_REJECT_REMOTE_CLIENTS`.** Named pipes are reachable over SMB by
//!    default: `\\machine\pipe\name` from another computer is the same object.
//!    That flag refuses any client that did not come from this machine, which
//!    is what keeps the transport off the network in the sense
//!    `docs/privacy.md` means.
//!
//! 3. **`FILE_FLAG_FIRST_PIPE_INSTANCE`.** The first instance is created with
//!    it, so a second recorder starting on the same endpoint fails immediately
//!    with [`TransportError::EndpointInUse`] instead of quietly serving half
//!    the connections. Two recorders sharing one pipe name would be two
//!    processes racing to own the same recording.
//!
//! # Blocking, and how the listener is stopped
//!
//! Everything here is synchronous. The listener blocks in `ConnectNamedPipe`
//! until a client arrives, which is exactly what a thread with nothing else to
//! do should be doing, and it means no overlapped I/O, no completion ports and
//! no event loop.
//!
//! Stopping it is the one thing that costs a trick. [`ListenerStopper::stop`]
//! raises a flag and then *connects to the pipe itself*, which is what releases
//! the blocked `ConnectNamedPipe`; the listener wakes, sees the flag and
//! returns. The obvious alternative — cancelling the pending I/O — has a race
//! that this does not: a stop that arrives just before the listener enters
//! `ConnectNamedPipe` has nothing to cancel and would leave it blocked for
//! ever, whereas a client connection is picked up whether or not the server was
//! already waiting for one.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND,
    ERROR_INSUFFICIENT_BUFFER, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken};

use super::{Endpoint, TransportError};

/// The pipe's buffer sizes, in bytes.
///
/// A hint to the kernel rather than a limit: it is how much a writer can put in
/// before it blocks waiting for the reader. Sixty-four kibibytes is comfortably
/// more than any message this protocol has, so neither side ever blocks in the
/// normal case, and it is small enough that a connection costs nothing worth
/// counting.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// How long a client waits between attempts when the endpoint is momentarily
/// unavailable.
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// One accepted connection, in either direction.
///
/// Reads and writes are the ordinary blocking ones, which is what lets the
/// framing in [`crate::frame`] be written against `Read` and `Write` and tested
/// without a pipe.
#[derive(Debug)]
pub struct Connection {
    pipe: File,
}

impl Read for Connection {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.pipe.read(buffer)
    }
}

impl Write for Connection {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.pipe.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.pipe.flush()
    }
}

/// A recorder's end of an endpoint.
///
/// Accepts one connection at a time and hands each to its caller; the server in
/// [`crate::server`] is what decides to give each one a thread.
#[derive(Debug)]
pub struct Listener {
    endpoint: Endpoint,
    /// The SDDL string every instance is created with, resolved once at bind
    /// so that the token query does not happen per connection.
    security: String,
    /// The instance waiting for its client. There is always one, so that the
    /// endpoint exists continuously and a client that connects between two
    /// accepts is not told there is nothing there.
    pending: File,
    stopping: Arc<AtomicBool>,
}

/// Asks a [`Listener`] to stop, from another thread.
#[derive(Debug, Clone)]
pub struct ListenerStopper {
    endpoint: Endpoint,
    stopping: Arc<AtomicBool>,
}

impl ListenerStopper {
    /// Stops the listener at its next opportunity.
    ///
    /// Idempotent, and safe to call from a signal handler's thread: it raises a
    /// flag and opens a connection to wake the accept that the flag is meant to
    /// end. A failure to connect is deliberately ignored — the only reason it
    /// can fail is that the listener has already gone, which is the outcome
    /// being asked for.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);

        if let Ok(connection) = open_client(&self.endpoint) {
            drop(connection);
        }
    }

    /// Whether a stop has been asked for.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }
}

impl Listener {
    /// Takes ownership of an endpoint.
    ///
    /// The endpoint exists from the moment this returns, so a client started
    /// immediately afterwards finds it.
    ///
    /// # Errors
    ///
    /// [`TransportError::EndpointInUse`] if another process is already
    /// listening there, and [`TransportError::Platform`] if Windows refused for
    /// any other reason — most usefully, if the process token could not be read
    /// to build the access-control list, since a pipe created without one would
    /// be readable by other accounts.
    pub fn bind(endpoint: &Endpoint) -> Result<Self, TransportError> {
        let security = security_descriptor_sddl()?;
        let pending = create_instance(endpoint, &security, FirstInstance::Yes)?;

        tracing::info!(
            endpoint = %endpoint,
            "listening for the desktop application"
        );

        Ok(Self {
            endpoint: endpoint.clone(),
            security,
            pending,
            stopping: Arc::new(AtomicBool::new(false)),
        })
    }

    /// A handle that can stop this listener from another thread.
    #[must_use]
    pub fn stopper(&self) -> ListenerStopper {
        ListenerStopper {
            endpoint: self.endpoint.clone(),
            stopping: Arc::clone(&self.stopping),
        }
    }

    /// Where this listener is.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Blocks until a client connects, or until the listener is stopped.
    ///
    /// Returns [`None`] once [`ListenerStopper::stop`] has been called, which
    /// is the signal to leave the accept loop.
    ///
    /// # Errors
    ///
    /// [`TransportError::Platform`] if the connection could not be waited for,
    /// or if the replacement instance for the next client could not be created.
    pub fn accept(&mut self) -> Result<Option<Connection>, TransportError> {
        if self.stopping.load(Ordering::SeqCst) {
            return Ok(None);
        }

        connect_instance(&self.pending)?;

        if self.stopping.load(Ordering::SeqCst) {
            // The connection that woke us is the stopper's own, and there is
            // nobody to serve. Dropping `self.pending` here would leave the
            // struct without one, so the instance is replaced as usual and both
            // are closed when the listener is dropped.
            return Ok(None);
        }

        // The next instance before this one is handed over, so the endpoint is
        // never momentarily absent: a client that connects while a connection
        // is being set up finds a pipe to wait on rather than "no recorder is
        // listening".
        let next = create_instance(&self.endpoint, &self.security, FirstInstance::No)?;
        let connected = std::mem::replace(&mut self.pending, next);

        Ok(Some(Connection { pipe: connected }))
    }
}

/// Opens a connection to a recorder.
///
/// Retries while the endpoint is momentarily unavailable — the instant between
/// a listener accepting one client and creating the instance for the next, and
/// the moment a recorder is starting — because both are normal and neither is
/// worth reporting to a user as a failure. Anything else is returned at once.
///
/// # Errors
///
/// [`TransportError::NotListening`] if no recorder appeared within `timeout`,
/// [`TransportError::AccessDenied`] if the endpoint belongs to another account,
/// and [`TransportError::Platform`] for anything else.
pub fn connect(endpoint: &Endpoint, timeout: Duration) -> Result<Connection, TransportError> {
    let deadline = Instant::now() + timeout;

    loop {
        match open_client(endpoint) {
            Ok(connection) => return Ok(connection),
            Err(error) => {
                let code = error.raw_os_error().map(|code| WIN32_ERROR(code as u32));
                let retryable = matches!(code, Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY));

                if !retryable {
                    return Err(if code == Some(ERROR_ACCESS_DENIED) {
                        TransportError::AccessDenied {
                            endpoint: endpoint.path(),
                        }
                    } else {
                        TransportError::Platform(error)
                    });
                }

                if Instant::now() >= deadline {
                    return Err(TransportError::NotListening {
                        endpoint: endpoint.path(),
                    });
                }

                thread::sleep(CONNECT_RETRY_INTERVAL);
            }
        }
    }
}

/// The terminal session this process belongs to.
///
/// # Errors
///
/// [`TransportError::Platform`] if Windows would not say.
pub(super) fn current_session_id() -> Result<u32, TransportError> {
    let mut session = 0_u32;

    // SAFETY: `GetCurrentProcessId` takes nothing and returns an integer.
    // `ProcessIdToSessionId` writes one `u32` through the pointer, which points
    // at a live local. Neither can fail in a way that leaves state behind.
    let queried = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &raw mut session) };
    queried.map_err(platform)?;

    Ok(session)
}

/// Whether an instance is the first one on its endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstInstance {
    /// Created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so it fails if the
    /// endpoint already exists.
    Yes,
    /// An additional instance of an endpoint this process already owns.
    No,
}

/// Creates one instance of the endpoint's pipe.
fn create_instance(
    endpoint: &Endpoint,
    sddl: &str,
    first: FirstInstance,
) -> Result<File, TransportError> {
    let path = wide(&endpoint.path());
    let descriptor = SecurityDescriptor::from_sddl(sddl)?;

    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: false.into(),
    };

    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first == FirstInstance::Yes {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }

    // SAFETY: `path` is a NUL-terminated wide string that outlives the call.
    // `attributes` points at a live local whose security descriptor outlives
    // the call as well, held by `descriptor`. The handle returned is taken over
    // by a `File` below, which closes it exactly once.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(path.as_ptr()),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(&raw mut attributes),
        )
    };

    if handle.is_invalid() {
        // SAFETY: reading the calling thread's last error immediately after the
        // call that set it. It takes no arguments and returns an integer.
        let failure = unsafe { GetLastError() };
        return Err(if failure == ERROR_ACCESS_DENIED {
            // With `FILE_FLAG_FIRST_PIPE_INSTANCE` this is what "somebody else
            // has this name" looks like, and it is much the more likely reading
            // of it: the alternative would be a pipe namespace this account
            // cannot write to at all.
            TransportError::EndpointInUse {
                endpoint: endpoint.path(),
            }
        } else {
            win32(failure)
        });
    }

    // SAFETY: the handle was just created and is owned by nothing else, so the
    // `File` becomes its sole owner and closes it when dropped.
    Ok(unsafe { File::from_raw_handle(handle.0.cast()) })
}

/// Waits for a client on an instance that has none.
fn connect_instance(instance: &File) -> Result<(), TransportError> {
    let handle = HANDLE(raw_handle(instance));

    // SAFETY: `handle` is the live pipe instance owned by the caller's `File`,
    // and it outlives the call. `None` asks for the blocking form, which needs
    // no `OVERLAPPED` to stay alive.
    let connected = unsafe { ConnectNamedPipe(handle, None) };

    match connected {
        Ok(()) => Ok(()),
        // The client got there between the instance being created and this
        // call. Documented as success, and reported as an error only because
        // there is no other way for the API to say "already".
        Err(error) if is(&error, ERROR_PIPE_CONNECTED) => Ok(()),
        Err(error) => Err(platform(error)),
    }
}

/// Opens the client end of an endpoint, once, without retrying.
fn open_client(endpoint: &Endpoint) -> io::Result<Connection> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(endpoint.path())
        .map(|pipe| Connection { pipe })
}

/// The SDDL for "this account, and nothing else".
///
/// `D:P` is a protected DACL — protected so that no inherited entry can widen
/// it — containing one entry granting `GA` (everything) to the user this
/// process is running as.
fn security_descriptor_sddl() -> Result<String, TransportError> {
    let sid = current_user_sid()?;
    Ok(format!("D:P(A;;GA;;;{sid})"))
}

/// The string form of the SID this process is running as.
fn current_user_sid() -> Result<String, TransportError> {
    let token = ProcessToken::open()?;
    let user = token.user_information()?;

    // SAFETY: `user` is the buffer `GetTokenInformation` filled with a
    // `TOKEN_USER`, whose `User.Sid` points inside that same buffer, which is
    // alive for the whole of this function.
    let record = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };

    let mut text = PWSTR::null();
    // SAFETY: the SID is valid for the life of `user`, and the out-parameter
    // points at a live local. On success Windows has allocated the string with
    // `LocalAlloc`, which `LocalFree` below releases.
    unsafe { ConvertSidToStringSidW(record.User.Sid, &raw mut text) }.map_err(platform)?;

    // SAFETY: `text` is the NUL-terminated string Windows just allocated.
    let sid = unsafe { text.to_string() }.map_err(|error| {
        TransportError::Platform(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Windows returned a SID that is not valid UTF-16: {error}"),
        ))
    });

    // SAFETY: `text` was allocated by `ConvertSidToStringSidW` with
    // `LocalAlloc`, is freed exactly once, and is not used afterwards.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.as_ptr().cast())));
    }

    sid
}

/// This process's access token, closed when dropped.
#[derive(Debug)]
struct ProcessToken(HANDLE);

impl ProcessToken {
    fn open() -> Result<Self, TransportError> {
        let mut token = HANDLE::default();

        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // closing, and the out-parameter points at a live local. The handle
        // written into it is owned by the `ProcessToken` returned.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
            .map_err(platform)?;

        Ok(Self(token))
    }

    /// The `TOKEN_USER` structure, in a buffer that owns it.
    fn user_information(&self) -> Result<Vec<u8>, TransportError> {
        let mut needed = 0_u32;

        // SAFETY: asking for the size with no buffer is the documented first
        // half of this call. It fails with ERROR_INSUFFICIENT_BUFFER and writes
        // the required length through the pointer, which points at a live
        // local.
        let sized = unsafe { GetTokenInformation(self.0, TokenUser, None, 0, &raw mut needed) };
        match sized {
            Ok(()) => {}
            Err(error) if is(&error, ERROR_INSUFFICIENT_BUFFER) => {}
            Err(error) => return Err(platform(error)),
        }

        let mut buffer = vec![0_u8; needed as usize];

        // SAFETY: the buffer is exactly the length Windows just asked for and
        // is alive for the call; `needed` is written again and ignored.
        unsafe {
            GetTokenInformation(
                self.0,
                TokenUser,
                Some(buffer.as_mut_ptr().cast::<c_void>()),
                needed,
                &raw mut needed,
            )
        }
        .map_err(platform)?;

        Ok(buffer)
    }
}

impl Drop for ProcessToken {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle was opened by `OpenProcessToken`, is owned
            // solely by this value, and is closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// A security descriptor built from SDDL, freed when dropped.
#[derive(Debug)]
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> Result<Self, TransportError> {
        let text = wide(sddl);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();

        // SAFETY: `text` is a NUL-terminated wide string that outlives the
        // call, and the out-parameter points at a live local. On success
        // Windows has allocated the descriptor with `LocalAlloc`, which this
        // value's `Drop` releases.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(text.as_ptr()),
                SDDL_REVISION_1,
                &raw mut descriptor,
                None,
            )
        }
        .map_err(platform)?;

        Ok(Self(descriptor))
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0 .0
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: allocated by
            // `ConvertStringSecurityDescriptorToSecurityDescriptorW` with
            // `LocalAlloc`, owned solely by this value, and freed exactly once.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0 .0)));
            }
        }
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The raw handle behind a `File`, as a `HANDLE`.
fn raw_handle(file: &File) -> *mut c_void {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle().cast()
}

/// Whether a Windows error is a particular Win32 code.
fn is(error: &windows::core::Error, code: WIN32_ERROR) -> bool {
    error.code() == HRESULT::from_win32(code.0)
}

/// A Win32 error code as this module's platform failure.
fn win32(code: WIN32_ERROR) -> TransportError {
    TransportError::Platform(io::Error::from_raw_os_error(code.0 as i32))
}

/// A Windows error as this module's platform failure.
fn platform(error: windows::core::Error) -> TransportError {
    let code = error.code();
    // A Win32 code wrapped in an HRESULT keeps its number in the low sixteen
    // bits. Unwrapping it means `io::Error` prints the system's own message for
    // it — "Access is denied" rather than a hexadecimal HRESULT nobody can look
    // up (AGENTS.md section 15).
    let raw = if code.0 & 0x7FFF_0000_u32 as i32 == 0x0007_0000 {
        code.0 & 0xFFFF
    } else {
        code.0
    };
    TransportError::Platform(io::Error::from_raw_os_error(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{read_message, write_message};

    /// An endpoint name no other test or recorder will be using.
    ///
    /// The pipe namespace is machine-wide, so a name shared with the recorder
    /// somebody is actually running would have a test talking to it.
    fn unique_endpoint(label: &str) -> Endpoint {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::SeqCst);
        Endpoint::named(&format!(
            "clipped-ipc-test.{label}.{}.{ordinal}",
            std::process::id()
        ))
        .expect("the generated name is valid")
    }

    #[test]
    fn a_client_and_a_listener_exchange_a_message_over_a_real_pipe() {
        let mut listener = Listener::bind(&unique_endpoint("round-trip")).expect("it binds");
        let endpoint = listener.endpoint().clone();

        let client = thread::spawn(move || {
            let mut connection =
                connect(&endpoint, Duration::from_secs(5)).expect("the listener is there");
            write_message(&mut connection, &"ping").expect("it writes");
            read_message::<_, String>(&mut connection).expect("it reads the reply")
        });

        let mut served = listener
            .accept()
            .expect("accepting works")
            .expect("a client arrived");
        let request: String = read_message(&mut served).expect("it reads");
        assert_eq!(request, "ping");
        write_message(&mut served, &"pong").expect("it writes");

        assert_eq!(client.join().expect("the client thread finished"), "pong");
    }

    #[test]
    fn a_second_listener_on_the_same_endpoint_is_refused_rather_than_sharing_it() {
        let endpoint = unique_endpoint("exclusive");
        let _first = Listener::bind(&endpoint).expect("the first one binds");

        let error = Listener::bind(&endpoint).expect_err("the second one must not");
        assert!(
            matches!(error, TransportError::EndpointInUse { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn connecting_to_an_endpoint_nobody_is_listening_on_says_so() {
        let error = connect(&unique_endpoint("absent"), Duration::from_millis(100))
            .expect_err("nothing is there");
        assert!(
            matches!(error, TransportError::NotListening { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_stopped_listener_stops_accepting() {
        let mut listener = Listener::bind(&unique_endpoint("stop")).expect("it binds");
        let stopper = listener.stopper();

        let stopping = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            stopper.stop();
        });

        // Blocks until the stopper's own connection releases it; the test
        // hanging here is the failure it guards against.
        assert!(
            listener.accept().expect("accepting works").is_none(),
            "a stopped listener must not hand back a connection"
        );
        stopping.join().expect("the stopping thread finished");
    }

    #[test]
    fn the_listener_serves_one_client_after_another() {
        let mut listener = Listener::bind(&unique_endpoint("sequence")).expect("it binds");
        let endpoint = listener.endpoint().clone();

        for round in 0..3_u32 {
            let endpoint = endpoint.clone();
            let client = thread::spawn(move || {
                let mut connection =
                    connect(&endpoint, Duration::from_secs(5)).expect("the listener is there");
                write_message(&mut connection, &round).expect("it writes");
            });

            let mut served = listener
                .accept()
                .expect("accepting works")
                .expect("a client arrived");
            assert_eq!(
                read_message::<_, u32>(&mut served).expect("it reads"),
                round,
                "each connection should be served in turn"
            );
            client.join().expect("the client thread finished");
        }
    }

    #[test]
    fn this_process_is_in_a_terminal_session() {
        // Not much of an assertion on its own; what it covers is that the call
        // succeeds, since the default endpoint name depends on it and a failure
        // here would leave the recorder unable to choose one.
        current_session_id().expect("Windows knows which session this process is in");
    }
}
