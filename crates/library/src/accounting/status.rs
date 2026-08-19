//! Whether the configured limits are satisfied right now.
//!
//! What a recording about to start asks — SPEC.md section 27 wants the user
//! warned *before* capture begins if a limit is already breached, not after a
//! session has filled the disk — and what the settings screen shows.
//!
//! # A limit can be breached, satisfied, or unknown
//!
//! The third state is the one worth being careful about, and it is why this is
//! not a boolean. A scan that was cancelled under-reports usage; a drive that is
//! not connected reports no free space at all. Answering "satisfied" from either
//! is how a quota comes to say "plenty of room" while a disk fills, and — with
//! issue #111 behind it — how a cleanup comes to delete from a library it only
//! half measured. So an evaluation that cannot be made is reported as
//! [`Unknown`] with the reason, and never as satisfied.
//!
//! One asymmetry makes partial measurements more useful than they sound:
//! **incomplete evidence can prove a breach, but not the absence of one.** Usage
//! only grows as more of the library is seen, so a partial scan that has already
//! passed the quota has proved the quota is passed. The same holds for the age
//! limit: a file older than the limit is older than the limit whether or not the
//! rest of the library was measured. This module says so rather than throwing
//! the finding away.

use core::fmt;
use core::time::Duration;
use std::time::SystemTime;

use crate::accounting::inventory::{Completeness, StorageInventory};
use crate::accounting::limits::StorageLimits;
use crate::accounting::volume::VolumeCapacity;

/// Which of the three configurable limits something is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LimitKind {
    /// The maximum the library may occupy.
    MaximumUsage,
    /// The minimum that must stay free on the volume.
    MinimumFreeSpace,
    /// The maximum age of a recording.
    MaximumAge,
}

impl LimitKind {
    /// A short lower-case name, for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaximumUsage => "maximum-usage",
            Self::MinimumFreeSpace => "minimum-free-space",
            Self::MaximumAge => "maximum-age",
        }
    }
}

impl fmt::Display for LimitKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A limit that is not being met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Breach {
    /// The library occupies more than its maximum.
    OverMaximumUsage {
        /// What it occupies, in bytes.
        used: u64,
        /// What it may occupy, in bytes.
        limit: u64,
    },
    /// Less than the configured minimum is free on the volume.
    ///
    /// Not necessarily Clipped's doing: everything else on the disk counts
    /// towards it, so this can be breached by a library that has not grown at
    /// all. It is a fact about the disk, and the reason SPEC.md section 27 has
    /// the setting at all.
    BelowMinimumFreeSpace {
        /// What is free, in bytes.
        free: u64,
        /// What must stay free, in bytes.
        required: u64,
    },
    /// Files older than the maximum age exist.
    ///
    /// Counted over the files a limit may select
    /// ([`StorageCategory::is_cleanup_candidate`](crate::accounting::StorageCategory::is_cleanup_candidate)),
    /// so an install that has been running for a year does not report its own
    /// database and rotated logs as over-age footage.
    OverMaximumAge {
        /// How many.
        files: usize,
        /// What they occupy, in bytes.
        bytes: u64,
        /// The configured maximum age.
        limit: Duration,
    },
}

impl Breach {
    /// Which limit this is about.
    #[must_use]
    pub const fn limit(&self) -> LimitKind {
        match self {
            Self::OverMaximumUsage { .. } => LimitKind::MaximumUsage,
            Self::BelowMinimumFreeSpace { .. } => LimitKind::MinimumFreeSpace,
            Self::OverMaximumAge { .. } => LimitKind::MaximumAge,
        }
    }
}

impl fmt::Display for Breach {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OverMaximumUsage { used, limit } => write!(
                formatter,
                "the library occupies {used} bytes, over its maximum of {limit}"
            ),
            Self::BelowMinimumFreeSpace { free, required } => write!(
                formatter,
                "{free} bytes are free on the recording drive, below the required {required}"
            ),
            Self::OverMaximumAge {
                files,
                bytes,
                limit,
            } => write!(
                formatter,
                "{files} files totalling {bytes} bytes are older than {} days",
                limit.as_secs() / 86_400
            ),
        }
    }
}

/// Why a limit could not be judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UnknownReason {
    /// The measurement of the library did not finish, so its usage is a floor
    /// rather than a total.
    AccountingIncomplete,
    /// The volume's free space could not be read — most often a drive that is
    /// not connected.
    VolumeUnreadable,
}

