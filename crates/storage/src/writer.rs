//! Writing to the database from a thread that must not stall.
//!
//! # The problem this solves
//!
//! A recording loop hands the encoder a frame every 8 milliseconds at 120 FPS.
//! AGENTS.md section 20 says a capture thread must not wait on the database, and
//! section 18 lists high-frequency database writes among the things to avoid
//! outright — not because SQLite is slow, but because a commit is a disk flush
//! and a disk flush is a pause of unpredictable length happening while somebody
//! is playing a game.
//!
//! So nothing on a recording path writes to the database. It hands the work to a
//! [`WriteQueue`], which is a bounded channel and nothing else: a submission
//! costs a move and a semaphore, never a lock on the database, never an fsync,
//! and never a wait on another thread's transaction. A [`Writer`] thread drains
//! the queue and commits what it finds in **one transaction per batch**, so a
//! hundred rows cost one flush rather than a hundred.
//!
//! # What a full queue means
//!
//! It is bounded, so under a load the writer cannot keep up with, submission
//! fails rather than blocking. That is the right way round: the video is what
//! cannot be made again and the metadata can be rebuilt from the session
//! sidecars beside the recordings (docs/storage.md). A caller is told, by an
//! error it must handle, and the count is in [`WriteStats::dropped`] — what must
//! not happen is a recording thread stopping to wait for a disk.
//!
//! # Nothing uses this yet
//!
//! The recorder does not write to the database in this build: the library index
//! that would fill it is M6's remaining work. This is the mechanism that work
//! plugs into, and the reason it exists now rather than then is that the shape
//! of the writing path is what decides whether the capture path can be made to
//! wait on it (AGENTS.md section 55).

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tracing::{debug, error, warn};

use crate::database::Database;
use crate::error::StorageError;

/// How the writer batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteSettings {
    /// How many submissions may be waiting before further ones are refused.
    pub capacity: usize,
    /// The most writes that may share one transaction.
    pub max_batch: usize,
    /// How long the writer waits for more work after the first item of a batch
    /// arrives, before committing what it has.
    pub linger: Duration,
}

impl Default for WriteSettings {
    /// A queue deep enough to absorb a session's worth of events, batches
    /// large enough that a burst is one commit, and a linger short enough that
    /// a single write is visible to a reader almost immediately.
    ///
    /// The numbers are not measurements of anything — nothing writes to this
    /// database yet — they are bounds chosen so that the failure modes are the
    /// tolerable ones: 1024 outstanding writes is far more than a session
    /// produces, and 50 milliseconds of delay on metadata is imperceptible in a
    /// library screen.
    fn default() -> Self {
        Self {
            capacity: 1024,
            max_batch: 256,
            linger: Duration::from_millis(50),
        }
    }
}

/// What the writer did, for diagnostics and for tests that measure it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WriteStats {
    /// Writes taken off the queue.
    pub received: usize,
    /// Writes that were committed.
    pub applied: usize,
    /// Writes that failed and were rolled back individually.
    pub failed: usize,
    /// Writes refused because the queue was full.
    pub dropped: usize,
    /// Transactions committed. This is the number that matters: it is how many
    /// times the disk was asked to flush.
    pub transactions: usize,
}

/// Why a write could not be queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubmitError {
    /// The queue is full. The writer is behind, and this write was not taken.
    Full {
        /// The name the caller gave the write.
        name: &'static str,
    },
    /// The writer has stopped, so nothing more will be written.
    Stopped {
        /// The name the caller gave the write.
        name: &'static str,
    },
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { name } => write!(
                f,
                "the database write queue is full, so the {name} write was not recorded"
            ),
            Self::Stopped { name } => write!(
                f,
                "the database writer has stopped, so the {name} write was not recorded"
            ),
        }
    }
}

impl std::error::Error for SubmitError {}

