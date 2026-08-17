//! The rules, against constructed event streams.
//!
//! No game, no GPU, no recording and no file: every rule in this module is a
//! function of a list of events and three layers of settings, which is why the
//! ticket that wrote it could be finished without any of them (AGENTS.md
//! section 23, "configuration resolution" and "event transforms").

use core::time::Duration;

use clipped_events::{Confidence, EventKind, EventSource, EventTime, EventTiming, GameEvent};
use serde_json::{json, Map, Value};

use super::*;
use crate::config::{GameKey, Scope, SettingSource};

const SECOND: Duration = Duration::from_secs(1);

fn at(seconds: i64) -> EventTime {
    EventTime::from_media_nanos(seconds * 1_000_000_000)
}

fn source() -> EventSource {
    EventSource::plugin("acme-cs2").expect("a valid identifier")
}

/// An event of `kind` at `seconds`, timed exactly and reported with certainty.
fn event(kind: EventKind, seconds: i64) -> GameEvent {
    GameEvent::new(
        kind,
        EventTiming::new(at(seconds), Duration::ZERO),
        source(),
        Confidence::CERTAIN,
    )
}

fn kill(seconds: i64) -> GameEvent {
    event(EventKind::Kill, seconds)
}

fn confidence(value: f32) -> Confidence {
    Confidence::new(value).expect("a certainty this test writes down")
}

fn defaults() -> ResolvedHighlightRules {
    HighlightRules::resolve(Scope::Global, &HighlightRules::none(), None)
}

fn counter_strike() -> Scope {
    Scope::Game(GameKey::parse("counter-strike-2").expect("a valid game key"))
}

fn seconds_of(highlight: &Highlight<'_>) -> (i64, i64) {
    (
        highlight.start().as_media_nanos() / 1_000_000_000,
        highlight.end().as_media_nanos() / 1_000_000_000,
    )
}

