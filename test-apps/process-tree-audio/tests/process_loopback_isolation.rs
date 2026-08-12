//! Isolation, proved by listening rather than asserted.
//!
//! # What this test is for
//!
//! Issue #26's first two acceptance criteria, and the reason ADR 0003 chose
//! process-scoped capture at all: a track scoped to a game's process tree has
//! to contain **that tree's audio and nothing else**, including when the sound
//! comes from a process the game started after the recording began.
//!
//! Nothing about that can be established by checking that an API returned
//! `S_OK`. A capture that quietly recorded the whole endpoint would satisfy
//! every structural assertion in this workspace and would be wrong in the way
//! that matters most — "other system audio" is defined as the complement of the
//! game's tree, so a capture that is really recording everything puts the whole
//! machine into the track labelled with the game's name, and nobody finds out
//! until they open the file in an editor days later (AGENTS.md section 21).
//!
//! So the test listens. Two process trees play two known tones at the same
//! time, on the same output device; the capture is scoped to one of them; and
//! the recording is measured with a Goertzel filter — the same measurement the
//! media harness uses, reused rather than written again (AGENTS.md
//! section 55).
//!
//! ```text
//!  this test ─┬─▶ process-tree-audio (silent parent) ──spawn──▶ child playing 997 Hz
//!             │        ▲
//!             │        └── the capture is scoped to this tree
//!             └─▶ process-tree-audio --play --frequency 1373   (its own tree; not captured)
//! ```
//!
//! # It plays a sound
//!
//! Two of them, quietly, for about two seconds. `CLIPPED_SKIP_AUDIO` asks for
//! quiet and skips the test; `CLIPPED_REQUIRE_AUDIO` turns a skip into a
//! failure on a machine that is supposed to be able to do this. Both are the
//! convention `crates/audio` already uses.

#![cfg(windows)]

use core::time::Duration;
use std::io::Write as _;
use std::time::Instant;

use clipped_audio::windows::ProcessLoopbackCapture;
use clipped_audio::{AudioError, Capture};
use clipped_media_validation::AudioContent;
use clipped_process_tree_audio::harness::ToneSubject;
use clipped_process_tree_audio::SECOND_FREQUENCY;

/// The subject, built by Cargo for this package.
const SUBJECT: &str = env!("CARGO_BIN_EXE_process-tree-audio");

/// How long a subject is given to start playing.
const PATIENCE: Duration = Duration::from_secs(10);

/// How much stronger the tree's own tone has to be than the one belonging to
/// the tree next door.
///
/// Eight times in amplitude, about 18 dB, which is
/// `clipped_media_validation::Tone`'s own default and is chosen there
/// for the same reason: two sources mixed at anything like equal level are
/// nowhere near that far apart, and a track that merely picked up a little
/// bleed through a shared device is.
const MINIMUM_RATIO: f64 = 8.0;

/// The environment variable that turns "this machine cannot do this" from a
/// skip into a failure.
const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

/// The environment variable that asks the tests which make a noise not to.
const SKIP_AUDIO: &str = "CLIPPED_SKIP_AUDIO";

fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Whether the caller should skip because the machine has been asked for quiet.
///
/// Consulted before anything is started, which is the difference between this
/// and [`skipped`]: by the time a test has discovered it cannot run, it has
/// already made whatever noise it was going to make.
fn suppressed() -> bool {
    if !is_set(SKIP_AUDIO) {
        return false;
    }
    assert!(
        !is_set(REQUIRE_AUDIO),
        "{SKIP_AUDIO} and {REQUIRE_AUDIO} are both set. One says these tests must not run and \
         the other says they must not be skipped; there is no behaviour that satisfies both, so \
         neither is being guessed at."
    );
    skipped(&format!("{SKIP_AUDIO} is set"));
    true
}

/// Reports that the test could not run here.
///
/// Written through `std::io::stderr()` rather than with `eprintln!` because
/// libtest captures the macros, and a skip nobody can see is how a test quietly
/// stops testing anything.
fn skipped(reason: &str) {
    if is_set(REQUIRE_AUDIO) {
        panic!("{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}");
    }
    let _ = writeln!(std::io::stderr(), "SKIPPED (audio): {reason}");
}

