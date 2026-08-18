//! A process that joins the game's tree while the capture is already running.
//!
//! # The criterion this makes checkable
//!
//! [Issue #27](https://github.com/wildware-uk/clipped/issues/27)'s second
//! acceptance criterion: *adding a process to the tree mid-recording moves its
//! audio to the game track without dropouts.* It is the one thing about the two
//! process-scoped tracks that nothing measured. `tests/audio/track_isolation.rs`
//! proves the **partition** — one tree's tone on the game track, everything
//! else's on the complement's — against a set of processes that was settled
//! before the recording started, and `process_loopback_isolation.rs` beside this
//! file proves that a tree the capture is scoped to may gain its **first noisy
//! member** afterwards. Neither asks what happens when a tree that is *already
//! being heard* gains another member, which is what a game does when it starts a
//! helper, a second renderer or an anti-cheat service an hour in.
//!
//! That is not a formality. `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS` names one root
//! process and Windows includes "the processes it started"; whether that set is
//! evaluated once at activation or continuously is documented nowhere, and the
//! difference decides where an ordinary game's audio lands. If a joiner is not
//! in the tree as far as the audio engine is concerned, its sound does not
//! merely go missing from the game track — the complement is defined as
//! everything the tree did not play, so it lands on **other system audio**,
//! labelled as if it were the browser. So this drives the platform and finds out
//! rather than reading the documentation.
//!
//! # The arrangement
//!
//! ```text
//!  this test ─┬─▶ process-tree-audio (silent root) ──spawn──▶ 997 Hz, from before the baseline
//!             │        ▲                            └spawn──▶ 1699 Hz, started mid-recording
//!             │        └── both captures are scoped to this tree, from one activation
//!             └─▶ process-tree-audio --play --frequency 1373   (its own tree; never captured)
//! ```
//!
//! Three tones, because three claims have to be told apart. 997 Hz is a member
//! of the tree that is **already audible** when the joiner arrives, and is what
//! makes "without dropouts" measurable at all: without it the tree's track is
//! silence before the join, and silence cannot be shown to have kept flowing.
//! 1699 Hz is the joiner. 1373 Hz is a tree that is nothing to do with the game,
//! playing for the whole run, and is the control: a capture that was quietly
//! recording the whole endpoint would satisfy "the joiner's tone arrived"
//! perfectly, and is caught only by a tone that must **not** be there.
//!
//! Both sides of the tree are opened, from one
//! `ProcessLoopbackCapture::open_pair`, because the criterion says *moves*. A
//! joiner whose tone appears on the game track and stays on the complement's as
//! well has not moved anywhere: that is the duplication the excluding side
//! exists to prevent, and it is invisible to a test that looks at one track.
//!
//! # What is measured
//!
//! | | |
//! | --- | --- |
//! | Arrival | The first 25 ms slice in which the joiner's tone reaches half the strength it settles at, against the moment the root was asked for it |
//! | Continuity | The strength of the tone that was **already** playing, in the same slices, across the join |
//! | Dropouts | Frames of silence the capture synthesised, and packets the audio engine flagged as following lost data, from the baseline to the end |
//! | The loss | The longest run of exact zeros among the frames the audio engine *delivered*, and what `CaptureStats::unflagged_dropouts` made of it |
//! | Isolation | What each of the two tracks holds and what it must not, at [`MINIMUM_RATIO`] apart |
//!
//! Two window lengths, deliberately. The isolation assertions are made over
//! hundreds of milliseconds, where a Goertzel bin is a few hertz wide and one
//! tone leaks nothing into another's bin. The arrival and continuity curves are
//! 25 ms, where a bin is about 40 Hz and leakage between the three tones is
//! real — so nothing is asserted there against another tone's strength, only
//! against *its own* strength earlier or later, which leakage cannot
//! manufacture.
//!
//! # What it found, which is half good news
//!
//! On Windows 11 Pro build 26200, over fourteen runs:
//!
//! **The move happens, and it happens quickly.** Windows follows a tree as it
//! grows rather than photographing it at activation: the joiner's tone reaches
//! the tree's track 50–75 ms after the root was asked to start the process —
//! which includes starting a process and opening a render stream — and 30–45 ms
//! after the process said its stream was open. It reaches the tree's track
//! *only*: on the complement's track it measures 0.00016 against the 0.029 it
//! measures on the tree's, so it moved rather than doubling.
//!
//! **The join is not free, and it is a hole rather than a click.** Every join
//! costs the tree's track **1,504 frames — 31.33 ms — of exact digital zeros**,
//! delivered inside ordinary packets whose flags are `0`: no
//! `AUDCLNT_BUFFERFLAGS_SILENT`, no `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`,
//! and `CaptureStats::discontinuities` at zero throughout. The run begins on a
//! packet boundary and ends inside one, and both edges are a step straight
//! from the signal to zero and back, which is what makes it audible: two clicks
//! with a dropout between them.
//!
//! It was read as a *splice at full amplitude* when
//! [issue #626](https://github.com/wildware-uk/clipped/issues/626) was written,
//! because the 25 ms window holding it measured 0.008 of a 0.0400 tone while
//! its peak amplitude stayed at 0.0400. That peak is the 5 ms of the window the
//! zeros do not cover: 0.00801/0.04000 is 0.2003, the window's root-mean-square
//! falls by exactly its square root, and dumping the samples shows 1,504
//! consecutive `0.0` between one sample at −0.0215 and the next at +0.0308. So
//! the three shapes that issue lists are one shape, and the odd run that showed
//! "a hole the capture covered with synthesised silence" was a reader falling
//! behind on a loaded machine, not a second phenomenon.
//!
//! **It belongs to the tap whose stream set changed.** A process starting
//! *outside* the tree puts the same 31.33 ms of zeros on the **complement's**
//! track and leaves the tree's flat, which is what
//! [`audio_starting_and_stopping_outside_the_tree_costs_the_complements_track_a_hole_each`]
//! measures. So it is not an endpoint-wide hiccup and not a fault of include
//! mode. A stream **leaving** costs the same as one joining, so in a real
//! recording every application that starts playing and every one that stops
//! costs the other-system-audio track 31 ms.
//!
//! **The whole-endpoint tap is immune.** `SystemAudioCapture` — an ordinary
//! `AUDCLNT_STREAMFLAGS_LOOPBACK` capture of the endpoint — was watched across
//! the same join and the same leave three times and produced no run of exact
//! zeros longer than a millisecond, with the joiner's tone measuring 0.04001
//! during against 0.00007 before. It is process-scoped taps specifically.
//!
//! So issue #27's second criterion is met in its first half and not in its
//! second, and issue #626 is the defect. What this file does about it is hold
//! it still: [`MAXIMUM_HOLE`] measures the zeros directly and fails if they
//! grow past 40 ms, [`DISTURBANCE_NEAR`] fails if they move away from the
//! change that caused them, and [`MAXIMUM_DISTURBED`] keeps the older,
//! coarser bound on how much of the tone is lost.
//!
//! **And that the recorder now counts it.** The hole cannot be closed — nothing
//! on the client side of WASAPI avoids it, see the table below — so what issue
//! #626 asks for is that it stops being invisible, and
//! `CaptureStats::unflagged_dropouts` is that count. Both halves are asserted
//! here, because either alone is worth nothing: the tap whose stream set
//! changed reports at least one run accounting for the zeros this file measured
//! in the samples, and the tap beside it, carrying audio over the same window
//! and losing none of it, reports **zero**. A counter that always says
//! something is as useless as one that never does.
//!
//! # What was tried against it, and did not help
//!
//! All of the following were measured on this machine, with the hole read as
//! the longest run of exact zeros among the frames the audio engine actually
//! delivered. Every one of them produced the same 1,504 frames:
//!
//! | Tried | Runs | Hole |
//! | --- | --- | --- |
//! | As shipped: event-driven, 200 ms buffer | 5 | 31.33 ms |
//! | Polling — `Initialize` without `AUDCLNT_STREAMFLAGS_EVENTCALLBACK` | 3 | 31.33 ms |
//! | A 10 ms buffer duration | 3 | 31.33 ms |
//! | A 1,000 ms buffer duration | 3 | 31.33 ms |
//! | `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` | 2 | 31.33 ms |
//! | `AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY` | 3 | 31.33 ms |
//! | A `--release` build | 3 | 31.33 ms |
//! | Two taps of one tree, activated separately | 1 | 31.33 ms, **the same 1,504 samples** |
//!
//! `AUDCLNT_STREAMFLAGS_NOPERSIST`, `AUDCLNT_STREAMFLAGS_RATEADJUST` and
//! `AUDCLNT_STREAMFLAGS_CROSSPROCESS` are refused outright by a process-loopback
//! client, with `AUDCLNT_E_INVALID_STREAM_FLAG` (`0x88890021`).
//!
//! The last row is the one that closes the subject. Two `IAudioClient`s
//! activated separately against the same tree produce **sample-identical**
//! tracks — a sum of squared differences of exactly 0.0 over 6,000 samples once
//! the two are aligned — and their holes start at the same sample and are the
//! same length. There is no second copy to splice over the first, so nothing on
//! the client side of WASAPI can avoid this. Every activation also costs 1,504
//! frames of zeros at the *front* of its track, which is the same rebuild seen
//! from the other end.
//!
//! # It plays a sound
//!
//! Three of them, quietly — about −28 dBFS each — for about four seconds.
//! `CLIPPED_SKIP_AUDIO` asks for quiet and skips; `CLIPPED_REQUIRE_AUDIO` turns
//! any skip into a failure, which is what a run whose numbers are being recorded
//! should use. It is `#[ignore]`d because it needs a real output endpoint and a
//! Windows that can scope a capture to a process.
//!
//! ```text
//! cargo test -p clipped-process-tree-audio --test mid_recording_joiner -- --ignored --nocapture
//! ```

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::Write as _;
use std::time::Instant;

