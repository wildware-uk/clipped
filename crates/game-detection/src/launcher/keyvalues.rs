//! Valve's KeyValues text format, as much of it as Steam's own files use.
//!
//! Steam records what it has installed in two kinds of file — the library index
//! `steamapps/libraryfolders.vdf` and one `steamapps/appmanifest_<appid>.acf`
//! per application — and both are written in this format:
//!
//! ```text
//! "AppState"
//! {
//!     "appid"     "730"
//!     "name"      "Counter-Strike 2"
//!     "installdir"    "Counter-Strike Global Offensive"
//!     "InstalledDepots"
//!     {
//!         "732"
//!         {
//!             "manifest"  "8865603041742505094"
//!         }
//!     }
//! }
//! ```
//!
//! A key is followed either by a value or by a braced table of more keys. That
//! is the whole grammar this module implements, and it is enough for every file
//! [`super::steam`] reads.
//!
//! # Why this is not a dependency
//!
//! `keyvalues-parser` and `steamlocate` both exist and both do more than this.
//! The rule (AGENTS.md section 10) is to ask whether the functionality is small
//! enough to implement safely, and the answer here is the file you are reading:
//! a tokeniser and a loop, in a format with three token kinds and no schema,
//! tested against text Valve's own client wrote. Against that, a dependency
//! brings a public API this crate would then be bound to, a second string
//! interner or `Cow` vocabulary, and — for `steamlocate` — a whole opinion about
//! what an installed game is, which is a decision this crate has already made in
//! [`crate::catalogue`] and cannot delegate.
//!
//! # What it deliberately does not do
//!
//! - **Platform conditionals.** `"key" "value" [$WIN32]` is legal KeyValues and
//!   appears in files people write by hand — `gameinfo.txt`, panel layouts. It
//!   does not appear in anything Steam writes for itself, so a conditional is
//!   reported as a syntax error naming the line rather than silently ignored.
//! - **`#base` and `#include`.** Same reasoning: Steam does not write them into
//!   these files, and quietly resolving a path out of a file that should not
//!   have one would be a way to read an arbitrary file off the machine.
//! - **Binary KeyValues.** `appcache/appinfo.vdf` and the icon hashes inside it
//!   are a different, undocumented format. [`super::steam`] says what it does
//!   about icons instead of guessing at that file.
//!
//! # Failure
//!
//! Every failure names the line. The caller adds the file, because this module
//! is handed text and never opens anything (issue #43: a malformed file must
//! fail with something naming the file rather than panicking or yielding
//! nothing). Parsing is iterative over an explicit stack rather than recursive,
//! and nesting is capped, so a file with ten thousand opening braces in it is a
//! [`SyntaxError`] rather than a blown stack — which on Windows is not an error
//! at all but the end of the process.

use std::fmt;

/// The deepest nesting accepted, in tables.
///
/// Steam's own files reach three: the root, the numbered library, its `apps`.
/// The cap exists so that malformed input fails as an error; there is no depth
/// at which a real file becomes unreadable that is not already far past the
/// point of being a real file.
const MAX_DEPTH: usize = 32;

/// A key and what it was assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    /// A quoted or bare string.
    String(String),
    /// A braced table.
    Table(Table),
}

/// A braced table of keys, in the order the file gave them.
///
/// A `Vec` and not a map, because KeyValues permits a key to appear twice and
/// order is sometimes meaning: dropping either would be this module deciding
/// something about a file it did not write. Lookups are linear over tables with
/// a handful of keys in them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Table {
    entries: Vec<(String, Value)>,
}

impl Table {
    /// Every key in the table, in file order.
    pub(crate) fn entries(&self) -> &[(String, Value)] {
        &self.entries
    }

    /// The first value under `key`, compared without case.
    ///
    /// KeyValues is a case-insensitive format, and Steam has taken advantage of
    /// that: the same manifest holds `appid` and `LastUpdated` and `StateFlags`,
    /// and which keys are capitalised has changed between client versions. A
    /// case-sensitive lookup here would be a detector that stops working after a
    /// Steam update, for no reason visible in a diff.
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    }

    /// The first value under `key`, when it is a string.
    pub(crate) fn string(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::String(value)) => Some(value),
            Some(Value::Table(_)) | None => None,
        }
    }

    /// The first value under `key`, when it is a table.
    pub(crate) fn table(&self, key: &str) -> Option<&Self> {
        match self.get(key) {
            Some(Value::Table(table)) => Some(table),
            Some(Value::String(_)) | None => None,
        }
    }
}

