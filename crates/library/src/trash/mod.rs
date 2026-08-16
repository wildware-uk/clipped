//! Deleting a recording so that it can be undeleted.
//!
//! SPEC.md section 28: deleting footage moves it into an application trash, a
//! configurable retention decides how long it stays, and it can be restored
//! until then. This module is that, and it is the reason the rest of M12 is
//! defensible — issue #111's automatic cleanup deletes recordings on the user's
//! behalf, and AGENTS.md section 56 makes recordings the one thing that must not
//! be lost, so an automatic deleter is only acceptable if there is a way back.
//!
//! `docs/storage-management.md` is the prose, including the decisions summarised
//! below and the reasoning behind each.
//!
//! # What "in the trash" means physically
//!
//! **The file is moved, on the same volume, with a rename.** It goes to
//! `<trash>\<when it was deleted>\<its own name>`, its row keeps existing, and
//! `path`, `deleted_at` and `deleted_from` in the schema say where it is now,
//! when it went and where it came from ([`crates/storage/migrations`]).
//!
//! The three plausible answers and why this is the one:
//!
//! | Answer | Why not |
//! | --- | --- |
//! | Marked in place | Nothing is reclaimed. A user who deletes 40 GB to make room and sees no change in free space has been lied to, and issue #111 deletes *to make room* |
//! | Copied, then unlinked | Needs as much free space as the file, on the one occasion — a full disk — when there is none, and holds two copies of a recording meanwhile |
//! | **Moved with a rename** | Costs no space and no time whatever the file's size, because a rename on one volume rewrites a directory entry and touches no data |
//!
//! The rename's one limitation is that it cannot cross a volume, so a library
//! spread over two drives needs a trash on each. That is refused with a message
//! naming both ([`TrashError::DifferentVolume`]) rather than silently becoming a
//! copy.
//!
//! # Retention, and what happens when it expires
//!
//! [`Retention`] is the four choices SPEC.md section 28 names and no fifth.
//! Expiry is judged from `deleted_at` and a `now` the caller passes, so nothing
//! here reads a clock and a test never waits.
//!
//! When it expires, [`Trash::expire`] destroys the item: the file is unlinked
//! and **the row is deleted**. That is the only place in `clipped-library` that
//! removes a row, and it is deliberate — an entry that can never be restored,
//! played or acted on is not a record of anything, and the schema's `ON DELETE`
//! rules were written for this moment (a clip outlives the recording it came
//! from; a session is never touched).
//!
//! Expiry is always an explicit call. Nothing in this module runs on a timer, so
//! the moment footage is destroyed is a moment the application chose, which is
//! what makes it possible to say when it happens.
//!
//! [`Retention::Immediate`] means "expires the instant it is deleted", not
//! "unlinked by the delete itself". There is one code path, the file is still
//! recoverable until the next sweep, and a user who picked the setting that
//! keeps nothing still gets the few minutes in which they realise.
//!
//! # What restore does when the original location is gone or occupied
//!
//! | The original location | What happens |
//! | --- | --- |
//! | Free | The file goes back to it exactly, and the row with it |
//! | Its folder has been deleted | The folder is recreated and the file goes back to it |
//! | **Occupied by another file** | The file goes back *beside* it as `name (restored).mkv`, and [`RestoreOutcome::diverted`] says so |
//! | On a drive that is not there | The move fails, nothing changes, and the item is still in the trash |
//!
//! Overwriting is never an option. Whatever is sitting at the original location
//! is a file the user did not ask to lose — most often the same recording put
//! back from a backup — and destroying it to make room for a restore would be
//! exactly the deletion nobody asked for.
//!
//! # The Windows Recycle Bin's role: none
//!
//! Asked and answered on [issue #103], where `clipped-recorder recover
//! --discard` faced the same question. Clipped never sends anything to the
//! Recycle Bin, for reasons that get worse the larger the file is:
//!
//! - **It silently destroys large files.** The Recycle Bin has a per-volume size
//!   cap, around 5% of the volume by default. A file larger than the cap is
//!   *permanently deleted* rather than recycled, with a warning at most. A
//!   recording is the largest file on most machines, so the case the recycle bin
//!   would be there to protect is exactly the case it does not.
//! - **It evicts silently too.** Recycling one large recording can push older
//!   items out of the bin to stay under the cap, which would make deleting one
//!   thing destroy another.
//! - **It is not everywhere.** Network shares and some removable media have no
//!   Recycle Bin, so a library kept on one would need this trash anyway, and two
//!   mechanisms that both mean "thrown away" is worse than one.
//! - **Its retention is not the user's.** SPEC.md section 28 offers 3, 7 and 30
//!   days; the Recycle Bin offers whatever Windows decides, and a restore made
//!   from Explorer would put the file back with the index still saying it was
//!   deleted.
//!
//! Emptying Clipped's trash therefore deletes, and says so. What the Recycle Bin
//! does keep is its place as the *user's* tool: recordings are ordinary files in
//! an ordinary folder (AGENTS.md section 32), so somebody who deletes one in
//! Explorer gets the Recycle Bin's behaviour and Clipped's reconciliation notices
//! the file has gone and marks the row rather than removing it.
//!
//! # Never delete anything a user did not ask to delete
//!
//! Three interlocks, each in one place so that each can be reviewed:
//!
//! - **Only the trash's own files can be unlinked.** `vault::discard` is the
//!   only function in this crate that removes a media file, and a path that is
//!   not inside the trash directory is left exactly where it is.
//! - **Only something already in the trash can be destroyed.**
//!   [`Trash::permanently_delete`] refuses an item whose `deleted_at` is unset,
//!   so no single call can reach a recording that is still in the library.
//! - **Emptying the trash is confirmed against what the user was shown.**
//!   [`EmptyTrash`] carries the count and the size from the listing, and
//!   [`Trash::empty`] refuses if the trash has changed since.
//!
//! And one more, which is why a failed delete is safe: **the file wins over the
//! row.** If the move succeeds and the database refuses the change, the file is
//! moved back before the error is returned. An index can be rebuilt from the
//! session sidecars beside the recordings; a recording cannot be rebuilt from
//! anything.
//!
//! # Threading
//!
//! Synchronous, on a thread the caller owns, and **never a capture thread**
//! (AGENTS.md section 20): a rename is a filesystem call. Every database
//! statement here is a single one, so it is its own transaction and the
//! database's one writer is never held for longer than a row update.
//!
//! [`crates/storage/migrations`]: https://github.com/wildware-uk/clipped/tree/main/crates/storage/migrations
//! [issue #103]: https://github.com/wildware-uk/clipped/issues/103