use clipped_audio::windows::ProcessLoopbackCapture;
use clipped_audio::{AudioError, Capture, SampleOrigin};
use clipped_media_validation::AudioContent;
use clipped_process_tree_audio::harness::{skipped, suppressed, PlayingTone, ToneSubject};
use clipped_video_pattern::steady_tone::{FREQUENCY, SECOND_FREQUENCY, THIRD_FREQUENCY};

/// The subject, built by Cargo for this package.
const SUBJECT: &str = env!("CARGO_BIN_EXE_process-tree-audio");

/// How long a subject is given to start playing.
const PATIENCE: Duration = Duration::from_secs(10);

/// How long a player is given between saying it is playing and being measured.
///
/// The audio engine holds a fifth of a second, so a measurement taken the
/// instant a player says its stream is open is mostly of the moment before it
/// was.
const SETTLE: Duration = Duration::from_millis(700);

/// How much of the track before the joiner is measured as the baseline.
const BASELINE: Duration = Duration::from_millis(900);

/// How long the recording carries on after the joiner has been asked for.
const AFTER_JOIN: Duration = Duration::from_millis(1_800);

/// How long each slice of the arrival and continuity curves is.
///
/// 25 ms is 1,200 frames at 48 kHz, which puts the arrival measurement's
/// resolution at 25 ms and a Goertzel bin at about 40 Hz. See the module
/// documentation for why nothing is asserted *between* tones at this length.
const SLICE: Duration = Duration::from_millis(25);

/// How long a reader waits for a block before looking at the stop flag again.
const READ_TIMEOUT: Duration = Duration::from_millis(50);

/// How much stronger a tone that belongs on a track has to be than one that does
/// not.
///
/// Eight times in amplitude, about 18 dB, which is
/// `clipped_media_validation::Tone`'s own default and is chosen there for the
/// same reason: two sources mixed at anything like equal level are nowhere near
/// that far apart, and a track that merely picked up a little bleed through a
/// shared device is.
const MINIMUM_RATIO: f64 = 8.0;

/// The fraction of its settled strength at which the joiner's tone counts as
/// having arrived.
///
/// Half, which is 6 dB down. The tone rises through the audio engine's buffer
/// over a few tens of milliseconds rather than appearing at full strength in one
/// slice, so a threshold at the top of that ramp would time the buffer rather
/// than the arrival, and one at the bottom would time the noise floor.
const ARRIVED: f64 = 0.5;

/// How far the tone that was already playing may fall before that slice counts
/// as disturbed.
///
/// Half its own baseline, which on this measurement is an enormous margin: the
/// undisturbed slices sit between 0.0398 and 0.0402 run after run, and a
/// disturbed one falls to about 0.008. Nothing lands in between, so the line
/// could go almost anywhere; it is here because half is the level at which a
/// listener would begin to hear a hole rather than a level this measurement
/// needs.
const CONTINUITY: f64 = 0.5;

/// How many 25 ms slices of the tree's *existing* audio a join may disturb.
///
/// **This is a defect being pinned, not a tolerance being granted.** Issue #27's
/// second criterion asks for a join "without dropouts" and this machine does not
/// deliver one: every join costs one slice, occasionally two, in which the tone
/// that was already playing loses its phase continuity — and about one join in
/// eight leaves an actual hole, 990 frames of silence the capture had to
/// synthesise because the audio engine delivered nothing. The module
/// documentation has the measurements and
/// [issue #626](https://github.com/wildware-uk/clipped/issues/626) is the defect.
///
/// So this bound exists to stop the click growing while nobody is looking. Three
/// slices against the one or two that were measured, which is enough headroom
/// for a busy machine and nowhere near enough to hide a join that started
/// costing a tenth of a second.
const MAXIMUM_DISTURBED: usize = 3;

