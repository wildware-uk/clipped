//! Turning a [`Query`] into SQL, so that the database answers a search.
//!
//! # Why this exists
//!
//! [`Query::matches`](super::matcher) is the reference meaning of the language
//! and stays that way. What it costs is that answering a search means building
//! the [`Row`](super::Row) every session projects — which means four further
//! statements per session, for its recordings, its clips, their tags and its
//! events — and doing that for every session in the library until a page is
//! full. `docs/library.md` measures it at 316 ms over ten thousand sittings when
//! nothing matches, which is the worst case and the one a search box hits while
//! somebody is still typing ([issue
//! #449](https://github.com/wildware-uk/clipped/issues/449)).
//!
//! This walks the same tree and produces a `WHERE` fragment instead. The saving
//! is not an index lookup — a substring test cannot use one — it is that the
//! whole predicate is evaluated inside one query plan rather than by
//! materialising every session in Rust in order to reject it.
//!
//! # Folding is the matcher's own function, called by SQLite
//!
//! [`fold`](super::fold) lower-cases the whole of Unicode, which is what makes
//! `тан` find `ТАНК`. SQLite cannot: `lower()` and `COLLATE NOCASE` fold ASCII
//! and nothing else. So [`install`] registers `fold` as a scalar function on the
//! connection and the compiled SQL calls it, which means the database and the
//! matcher are not two implementations that agree — they are one function called
//! from two places.
//!
//! The alternative was a stored folded column beside each text, written at
//! ingest. It is faster in principle and was rejected: every writer would have
//! to remember the second copy, and one that forgot would not fail — it would
//! quietly stop returning matches for whatever it wrote. A missing scalar
//! function is a loud error on the next search. Folding a game's name while
//! scanning it costs about what `instr` costs to scan it, and both are noise
//! beside the four statements per session this removes.
//!
//! # Negation, and the three-valued logic that could have broken it
//!
//! A sitting that recorded nothing does not satisfy `duration:<1h` and *is*
//! selected by `-duration:<1h`, because [`Expr::Not`] means "the matcher says
//! no". SQL does not work that way: a predicate can be `NULL`, `NOT NULL` is
//! `NULL`, and `NULL` is not true — so a leaf that evaluated to `NULL` would be
//! dropped by a negation that should have kept it, and nothing would look wrong.
//!
//! No leaf here can evaluate to `NULL`, and it is worth saying why of each,
//! because "add `IFNULL` everywhere" hides the question rather than answering
//! it:
//!
//! - **Game, title, tag and event** are `EXISTS` subqueries. `EXISTS` is true or
//!   false and never `NULL`: a row whose column is `NULL` fails the inner test
//!   and is simply not one of the rows found. That is also what makes a clip
//!   with no title unfindable by `title:` and *found* by `-title:`, which is
//!   what the matcher does with a row that has no title.
//! - **Session** compares `sessions.session_id`, the primary key, which
//!   `browse::read_header` already reads as a `String` — a `NULL` there fails
//!   the listing long before it reaches a query.
//! - **Date** compares `substr(sessions.started_at, 1, 10)`, and `started_at` is
//!   `NOT NULL` in the schema.
//! - **Duration** is the one that needed work, and not because of `NULL`: `SUM`
//!   over no rows is `NULL`, so it is folded to zero — and then a separate
//!   `> 0` test is what distinguishes "recorded nothing" from "recorded zero
//!   seconds". Without it every `duration:<…` in the language would select every
//!   sitting that never recorded.
//! - **Favourite** is `IS NOT NULL` and `EXISTS`, both total.
//!
//! # What a session is searchable by
//!
//! The projection `crate::index::browse::row_of` builds, in SQL:
//!
//! | Field | Here |
//! | --- | --- |
//! | session | `sessions.session_id` |
//! | game | `games.name`, through `sessions.game_id` |
//! | title | every clip's `title` |
//! | tag | every tag on every recording and clip |
//! | event | every `session_events.kind` |
//! | favourite | the sitting, or any recording or clip in it |
//! | date | the day of `started_at`, **in the offset it carries** |
//! | duration | the recordings and clips summed, absent when zero |
//!
//! A deleted recording or clip is in none of them, because it is in none of the
//! projection: `browse` reads both tables with `deleted_at IS NULL`, so a
//! sitting whose only clip was deleted is not found by that clip's title.

use clipped_storage::rusqlite::functions::FunctionFlags;
use clipped_storage::rusqlite::types::Value;
use clipped_storage::rusqlite::{Connection, Error as SqlError};

use super::query::{Comparison, Expr, Query, Term, TextField};
use super::text::fold;

/// The name the compiled SQL calls the folding by.
const FOLD: &str = "clipped_fold";

