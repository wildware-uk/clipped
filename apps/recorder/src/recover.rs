//! The `recover` subcommand: the footage a killed recorder left behind.
//!
//! `clipped-session`'s `automatic::recovery` does the work — it reads the
//! session records in a directory, finds the recordings that began and were
//! never recorded as having ended, and closes them off. This module is the
//! command line over it, and the part that decides what a person is told.
//!
//! # What it is for
//!
//! A recorder that is killed leaves a Matroska file that plays as far as the
//! last cluster it closed ([ADR 0001], `docs/muxing.md`) and a session record
//! that never says the recording finished. The footage is not lost; nothing
//! knows about it, and every later launch would offer it again. This is where
//! somebody says "keep it" or "throw it away", once.
//!
//! [ADR 0001]: ../../../docs/adr/0001-mkv-archival-container.md
//!
//! # Why listing is the default
//!
//! Because the destructive action is the other one. Running `recover` with no
//! arguments prints what there is and changes nothing, which is the behaviour
//! somebody investigating "where did my recording go" wants; `--adopt` and
//! `--discard` are what they type once they have read it (AGENTS.md section
//! 56).

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clipped_logging::RedactedPath;
use clipped_session::automatic::recovery::{
    adopt, discard, interrupted_recordings, InterruptedRecording,
};
use clipped_session::disk::describe_bytes;

use crate::cli::{RecoverAction, RecoverArgs};

/// Why `recover` could not answer.
#[derive(Debug)]
pub enum RecoverError {
    /// There is no home directory, and none was named.
    NoDirectory,
    /// The recordings directory could not be read.
    Unreadable {
        /// The directory.
        directory: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// A session's record could not be rewritten, so a recording could not be
    /// closed off.
    NotClosed {
        /// Which session.
        session: String,
        /// What the filesystem said.
        source: io::Error,
    },
    /// `--session` named something that is not waiting to be recovered.
    NoSuchRecording {
        /// What was named.
        session: String,
    },
    /// `--discard` was asked for without naming exactly one session.
    DiscardNeedsASession,
}

impl fmt::Display for RecoverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDirectory => formatter.write_str(
                "there is no home directory to look for recordings in, so a directory is required",
            ),
            Self::Unreadable { directory, source } => write!(
                formatter,
                "{} could not be read: {source}",
                directory.display()
            ),
            Self::NotClosed { session, source } => write!(
                formatter,
                "the record of session {session} could not be updated, so the recording is still \
                 listed as interrupted: {source}. The footage itself is untouched"
            ),
            Self::NoSuchRecording { session } => write!(
                formatter,
                "no interrupted recording of session {session} was found; run `clipped-recorder \
                 recover` with no arguments to see what there is"
            ),
            Self::DiscardNeedsASession => formatter.write_str(
                "--discard deletes a recording, so it names one: pass --session <ID> with the \
                 identifier from `clipped-recorder recover`",
            ),
        }
    }
}

impl Error for RecoverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } | Self::NotClosed { source, .. } => Some(source),
            Self::NoDirectory | Self::NoSuchRecording { .. } | Self::DiscardNeedsASession => None,
        }
    }
}

/// Reports, adopts or discards the recordings an interrupted recorder left.
///
/// Standard output is left empty, as it is for every other subcommand: what
/// this produces is a decision about files, and everything it says goes to
/// standard error and to the log (`docs/recorder-cli.md`).
///
/// # Errors
///
/// [`RecoverError`], which names what could not be read or rewritten. Nothing
/// here can damage a recording: the only path that deletes one is `--discard`,
/// and it names a single session.
pub fn run(args: &RecoverArgs) -> Result<(), RecoverError> {
    let directory = match &args.directory {
        Some(named) => named.clone(),
        None => crate::config::default_output_directory().ok_or(RecoverError::NoDirectory)?,
    };

    let found = interrupted_recordings(&directory).map_err(|source| RecoverError::Unreadable {
        directory: directory.clone(),
        source,
    })?;

    tracing::info!(
        directory = %RedactedPath::new(&directory),
        interrupted = found.len(),
        "looked for recordings an interrupted recorder left behind"
    );

    if found.is_empty() {
        eprintln!(
            "No interrupted recordings in {}. Nothing to recover.",
            directory.display()
        );
        return Ok(());
    }

    let chosen = select(&found, args.session.as_deref())?;

    match args.action() {
        RecoverAction::List => {
            report(&chosen, &directory);
            Ok(())
        }
        RecoverAction::Adopt => adopt_all(&chosen),
        RecoverAction::Discard => discard_all(&chosen, args.session.as_deref()),
    }
}

/// The recordings the arguments name.
fn select<'a>(
    found: &'a [InterruptedRecording],
    session: Option<&str>,
) -> Result<Vec<&'a InterruptedRecording>, RecoverError> {
    let Some(session) = session else {
        return Ok(found.iter().collect());
    };

    let chosen: Vec<&InterruptedRecording> = found
        .iter()
        .filter(|recording| recording.session_id() == session)
        .collect();

    if chosen.is_empty() {
        return Err(RecoverError::NoSuchRecording {
            session: session.to_owned(),
        });
    }
    Ok(chosen)
}