/// The longest run of digital silence a change to a tap's stream set may cost.
///
/// **This is a defect being pinned, not a tolerance being granted**, and it is
/// the sharp form of [`MAXIMUM_DISTURBED`]: the loss is not a tone measurement
/// that dipped, it is 1,504 frames of exactly `0.0` in the middle of a track,
/// and reading it as such takes the bound from the 75 ms three slices allow to
/// 40 ms and stops it depending on what the subject was playing.
///
/// 1,504 frames is 31.33 ms at 48 kHz and it does not vary: over more than
/// thirty runs across every initialisation in the module documentation's table
/// it was 1,504 every time, on both taps, in debug and release. 40 ms is
/// therefore about a quarter of headroom — room for an endpoint whose device
/// period is not this machine's, and nowhere near room for a hole that had
/// doubled.
///
/// Measured over the frames the audio engine *delivered*: silence this crate
/// synthesised because a reader fell behind is zeros too, and counting it here
/// would fail this test for a busy machine rather than for
/// [issue #626](https://github.com/wildware-uk/clipped/issues/626).
/// [`Track::holes`] does the excluding, and the frames themselves are asserted
/// on separately.
const MAXIMUM_HOLE: Duration = Duration::from_millis(40);

/// How far from the joiner's arrival a disturbed slice may sit before it is a
/// different problem.
///
/// The disturbance measured here begins in the slice immediately before the one
/// the joiner's tone arrives in, every time. A dip somewhere else in the run is
/// not the cost of a join — it is a capture that drops audio for reasons nothing
/// here has identified, and it should fail rather than be absorbed by a count.
const DISTURBANCE_NEAR: Duration = Duration::from_millis(100);

/// How far the capture's own count of the frames it lost may sit from the count
/// made here over the samples it handed back.
///
/// The two are the same measurement made twice, so they agree exactly on this
/// machine run after run — but they are not made on the same samples. The
/// capture counts as the packet goes past, after drift correction has nudged its
/// frame count; the count here is made over what the reader kept, and a run
/// whose first or last frame the resampler interpolated against its neighbour is
/// one frame shorter or longer on one side than the other. Four frames is 83
/// microseconds and cannot hide a disagreement worth knowing about: the runs
/// being compared are 1,504 frames each.
const FRAMES_EITHER_EDGE: u64 = 4;

/// The longest the joiner's audio may take to reach the tree's track, measured
/// from the moment the root was asked to start it.
///
/// That moment is before the process exists, so this covers starting a process,
/// opening a render stream and the audio engine's own buffering as well as
/// anything Windows spends noticing that the tree has a new member. Measured at
/// 50–75 ms over fourteen runs; the bound is ten times that because the failure
/// worth catching is not a slow arrival but a tree photographed at activation
/// and never looked at again, which produces no arrival at all. A bound set
/// close to the measurement would fail on a busy machine for a reason that has
/// nothing to do with the claim.
const MAXIMUM_ARRIVAL: Duration = Duration::from_millis(750);