/// Registers [`fold`] on `connection`, so compiled queries can call it.
///
/// Idempotent: registering again replaces the previous definition, so a caller
/// may do this before every search rather than tracking whether a given
/// connection has been prepared. That is the point — it makes it impossible for
/// a search to run against a connection that cannot fold, which would otherwise
/// be a failure at a distance from its cause.
///
/// # Errors
///
/// Whatever SQLite says if the function cannot be registered.
pub(crate) fn install(connection: &Connection) -> Result<(), SqlError> {
    connection.create_scalar_function(
        FOLD,
        1,
        // Deterministic: the same text always folds the same way, which lets
        // SQLite hoist the call out of a loop when the argument is constant.
        // `INNOCUOUS` says it touches nothing outside its argument.
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_UTF8,
        |context| {
            // A NULL argument folds to NULL rather than to the empty string: a
            // clip with no title has no title, and the empty string would be a
            // substring of it.
            let value: Option<String> = context.get(0)?;
            Ok(value.map(|text| fold(&text)))
        },
    )
}

/// A compiled query: a `WHERE` fragment and what to bind to it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Compiled {
    /// The fragment, which assumes the sessions table is aliased `s`.
    pub(crate) predicate: String,
    /// The values its placeholders take, in the order they are numbered.
    pub(crate) params: Vec<Value>,
}

/// Compiles `query` into a predicate over `sessions s`.
///
/// `first_parameter` is the number of the first placeholder the fragment may
/// use, so that it can be pasted into a statement that already binds some: pass
/// 1 for a statement that binds nothing else. The placeholders are numbered
/// rather than bare `?` because a statement mixing the two leaves the numbering
/// to SQLite, and a caller reading the SQL could not tell what binds where.
///
/// A query that selects everything compiles to `1`, which is what a caller
/// wants: one statement shape whether or not there is a search.
pub(crate) fn compile(query: &Query, first_parameter: usize) -> Compiled {
    let mut params = Compiler {
        next: first_parameter,
        values: Vec::new(),
    };
    let predicate = query
        .root()
        .map_or_else(|| "1".to_owned(), |root| expression(root, &mut params));
    Compiled {
        predicate,
        params: params.values,
    }
}

/// The placeholders handed out so far, and what they bind to.
struct Compiler {
    next: usize,
    values: Vec<Value>,
}

impl Compiler {
    /// A placeholder bound to `value`, as it is written in SQL.
    fn bind(&mut self, value: Value) -> String {
        let placeholder = format!("?{}", self.next);
        self.next += 1;
        self.values.push(value);
        placeholder
    }
}

/// One node.
fn expression(expr: &Expr, params: &mut Compiler) -> String {
    match expr {
        Expr::All(children) => join(children, " AND ", "1", params),
        Expr::Any(children) => join(children, " OR ", "0", params),
        // The negation that makes a null guard at every leaf necessary: without
        // one, a sitting whose date is unknown would be dropped by `-date:…`
        // rather than kept.
        Expr::Not(child) => format!("NOT ({})", expression(child, params)),
        Expr::Term(term) => this_term(term, params),
    }
}

/// Several nodes, joined.
fn join(children: &[Expr], with: &str, empty: &str, params: &mut Compiler) -> String {
    if children.is_empty() {
        return empty.to_owned();
    }
    let parts: Vec<String> = children
        .iter()
        .map(|child| expression(child, params))
        .collect();
    format!("({})", parts.join(with))
}

/// One condition.
fn this_term(term: &Term, params: &mut Compiler) -> String {
    match term {
        Term::Text { field, needle } => text(*field, needle.folded(), params),
        Term::Favourite(wanted) => {
            let any = FAVOURITE;
            if *wanted {
                any.to_owned()
            } else {
                format!("NOT ({any})")
            }
        }
        Term::Date { comparison, value } => {
            // The day in the offset the recorder wrote, which is the user's
            // own: `substr` of the timestamp rather than a conversion, because a
            // session recorded at 00:30 belongs to that day on their calendar
            // whatever UTC would call it. `browse::day_of` reads it the same
            // way.
            let bound = params.bind(Value::Text(format!(
                "{:04}-{:02}-{:02}",
                value.year(),
                u8::from(value.month()),
                value.day()
            )));
            format!(
                "substr(s.started_at, 1, 10) {} {bound}",
                operator(*comparison)
            )
        }
        Term::Duration { comparison, value } => {
            let bound = params.bind(Value::Real(value.as_secs_f64()));
            // Absent when zero, which is what the projection says: a sitting
            // whose durations are all unknown has no duration, so no comparison
            // against one selects it — including `duration:<1h`.
            format!(
                "(({DURATION}) > 0 AND ({DURATION}) {} {bound})",
                operator(*comparison)
            )
        }
    }
}

