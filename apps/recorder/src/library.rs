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
    ErrorCode, LibraryClip, LibraryGame, LibraryRecording, LibrarySession, LibrarySessionPage,
    LibrarySessions, ProtocolError, MAX_FRAME_BYTES,
};
use clipped_library::index::{
    cursor_of, game_summaries, list_sessions, reconcile, GameSummary, IndexControl, IndexReport,
    IndexSettings, IndexedClip, IndexedRecording, IndexedSession, SessionListing,
};
use clipped_library::search::Query;
use clipped_storage::Database;

/// The file the library index lives in, under Clipped's per-user directory.
const LIBRARY_FILE: &str = "library.db";

/// The recording library, as this process reads it for the window.
#[derive(Debug)]
pub struct LibraryReader {
    /// Where the index is, or [`None`] when this machine describes no per-user
    /// directory at all — which on Windows means `%LOCALAPPDATA%` is unset.
    path: Option<PathBuf>,
    database: Mutex<Option<Database>>,
}

impl LibraryReader {
    /// A reader for the library at Clipped's usual place:
    /// `%LOCALAPPDATA%\Clipped\library.db`, beside the logs (`docs/storage.md`).
    #[must_use]
    pub fn for_this_user() -> Self {
        Self::at(
            clipped_logging::application_directory().map(|directory| directory.join(LIBRARY_FILE)),
        )
    }

    /// A reader for the library at a named path, or for no library at all.
    #[must_use]
    pub fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            database: Mutex::new(None),
        }
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
        recordings: session.recordings.iter().map(recording).collect(),
        clips: session.clips.iter().map(clip).collect(),
    }
}

/// One recording, on the wire.
fn recording(recording: &IndexedRecording) -> LibraryRecording {
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
    control: IndexControl,
    state: Mutex<IndexerState>,
    woken: Condvar,
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
        Self::at(
            clipped_logging::application_directory().map(|directory| directory.join(LIBRARY_FILE)),
            crate::config::default_output_directory()
                .into_iter()
                .collect(),
        )
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
                settings: IndexSettings::new(roots),
                control: IndexControl::new(),
                state: Mutex::new(IndexerState::default()),
                woken: Condvar::new(),
            }),
            thread: Mutex::new(None),
        }
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
            Ok(report) => report_unindexed(&report),
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
}