#[test]
#[ignore = "plays three tones, and needs an output endpoint and process-scoped capture"]
fn a_process_joining_the_tree_mid_recording_moves_onto_the_trees_track_without_a_dropout() {
    if suppressed() {
        return;
    }

    // The game: a process that plays nothing itself.
    let mut game = match ToneSubject::start(SUBJECT, &[]) {
        Ok(subject) => subject,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };

    // The tree next door: an unrelated process playing into the same endpoint
    // for the whole run. It is started by this test rather than by the game, so
    // it is nothing to do with the game's tree — which is what says whether
    // either capture is really filtering anything at all.
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

    // Both sides of one tree, from one activation, because the criterion is that
    // the joiner's audio *moves*: appearing on the game's track while still on
    // the complement's is duplication rather than a move.
    let (game_side, other_side) = match ProcessLoopbackCapture::open_pair(game.pid()) {
        Ok(pair) => pair,
        Err(error @ AudioError::ProcessLoopbackUnavailable { .. }) => {
            skipped(&error.to_string());
            return;
        }
        Err(error) => {
            skipped(&format!("the game's audio could not be captured: {error}"));
            return;
        }
    };

    // A thread each, which is what `open_pair` is documented to expect and what a
    // recording does with them: one loop serving both would decide the shape of
    // every gap it then measured.
    let stop = AtomicBool::new(false);
    let (script, tree, complement) = std::thread::scope(|scope| {
        let stop = &stop;
        let tree = scope.spawn(move || collect(game_side, stop));
        let complement = scope.spawn(move || collect(other_side, stop));

        let script = play_the_script(&mut game);

        stop.store(true, Ordering::Relaxed);
        (
            script,
            tree.join().expect("the tree's reader did not panic"),
            complement
                .join()
                .expect("the complement's reader did not panic"),
        )
    });

    game.stop();
    neighbour.stop();

    let script = match script {
        Ok(script) => script,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };
    assert_ne!(
        script.incumbent, script.joiner,
        "the joiner is a second process, not the one that was already playing"
    );
    for track in [&tree, &complement] {
        assert!(
            track.failure.is_none(),
            "a healthy capture does not fail: {}",
            track.failure.as_deref().unwrap_or_default()
        );
        assert!(
            !track.samples.is_empty(),
            "a capture that produced no samples at all cannot answer anything"
        );
    }

    // -- what each track holds, over windows long enough to separate tones ----

    let baseline_from = tree.frame_at(script.baseline_from);
    let requested_at = tree.frame_at(script.requested);
    let ready_at = tree.frame_at(script.ready);

    let before = tree.content(baseline_from, requested_at);
    let after = tree.content(tree.frame_at(script.ready + SETTLE), tree.samples.len());
    let elsewhere = complement.content(
        complement.frame_at(script.ready + SETTLE),
        complement.samples.len(),
    );

    let own = f64::from(FREQUENCY);
    let joining = f64::from(THIRD_FREQUENCY);
    let next_door = f64::from(SECOND_FREQUENCY);

    let own_before = before.magnitude_at(own);
    let own_after = after.magnitude_at(own);
    let joiner_before = before.magnitude_at(joining);
    let joiner_after = after.magnitude_at(joining);
    let next_door_before = before.magnitude_at(next_door);
    let next_door_after = after.magnitude_at(next_door);
    let elsewhere_own = elsewhere.magnitude_at(own);
    let elsewhere_joiner = elsewhere.magnitude_at(joining);
    let elsewhere_next_door = elsewhere.magnitude_at(next_door);

    // -- when the joiner arrived, and what happened to what was playing -------

    let curve = tree.slices(THIRD_FREQUENCY, baseline_from);
    // What the joiner's frequency measures while the joiner does not exist,
    // which is what says the settled level below is a tone rather than a floor.
    let quiet = median(&head(&curve, BASELINE));
    let settled = median(&tail(&curve, AFTER_JOIN - SETTLE));
    let threshold = (settled * ARRIVED).max(quiet * MINIMUM_RATIO);
    let arrival = curve
        .iter()
        .copied()
        .skip_while(|(at, _)| *at < requested_at)
        .find(|(_, magnitude)| *magnitude >= threshold)
        .map(|(at, _)| at);

    let continuity = tree.slices(FREQUENCY, baseline_from);
    let held = median(&head(&continuity, BASELINE));
    let (weakest_at, weakest) = continuity
        .iter()
        .copied()
        .min_by(|(_, left), (_, right)| left.total_cmp(right))
        .unwrap_or((baseline_from, 0.0));
    let disturbed: Vec<(usize, f64)> = continuity
        .iter()
        .copied()
        .filter(|(_, magnitude)| *magnitude < held * CONTINUITY)
        .collect();
    let silent = disturbed
        .iter()
        .filter(|(at, _)| tree.content(*at, at + slice_frames(tree.rate)).is_silent())
        .count();

    let synthesised = tree.synthesised_since(baseline_from);
    let discontinuities = tree.discontinuities_since(baseline_from);

    let arrived_in = arrival.map_or_else(
        || "never crossed".to_owned(),
        |at| {
            format!(
                "crossed {:.0} ms after the root was asked for it, and {:.0} ms after it said its \
                 stream was open",
                tree.millis_between(requested_at, at),
                tree.millis_between(ready_at, at)
            )
        },
    );
    let mut out = std::io::stderr();
    let _ = writeln!(
        out,
        "the tree's track: its own {FREQUENCY} Hz measured {own_before:.5} before the joiner and \
         {own_after:.5} after; the joiner's {THIRD_FREQUENCY} Hz measured {joiner_before:.5} \
         before and {joiner_after:.5} after; the tree next door's {SECOND_FREQUENCY} Hz measured \
         {next_door_before:.5} before and {next_door_after:.5} after."
    );
    let _ = writeln!(
        out,
        "the complement's track: the tree next door measured {elsewhere_next_door:.5}, the tree's \
         own tone {elsewhere_own:.5}, the joiner {elsewhere_joiner:.5}."
    );
    let _ = writeln!(
        out,
        "arrival: process {} settled at {settled:.5} against {quiet:.5} while it did not exist, \
         so the threshold was {threshold:.5} — {arrived_in}.",
        script.joiner
    );
    let disturbance = match (disturbed.first(), disturbed.last()) {
        (Some((first, _)), Some((last, _))) => format!(
            "{} slices of {} were disturbed, {silent} of them silent, spanning {:.0} ms and \
             beginning {:.0} ms from the arrival",
            disturbed.len(),
            continuity.len(),
            tree.millis_between(*first, *last) + SLICE.as_millis() as f64,
            arrival.map_or(f64::NAN, |at| tree.millis_between(at, *first)),
        ),
        _ => format!("none of its {} slices were disturbed", continuity.len()),
    };
    let _ = writeln!(
        out,
        "continuity: the {FREQUENCY} Hz already playing held a median {held:.5} over the baseline \
         and its weakest 25 ms slice of the whole watched window was {weakest:.5} ({:.2}x, \
         {:.0} ms in); {disturbance}; {synthesised} frames of synthesised silence and \
         {discontinuities} flagged discontinuities over {} blocks.",
        weakest / held.max(f64::MIN_POSITIVE),
        tree.millis_between(baseline_from, weakest_at),
        tree.blocks.len(),
    );

    // The evidence for the disturbance, slice by slice, and what the *other*
    // tap was doing at the same moment — which is what says the click belongs to
    // the tap that gained a stream rather than to the endpoint.
    let elsewhere_curve =
        complement.slices(SECOND_FREQUENCY, complement.frame_at(script.baseline_from));
    for (at, magnitude) in disturbed.iter().copied() {
        let slice = tree.content(at, at + slice_frames(tree.rate));
        let mirror = tree
            .instant_at(at)
            .map(|when| complement.frame_at(when))
            .and_then(|frame| {
                elsewhere_curve
                    .iter()
                    .copied()
                    .min_by_key(|(other, _)| other.abs_diff(frame))
            });
        let _ = writeln!(
            out,
            "  disturbed at {:.0} ms: {FREQUENCY} Hz {magnitude:.5}, peak {:.5}; the complement's \
             {SECOND_FREQUENCY} Hz at the same moment: {:.5}",
            tree.millis_between(baseline_from, at),
            slice.peak_amplitude(),
            mirror.map_or(f64::NAN, |(_, magnitude)| magnitude),
        );
    }

    // What the loss actually is, which is not a dip in a tone measurement but a
    // run of exact zeros. Reported before it is asserted on so that a machine
    // where the number is different says so in its output.
    let holes = tree.holes(baseline_from);
    let (hole_at, hole) = tree.deepest_hole(baseline_from);
    let _ = writeln!(
        out,
        "the hole: {} run(s) of exact zeros among the frames the audio engine delivered, the \
         longest {:.2} ms ({hole} frames) beginning {:.0} ms into the watched window and {:.0} ms \
         from the joiner's arrival",
        holes.len(),
        tree.millis_between(hole_at, hole_at + hole),
        tree.millis_between(baseline_from, hole_at),
        arrival.map_or(f64::NAN, |at| tree.millis_between(at, hole_at)),
    );

    // And what the recorder itself made of it, which before issue #626 was
    // nothing at all.
    let (counted, counted_frames) = tree.dropouts_since(baseline_from);
    let (elsewhere_counted, _) =
        complement.dropouts_since(complement.frame_at(script.baseline_from));
    let _ = writeln!(
        out,
        "the count: the capture reported {counted} run(s) totalling {counted_frames} frames \
         ({:.2} ms) as lost without the audio engine flagging it, against the {:.2} ms the \
         samples show; the complement's capture reported {elsewhere_counted} over the same \
         window",
        tree.millis_between(0, counted_frames as usize),
        tree.millis_between(hole_at, hole_at + hole),
    );

    // -- 1. the tree was audible before the joiner, and only the tree ---------

    let (peak, _) = before.dominant_frequency();
    assert!(
        (peak - own).abs() <= 5.0,
        "before the joiner started, the strongest frequency on the tree's track should be the \
         {FREQUENCY} Hz a member was already playing, not {peak:.1} Hz"
    );
    assert!(
        next_door_before * MINIMUM_RATIO < own_before,
        "the neighbouring tree's {SECOND_FREQUENCY} Hz must not be audible on the game's track: \
         it measured {next_door_before:.5} against the tree's own {own_before:.5}"
    );

    // -- 2. the joiner's audio reached the tree's track -----------------------

    let Some(arrival) = arrival else {
        panic!(
            "a process that joined the tree while the capture was running was never heard on the \
             tree's track: {THIRD_FREQUENCY} Hz measured {joiner_after:.5} there over the {:.2}s \
             after it started, against {joiner_before:.5} before it existed. Windows scoped this \
             capture to the tree as it stood at activation, so a helper a game starts \
             mid-session is recorded onto the other-system-audio track instead of the game's",
            after.duration().as_secs_f64()
        );
    };
    assert!(
        joiner_after >= joiner_before * MINIMUM_RATIO,
        "the joiner's {THIRD_FREQUENCY} Hz has to be on the tree's track because the joiner \
         played it, not because it was already there: it measured {joiner_after:.5} after \
         against {joiner_before:.5} before, which is only {:.1}x",
        joiner_after / joiner_before.max(f64::MIN_POSITIVE)
    );
    assert!(
        joiner_after >= next_door_after * MINIMUM_RATIO,
        "the joiner's {THIRD_FREQUENCY} Hz has to be on the tree's track as a source rather than \
         as bleed: it measured {joiner_after:.5} against the {next_door_after:.5} of a tone \
         playing into the same endpoint from a process this capture is not scoped to"
    );

    let asked = tree.millis_between(requested_at, arrival);
    assert!(
        asked <= MAXIMUM_ARRIVAL.as_millis() as f64,
        "the joiner's audio took {asked:.0} ms to reach the tree's track, measured from the \
         moment the root was asked to start it; anything over {} ms is long enough that a game's \
         helper is recorded onto the wrong track for a passage somebody would hear",
        MAXIMUM_ARRIVAL.as_millis()
    );

    // -- 3. it moved there, rather than being on both tracks ------------------

    assert!(
        joiner_after >= elsewhere_joiner * MINIMUM_RATIO,
        "the joiner's audio has to *move* onto the tree's track: {THIRD_FREQUENCY} Hz measured \
         {elsewhere_joiner:.5} on the complement's track against {joiner_after:.5} on the \
         tree's, which is one process recorded onto both tracks rather than onto one"
    );
    assert!(
        elsewhere_next_door >= elsewhere_own * MINIMUM_RATIO,
        "the complement's track has to be everything the tree did not play: the tree next door's \
         {SECOND_FREQUENCY} Hz measured {elsewhere_next_door:.5} there and the tree's own \
         {FREQUENCY} Hz measured {elsewhere_own:.5}, which is the game's audio on the \
         other-system-audio track"
    );

    // -- 4. and the audio that was already flowing carried on -----------------

    assert!(
        own_after >= own_before * CONTINUITY,
        "the tree's existing audio has to survive the tree gaining a member: the {FREQUENCY} Hz a \
         member had been playing measured {own_before:.5} before the joiner and \
         {own_after:.5} after, so the member that was already there stopped being part of what \
         this capture records when a second one arrived"
    );
    assert!(
        disturbed.len() <= MAXIMUM_DISTURBED,
        "a join costs the tree's track a click, and this one cost {} slices of 25 ms ({silent} of \
         them silent) against the {MAXIMUM_DISTURBED} that issue #626 pins it at. The tone that \
         was already playing held a median {held:.5} and fell to {weakest:.5}. Either the defect \
         has grown or this machine is doing something the measurement has not seen",
        disturbed.len()
    );
    assert!(
        tree.millis_between(hole_at, hole_at + hole) <= MAXIMUM_HOLE.as_millis() as f64,
        "a process joining the tree cost the tree's track {:.2} ms of exact digital zeros ({hole} \
         frames) against the {} ms issue #626 pins it at. That is audio the game played and this \
         recording does not have, bounded by two steps straight to zero and back — the audio \
         engine delivered it inside packets it flagged as neither silent nor discontinuous, so \
         nothing else in the recorder can notice it",
        tree.millis_between(hole_at, hole_at + hole),
        MAXIMUM_HOLE.as_millis(),
    );
    if hole > 0 {
        let from_arrival = tree.millis_between(arrival, hole_at);
        assert!(
            from_arrival.abs() <= DISTURBANCE_NEAR.as_millis() as f64,
            "the tree's track lost {:.2} ms of audio to exact zeros {from_arrival:.0} ms from the \
             joiner's arrival — too far away to be the cost of the join. A capture that loses the \
             game's audio at some unrelated moment is a defect nothing here has an explanation \
             for",
            tree.millis_between(hole_at, hole_at + hole),
        );
    }
    for (at, magnitude) in disturbed.iter().copied() {
        let from_arrival = tree.millis_between(arrival, at);
        assert!(
            from_arrival.abs() <= DISTURBANCE_NEAR.as_millis() as f64,
            "the tree's audio fell to {magnitude:.5}, {:.2}x the {held:.5} it was holding, {:.0} \
             ms from the joiner's arrival — too far away to be the cost of the join. A capture \
             that drops the game's audio at some unrelated moment is a defect nothing here has \
             an explanation for",
            magnitude / held.max(f64::MIN_POSITIVE),
            from_arrival.abs()
        );
    }
    assert!(
        synthesised <= MAXIMUM_DISTURBED * slice_frames(tree.rate),
        "the audio engine delivered nothing for {synthesised} frames' worth of the run and the \
         capture synthesised silence to cover it. A hole that long is past the click a join is \
         known to cost (issue #626) and is a passage of the game's audio that is simply missing"
    );
    assert_eq!(
        discontinuities, 0,
        "the audio engine flagged packets as following lost data. It has never done so here, \
         including across the joins that cost the click issue #626 describes — so this is new \
         information rather than that defect, and the flag is the only account of audio the \
         engine captured and threw away"
    );

    // -- 5. and the recorder counted it, which is what issue #626 asks for ----

    let measured: usize = holes.iter().map(|(_, length)| length).sum();
    assert_eq!(
        counted,
        holes.len() as u64,
        "the tree's track lost {:.2} ms of audio to a join ({hole} frames of exact zeros at \
         {:.0} ms from the arrival) in {} run(s), and the capture reported {counted}. The loss \
         cannot be fixed and this count is the whole of what a diagnostics screen has to say \
         about it: a count of zero means the recording lost audio and nothing anywhere knows, \
         and a count above the runs the samples hold is a number a support conversation would \
         act on that does not describe the track",
        tree.millis_between(hole_at, hole_at + hole),
        tree.millis_between(arrival, hole_at),
        holes.len(),
    );
    assert!(
        counted_frames.abs_diff(measured as u64) <= FRAMES_EITHER_EDGE,
        "the capture counted {counted_frames} frames lost where the samples show {measured}. \
         The two are the same measurement made twice — here over the samples the reader kept, \
         and there on the capture thread as they went past — so anything but agreement to \
         within an edge frame or two of drift correction means one of them is measuring \
         something else"
    );
}

