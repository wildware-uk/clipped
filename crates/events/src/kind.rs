//! What happened: the shared vocabulary, the open variant, and what stops the
//! open variant swallowing the shared one.

use core::fmt;

use serde::{Deserialize, Serialize};

/// The longest an identifier may be, in bytes.
///
/// Long enough for `some-plugin.objective_captured_by_team` and short enough
/// that a database column, a log line and a UI label can all hold one without
/// anybody deciding what to do about the overflow.
pub const MAX_IDENTIFIER_BYTES: usize = 64;

/// The longest a user-typed event label may be, in bytes.
///
/// A label is prose a person typed, not an identifier — see [`UserLabel`] —
/// so it is bounded separately from [`MAX_IDENTIFIER_BYTES`] and at the same
/// figure `docs/bookmarks.md` already uses for a bookmark's own label, on the
/// grounds that they are the same kind of field shown back to the same person
/// and there is no reason for the two limits to disagree.
pub const MAX_USER_LABEL_BYTES: usize = 200;

/// What happened, in terms nothing above this crate has to translate.
///
/// # The vocabulary is closed on purpose
///
/// Counter-Strike's Game State Integration, League's Live Client Data API and
/// Dota's GSI are three unrelated shapes, and the point of this enumeration is
/// that the session, the timeline and the highlight rules never see any of
/// them. A variant here is therefore a concept the *application* acts on —
/// something a clip can be cut around, a timeline marker drawn for, or a rule
/// written against — not a concept a particular game happens to have.
///
/// Game-specific detail is not lost; it goes in the event's
/// [`EventPayload`](crate::EventPayload), where it is available to whoever
/// knows what it means and ignorable by everyone else.
///
/// # …except for two variants, which is why the dot matters
///
/// [`Custom`](Self::Custom) is how a plugin says something this list does not
/// cover, and an open variant in a shared vocabulary is also how that
/// vocabulary rots into a bag of strings. What holds it back is a single
/// syntactic rule: **a custom name is namespaced, and a standard tag is not.**
///
/// - `kill`, `round_started` — the vocabulary. No dot.
/// - `acme-cs2.flashbang_blinded_five` — someone's own word. Namespaced, with
///   the plugin's identifier in front of it.
///
/// Three things follow, and they are the reason for the rule:
///
/// 1. **A plugin can never shadow or pre-empt a standard event.** It cannot
///    emit `kill` as a custom name, because that name has no namespace, and it
///    cannot claim `objective_taken` before the project defines it.
/// 2. **A name says who is answerable for it.** A custom event on a timeline
///    that nobody can explain is traceable to the plugin whose namespace it
///    carries, without a registry to consult.
/// 3. **The vocabulary can grow without breaking anything.** A build that has
///    never heard of `objective_taken` reads it as
///    [`Unrecognised`](Self::Unrecognised) and keeps it; a build that has never
///    heard of `acme-cs2.flashbang_blinded_five` still reads it as
///    [`Custom`](Self::Custom), because the rule is syntactic and needs no
///    table. `docs/plugin-api.md` records the promotion path for a custom
///    event that turns out to be universal.
///
/// [`UserLabelled`](Self::UserLabelled) is the third case, and it is not a
/// plugin's word at all: it is what the *user* typed for something Clipped
/// itself produced — an input binding they named, a fingerprint match they
/// labelled. It is marked with its own prefix rather than a namespace, for the
/// reason [`UserLabel`] gives, and issue #345 is the record of why it needed a
/// place separate from both [`Custom`](Self::Custom) and the closed
/// vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum EventKind {
    /// A game the application recognises started running.
    GameStarted,
    /// It stopped.
    GameEnded,
    /// A match, game or session inside the game began.
    MatchStarted,
    /// It finished, however it finished. [`Win`](Self::Win) and
    /// [`Loss`](Self::Loss) say how.
    MatchEnded,
    /// The player killed someone.
    Kill,
    /// The player died.
    Death,
    /// The player helped kill someone without landing the last hit.
    Assist,
    /// A round, wave or life inside a match began.
    RoundStarted,
    /// It ended.
    RoundEnded,
    /// The player, or the player's team, won.
    Win,
    /// The player, or the player's team, lost.
    Loss,
    /// Points were scored. How many is the payload's business.
    Score,
    /// A goal, objective or capture was completed.
    Goal,
    /// The game awarded something: an achievement, a trophy, a level.
    Achievement,
    /// Something this vocabulary does not cover, named by the plugin that
    /// reports it. See the type documentation for what constrains it.
    Custom(CustomName),
    /// Something Clipped itself produced, labelled with a person's own words
    /// rather than a plugin's or the project's. See [`UserLabel`] for the
    /// syntax and why it is not [`Custom`](Self::Custom).
    UserLabelled(UserLabel),
    /// A kind this build has never heard of, kept exactly as it arrived.
    ///
    /// Only ever produced by *reading*. A newer build may store an event whose
    /// kind did not exist when this one was compiled, and `docs/plugin-api.md`
    /// says adding one does not bump the schema version — so without somewhere
    /// for it to go, the whole event would fail to parse and a mark would
    /// vanish from a user's timeline. The envelope around it is frozen and
    /// still readable, so an event this build cannot name is still an event it
    /// can place, draw and hand back unchanged.
    Unrecognised(String),
}

