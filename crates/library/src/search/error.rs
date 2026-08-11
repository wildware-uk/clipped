//! Why a query could not be parsed, and where.

use core::fmt;

use super::parser::KNOWN_FIELDS_FOR_MESSAGE;

/// Why a query could not be parsed.
///
/// A search that cannot be understood must say so. The alternative — dropping
/// the part that did not parse and running the rest, or returning nothing at
/// all — tells a user their library is empty when what actually happened is
/// that they mistyped a field name, and there is no way for them to tell those
/// two apart (AGENTS.md section 45). So every variant here carries a position
/// and names what was expected there, and every message is a sentence a search
/// box can show as it is (AGENTS.md section 28).
///
/// The enum is exhaustive on purpose. A new way for a query to be wrong needs
/// new words in front of the user, and a `_ =>` arm in the desktop application
/// would let one ship without them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// A field-qualified term named a field Clipped does not have.
    UnknownField {
        /// The name as it was written.
        name: String,
        /// Where the name starts.
        position: usize,
    },
    /// A field was named with nothing after the colon.
    MissingValue {
        /// The field's name as it was written.
        field: String,
        /// Where the field's name starts.
        position: usize,
    },
    /// A pair of quotes with nothing between them, which would match every row
    /// and so cannot be what the user meant.
    EmptyTerm {
        /// Where the empty term starts.
        position: usize,
    },
    /// A quoted phrase was opened and never closed.
    UnterminatedQuote {
        /// Where the opening quote is.
        position: usize,
    },
    /// A `date:` term whose value is not a date on the calendar.
    InvalidDate {
        /// The value as it was written.
        text: String,
        /// Where the value starts.
        position: usize,
    },
    /// A `duration:` term whose value is not a length of time.
    InvalidDuration {
        /// The value as it was written.
        text: String,
        /// Where the value starts.
        position: usize,
    },
    /// A `favourite:` term whose value is neither a yes nor a no.
    InvalidFavourite {
        /// The value as it was written.
        text: String,
        /// Where the value starts.
        position: usize,
    },
    /// A comparison operator on a field that is not compared but matched:
    /// `game:>cs2`, or `favourite:>true`.
    ComparisonNotSupported {
        /// The field's name as it was written.
        field: String,
        /// The operator as it was written.
        operator: String,
        /// Where the operator is.
        position: usize,
    },
    /// An operator with nothing for it to operate on: a trailing `OR`, a `-`
    /// with no term after it, or an empty pair of brackets.
    MissingOperand {
        /// The operator as it was written.
        operator: String,
        /// Where the operator is.
        position: usize,
    },
    /// A bracket that is never closed.
    UnclosedGroup {
        /// Where the opening bracket is.
        position: usize,
    },
    /// A closing bracket with no opening one.
    UnexpectedGroupEnd {
        /// Where the closing bracket is.
        position: usize,
    },
}

