//! Generating clips, against constructed event streams and stand-in
//! recordings.
//!
//! No game, no GPU and no file: a session's recordings are described here by
//! the span of the timeline each one covers, which is exactly what generation
//! is given in the product (`clipped_library::events::RecordedSegment`). Every
//! assertion below is therefore about the arithmetic and the policy — which
//! file a clip comes from, what it is called, and whether it should exist at
//! all — which is what makes it a test that runs on any machine (AGENTS.md
//! section 23).

use core::time::Duration;

use clipped_edit::{RecordingId, SourceSpan, SourceTime};
use clipped_events::{
    Confidence, CustomName, EventKind, EventSource, EventTime, EventTiming, GameEvent, RecordedSpan,
};
use clipped_library::events::{NotRecorded, RecordedSegment, SessionRecordings};
use clipped_library::virtual_clip::{
    ClipOrigin, ClipState, HighlightCause, SourceAvailability, VirtualClip,
};

use super::*;
use crate::config::{GameKey, Scope};

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

fn defaults() -> ResolvedHighlightRules {
    HighlightRules::resolve(Scope::Global, &HighlightRules::none(), None)
}

/// Rules that are the shipped ones with `change` applied to the global layer.
fn rules_with(change: impl FnOnce(&mut HighlightRules)) -> ResolvedHighlightRules {
    let mut global = HighlightRules::none();
    change(&mut global);
    HighlightRules::resolve(Scope::Global, &global, None)
}

fn counter_strike() -> Scope {
    Scope::Game(GameKey::parse("counter-strike-2").expect("a valid game key"))
}

/// One file of a session, covering `from..to` of the session's timeline.
fn segment(name: &str, from: i64, to: i64) -> RecordedSegment {
    RecordedSegment::new(
        RecordingId::new(name),
        RecordedSpan::new(at(from), at(to)).expect("a span that ends after it starts"),
    )
}

/// A session of one recording written from its start, `seconds` long.
fn one_recording(seconds: i64) -> SessionRecordings {
    SessionRecordings::of([segment("rec-1", 0, seconds)])
}

/// Which recording a clip plays, and which seconds of it.
fn cut_of(clip: &VirtualClip) -> (String, u64, u64) {
    let document = clip.edit();
    let segment = document
        .segments
        .first()
        .expect("a generated clip has one segment");
    let source = document
        .source(segment.source)
        .expect("that segment's source is declared");
    (
        source.recording.as_str().to_owned(),
        segment.span.start().as_nanos() / 1_000_000_000,
        segment.span.end().as_nanos() / 1_000_000_000,
    )
}

fn titles(generated: &GeneratedHighlights) -> Vec<&str> {
    generated.clips().iter().map(VirtualClip::title).collect()
}

fn reasons(generated: &GeneratedHighlights) -> Vec<&NotGenerated> {
    generated
        .withheld()
        .iter()
        .map(WithheldHighlight::reason)
        .collect()
}

