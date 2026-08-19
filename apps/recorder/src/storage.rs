//! `clipped-recorder storage`: what the library occupies, and what automatic
//! cleanup would do about it.
//!
//! # Why a command that only looks
//!
//! Automatic cleanup deletes user footage, which
//! [issue #111](https://github.com/wildware-uk/clipped/issues/111) calls the
//! most dangerous thing this application does. Its Scope asks for two things
//! this provides and nothing else does:
//!
//! - **A dry run.** What *would* be deleted, before anybody trusts a limit with
//!   their recordings. It is the same measurement the sweep takes —
//!   `clipped_session::cleanup::preview` — so it cannot disagree with what
//!   actually happens; the sweep is that measurement plus the last step.
//! - **A path to review large recordings**, so a user can act before automatic
//!   deletion does. That is the list at the end, largest first, and it is worth
//!   printing even when no limit is set: "400 GB of recordings and no limit" is
//!   the most useful thing this can say to somebody deciding whether to set one.
//!
//! # It never deletes
//!
//! There is no `--apply`. Deleting is the sweep's, it happens after a
//! reconciliation, and a second way to start one would be a second place for
//! the decision to live. This reads the database, measures the disk, and prints.

use clipped_session::cleanup::{preview, Measurement, Skipped};
use clipped_session::config::{Configuration, ConfigurationStore};
use clipped_storage::Database;

use crate::cli::StorageArgs;

/// Why the report could not be produced.
#[derive(Debug)]
pub enum StorageError {
    /// This machine describes no per-user directory, so there is no library.
    NoLibrary,
    /// The library index could not be opened.
    Database(String),
    /// There is nowhere the recordings would be.
    NoRecordings,
    /// A measurement could not be taken.
    Measurement(Skipped),
}

impl core::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoLibrary => formatter.write_str(
                "this account has no application data directory, so Clipped has no library here",
            ),
            Self::Database(reason) => {
                write!(formatter, "the library index could not be opened: {reason}")
            }
            Self::NoRecordings => formatter.write_str(
                "Clipped could not work out where your recordings are; pass --directory",
            ),
            Self::Measurement(reason) => write!(
                formatter,
                "the library could not be measured: {}",
                describe(reason)
            ),
        }
    }
}

impl std::error::Error for StorageError {}

/// What a skipped measurement means, in the words a person reads.
///
/// `pub(crate)` rather than private: the `get_storage` handler refuses with the
/// same sentence, and a second wording for the same four failures would be two
/// answers to one question (AGENTS.md section 55). What a person is told about a
/// drive that could not be measured must not depend on whether they asked from
/// the terminal or from the window.
pub(crate) fn describe(reason: &Skipped) -> String {
    match reason {
        Skipped::NoLimit => "no storage limit is configured".to_owned(),
        Skipped::Roots(detail) => format!("the directories could not be declared: {detail}"),
        Skipped::Volume(detail) => format!("the drive could not be measured: {detail}"),
        Skipped::Candidates(detail) => format!("the index could not be read: {detail}"),
    }
}

/// Prints the report.
///
/// # Errors
///
/// [`StorageError`] when there is nothing to measure or the measurement failed.
pub fn run(args: &StorageArgs) -> Result<(), StorageError> {
    let recordings = args
        .directory
        .clone()
        .or_else(crate::config::default_output_directory)
        .ok_or(StorageError::NoRecordings)?;

    let library = clipped_logging::application_directory()
        .map(|directory| directory.join("library.db"))
        .ok_or(StorageError::NoLibrary)?;
    let database =
        Database::open(&library).map_err(|error| StorageError::Database(error.to_string()))?;

    let configuration =
        crate::watch::load_configuration(ConfigurationStore::default_path().as_deref());

    let measured = preview(
        &configuration,
        &recordings,
        &database,
        std::time::SystemTime::now(),
    )
    .map_err(StorageError::Measurement)?;

    print!("{}", render(&measured, &recordings, &configuration));
    Ok(())
}

