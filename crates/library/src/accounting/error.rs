//! What storage accounting refuses, and why.
//!
//! Three kinds of refusal, kept apart because a caller answers each one
//! differently. A [`RootsError`] is a mistake in how the library was described
//! and is fixed by describing it correctly. A [`LimitError`] is a setting a user
//! typed that cannot be satisfied, and its message has to say what would be
//! acceptable instead — an error a person reads in a settings screen is worth
//! more than a code (AGENTS.md section 45). A [`VolumeError`] is a fact about the
//! machine right now: the drive is not there, which is a state to handle rather
//! than a bug to report (AGENTS.md section 16).
//!
//! A scan produces none of these. It cannot fail as a whole: a root it could not
//! read is reported as
//! [`UnavailableRoot`](crate::accounting::UnavailableRoot) inside the inventory,
//! and the inventory says it is partial. Losing one drive of a two-drive library
//! must still measure the other one.

use core::fmt;
use core::time::Duration;
use std::path::PathBuf;

/// A directory that cannot be part of a set of storage roots.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RootsError {
    /// The path is relative.
    NotAbsolute {
        /// What was given.
        path: PathBuf,
    },
    /// The path contains, is contained by, or is equal to a root already
    /// declared, so its files would be counted twice.
    Overlapping {
        /// The root that was already there.
        existing: PathBuf,
        /// The one that would have overlapped it.
        added: PathBuf,
    },
}

impl fmt::Display for RootsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsolute { path } => write!(
                formatter,
                "a storage root must be an absolute path, and '{}' is not",
                path.display()
            ),
            Self::Overlapping { existing, added } => write!(
                formatter,
                "'{}' overlaps the storage root '{}', so its files would be counted twice",
                added.display(),
                existing.display()
            ),
        }
    }
}

impl core::error::Error for RootsError {}

/// A storage limit that cannot be satisfied as configured.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitError {
    /// A maximum usage below what any library could work within.
    ///
    /// Zero is the case that matters: it would mean everything is over quota
    /// from the first recording. "No limit" is expressed by not setting one, not
    /// by setting it to nothing.
    QuotaTooSmall {
        /// What was asked for, in bytes.
        requested: u64,
        /// The smallest quota that may be configured, in bytes.
        minimum: u64,
    },
    /// A minimum free space, or a maximum usage, larger than the volume itself.
    ///
    /// The example in issue #93's acceptance criteria: a minimum free space that
    /// exceeds the disk is a limit that can never be met, so every recording
    /// would start against a breached limit.
    LargerThanVolume {
        /// The limit that does not fit.
        limit: &'static str,
        /// What was asked for, in bytes.
        requested: u64,
        /// The size of the volume it was configured against, in bytes.
        volume: u64,
    },
    /// A maximum recording age shorter than the floor.
    ///
    /// Zero would mean every recording is over-age the moment it is written,
    /// which combined with issue #111 is a setting that deletes a library. A
    /// user who wants that deletes their recordings themselves (AGENTS.md
    /// section 56).
    AgeTooShort {
        /// What was asked for.
        requested: Duration,
        /// The shortest age that may be configured.
        minimum: Duration,
    },
}

impl fmt::Display for LimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QuotaTooSmall { requested, minimum } => write!(
                formatter,
                "a maximum usage of {} is too small to hold a library; \
                 the smallest that can be set is {}, and leaving it unset means no limit",
                gigabytes(*requested),
                gigabytes(*minimum)
            ),
            Self::LargerThanVolume {
                limit,
                requested,
                volume,
            } => write!(
                formatter,
                "a {limit} of {} does not fit on a {} drive",
                gigabytes(*requested),
                gigabytes(*volume)
            ),
            Self::AgeTooShort { requested, minimum } => write!(
                formatter,
                "a maximum recording age of {} is shorter than the shortest that can be set, {}",
                days(*requested),
                days(*minimum)
            ),
        }
    }
}

impl core::error::Error for LimitError {}

