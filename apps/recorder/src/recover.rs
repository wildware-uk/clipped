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

use clipped_library::index::{reconcile, IndexControl, IndexError, IndexPace, IndexSettings};
use clipped_library::trash::{Trash, TrashError, TrashItem};
use clipped_logging::RedactedPath;
use clipped_session::automatic::recovery::{
    adopt, interrupted_recordings, orphaned_recordings, record_discarded, InterruptedRecording,
    OrphanedRecording,
};
use clipped_session::cleanup;
use clipped_session::config::ConfigurationStore;
use clipped_session::disk::describe_bytes;
use clipped_storage::rusqlite::{params, OptionalExtension};
use clipped_storage::{Database, StorageError};

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
    /// This machine describes no per-user directory, so there is no library
    /// to index a discarded recording into.
    NoLibrary,
    /// The library index could not be opened.
    DatabaseUnavailable {
        /// What the filesystem or the index said.
        ///
        /// Boxed: `StorageError` carries a `PathBuf` and a `rusqlite::Error`
        /// per variant, which is large enough on its own to make every
        /// `Result<_, RecoverError>` pay for the biggest error this type can
        /// hold (`clippy::result_large_err`).
        source: Box<StorageError>,
    },
    /// The recordings directory could not be indexed, so the recording named
    /// by `--session` still has no row and nothing was moved.
    NotIndexed {
        /// Which session.
        session: String,
        /// What the index refused. Boxed for the same reason
        /// [`Self::DatabaseUnavailable`]'s is: `IndexError` wraps a
        /// `StorageError`.
        source: Box<IndexError>,
    },
    /// Indexing finished but the library still has no row for this recording.
    /// This should not happen — a sidecar `interrupted_recordings` just read
    /// names every recording it lists — and is answered rather than
    /// unwrapped.
    NotFoundAfterIndexing {
        /// Which session.
        session: String,
    },
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
            Self::NoLibrary => formatter.write_str(
                "this account has no application data directory, so Clipped has no library to \
                 index a discarded recording into; nothing was touched",
            ),
            Self::DatabaseUnavailable { source } => write!(
                formatter,
                "the library index could not be opened, so a recording cannot be moved to the \
                 trash: {source}. Nothing was touched"
            ),
            Self::NotIndexed { session, source } => write!(
                formatter,
                "the recordings directory could not be indexed, so session {session} still has \
                 no library row and nothing was moved: {source}"
            ),
            Self::NotFoundAfterIndexing { session } => write!(
                formatter,
                "session {session} was indexed but the library still has no row for it, so \
                 nothing could be moved to the trash; the footage itself is untouched"
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
                "session {session}'s file was moved to the trash, at {}, and is now listed \
                 there — but its session record could not be updated to say so: {source}. \
                 Recovering the same directory will offer it again",
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
            Self::DatabaseUnavailable { source } => Some(source),
            Self::NotIndexed { source, .. } => Some(source),
            Self::NotTrashed { source, .. } => Some(source),
            Self::NoDirectory
            | Self::NoSuchRecording { .. }
            | Self::DiscardNeedsASession
            | Self::NoLibrary
            | Self::NotFoundAfterIndexing { .. } => None,
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
/// [`RecoverError`], which names what could not be read, indexed or
/// rewritten. Nothing here can lose a recording: the only path that moves one
/// is `--discard`, it names a single session, and what it moves to is
/// `clipped_library`'s real trash rather than deletion (issue #451) — the
/// same listing, restore and retention as anything else deleted from the
/// library, because `--discard` indexes the fragment before it sends it
/// there. See [`discard_all`] for the order that makes a failure partway
/// through safe rather than a fragment stuck in limbo.
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

    // Media no session record accounts for. Listed alongside the interrupted
    // recordings because they are the same question from a user's side — "there
    // is a file and the application does not know about it" — and because
    // reporting the first while staying silent about the second is how a
    // recording whose sidecar was deleted stayed invisible (issue #272).
    let orphaned = orphaned_recordings(&directory).unwrap_or_else(|error| {
        tracing::warn!(
            directory = %RedactedPath::new(&directory),
            %error,
            "the recording directory could not be searched for unaccounted media; the \
             interrupted recordings were still found"
        );
        Vec::new()
    });

    if found.is_empty() {
        if orphaned.is_empty() {
            eprintln!(
                "No interrupted recordings in {}. Nothing to recover.",
                directory.display()
            );
        } else {
            eprintln!("No interrupted recordings in {}.", directory.display());
            report_orphaned(&orphaned);
        }
        return Ok(());
    }

    let chosen = select(&found, args.session.as_deref())?;

    match args.action() {
        RecoverAction::List => {
            report(&chosen, &directory);
            report_orphaned(&orphaned);
            Ok(())
        }
        RecoverAction::Adopt => adopt_all(&chosen),
        RecoverAction::Discard => {
            // Resolved here, once, and only for the branch that needs it: a
            // settings file read and a database open are wasted work on every
            // `recover` call that is only listing or adopting.
            let configuration =
                crate::watch::load_configuration(ConfigurationStore::default_path().as_deref());
            let trash = Trash::new(cleanup::trash_directory(&configuration, &directory));

            let library_path = clipped_logging::application_directory()
                .map(|home| home.join(crate::library::LIBRARY_FILE))
                .ok_or(RecoverError::NoLibrary)?;
            let mut database = Database::open(&library_path).map_err(|source| {
                RecoverError::DatabaseUnavailable {
                    source: Box::new(source),
                }
            })?;

            discard_all(
                &chosen,
                args.session.as_deref(),
                &directory,
                &mut database,
                &trash,
            )
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
/// Says what media in the folder no session record accounts for.
///
/// Silent when there is none, because a line saying "nothing else" on every run
/// is noise on the machines where this is the ordinary answer.
///
/// Each file is described from what it says about itself rather than from its
/// name: a length, a picture size and a track count are what tell somebody
/// whether the file is footage they want back, and a name and a size are not.
/// A file that could not be opened says so — it is either not media, or damaged
/// beyond reading, and both are worth seeing.
///
/// It offers nothing to do about them. Adopting one means writing a session
/// record, and a session record carries what the recorder measured — the
/// encoder, the codec, how many frames were dropped — none of which is knowable
/// from a file. Inventing those is the one thing this workspace is built not to
/// do, so what an adopted record looks like is a schema question issue #272 has
/// to answer before there is an action to offer (AGENTS.md section 27: a
/// control that did nothing would be worse than none).
fn report_orphaned(orphaned: &[OrphanedRecording]) {
    if orphaned.is_empty() {
        return;
    }

    eprintln!();
    eprintln!(
        "{} recording{} no session record accounts for:",
        orphaned.len(),
        if orphaned.len() == 1 { "" } else { "s" }
    );

    for recording in orphaned {
        match recording.facts() {
            Some(facts) => {
                let length = facts.duration().map_or_else(
                    || "length unknown".to_owned(),
                    |duration| format!("{:.1} s", duration.as_secs_f64()),
                );
                let picture = facts.picture_size().map_or_else(
                    || "no picture".to_owned(),
                    |(width, height)| format!("{width}x{height}"),
                );
                let tracks = facts.audio_tracks();
                eprintln!(
                    "  {}, {length}, {picture}, {tracks} sound track{}, {}",
                    describe_bytes(recording.bytes()),
                    if tracks == 1 { "" } else { "s" },
                    recording.path().display()
                );
            }
            None => eprintln!(
                "  {}, could not be opened, {}",
                describe_bytes(recording.bytes()),
                recording.path().display()
            ),
        }
    }

    eprintln!();
    eprintln!(
        "These are not offered for adoption yet: a session record carries what the recorder \
         measured, and none of it can be read back out of a file. See issue #272."
    );
}

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

/// Indexes, then moves, the chosen recordings to the trash.
///
/// # The order, and why a failure between two steps is always safe
///
/// Three things happen per recording, in this order and not another, because
/// the order is what decides which state a failure between two of them
/// leaves behind — and none of them is a fragment stuck in limbo:
///
/// 1. **Index the recordings directory** (once, for every recording chosen).
///    A recovered fragment has no library row until this runs: the library
///    only indexes a recording once its session record exists, and the
///    sidecar is still open at this point — [`record_discarded`] has not run
///    yet. If indexing fails, nothing else has happened at all: no file has
///    moved and every sidecar is still open, so the next `recover` run offers
///    exactly what this one did.
/// 2. **[`Trash::send`] the row indexing just gave it.** The same path a
///    deletion from the library takes: a rename into the trash, the row
///    marked `deleted_at`. If this fails, the row exists but is not marked
///    deleted, the file is still where it was, and the sidecar is still
///    open — also fully retryable.
/// 3. **Close the sidecar record** with the `discarded` outcome. If *this*
///    fails, the recording is already genuinely in the trash — listed,
///    restorable, swept by retention — but its sidecar still says it is
///    open. Recovering the same directory offers it again; discarding it a
///    second time re-indexes the same row (already marked deleted, so
///    ingestion keeps its trashed path rather than the sidecar's stale one —
///    `crate::index::ingest`'s own rule for a row already in the trash) and
///    [`Trash::send`] answers [`TrashError::AlreadyInTrash`] rather than
///    moving anything twice; only the record-closing step is retried.
///
/// So a failure anywhere in this list leaves the footage exactly as
/// recoverable as it was the moment before — never less, and from step 2
/// onward, strictly more, because the trash's own bookkeeping now knows about
/// it.
fn discard_all(
    chosen: &[&InterruptedRecording],
    session: Option<&str>,
    directory: &Path,
    database: &mut Database,
    trash: &Trash,
) -> Result<(), RecoverError> {
    // Never in bulk. This is the only path in the recorder that moves
    // somebody's footage into the trash, and it is reached by naming the
    // recording it moves (AGENTS.md section 56).
    let Some(session_id) = session else {
        return Err(RecoverError::DiscardNeedsASession);
    };
    let now = SystemTime::now();

    let mut settings = IndexSettings::new([directory.to_path_buf()]);
    settings.pace = IndexPace::foreground();
    reconcile(database, &settings, &IndexControl::new(), now).map_err(|source| {
        RecoverError::NotIndexed {
            session: session_id.to_owned(),
            source: Box::new(source),
        }
    })?;

    for recording in chosen {
        let item =
            indexed_item(database, recording.session_id(), recording.index()).ok_or_else(|| {
                RecoverError::NotFoundAfterIndexing {
                    session: recording.session_id().to_owned(),
                }
            })?;

        let entry = match trash.send(database, item, now) {
            Ok(entry) => entry,
            // A previous `--discard` on this session moved the file and
            // marked the row, then failed at step 3 below before it could
            // close the sidecar. Re-indexing just now kept the row's already
            // -trashed path rather than the sidecar's stale one
            // (`crate::index::ingest`'s rule for a row already in the
            // trash), so this is that retry finishing the one step that did
            // not complete — not a second move, and not an error to report.
            Err(TrashError::AlreadyInTrash { .. }) => trash
                .entry(database, item)
                .map_err(|source| RecoverError::NotTrashed {
                    session: recording.session_id().to_owned(),
                    source,
                })?
                .ok_or(TrashError::NotInTrash { item })
                .map_err(|source| RecoverError::NotTrashed {
                    session: recording.session_id().to_owned(),
                    source,
                })?,
            Err(source) => {
                return Err(RecoverError::NotTrashed {
                    session: recording.session_id().to_owned(),
                    source,
                })
            }
        };

        record_discarded(recording, now).map_err(|source| RecoverError::RecordNotUpdated {
            session: recording.session_id().to_owned(),
            // A recording always names a file -- `recordings.path` is `NOT
            // NULL` -- and where there was nothing to move that name is still
            // where the file would be. Only a clip can be pathless.
            trash_path: entry
                .path
                .clone()
                .unwrap_or_else(|| recording.output().to_path_buf()),
            source,
        })?;

        // `send` leaves `path` equal to `original_path` in exactly the one
        // case where there was nothing to move (`TrashEntry`'s own
        // documentation) — the same "no footage" case `bytes() == None`
        // reports before anything is indexed.
        let moved = entry.path != entry.original_path;
        tracing::info!(
            session = recording.session_id(),
            index = recording.index(),
            moved,
            "an interrupted recording was discarded"
        );
        if let Some(trash_path) = entry.path.as_ref().filter(|_| moved) {
            eprintln!(
                "Discarded {}: moved to the trash at {}, and listed there — restorable until \
                 the trash is emptied or its retention expires it.",
                recording.output().display(),
                trash_path.display()
            );
        } else {
            eprintln!(
                "Closed the record of {}: there was no file to move.",
                recording.session_id()
            );
        }
    }
    Ok(())
}

/// The row indexing just wrote — or already held — for one recording.
///
/// [`None`] only if indexing could not read this particular session's sidecar
/// a moment after `interrupted_recordings` did — a race with something else
/// touching the directory, not the ordinary case.
fn indexed_item(database: &Database, session_id: &str, index: u32) -> Option<TrashItem> {
    database
        .connection()
        .query_row(
            "SELECT recording_id FROM recordings WHERE session_id = ?1 AND session_index = ?2",
            params![session_id, index],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .map(TrashItem::recording)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use crate::test_support::Scratch;
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
        // recording on the machine. Nothing is indexed and the trash is never
        // even resolved: the refusal has to come before anything about where
        // a file might go is asked.
        let directory = TemporaryDirectory::new("bulk-refused");
        let mut database = directory.database();
        let trash = Trash::new(directory.path().join("Trash"));

        let error = discard_all(&[], None, directory.path(), &mut database, &trash)
            .expect_err("a bulk discard must be refused");

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

    /// A directory under the system temporary directory, removed when the test
    /// that made it passes.
    ///
    /// The removal is [`Scratch`]'s rather than this type's own. What it had
    /// was `let _ = fs::remove_dir_all(…)` in [`Drop`]: that takes a failing
    /// test's evidence with it, and it cannot report a removal that did not
    /// happen — the defect PR #597 was written to expose (issue #598).
    struct TemporaryDirectory(Scratch);

    impl TemporaryDirectory {
        fn new(purpose: &str) -> Self {
            Self(Scratch::new(&format!("recover-cli-{purpose}")))
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// A fresh library index, in this same temporary directory.
        fn database(&self) -> Database {
            Database::open(self.0.join("library.db")).expect("a library index can be opened")
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
    fn discarding_indexes_the_recording_and_moves_it_into_the_real_trash() {
        // The acceptance criterion (issue #451), proved end to end rather than
        // by inspecting what `discard_all` calls: the bytes exist somewhere on
        // disk afterwards, the library has a real row for them (listed,
        // restorable, under retention — not the second-class "moved with no
        // row" this started as), and recovering the same directory again does
        // not offer the recording a second time.
        let directory = TemporaryDirectory::new("discard-e2e");
        interrupted_session(directory.path(), "cs2-20260811-143205", 4096);
        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");
        let chosen: Vec<&InterruptedRecording> = found.iter().collect();
        let original = chosen[0].output().to_path_buf();
        let mut database = directory.database();
        let trash_directory = directory.path().join("Trash");
        let trash = Trash::new(&trash_directory);

        discard_all(
            &chosen,
            Some("cs2-20260811-143205"),
            directory.path(),
            &mut database,
            &trash,
        )
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

        // The part that is new: a real row, not a file with nothing pointing
        // at it. `Trash::list` is what the trash screen and `Trash::empty`'s
        // confirmation are built from, so this is the same proof they get.
        let listed = trash.list(&database).expect("the trash can be listed");
        assert_eq!(
            listed.len(),
            1,
            "the discarded recording should have a row in the trash: {listed:?}"
        );
        let listed_path = listed[0]
            .path
            .as_deref()
            .expect("a recording in the trash names its file");
        assert!(
            listed_path.starts_with(&trash_directory),
            "the listed row should point at the file inside the trash: {listed_path:?}"
        );

        assert!(
            interrupted_recordings(directory.path())
                .expect("listed")
                .is_empty(),
            "a discarded recording must not be offered for recovery again"
        );
    }

    #[test]
    fn discarding_a_recording_with_no_footage_closes_the_record_and_still_gets_a_row() {
        // A recorder killed before the encoder wrote a first packet leaves a
        // sidecar entry and no file (issue #283). There is nothing to move,
        // and that must not be treated as a failure — but the row still has
        // to exist, marked deleted, so the entry is not silently dropped from
        // the trash's own bookkeeping.
        let directory = TemporaryDirectory::new("discard-no-footage");
        interrupted_session(directory.path(), "cs2-20260811-143205", 0);
        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");
        std::fs::remove_file(found[0].output()).expect("the file can be removed");
        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");
        let chosen: Vec<&InterruptedRecording> = found.iter().collect();
        let mut database = directory.database();
        let trash = Trash::new(directory.path().join("Trash"));

        discard_all(
            &chosen,
            Some("cs2-20260811-143205"),
            directory.path(),
            &mut database,
            &trash,
        )
        .expect("discarding with no footage is not a failure");

        assert!(
            interrupted_recordings(directory.path())
                .expect("listed")
                .is_empty(),
            "the record must still be closed even with nothing to move"
        );
        let listed = trash.list(&database).expect("the trash can be listed");
        assert_eq!(
            listed.len(),
            1,
            "the entry should still be tracked: {listed:?}"
        );
    }

    #[test]
    fn discarding_after_the_sidecar_rewrite_previously_failed_finishes_without_moving_twice() {
        // Simulates the one partial-failure state `discard_all`'s own
        // documentation names: the file already moved and the row already
        // marked deleted, but the sidecar still says the recording is open —
        // exactly what a failure at step 3 (closing the record) would leave
        // behind. A second `--discard` on the same session has to finish the
        // job rather than trying to move the file a second time or failing
        // again with nothing changed.
        let directory = TemporaryDirectory::new("discard-retry");
        interrupted_session(directory.path(), "cs2-20260811-143205", 4096);
        let mut database = directory.database();
        let trash = Trash::new(directory.path().join("Trash"));
        let now = SystemTime::UNIX_EPOCH + core::time::Duration::from_secs(1_786_459_085);

        // The partial run: index and send, but never close the sidecar — the
        // state a failed `record_discarded` would leave.
        let mut settings = IndexSettings::new([directory.path().to_path_buf()]);
        settings.pace = IndexPace::foreground();
        reconcile(&mut database, &settings, &IndexControl::new(), now).expect("indexed");
        let item = indexed_item(&database, "cs2-20260811-143205", 1).expect("indexing wrote a row");
        let first_send = trash.send(&mut database, item, now).expect("moved once");

        // The sidecar is untouched, so `recover` still offers it — the same
        // "still open" state a failed rewrite leaves.
        let found = interrupted_recordings(directory.path()).expect("listed");
        assert_eq!(
            found.len(),
            1,
            "the partial run should not have closed the sidecar"
        );
        let chosen: Vec<&InterruptedRecording> = found.iter().collect();

        discard_all(
            &chosen,
            Some("cs2-20260811-143205"),
            directory.path(),
            &mut database,
            &trash,
        )
        .expect("a retry should finish the job rather than failing again");

        assert!(
            first_send
                .path
                .as_deref()
                .expect("a recording in the trash names its file")
                .exists(),
            "the file should not have been moved a second time"
        );
        let listed = trash.list(&database).expect("the trash can be listed");
        assert_eq!(listed.len(), 1, "there must still be exactly one entry");
        assert!(
            interrupted_recordings(directory.path())
                .expect("listed")
                .is_empty(),
            "the retry should finally close the sidecar record"
        );
    }
}