/// What a queued write actually is.
///
/// A closure rather than a description of a row, because this crate does not
/// know what a row means. The caller — `clipped-library`, when it indexes a
/// session — writes its own SQL against the schema and hands it here to be run
/// inside the writer's transaction. Anything else would mean inventing a
/// vocabulary of records in the crate that is meant to sit underneath the one
/// that has it.
type Apply = Box<dyn FnOnce(&Connection) -> rusqlite::Result<()> + Send>;

/// One unit of work for the writer.
struct Write {
    name: &'static str,
    apply: Apply,
}

impl fmt::Debug for Write {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Write").field("name", &self.name).finish()
    }
}

/// What travels down the channel.
#[derive(Debug)]
enum Message {
    Write(Write),
    /// Sent by [`Writer::stop`]. It is a message rather than a dropped sender
    /// so that stopping does not depend on every [`WriteQueue`] the caller
    /// handed out having been dropped first, and so that everything already
    /// queued is written before the writer goes: it arrives behind that work in
    /// the same channel.
    Stop,
}

/// A handle a thread submits database writes through.
///
/// Cheap to clone and safe to hold on a hot path: submitting touches a channel
/// and nothing else.
#[derive(Debug, Clone)]
pub struct WriteQueue {
    sender: SyncSender<Message>,
    dropped: Arc<AtomicUsize>,
}

impl WriteQueue {
    /// Queues `apply` to run inside the writer's next transaction.
    ///
    /// `name` names the write for diagnostics — `"session"`, `"recording"` —
    /// and appears in the log if it fails.
    ///
    /// This never blocks and never touches the database.
    ///
    /// # Errors
    ///
    /// [`SubmitError::Full`] if the writer is behind, [`SubmitError::Stopped`]
    /// if it has gone. Both mean this write was not recorded, and neither is a
    /// reason to stop recording video.
    pub fn submit<F>(&self, name: &'static str, apply: F) -> Result<(), SubmitError>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<()> + Send + 'static,
    {
        let write = Write {
            name,
            apply: Box::new(apply),
        };
        match self.sender.try_send(Message::Write(write)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Err(SubmitError::Full { name })
            }
            Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Err(SubmitError::Stopped { name })
            }
        }
    }
}

/// The thread that owns the writing connection.
///
/// It is the only thing that writes, which is what makes the concurrency model
/// in [`Database`] true rather than aspirational: one writer, any number of
/// readers.
#[derive(Debug)]
pub struct Writer {
    queue: WriteQueue,
    thread: Option<JoinHandle<WriteStats>>,
}

impl Writer {
    /// Takes ownership of `database` and starts writing to it on a thread of
    /// its own.
    #[must_use]
    pub fn spawn(database: Database, settings: WriteSettings) -> Self {
        let (sender, receiver) = sync_channel(settings.capacity);
        let dropped = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&dropped);

        let thread = std::thread::Builder::new()
            .name("clipped-storage-writer".to_owned())
            .spawn(move || {
                let mut stats = run(database, &receiver, settings);
                stats.dropped = counted.load(Ordering::Relaxed);
                stats
            })
            .expect("a thread can be started");

        Self {
            queue: WriteQueue { sender, dropped },
            thread: Some(thread),
        }
    }

    /// A handle to submit writes through. Clone it for each thread that writes.
    #[must_use]
    pub fn queue(&self) -> WriteQueue {
        self.queue.clone()
    }

    /// Writes everything already queued, then stops.
    ///
    /// # Errors
    ///
    /// [`StorageError::Sqlite`] if the writer thread panicked, which is the one
    /// way its statistics can be unavailable.
    pub fn stop(mut self) -> Result<WriteStats, StorageError> {
        Self::join(&mut self.queue, &mut self.thread)
            .ok_or_else(|| StorageError::Sqlite(rusqlite::Error::InvalidQuery))
    }

    /// Asks the thread to finish and waits for it.
    fn join(
        queue: &mut WriteQueue,
        thread: &mut Option<JoinHandle<WriteStats>>,
    ) -> Option<WriteStats> {
        let handle = thread.take()?;
        // A full queue must not stop the writer from being stopped, and the
        // thread is draining it, so this waits rather than refusing.
        if queue.sender.send(Message::Stop).is_err() {
            debug!("the database writer had already stopped");
        }
        match handle.join() {
            Ok(stats) => Some(stats),
            Err(_) => {
                error!("the database writer thread panicked; queued writes were lost");
                None
            }
        }
    }
}

