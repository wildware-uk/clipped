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
//! # Search is compiled to SQL, and checked against the matcher
//!
//! A [`Query`](crate::search::Query) becomes a `WHERE` fragment
//! (`crate::search::sql`) and the database answers it. That was not the first
//! design: this used to build the [`Row`](crate::search::Row) each session
//! projects and ask [`Query::matches`](crate::search::Query::matches), which
//! meant four further statements per session — its recordings, its clips, their
//! tags and its events — for every session in the library until a page was full,
//! most of them discarded. `docs/library.md` measured 316 ms over ten thousand
//! sittings when nothing matched, which is the worst case and the one a search
//! box hits on every keystroke
//! ([issue #449](https://github.com/wildware-uk/clipped/issues/449)).
//!
//! The reason not to compile was that it is a second implementation of what a
//! match means, and two of those disagreeing is a bug nobody can see. That
//! reason has not gone away; it is answered rather than dismissed. The matcher
//! is still the definition, [`row_of`] still builds the row it is defined over,
//! and `the_database_and_the_matcher_select_the_same_sessions` runs both over
//! the same library and fails if they ever part company. The projection is not
//! dead code kept for a test — it is the specification the test holds the SQL
//! to.
//!
//! Folding is not duplicated either: the compiled SQL calls
//! [`fold`](crate::search::fold) itself, registered on the connection, rather
//! than SQLite's ASCII-only `lower()`.
//!
//! # What a query means for a *session*
//!
//! The language matches one searchable thing (`docs/search.md`), and the thing
//! being listed here is a sitting. So a session's row is built from what the
//! sitting is: its game, its identifier, the day it started, how much footage it
//! produced, whether it or anything in it is favourited, the tags on its
//! recordings and clips, the titles of **all** of those clips, and the kinds of
//! event its plugins reported. `game:cs2`, `tag:clutch`, `favourite`,
//! `date:>2026-08-01`, `duration:>30m`, `title:mirage` and `event:kill`
//! therefore all mean what a person would expect of a session list.
//!
//! The last two are recent. `title:` kept only the *last* clip's title, because
//! the projection assigned it rather than adding it, and `event:` matched
//! nothing at all, because nothing here read `session_events`
//! ([issue #520](https://github.com/wildware-uk/clipped/issues/520)). Both were
//! invisible from the language's side: the parser accepted the terms and the
//! matcher implemented them, and the row they were asked about was smaller than
//! either knew.

use std::collections::HashMap;

use crate::search::{sql, Query};
use clipped_storage::rusqlite::types::Value;
use clipped_storage::rusqlite::{params, params_from_iter, Connection, Row as SqlRow};
use clipped_storage::Database;

// The searchable projection is built only by the differential test now; see
// [`row_of`] for why it is kept.
#[cfg(test)]
use crate::search::{Date, Row};
#[cfg(test)]
use time::format_description::well_known::Rfc3339;
#[cfg(test)]
use time::{Duration as TimeDuration, OffsetDateTime};

use super::error::IndexError;

/// How many sessions a page holds when the caller does not say.
pub const DEFAULT_PAGE_LIMIT: usize = 50;