/// The invariant the whole module exists for, asserted over every scenario
/// rather than case by case.
fn assert_no_two_overlap(highlights: &[Highlight<'_>]) {
    for (index, highlight) in highlights.iter().enumerate() {
        for other in &highlights[index + 1..] {
            assert!(
                !highlight.overlaps(other),
                "two highlights cover the same footage: {:?} and {:?}",
                seconds_of(highlight),
                seconds_of(other)
            );
        }
    }
    assert!(
        highlights
            .windows(2)
            .all(|pair| pair[0].start() <= pair[1].start()),
        "highlights should be in order"
    );
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

#[test]
fn a_kill_keeps_the_window_the_specification_names() {
    // SPEC.md section 7's worked example, which is the one number a user is
    // most likely to have an opinion about.
    let rule = defaults().rule_for(&EventKind::Kill);
    assert!(rule.enabled().get());
    assert_eq!(rule.lead().get(), SECOND * 15);
    assert_eq!(rule.trail().get(), SECOND * 10);
    assert_eq!(rule.lead().source(), SettingSource::Default);
}

#[test]
fn a_death_keeps_less_after_it_than_a_kill_does() {
    // Issue #75's other worked example: the interesting part of dying is what
    // led to it, so the lead survives and the trail is shorter.
    let rule = defaults().rule_for(&EventKind::Death);
    assert!(rule.enabled().get());
    assert_eq!(rule.lead().get(), SECOND * 10);
    assert_eq!(rule.trail().get(), SECOND * 5);
}

#[test]
fn the_moments_are_on_and_the_boundaries_are_off_without_any_configuration() {
    let rules = defaults();
    for kind in [
        EventKind::Kill,
        EventKind::Death,
        EventKind::Assist,
        EventKind::Win,
        EventKind::Score,
        EventKind::Goal,
        EventKind::Achievement,
    ] {
        assert!(
            rules.rule_for(&kind).enabled().get(),
            "{kind} should be clipped by default"
        );
    }
    for kind in [
        EventKind::GameStarted,
        EventKind::GameEnded,
        EventKind::MatchStarted,
        EventKind::MatchEnded,
        EventKind::RoundStarted,
        EventKind::RoundEnded,
        EventKind::Loss,
    ] {
        assert!(
            !rules.rule_for(&kind).enabled().get(),
            "{kind} is a boundary rather than a moment and should not be clipped by default"
        );
    }
}

#[test]
fn a_plugins_own_invention_is_not_clipped_until_somebody_says_so() {
    // The half of the defaults table that matters most: a plugin can invent a
    // name, and a plugin that could put clips in a user's library by inventing
    // one would be deciding what that library contains.
    let rules = defaults();
    let custom = EventKind::from("acme-cs2.flag_captured".to_owned());
    assert!(matches!(custom, EventKind::Custom(_)));
    assert!(!rules.rule_for(&custom).enabled().get());

    let newer = EventKind::from("objective_taken".to_owned());
    assert!(matches!(newer, EventKind::Unrecognised(_)));
    assert!(
        !rules.rule_for(&newer).enabled().get(),
        "a kind this build cannot name is not one it can judge"
    );

    // And each still carries a real window, so switching it on is one change.
    assert!(!rules.rule_for(&custom).lead().get().is_zero());
}

#[test]
fn the_users_own_label_is_not_clipped_until_they_say_so_either() {
    // A different reason from the test above, and worth keeping apart from it.
    // Nobody distrusts the name here — the user typed it. What a default `on`
    // would cost is the library: the signals this kind exists for (issue #345)
    // can fire many times a minute, which no game event does.
    let rules = defaults();
    let labelled = EventKind::UserLabelled(
        clipped_events::UserLabel::new("my ultimate").expect("prose is a label"),
    );

    assert!(
        !rules.rule_for(&labelled).enabled().get(),
        "a mark the user put on a timeline is not by itself a request for a clip"
    );
    assert!(
        !rules.rule_for(&labelled).lead().get().is_zero(),
        "and it must still carry a real window, so wanting the clips is one change"
    );
}

#[test]
fn every_kind_has_an_answer_including_ones_this_build_cannot_name() {
    // `rule_for` must total: the vocabulary is open, and a lookup that could
    // fail would make every consumer decide what to do about a kind it has
    // never met.
    let rules = defaults();
    for name in ["kill", "objective_taken", "acme.thing", "", "Not A Kind"] {
        let kind = EventKind::from(name.to_owned());
        let rule = rules.rule_for(&kind);
        assert!(rule.minimum_confidence().get().is_usable());
    }
}

// ---------------------------------------------------------------------------
// Resolution through the configuration layers
// ---------------------------------------------------------------------------

#[test]
fn a_game_that_says_nothing_inherits_the_global_rules() {
    let mut global = HighlightRules::none();
    global
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_lead(Some(SECOND * 20))),
        )
        .expect("twenty seconds is in range");

    let resolved = HighlightRules::resolve(counter_strike(), &global, None);
    let rule = resolved.rule_for(&EventKind::Kill);
    assert_eq!(rule.lead().get(), SECOND * 20);
    assert_eq!(rule.lead().source(), SettingSource::Global);
    assert!(!rule.lead().is_overridden());
    assert_eq!(
        rule.trail().get(),
        SECOND * 10,
        "what the global layer does not mention still comes from the defaults"
    );
    assert_eq!(rule.trail().source(), SettingSource::Default);
}

#[test]
fn a_game_overrides_one_field_and_inherits_the_rest_of_the_same_rule() {
    // The half of AGENTS.md section 30 that a per-rule inheritance would break:
    // a game that wants five more seconds after a kill says so, and keeps
    // following the global lead when the user changes it.
    let mut global = HighlightRules::none();
    global
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_lead(Some(SECOND * 20))),
        )
        .expect("in range");

    let mut game = HighlightRules::none();
    game.set_rule(
        EventKind::Kill,
        Some(HighlightRule::unset().with_trail(Some(SECOND * 15))),
    )
    .expect("in range");

    let resolved = HighlightRules::resolve(counter_strike(), &global, Some(&game));
    let rule = resolved.rule_for(&EventKind::Kill);
    assert_eq!(rule.lead().get(), SECOND * 20);
    assert_eq!(rule.lead().source(), SettingSource::Global);
    assert_eq!(rule.trail().get(), SECOND * 15);
    assert_eq!(rule.trail().source(), SettingSource::Game);
    assert!(rule.trail().is_overridden());
}

#[test]
fn a_game_can_switch_a_kind_off_that_the_global_rules_switch_on() {
    let mut game = HighlightRules::none();
    game.set_rule(
        EventKind::Death,
        Some(HighlightRule::unset().with_enabled(Some(false))),
    )
    .expect("a boolean is always in range");

    let resolved = HighlightRules::resolve(counter_strike(), &HighlightRules::none(), Some(&game));
    assert!(!resolved.rule_for(&EventKind::Death).enabled().get());
    assert!(
        resolved.rule_for(&EventKind::Kill).enabled().get(),
        "one kind's override says nothing about another's"
    );
    assert_eq!(
        resolved
            .decision_for(&event(EventKind::Death, 60))
            .skipped(),
        Some(SkipReason::Disabled)
    );
}