impl UnknownReason {
    /// A short lower-case name, for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccountingIncomplete => "accounting-incomplete",
            Self::VolumeUnreadable => "volume-unreadable",
        }
    }
}

impl fmt::Display for UnknownReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A configured limit that could not be judged, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unknown {
    limit: LimitKind,
    reason: UnknownReason,
}

impl Unknown {
    /// Which limit could not be judged.
    #[must_use]
    pub const fn limit(&self) -> LimitKind {
        self.limit
    }

    /// Why not.
    #[must_use]
    pub const fn reason(&self) -> UnknownReason {
        self.reason
    }
}

impl fmt::Display for Unknown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the {} limit cannot be judged: {}",
            self.limit, self.reason
        )
    }
}

/// What the limits say about the library as it stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageStatus {
    used_bytes: u64,
    free_bytes: Option<u64>,
    measurement: Completeness,
    breaches: Vec<Breach>,
    unknown: Vec<Unknown>,
}

impl StorageStatus {
    /// Judges `limits` against what `inventory` found and what `volume` says.
    ///
    /// `volume` is `None` when the recording drive could not be read, which is a
    /// state to report rather than an error to raise: a limit needing it becomes
    /// [`Unknown`], and a limit that does not is still judged.
    ///
    /// `now` is passed in rather than read so that the age limit is testable and
    /// so that one evaluation uses one instant throughout (AGENTS.md section 25).
    #[must_use]
    pub fn evaluate(
        inventory: &StorageInventory,
        limits: &StorageLimits,
        volume: Option<&VolumeCapacity>,
        now: SystemTime,
    ) -> Self {
        let used_bytes = inventory.total_bytes();
        let mut status = Self {
            used_bytes,
            free_bytes: volume.map(VolumeCapacity::free_bytes),
            measurement: inventory.completeness(),
            breaches: Vec::new(),
            unknown: Vec::new(),
        };

        if let Some(limit) = limits.maximum_usage() {
            if used_bytes > limit {
                // True whether or not the measurement finished: what was
                // measured is already over the limit, and the rest can only add
                // to it.
                status.breaches.push(Breach::OverMaximumUsage {
                    used: used_bytes,
                    limit,
                });
            } else if !inventory.is_complete() {
                status.unknown(LimitKind::MaximumUsage, UnknownReason::AccountingIncomplete);
            }
        }

        if let Some(required) = limits.minimum_free_space() {
            match volume {
                Some(volume) if volume.free_bytes() < required => {
                    status.breaches.push(Breach::BelowMinimumFreeSpace {
                        free: volume.free_bytes(),
                        required,
                    });
                }
                Some(_) => {}
                None => {
                    status.unknown(LimitKind::MinimumFreeSpace, UnknownReason::VolumeUnreadable)
                }
            }
        }

        if let Some(limit) = limits.maximum_age() {
            let (files, bytes) = inventory.cleanup_candidates_older_than(limit, now);
            if files > 0 {
                status.breaches.push(Breach::OverMaximumAge {
                    files,
                    bytes,
                    limit,
                });
            } else if !inventory.is_complete() {
                status.unknown(LimitKind::MaximumAge, UnknownReason::AccountingIncomplete);
            }
        }

        status
    }

    /// Records a limit that could not be judged.
    fn unknown(&mut self, limit: LimitKind, reason: UnknownReason) {
        self.unknown.push(Unknown { limit, reason });
    }

    /// What the library occupies, in bytes.
    ///
    /// A floor rather than a total when the measurement behind it was partial,
    /// which [`measurement`](Self::measurement) reports. A screen showing this
    /// figure has to ask: an under-reported usage presented as final is what
    /// this module's header exists to prevent, and a scan can be cancelled or
    /// run out of budget without any *limit* becoming unjudgeable, so
    /// [`is_certain`](Self::is_certain) — which is about the limits — will
    /// happily say `true` over a figure that is a floor.
    #[must_use]
    pub const fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    /// How complete the measurement behind [`used_bytes`](Self::used_bytes)
    /// was, and why it was not.
    ///
    /// Carried through from the inventory rather than left behind with it,
    /// because the figure and its completeness are only meaningful together:
    /// "212 GB" and "at least 212 GB" are different sentences.
    ///
    /// **Nothing reads it yet.** The Storage screen
    /// ([issue #95](https://github.com/wildware-uk/clipped/issues/95)) was built
    /// on `clipped_session::cleanup::preview` — the measurement its sweep also
    /// takes, so that a dry run cannot disagree with what happens — and that
    /// path does not evaluate a [`StorageStatus`] at all. `preview` walks with
    /// an unbudgeted [`ScanOptions`](crate::accounting::ScanOptions), so its
    /// inventory is complete or the walk failed; a screen that could be handed a
    /// partial one is what would need this, and no caller is.
    #[must_use]
    pub const fn measurement(&self) -> &Completeness {
        &self.measurement
    }

