//! Query text to tokens, each remembering where it came from.
//!
//! The lexer knows nothing about fields, dates or durations: it splits the text
//! into brackets, operators and terms, and records for each term whether it was
//! quoted, what came before its colon and what came after. Deciding whether
//! `colour` is a field and whether `5x` is a length is [the parser's
//! job](super::parser), which is where every message the user sees is written.
//!
//! **Positions are counted in characters, not bytes.** A user counts characters
//! and a text box selects characters, so a query with a Cyrillic game name in it
//! must not report a position seven along when the mistake is four along.

use super::error::QueryError;

/// A token and where it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Spanned {
    pub(super) token: Token,
    /// 0-based character offset of the token's first character.
    pub(super) position: usize,
}

/// What the query text is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Token {
    /// `(`
    GroupStart,
    /// `)`
    GroupEnd,
    /// `OR`
    Or,
    /// `AND`
    And,
    /// `-` or `NOT`, remembering which was written so a message can quote it.
    Not { written: &'static str },
    /// Everything else.
    Term(RawTerm),
}

/// A term as it was written, before anything is known about what it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawTerm {
    /// The name before the colon, when there was one.
    pub(super) field: Option<RawField>,
    /// A leading `<`, `<=`, `>` or `>=` on an unquoted field value.
    pub(super) operator: Option<RawOperator>,
    /// The value, with quotes removed and escapes resolved.
    pub(super) value: String,
    /// Where the value starts, after the field name and the operator.
    pub(super) value_position: usize,
    /// Whether any part of the value was quoted. A quoted value is text and
    /// nothing else: it is never an operator word, and it never carries a
    /// comparison.
    pub(super) quoted: bool,
}

/// A field name as it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawField {
    pub(super) name: String,
    pub(super) position: usize,
}

/// A comparison operator as it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawOperator {
    pub(super) written: &'static str,
    pub(super) position: usize,
}

/// The comparison operators, longest first so that `>=` is not read as `>`.
const OPERATORS: [&str; 4] = [">=", "<=", ">", "<"];

/// Splits `query` into tokens.
///
/// # Errors
///
/// [`QueryError::UnterminatedQuote`] is the only failure the lexer can find on
/// its own: a quote that is never closed leaves no way to tell where the term
/// was meant to end.
pub(super) fn tokenise(query: &str) -> Result<Vec<Spanned>, QueryError> {
    let characters: Vec<char> = query.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < characters.len() {
        let position = index;
        let character = characters[index];
        if character.is_whitespace() {
            index += 1;
            continue;
        }
        let token = match character {
            '(' => {
                index += 1;
                Token::GroupStart
            }
            ')' => {
                index += 1;
                Token::GroupEnd
            }
            // A `-` only negates at the start of a term; inside one it is an
            // ordinary character, which is what makes `counter-strike` a single
            // word and `-favourite` an exclusion.
            '-' => {
                index += 1;
                Token::Not { written: "-" }
            }
            _ => {
                let (token, next) = read_term(&characters, index)?;
                index = next;
                token
            }
        };
        tokens.push(Spanned { token, position });
    }

    Ok(tokens)
}

/// Reads one term, starting at a character that is not whitespace or a bracket.
///
/// Returns the token and the index to carry on from.
fn read_term(characters: &[char], start: usize) -> Result<(Token, usize), QueryError> {
    let mut index = start;
    let mut buffer = String::new();
    let mut field: Option<RawField> = None;
    let mut value_position = start;
    // Whether the part being read now — the field name, or the value once a
    // field has been split off — contained a quote.
    let mut quoted = false;

    while index < characters.len() {
        let character = characters[index];
        if character.is_whitespace() || character == '(' || character == ')' {
            break;
        }
        if character == '"' {
            index = read_quoted(characters, index, &mut buffer)?;
            quoted = true;
            continue;
        }
        // A colon only introduces a field when what precedes it could be a
        // field name: unquoted, non-empty and letters only. That is what keeps
        // `12:30` and `21:04–22:37` as text a user can search for, and it is
        // why quoting a term is a complete escape from this syntax.
        if character == ':' && field.is_none() && !quoted && is_field_name(&buffer) {
            field = Some(RawField {
                name: buffer.clone(),
                position: start,
            });
            buffer.clear();
            index += 1;
            value_position = index;
            quoted = false;
            continue;
        }
        buffer.push(character);
        index += 1;
    }

    let mut operator = None;
    if field.is_some() && !quoted {
        if let Some(written) = OPERATORS
            .into_iter()
            .find(|candidate| buffer.starts_with(candidate))
        {
            operator = Some(RawOperator {
                written,
                position: value_position,
            });
            buffer.drain(..written.len());
            value_position += written.len();
        }
    }

    // The operator words are recognised in capitals only. A user searching for
    // a clip called "not my finest hour" types words, not operators, and the
    // capitals are what tells the two apart without a second escape rule
    // (quoting is the other way to say "this is text").
    if field.is_none() && !quoted {
        let word = match buffer.as_str() {
            "OR" => Some(Token::Or),
            "AND" => Some(Token::And),
            "NOT" => Some(Token::Not { written: "NOT" }),
            _ => None,
        };
        if let Some(token) = word {
            return Ok((token, index));
        }
    }

    Ok((
        Token::Term(RawTerm {
            field,
            operator,
            value: buffer,
            value_position,
            quoted,
        }),
        index,
    ))
}