impl EventKind {
    /// The wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::GameStarted => "game_started",
            Self::GameEnded => "game_ended",
            Self::MatchStarted => "match_started",
            Self::MatchEnded => "match_ended",
            Self::Kill => "kill",
            Self::Death => "death",
            Self::Assist => "assist",
            Self::RoundStarted => "round_started",
            Self::RoundEnded => "round_ended",
            Self::Win => "win",
            Self::Loss => "loss",
            Self::Score => "score",
            Self::Goal => "goal",
            Self::Achievement => "achievement",
            Self::Custom(name) => name.as_str(),
            Self::UserLabelled(label) => label.as_str(),
            Self::Unrecognised(tag) => tag,
        }
    }

    /// Whether this build knows what the event means.
    ///
    /// False only for [`Unrecognised`](Self::Unrecognised). A
    /// [`Custom`](Self::Custom) event *is* understood in the only sense this
    /// crate promises: it is a named mark, from a named plugin, at a known
    /// moment.
    #[must_use]
    pub const fn is_recognised(&self) -> bool {
        !matches!(self, Self::Unrecognised(_))
    }
}

impl From<String> for EventKind {
    fn from(tag: String) -> Self {
        match tag.as_str() {
            "game_started" => Self::GameStarted,
            "game_ended" => Self::GameEnded,
            "match_started" => Self::MatchStarted,
            "match_ended" => Self::MatchEnded,
            "kill" => Self::Kill,
            "death" => Self::Death,
            "assist" => Self::Assist,
            "round_started" => Self::RoundStarted,
            "round_ended" => Self::RoundEnded,
            "win" => Self::Win,
            "loss" => Self::Loss,
            "score" => Self::Score,
            "goal" => Self::Goal,
            "achievement" => Self::Achievement,
            // A tag carrying `USER_LABEL_PREFIX` is checked before the
            // namespace rule, because it is syntactically disjoint from a
            // `CustomName` — no dot required, no lowercase-ascii-segment rule —
            // and would otherwise fall through to `NotNamespaced` and be
            // misread as a malformed custom name rather than what it is.
            _ if tag.starts_with(USER_LABEL_PREFIX) => {
                match UserLabel::new(&tag[USER_LABEL_PREFIX.len()..]) {
                    Ok(label) => Self::UserLabelled(label),
                    Err(_) => Self::Unrecognised(tag),
                }
            }
            // A namespaced name is somebody's own word, whether or not this
            // build has met it. One that does not survive validation is kept
            // verbatim rather than repaired: an event nobody can explain is
            // still better than an event nobody can find.
            _ => match CustomName::new(&tag) {
                Ok(name) => Self::Custom(name),
                Err(_) => Self::Unrecognised(tag),
            },
        }
    }
}

