//! Tokens to a [`Query`], and every message a malformed query produces.
//!
//! The grammar, in the order the functions below appear:
//!
//! ```text
//! query   := alternatives?
//! alternatives := conjunction ( "OR" conjunction )*
//! conjunction  := unary ( "AND"? unary )*
//! unary        := ( "-" | "NOT" ) unary | "(" alternatives ")" | term
//! term         := field ":" comparison? value | value
//! ```
//!
//! So `NOT` binds tighter than `AND`, which binds tighter than `OR`, and
//! writing two terms next to each other means `AND`. That is the precedence
//! every search box the user has ever used has, and the tree the parser returns
//! has it applied already: nothing downstream re-decides how `a b OR c` groups.

use core::time::Duration;

use super::date::Date;
use super::error::QueryError;
use super::lexer::{self, RawTerm, Spanned, Token};
use super::query::{Comparison, Expr, Query, Term, TextField};
use super::text::{fold, FoldedText};

/// The fields, as [`QueryError::UnknownField`] lists them back to the user.
///
/// This is prose rather than a generated list because it is user-facing copy
/// (AGENTS.md section 28) — but it is not allowed to drift from what
/// [`field_from_name`] accepts, which
/// `the_message_listing_the_fields_lists_only_fields_that_work` asserts.
pub(super) const KNOWN_FIELDS_FOR_MESSAGE: &str =
    "game:, session:, title:, tag:, event:, date:, duration: or favourite";

/// What a field name means once it is recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// A field matched as text.
    Text(TextField),
    /// `date:`, compared against a calendar date.
    Date,
    /// `duration:`, compared against a length of time.
    Duration,
    /// `favourite:`, which is a yes or a no.
    Favourite,
}

/// The field a name refers to, if any.
///
/// Names are matched case-insensitively, and both spellings of "favourite" are
/// accepted: the product is written in British English, and a user typing the
/// American spelling into a search box has made no mistake worth a message.
/// The plurals are accepted because a row has several tags and several events,
/// and `tags:` is what a user types about half the time.
fn field_from_name(name: &str) -> Option<Field> {
    match fold(name).as_str() {
        "game" => Some(Field::Text(TextField::Game)),
        "session" => Some(Field::Text(TextField::Session)),
        "title" => Some(Field::Text(TextField::Title)),
        "tag" | "tags" => Some(Field::Text(TextField::Tag)),
        "event" | "events" => Some(Field::Text(TextField::Event)),
        "date" => Some(Field::Date),
        "duration" => Some(Field::Duration),
        "favourite" | "favorite" => Some(Field::Favourite),
        _ => None,
    }
}

/// Whether a bare word is the favourite flag rather than text to look for.
///
/// SPEC.md section 30 writes the example query as `game:cs2 kill favourite`, so
/// a bare `favourite` is a filter and not a word to find in a title. Quoting it
/// — `"favourite"` — searches for the word, which is the same escape quoting
/// provides everywhere else in this syntax.
fn is_favourite_flag(value: &str) -> bool {
    matches!(fold(value).as_str(), "favourite" | "favorite")
}

/// Parses `query`.
///
/// # Errors
///
/// [`QueryError`], which names a position and what was expected there. An empty
/// query is not an error: it is [`Query::everything`].
pub(super) fn parse(query: &str) -> Result<Query, QueryError> {
    let tokens = lexer::tokenise(query)?;
    let mut parser = Parser {
        tokens: &tokens,
        index: 0,
    };
    let root = parser.alternatives()?;

    // Anything left over is a token nothing could consume. Each one has its own
    // message, because "unexpected token" is not something a user can act on.
    if let Some(spanned) = parser.peek() {
        return Err(match &spanned.token {
            Token::GroupEnd => QueryError::UnexpectedGroupEnd {
                position: spanned.position,
            },
            Token::Or => QueryError::MissingOperand {
                operator: "OR".to_owned(),
                position: spanned.position,
            },
            Token::And => QueryError::MissingOperand {
                operator: "AND".to_owned(),
                position: spanned.position,
            },
            // `unary` consumes all three of these, so reaching one here would
            // mean it returned without consuming what it looked at.
            // `no_arrangement_of_the_syntax_can_panic_the_parser` is the guard
            // on that claim.
            Token::Not { .. } | Token::Term(_) | Token::GroupStart => unreachable!(
                "a term or a negation at position {} was left unparsed",
                spanned.position
            ),
        });
    }

    Ok(root.map_or_else(Query::everything, Query::new))
}