#[test]
fn the_global_page_never_shows_a_value_a_game_set() {
    // Otherwise the user edits the global rules and watches a game's number
    // change under their hands.
    let mut game = HighlightRules::none();
    game.set_rule(
        EventKind::Kill,
        Some(HighlightRule::unset().with_lead(Some(SECOND * 45))),
    )
    .expect("in range");

    let resolved = HighlightRules::resolve(Scope::Global, &HighlightRules::none(), Some(&game));
    let rule = resolved.rule_for(&EventKind::Kill);
    assert_eq!(rule.lead().get(), SECOND * 15);
    assert_eq!(rule.lead().source(), SettingSource::Default);
}

#[test]
fn setting_a_value_to_the_one_it_already_had_is_still_an_override() {
    // The distinction the whole layering exists for: this game keeps fifteen
    // seconds when the shipped default moves, and a game that never touched it
    // follows.
    let mut game = HighlightRules::none();
    game.set_rule(
        EventKind::Kill,
        Some(HighlightRule::unset().with_lead(Some(SECOND * 15))),
    )
    .expect("in range");

    let resolved = HighlightRules::resolve(counter_strike(), &HighlightRules::none(), Some(&game));
    let rule = resolved.rule_for(&EventKind::Kill);
    assert_eq!(rule.lead().get(), SECOND * 15);
    assert!(rule.lead().is_overridden());
}

#[test]
fn the_merge_settings_resolve_through_the_same_three_layers() {
    let mut global = HighlightRules::none();
    global.set_merge_gap(Some(SECOND * 20)).expect("in range");
    global
        .set_maximum_length(Some(SECOND * 300))
        .expect("in range");

    let mut game = HighlightRules::none();
    game.set_merge_gap(Some(SECOND * 2)).expect("in range");

    let resolved = HighlightRules::resolve(counter_strike(), &global, Some(&game));
    assert_eq!(resolved.merge_gap().get(), SECOND * 2);
    assert_eq!(resolved.merge_gap().source(), SettingSource::Game);
    assert_eq!(resolved.maximum_length().get(), SECOND * 300);
    assert_eq!(resolved.maximum_length().source(), SettingSource::Global);
}

#[test]
fn the_configured_kinds_are_both_layers_without_duplicates() {
    let mut global = HighlightRules::none();
    global
        .set_rule(EventKind::Kill, Some(HighlightRule::unset()))
        .expect("an empty rule is valid");
    let mut game = HighlightRules::none();
    game.set_rule(EventKind::Kill, Some(HighlightRule::unset()))
        .expect("valid");
    game.set_rule(EventKind::Death, Some(HighlightRule::unset()))
        .expect("valid");

    let resolved = HighlightRules::resolve(counter_strike(), &global, Some(&game));
    let kinds: Vec<&EventKind> = resolved.configured_kinds().collect();
    assert_eq!(kinds, vec![&EventKind::Kill, &EventKind::Death]);
}

// ---------------------------------------------------------------------------
// Selecting one event
// ---------------------------------------------------------------------------

#[test]
fn an_event_the_source_is_unsure_of_is_not_clipped() {
    let unsure = GameEvent::new(
        EventKind::Kill,
        EventTiming::new(at(60), Duration::ZERO),
        source(),
        confidence(0.4),
    );
    assert_eq!(
        defaults().decision_for(&unsure).skipped(),
        Some(SkipReason::Uncertain {
            confidence: confidence(0.4),
            minimum: default_minimum_confidence(),
        })
    );
    assert!(defaults().window_for(&unsure).is_none());
}

#[test]
fn a_rule_can_be_told_to_take_a_guess() {
    let mut global = HighlightRules::none();
    global
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_minimum_confidence(Some(confidence(0.25)))),
        )
        .expect("a quarter is in range");

    let rules = HighlightRules::resolve(Scope::Global, &global, None);
    let unsure = GameEvent::new(
        EventKind::Kill,
        EventTiming::new(at(60), Duration::ZERO),
        source(),
        confidence(0.4),
    );
    assert!(rules.decision_for(&unsure).is_included());
}

