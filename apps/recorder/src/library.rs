//! Answering the desktop application's questions about the recording library.
//!
//! The window cannot read `library.db`. It has no file-system permission for
//! the file and it may not link `clipped-library`
//! ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md),
//! `tests/integration/tests/workspace_layering.rs`), so the process that owns
//! the database answers for it — which is this one
//! ([issue #301](https://github.com/wildware-uk/clipped/issues/301)).
//!
//! Two halves, and they are deliberately separate:
//!
//! - [`LibraryReader`] answers questions. It is the join between two
//!   vocabularies and nothing else — `clipped_library::index` says what a row
//!   is, `clipped_ipc::library` says what goes on the wire, and neither knows
//!   about the other. Keeping the conversion here is what lets the protocol
//!   crate stay a leaf that the window can link (`crates/ipc/src/lib.rs`) and
//!   the library crate stay ignorant of the protocol.
//! - [`LibraryIndexer`] keeps there being anything to answer *with*. Before
//!   [issue #402](https://github.com/wildware-uk/clipped/issues/402) there was
//!   no such half at all: `reconcile` had no caller anywhere in the product, so
//!   every recording anybody made was on disk and in no index.
//!
//! They own a connection each, for the reason [`LibraryIndexer`] gives: a page
//! of the library must not wait behind a reconciliation.
//!
//! # Opening is lazy, and a failure is not permanent
//!
//! The database is opened on the first question and kept afterwards. It is not
//! opened during `serve` startup, for two reasons: a recorder that cannot open
//! its library must still record — that is the thing it exists to do — and the
//! most likely reason it cannot is a drive that is not plugged in yet, which
//! stops being true without restarting anything. So an attempt that fails is
//! reported and *retried* on the next question rather than remembered.
//!
//! # The failure is never an empty library
//!
//! A read that could not happen answers [`ErrorCode::LibraryUnavailable`] with
//! the reason. Answering an empty page would have a window drawing "nothing
//! recorded yet" over a database that is locked, corrupt, from a newer build or
//! on a disconnected drive (AGENTS.md sections 27 and 54).
//!
//! # Threading
//!
//! A [`CommandHandler`](clipped_ipc::CommandHandler) is called from several
//! connection threads at once, and an SQLite connection is `Send` and not
//! `Sync`, so the connection sits behind a mutex and library reads are
//! serialised against each other. They are not serialised against anything
//! else: a page is a bounded read of local data, and nothing on a capture,
//! encoder or recording thread ever waits on this lock.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::SystemTime;

use clipped_ipc::{
    ErrorCode, FavouriteMark, LibraryClip, LibraryEventLane, LibraryEventMark, LibraryEvents,
    LibraryGame, LibraryRecording, LibrarySession, LibrarySessionPage, LibrarySessions, LockMark,
    ProtocolError, RestoreFromTrash, RestoredItem, SetFavourite, SetLock, TrashEmptied,
    TrashListing, TrashedItem, MAX_FRAME_BYTES,
};
use clipped_library::favourites::Favourite;
use clipped_library::index::{
    cursor_of, game_summaries, list_sessions, reconcile, GameSummary, IndexControl, IndexReport,
    IndexSettings, IndexedClip, IndexedRecording, IndexedSession, SessionListing,
};
use clipped_library::locks::Lockable;
use clipped_library::search::Query;
use clipped_library::thumbnail::{ServiceOptions, ThumbnailCache, ThumbnailService};
use clipped_storage::Database;
use clipped_waveform::{ServiceOptions as WaveformOptions, WaveformCache, WaveformService};

/// The file the library index lives in, under Clipped's per-user directory.
///
/// `pub(crate)` rather than private: `crate::recover`'s `--discard` indexes a
/// recovered fragment before it sends it to the trash (issue #451), and it
/// opens the same database this module does. Naming a second `"library.db"`
/// literal there would be the two answers to one question AGENTS.md
/// section 55 warns about.
pub(crate) const LIBRARY_FILE: &str = "library.db";

/// The recording library, as this process reads it for the window.
#[derive(Debug)]
pub struct LibraryReader {
    /// Where the index is, or [`None`] when this machine describes no per-user
    /// directory at all — which on Windows means `%LOCALAPPDATA%` is unset.
    path: Option<PathBuf>,
    database: Mutex<Option<Database>>,
    /// Where deleted media waits, so that a trash listing can say where the
    /// files went rather than only that they went.
    ///
    /// Listing the trash reads the *index* — the directory is not consulted —
    /// so this is here to be reported rather than to be walked.
    trash_directory: Option<PathBuf>,
}

/// The item a request names, or a refusal saying what a kind may be.
fn trash_item(kind: &str, id: i64) -> Result<clipped_library::trash::TrashItem, ProtocolError> {
    match kind {
        "recording" => Ok(clipped_library::trash::TrashItem::recording(id)),
        "clip" => Ok(clipped_library::trash::TrashItem::clip(id)),
        other => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!("`{other}` is not something the trash holds; it holds `recording` and `clip`"),
        )),
    }
}

/// What a `set_favourite` request names, or why it names nothing.
///
/// The target takes two fields because the schema does — a sitting is keyed by
/// text and a recording or clip by an integer — and exactly one of them is read.
/// The half that is not read is *not* checked: a window echoing a whole row back
/// may well fill in both, and refusing that would be refusing a request whose
/// meaning is unambiguous.
///
/// What is refused is a target that names nothing. A `session` with no
/// identifier and a `recording` with an identifier of zero are both requests to
/// favourite something that does not exist, and answering "done" to one would be
/// a window drawing a star it will lose on the next read.
fn favourite_target(request: &SetFavourite) -> Result<Favourite, ProtocolError> {
    let missing = |what: &str| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "a `{}` is favourited by its {what}, and this request carries none",
                request.kind
            ),
        )
    };

    match request.kind.as_str() {
        "session" if request.session_id.is_empty() => Err(missing("session_id")),
        "session" => Ok(Favourite::Session(request.session_id.clone())),
        "recording" | "clip" if request.id == 0 => Err(missing("id")),
        "recording" => Ok(Favourite::Recording(request.id)),
        "clip" => Ok(Favourite::Clip(request.id)),
        other => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "`{other}` is not something that can be favourited; Clipped favourites \
                 `session`, `recording` and `clip`"
            ),
        )),
    }
}

/// What a `set_lock` request names, or why it names nothing.
///
/// The same rules as [`favourite_target`], over a shorter vocabulary: a clip
/// cannot be locked. Asking to lock one is refused rather than ignored, because
/// a window told "done" would draw a padlock against a clip that automatic
/// cleanup does not consult (`clipped_library::locks`).
fn lock_target(request: &SetLock) -> Result<Lockable, ProtocolError> {
    let missing = |what: &str| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "a `{}` is locked by its {what}, and this request carries none",
                request.kind
            ),
        )
    };

    match request.kind.as_str() {
        "session" if request.session_id.is_empty() => Err(missing("session_id")),
        "session" => Ok(Lockable::Session(request.session_id.clone())),
        "recording" if request.id == 0 => Err(missing("id")),
        "recording" => Ok(Lockable::Recording(request.id)),
        "clip" => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            "a clip cannot be locked: automatic cleanup deletes recordings, and a clip already \
             keeps the recording it was cut from. Lock that recording instead",
        )),
        other => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "`{other}` is not something that can be locked; Clipped locks `session` and \
                 `recording`"
            ),
        )),
    }
}

/// Why a restore did not happen.
fn restore_refused(error: clipped_library::trash::TrashError) -> ProtocolError {
    // A named item the trash does not hold is the caller's mistake and a
    // different thing from a library that could not be read, which is a
    // machine's problem. Saying so is what lets a window offer the right next
    // step (AGENTS.md section 15).
    let code = match &error {
        clipped_library::trash::TrashError::NoSuchItem { .. }
        | clipped_library::trash::TrashError::NotInTrash { .. } => ErrorCode::InvalidParameters,
        _ => ErrorCode::LibraryUnavailable,
    };
    ProtocolError::new(code, error.to_string())
}

/// Why the trash was not emptied.
fn empty_refused(error: clipped_library::trash::TrashError) -> ProtocolError {
    let code = match &error {
        // The confirmation did not match what is there. The message names both
        // counts, so a window can say what changed rather than only that
        // something did.
        clipped_library::trash::TrashError::Changed { .. } => ErrorCode::InvalidParameters,
        _ => ErrorCode::LibraryUnavailable,
    };
    ProtocolError::new(code, error.to_string())
}

