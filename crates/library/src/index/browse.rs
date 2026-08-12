//! Reading the library back out: the sittings, the files they produced, and
//! which of those files have gone.
//!
//! [`super`] writes the index. This is the other direction, and it is what the
//! desktop application asks for over the control protocol
//! ([issue #301](https://github.com/wildware-uk/clipped/issues/301)): the
//! window may not link this crate, so the recorder reads here and answers there.
//!
//! # Everything is a page
//!
//! A library is unbounded — `docs/library.md` measures ten thousand sessions —
//! so there is no function here that returns all of it. [`list_sessions`] takes
//! a [`SessionListing`] and answers one page, with a [`SessionPage::next`]
//! cursor when there is more.
//!
//! The cursor is a **keyset**, not an offset: it names the last session on the
//! page, and the next page is "everything ordered after that one". An offset
//! would make the tenth page ten times the cost of the first, and would skip or
//! repeat rows when a reconciliation inserted a session between two requests.
//! Keyset paging costs the same at every depth, which is what lets
//! `docs/library.md` state one figure for what a page costs rather than a curve.
//!
//! # Newest first, and always the same order
//!
//! Sessions are ordered by `started_at` descending and then by `session_id`
//! descending. The second is not decoration: two sessions can share a start
//! moment — a machine that resumed and started two games — and an order that is
//! not total cannot be paged through without losing or repeating one.
//!
//! # A missing file is listed, never omitted
//!
//! [`IndexedRecording::missing_since`] crosses the boundary because a screen has
//! to *say* a file has gone rather than draw a broken tile (AGENTS.md section
//! 27). Filtering missing rows out here would leave the window unable to tell
//! "you deleted this" from "this was never recorded". A row in the trash
//! (`deleted_at`) is a different thing and is left out: it is deleted as far as
//! the library is concerned, and the trash has a screen of its own
//! ([issue #94](https://github.com/wildware-uk/clipped/issues/94)).
//!
//! # Search runs the matcher rather than a compiler
//!
//! A [`Query`](crate::search::Query) is applied by building the
//! [`Row`](crate::search::Row) a session projects and asking
//! [`Query::matches`](crate::search::Query::matches). The alternative —
//! compiling the query into SQL, which `crate::search` documents how to do — is
//! a second implementation of what a match means, and the two disagreeing would
//! be a bug nobody could see. The matcher stays the definition; this is the
//! caller of it.
//!
//! What that costs is a walk of the sessions rather than an index lookup, and
//! `docs/library.md` measures it. It is bounded work over local data with no
//! allocation in the match itself, and the moment it stops being fast enough the
//! answer is a compiler checked against this matcher, not instead of it.
//!
//! # What a query means for a *session*
//!
//! The language matches one searchable thing (`docs/search.md`), and the thing
//! being listed here is a sitting. So a session's row is built from what the
//! sitting is: its game, its identifier, the day it started, how much footage it
//! produced, whether it or anything in it is favourited, the tags on its
//! recordings and clips, and the titles of those clips. `game:cs2`,
//! `tag:clutch`, `favourite`, `date:>2026-08-01` and `duration:>30m` therefore
//! all mean what a person would expect of a session list.

use std::collections::HashMap;

use clipped_storage::rusqlite::{params, Connection, Row as SqlRow};
use clipped_storage::Database;
use time::format_description::well_known::Rfc3339;
use time::{Duration as TimeDuration, OffsetDateTime};

use crate::search::{Date, Query, Row};

use super::error::IndexError;

/// How many sessions a page holds when the caller does not say.
pub const DEFAULT_PAGE_LIMIT: usize = 50;

/// The most a page will hold, whatever is asked for.
///
/// A page is one protocol frame, and a frame has a ceiling
/// (`clipped_ipc::MAX_FRAME_BYTES`). Two hundred sessions with their recordings
/// is comfortably inside it; a caller that asks for ten thousand is asking for a
/// frame that cannot be sent, and clamping is a better answer than a refusal
/// nobody could have predicted.
pub const MAX_PAGE_LIMIT: usize = 200;