/// What was wrong with a file, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxError {
    line: usize,
    problem: Problem,
}

impl SyntaxError {
    /// The one-based line the problem was found on.
    #[cfg(test)]
    pub(crate) const fn line(&self) -> usize {
        self.line
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.problem)
    }
}

impl std::error::Error for SyntaxError {}

/// The ways a KeyValues file can fail to be one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Problem {
    /// A quoted string ran to the end of the file.
    UnterminatedString,
    /// A `{` appeared where a key should have been.
    TableWithoutKey,
    /// A key was followed by `}` or by the end of the file.
    KeyWithoutValue {
        /// The key left waiting.
        key: String,
    },
    /// A `}` appeared with no table open.
    UnmatchedClose,
    /// The file ended with a table still open.
    UnclosedTable {
        /// The key that opened the table still waiting to be closed.
        key: String,
    },
    /// Tables are nested deeper than [`MAX_DEPTH`].
    TooDeep,
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedString => formatter.write_str("a quoted string is never closed"),
            Self::TableWithoutKey => {
                formatter.write_str("a `{` opens a table that no key was given for")
            }
            Self::KeyWithoutValue { key } => {
                write!(formatter, "the key `{key}` has no value after it")
            }
            Self::UnmatchedClose => {
                formatter.write_str("a `}` closes a table that was never opened")
            }
            Self::UnclosedTable { key } => {
                write!(formatter, "the table `{key}` is never closed")
            }
            Self::TooDeep => write!(
                formatter,
                "tables are nested more than {MAX_DEPTH} deep, which no file Steam writes is"
            ),
        }
    }
}

/// Reads KeyValues text into its tables.
///
/// The result is the file's top level, which for everything Steam writes holds
/// exactly one key — `libraryfolders`, `AppState` — so callers ask for that key
/// by name rather than assuming a shape this function could not have checked.
pub(crate) fn parse(text: &str) -> Result<Table, SyntaxError> {
    let mut tokens = Tokens::new(text);

    // The table currently being filled is the last of `open`; the key that
    // opened it is the matching entry of `keys`. An explicit stack rather than
    // recursion, so that depth is a number this function can refuse rather than
    // a stack frame the process cannot survive.
    let mut open: Vec<Table> = vec![Table::default()];
    let mut keys: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;

    while let Some(token) = tokens.next_token()? {
        match token {
            Token::Text(text) => match pending.take() {
                None => pending = Some(text),
                Some(key) => push(&mut open, key, Value::String(text)),
            },
            Token::Open => {
                let Some(key) = pending.take() else {
                    return Err(tokens.error(Problem::TableWithoutKey));
                };
                if open.len() > MAX_DEPTH {
                    return Err(tokens.error(Problem::TooDeep));
                }
                keys.push(key);
                open.push(Table::default());
            }
            Token::Close => {
                if let Some(key) = pending.take() {
                    return Err(tokens.error(Problem::KeyWithoutValue { key }));
                }
                let Some(key) = keys.pop() else {
                    return Err(tokens.error(Problem::UnmatchedClose));
                };
                let finished = open.pop().unwrap_or_default();
                push(&mut open, key, Value::Table(finished));
            }
        }
    }

    if let Some(key) = pending {
        return Err(tokens.error(Problem::KeyWithoutValue { key }));
    }
    if let Some(key) = keys.pop() {
        return Err(tokens.error(Problem::UnclosedTable { key }));
    }

    Ok(open.pop().unwrap_or_default())
}

/// Adds an entry to the table currently being filled.
fn push(open: &mut [Table], key: String, value: Value) {
    if let Some(table) = open.last_mut() {
        table.entries.push((key, value));
    }
}

/// One thing the grammar recognises.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// `{`
    Open,
    /// `}`
    Close,
    /// A quoted or bare string.
    Text(String),
}

/// The tokeniser, which is where lines are counted.
struct Tokens<'a> {
    characters: std::iter::Peekable<std::str::Chars<'a>>,
    line: usize,
}

impl<'a> Tokens<'a> {
    /// A tokeniser over a whole file.
    fn new(text: &'a str) -> Self {
        Self {
            characters: text.chars().peekable(),
            line: 1,
        }
    }

    /// A failure at the line reading has reached.
    fn error(&self, problem: Problem) -> SyntaxError {
        SyntaxError {
            line: self.line,
            problem,
        }
    }

    /// Consumes one character, counting lines.
    fn advance(&mut self) -> Option<char> {
        let character = self.characters.next()?;
        if character == '\n' {
            self.line += 1;
        }
        Some(character)
    }

