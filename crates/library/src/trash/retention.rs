//! How long the trash keeps something, and when that has run out.
//!
//! SPEC.md section 28 names four choices and no others, so this is an
//! enumeration of exactly those rather than a duration a settings file could
//! hold any number in. A retention of four hours would be a setting nobody
//! designed and a way to lose footage that a user believed was recoverable
//! (AGENTS.md section 30).
//!
//! # Judging expiry
//!
//! From the moment the item was deleted, which is a column in the database, and
//! a `now` the caller passes in. Neither is read from the clock here, so a test
//! decides both and never waits (AGENTS.md section 25) — the acceptance
//! criterion on issue #94 asks for exactly that.
//!
//! One rule is worth stating on its own: **a timestamp that cannot be read never
//! expires.** `deleted_at` is RFC 3339 text and a row could carry something else
//! — hand-edited, restored from a corrupt backup, written by a build that is not
//! this one. Treating an unreadable moment as "long ago" would destroy footage
//! on the strength of a value nothing understands, which is the deletion nobody
//! asked for.

use std::time::{Duration, SystemTime};

use crate::index::moment;

/// A day, as the retention periods count them.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// How long deleted footage is kept before it is destroyed.
///
/// The four values SPEC.md section 28 specifies, and no fifth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum Retention {
    /// Keep nothing: an item expires the moment it is deleted.
    ///
    /// It still goes to the trash and is still destroyed by an explicit sweep
    /// rather than by the delete itself, so there is one code path and a user
    /// who changes their mind before the next sweep can still restore. See the
    /// module documentation of [`super`] for why that is not a fudge.
    Immediate,
    /// Three days.
    ThreeDays,
    /// Seven days. The default.
    #[default]
    SevenDays,
    /// Thirty days.
    ThirtyDays,
}

impl Retention {
    /// Every choice, in the order a settings screen offers them.
    pub const ALL: [Self; 4] = [
        Self::Immediate,
        Self::ThreeDays,
        Self::SevenDays,
        Self::ThirtyDays,
    ];

    /// How long this keeps something.
    #[must_use]
    pub const fn duration(self) -> Duration {
        match self {
            Self::Immediate => Duration::ZERO,
            Self::ThreeDays => Duration::from_secs(3 * DAY.as_secs()),
            Self::SevenDays => Duration::from_secs(7 * DAY.as_secs()),
            Self::ThirtyDays => Duration::from_secs(30 * DAY.as_secs()),
        }
    }

    /// How many days this keeps something, which is zero for
    /// [`Immediate`](Self::Immediate).
    #[must_use]
    pub const fn days(self) -> u32 {
        match self {
            Self::Immediate => 0,
            Self::ThreeDays => 3,
            Self::SevenDays => 7,
            Self::ThirtyDays => 30,
        }
    }

    /// The retention that keeps something for this many days, if one does.
    ///
    /// The route a stored setting takes back into this type (issue #108). A
    /// number outside the four is `None` rather than the nearest match, so a
    /// hand-edited settings file cannot install a retention this module would
    /// not have offered.
    #[must_use]
    pub fn from_days(days: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.days() == days)
    }

    /// The word for it, in the copy a settings screen uses (AGENTS.md
    /// section 28).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Immediate => "Immediately",
            Self::ThreeDays => "3 days",
            Self::SevenDays => "7 days",
            Self::ThirtyDays => "30 days",
        }
    }
}

