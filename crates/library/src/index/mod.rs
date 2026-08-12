//! Reconciling the library index against what is on disk.
//!
//! The index is **derived**. The recordings are ordinary files, the session
//! sidecar the recorder writes beside them is the authority for what they are
//! (`docs/sessions.md`), and the SQLite database is a fast answer to questions
//! a directory of JSON files cannot answer without reading all of it
//! (`docs/storage.md`). This module is what keeps the second in step with the
//! first, and it can rebuild the whole of it from nothing but the files.
//!
//! [docs/library.md] is the prose: what a run does, what it costs, and every
//! decision below with the reasoning behind it.
//!
//! # Reconciliation, not import
//!
//! Users move, rename and delete files behind the application's back. A run
//! therefore never assumes the database and the disk agree:
//!
//! | What it finds | What it does |
//! | --- | --- |
//! | A sidecar the index does not have | Indexes the session and its recordings |
//! | A sidecar the index already has | Updates it; a re-run changes nothing |
//! | A row whose file has gone | Marks it `missing_since`, and **never deletes it** |
//! | A row whose file has come back | Clears the mark and re-measures the file |
//! | A row under a root that was not walked | Leaves it alone; it was not looked at |
//! | A media file no session claims | Reports it; invents no row for it |
//!
//! The one rule underneath all of that is AGENTS.md section 56: **nothing here
//! deletes a file or a row.** A recording that has gone may be on a drive that
//! is about to be plugged back in, and an index that tidied itself up would
//! have thrown away the favourite, the tags and the bookmarks that were on it.
//!
//! # Where it runs, and what stops it competing with a recording
//!
//! On a thread the caller owns, in the process that holds the database's
//! writing connection, and **never on a capture, encoder or UI thread**
//! (AGENTS.md section 20). [`reconcile`] is a synchronous function with no
//! threads of its own so that the caller decides where the work happens and
//! when it stops.
//!
//! Three properties are what keep it out of a recording's way:
//!
//! - **Every transaction is short and bounded.** Sessions are written
//!   [`IndexPace::batch`] at a time and rows are reconciled
//!   [`IndexPace::page`] at a time, each in a transaction of its own.
//!   The database's writer is never held for the length of a run, so a recorder
//!   with something to write waits for one batch at most.
//! - **No file is touched while a transaction is open.** Reading a sidecar and
//!   looking at the files it names happen before the transaction is opened, so
//!   the write lock is never held waiting on a disk.
//! - **It rests between batches.** [`IndexPace::rest`] is the pause that gives a
//!   drive and a core back to whatever else is running;
//!   [`IndexPace::background`] is the default and assumes a game may be
//!   recording.
//!
//! Readers are never blocked by any of it: the database is in write-ahead
//! logging mode, so a library screen reads a consistent snapshot while a
//! reconciliation writes (`docs/storage.md`).
//!
//! A run can be stopped at any point with [`IndexControl::cancel`], and what it
//! had already committed stays committed — the next run carries on rather than
//! starting again.
//!
//! [docs/library.md]: https://github.com/wildware-uk/clipped/blob/main/docs/library.md

mod browse;
mod error;
mod ingest;
/// Comparing and writing the moments the schema stores.
///
/// Visible to the rest of the crate rather than to this module alone: the trash
/// writes `deleted_at` and reads it back to judge retention, and a second RFC
/// 3339 writer beside this one would be two places for one format to drift
/// (AGENTS.md section 55).
pub(crate) mod moment;
mod presence;
mod scan;
mod sidecar;
mod summary;

#[cfg(test)]
pub(crate) mod test_support;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use clipped_storage::rusqlite::params;
use clipped_storage::Database;
use tracing::{debug, info, warn};

pub use browse::{
    cursor_of, list_sessions, IndexedClip, IndexedRecording, IndexedSession, SessionListing,
    SessionPage, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT,
};
pub use error::{IndexError, IndexProblem};
pub use scan::UnavailableRoot;
pub use summary::{game_summaries, GameSummary};

/// How many problems a run writes to the log before it stops repeating itself.
///
/// A library with a thousand unreadable sidecars is one fault, not a thousand
/// log entries, and the whole list is in the report either way (AGENTS.md
/// section 35).
const PROBLEMS_LOGGED: usize = 10;

