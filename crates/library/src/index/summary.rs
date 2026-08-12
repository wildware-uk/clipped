//! What the library holds, per game.
//!
//! SPEC.md section 17's games view is four numbers under a name — sessions,
//! clips, favourites, size — and this is where they come from. They are counted
//! by SQLite rather than by walking rows in Rust, because the whole reason the
//! index exists is that a directory of JSON files cannot answer "what is using
//! 40 GB?" without reading all of it (`docs/storage.md`).
//!
//! # What counts, and what does not
//!
//! - **A missing file contributes nothing to the size** and is counted
//!   separately, because the space it used is not being used. Its row keeps the
//!   size it had when it was last seen, so a drive coming back needs no
//!   re-measurement — but a library that reported 83 GB of files nobody can
//!   find would be a lie.
//! - **Anything in the trash contributes nothing.** It is deleted as far as the
//!   library is concerned, and the trash's own screen is what accounts for it
//!   (#94).
//! - **Unattributed sessions get a row of their own**, with no identifier and no
//!   name. The catalogue refused to guess which game they were
//!   (`docs/sessions.md`) and neither does this; what to call that group on
//!   screen is a decision for the screen.

use std::collections::HashMap;

use clipped_storage::Database;

use super::error::IndexError;

/// What the library holds for one game.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameSummary {
    /// The catalogue's identifier, or `None` for sessions no game was
    /// attributed to.
    pub game_id: Option<String>,
    /// The name as the catalogue knew it when the game was last played, or
    /// `None` for unattributed sessions.
    pub name: Option<String>,
    /// When the first session of this game was recorded.
    pub first_seen_at: Option<String>,
    /// When the most recent one was.
    pub last_played_at: Option<String>,
    /// Sessions recorded.
    pub sessions: u64,
    /// Recordings that are not in the trash.
    pub recordings: u64,
    /// Clips that are not in the trash.
    pub clips: u64,
    /// Sessions, recordings and clips the user has favourited (SPEC.md
    /// section 29).
    pub favourites: u64,
    /// What the files that are still there occupy, in bytes.
    pub bytes: u64,
    /// Recordings and clips whose file could not be found when the library was
    /// last reconciled.
    pub missing: u64,
}

/// Every game in the library, with what it holds.
///
/// Sorted by identifier, with the unattributed sessions last, so that two calls
/// answer in the same order and a screen that does not sort still looks stable.
///
/// # Errors
///
/// [`IndexError::Database`] if the database refuses. This reads and writes
/// nothing, so it is safe to call on a read-only connection from the desktop
/// process while the recorder writes.
pub fn game_summaries(database: &Database) -> Result<Vec<GameSummary>, IndexError> {
    let connection = database.connection();
    let mut summaries: HashMap<Option<String>, GameSummary> = HashMap::new();

    let mut games =
        connection.prepare("SELECT game_id, name, first_seen_at, last_played_at FROM games")?;
    let mut rows = games.query([])?;
    while let Some(row) = rows.next()? {
        let game_id: String = row.get(0)?;
        summaries.insert(
            Some(game_id.clone()),
            GameSummary {
                game_id: Some(game_id),
                name: Some(row.get(1)?),
                first_seen_at: Some(row.get(2)?),
                last_played_at: row.get(3)?,
                ..GameSummary::default()
            },
        );
    }

    let mut sessions = connection.prepare(
        "SELECT game_id, COUNT(*), SUM(favourited_at IS NOT NULL) FROM sessions GROUP BY game_id",
    )?;
    let mut rows = sessions.query([])?;
    while let Some(row) = rows.next()? {
        let summary = entry(&mut summaries, row.get(0)?);
        summary.sessions = row.get::<_, i64>(1)?.max(0).unsigned_abs();
        summary.favourites += row.get::<_, i64>(2)?.max(0).unsigned_abs();
    }

    // The two file-holding tables are counted the same way and differ only in
    // their name, so the query is written once. A row in the trash is left out
    // of every figure; a row whose file is missing is counted, but the space it
    // is not occupying is not.
    for (table, join) in [
        (
            "recordings",
            "JOIN sessions ON sessions.session_id = recordings.session_id",
        ),
        (
            "clips",
            "LEFT JOIN sessions ON sessions.session_id = clips.session_id",
        ),
    ] {
        let mut statement = connection.prepare(&format!(
            "SELECT sessions.game_id, COUNT(*), \
                SUM({table}.missing_since IS NOT NULL), \
                SUM(CASE WHEN {table}.missing_since IS NULL \
                    THEN COALESCE({table}.size_bytes, 0) ELSE 0 END), \
                SUM({table}.favourited_at IS NOT NULL) \
             FROM {table} {join} \
             WHERE {table}.deleted_at IS NULL \
             GROUP BY sessions.game_id"
        ))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let summary = entry(&mut summaries, row.get(0)?);
            let count = row.get::<_, i64>(1)?.max(0).unsigned_abs();
            if table == "recordings" {
                summary.recordings = count;
            } else {
                summary.clips = count;
            }
            summary.missing += row.get::<_, i64>(2)?.max(0).unsigned_abs();
            summary.bytes += row.get::<_, i64>(3)?.max(0).unsigned_abs();
            summary.favourites += row.get::<_, i64>(4)?.max(0).unsigned_abs();
        }
    }

    let mut summaries: Vec<GameSummary> = summaries.into_values().collect();
    summaries.sort_by(|left, right| match (&left.game_id, &right.game_id) {
        (Some(left), Some(right)) => left.cmp(right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    Ok(summaries)
}

/// The summary for a game, created empty if this is the first row seen for it.
///
/// A game with rows but no `games` entry cannot happen through ingestion — the
/// schema's foreign keys forbid it — but the unattributed group has no entry by
/// design, and this is what gives it one.
fn entry(
    summaries: &mut HashMap<Option<String>, GameSummary>,
    game_id: Option<String>,
) -> &mut GameSummary {
    summaries
        .entry(game_id.clone())
        .or_insert_with(|| GameSummary {
            game_id,
            ..GameSummary::default()
        })
}