/// The most sessions a page will hold, whatever is asked for.
///
/// A bound on how much this module will *read* in one go — a caller that asks
/// for ten thousand is asking for a query that walks the library, and clamping
/// is a better answer than a refusal nobody could have predicted.
///
/// It is deliberately **not** the bound on how large the answer may be, because
/// a count of sessions cannot be one: a session holds any number of recordings
/// and clips, so two hundred sessions is anywhere between a hundred kilobytes
/// and several megabytes. Whatever carries these across a process boundary has
/// to bound its own payload in bytes, which is what
/// `apps/recorder/src/library.rs` does against `clipped_ipc::MAX_FRAME_BYTES`.
/// This crate knows nothing about frames.
pub const MAX_PAGE_LIMIT: usize = 200;

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
    /// Whether the user locked the sitting.
    ///
    /// A locked sitting protects every recording in it from automatic cleanup,
    /// which is what locking one means (`crate::locks`). The recordings do not
    /// carry a mark of their own for it, so a screen drawing a padlock against
    /// one has to read this as well as [`IndexedRecording::locked`].
    pub locked: bool,
    /// The files it recorded, in the order they were recorded.
    pub recordings: Vec<IndexedRecording>,
    /// The clips cut from it.
    ///
    /// One per replay saved out of one of its recordings
    /// ([issue #38](https://github.com/wildware-uk/clipped/issues/38)): the
    /// recorder writes them into the session's sidecar and
    /// [`super::ingest`] reads them out of it. Clips a *timeline* selection
    /// produces are still to come
    /// ([issue #91](https://github.com/wildware-uk/clipped/issues/91)), and a
    /// clip saved with no session behind it is not reachable from a session
    /// listing at all — it needs a listing of its own, with the screen that
    /// creates one.
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
    /// Whether the user locked this recording itself.
    ///
    /// Its own lock only. A recording inside a locked sitting is protected
    /// without carrying one, so this is the field a *control* is drawn from —
    /// there is nothing here to release — and
    /// [`IndexedSession::locked`] is the other half of what a padlock means.
    pub locked: bool,
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
    let connection = database.connection();
    let after = cursor_parts(listing.after.as_deref());

    // One row past the page, so that `next` is offered only when a further
    // session really exists: a cursor handed out at the end of the library would
    // cost the caller a request to discover the page was empty.
    let mut headers = session_headers(connection, after.as_ref(), listing.query, limit + 1)?;

    // The cursor names the last session *on* the page, not the one past it, or
    // the session that proved there was more would be the one the next page
    // skipped.
    let more = headers.len() > limit;
    headers.truncate(limit);

    let mut sessions = Vec::with_capacity(headers.len());
    for header in headers {
        sessions.push(hydrate(connection, header)?);
    }

    let next = more.then(|| sessions.last().map(cursor_of)).flatten();
    Ok(SessionPage { sessions, next })
}

/// The limit a listing actually gets.
fn page_limit(asked: usize) -> usize {
    match asked {
        0 => DEFAULT_PAGE_LIMIT,
        asked => asked.min(MAX_PAGE_LIMIT),
    }
}

/// The cursor that continues after this session.
///
/// Public because a caller may answer with fewer sessions than it was given —
/// the recorder truncates a page to what one protocol frame can carry
/// (`apps/recorder/src/library.rs`) — and the cursor it hands out then has to be
/// the one *this* module would read back. A second place formatting a cursor is
/// two definitions of where a page continues, and the two drifting would skip or
/// repeat sessions in a way nothing would notice.
#[must_use]
pub fn cursor_of(session: &IndexedSession) -> String {
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
    locked: bool,
}