mod entry;
mod error;
mod retention;
mod vault;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clipped_logging::RedactedPath;
use clipped_storage::rusqlite::{params, OptionalExtension};
use clipped_storage::Database;
use tracing::{info, warn};

pub use entry::{
    EmptyTrash, ExpiryFailure, ExpiryReport, FileOutcome, ItemKind, Removal, RestoreOutcome,
    TrashEntry, TrashItem, UntrackedStow,
};
pub use error::TrashError;
pub use retention::Retention;

/// The application trash: one directory, and the operations over it.
///
/// Cheap to construct and holds no state beyond where the directory is, so a
/// caller makes one where it needs one rather than passing one around. A library
/// spread over two drives has one of these per drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trash {
    directory: PathBuf,
}

impl Trash {
    /// The trash kept in `directory`.
    ///
    /// The directory is not created until something is deleted, so a library
    /// nobody has deleted from has no empty folder in it. It must be on the same
    /// volume as the media it will hold, and it must not be inside a directory
    /// storage accounting walks as another category — `StorageRoots` refuses
    /// that overlap (`crate::accounting`), because a trash inside the recordings
    /// folder would be counted twice.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Where the trash is.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Deletes `item`: moves its file into the trash and marks its row.
    ///
    /// `at` is the moment the deletion is recorded as, and what retention is
    /// later judged from. Callers pass [`SystemTime::now`]; tests pass a fixed
    /// reading (AGENTS.md section 25).
    ///
    /// # Errors
    ///
    /// [`TrashError::NoSuchItem`] if the index has no such row and
    /// [`TrashError::AlreadyInTrash`] if it is deleted already — neither touches
    /// anything. [`TrashError::DifferentVolume`] if the file cannot be renamed
    /// into the trash, and [`TrashError::Move`] or
    /// [`TrashError::CreateDirectory`] if the move itself failed; the file is
    /// still where it was in every one of those.
    ///
    /// [`TrashError::Database`] if the row could not be marked, in which case the
    /// file has been moved *back*; [`TrashError::Stranded`] in the one case where
    /// that failed too, naming both paths.
    pub fn send(
        &self,
        database: &mut Database,
        item: TrashItem,
        at: SystemTime,
    ) -> Result<TrashEntry, TrashError> {
        let row = self.read(database, item)?;
        if let Some(deleted_at) = row.deleted_at {
            return Err(TrashError::AlreadyInTrash { item, deleted_at });
        }

        let stowed = vault::stow(&self.directory, &row.path, at)?;
        let moved_to = match &stowed {
            vault::Stowed::Moved(path) => Some(path.clone()),
            vault::Stowed::NoFile => None,
        };
        // A row whose media had already gone keeps the path it had: there is no
        // file in the trash for it to name, and `deleted_from` still records
        // where the recording used to be.
        let path = moved_to.clone().unwrap_or_else(|| row.path.clone());
        let deleted_at = crate::index::moment::rfc3339(at);

        let written = database.connection().execute(
            &format!(
                "UPDATE {} SET path = ?2, deleted_at = ?3, deleted_from = ?4 WHERE {} = ?1",
                item.kind.table(),
                item.kind.id_column()
            ),
            params![
                item.id,
                path.display().to_string(),
                deleted_at,
                row.path.display().to_string(),
            ],
        );
        if let Err(error) = written {
            return Err(self.put_back(moved_to.as_deref(), &row.path, error));
        }

        info!(
            item = %item,
            from = %RedactedPath::new(&row.path),
            to = %RedactedPath::new(&path),
            file_moved = moved_to.is_some(),
            "a library item was moved to the trash"
        );

        // Said after the move rather than instead of it. Deleting a recording
        // that clips were cut from is the user's decision and is not refused --
        // it is recoverable, which is what the trash is for -- but it must not
        // be silent: those clips are ranges of a file that is no longer where
        // they point, and nothing else would tell anybody
        // ([issue #74](https://github.com/wildware-uk/clipped/issues/74)).
        //
        // Automatic cleanup takes the other answer and will not touch such a
        // recording at all (`crate::accounting::cleanup`), because nobody chose
        // that deletion.
        let dependent_clips = dependent_clips(database, item);
        if dependent_clips > 0 {
            tracing::warn!(
                item = %item,
                clips = dependent_clips,
                "this recording is the source of clips, and they now point at a file in the                  trash; restoring it puts them back"
            );
        }
        Ok(TrashEntry {
            item,
            path,
            original_path: row.path,
            deleted_at,
            size_bytes: row.size_bytes,
            dependent_clips,
        })
    }

