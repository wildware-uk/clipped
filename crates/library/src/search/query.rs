//! The query model: what a parsed search *is*, before anything runs it.
//!
//! This is deliberately a tree of values with no execution in it. Two things
//! consume it, and neither should have to agree with the other by accident:
//! [`Query::matches`](super::matcher) walks it against a [`Row`](super::Row) in
//! memory, and a database-backed executor
//! ([issue #55](https://github.com/wildware-uk/clipped/issues/55),
//! [issue #56](https://github.com/wildware-uk/clipped/issues/56)) walks the same
//! tree to build SQL. The tree is the contract between them, which is why every
//! part of it is public and why folding, validation and defaulting all happen
//! during parsing rather than during matching.

use core::cmp::Ordering;
use core::time::Duration;

use super::date::Date;
use super::text::FoldedText;

/// A parsed search query.
///
/// Build one by parsing text:
///
/// ```
/// use clipped_library::search::Query;
///
/// let query: Query = "game:cs2 kill favourite".parse()?;
/// # Ok::<(), clipped_library::search::QueryError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// `None` for a query with no terms in it, which selects everything.
    root: Option<Expr>,
}

impl Query {
    /// A query built from an expression.
    #[must_use]
    pub fn new(root: Expr) -> Self {
        Self { root: Some(root) }
    }

    /// The query that selects the whole library.
    ///
    /// This is what an empty search box parses to. It is not an error: a user
    /// who clears the box is asking to see everything, and refusing that would
    /// mean the library screen could not use one code path for "browsing" and
    /// "searching".
    #[must_use]
    pub const fn everything() -> Self {
        Self { root: None }
    }

    /// The expression, or `None` when this query selects everything.
    #[must_use]
    pub const fn root(&self) -> Option<&Expr> {
        self.root.as_ref()
    }

    /// Whether this query selects everything, having no terms to narrow by.
    #[must_use]
    pub const fn is_everything(&self) -> bool {
        self.root.is_none()
    }
}

/// One node of a query.
///
/// Precedence is already resolved: the parser has applied `NOT` tighter than
/// `AND`, and `AND` tighter than `OR`, so a consumer walks the tree it is given
/// and never re-decides how `a b OR c` groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Every child must match. Written by writing terms next to each other, or
    /// with an explicit `AND`.
    All(Vec<Expr>),
    /// At least one child must match. Written with `OR`.
    Any(Vec<Expr>),
    /// The child must not match. Written with a leading `-`, or with `NOT`.
    Not(Box<Expr>),
    /// A single condition.
    Term(Term),
}

/// A single condition in a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// Text that must appear, ignoring case, in `field`.
    Text {
        /// Where the text has to appear.
        field: TextField,
        /// The text to look for, folded once at parse time.
        needle: FoldedText,
    },
    /// The favourite flag, from a bare `favourite` or from `favourite:false`.
    Favourite(bool),
    /// The day a row belongs to, compared against `value`.
    Date {
        /// How the row's date has to compare to `value`.
        comparison: Comparison,
        /// The date written in the query.
        value: Date,
    },
    /// How long a row lasts, compared against `value`.
    Duration {
        /// How the row's duration has to compare to `value`.
        comparison: Comparison,
        /// The duration written in the query.
        value: Duration,
    },
}

/// Which text a [`Term::Text`] has to appear in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TextField {
    /// Any of the text a row carries: its title, its game, its session, its
    /// tags and its event types. This is what a bare word or a quoted phrase
    /// searches, and it is the only variant a user reaches without naming a
    /// field.
    Anywhere,
    /// The game's name, from `game:`.
    Game,
    /// The session's name, from `session:`.
    Session,
    /// The title of the recording, clip or screenshot, from `title:`.
    Title,
    /// One of the row's tags, from `tag:`.
    Tag,
    /// One of the row's event types, from `event:`.
    Event,
}

/// How a row's value has to compare to the one written in the query.
///
/// `date:2026-08-11` and `duration:>90s` are the two shapes: an unadorned value
/// means [`Equal`](Comparison::Equal), and `>`, `>=`, `<` and `<=` mean what
/// they look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Comparison {
    /// `=`, or no operator at all.
    Equal,
    /// `<`.
    Less,
    /// `<=`.
    LessOrEqual,
    /// `>`.
    Greater,
    /// `>=`.
    GreaterOrEqual,
}

impl Comparison {
    /// Whether a row whose value compares to the query's as `ordering` matches.
    #[must_use]
    pub const fn accepts(self, ordering: Ordering) -> bool {
        match self {
            Self::Equal => matches!(ordering, Ordering::Equal),
            Self::Less => matches!(ordering, Ordering::Less),
            Self::LessOrEqual => matches!(ordering, Ordering::Less | Ordering::Equal),
            Self::Greater => matches!(ordering, Ordering::Greater),
            Self::GreaterOrEqual => matches!(ordering, Ordering::Greater | Ordering::Equal),
        }
    }

    /// The operator as it is written in a query, for a message that quotes it
    /// back. [`Equal`](Comparison::Equal) writes nothing, because that is how a
    /// user writes it.
    #[must_use]
    pub const fn as_written(self) -> &'static str {
        match self {
            Self::Equal => "",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Greater => ">",
            Self::GreaterOrEqual => ">=",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Comparison, Expr, Query, Term, TextField};
    use crate::search::text::FoldedText;
    use core::cmp::Ordering;

    #[test]
    fn a_comparison_accepts_exactly_the_orderings_it_is_named_for() {
        let cases = [
            (Comparison::Equal, [false, true, false]),
            (Comparison::Less, [true, false, false]),
            (Comparison::LessOrEqual, [true, true, false]),
            (Comparison::Greater, [false, false, true]),
            (Comparison::GreaterOrEqual, [false, true, true]),
        ];
        for (comparison, expected) in cases {
            let actual = [Ordering::Less, Ordering::Equal, Ordering::Greater]
                .map(|ordering| comparison.accepts(ordering));
            assert_eq!(actual, expected, "{comparison:?}");
        }
    }

    #[test]
    fn an_empty_query_is_a_query_rather_than_a_failure() {
        assert!(Query::everything().is_everything());
        assert_eq!(Query::everything().root(), None);

        let narrowed = Query::new(Expr::Term(Term::Text {
            field: TextField::Anywhere,
            needle: FoldedText::new("ace"),
        }));
        assert!(!narrowed.is_everything());
        assert!(narrowed.root().is_some());
    }
}
