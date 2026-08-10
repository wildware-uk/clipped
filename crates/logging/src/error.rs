//! The error type returned by fallible operations in this crate.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Something went wrong while configuring diagnostics.
///
/// These errors are returned to the caller rather than logged, because most of
/// them happen before a subscriber exists. Messages therefore favour being
/// understandable on a console: the log directory is named in full so a user
/// can act on a permissions failure.
///
/// [`LoggingError::InvalidIdentifier`] is the exception, and never repeats the
/// value it rejected: the whole point of rejecting it is that it might be
/// content that must not reach a log or a terminal (AGENTS.md section 13).
#[derive(Debug)]
#[non_exhaustive]
pub enum LoggingError {
    /// No log directory was supplied and the platform's per-user data
    /// directory could not be determined from the environment.
    NoLogDirectory,
    /// The log directory could not be created.
    CreateLogDirectory {
        /// The directory that could not be created.
        directory: PathBuf,
        /// The underlying filesystem error.
        source: io::Error,
    },
    /// The rolling file appender could not be opened in the log directory.
    OpenLogFile {
        /// The directory the appender was asked to write into.
        directory: PathBuf,
        /// The underlying appender error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A subscriber has already been installed for this process.
    AlreadyInitialised,
    /// A value offered as a [`SessionId`](crate::SessionId) or
    /// [`GameId`](crate::GameId) is not a plain identifier.
    InvalidIdentifier {
        /// Which field rejected the value, for example `"session_id"`.
        field: &'static str,
        /// Why it was rejected. Never contains the rejected value.
        reason: IdentifierRejection,
    },
}

/// Why a value was refused as a logging identifier.
///
/// Deliberately describes the *shape* of the problem and not the value, so that
/// rejecting a window title or a chat message does not print it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentifierRejection {
    /// The value was empty.
    Empty,
    /// The value was longer than the permitted number of characters.
    TooLong {
        /// The maximum number of characters allowed.
        limit: usize,
        /// How many characters were supplied.
        length: usize,
    },
    /// The value contained a character outside `[A-Za-z0-9._-]`, at the given
    /// zero-based character index.
    ForbiddenCharacter {
        /// Where the first offending character appeared.
        index: usize,
    },
}

impl fmt::Display for LoggingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLogDirectory => write!(
                f,
                "cannot determine a per-user log directory from the environment; \
                 set one explicitly with LogSettings::with_directory"
            ),
            Self::CreateLogDirectory { directory, .. } => {
                write!(f, "failed to create log directory {}", directory.display())
            }
            Self::OpenLogFile { directory, .. } => {
                write!(f, "failed to open a log file in {}", directory.display())
            }
            Self::AlreadyInitialised => {
                write!(
                    f,
                    "a tracing subscriber is already installed for this process"
                )
            }
            Self::InvalidIdentifier { field, reason } => match reason {
                IdentifierRejection::Empty => write!(f, "{field} must not be empty"),
                IdentifierRejection::TooLong { limit, length } => write!(
                    f,
                    "{field} must be at most {limit} characters, but {length} were supplied"
                ),
                IdentifierRejection::ForbiddenCharacter { index } => write!(
                    f,
                    "{field} must contain only letters, digits, '.', '-' and '_', \
                     but character {index} is none of those"
                ),
            },
        }
    }
}

impl std::error::Error for LoggingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateLogDirectory { source, .. } => Some(source),
            Self::OpenLogFile { source, .. } => Some(source.as_ref()),
            Self::NoLogDirectory | Self::AlreadyInitialised | Self::InvalidIdentifier { .. } => {
                None
            }
        }
    }
}
