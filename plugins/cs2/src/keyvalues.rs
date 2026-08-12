//! Just enough of Valve's KeyValues format to read a `gamestate_integration`
//! file.
//!
//! Counter-Strike's configuration files are KeyValues: quoted keys followed by
//! either a quoted value or a brace-delimited block, with `//` comments.
//!
//! ```text
//! "Clipped Game State Integration v1"
//! {
//!     "uri"   "http://127.0.0.1:3212/"   // where to post
//!     "auth"  { "token" "…" }
//! }
//! ```
//!
//! # Why this is here rather than a dependency
//!
//! AGENTS.md section 10 asks whether the functionality is small enough to
//! implement safely, and this is: two token kinds, one nesting rule, no
//! includes, no conditionals, no macros. What it is used for is narrower still
//! — reading back a file this plugin wrote, and looking at one key in files
//! other tools wrote — and both of those are on a path where a mistake means a
//! refusal rather than a wrong answer, because nothing here is *written*
//! through this module. `crate::integration` renders its file from a template.
//!
//! The parts of KeyValues that are deliberately absent are named in
//! [`KeyValuesError`], so a file using one of them is refused by name rather
//! than silently misread.

use core::fmt;

/// A parsed KeyValues document: the entries at the top level, in order.
///
/// Order is kept because a file can repeat a key, and this reader is used to
/// look at files it did not write.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyValues {
    entries: Vec<(String, KeyValue)>,
}

/// What a key maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyValue {
    /// A string. Every scalar in KeyValues is one; `"1"` is how a flag is
    /// written.
    Text(String),
    /// A nested block.
    Block(KeyValues),
}

impl KeyValues {
    /// Reads a document.
    ///
    /// # Errors
    ///
    /// [`KeyValuesError`], naming what was found and where.
    pub fn parse(source: &str) -> Result<Self, KeyValuesError> {
        let mut tokens = Tokens::new(source);
        let document = parse_entries(&mut tokens, 0)?;
        match tokens.take()? {
            None => Ok(document),
            Some(Token::CloseBrace) => Err(KeyValuesError::UnexpectedCloseBrace),
            Some(Token::OpenBrace | Token::Text(_)) => Err(KeyValuesError::TrailingContent),
        }
    }

    /// The entries, in the order the file wrote them.
    #[must_use]
    pub fn entries(&self) -> &[(String, KeyValue)] {
        &self.entries
    }

    /// The first value for `key`, whatever it is.
    ///
    /// Keys are matched without regard to case, because that is how Valve's own
    /// reader matches them and this one is used to read files other tools wrote
    /// (`crate::integration::neighbour_posting_to` looks for their `uri`). A
    /// case-sensitive lookup here would let a neighbouring integration file
    /// that happens to spell it `URI` past the check for two tools on one port,
    /// and Counter-Strike would load both and post to one of them.
    ///
    /// The same rule is already implemented, and for the same reason, in
    /// `clipped_game_detection`'s reader for Steam's files; the two are
    /// duplicates, which
    /// [issue #69](https://github.com/wildware-uk/clipped/issues/69) is where
    /// the shared home for them is being decided.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&KeyValue> {
        self.entries
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }

    /// The first text value for `key`.
    #[must_use]
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            KeyValue::Text(text) => Some(text),
            KeyValue::Block(_) => None,
        }
    }

    /// The first block for `key`.
    #[must_use]
    pub fn block(&self, key: &str) -> Option<&Self> {
        match self.get(key)? {
            KeyValue::Block(block) => Some(block),
            KeyValue::Text(_) => None,
        }
    }
}

/// The most deeply a document may nest.
///
/// A `gamestate_integration` file is three levels at most. The bound is here
/// because the parser recurses and the file is another program's data: a
/// thousand open braces should be a refusal, not a stack overflow.
const MAX_DEPTH: usize = 16;

