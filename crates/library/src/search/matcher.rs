//! Running a [`Query`] against a [`Row`] in memory.
//!
//! This is the reference meaning of the language. A database-backed executor
//! translates the same tree into SQL rather than calling this
//! ([issue #55](https://github.com/wildware-uk/clipped/issues/55)), and when
//! the two disagree this is the one that is right: it is the definition every
//! test in this module pins down, and it needs no schema to run against.

use super::query::{Expr, Query, Term, TextField};
use super::row::Row;
use super::text::FoldedText;

impl Query {
    /// Whether `row` matches this query.
    ///
    /// A query with no terms in it matches every row: an empty search box is a
    /// request to see the library, not to see nothing.
    #[must_use]
    pub fn matches(&self, row: &Row) -> bool {
        self.root().is_none_or(|expression| expression.matches(row))
    }
}

impl Expr {
    /// Whether `row` matches this expression.
    #[must_use]
    pub fn matches(&self, row: &Row) -> bool {
        match self {
            Self::All(expressions) => expressions.iter().all(|expression| expression.matches(row)),
            Self::Any(expressions) => expressions.iter().any(|expression| expression.matches(row)),
            Self::Not(expression) => !expression.matches(row),
            Self::Term(term) => term.matches(row),
        }
    }
}

impl Term {
    /// Whether `row` satisfies this condition.
    #[must_use]
    pub fn matches(&self, row: &Row) -> bool {
        match self {
            Self::Text { field, needle } => matches_text(*field, needle, row),
            Self::Favourite(wanted) => row.is_favourite() == *wanted,
            // A row with no date or no duration cannot satisfy a comparison
            // against one. It is still selected by the negation of that
            // comparison, which is `Expr::Not`'s business rather than this
            // function's.
            Self::Date { comparison, value } => row
                .date()
                .is_some_and(|date| comparison.accepts(date.cmp(value))),
            Self::Duration { comparison, value } => row
                .duration()
                .is_some_and(|duration| comparison.accepts(duration.cmp(value))),
        }
    }
}

/// Whether `needle` appears in the text `field` selects.
fn matches_text(field: TextField, needle: &FoldedText, row: &Row) -> bool {
    let contains = |text: Option<&FoldedText>| text.is_some_and(|text| text.contains(needle));
    let contains_any =
        |texts: &[FoldedText]| texts.iter().any(|text: &FoldedText| text.contains(needle));

    match field {
        // The fields a bare word searches. Everything a row carries as text is
        // in here, so that typing a word finds it wherever it was written —
        // which is what a user expects a search box to do, and why the field
        // prefixes exist for the times they want to be specific.
        TextField::Anywhere => {
            contains(row.title())
                || contains(row.game())
                || contains(row.session())
                || contains_any(row.tags())
                || contains_any(row.events())
        }
        TextField::Title => contains(row.title()),
        TextField::Game => contains(row.game()),
        TextField::Session => contains(row.session()),
        TextField::Tag => contains_any(row.tags()),
        TextField::Event => contains_any(row.events()),
    }
}

#[cfg(test)]
mod tests {
    use crate::search::{Date, Query, Row};
    use core::time::Duration;

    /// A row with something in every field, so that a test can say what it is
    /// about by changing one thing.
    fn row() -> Row {
        Row::new()
            .with_game("Counter-Strike 2")
            .with_session("Friday Night")
            .with_title("Ace on Mirage")
            .with_tag("clutch")
            .with_tag("ЗАМЕС")
            .with_event("kill")
            .with_event("round_win")
            .favourite(true)
            .with_date(Date::new(2026, 8, 11).expect("a real date"))
            .with_duration(Duration::from_secs(90))
    }

    fn matches(query: &str, row: &Row) -> bool {
        query
            .parse::<Query>()
            .unwrap_or_else(|error| panic!("{query} should parse: {error}"))
            .matches(row)
    }

    #[test]
    fn a_bare_word_looks_in_every_piece_of_text_a_row_carries() {
        let row = row();
        for query in ["mirage", "counter", "friday", "clutch", "round_win"] {
            assert!(matches(query, &row), "{query}");
        }
        assert!(!matches("inferno", &row));
    }

    #[test]
    fn a_field_looks_only_in_the_field_it_names() {
        let row = row();
        assert!(matches("game:counter", &row));
        assert!(!matches("game:mirage", &row), "Mirage is in the title");
        assert!(matches("title:mirage", &row));
        assert!(!matches("title:counter", &row));
        assert!(matches("session:friday", &row));
        assert!(!matches("session:mirage", &row));
        assert!(matches("tag:clutch", &row));
        assert!(!matches("tag:kill", &row), "kill is an event, not a tag");
        assert!(matches("event:kill", &row));
        assert!(!matches("event:clutch", &row));
    }

    #[test]
    fn text_matching_ignores_case_in_any_alphabet() {
        let row = row();
        assert!(matches("game:COUNTER-STRIKE", &row));
        assert!(matches("game:counter-strike", &row));
        assert!(matches("tag:замес", &row));
        assert!(matches("tag:ЗАМЕС", &row));
        assert!(matches("tag:ЗаМеС", &row));
        assert!(matches("замес", &row), "and from a bare word too");
    }