/// One trash entry, as the window is told about it.
///
/// `original_path` is the one a person recognises: a file inside the trash is
/// named for the trash, and a screen that showed only that would be asking
/// somebody to identify their own recording by a name they have never seen.
fn trashed_item(entry: clipped_library::trash::TrashEntry) -> TrashedItem {
    TrashedItem {
        kind: match entry.item.kind {
            clipped_library::trash::ItemKind::Recording => "recording".to_owned(),
            clipped_library::trash::ItemKind::Clip => "clip".to_owned(),
        },
        id: entry.item.id,
        path: entry.path.to_string_lossy().into_owned(),
        original_path: entry.original_path.to_string_lossy().into_owned(),
        deleted_at: entry.deleted_at,
        // See `LibraryReader::trash`: nothing configures the retention period,
        // so there is no date to give that would not be invented.
        expires_at: None,
        size_bytes: entry.size_bytes,
        dependent_clips: entry.dependent_clips,
    }
}

impl LibraryReader {
    /// A reader for the library at Clipped's usual place:
    /// `%LOCALAPPDATA%\Clipped\library.db`, beside the logs (`docs/storage.md`).
    #[must_use]
    pub fn for_this_user() -> Self {
        let mut reader = Self::at(
            clipped_logging::application_directory().map(|directory| directory.join(LIBRARY_FILE)),
        );
        reader.trash_directory = crate::config::default_output_directory()
            .map(|recordings| clipped_session::config::trash_beside(&recordings));
        reader
    }

    /// A reader for the library at a named path, or for no library at all.
    #[must_use]
    pub fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            database: Mutex::new(None),
            trash_directory: None,
        }
    }

    /// What is waiting in the trash, newest deletion first.
    ///
    /// Reads the index and nothing else: an entry is a row with `deleted_at`
    /// set, so a trash on a drive that is not present still *lists*, and each
    /// item says where its file is. That is the difference between a screen
    /// that says "your deleted recordings are on a drive that is not here" and
    /// one that appears empty
    /// ([issue #450](https://github.com/wildware-uk/clipped/issues/450)).
    ///
    /// `expires_at` is absent on every item, deliberately: nothing configures
    /// the retention period yet, and a date computed from a policy nobody set
    /// would be a screen promising a deletion date it cannot keep.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::LibraryUnavailable`] when the index cannot be read.
    pub fn trash(&self) -> Result<TrashListing, ProtocolError> {
        let directory = self
            .trash_directory
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));

        self.with_database(|database| {
            let entries = clipped_library::trash::Trash::new(&directory)
                .list(database)
                .map_err(|error| {
                    ProtocolError::new(
                        ErrorCode::LibraryUnavailable,
                        format!("the trash could not be read: {error}"),
                    )
                })?;

            let total_bytes = entries
                .iter()
                .map(clipped_library::trash::TrashEntry::bytes)
                .sum();

            Ok(TrashListing {
                total_items: entries.len() as u64,
                total_bytes,
                directory: directory.to_string_lossy().into_owned(),
                items: entries.into_iter().map(trashed_item).collect(),
            })
        })
    }

    /// One page of the library.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] if `request.query` is not a query the
    /// search language accepts, carrying the position and what was expected
    /// there; [`ErrorCode::LibraryUnavailable`] if the index could not be read,
    /// carrying why.
    pub fn sessions(&self, request: &LibrarySessions) -> Result<LibrarySessionPage, ProtocolError> {
        // Before the database is opened: a query that cannot be parsed is the
        // caller's mistake and should be reported as one even on a machine
        // whose library is unreadable.
        let query = request
            .query
            .as_deref()
            .filter(|query| !query.trim().is_empty())
            .map(|query| {
                query.parse::<Query>().map_err(|error| {
                    ProtocolError::new(
                        ErrorCode::InvalidParameters,
                        format!("`library_sessions` was not given a usable query: {error}"),
                    )
                })
            })
            .transpose()?;

        self.with_database(|database| {
            let page = list_sessions(
                database,
                &SessionListing {
                    limit: request
                        .limit
                        .map_or(0, |limit| usize::try_from(limit).unwrap_or(usize::MAX)),
                    after: request.after.clone(),
                    query: query.as_ref(),
                },
            )
            .map_err(unavailable)?;

            Ok(within_one_frame(&page.sessions, page.next))
        })
    }

    /// What the library holds per game.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::LibraryUnavailable`] if the index could not be read,
    /// carrying why.
    pub fn games(&self) -> Result<Vec<LibraryGame>, ProtocolError> {
        self.with_database(|database| {
            let summaries = game_summaries(database).map_err(unavailable)?;
            Ok(summaries.iter().map(game).collect())
        })
    }

    /// The game events of one recording, placed in that recording's file.
    ///
    /// The offset is a subtraction the index can do because both halves are
    /// columns: an event's moment on the session's timeline, and where that
    /// recording starts on the same timeline (`starts_at_nanos`, migration
    /// `0005`). Nothing opens a file, and nothing re-reads a sidecar.
    ///
    /// A recording with no events answers with an empty lane rather than an
    /// error. "None" and "the question was never asked" are different things to
    /// draw, and the reply arriving at all is what tells them apart
    /// (AGENTS.md section 27).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] if the recording is not named by an
    /// identifier the library uses, and [`ErrorCode::LibraryUnavailable`] if
    /// the index could not be read.
    pub fn events(&self, request: &LibraryEvents) -> Result<LibraryEventLane, ProtocolError> {
        // Before the database is opened, for the reason `sessions` parses its
        // query first: a malformed identifier is the caller's mistake and is
        // worth saying so even on a machine whose library cannot be read.
        let recording: i64 = request.recording.parse().map_err(|_| {
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                format!(
                    "`library_events` was given `{}`, which is not a recording identifier this                      library uses",
                    request.recording
                ),
            )
        })?;

        self.with_database(|database| {
            let mut statement = database
                .connection()
                .prepare(
                    "SELECT game_events.at_nanos - recordings.starts_at_nanos, \n                            game_events.kind, game_events.source \n                     FROM game_events \n                     JOIN recordings \n                         ON recordings.recording_id = game_events.recording_id \n                     WHERE game_events.recording_id = ?1 \n                       AND recordings.starts_at_nanos IS NOT NULL \n                     ORDER BY game_events.at_nanos",
                )
                .map_err(unreadable)?;

            let marks = statement
                .query_map([recording], |row| {
                    Ok(LibraryEventMark {
                        recording: request.recording.clone(),
                        at: row.get(0)?,
                        kind: row.get(1)?,
                        source: row.get(2)?,
                    })
                })
                .map_err(unreadable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(unreadable)?;

            Ok(LibraryEventLane { marks })
        })
    }

    /// Runs a read against the open database, opening it if this is the first
    /// question or if the last attempt failed.
    ///
    /// The lock is held for the length of the read, which is what serialises the
    /// connection between connection threads. A read is a bounded query over
    /// local data; nothing that must not stall waits on this.
    fn with_database<T>(
        &self,
        read: impl FnOnce(&Database) -> Result<T, ProtocolError>,
    ) -> Result<T, ProtocolError> {
        let mut held = self.database.lock().map_err(poisoned)?;

        if held.is_none() {
            let path = self.path.as_ref().ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    "this machine describes no per-user application directory, so Clipped has \
                     nowhere to keep its library index",
                )
            })?;

            // `Database::open` creates the file and runs the migrations. This
            // process owns the writing connection (`docs/storage.md`), so it is
            // the one that may do that; the window never opens the file at all.
            //
            // A failure leaves this `None`, so the next question tries again —
            // the usual cause is a drive that is not plugged in, and that stops
            // being true without restarting anything.
            let opened = Database::open(path).map_err(|error| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    format!("the recording library could not be opened: {error}"),
                )
            })?;
            *held = Some(opened);
        }

        read(held.as_ref().expect("the database was just opened"))
    }

    /// Puts one thing back where it was.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] for a kind this build does not know or
    /// an item the trash does not hold, and [`ErrorCode::LibraryUnavailable`]
    /// when the index or the file system refuses.
    pub fn restore(&self, request: &RestoreFromTrash) -> Result<RestoredItem, ProtocolError> {
        let item = trash_item(&request.kind, request.id)?;
        let directory = self.trash_path();

        self.with_database_mut(|database| {
            let outcome = clipped_library::trash::Trash::new(&directory)
                .restore(database, item)
                .map_err(restore_refused)?;

            Ok(RestoredItem {
                kind: request.kind.clone(),
                id: request.id,
                path: outcome.path.to_string_lossy().into_owned(),
                file_restored: outcome.file_restored,
                renamed: outcome.path != outcome.original_path,
            })
        })
    }

    /// Destroys everything in the trash, if it is still what the caller was
    /// shown.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] when the trash is not what was
    /// confirmed — naming both counts, so a window can say what changed and
    /// show the new listing — and [`ErrorCode::LibraryUnavailable`] when it
    /// could not be read.
    pub fn empty(&self, request: &clipped_ipc::EmptyTrash) -> Result<TrashEmptied, ProtocolError> {
        let directory = self.trash_path();
        let confirmation = clipped_library::trash::EmptyTrash::confirmed(
            usize::try_from(request.items).unwrap_or(usize::MAX),
            request.bytes,
        );

        self.with_database_mut(|database| {
            let report = clipped_library::trash::Trash::new(&directory)
                .empty(database, confirmation)
                .map_err(empty_refused)?;

            Ok(TrashEmptied {
                removed: report.removed.len() as u64,
                reclaimed_bytes: report.bytes_reclaimed(),
                // Each failure names the item it is about as well as the
                // reason, because "one file would not go" without saying which
                // is a sentence nobody can act on (AGENTS.md section 15).
                refused: report
                    .failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.item, failure.error))
                    .collect(),
            })
        })
    }

    /// Marks one thing a favourite, or clears the mark.
    ///
    /// [Issue #58](https://github.com/wildware-uk/clipped/issues/58). `at` is
    /// when, and it is passed in rather than read here for the reason
    /// `clipped_library::favourites` takes one: every write in that crate does,
    /// so a test does not have to wait for a clock.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] for a kind this build does not know or
    /// a target that names nothing, and [`ErrorCode::LibraryUnavailable`] when
    /// the index refuses.
    pub fn set_favourite(
        &self,
        request: &SetFavourite,
        at: SystemTime,
    ) -> Result<FavouriteMark, ProtocolError> {
        let what = favourite_target(request)?;

        self.with_database_mut(|database| {
            let changed = if request.favourite {
                clipped_library::favourites::mark(database, &what, at)
            } else {
                clipped_library::favourites::unmark(database, &what)
            }
            .map_err(unreadable)?;

            // Read back rather than assume. The two disagree for a target that
            // is not there: nothing was written, and a window told "favourited"
            // would draw a full star against a row that has none.
            let favourite =
                clipped_library::favourites::is_marked(database, &what).map_err(unreadable)?;

            Ok(FavouriteMark {
                kind: request.kind.clone(),
                session_id: request.session_id.clone(),
                id: request.id,
                favourite,
                changed,
            })
        })
    }

    /// Locks one thing against automatic cleanup, or unlocks it.
    ///
    /// [Issue #472](https://github.com/wildware-uk/clipped/issues/472).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] for a kind that cannot be locked or a
    /// target that names nothing, and [`ErrorCode::LibraryUnavailable`] when the
    /// index refuses.
    pub fn set_lock(&self, request: &SetLock, at: SystemTime) -> Result<LockMark, ProtocolError> {
        let what = lock_target(request)?;

        self.with_database_mut(|database| {
            let changed = if request.locked {
                clipped_library::locks::lock(database, &what, at)
            } else {
                clipped_library::locks::unlock(database, &what)
            }
            .map_err(unreadable)?;

            // Read back, for the reason `set_favourite` does: a target that is
            // not there is written to by nothing.
            let locked = clipped_library::locks::is_locked(database, &what).map_err(unreadable)?;

            // And separately, whether the sweep will leave it alone — which is
            // a different question for a recording inside a locked sitting, and
            // is the one a padlock on a screen is drawn from.
            let protected = match &what {
                Lockable::Session(_) => locked,
                Lockable::Recording(id) => {
                    clipped_library::locks::protects(database, *id).map_err(unreadable)?
                }
            };

            Ok(LockMark {
                kind: request.kind.clone(),
                session_id: request.session_id.clone(),
                id: request.id,
                locked,
                protected,
                changed,
            })
        })
    }

    /// Where deleted media waits, as this reader was told.
    fn trash_path(&self) -> PathBuf {
        self.trash_directory
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// The same, for the two commands that change the trash.
    ///
    /// Restoring and emptying write, and the trash module's every statement is
    /// its own transaction (`clipped_library::trash`), so this holds the same
    /// connection the reads use rather than opening a second one. The indexer
    /// has a connection of its own and may be reconciling at the same moment;
    /// the database is in write-ahead logging mode, which is what makes that
    /// safe (`docs/storage.md`).
    fn with_database_mut<T>(
        &self,
        write: impl FnOnce(&mut Database) -> Result<T, ProtocolError>,
    ) -> Result<T, ProtocolError> {
        let mut held = self.database.lock().map_err(poisoned)?;

        if held.is_none() {
            let path = self.path.as_ref().ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    "this machine describes no per-user application directory, so Clipped has                      nowhere to keep its library index",
                )
            })?;
            let opened = Database::open(path).map_err(|error| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    format!("the recording library could not be opened: {error}"),
                )
            })?;
            *held = Some(opened);
        }

        write(held.as_mut().expect("the database was just opened"))
    }
}