/// Prints what there is, and what can be done about it.
fn report(chosen: &[&InterruptedRecording], directory: &Path) {
    eprintln!(
        "{} interrupted recording{} in {}:",
        chosen.len(),
        if chosen.len() == 1 { "" } else { "s" },
        directory.display()
    );

    for recording in chosen {
        match recording.bytes() {
            Some(bytes) => eprintln!(
                "  {} of {}, started {}, {} at {}",
                recording.session_id(),
                recording.game(),
                recording.started_at(),
                describe_bytes(bytes),
                recording.output().display()
            ),
            // Said differently, because it is a different answer: the recorder
            // died before the encoder produced anything, and there is nothing
            // to keep. Printing a path to a file that is not there would send
            // somebody looking for footage that never existed.
            None => eprintln!(
                "  {} of {}, started {}, no file was written",
                recording.session_id(),
                recording.game(),
                recording.started_at()
            ),
        }
    }

    eprintln!(
        "\nThese recordings play from the start. They have no index, so seeking scans the file \
         until it is rewritten (issue #283)."
    );
    eprintln!("  --adopt                    keep them, and stop listing them here");
    eprintln!("  --discard --session <ID>   delete one recording and record that you did");
}

/// Keeps every chosen recording, and says what was kept.
fn adopt_all(chosen: &[&InterruptedRecording]) -> Result<(), RecoverError> {
    let now = SystemTime::now();

    for recording in chosen {
        adopt(recording, now).map_err(|source| RecoverError::NotClosed {
            session: recording.session_id().to_owned(),
            source,
        })?;

        tracing::info!(
            session = recording.session_id(),
            index = recording.index(),
            bytes = recording.bytes(),
            "an interrupted recording was adopted"
        );
        match recording.bytes() {
            Some(bytes) => eprintln!(
                "Kept {} of {} ({}).",
                recording.output().display(),
                recording.game(),
                describe_bytes(bytes)
            ),
            None => eprintln!(
                "Closed the record of {}: the recorder stopped before anything was written.",
                recording.session_id()
            ),
        }
    }
    Ok(())
}

/// Deletes the one chosen recording.
fn discard_all(
    chosen: &[&InterruptedRecording],
    session: Option<&str>,
) -> Result<(), RecoverError> {
    // Never in bulk. This is the only path in the recorder that deletes
    // somebody's footage, and it is reached by naming the recording it deletes
    // (AGENTS.md section 56).
    if session.is_none() {
        return Err(RecoverError::DiscardNeedsASession);
    }
    let now = SystemTime::now();

    for recording in chosen {
        let discarded = discard(recording, now).map_err(|source| RecoverError::NotClosed {
            session: recording.session_id().to_owned(),
            source,
        })?;

        tracing::info!(
            session = recording.session_id(),
            index = recording.index(),
            bytes_freed = discarded.bytes_freed(),
            "an interrupted recording was discarded"
        );
        match discarded.bytes_freed() {
            Some(bytes) => eprintln!(
                "Discarded {} ({} freed).",
                recording.output().display(),
                describe_bytes(bytes)
            ),
            None => eprintln!(
                "Closed the record of {}: there was no file to delete.",
                recording.session_id()
            ),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn args(arguments: &[&str]) -> RecoverArgs {
        let parsed = Cli::try_parse_from(
            std::iter::once("clipped-recorder").chain(arguments.iter().copied()),
        )
        .expect("the arguments are valid");
        let Command::Recover(args) = parsed.command else {
            panic!("expected the recover subcommand");
        };
        args
    }

    #[test]
    fn listing_is_what_happens_when_nothing_is_asked_for() {
        // The default has to be the one that changes nothing: somebody typing
        // `recover` to find out where their recording went must not have
        // anything happen to it.
        assert_eq!(args(&["recover"]).action(), RecoverAction::List);
        assert_eq!(args(&["recover", "--adopt"]).action(), RecoverAction::Adopt);
        assert_eq!(
            args(&["recover", "--discard", "--session", "cs2-1"]).action(),
            RecoverAction::Discard
        );
    }

    #[test]
    fn discarding_without_naming_a_session_is_refused_rather_than_applied_to_everything() {
        // The failure this rules out is somebody typing `recover --discard`,
        // meaning "throw away that one", and losing every interrupted
        // recording on the machine.
        let error = discard_all(&[], None).expect_err("a bulk discard must be refused");

        assert!(matches!(error, RecoverError::DiscardNeedsASession));
        assert!(
            error.to_string().contains("--session"),
            "the refusal should say what to type: {error}"
        );
    }

    #[test]
    fn a_session_that_is_not_waiting_to_be_recovered_is_said_so_rather_than_ignored() {
        let error = select(&[], Some("cs2-20260811-143205"))
            .expect_err("nothing is waiting to be recovered");

        match &error {
            RecoverError::NoSuchRecording { session } => {
                assert_eq!(session, "cs2-20260811-143205");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(
            error.to_string().contains("recover"),
            "the message should say how to see what there is: {error}"
        );
    }

    #[test]
    fn a_failure_to_rewrite_a_record_says_the_footage_is_untouched() {
        // The sentence that matters: a user who is told "the recording could
        // not be recovered" will assume the file is gone, and it is not.
        let error = RecoverError::NotClosed {
            session: "cs2-20260811-143205".to_owned(),
            source: io::Error::other("the file is read-only"),
        };

        assert!(error.to_string().contains("footage itself is untouched"));
        assert!(error.source().is_some(), "the cause should stay reachable");
    }
}
