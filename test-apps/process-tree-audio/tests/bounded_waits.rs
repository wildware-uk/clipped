//! That a subject which stops talking fails its test rather than hanging it.
//!
//! # What this is for
//!
//! Issue [#136](https://github.com/wildware-uk/clipped/issues/136)'s second
//! acceptance criterion is that a test can start an audio test application,
//! capture it and assert on the result with nobody watching, **and that nothing
//! is left running afterwards**. The first half is measured by
//! `process_loopback_isolation.rs` and `mid_recording_joiner.rs`. This file is
//! the second half, and it is here because the harness those two use did not
//! have it.
//!
//! [`ToneSubject`] used to wait for the subject's lines with a blocking
//! `read_line`. A subject that started and then said nothing — a player wedged
//! in the audio endpoint it is opening, which is the one slow thing a player
//! does before it can announce anything — blocked the test for ever. Worse,
//! `spawn_child` **documented** a bound it did not have: its deadline was
//! looked at only between lines, so it expired promptly against a chatty
//! subject and never at all against a silent one, which is the only case a
//! deadline is for. An unbounded wait of exactly this shape in
//! `plugins/cs2/tests/plugin_process.rs` once cost a six-hour
//! continuous-integration job.
//!
//! # Why the subjects here are not `process-tree-audio`
//!
//! Proving a timeout needs a subject that does not answer, and
//! `process-tree-audio` always answers — that is the whole point of it. So each
//! test below points the harness at a program built out of `cmd` that behaves
//! the way a wedged subject behaves: `findstr` reading standard input for a
//! pattern nothing sent to it will ever match consumes everything the harness
//! writes and prints nothing back, and `echo` in front of it supplies an
//! announcement for the test that needs one.
//!
//! Neither test opens an audio device, plays a sound or starts a recording, so
//! neither is `#[ignore]`d and neither consults `CLIPPED_SKIP_AUDIO`: they run
//! in `cargo test -p clipped-process-tree-audio` on any Windows machine.

#![cfg(windows)]

use core::time::Duration;
use std::time::Instant;

use clipped_process_tree_audio::harness::ToneSubject;

/// How long each test is prepared to wait for a subject that will not answer.
///
/// Short, because the answer never comes and the only thing being timed is the
/// giving up.
const PATIENCE: Duration = Duration::from_millis(500);

/// How long the whole exchange may take before the wait is not a wait but a
/// hang.
///
/// Ten times [`PATIENCE`] plus the two seconds the harness gives a subject to
/// leave when it closes its standard input, plus room for `cmd` to start on a
/// loaded machine. Generous on purpose: this must not fail because a build was
/// running at the same time. It is still finite, which is the entire claim —
/// before the harness was fixed, both of these ran until somebody noticed.
const HANG: Duration = Duration::from_secs(20);

/// A program that reads standard input, prints nothing, and does not exit.
///
/// `findstr` for a pattern the harness's own commands cannot contain: it
/// consumes every line written to it and matches none of them, so the subject
/// is running, reachable and permanently silent.
const SILENT: &str = "findstr clipped-never-matches-this";

/// The same, behind an announcement, so a test can get past `start`.
const SILENT_AFTER_ANNOUNCING: &str =
    "echo ready pid=1 role=parent&findstr clipped-never-matches-this";

/// A program that announces itself and then never stops talking.
///
/// The opposite failure, and the one a deadline consulted only between lines
/// does catch. The count is finite so that a run which somehow escapes the
/// harness still ends by itself; nothing here waits for it to.
const CHATTY_AFTER_ANNOUNCING: &str =
    "echo ready pid=1 role=parent&for /l %i in (1,1,2000000) do @echo noise";

#[test]
fn a_subject_that_never_announces_itself_is_given_up_on_rather_than_waited_for() {
    let started = Instant::now();
    let outcome = ToneSubject::start_within("cmd", &["/c", SILENT], PATIENCE);
    let elapsed = started.elapsed();

    let reason = outcome.expect_err("a subject that printed nothing cannot have been started");
    assert!(
        reason.contains("said nothing within 0.5s"),
        "the failure should name the wait that expired, and says: {reason}"
    );
    assert!(
        elapsed < HANG,
        "the wait for an announcement was not bounded: it took {elapsed:?}"
    );
}

#[test]
fn a_parent_that_never_starts_a_child_is_given_up_on_rather_than_waited_for() {
    // The subject announces itself, so `start` succeeds and the test reaches
    // the wait that matters. It then answers nothing at all, which is the case
    // the old deadline could not see: it was consulted only after a line had
    // been read, and no line was ever coming.
    let mut parent = ToneSubject::start_within("cmd", &["/c", SILENT_AFTER_ANNOUNCING], HANG)
        .expect("the announcing subject starts");
    assert_eq!(
        parent.pid(),
        1,
        "the announcement is read as it was written"
    );

    let started = Instant::now();
    let outcome = parent.spawn_child(PATIENCE);
    let elapsed = started.elapsed();

    let reason = outcome.expect_err("a subject that started no child cannot have reported one");
    assert!(
        reason.contains("did not start a child within 0.5s"),
        "the failure should name the wait that expired, and says: {reason}"
    );
    assert!(
        elapsed < HANG,
        "the wait for a child was not bounded: it took {elapsed:?}"
    );
}

#[test]
fn a_parent_that_talks_without_starting_a_child_is_given_up_on_at_the_same_deadline() {
    // The other half of the same bound, and the half a deadline looked at only
    // between lines already caught. It is here so that the fix for the silent
    // case cannot quietly undo it: the wait is now made of many short waits,
    // and a subject supplying a line for every one of them would renew it for
    // ever if the deadline were not checked before each.
    let mut parent = ToneSubject::start_within("cmd", &["/c", CHATTY_AFTER_ANNOUNCING], HANG)
        .expect("the announcing subject starts");

    let started = Instant::now();
    let outcome = parent.spawn_child(PATIENCE);
    let elapsed = started.elapsed();

    let reason = outcome.expect_err("a subject that started no child cannot have reported one");
    assert!(
        reason.contains("did not start a child within 0.5s"),
        "the failure should name the wait that expired, and says: {reason}"
    );
    assert!(
        elapsed < HANG,
        "a talkative subject renewed the wait indefinitely: it took {elapsed:?}"
    );
}