/// A lock a panic elsewhere left poisoned.
///
/// Unlike the recorder's status — which is read through a poisoned lock so that
/// a panic cannot turn "recording" into "unknown" — a poisoned database
/// connection may be mid-statement, so this refuses rather than reading through
/// it. The refusal says so, which is more use than a second panic.
fn poisoned<T>(_: std::sync::PoisonError<T>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::LibraryUnavailable,
        "the recording library was left in an unknown state by an earlier failure; restart \
         Clipped's recorder to read it again",
    )
}

/// A read the index refused.
fn unreadable(error: clipped_storage::rusqlite::Error) -> ProtocolError {
    // The same code a failed index read answers with, because it is the same
    // thing from the caller's side: the library is there and could not be
    // asked. A window shows one message for both and retries the same way.
    ProtocolError::new(
        ErrorCode::LibraryUnavailable,
        format!("the recording library could not be read: {error}"),
    )
}

fn unavailable(error: clipped_library::index::IndexError) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::LibraryUnavailable,
        format!("the recording library could not be read: {error}"),
    )
}

/// How many bytes of one frame a page of sessions may fill.
///
/// [`clipped_ipc::MAX_FRAME_BYTES`] is a bound on damage rather than a budget to
/// spend, and a frame over it is **not** a failed request: the reader closes the
/// connection rather than resynchronising (`crates/ipc/src/frame.rs`). So a page
/// that overran it would drop the window's control connection, and would do it
/// again on every retry, with nothing on screen to explain why.
///
/// Half a frame, because this measures the sessions alone and the response
/// envelope, the reply tag and the cursor are carried around them.
///
/// A count of sessions cannot be this bound, which is the thing worth knowing
/// here: a session holds any number of recordings and clips, so two hundred of
/// them — `clipped_library::index::MAX_PAGE_LIMIT` — is 135 KB with one
/// recording each and over 3 MB with thirty. Measured, not guessed:
/// `a_page_is_bounded_by_what_a_frame_can_carry_rather_than_by_a_count_of_sessions`
/// builds a real index of that shape and reads it back through
/// [`LibraryReader::sessions`].
const PAGE_BUDGET_BYTES: usize = MAX_FRAME_BYTES as usize / 2;

/// As many of these sittings as one frame can carry, and where to continue.
///
/// `beyond` is the cursor the index offered for the page it answered. It is kept
/// when every session fits; when the page is cut short, the cursor becomes that
/// of the last session actually carried, so the next page starts with the first
/// one left out rather than skipping it.
///
/// **At least one session is always carried, whatever it costs.** A page that
/// could come back empty because its first session was too large would page for
/// ever and never show it, which is worse than one oversized frame — and the
/// frame reader's ceiling is an order of magnitude above any single sitting.
fn within_one_frame(sessions: &[IndexedSession], beyond: Option<String>) -> LibrarySessionPage {
    let mut carried = Vec::with_capacity(sessions.len());
    let mut remaining = PAGE_BUDGET_BYTES;

    for indexed in sessions {
        let wire = session(indexed);
        // A session that cannot be measured is treated as the whole budget
        // rather than as free: the alternative is a page that grows without
        // bound because nothing could size it.
        let cost = serde_json::to_vec(&wire).map_or(PAGE_BUDGET_BYTES, |bytes| bytes.len());

        if !carried.is_empty() && cost > remaining {
            // `carried` is non-empty, so it has a last session, and it is the
            // one the cursor has to name.
            let last = &sessions[carried.len() - 1];
            return LibrarySessionPage {
                sessions: carried,
                next_cursor: Some(cursor_of(last)),
            };
        }

        remaining = remaining.saturating_sub(cost);
        carried.push(wire);
    }

    LibrarySessionPage {
        sessions: carried,
        next_cursor: beyond,
    }
}

/// One sitting, on the wire.
fn session(session: &IndexedSession) -> LibrarySession {
    LibrarySession {
        session_id: session.session_id.clone(),
        game_id: session.game_id.clone(),
        game_name: session.game_name.clone(),
        started_at: session.started_at.clone(),
        ended_at: session.ended_at.clone(),
        end_reason: session.end_reason.clone(),
        favourite: session.favourite,
        locked: session.locked,
        // The sitting's lock is passed down rather than looked up again: it is
        // the cascade, and working it out per recording would be a second
        // expression of a rule `clipped_library::locks` already owns.
        recordings: session
            .recordings
            .iter()
            .map(|held| recording(held, session.locked))
            .collect(),
        clips: session.clips.iter().map(clip).collect(),
    }
}