    /// The next token, or `None` at the end of the file.
    fn next_token(&mut self) -> Result<Option<Token>, SyntaxError> {
        self.skip_gaps();
        let Some(&character) = self.characters.peek() else {
            return Ok(None);
        };
        match character {
            '{' => {
                self.advance();
                Ok(Some(Token::Open))
            }
            '}' => {
                self.advance();
                Ok(Some(Token::Close))
            }
            '"' => self.quoted().map(|text| Some(Token::Text(text))),
            _ => Ok(Some(Token::Text(self.bare()))),
        }
    }

    /// Skips whitespace and `//` comments.
    fn skip_gaps(&mut self) {
        while let Some(&character) = self.characters.peek() {
            if character.is_whitespace() {
                self.advance();
            } else if character == '/' && self.second_is('/') {
                while let Some(character) = self.advance() {
                    if character == '\n' {
                        break;
                    }
                }
            } else {
                return;
            }
        }
    }

    /// Whether the character after the one being peeked at is `wanted`.
    fn second_is(&self, wanted: char) -> bool {
        let mut lookahead = self.characters.clone();
        lookahead.next();
        lookahead.next() == Some(wanted)
    }

    /// Reads a quoted string, applying escapes.
    ///
    /// Steam writes these files with escaping on, which is why every path in
    /// one is spelled `C:\\Program Files (x86)\\Steam`. Reading `\\` as one
    /// backslash is therefore not a nicety: without it every library path is
    /// wrong.
    ///
    /// An escape this format does not define keeps **both** characters. Valve's
    /// own reader yields the escaped character alone, which would turn a
    /// singly-escaped `C:\Program Files` — not something Steam writes, but
    /// something a hand-edited file may contain — into `C:Program Files`, a
    /// different and existing directory. A path that is silently wrong is worse
    /// here than one that is visibly odd.
    fn quoted(&mut self) -> Result<String, SyntaxError> {
        let opened_on = self.line;
        self.advance();
        let mut text = String::new();
        loop {
            let Some(character) = self.advance() else {
                return Err(SyntaxError {
                    line: opened_on,
                    problem: Problem::UnterminatedString,
                });
            };
            match character {
                '"' => return Ok(text),
                '\\' => match self.advance() {
                    Some('\\') => text.push('\\'),
                    Some('"') => text.push('"'),
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some(other) => {
                        text.push('\\');
                        text.push(other);
                    }
                    None => {
                        return Err(SyntaxError {
                            line: opened_on,
                            problem: Problem::UnterminatedString,
                        })
                    }
                },
                other => text.push(other),
            }
        }
    }

