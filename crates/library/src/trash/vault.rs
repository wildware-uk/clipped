//! The filesystem half of the trash: moving a file in, moving it back, and the
//! one place anything is ever unlinked.
//!
//! Nothing here knows about the database. It is the part that has to be right
//! when the drive is nearly full, which — because automatic cleanup is what
//! empties a full drive (issue #111) — is exactly when it runs.
//!
//! # Why a rename and never a copy
//!
//! Deleting is [`fs::rename`] into the trash. On one volume that is a directory
//! entry being rewritten: it needs **no free space**, it does not read or write
//! the file's contents, and it takes the same time for a 40 GB recording as for
//! an empty one. A copy-then-delete would need as much free space as the file
//! and would take minutes, on the one occasion — a disk with nothing left on it
//! — when neither is available. It would also mean holding two copies of
//! somebody's recording while a machine that is already in trouble decides
//! whether to keep going.
//!
//! The price is that a rename cannot cross a volume, so a library spread over
//! two drives needs a trash on each. That is refused explicitly
//! ([`TrashError::DifferentVolume`]) rather than silently falling back on a copy.
//!
//! # Why each file gets a directory of its own
//!
//! `Trash\20260812-091500\clipped-cs2-20260812-090000.mkv`. The alternative —
//! renaming the file itself to something unique — loses the name the user
//! recognises, which is the name the trash screen has to show and the name a
//! restore has to put back. A directory per item keeps the file exactly as it
//! was, which is also what makes "restored byte for byte" a property of the
//! filesystem rather than of this code.
//!
//! # The guard
//!
//! [`discard`] is the only function in the crate that unlinks a media file, and
//! it refuses a path that is not inside the trash directory. Everything that
//! destroys footage goes through it, so "never delete anything a user did not
//! ask to delete" is one function to review rather than a convention.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use time::OffsetDateTime;

use crate::accounting::roots::contains;
use crate::trash::entry::FileOutcome;
use crate::trash::TrashError;

/// How many entry directories may share one second before this gives up.
///
/// Reached only by deleting more than this many items inside one second, which
/// a person cannot do and a bulk operation can. Bounded so that a directory that
/// cannot be created for some *other* reason — a full disk, a permission — ends
/// as an error rather than as a loop.
const ENTRIES_PER_SECOND: u32 = 1_000;

/// How many times a restore will try a different name before giving up.
const RESTORE_ATTEMPTS: u32 = 1_000;

/// What the on-disk half of a delete produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stowed {
    /// The file was moved, and is now here.
    Moved(PathBuf),
    /// There was no file to move. The item's media had already gone.
    NoFile,
}

/// Moves `file` into a directory of its own inside `trash`.
///
/// `at` names the moment, and is what the directory is called. Answers
/// [`Stowed::NoFile`] when there is nothing at `file` — an item whose media the
/// user had already removed behind the application's back is still an item they
/// can delete from their library.
pub(crate) fn stow(trash: &Path, file: &Path, at: SystemTime) -> Result<Stowed, TrashError> {
    match fs::symlink_metadata(file) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Stowed::NoFile),
        // Any other error is *not* evidence of absence — a busy drive, a
        // permission — so the move is attempted and reports what really
        // happened.
        Err(_) | Ok(_) => {}
    }

    if !same_volume(file, trash) {
        return Err(TrashError::DifferentVolume {
            file: file.to_path_buf(),
            trash: trash.to_path_buf(),
        });
    }

    let name = file.file_name().unwrap_or_else(|| OsStr::new("recording"));
    let directory = create_entry_directory(trash, at)?;
    let destination = directory.join(name);

    if let Err(source) = fs::rename(file, &destination) {
        // The directory was made for this file and now holds nothing.
        let _ = fs::remove_dir(&directory);
        return Err(move_failure(file, &destination, trash, source));
    }
    Ok(Stowed::Moved(destination))
}

/// Moves `file` back to `destination`, or beside it when something is there.
///
/// Never overwrites. The occupant of `destination` is a file the user did not
/// ask to lose — most often the same recording restored from a backup, or a new
/// one written to a name that was free again — so a collision produces
/// `name (restored).mkv` and the caller reports where it went.
pub(crate) fn restore_to(file: &Path, destination: &Path) -> Result<PathBuf, TrashError> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            // The folder a recording came from can have been deleted while the
            // recording sat in the trash. Restoring it means putting it back
            // where it was, which means putting the folder back too.
            fs::create_dir_all(parent).map_err(|source| TrashError::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    let claimed = claim_free_name(destination)?;
    if let Err(source) = fs::rename(file, &claimed) {
        // The placeholder is this function's own; removing it leaves the
        // directory as it was found.
        let _ = fs::remove_file(&claimed);
        return Err(TrashError::Move {
            from: file.to_path_buf(),
            to: claimed,
            source,
        });
    }
    Ok(claimed)
}

