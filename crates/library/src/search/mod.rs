//! Local search: the query language, its parser, and a matcher for it.
//!
//! # What it does
//!
//! Turns what a user typed into a search box into a [`Query`], and answers
//! whether a [`Row`] matches one. `docs/search.md` is the user-facing
//! description of the language; this is the implementation of it, and SPEC.md
//! section 30 is the brief both come from.
//!
//! ```
//! use clipped_library::search::{Query, Row};
//!
//! let query: Query = "game:cs2 kill favourite -tag:spoiler".parse()?;
//!
//! let clip = Row::new()
//!     .with_game("cs2")
//!     .with_title("Ace on Mirage")
//!     .with_event("kill")
//!     .favourite(true);
//!
//! assert!(query.matches(&clip));
//! # Ok::<(), clipped_library::search::QueryError>(())
//! ```
//!
//! # The language, in short
//!
//! | Written | Means |
//! | --- | --- |
//! | `ace` | the text appears somewhere on the row |
//! | `"grand final"` | the phrase appears, spaces and all |
//! | `game:cs2` | in the game's name; also `session:`, `title:`, `tag:`, `event:` |
//! | `favourite` | favourited; also `favourite:true` and `favourite:false` |
//! | `date:>2026-08-01` | the day it belongs to; `<`, `<=`, `>`, `>=`, or none for that day |
//! | `duration:>90s` | how long it lasts; `90s`, `5m`, `1h30m` |
//! | `-tag:spoiler` | not that; `NOT` says the same |
//! | `a b` | both; `AND` says the same |
//! | `a OR b` | either |
//! | `(a OR b) c` | brackets, when the precedence is not what you want |
//!
//! `NOT` binds tighter than `AND`, which binds tighter than `OR`. Text matching
//! is a case-insensitive substring test, folded through [`fold`]. An empty
//! query selects the whole library, and a malformed one is a [`QueryError`]
//! that names a position and what was expected there — never an empty result
//! set (AGENTS.md section 45).
//!
//! # What it is not
//!
//! - **Not an index.** Matching a query against a million rows one at a time is
//!   what the database is for; this module defines what a match *is*.
//! - **Not a ranking.** A row matches or it does not. Ordering the results is
//!   the library screen's decision, not the query's.
//! - **Not coupled to storage.** [`Row`] is a projection a caller builds, not a
//!   schema. That is what lets the language exist and be tested before the
//!   library index ([issue #56](https://github.com/wildware-uk/clipped/issues/56))
//!   and the database behind it
//!   ([issue #55](https://github.com/wildware-uk/clipped/issues/55)) are built.
//!
//! # How a database-backed executor uses this
//!
//! It consumes the same [`Query`] this module produces, and walks it instead of
//! calling [`Query::matches`]:
//!
//! - [`Expr::All`], [`Expr::Any`] and [`Expr::Not`] become `AND`, `OR` and
//!   `NOT` around bracketed fragments, which is why the parser resolves
//!   precedence into the tree rather than leaving it to the caller.
//! - [`Term::Text`] becomes a `LIKE '%' || ? || '%'` against a **folded**
//!   column, with [`FoldedText::folded`] as the bound parameter. It must not
//!   use SQLite's `COLLATE NOCASE`, which folds ASCII only and would answer
//!   differently from this module for every non-ASCII game, tag and title. That
//!   is what [`fold`] is public for: the indexer writes the folded column with
//!   it, so the two agree by construction.
//! - [`Term::Favourite`] is a boolean column; [`Term::Date`] and
//!   [`Term::Duration`] are comparisons against columns worth an index, using
//!   [`Comparison`] as written.
//! - [`TextField::Anywhere`] is the one that decides the schema: it is an `OR`
//!   across every text column, or a single denormalised column holding all of
//!   them folded together. Either satisfies this module's definition.
//!
//! Whatever it does, [`Query::matches`] stays the reference answer, and the two
//! disagreeing is a bug in the executor.
//!
//! # Threading
//!
//! Everything here is a value. Parsing and matching allocate no shared state,
//! touch no I/O and take no lock, so a query can be parsed on one thread and
//! matched on several.

mod date;
mod error;
mod lexer;
mod matcher;
mod parser;
mod query;
mod row;
mod text;

use core::str::FromStr;

pub use date::Date;
pub use error::QueryError;
pub use query::{Comparison, Expr, Query, Term, TextField};
pub use row::Row;
pub use text::{fold, FoldedText};

impl FromStr for Query {
    type Err = QueryError;

    /// Parses a query.
    ///
    /// # Errors
    ///
    /// [`QueryError`], naming the position of the problem and what was expected
    /// there. Empty text is not an error: it parses to
    /// [`Query::everything`].
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        parser::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use super::{Date, Query, Row};
    use core::time::Duration;
    use std::time::Instant;

