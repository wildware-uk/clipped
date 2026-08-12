//! The three limits a user configures, and what makes one impossible.
//!
//! SPEC.md section 27 offers exactly three: a maximum number of bytes Clipped's
//! own library may occupy, a minimum number of bytes that must stay free on the
//! volume, and a maximum age for a recording. They are independent — any
//! combination of them may be set or left unset — and *unset means no limit*
//! rather than a default invented here.
//!
//! # Validation happens twice, deliberately
//!
//! Some limits are impossible on their own: a maximum usage of nothing, or a
//! maximum age of nothing. Those are refused by the constructors, so a value of
//! that kind cannot exist.
//!
//! The rest are impossible only against a particular disk — "minimum free space
//! cannot exceed the disk" is issue #93's own example — and the disk is not
//! known when the setting is typed, may be a different one by the time it is
//! used, and may not be connected at all. So [`StorageLimits::validate_for`] is
//! a second pass, run when the limits meet a volume. Both are cheap and both are
//! required: the settings screen runs the first as the user types and the second
//! when it knows the recording location.
//!
//! # Nothing here is a default
//!
//! [`StorageLimits::unlimited`] is the only value this module will invent. What
//! a fresh installation should be configured with is a product decision that
//! belongs to the configuration API
//! ([issue #108](https://github.com/wildware-uk/clipped/issues/108)) and to the
//! settings screen that presents it
//! ([issue #95](https://github.com/wildware-uk/clipped/issues/95)), and a quota
//! silently applied by a library crate would be a quota nobody chose.

use core::time::Duration;

use crate::accounting::error::LimitError;
use crate::accounting::volume::VolumeCapacity;

/// The smallest maximum usage that may be configured, in bytes.
///
/// One gigabyte: below a single recording, so a quota beneath it is one that is
/// breached from the first session and can only be satisfied by a library with
/// nothing in it. The point of the floor is to catch the value that is really
/// meant as "no recordings at all" — zero — before it reaches issue #111's
/// cleanup, which would honour it.
pub const MINIMUM_QUOTA: u64 = 1_000_000_000;

/// The shortest maximum recording age that may be configured.
///
/// One day. Anything shorter, once issue #111 acts on it, deletes footage the
/// user recorded this afternoon; zero deletes it as it is written. A user who
/// genuinely wants no history keeps no recordings, and the application does not
/// offer a setting whose effect is indistinguishable from a bug (AGENTS.md
/// section 56).
pub const MAXIMUM_AGE_FLOOR: Duration = Duration::from_secs(86_400);

/// What a library is allowed to occupy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageLimits {
    maximum_usage: Option<u64>,
    minimum_free_space: Option<u64>,
    maximum_age: Option<Duration>,
}