/// How long the outsider plays for, and how long the recording carries on after
/// it has gone.
///
/// Long enough either side that the hole a start costs and the hole a stop
/// costs are separate runs of zeros rather than one, and that neither can be
/// mistaken for the other.
const OUTSIDER_PLAYS: Duration = Duration::from_millis(1_200);

#[test]
#[ignore = "plays three tones, and needs an output endpoint and process-scoped capture"]
fn audio_starting_and_stopping_outside_the_tree_costs_the_complements_track_a_hole_each() {
    if suppressed() {
        return;
    }

    // The same arrangement as the test above, and the opposite event: the tap
    // whose stream set changes is the **complement's**, because the process
    // that starts is nothing to do with the game. This is the common case in a
    // real recording — the browser tab, the notification, the voice call — and
    // it is the reason issue #626 is a product defect rather than a curiosity
    // about trees.
    let mut game = match ToneSubject::start(SUBJECT, &[]) {
        Ok(subject) => subject,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };
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

    let (game_side, other_side) = match ProcessLoopbackCapture::open_pair(game.pid()) {
        Ok(pair) => pair,
        Err(error @ AudioError::ProcessLoopbackUnavailable { .. }) => {
            skipped(&error.to_string());
            return;
        }
        Err(error) => {
            skipped(&format!("the game's audio could not be captured: {error}"));
            return;
        }
    };

    let stop = AtomicBool::new(false);
    let (moments, tree, complement) = std::thread::scope(|scope| {
        let stop = &stop;
        let tree = scope.spawn(move || collect(game_side, stop));
        let complement = scope.spawn(move || collect(other_side, stop));

        // The game's tree has to be audible too, so that "the tree's track was
        // untouched" is a claim about a track carrying audio rather than about
        // a track carrying nothing.
        let moments = play_the_outsider(&mut game);

        stop.store(true, Ordering::Relaxed);
        (
            moments,
            tree.join().expect("the tree's reader did not panic"),
            complement
                .join()
                .expect("the complement's reader did not panic"),
        )
    });

    game.stop();
    neighbour.stop();

    let (baseline_from, started, stopped) = match moments {
        Ok(moments) => moments,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };
    for track in [&tree, &complement] {
        assert!(
            track.failure.is_none(),
            "a healthy capture does not fail: {}",
            track.failure.as_deref().unwrap_or_default()
        );
        assert!(
            !track.samples.is_empty(),
            "a capture that produced no samples at all cannot answer anything"
        );
    }

    let watched = complement.frame_at(baseline_from);
    let started_at = complement.frame_at(started);
    let stopped_at = complement.frame_at(stopped);

    // The outsider has to have been heard on the complement's track, or the
    // holes below are somebody else's audio starting and this measures nothing.
    let before = complement.content(watched, started_at);
    let during = complement.content(started_at, stopped_at);
    let outsider_before = before.magnitude_at(f64::from(THIRD_FREQUENCY));
    let outsider_during = during.magnitude_at(f64::from(THIRD_FREQUENCY));

    let holes = complement.holes(watched);
    let (hole_at, hole) = complement.deepest_hole(watched);
    let tree_from = tree.frame_at(baseline_from);
    let (tree_hole_at, tree_hole) = tree.deepest_hole(tree_from);

    let mut out = std::io::stderr();
    let _ = writeln!(
        out,
        "the outsider: {THIRD_FREQUENCY} Hz measured {outsider_before:.5} on the complement's \
         track before it started and {outsider_during:.5} while it played."
    );
    for (at, length) in &holes {
        let _ = writeln!(
            out,
            "the complement's track: {:.2} ms of exact zeros ({length} frames) at {:.0} ms, which \
             is {:.0} ms from the start and {:.0} ms from the stop",
            complement.millis_between(*at, at + length),
            complement.millis_between(watched, *at),
            complement.millis_between(started_at, *at),
            complement.millis_between(stopped_at, *at),
        );
    }
    let _ = writeln!(
        out,
        "the tree's track over the same window: {} run(s) of exact zeros, the longest {:.2} ms at \
         {:.0} ms; the complement had {} frames of synthesised silence and {} flagged \
         discontinuities",
        tree.holes(tree_from).len(),
        tree.millis_between(tree_hole_at, tree_hole_at + tree_hole),
        tree.millis_between(tree_from, tree_hole_at),
        complement.synthesised_since(watched),
        complement.discontinuities_since(watched),
    );

    // The recorder's own account of both sides. The tree's is the half that
    // makes this measurement worth anything: a counter that reports a loss on a
    // track that lost nothing says nothing at all when it reports one on a
    // track that did.
    let (counted, counted_frames) = complement.dropouts_since(watched);
    let (tree_counted, tree_counted_frames) = tree.dropouts_since(tree_from);
    let _ = writeln!(
        out,
        "the count: the complement's capture reported {counted} run(s) totalling \
         {counted_frames} frames as lost without the audio engine flagging it; the tree's \
         capture reported {tree_counted} ({tree_counted_frames} frames) over the same window"
    );

    assert!(
        outsider_during >= outsider_before * MINIMUM_RATIO,
        "the process that started outside the tree has to be audible on the complement's track \
         for anything below to be about it: {THIRD_FREQUENCY} Hz measured {outsider_during:.5} \
         while it played against {outsider_before:.5} before it existed"
    );

    // -- what the change cost the tap that took it ---------------------------

    assert!(
        complement.millis_between(hole_at, hole_at + hole) <= MAXIMUM_HOLE.as_millis() as f64,
        "a process starting and stopping outside the game's tree cost the other-system-audio \
         track {:.2} ms of exact digital zeros ({hole} frames) against the {} ms issue #626 pins \
         it at. Every application that starts playing during a recording costs this once and \
         every one that stops costs it again, and the audio engine flags none of it",
        complement.millis_between(hole_at, hole_at + hole),
        MAXIMUM_HOLE.as_millis(),
    );
    for (at, length) in &holes {
        let from_start = complement.millis_between(started_at, *at).abs();
        let from_stop = complement.millis_between(stopped_at, *at).abs();
        assert!(
            from_start.min(from_stop) <= DISTURBANCE_NEAR.as_millis() as f64,
            "the other-system-audio track lost {:.2} ms to exact zeros {:.0} ms into the watched \
             window, which is {from_start:.0} ms from the moment a process outside the tree \
             started and {from_stop:.0} ms from the moment it stopped. A capture that loses audio \
             at some unrelated moment is a defect nothing here has an explanation for",
            complement.millis_between(*at, at + length),
            complement.millis_between(watched, *at),
        );
    }

    // -- and that the tap that did not take it was left alone ----------------

    assert_eq!(
        tree_hole,
        0,
        "the game's track must not lose anything when a process that is nothing to do with the \
         game starts or stops: it lost {:.2} ms to exact zeros at {:.0} ms into the same window. \
         Issue #626 belongs to the tap whose stream set changed, and a hole on this side would \
         mean it is endpoint-wide after all",
        tree.millis_between(tree_hole_at, tree_hole_at + tree_hole),
        tree.millis_between(tree_from, tree_hole_at),
    );

    // -- and that the count says both of those things ------------------------

    let measured: usize = holes.iter().map(|(_, length)| length).sum();
    assert_eq!(
        counted,
        holes.len() as u64,
        "an application outside the game's tree started and stopped, which cost the \
         other-system-audio track {} run(s) of exact zeros — the longest {:.2} ms — and the \
         capture reported {counted}. This is the common case: a browser tab, a notification, a \
         voice call. A count below it leaves every recording ever made saying nothing happened, \
         and a count above it says something happened that did not",
        holes.len(),
        complement.millis_between(hole_at, hole_at + hole),
    );
    assert!(
        counted_frames.abs_diff(measured as u64) <= FRAMES_EITHER_EDGE,
        "the capture counted {counted_frames} frames lost where the samples show {measured}. \
         The two are the same measurement made twice and have to agree"
    );
    assert_eq!(
        (tree_counted, tree_counted_frames),
        (0, 0),
        "the game's track lost nothing while a process outside the tree started and stopped — \
         its longest run of zeros was {:.2} ms — and the capture reported {tree_counted} runs \
         of {tree_counted_frames} frames anyway. A counter that reports something on a track \
         that lost nothing is worth exactly as little as one that reports nothing on a track \
         that did, and it would put the two process-scoped tracks' losses on whichever one a \
         reader happened to look at",
        tree.millis_between(tree_hole_at, tree_hole_at + tree_hole),
    );
}