/// How many sessions a search reads per statement.
///
/// Most of them will be discarded, so reading one at a time would be a round
/// trip per rejected session.
const SEARCH_BATCH: usize = 256;

/// Which page of the library to read, and which part of it.
#[derive(Debug, Clone, Default)]
pub struct SessionListing<'a> {
    /// How many sessions to answer with. Clamped to [`MAX_PAGE_LIMIT`]; zero
    /// means [`DEFAULT_PAGE_LIMIT`].
    pub limit: usize,
    /// Continue after the session this cursor names, as [`SessionPage::next`]
    /// gave it. [`None`] starts at the newest session.
    pub after: Option<String>,
    /// Only sessions this query selects. [`None`] selects every session.
    pub query: Option<&'a Query>,
}

/// One page of sessions, newest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionPage {
    /// The sessions on this page.
    pub sessions: Vec<IndexedSession>,
    /// The cursor for the page after this one, or [`None`] at the end of the
    /// library.
    ///
    /// Present only when another session was actually found, so a caller can
    /// stop on this rather than on an empty page.
    pub next: Option<String>,
}

/// One sitting, with what it produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexedSession {
    /// The identifier the recorder generated, which the sidecar and the files
    /// share.
    pub session_id: String,
    /// The catalogue's identifier for the game, or [`None`] for a session
    /// nothing was attributed to.
    pub game_id: Option<String>,
    /// The game's name as the catalogue knew it, or [`None`] for the same
    /// reason. What to call an unattributed sitting on screen is the screen's
    /// decision, not this module's.
    pub game_name: Option<String>,
    /// When the sitting started, RFC 3339 with an offset.
    pub started_at: String,
    /// When it ended, or [`None`] for one that has not.
    pub ended_at: Option<String>,
    /// Why it ended, in `clipped_session`'s vocabulary.
    pub end_reason: Option<String>,
    /// Whether the user favourited the sitting itself.
    pub favourite: bool,
    /// The files it recorded, in the order they were recorded.
    pub recordings: Vec<IndexedRecording>,
    /// The clips cut from it.
    ///
    /// Empty in every database this build writes: nothing creates a clip yet
    /// ([issue #91](https://github.com/wildware-uk/clipped/issues/91)). The
    /// shape is here because the read is, and a clip saved with no session
    /// behind it is not reachable from a session listing — it needs a listing of
    /// its own, with the screen that creates one.
    pub clips: Vec<IndexedClip>,
}

/// One media file a sitting produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexedRecording {
    /// The index's own identifier for it.
    pub recording_id: i64,
    /// Its ordinal within the session, as the sidecar recorded it.
    pub session_index: i64,
    /// The file.
    pub path: String,
    /// When it started, RFC 3339 with an offset.
    pub started_at: String,
    /// When it ended, or [`None`] for one still running.
    pub ended_at: Option<String>,
    /// What became of it: `recorded`, `no-window` or `failed`.
    pub outcome: Option<String>,
    /// Why it ended, and only meaningful for `recorded`.
    pub end_reason: Option<String>,
    /// How long it runs for.
    pub duration_seconds: Option<f64>,
    /// The encoded picture size.
    pub width: Option<i64>,
    /// The encoded picture size.
    pub height: Option<i64>,
    /// What the file occupied when it was last seen.
    ///
    /// Kept for a row whose file has gone, so a drive coming back needs no
    /// re-measurement — but a screen must not add it into a total while
    /// [`Self::missing_since`] is set, because that space is not being used.
    pub size_bytes: Option<i64>,
    /// When reconciliation first found the file gone, or [`None`] while it is
    /// there.
    pub missing_since: Option<String>,
    /// Whether the user favourited it.
    pub favourite: bool,
    /// The tags on it, alphabetically.
    pub tags: Vec<String>,
}

