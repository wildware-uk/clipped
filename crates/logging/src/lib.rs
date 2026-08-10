//! Diagnostic subscriber setup, log configuration and typed logging context.
//!
//! Clipped runs alongside games for hours at a time and is usually debugged
//! without access to the machine it failed on, so logs are the primary
//! diagnostic (SPEC.md section 36). This crate owns *where logs go and how much
//! is recorded*. It deliberately does not own *logging itself*: every other
//! crate depends on `tracing` and calls [`tracing::info!`] and friends
//! directly, so nothing has to route diagnostics through a Clipped-specific
//! wrapper.
//!
//! # Responsibilities
//!
//! - Installing the process-wide subscriber ([`init`]).
//! - Resolving the log level from the environment and a configuration file,
//!   without a rebuild.
//! - File output with bounded rotation under a documented per-user directory.
//! - The standard context fields ([`SessionContext`]) as types rather than
//!   loose strings, so a typo cannot produce an unsearchable log line.
//! - The guard rails that keep per-frame logging off capture threads
//!   ([`trace_frame!`], [`FrameSampler`]).
//!
//! # Not responsible for
//!
//! Emitting events (every crate does that itself), the diagnostics overlay
//! (SPEC.md section 37) or the support bundle exporter (milestone M14).
//!
//! # Position in the architecture
//!
//! A leaf crate. It depends on no other `clipped-*` crate so that platform
//! primitives and application logic alike can use it without creating a cycle.
//!
//! # Dependencies
//!
//! Three, all MIT-licensed and therefore compatible with MPL-2.0:
//!
//! - `tracing` is the facade the whole Rust ecosystem has settled on, and the
//!   only one with structured spans rather than formatted strings, which is
//!   what AGENTS.md section 35 asks for.
//! - `tracing-subscriber` provides the subscriber and `EnvFilter`, the
//!   directive parser that makes the level configurable at runtime.
//! - `tracing-appender` provides rolling file output and a background writer
//!   thread, so a capture thread never blocks on disk (AGENTS.md section 20).
//!
//! # Threading
//!
//! [`init`] must be called once, early, from the process's main thread, and the
//! returned [`LoggingGuard`] must be kept alive until shutdown. Events written
//! after the guard is dropped are discarded because the background writer has
//! stopped. Everything else in this crate is usable from any thread;
//! [`FrameSampler`] is deliberately not `Sync`-shared state and belongs to a
//! single thread.
//!
//! # Example
//!
//! ```no_run
//! use clipped_logging::{LogSettings, SessionContext, SessionId};
//!
//! let _guard = clipped_logging::init(&LogSettings::default())?;
//!
//! let context = SessionContext::new(SessionId::new("01H8XGJ")?);
//! let span = context.span();
//! let _entered = span.enter();
//! tracing::info!("recording started");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod context;
mod error;
mod filter;
mod frame;
mod init;
mod redact;

pub use context::{AudioSource, CaptureBackend, GameId, SessionContext, SessionId, VideoEncoder};
pub use error::{IdentifierRejection, LoggingError};
pub use frame::{FrameSampler, FRAME_TRACING};
pub use init::{
    default_level_file, default_log_directory, init, LogSettings, LoggingGuard,
    DEFAULT_RETAINED_FILES,
};
pub use redact::RedactedPath;

/// Implementation detail of [`trace_frame!`]. Not a stable API.
#[doc(hidden)]
pub mod __macro_support {
    pub use tracing;
}