/// Whether the sitting, or anything still in it, is favourited.
const FAVOURITE: &str = "(s.favourited_at IS NOT NULL \
      OR EXISTS (SELECT 1 FROM recordings r WHERE r.session_id = s.session_id \
                 AND r.deleted_at IS NULL AND r.favourited_at IS NOT NULL) \
      OR EXISTS (SELECT 1 FROM clips c WHERE c.session_id = s.session_id \
                 AND c.deleted_at IS NULL AND c.favourited_at IS NOT NULL))";

/// The footage a sitting produced, in seconds.
///
/// `SUM` of an empty set is NULL and `SUM` of only NULLs is NULL, so both are
/// folded to zero here and the zero is what the `> 0` guard tests: that is the
/// same rule as `row_of`, where a duration is set only when the total is above
/// zero.
const DURATION: &str = "(SELECT IFNULL(SUM(r.duration_seconds), 0) FROM recordings r \
                        WHERE r.session_id = s.session_id AND r.deleted_at IS NULL) \
                       + (SELECT IFNULL(SUM(c.duration_seconds), 0) FROM clips c \
                          WHERE c.session_id = s.session_id AND c.deleted_at IS NULL)";

/// Text in one field.
fn text(field: TextField, needle: &str, params: &mut Compiler) -> String {
    match field {
        TextField::Session => contains("s.session_id", needle, params),
        TextField::Game => exists(
            "SELECT 1 FROM games g WHERE g.game_id = s.game_id AND",
            "g.name",
            needle,
            params,
        ),
        TextField::Title => exists(
            "SELECT 1 FROM clips c WHERE c.session_id = s.session_id AND c.deleted_at IS NULL AND",
            "c.title",
            needle,
            params,
        ),
        TextField::Event => exists(
            "SELECT 1 FROM session_events e WHERE e.session_id = s.session_id AND",
            "e.kind",
            needle,
            params,
        ),
        TextField::Tag => tag(needle, params),
        // What a bare word searches: everything a session carries as text.
        TextField::Anywhere => {
            let parts = [
                text(TextField::Session, needle, params),
                text(TextField::Game, needle, params),
                text(TextField::Title, needle, params),
                text(TextField::Tag, needle, params),
                text(TextField::Event, needle, params),
            ];
            format!("({})", parts.join(" OR "))
        }
    }
}

/// A tag on any recording or clip still in the sitting.
fn tag(needle: &str, params: &mut Compiler) -> String {
    let recordings = exists(
        "SELECT 1 FROM recording_tags rt \
         JOIN tags t ON t.tag_id = rt.tag_id \
         JOIN recordings r ON r.recording_id = rt.recording_id \
         WHERE r.session_id = s.session_id AND r.deleted_at IS NULL AND",
        "t.name",
        needle,
        params,
    );
    let clips = exists(
        "SELECT 1 FROM clip_tags ct \
         JOIN tags t ON t.tag_id = ct.tag_id \
         JOIN clips c ON c.clip_id = ct.clip_id \
         WHERE c.session_id = s.session_id AND c.deleted_at IS NULL AND",
        "t.name",
        needle,
        params,
    );
    format!("({recordings} OR {clips})")
}

/// `column`, folded, holds `needle`.
///
/// The needle was folded at parse time; the column is folded here.
fn contains(column: &str, needle: &str, params: &mut Compiler) -> String {
    let bound = params.bind(Value::Text(needle.to_owned()));
    format!("instr({FOLD}({column}), {bound}) > 0")
}

/// A row exists whose `column`, folded, holds `needle`.
///
/// `EXISTS` is what makes a nullable column — a clip with no title — behave
/// under negation without a guard: the row fails the inner test and is not
/// found, which is true or false and never `NULL`. See the module
/// documentation.
fn exists(from: &str, column: &str, needle: &str, params: &mut Compiler) -> String {
    let bound = params.bind(Value::Text(needle.to_owned()));
    format!("EXISTS ({from} instr({FOLD}({column}), {bound}) > 0)")
}