impl Drop for Writer {
    /// Stops the thread if [`Writer::stop`] was not called.
    ///
    /// A writer that outlived its owner would hold the database open and the
    /// process would not exit (AGENTS.md section 58).
    fn drop(&mut self) {
        if let Some(stats) = Self::join(&mut self.queue, &mut self.thread) {
            debug!(
                ?stats,
                "the database writer stopped when its handle was dropped"
            );
        }
    }
}

/// The writer thread.
fn run(
    mut database: Database,
    receiver: &Receiver<Message>,
    settings: WriteSettings,
) -> WriteStats {
    let mut stats = WriteStats::default();

    loop {
        // Blocking until there is something to do, so an idle recorder costs
        // nothing at all.
        let first = match receiver.recv() {
            Ok(Message::Write(write)) => write,
            Ok(Message::Stop) | Err(_) => break,
        };

        let mut batch = vec![first];
        let deadline = Instant::now() + settings.linger;
        let mut stopping = false;

        while batch.len() < settings.max_batch {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(Message::Write(write)) => batch.push(write),
                Ok(Message::Stop) => {
                    stopping = true;
                    break;
                }
                Err(_) => break,
            }
        }

        stats.received += batch.len();
        apply(&mut database, batch, &mut stats);

        if stopping {
            break;
        }
    }

    debug!(?stats, "the database writer finished");
    stats
}