    /// Parsing is fed whatever a user types, so it has to fail rather than
    /// panic on any of it.
    ///
    /// Every string of up to four characters over an alphabet of the syntax's
    /// own punctuation is 15,000 or so queries, most of them nonsense — which
    /// is the point: each one has to come back as a query or as an error that
    /// names a position inside the text.
    #[test]
    fn no_arrangement_of_the_syntax_can_panic_the_parser() {
        let alphabet = ['a', '"', '(', ')', '-', ':', '>', ' ', '\\'];
        let mut queries = vec![String::new()];
        let mut previous = vec![String::new()];
        for _ in 0..4 {
            let mut next = Vec::with_capacity(previous.len() * alphabet.len());
            for stem in &previous {
                for character in alphabet {
                    let mut candidate = stem.clone();
                    candidate.push(character);
                    next.push(candidate);
                }
            }
            queries.extend(next.iter().cloned());
            previous = next;
        }
        assert!(queries.len() > 6_000, "{} queries", queries.len());

        for query in &queries {
            match query.parse::<Query>() {
                Ok(parsed) => {
                    // A parsed query has to be usable, not merely built.
                    let _ = parsed.matches(&Row::new().with_title("a"));
                }
                Err(error) => {
                    let length = query.chars().count();
                    assert!(
                        error.position() < length.max(1),
                        "{query:?} was refused at position {}, which is past its end: {error}",
                        error.position()
                    );
                    assert!(!error.to_string().is_empty(), "{query:?}");
                }
            }
        }
    }

    /// A row for each of `count` recordings, deterministic in `index` so that
    /// the number a query selects can be asserted rather than believed.
    fn fixture_library(count: usize) -> Vec<Row> {
        const GAMES: [&str; 5] = [
            "Counter-Strike 2",
            "Apex Legends",
            "Minecraft",
            "Elden Ring",
            "Мир танков",
        ];
        (0..count)
            .map(|index| {
                let mut row = Row::new()
                    .with_game(GAMES[index % GAMES.len()])
                    .with_session(format!("Session {}", index / 20))
                    .with_title(format!("Match {index} on Mirage"))
                    .favourite(index % 11 == 0)
                    .with_date(
                        Date::new(2026, 8, u8::try_from(1 + index % 28).expect("1 to 28"))
                            .expect("a real date"),
                    )
                    .with_duration(Duration::from_secs(
                        u64::try_from(30 + index % 600).expect("a small number of seconds"),
                    ));
                if index % 7 == 0 {
                    row = row.with_tag("clutch");
                }
                if index % 3 == 0 {
                    row = row.with_event("kill");
                }
                row
            })
            .collect()
    }

    /// The measurement documented in `docs/search.md`.
    ///
    /// What each query should select is stated a second time, as a predicate
    /// over the index the fixture was built from, and asserted. Timing a query
    /// that quietly selects nothing would measure nothing, and a count taken
    /// from the matcher itself would prove nothing.
    ///
    /// The time assertion is deliberately far above what this takes: it is
    /// there to catch a change that makes matching accidentally quadratic, not
    /// to measure a machine that may be running eight other builds. The number
    /// that means something is the one printed, which is why it is printed.
    #[test]
    fn a_large_library_is_searched_in_a_measured_time() {
        const ROWS: usize = 100_000;
        let rows = fixture_library(ROWS);

        // A query, and what the fixture says it should select.
        type Case = (&'static str, fn(usize) -> bool);

        let cases: [Case; 6] = [
            ("", |_| true),
            ("mirage", |_| true),
            ("game:counter kill favourite", |index| {
                index % 5 == 0 && index % 3 == 0 && index % 11 == 0
            }),
            ("тан", |index| index % 5 == 4),
            (r#"game:"Elden Ring" duration:>5m -favourite"#, |index| {
                index % 5 == 3 && 30 + index % 600 > 300 && index % 11 != 0
            }),
            (
                "date:>=2026-08-20 (tag:clutch OR event:kill) -game:minecraft",
                |index| {
                    1 + index % 28 >= 20 && (index % 7 == 0 || index % 3 == 0) && index % 5 != 2
                },
            ),
        ];

        for (query, should_match) in cases {
            let expected = (0..ROWS).filter(|index| should_match(*index)).count();
            let parsed: Query = query.parse().expect("the query parses");

            let start = Instant::now();
            let matched = rows.iter().filter(|row| parsed.matches(row)).count();
            let elapsed = start.elapsed();

            eprintln!("search: {ROWS} rows, {elapsed:?}, {matched} matched, query {query:?}");
            assert_eq!(matched, expected, "{query}");
            assert!(
                elapsed < Duration::from_secs(5),
                "{query} took {elapsed:?} over {ROWS} rows"
            );
        }
    }
}