/// How much of a run is spent working, and how much of it is spent out of the
/// way.
///
/// The numbers bound the two things a reconciliation can take from a recording:
/// the database's single writer, and the disk. A batch is a transaction, so
/// `batch` and `page` decide how long anything else waits to write; `rest` is
/// what it gives back between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexPace {
    /// Sessions written per transaction.
    pub batch: usize,
    /// Rows reconciled against the filesystem per transaction.
    pub page: usize,
    /// How long to wait between transactions.
    pub rest: Duration,
}

impl IndexPace {
    /// The pace to index at when a game may be recording.
    ///
    /// Small batches and a real pause between them. This is the default, and
    /// the assumption behind it is the safe one: the recorder runs
    /// unattended, so a run started because a session just finished cannot know
    /// that another game is not about to start.
    #[must_use]
    pub const fn background() -> Self {
        Self {
            batch: 16,
            page: 512,
            rest: Duration::from_millis(25),
        }
    }

    /// The pace to index at when the user is waiting for the result.
    ///
    /// Larger batches and no pause, for the case where somebody has asked for a
    /// rescan and is watching a progress indicator. It is not the default
    /// because "the user is watching" is something only the caller knows.
    #[must_use]
    pub const fn foreground() -> Self {
        Self {
            batch: 128,
            page: 4096,
            rest: Duration::ZERO,
        }
    }
}

impl Default for IndexPace {
    fn default() -> Self {
        Self::background()
    }
}

/// What to reconcile, and how hard to work at it.
#[derive(Debug, Clone)]
pub struct IndexSettings {
    /// The directories recordings are kept in.
    ///
    /// `%USERPROFILE%\Videos\Clipped` unless the recorder was told otherwise
    /// (`docs/recorder-cli.md`). A root that cannot be reached is reported and
    /// nothing under it is judged.
    pub roots: Vec<PathBuf>,
    /// How many directories deep to look below each root.
    ///
    /// The recorder writes into the root itself, so this is entirely for
    /// libraries a person has organised by hand. Bounded because a directory
    /// tree can refer to itself.
    pub max_depth: usize,
    /// How many unindexed media files to name in the report.
    ///
    /// The count is exact whatever this is; the sample is what stops a folder
    /// of ten thousand holiday videos becoming a ten thousand element vector
    /// nobody reads.
    pub sample_limit: usize,
    /// How much of the machine to use.
    pub pace: IndexPace,
}

impl IndexSettings {
    /// Settings for the given recording folders, at the background pace.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
            max_depth: 8,
            sample_limit: 32,
            pace: IndexPace::background(),
        }
    }
}

/// The handle that stops a run.
///
/// Cloneable and shared: the thread doing the work holds one, and whatever may
/// need to stop it — a window closing, the recorder shutting down — holds
/// another. Cancellation is cooperative and is checked between files and
/// between transactions, so a cancelled run stops promptly and leaves the
/// database consistent rather than half-written.
#[derive(Debug, Clone, Default)]
pub struct IndexControl {
    cancelled: Arc<AtomicBool>,
}

impl IndexControl {
    /// A control that has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the run to stop as soon as it can.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether the run has been asked to stop.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// What one reconciliation did.
///
/// Everything here is counted as it happens rather than queried afterwards, so
/// the numbers describe this run and not the state of the library. Nothing in
/// it is an estimate.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct IndexReport {
    /// How long the run took, walk included.
    pub duration: Duration,
    /// Session sidecars found on disk.
    pub sidecars_found: usize,
    /// Sessions written to the index.
    pub sessions_indexed: usize,
    /// Recordings written to the index.
    pub recordings_indexed: usize,
    /// Recordings found to have gone since the last run.
    pub recordings_newly_missing: usize,
    /// Recordings whose file has come back.
    pub recordings_returned: usize,
    /// Clips found to have gone since the last run.
    pub clips_newly_missing: usize,
    /// Clips whose file has come back.
    pub clips_returned: usize,
    /// Media files under the roots that no session claims.
    pub unindexed_media: usize,
    /// The first few of them, for a message a person can act on.
    pub unindexed_sample: Vec<PathBuf>,
    /// Roots that could not be reached. Nothing under one is judged.
    pub unavailable_roots: Vec<UnavailableRoot>,
    /// Everything the run could not use, and why.
    pub problems: Vec<IndexProblem>,
    /// Transactions committed.
    pub transactions: usize,
    /// The longest any one of them was open.
    ///
    /// This is the figure that says whether indexing can compete with a
    /// recording: it is how long anything else with a row to write could have
    /// been kept waiting.
    pub longest_transaction: Duration,
    /// Whether the run was cancelled before it finished.
    pub cancelled: bool,
}