impl StorageLimits {
    /// No limit of any kind.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            maximum_usage: None,
            minimum_free_space: None,
            maximum_age: None,
        }
    }

    /// Limits the library to `bytes`.
    ///
    /// # Errors
    ///
    /// [`LimitError::QuotaTooSmall`] below [`MINIMUM_QUOTA`], which is where
    /// zero is caught.
    pub const fn with_maximum_usage(self, bytes: u64) -> Result<Self, LimitError> {
        if bytes < MINIMUM_QUOTA {
            return Err(LimitError::QuotaTooSmall {
                requested: bytes,
                minimum: MINIMUM_QUOTA,
            });
        }

        Ok(Self {
            maximum_usage: Some(bytes),
            ..self
        })
    }

    /// Requires `bytes` to stay free on the volume.
    ///
    /// Zero is accepted and means what it says: fill the disk. It is a
    /// defensible thing to want on a drive kept for nothing else, and unlike a
    /// quota of zero it cannot delete anything on its own.
    ///
    /// Whether the figure fits on the disk cannot be known here; that is
    /// [`validate_for`](Self::validate_for).
    #[must_use]
    pub const fn with_minimum_free_space(self, bytes: u64) -> Self {
        Self {
            minimum_free_space: Some(bytes),
            ..self
        }
    }

    /// Marks recordings older than `age` as over-age.
    ///
    /// Nothing in this module acts on that; it reports how much is over-age
    /// ([`StorageInventory::cleanup_candidates_older_than`](crate::accounting::StorageInventory::cleanup_candidates_older_than)).
    ///
    /// *Recordings*, in the sense
    /// [`StorageCategory::is_cleanup_candidate`](crate::accounting::StorageCategory::is_cleanup_candidate)
    /// defines: the database, the logs and the replay buffer's disk backing are
    /// older than any age a user would set here and are not what this setting
    /// is about.
    ///
    /// # Errors
    ///
    /// [`LimitError::AgeTooShort`] below [`MAXIMUM_AGE_FLOOR`].
    pub const fn with_maximum_age(self, age: Duration) -> Result<Self, LimitError> {
        // `Duration` comparison is not const, so compare the parts. Seconds
        // alone would accept 86_399.9 seconds; the floor is a whole number of
        // seconds, so a shorter duration with any nanoseconds is still shorter.
        if age.as_secs() < MAXIMUM_AGE_FLOOR.as_secs() {
            return Err(LimitError::AgeTooShort {
                requested: age,
                minimum: MAXIMUM_AGE_FLOOR,
            });
        }

        Ok(Self {
            maximum_age: Some(age),
            ..self
        })
    }

    /// The configured maximum usage, in bytes.
    #[must_use]
    pub const fn maximum_usage(&self) -> Option<u64> {
        self.maximum_usage
    }

    /// The configured minimum free space, in bytes.
    #[must_use]
    pub const fn minimum_free_space(&self) -> Option<u64> {
        self.minimum_free_space
    }

    /// The configured maximum recording age.
    #[must_use]
    pub const fn maximum_age(&self) -> Option<Duration> {
        self.maximum_age
    }

    /// Whether any limit at all is configured.
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        self.maximum_usage.is_none()
            && self.minimum_free_space.is_none()
            && self.maximum_age.is_none()
    }

    /// Checks the limits against the volume they will be applied to.
    ///
    /// The acceptance criterion's own example lives here: a minimum free space
    /// larger than the disk can never be satisfied, so every recording would
    /// start against a breached limit and a cleanup would delete a library
    /// trying to reach a figure that is not reachable. A maximum usage larger
    /// than the disk is refused for the same reason in reverse — it can never
    /// bite, so a user who set it believes they have a quota and does not.
    ///
    /// # Errors
    ///
    /// [`LimitError::LargerThanVolume`], naming which limit did not fit.
    pub const fn validate_for(&self, volume: &VolumeCapacity) -> Result<(), LimitError> {
        if let Some(minimum) = self.minimum_free_space {
            if minimum > volume.total_bytes() {
                return Err(LimitError::LargerThanVolume {
                    limit: "minimum free space",
                    requested: minimum,
                    volume: volume.total_bytes(),
                });
            }
        }

        if let Some(maximum) = self.maximum_usage {
            if maximum > volume.total_bytes() {
                return Err(LimitError::LargerThanVolume {
                    limit: "maximum usage",
                    requested: maximum,
                    volume: volume.total_bytes(),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 500 GB drive, as a drive is sold.
    fn drive() -> VolumeCapacity {
        VolumeCapacity::new(500_000_000_000, 120_000_000_000)
    }

    #[test]
    fn no_limits_are_configured_by_default() {
        let limits = StorageLimits::unlimited();

        assert!(limits.is_unlimited());
        assert_eq!(limits.maximum_usage(), None);
        assert_eq!(limits.minimum_free_space(), None);
        assert_eq!(limits.maximum_age(), None);
        assert_eq!(StorageLimits::default(), limits);
    }

    #[test]
    fn the_three_limits_are_independent() {
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(250_000_000_000)
            .expect("250 GB is a quota")
            .with_minimum_free_space(20_000_000_000)
            .with_maximum_age(Duration::from_secs(90 * 86_400))
            .expect("90 days is an age");

        assert_eq!(limits.maximum_usage(), Some(250_000_000_000));
        assert_eq!(limits.minimum_free_space(), Some(20_000_000_000));
        assert_eq!(limits.maximum_age(), Some(Duration::from_secs(90 * 86_400)));
        assert!(!limits.is_unlimited());
    }

    #[test]
    fn a_quota_of_zero_is_refused_rather_than_meaning_no_recordings() {
        let error = StorageLimits::unlimited()
            .with_maximum_usage(0)
            .expect_err("zero would put every library over quota");

        match error {
            LimitError::QuotaTooSmall { requested, minimum } => {
                assert_eq!(requested, 0);
                assert_eq!(minimum, MINIMUM_QUOTA);
            }
            other => panic!("expected a quota refusal, got {other}"),
        }
    }

    #[test]
    fn a_quota_just_below_the_floor_is_refused_and_the_floor_itself_is_accepted() {
        assert!(StorageLimits::unlimited()
            .with_maximum_usage(MINIMUM_QUOTA - 1)
            .is_err());
        assert!(StorageLimits::unlimited()
            .with_maximum_usage(MINIMUM_QUOTA)
            .is_ok());
    }

    #[test]
    fn a_maximum_age_of_nothing_is_refused() {
        // With issue #111 behind it, this setting deletes a recording as it is
        // written. AGENTS.md section 56 is the reason it cannot be configured.
        let error = StorageLimits::unlimited()
            .with_maximum_age(Duration::ZERO)
            .expect_err("zero would delete footage as it was recorded");

        assert!(matches!(error, LimitError::AgeTooShort { .. }), "{error}");
    }

    #[test]
    fn an_age_just_below_a_day_is_refused_and_a_day_is_accepted() {
        assert!(StorageLimits::unlimited()
            .with_maximum_age(Duration::from_secs(86_399))
            .is_err());
        assert!(StorageLimits::unlimited()
            .with_maximum_age(MAXIMUM_AGE_FLOOR)
            .is_ok());
    }

    #[test]
    fn a_minimum_free_space_of_zero_is_allowed() {
        // Unlike a quota of zero, this deletes nothing: it says the disk may be
        // filled, which is a defensible thing to want.
        let limits = StorageLimits::unlimited().with_minimum_free_space(0);

        assert_eq!(limits.minimum_free_space(), Some(0));
    }

    #[test]
    fn a_minimum_free_space_larger_than_the_disk_is_refused() {
        // The acceptance criterion's own example.
        let limits = StorageLimits::unlimited().with_minimum_free_space(900_000_000_000);

        let error = limits
            .validate_for(&drive())
            .expect_err("900 GB cannot be kept free on a 500 GB drive");

        match error {
            LimitError::LargerThanVolume {
                limit,
                requested,
                volume,
            } => {
                assert_eq!(limit, "minimum free space");
                assert_eq!(requested, 900_000_000_000);
                assert_eq!(volume, 500_000_000_000);
            }
            other => panic!("expected a volume refusal, got {other}"),
        }
    }

    #[test]
    fn a_minimum_free_space_of_exactly_the_disk_is_accepted() {
        // Absurd but satisfiable: it means "keep the drive empty", and it is the
        // boundary the refusal above is on the other side of.
        let limits = StorageLimits::unlimited().with_minimum_free_space(500_000_000_000);

        assert!(limits.validate_for(&drive()).is_ok());
    }

    #[test]
    fn a_quota_larger_than_the_disk_is_refused() {
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(2_000_000_000_000)
            .expect("2 TB is a quota");

        let error = limits
            .validate_for(&drive())
            .expect_err("a 2 TB quota on a 500 GB drive can never bite");

        assert!(
            matches!(
                error,
                LimitError::LargerThanVolume {
                    limit: "maximum usage",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn limits_that_fit_are_accepted_and_unlimited_always_is() {
        let limits = StorageLimits::unlimited()
            .with_maximum_usage(250_000_000_000)
            .expect("a quota")
            .with_minimum_free_space(20_000_000_000);

        assert!(limits.validate_for(&drive()).is_ok());
        assert!(StorageLimits::unlimited().validate_for(&drive()).is_ok());
    }

    #[test]
    fn the_same_limits_are_refused_by_a_smaller_drive_and_accepted_by_a_larger_one() {
        // Why validation happens twice: the limits are unchanged and only the
        // disk they are applied to differs, which is exactly what happens when a
        // user moves their recording location to another drive.
        let limits = StorageLimits::unlimited().with_minimum_free_space(300_000_000_000);

        assert!(limits
            .validate_for(&VolumeCapacity::new(256_000_000_000, 100_000_000_000))
            .is_err());
        assert!(limits
            .validate_for(&VolumeCapacity::new(2_000_000_000_000, 100_000_000_000))
            .is_ok());
    }
}