/// The position in the token stream.
struct Parser<'tokens> {
    tokens: &'tokens [Spanned],
    index: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Spanned> {
        self.tokens.get(self.index)
    }

    /// Consumes the next token if `wanted` matches it, returning its position.
    fn eat(&mut self, wanted: &Token) -> Option<usize> {
        let position = self.peek().filter(|next| &next.token == wanted)?.position;
        self.index += 1;
        Some(position)
    }

    /// `conjunction ( "OR" conjunction )*`
    fn alternatives(&mut self) -> Result<Option<Expr>, QueryError> {
        let Some(first) = self.conjunction()? else {
            return Ok(None);
        };
        let mut alternatives = vec![first];
        while let Some(position) = self.eat(&Token::Or) {
            let next = self.conjunction()?.ok_or(QueryError::MissingOperand {
                operator: "OR".to_owned(),
                position,
            })?;
            alternatives.push(next);
        }
        Ok(Some(collapse(alternatives, Expr::Any)))
    }

    /// `unary ( "AND"? unary )*` — terms written next to each other mean `AND`.
    fn conjunction(&mut self) -> Result<Option<Expr>, QueryError> {
        let mut conjuncts = Vec::new();
        loop {
            if !conjuncts.is_empty() {
                if let Some(position) = self.eat(&Token::And) {
                    let next = self.unary()?.ok_or(QueryError::MissingOperand {
                        operator: "AND".to_owned(),
                        position,
                    })?;
                    conjuncts.push(next);
                    continue;
                }
            }
            match self.unary()? {
                Some(expression) => conjuncts.push(expression),
                None => break,
            }
        }
        if conjuncts.is_empty() {
            return Ok(None);
        }
        Ok(Some(collapse(conjuncts, Expr::All)))
    }

    /// `( "-" | "NOT" ) unary | "(" alternatives ")" | term`
    fn unary(&mut self) -> Result<Option<Expr>, QueryError> {
        let Some(spanned) = self.peek() else {
            return Ok(None);
        };
        let position = spanned.position;
        match spanned.token.clone() {
            Token::Not { written } => {
                self.index += 1;
                let inner = self.unary()?.ok_or(QueryError::MissingOperand {
                    operator: written.to_owned(),
                    position,
                })?;
                Ok(Some(Expr::Not(Box::new(inner))))
            }
            Token::GroupStart => {
                self.index += 1;
                let inner = self.alternatives()?;
                if self.eat(&Token::GroupEnd).is_none() {
                    return Err(QueryError::UnclosedGroup { position });
                }
                inner.map(Some).ok_or(QueryError::MissingOperand {
                    operator: "(".to_owned(),
                    position,
                })
            }
            Token::Term(raw) => {
                self.index += 1;
                term(raw, position).map(Some)
            }
            Token::Or | Token::And | Token::GroupEnd => Ok(None),
        }
    }
}

/// One expression from several, without wrapping a single one in a list.
fn collapse(mut expressions: Vec<Expr>, combine: fn(Vec<Expr>) -> Expr) -> Expr {
    if expressions.len() == 1 {
        // The length was just checked, so the element is there.
        expressions.remove(0)
    } else {
        combine(expressions)
    }
}