impl From<EventKind> for String {
    fn from(kind: EventKind) -> Self {
        match kind {
            EventKind::Unrecognised(tag) => tag,
            EventKind::Custom(name) => name.into_string(),
            EventKind::UserLabelled(label) => label.into_string(),
            ref standard => standard.as_str().to_owned(),
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A namespaced name for an event the shared vocabulary does not cover.
///
/// The syntax, in full:
///
/// ```text
/// namespace.name[.name…]
/// ```
///
/// - Two or more segments separated by `.`, so every custom name carries a
///   namespace. This is the rule that keeps [`EventKind`]'s vocabulary closed:
///   a standard tag never contains a dot, so a plugin cannot emit one.
/// - Each segment starts with an ASCII lowercase letter and continues with
///   lowercase letters, digits, `-` or `_`.
/// - At most [`MAX_IDENTIFIER_BYTES`] bytes in total.
/// - The namespace `clipped` is reserved for the project, so that a name the
///   application appears to have blessed cannot come from a third party.
///
/// Lowercase-only is not fussiness: these names are compared, grouped and
/// written to a database by everything downstream, and `Flag_Captured` beside
/// `flag_captured` is two events on a timeline that a user believes is one.
/// Rejecting the second form is cheaper than case-folding it everywhere it is
/// read.
///
/// By convention the namespace is the reporting plugin's identifier, which
/// makes an unexplained mark on a timeline traceable without a registry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CustomName(String);

/// The namespace reserved for events the project itself defines.
pub const RESERVED_NAMESPACE: &str = "clipped";

impl CustomName {
    /// Validates a custom event name.
    ///
    /// # Errors
    ///
    /// [`InvalidCustomName`] describing which rule the name broke, so the
    /// plugin author reads a sentence rather than a rejection.
    pub fn new(name: &str) -> Result<Self, InvalidCustomName> {
        if name.len() > MAX_IDENTIFIER_BYTES {
            return Err(InvalidCustomName::TooLong {
                name: name.to_owned(),
                bytes: name.len(),
            });
        }
        let mut segments = name.split('.');
        let namespace = segments.next().unwrap_or_default();
        if segments.clone().next().is_none() {
            return Err(InvalidCustomName::NotNamespaced {
                name: name.to_owned(),
            });
        }
        if namespace == RESERVED_NAMESPACE {
            return Err(InvalidCustomName::ReservedNamespace {
                name: name.to_owned(),
            });
        }
        for segment in name.split('.') {
            if !is_valid_segment(segment) {
                return Err(InvalidCustomName::MalformedSegment {
                    name: name.to_owned(),
                    segment: segment.to_owned(),
                });
            }
        }
        Ok(Self(name.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The namespace: everything before the first dot.
    #[must_use]
    pub fn namespace(&self) -> &str {
        self.0.split('.').next().unwrap_or_default()
    }

    /// The name itself, consuming the wrapper.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Whether one dot-separated segment is well formed.
pub(crate) fn is_valid_segment(segment: &str) -> bool {
    let mut characters = segment.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
}

impl fmt::Display for CustomName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for CustomName {
    type Error = InvalidCustomName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(&name)
    }
}

impl From<CustomName> for String {
    fn from(name: CustomName) -> Self {
        name.0
    }
}

/// Why a custom event name was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidCustomName {
    /// It has no namespace, so it would sit in the same space as the shared
    /// vocabulary.
    NotNamespaced {
        /// The name offered.
        name: String,
    },
    /// Its namespace is [`RESERVED_NAMESPACE`].
    ReservedNamespace {
        /// The name offered.
        name: String,
    },
    /// A segment is empty, or contains something other than lowercase ASCII
    /// letters, digits, `-` and `_`, or does not start with a letter.
    MalformedSegment {
        /// The name offered.
        name: String,
        /// The segment that broke the rule.
        segment: String,
    },
    /// It is longer than [`MAX_IDENTIFIER_BYTES`].
    TooLong {
        /// The name offered.
        name: String,
        /// How long it was.
        bytes: usize,
    },
}

impl fmt::Display for InvalidCustomName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotNamespaced { name } => write!(
                f,
                "the custom event name `{name}` has no namespace: a custom name is \
                 `<plugin>.<name>`, so that it cannot collide with a standard event kind"
            ),
            Self::ReservedNamespace { name } => write!(
                f,
                "the custom event name `{name}` uses the `{RESERVED_NAMESPACE}` namespace, \
                 which is reserved for events Clipped itself defines"
            ),
            Self::MalformedSegment { name, segment } => write!(
                f,
                "the custom event name `{name}` has an invalid segment `{segment}`: each \
                 segment starts with a lowercase ASCII letter and continues with lowercase \
                 letters, digits, `-` or `_`"
            ),
            Self::TooLong { name, bytes } => write!(
                f,
                "the custom event name `{name}` is {bytes} bytes, over the \
                 {MAX_IDENTIFIER_BYTES}-byte limit"
            ),
        }
    }
}

impl core::error::Error for InvalidCustomName {}

/// The marker that opens a user-typed event label on the wire.
///
/// Not a namespace: [`CustomName`] requires two or more dot-separated
/// segments, each starting with a lowercase ASCII letter, because its
/// namespace is an *identifier* — the reporting plugin's own name, compared
/// and looked up by other code. A user's label is prose: "My Ultimate!",
/// "clutch", a fingerprint the user typed with capitals and an accent in it.
/// Forcing it through `CustomName`'s syntax would mean either inventing a
/// namespace nobody asked the user for, or slugging their words into
/// something that no longer reads back as what they wrote — and issue #345's
/// acceptance criterion is that the label round-trips *intact*, not
/// normalised. So [`UserLabel`] is marked with a prefix a `CustomName` can
/// never produce (it contains neither a dot nor a colon), rather than being
/// squeezed into the namespace rule that exists for a different problem.
pub const USER_LABEL_PREFIX: &str = "user:";

/// Text the user typed to label something Clipped itself produced.
///
/// An input binding they named "my ultimate", a fingerprint match they typed
/// a name for: the host knows something happened and *that* a person named
/// it, but the name itself is not the host's to invent, and it is not a
/// plugin's namespace either — see [`USER_LABEL_PREFIX`] for why it is not
/// [`CustomName`].
///
/// # Syntax
///
/// ```text
/// user:<label>
/// ```
///
/// `<label>` may be anything a person can type: any case, spaces,
/// punctuation, any script — the one thing this crate rules out is a control
/// character, for the same reason a bookmark's label does
/// (`docs/bookmarks.md`): it is displayed, and a stray newline or escape
/// sequence in the middle of a UI label is a rendering bug waiting to be
/// filed. It is bounded by [`MAX_USER_LABEL_BYTES`], for the reason every
/// user-supplied string in this workspace is bounded
/// (`crates/plugins/src/manifest.rs`, `MAX_PROBLEM_BYTES`): it is stored and
/// shown, and an unbounded one is a cost paid forever.
///
/// # Who may produce one
///
/// Nothing in this crate checks that — `EventKind` has no producer boundary
/// of its own, unlike [`EventSource`](crate::EventSource) or
/// [`CustomName`], because it does not know who is calling it. **The
/// producer boundary belongs to whoever turns untrusted input into a
/// [`GameEvent`](crate::GameEvent).** For a plugin's report that is
/// `crates/plugins/src/report.rs::ReportedEvent::into_event`, and it must
/// refuse a [`UserLabelled`](EventKind::UserLabelled) kind exactly as it
/// already refuses [`Unrecognised`](EventKind::Unrecognised): a plugin
/// producing one would be a plugin claiming a person said something they
/// never typed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserLabel(String);

impl UserLabel {
    /// A label from the text a person typed, unprefixed.
    ///
    /// # Errors
    ///
    /// [`InvalidUserLabel`] describing which rule the text broke.
    pub fn new(label: &str) -> Result<Self, InvalidUserLabel> {
        if label.is_empty() {
            return Err(InvalidUserLabel::Empty);
        }
        if label.len() > MAX_USER_LABEL_BYTES {
            return Err(InvalidUserLabel::TooLong {
                label: label.to_owned(),
                bytes: label.len(),
            });
        }
        if label.chars().any(char::is_control) {
            return Err(InvalidUserLabel::ControlCharacter {
                label: label.to_owned(),
            });
        }
        Ok(Self(format!("{USER_LABEL_PREFIX}{label}")))
    }

    /// The text the person typed, without the wire prefix.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.0[USER_LABEL_PREFIX.len()..]
    }

    /// The wire string: [`USER_LABEL_PREFIX`] followed by the label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The wire string, consuming the wrapper.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for UserLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a user label was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidUserLabel {
    /// The person typed nothing.
    Empty,
    /// It is longer than [`MAX_USER_LABEL_BYTES`].
    TooLong {
        /// The text offered.
        label: String,
        /// How long it was.
        bytes: usize,
    },
    /// It contains a control character.
    ControlCharacter {
        /// The text offered.
        label: String,
    },
}

impl fmt::Display for InvalidUserLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a user label cannot be empty"),
            Self::TooLong { label, bytes } => write!(
                f,
                "the user label `{label}` is {bytes} bytes, over the \
                 {MAX_USER_LABEL_BYTES}-byte limit"
            ),
            Self::ControlCharacter { label } => write!(
                f,
                "the user label `{label}` contains a control character, and a label is \
                 displayed as written"
            ),
        }
    }
}