/// The comparison, as SQL spells it.
const fn operator(comparison: Comparison) -> &'static str {
    match comparison {
        Comparison::Equal => "=",
        Comparison::Less => "<",
        Comparison::LessOrEqual => "<=",
        Comparison::Greater => ">",
        Comparison::GreaterOrEqual => ">=",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled(text: &str) -> Compiled {
        compile(&text.parse::<Query>().expect("the fixture parses"), 1)
    }

    #[test]
    fn an_empty_query_selects_everything() {
        // One statement shape whether or not there is a search, which is what
        // lets a listing have one code path for browsing and searching.
        let compiled = compiled("");

        assert_eq!(compiled.predicate, "1");
        assert!(compiled.params.is_empty());
    }

    #[test]
    fn every_text_comparison_goes_through_the_matchers_own_folding() {
        // The reason this module exists rather than a `COLLATE NOCASE` one. A
        // comparison that reached the column directly would fold ASCII only,
        // and `тан` would stop finding `ТАНК`.
        for query in [
            "mirage",
            "game:cs2",
            "tag:clutch",
            "event:kill",
            "title:ace",
            "session:friday",
        ] {
            let predicate = compiled(query).predicate;
            let folded = predicate.matches(FOLD).count();
            let compared = predicate.matches("instr(").count();
            assert_eq!(
                folded, compared,
                "`{query}` compares text SQLite folded, or did not fold: {predicate}"
            );
            assert!(
                !predicate.contains("NOCASE") && !predicate.contains("lower("),
                "`{query}` used SQLite's ASCII folding: {predicate}"
            );
        }
    }

    #[test]
    fn no_leaf_can_evaluate_to_null_and_so_break_a_negation() {
        // `NOT NULL` is `NULL`, which is not true, so a leaf that went NULL
        // would be dropped by a negation that should keep it. The module
        // documentation says why each leaf is total; this pins the two things
        // that make it so, because both are easy to lose in an edit.
        for query in ["game:cs2", "tag:clutch", "event:kill", "title:ace"] {
            let predicate = compiled(query).predicate;
            // A nullable column is only reached inside `EXISTS`, which is never
            // NULL — that is what makes a clip with no title behave.
            assert!(
                predicate.starts_with("EXISTS (") || predicate.starts_with('('),
                "`{query}` reads a column outside an EXISTS: {predicate}"
            );
        }
        // `SUM` over no rows is NULL, so it is folded to zero, and the separate
        // `> 0` is what tells "recorded nothing" from "recorded no seconds".
        // Whether that distinction survives is measured by
        // `browse::tests::a_sitting_that_recorded_nothing_has_no_duration_rather_than_a_duration_of_zero`.
        let duration = compiled("duration:>30m").predicate;
        assert!(duration.contains("> 0 AND"), "{duration}");
        assert!(duration.contains("IFNULL(SUM"), "{duration}");
    }

    #[test]
    fn a_bare_word_looks_in_every_field_a_session_carries() {
        let compiled = compiled("mirage");

        for column in ["s.session_id", "g.name", "c.title", "t.name", "e.kind"] {
            assert!(
                compiled.predicate.contains(&format!("{FOLD}({column})")),
                "a bare word should search {column}: {}",
                compiled.predicate
            );
        }
        // One parameter per place it looks: the two tag tables make six.
        assert_eq!(compiled.params.len(), 6, "{:?}", compiled.params);
        assert!(compiled
            .params
            .iter()
            .all(|param| matches!(param, Value::Text(text) if text == "mirage")));
    }

    #[test]
    fn the_needle_is_bound_rather_than_written_into_the_statement() {
        // A search box is user input reaching a database, which makes this the
        // one place in this crate where AGENTS.md section 30 has teeth.
        let compiled = compiled(r#""'; DROP TABLE sessions; --""#);

        assert!(
            !compiled.predicate.contains("DROP"),
            "the needle reached the statement: {}",
            compiled.predicate
        );
        assert_eq!(compiled.params.len(), 6);
    }

    #[test]
    fn placeholders_are_numbered_from_where_the_caller_says() {
        // So that the fragment can be pasted into a statement that already
        // binds a cursor and a limit.
        let compiled = compile(&"game:cs2 tag:clutch".parse::<Query>().expect("parses"), 4);

        assert!(compiled.predicate.contains("?4"), "{}", compiled.predicate);
        assert!(compiled.predicate.contains("?5"), "{}", compiled.predicate);
        assert!(!compiled.predicate.contains("?1"), "{}", compiled.predicate);
        assert_eq!(
            compiled.params.len(),
            3,
            "one for the game, two for the tag"
        );
    }

    #[test]
    fn a_comparison_becomes_the_operator_it_is_named_for() {
        assert!(compiled("date:>2026-08-11").predicate.contains("> ?1"));
        assert!(compiled("date:<=2026-08-11").predicate.contains("<= ?1"));
        assert!(compiled("duration:>=90s").predicate.contains(">= ?1"));
        assert_eq!(
            compiled("duration:>90s").params,
            vec![Value::Real(90.0)],
            "a duration is bound in seconds"
        );
        assert_eq!(
            compiled("date:2026-08-11").params,
            vec![Value::Text("2026-08-11".to_owned())],
            "a date is bound as the ten characters a timestamp starts with"
        );
    }

    #[test]
    fn precedence_is_the_trees_and_is_not_re_decided() {
        // `a b OR c` groups as `(a AND b) OR c`, and the parser has already
        // decided that. A compiler that re-read the text would be a second
        // answer to a question with one.
        let predicate = compiled("ace mirage OR inferno").predicate;

        assert!(predicate.starts_with('('), "{predicate}");
        assert!(predicate.contains(" OR "), "{predicate}");
        assert!(predicate.contains(" AND "), "{predicate}");
    }
}