/// The next `limit` session rows after the cursor that `query` selects, newest
/// first.
///
/// This is the whole of what a search costs now: the query is a predicate in
/// this statement rather than a filter applied to hydrated sessions, so the
/// sessions this reads are the ones being returned rather than every session in
/// the library.
fn session_headers(
    connection: &Connection,
    after: Option<&(String, String)>,
    query: Option<&Query>,
    limit: usize,
) -> Result<Vec<SessionHeader>, IndexError> {
    // The folding the compiled predicate calls. Registered before every search
    // rather than once at open: it is a map insert, and it makes it impossible
    // for a search to reach a connection that cannot fold — which would
    // otherwise fail a long way from its cause.
    let filter = match query {
        Some(query) => {
            sql::install(connection)?;
            // From 4: the cursor takes 1 and 2, the limit takes 3.
            sql::compile(query, 4)
        }
        None => sql::compile(&Query::everything(), 4),
    };

    // Row values — `(a, b) < (?, ?)` — are what make the cursor one comparison
    // rather than the three-way `a < ? OR (a = ? AND b < ?)`, and SQLite uses
    // the `sessions_started_at` index for it either way.
    let mut statement = connection.prepare(&format!(
        "SELECT s.session_id, s.game_id, catalogue.name, \
                s.started_at, s.ended_at, s.end_reason, s.favourited_at, s.locked_at \
         FROM sessions s LEFT JOIN games catalogue ON catalogue.game_id = s.game_id \
         WHERE ((?1 IS NULL) OR ((s.started_at, s.session_id) < (?1, ?2))) \
           AND ({}) \
         ORDER BY s.started_at DESC, s.session_id DESC \
         LIMIT ?3",
        filter.predicate
    ))?;

    let (at, id) = match after {
        Some((at, id)) => (Value::Text(at.clone()), Value::Text(id.clone())),
        None => (Value::Null, Value::Null),
    };
    let mut bound = vec![
        at,
        id,
        Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)),
    ];
    bound.extend(filter.params);

    let mut rows = statement.query(params_from_iter(bound))?;

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
        locked: row.get::<_, Option<String>>(7)?.is_some(),
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
        locked: header.locked,
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
                duration_seconds, width, height, size_bytes, missing_since, favourited_at, locked_at \
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
            locked: row.get::<_, Option<String>>(13)?.is_some(),
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