/// One clip cut from a sitting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexedClip {
    /// The index's own identifier for it.
    pub clip_id: i64,
    /// The file.
    pub path: String,
    /// What it is called, if anything.
    pub title: Option<String>,
    /// When it was made, RFC 3339 with an offset.
    pub created_at: String,
    /// How long it runs for.
    pub duration_seconds: Option<f64>,
    /// What the file occupies, with the same caveat as a recording's.
    pub size_bytes: Option<i64>,
    /// When reconciliation first found the file gone.
    pub missing_since: Option<String>,
    /// Whether the user favourited it.
    pub favourite: bool,
    /// The tags on it, alphabetically.
    pub tags: Vec<String>,
}

/// One page of sessions, newest first, with everything each one produced.
///
/// # Errors
///
/// [`IndexError::Database`] if the database refuses. This reads and writes
/// nothing, so it is safe to run while a reconciliation writes: the database is
/// in write-ahead logging mode and a reader sees a consistent snapshot
/// (`docs/storage.md`).
pub fn list_sessions(
    database: &Database,
    listing: &SessionListing<'_>,
) -> Result<SessionPage, IndexError> {
    let limit = page_limit(listing.limit);
    let batch = batch_size(limit, listing.query);
    let connection = database.connection();

    let mut sessions = Vec::with_capacity(limit);
    let mut next = None;
    let mut after = cursor_parts(listing.after.as_deref());

    // A batch at a time rather than the whole library: an unfiltered listing
    // reads one batch and stops, and a search walks as many as it has to
    // without ever holding more than one batch in memory.
    'walk: loop {
        let headers = session_headers(connection, after.as_ref(), batch)?;
        let exhausted = headers.len() < batch;
        if headers.is_empty() {
            break;
        }
        after = headers
            .last()
            .map(|header| (header.started_at.clone(), header.session_id.clone()));

        for header in headers {
            let session = hydrate(connection, header)?;
            if listing
                .query
                .is_some_and(|query| !query.matches(&row_of(&session)))
            {
                continue;
            }
            // The walk looks one session past the page, so that `next` is
            // offered only when a further session really exists — a cursor
            // handed out at the end of the library would cost the caller a
            // request to discover the page was empty. The cursor names the last
            // session *on* the page, not the one past it, or the session that
            // proved there was more would be the one the next page skipped.
            if sessions.len() == limit {
                next = sessions.last().map(cursor_of);
                break 'walk;
            }
            sessions.push(session);
        }

        if exhausted {
            break;
        }
    }

    Ok(SessionPage { sessions, next })
}

/// The limit a listing actually gets.
fn page_limit(asked: usize) -> usize {
    match asked {
        0 => DEFAULT_PAGE_LIMIT,
        asked => asked.min(MAX_PAGE_LIMIT),
    }
}

/// How many session rows to read at a time.
///
/// Without a query, one past the page is all that will ever be looked at.
fn batch_size(limit: usize, query: Option<&Query>) -> usize {
    if query.is_some() {
        SEARCH_BATCH.max(limit + 1)
    } else {
        limit + 1
    }
}

/// The cursor that continues after this session.
fn cursor_of(session: &IndexedSession) -> String {
    format!("{}|{}", session.started_at, session.session_id)
}

/// Where a cursor says to carry on from.
///
/// An unreadable cursor is not an error: it is a string a caller kept across a
/// restart, and starting at the newest session is a better answer than refusing
/// to draw a library. A cursor with no `|` in it has no session in it either, so
/// there is nothing to continue after.
fn cursor_parts(cursor: Option<&str>) -> Option<(String, String)> {
    let cursor = cursor?;
    // The first separator: an RFC 3339 timestamp contains no `|`, and a session
    // identifier conceivably could.
    let (started_at, session_id) = cursor.split_once('|')?;
    if started_at.is_empty() || session_id.is_empty() {
        return None;
    }
    Some((started_at.to_owned(), session_id.to_owned()))
}

/// A session as its own row holds it, before anything it produced is read.
struct SessionHeader {
    session_id: String,
    game_id: Option<String>,
    game_name: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    end_reason: Option<String>,
    favourite: bool,
}