impl fmt::Display for IndexReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} session(s) and {} recording(s) indexed in {:.2?}; {} missing, {} returned, \
             {} unindexed file(s), {} problem(s)",
            self.sessions_indexed,
            self.recordings_indexed,
            self.duration,
            self.recordings_newly_missing,
            self.recordings_returned,
            self.unindexed_media,
            self.problems.len(),
        )
    }
}

/// Reconciles the index in `database` against the files under
/// `settings.roots`.
///
/// `observed_at` is the moment the run is stamped with — what a file found to
/// have gone is marked missing since. Callers pass [`SystemTime::now`]; tests
/// pass a fixed reading, so that what is written does not depend on when the
/// test ran (AGENTS.md section 25).
///
/// # Errors
///
/// [`IndexError::Database`] if the database refuses, which is the only failure
/// that ends a run. Everything else — an unreadable file, a folder that will
/// not open, a session from a newer build — is an [`IndexProblem`] in the
/// report, and the rest of the library is still indexed.
///
/// Whatever had been committed before a failure stays committed: each batch is
/// its own transaction, so a run that ends early leaves a smaller index rather
/// than a broken one, and the next run continues from there.
pub fn reconcile(
    database: &mut Database,
    settings: &IndexSettings,
    control: &IndexControl,
    observed_at: SystemTime,
) -> Result<IndexReport, IndexError> {
    let started = Instant::now();
    let stamp = moment::rfc3339(observed_at);
    let mut report = IndexReport::default();

    let mut walked = scan::walk(&settings.roots, settings.max_depth, control);
    report.sidecars_found = walked.sidecars.len();
    report.cancelled = walked.cancelled;
    for unreadable in std::mem::take(&mut walked.unreadable_directories) {
        report.problems.push(IndexProblem::UnreadableDirectory {
            path: unreadable.path,
            error: unreadable.error,
        });
    }
    report.unavailable_roots = std::mem::take(&mut walked.unavailable_roots);

    let indexed = index_sessions(
        database,
        &walked.sidecars,
        settings,
        control,
        &stamp,
        &mut report,
    )?;

    if !report.cancelled {
        reconcile_rows(
            database,
            &walked,
            settings,
            control,
            &stamp,
            &indexed,
            &mut report,
        )?;
        report_unindexed_media(database, &walked.media, settings, &mut report)?;
    }

    report.duration = started.elapsed();
    log(&report);
    Ok(report)
}

/// Reads every sidecar found and writes what it says, a batch per transaction.
///
/// Answers with the recordings it wrote, so that the pass over everything else
/// does not look at the same files twice.
fn index_sessions(
    database: &mut Database,
    sidecars: &[PathBuf],
    settings: &IndexSettings,
    control: &IndexControl,
    stamp: &str,
    report: &mut IndexReport,
) -> Result<HashSet<i64>, IndexError> {
    let mut indexed = HashSet::new();

    for batch in sidecars.chunks(settings.pace.batch.max(1)) {
        if control.is_cancelled() {
            report.cancelled = true;
            break;
        }

        // Every file this batch needs is read here, outside the transaction.
        let mut prepared = Vec::with_capacity(batch.len());
        for path in batch {
            match ingest::prepare(path) {
                Ok(session) => prepared.push(session),
                Err(error) => report.problems.push(problem(path, error)),
            }
        }
        if prepared.is_empty() {
            continue;
        }

        let opened = Instant::now();
        let mut transaction = database.transaction()?;
        for session in &prepared {
            let mut savepoint = transaction.savepoint()?;
            let session_id = session.sidecar.session_id.clone();
            match ingest::write(&mut savepoint, session, stamp, &mut report.problems) {
                Ok(written) => {
                    savepoint.commit()?;
                    report.sessions_indexed += 1;
                    report.recordings_indexed += written.recordings;
                    report.recordings_newly_missing += written.newly_missing;
                    report.recordings_returned += written.returned;
                    indexed.extend(written.recording_ids);
                }
                Err(error) => {
                    // The savepoint rolls back as it is dropped, so the session
                    // that failed leaves nothing behind and the batch carries on.
                    drop(savepoint);
                    report
                        .problems
                        .push(IndexProblem::SessionRefused { session_id, error });
                }
            }
        }
        transaction.commit().map_err(IndexError::from)?;
        record_transaction(report, opened.elapsed());

        rest(settings.pace.rest);
    }

    Ok(indexed)
}