/// The report, as text.
///
/// Separated from [`run`] so that what it says can be tested without a disk, a
/// database or a settings file — the same split every other subcommand here
/// makes between measuring and saying.
#[must_use]
pub fn render(
    measured: &Measurement,
    recordings: &std::path::Path,
    configuration: &Configuration,
) -> String {
    let mut out = String::new();

    out.push_str(&format!("Recordings  {}\n", recordings.display()));
    out.push_str(&format!("Trash       {}\n", measured.trash.display()));
    out.push_str(&format!(
        "Using       {} of the drive, with {} free\n",
        gigabytes(measured.usage_bytes),
        gigabytes(measured.free_bytes)
    ));
    for (category, bytes) in &measured.by_category {
        if *bytes > 0 {
            out.push_str(&format!("  {category:<14} {}\n", gigabytes(*bytes)));
        }
    }

    out.push_str("\nLimits      ");
    let storage = configuration.storage();
    if measured.limits.is_unlimited() {
        out.push_str(
            "none set, so nothing is ever deleted automatically.\n            \
             `docs/configuration.md` has the three keys that set one.\n",
        );
    } else {
        out.push('\n');
        if let Some(bytes) = storage.maximum_usage() {
            out.push_str(&format!("  maximum size   {}\n", gigabytes(bytes)));
        }
        if let Some(bytes) = storage.minimum_free_space() {
            out.push_str(&format!("  keep free      {}\n", gigabytes(bytes)));
        }
        if let Some(days) = storage.maximum_age_days() {
            out.push_str(&format!("  maximum age    {days} days\n"));
        }
    }

    out.push('\n');
    if measured.plan.deletions.is_empty() {
        out.push_str("Nothing would be deleted.\n");
    } else {
        out.push_str(&format!(
            "A sweep would move {} recording(s) to the trash, freeing {}:\n",
            measured.plan.deletions.len(),
            gigabytes(measured.plan.reclaimed_bytes)
        ));
        for candidate in &measured.plan.deletions {
            out.push_str(&format!(
                "  {:>10}  {}  {}\n",
                gigabytes(candidate.size_bytes),
                day_of(&candidate.started_at),
                candidate.path.display()
            ));
        }
        if measured.plan.still_over_limit > 0 {
            out.push_str(&format!(
                "  and it would still be {} over, because everything else is protected.\n",
                gigabytes(measured.plan.still_over_limit)
            ));
        }
    }

    let protected = measured.plan.protected.len();
    if protected > 0 {
        out.push_str(&format!(
            "{protected} recording(s) are protected and would never be taken.\n"
        ));
    }

    if !measured.largest.is_empty() {
        out.push_str("\nLargest recordings\n");
        for candidate in measured.largest.iter().take(LARGEST) {
            out.push_str(&format!(
                "  {:>10}  {}  {}{}\n",
                gigabytes(candidate.size_bytes),
                day_of(&candidate.started_at),
                candidate.path.display(),
                candidate
                    .protection
                    .as_ref()
                    .map_or_else(String::new, |_| "  (protected)".to_owned())
            ));
        }
    }

    out
}

/// How many recordings the review list holds.
///
/// Ten: enough to find what is filling a drive, short enough to read without
/// paging. Somebody who wants all of them wants the library screen.
const LARGEST: usize = 10;