/// Reads a quoted run into `buffer`, returning the index after the closing
/// quote.
///
/// `\"` and `\\` are the two escapes; a backslash before anything else is an
/// ordinary backslash, because a user typing a Windows path into a search box
/// means the backslash.
fn read_quoted(
    characters: &[char],
    opening: usize,
    buffer: &mut String,
) -> Result<usize, QueryError> {
    let mut index = opening + 1;
    while index < characters.len() {
        match characters[index] {
            '"' => return Ok(index + 1),
            '\\' if matches!(characters.get(index + 1), Some('"' | '\\')) => {
                buffer.push(characters[index + 1]);
                index += 2;
            }
            character => {
                buffer.push(character);
                index += 1;
            }
        }
    }
    Err(QueryError::UnterminatedQuote { position: opening })
}

/// Whether `candidate` could be a field name: letters, and at least one.
fn is_field_name(candidate: &str) -> bool {
    !candidate.is_empty() && candidate.chars().all(char::is_alphabetic)
}

#[cfg(test)]
mod tests {
    use super::{tokenise, RawTerm, Token};
    use crate::search::error::QueryError;

    /// The terms of a query, for tests that only care about what was read.
    fn terms(query: &str) -> Vec<RawTerm> {
        tokenise(query)
            .expect("the query lexes")
            .into_iter()
            .filter_map(|spanned| match spanned.token {
                Token::Term(term) => Some(term),
                _ => None,
            })
            .collect()
    }

    fn only_term(query: &str) -> RawTerm {
        let mut terms = terms(query);
        assert_eq!(terms.len(), 1, "{query} is one term");
        terms.remove(0)
    }

    #[test]
    fn a_field_is_split_off_at_its_colon() {
        let term = only_term("game:cs2");
        assert_eq!(
            term.field.as_ref().map(|field| field.name.as_str()),
            Some("game")
        );
        assert_eq!(term.value, "cs2");
        assert_eq!(term.value_position, 5);
        assert!(term.operator.is_none());
    }

    #[test]
    fn text_that_only_looks_like_a_field_stays_text() {
        for query in ["12:30", "21:04", "3:2"] {
            let term = only_term(query);
            assert!(term.field.is_none(), "{query} named a field");
            assert_eq!(term.value, query);
        }
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces_and_escapes() {
        let term = only_term(r#"title:"grand \"final\" round""#);
        assert_eq!(
            term.field.as_ref().map(|field| field.name.as_str()),
            Some("title")
        );
        assert_eq!(term.value, r#"grand "final" round"#);
        assert!(term.quoted);
    }

    #[test]
    fn a_comparison_is_taken_off_the_front_of_a_field_value() {
        let term = only_term("duration:>=1h30m");
        let operator = term.operator.expect("the operator is read");
        assert_eq!(operator.written, ">=");
        assert_eq!(operator.position, 9);
        assert_eq!(term.value, "1h30m");
        assert_eq!(term.value_position, 11);
    }

    #[test]
    fn a_quoted_value_carries_no_comparison_so_that_quoting_escapes_the_syntax() {
        let term = only_term(r#"title:">""#);
        assert!(term.operator.is_none());
        assert_eq!(term.value, ">");
    }

    #[test]
    fn the_operator_words_are_capitals_only() {
        let tokens: Vec<Token> = tokenise("a OR b or c NOT d not e AND f and g")
            .expect("the query lexes")
            .into_iter()
            .map(|spanned| spanned.token)
            .collect();
        let operators: Vec<&Token> = tokens
            .iter()
            .filter(|token| !matches!(token, Token::Term(_)))
            .collect();
        assert_eq!(
            operators,
            vec![&Token::Or, &Token::Not { written: "NOT" }, &Token::And],
            "lower-case operator words are ordinary text"
        );
        let words: Vec<String> = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Term(term) => Some(term.value.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            words,
            ["a", "b", "or", "c", "d", "not", "e", "f", "and", "g"]
        );
    }

    #[test]
    fn a_hyphen_negates_only_at_the_start_of_a_term() {
        let tokens: Vec<Token> = tokenise("-favourite counter-strike")
            .expect("the query lexes")
            .into_iter()
            .map(|spanned| spanned.token)
            .collect();
        assert_eq!(tokens[0], Token::Not { written: "-" });
        let words: Vec<String> = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Term(term) => Some(term.value.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(words, ["favourite", "counter-strike"]);
    }

    #[test]
    fn brackets_end_a_term_without_whitespace() {
        let tokens: Vec<Token> = tokenise("(ace OR clutch)")
            .expect("the query lexes")
            .into_iter()
            .map(|spanned| spanned.token)
            .collect();
        assert_eq!(tokens.first(), Some(&Token::GroupStart));
        assert_eq!(tokens.last(), Some(&Token::GroupEnd));
        assert_eq!(tokens.len(), 5);
    }

    /// Positions are character offsets. This query is four characters before
    /// the mistake and seven bytes, and the seven is the wrong answer.
    #[test]
    fn positions_count_characters_rather_than_bytes() {
        let query = "кс2 colour:red";
        assert_eq!(query.chars().count(), 14);
        assert_eq!(query.len(), 16, "the query is longer in bytes");
        let term = only_term(query.split(' ').nth(1).expect("two words"));
        assert_eq!(term.value, "red");

        let tokens = tokenise(query).expect("the query lexes");
        let field = match &tokens[1].token {
            Token::Term(term) => term.field.as_ref().expect("a field was written"),
            other => panic!("expected a term, got {other:?}"),
        };
        assert_eq!(field.position, 4);
        assert_eq!(tokens[1].position, 4);
    }

    #[test]
    fn a_quote_that_is_never_closed_is_reported_where_it_opened() {
        assert_eq!(
            tokenise(r#"game:"cs2"#),
            Err(QueryError::UnterminatedQuote { position: 5 })
        );
        assert_eq!(
            tokenise(r#"кс2 "grand final"#),
            Err(QueryError::UnterminatedQuote { position: 4 })
        );
    }
}