/// Looks at every recording and clip the sessions did not account for, and
/// marks what has gone.
///
/// This is the half of reconciliation that catches what ingestion cannot: a
/// session whose sidecar has itself been deleted, and rows written by a run
/// against roots this one was not given.
fn reconcile_rows(
    database: &mut Database,
    walked: &scan::Walk,
    settings: &IndexSettings,
    control: &IndexControl,
    stamp: &str,
    already_indexed: &HashSet<i64>,
    report: &mut IndexReport,
) -> Result<(), IndexError> {
    let page = settings.pace.page.max(1);

    for table in [Table::Recordings, Table::Clips] {
        let mut after = 0i64;
        loop {
            if control.is_cancelled() {
                report.cancelled = true;
                return Ok(());
            }

            let rows = read_page(database, table, after, page)?;
            if rows.is_empty() {
                break;
            }
            after = rows.last().map_or(after, |row| row.id);

            // The filesystem is consulted here, with no transaction open.
            let mut updates = Vec::new();
            for row in rows {
                if table == Table::Recordings && already_indexed.contains(&row.id) {
                    continue;
                }
                let path = PathBuf::from(&row.path);
                if !walked.covers(&path) {
                    // Not looked at. A row on a drive nobody plugged in is not
                    // evidence of anything.
                    continue;
                }
                let facts = presence::look_at(&path);
                let judged = presence::judge(
                    facts.present,
                    row.deleted_at.as_deref(),
                    row.missing_since.as_deref(),
                    stamp,
                );
                if !judged.newly_missing && !judged.returned {
                    continue;
                }
                match table {
                    Table::Recordings if judged.newly_missing => {
                        report.recordings_newly_missing += 1;
                    }
                    Table::Recordings => report.recordings_returned += 1,
                    Table::Clips if judged.newly_missing => report.clips_newly_missing += 1,
                    Table::Clips => report.clips_returned += 1,
                }
                updates.push((row.id, judged.missing_since, facts.size_bytes));
            }
            if updates.is_empty() {
                continue;
            }

            let opened = Instant::now();
            let transaction = database.transaction()?;
            {
                let mut statement = transaction.prepare(table.update())?;
                for (id, missing_since, size_bytes) in updates {
                    // A file that has gone keeps its last known size; only one
                    // that is there is re-measured.
                    match size_bytes {
                        Some(size) => statement.execute(params![id, missing_since, size])?,
                        None => statement.execute(params![id, missing_since, None::<i64>])?,
                    };
                }
            }
            transaction.commit().map_err(IndexError::from)?;
            record_transaction(report, opened.elapsed());

            rest(settings.pace.rest);
        }
    }

    Ok(())
}

/// The two tables that hold a path to a file this crate does not own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Recordings,
    Clips,
}

impl Table {
    /// One page of rows, ordered by identifier so that paging cannot miss one.
    fn page(self) -> &'static str {
        match self {
            Self::Recordings => {
                "SELECT recording_id, path, missing_since, deleted_at FROM recordings \
                 WHERE recording_id > ?1 ORDER BY recording_id LIMIT ?2"
            }
            Self::Clips => {
                "SELECT clip_id, path, missing_since, deleted_at FROM clips \
                 WHERE clip_id > ?1 ORDER BY clip_id LIMIT ?2"
            }
        }
    }

    /// The mark, and the size when there was a file to measure.
    fn update(self) -> &'static str {
        match self {
            Self::Recordings => {
                "UPDATE recordings SET missing_since = ?2, \
                 size_bytes = COALESCE(?3, size_bytes) WHERE recording_id = ?1"
            }
            Self::Clips => {
                "UPDATE clips SET missing_since = ?2, \
                 size_bytes = COALESCE(?3, size_bytes) WHERE clip_id = ?1"
            }
        }
    }
}

/// One row of a table that names a file.
#[derive(Debug)]
struct FileRow {
    id: i64,
    path: String,
    missing_since: Option<String>,
    deleted_at: Option<String>,
}