#[test]
fn a_stored_certainty_that_is_not_a_certainty_is_skipped_rather_than_guessed_at() {
    // `clipped_events` keeps a confidence already in a user's library verbatim
    // rather than destroying the event over it, so a rule has to meet one. The
    // honest answer is that the source's certainty is unknown; clamping it to
    // 1.0 would put a claim in the library that nobody made.
    let stored: Confidence = serde_json::from_str("1.5").expect("a stored certainty is readable");
    assert!(!stored.is_usable());
    let event = GameEvent::new(
        EventKind::Kill,
        EventTiming::new(at(60), Duration::ZERO),
        source(),
        stored,
    );
    assert_eq!(
        defaults().decision_for(&event).skipped(),
        Some(SkipReason::ConfidenceUnusable { confidence: stored })
    );
}

#[test]
fn a_window_is_widened_by_how_well_the_source_knows_the_moment() {
    // A source polled every two seconds knows the moment to within a second and
    // says so. A window built from the nominal time alone would cut a second
    // before the kill it is a clip of.
    let polled = GameEvent::new(
        EventKind::Kill,
        EventTiming::new(at(60), SECOND),
        source(),
        Confidence::CERTAIN,
    );
    let (start, end) = defaults().window_for(&polled).expect("a kill is clipped");
    assert_eq!(start, at(44));
    assert_eq!(end, at(71));

    let exact = kill(60);
    assert_eq!(defaults().window_for(&exact), Some((at(45), at(70))));
}

#[test]
fn a_late_report_does_not_move_the_window() {
    // The event carries the moment it describes, not the moment it was heard.
    // A clip built around the arrival time is a clip built around the wrong
    // half-minute.
    let reported_late = GameEvent::new(
        EventKind::Kill,
        EventTiming::new(at(60), Duration::ZERO).reported_late_by(SECOND * 30),
        source(),
        Confidence::CERTAIN,
    );
    assert_eq!(
        defaults().window_for(&reported_late),
        Some((at(45), at(70)))
    );
}

#[test]
fn a_rule_that_keeps_nothing_either_side_produces_no_clip() {
    // Reachable only across layers, because neither zero is refused on its own.
    let mut global = HighlightRules::none();
    global
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_lead(Some(Duration::ZERO))),
        )
        .expect("zero is in range");
    let mut game = HighlightRules::none();
    game.set_rule(
        EventKind::Kill,
        Some(HighlightRule::unset().with_trail(Some(Duration::ZERO))),
    )
    .expect("zero is in range");

    let rules = HighlightRules::resolve(counter_strike(), &global, Some(&game));
    assert_eq!(
        rules.decision_for(&kill(60)).skipped(),
        Some(SkipReason::EmptyWindow)
    );
    assert!(rules.highlights(&[kill(60)]).is_empty());
}

#[test]
fn a_window_before_the_start_of_the_recording_is_not_clamped_here() {
    // Clamping to what a file contains is `clipped_library::window_around`,
    // which knows that a saved replay does not start at zero. A rule that
    // clamped as well would be the second copy of that arithmetic.
    let (start, end) = defaults().window_for(&kill(4)).expect("a kill is clipped");
    assert_eq!(start, EventTime::from_media_nanos(-11_000_000_000));
    assert_eq!(end, at(14));
}

// ---------------------------------------------------------------------------
// Merging
// ---------------------------------------------------------------------------

#[test]
fn a_kill_streak_is_one_highlight_and_not_five() {
    // The scenario the ticket is about. Five kills two seconds apart is one
    // firefight; five clips of the same twenty seconds is a library nobody can
    // use, and nothing about it fails or warns.
    let events: Vec<GameEvent> = (0..5).map(|index| kill(60 + index * 2)).collect();
    let highlights = defaults().highlights(&events);

    assert_eq!(highlights.len(), 1);
    assert_eq!(seconds_of(&highlights[0]), (45, 78));
    assert_eq!(highlights[0].causes().len(), 5);
    assert_eq!(highlights[0].primary().timing().at(), at(60));
    assert_no_two_overlap(&highlights);
}

#[test]
fn two_fights_a_lull_apart_are_two_highlights() {
    let events = [kill(60), kill(160)];
    let highlights = defaults().highlights(&events);

    assert_eq!(highlights.len(), 2);
    assert_eq!(seconds_of(&highlights[0]), (45, 70));
    assert_eq!(seconds_of(&highlights[1]), (145, 170));
    assert_no_two_overlap(&highlights);
}