    /// Moves a file into the trash with no row to track it by.
    ///
    /// Every other operation here is keyed to a row in `recordings` or
    /// `clips` — that is what lets an item be listed, restored by
    /// [`Self::restore`] and swept by [`Self::expire`]. Some callers have a
    /// file to get rid of and no such row to give it: `clipped-recorder
    /// recover --discard` (issue #451) hands back a fragment an interrupted
    /// recorder left, which the library has deliberately not indexed —
    /// `clipped_session::automatic::recovery` only writes a sidecar entry a
    /// recording is finished, and indexing it here purely so it could be
    /// trashed would make a delete action responsible for ingestion, which is
    /// not this crate's job and is not free: it would run head-first into
    /// `crate::index::ingest`'s `RECORDING_OUTCOMES` not yet recognising the
    /// outcome recovery writes, which is a real gap and a separate ticket, not
    /// something to paper over here (AGENTS.md section 55).
    ///
    /// So this does only the half it owns: the same rename, into the same
    /// trash directory, under the same cross-volume refusal as [`Self::send`].
    /// What it does **not** do is the whole reason it has a different name,
    /// and a caller has to be told plainly:
    ///
    /// - The file will not appear in [`Self::list`], is not counted towards
    ///   [`Self::empty`]'s confirmation, and [`Self::expire`] will never reach
    ///   it — there is no `deleted_at` for retention to be judged from.
    /// - It is undone by moving the file out of the trash directory by hand,
    ///   not by [`Self::restore`].
    ///
    /// It is real trash in the one sense that has to be true for a delete
    /// command to reach for it at all: the file is on disk, inside the trash
    /// directory, byte for byte — which is what makes a mistaken `--discard`
    /// recoverable, even without a database row saying so.
    ///
    /// # Errors
    ///
    /// [`TrashError::DifferentVolume`] if the file cannot be renamed into the
    /// trash — refused rather than silently copied, for the reason
    /// [`Self::send`] is. [`TrashError::CreateDirectory`] or
    /// [`TrashError::Move`] if the move itself failed; the file is still where
    /// it was in every one of those.
    pub fn stow_untracked(&self, file: &Path, at: SystemTime) -> Result<UntrackedStow, TrashError> {
        let stowed = vault::stow(&self.directory, file, at)?;
        let path = match stowed {
            vault::Stowed::Moved(path) => Some(path),
            vault::Stowed::NoFile => None,
        };
        if let Some(path) = &path {
            info!(
                from = %RedactedPath::new(file),
                to = %RedactedPath::new(path),
                "a file with no library row was moved to the trash"
            );
        }
        Ok(UntrackedStow { path })
    }