/// Whether there is anything at `path`.
///
/// An error that is not "no such file" — a busy drive, a permission — answers
/// **true**, deliberately. The one thing that must not happen is a restore
/// deciding a recording is not there because Windows was busy, and quietly
/// clearing the row that says where it is.
pub(crate) fn is_there(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

/// Puts `file` back at `destination` exactly, for undoing a move this module
/// just made.
///
/// Unlike [`restore_to`] this never diverts, because the destination was
/// occupied by this very file a moment ago and nothing else can have claimed it.
/// Anything else would hide a compensating move that did not work.
pub(crate) fn move_back(file: &Path, destination: &Path) -> io::Result<()> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::rename(file, destination)
}

/// Unlinks a file from the trash. **The only place this crate deletes media.**
///
/// Anything that is not inside `trash` is left exactly where it is and reported
/// as [`FileOutcome::LeftInPlace`], which is what stops a path in a database row
/// from reaching a recording the user still has. That is not an error: an item
/// whose media had already gone when it was deleted names a path in the library
/// rather than in the trash, and its entry still has to be removable.
pub(crate) fn discard(trash: &Path, file: &Path) -> Result<FileOutcome, TrashError> {
    match fs::symlink_metadata(file) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tidy_entry_directory(trash, file);
            return Ok(FileOutcome::AlreadyGone);
        }
        Err(_) | Ok(_) => {}
    }

    if !contains(trash, file) {
        return Ok(FileOutcome::LeftInPlace);
    }

    fs::remove_file(file).map_err(|source| TrashError::Remove {
        path: file.to_path_buf(),
        source,
    })?;
    tidy_entry_directory(trash, file);
    Ok(FileOutcome::Deleted)
}

/// Removes the directory a file that has left the trash had to itself.
///
/// Called after a restore and after a compensating move, which are the two ways
/// a file leaves the trash without being destroyed. `moved_from` is where the
/// file *was*; a path that is not inside the trash — a restore's compensating
/// move, which puts a file back into the trash from the library — is ignored,
/// which is why this is safe to call either way round.
pub(crate) fn tidy(trash: &Path, moved_from: &Path) {
    tidy_entry_directory(trash, moved_from);
}

/// Removes the directory one trashed file had to itself, once it is empty.
///
/// Best effort and deliberately silent: `remove_dir` refuses a directory that
/// still holds something, which is the answer wanted for a trash entry that
/// somebody has put another file into by hand.
fn tidy_entry_directory(trash: &Path, file: &Path) {
    let Some(directory) = file.parent() else {
        return;
    };
    // Strictly inside: `contains` both ways round means the two paths are the
    // same directory, which is the trash itself and is not this to remove.
    if !contains(trash, directory) || contains(directory, trash) {
        return;
    }
    let _ = fs::remove_dir(directory);
}

/// Creates the directory this deletion's file will live in, and answers where it
/// is.
///
/// Named for the moment, with a counter when a second already has one, and
/// created with [`fs::create_dir`] rather than `create_dir_all` so that "does it
/// exist already?" is answered by the filesystem atomically instead of by a
/// check this code could lose a race on.
fn create_entry_directory(trash: &Path, at: SystemTime) -> Result<PathBuf, TrashError> {
    fs::create_dir_all(trash).map_err(|source| TrashError::CreateDirectory {
        path: trash.to_path_buf(),
        source,
    })?;

    let stamp = stamp(at);
    for attempt in 1..=ENTRIES_PER_SECOND {
        let candidate = trash.join(if attempt == 1 {
            stamp.clone()
        } else {
            format!("{stamp}-{attempt}")
        });
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(TrashError::CreateDirectory {
                    path: candidate,
                    source,
                })
            }
        }
    }

    Err(TrashError::CreateDirectory {
        path: trash.join(stamp),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{ENTRIES_PER_SECOND} trash entries already share this second"),
        ),
    })
}