#[test]
fn a_gap_shorter_than_the_merge_gap_is_bridged() {
    // The windows do not touch — one ends at 70, the next starts at 72 — and
    // they are still one clip, because two seconds of nothing between two
    // fights is not a boundary a viewer would recognise.
    let events = [kill(60), kill(87)];
    let highlights = defaults().highlights(&events);

    assert_eq!(highlights.len(), 1);
    assert_eq!(seconds_of(&highlights[0]), (45, 97));
    assert_no_two_overlap(&highlights);
}

#[test]
fn a_gap_wider_than_the_merge_gap_is_not() {
    let events = [kill(60), kill(92)];
    let highlights = defaults().highlights(&events);

    assert_eq!(highlights.len(), 2, "seven seconds of gap is two clips");
    assert_no_two_overlap(&highlights);
}

#[test]
fn a_burst_of_different_kinds_is_still_one_highlight() {
    // Merging is across kinds, not within them: a kill, the death that answers
    // it and the assist in between are one moment.
    let events = [
        kill(60),
        event(EventKind::Assist, 63),
        event(EventKind::Death, 66),
    ];
    let highlights = defaults().highlights(&events);

    assert_eq!(highlights.len(), 1);
    assert_eq!(seconds_of(&highlights[0]), (45, 71));
    assert_eq!(highlights[0].causes().len(), 3);
    assert_no_two_overlap(&highlights);
}

#[test]
fn the_causes_of_a_highlight_are_in_the_order_things_happened() {
    // Not the order their windows opened. A death keeps ten seconds before it
    // and a kill fifteen, so the death here happens *first* at sixty seconds
    // and opens its window *second*, at fifty. Merging appends in window order,
    // so without the final sort the clip would claim the kill came first.
    let events = [event(EventKind::Death, 60), kill(61)];
    let highlights = defaults().highlights(&events);

    assert_eq!(highlights.len(), 1);
    let moments: Vec<(&EventKind, EventTime)> = highlights[0]
        .causes()
        .iter()
        .map(|event| (event.kind(), event.timing().at()))
        .collect();
    assert_eq!(
        moments,
        vec![(&EventKind::Death, at(60)), (&EventKind::Kill, at(61))]
    );
    assert_eq!(highlights[0].primary().kind(), &EventKind::Death);
}

#[test]
fn events_out_of_order_produce_the_same_highlights_as_events_in_order() {
    // Nothing places an event by its arrival: two integrations reporting the
    // same second arrive in whatever order their transports allow.
    let ordered = [kill(60), kill(63), kill(200)];
    let shuffled = [kill(200), kill(63), kill(60)];

    let from_ordered = defaults().highlights(&ordered);
    let from_shuffled = defaults().highlights(&shuffled);

    assert_eq!(from_ordered.len(), 2);
    assert_eq!(
        from_ordered
            .iter()
            .map(seconds_of)
            .collect::<Vec<(i64, i64)>>(),
        from_shuffled
            .iter()
            .map(seconds_of)
            .collect::<Vec<(i64, i64)>>()
    );
    assert_no_two_overlap(&from_shuffled);
}

#[test]
fn merging_across_a_gap_stops_at_the_maximum_length() {
    // Two windows that do not touch, close enough to bridge, whose union would
    // be longer than the ceiling allows: two clips, and neither is shortened.
    let mut global = HighlightRules::none();
    global
        .set_maximum_length(Some(SECOND * 40))
        .expect("in range");
    let rules = HighlightRules::resolve(Scope::Global, &global, None);

    let events = [kill(60), kill(87)];
    let highlights = rules.highlights(&events);

    assert_eq!(highlights.len(), 2);
    assert_eq!(seconds_of(&highlights[0]), (45, 70));
    assert_eq!(seconds_of(&highlights[1]), (72, 97));
    assert_no_two_overlap(&highlights);
}

#[test]
fn overlapping_windows_merge_even_past_the_maximum_length() {
    // The one place the two rules disagree, and the resolution is stated rather
    // than left to the order of the `if`s: two clips of the same footage is the
    // failure this module exists to prevent, and a user who set a short maximum
    // asked for shorter clips, not for duplicates of the same seconds.
    let mut global = HighlightRules::none();
    global
        .set_maximum_length(Some(SHORTEST_MAXIMUM_LENGTH))
        .expect("in range");
    let rules = HighlightRules::resolve(Scope::Global, &global, None);

    let events: Vec<GameEvent> = (0..6).map(|index| kill(60 + index * 20)).collect();
    let highlights = rules.highlights(&events);

    assert_eq!(highlights.len(), 1);
    assert!(highlights[0].duration() > SHORTEST_MAXIMUM_LENGTH);
    assert_eq!(highlights[0].causes().len(), 6);
    assert_no_two_overlap(&highlights);
}