/// How long is left of `retention` for something deleted at `deleted_at`.
///
/// `Some(Duration::ZERO)` once it has run out, and `None` when `deleted_at` is
/// not a moment this build can read.
pub(crate) fn remaining(
    deleted_at: &str,
    retention: Retention,
    now: SystemTime,
) -> Option<Duration> {
    let deleted_at = moment::instant(deleted_at)?;
    let expires_at = SystemTime::from(deleted_at).checked_add(retention.duration())?;
    Some(expires_at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Whether `retention` has run out for something deleted at `deleted_at`.
///
/// `false` when the moment cannot be read — see the module documentation.
pub(crate) fn has_expired(deleted_at: &str, retention: Retention, now: SystemTime) -> bool {
    remaining(deleted_at, retention, now).is_some_and(|left| left.is_zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-08-12T09:00:00+01:00, the moment the fixtures below are deleted at.
    const DELETED_AT: &str = "2026-08-12T09:00:00+01:00";
    const DELETED_AT_UNIX: u64 = 1_786_521_600;

    fn at(offset: Duration) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(DELETED_AT_UNIX) + offset
    }

    #[test]
    fn the_fixture_names_the_moment_this_module_thinks_it_does() {
        // Everything below is arithmetic from these two agreeing, so they are
        // checked rather than assumed.
        let parsed = moment::instant(DELETED_AT).expect("the fixture is RFC 3339");
        assert_eq!(
            u64::try_from(parsed.unix_timestamp()).expect("after the epoch"),
            DELETED_AT_UNIX
        );
    }

    #[test]
    fn each_retention_keeps_something_for_as_long_as_its_name_says() {
        assert_eq!(Retention::Immediate.duration(), Duration::ZERO);
        assert_eq!(Retention::ThreeDays.duration(), 3 * DAY);
        assert_eq!(Retention::SevenDays.duration(), 7 * DAY);
        assert_eq!(Retention::ThirtyDays.duration(), 30 * DAY);
    }

    #[test]
    fn the_default_is_the_choice_that_keeps_footage_rather_than_the_one_that_loses_it() {
        // An unset retention must not be the setting that destroys. SPEC.md
        // section 28 names no default, so the safe end of the range is it.
        assert_eq!(Retention::default(), Retention::SevenDays);
    }

    #[test]
    fn a_stored_number_outside_the_four_choices_is_refused_rather_than_rounded() {
        assert_eq!(Retention::from_days(7), Some(Retention::SevenDays));
        assert_eq!(Retention::from_days(0), Some(Retention::Immediate));
        assert_eq!(Retention::from_days(1), None);
        assert_eq!(Retention::from_days(365), None);
    }

    #[test]
    fn expiry_is_judged_from_the_moment_it_was_deleted_and_not_from_now() {
        // The acceptance criterion: controlled timestamps, no waiting.
        assert!(!has_expired(DELETED_AT, Retention::SevenDays, at(6 * DAY)));
        assert!(has_expired(
            DELETED_AT,
            Retention::SevenDays,
            at(7 * DAY + Duration::from_secs(1))
        ));
    }

    #[test]
    fn the_moment_retention_runs_out_is_expired_rather_than_kept() {
        // The boundary, stated: at exactly seven days there is nothing left.
        assert_eq!(
            remaining(DELETED_AT, Retention::SevenDays, at(7 * DAY)),
            Some(Duration::ZERO)
        );
        assert!(has_expired(DELETED_AT, Retention::SevenDays, at(7 * DAY)));
    }

    #[test]
    fn what_is_left_counts_down_and_stops_at_nothing() {
        assert_eq!(
            remaining(DELETED_AT, Retention::ThreeDays, at(Duration::ZERO)),
            Some(3 * DAY)
        );
        assert_eq!(
            remaining(DELETED_AT, Retention::ThreeDays, at(DAY)),
            Some(2 * DAY)
        );
        assert_eq!(
            remaining(DELETED_AT, Retention::ThreeDays, at(300 * DAY)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn immediate_expires_at_once_but_only_when_a_sweep_asks() {
        // Nothing here deletes; `Trash::expire` does, and this is only the
        // judgement it makes.
        assert!(has_expired(
            DELETED_AT,
            Retention::Immediate,
            at(Duration::ZERO)
        ));
        assert_eq!(
            remaining(DELETED_AT, Retention::Immediate, at(Duration::ZERO)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn a_clock_that_has_been_wound_back_leaves_footage_alone() {
        // A user who corrects their machine's clock must not be told their
        // recording has already expired, and must not lose it either.
        assert!(!has_expired(
            DELETED_AT,
            Retention::ThreeDays,
            at(Duration::ZERO) - DAY
        ));
    }

    #[test]
    fn a_moment_that_cannot_be_read_never_expires() {
        // The rule that keeps a corrupt row from becoming a deletion. Even
        // `Immediate` — the retention that expires everything — keeps this.
        for retention in Retention::ALL {
            assert!(
                !has_expired("not a timestamp", retention, at(4_000 * DAY)),
                "{retention:?} destroyed footage on an unreadable timestamp"
            );
            assert_eq!(
                remaining("not a timestamp", retention, at(Duration::ZERO)),
                None
            );
        }
    }

    #[test]
    fn two_moments_in_different_offsets_are_compared_as_instants() {
        // `deleted_at` carries whatever offset the machine was in when the
        // delete happened, and a library that moved country would otherwise
        // gain or lose an hour of retention. `+01:00` is an hour *before* `Z`.
        let zulu = "2026-08-12T08:00:00Z";

        assert_eq!(
            remaining(zulu, Retention::ThreeDays, at(Duration::ZERO)),
            remaining(DELETED_AT, Retention::ThreeDays, at(Duration::ZERO)),
            "the same instant written two ways expired at two different times"
        );
    }
}