/// Makes the game's tree audible, then starts and stops a process outside it.
///
/// Answers when the watched window begins, when the outsider was started and
/// when it was asked to stop.
///
/// # Errors
///
/// A sentence saying why there is no tree making a noise, which on a machine
/// that cannot render a tone is a reason to skip rather than to fail.
fn play_the_outsider(game: &mut ToneSubject) -> Result<(Instant, Instant, Instant), String> {
    let (_, playing) = game.spawn_child(PATIENCE)?;
    assert_playing(playing, FREQUENCY);
    std::thread::sleep(SETTLE);

    let baseline_from = Instant::now();
    std::thread::sleep(BASELINE);

    let started = Instant::now();
    let mut outsider = ToneSubject::start(
        SUBJECT,
        &["--play", "--frequency", &THIRD_FREQUENCY.to_string()],
    )?;
    assert!(
        outsider.tone().is_some(),
        "the outsider reported itself as playing"
    );
    std::thread::sleep(OUTSIDER_PLAYS);

    let stopped = Instant::now();
    outsider.stop();
    std::thread::sleep(OUTSIDER_PLAYS);

    Ok((baseline_from, started, stopped))
}

/// What the test asked the subjects to do, and when.
#[derive(Debug)]
struct Script {
    /// The member that was already playing when the joiner arrived.
    incumbent: u32,
    /// The member that arrived while the capture was running.
    joiner: u32,
    /// When the baseline begins: the tree audible, the joiner not yet asked for.
    baseline_from: Instant,
    /// When the root was asked for the joiner. The joiner does not exist yet at
    /// this moment, so an arrival measured from here is an upper bound that
    /// includes starting a process and opening a render stream.
    requested: Instant,
    /// When the joiner said its stream was open. It may already have been making
    /// a sound by the time this was read, so an arrival measured from here is a
    /// lower bound.
    ready: Instant,
}

