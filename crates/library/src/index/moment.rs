//! Comparing and writing the moments the schema stores.
//!
//! Every timestamp in the library is RFC 3339 text carrying an offset, exactly
//! as a session sidecar writes it (`docs/storage.md`). Storing one is therefore
//! a copy and needs nothing from this module. Two things do:
//!
//! - **Comparing** two of them, to decide which session is a game's most recent
//!   and which is its first. Text comparison is wrong the moment two rows carry
//!   different offsets — `2026-08-11T14:00:00+01:00` is *before*
//!   `2026-08-11T14:00:00Z` and sorts after it — and a library that files a
//!   session under the wrong date once a year is worse than one that never
//!   does.
//! - **Writing** one this crate observed rather than copied: when a recording
//!   was first found to be gone.

use std::time::SystemTime;

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

/// The instant an RFC 3339 timestamp names, if it is one.
///
/// `None` for anything else, including the `unknown-time` a session writes when
/// its clock could not be formatted at all. A caller must then fall back on
/// something that does not need an ordering rather than inventing one.
pub(crate) fn instant(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

/// `when` as this crate writes it: RFC 3339 in the machine's own offset, the
/// same form the sidecars use.
///
/// The offset is the local one because every other timestamp in the database is
/// local, and a `missing_since` in UTC beside a `started_at` in local time is a
/// column two people will read two ways. `time` refuses to read the local offset
/// in a process where doing so would be unsound, which the desktop application
/// certainly is, so UTC is the documented fallback rather than a panic.
pub(crate) fn rfc3339(when: SystemTime) -> String {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    OffsetDateTime::from(when)
        .to_offset(offset)
        .format(&Rfc3339)
        // A `SystemTime` outside the years RFC 3339 can express is a clock
        // nobody should trust; the epoch is written rather than nothing, so the
        // row still says "this was observed missing" and the oddity is visible.
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// The later of two moments, preferring `candidate` when neither can be read.
///
/// Used to advance a game's `last_played_at` as sessions are ingested in
/// whatever order the walk met them.
pub(crate) fn later(stored: Option<&str>, candidate: &str) -> String {
    match stored {
        None => candidate.to_owned(),
        Some(stored) => match (instant(stored), instant(candidate)) {
            (Some(stored_at), Some(candidate_at)) if stored_at >= candidate_at => stored.to_owned(),
            (Some(_), None) => stored.to_owned(),
            _ => candidate.to_owned(),
        },
    }
}

/// The earlier of two moments, preferring the stored one when neither can be
/// read.
///
/// Used for a game's `first_seen_at`, which must never move forwards: it is the
/// answer to "how long have I been playing this?" and a re-index must not
/// change it.
pub(crate) fn earlier(stored: &str, candidate: &str) -> String {
    match (instant(stored), instant(candidate)) {
        (Some(stored_at), Some(candidate_at)) if candidate_at < stored_at => candidate.to_owned(),
        _ => stored.to_owned(),
    }
}

/// Whether `candidate` names an instant at or after `stored`.
///
/// `false` when either cannot be read, which is what keeps an unreadable
/// timestamp from displacing a name that was written from a readable one.
pub(crate) fn at_or_after(candidate: &str, stored: Option<&str>) -> bool {
    let Some(candidate_at) = instant(candidate) else {
        return false;
    };
    match stored {
        None => true,
        Some(stored) => instant(stored).is_some_and(|stored_at| candidate_at >= stored_at),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn two_moments_in_different_offsets_compare_as_instants_and_not_as_text() {
        // The bug this exists to prevent: `+01:00` sorts after `Z` as text, and
        // is an hour earlier as a moment.
        let winter = "2026-08-11T14:00:00Z";
        let summer = "2026-08-11T14:00:00+01:00";

        assert_eq!(later(Some(summer), winter), winter);
        assert_eq!(earlier(winter, summer), summer);
        assert!(at_or_after(winter, Some(summer)));
        assert!(!at_or_after(summer, Some(winter)));
    }

    #[test]
    fn the_first_time_a_game_was_seen_never_moves_forwards() {
        let first = "2026-08-01T09:00:00+01:00";
        let later_session = "2026-08-11T14:00:00+01:00";

        assert_eq!(earlier(first, later_session), first);
        assert_eq!(earlier(later_session, first), first);
    }

    #[test]
    fn a_timestamp_that_cannot_be_read_does_not_displace_one_that_can() {
        // `clipped-session` writes `unknown-time` when a clock reading cannot be
        // formatted. Letting it win would move a game's first_seen_at to a
        // value nothing can sort.
        let known = "2026-08-11T14:00:00+01:00";

        assert_eq!(later(Some(known), "unknown-time"), known);
        assert_eq!(earlier(known, "unknown-time"), known);
        assert!(!at_or_after("unknown-time", Some(known)));
    }

    #[test]
    fn what_this_crate_writes_can_be_read_back_by_what_reads_the_schema() {
        let written = rfc3339(SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725));

        let read_back = instant(&written).expect("what we write, we can read: {written}");
        assert_eq!(
            read_back.unix_timestamp(),
            1_786_458_725,
            "the moment changed on the way through: {written}"
        );
    }
}
