//! Answers that carry how they were arrived at.
//!
//! Every capability this crate reports is one of three things: something the
//! machine was asked and answered, something derived from a published table, or
//! something nobody here knows. [`Claim`] is that distinction, made part of the
//! value rather than left to a comment, so that a caller cannot print an
//! inferred capability as a measured one without writing the word "inferred"
//! itself.
//!
//! The reason is in the failure mode. A user who picks AV1 because a table
//! keyed on a GPU model said AV1, and who then loses a recording because their
//! driver disagrees, has been lied to by this crate. The rule that follows is
//! stated once, here, and enforced by the table in [`crate::reference`]: a
//! capability whose absence would break a recording is either
//! [`Measured`](Claim::Measured) or [`Unknown`](Claim::Unknown), never
//! [`Inferred`](Claim::Inferred) as `true`.

use core::fmt;

use serde::{Deserialize, Serialize};

/// How an answer was arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// The machine, its driver or the operating system was asked, in this run,
    /// and this is what it said.
    Measured,
    /// Derived from what was measured plus a table of published limits. True of
    /// the hardware the table covers; not a promise about this machine.
    Inferred,
}

impl fmt::Display for Evidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Measured => "measured",
            Self::Inferred => "inferred",
        })
    }
}

/// A capability, and whether it was measured, inferred or neither.
///
/// [`Display`](fmt::Display) writes the qualifier alongside the value —
/// `4096x4096 (inferred)` — because the report and the logs are the two places
/// the distinction has to survive, and both go through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Claim<T> {
    /// Read back from the system in this run.
    Measured(T),
    /// Derived from a table of published limits.
    Inferred(T),
    /// Not measured, and no table entry covers it.
    ///
    /// Deliberately not a default value. "We do not know whether this GPU
    /// encodes AV1" and "this GPU does not encode AV1" are different answers,
    /// and collapsing them is how a table starts lying.
    Unknown,
}

impl<T> Claim<T> {
    /// The value, whatever its evidence, or `None` when there is none.
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Measured(value) | Self::Inferred(value) => Some(value),
            Self::Unknown => None,
        }
    }

    /// How the value was arrived at, or `None` when there is no value.
    pub const fn evidence(&self) -> Option<Evidence> {
        match self {
            Self::Measured(_) => Some(Evidence::Measured),
            Self::Inferred(_) => Some(Evidence::Inferred),
            Self::Unknown => None,
        }
    }

    /// Whether this was read back from the system rather than inferred.
    ///
    /// This is the question a caller about to act on a capability should ask:
    /// selecting a codec on a measured claim risks nothing, selecting one on an
    /// inferred claim risks the recording.
    pub const fn is_measured(&self) -> bool {
        matches!(self, Self::Measured(_))
    }

    /// Applies `f` to the value, keeping the evidence.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Claim<U> {
        match self {
            Self::Measured(value) => Claim::Measured(f(value)),
            Self::Inferred(value) => Claim::Inferred(f(value)),
            Self::Unknown => Claim::Unknown,
        }
    }
}

impl Claim<bool> {
    /// Whether this is a measured `true`.
    ///
    /// The predicate for "may a recording depend on this?", spelled out so that
    /// call sites do not each write their own version of it and get one of them
    /// wrong.
    pub const fn is_measured_true(&self) -> bool {
        matches!(self, Self::Measured(true))
    }
}

impl<T: fmt::Display> fmt::Display for Claim<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Measured(value) => write!(formatter, "{value}"),
            Self::Inferred(value) => write!(formatter, "{value} (inferred)"),
            Self::Unknown => formatter.write_str("unknown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inferred_value_says_so_when_it_is_printed() {
        assert_eq!(Claim::Measured(60).to_string(), "60");
        assert_eq!(Claim::Inferred(60).to_string(), "60 (inferred)");
        assert_eq!(Claim::<u32>::Unknown.to_string(), "unknown");
    }

    #[test]
    fn unknown_is_not_a_value() {
        let unknown = Claim::<bool>::Unknown;
        assert_eq!(unknown.value(), None);
        assert_eq!(unknown.evidence(), None);
        assert!(!unknown.is_measured());
        assert!(!unknown.is_measured_true());
    }

    #[test]
    fn only_a_measured_true_lets_a_recording_depend_on_a_capability() {
        assert!(Claim::Measured(true).is_measured_true());
        assert!(!Claim::Inferred(true).is_measured_true());
        assert!(!Claim::Measured(false).is_measured_true());
    }

    #[test]
    fn mapping_keeps_the_evidence() {
        assert_eq!(
            Claim::Inferred(2).map(|value| value * 2),
            Claim::Inferred(4)
        );
        assert_eq!(
            Claim::Measured(2).map(|value| value * 2),
            Claim::Measured(4)
        );
        assert_eq!(Claim::<u8>::Unknown.map(u16::from), Claim::Unknown);
    }
}