/// The invariant a library depends on, asserted over whole runs rather than
/// case by case: no two generated clips of one recording cover the same
/// footage.
fn assert_no_two_cover_the_same_seconds(clips: &[VirtualClip]) {
    let cuts: Vec<(String, u64, u64)> = clips.iter().map(cut_of).collect();
    for (index, (recording, start, end)) in cuts.iter().enumerate() {
        for (other, from, to) in &cuts[index + 1..] {
            assert!(
                recording != other || start >= to || from >= end,
                "two clips of {recording} cover the same seconds: {start}..{end} and {from}..{to}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// What a clip is made of
// ---------------------------------------------------------------------------

#[test]
fn a_kill_becomes_a_clip_of_the_recording_it_happened_in() {
    let recordings = one_recording(1_800);
    let events = [kill(600)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert_eq!(generated.clips().len(), 1);
    assert!(generated.withheld().is_empty());
    let clip = &generated.clips()[0];

    // Fifteen seconds before the kill and ten after (SPEC.md section 7), as a
    // range of the file rather than of the session.
    assert_eq!(cut_of(clip), ("rec-1".to_owned(), 585, 610));
    assert_eq!(clip.duration(), Some(SECOND * 25));
    assert_eq!(
        clip.title(),
        "Kill at 10:00",
        "the title times the kill, not the fifteen seconds of lead the clip opens with"
    );
    assert_eq!(clip.tags(), ["kill"]);

    // It is a clip, and it is traceable to the event that caused it.
    clip.edit().validate().expect("a generated clip is valid");
    assert_eq!(
        clip.state(|_| SourceAvailability::Present),
        ClipState::Playable
    );
    let cause = clip
        .origin()
        .cause()
        .expect("a generated clip says what caused it");
    assert_eq!(cause.kind(), &EventKind::Kill);
    assert_eq!(cause.at(), at(600));
    assert_eq!(cause.source().as_str(), "acme-cs2");
}

#[test]
fn a_firefight_is_one_clip_named_after_everything_in_it() {
    // The failure the merge exists to prevent, asserted where a user would see
    // it: three kills in six seconds are one clip in the library, not three of
    // nearly the same twenty seconds.
    let recordings = one_recording(300);
    let events = [kill(60), kill(63), kill(66)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert_eq!(generated.clips().len(), 1);
    let clip = &generated.clips()[0];
    assert_eq!(cut_of(clip), ("rec-1".to_owned(), 45, 76));
    assert_eq!(clip.title(), "Kill ×3 at 1:00");
    assert_eq!(
        clip.tags(),
        ["kill"],
        "one tag for three events of the same kind"
    );
    assert_eq!(
        clip.origin().cause().map(HighlightCause::at),
        Some(at(60)),
        "the clip is named after the event that opened the moment"
    );
}

#[test]
fn a_clip_of_the_second_recording_is_measured_from_that_file() {
    // A window destroyed and recreated gives one session two files, and the
    // second one's zero is not the session's. A subtraction at the call site
    // would put this clip 250 seconds into a file that is 200 seconds long.
    let recordings = SessionRecordings::of([segment("rec-1", 0, 120), segment("rec-2", 200, 400)]);
    let events = [kill(30), kill(250)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert_eq!(
        generated
            .clips()
            .iter()
            .map(cut_of)
            .collect::<Vec<(String, u64, u64)>>(),
        vec![("rec-1".to_owned(), 15, 40), ("rec-2".to_owned(), 35, 60),]
    );
}

#[test]
fn a_clip_of_a_saved_replay_starts_where_the_file_does() {
    // A replay clip's file begins at the keyframe the buffer started with,
    // twenty minutes down the session's timeline. Fifteen seconds of lead
    // before a kill ten seconds in reaches past the front of the file, so the
    // clip opens at the beginning rather than being refused.
    let recordings = SessionRecordings::of([segment("replay", 1_200, 1_230)]);
    let events = [kill(1_210)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert_eq!(generated.clips().len(), 1);
    assert_eq!(cut_of(&generated.clips()[0]), ("replay".to_owned(), 0, 20));
    assert_eq!(
        generated.clips()[0].title(),
        "Kill at 0:10",
        "ten seconds into the file, rather than 20:10 into the session it was saved from"
    );
}

#[test]
fn a_window_that_reaches_past_the_end_of_the_file_stops_there() {
    let recordings = one_recording(300);
    let events = [kill(295)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert_eq!(
        cut_of(&generated.clips()[0]),
        ("rec-1".to_owned(), 280, 300)
    );
}

#[test]
fn a_clip_is_named_and_tagged_only_for_what_its_own_seconds_contain() {
    // A firefight that ran past the end of the file. The window is clamped to
    // the recording it started in rather than split across two ([#88]), so the
    // death is not in the clip — and a clip titled "Kill, death" and tagged
    // `death` would be claiming footage it does not contain (AGENTS.md section
    // 27). The event is reported instead of being quietly folded in.
    let recordings = one_recording(120);
    let events = [kill(105), event(EventKind::Death, 125)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert_eq!(generated.clips().len(), 1);
    let clip = &generated.clips()[0];
    assert_eq!(cut_of(clip), ("rec-1".to_owned(), 90, 120));
    assert_eq!(clip.title(), "Kill at 1:45");
    assert_eq!(
        clip.tags(),
        ["kill"],
        "the death is outside the clip, so it is not one of its tags"
    );

    assert_eq!(reasons(&generated), vec![&NotGenerated::OutsideTheClip]);
    assert_eq!(
        generated.withheld()[0].cause().at(),
        at(125),
        "the event with no clip is named, so a caller can say which one it was"
    );
    assert!(!generated.withheld()[0].reason().to_string().is_empty());
}

#[test]
fn an_event_no_clip_contained_is_clipped_when_its_own_recording_ends() {
    // What follows from withholding it rather than claiming it: the death got no
    // clip, so it is still owed one, and the run after the recording that holds
    // it has been finished offers it rather than an `AlreadyGenerated`.
    //
    // What is generated across runs is decided by the causes an existing clip
    // *carries*, and a clip carries the one it is named after — so the same
    // thing happens whether or not generation also marked the death as clipped
    // inside the earlier run. This test is the behaviour, not a proof of that
    // bookkeeping; nothing observable distinguishes it today.
    let first = SessionRecordings::of([segment("rec-1", 0, 120)]);
    let events = [kill(105), event(EventKind::Death, 125)];

    let during = HighlightGeneration::new(&defaults(), &first).generate(&events);
    assert_eq!(cut_of(&during.clips()[0]), ("rec-1".to_owned(), 90, 120));

    // The user switches kills off, so the death is a moment of its own rather
    // than part of the kill's — which is the case the cause check can answer.
    let deaths_only = rules_with(|global| {
        global
            .set_rule(
                EventKind::Kill,
                Some(HighlightRule::unset().with_enabled(Some(false))),
            )
            .expect("a kind can be switched off");
    });
    let both = SessionRecordings::of([segment("rec-1", 0, 120), segment("rec-2", 121, 300)]);
    let after = HighlightGeneration::new(&deaths_only, &both)
        .with_existing_clips(during.clips())
        .generate(&events);

    assert_eq!(after.clips().len(), 1, "{:?}", reasons(&after));
    assert_eq!(cut_of(&after.clips()[0]), ("rec-2".to_owned(), 0, 9));
    assert_eq!(after.clips()[0].title(), "Death at 0:04");
    assert!(after.withheld().is_empty());
}

#[test]
fn the_moment_a_recording_ends_on_has_nothing_to_cut() {
    // Reachable only when a rule keeps nothing before the event: the window
    // clamped to the file collapses to a point, and a clip of no length plays
    // nothing. Refusing it is the alternative to a row in the library that
    // opens on an empty player.
    let rules = rules_with(|global| {
        global
            .set_rule(
                EventKind::Kill,
                Some(HighlightRule::unset().with_lead(Some(Duration::ZERO))),
            )
            .expect("a lead of nothing is allowed");
    });
    let recordings = one_recording(300);
    let events = [kill(300)];

    let generated = HighlightGeneration::new(&rules, &recordings).generate(&events);

    assert!(generated.clips().is_empty());
    assert_eq!(reasons(&generated), vec![&NotGenerated::NothingToCut]);
}

// ---------------------------------------------------------------------------
// Which source a clip comes from
// ---------------------------------------------------------------------------

#[test]
fn a_moment_no_file_covers_produces_no_clip_and_says_which_case_it_was() {
    // Four different things to tell somebody, and only one of them is the
    // ordinary "it happened outside the recording" (AGENTS.md section 45).
    let recordings = SessionRecordings::of([segment("rec-1", 0, 120), segment("rec-2", 200, 400)]);
    let events = [kill(-30), kill(160), kill(500)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert!(generated.clips().is_empty());
    assert_eq!(
        reasons(&generated),
        vec![
            &NotGenerated::NotRecorded(NotRecorded::BeforeTheFirstRecording),
            &NotGenerated::NotRecorded(NotRecorded::BetweenRecordings),
            &NotGenerated::NotRecorded(NotRecorded::AfterTheLastRecording),
        ]
    );
    assert_eq!(
        generated.withheld()[0].cause().at(),
        at(-30),
        "the event is still named, so a caller can say which kill it was"
    );
    for withheld in generated.withheld() {
        assert!(
            !withheld.reason().to_string().is_empty(),
            "every reason has to be sayable to a person"
        );
    }
}

#[test]
fn a_replay_buffer_only_session_generates_nothing_and_writes_nothing() {
    // The question the ticket asks: the material is in the buffer rather than
    // on disk. Keeping it would mean writing a file at that moment, which is
    // Highlights Only (#77) and not generation — so nothing is produced, and
    // the reason says the session recorded nothing rather than pretending the
    // moment was outside a file that does not exist.
    let recordings = SessionRecordings::none_recorded();
    let events = [kill(600), kill(900)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert!(generated.clips().is_empty());
    assert_eq!(
        reasons(&generated),
        vec![
            &NotGenerated::NotRecorded(NotRecorded::NothingRecorded),
            &NotGenerated::NotRecorded(NotRecorded::NothingRecorded),
        ]
    );
}

#[test]
fn a_recording_still_being_written_is_generated_from_when_it_ends() {
    // Generation is over the files a session has *finished*: a `RecordedSpan`
    // is only known once a file has been closed. So a session that is halfway
    // through its second recording generates the first one's highlights now and
    // the second one's when that file ends — which is what keeps this off the
    // capture thread rather than a comment saying so.
    let events = [kill(60), kill(250)];
    let first = SessionRecordings::of([segment("rec-1", 0, 120)]);

    let during = HighlightGeneration::new(&defaults(), &first).generate(&events);
    assert_eq!(during.clips().len(), 1);
    assert_eq!(cut_of(&during.clips()[0]), ("rec-1".to_owned(), 45, 70));
    assert_eq!(
        reasons(&during),
        vec![&NotGenerated::NotRecorded(
            NotRecorded::AfterTheLastRecording
        )]
    );

    // The second recording ends, and the run that follows finds it — without
    // offering the first one's clip again.
    let both = SessionRecordings::of([segment("rec-1", 0, 120), segment("rec-2", 200, 400)]);
    let after = HighlightGeneration::new(&defaults(), &both)
        .with_existing_clips(during.clips())
        .generate(&events);

    assert_eq!(after.clips().len(), 1);
    assert_eq!(cut_of(&after.clips()[0]), ("rec-2".to_owned(), 35, 60));
    assert_eq!(reasons(&after), vec![&NotGenerated::AlreadyGenerated]);
}

// ---------------------------------------------------------------------------
// Running it twice
// ---------------------------------------------------------------------------

#[test]
fn running_generation_again_over_the_same_session_produces_nothing() {
    // The acceptance criterion, and the failure it guards against is a library
    // that doubles every time something re-runs.
    let recordings = one_recording(1_800);
    let events = [kill(60), kill(63), kill(600), event(EventKind::Death, 900)];
    let rules = defaults();
    let generation = HighlightGeneration::new(&rules, &recordings);

    let first = generation.generate(&events);
    assert_eq!(first.clips().len(), 3);
    assert_no_two_cover_the_same_seconds(first.clips());

    let second = generation
        .with_existing_clips(first.clips())
        .generate(&events);

    assert!(
        second.clips().is_empty(),
        "a second run produced {:?}",
        titles(&second)
    );
    assert_eq!(second.withheld().len(), 3);
    assert_eq!(
        second.withheld_for(&NotGenerated::AlreadyGenerated),
        3,
        "each moment already has a clip of its own"
    );

    // And a third, over the union, which is what a caller that stored both runs
    // would hand back.
    let stored: Vec<VirtualClip> = first
        .clips()
        .iter()
        .chain(second.clips())
        .cloned()
        .collect();
    let third = generation.with_existing_clips(&stored).generate(&events);
    assert!(third.clips().is_empty());
}

#[test]
fn a_late_event_that_joins_a_moment_already_clipped_does_not_clip_it_twice() {
    // Events arrive late (`clipped_events::EventTiming`), so the second run of
    // a session can see one the first did not. A kill three seconds after one
    // that already has a clip is part of the same firefight, and a second clip
    // of nearly the same twenty seconds is what the merge exists to prevent —
    // across runs as well as within one.
    let recordings = one_recording(300);
    let rules = defaults();
    let generation = HighlightGeneration::new(&rules, &recordings);

    let first = generation.generate([&kill(60)]);
    assert_eq!(first.clips().len(), 1);

    let all = [kill(60), kill(63)];
    let second = generation.with_existing_clips(first.clips()).generate(&all);

    assert!(second.clips().is_empty());
    assert_eq!(reasons(&second), vec![&NotGenerated::AlreadyGenerated]);
}

#[test]
fn regenerating_after_a_rule_change_never_offers_seconds_a_clip_already_covers() {
    // The backstop, for the case the cause check cannot answer: the user
    // switched kills off and deaths on, so the moment that used to be a kill's
    // is now a death's and shares none of its events. Its window still covers
    // seconds the clip in the library covers, and a second clip of them would
    // be the duplicate wearing a different kind.
    let recordings = one_recording(300);
    let events = [kill(60), event(EventKind::Death, 70)];

    let first = HighlightGeneration::new(&defaults(), &recordings).generate([&events[0]]);
    assert_eq!(cut_of(&first.clips()[0]), ("rec-1".to_owned(), 45, 70));

    let without_kills = rules_with(|global| {
        global
            .set_rule(
                EventKind::Kill,
                Some(HighlightRule::unset().with_enabled(Some(false))),
            )
            .expect("a kind can be switched off");
    });
    let second = HighlightGeneration::new(&without_kills, &recordings)
        .with_existing_clips(first.clips())
        .generate(&events);

    assert!(second.clips().is_empty());
    assert_eq!(
        reasons(&second),
        vec![&NotGenerated::OverlapsAnExistingClip]
    );
}

#[test]
fn a_clip_the_user_made_by_hand_neither_suppresses_a_highlight_nor_is_touched() {
    // "What did Clipped generate" is a different question from "what did I
    // save", and the library filters on the origin for exactly this. A user who
    // clipped the same firefight themselves still gets the generated one.
    let recordings = one_recording(300);
    let mine = VirtualClip::of_range(
        "My ace",
        RecordingId::new("rec-1"),
        SourceSpan::new(
            SourceTime::from_nanos(40 * 1_000_000_000),
            SourceTime::from_nanos(80 * 1_000_000_000),
        )
        .expect("a span that ends after it starts"),
        ClipOrigin::Manual,
    );

    let generated = HighlightGeneration::new(&defaults(), &recordings)
        .with_existing_clips(std::slice::from_ref(&mine))
        .generate([&kill(60)]);

    assert_eq!(generated.clips().len(), 1);
    assert_eq!(cut_of(&generated.clips()[0]), ("rec-1".to_owned(), 45, 70));
    assert_eq!(mine.title(), "My ace", "the user's clip is untouched");
}

// ---------------------------------------------------------------------------
// Titles and tags
// ---------------------------------------------------------------------------

#[test]
fn a_title_names_the_kinds_in_the_order_they_happened() {
    let recordings = one_recording(300);
    let events = [kill(60), event(EventKind::Assist, 62), kill(64)];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert_eq!(titles(&generated), ["Kill ×2, assist at 1:00"]);
    assert_eq!(
        generated.clips()[0].tags(),
        ["kill", "assist"],
        "tagged by event type, in the wire spelling a search filters on"
    );
}

#[test]
fn a_title_past_an_hour_reads_as_hours_minutes_and_seconds() {
    let recordings = one_recording(2 * 60 * 60);
    let generated = HighlightGeneration::new(&defaults(), &recordings).generate([&kill(3_700)]);

    assert_eq!(titles(&generated), ["Kill at 1:01:40"]);
}

#[test]
fn a_kind_this_build_has_no_word_for_still_titles_and_tags_a_clip() {
    // A plugin's own name is namespaced, and the namespace is how the
    // vocabulary stays collision-free rather than something to show somebody.
    // The tag keeps the whole wire name, because that is what a search matches.
    let objective = EventKind::Unrecognised("objective_taken".to_owned());
    let flag = EventKind::Custom(CustomName::new("acme-cs2.flag_captured").expect("a valid name"));
    let rules = rules_with(|global| {
        for kind in [objective.clone(), flag.clone()] {
            global
                .set_rule(kind, Some(HighlightRule::unset().with_enabled(Some(true))))
                .expect("a kind a user switched on");
        }
    });
    let recordings = one_recording(300);
    let events = [event(objective, 60), event(flag, 62)];

    let generated = HighlightGeneration::new(&rules, &recordings).generate(&events);

    assert_eq!(
        titles(&generated),
        ["Objective taken, flag captured at 1:00"]
    );
    assert_eq!(
        generated.clips()[0].tags(),
        ["objective_taken", "acme-cs2.flag_captured"]
    );
}

#[test]
fn a_title_cannot_grow_without_limit_however_a_kind_is_spelled() {
    // A kind read back from storage is whatever text was stored, and a title is
    // a row in a list rather than a place to put a plugin's essay.
    let long = EventKind::Unrecognised("a".repeat(400));
    let rules = rules_with(|global| {
        global
            .set_rule(
                long.clone(),
                Some(HighlightRule::unset().with_enabled(Some(true))),
            )
            .expect("a kind a user switched on");
    });
    let recordings = one_recording(300);

    let generated = HighlightGeneration::new(&rules, &recordings).generate([&event(long, 60)]);

    let title = generated.clips()[0].title();
    assert!(
        title.chars().count() <= 96,
        "a title of {} characters: {title}",
        title.chars().count()
    );
    assert!(title.ends_with('…'), "{title}");
}

// ---------------------------------------------------------------------------
// The rules decide, and generation does not decide again
// ---------------------------------------------------------------------------

#[test]
fn an_event_the_rules_do_not_select_produces_nothing_at_all() {
    // A round ending is a boundary rather than a moment, and a plugin's own
    // invention is off until a user says otherwise. Neither is withheld here,
    // because neither ever became a moment: `decision_for` is what says why.
    let recordings = one_recording(300);
    let events = [
        event(EventKind::RoundEnded, 40),
        event(
            EventKind::Custom(CustomName::new("acme-cs2.smoke_thrown").expect("a valid name")),
            45,
        ),
    ];

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate(&events);

    assert!(generated.is_empty(), "{:?}", titles(&generated));
}

#[test]
fn a_game_that_keeps_longer_clips_of_a_kill_gets_them() {
    // The per-game layer, reaching the clip: the same three-layer fold the
    // frame rate uses, and generation reads whatever it resolved to rather than
    // having a second opinion about it.
    let mut game = HighlightRules::none();
    game.set_rule(
        EventKind::Kill,
        Some(HighlightRule::unset().with_trail(Some(SECOND * 30))),
    )
    .expect("a valid trail");
    let rules = HighlightRules::resolve(counter_strike(), &HighlightRules::none(), Some(&game));
    let recordings = one_recording(1_800);

    let generated = HighlightGeneration::new(&rules, &recordings).generate([&kill(600)]);

    assert_eq!(
        cut_of(&generated.clips()[0]),
        ("rec-1".to_owned(), 585, 630),
        "the global fifteen seconds of lead, and the game's thirty of trail"
    );
}

#[test]
fn a_source_the_rules_do_not_trust_enough_produces_no_clip() {
    // Confidence is filtered by the rules and never re-judged here: an event
    // that did not pass never becomes a moment, so there is nothing to withhold
    // and nothing to explain twice.
    let recordings = one_recording(300);
    let unsure = GameEvent::new(
        EventKind::Kill,
        EventTiming::new(at(60), Duration::ZERO),
        source(),
        Confidence::new(0.2).expect("a valid confidence"),
    );

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate([&unsure]);

    assert!(generated.is_empty());
}

#[test]
fn precision_widens_the_clip_rather_than_moving_the_moment() {
    // A source polled every two seconds knows the moment to within a second and
    // says so, so the clip keeps a second more either side. The cause is still
    // the moment the event describes.
    let recordings = one_recording(300);
    let polled = GameEvent::new(
        EventKind::Kill,
        EventTiming::new(at(60), SECOND).reported_late_by(SECOND * 3),
        source(),
        Confidence::CERTAIN,
    );

    let generated = HighlightGeneration::new(&defaults(), &recordings).generate([&polled]);

    assert_eq!(cut_of(&generated.clips()[0]), ("rec-1".to_owned(), 44, 71));
    assert_eq!(
        generated.clips()[0]
            .origin()
            .cause()
            .map(HighlightCause::at),
        Some(at(60)),
        "the clip is built around the kill, not around the moment it was heard"
    );
}

#[test]
fn events_out_of_order_and_from_two_sources_produce_the_same_clips() {
    // Nothing places an event by its arrival, and two integrations reporting
    // the same second arrive in whatever order their transports allow.
    let recordings = one_recording(1_800);
    let other = GameEvent::new(
        EventKind::Assist,
        EventTiming::new(at(63), Duration::ZERO),
        EventSource::plugin("other-integration").expect("a valid identifier"),
        Confidence::CERTAIN,
    );
    let forwards = [kill(60), other.clone(), kill(600)];
    let backwards = [kill(600), other, kill(60)];

    let rules = defaults();
    let generation = HighlightGeneration::new(&rules, &recordings);
    let first = generation.generate(&forwards);
    let second = generation.generate(&backwards);

    assert_eq!(titles(&first), titles(&second));
    assert_eq!(
        first
            .clips()
            .iter()
            .map(cut_of)
            .collect::<Vec<(String, u64, u64)>>(),
        second
            .clips()
            .iter()
            .map(cut_of)
            .collect::<Vec<(String, u64, u64)>>()
    );
    assert_no_two_cover_the_same_seconds(first.clips());
}