impl QueryError {
    /// Where in the query the problem is, as a **0-based** offset in
    /// characters — not in bytes, so that a query containing anything outside
    /// ASCII still points at the right place.
    ///
    /// Use this to underline the offending text. The message written by
    /// [`Display`](fmt::Display) counts from one instead, because a person
    /// counting characters in their own query starts at one; a caller that
    /// shows both should show the underline rather than the number.
    #[must_use]
    pub const fn position(&self) -> usize {
        match self {
            Self::UnknownField { position, .. }
            | Self::MissingValue { position, .. }
            | Self::EmptyTerm { position }
            | Self::UnterminatedQuote { position }
            | Self::InvalidDate { position, .. }
            | Self::InvalidDuration { position, .. }
            | Self::InvalidFavourite { position, .. }
            | Self::ComparisonNotSupported { position, .. }
            | Self::MissingOperand { position, .. }
            | Self::UnclosedGroup { position }
            | Self::UnexpectedGroupEnd { position } => *position,
        }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Counted from one for the reader; `position()` is the offset a caller
        // underlines with.
        let at = self.position() + 1;
        match self {
            Self::UnknownField { name, .. } => write!(
                formatter,
                "`{name}` at position {at} is not something Clipped can search by. \
                 Use {KNOWN_FIELDS_FOR_MESSAGE}, or put the text in quotes to search for it \
                 as words"
            ),
            Self::MissingValue { field, .. } => write!(
                formatter,
                "`{field}:` at position {at} says what to search but not what to look for. \
                 Write the value after the colon, such as {field}:cs2"
            ),
            Self::EmptyTerm { .. } => write!(
                formatter,
                "the quotes at position {at} have nothing between them. \
                 Put the words to search for inside them, or remove them"
            ),
            Self::UnterminatedQuote { .. } => write!(
                formatter,
                "the quote opened at position {at} is never closed. \
                 Add a closing quote, or remove the opening one"
            ),
            Self::InvalidDate { text, .. } => write!(
                formatter,
                "`{text}` at position {at} is not a date on the calendar. \
                 Write a date as year-month-day, such as date:2026-08-11, \
                 date:>2026-08-01 or date:<=2026-08-31"
            ),
            Self::InvalidDuration { text, .. } => write!(
                formatter,
                "`{text}` at position {at} is not a length of time. \
                 Write a number and a unit, such as duration:>30s, duration:<5m \
                 or duration:>=1h30m"
            ),
            Self::InvalidFavourite { text, .. } => write!(
                formatter,
                "`{text}` at position {at} is neither yes nor no. \
                 Write favourite on its own for favourites, -favourite for everything else, \
                 or favourite:true and favourite:false to be explicit"
            ),
            Self::ComparisonNotSupported {
                field, operator, ..
            } => write!(
                formatter,
                "`{field}:` cannot be compared with `{operator}` at position {at}. \
                 Only date: and duration: are compared with < and >; for `{field}:` write \
                 the value on its own, or quote it to search for `{operator}` as text"
            ),
            // One variant, three shapes of operator, because "`-` needs a term
            // on each side of it" would be wrong and "`OR` has nothing after
            // it" would be wrong for a leading OR.
            Self::MissingOperand { operator, .. } => match operator.as_str() {
                "-" | "NOT" => write!(
                    formatter,
                    "`{operator}` at position {at} has nothing after it to leave out. \
                     Write what to exclude, such as -favourite, or remove the `{operator}`"
                ),
                "(" => write!(
                    formatter,
                    "the brackets at position {at} have nothing between them. \
                     Put part of the search inside them, or remove them"
                ),
                _ => write!(
                    formatter,
                    "`{operator}` at position {at} needs something to search for on each \
                     side of it. Add the missing side, or remove the `{operator}`"
                ),
            },
            Self::UnclosedGroup { .. } => write!(
                formatter,
                "the bracket opened at position {at} is never closed. \
                 Add a closing bracket, or remove the opening one"
            ),
            Self::UnexpectedGroupEnd { .. } => write!(
                formatter,
                "the closing bracket at position {at} has no opening bracket before it. \
                 Remove it, or add the bracket it should close"
            ),
        }
    }
}

impl core::error::Error for QueryError {}

#[cfg(test)]
mod tests {
    use super::QueryError;

    /// Every message has to name the position, or the user is left hunting
    /// through their own query for the character Clipped objected to.
    #[test]
    fn every_message_names_the_position_counted_from_one() {
        let errors = [
            QueryError::UnknownField {
                name: "colour".to_owned(),
                position: 4,
            },
            QueryError::MissingValue {
                field: "game".to_owned(),
                position: 4,
            },
            QueryError::EmptyTerm { position: 4 },
            QueryError::UnterminatedQuote { position: 4 },
            QueryError::InvalidDate {
                text: "2026-13-01".to_owned(),
                position: 4,
            },
            QueryError::InvalidDuration {
                text: "5x".to_owned(),
                position: 4,
            },
            QueryError::InvalidFavourite {
                text: "maybe".to_owned(),
                position: 4,
            },
            QueryError::ComparisonNotSupported {
                field: "game".to_owned(),
                operator: ">".to_owned(),
                position: 4,
            },
            QueryError::MissingOperand {
                operator: "OR".to_owned(),
                position: 4,
            },
            QueryError::MissingOperand {
                operator: "-".to_owned(),
                position: 4,
            },
            QueryError::MissingOperand {
                operator: "(".to_owned(),
                position: 4,
            },
            QueryError::UnclosedGroup { position: 4 },
            QueryError::UnexpectedGroupEnd { position: 4 },
        ];

        for error in errors {
            assert_eq!(error.position(), 4, "{error:?}");
            let message = error.to_string();
            assert!(
                message.contains("position 5"),
                "{error:?} does not name its position: {message}"
            );
            assert!(
                !message.is_empty() && message.len() > 40,
                "{error:?} does not say enough to act on: {message}"
            );
        }
    }
}
