//! The thing a query is matched against.

use core::time::Duration;

use super::date::Date;
use super::text::FoldedText;

/// One searchable thing in the library: a recording, a clip, a screenshot or a
/// session.
///
/// This is deliberately **not** a database row. Nothing here knows what the
/// library's schema will be, what a primary key looks like or how a tag is
/// stored, because none of that is settled
/// ([issue #55](https://github.com/wildware-uk/clipped/issues/55),
/// [issue #56](https://github.com/wildware-uk/clipped/issues/56)) and the query
/// language should not have to change when it is. A row is the *projection*
/// whatever holds the library produces: the text a search can look in, the flag
/// it can filter by and the two values it can compare. What a row identifies is
/// the caller's business — it holds the identifier alongside the row it built.
///
/// It is built once and matched many times, so every piece of text is folded on
/// the way in ([`FoldedText`]) and matching allocates nothing.
///
/// ```
/// use clipped_library::search::{Query, Row};
///
/// let row = Row::new()
///     .with_game("Counter-Strike 2")
///     .with_title("Ace on Mirage")
///     .with_tag("clutch")
///     .favourite(true);
///
/// let query: Query = "game:counter kill favourite".parse()?;
/// assert!(!query.matches(&row), "the row is a favourite, but nothing on it says kill");
/// # Ok::<(), clipped_library::search::QueryError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    titles: Vec<FoldedText>,
    game: Option<FoldedText>,
    session: Option<FoldedText>,
    tags: Vec<FoldedText>,
    events: Vec<FoldedText>,
    favourite: bool,
    date: Option<Date>,
    duration: Option<Duration>,
}

impl Row {
    /// A row with no text, not favourited, with no date and no duration.
    ///
    /// Every absent value matches nothing rather than everything: a row with no
    /// date is not selected by `date:>2026-08-01`, in the same way a row with
    /// no tags is not selected by `tag:clutch`. It is still selected by the
    /// negation of either, because "not after that date" is true of a thing
    /// with no date.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A title of something the row covers: a recording, a clip or a
    /// screenshot.
    ///
    /// Added rather than replacing, the way [`with_tag`](Self::with_tag) is. A
    /// row is often a *sitting* rather than a single file — the library screen
    /// lists sessions — and a sitting that produced three named clips is
    /// searchable by all three. Assigning here kept only the last, so two of
    /// them could not be found by name at all
    /// ([issue #520](https://github.com/wildware-uk/clipped/issues/520)).
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.titles.push(FoldedText::new(title));
        self
    }

    /// The game it belongs to.
    #[must_use]
    pub fn with_game(mut self, game: impl Into<String>) -> Self {
        self.game = Some(FoldedText::new(game));
        self
    }

    /// The session it belongs to.
    #[must_use]
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(FoldedText::new(session));
        self
    }

    /// Adds a tag.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(FoldedText::new(tag));
        self
    }

    /// Adds an event type, such as the `kill` a highlight was cut from.
    #[must_use]
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.events.push(FoldedText::new(event));
        self
    }

    /// Whether the user has favourited it.
    #[must_use]
    pub fn favourite(mut self, favourite: bool) -> Self {
        self.favourite = favourite;
        self
    }

    /// The day it belongs to, on the user's calendar.
    #[must_use]
    pub fn with_date(mut self, date: Date) -> Self {
        self.date = Some(date);
        self
    }

    /// How long it lasts.
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    pub(super) fn titles(&self) -> &[FoldedText] {
        &self.titles
    }

    pub(super) fn game(&self) -> Option<&FoldedText> {
        self.game.as_ref()
    }

    pub(super) fn session(&self) -> Option<&FoldedText> {
        self.session.as_ref()
    }

    pub(super) fn tags(&self) -> &[FoldedText] {
        &self.tags
    }

    pub(super) fn events(&self) -> &[FoldedText] {
        &self.events
    }

    pub(super) const fn is_favourite(&self) -> bool {
        self.favourite
    }

    pub(super) const fn date(&self) -> Option<Date> {
        self.date
    }

    pub(super) const fn duration(&self) -> Option<Duration> {
        self.duration
    }
}