/// Runs the subjects through the sequence the measurement needs.
///
/// Both captures are already being read on their own threads when this starts,
/// so nothing here has to hurry: what it produces is the moments the analysis
/// divides the tracks at.
///
/// # Errors
///
/// A sentence saying why there is no tree making a noise, which on a machine
/// that cannot render a tone is a reason to skip rather than to fail.
fn play_the_script(game: &mut ToneSubject) -> Result<Script, String> {
    let (incumbent, playing) = game.spawn_child(PATIENCE)?;
    assert_playing(playing, FREQUENCY);
    std::thread::sleep(SETTLE);

    let baseline_from = Instant::now();
    std::thread::sleep(BASELINE);

    let requested = Instant::now();
    let (joiner, joining) = game.spawn_child_playing(THIRD_FREQUENCY, PATIENCE)?;
    let ready = Instant::now();
    assert_playing(joining, THIRD_FREQUENCY);
    std::thread::sleep(AFTER_JOIN);

    Ok(Script {
        incumbent,
        joiner,
        baseline_from,
        requested,
        ready,
    })
}

/// Checks a child is playing what it was asked for.
///
/// A joiner that quietly fell back to the frequency its sibling is playing would
/// make every assertion below pass without a joiner's tone ever being found.
fn assert_playing(tone: PlayingTone, frequency: f32) {
    assert!(
        (tone.frequency - frequency).abs() < 0.5,
        "a child asked for {frequency} Hz reported itself as playing {} Hz",
        tone.frequency
    );
}

/// One capture, as it was read.
#[derive(Debug)]
struct Track {
    /// The first channel's samples, in order and without gaps.
    ///
    /// One channel rather than the interleaved buffer, because a Goertzel filter
    /// over interleaved stereo measures every frequency at half of what it is.
    samples: Vec<f32>,
    /// The endpoint's rate, which is what the sample indices above are in.
    rate: u32,
    /// One entry per block handed over, in order.
    blocks: Vec<Block>,
    /// Why reading stopped early, if it did.
    failure: Option<String>,
}

/// One block of audio, and what the capture said about itself when it arrived.
#[derive(Debug, Clone, Copy)]
struct Block {
    /// Where this block's first frame sits in [`Track::samples`].
    at: usize,
    /// When it was read. The audio in it is *older* than this, by however long
    /// the audio engine held it.
    when: Instant,
    /// How many frames it carried.
    frames: usize,
    /// Whether those frames were captured or invented.
    origin: SampleOrigin,
    /// The capture's running count of packets the audio engine flagged as
    /// following lost data, as of this block.
    discontinuities: u64,
    /// The capture's running count of the runs of zeros issue #626 is, as of
    /// this block, and the frames they held.
    ///
    /// The recorder's own account of the loss this file measures directly, and
    /// the two are asserted against each other below: a counter that reports a
    /// hole where the samples show none, or none where the samples show one, is
    /// a counter nobody could act on.
    dropouts: u64,
    dropout_frames: u64,
}

impl Track {
    /// The latest sample index that had certainly been captured by `instant`.
    ///
    /// The start of the last block read *before* it, which is the conservative
    /// end: the block read at that moment holds audio from before it, so
    /// anything later would claim samples nobody had heard yet. Every duration
    /// measured forwards from here is therefore an upper bound.
    fn frame_at(&self, instant: Instant) -> usize {
        let after = self.blocks.partition_point(|block| block.when <= instant);
        self.blocks
            .get(after.saturating_sub(1))
            .map_or(0, |block| block.at)
    }

    /// The samples between two indices, ready to be measured.
    fn content(&self, from: usize, to: usize) -> AudioContent {
        let to = to.min(self.samples.len());
        let from = from.min(to);
        AudioContent::from_samples(self.samples[from..to].to_vec(), self.rate)
    }