/// The distinct event kinds each of these sessions carries.
///
/// One statement for the whole batch, for the reason [`tags_for`] gives. The
/// kinds rather than the events: a search asks whether a session has an event
/// *of a kind*, so reading the moments and the details as well would be reading
/// a timeline to answer a yes-or-no question.
///
/// Only the differential test reads this now. Listing does not, because the
/// database applies the query without a row being built at all; this is half of
/// building the row the SQL is checked against.
#[cfg(test)]
fn event_kinds_of(
    connection: &Connection,
    headers: &[SessionHeader],
) -> Result<HashMap<String, Vec<String>>, IndexError> {
    let mut kinds: HashMap<String, Vec<String>> = HashMap::new();
    if headers.is_empty() {
        return Ok(kinds);
    }

    // Bound rather than written into the statement: a session identifier is a
    // string the recorder generated, and a string is never interpolated into
    // SQL here however safe this one looks (AGENTS.md section 30).
    let placeholders = (1..=headers.len())
        .map(|nth| format!("?{nth}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = connection.prepare(&format!(
        "SELECT DISTINCT session_id, kind FROM session_events          WHERE session_id IN ({placeholders}) ORDER BY kind"
    ))?;

    let ids: Vec<&str> = headers
        .iter()
        .map(|header| header.session_id.as_str())
        .collect();
    let mut rows = statement.query(params_from_iter(ids))?;
    while let Some(row) = rows.next()? {
        kinds.entry(row.get(0)?).or_default().push(row.get(1)?);
    }
    Ok(kinds)
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
///
/// **This is the definition of what a session is searchable by**, and the SQL in
/// `crate::search::sql` is checked against it rather than trusted beside it.
/// Listing no longer calls it — the database applies the query — so the only
/// caller is `the_database_and_the_matcher_select_the_same_sessions`. That is
/// deliberate: a specification with one consumer is still a specification, and
/// deleting it would leave the compiled SQL as the only statement of what a
/// match means, which is exactly what the module documentation says not to do.
#[cfg(test)]
fn row_of(session: &IndexedSession, events: &[String]) -> Row {
    let mut row = Row::new().with_session(&session.session_id);

    // What the plugins reported during the sitting. Not part of
    // `IndexedSession`, because a listing does not draw them — the timeline
    // does, out of `library_events` — but a session is searchable by them, and
    // for a while it was not: `event:kill` matched nothing at all, because
    // nothing put them here (issue #520).
    for kind in events {
        row = row.with_event(kind);
    }

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
/// whatever UTC calls it (`crate::search::date`). The compiled SQL takes the
/// same ten characters out of the same string, which is why the two agree about
/// a session recorded either side of midnight.
#[cfg(test)]
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

    /// A library of `sessions` sittings, newest last, each holding nothing.
    ///
    /// Rows in a real index rather than a limit handed straight to the private
    /// function that clamps one, because the property this exists for belongs to
    /// [`list_sessions`] and not to anything it happens to call: a test that
    /// never read a page could not tell "the page a caller receives is clamped"
    /// from "a function exists which would clamp one if it were called". What a
    /// sitting holds is beside the point here — the clamp counts sittings — so
    /// they hold nothing, which keeps two hundred of them cheap.
    fn library_of(name: &str, sessions: usize) -> Database {
        let database = library(name);
        let connection = database.connection();
        // One transaction for the lot. Two hundred commits would make this slow
        // enough that somebody stops running it.
        connection
            .execute_batch("BEGIN")
            .expect("a transaction begins");
        connection
            .execute(
                "INSERT INTO games (game_id, name, first_seen_at) \
                 VALUES ('cs2', 'cs2', '2026-08-11T00:00:00+01:00')",
                [],
            )
            .expect("a game inserts");
        for index in 0..sessions {
            connection
                .execute(
                    "INSERT INTO sessions (session_id, game_id, started_at) \
                     VALUES (?1, 'cs2', ?2)",
                    params![
                        session_id_of(index),
                        // Distinct per sitting, so the newest-first order this
                        // reasons about is total rather than a tie broken by
                        // the identifier.
                        format!("2026-08-11T{:02}:{:02}:00+01:00", index / 60, index % 60),
                    ],
                )
                .expect("a session inserts");
        }
        connection.execute_batch("COMMIT").expect("it commits");
        database
    }

    /// The identifier of the sitting [`library_of`] inserted `index`th.
    fn session_id_of(index: usize) -> String {
        format!("cs2-{index:04}")
    }

    #[test]
    fn a_page_is_clamped_so_that_a_reply_can_always_be_sent() {
        // Read through `list_sessions`, which is what a caller calls: a clamp is
        // only worth anything on the path an actual request takes. Whatever
        // carries a page across a process boundary has to bound its own payload
        // in bytes as well (`apps/recorder/src/library.rs`), but it can only do
        // that to a page it was able to receive in the first place — an
        // unclamped ten thousand is a query that walks the library.
        const SESSIONS: usize = MAX_PAGE_LIMIT + 5;
        let database = library_of("browse-clamp", SESSIONS);

        let unasked = list_sessions(&database, &SessionListing::default()).expect("a page");
        assert_eq!(
            unasked.sessions.len(),
            DEFAULT_PAGE_LIMIT,
            "a caller that does not say gets the default rather than the library"
        );

        let asked = list_sessions(
            &database,
            &SessionListing {
                limit: 10,
                ..SessionListing::default()
            },
        )
        .expect("a page");
        assert_eq!(
            asked.sessions.len(),
            10,
            "a limit inside the bound is answered exactly"
        );

        let greedy = list_sessions(
            &database,
            &SessionListing {
                limit: 100_000,
                ..SessionListing::default()
            },
        )
        .expect("a page");
        assert_eq!(
            greedy.sessions.len(),
            MAX_PAGE_LIMIT,
            "a caller asking for the whole library gets a page of it"
        );
        // Clamping is not truncation: the five sittings it did not carry are
        // still reachable. Asked for rather than compared to a string, because
        // what the next request does with a cursor is the whole of what a cursor
        // is for.
        let rest = list_sessions(
            &database,
            &SessionListing {
                limit: 100_000,
                after: greedy.next.clone(),
                ..SessionListing::default()
            },
        )
        .expect("a page");
        // The oldest `SESSIONS - MAX_PAGE_LIMIT` sittings are the ones left out,
        // and a listing is newest first.
        let left_out = (0..SESSIONS - MAX_PAGE_LIMIT)
            .rev()
            .map(session_id_of)
            .collect::<Vec<_>>();
        assert_eq!(
            rest.sessions
                .iter()
                .map(|session| session.session_id.clone())
                .collect::<Vec<_>>(),
            left_out,
            "the page after a clamped one carries exactly the sittings it left out"
        );
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

    /// Every session a query selects, by identifier.
    fn found(database: &Database, text: &str) -> Vec<String> {
        let query: Query = text.parse().expect("a query parses");
        list_sessions(
            database,
            &SessionListing {
                query: Some(&query),
                ..SessionListing::default()
            },
        )
        .expect("a page")
        .sessions
        .into_iter()
        .map(|session| session.session_id)
        .collect()
    }

    #[test]
    fn a_sitting_is_found_by_the_name_of_any_of_its_clips_not_only_the_last() {
        // A sitting produces one clip per replay saved out of it, and somebody
        // who names two of them expects to find either. The projection assigned
        // the title rather than adding it, so the first name was overwritten by
        // the second and could not be searched for at all — by `title:` or by
        // typing it (issue #520).
        let database = library("clip-titles");
        let session = add_session(&database, 1, Some("cs2"));
        for (index, title) in [(1, "Ace on Mirage"), (2, "Clutch on Inferno")] {
            database
                .connection()
                .execute(
                    "INSERT INTO clips (session_id, path, title, created_at, duration_seconds)                      VALUES (?1, ?2, ?3, ?4, 30.0)",
                    params![
                        session,
                        format!(r"D:\clips\{session}-clip{index}.mkv"),
                        title,
                        "2026-08-11T12:30:00+01:00",
                    ],
                )
                .expect("a clip inserts");
        }

        for text in ["title:mirage", "mirage", "title:inferno", "inferno"] {
            assert_eq!(
                found(&database, text),
                vec![session.clone()],
                "`{text}` names a clip of this sitting"
            );
        }
        assert!(found(&database, "title:dust2").is_empty());
    }

    #[test]
    fn a_sitting_is_found_by_an_event_its_plugins_reported() {
        // `event:` is in the language, the matcher implements it, and
        // `session_events` has rows in it — and nothing joined the two, so
        // every `event:` term was false for every session (issue #520).
        let database = library("session-events");
        let session = add_session(&database, 2, Some("cs2"));
        for kind in ["kill", "round_win"] {
            database
                .connection()
                .execute(
                    "INSERT INTO session_events (session_id, at, kind) VALUES (?1, ?2, ?3)",
                    params![session, "2026-08-12T12:10:00+01:00", kind],
                )
                .expect("an event inserts");
        }

        for text in ["event:kill", "kill", "event:round_win", "round_win"] {
            assert_eq!(
                found(&database, text),
                vec![session.clone()],
                "`{text}` names an event of this sitting"
            );
        }
        assert!(found(&database, "event:death").is_empty());
    }

    /// Every session the **matcher** selects, by identifier, newest first.
    ///
    /// This is what `list_sessions` used to do, written out: hydrate each
    /// session, build the row it projects, and ask the query. It is the
    /// reference answer that the compiled SQL is checked against.
    fn matched(database: &Database, text: &str) -> Vec<String> {
        let query: Query = text.parse().expect("a query parses");
        let connection = database.connection();
        let headers =
            session_headers(connection, None, None, 10_000).expect("every session is readable");
        let kinds = event_kinds_of(connection, &headers).expect("the events are readable");

        let mut selected = Vec::new();
        for header in headers {
            let events = kinds.get(&header.session_id).cloned().unwrap_or_default();
            let session = hydrate(connection, header).expect("a session hydrates");
            if query.matches(&row_of(&session, &events)) {
                selected.push(session.session_id);
            }
        }
        selected
    }

    /// A library with one of everything a query can ask about, and several
    /// things it should *not* find.
    fn varied_library(name: &str) -> Database {
        let database = library(name);
        let connection = database.connection();

        // Cyrillic, so that a folding that only handles ASCII fails here rather
        // than in a user's library.
        // The last two are on the far side of 2026-08-20, which is what makes
        // the documented `date:>=` query select anything here.
        let sessions = [
            (11, Some(("cs2", "Counter-Strike 2"))),
            (12, Some(("tanks", "Мир танков"))),
            (13, Some(("elden", "Elden Ring"))),
            (14, None),
            (15, Some(("cs2", "Counter-Strike 2"))),
            (21, Some(("minecraft", "Minecraft"))),
            (22, Some(("elden", "Elden Ring"))),
        ];
        let mut ids = Vec::new();
        for (day, game) in sessions {
            let started_at = format!("2026-08-{day}T12:00:00+01:00");
            if let Some((id, name)) = game {
                connection
                    .execute(
                        "INSERT OR IGNORE INTO games (game_id, name, first_seen_at) \
                         VALUES (?1, ?2, ?3)",
                        params![id, name, started_at],
                    )
                    .expect("a game inserts");
            }
            let session_id = format!("session-{day}");
            connection
                .execute(
                    "INSERT INTO sessions (session_id, game_id, started_at) VALUES (?1, ?2, ?3)",
                    params![session_id, game.map(|(id, _)| id), started_at],
                )
                .expect("a session inserts");
            ids.push(session_id);
        }

        // A recording each, except the fourth, which recorded nothing: it is
        // what `duration:` has to be absent for rather than zero. The lengths
        // straddle five minutes, so `duration:>5m` divides the library rather
        // than selecting all of it or none.
        for (index, seconds) in [60.0, 120.0, 180.0, 0.0, 300.0, 400.0, 600.0]
            .into_iter()
            .enumerate()
        {
            if seconds == 0.0 {
                continue;
            }
            let session = &ids[index];
            connection
                .execute(
                    "INSERT INTO recordings \
                        (session_id, session_index, path, started_at, duration_seconds) \
                     VALUES (?1, 1, ?2, ?3, ?4)",
                    params![
                        session,
                        format!(r"D:\clips\{session}.mkv"),
                        "2026-08-11T12:00:00+01:00",
                        seconds,
                    ],
                )
                .expect("a recording inserts");
        }

        // A deleted recording on the first sitting, carrying a duration, a
        // favourite and a tag none of which may be found: `deleted_at` is the
        // trash, and the trash is not the library.
        connection
            .execute(
                "INSERT INTO recordings \
                    (session_id, session_index, path, started_at, duration_seconds, \
                     favourited_at, deleted_at) \
                 VALUES (?1, 9, ?2, ?3, 9999.0, ?3, ?3)",
                params![ids[0], r"D:\clips\deleted.mkv", "2026-08-11T12:00:00+01:00"],
            )
            .expect("a deleted recording inserts");

        // The fourth has no title at all, which is what makes `-title:` a real
        // question: the matcher selects a sitting whose clip has no title, and
        // SQL would only agree if the comparison is inside an `EXISTS`.
        for (name, session, title, deleted) in [
            ("Ace on Mirage", 0, true, false),
            ("Clutch on Inferno", 0, true, false),
            ("Тащу катку", 1, true, false),
            ("Deleted highlight", 2, true, true),
            ("Untitled", 4, false, false),
        ] {
            connection
                .execute(
                    "INSERT INTO clips (session_id, path, title, created_at, duration_seconds, \
                                        deleted_at) \
                     VALUES (?1, ?2, ?3, ?4, 30.0, ?5)",
                    params![
                        ids[session],
                        format!(r"D:\clips\{name}.mkv"),
                        title.then_some(name),
                        "2026-08-11T12:30:00+01:00",
                        deleted.then_some("2026-08-13T00:00:00+01:00"),
                    ],
                )
                .expect("a clip inserts");
        }

        // Tags on both sides of the join, including one only a deleted clip
        // carries.
        for tag in ["clutch", "ЛУЧШЕЕ", "spoiler"] {
            connection
                .execute("INSERT INTO tags (name) VALUES (?1)", params![tag])
                .expect("a tag inserts");
        }
        connection
            .execute(
                "INSERT INTO recording_tags (recording_id, tag_id) \
                 SELECT r.recording_id, t.tag_id FROM recordings r, tags t \
                 WHERE r.session_id = ?1 AND r.deleted_at IS NULL AND t.name = 'clutch'",
                params![ids[1]],
            )
            .expect("a recording tag applies");
        connection
            .execute(
                "INSERT INTO clip_tags (clip_id, tag_id) \
                 SELECT c.clip_id, t.tag_id FROM clips c, tags t \
                 WHERE c.title = 'Ace on Mirage' AND t.name = 'ЛУЧШЕЕ'",
                [],
            )
            .expect("a clip tag applies");
        connection
            .execute(
                "INSERT INTO clip_tags (clip_id, tag_id) \
                 SELECT c.clip_id, t.tag_id FROM clips c, tags t \
                 WHERE c.title = 'Deleted highlight' AND t.name = 'spoiler'",
                [],
            )
            .expect("a tag on a deleted clip applies");

        // A favourite at each of the three levels the projection reads: the
        // sitting itself, a recording in it, and a clip cut from it.
        connection
            .execute(
                "UPDATE sessions SET favourited_at = ?2 WHERE session_id = ?1",
                params![ids[2], "2026-08-13T09:00:00+01:00"],
            )
            .expect("a session is favourited");
        connection
            .execute(
                "UPDATE recordings SET favourited_at = ?2 \
                 WHERE session_id = ?1 AND deleted_at IS NULL",
                params![ids[0], "2026-08-13T09:00:00+01:00"],
            )
            .expect("a recording is favourited");
        connection
            .execute(
                "UPDATE clips SET favourited_at = ?1 WHERE title = 'Тащу катку'",
                params!["2026-08-13T09:00:00+01:00"],
            )
            .expect("a clip is favourited");

        for (session, kind) in [
            (0, "kill"),
            (0, "round_win"),
            (1, "УБИЙСТВО"),
            (5, "kill"),
            (6, "kill"),
        ] {
            connection
                .execute(
                    "INSERT INTO session_events (session_id, at, kind) VALUES (?1, ?2, ?3)",
                    params![ids[session], "2026-08-11T12:10:00+01:00", kind],
                )
                .expect("an event inserts");
        }

        database
    }

    /// The queries the two are compared over.
    ///
    /// Every field, every comparison, both connectives, negation, brackets,
    /// non-ASCII text on both sides, and the three states that are null in the
    /// database — no game, no recordings, no clip title.
    const CASES: &[&str] = &[
        "",
        "mirage",
        "MIRAGE",
        "тащу",
        "ТАЩУ",
        "танков",
        "game:counter",
        "game:мир",
        "session:15",
        "title:inferno",
        "title:deleted",
        "tag:clutch",
        "tag:лучшее",
        "tag:spoiler",
        "event:kill",
        "event:убийство",
        "favourite",
        "favourite:false",
        "-favourite",
        "-title:mirage",
        "-tag:clutch",
        "-event:kill",
        "-session:15",
        "date:2026-08-11",
        "date:>2026-08-12",
        "date:<=2026-08-12",
        "-date:>2026-08-12",
        "duration:>60s",
        "duration:<1h",
        "-duration:<1h",
        "duration:>=90s",
        "game:cs OR tag:clutch",
        "game:counter kill",
        "(tag:clutch OR event:kill) -game:elden",
        "-game:counter -game:elden",
        "nothing-in-this-library",
    ];

    /// The queries `docs/search.md` measures the matcher over, verbatim.
    ///
    /// Issue #449 asks for these specifically — "the results are identical to
    /// the in-memory executor's for every query in `docs/search.md`'s table,
    /// including the accent-folding cases" — so they are named here rather than
    /// paraphrased into [`CASES`], and the fixture above is shaped so that each
    /// of them selects something.
    const DOCUMENTED: &[&str] = &[
        "",
        "mirage",
        "game:counter kill favourite",
        "тан",
        r#"game:"Elden Ring" duration:>5m -favourite"#,
        "date:>=2026-08-20 (tag:clutch OR event:kill) -game:minecraft",
    ];

    /// Asserts the two agree about `queries`, and that they agreed about
    /// something.
    fn agree_about(database: &Database, queries: &[&str], allowed_empty: usize) {
        let mut selected_something = 0_usize;
        for text in queries {
            let expected = matched(database, text);
            assert_eq!(
                found(database, text),
                expected,
                "`{text}` is answered differently by the database and by the matcher"
            );
            if !expected.is_empty() {
                selected_something += 1;
            }
        }

        // A differential test where both sides return nothing agrees about
        // nothing. Most of these have to actually select sittings for the
        // comparison above to have been a comparison at all.
        assert!(
            selected_something + allowed_empty >= queries.len(),
            "only {selected_something} of {} queries selected anything; the fixture is not \
             exercising the language",
            queries.len()
        );
    }

    #[test]
    fn the_database_and_the_matcher_select_the_same_sessions() {
        // The whole correctness argument for answering a search in SQL. The
        // matcher is the definition of the language; this asserts the compiler
        // is a translation of it and not a second opinion.
        //
        // Four may select nothing: `tag:spoiler` and `title:deleted` name what
        // is in the trash, `-duration:<1h` names a sitting longer than an hour,
        // and `nothing-in-this-library` says so.
        agree_about(&varied_library("search-differential"), CASES, 4);
    }

    #[test]
    fn the_two_agree_about_every_query_the_language_is_documented_by() {
        // Note that `тан` does *not* exercise the folding, here or in the
        // documented table: `Мир танков` is already lower-case where the needle
        // matches it, so SQLite's ASCII `lower()` would answer this one
        // correctly too. The queries that actually divide the two are in
        // [`CASES`] — `ТАЩУ`, `game:мир`, `tag:лучшее`, `event:убийство` — each
        // of which asks about text whose case the fold has to change.
        agree_about(&varied_library("search-documented"), DOCUMENTED, 0);
    }

    #[test]
    fn what_is_in_the_trash_is_not_found_by_what_it_carries() {
        // The one place the compiled SQL could plausibly disagree with the
        // projection without any query looking odd: `browse` reads recordings
        // and clips with `deleted_at IS NULL`, so a deleted row's title, tag,
        // duration and favourite are not the sitting's.
        let database = varied_library("search-trash");

        assert!(
            found(&database, "title:deleted").is_empty(),
            "a deleted clip's title is not the sitting's"
        );
        assert!(
            found(&database, "tag:spoiler").is_empty(),
            "a tag only a deleted clip carries is not the sitting's"
        );
        assert!(
            found(&database, "duration:>2h").is_empty(),
            "the 9999 seconds on the deleted recording are not counted"
        );
        // And the sitting that holds them is still findable by what it does
        // have, so the assertions above are not passing because the fixture is
        // empty.
        assert_eq!(found(&database, "title:mirage"), vec!["session-11"]);
    }

    #[test]
    fn a_sitting_that_recorded_nothing_has_no_duration_rather_than_a_duration_of_zero() {
        // `IFNULL(SUM(...), 0)` makes the sum a number; the `> 0` guard beside
        // it is what stops that number being an answer. Without the guard this
        // sitting would be selected by every `duration:<` in the language.
        let database = varied_library("search-no-duration");

        let short = found(&database, "duration:<1h");
        assert!(
            !short.contains(&"session-14".to_owned()),
            "a sitting with no recordings has no duration: {short:?}"
        );
        assert!(!short.is_empty(), "other sittings are shorter than an hour");
    }
}
