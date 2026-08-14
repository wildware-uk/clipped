//! Replacing a file only once the whole of it has been written.
//!
//! **Eight** places in this workspace write a file by putting it somewhere
//! temporary and renaming over the destination: the encoder's capability cache,
//! the catalogue overlay, the thumbnail cache, the settings store, the recovery
//! and sidecar writers, the bookmark file and a screenshot. That pattern is
//! right — a rename is atomic, so a reader never sees half a file, and a crash
//! mid-write leaves the old one intact.
//!
//! They had two failures between them, and neither was visible from any single
//! call site.
//!
//! **An abandoned temporary is never cleaned up.** The temporary is named after
//! the writing process so that two writers cannot collide, and a process that
//! dies between creating the file and renaming it leaves that file behind for
//! ever. Nothing swept them, and the name guarantees the next run will not reuse
//! one: it picks its own identifier and leaves the old one where it is.
//!
//! **And four of the eight did not name it after the process at all** — a fixed
//! `.tmp`, shared by every process writing that destination. Two of them
//! interleaving is how a truncated file gets renamed into place, which is the
//! thing the pattern exists to prevent. Both are fixed by there being one
//! implementation rather than eight (AGENTS.md section 55).
//!
//! ```text
//! C:\Users\…\AppData\Local\Clipped\
//!     encoder-capabilities.json
//!     encoder-capabilities.json.52248.tmp   <- 0 bytes, orphaned
//! ```
//!
//! One is litter; they accumulate, in a directory somebody occasionally looks at
//! and never cleans ([issue #400](https://github.com/wildware-uk/clipped/issues/400)).
//!
//! # How an orphan is told from a file somebody is writing
//!
//! Two conditions, both of which have to hold before anything is removed.
//!
//! **The file is not held open.** [`write_atomically`] keeps its temporary open
//! with no sharing for as long as it is writing, so a sweep cannot even open a
//! live writer's file. This is the same mechanism the replay buffer uses to tell
//! a crashed process's spill directory from a running one's
//! (`clipped_replay::spill`), and it has the same property: the operating system
//! releases the handle when the process ends, however it ended, so it cannot be
//! fooled by a process identifier that has been reused.
//!
//! **And it is older than [`ORPHAN_AGE`].** There is a moment between closing
//! the temporary and renaming it when nothing holds it, and a sweep running in
//! another process at exactly that moment could otherwise take it — which would
//! turn somebody else's successful write into a failed one. That window is
//! microseconds; a minute of slack closes it completely.
//!
//! A sweep never fails the write it is attached to. Litter is a smaller problem
//! than a refused write (AGENTS.md section 17), so every error it meets is
//! ignored and the file will simply be met again next time.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// What every temporary written here ends with.
const TEMPORARY_SUFFIX: &str = ".tmp";

/// How old an unheld temporary has to be before it is treated as abandoned.
///
/// See the module documentation: this closes the gap between a writer closing
/// its temporary and renaming it.
pub const ORPHAN_AGE: Duration = Duration::from_secs(60);