/// What a term means: the only place a field name, a date or a length is
/// judged, and so the only place those messages are written.
fn term(raw: RawTerm, position: usize) -> Result<Expr, QueryError> {
    let Some(field) = raw.field else {
        if raw.value.is_empty() {
            return Err(QueryError::EmptyTerm { position });
        }
        if !raw.quoted && is_favourite_flag(&raw.value) {
            return Ok(Expr::Term(Term::Favourite(true)));
        }
        return Ok(Expr::Term(Term::Text {
            field: TextField::Anywhere,
            needle: FoldedText::new(raw.value),
        }));
    };

    let Some(kind) = field_from_name(&field.name) else {
        return Err(QueryError::UnknownField {
            name: field.name,
            position: field.position,
        });
    };

    let comparison = match (&raw.operator, kind) {
        (Some(operator), Field::Text(_) | Field::Favourite) => {
            return Err(QueryError::ComparisonNotSupported {
                field: field.name,
                operator: operator.written.to_owned(),
                position: operator.position,
            })
        }
        (Some(operator), Field::Date | Field::Duration) => match operator.written {
            ">" => Comparison::Greater,
            ">=" => Comparison::GreaterOrEqual,
            "<" => Comparison::Less,
            // The lexer produces exactly the four operators in its table.
            _ => Comparison::LessOrEqual,
        },
        (None, _) => Comparison::Equal,
    };

    if raw.value.is_empty() {
        return Err(QueryError::MissingValue {
            field: field.name,
            position: field.position,
        });
    }

    Ok(Expr::Term(match kind {
        Field::Text(text_field) => Term::Text {
            field: text_field,
            needle: FoldedText::new(raw.value),
        },
        Field::Date => Term::Date {
            comparison,
            value: parse_date(&raw.value, raw.value_position)?,
        },
        Field::Duration => Term::Duration {
            comparison,
            value: parse_duration(&raw.value, raw.value_position)?,
        },
        Field::Favourite => Term::Favourite(parse_favourite(&raw.value, raw.value_position)?),
    }))
}

/// `2026-08-11`, and nothing else.
///
/// One format, because a search box cannot ask whether `03-04-2026` is March or
/// April and guessing wrong returns the wrong recordings silently.
fn parse_date(value: &str, position: usize) -> Result<Date, QueryError> {
    let invalid = || QueryError::InvalidDate {
        text: value.to_owned(),
        position,
    };
    let mut parts = value.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    if (year.len(), month.len(), day.len()) != (4, 2, 2) {
        return Err(invalid());
    }
    // Only ASCII digits: `str::parse` also accepts a leading `+`, and
    // `date:+026-08-11` is a typing mistake rather than a date.
    let digits = |part: &str| part.chars().all(|character| character.is_ascii_digit());
    if !digits(year) || !digits(month) || !digits(day) {
        return Err(invalid());
    }
    let (Ok(year), Ok(month), Ok(day)) = (year.parse(), month.parse(), day.parse()) else {
        return Err(invalid());
    };
    Date::new(year, month, day).ok_or_else(invalid)
}

/// `90s`, `5m`, `1h30m`: a number and a unit, at least once.
///
/// A bare number is refused rather than assumed to be seconds. Half a library's
/// durations are minutes and half are seconds, and a wrong guess here is a
/// filter that quietly selects the wrong recordings.
fn parse_duration(value: &str, position: usize) -> Result<Duration, QueryError> {
    let invalid = || QueryError::InvalidDuration {
        text: value.to_owned(),
        position,
    };

    let mut seconds: u64 = 0;
    let mut digits = String::new();
    let mut used = [false; 3];
    let mut units = 0;

    for character in value.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        let unit = match character.to_ascii_lowercase() {
            'h' => 0,
            'm' => 1,
            's' => 2,
            _ => return Err(invalid()),
        };
        if digits.is_empty() || used[unit] {
            return Err(invalid());
        }
        let amount: u64 = digits.parse().map_err(|_| invalid())?;
        seconds = amount
            .checked_mul([3600, 60, 1][unit])
            .and_then(|amount| seconds.checked_add(amount))
            .ok_or_else(invalid)?;
        digits.clear();
        used[unit] = true;
        units += 1;
    }

    if units == 0 || !digits.is_empty() {
        return Err(invalid());
    }
    Ok(Duration::from_secs(seconds))
}