/// The next `limit` session rows after the cursor, newest first.
fn session_headers(
    connection: &Connection,
    after: Option<&(String, String)>,
    limit: usize,
) -> Result<Vec<SessionHeader>, IndexError> {
    // Row values — `(a, b) < (?, ?)` — are what make this one comparison rather
    // than the three-way `a < ? OR (a = ? AND b < ?)`, and SQLite uses the
    // `sessions_started_at` index for it either way.
    let mut statement = connection.prepare(
        "SELECT sessions.session_id, sessions.game_id, games.name, \
                sessions.started_at, sessions.ended_at, sessions.end_reason, \
                sessions.favourited_at \
         FROM sessions LEFT JOIN games ON games.game_id = sessions.game_id \
         WHERE (?1 IS NULL) \
            OR ((sessions.started_at, sessions.session_id) < (?1, ?2)) \
         ORDER BY sessions.started_at DESC, sessions.session_id DESC \
         LIMIT ?3",
    )?;

    let (at, id) = match after {
        Some((at, id)) => (Some(at.as_str()), Some(id.as_str())),
        None => (None, None),
    };
    let mut rows = statement.query(params![at, id, i64::try_from(limit).unwrap_or(i64::MAX)])?;

    let mut headers = Vec::new();
    while let Some(row) = rows.next()? {
        headers.push(read_header(row)?);
    }
    Ok(headers)
}

/// One session row.
fn read_header(row: &SqlRow<'_>) -> Result<SessionHeader, IndexError> {
    Ok(SessionHeader {
        session_id: row.get(0)?,
        game_id: row.get(1)?,
        game_name: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        end_reason: row.get(5)?,
        favourite: row.get::<_, Option<String>>(6)?.is_some(),
    })
}

/// A session header, with everything the sitting produced.
fn hydrate(connection: &Connection, header: SessionHeader) -> Result<IndexedSession, IndexError> {
    let recordings = recordings_of(connection, &header.session_id)?;
    let clips = clips_of(connection, &header.session_id)?;
    Ok(IndexedSession {
        session_id: header.session_id,
        game_id: header.game_id,
        game_name: header.game_name,
        started_at: header.started_at,
        ended_at: header.ended_at,
        end_reason: header.end_reason,
        favourite: header.favourite,
        recordings,
        clips,
    })
}

/// The recordings of one session, in the order they were recorded.
fn recordings_of(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<IndexedRecording>, IndexError> {
    let mut statement = connection.prepare(
        "SELECT recording_id, session_index, path, started_at, ended_at, outcome, end_reason, \
                duration_seconds, width, height, size_bytes, missing_since, favourited_at \
         FROM recordings WHERE session_id = ?1 AND deleted_at IS NULL \
         ORDER BY session_index",
    )?;
    let mut rows = statement.query(params![session_id])?;

    let mut recordings = Vec::new();
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        let recording_id: i64 = row.get(0)?;
        ids.push(recording_id);
        recordings.push(IndexedRecording {
            recording_id,
            session_index: row.get(1)?,
            path: row.get(2)?,
            started_at: row.get(3)?,
            ended_at: row.get(4)?,
            outcome: row.get(5)?,
            end_reason: row.get(6)?,
            duration_seconds: row.get(7)?,
            width: row.get(8)?,
            height: row.get(9)?,
            size_bytes: row.get(10)?,
            missing_since: row.get(11)?,
            favourite: row.get::<_, Option<String>>(12)?.is_some(),
            tags: Vec::new(),
        });
    }

    let mut tags = tags_for(connection, "recording_tags", "recording_id", &ids)?;
    for recording in &mut recordings {
        recording.tags = tags.remove(&recording.recording_id).unwrap_or_default();
    }
    Ok(recordings)
}