    /// What is free on the volume, if it could be read.
    #[must_use]
    pub const fn free_bytes(&self) -> Option<u64> {
        self.free_bytes
    }

    /// Every limit that is not being met.
    #[must_use]
    pub fn breaches(&self) -> &[Breach] {
        &self.breaches
    }

    /// Whether any limit is breached.
    ///
    /// What a recording about to start checks. It is deliberately not "may I
    /// record": nothing here stops a recording, and what a breach *does* is
    /// issue #111's and the interface's to decide.
    #[must_use]
    pub fn is_breached(&self) -> bool {
        !self.breaches.is_empty()
    }

    /// Every limit that could not be judged, and why.
    #[must_use]
    pub fn unknowns(&self) -> &[Unknown] {
        &self.unknown
    }

    /// Whether every configured limit could be judged.
    ///
    /// `false` means some limit's state is genuinely not known — not that it is
    /// satisfied.
    ///
    /// This is about the *limits* and not about the figures. With no limits
    /// configured there is nothing to be uncertain about, so a half-measured
    /// library is `true` here; [`measurement`](Self::measurement) is what says
    /// whether [`used_bytes`](Self::used_bytes) is a total or a floor.
    #[must_use]
    pub fn is_certain(&self) -> bool {
        self.unknown.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::accounting::inventory::{FileEntry, PartialReason};
    use crate::accounting::StorageCategory;

    /// A fixed instant, so nothing here depends on the wall clock.
    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn path(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"D:\Clipped\{name}"))
        } else {
            PathBuf::from(format!("/clipped/{name}"))
        }
    }

    /// An inventory holding one file of `bytes` bytes, `age` old.
    fn library_of(bytes: u64, age: Duration) -> StorageInventory {
        let mut inventory = StorageInventory::new();
        inventory.record_added(FileEntry::new(
            path("a.mkv"),
            StorageCategory::Recordings,
            bytes,
            Some(now() - age),
        ));
        inventory
    }

    fn drive_with_free(free: u64) -> VolumeCapacity {
        VolumeCapacity::new(500_000_000_000, free)
    }

    #[test]
    fn no_limits_means_nothing_to_breach_and_nothing_unknown() {
        let status = StorageStatus::evaluate(
            &library_of(400_000_000_000, Duration::from_secs(60)),
            &StorageLimits::unlimited(),
            Some(&drive_with_free(1)),
            now(),
        );

        assert!(!status.is_breached());
        assert!(status.is_certain());
        assert_eq!(status.used_bytes(), 400_000_000_000);
        assert_eq!(status.free_bytes(), Some(1));
    }

    #[test]
    fn a_library_within_its_quota_is_not_breached() {
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(250_000_000_000)
            .expect("a quota");

        let status = StorageStatus::evaluate(
            &library_of(200_000_000_000, Duration::from_secs(60)),
            &limits,
            None,
            now(),
        );

        assert!(!status.is_breached());
        assert!(status.is_certain(), "{:?}", status.unknowns());
    }

    #[test]
    fn a_library_exactly_at_its_quota_is_not_breached() {
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(250_000_000_000)
            .expect("a quota");

        let status = StorageStatus::evaluate(
            &library_of(250_000_000_000, Duration::from_secs(60)),
            &limits,
            None,
            now(),
        );

        assert!(!status.is_breached());
    }

    #[test]
    fn a_library_over_its_quota_says_both_numbers() {
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(250_000_000_000)
            .expect("a quota");

        let status = StorageStatus::evaluate(
            &library_of(250_000_000_001, Duration::from_secs(60)),
            &limits,
            None,
            now(),
        );

        assert_eq!(
            status.breaches(),
            &[Breach::OverMaximumUsage {
                used: 250_000_000_001,
                limit: 250_000_000_000,
            }]
        );
        assert_eq!(status.breaches()[0].limit(), LimitKind::MaximumUsage);
    }

    #[test]
    fn a_partial_measurement_cannot_say_a_quota_is_satisfied() {
        // The failure this exists to prevent: a cancelled scan measured 10 GB of
        // a 300 GB library, and reporting "well within 250 GB" from that is how
        // a disk fills with a quota configured.
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(250_000_000_000)
            .expect("a quota");
        let mut inventory = library_of(10_000_000_000, Duration::from_secs(60));
        inventory.record_partial(PartialReason::Cancelled);

        let status = StorageStatus::evaluate(&inventory, &limits, None, now());

        assert!(!status.is_breached());
        assert!(!status.is_certain());
        assert_eq!(status.unknowns().len(), 1);
        assert_eq!(status.unknowns()[0].limit(), LimitKind::MaximumUsage);
        assert_eq!(
            status.unknowns()[0].reason(),
            UnknownReason::AccountingIncomplete
        );
    }

    #[test]
    fn a_partial_measurement_can_still_prove_a_quota_is_breached() {
        // Usage only grows as more of the library is seen, so what has already
        // been measured being over the limit settles it.
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(250_000_000_000)
            .expect("a quota");
        let mut inventory = library_of(400_000_000_000, Duration::from_secs(60));
        inventory.record_partial(PartialReason::TimeBudgetExhausted);

        let status = StorageStatus::evaluate(&inventory, &limits, None, now());

        assert!(status.is_breached());
        assert!(
            status.is_certain(),
            "a proved breach leaves nothing unknown: {:?}",
            status.unknowns()
        );
    }

    #[test]
    fn free_space_below_the_minimum_is_a_breach() {
        let limits = StorageLimits::unlimited().with_minimum_free_space(20_000_000_000);

        let status = StorageStatus::evaluate(
            &library_of(1_000, Duration::from_secs(60)),
            &limits,
            Some(&drive_with_free(19_999_999_999)),
            now(),
        );

        assert_eq!(
            status.breaches(),
            &[Breach::BelowMinimumFreeSpace {
                free: 19_999_999_999,
                required: 20_000_000_000,
            }]
        );
    }

    #[test]
    fn exactly_the_minimum_free_space_is_not_a_breach() {
        let limits = StorageLimits::unlimited().with_minimum_free_space(20_000_000_000);

        let status = StorageStatus::evaluate(
            &library_of(1_000, Duration::from_secs(60)),
            &limits,
            Some(&drive_with_free(20_000_000_000)),
            now(),
        );

        assert!(!status.is_breached());
    }

    #[test]
    fn a_drive_that_could_not_be_read_makes_the_free_space_limit_unknown() {
        // The disconnected recording drive. Reporting "free space fine" here is
        // the same class of mistake as reporting a partial library as complete.
        let limits = StorageLimits::unlimited().with_minimum_free_space(20_000_000_000);

        let status = StorageStatus::evaluate(
            &library_of(1_000, Duration::from_secs(60)),
            &limits,
            None,
            now(),
        );

        assert!(!status.is_breached());
        assert!(!status.is_certain());
        assert_eq!(status.unknowns()[0].limit(), LimitKind::MinimumFreeSpace);
        assert_eq!(
            status.unknowns()[0].reason(),
            UnknownReason::VolumeUnreadable
        );
        assert_eq!(status.free_bytes(), None);
    }

    #[test]
    fn a_quota_is_still_judged_when_the_drive_cannot_be_read() {
        // One limit being unjudgeable must not take the others with it.
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(250_000_000_000)
            .expect("a quota")
            .with_minimum_free_space(20_000_000_000);

        let status = StorageStatus::evaluate(
            &library_of(300_000_000_000, Duration::from_secs(60)),
            &limits,
            None,
            now(),
        );

        assert_eq!(status.breaches().len(), 1);
        assert_eq!(status.breaches()[0].limit(), LimitKind::MaximumUsage);
        assert_eq!(status.unknowns().len(), 1);
        assert_eq!(status.unknowns()[0].limit(), LimitKind::MinimumFreeSpace);
    }

    #[test]
    fn footage_older_than_the_maximum_age_is_a_breach_that_counts_it() {
        let limits = StorageLimits::unlimited()
            .with_maximum_age(Duration::from_secs(90 * 86_400))
            .expect("90 days");

        let status = StorageStatus::evaluate(
            &library_of(84_000_000_000, Duration::from_secs(120 * 86_400)),
            &limits,
            None,
            now(),
        );

        assert_eq!(
            status.breaches(),
            &[Breach::OverMaximumAge {
                files: 1,
                bytes: 84_000_000_000,
                limit: Duration::from_secs(90 * 86_400),
            }]
        );
    }

    #[test]
    fn the_database_and_the_logs_are_not_over_age_footage() {
        // The user-facing sentence this protects: "3 files totalling 3,000
        // bytes are older than 90 days" must be about recordings. An install
        // that has been running for a year has a database and rotated logs
        // older than any age limit a user would set.
        let limits = StorageLimits::unlimited()
            .with_maximum_age(Duration::from_secs(90 * 86_400))
            .expect("90 days");
        let mut inventory = StorageInventory::new();
        for (name, category) in [
            ("clipped.db", StorageCategory::Metadata),
            ("clipped.log", StorageCategory::Logs),
            ("buffer.mkv", StorageCategory::ReplayBuffer),
        ] {
            inventory.record_added(FileEntry::new(
                path(name),
                category,
                1_000,
                Some(now() - Duration::from_secs(200 * 86_400)),
            ));
        }

        let status = StorageStatus::evaluate(&inventory, &limits, None, now());

        assert!(!status.is_breached(), "{:?}", status.breaches());
        assert_eq!(
            status.used_bytes(),
            3_000,
            "all three are still counted towards usage"
        );
    }

    #[test]
    fn footage_within_the_maximum_age_is_not_a_breach() {
        let limits = StorageLimits::unlimited()
            .with_maximum_age(Duration::from_secs(90 * 86_400))
            .expect("90 days");

        let status = StorageStatus::evaluate(
            &library_of(84_000_000_000, Duration::from_secs(10 * 86_400)),
            &limits,
            None,
            now(),
        );

        assert!(!status.is_breached());
    }

    #[test]
    fn every_breached_limit_is_reported_rather_than_only_the_first() {
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(1_000_000_000)
            .expect("a quota")
            .with_minimum_free_space(20_000_000_000)
            .with_maximum_age(Duration::from_secs(86_400))
            .expect("a day");

        let status = StorageStatus::evaluate(
            &library_of(400_000_000_000, Duration::from_secs(10 * 86_400)),
            &limits,
            Some(&drive_with_free(1_000)),
            now(),
        );

        let breached: Vec<_> = status.breaches().iter().map(Breach::limit).collect();
        assert_eq!(
            breached,
            vec![
                LimitKind::MaximumUsage,
                LimitKind::MinimumFreeSpace,
                LimitKind::MaximumAge
            ]
        );
    }

    #[test]
    fn a_partial_measurement_says_so_even_when_every_limit_is_judgeable() {
        // The gap this closes. With no limits configured there is nothing
        // unknown, so `is_certain` is true — and `used_bytes` is still a floor,
        // because the scan behind it was cancelled. A screen handed only this
        // type has to be able to tell.
        let mut inventory = library_of(10_000_000_000, Duration::from_secs(60));
        inventory.record_partial(PartialReason::Cancelled);

        let status = StorageStatus::evaluate(&inventory, &StorageLimits::unlimited(), None, now());

        assert!(
            status.is_certain(),
            "no limit is configured to be unsure of"
        );
        assert!(!status.measurement().is_complete());
        assert_eq!(
            status.measurement().reasons(),
            &[PartialReason::Cancelled],
            "and it says which half of the library was not seen"
        );
    }

    #[test]
    fn a_complete_measurement_is_reported_as_complete() {
        let status = StorageStatus::evaluate(
            &library_of(1_000, Duration::from_secs(60)),
            &StorageLimits::unlimited(),
            None,
            now(),
        );

        assert!(status.measurement().is_complete());
        assert!(status.measurement().reasons().is_empty());
    }

    #[test]
    fn a_breach_reads_as_a_sentence_with_the_numbers_in_it() {
        let breach = Breach::BelowMinimumFreeSpace {
            free: 1_000,
            required: 20_000,
        };

        assert_eq!(
            breach.to_string(),
            "1000 bytes are free on the recording drive, below the required 20000"
        );
    }

    #[test]
    fn an_unknown_reads_as_which_limit_and_why() {
        let unknown = Unknown {
            limit: LimitKind::MaximumUsage,
            reason: UnknownReason::AccountingIncomplete,
        };

        assert_eq!(
            unknown.to_string(),
            "the maximum-usage limit cannot be judged: accounting-incomplete"
        );
    }
}
