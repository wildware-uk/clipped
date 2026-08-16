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

use clipped_library::trash::{Trash, TrashError};
use clipped_logging::RedactedPath;
use clipped_session::automatic::recovery::{
    adopt, interrupted_recordings, record_discarded, InterruptedRecording,
};
use clipped_session::cleanup;
use clipped_session::config::ConfigurationStore;
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
    /// A recording could not be moved into the trash. Nothing was touched:
    /// every way this fails leaves the file exactly where it was.
    NotTrashed {
        /// Which session.
        session: String,
        /// What the trash refused, or why the move failed.
        source: TrashError,
    },
    /// The file was moved to the trash, but the session's record could not be
    /// rewritten to say so.
    RecordNotUpdated {
        /// Which session.
        session: String,
        /// Where the file actually is now, so it can still be found by hand.
        trash_path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
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
                "--discard moves one recording's file, so it names one: pass --session <ID> \
                 with the identifier from `clipped-recorder recover`. It is recoverable, but \
                 never as a choice nobody made item by item",
            ),
            Self::NotTrashed { session, source } => write!(
                formatter,
                "the recording of session {session} could not be moved to the trash: {source}. \
                 Nothing was deleted"
            ),
            Self::RecordNotUpdated {
                session,
                trash_path,
                source,
            } => write!(
                formatter,
                "session {session}'s file was moved to the trash, at {}, but its record could \
                 not be updated to say so: {source}. The footage is safe there",
                trash_path.display()
            ),
        }
    }
}

impl Error for RecoverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unreadable { source, .. }
            | Self::NotClosed { source, .. }
            | Self::RecordNotUpdated { source, .. } => Some(source),
            Self::NotTrashed { source, .. } => Some(source),
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
/// here can lose a recording: the only path that moves one is `--discard`, it
/// names a single session, and what it moves to is `clipped_library`'s trash
/// directory rather than deletion (issue #451). That makes it recoverable —
/// the file is on disk, untouched — though not yet under the same retention
/// and restore screen as everything else deleted from the library; see
/// [`discard_all`] for exactly what that costs.
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
        RecoverAction::Discard => {
            // Resolved here, once, and only for the branch that needs it: a
            // settings file read is wasted work on every `recover` call that
            // is only listing or adopting.
            let configuration =
                crate::watch::load_configuration(ConfigurationStore::default_path().as_deref());
            let trash = Trash::new(cleanup::trash_directory(&configuration, &directory));
            discard_all(&chosen, args.session.as_deref(), &trash)
        }
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
    eprintln!(
        "  --discard --session <ID>   move one recording to the trash and record that you did"
    );
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