/// Where this process writes `destination` before renaming it into place.
///
/// The identifier is what stops two processes writing the same destination from
/// sharing a temporary and leaving a truncated file where the finished one
/// should be.
#[must_use]
pub fn temporary_path(destination: &Path) -> PathBuf {
    let name = destination.file_name().map_or_else(
        || "clipped".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    destination.with_file_name(format!("{name}.{}{TEMPORARY_SUFFIX}", std::process::id()))
}

/// Opens a file such that nobody else may open it while it is held.
///
/// The whole sweep rule rests on this. On a platform without the concept the
/// file is opened normally and the age check alone decides, which is a weaker
/// rule and not a wrong one — no such platform builds this crate's callers.
fn create_exclusive(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(path)
}

/// Writes `destination` by way of a temporary, replacing it only if the whole
/// write succeeds.
///
/// `contents` is handed the temporary file to write into. If it fails, or the
/// rename fails, the destination is left exactly as it was and the temporary is
/// removed.
///
/// Orphaned temporaries beside the destination are swept afterwards, on the
/// terms the module documentation sets out. That happens after the rename and
/// its result is discarded: a sweep may not cost somebody a write that
/// otherwise worked.
///
/// # Errors
///
/// Whatever creating, writing or renaming reported — for these files, the disk
/// filling or a directory that is not there.
pub fn write_atomically(
    destination: &Path,
    contents: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let temporary = temporary_path(destination);

    let outcome = (|| {
        let mut file = create_exclusive(&temporary)?;
        contents(&mut file)?;
        file.flush()?;
        // Dropped before the rename, because Windows will not rename a file
        // that is still open with no sharing — which is the same ordering trap
        // the replay spill directory has.
        drop(file);
        fs::rename(&temporary, destination)
    })();

    if outcome.is_err() {
        // The destination is untouched; this is only the half-written temporary.
        let _ = fs::remove_file(&temporary);
    }

    sweep_orphaned_temporaries(destination);
    outcome
}

/// Removes temporaries beside `destination` that no writer is holding and that
/// are old enough to be abandoned.
///
/// Returns how many went. Safe to call while other processes are writing the
/// same destination: a temporary one of them holds cannot be opened, so it is
/// left alone.
///
/// Never fails, for the reason the module documentation gives.
pub fn sweep_orphaned_temporaries(destination: &Path) -> usize {
    let Some(directory) = destination.parent() else {
        return 0;
    };
    let Some(stem) = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return 0;
    };
    let prefix = format!("{stem}.");
    let Ok(entries) = fs::read_dir(directory) else {
        return 0;
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if !name.starts_with(&prefix) || !name.ends_with(TEMPORARY_SUFFIX) {
            continue;
        }
        if !is_abandoned(&path) {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether a temporary is old enough and unheld enough to remove.
fn is_abandoned(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let old_enough = metadata
        .modified()
        .ok()
        .and_then(|written| SystemTime::now().duration_since(written).ok())
        .is_some_and(|age| age >= ORPHAN_AGE);
    if !old_enough {
        return false;
    }

    // Opening it exclusively is what proves nobody is writing it. `create` is
    // deliberately not set: a temporary that has gone in the meantime is not
    // one to make.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "clipped-atomic-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("a scratch directory can be made");
        directory
    }

    /// Makes a temporary that looks like one an older process left behind.
    fn abandoned(destination: &Path, owner: u32) -> PathBuf {
        let name = destination.file_name().expect("a name").to_string_lossy();
        let path = destination.with_file_name(format!("{name}.{owner}{TEMPORARY_SUFFIX}"));
        fs::write(&path, b"half a file").expect("the temporary can be written");
        age(&path, ORPHAN_AGE + Duration::from_secs(60));
        path
    }

    /// Backdates a file, so that an age rule can be tested without waiting.
    fn age(path: &Path, by: Duration) {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .expect("the file can be opened");
        let when = SystemTime::now() - by;
        file.set_modified(when).expect("its time can be set");
    }

    #[test]
    fn a_write_that_succeeds_replaces_the_destination() {
        let directory = scratch("success");
        let destination = directory.join("settings.json");

        write_atomically(&destination, |file| file.write_all(b"{}"))
            .expect("a write to a real directory succeeds");

        assert_eq!(
            fs::read_to_string(&destination).expect("the file is there"),
            "{}"
        );
        assert!(
            !temporary_path(&destination).exists(),
            "the temporary is renamed, not left beside it"
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_write_that_fails_leaves_the_destination_alone_and_takes_its_temporary_with_it() {
        let directory = scratch("failure");
        let destination = directory.join("settings.json");
        fs::write(&destination, b"the old contents").expect("a destination to protect");

        let error = write_atomically(&destination, |_| {
            Err(io::Error::other(
                "the caller could not produce the contents",
            ))
        })
        .expect_err("a write whose contents fail is a failed write");
        assert_eq!(error.kind(), io::ErrorKind::Other);

        assert_eq!(
            fs::read_to_string(&destination).expect("the file is still there"),
            "the old contents",
            "a failed write must not cost somebody the file they had"
        );
        assert!(
            !temporary_path(&destination).exists(),
            "and must not leave its own half-written temporary behind"
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_temporary_left_by_a_process_that_is_gone_is_removed_by_the_next_write() {
        // The defect this exists for: one of these per crash, for ever, in a
        // directory somebody occasionally looks at.
        let directory = scratch("orphan");
        let destination = directory.join("encoder-capabilities.json");
        let orphan = abandoned(&destination, 52_248);
        assert!(orphan.is_file());

        write_atomically(&destination, |file| file.write_all(b"{}")).expect("the write succeeds");

        assert!(
            !orphan.exists(),
            "an abandoned temporary has to go on the next successful write"
        );
        assert!(destination.is_file(), "and the write still happened");

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_temporary_somebody_is_writing_is_left_alone() {
        // The half that matters more. A second Clipped, or a concurrent probe,
        // is exactly what the identifier in the name exists to protect — and
        // holding the file is what proves it is being written rather than
        // abandoned.
        let directory = scratch("live");
        let destination = directory.join("library.db");
        let held = abandoned(&destination, 999_999);

        // Old enough to sweep, but held open with no sharing, exactly as
        // `write_atomically` holds its own.
        let open = create_exclusive(&held).expect("the temporary can be held");

        let removed = sweep_orphaned_temporaries(&destination);

        assert_eq!(removed, 0, "a held temporary is not abandoned");
        assert!(
            held.is_file(),
            "removing a temporary somebody is writing turns their successful write into a \
             failed one"
        );

        drop(open);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_temporary_that_is_merely_recent_is_left_alone() {
        // The window between a writer closing its temporary and renaming it.
        // Nothing holds the file then, and taking it would break their write.
        let directory = scratch("recent");
        let destination = directory.join("overlay.toml");
        let name = destination.file_name().expect("a name").to_string_lossy();
        let recent = destination.with_file_name(format!("{name}.4242{TEMPORARY_SUFFIX}"));
        fs::write(&recent, b"mid-write").expect("the temporary can be written");

        assert_eq!(sweep_orphaned_temporaries(&destination), 0);
        assert!(
            recent.is_file(),
            "a temporary written a moment ago is in use"
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_file_that_is_not_one_of_these_temporaries_is_never_touched() {
        let directory = scratch("bystanders");
        let destination = directory.join("settings.json");
        let bystanders = [
            directory.join("settings.json"),
            directory.join("other.json.1234.tmp"),
            directory.join("settings.json.backup"),
            directory.join("settings.jsonx.1234.tmp"),
        ];
        for path in &bystanders {
            fs::write(path, b"leave me").expect("a bystander can be written");
            age(path, ORPHAN_AGE + Duration::from_secs(60));
        }

        let removed = sweep_orphaned_temporaries(&destination);

        assert_eq!(removed, 0, "nothing here is one of this destination's");
        for path in &bystanders {
            assert!(path.is_file(), "{} was taken", path.display());
        }

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn sweeping_somewhere_that_does_not_exist_is_not_a_failure() {
        assert_eq!(
            sweep_orphaned_temporaries(&scratch("nowhere").join("gone").join("settings.json")),
            0
        );
    }
}