#[test]
fn a_trees_track_holds_the_tone_its_child_played_and_not_the_one_next_door() {
    if suppressed() {
        return;
    }

    // The game: a process that plays nothing itself. The capture is opened on
    // it *before* it starts the child that makes the noise, so what this
    // asserts is that Windows follows the tree as it grows rather than
    // photographing it at activation.
    let mut game = match ToneSubject::start(SUBJECT, &[]) {
        Ok(subject) => subject,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };

    let mut capture = match ProcessLoopbackCapture::open(game.pid()) {
        Ok(capture) => capture,
        Err(error @ AudioError::ProcessLoopbackUnavailable { .. }) => {
            skipped(&error.to_string());
            return;
        }
        Err(error) => {
            skipped(&format!("the game's audio could not be captured: {error}"));
            return;
        }
    };
    let format = capture.format();
    let channels = usize::from(format.channels().get());

    // The tree next door: an unrelated process playing a different tone into
    // the same output device for the whole test. It is started by this test
    // rather than by the game, so it is nothing to do with the game's tree —
    // which is exactly what the capture has to notice.
    let mut neighbour = match ToneSubject::start(
        SUBJECT,
        &["--play", "--frequency", &SECOND_FREQUENCY.to_string()],
    ) {
        Ok(subject) => subject,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };
    assert!(
        neighbour.tone().is_some(),
        "the neighbouring subject reported itself as playing"
    );

    // Before the game plays anything: the machine is not quiet — the tree next
    // door is playing — and this track still has to be silence.
    let before = record(&mut capture, Duration::from_millis(500), channels);

    let (child, tone) = match game.spawn_child(PATIENCE) {
        Ok(child) => child,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };
    assert_ne!(child, game.pid(), "the tone comes from a child process");

    // The endpoint holds a little audio, so the tone takes a moment to reach
    // the capture; recording through that moment would put pre-tone audio into
    // the measurement.
    let _ = record(&mut capture, Duration::from_millis(400), channels);
    let during = record(&mut capture, Duration::from_millis(900), channels);

    game.stop();
    neighbour.stop();

    let rate = format.sample_rate().get();
    let before = AudioContent::from_samples(before, rate);
    let during = AudioContent::from_samples(during, rate);

    let own = during.magnitude_at(f64::from(tone.frequency));
    let intruder = during.magnitude_at(f64::from(SECOND_FREQUENCY));
    let (peak, peak_magnitude) = during.dominant_frequency();
    let _ = writeln!(
        std::io::stderr(),
        "process {child}'s {} Hz measured {own:.5}; the neighbouring tree's {SECOND_FREQUENCY} Hz \
         measured {intruder:.5}; the strongest frequency in the track was {peak:.1} Hz \
         ({peak_magnitude:.5}). Before the child started, the track's peak amplitude was {:.2e}",
        tone.frequency,
        before.peak_amplitude()
    );

    // 1. The game's own audio is there, and it came from a process that did not
    //    exist when the capture was opened.
    assert!(
        !during.is_silent(),
        "the track has to contain the tone the game's child played, and it is silent"
    );
    assert!(
        (peak - f64::from(tone.frequency)).abs() <= 5.0,
        "the strongest frequency in the game's track should be the {} Hz its child played, not \
         {peak:.1} Hz",
        tone.frequency
    );

    // 2. The tree next door is not, which is the whole product.
    assert!(
        intruder * MINIMUM_RATIO < own,
        "the neighbouring process tree's {SECOND_FREQUENCY} Hz must not be audible in the game's \
         track: it measured {intruder:.5} against the game's own {own:.5}, which is only {:.1}x \
         apart",
        own / intruder.max(f64::MIN_POSITIVE)
    );

    // 3. And it was not audible before the game played anything either, which
    //    is the same claim with nothing of the game's to hide behind: a capture
    //    that was really recording the endpoint would have the neighbour's tone
    //    here at full strength.
    assert!(
        before.is_silent(),
        "a track scoped to a process that plays nothing has to be silent while another process \
         is playing: its peak amplitude was {:.2e} and {SECOND_FREQUENCY} Hz measured {:.5}",
        before.peak_amplitude(),
        before.magnitude_at(f64::from(SECOND_FREQUENCY))
    );
}

/// Records for `duration`, returning the first channel's samples.
///
/// One channel rather than the interleaved buffer, because a Goertzel filter
/// over interleaved stereo measures every frequency at half of what it is.
fn record(capture: &mut ProcessLoopbackCapture, duration: Duration, channels: usize) -> Vec<f32> {
    let until = Instant::now() + duration;
    let mut samples = Vec::new();
    while Instant::now() < until {
        match capture
            .read(Duration::from_millis(100))
            .expect("a healthy capture does not fail")
        {
            Capture::Samples(block) => {
                samples.extend(block.samples().iter().step_by(channels).copied());
            }
            Capture::Idle | Capture::FormatChanged(_) => {}
        }
    }
    samples
}