fn read_page(
    database: &Database,
    table: Table,
    after: i64,
    limit: usize,
) -> Result<Vec<FileRow>, IndexError> {
    let mut statement = database.connection().prepare(table.page())?;
    let rows = statement.query_map(
        params![after, i64::try_from(limit).unwrap_or(i64::MAX)],
        |row| {
            Ok(FileRow {
                id: row.get(0)?,
                path: row.get(1)?,
                missing_since: row.get(2)?,
                deleted_at: row.get(3)?,
            })
        },
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Counts the media under the roots that no row in the index claims.
///
/// Reported and nothing else. A file with no session sidecar cannot be
/// attributed to a game without inventing the answer, and an index that guessed
/// would file somebody's footage under a game they were not playing (AGENTS.md
/// section 27). Recovering these deliberately is issue #272.
fn report_unindexed_media(
    database: &Database,
    media: &[PathBuf],
    settings: &IndexSettings,
    report: &mut IndexReport,
) -> Result<(), IndexError> {
    if media.is_empty() {
        return Ok(());
    }

    // Keyed by the media found on disk rather than by the whole index, so the
    // memory this needs is bounded by what was walked and not by how large the
    // library has grown.
    let mut unclaimed: HashMap<String, &PathBuf> =
        media.iter().map(|path| (normalise(path), path)).collect();

    for query in [
        "SELECT path FROM recordings",
        "SELECT path FROM clips",
        // A user who moves a file and lets Clipped find it again still has the
        // old path in `deleted_from`; a file sitting there is accounted for.
        "SELECT deleted_from FROM recordings WHERE deleted_from IS NOT NULL",
        "SELECT deleted_from FROM clips WHERE deleted_from IS NOT NULL",
    ] {
        let mut statement = database.connection().prepare(query)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            unclaimed.remove(&normalise(Path::new(&path)));
        }
        if unclaimed.is_empty() {
            break;
        }
    }

    report.unindexed_media = unclaimed.len();
    // Sorted before it is cut down, so that the sample is the same files every
    // time rather than whichever ones the hash map happened to yield first.
    let mut sample: Vec<PathBuf> = unclaimed.into_values().cloned().collect();
    sample.sort();
    sample.truncate(settings.sample_limit);
    report.unindexed_sample = sample;
    Ok(())
}

/// A path in the form two paths are compared in.
///
/// Lower case, with one separator, because Windows file names are
/// case-insensitive and accept both (SPEC.md section 3): `D:\Clips\a.mkv` and
/// `d:/clips/a.mkv` are one file, and an index that thought otherwise would
/// report every recording as unindexed on a machine whose folder was
/// configured with a different capitalisation.
fn normalise(path: &Path) -> String {
    path.to_string_lossy()
        .to_lowercase()
        .replace('/', std::path::MAIN_SEPARATOR_STR)
}

fn record_transaction(report: &mut IndexReport, held: Duration) {
    report.transactions += 1;
    report.longest_transaction = report.longest_transaction.max(held);
}

/// The pause between transactions.
///
/// A separate function because it is the whole of "does not compete with
/// recording" that is not a property of transaction size, and because zero is
/// worth not calling the scheduler for.
fn rest(pause: Duration) {
    if !pause.is_zero() {
        std::thread::sleep(pause);
    }
}

fn problem(path: &Path, error: sidecar::SidecarError) -> IndexProblem {
    match error {
        sidecar::SidecarError::Unreadable(error) => IndexProblem::UnreadableSidecar {
            path: path.to_path_buf(),
            error,
        },
        sidecar::SidecarError::Malformed(error) => IndexProblem::MalformedSidecar {
            path: path.to_path_buf(),
            detail: error.to_string(),
        },
        sidecar::SidecarError::UnsupportedSchema { found } => {
            IndexProblem::UnsupportedSidecarVersion {
                path: path.to_path_buf(),
                found,
                supported: sidecar::SUPPORTED_SCHEMA_VERSION,
            }
        }
        sidecar::SidecarError::Incomplete { detail } => IndexProblem::MalformedSidecar {
            path: path.to_path_buf(),
            detail: detail.to_owned(),
        },
    }
}

fn log(report: &IndexReport) {
    info!(
        sessions = report.sessions_indexed,
        recordings = report.recordings_indexed,
        missing = report.recordings_newly_missing,
        returned = report.recordings_returned,
        unindexed = report.unindexed_media,
        transactions = report.transactions,
        longest_transaction_ms = report.longest_transaction.as_millis(),
        duration_ms = report.duration.as_millis(),
        cancelled = report.cancelled,
        "the library index was reconciled against the recording folders"
    );

    for root in &report.unavailable_roots {
        warn!(
            root = %root.path.display(),
            error = %root.error,
            "a recording folder could not be reached, so nothing in it was marked missing"
        );
    }
    for problem in report.problems.iter().take(PROBLEMS_LOGGED) {
        warn!("{problem}");
    }
    if report.problems.len() > PROBLEMS_LOGGED {
        debug!(
            total = report.problems.len(),
            "further indexing problems are in the report rather than the log"
        );
    }
}