    /// Restores `item` from the trash: moves its file back and clears its marks.
    ///
    /// The file returns to where it came from unless something is there, in
    /// which case it lands beside it and [`RestoreOutcome::diverted`] says so.
    /// Nothing at the original location is ever overwritten.
    ///
    /// Everything the user put on the item — its favourite, its tags, its
    /// bookmarks, the clips cut from it — is untouched throughout, because the
    /// row was never removed. That is the whole reason deleting marks a row
    /// instead of dropping one.
    ///
    /// # Errors
    ///
    /// [`TrashError::NoSuchItem`] or [`TrashError::NotInTrash`] if there is
    /// nothing to restore, [`TrashError::Move`] or
    /// [`TrashError::CreateDirectory`] if the file could not be put back — the
    /// item stays in the trash in all of those — and [`TrashError::Database`] if
    /// the row could not be cleared, in which case the file has been returned to
    /// the trash.
    pub fn restore(
        &self,
        database: &mut Database,
        item: TrashItem,
    ) -> Result<RestoreOutcome, TrashError> {
        let row = self.read(database, item)?;
        if row.deleted_at.is_none() {
            return Err(TrashError::NotInTrash { item });
        }
        let original_path = row.deleted_from.clone().unwrap_or_else(|| row.path.clone());

        // Two ways there is nothing to move: the item's media had already gone
        // when it was deleted, and somebody has emptied the trash directory by
        // hand since. Both restore the row, which is the metadata the user
        // asked for back, and report that no file came with it.
        let moved = row.path != original_path && vault::is_there(&row.path);
        let path = if moved {
            vault::restore_to(&row.path, &original_path)?
        } else {
            original_path.clone()
        };

        let written = database.connection().execute(
            &format!(
                "UPDATE {} SET path = ?2, deleted_at = NULL, deleted_from = NULL WHERE {} = ?1",
                item.kind.table(),
                item.kind.id_column()
            ),
            params![item.id, path.display().to_string()],
        );
        if let Err(error) = written {
            let moved_from = moved.then(|| path.clone());
            return Err(self.put_back(moved_from.as_deref(), &row.path, error));
        }
        if moved {
            vault::tidy(&self.directory, &row.path);
        }

        info!(
            item = %item,
            to = %RedactedPath::new(&path),
            diverted = path != original_path,
            file_restored = moved,
            "a library item was restored from the trash"
        );
        Ok(RestoreOutcome {
            item,
            path,
            original_path,
            file_restored: moved,
        })
    }

    /// What is in the trash, newest deletion first.
    ///
    /// Reads and writes nothing, so the desktop application can call it on a
    /// read-only connection while the recorder writes.
    ///
    /// # Errors
    ///
    /// [`TrashError::Database`] if the index refuses.
    pub fn list(&self, database: &Database) -> Result<Vec<TrashEntry>, TrashError> {
        let mut entries = Vec::new();
        for kind in ItemKind::ALL {
            let mut statement = database.connection().prepare(&format!(
                "SELECT {}, path, deleted_at, deleted_from, size_bytes FROM {} \
                 WHERE deleted_at IS NOT NULL",
                kind.id_column(),
                kind.table()
            ))?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path = PathBuf::from(row.get::<_, String>(1)?);
                let original_path = row
                    .get::<_, Option<String>>(3)?
                    .map_or_else(|| path.clone(), PathBuf::from);
                let item = TrashItem {
                    kind,
                    id: row.get(0)?,
                };
                entries.push(TrashEntry {
                    item,
                    dependent_clips: dependent_clips(database, item),
                    path,
                    original_path,
                    deleted_at: row.get(2)?,
                    size_bytes: row.get(4)?,
                });
            }
        }

