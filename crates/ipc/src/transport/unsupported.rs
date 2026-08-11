//! The transport on a platform that has no named pipes.
//!
//! The workspace compiles and unit-tests off Windows so that a contributor's
//! other machine is not useless to them, and the protocol, the framing and the
//! dispatch loop are all platform-independent and tested there. What cannot
//! exist is the transport, so it says so plainly rather than being quietly
//! absent — the same choice `clipped-session` makes for recording itself
//! (AGENTS.md section 54).

use std::io::{self, Read, Write};
use std::time::Duration;

use super::{Endpoint, TransportError};

/// A connection that cannot be created on this platform.
#[derive(Debug)]
pub struct Connection(());

impl Read for Connection {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        unreachable!("no connection can be created on this platform")
    }
}

impl Write for Connection {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        unreachable!("no connection can be created on this platform")
    }

    fn flush(&mut self) -> io::Result<()> {
        unreachable!("no connection can be created on this platform")
    }
}

impl Connection {
    /// Unreachable: no connection can be constructed.
    ///
    /// # Errors
    ///
    /// Never returns.
    pub fn server_process_id(&self) -> io::Result<u32> {
        unreachable!("no connection can be created on this platform")
    }
}

/// A listener that cannot be created on this platform.
#[derive(Debug)]
pub struct Listener(());

/// A stopper for a listener that cannot exist.
#[derive(Debug, Clone)]
pub struct ListenerStopper(());

impl ListenerStopper {
    /// Does nothing; there is no listener.
    pub fn stop(&self) {}

    /// Always false.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        false
    }
}

impl Listener {
    /// Always fails.
    ///
    /// # Errors
    ///
    /// Always [`TransportError::Unsupported`].
    pub fn bind(_endpoint: &Endpoint) -> Result<Self, TransportError> {
        Err(TransportError::Unsupported)
    }

    /// Unreachable: no listener can be constructed.
    #[must_use]
    pub fn stopper(&self) -> ListenerStopper {
        ListenerStopper(())
    }

    /// Unreachable: no listener can be constructed.
    ///
    /// # Errors
    ///
    /// Always [`TransportError::Unsupported`].
    pub fn accept(&mut self) -> Result<Option<Connection>, TransportError> {
        Err(TransportError::Unsupported)
    }

    /// Unreachable: no listener can be constructed.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        unreachable!("no listener can be created on this platform")
    }
}

/// Always fails.
///
/// # Errors
///
/// Always [`TransportError::Unsupported`].
pub fn connect(_endpoint: &Endpoint, _timeout: Duration) -> Result<Connection, TransportError> {
    Err(TransportError::Unsupported)
}