/// One recording, on the wire.
///
/// `locked_session` is whether the sitting it belongs to is locked, which is
/// what makes a recording with no lock of its own protected.
fn recording(recording: &IndexedRecording, locked_session: bool) -> LibraryRecording {
    LibraryRecording {
        recording_id: recording.recording_id,
        session_index: u32::try_from(recording.session_index).unwrap_or(u32::MAX),
        path: recording.path.clone(),
        started_at: recording.started_at.clone(),
        ended_at: recording.ended_at.clone(),
        outcome: recording.outcome.clone(),
        end_reason: recording.end_reason.clone(),
        duration_seconds: recording.duration_seconds,
        width: recording.width.and_then(|width| u32::try_from(width).ok()),
        height: recording
            .height
            .and_then(|height| u32::try_from(height).ok()),
        size_bytes: recording.size_bytes.map(count),
        // The field the whole read exists for. A screen that cannot see this
        // draws a broken tile instead of saying the file has gone (AGENTS.md
        // section 27).
        missing_since: recording.missing_since.clone(),
        favourite: recording.favourite,
        locked: recording.locked,
        // The cascade, expressed once and here rather than in every window:
        // a recording inside a locked sitting is protected without carrying
        // a lock of its own (`clipped_library::locks`).
        protected: recording.locked || locked_session,
        tags: recording.tags.clone(),
    }
}

/// One clip, on the wire.
fn clip(clip: &IndexedClip) -> LibraryClip {
    LibraryClip {
        clip_id: clip.clip_id,
        path: clip.path.clone(),
        title: clip.title.clone(),
        created_at: clip.created_at.clone(),
        duration_seconds: clip.duration_seconds,
        size_bytes: clip.size_bytes.map(count),
        missing_since: clip.missing_since.clone(),
        favourite: clip.favourite,
        tags: clip.tags.clone(),
    }
}

/// What the library holds for one game, on the wire.
fn game(summary: &GameSummary) -> LibraryGame {
    LibraryGame {
        game_id: summary.game_id.clone(),
        name: summary.name.clone(),
        first_seen_at: summary.first_seen_at.clone(),
        last_played_at: summary.last_played_at.clone(),
        sessions: summary.sessions,
        recordings: summary.recordings,
        clips: summary.clips,
        favourites: summary.favourites,
        bytes: summary.bytes,
        missing: summary.missing,
    }
}

/// A count the schema stores as a signed integer, as an unsigned one.
///
/// The columns carry `CHECK (… >= 0)`, so a negative value means somebody wrote
/// to the database with something other than Clipped. Zero is the honest answer
/// to "how large is this file" in that case; a wrapped enormous number is not.
fn count(value: i64) -> u64 {
    value.max(0).unsigned_abs()
}

/// Keeps the index in step with what is on disk.
///
/// The counterpart to [`LibraryReader`], and the answer to the second half of
/// [issue #402](https://github.com/wildware-uk/clipped/issues/402):
/// `clipped_library::index::reconcile` was a well-tested subsystem with nothing
/// in the product calling it, so a recording that had just been made never
/// reached the library at all. This is the caller.
///
/// # When it runs, and why those moments
///
/// **At start-up**, once, after the endpoint is listening and the ready line has
/// been printed — so it never delays a window being able to connect. This is
/// what picks up everything produced while nothing was indexing: sessions
/// `watch` recorded in a process of its own, recordings copied onto the machine,
/// a library file the user deleted. It is cheap when there is nothing new,
/// because reconciliation is an upsert against what is already there.
///
/// **After a recording finishes**, from `serve`'s recording state. That is the
/// moment the recording's session record is final, and it is what makes the
/// window's Library screen show a recording made from the window *in the same
/// session, without a restart* — the acceptance criterion the ticket is about.
///
/// Deliberately **not** on a command from the window. Nothing in the protocol
/// asks for a rescan, adding one would be a protocol change, and the two moments
/// above already make the library correct for everything this build can produce.
/// A rescan a user can ask for belongs with recovering sittings the library has
/// no record of ([issue #272](https://github.com/wildware-uk/clipped/issues/272)).
///
/// # Where it runs
///
/// On a thread of its own, and never on the recording thread, a capture thread
/// or a connection thread (AGENTS.md section 20). `reconcile` walks directories
/// and opens transactions; a recording thread that did that between finishing
/// its file and reporting its outcome would hold up the reply to
/// `stop_recording` for as long as the walk took.
///
/// Requests **coalesce**: asking while a run is in flight schedules exactly one
/// more, so a burst of recordings cannot queue a run each. The run itself is
/// paced by `IndexPace::background`, which assumes a game may be recording.
///
/// # Its own connection
///
/// The indexer opens the database itself rather than borrowing
/// [`LibraryReader`]'s. A reconciliation holds its connection for the length of
/// the run, and a library screen that had to wait behind it would be the stall
/// this design exists to avoid. SQLite in write-ahead logging mode is built for
/// exactly this: readers are never blocked by a writer, and both connections are
/// in this process, which is the one that owns the writing one
/// (`docs/storage.md`).
///
/// # What happens to a recording no session claims
///
/// Nothing, and it is reported. `reconcile` never invents a row for a media file
/// no sidecar names, and never deletes or moves one — the files stay exactly
/// where they are, and the run says how many it found and names the first few.
/// That is the state a user upgrading from a build whose `serve` wrote no
/// session record is in: their recordings are on disk, playable, and reported at
/// every run until [issue #272](https://github.com/wildware-uk/clipped/issues/272)
/// offers to recover them.
#[derive(Debug)]
pub struct LibraryIndexer {
    shared: Arc<Indexer>,
    /// Joined by [`Self::shut_down`], so that a run in progress is given the
    /// chance to stop before the process exits.
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// What the indexer thread and everything that pokes it share.
#[derive(Debug)]
struct Indexer {
    /// Where the index is, or [`None`] when this machine describes no per-user
    /// directory at all.
    path: Option<PathBuf>,
    settings: IndexSettings,
    /// The recording folders, kept because the storage sweep needs one and
    /// `IndexSettings` does not hand them back.
    roots: Vec<PathBuf>,
    /// What the library is allowed to occupy, as the user configured it.
    ///
    /// Handed in rather than read here, and empty unless a caller says
    /// otherwise, so that a test's answer does not depend on the settings file
    /// of whoever is running it (AGENTS.md section 25).
    storage: clipped_session::config::StorageSettings,
    control: IndexControl,
    state: Mutex<IndexerState>,
    woken: Condvar,
    /// Makes the pictures the library screens draw, or [`None`] on a machine
    /// with nowhere to keep them.
    ///
    /// Started with the indexer and asked for a picture per recording after
    /// every successful reconciliation. Before this, `ThumbnailService` was
    /// referenced by nothing but its own tests: the service, the cache and the
    /// renderer were all finished and unreachable, so a shipped build made no
    /// thumbnails at all
    /// ([issue #57](https://github.com/wildware-uk/clipped/issues/57)).
    thumbnails: Option<ThumbnailService>,
    /// Makes the peaks a timeline draws, on the same terms.
    ///
    /// `clipped-waveform` was not a dependency of anything before this: it
    /// appeared in the workspace root manifest and in comments and in nobody's
    /// `[dependencies]`, so no waveform was ever generated
    /// ([issue #66](https://github.com/wildware-uk/clipped/issues/66)).
    waveforms: Option<WaveformService>,
}

/// The indexer's own state, which is all that a request touches.
#[derive(Debug, Default)]
struct IndexerState {
    /// A run has been asked for and has not started yet.
    requested: bool,
    /// A run is in flight.
    running: bool,
    /// No further run will be started.
    stopping: bool,
    /// Runs that have finished, for a test to wait on.
    completed: u64,
}

impl LibraryIndexer {
    /// An indexer for this user's library and this user's recordings folder.
    ///
    /// The recordings folder is the one `record` and `watch` write into by
    /// default (`crate::config::default_output_directory`). A recording written
    /// somewhere else — an `output` the request named — has its session record
    /// beside it and is *not* under this root, so it is not indexed; that is the
    /// same answer `watch --output-directory` gets, and which folders make up a
    /// library is issue #272's question rather than this one's.
    #[must_use]
    pub fn for_this_user() -> Self {
        let mut indexer = Self::at(
            clipped_logging::application_directory().map(|directory| directory.join(LIBRARY_FILE)),
            crate::config::default_output_directory()
                .into_iter()
                .collect(),
        );
        // Only here, and deliberately not in `at`: a test must not write
        // pictures into the cache of whoever is running it (AGENTS.md section
        // 25), and `at` is what every test uses.
        if let Some(shared) = Arc::get_mut(&mut indexer.shared) {
            shared.thumbnails = ThumbnailCache::in_default_directory()
                .map(|cache| ThumbnailService::start(cache, ServiceOptions::new()));
            shared.waveforms = WaveformCache::in_default_directory()
                .map(|cache| WaveformService::start(cache, WaveformOptions::new()));
        }
        indexer
    }