#[test]
fn a_single_events_own_window_is_never_truncated() {
    let mut global = HighlightRules::none();
    global
        .set_maximum_length(Some(SHORTEST_MAXIMUM_LENGTH))
        .expect("in range");
    let rules = HighlightRules::resolve(Scope::Global, &global, None);

    let events = [kill(60)];
    let highlights = rules.highlights(&events);
    assert_eq!(seconds_of(&highlights[0]), (45, 70));
    assert!(
        highlights[0].duration() > SHORTEST_MAXIMUM_LENGTH,
        "the maximum bounds what merging adds, not what a rule asked for"
    );
}

#[test]
fn a_disabled_kind_neither_extends_a_highlight_nor_splits_one() {
    // A rule that is off is off: the round ending in the middle of a firefight
    // changes nothing about the clip of that firefight.
    let events = [
        kill(60),
        event(EventKind::RoundEnded, 63),
        kill(66),
        event(EventKind::MatchStarted, 300),
    ];
    let highlights = defaults().highlights(&events);

    assert_eq!(highlights.len(), 1);
    assert_eq!(seconds_of(&highlights[0]), (45, 76));
    assert_eq!(highlights[0].causes().len(), 2);
}

#[test]
fn nothing_selected_is_no_highlights_rather_than_an_empty_one() {
    let events = [
        event(EventKind::MatchStarted, 10),
        event(EventKind::Loss, 60),
    ];
    assert!(defaults().highlights(&events).is_empty());
    assert!(defaults().highlights(&[]).is_empty());
}

#[test]
fn every_selected_event_is_a_cause_of_exactly_one_highlight() {
    // Merging must not lose an event. A burst, a lull, a second burst, and one
    // kind that is off in the middle of it.
    let events: Vec<GameEvent> = [60, 62, 64, 200, 203, 400]
        .into_iter()
        .map(kill)
        .chain([event(EventKind::Loss, 250)])
        .collect();
    let highlights = defaults().highlights(&events);

    let causes: usize = highlights.iter().map(|one| one.causes().len()).sum();
    assert_eq!(causes, 6, "every kill, and only the kills");
    assert_eq!(highlights.len(), 3);
    assert_no_two_overlap(&highlights);
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[test]
fn a_lead_longer_than_the_limit_is_refused_and_names_what_it_accepts() {
    let mut rules = HighlightRules::none();
    let error = rules
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_lead(Some(MAXIMUM_LEAD + SECOND))),
        )
        .expect_err("over the limit");
    let message = error.to_string();
    assert!(
        message.contains("lead_seconds") && message.contains("300"),
        "the refusal should name the setting and the range: {message}"
    );
}

#[test]
fn a_refusal_names_the_event_kind_the_rule_is_for() {
    // "lead_seconds is 900 seconds" is not enough to act on when there are
    // fourteen rules in the file.
    let section = json!({"events": {"kill": {"lead_seconds": 900}}});
    let error = HighlightRules::read(section.as_object().unwrap())
        .expect_err("fifteen minutes is over the limit");
    assert_eq!(error.kind(), Some(&EventKind::Kill));
    assert!(
        error.to_string().starts_with("kill's lead_seconds"),
        "the refusal should name the kind: {error}"
    );
}

#[test]
fn a_fractional_duration_is_refused_because_the_file_cannot_hold_it() {
    let error = HighlightRules::none()
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_lead(Some(Duration::from_millis(1500)))),
        )
        .expect_err("not a whole number of seconds");
    assert!(
        error.to_string().contains("whole number of seconds"),
        "the refusal should say why: {error}"
    );
}

#[test]
fn a_threshold_no_event_could_meet_is_refused() {
    // A `Confidence` read back from storage may be outside 0..=1, and a
    // threshold of 1.5 would switch a kind off while appearing to be on.
    let stored: Confidence = serde_json::from_str("1.5").expect("readable");
    let error = HighlightRules::none()
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_minimum_confidence(Some(stored))),
        )
        .expect_err("not a usable certainty");
    assert!(error.to_string().contains("between 0 and 1"), "{error}");
}