    /// Reads an unquoted token, which ends at whitespace or punctuation.
    ///
    /// Escapes are not applied, matching Valve's reader: a bare token is a word,
    /// and the files this module reads use them only for keys.
    fn bare(&mut self) -> String {
        let mut text = String::new();
        while let Some(&character) = self.characters.peek() {
            if character.is_whitespace() || matches!(character, '{' | '}' | '"') {
                break;
            }
            if character == '/' && self.second_is('/') {
                break;
            }
            self.advance();
            text.push(character);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape every file Steam writes has: one named table at the top.
    fn root(text: &str) -> Table {
        parse(text).expect("the fixture parses")
    }

    #[test]
    fn a_table_of_pairs_reads_as_its_pairs() {
        let table = root(
            r#"
"AppState"
{
    "appid"     "730"
    "name"      "Counter-Strike 2"
}
"#,
        );
        let app = table.table("AppState").expect("the root table is AppState");
        assert_eq!(app.string("appid"), Some("730"));
        assert_eq!(app.string("name"), Some("Counter-Strike 2"));
    }

    #[test]
    fn a_doubled_backslash_is_one_backslash() {
        // Not a nicety: Steam writes every path in these files escaped, so
        // reading `\\` as two characters makes every library path wrong.
        let table = root(r#""libraryfolders" { "path" "C:\\Program Files (x86)\\Steam" }"#);
        assert_eq!(
            table
                .table("libraryfolders")
                .and_then(|folders| folders.string("path")),
            Some(r"C:\Program Files (x86)\Steam")
        );
    }

    #[test]
    fn an_escape_the_format_does_not_define_keeps_both_characters() {
        // Valve's reader would yield `C:Program Files`, which is a different
        // directory and one that can exist. Better visibly odd than silently
        // wrong.
        let table = root(r#""root" { "path" "C:\Program Files" }"#);
        assert_eq!(
            table.table("root").and_then(|root| root.string("path")),
            Some(r"C:\Program Files")
        );
    }

    #[test]
    fn keys_are_matched_without_case_as_the_format_defines_them() {
        // Steam has changed the capitalisation of these keys between client
        // versions; a case-sensitive lookup is a detector that stops working
        // after an update.
        let table = root(r#""AppState" { "InstallDir" "Portal 2" }"#);
        let app = table
            .table("appstate")
            .expect("the key matches without regard to case");
        assert_eq!(app.string("installdir"), Some("Portal 2"));
    }

    #[test]
    fn comments_and_bare_tokens_are_read() {
        let table = root(
            r#"
// a comment about the file
"root"
{
    bare_key "a value"   // a comment about the key
}
"#,
        );
        assert_eq!(
            table.table("root").and_then(|root| root.string("bare_key")),
            Some("a value")
        );
    }

    #[test]
    fn a_repeated_key_keeps_both_and_answers_with_the_first() {
        let table = root(r#""root" { "key" "first" "key" "second" }"#);
        let root = table.table("root").expect("the root table");
        assert_eq!(root.string("key"), Some("first"));
        assert_eq!(root.entries().len(), 2);
    }

    #[test]
    fn a_string_asked_for_as_a_table_is_not_one() {
        let table = root(r#""root" { "key" "value" }"#);
        let root = table.table("root").expect("the root table");
        assert_eq!(root.table("key"), None);
        assert_eq!(root.string("missing"), None);
    }

    #[test]
    fn an_unterminated_string_names_the_line_it_opened_on() {
        let error = parse("\"root\"\n{\n    \"key\" \"never closed\n}\n")
            .expect_err("an unterminated string is not valid");
        assert_eq!(error.line(), 3);
        assert!(
            error.to_string().contains("never closed"),
            "the message should say what is wrong: {error}"
        );
    }

    #[test]
    fn a_key_with_no_value_is_refused_and_quoted() {
        let error =
            parse("\"root\"\n{\n    \"key\"\n}\n").expect_err("a key with no value is not valid");
        let message = error.to_string();
        assert!(
            message.contains("`key`"),
            "the message should name the key: {message}"
        );
    }

    #[test]
    fn an_unclosed_table_is_refused_rather_than_returned_half_read() {
        let error = parse("\"root\"\n{\n    \"key\" \"value\"\n")
            .expect_err("an unclosed table is not valid");
        assert!(
            error.to_string().contains("`root`"),
            "the message should name the table: {error}"
        );
    }

    #[test]
    fn a_close_with_nothing_open_is_refused() {
        let error = parse("\"root\" \"value\"\n}\n").expect_err("a stray brace is not valid");
        assert_eq!(error.line(), 2);
    }

    #[test]
    fn a_brace_with_no_key_is_refused() {
        let error = parse("{\n}\n").expect_err("a table with no key is not valid");
        assert!(
            error.to_string().contains("no key"),
            "the message should say what is missing: {error}"
        );
    }

    #[test]
    fn a_platform_conditional_is_refused_rather_than_half_understood() {
        // `[$WIN32]` is legal KeyValues in files people write by hand. Steam
        // does not write one into anything this crate reads, and guessing at
        // what it means would be inventing behaviour; it fails, naming the line.
        let error = parse("\"root\"\n{\n    \"key\" \"value\" [$WIN32]\n}\n")
            .expect_err("a conditional is outside what this reads");
        assert_eq!(error.line(), 4);
    }

    #[test]
    fn ten_thousand_opening_braces_are_an_error_rather_than_a_dead_process() {
        // Recursive descent would take the process out with a stack overflow,
        // which on Windows is not a catchable failure. A malformed file has to
        // fail as an error (issue #43).
        let hostile = "\"key\" {".repeat(10_000);
        let error = parse(&hostile).expect_err("nesting that deep is not a Steam file");
        assert!(
            error.to_string().contains("nested"),
            "the message should say what the limit was: {error}"
        );
    }

    #[test]
    fn an_empty_file_is_an_empty_table_rather_than_a_failure() {
        // A zero-length `libraryfolders.vdf` is a real state: Steam truncates
        // and rewrites it. Nothing is claimed about it here; the caller decides
        // what an absent `libraryfolders` key means.
        assert_eq!(parse("").expect("empty is valid"), Table::default());
        assert_eq!(
            parse("// nothing but a comment\n").expect("a comment is valid"),
            Table::default()
        );
    }
}
