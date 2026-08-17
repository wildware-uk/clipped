//! What the index does when a file is not where it says it is.
//!
//! Users move, rename and delete recordings behind the application's back, and
//! a library that assumed otherwise would either show rows that do nothing when
//! clicked or — far worse — tidy itself up by deleting them. Neither happens
//! here. One rule, and it is AGENTS.md section 56:
//!
//! > **A recording that has gone is marked, never removed, and one that comes
//! > back is unmarked.**
//!
//! `missing_since` is that mark. It holds the moment the file was *first* found
//! to be absent and does not move while it stays absent, so "gone since
//! Tuesday" survives Wednesday's reconciliation. Nothing in this crate deletes
//! a row or a file, and `crates/library/tests/reconciliation.rs` provokes every
//! path here against a real database and real files to hold it to that.
//!
//! # What is deliberately not judged
//!
//! - **A file under a root that was not walked**, or under one that could not be
//!   read. An unplugged drive is not evidence of deletion (`super::scan`).
//! - **A row in the trash.** `deleted_at` means the user deleted it and the file
//!   was moved rather than unlinked (SPEC.md section 28); its absence from where
//!   it used to be is the expected outcome of that, not a discovery. The trash
//!   is #94 and owns those rows.

use std::fs;
use std::path::Path;

/// What reconciliation decided about one file, and what changed by deciding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Judgement {
    /// The value `missing_since` should hold afterwards.
    pub(crate) missing_since: Option<String>,
    /// The file was there before and is not now.
    pub(crate) newly_missing: bool,
    /// The file was missing and has come back.
    pub(crate) returned: bool,
}

/// Decides what a row's `missing_since` should be, given what is on disk.
///
/// `observed_at` is when this reconciliation ran, and is the only moment this
/// crate ever writes into the database.
pub(crate) fn judge(
    present: bool,
    deleted_at: Option<&str>,
    missing_since: Option<&str>,
    observed_at: &str,
) -> Judgement {
    if deleted_at.is_some() {
        // The trash's row. Left exactly as it is, including a mark it already
        // carries.
        return Judgement {
            missing_since: missing_since.map(str::to_owned),
            newly_missing: false,
            returned: false,
        };
    }

    match (present, missing_since) {
        (true, None) => Judgement {
            missing_since: None,
            newly_missing: false,
            returned: false,
        },
        (true, Some(_)) => Judgement {
            missing_since: None,
            newly_missing: false,
            returned: true,
        },
        (false, None) => Judgement {
            missing_since: Some(observed_at.to_owned()),
            newly_missing: true,
            returned: false,
        },
        // Still gone. The mark keeps the moment it was first noticed rather
        // than being reset to now on every run, because "missing since
        // Tuesday" is the useful fact and "missing since a second ago" is not.
        (false, Some(first)) => Judgement {
            missing_since: Some(first.to_owned()),
            newly_missing: false,
            returned: false,
        },
    }
}

/// What the filesystem says about one media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileFacts {
    /// Whether the file is there.
    pub(crate) present: bool,
    /// Its size in bytes, when it is.
    pub(crate) size_bytes: Option<i64>,
}

/// Looks at `path`, without opening it.
///
/// Nothing in the library ever opens a media file (`docs/storage.md`), so this
/// is `stat` and nothing more. An error that is not "no such file" — a
/// permission failure, a drive that answered badly — is treated as **present
/// but unmeasured**, deliberately: the one thing that must not happen is a file
/// that exists being marked missing because Windows was busy.
pub(crate) fn look_at(path: &Path) -> FileFacts {
    match fs::metadata(path) {
        Ok(metadata) => FileFacts {
            present: true,
            size_bytes: i64::try_from(metadata.len()).ok(),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileFacts {
            present: false,
            size_bytes: None,
        },
        Err(error) => {
            tracing::debug!(
                path = %path.display(),
                %error,
                "a recording could not be measured, and is assumed to still be there"
            );
            FileFacts {
                present: true,
                size_bytes: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::scratch_directory;

    const NOW: &str = "2026-08-12T09:00:00+01:00";
    const TUESDAY: &str = "2026-08-11T14:00:00+01:00";

    #[test]
    fn a_file_that_has_gone_is_marked_with_the_moment_it_was_noticed() {
        let judged = judge(false, None, None, NOW);

        assert_eq!(judged.missing_since.as_deref(), Some(NOW));
        assert!(judged.newly_missing);
    }

    #[test]
    fn a_file_that_is_still_gone_keeps_the_day_it_went_rather_than_todays_date() {
        let judged = judge(false, None, Some(TUESDAY), NOW);

        assert_eq!(judged.missing_since.as_deref(), Some(TUESDAY));
        assert!(
            !judged.newly_missing,
            "the second run must not report the same loss again"
        );
    }

    #[test]
    fn a_file_that_comes_back_is_unmarked() {
        let judged = judge(true, None, Some(TUESDAY), NOW);

        assert_eq!(judged.missing_since, None);
        assert!(judged.returned);
    }

    #[test]
    fn a_row_in_the_trash_is_left_alone() {
        // Its file was moved on purpose. Marking it missing would have the trash
        // screen show every deleted item as a problem.
        let judged = judge(false, Some("2026-08-10T10:00:00+01:00"), None, NOW);

        assert_eq!(judged.missing_since, None);
        assert!(!judged.newly_missing && !judged.returned);
    }

    #[test]
    fn a_file_that_is_there_is_measured() {
        let directory = scratch_directory("presence-measure");
        let path = directory.join("clipped-a.mkv");
        fs::write(&path, [0u8; 2048]).expect("a file can be written");

        let facts = look_at(&path);

        assert!(facts.present);
        assert_eq!(facts.size_bytes, Some(2048));
    }

    #[test]
    fn a_file_that_is_not_there_is_reported_absent_rather_than_zero_sized() {
        let directory = scratch_directory("presence-absent");

        let facts = look_at(&directory.join("never-existed.mkv"));

        assert!(!facts.present);
        assert_eq!(facts.size_bytes, None);
    }
}