#[test]
fn the_merge_settings_are_bounded() {
    let mut rules = HighlightRules::none();
    assert!(rules
        .set_merge_gap(Some(MAXIMUM_MERGE_GAP + SECOND))
        .is_err());
    assert!(rules.set_merge_gap(Some(Duration::ZERO)).is_ok());
    assert!(rules
        .set_maximum_length(Some(SHORTEST_MAXIMUM_LENGTH - SECOND))
        .is_err());
    assert!(rules
        .set_maximum_length(Some(LONGEST_MAXIMUM_LENGTH + SECOND))
        .is_err());
    assert!(rules.set_maximum_length(Some(SECOND * 90)).is_ok());
}

#[test]
fn a_refused_change_leaves_the_previous_rules_standing() {
    let mut rules = HighlightRules::none();
    rules.set_merge_gap(Some(SECOND * 9)).expect("in range");
    let _ = rules.set_merge_gap(Some(MAXIMUM_MERGE_GAP + SECOND));
    assert_eq!(rules.merge_gap(), Some(SECOND * 9));
}

// ---------------------------------------------------------------------------
// The settings-file section
// ---------------------------------------------------------------------------

fn read(section: &Value) -> HighlightRules {
    HighlightRules::read(section.as_object().expect("an object literal"))
        .expect("a section this test writes down")
}

#[test]
fn a_layer_survives_a_round_trip_through_the_file() {
    let mut rules = HighlightRules::none();
    rules.set_merge_gap(Some(SECOND * 8)).expect("in range");
    rules
        .set_maximum_length(Some(SECOND * 150))
        .expect("in range");
    rules
        .set_rule(
            EventKind::Kill,
            Some(
                HighlightRule::unset()
                    .with_lead(Some(SECOND * 20))
                    .with_minimum_confidence(Some(confidence(0.75))),
            ),
        )
        .expect("in range");
    rules
        .set_rule(
            EventKind::Death,
            Some(HighlightRule::unset().with_enabled(Some(false))),
        )
        .expect("valid");

    let written = rules.write();
    assert_eq!(
        HighlightRules::read(&written).expect("what this build wrote, it reads"),
        rules
    );
}

#[test]
fn a_layer_says_only_what_it_changes() {
    let mut rules = HighlightRules::none();
    rules
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_lead(Some(SECOND * 20))),
        )
        .expect("in range");

    let written = Value::Object(rules.write());
    assert_eq!(written, json!({"events": {"kill": {"lead_seconds": 20}}}));
}

#[test]
fn an_absent_section_is_every_rule_inherited() {
    // Which is the whole migration path: no build has written this section, so
    // the only older shape is its absence, and absence is what an unconfigured
    // user already gets.
    let empty = read(&json!({}));
    assert!(empty.is_empty());
    let resolved = HighlightRules::resolve(Scope::Global, &empty, None);
    assert_eq!(
        resolved.rule_for(&EventKind::Kill).lead().get(),
        SECOND * 15
    );
    assert_eq!(resolved.merge_gap().get(), DEFAULT_MERGE_GAP);
    assert_eq!(resolved.maximum_length().get(), DEFAULT_MAXIMUM_LENGTH);
}

#[test]
fn a_key_a_newer_build_wrote_is_kept_and_written_back() {
    // Losing a user's settings because their other machine is a version ahead
    // is the destruction AGENTS.md section 56 forbids.
    let section = json!({
        "merge_gap_seconds": 8,
        "minimum_streak": 3,
        "events": {"kill": {"lead_seconds": 20, "weapon": "ak47"}}
    });
    let rules = read(&section);

    assert_eq!(
        rules.unrecognised_keys().collect::<Vec<&str>>(),
        vec!["minimum_streak"]
    );
    let written = Value::Object(rules.write());
    assert_eq!(written["minimum_streak"], json!(3));
    assert_eq!(written["events"]["kill"]["weapon"], json!("ak47"));
    assert_eq!(written["events"]["kill"]["lead_seconds"], json!(20));
}