        // Newest first, and ties broken by something stable so that two calls
        // answer in the same order and a screen that does not sort still looks
        // still.
        entries.sort_by(|left, right| {
            right
                .deleted_at
                .cmp(&left.deleted_at)
                .then_with(|| left.item.cmp(&right.item))
        });
        Ok(entries)
    }

    /// The trash's entry for `item`, if it has one.
    ///
    /// # Errors
    ///
    /// [`TrashError::Database`] if the index refuses.
    pub fn entry(
        &self,
        database: &Database,
        item: TrashItem,
    ) -> Result<Option<TrashEntry>, TrashError> {
        let row = self.read(database, item)?;
        let dependent_clips = dependent_clips(database, item);
        Ok(row.deleted_at.map(|deleted_at| TrashEntry {
            item,
            original_path: row.deleted_from.unwrap_or_else(|| row.path.clone()),
            path: row.path,
            deleted_at,
            size_bytes: row.size_bytes,
            dependent_clips,
        }))
    }

    /// Destroys everything `retention` has expired by `now`.
    ///
    /// This is the call that ends the recoverable period, and it is the only
    /// thing in Clipped that destroys footage without somebody naming the item.
    /// It reaches only rows that are already in the trash, only files inside the
    /// trash directory, and only items whose `deleted_at` it could read.
    ///
    /// One item that cannot be destroyed does not stop the sweep: it is reported
    /// in [`ExpiryReport::failures`] and tried again next time.
    ///
    /// # Errors
    ///
    /// [`TrashError::Database`] if the trash could not be listed at all. A
    /// failure on one item is in the report rather than here.
    pub fn expire(
        &self,
        database: &mut Database,
        retention: Retention,
        now: SystemTime,
    ) -> Result<ExpiryReport, TrashError> {
        let expired: Vec<TrashEntry> = self
            .list(database)?
            .into_iter()
            .filter(|entry| entry.has_expired(retention, now))
            .collect();
        Ok(self.destroy_all(database, &expired, "retention expired"))
    }

    /// Destroys one item the user has named.
    ///
    /// # Errors
    ///
    /// [`TrashError::NoSuchItem`] if there is no such row, and
    /// [`TrashError::NotInTrash`] if it is not in the trash — the interlock that
    /// makes it impossible to destroy a recording the library still holds.
    /// [`TrashError::Remove`] if the file would not go, in which case the row is
    /// left as it was and the item is still in the trash.
    pub fn permanently_delete(
        &self,
        database: &mut Database,
        item: TrashItem,
    ) -> Result<Removal, TrashError> {
        let Some(entry) = self.entry(database, item)? else {
            return Err(TrashError::NotInTrash { item });
        };
        let removal = self.destroy(database, &entry)?;
        info!(
            item = %item,
            reason = "the user asked for it",
            outcome = ?removal.file,
            "an item was permanently deleted from the trash"
        );
        Ok(removal)
    }

    /// Destroys everything in the trash.
    ///
    /// `confirmation` carries the figures the user was shown when they agreed to
    /// it, and this refuses if the trash no longer matches them — see
    /// [`EmptyTrash`] for why that is the shape rather than a boolean.
    ///
    /// # Errors
    ///
    /// [`TrashError::Changed`] if the trash is not what was confirmed, in which
    /// case nothing is touched and the caller shows the new listing.
    /// [`TrashError::Database`] if it could not be listed.
    pub fn empty(
        &self,
        database: &mut Database,
        confirmation: EmptyTrash,
    ) -> Result<ExpiryReport, TrashError> {
        let entries = self.list(database)?;
        let found = EmptyTrash::for_listing(&entries);
        if found != confirmation {
            return Err(TrashError::Changed {
                confirmed_items: confirmation.items,
                confirmed_bytes: confirmation.bytes,
                found_items: found.items,
                found_bytes: found.bytes,
            });
        }
        Ok(self.destroy_all(database, &entries, "the user emptied the trash"))
    }

    /// Destroys each of `entries`, collecting what would not go.
    fn destroy_all(
        &self,
        database: &mut Database,
        entries: &[TrashEntry],
        reason: &'static str,
    ) -> ExpiryReport {
        let mut report = ExpiryReport::default();
        for entry in entries {
            match self.destroy(database, entry) {
                Ok(removal) => report.removed.push(removal),
                Err(error) => {
                    warn!(
                        item = %entry.item,
                        path = %RedactedPath::new(&entry.path),
                        %error,
                        "an item could not be removed from the trash, and will be tried again"
                    );
                    report.failures.push(ExpiryFailure {
                        item: entry.item,
                        error,
                    });
                }
            }
        }
        info!(
            %reason,
            removed = report.removed.len(),
            bytes_reclaimed = report.bytes_reclaimed(),
            failures = report.failures.len(),
            "the trash was swept"
        );
        report
    }

    /// Destroys one entry: the file first, then the row.
    ///
    /// That order, and not the other one. A row removed before its file leaves
    /// bytes in the trash that nothing knows about and no sweep will ever look
    /// at again; a file removed before its row leaves an entry whose next sweep
    /// tidies it up.
    fn destroy(&self, database: &mut Database, entry: &TrashEntry) -> Result<Removal, TrashError> {
        let file = vault::discard(&self.directory, &entry.path)?;
        database.connection().execute(
            &format!(
                "DELETE FROM {} WHERE {} = ?1",
                entry.item.kind.table(),
                entry.item.kind.id_column()
            ),
            params![entry.item.id],
        )?;
        Ok(Removal {
            item: entry.item,
            path: entry.path.clone(),
            size_bytes: entry.size_bytes,
            file,
        })
    }

    /// Undoes a move this module has just made, after the index refused the
    /// change that went with it.
    ///
    /// The file is what matters: an index can be rebuilt from the session
    /// sidecars beside the recordings, and a recording cannot be rebuilt from
    /// anything (`docs/storage.md`, AGENTS.md section 56).
    fn put_back(
        &self,
        moved_to: Option<&Path>,
        belongs_at: &Path,
        error: clipped_storage::rusqlite::Error,
    ) -> TrashError {
        let Some(file) = moved_to else {
            return TrashError::Database(error);
        };
        match vault::move_back(file, belongs_at) {
            Ok(()) => {
                // The entry directory the move made is now empty. Leaving it
                // would put a folder in the trash for a deletion that did not
                // happen.
                vault::tidy(&self.directory, file);
                TrashError::Database(error)
            }
            Err(source) => TrashError::Stranded {
                file: file.to_path_buf(),
                belongs_at: belongs_at.to_path_buf(),
                source,
            },
        }
    }

    /// The three columns every operation here starts from.
    fn read(&self, database: &Database, item: TrashItem) -> Result<ItemRow, TrashError> {
        database
            .connection()
            .prepare(&format!(
                "SELECT path, deleted_at, deleted_from, size_bytes FROM {} WHERE {} = ?1",
                item.kind.table(),
                item.kind.id_column()
            ))?
            .query_row(params![item.id], |row| {
                Ok(ItemRow {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    deleted_at: row.get(1)?,
                    deleted_from: row.get::<_, Option<String>>(2)?.map(PathBuf::from),
                    size_bytes: row.get(3)?,
                })
            })
            .optional()?
            .ok_or(TrashError::NoSuchItem { item })
    }
}

/// One row of `recordings` or `clips`, in the columns the trash cares about.
#[derive(Debug)]
struct ItemRow {
    path: PathBuf,
    deleted_at: Option<String>,
    deleted_from: Option<PathBuf>,
    size_bytes: Option<i64>,
}

/// How many clips name `item` as the recording they were cut from.
///
/// Zero for a clip, which is not a source of anything. A failure to ask is
/// reported as zero rather than as an error: this exists to add a sentence to a
/// log, and a deletion that worked must not become a failure because a count
/// could not be read (AGENTS.md section 17).
fn dependent_clips(database: &Database, item: TrashItem) -> u32 {
    if item.kind != ItemKind::Recording {
        return 0;
    }
    database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM clips WHERE source_recording_id = ?1",
            params![item.id],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| u32::try_from(count).unwrap_or(u32::MAX))
        .unwrap_or(0)
}