    /// How much of `frequency` sits in each [`SLICE`] from `from` onwards.
    ///
    /// Answered as `(sample index, magnitude)` so that a slice can be turned
    /// back into a moment. A trailing part-slice is dropped rather than
    /// measured, because a shorter window measures a different number.
    fn slices(&self, frequency: f32, from: usize) -> Vec<(usize, f64)> {
        let length = slice_frames(self.rate);
        if length == 0 {
            return Vec::new();
        }

        let mut found = Vec::new();
        let mut at = from;
        while at + length <= self.samples.len() {
            let slice = self.content(at, at + length);
            found.push((at, slice.magnitude_at(f64::from(frequency))));
            at += length;
        }
        found
    }

    /// Runs of exactly `0.0` in the frames the audio engine delivered, from
    /// `from` to the end, as `(sample index, length)`.
    ///
    /// The measurement issue #626 is really about. Silence this crate
    /// synthesised is excluded rather than counted: it is zeros as well, and a
    /// reader that fell behind would otherwise be reported as the audio engine
    /// having lost audio it never lost. A synthesised block therefore ends a
    /// run rather than extending it, which is the conservative direction —
    /// it can only make the hole look shorter than it was.
    ///
    /// Runs shorter than a millisecond are dropped: a handful of zero samples
    /// is where a waveform crosses zero, not a hole.
    fn holes(&self, from: usize) -> Vec<(usize, usize)> {
        let least = self.rate as usize / 1_000;
        let mut found = Vec::new();
        let mut run: Option<usize> = None;
        let close = |run: &mut Option<usize>, at: usize, found: &mut Vec<(usize, usize)>| {
            if let Some(start) = run.take() {
                if at - start >= least {
                    found.push((start, at - start));
                }
            }
        };

        for block in &self.blocks {
            let end = (block.at + block.frames).min(self.samples.len());
            if block.origin == SampleOrigin::SynthesisedSilence || block.at >= end {
                close(&mut run, block.at.min(self.samples.len()), &mut found);
                continue;
            }
            for index in block.at.max(from)..end {
                if self.samples[index] == 0.0 {
                    run.get_or_insert(index);
                } else {
                    close(&mut run, index, &mut found);
                }
            }
        }
        close(&mut run, self.samples.len(), &mut found);
        found
    }

    /// The longest [`Track::holes`] run, as `(sample index, length)`.
    fn deepest_hole(&self, from: usize) -> (usize, usize) {
        self.holes(from)
            .into_iter()
            .max_by_key(|(_, length)| *length)
            .unwrap_or((from, 0))
    }

    /// Frames of silence this crate invented from `from` to the end.
    fn synthesised_since(&self, from: usize) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.at >= from && block.origin == SampleOrigin::SynthesisedSilence)
            .map(|block| block.frames)
            .sum()
    }

    /// Packets the audio engine flagged as following lost data from `from` to
    /// the end.
    fn discontinuities_since(&self, from: usize) -> u64 {
        let before = self
            .blocks
            .iter()
            .rev()
            .find(|block| block.at < from)
            .map_or(0, |block| block.discontinuities);
        self.blocks
            .last()
            .map_or(0, |block| block.discontinuities)
            .saturating_sub(before)
    }

    /// What the capture itself counted as lost between `from` and the end, as
    /// `(runs, frames)`.
    ///
    /// The same delta as [`discontinuities_since`](Self::discontinuities_since)
    /// and for the same reason: the capture counts from the moment it was
    /// opened, and the moment worth measuring from is the baseline.
    fn dropouts_since(&self, from: usize) -> (u64, u64) {
        let before = self.blocks.iter().rev().find(|block| block.at < from);
        let last = self.blocks.last();
        (
            last.map_or(0, |block| block.dropouts)
                .saturating_sub(before.map_or(0, |block| block.dropouts)),
            last.map_or(0, |block| block.dropout_frames)
                .saturating_sub(before.map_or(0, |block| block.dropout_frames)),
        )
    }

    /// When the block holding `index` was read.
    fn instant_at(&self, index: usize) -> Option<Instant> {
        let after = self.blocks.partition_point(|block| block.at <= index);
        self.blocks
            .get(after.saturating_sub(1))
            .map(|block| block.when)
    }

    /// How long, in milliseconds, sits between two sample indices.
    ///
    /// Signed, because an arrival can land either side of the moment it is being
    /// measured against.
    fn millis_between(&self, from: usize, to: usize) -> f64 {
        (to as f64 - from as f64) * 1_000.0 / f64::from(self.rate)
    }
}

/// Reads one capture until `stop`, keeping the samples and what was said about
/// each block.
///
/// A failure is recorded rather than asserted on: this runs on a thread of its
/// own, and a panic here would be reported as "the reader panicked" rather than
/// as what went wrong.
fn collect(mut capture: ProcessLoopbackCapture, stop: &AtomicBool) -> Track {
    let format = capture.format();
    let channels = usize::from(format.channels().get());
    let mut track = Track {
        samples: Vec::new(),
        rate: format.sample_rate().get(),
        blocks: Vec::new(),
        failure: None,
    };

    while !stop.load(Ordering::Relaxed) {
        let at = track.samples.len();
        let described = match capture.read(READ_TIMEOUT) {
            Ok(Capture::Samples(audio)) => {
                let described = (audio.frames(), audio.origin());
                track
                    .samples
                    .extend(audio.samples().iter().step_by(channels).copied());
                Some(described)
            }
            Ok(Capture::Idle | Capture::FormatChanged(_)) => None,
            Err(error) => {
                track.failure = Some(error.to_string());
                break;
            }
        };
        let when = Instant::now();

        if let Some((frames, origin)) = described {
            track.blocks.push(Block {
                at,
                when,
                frames,
                origin,
                discontinuities: capture.stats().discontinuities,
                dropouts: capture.stats().unflagged_dropouts,
                dropout_frames: capture.stats().unflagged_dropout_frames,
            });
        }
    }

    track
}

/// How many frames one [`SLICE`] is at `rate`.
fn slice_frames(rate: u32) -> usize {
    (SLICE.as_secs_f64() * f64::from(rate)) as usize
}

/// The magnitudes of the first `length` of a curve.
fn head(curve: &[(usize, f64)], length: Duration) -> Vec<f64> {
    let slices = (length.as_secs_f64() / SLICE.as_secs_f64()) as usize;
    curve
        .iter()
        .take(slices)
        .map(|(_, magnitude)| *magnitude)
        .collect()
}

/// The magnitudes of the last `length` of a curve.
fn tail(curve: &[(usize, f64)], length: Duration) -> Vec<f64> {
    let slices = (length.as_secs_f64() / SLICE.as_secs_f64()) as usize;
    curve
        .iter()
        .skip(curve.len().saturating_sub(slices))
        .map(|(_, magnitude)| *magnitude)
        .collect()
}

/// The middle value, which is how a settled level is read: one slice spoiled by
/// a scheduling hiccup moves a mean and does not move this.
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get(sorted.len() / 2).copied().unwrap_or_default()
}
