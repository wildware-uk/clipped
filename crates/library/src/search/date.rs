//! The calendar date a `date:` term compares against.

use core::fmt;

/// A calendar date, with no time and no time zone.
///
/// A query says `date:>2026-08-01`, and what a user means by that is a *day on
/// their calendar*, not an instant. So this crate compares days, and converting
/// a stored instant into the day it fell on — which is a question about the
/// user's time zone, and about which side of midnight a session that ran until
/// 01:30 belongs to — is the indexer's job, not the matcher's
/// ([issue #56](https://github.com/wildware-uk/clipped/issues/56)). Keeping the
/// time zone out of here is what makes the matcher testable without a clock
/// (AGENTS.md section 25).
///
/// Ordering is chronological: the fields are declared in the order they are
/// compared in, which is what the derived [`Ord`] uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// A date, if the calendar has one.
    ///
    /// Returns `None` for a month outside 1–12, a day outside the length of
    /// that month, or a year outside 1–9999. The leap year rule is the
    /// Gregorian one, so 2024-02-29 exists and 2025-02-29 does not: a date the
    /// calendar does not have can never match a row, and accepting it silently
    /// would make `date:2025-02-29` an empty result set with no explanation
    /// (AGENTS.md section 45).
    #[must_use]
    pub const fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        if year == 0 || month == 0 || month > 12 {
            return None;
        }
        if day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Self { year, month, day })
    }

    /// The year.
    #[must_use]
    pub const fn year(self) -> u16 {
        self.year
    }

    /// The month, 1 to 12.
    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }

    /// The day of the month, from 1.
    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }
}

impl fmt::Display for Date {
    /// Writes the form a query is written in, `2026-08-11`, so that a date
    /// taken out of a parsed query can be put back into one.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

/// How many days that month has, in that year.
const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// The Gregorian leap year rule.
const fn is_leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::Date;

    fn date(year: u16, month: u8, day: u8) -> Date {
        Date::new(year, month, day).expect("the test names a real date")
    }

    #[test]
    fn a_date_the_calendar_does_not_have_is_refused() {
        assert_eq!(Date::new(2026, 0, 1), None, "there is no month zero");
        assert_eq!(Date::new(2026, 13, 1), None, "there is no thirteenth month");
        assert_eq!(Date::new(2026, 8, 0), None, "there is no zeroth day");
        assert_eq!(Date::new(2026, 4, 31), None, "April has thirty days");
        assert_eq!(Date::new(0, 1, 1), None, "there is no year zero");
    }

    #[test]
    fn february_follows_the_gregorian_leap_year_rule() {
        assert!(Date::new(2024, 2, 29).is_some(), "2024 is a leap year");
        assert_eq!(Date::new(2025, 2, 29), None, "2025 is not");
        assert!(Date::new(2000, 2, 29).is_some(), "2000 is, being /400");
        assert_eq!(Date::new(1900, 2, 29), None, "1900 is not, being /100");
    }

    #[test]
    fn dates_order_chronologically() {
        assert!(date(2025, 12, 31) < date(2026, 1, 1));
        assert!(date(2026, 8, 1) < date(2026, 8, 2));
        assert!(date(2026, 7, 31) < date(2026, 8, 1));
        assert_eq!(date(2026, 8, 11), date(2026, 8, 11));
    }

    #[test]
    fn a_date_is_written_the_way_a_query_writes_one() {
        assert_eq!(date(2026, 8, 1).to_string(), "2026-08-01");
        assert_eq!(date(999, 12, 31).to_string(), "0999-12-31");
    }
}