/// The clips of one session, oldest first.
fn clips_of(connection: &Connection, session_id: &str) -> Result<Vec<IndexedClip>, IndexError> {
    let mut statement = connection.prepare(
        "SELECT clip_id, path, title, created_at, duration_seconds, size_bytes, missing_since, \
                favourited_at \
         FROM clips WHERE session_id = ?1 AND deleted_at IS NULL \
         ORDER BY created_at, clip_id",
    )?;
    let mut rows = statement.query(params![session_id])?;

    let mut clips = Vec::new();
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        let clip_id: i64 = row.get(0)?;
        ids.push(clip_id);
        clips.push(IndexedClip {
            clip_id,
            path: row.get(1)?,
            title: row.get(2)?,
            created_at: row.get(3)?,
            duration_seconds: row.get(4)?,
            size_bytes: row.get(5)?,
            missing_since: row.get(6)?,
            favourite: row.get::<_, Option<String>>(7)?.is_some(),
            tags: Vec::new(),
        });
    }

    let mut tags = tags_for(connection, "clip_tags", "clip_id", &ids)?;
    for clip in &mut clips {
        clip.tags = tags.remove(&clip.clip_id).unwrap_or_default();
    }
    Ok(clips)
}

/// The tags on each of these rows, alphabetically.
///
/// One statement for the whole set rather than one per row: a session with three
/// recordings would otherwise be four round trips to answer a question about
/// tags nobody has applied yet.
fn tags_for(
    connection: &Connection,
    join_table: &str,
    key: &str,
    ids: &[i64],
) -> Result<HashMap<i64, Vec<String>>, IndexError> {
    let mut tags: HashMap<i64, Vec<String>> = HashMap::new();
    if ids.is_empty() {
        return Ok(tags);
    }

    // The identifiers are `i64` read out of this database's own primary keys, so
    // writing them into the statement cannot carry anything but digits. The
    // alternative — a bound parameter per identifier — is a different statement
    // for every list length.
    let list = ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = connection.prepare(&format!(
        "SELECT {join_table}.{key}, tags.name \
         FROM {join_table} JOIN tags ON tags.tag_id = {join_table}.tag_id \
         WHERE {join_table}.{key} IN ({list}) \
         ORDER BY tags.name"
    ))?;

    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        tags.entry(row.get(0)?).or_default().push(row.get(1)?);
    }
    Ok(tags)
}

/// The searchable projection of a session.
///
/// See the module documentation for what a query means at this granularity. The
/// duration is the footage the sitting produced rather than the wall-clock span
/// from its first frame to its last: a session left open across a dinner break
/// did not record the dinner, and `duration:>2h` should not select it.
fn row_of(session: &IndexedSession) -> Row {
    let mut row = Row::new().with_session(&session.session_id);

    if let Some(game) = &session.game_name {
        row = row.with_game(game);
    }
    if let Some(date) = day_of(&session.started_at) {
        row = row.with_date(date);
    }

    let seconds: f64 = session
        .recordings
        .iter()
        .filter_map(|recording| recording.duration_seconds)
        .chain(
            session
                .clips
                .iter()
                .filter_map(|clip| clip.duration_seconds),
        )
        .sum();
    if seconds > 0.0 {
        row = row.with_duration(TimeDuration::seconds_f64(seconds).unsigned_abs());
    }

    let mut favourite = session.favourite;
    for recording in &session.recordings {
        favourite |= recording.favourite;
        for tag in &recording.tags {
            row = row.with_tag(tag);
        }
    }
    for clip in &session.clips {
        favourite |= clip.favourite;
        if let Some(title) = &clip.title {
            row = row.with_title(title);
        }
        for tag in &clip.tags {
            row = row.with_tag(tag);
        }
    }

    row.favourite(favourite)
}