/// Applies a batch inside one transaction.
///
/// Each write gets a savepoint of its own, so one that fails is rolled back
/// alone and the rest of the batch still commits. Losing a hundred good writes
/// because the hundred-and-first referred to a session that had been deleted is
/// not a trade worth making for a metadata index.
fn apply(database: &mut Database, batch: Vec<Write>, stats: &mut WriteStats) {
    let mut transaction = match database.transaction() {
        Ok(transaction) => transaction,
        Err(error) => {
            error!(%error, writes = batch.len(), "the database writer could not begin a transaction");
            stats.failed += batch.len();
            return;
        }
    };

    let mut applied = 0;
    for write in batch {
        let savepoint = match transaction.savepoint() {
            Ok(savepoint) => savepoint,
            Err(error) => {
                error!(name = write.name, %error, "the database writer could not open a savepoint");
                stats.failed += 1;
                continue;
            }
        };

        match (write.apply)(&savepoint) {
            Ok(()) => match savepoint.commit() {
                Ok(()) => applied += 1,
                Err(error) => {
                    warn!(name = write.name, %error, "a database write could not be released");
                    stats.failed += 1;
                }
            },
            Err(error) => {
                // Dropping the savepoint rolls this write back and leaves the
                // rest of the transaction intact.
                warn!(name = write.name, %error, "a database write failed and was rolled back");
                stats.failed += 1;
            }
        }
    }

    match transaction.commit() {
        Ok(()) => {
            stats.applied += applied;
            stats.transactions += 1;
        }
        Err(error) => {
            error!(%error, writes = applied, "a batch of database writes could not be committed");
            stats.failed += applied;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::*;
    use crate::test_support::scratch_directory;

    fn database(name: &str) -> (std::path::PathBuf, Database) {
        let path = scratch_directory(name).join("library.db");
        let database = Database::open(&path).expect("a database can be opened");
        (path, database)
    }

    /// A write that inserts one game, named after `index`.
    fn insert_game(index: usize) -> impl FnOnce(&Connection) -> rusqlite::Result<()> + Send {
        move |connection| {
            connection.execute(
                "INSERT INTO games (game_id, name, first_seen_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    format!("game-{index}"),
                    format!("Game {index}"),
                    "2026-08-11T14:32:05+01:00"
                ],
            )?;
            Ok(())
        }
    }

    fn count(path: &std::path::Path, table: &str) -> i64 {
        let connection = Connection::open(path).expect("the database reopens");
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("the rows can be counted")
    }

    #[test]
    fn queued_writes_reach_the_database() {
        let (path, database) = database("writer-basics");
        let writer = Writer::spawn(database, WriteSettings::default());
        let queue = writer.queue();

        for index in 0..10 {
            queue.submit("game", insert_game(index)).expect("it queues");
        }
        let stats = writer.stop().expect("the writer stops");

        assert_eq!(stats.received, 10);
        assert_eq!(stats.applied, 10);
        assert_eq!(stats.failed, 0);
        assert_eq!(count(&path, "games"), 10);
    }

    #[test]
    fn submitting_never_waits_for_the_database() {
        // The measurement behind "no high-frequency writes from capture
        // threads": with the writer deliberately held inside a transaction for
        // a second, a thread that submits work is not delayed by it at all.
        const HELD: Duration = Duration::from_secs(1);
        const SUBMISSIONS: usize = 5_000;

        let (path, database) = database("writer-never-waits");
        let writer = Writer::spawn(
            database,
            WriteSettings {
                capacity: SUBMISSIONS + 16,
                ..WriteSettings::default()
            },
        );
        let queue = writer.queue();

        queue
            .submit("slow", move |_| {
                std::thread::sleep(HELD);
                Ok(())
            })
            .expect("the slow write queues");
        // Let the writer pick it up, so the database really is busy for the
        // whole of the measurement below.
        std::thread::sleep(Duration::from_millis(150));

        let started = Instant::now();
        for index in 0..SUBMISSIONS {
            queue.submit("game", insert_game(index)).expect("it queues");
        }
        let elapsed = started.elapsed();

        println!(
            "{SUBMISSIONS} submissions took {elapsed:?} ({:?} each) while the database was busy",
            elapsed / u32::try_from(SUBMISSIONS).expect("the count fits")
        );
        assert!(
            elapsed < HELD / 4,
            "submitting waited on the database: {SUBMISSIONS} submissions took {elapsed:?} \
             while it was held for {HELD:?}"
        );

        let stats = writer.stop().expect("the writer stops");
        assert_eq!(stats.applied, SUBMISSIONS + 1);
        assert_eq!(
            count(&path, "games"),
            i64::try_from(SUBMISSIONS).expect("it fits")
        );
    }

    #[test]
    fn a_burst_of_writes_costs_far_fewer_transactions_than_it_has_writes() {
        // Counted by SQLite itself through its commit hook, not by the writer's
        // own bookkeeping, so the measurement does not come from the code being
        // measured (AGENTS.md section 19).
        const SUBMISSIONS: usize = 10_000;
        const MAX_BATCH: usize = 256;

        let (path, database) = database("writer-batches");
        let commits = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&commits);
        database
            .connection()
            .commit_hook(Some(move || {
                counted.fetch_add(1, Ordering::Relaxed);
                false
            }))
            .expect("SQLite accepts a commit hook");

        let writer = Writer::spawn(
            database,
            WriteSettings {
                capacity: SUBMISSIONS + 16,
                max_batch: MAX_BATCH,
                linger: Duration::from_millis(50),
            },
        );
        let queue = writer.queue();
        for index in 0..SUBMISSIONS {
            queue.submit("game", insert_game(index)).expect("it queues");
        }
        let stats = writer.stop().expect("the writer stops");

        let committed = commits.load(Ordering::Relaxed);
        println!(
            "{SUBMISSIONS} writes were committed in {committed} transactions \
             (about {} writes each)",
            u64::try_from(SUBMISSIONS).expect("it fits") / committed.max(1)
        );
        assert_eq!(stats.applied, SUBMISSIONS);
        assert_eq!(
            count(&path, "games"),
            i64::try_from(SUBMISSIONS).expect("it fits")
        );
        assert_eq!(
            committed,
            u64::try_from(stats.transactions).expect("it fits"),
            "SQLite and the writer disagree about how many transactions there were"
        );
        assert!(
            committed <= u64::try_from(SUBMISSIONS / MAX_BATCH + 4).expect("it fits"),
            "the writer committed {committed} transactions for {SUBMISSIONS} writes, \
             which is not batching"
        );
    }

    #[test]
    fn a_write_that_fails_does_not_take_the_rest_of_its_batch_with_it() {
        let (path, database) = database("writer-partial-failure");
        let writer = Writer::spawn(
            database,
            WriteSettings {
                // One batch, so the failure and the good writes really do share
                // a transaction.
                linger: Duration::from_millis(200),
                ..WriteSettings::default()
            },
        );
        let queue = writer.queue();

        queue.submit("game", insert_game(1)).expect("it queues");
        queue
            .submit("orphan", |connection| {
                connection.execute(
                    "INSERT INTO sessions (session_id, game_id, started_at) \
                     VALUES ('x-20260811-143205', 'never-played', '2026-08-11T14:32:05+01:00')",
                    [],
                )?;
                Ok(())
            })
            .expect("it queues");
        queue.submit("game", insert_game(2)).expect("it queues");

        let stats = writer.stop().expect("the writer stops");

        assert_eq!(stats.received, 3);
        assert_eq!(stats.applied, 2);
        assert_eq!(stats.failed, 1);
        assert_eq!(
            stats.transactions, 1,
            "the three writes shared a transaction"
        );
        assert_eq!(count(&path, "games"), 2);
        assert_eq!(count(&path, "sessions"), 0);
    }

    #[test]
    fn a_full_queue_refuses_a_write_rather_than_blocking_the_thread_that_offered_it() {
        let (_path, database) = database("writer-full-queue");
        let writer = Writer::spawn(
            database,
            WriteSettings {
                capacity: 2,
                ..WriteSettings::default()
            },
        );
        let queue = writer.queue();

        // Hold the writer inside a transaction so nothing drains behind us.
        queue
            .submit("slow", |_| {
                std::thread::sleep(Duration::from_millis(750));
                Ok(())
            })
            .expect("the slow write queues");
        std::thread::sleep(Duration::from_millis(150));

        let mut refused = None;
        let started = Instant::now();
        for index in 0..64 {
            if let Err(error) = queue.submit("game", insert_game(index)) {
                refused = Some(error);
                break;
            }
        }
        let elapsed = started.elapsed();

        assert_eq!(
            refused,
            Some(SubmitError::Full { name: "game" }),
            "the queue accepted more than its capacity, or blocked"
        );
        assert!(
            elapsed < Duration::from_millis(250),
            "a full queue blocked the submitting thread for {elapsed:?}"
        );

        let stats = writer.stop().expect("the writer stops");
        assert!(stats.dropped >= 1, "the refusal was not counted: {stats:?}");
    }

    #[test]
    fn stopping_writes_everything_that_was_already_queued() {
        // A recorder shutting down has usually just queued the end of a
        // session. Losing it because the writer was asked to stop in the same
        // breath would make the last session of every run the unreliable one.
        let (path, database) = database("writer-drains-on-stop");
        let writer = Writer::spawn(database, WriteSettings::default());
        let queue = writer.queue();
        for index in 0..200 {
            queue.submit("game", insert_game(index)).expect("it queues");
        }

        let stats = writer.stop().expect("the writer stops");

        assert_eq!(stats.applied, 200);
        assert_eq!(count(&path, "games"), 200);
    }
}
