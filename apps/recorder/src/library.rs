//! Answering the desktop application's questions about the recording library.
//!
//! The window cannot read `library.db`. It has no file-system permission for
//! the file and it may not link `clipped-library`
//! ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md),
//! `tests/integration/tests/workspace_layering.rs`), so the process that owns
//! the database answers for it — which is this one
//! ([issue #301](https://github.com/wildware-uk/clipped/issues/301)).
//!
//! This module is the join between two vocabularies and nothing else:
//! `clipped_library::index` says what a row is, `clipped_ipc::library` says what
//! goes on the wire, and neither knows about the other. Keeping the conversion
//! here is what lets the protocol crate stay a leaf that the window can link
//! (`crates/ipc/src/lib.rs`) and the library crate stay ignorant of the
//! protocol.
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
use std::sync::Mutex;

use clipped_ipc::{
    ErrorCode, LibraryClip, LibraryGame, LibraryRecording, LibrarySession, LibrarySessionPage,
    LibrarySessions, ProtocolError, MAX_FRAME_BYTES,
};
use clipped_library::index::{
    cursor_of, game_summaries, list_sessions, GameSummary, IndexedClip, IndexedRecording,
    IndexedSession, SessionListing,
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
/// builds both.
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

    /// A sitting with `recordings` recordings, sized like a real one.
    fn bulky_session(index: usize, recordings: usize) -> IndexedSession {
        IndexedSession {
            session_id: format!("counter-strike-2-2026081{index}-201400"),
            game_id: Some("counter-strike-2".to_owned()),
            game_name: Some("Counter-Strike 2".to_owned()),
            started_at: format!("2026-08-{:02}T20:14:00+01:00", (index % 28) + 1),
            ended_at: Some("2026-08-11T22:03:00+01:00".to_owned()),
            end_reason: Some("game-exited".to_owned()),
            favourite: true,
            recordings: (0..recordings)
                .map(|n| IndexedRecording {
                    recording_id: n as i64,
                    session_index: n as i64 + 1,
                    path: format!(
                        r"D:\Recordings\Counter-Strike 2\clipped-cs2-2026081{index}-201400-{n}.mkv"
                    ),
                    started_at: "2026-08-11T20:14:00+01:00".to_owned(),
                    ended_at: Some("2026-08-11T22:03:00+01:00".to_owned()),
                    outcome: Some("recorded".to_owned()),
                    end_reason: Some("target-lost".to_owned()),
                    duration_seconds: Some(6_540.0),
                    width: Some(2560),
                    height: Some(1440),
                    size_bytes: Some(9_812_009_112),
                    missing_since: None,
                    favourite: true,
                    tags: vec!["clutch".to_owned(), "ace".to_owned(), "ranked".to_owned()],
                })
                .collect(),
            clips: Vec::new(),
        }
    }

    #[test]
    fn a_page_is_bounded_by_what_a_frame_can_carry_rather_than_by_a_count_of_sessions() {
        // The defect this exists for is not hypothetical: the index will hand
        // over `MAX_PAGE_LIMIT` — two hundred — sessions, and two hundred
        // sessions is 135 KB with one recording each and over 3 MB with thirty.
        // A frame over `MAX_FRAME_BYTES` is not a failed request; the reader
        // closes the connection (`crates/ipc/src/frame.rs`), so a busy library
        // would drop the window's control connection on every attempt.
        let bulky: Vec<IndexedSession> = (0..200).map(|i| bulky_session(i, 30)).collect();

        let page = within_one_frame(&bulky, Some("the index's own cursor".to_owned()));
        let bytes = serde_json::to_vec(&page).expect("a page serialises").len();

        assert!(
            bytes < MAX_FRAME_BYTES as usize,
            "a page has to fit in a frame, and this one is {bytes} bytes"
        );
        assert!(
            page.sessions.len() < bulky.len(),
            "this page could only fit by being cut short, and it was not"
        );

        // Cut short, so the cursor must name the last session *carried* — not
        // the one the index offered, which is past every session left out. That
        // cursor is what the next request continues after, so getting it wrong
        // silently loses every session between the two.
        let last = &bulky[page.sessions.len() - 1];
        assert_eq!(page.next_cursor.as_deref(), Some(cursor_of(last).as_str()));
    }

    #[test]
    fn a_page_that_fits_keeps_the_cursor_the_index_gave_it() {
        // The other half: truncation must not fire when it is not needed, or
        // every page would end early and paging would take more round trips
        // than there are sessions.
        let small: Vec<IndexedSession> = (0..25).map(|i| bulky_session(i, 1)).collect();

        let page = within_one_frame(&small, Some("the index's own cursor".to_owned()));

        assert_eq!(page.sessions.len(), 25);
        assert_eq!(page.next_cursor.as_deref(), Some("the index's own cursor"));
    }

    #[test]
    fn a_single_session_too_large_for_the_budget_is_still_answered() {
        // Otherwise a page comes back empty, the caller reads "nothing here",
        // and the session can never be reached however many times it pages.
        // One oversized frame is the lesser evil, and the frame reader's own
        // ceiling is well above any one sitting.
        let enormous = vec![bulky_session(0, 4_000)];

        let page = within_one_frame(&enormous, None);

        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.next_cursor, None);
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