/// Claims a free name at or beside `destination`, and answers which one.
///
/// The name is claimed by *creating* an empty file at it rather than by asking
/// whether one is there. `fs::rename` on Windows replaces an existing
/// destination, so a check followed by a rename would overwrite a file that
/// appeared in between; creating it exclusively first means the name cannot be
/// taken by anything else, and the rename then replaces this function's own
/// placeholder.
fn claim_free_name(destination: &Path) -> Result<PathBuf, TrashError> {
    for attempt in 1..=RESTORE_ATTEMPTS {
        let candidate = restored_name(destination, attempt);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(TrashError::CreateDirectory {
                    path: candidate,
                    source,
                })
            }
        }
    }

    Err(TrashError::CreateDirectory {
        path: restored_name(destination, RESTORE_ATTEMPTS),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "every candidate name beside the original was taken",
        ),
    })
}

/// The `attempt`th name to try when restoring to `destination`.
///
/// The first is the original, unchanged, which is what almost every restore
/// gets. The rest keep the extension so the file still opens by double-click:
/// `clipped-cs2.mkv`, `clipped-cs2 (restored).mkv`,
/// `clipped-cs2 (restored 2).mkv`.
fn restored_name(destination: &Path, attempt: u32) -> PathBuf {
    if attempt == 1 {
        return destination.to_path_buf();
    }

    let stem = destination
        .file_stem()
        .unwrap_or_else(|| OsStr::new("restored"));
    let suffix = if attempt == 2 {
        " (restored)".to_owned()
    } else {
        format!(" (restored {})", attempt - 1)
    };

    let mut name = OsString::from(stem);
    name.push(suffix);
    if let Some(extension) = destination.extension() {
        name.push(".");
        name.push(extension);
    }
    destination.with_file_name(name)
}

/// The directory name one deletion gets: `20260812-091500`.
///
/// UTC, unlike the timestamps in the database, and deliberately: this is a
/// filename rather than a moment anything compares, it has to be the same length
/// every time so that a directory listing sorts, and a folder whose name carries
/// an offset would be a second place the machine's time zone could be misread.
/// The moment that *matters* — what retention is judged from — is `deleted_at`
/// in the database, in local time like everything else.
fn stamp(at: SystemTime) -> String {
    let at = OffsetDateTime::from(at);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// The volume a path is on, as far as a path alone can say.
///
/// The drive letter or UNC share on Windows, and nothing anywhere else. `None`
/// for both paths means "no reason to think they differ", which is the right
/// answer on a filesystem with no such concept and for two relative paths.
fn volume_of(path: &Path) -> Option<OsString> {
    path.components()
        .next()
        .and_then(|component| match component {
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_ascii_lowercase()),
            _ => None,
        })
}

/// Whether two paths are on the same volume, judged from the paths alone.
///
/// Cheap, needs neither path to exist, and gives a message naming both drives
/// instead of an operating system error number. It cannot see a directory
/// mounted from another volume, which is why [`stow`] also translates the
/// rename's own answer — see [`move_failure`].
fn same_volume(left: &Path, right: &Path) -> bool {
    volume_of(left) == volume_of(right)
}

/// `ERROR_NOT_SAME_DEVICE`, which Windows answers a rename across volumes with.
///
/// The same number as `EXDEV` on Unix, and for the same reason.
const NOT_SAME_DEVICE: i32 = 17;