fn parse_entries(tokens: &mut Tokens<'_>, depth: usize) -> Result<KeyValues, KeyValuesError> {
    if depth > MAX_DEPTH {
        return Err(KeyValuesError::TooDeep);
    }

    let mut entries = Vec::new();
    while let Some(token) = tokens.take()? {
        let key = match token {
            // The caller owns the brace that closes the block it opened, so it
            // goes back where it came from rather than being consumed here.
            Token::CloseBrace => {
                tokens.put_back(Token::CloseBrace);
                break;
            }
            Token::OpenBrace => return Err(KeyValuesError::BlockWithoutKey),
            Token::Text(text) => text,
        };

        let value = match tokens.take()? {
            Some(Token::Text(text)) => KeyValue::Text(text),
            Some(Token::OpenBrace) => {
                let block = parse_entries(tokens, depth + 1)?;
                match tokens.take()? {
                    Some(Token::CloseBrace) => KeyValue::Block(block),
                    _ => return Err(KeyValuesError::UnclosedBlock { key }),
                }
            }
            Some(Token::CloseBrace) | None => return Err(KeyValuesError::KeyWithoutValue { key }),
        };
        entries.push((key, value));
    }
    Ok(KeyValues { entries })
}

/// What was wrong with a KeyValues file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyValuesError {
    /// A quoted string with no closing quote.
    UnterminatedQuote,
    /// A key with nothing after it.
    KeyWithoutValue {
        /// The key.
        key: String,
    },
    /// A block that the file ended inside.
    UnclosedBlock {
        /// The key that opened it.
        key: String,
    },
    /// A `{` where a key was expected.
    BlockWithoutKey,
    /// A `}` with no block open.
    UnexpectedCloseBrace,
    /// Content after the document ended.
    TrailingContent,
    /// More nesting than [`MAX_DEPTH`].
    TooDeep,
}

impl fmt::Display for KeyValuesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedQuote => formatter.write_str("a quoted value has no closing quote"),
            Self::KeyWithoutValue { key } => write!(formatter, "`{key}` has no value after it"),
            Self::UnclosedBlock { key } => {
                write!(formatter, "the block under `{key}` is not closed")
            }
            Self::BlockWithoutKey => formatter.write_str("a `{` appears where a key was expected"),
            Self::UnexpectedCloseBrace => {
                formatter.write_str("a `}` closes a block that was never opened")
            }
            Self::TrailingContent => {
                formatter.write_str("there is content after the end of the document")
            }
            Self::TooDeep => write!(
                formatter,
                "the file nests more than {MAX_DEPTH} levels deep"
            ),
        }
    }
}

impl core::error::Error for KeyValuesError {}

/// One thing in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Text(String),
    OpenBrace,
    CloseBrace,
}

/// The tokeniser, with room to hand one token back.
struct Tokens<'a> {
    rest: &'a str,
    returned: Option<Token>,
}

