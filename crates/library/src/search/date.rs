//! The calendar date a `date:` term compares against.
//!
//! It is [`time::Date`], re-exported rather than written here.
//!
//! # Why the query language borrows a date rather than defining one
//!
//! A query says `date:>2026-08-01`, and what a user means by that is a *day on
//! their calendar*, not an instant. So the thing this module needs is a date
//! with no time and no time zone in it, ordered chronologically and validated
//! against the Gregorian calendar — which is precisely what `time::Date` is.
//! Writing a second one would put a second leap-year rule in the product, and
//! two calendars in one application is the kind of duplication AGENTS.md
//! section 55 exists to stop. `clipped-session` already depends on `time` for
//! the same reason, and its manifest says so: working a date out by hand is
//! exactly the thing not to hand-roll.
//!
//! It also removes an adapter from [issue
//! #56](https://github.com/wildware-uk/clipped/issues/56). The indexer will
//! hold an [`OffsetDateTime`](time::OffsetDateTime) — that is what
//! `crates/session/src/automatic/clock.rs` produces — and
//! [`OffsetDateTime::date`](time::OffsetDateTime::date) is already the day a
//! row belongs to. Nothing has to convert between two spellings of a date to
//! put a row into a [`Row`](super::Row).
//!
//! # What stays out of here
//!
//! The time zone. Converting a stored instant into the day it fell on is a
//! question about the user's zone, and about which side of midnight a session
//! that ran until 01:30 belongs to; that is the indexer's decision, not the
//! matcher's. Keeping it out is what lets every test in this module run without
//! a clock (AGENTS.md section 25), which is why `time`'s `local-offset` feature
//! is not enabled for this crate.

pub use time::Date;