    #[test]
    fn the_favourite_flag_filters_both_ways() {
        let favourited = row();
        let ordinary = row().favourite(false);

        assert!(matches("favourite", &favourited));
        assert!(!matches("favourite", &ordinary));
        assert!(matches("favorite", &favourited), "the American spelling");
        assert!(!matches("-favourite", &favourited));
        assert!(matches("-favourite", &ordinary));
        assert!(matches("favourite:false", &ordinary));
        assert!(!matches("favourite:true", &ordinary));
        assert!(
            !matches(r#""favourite""#, &favourited),
            "quoted, it is a word to find rather than the flag, and no text here has it"
        );
    }

    #[test]
    fn a_date_is_compared_as_a_day() {
        let row = row();
        assert!(matches("date:2026-08-11", &row));
        assert!(!matches("date:2026-08-10", &row));
        assert!(matches("date:>2026-08-10", &row));
        assert!(!matches("date:>2026-08-11", &row));
        assert!(matches("date:>=2026-08-11", &row));
        assert!(matches("date:<2026-09-01", &row));
        assert!(matches("date:<=2026-08-11", &row));
        assert!(!matches("date:<2026-08-11", &row));
        assert!(
            matches("date:>=2026-08-01 date:<=2026-08-31", &row),
            "a range is two comparisons"
        );
    }

    #[test]
    fn a_duration_is_compared_as_a_length() {
        let row = row();
        assert!(matches("duration:90s", &row));
        assert!(matches("duration:>1m", &row));
        assert!(matches("duration:<2m", &row));
        assert!(matches("duration:<=1m30s", &row));
        assert!(!matches("duration:>2m", &row));
        assert!(!matches("duration:5m", &row));
    }

    #[test]
    fn a_row_missing_a_value_matches_no_comparison_against_it() {
        let undated = Row::new().with_title("Ace on Mirage");
        assert!(!matches("date:2026-08-11", &undated));
        assert!(!matches("date:>2026-08-11", &undated));
        assert!(!matches("date:<2026-08-11", &undated));
        assert!(!matches("duration:>0s", &undated));
        assert!(
            matches("-date:>2026-08-11", &undated),
            "but it is not after that date either, so the negation holds"
        );
        assert!(!matches("tag:clutch", &undated));
        assert!(!matches("game:counter", &undated));
    }

    #[test]
    fn the_operators_combine_the_way_the_precedence_says() {
        let row = row();
        assert!(matches("mirage clutch", &row), "both hold");
        assert!(!matches("mirage inferno", &row), "one does not");
        assert!(matches("inferno OR mirage", &row));
        assert!(!matches("inferno OR dust2", &row));
        assert!(
            matches("inferno mirage OR clutch", &row),
            "(inferno AND mirage) OR clutch, and clutch holds"
        );
        assert!(
            !matches("inferno (mirage OR clutch)", &row),
            "brackets change what it means, and inferno does not hold"
        );
        assert!(matches("-inferno", &row));
        assert!(!matches("-mirage", &row));
        assert!(matches("-inferno mirage", &row));
        assert!(matches("NOT inferno", &row));
    }

    #[test]
    fn the_example_query_from_the_specification_selects_what_it_should() {
        let clip = Row::new()
            .with_game("Counter-Strike 2")
            .with_title("Match point")
            .with_event("kill")
            .favourite(true);

        assert!(matches(
            "game:cs2 kill favourite",
            &Row::new()
                .with_game("cs2")
                .with_event("kill")
                .favourite(true)
        ));
        assert!(
            !matches("game:cs2 kill favourite", &clip),
            "the game is written out in full here, so `game:cs2` does not select it"
        );
        assert!(matches("game:counter kill favourite", &clip));
        assert!(
            !matches(
                "game:counter kill favourite",
                &clip.clone().favourite(false)
            ),
            "and it stops matching when it stops being a favourite"
        );
    }

    /// A query that should match nothing matches nothing — including against a
    /// row that has something in every field it could look in.
    #[test]
    fn a_query_that_should_select_nothing_selects_nothing() {
        let rows = [
            row(),
            row().favourite(false),
            Row::new(),
            Row::new().with_title("Ace on Mirage"),
        ];
        let queries = [
            "dust2",
            "game:dust2",
            "tag:dust2",
            "event:dust2",
            "session:dust2",
            "title:dust2",
            "date:1999-01-01",
            "duration:>10h",
            "mirage dust2",
            "favourite -favourite",
            "-mirage mirage",
            r#""on mirage" dust2"#,
        ];
        for query in queries {
            for row in &rows {
                assert!(!matches(query, row), "{query} selected {row:?}");
            }
        }
    }

    #[test]
    fn an_empty_query_selects_every_row() {
        for row in [row(), Row::new()] {
            assert!(matches("", &row));
            assert!(matches("   ", &row));
        }
    }

    #[test]
    fn a_quoted_phrase_matches_across_a_space_and_an_unquoted_one_does_not() {
        let row = row();
        assert!(matches(r#""ace on mirage""#, &row));
        assert!(matches(r#"title:"ace on""#, &row));
        assert!(
            !matches(r#""ace mirage""#, &row),
            "a phrase is a substring, not a set of words"
        );
        assert!(
            matches("ace mirage", &row),
            "unquoted, those are two terms and both hold"
        );
    }
}