impl core::error::Error for InvalidUserLabel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_standard_kind_round_trips_through_its_wire_string() {
        // The exhaustive list is deliberately written out: a variant added to
        // `EventKind` should make somebody add it here and see it survive a
        // round trip, rather than discovering later that it reads back as
        // `Unrecognised`.
        let kinds = [
            EventKind::GameStarted,
            EventKind::GameEnded,
            EventKind::MatchStarted,
            EventKind::MatchEnded,
            EventKind::Kill,
            EventKind::Death,
            EventKind::Assist,
            EventKind::RoundStarted,
            EventKind::RoundEnded,
            EventKind::Win,
            EventKind::Loss,
            EventKind::Score,
            EventKind::Goal,
            EventKind::Achievement,
        ];
        for kind in kinds {
            let tag = kind.as_str().to_owned();
            assert!(
                !tag.contains('.'),
                "a standard tag must not be namespaced, or a plugin could emit it: {tag}"
            );
            assert_eq!(EventKind::from(tag.clone()), kind, "{tag} round trips");
            assert!(kind.is_recognised());
        }
    }

    #[test]
    fn the_wire_strings_are_the_ones_stored_events_already_use() {
        // Literal on purpose. These strings are in users' databases from the
        // first release onward, so changing one should fail here and make
        // somebody explain themselves (AGENTS.md section 43).
        assert_eq!(EventKind::Kill.as_str(), "kill");
        assert_eq!(EventKind::MatchStarted.as_str(), "match_started");
        assert_eq!(EventKind::RoundEnded.as_str(), "round_ended");
        assert_eq!(EventKind::Achievement.as_str(), "achievement");
    }

    #[test]
    fn a_namespaced_name_is_custom_even_when_this_build_has_never_met_it() {
        let kind = EventKind::from("acme-cs2.flashbang_blinded_five".to_owned());
        let EventKind::Custom(name) = &kind else {
            panic!("a namespaced name is a custom event, not {kind:?}");
        };
        assert_eq!(name.namespace(), "acme-cs2");
        assert!(kind.is_recognised());
    }

    #[test]
    fn an_unnamespaced_tag_this_build_does_not_know_is_kept_verbatim() {
        // A kind added to the vocabulary after this build shipped. It must
        // survive reading, because the event around it is still a mark on
        // somebody's timeline.
        let kind = EventKind::from("objective_taken".to_owned());
        assert_eq!(kind, EventKind::Unrecognised("objective_taken".to_owned()));
        assert!(!kind.is_recognised());
        assert_eq!(String::from(kind), "objective_taken");
    }

    #[test]
    fn a_plugin_cannot_name_a_custom_event_after_a_standard_one() {
        // The rule the vocabulary depends on. `kill` has no namespace, so it
        // cannot be constructed as a custom name at all.
        assert_eq!(
            CustomName::new("kill"),
            Err(InvalidCustomName::NotNamespaced {
                name: "kill".to_owned()
            })
        );
        // And a namespaced one that ends in `kill` is a different string, so it
        // cannot be mistaken for the standard event.
        let namespaced = CustomName::new("acme.kill").expect("a namespaced name is fine");
        assert_ne!(namespaced.as_str(), EventKind::Kill.as_str());
    }

    #[test]
    fn the_projects_own_namespace_is_not_available_to_plugins() {
        assert_eq!(
            CustomName::new("clipped.something"),
            Err(InvalidCustomName::ReservedNamespace {
                name: "clipped.something".to_owned()
            })
        );
    }

    #[test]
    fn a_name_that_is_not_lowercase_ascii_is_refused() {
        for name in ["Acme.Kill", "acme.flag captured", "acme.flag!", "acme.é"] {
            assert!(
                matches!(
                    CustomName::new(name),
                    Err(InvalidCustomName::MalformedSegment { .. })
                ),
                "`{name}` should be refused: two spellings of one event are two marks on a \
                 timeline the user believes is one"
            );
        }
    }

    #[test]
    fn a_segment_must_not_be_empty_or_start_with_a_digit() {
        for name in ["acme..flag", ".flag", "acme.", "9acme.flag", "acme.9flag"] {
            assert!(
                matches!(
                    CustomName::new(name),
                    Err(InvalidCustomName::MalformedSegment { .. })
                ),
                "`{name}` should be refused"
            );
        }
    }

    #[test]
    fn a_name_longer_than_the_limit_is_refused_before_it_reaches_a_database() {
        let long = format!("acme.{}", "a".repeat(MAX_IDENTIFIER_BYTES));
        assert!(matches!(
            CustomName::new(&long),
            Err(InvalidCustomName::TooLong { .. })
        ));
        let limit = format!("acme.{}", "a".repeat(MAX_IDENTIFIER_BYTES - 5));
        assert!(
            CustomName::new(&limit).is_ok(),
            "the limit itself is allowed"
        );
    }

    #[test]
    fn a_malformed_namespaced_name_is_kept_rather_than_repaired() {
        // It cannot be a `CustomName`, but the event it labels was still stored
        // by somebody, and losing it would be worse than not understanding it.
        let kind = EventKind::from("Acme.Kill".to_owned());
        assert_eq!(kind, EventKind::Unrecognised("Acme.Kill".to_owned()));
        assert_eq!(String::from(kind), "Acme.Kill");
    }

    #[test]
    fn a_kind_serialises_as_a_bare_string() {
        assert_eq!(
            serde_json::to_string(&EventKind::Kill).expect("it serialises"),
            r#""kill""#
        );
        let custom = EventKind::Custom(CustomName::new("acme.flag_captured").expect("valid"));
        assert_eq!(
            serde_json::to_string(&custom).expect("it serialises"),
            r#""acme.flag_captured""#
        );
        assert_eq!(
            serde_json::from_str::<EventKind>(r#""acme.flag_captured""#).expect("it reads back"),
            custom
        );
    }

    #[test]
    fn the_error_says_which_rule_was_broken() {
        let error = CustomName::new("kill").expect_err("not namespaced");
        let message = error.to_string();
        assert!(
            message.contains("<plugin>.<name>"),
            "the message should say what a valid name looks like: {message}"
        );
    }

    #[test]
    fn a_user_label_keeps_the_text_a_person_typed_intact() {
        // The acceptance criterion issue #345 states directly: the label
        // round-trips, not a normalised or slugged version of it. Mixed case,
        // punctuation, a space and a non-ASCII character all have to survive.
        let label = UserLabel::new("My Ultimate! (é)").expect("a well-formed label");
        assert_eq!(label.label(), "My Ultimate! (é)");
        assert_eq!(label.as_str(), "user:My Ultimate! (é)");

        let kind = EventKind::from(label.as_str().to_owned());
        let EventKind::UserLabelled(read_back) = &kind else {
            panic!("a `user:`-prefixed tag is a user-labelled kind, not {kind:?}");
        };
        assert_eq!(read_back.label(), "My Ultimate! (é)");
        assert!(kind.is_recognised());
        assert_eq!(String::from(kind), "user:My Ultimate! (é)");
    }

    #[test]
    fn a_user_label_is_not_a_custom_name() {
        // The type-level half of issue #345's first constraint: a user's
        // label and a plugin's namespaced word are different variants, and a
        // label cannot be mistaken for a namespace because it has neither a
        // dot nor the lowercase-ascii-segment syntax `CustomName` requires.
        let kind = EventKind::from("user:clutch".to_owned());
        assert!(matches!(kind, EventKind::UserLabelled(_)));
        assert!(
            CustomName::new("user:clutch").is_err(),
            "`user:clutch` has no dot, so it cannot be a namespaced custom name at all"
        );

        // And the reverse: a namespaced custom name is never read as a label,
        // even one whose namespace happens to be `user` with a dot rather
        // than a colon.
        let namespaced = EventKind::from("user.something".to_owned());
        assert!(matches!(namespaced, EventKind::Custom(_)));
    }

    #[test]
    fn an_empty_or_oversized_label_is_refused_at_the_point_it_is_typed() {
        assert_eq!(UserLabel::new(""), Err(InvalidUserLabel::Empty));

        let long = "a".repeat(MAX_USER_LABEL_BYTES + 1);
        assert!(matches!(
            UserLabel::new(&long),
            Err(InvalidUserLabel::TooLong { .. })
        ));
        let limit = "a".repeat(MAX_USER_LABEL_BYTES);
        assert!(UserLabel::new(&limit).is_ok(), "the limit itself is fine");

        let refusal = UserLabel::new("bad\u{7}label").expect_err("a control character");
        assert!(matches!(refusal, InvalidUserLabel::ControlCharacter { .. }));
        assert!(
            refusal.to_string().contains("control character"),
            "{refusal}"
        );
    }

    #[test]
    fn a_malformed_user_label_from_storage_is_kept_rather_than_refused() {
        // The read-path rule this crate already applies to a malformed
        // `CustomName` (`a_malformed_namespaced_name_is_kept_rather_than_repaired`),
        // applied to the other open kind: an event already in a user's
        // library is not deleted to enforce a rule it has already broken.
        let empty = EventKind::from("user:".to_owned());
        assert_eq!(empty, EventKind::Unrecognised("user:".to_owned()));

        let oversized_source = format!("user:{}", "a".repeat(MAX_USER_LABEL_BYTES + 1));
        let kind = EventKind::from(oversized_source.clone());
        assert_eq!(kind, EventKind::Unrecognised(oversized_source.clone()));
        assert_eq!(String::from(kind), oversized_source);
    }

    #[test]
    fn a_build_without_this_feature_reads_a_user_label_as_unrecognised() {
        // What an older build sees, simulated: `user:clutch` fails `CustomName`
        // (no dot) exactly as it would if this build had never heard of the
        // `user:` prefix, so it falls back to `Unrecognised` and the mark
        // survives on a timeline that build cannot yet label correctly.
        assert!(CustomName::new("user:clutch").is_err());
    }
}