    /// An indexer for a named library and named recording folders.
    ///
    /// For tests, which must not read or write the library of whoever is
    /// running them (AGENTS.md section 25).
    #[must_use]
    pub fn at(path: Option<PathBuf>, roots: Vec<PathBuf>) -> Self {
        Self {
            shared: Arc::new(Indexer {
                path,
                settings: IndexSettings::new(roots.clone()),
                roots,
                storage: clipped_session::config::StorageSettings::none(),
                control: IndexControl::new(),
                state: Mutex::new(IndexerState::default()),
                woken: Condvar::new(),
                thumbnails: None,
                waveforms: None,
            }),
            thread: Mutex::new(None),
        }
    }

    /// The same indexer, enforcing these storage limits after each run.
    ///
    /// Without it nothing is ever deleted, which is what Clipped ships with:
    /// [`StorageSettings::none`](clipped_session::config::StorageSettings::none)
    /// is unlimited and `sweep` stops before it reads a directory
    /// ([issue #111](https://github.com/wildware-uk/clipped/issues/111)).
    #[must_use]
    pub fn with_storage(mut self, storage: clipped_session::config::StorageSettings) -> Self {
        if let Some(shared) = Arc::get_mut(&mut self.shared) {
            shared.storage = storage;
        }
        self
    }

    /// Starts the thread, and asks it for the run that catches up on everything
    /// produced while nothing was indexing.
    ///
    /// Called once, by `serve`. Calling it again is a no-op rather than a second
    /// thread.
    pub fn start(&self) {
        let mut thread = match self.thread.lock() {
            Ok(thread) => thread,
            Err(poisoned) => poisoned.into_inner(),
        };
        if thread.is_some() {
            return;
        }

        let shared = Arc::clone(&self.shared);
        *thread = Some(
            std::thread::Builder::new()
                .name("clipped-library-index".to_owned())
                .spawn(move || shared.run())
                .expect("a thread can be started to index on"),
        );
        drop(thread);
        self.request();
    }

    /// Asks for the index to be brought up to date.
    ///
    /// Returns immediately. A request made while a run is in flight schedules
    /// one more run rather than a run per request, so a caller may ask as often
    /// as it likes.
    pub fn request(&self) {
        let Ok(mut state) = self.shared.state.lock() else {
            tracing::error!(
                "the library indexer was left in an unknown state by an earlier failure, so the \
                 index was not brought up to date"
            );
            return;
        };
        if state.stopping {
            return;
        }
        state.requested = true;
        drop(state);
        self.shared.woken.notify_all();
    }

    /// Stops indexing and waits for a run in progress to give up.
    ///
    /// Cancellation is cooperative and is checked between files and between
    /// transactions, so what had been written stays written and the next run
    /// carries on from there (`clipped_library::index`).
    pub fn shut_down(&self) {
        self.shared.control.cancel();
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopping = true;
            state.requested = false;
        }
        self.shared.woken.notify_all();

        let taken = match self.thread.lock() {
            Ok(mut thread) => thread.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(thread) = taken {
            let _ = thread.join();
        }
    }

    /// Waits until nothing is queued and nothing is running, for a test that has
    /// to look at what a run wrote.
    ///
    /// Answers whether it settled within `patience`. Nothing in the product
    /// waits for the index: a run is background work by construction, and a
    /// caller that blocked on one would be the stall this whole design avoids.
    #[cfg(test)]
    pub(crate) fn settled_within(&self, patience: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + patience;
        let mut state = self
            .shared
            .state
            .lock()
            .expect("a lock that is not held long");
        loop {
            if !state.requested && !state.running {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return false;
            };
            let (held, _) = self
                .shared
                .woken
                .wait_timeout(state, left)
                .expect("a lock that is not held long");
            state = held;
        }
    }

    /// How many runs have finished.
    #[cfg(test)]
    pub(crate) fn runs(&self) -> u64 {
        self.shared
            .state
            .lock()
            .expect("a lock that is not held long")
            .completed
    }
}

impl Indexer {
    /// The indexer thread: wait to be asked, reconcile, repeat.
    fn run(self: Arc<Self>) {
        let mut database = None;

        loop {
            {
                let Ok(mut state) = self.state.lock() else {
                    tracing::error!("the library indexer stopped because its state was poisoned");
                    return;
                };
                while !state.requested && !state.stopping {
                    let Ok(held) = self.woken.wait(state) else {
                        tracing::error!(
                            "the library indexer stopped because its state was poisoned"
                        );
                        return;
                    };
                    state = held;
                }
                if state.stopping {
                    return;
                }
                state.requested = false;
                state.running = true;
            }

            self.reconcile_once(&mut database);

            if let Ok(mut state) = self.state.lock() {
                state.running = false;
                state.completed += 1;
            }
            self.woken.notify_all();
        }
    }

    /// One reconciliation, opening the database if this is the first run or if
    /// the last attempt failed.
    ///
    /// A failure to open is reported and *retried* on the next run, for the
    /// reason [`LibraryReader`] retries: the usual cause is a drive that is not
    /// plugged in, and that stops being true without restarting anything.
    fn reconcile_once(&self, database: &mut Option<Database>) {
        let Some(path) = self.path.as_ref() else {
            tracing::warn!(
                "this machine describes no per-user application directory, so there is nowhere \
                 to keep a library index and nothing was indexed"
            );
            return;
        };
        if self.settings.roots.is_empty() {
            tracing::warn!("no recording folder could be worked out, so there is nothing to index");
            return;
        }

        if database.is_none() {
            match Database::open(path) {
                Ok(opened) => *database = Some(opened),
                Err(error) => {
                    tracing::warn!(
                        library = %clipped_logging::RedactedPath::new(path),
                        %error,
                        "the recording library could not be opened, so the index was not brought \
                         up to date; the next recording will try again"
                    );
                    return;
                }
            }
        }

        let open = database.as_mut().expect("the database was just opened");
        match reconcile(open, &self.settings, &self.control, SystemTime::now()) {
            Ok(report) => {
                report_unindexed(&report);
                self.picture_what_was_indexed(open);
                self.enforce_storage_limits(open);
            }
            Err(error) => {
                // The connection is dropped rather than kept: a database that
                // refused may have gone with its drive, and the next run should
                // open it again rather than reuse a handle to something that is
                // no longer there.
                *database = None;
                tracing::warn!(
                    %error,
                    "the library index could not be brought up to date; the next recording will \
                     try again"
                );
            }
        }
    }
}

impl Indexer {
    /// Enforces the configured storage limits, if there are any.
    ///
    /// After the reconciliation and on the same thread, which is the whole
    /// reason it is here rather than at the end of a recording: the sweep walks
    /// directories, and a recording that had to wait for one before it could
    /// report itself finished would be a recording that looks like it hung. The
    /// indexer already runs off the recording path and is already asked for a
    /// run when a recording ends.
    ///
    /// Nothing here fails a reconciliation. A library that could not be swept is
    /// a library that is over its limit, which is a smaller problem than an
    /// index that did not update — and every reason it could not is logged by
    /// `sweep` against the thing that caused it (AGENTS.md section 17).
    fn enforce_storage_limits(&self, database: &mut Database) {
        // The first root is where this recorder writes. A library spread over
        // several folders is issue #272's question, and so is which of them a
        // sweep should measure; this measures the one Clipped records into,
        // which is the one that grows.
        let Some(recordings) = self.roots.first() else {
            return;
        };

        let mut configuration = clipped_session::config::Configuration::defaults();
        configuration.set_storage(self.storage.clone());

        let report = clipped_session::cleanup::sweep(
            &configuration,
            recordings,
            database,
            SystemTime::now(),
        );

        match &report.skipped {
            // The ordinary case on a machine nobody has set a limit on, and it
            // is `debug` rather than `info` because it happens after every run
            // and says that nothing happened.
            Some(clipped_session::cleanup::Skipped::NoLimit) => {}
            Some(reason) => tracing::warn!(
                ?reason,
                "the configured storage limits could not be enforced this run"
            ),
            None if report.deleted > 0 => tracing::info!(
                deleted = report.deleted,
                reclaimed_bytes = report.reclaimed_bytes,
                refused = report.refused,
                "automatic cleanup moved recordings to the trash to stay inside the storage limits"
            ),
            None => {}
        }
    }

    /// Asks for a picture and a waveform of every recording the index holds.
    ///
    /// After the reconciliation rather than during it, and by
    /// [`ThumbnailService::request`] rather than
    /// [`ThumbnailService::thumbnail`], because a scan has thousands of files
    /// and wants none of the pictures yet — the service reads its own cache and
    /// does the work on a background thread at the lowest priority it can get
    /// (`clipped_library::thumbnail`).
    ///
    /// Nothing here waits, and nothing here fails a reconciliation. A recording
    /// with no picture is a tile with no picture, which is a smaller problem
    /// than an index that did not update (AGENTS.md section 17).
    fn picture_what_was_indexed(&self, database: &Database) {
        if self.thumbnails.is_none() && self.waveforms.is_none() {
            return;
        }
        let recordings = match clipped_library::thumbnail::recordings_worth_reading(database) {
            Ok(recordings) => recordings,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the library index could not be asked which recordings need a picture, so \
                     none were made this time"
                );
                return;
            }
        };

