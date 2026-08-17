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
//! # An item with no file is in the trash too, and can be put back
//!
//! `clips.path` is nullable — `0004_clips_without_a_file.sql` made it so
//! deliberately, because a generated highlight is a *range of a recording* and
//! costs no disk and no encoder time until somebody exports it. So a clip the
//! user deletes may have no file at all, and the question is whether the trash
//! should hold it, list it and restore it, or quietly leave it out.
//!
//! It holds it, and the reasoning is the trash's own rather than
//! [issue #591](https://github.com/wildware-uk/clipped/issues/591)'s, which
//! asked the same question of the library screen. Three reasons, in the order
//! they matter:
//!
//! - **This module deletes and restores *rows*, not files.** That is the whole
//!   design: a delete marks a row and moves a file, and a restore clears the
//!   marks and moves it back, so that the favourite, the tags, the bookmarks
//!   and the clips survive. The file half has always been allowed to be absent
//!   — `vault::Stowed::NoFile` and [`RestoreOutcome::file_restored`] exist for
//!   an item whose media the user removed in Explorer — and a clip that never
//!   had a file is that same state reached from the other side. What is put
//!   back is the clip: what it is called, what it is made of (`clips.edit`) and
//!   why it exists.
//! - **A hidden item would be unreachable in every direction.**
//!   [`Trash::expire`] and [`Trash::empty`] are both built from [`Trash::list`],
//!   so a row that listing filtered out would be marked deleted for ever: never
//!   shown, never restorable, never destroyed, and never counted by the
//!   confirmation [`EmptyTrash`] checks — which would then disagree with what
//!   is actually there. Filtering here does not hide an item, it strands one
//!   (AGENTS.md section 56).
//! - **Hiding a row is deleting it, one screen further out** (AGENTS.md
//!   section 27), which is #591's argument and applies here too.
//!
//! What follows is that [`TrashEntry::path`] and
//! [`TrashEntry::original_path`] are optional, and that nothing in this module
//! reads `path` as though the schema required one. `recordings.path` *is* `NOT
//! NULL`, so only a clip can be pathless; the queries here are shared by both
//! tables and so must allow it.
//!
//! No path is also not `missing_since`, which `crate::index` sets when a file
//! it expected is not there: no path is "there is no file yet", `missing_since`
//! is "there was one and it has gone". A screen that conflated them would tell
//! somebody a highlight had been lost when nothing was ever written.
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
    TrashEntry, TrashItem,
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

        // Two different reasons there is nothing to move, and both end here: a
        // clip nothing ever exported has no file to begin with, and an item
        // whose media the user removed in Explorer no longer has one. What is
        // being deleted is the row either way.
        let stowed = match &row.path {
            Some(file) => vault::stow(&self.directory, file, at)?,
            None => vault::Stowed::NoFile,
        };
        let moved_to = match &stowed {
            vault::Stowed::Moved(path) => Some(path.clone()),
            vault::Stowed::NoFile => None,
        };
        // A row whose media had already gone keeps the path it had: there is no
        // file in the trash for it to name, and `deleted_from` still records
        // where the recording used to be. A row that never had one keeps that
        // too — it goes to the trash with no path, exactly as it sat in the
        // library.
        let path = moved_to.clone().or_else(|| row.path.clone());
        let deleted_at = crate::index::moment::rfc3339(at);

        let written = database.connection().execute(
            &format!(
                "UPDATE {} SET path = ?2, deleted_at = ?3, deleted_from = ?4 WHERE {} = ?1",
                item.kind.table(),
                item.kind.id_column()
            ),
            params![
                item.id,
                text(path.as_deref()),
                deleted_at,
                text(row.path.as_deref()),
            ],
        );
        if let Err(error) = written {
            return Err(self.put_back(moved_to.as_deref(), row.path.as_deref(), error));
        }

        info!(
            item = %item,
            from = %redacted(row.path.as_deref()),
            to = %redacted(path.as_deref()),
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
        let original_path = row.deleted_from.clone().or_else(|| row.path.clone());

        // Three ways there is nothing to move, and all three restore the row —
        // which is the metadata the user asked to have back — and report that
        // no file came with it: the item never had one, its media had already
        // gone when it was deleted, or somebody has emptied the trash directory
        // by hand since.
        let to_move = match (row.path.as_deref(), original_path.as_deref()) {
            (Some(here), Some(there)) if here != there && vault::is_there(here) => {
                Some((here, there))
            }
            _ => None,
        };
        let path = match to_move {
            Some((here, there)) => Some(vault::restore_to(here, there)?),
            None => original_path.clone(),
        };
        let moved = to_move.is_some();

        let written = database.connection().execute(
            &format!(
                "UPDATE {} SET path = ?2, deleted_at = NULL, deleted_from = NULL WHERE {} = ?1",
                item.kind.table(),
                item.kind.id_column()
            ),
            params![item.id, text(path.as_deref())],
        );
        if let Err(error) = written {
            let moved_from = if moved { path.clone() } else { None };
            return Err(self.put_back(moved_from.as_deref(), row.path.as_deref(), error));
        }
        if let Some(here) = row.path.as_deref().filter(|_| moved) {
            vault::tidy(&self.directory, here);
        }

        info!(
            item = %item,
            to = %redacted(path.as_deref()),
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
                // `Option`, because `clips.path` is nullable and this statement
                // is run against `clips` as well as `recordings`. It used to be
                // a `String`, which was safe only because the sole writer of
                // `deleted_at` reads the row first and so happened to reject a
                // pathless clip before it could ever be marked — an invariant
                // of the call graph that no query, type or comment stated, and
                // that `Trash::send` deliberately no longer holds
                // ([issue #593](https://github.com/wildware-uk/clipped/issues/593)).
                let path = row.get::<_, Option<String>>(1)?.map(PathBuf::from);
                let original_path = row
                    .get::<_, Option<String>>(3)?
                    .map(PathBuf::from)
                    .or_else(|| path.clone());
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
            original_path: row.deleted_from.or_else(|| row.path.clone()),
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
                        path = %redacted(entry.path.as_deref()),
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
        let file = match entry.path.as_deref() {
            Some(path) => vault::discard(&self.directory, path)?,
            // Nothing was ever written for it, so there is nothing to unlink
            // and no bytes to reclaim. The row still goes: an entry that can
            // never be restored or acted on is not a record of anything.
            None => FileOutcome::AlreadyGone,
        };
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
        belongs_at: Option<&Path>,
        error: clipped_storage::rusqlite::Error,
    ) -> TrashError {
        // Nothing was moved, or there is nowhere for it to go back to, and in
        // both cases the index refusing the change is the whole of what
        // happened.
        let (Some(file), Some(belongs_at)) = (moved_to, belongs_at) else {
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
                    // `Option`, because `clips.path` is nullable. Read as a
                    // `String` this answered `InvalidColumnType` for a clip
                    // nothing had exported, and because every operation in this
                    // module starts here that became the answer to *restoring*
                    // one: `library_unavailable` where the truth was
                    // `invalid_parameters`
                    // ([issue #593](https://github.com/wildware-uk/clipped/issues/593)).
                    path: row.get::<_, Option<String>>(0)?.map(PathBuf::from),
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
    /// The file, when the row names one.
    ///
    /// [`None`] only for a clip: `recordings.path` is `NOT NULL` and
    /// `clips.path` has been nullable since `0004_clips_without_a_file.sql`.
    path: Option<PathBuf>,
    deleted_at: Option<String>,
    deleted_from: Option<PathBuf>,
    size_bytes: Option<i64>,
}

/// A path as the index stores it, or NULL where there is none.
fn text(path: Option<&Path>) -> Option<String> {
    path.map(|path| path.display().to_string())
}

/// A path for a log line, redacted, or a phrase where there is none.
///
/// `RedactedPath` cannot say "no file", and a log line reading `path=` with
/// nothing after it is one a reader cannot tell from a bug.
fn redacted(path: Option<&Path>) -> String {
    path.map_or_else(
        || "(no file)".to_owned(),
        |path| RedactedPath::new(path).to_string(),
    )
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