/// The day an RFC 3339 timestamp falls on, in the offset it carries.
///
/// The offset is the one the recorder wrote, which is the user's own: a session
/// recorded at 00:30 on the eleventh belongs to the eleventh on their calendar,
/// whatever UTC calls it (`crate::search::date`).
fn day_of(timestamp: &str) -> Option<Date> {
    OffsetDateTime::parse(timestamp, &Rfc3339)
        .ok()
        .map(OffsetDateTime::date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::test_support::scratch_directory;

    /// An empty library of this test's own.
    fn library(name: &str) -> Database {
        let directory = scratch_directory(name);
        Database::open(directory.join("library.db")).expect("a database opens")
    }

    /// Adds a session, on the given day of August 2026, and returns its
    /// identifier.
    fn add_session(database: &Database, day: u32, game: Option<&str>) -> String {
        let session_id = format!("{}-2026081{day}-120000", game.unwrap_or("none"));
        let started_at = format!("2026-08-1{day}T12:00:00+01:00");
        if let Some(game) = game {
            database
                .connection()
                .execute(
                    "INSERT OR IGNORE INTO games (game_id, name, first_seen_at) \
                     VALUES (?1, ?2, ?3)",
                    params![game, game, started_at],
                )
                .expect("a game inserts");
        }
        database
            .connection()
            .execute(
                "INSERT INTO sessions (session_id, game_id, started_at) VALUES (?1, ?2, ?3)",
                params![session_id, game, started_at],
            )
            .expect("a session inserts");
        session_id
    }

    fn add_recording(database: &Database, session_id: &str, index: i64, missing: bool) -> i64 {
        database
            .connection()
            .execute(
                "INSERT INTO recordings \
                    (session_id, session_index, path, started_at, duration_seconds, size_bytes, \
                     missing_since) \
                 VALUES (?1, ?2, ?3, ?4, 60.0, 1024, ?5)",
                params![
                    session_id,
                    index,
                    format!(r"D:\clips\{session_id}-{index}.mkv"),
                    "2026-08-11T12:00:00+01:00",
                    missing.then(|| "2026-08-12T09:00:00+01:00".to_owned()),
                ],
            )
            .expect("a recording inserts");
        database.connection().last_insert_rowid()
    }

    #[test]
    fn a_page_is_newest_first_and_the_cursor_continues_after_its_last_session() {
        let database = library("browse-paging");
        for day in 1..=5 {
            let session = add_session(&database, day, Some("cs2"));
            add_recording(&database, &session, 1, false);
        }

        let first = list_sessions(
            &database,
            &SessionListing {
                limit: 2,
                ..SessionListing::default()
            },
        )
        .expect("a page");
        assert_eq!(
            first
                .sessions
                .iter()
                .map(|session| session.started_at.as_str())
                .collect::<Vec<_>>(),
            ["2026-08-15T12:00:00+01:00", "2026-08-14T12:00:00+01:00"],
            "a library is drawn newest first"
        );

        let second = list_sessions(
            &database,
            &SessionListing {
                limit: 2,
                after: first.next.clone(),
                ..SessionListing::default()
            },
        )
        .expect("a second page");
        assert_eq!(
            second
                .sessions
                .iter()
                .map(|session| session.started_at.as_str())
                .collect::<Vec<_>>(),
            ["2026-08-13T12:00:00+01:00", "2026-08-12T12:00:00+01:00"],
            "the second page continues after the first rather than repeating it"
        );

        let last = list_sessions(
            &database,
            &SessionListing {
                limit: 2,
                after: second.next.clone(),
                ..SessionListing::default()
            },
        )
        .expect("a third page");
        assert_eq!(last.sessions.len(), 1);
        assert_eq!(
            last.next, None,
            "a cursor at the end of the library would cost a request to discover it was empty"
        );
    }

    #[test]
    fn a_recording_whose_file_has_gone_is_listed_and_says_when_it_went() {
        // AGENTS.md section 27: the screen has to be able to say "this file has
        // gone". Leaving the row out would make it indistinguishable from a
        // session that never recorded anything.
        let database = library("browse-missing");
        let session = add_session(&database, 1, Some("cs2"));
        add_recording(&database, &session, 1, true);

        let page = list_sessions(&database, &SessionListing::default()).expect("a page");
        let recording = &page.sessions[0].recordings[0];
        assert_eq!(
            recording.missing_since.as_deref(),
            Some("2026-08-12T09:00:00+01:00"),
        );
        assert_eq!(
            recording.size_bytes,
            Some(1024),
            "the size it had is kept, so a drive coming back needs no re-measurement"
        );
    }

    #[test]
    fn a_query_selects_sessions_by_what_the_sitting_is() {
        let database = library("browse-search");
        let counter_strike = add_session(&database, 1, Some("cs2"));
        add_recording(&database, &counter_strike, 1, false);
        let minecraft = add_session(&database, 2, Some("minecraft"));
        add_recording(&database, &minecraft, 1, false);

        let query: Query = "game:minecraft".parse().expect("a query parses");
        let page = list_sessions(
            &database,
            &SessionListing {
                query: Some(&query),
                ..SessionListing::default()
            },
        )
        .expect("a page");

        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].game_name.as_deref(), Some("minecraft"));
    }

    #[test]
    fn a_page_of_matches_is_a_full_page_rather_than_the_matches_in_a_page() {
        // The trap a naive implementation falls into: filtering after taking
        // `limit` rows from SQL, so a search whose matches are spread through
        // the library answers three rows and claims there are no more. The walk
        // has to keep reading until the page is full.
        let database = library("browse-search-paging");
        for day in 1..=6 {
            let game = if day % 2 == 0 { "cs2" } else { "minecraft" };
            let session = add_session(&database, day, Some(game));
            add_recording(&database, &session, 1, false);
        }

        let query: Query = "game:cs2".parse().expect("a query parses");
        let page = list_sessions(
            &database,
            &SessionListing {
                limit: 3,
                query: Some(&query),
                ..SessionListing::default()
            },
        )
        .expect("a page");

        assert_eq!(page.sessions.len(), 3, "every cs2 session, not every other");
        assert!(page
            .sessions
            .iter()
            .all(|session| session.game_name.as_deref() == Some("cs2")));
        assert_eq!(page.next, None);
    }

    #[test]
    fn an_empty_library_is_an_empty_page_rather_than_a_failure() {
        let database = library("browse-empty");
        let page = list_sessions(&database, &SessionListing::default()).expect("a page");
        assert_eq!(page, SessionPage::default());
    }

    #[test]
    fn a_cursor_that_cannot_be_read_starts_at_the_newest_session() {
        // A cursor is a string a window kept across a restart. Refusing to draw
        // a library because of one would be the least useful possible answer.
        let database = library("browse-bad-cursor");
        let session = add_session(&database, 1, Some("cs2"));
        add_recording(&database, &session, 1, false);

        let page = list_sessions(
            &database,
            &SessionListing {
                after: Some("not a cursor".to_owned()),
                ..SessionListing::default()
            },
        )
        .expect("a page");
        assert_eq!(page.sessions.len(), 1);
    }

    #[test]
    fn a_page_is_clamped_so_that_a_reply_can_always_be_sent() {
        assert_eq!(page_limit(0), DEFAULT_PAGE_LIMIT);
        assert_eq!(page_limit(10), 10);
        assert_eq!(page_limit(100_000), MAX_PAGE_LIMIT);
    }

    #[test]
    fn a_session_projects_the_tags_and_favourites_of_what_is_inside_it() {
        let database = library("browse-projection");
        let session = add_session(&database, 1, Some("cs2"));
        let recording = add_recording(&database, &session, 1, false);
        database
            .connection()
            .execute("INSERT INTO tags (name) VALUES ('clutch')", [])
            .expect("a tag inserts");
        database
            .connection()
            .execute(
                "INSERT INTO recording_tags (recording_id, tag_id) \
                 VALUES (?1, (SELECT tag_id FROM tags WHERE name = 'clutch'))",
                params![recording],
            )
            .expect("the tag is applied");
        database
            .connection()
            .execute(
                "UPDATE recordings SET favourited_at = '2026-08-12T09:00:00+01:00' \
                 WHERE recording_id = ?1",
                params![recording],
            )
            .expect("the recording is favourited");

        let page = list_sessions(&database, &SessionListing::default()).expect("a page");
        assert_eq!(page.sessions[0].recordings[0].tags, ["clutch"]);

        for text in [
            "tag:clutch",
            "favourite",
            "duration:>30s",
            "date:2026-08-11",
        ] {
            let query: Query = text.parse().expect("a query parses");
            let found = list_sessions(
                &database,
                &SessionListing {
                    query: Some(&query),
                    ..SessionListing::default()
                },
            )
            .expect("a page");
            assert_eq!(
                found.sessions.len(),
                1,
                "`{text}` should select the sitting that holds it"
            );
        }
    }
}