/// Why a volume's size and free space could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VolumeError {
    /// The operating system refused the question.
    ///
    /// Overwhelmingly this is a drive that is not connected, which is a state
    /// the application handles rather than an error it reports: the recording
    /// location is on a disk that is not there.
    Unreadable {
        /// The path that was asked about.
        path: PathBuf,
        /// What the operating system said.
        reason: String,
    },
    /// This build cannot ask.
    ///
    /// Clipped is a Windows application (AGENTS.md section 50), and free space
    /// is the one thing in this module that needs the platform. Every other
    /// part of accounting compiles and runs its tests anywhere, which is what
    /// keeps that boundary checkable rather than claimed.
    Unsupported,
}

impl fmt::Display for VolumeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, reason } => write!(
                formatter,
                "the drive holding '{}' could not be read: {reason}",
                path.display()
            ),
            Self::Unsupported => {
                formatter.write_str("free disk space can only be measured on Windows")
            }
        }
    }
}

impl core::error::Error for VolumeError {}

/// A byte count in gigabytes, for a message a user reads.
///
/// Decimal GB — 1000³ — because that is what a drive is sold as and what the
/// settings screen asks for. Memory figures elsewhere in this workspace are in
/// binary units and say so; mixing the two silently is the confusion worth not
/// adding to.
fn gigabytes(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let gigabytes = bytes as f64 / 1_000_000_000.0;
    if gigabytes < 0.1 {
        format!("{bytes} bytes")
    } else {
        format!("{gigabytes:.1} GB")
    }
}

/// A duration in days, for a message a user reads.
fn days(age: Duration) -> String {
    let days = age.as_secs() / 86_400;
    match days {
        0 => format!("{} hours", age.as_secs() / 3_600),
        1 => "1 day".to_owned(),
        _ => format!("{days} days"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_root_says_which_path_was_wrong() {
        let error = RootsError::NotAbsolute {
            path: PathBuf::from("Recordings"),
        };

        assert_eq!(
            error.to_string(),
            "a storage root must be an absolute path, and 'Recordings' is not"
        );
    }

    #[test]
    fn an_overlap_names_both_directories_and_the_consequence() {
        let error = RootsError::Overlapping {
            existing: PathBuf::from("/clips"),
            added: PathBuf::from("/clips/trash"),
        };

        assert_eq!(
            error.to_string(),
            "'/clips/trash' overlaps the storage root '/clips', \
             so its files would be counted twice"
        );
    }

    #[test]
    fn a_limit_larger_than_the_drive_says_both_numbers() {
        // The acceptance criterion's own example. A user reads this in a
        // settings screen, so it has to say what does not fit and what it does
        // not fit on (AGENTS.md section 45).
        let error = LimitError::LargerThanVolume {
            limit: "minimum free space",
            requested: 900_000_000_000,
            volume: 500_000_000_000,
        };

        assert_eq!(
            error.to_string(),
            "a minimum free space of 900.0 GB does not fit on a 500.0 GB drive"
        );
    }

    #[test]
    fn a_quota_of_nothing_is_explained_as_the_difference_from_no_quota() {
        let error = LimitError::QuotaTooSmall {
            requested: 0,
            minimum: 1_000_000_000,
        };

        assert_eq!(
            error.to_string(),
            "a maximum usage of 0 bytes is too small to hold a library; \
             the smallest that can be set is 1.0 GB, and leaving it unset means no limit"
        );
    }

    #[test]
    fn an_age_refusal_is_stated_in_days() {
        let error = LimitError::AgeTooShort {
            requested: Duration::from_secs(3_600),
            minimum: Duration::from_secs(86_400),
        };

        assert_eq!(
            error.to_string(),
            "a maximum recording age of 1 hours is shorter than the shortest that can be set, 1 day"
        );
    }

    #[test]
    fn an_unreadable_drive_reads_as_a_drive_problem_rather_than_a_code() {
        let error = VolumeError::Unreadable {
            path: PathBuf::from(r"E:\Clipped"),
            reason: "The device is not ready. (os error 21)".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            r"the drive holding 'E:\Clipped' could not be read: The device is not ready. (os error 21)"
        );
    }
}
