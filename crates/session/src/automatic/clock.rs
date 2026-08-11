//! The two forms a session writes a moment in.
//!
//! Sessions are stamped from the wall clock rather than from a monotonic one,
//! because the two things they are stamped *for* — a file name a person reads
//! and a start time a library sorts by — are both calendar questions. Elapsed
//! time inside the session manager is a different matter and is measured as a
//! difference between two wall-clock readings, which is why the manager also
//! notices when that difference is impossible (`super::SessionManager`).
//!
//! Local time, not UTC, and deliberately: `clipped-recorder record` already
//! names its files `clipped-<yyyymmdd>-<hhmmss>` in local time, and a directory
//! holding automatic recordings stamped an hour differently from manual ones is
//! a puzzle nobody should have to solve. The cost is that the stamp is not
//! unique across a daylight-saving rollback; what that can cost is one refused
//! recording rather than a lost one, because the recorder never overwrites an
//! existing file unless it was told to (`crate::RecordingSettings::overwrite`).

use std::time::SystemTime;

use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

/// What is written when a moment cannot be formatted at all.
///
/// Formatting only fails for a year outside the range the format can express,
/// which means a clock reading nobody should trust. It is written rather than
/// dropped so that a session file is still readable and the oddity is visible.
const UNFORMATTABLE: &str = "unknown-time";

/// This machine's offset from UTC, or UTC when it cannot be determined.
///
/// `time` refuses to read the local offset in a process where doing so would be
/// unsound, which is a real possibility here — the recorder is
/// multi-threaded — so the fallback is not theoretical and is why nothing calls
/// `unwrap`.
pub(crate) fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

/// The stamp a session identifier and its file names carry.
pub(crate) fn stamp(when: SystemTime) -> String {
    stamp_at(when, local_offset())
}

/// The same, against a stated offset, which is what tests use.
pub(crate) fn stamp_at(when: SystemTime, offset: UtcOffset) -> String {
    let format = format_description!("[year][month][day]-[hour][minute][second]");
    OffsetDateTime::from(when)
        .to_offset(offset)
        .format(format)
        .unwrap_or_else(|_| UNFORMATTABLE.to_owned())
}

/// A moment as it is written in a session sidecar.
///
/// RFC 3339, carrying the offset, so that a reader can tell what the local time
/// was without also having to know where the machine was.
pub(crate) fn rfc3339(when: SystemTime) -> String {
    OffsetDateTime::from(when)
        .to_offset(local_offset())
        .format(&Rfc3339)
        .unwrap_or_else(|_| UNFORMATTABLE.to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 2026-08-11T14:32:05Z.
    fn moment() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725)
    }

    #[test]
    fn a_stamp_is_sortable_and_free_of_characters_windows_forbids() {
        let stamped = stamp_at(moment(), UtcOffset::UTC);
        assert_eq!(stamped, "20260811-143205");
        assert!(
            !stamped.contains([':', '/', '\\', '*', '?', '"', '<', '>', '|']),
            "a stamp ends up in a file name: {stamped}"
        );
    }

    #[test]
    fn a_stamp_is_taken_in_the_offset_it_is_given() {
        // The failure this catches is a session recorded at 15:32 in Britain
        // being filed as 14:32 while `record`'s own files say 15:32.
        let offset = UtcOffset::from_hms(1, 0, 0).expect("an hour is a valid offset");
        assert_eq!(stamp_at(moment(), offset), "20260811-153205");
    }

    #[test]
    fn a_sidecar_time_carries_its_offset_rather_than_being_a_bare_local_time() {
        // The date is not asserted, because it depends on where the machine
        // running the test is; what is asserted is that a reader elsewhere can
        // tell what instant it names, which a bare local time does not allow.
        let written = rfc3339(moment());
        let (date, rest) = written
            .split_once('T')
            .unwrap_or_else(|| panic!("not an RFC 3339 timestamp: {written}"));

        assert_eq!(date.len(), 10, "unexpected date part: {written}");
        assert!(
            date.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "unexpected date part: {written}"
        );
        assert!(
            rest.ends_with('Z') || rest.contains('+') || rest[1..].contains('-'),
            "a time with no offset cannot be read back unambiguously: {written}"
        );
    }
}