/// A byte count in a unit a person can read.
///
/// Gigabytes for a library and megabytes for a short recording, because a
/// column of `0.00 GB` says nothing about which of them is filling a drive —
/// which is the one question this report exists to answer.
fn gigabytes(bytes: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a drive's size in bytes is exact in f64 well past any real one"
    )]
    let value = bytes as f64;

    if value >= 1_000_000_000.0 {
        let gigabytes = value / 1_000_000_000.0;
        if gigabytes < 10.0 {
            format!("{gigabytes:.2} GB")
        } else {
            format!("{gigabytes:.1} GB")
        }
    } else if value >= 1_000_000.0 {
        format!("{:.1} MB", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.0} kB", value / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

/// The day part of an RFC 3339 timestamp, which is all this report shows.
fn day_of(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipped_library::accounting::cleanup::{Candidate, CleanupPlan, Protection};
    use clipped_library::accounting::{StorageCategory, StorageLimits};
    use clipped_library::trash::TrashItem;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn candidate(
        size: u64,
        started: &str,
        path: &str,
        protection: Option<Protection>,
    ) -> Candidate {
        Candidate {
            item: TrashItem::recording(1),
            path: PathBuf::from(path),
            size_bytes: size,
            started_at: started.to_owned(),
            protection,
        }
    }

    fn measured(limits: StorageLimits, plan: CleanupPlan, largest: Vec<Candidate>) -> Measurement {
        let mut by_category = BTreeMap::new();
        by_category.insert(StorageCategory::Recordings, 400_000_000_000);
        Measurement {
            usage_bytes: 400_000_000_000,
            by_category,
            free_bytes: 88_000_000_000,
            limits,
            plan,
            largest,
            trash: PathBuf::from(r"D:\Clips.trash"),
        }
    }

    #[test]
    fn a_library_with_no_limit_says_so_and_still_shows_what_it_holds() {
        // The review path, and the reason it runs without a limit: somebody
        // deciding whether to set one needs to see what they have.
        let report = render(
            &measured(
                StorageLimits::unlimited(),
                CleanupPlan::nothing_to_do(),
                vec![candidate(
                    42_100_000_000,
                    "2026-03-14T20:00:00+00:00",
                    r"D:\Clips\clipped-cs2.mkv",
                    None,
                )],
            ),
            std::path::Path::new(r"D:\Clips"),
            &Configuration::defaults(),
        );

        assert!(report.contains("none set"), "{report}");
        assert!(report.contains("Nothing would be deleted."), "{report}");
        assert!(
            report.contains("Largest recordings") && report.contains("42.1 GB"),
            "a library with no limit still shows what is filling it: {report}"
        );
        assert!(report.contains("2026-03-14"), "{report}");
    }

    #[test]
    fn a_dry_run_names_every_recording_it_would_take() {
        // The whole point: before anybody trusts a limit with their footage,
        // this says exactly which files it would move and what that frees.
        let plan = CleanupPlan {
            deletions: vec![
                candidate(
                    30_000_000_000,
                    "2026-01-02T10:00:00+00:00",
                    r"D:\Clips\old-one.mkv",
                    None,
                ),
                candidate(
                    31_200_000_000,
                    "2026-01-03T10:00:00+00:00",
                    r"D:\Clips\old-two.mkv",
                    None,
                ),
            ],
            protected: vec![candidate(
                9_000_000_000,
                "2026-01-01T10:00:00+00:00",
                r"D:\Clips\favourite.mkv",
                Some(Protection::Favourite),
            )],
            reclaimed_bytes: 61_200_000_000,
            still_over_limit: 0,
        };

        let report = render(
            &measured(
                StorageLimits::unlimited()
                    .with_maximum_usage(500_000_000_000)
                    .expect("half a terabyte is above the floor"),
                plan,
                Vec::new(),
            ),
            std::path::Path::new(r"D:\Clips"),
            &Configuration::defaults(),
        );

        assert!(report.contains("would move 2 recording(s)"), "{report}");
        assert!(
            report.contains("old-one.mkv") && report.contains("old-two.mkv"),
            "{report}"
        );
        assert!(report.contains("61.2 GB"), "{report}");
        assert!(
            report.contains("1 recording(s) are protected"),
            "a favourite is never taken and the report has to say so: {report}"
        );
    }

    #[test]
    fn a_sweep_that_cannot_finish_says_how_far_short_it_would_fall() {
        // "Cleanup ran and you are still over" is a different thing from
        // "cleanup ran", and a user whose drive is full of favourites needs to
        // be told which one they have.
        let plan = CleanupPlan {
            deletions: vec![candidate(
                1_000_000_000,
                "2026-01-02T10:00:00+00:00",
                r"D:\Clips\one.mkv",
                None,
            )],
            protected: Vec::new(),
            reclaimed_bytes: 1_000_000_000,
            still_over_limit: 40_000_000_000,
        };

        let report = render(
            &measured(StorageLimits::unlimited(), plan, Vec::new()),
            std::path::Path::new(r"D:\Clips"),
            &Configuration::defaults(),
        );

        assert!(
            report.contains("still be 40.0 GB over"),
            "the shortfall has to be said, not implied: {report}"
        );
    }
}