/// Moves the one chosen recording to the trash.
///
/// Two steps, in this order and not the other: the file goes into `trash`
/// first, and the session record is only rewritten once that has actually
/// happened. A rewrite that failed before the move would risk a sidecar
/// saying "discarded" about a file that was still sitting in the library; a
/// move that fails leaves the sidecar saying the recording is still open,
/// which is the truth and offers it again rather than losing track of it.
///
/// # Where the file goes, and what that does not include
///
/// `trash` is `clipped_library`'s real trash directory — the same one a
/// deletion from the library uses — but the row that usually comes with a
/// trashed item does not exist here: an interrupted recording has no library
/// row until it is closed off, and closing it off with the `discarded`
/// outcome is exactly what this function is doing. So the file is genuinely
/// recoverable — it is sitting on disk, untouched, inside the trash directory
/// — but it will not appear on a trash screen or be swept by retention until
/// something adopts it into the index (`Trash::stow_untracked`'s own
/// documentation says why, and `docs/recorder-cli.md` says it to the person
/// running the command).
fn discard_all(
    chosen: &[&InterruptedRecording],
    session: Option<&str>,
    trash: &Trash,
) -> Result<(), RecoverError> {
    // Never in bulk. This is the only path in the recorder that moves
    // somebody's footage into the trash, and it is reached by naming the
    // recording it moves (AGENTS.md section 56).
    if session.is_none() {
        return Err(RecoverError::DiscardNeedsASession);
    }
    let now = SystemTime::now();

    for recording in chosen {
        let stowed = trash
            .stow_untracked(recording.output(), now)
            .map_err(|source| RecoverError::NotTrashed {
                session: recording.session_id().to_owned(),
                source,
            })?;

        record_discarded(recording, now).map_err(|source| RecoverError::RecordNotUpdated {
            session: recording.session_id().to_owned(),
            trash_path: stowed
                .path
                .clone()
                .unwrap_or_else(|| recording.output().to_path_buf()),
            source,
        })?;

        tracing::info!(
            session = recording.session_id(),
            index = recording.index(),
            moved = stowed.path.is_some(),
            "an interrupted recording was discarded"
        );
        match &stowed.path {
            Some(path) => eprintln!(
                "Discarded {}: moved to the trash at {}. It does not yet show on the trash \
                 screen -- open the folder to restore it or remove it for good; it will not \
                 expire on its own.",
                recording.output().display(),
                path.display()
            ),
            None => eprintln!(
                "Closed the record of {}: there was no file to move.",
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
        // The trash is never even resolved: the refusal has to come before
        // anything about where a file might go is asked.
        let trash = Trash::new(PathBuf::from("unused"));
        let error = discard_all(&[], None, &trash).expect_err("a bulk discard must be refused");

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

    /// A directory under the system temporary directory, removed when dropped.
    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new(purpose: &str) -> Self {
            use core::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "clipped-recover-cli-{purpose}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("a temporary directory can be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes a sidecar whose one recording never ended, and the file behind
    /// it — what a killed recorder leaves. The same shape
    /// `clipped_session::automatic::recovery`'s own tests write, kept in step
    /// by hand since the shape is `clipped_session`'s and not this crate's to
    /// import a fixture for.
    fn interrupted_session(directory: &Path, session_id: &str, bytes: usize) {
        let recording = directory.join(format!("clipped-{session_id}.mkv"));
        std::fs::write(&recording, vec![0u8; bytes]).expect("the recording can be written");

        let sidecar = directory.join(format!("clipped-{session_id}.session.json"));
        let file = serde_json::json!({
            "schema_version": 1,
            "session_id": session_id,
            "game": { "kind": "known", "game_id": "counter-strike-2", "name": "Counter-Strike 2" },
            "started_at": "2026-08-11T14:32:05+01:00",
            "ended_at": null,
            "recordings": [{
                "index": 1,
                "output": recording.display().to_string(),
                "started_at": "2026-08-11T14:32:05+01:00",
                "ended_at": null,
                "outcome": null
            }],
            "clips": [],
            "bookmarks": [],
            "events": [
                { "at": "2026-08-11T14:32:05+01:00", "event": "session-started", "pid": 4242 },
                { "at": "2026-08-11T14:32:05+01:00", "event": "recording-started", "index": 1 }
            ]
        });
        std::fs::write(
            &sidecar,
            serde_json::to_vec_pretty(&file).expect("the shape encodes"),
        )
        .expect("the sidecar can be written");
    }

    #[test]
    fn discarding_moves_the_file_into_the_trash_rather_than_deleting_it() {
        // The acceptance criterion (issue #451), proved end to end rather than
        // by inspecting what `discard_all` calls: the bytes exist somewhere on
        // disk afterwards, and recovering the same directory again does not
        // offer the recording a second time.
        let directory = TemporaryDirectory::new("discard-e2e");
        interrupted_session(directory.path(), "cs2-20260811-143205", 4096);
        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");
        let chosen: Vec<&InterruptedRecording> = found.iter().collect();
        let original = chosen[0].output().to_path_buf();
        let trash_directory = directory.path().join("Trash");
        let trash = Trash::new(&trash_directory);

        discard_all(&chosen, Some("cs2-20260811-143205"), &trash)
            .expect("the recording is discarded");

        assert!(
            !original.exists(),
            "the file was left in the recordings directory"
        );
        let moved = std::fs::read_dir(&trash_directory)
            .expect("something was moved into the trash directory")
            .filter_map(Result::ok)
            .flat_map(|entry| std::fs::read_dir(entry.path()).into_iter().flatten())
            .filter_map(Result::ok)
            .find(|entry| entry.file_name() == "clipped-cs2-20260811-143205.mkv");
        assert!(
            moved.is_some(),
            "the recording's file could not be found anywhere in the trash"
        );

        assert!(
            interrupted_recordings(directory.path())
                .expect("listed")
                .is_empty(),
            "a discarded recording must not be offered for recovery again"
        );
    }

    #[test]
    fn discarding_a_recording_with_no_footage_closes_the_record_without_touching_the_trash() {
        // A recorder killed before the encoder wrote a first packet leaves a
        // sidecar entry and no file (issue #283). There is nothing to move,
        // and that must not be treated as a failure.
        let directory = TemporaryDirectory::new("discard-no-footage");
        interrupted_session(directory.path(), "cs2-20260811-143205", 0);
        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");
        std::fs::remove_file(found[0].output()).expect("the file can be removed");
        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");
        let chosen: Vec<&InterruptedRecording> = found.iter().collect();
        let trash = Trash::new(directory.path().join("Trash"));

        discard_all(&chosen, Some("cs2-20260811-143205"), &trash)
            .expect("discarding with no footage is not a failure");

        assert!(
            interrupted_recordings(directory.path())
                .expect("listed")
                .is_empty(),
            "the record must still be closed even with nothing to move"
        );
    }
}