impl<'a> Tokens<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            rest: source,
            returned: None,
        }
    }

    /// Hands a token back, for the next `take` to return.
    fn put_back(&mut self, token: Token) {
        self.returned = Some(token);
    }

    fn take(&mut self) -> Result<Option<Token>, KeyValuesError> {
        match self.returned.take() {
            Some(token) => Ok(Some(token)),
            None => self.read(),
        }
    }

    fn read(&mut self) -> Result<Option<Token>, KeyValuesError> {
        loop {
            self.rest = self.rest.trim_start();
            // `//` runs to the end of the line, which is how Valve's own
            // example configuration files are annotated.
            if let Some(comment) = self.rest.strip_prefix("//") {
                self.rest = comment.find('\n').map_or("", |end| &comment[end..]);
                continue;
            }
            break;
        }

        let mut characters = self.rest.char_indices();
        let Some((_, first)) = characters.next() else {
            return Ok(None);
        };

        match first {
            '{' => {
                self.rest = &self.rest[1..];
                Ok(Some(Token::OpenBrace))
            }
            '}' => {
                self.rest = &self.rest[1..];
                Ok(Some(Token::CloseBrace))
            }
            '"' => {
                let body = &self.rest[1..];
                let end = body.find('"').ok_or(KeyValuesError::UnterminatedQuote)?;
                self.rest = &body[end + 1..];
                Ok(Some(Token::Text(body[..end].to_owned())))
            }
            // KeyValues allows unquoted words. Nothing this plugin writes uses
            // them, and a file another tool wrote might.
            _ => {
                let end = self
                    .rest
                    .find(|character: char| character.is_whitespace() || "{}\"".contains(character))
                    .unwrap_or(self.rest.len());
                let word = &self.rest[..end];
                self.rest = &self.rest[end..];
                Ok(Some(Token::Text(word.to_owned())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gamestate_integration_file_reads_into_keys_and_blocks() {
        let source = r#"
            // Written by something
            "Some Tool v2"
            {
                "uri"      "http://127.0.0.1:3000/"
                "timeout"  "5.0"
                "auth"
                {
                    "token" "abc123"
                }
                "data"
                {
                    "provider" "1"
                    "map"      "1"
                }
            }
        "#;

        let document = KeyValues::parse(source).expect("a well-formed file");
        let service = document.block("Some Tool v2").expect("the outer block");
        assert_eq!(service.text("uri"), Some("http://127.0.0.1:3000/"));
        assert_eq!(
            service.block("auth").and_then(|auth| auth.text("token")),
            Some("abc123")
        );
        assert_eq!(
            service.block("data").and_then(|data| data.text("map")),
            Some("1")
        );
        assert_eq!(
            document.entries().len(),
            1,
            "the comment is not an entry, and neither is the whitespace"
        );
    }

    #[test]
    fn a_key_is_found_however_the_file_that_wrote_it_spelt_it() {
        // Valve's own reader matches keys without regard to case, and these
        // files are written by other people's tools. `crate::integration` reads
        // a neighbouring file's `uri` to find out whether it is already posting
        // to the port being installed; a lookup that missed `URI` would install
        // a second integration on one port, and Counter-Strike loads both.
        let document = KeyValues::parse(
            r#"
            "Some Other Tool"
            {
                "URI"   "http://127.0.0.1:3212/"
                "Auth"  { "Token" "abc123" }
            }
        "#,
        )
        .expect("a well-formed file");

        let service = document
            .block("some other tool")
            .expect("the outer block, whatever its case");
        assert_eq!(service.text("uri"), Some("http://127.0.0.1:3212/"));
        assert_eq!(
            service.block("auth").and_then(|auth| auth.text("token")),
            Some("abc123")
        );
    }

    #[test]
    fn a_comment_at_the_end_of_a_line_does_not_swallow_the_next_one() {
        let source = "\"a\" \"1\" // why\n\"b\" \"2\"";
        let document = KeyValues::parse(source).expect("a well-formed file");
        assert_eq!(document.text("a"), Some("1"));
        assert_eq!(document.text("b"), Some("2"));
    }

    #[test]
    fn unquoted_words_read_because_other_tools_write_them() {
        let document = KeyValues::parse("uri http://127.0.0.1:3000/").expect("a well-formed file");
        assert_eq!(document.text("uri"), Some("http://127.0.0.1:3000/"));
    }

    #[test]
    fn a_malformed_file_is_refused_by_name() {
        assert_eq!(
            KeyValues::parse("\"uri\" \"unterminated"),
            Err(KeyValuesError::UnterminatedQuote)
        );
        assert_eq!(
            KeyValues::parse("\"uri\""),
            Err(KeyValuesError::KeyWithoutValue {
                key: "uri".to_owned()
            })
        );
        assert_eq!(
            KeyValues::parse("\"block\" {"),
            Err(KeyValuesError::UnclosedBlock {
                key: "block".to_owned()
            })
        );
        assert_eq!(
            KeyValues::parse("{ }"),
            Err(KeyValuesError::BlockWithoutKey)
        );
        assert_eq!(
            KeyValues::parse("\"a\" \"1\" }"),
            Err(KeyValuesError::UnexpectedCloseBrace)
        );
        assert!(!KeyValues::parse("\"a\" \"1\" }")
            .expect_err("a stray brace is refused")
            .to_string()
            .is_empty());
    }

    #[test]
    fn a_file_of_nothing_but_open_braces_is_refused_rather_than_recursed_into() {
        // Another program's data. The bound is what makes a deliberately
        // malformed file a message instead of a stack overflow.
        let bomb = "\"a\" {".repeat(MAX_DEPTH + 4);
        assert_eq!(KeyValues::parse(&bomb), Err(KeyValuesError::TooDeep));
    }

    #[test]
    fn an_empty_file_is_an_empty_document() {
        assert_eq!(
            KeyValues::parse("   // nothing at all\n").expect("it reads"),
            KeyValues::default()
        );
    }
}