/// The error a failed move into the trash should carry.
///
/// A directory can be a mount point for another volume, in which case two paths
/// that share a drive letter are still on two volumes and only the rename knows.
/// Reporting that as a plain move failure would leave a user reading
/// "the system cannot move the file to a different disk drive" with no idea that
/// their trash is the different disk drive.
fn move_failure(file: &Path, destination: &Path, trash: &Path, source: io::Error) -> TrashError {
    if source.raw_os_error() == Some(NOT_SAME_DEVICE) {
        return TrashError::DifferentVolume {
            file: file.to_path_buf(),
            trash: trash.to_path_buf(),
        };
    }
    TrashError::Move {
        from: file.to_path_buf(),
        to: destination.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::test_support::scratch_directory;

    /// 2026-08-12T08:15:00Z.
    fn moment() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_522_500)
    }

    #[test]
    fn a_deletion_is_filed_under_the_moment_it_happened() {
        assert_eq!(stamp(moment()), "20260812-081500");
    }

    #[test]
    fn two_deletions_in_the_same_second_get_directories_of_their_own() {
        let directory = scratch_directory("vault-same-second");
        let trash = directory.join("Trash");

        let first = create_entry_directory(&trash, moment()).expect("the first entry");
        let second = create_entry_directory(&trash, moment()).expect("the second entry");

        assert_eq!(first.file_name().expect("a name"), "20260812-081500");
        assert_eq!(second.file_name().expect("a name"), "20260812-081500-2");
        assert!(first.is_dir() && second.is_dir());
    }

    #[test]
    fn a_deleted_file_keeps_the_name_the_user_knows_it_by() {
        let directory = scratch_directory("vault-keeps-name");
        let file = directory.join("clipped-cs2-20260812-090000.mkv");
        fs::write(&file, b"footage").expect("a recording can be written");
        let trash = directory.join("Trash");

        let stowed = stow(&trash, &file, moment()).expect("it is moved");

        let Stowed::Moved(path) = stowed else {
            panic!("a file that is there is moved, not skipped");
        };
        assert_eq!(
            path.file_name().expect("a name"),
            "clipped-cs2-20260812-090000.mkv"
        );
        assert!(!file.exists(), "the original is still there");
        assert_eq!(fs::read(&path).expect("it can be read"), b"footage");
    }

    #[test]
    fn an_item_with_no_file_is_reported_rather_than_failing() {
        let directory = scratch_directory("vault-no-file");
        let trash = directory.join("Trash");

        let stowed = stow(&trash, &directory.join("never-existed.mkv"), moment())
            .expect("a missing file is not an error");

        assert_eq!(stowed, Stowed::NoFile);
        assert!(
            !trash.exists(),
            "an entry directory was made for a file that does not exist"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_file_on_another_drive_is_refused_rather_than_copied() {
        // The whole reason deleting is a rename: a copy needs as much free space
        // as the file, on the one occasion — a full disk — when there is none.
        let directory = scratch_directory("vault-other-drive");
        let file = directory.join("clipped-cs2.mkv");
        fs::write(&file, b"footage").expect("a recording can be written");

        let elsewhere = Path::new(r"Z:\Clipped\Trash");
        assert!(
            !same_volume(&file, elsewhere),
            "this test's premise is gone: the scratch directory is on Z:"
        );

        let error =
            stow(elsewhere, &file, moment()).expect_err("a trash on another drive is refused");

        assert!(
            matches!(error, TrashError::DifferentVolume { .. }),
            "{error}"
        );
        assert!(file.exists(), "the recording was moved anyway");
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_letter_is_compared_without_regard_to_its_case() {
        assert!(same_volume(Path::new(r"C:\a"), Path::new(r"c:\b")));
        assert!(!same_volume(Path::new(r"C:\a"), Path::new(r"D:\a")));
    }

    #[test]
    fn restoring_over_something_else_puts_the_file_beside_it_instead() {
        let directory = scratch_directory("vault-occupied");
        let occupant = directory.join("clipped-cs2.mkv");
        fs::write(&occupant, b"somebody elses file").expect("the occupant can be written");
        let trashed = directory.join("trashed.mkv");
        fs::write(&trashed, b"the restored footage").expect("the trashed file can be written");

        let landed = restore_to(&trashed, &occupant).expect("it is restored");

        assert_eq!(
            landed.file_name().expect("a name"),
            "clipped-cs2 (restored).mkv"
        );
        assert_eq!(
            fs::read(&occupant).expect("it can be read"),
            b"somebody elses file",
            "restoring overwrote a file the user did not ask to lose"
        );
        assert_eq!(
            fs::read(&landed).expect("it can be read"),
            b"the restored footage"
        );
    }

    #[test]
    fn a_second_collision_is_numbered_rather_than_repeated() {
        let directory = scratch_directory("vault-occupied-twice");
        let occupant = directory.join("clipped-cs2.mkv");
        fs::write(&occupant, b"one").expect("a file");
        fs::write(directory.join("clipped-cs2 (restored).mkv"), b"two").expect("a file");
        let trashed = directory.join("trashed.mkv");
        fs::write(&trashed, b"three").expect("a file");

        let landed = restore_to(&trashed, &occupant).expect("it is restored");

        assert_eq!(
            landed.file_name().expect("a name"),
            "clipped-cs2 (restored 2).mkv"
        );
    }

    #[test]
    fn restoring_recreates_a_folder_that_has_gone() {
        let directory = scratch_directory("vault-folder-gone");
        let trashed = directory.join("trashed.mkv");
        fs::write(&trashed, b"footage").expect("a file");
        let destination = directory.join("Counter-Strike 2").join("clipped-cs2.mkv");

        let landed = restore_to(&trashed, &destination).expect("it is restored");

        assert_eq!(landed, destination);
        assert_eq!(fs::read(&landed).expect("it can be read"), b"footage");
    }

    #[test]
    fn nothing_outside_the_trash_is_ever_unlinked() {
        // The guard behind "never delete anything a user did not ask to
        // delete": a row naming a path outside the trash cannot reach
        // `remove_file`.
        let directory = scratch_directory("vault-guard");
        let trash = directory.join("Trash");
        fs::create_dir_all(&trash).expect("a trash directory");
        let elsewhere = directory.join("still-in-the-library.mkv");
        fs::write(&elsewhere, b"footage").expect("a recording");

        let outcome = discard(&trash, &elsewhere).expect("it is refused rather than failing");

        assert_eq!(outcome, FileOutcome::LeftInPlace);
        assert!(elsewhere.exists(), "a file outside the trash was deleted");
    }

    #[test]
    fn discarding_removes_the_file_and_the_directory_it_had_to_itself() {
        let directory = scratch_directory("vault-discard");
        let file = directory.join("clipped-cs2.mkv");
        fs::write(&file, b"footage").expect("a recording");
        let trash = directory.join("Trash");
        let Stowed::Moved(stowed) = stow(&trash, &file, moment()).expect("it is moved") else {
            panic!("a file that is there is moved");
        };
        let entry = stowed.parent().expect("an entry directory").to_path_buf();

        let outcome = discard(&trash, &stowed).expect("it is discarded");

        assert_eq!(outcome, FileOutcome::Deleted);
        assert!(!stowed.exists());
        assert!(!entry.exists(), "the entry directory was left behind");
        assert!(trash.exists(), "the trash itself was removed");
    }

    #[test]
    fn a_trash_entry_somebody_has_put_another_file_into_is_left_alone() {
        // `remove_dir` refuses a directory that still holds something, which is
        // the answer wanted: the other file is not this module's to destroy.
        let directory = scratch_directory("vault-shared-entry");
        let file = directory.join("clipped-cs2.mkv");
        fs::write(&file, b"footage").expect("a recording");
        let trash = directory.join("Trash");
        let Stowed::Moved(stowed) = stow(&trash, &file, moment()).expect("it is moved") else {
            panic!("a file that is there is moved");
        };
        let entry = stowed.parent().expect("an entry directory").to_path_buf();
        let intruder = entry.join("notes.txt");
        fs::write(&intruder, b"mine").expect("another file");

        discard(&trash, &stowed).expect("it is discarded");

        assert!(intruder.exists(), "somebody else's file was deleted");
    }

    #[cfg(windows)]
    #[test]
    fn a_path_windows_will_not_answer_for_is_assumed_to_hold_a_file() {
        // The one thing that must not happen is a restore deciding a recording
        // is not there because Windows would not answer, and then clearing the
        // row that says where it is. `|` is not legal in a Windows file name,
        // so asking about it fails with something that is deliberately not
        // "no such file".
        let directory = scratch_directory("vault-illegible");
        let path = directory.join("a|b.mkv");
        let answer = fs::symlink_metadata(&path);
        assert!(
            matches!(&answer, Err(error) if error.kind() != io::ErrorKind::NotFound),
            "this test's premise is gone: {answer:?}"
        );

        assert!(is_there(&path));
    }

    #[test]
    fn discarding_something_that_has_already_gone_is_not_a_failure() {
        let directory = scratch_directory("vault-already-gone");
        let trash = directory.join("Trash");
        fs::create_dir_all(&trash).expect("a trash directory");

        let outcome = discard(&trash, &trash.join("20260812-081500").join("gone.mkv"))
            .expect("an absent file is not an error");

        assert_eq!(outcome, FileOutcome::AlreadyGone);
    }

    #[test]
    fn a_rename_that_failed_because_of_a_mount_point_is_named_as_a_drive_problem() {
        // Two paths can share a drive letter and still be on two volumes, and
        // only the rename knows. The message has to say so.
        let error = move_failure(
            Path::new(r"C:\Videos\a.mkv"),
            Path::new(r"C:\Videos\Trash\20260812-081500\a.mkv"),
            Path::new(r"C:\Videos\Trash"),
            io::Error::from_raw_os_error(NOT_SAME_DEVICE),
        );

        assert!(
            matches!(error, TrashError::DifferentVolume { .. }),
            "{error}"
        );
    }
}