/// `favourite:true`, and the words a person actually types for yes and no.
fn parse_favourite(value: &str, position: usize) -> Result<bool, QueryError> {
    match fold(value).as_str() {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(QueryError::InvalidFavourite {
            text: value.to_owned(),
            position,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{field_from_name, parse, KNOWN_FIELDS_FOR_MESSAGE};
    use crate::search::error::QueryError;
    use crate::search::query::{Comparison, Expr, Term, TextField};
    use crate::search::text::FoldedText;
    use core::time::Duration;

    fn root(query: &str) -> Expr {
        parse(query)
            .unwrap_or_else(|error| panic!("{query} should parse: {error}"))
            .root()
            .expect("the query has terms")
            .clone()
    }

    fn error(query: &str) -> QueryError {
        parse(query).expect_err(query)
    }

    fn text(field: TextField, needle: &str) -> Expr {
        Expr::Term(Term::Text {
            field,
            needle: FoldedText::new(needle),
        })
    }

    #[test]
    fn the_example_from_the_specification_parses_to_what_it_says() {
        assert_eq!(
            root("game:cs2 kill favourite"),
            Expr::All(vec![
                text(TextField::Game, "cs2"),
                text(TextField::Anywhere, "kill"),
                Expr::Term(Term::Favourite(true)),
            ])
        );
    }

    #[test]
    fn every_field_reaches_the_term_it_names() {
        assert_eq!(root("game:cs2"), text(TextField::Game, "cs2"));
        assert_eq!(root("session:friday"), text(TextField::Session, "friday"));
        assert_eq!(root("title:ace"), text(TextField::Title, "ace"));
        assert_eq!(root("tag:funny"), text(TextField::Tag, "funny"));
        assert_eq!(root("tags:funny"), text(TextField::Tag, "funny"));
        assert_eq!(root("event:kill"), text(TextField::Event, "kill"));
        assert_eq!(root("events:kill"), text(TextField::Event, "kill"));
        assert_eq!(root("GAME:cs2"), text(TextField::Game, "cs2"));
        assert_eq!(root("favourite"), Expr::Term(Term::Favourite(true)));
        assert_eq!(root("favorite"), Expr::Term(Term::Favourite(true)));
        assert_eq!(root("favourite:false"), Expr::Term(Term::Favourite(false)));
        assert_eq!(root("favourite:no"), Expr::Term(Term::Favourite(false)));
        assert_eq!(
            root("date:2026-08-11"),
            Expr::Term(Term::Date {
                comparison: Comparison::Equal,
                value: crate::search::Date::new(2026, 8, 11).expect("a real date"),
            })
        );
        assert_eq!(
            root("duration:>=1h30m"),
            Expr::Term(Term::Duration {
                comparison: Comparison::GreaterOrEqual,
                value: Duration::from_secs(5400),
            })
        );
    }

    #[test]
    fn every_comparison_operator_is_read() {
        let cases = [
            ("duration:>30s", Comparison::Greater),
            ("duration:>=30s", Comparison::GreaterOrEqual),
            ("duration:<30s", Comparison::Less),
            ("duration:<=30s", Comparison::LessOrEqual),
            ("duration:30s", Comparison::Equal),
        ];
        for (query, expected) in cases {
            assert_eq!(
                root(query),
                Expr::Term(Term::Duration {
                    comparison: expected,
                    value: Duration::from_secs(30),
                }),
                "{query}"
            );
        }
    }

    #[test]
    fn a_length_may_be_written_in_any_of_its_units() {
        let cases = [
            ("duration:90s", 90),
            ("duration:5m", 300),
            ("duration:2h", 7200),
            ("duration:1h30m", 5400),
            ("duration:1h2m3s", 3723),
            ("duration:1H30M", 5400),
        ];
        for (query, seconds) in cases {
            assert_eq!(
                root(query),
                Expr::Term(Term::Duration {
                    comparison: Comparison::Equal,
                    value: Duration::from_secs(seconds),
                }),
                "{query}"
            );
        }
    }

    #[test]
    fn a_length_that_is_not_one_says_so_rather_than_guessing() {
        for query in [
            "duration:90",
            "duration:m",
            "duration:5x",
            "duration:5m5m",
            "duration:1h30",
            "duration:-5m",
            "duration:٣m",
            "duration:99999999999999999999h",
        ] {
            assert!(
                matches!(error(query), QueryError::InvalidDuration { .. }),
                "{query} was accepted, or refused for the wrong reason: {:?}",
                parse(query)
            );
        }
    }

    #[test]
    fn a_date_that_is_not_one_says_so_rather_than_guessing() {
        for query in [
            "date:2026-13-01",
            "date:2026-02-30",
            "date:2026-8-1",
            "date:2026-08",
            "date:11-08-2026",
            "date:yesterday",
            "date:2026-08-01-01",
        ] {
            assert!(
                matches!(error(query), QueryError::InvalidDate { .. }),
                "{query} was accepted, or refused for the wrong reason: {:?}",
                parse(query)
            );
        }
    }

    #[test]
    fn precedence_is_not_before_and_before_or() {
        assert_eq!(
            root("a b OR c"),
            Expr::Any(vec![
                Expr::All(vec![
                    text(TextField::Anywhere, "a"),
                    text(TextField::Anywhere, "b")
                ]),
                text(TextField::Anywhere, "c"),
            ])
        );
        assert_eq!(
            root("a AND b OR c"),
            root("a b OR c"),
            "an explicit AND means what writing two terms together means"
        );
        assert_eq!(
            root("-a b"),
            Expr::All(vec![
                Expr::Not(Box::new(text(TextField::Anywhere, "a"))),
                text(TextField::Anywhere, "b"),
            ]),
            "a negation binds to its own term, not to the rest of the query"
        );
        assert_eq!(root("NOT a b"), root("-a b"));
        assert_eq!(
            root("(a OR b) c"),
            Expr::All(vec![
                Expr::Any(vec![
                    text(TextField::Anywhere, "a"),
                    text(TextField::Anywhere, "b")
                ]),
                text(TextField::Anywhere, "c"),
            ]),
            "brackets override the precedence"
        );
        assert_eq!(
            root("-(a OR b)"),
            Expr::Not(Box::new(Expr::Any(vec![
                text(TextField::Anywhere, "a"),
                text(TextField::Anywhere, "b"),
            ]))),
            "a negation applies to a bracketed group"
        );
        assert_eq!(
            root("--a"),
            Expr::Not(Box::new(Expr::Not(Box::new(text(
                TextField::Anywhere,
                "a"
            ))))),
            "negation nests"
        );
    }

    #[test]
    fn a_quoted_phrase_is_one_term_and_escapes_the_syntax() {
        assert_eq!(
            root(r#""grand final""#),
            text(TextField::Anywhere, "grand final")
        );
        assert_eq!(
            root(r#""favourite""#),
            text(TextField::Anywhere, "favourite"),
            "quoting the word searches for it instead of filtering by the flag"
        );
        assert_eq!(
            root(r#""game:cs2""#),
            text(TextField::Anywhere, "game:cs2"),
            "quoting escapes the field syntax"
        );
        assert_eq!(
            root(r#""OR""#),
            text(TextField::Anywhere, "OR"),
            "quoting escapes the operator words"
        );
        assert_eq!(
            root(r#""-a""#),
            text(TextField::Anywhere, "-a"),
            "quoting escapes the leading hyphen"
        );
    }

    #[test]
    fn an_empty_query_selects_everything_rather_than_failing() {
        for query in ["", "   ", "\t\n"] {
            let parsed = parse(query).expect("an empty query is not a mistake");
            assert!(parsed.is_everything(), "{query:?}");
        }
    }

    #[test]
    fn an_unknown_field_is_named_and_placed_rather_than_returning_nothing() {
        let error = error("game:cs2 colour:red");
        assert_eq!(
            error,
            QueryError::UnknownField {
                name: "colour".to_owned(),
                position: 9,
            }
        );
        let message = error.to_string();
        assert!(message.contains("colour"), "{message}");
        assert!(message.contains("position 10"), "{message}");
        assert!(message.contains("game:"), "{message}");
    }

    /// The list in the message is copy, and copy drifts. Every field it names
    /// has to be one the parser accepts.
    #[test]
    fn the_message_listing_the_fields_lists_only_fields_that_work() {
        let named: Vec<&str> = KNOWN_FIELDS_FOR_MESSAGE
            .split(|character: char| !character.is_alphabetic())
            .filter(|word| !word.is_empty() && *word != "or")
            .collect();
        assert_eq!(named.len(), 8, "{named:?}");
        for name in named {
            assert!(
                field_from_name(name).is_some(),
                "the message offers `{name}:`, which the parser does not accept"
            );
        }
    }

    #[test]
    fn each_way_a_query_can_be_malformed_is_reported_at_its_own_position() {
        assert_eq!(
            error("game:"),
            QueryError::MissingValue {
                field: "game".to_owned(),
                position: 0,
            }
        );
        assert_eq!(
            error("ace game:"),
            QueryError::MissingValue {
                field: "game".to_owned(),
                position: 4,
            }
        );
        assert_eq!(error(r#"ace """#), QueryError::EmptyTerm { position: 4 });
        assert_eq!(
            error(r#"ace "clutch"#),
            QueryError::UnterminatedQuote { position: 4 }
        );
        assert_eq!(
            error("game:>cs2"),
            QueryError::ComparisonNotSupported {
                field: "game".to_owned(),
                operator: ">".to_owned(),
                position: 5,
            }
        );
        assert_eq!(
            error("favourite:>true"),
            QueryError::ComparisonNotSupported {
                field: "favourite".to_owned(),
                operator: ">".to_owned(),
                position: 10,
            }
        );
        assert_eq!(
            error("favourite:maybe"),
            QueryError::InvalidFavourite {
                text: "maybe".to_owned(),
                position: 10,
            }
        );
        assert_eq!(
            error("ace OR"),
            QueryError::MissingOperand {
                operator: "OR".to_owned(),
                position: 4,
            }
        );
        assert_eq!(
            error("OR ace"),
            QueryError::MissingOperand {
                operator: "OR".to_owned(),
                position: 0,
            }
        );
        assert_eq!(
            error("ace AND"),
            QueryError::MissingOperand {
                operator: "AND".to_owned(),
                position: 4,
            }
        );
        assert_eq!(
            error("ace -"),
            QueryError::MissingOperand {
                operator: "-".to_owned(),
                position: 4,
            }
        );
        assert_eq!(
            error("ace NOT"),
            QueryError::MissingOperand {
                operator: "NOT".to_owned(),
                position: 4,
            }
        );
        assert_eq!(
            error("ace ()"),
            QueryError::MissingOperand {
                operator: "(".to_owned(),
                position: 4,
            }
        );
        assert_eq!(
            error("(ace OR clutch"),
            QueryError::UnclosedGroup { position: 0 }
        );
        assert_eq!(
            error("ace (clutch"),
            QueryError::UnclosedGroup { position: 4 }
        );
        assert_eq!(
            error("ace) clutch"),
            QueryError::UnexpectedGroupEnd { position: 3 }
        );
    }

    /// Positions are character offsets, so a query with a Cyrillic word in it
    /// points at the same character a person would count to.
    #[test]
    fn a_position_is_counted_in_characters_even_when_the_query_is_not_ascii() {
        assert_eq!(
            error("замес colour:red"),
            QueryError::UnknownField {
                name: "colour".to_owned(),
                position: 6,
            }
        );
        assert_eq!(
            error("замес game:"),
            QueryError::MissingValue {
                field: "game".to_owned(),
                position: 6,
            }
        );
    }

    #[test]
    fn a_needle_is_folded_once_when_the_query_is_parsed() {
        let Expr::Term(Term::Text { needle, .. }) = root("game:CS2") else {
            panic!("a text term")
        };
        assert_eq!(needle.text(), "CS2");
        assert_eq!(needle.folded(), "cs2");
    }
}