#[test]
fn a_rule_for_a_kind_this_build_cannot_name_still_applies_to_it() {
    // The reason the map is keyed by `EventKind` and not by a closed
    // enumeration of the kinds this build knows: a newer Clipped's
    // `objective_taken` rule keeps working, because the match is on the same
    // string the event carries.
    let rules = read(&json!({
        "events": {"objective_taken": {"enabled": true, "lead_seconds": 8, "trail_seconds": 4}}
    }));
    let resolved = HighlightRules::resolve(Scope::Global, &rules, None);

    let unknown = EventKind::from("objective_taken".to_owned());
    assert!(matches!(unknown, EventKind::Unrecognised(_)));
    let event = event(unknown, 60);
    assert_eq!(
        resolved.window_for(&event),
        Some((at(52), at(64))),
        "a rule this build cannot name is still a rule it can apply"
    );

    // And it goes back out under the name it came in with.
    let written = Value::Object(rules.write());
    assert_eq!(
        written["events"]["objective_taken"]["lead_seconds"],
        json!(8)
    );
}

#[test]
fn a_reset_field_reads_as_unset_rather_than_as_a_value() {
    // `null` is what a settings screen writes when the user presses Reset.
    let rules = read(&json!({
        "merge_gap_seconds": null,
        "events": {"kill": {"lead_seconds": null, "trail_seconds": 12}}
    }));
    assert_eq!(rules.merge_gap(), None);
    let rule = rules.rule(&EventKind::Kill).expect("the kind is present");
    assert_eq!(rule.lead(), None);
    assert_eq!(rule.trail(), Some(SECOND * 12));
}

#[test]
fn a_value_of_the_wrong_type_is_refused_by_name() {
    let section = json!({"events": {"kill": {"enabled": "yes"}}});
    let error = HighlightRules::read(section.as_object().unwrap()).expect_err("not a boolean");
    let message = error.to_string();
    assert!(
        message.contains("kill's enabled") && message.contains("true or false"),
        "the refusal should name the setting and the type: {message}"
    );
}

#[test]
fn a_section_that_is_not_the_shape_of_a_rule_set_is_refused() {
    let section = json!({"events": 5});
    let error = HighlightRules::read(section.as_object().unwrap()).expect_err("not an object");
    assert!(
        error.to_string().contains("\"events\" is an object"),
        "{error}"
    );

    let section = json!({"events": {"kill": 5}});
    let error = HighlightRules::read(section.as_object().unwrap()).expect_err("not an object");
    assert!(
        error.to_string().contains("kill's rule is an object"),
        "{error}"
    );
}

#[test]
fn a_certainty_outside_the_range_is_refused_by_the_file_reader() {
    let section = json!({"events": {"kill": {"minimum_confidence": 1.5}}});
    let error = HighlightRules::read(section.as_object().unwrap()).expect_err("not a certainty");
    assert_eq!(error.setting(), Some(RuleSetting::MinimumConfidence));
    assert_eq!(error.kind(), Some(&EventKind::Kill));
}

#[test]
fn every_setting_round_trips_through_its_file_key() {
    for setting in RuleSetting::ALL {
        assert_eq!(RuleSetting::from_name(setting.name()), Some(setting));
    }
    let mut names: Vec<&str> = RuleSetting::ALL.iter().map(|one| one.name()).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count, "two settings share a key: {names:?}");
}

#[test]
fn the_shipped_table_and_the_document_agree_about_what_a_default_is() {
    // A rule that repeats a shipped value is an override, and must survive the
    // file as one — otherwise a user who pinned a kill to fifteen seconds would
    // silently start following a later change to the default.
    let mut rules = HighlightRules::none();
    let shipped = shipped_rule(&EventKind::Kill);
    rules
        .set_rule(
            EventKind::Kill,
            Some(HighlightRule::unset().with_lead(Some(shipped.lead))),
        )
        .expect("in range");

    let read_back = HighlightRules::read(&rules.write()).expect("it reads");
    let resolved = HighlightRules::resolve(Scope::Global, &read_back, None);
    assert!(resolved.rule_for(&EventKind::Kill).lead().is_overridden());
}

#[test]
fn a_map_of_rules_is_written_in_a_stable_order() {
    // A settings file that reorders itself on every save is a file nobody can
    // diff, and a `HashMap` here would do exactly that.
    let mut rules = HighlightRules::none();
    for name in ["win", "kill", "death"] {
        rules
            .set_rule(
                EventKind::from(name.to_owned()),
                Some(HighlightRule::unset().with_enabled(Some(true))),
            )
            .expect("valid");
    }
    let written: Map<String, Value> = rules.write();
    let events = written["events"].as_object().expect("an object");
    assert_eq!(
        events.keys().map(String::as_str).collect::<Vec<&str>>(),
        vec!["death", "kill", "win"]
    );
}