        let asked = recordings.len();
        for recording in recordings {
            if let Some(service) = self.thumbnails.as_ref() {
                let _ = service.request(&recording);
            }
            if let Some(service) = self.waveforms.as_ref() {
                let _ = service.request(&recording);
            }
        }
        if asked > 0 {
            tracing::debug!(
                recordings = asked,
                "asked for a picture and a waveform of every recording the index holds"
            );
        }
    }
}

/// Says what happened to media files no session claims.
///
/// `reconcile` logs everything else about a run. This is said here because it is
/// the one part of the report that is about *the user's files* rather than about
/// the index: recordings made by a build whose `serve` wrote no session record
/// are exactly this case, and they are left alone rather than adopted, deleted or
/// renamed (AGENTS.md section 56). Recovering them is
/// [issue #272](https://github.com/wildware-uk/clipped/issues/272).
fn report_unindexed(report: &IndexReport) {
    if report.unindexed_media == 0 {
        return;
    }
    tracing::info!(
        files = report.unindexed_media,
        first = ?report
            .unindexed_sample
            .iter()
            .map(|path| clipped_logging::RedactedPath::new(path).to_string())
            .collect::<Vec<String>>(),
        "recordings under the recording folders belong to no session record, so the library \
         cannot say what they are; they have been left exactly where they are (issue #272)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipped_storage::rusqlite::params;

    /// An empty directory of this test's own.
    fn scratch_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("clipped-recorder-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a scratch directory can be created");
        directory
    }

    /// A reader over a library of one sitting, whose file has gone.
    fn library_with_a_missing_recording(name: &str) -> LibraryReader {
        let path = scratch_directory(name).join("library.db");
        {
            let database = Database::open(&path).expect("a database opens");
            let connection = database.connection();
            connection
                .execute(
                    "INSERT INTO games (game_id, name, first_seen_at) \
                     VALUES ('cs2', 'Counter-Strike 2', '2026-08-11T20:14:00+01:00')",
                    [],
                )
                .expect("a game inserts");
            connection
                .execute(
                    "INSERT INTO sessions (session_id, game_id, started_at) \
                     VALUES ('cs2-20260811-201400', 'cs2', '2026-08-11T20:14:00+01:00')",
                    [],
                )
                .expect("a session inserts");
            connection
                .execute(
                    "INSERT INTO recordings \
                        (session_id, session_index, path, started_at, duration_seconds, \
                         size_bytes, missing_since) \
                     VALUES ('cs2-20260811-201400', 1, ?1, '2026-08-11T20:14:00+01:00', 6540.0, \
                             9812009112, '2026-08-12T09:00:00+01:00')",
                    params![r"D:\clips\cs2-20260811-201400-1.mkv"],
                )
                .expect("a recording inserts");
        }
        LibraryReader::at(Some(path))
    }

    #[test]
    fn a_recording_whose_file_has_gone_reaches_the_window_saying_so() {
        // Issue #305's second acceptance criterion, at the boundary rather than
        // in the index: the row has to be *on the wire*, carrying
        // `missing_since`, or the screen cannot say the file has gone.
        let library = library_with_a_missing_recording("library-missing");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");

        assert_eq!(page.sessions.len(), 1);
        let recording = &page.sessions[0].recordings[0];
        assert_eq!(
            recording.missing_since.as_deref(),
            Some("2026-08-12T09:00:00+01:00"),
            "a missing recording must be reported as missing rather than omitted"
        );
        assert_eq!(recording.size_bytes, Some(9_812_009_112));
        assert_eq!(
            page.sessions[0].game_name.as_deref(),
            Some("Counter-Strike 2")
        );
    }

    #[test]
    fn the_per_game_figures_cross_the_boundary_including_what_is_missing() {
        let library = library_with_a_missing_recording("library-games");

        let games = library.games().expect("the library reads");

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_id.as_deref(), Some("cs2"));
        assert_eq!(games[0].sessions, 1);
        assert_eq!(games[0].recordings, 1);
        assert_eq!(games[0].missing, 1);
        assert_eq!(
            games[0].bytes, 0,
            "a file nobody can find is not occupying the space it used to"
        );
    }

    #[test]
    fn an_empty_library_answers_an_empty_page_rather_than_a_refusal() {
        // The distinction issue #305's last acceptance criterion turns on. This
        // is the empty half; `a_library_that_cannot_be_opened_says_so` is the
        // other, and they must not be the same reply.
        let library =
            LibraryReader::at(Some(scratch_directory("library-empty").join("library.db")));

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("an empty library is not a failure");

        assert!(page.sessions.is_empty());
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn a_library_that_cannot_be_opened_says_so_rather_than_looking_empty() {
        // A file that is not a Clipped database stands in for every reason the
        // index cannot be read — a drive that is not plugged in, a database
        // from a newer build, a corrupt file. What matters is that none of them
        // reaches the window as "you have not recorded anything".
        let path = scratch_directory("library-unopenable").join("library.db");
        std::fs::write(&path, b"this is not a database").expect("the file is written");
        let library = LibraryReader::at(Some(path));

        let refusal = library
            .sessions(&LibrarySessions::default())
            .expect_err("an unreadable library is not an empty one");

        assert_eq!(refusal.code, ErrorCode::LibraryUnavailable);
        assert!(
            refusal.message.contains("could not be opened"),
            "the refusal has to say what went wrong: {}",
            refusal.message
        );
    }

    #[test]
    fn a_search_that_does_not_parse_is_refused_with_what_was_wrong_with_it() {
        // Not an empty result set: a search box has to be able to say what is
        // wrong with what was typed (AGENTS.md section 45).
        let library = library_with_a_missing_recording("library-bad-query");

        let refusal = library
            .sessions(&LibrarySessions {
                query: Some("game:".to_owned()),
                ..LibrarySessions::default()
            })
            .expect_err("an unparseable query is refused");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("library_sessions"),
            "the refusal should name the command: {}",
            refusal.message
        );
    }

    #[test]
    fn a_query_that_parses_selects_the_sittings_it_names() {
        let library = library_with_a_missing_recording("library-query");

        let matched = library
            .sessions(&LibrarySessions {
                query: Some("game:counter".to_owned()),
                ..LibrarySessions::default()
            })
            .expect("the library reads");
        assert_eq!(matched.sessions.len(), 1);

        let unmatched = library
            .sessions(&LibrarySessions {
                query: Some("game:minecraft".to_owned()),
                ..LibrarySessions::default()
            })
            .expect("the library reads");
        assert!(
            unmatched.sessions.is_empty(),
            "a query that selects nothing is an empty page, not every session"
        );
    }

    #[test]
    fn a_blank_search_is_the_whole_library_rather_than_a_refusal() {
        // What a search box sends when the user clears it.
        let library = library_with_a_missing_recording("library-blank-query");

        let page = library
            .sessions(&LibrarySessions {
                query: Some("   ".to_owned()),
                ..LibrarySessions::default()
            })
            .expect("a blank query is not a bad one");

        assert_eq!(page.sessions.len(), 1);
    }

    /// A library of `sessions` sittings, each holding `recordings` recordings.
    ///
    /// Rows in a real index rather than [`IndexedSession`] values handed
    /// straight to a private function, because the properties the three tests
    /// below are named for belong to [`LibraryReader::sessions`] and not to
    /// anything it happens to call. A test that never opened a database could
    /// not tell "the page a window receives is bounded" from "a function exists
    /// which would bound one if it were called".
    ///
    /// The sittings are ordered by `started_at`, newest last, so the session at
    /// rank `r` of a page — newest first — is [`session_id_at_rank`].
    fn library_of(name: &str, sessions: usize, recordings_each: usize) -> LibraryReader {
        let path = scratch_directory(name).join("library.db");
        {
            let database = Database::open(&path).expect("a database opens");
            let connection = database.connection();
            // One transaction for the lot. Several thousand rows at one commit
            // each would make these tests slow enough that somebody stops
            // running them.
            connection
                .execute_batch("BEGIN")
                .expect("a transaction begins");
            connection
                .execute(
                    "INSERT INTO games (game_id, name, first_seen_at) \
                     VALUES ('counter-strike-2', 'Counter-Strike 2', '2026-01-01T00:00:00+01:00')",
                    [],
                )
                .expect("a game inserts");

            for index in 0..sessions {
                let session_id = session_id_of(index);
                connection
                    .execute(
                        "INSERT INTO sessions \
                            (session_id, game_id, started_at, ended_at, end_reason, favourited_at) \
                         VALUES (?1, 'counter-strike-2', ?2, ?3, 'game-exited', ?3)",
                        params![
                            session_id,
                            started_at_of(index),
                            "2026-01-02T00:00:00+01:00"
                        ],
                    )
                    .expect("a session inserts");

                for ordinal in 0..recordings_each {
                    connection
                        .execute(
                            "INSERT INTO recordings \
                                (session_id, session_index, path, started_at, ended_at, outcome, \
                                 end_reason, duration_seconds, width, height, size_bytes, \
                                 favourited_at) \
                             VALUES (?1, ?2, ?3, ?4, '2026-01-02T00:00:00+01:00', 'recorded', \
                                     'target-lost', 6540.0, 2560, 1440, 9812009112, ?4)",
                            params![
                                session_id,
                                ordinal as i64 + 1,
                                format!(
                                    r"D:\Recordings\Counter-Strike 2\clipped-{session_id}-{ordinal}.mkv"
                                ),
                                started_at_of(index),
                            ],
                        )
                        .expect("a recording inserts");
                }
            }

            connection.execute_batch("COMMIT").expect("it commits");
        }
        LibraryReader::at(Some(path))
    }

    /// The identifier of the sitting `library_of` inserted `index`th.
    fn session_id_of(index: usize) -> String {
        format!("counter-strike-2-{index:04}")
    }

    /// When it started. Distinct per sitting, so the newest-first order these
    /// tests reason about is total rather than a tie broken by the identifier.
    fn started_at_of(index: usize) -> String {
        format!("2026-01-01T{:02}:{:02}:00+01:00", index / 60, index % 60)
    }

    /// The identifier of the sitting at `rank` of a newest-first listing over a
    /// library of `sessions` sittings, counting from zero.
    fn session_id_at_rank(sessions: usize, rank: usize) -> String {
        session_id_of(sessions - 1 - rank)
    }

    #[test]
    fn a_page_is_bounded_by_what_a_frame_can_carry_rather_than_by_a_count_of_sessions() {
        // The defect this exists for is not hypothetical: the index will hand
        // over `MAX_PAGE_LIMIT` — two hundred — sessions, and two hundred
        // sessions is 135 KB with one recording each and over 3 MB with thirty.
        // A frame over `MAX_FRAME_BYTES` is not a failed request; the reader
        // closes the connection (`crates/ipc/src/frame.rs`), so a busy library
        // would drop the window's control connection on every attempt.
        //
        // Read through `sessions`, which is what the recorder's dispatch calls:
        // the bound is only worth anything if it is on the path a window's
        // question actually takes.
        const SESSIONS: usize = 120;
        let library = library_of("library-frame-bound", SESSIONS, 40);

        let page = library
            .sessions(&LibrarySessions {
                limit: Some(200),
                ..LibrarySessions::default()
            })
            .expect("the library reads");
        let bytes = serde_json::to_vec(&page).expect("a page serialises").len();

        assert!(
            bytes < MAX_FRAME_BYTES as usize,
            "a page has to fit in a frame, and this one is {bytes} bytes"
        );
        assert!(
            !page.sessions.is_empty() && page.sessions.len() < SESSIONS,
            "this page could only fit by being cut short, and it carried {} of {SESSIONS}",
            page.sessions.len()
        );

        // Cut short, so the cursor must continue at the first session left out
        // — not past every one of them, which is where the index's own cursor
        // points. Asked rather than compared to a string, because what the next
        // request does with it is the whole of what the cursor is for: getting
        // this wrong silently loses every session between the two.
        let carried = page.sessions.len();
        let next = library
            .sessions(&LibrarySessions {
                limit: Some(200),
                after: page.next_cursor.clone(),
                ..LibrarySessions::default()
            })
            .expect("the library reads");

        assert_eq!(
            next.sessions
                .first()
                .map(|session| session.session_id.as_str()),
            Some(session_id_at_rank(SESSIONS, carried).as_str()),
            "the page after a truncated one has to start with the first sitting left out"
        );
    }

    #[test]
    fn a_page_that_fits_keeps_the_cursor_the_index_gave_it() {
        // The other half: truncation must not fire when it is not needed, or
        // every page would end early and paging would take more round trips
        // than there are sessions.
        const SESSIONS: usize = 30;
        let library = library_of("library-page-fits", SESSIONS, 1);

        let page = library
            .sessions(&LibrarySessions {
                limit: Some(25),
                ..LibrarySessions::default()
            })
            .expect("the library reads");

        assert_eq!(page.sessions.len(), 25);

        let next = library
            .sessions(&LibrarySessions {
                limit: Some(25),
                after: page.next_cursor.clone(),
                ..LibrarySessions::default()
            })
            .expect("the library reads");

        assert_eq!(
            next.sessions
                .first()
                .map(|session| session.session_id.as_str()),
            Some(session_id_at_rank(SESSIONS, 25).as_str()),
            "a page that fitted has to continue where the index said, not earlier"
        );
    }

    #[test]
    fn a_single_session_too_large_for_the_budget_is_still_answered() {
        // Otherwise a page comes back empty, the caller reads "nothing here",
        // and the session can never be reached however many times it pages.
        // One oversized frame is the lesser evil, and the frame reader's own
        // ceiling is well above any one sitting.
        let library = library_of("library-one-enormous", 1, 2_500);

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");

        assert_eq!(
            page.sessions.len(),
            1,
            "a sitting that will not fit the budget still has to be answered"
        );
        assert_eq!(page.sessions[0].recordings.len(), 2_500);
        assert_eq!(page.next_cursor, None);

        // And it really is over the budget, or this test proves nothing: an
        // assertion that a page of one holds one is only interesting when that
        // one was too large to carry.
        let cost = serde_json::to_vec(&page.sessions[0])
            .expect("a session serialises")
            .len();
        assert!(
            cost > PAGE_BUDGET_BYTES,
            "this sitting was meant to overrun the {PAGE_BUDGET_BYTES}-byte budget and is {cost} \
             bytes"
        );
    }

    #[test]
    fn a_machine_with_no_per_user_directory_says_that_rather_than_answering_nothing() {
        let library = LibraryReader::at(None);

        let refusal = library
            .games()
            .expect_err("there is no library to read on such a machine");

        assert_eq!(refusal.code, ErrorCode::LibraryUnavailable);
    }

    /// A request to favourite something.
    fn asking(kind: &str, session_id: &str, id: i64, favourite: bool) -> SetFavourite {
        SetFavourite {
            kind: kind.to_owned(),
            session_id: session_id.to_owned(),
            id,
            favourite,
        }
    }

    /// When the tests below claim things happened.
    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + core::time::Duration::from_secs(1_786_000_000)
    }

    #[test]
    fn a_sitting_and_the_recording_in_it_can_both_be_favourited_and_unfavourited() {
        // Issue #58's first acceptance criterion at the boundary the window
        // actually reaches: the mark has to *persist*, and it is the reads on
        // this same reader that say whether it did.
        let library = library_with_a_missing_recording("favourite-round-trip");
        let recording_id = 1;

        for (kind, session_id, id) in [
            ("session", "cs2-20260811-201400", 0),
            ("recording", "", recording_id),
        ] {
            let marked = library
                .set_favourite(&asking(kind, session_id, id, true), at())
                .expect("a favourite is written");
            assert!(marked.favourite, "{kind} should be a favourite now");
            assert!(marked.changed, "{kind} was not one before");

            // Again, which is a second click on a full star. It must not look
            // like a failure and must not move the instant it was first marked.
            let again = library
                .set_favourite(&asking(kind, session_id, id, true), at())
                .expect("marking twice is not an error");
            assert!(again.favourite, "{kind}");
            assert!(!again.changed, "{kind} was already a favourite");

            let cleared = library
                .set_favourite(&asking(kind, session_id, id, false), at())
                .expect("a favourite is cleared");
            assert!(!cleared.favourite, "{kind}");
            assert!(cleared.changed, "{kind} was a favourite until now");
        }
    }

    #[test]
    fn the_mark_a_favourite_reports_is_the_one_the_library_read_answers_with() {
        // The reply is not what was asked for, it is what is true. A row the
        // index does not hold is written to by nothing, and a window told
        // "favourited" would draw a star it loses on the next read.
        let library = library_with_a_missing_recording("favourite-read-back");

        let answer = library
            .set_favourite(&asking("recording", "", 9_999, true), at())
            .expect("a write against a row that is not there is not an error");

        assert!(
            !answer.favourite,
            "nothing was written, so nothing is favourited"
        );
        assert!(!answer.changed);

        // And the session listing agrees, which is the read a screen makes.
        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");
        assert!(!page.sessions[0].recordings[0].favourite);
    }

    #[test]
    fn a_target_that_names_nothing_is_refused_rather_than_marking_row_zero() {
        // Both halves of the target are optional on the wire — one kind reads
        // each — so a request that filled in neither would otherwise favourite
        // whatever row happens to sit at identifier zero.
        for (kind, session_id, id, expected) in [
            ("session", "", 0, "session_id"),
            ("recording", "", 0, "id"),
            ("clip", "", 0, "id"),
        ] {
            let refusal = LibraryReader::at(None)
                .set_favourite(&asking(kind, session_id, id, true), at())
                .expect_err("a target that names nothing is refused");

            assert_eq!(refusal.code, ErrorCode::InvalidParameters, "{kind}");
            assert!(
                refusal.message.contains(expected) && refusal.message.contains(kind),
                "a refusal has to name the field that was missing: {}",
                refusal.message
            );
        }
    }

    #[test]
    fn a_kind_nothing_can_be_favourited_by_is_refused_and_says_what_can() {
        // A screenshot is in issue #58's scope and has no table in the schema,
        // so this is the message somebody gets when the two disagree.
        let refusal = LibraryReader::at(None)
            .set_favourite(&asking("screenshot", "", 1, true), at())
            .expect_err("screenshots have nowhere to keep a mark");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        for named in ["session", "recording", "clip"] {
            assert!(
                refusal.message.contains(named),
                "the refusal should say what can be favourited: {}",
                refusal.message
            );
        }
    }

    #[test]
    fn the_target_is_read_before_the_library_is_opened() {
        // A malformed request is the caller's mistake whatever the state of the
        // machine, and `LibraryReader::at(None)` is a machine with no library at
        // all: these refusals arrive as `InvalidParameters` rather than as
        // `LibraryUnavailable`, which is what the tests above rely on.
        let refusal = LibraryReader::at(None)
            .set_favourite(&asking("session", "cs2-20260811-201400", 0, true), at())
            .expect_err("there is no library on such a machine");

        assert_eq!(
            refusal.code,
            ErrorCode::LibraryUnavailable,
            "a well-formed request against no library is the machine's problem, not the caller's"
        );
    }
    /// A request to lock something.
    fn locking(kind: &str, session_id: &str, id: i64, locked: bool) -> SetLock {
        SetLock {
            kind: kind.to_owned(),
            session_id: session_id.to_owned(),
            id,
            locked,
        }
    }

    #[test]
    fn a_sitting_and_the_recording_in_it_can_both_be_locked_and_unlocked() {
        let library = library_with_a_missing_recording("lock-round-trip");

        for (kind, session_id, id) in [("session", "cs2-20260811-201400", 0), ("recording", "", 1)]
        {
            let set = library
                .set_lock(&locking(kind, session_id, id, true), at())
                .expect("a lock is written");
            assert!(set.locked, "{kind} should be locked now");
            assert!(set.protected, "{kind} should be protected now");
            assert!(set.changed, "{kind} was not locked before");

            let again = library
                .set_lock(&locking(kind, session_id, id, true), at())
                .expect("locking twice is not an error");
            assert!(again.locked, "{kind}");
            assert!(!again.changed, "{kind} was already locked");

            let cleared = library
                .set_lock(&locking(kind, session_id, id, false), at())
                .expect("a lock is cleared");
            assert!(!cleared.locked, "{kind}");
            assert!(cleared.changed, "{kind} was locked until now");
        }
    }

    #[test]
    fn a_recording_in_a_locked_sitting_is_protected_without_being_locked() {
        // The cascade, at the boundary. `locked` and `protected` are different
        // questions and this is the case that separates them: a screen drawing
        // a padlock from `locked` alone would show this recording as one
        // cleanup may take, and cleanup would not take it.
        let library = library_with_a_missing_recording("lock-cascade");

        library
            .set_lock(&locking("session", "cs2-20260811-201400", 0, true), at())
            .expect("the sitting locks");

        // Asked about the recording without changing it: setting the state it
        // is already in writes nothing, and the reply still reports both.
        let recording = library
            .set_lock(&locking("recording", "", 1, false), at())
            .expect("it can be asked");

        assert!(
            !recording.locked,
            "the recording has no lock of its own, and unlocking the sitting must not have to \
             find and clear one"
        );
        assert!(
            recording.protected,
            "it is inside a locked sitting, so automatic cleanup will not take it"
        );
        assert!(!recording.changed, "nothing was written");
    }

    #[test]
    fn a_recording_reaches_the_window_protected_by_its_sittings_lock() {
        // The cascade as it crosses the boundary, which is the one place it is
        // expressed for a window: `protected` is the recording's own lock *or*
        // its sitting's, worked out here so no window has to know the rule.
        //
        // A screen test cannot cover this — it stubs the page read — so without
        // this case the mapping could drop the sitting's half and nothing would
        // fail.
        let library = library_with_a_missing_recording("lock-on-the-wire");

        library
            .set_lock(&locking("session", "cs2-20260811-201400", 0, true), at())
            .expect("the sitting locks");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");
        let session = &page.sessions[0];
        let recording = &session.recordings[0];

        assert!(session.locked, "the sitting carries its own lock");
        assert!(
            !recording.locked,
            "the recording has none of its own, which is what a control is drawn from"
        );
        assert!(
            recording.protected,
            "and it is protected anyway, which is what a padlock is drawn from"
        );
    }

    #[test]
    fn a_recordings_own_lock_reaches_the_window_as_its_own_lock() {
        // The column beside it is `favourited_at`, and reading that one instead
        // would be invisible to every other test here: the cascade case asks
        // about a *session's* lock, and the negative case has both columns
        // empty. So this one locks the recording and favourites nothing, which
        // is the only arrangement that tells the two columns apart.
        let library = library_with_a_missing_recording("lock-own-on-the-wire");

        library
            .set_lock(&locking("recording", "", 1, true), at())
            .expect("the recording locks");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");
        let recording = &page.sessions[0].recordings[0];

        assert!(recording.locked, "its own lock is what it carries");
        assert!(recording.protected, "and its own lock protects it");
        assert!(
            !recording.favourite,
            "nothing favourited it, so a `locked` read off the favourite column \
             would be false here rather than true"
        );
        assert!(
            !page.sessions[0].locked,
            "the sitting has no lock of its own"
        );
    }

    #[test]
    fn a_favourite_is_not_mistaken_for_a_lock() {
        // The other direction of the same off-by-one: favouriting a recording
        // must not make it look locked.
        let library = library_with_a_missing_recording("favourite-is-not-a-lock");

        library
            .set_favourite(&asking("recording", "", 1, true), at())
            .expect("the recording is favourited");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");
        let recording = &page.sessions[0].recordings[0];

        assert!(recording.favourite);
        assert!(!recording.locked, "a favourite is not a lock");
        assert!(
            !recording.protected,
            "and it does not protect against cleanup through the lock path — \
             `Protection::Favourite` is a different rule, in `accounting::cleanup`"
        );
    }

    #[test]
    fn a_recording_with_no_lock_anywhere_reaches_the_window_unprotected() {
        // The other side of the same rule, or the assertion above would be
        // satisfied by a field that was always true.
        let library = library_with_a_missing_recording("lock-on-the-wire-absent");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");

        assert!(!page.sessions[0].locked);
        assert!(!page.sessions[0].recordings[0].locked);
        assert!(!page.sessions[0].recordings[0].protected);
    }

    #[test]
    fn a_clip_cannot_be_locked_and_the_refusal_says_what_to_lock_instead() {
        // Not silently ignored: a window told "done" would draw a padlock
        // against a clip that automatic cleanup never consults.
        let refusal = LibraryReader::at(None)
            .set_lock(&locking("clip", "", 1, true), at())
            .expect_err("clips are not what cleanup deletes");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("recording"),
            "the refusal has to say what to lock instead: {}",
            refusal.message
        );
    }

    #[test]
    fn a_lock_target_that_names_nothing_is_refused_rather_than_locking_row_zero() {
        for (kind, session_id, id, expected) in
            [("session", "", 0, "session_id"), ("recording", "", 0, "id")]
        {
            let refusal = LibraryReader::at(None)
                .set_lock(&locking(kind, session_id, id, true), at())
                .expect_err("a target that names nothing is refused");

            assert_eq!(refusal.code, ErrorCode::InvalidParameters, "{kind}");
            assert!(
                refusal.message.contains(expected) && refusal.message.contains(kind),
                "a refusal has to name the field that was missing: {}",
                refusal.message
            );
        }
    }

    #[test]
    fn a_lock_stops_automatic_cleanup_taking_the_recording() {
        // The whole point of the column, through the boundary a window reaches:
        // lock it here, and the sweep's own candidate list says why it will not
        // be taken.
        let library = library_with_a_missing_recording("lock-protects");

        library
            .set_lock(&locking("recording", "", 1, true), at())
            .expect("it locks");

        let protection = library
            .with_database_mut(|database| {
                let candidates = clipped_library::accounting::cleanup::candidates(database)
                    .map_err(unreadable)?;
                Ok(candidates
                    .iter()
                    .find(|candidate| candidate.item.id == 1)
                    .and_then(|candidate| candidate.protection))
            })
            .expect("the candidates can be read");

        assert_eq!(
            protection,
            Some(clipped_library::accounting::cleanup::Protection::Locked),
            "the sweep has to say the lock is what saved it"
        );
    }
}
