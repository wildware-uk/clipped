//! Measures what logging costs a capture loop when it is switched off.
//!
//! AGENTS.md section 35 requires that disabled debug diagnostics do not
//! measurably affect recording, and section 18 forbids claiming that without
//! measuring it. This file measures it, in the same run as the rest of the test
//! suite, so the claim cannot quietly stop being true.
//!
//! Two things are measured against an identical loop that logs nothing:
//!
//! - [`clipped_logging::trace_frame!`] in a build without the `frame-tracing`
//!   feature, which is what every shipped build is.
//! - `tracing::debug!` with a subscriber installed at `info`, which is the
//!   normal configuration: the statement is compiled in and rejected at
//!   runtime.
//!
//! # Method
//!
//! Four `#[inline(never)]` loops with identical arithmetic, two of which also
//! contain a log statement. Each is run for the same iteration count, once per
//! round, in the same order every round; the fastest round for each loop is
//! kept. The fastest round is used rather than the mean because the quantity
//! being measured is smaller than the scheduling noise on a desktop that is
//! also running a browser, and noise only ever makes a round slower.
//!
//! Two of the four loops are byte-for-byte identical. The difference between
//! them is the noise floor — the residual difference between two things that
//! genuinely cost the same — and it is what "no measurable cost" is judged
//! against. Reporting an overhead smaller than that difference would be
//! reporting nothing.
//!
//! This binary contains one test, deliberately. Installing a second subscriber
//! anywhere in the process makes `tracing` abandon its cached per-callsite
//! decisions, and the disabled `debug!` then costs several times as much —
//! which is a property of the test process, not of Clipped. The companion test
//! that needs its own subscriber lives in `frame_tracing.rs`.
//!
//! Run it with the numbers visible:
//!
//! ```text
//! cargo test --release -p clipped-logging --test hot_loop_cost -- --nocapture
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

/// Iterations per round. At 240 frames a second this is roughly six hours of
/// capture, which is long enough that a per-iteration cost of a nanosecond
/// shows up above the timer's resolution.
const ITERATIONS: u64 = 5_000_000;

/// Rounds per loop. Nine is enough for the fastest round to settle without the
/// test taking long enough to be skipped.
const ROUNDS: usize = 9;

/// The largest per-iteration overhead accepted from a disabled `debug!`, in
/// nanoseconds, on a machine where the baseline loop is faster than this.
///
/// Five nanoseconds is 1.2 microseconds per second of capture at 240 frames a
/// second. The measured figure is far below it (see the pull request for issue
/// #5); the headroom is there because the assertion exists to catch a
/// structural regression — an argument being evaluated eagerly, a lock, an
/// allocation — and not a single extra branch, which no threshold could
/// distinguish from a busy machine.
const DISABLED_DEBUG_TOLERANCE_NS: f64 = 5.0;

/// The largest per-iteration overhead accepted from `trace_frame!` *above the
/// measured noise floor*, in nanoseconds.
///
/// Tighter than the tolerance above because a disabled `trace_frame!` is not
/// merely cheap, it is absent: the statement is behind a `const false`. The
/// deterministic proof of that is
/// `a_disabled_frame_trace_does_not_evaluate_its_arguments`; this bound is what
/// notices if it stops being true in the generated code.
const FRAME_TRACE_TOLERANCE_NS: f64 = 0.5;

/// The same tolerance expressed as a fraction of the baseline loop's own cost,
/// whichever is larger.
///
/// Code placement noise is a proportion of what the loop costs, not a fixed
/// number of nanoseconds: in an unoptimised build the baseline is around 4
/// ns/iteration and two identical loops differ by about 0.2 ns, so a flat 0.5 ns
/// bound is only two and a half times the spread it has to sit above, and a slow
/// or contended runner would fail this test for being slow rather than for
/// logging costing anything. A failure nobody can distinguish from a real
/// regression is worse than no assertion (AGENTS.md sections 25 and 51), so the
/// bound scales the way the debug tolerance below already does. A quarter of the
/// baseline is still far below the cost of the regression this exists to catch:
/// a `trace_frame!` that lost its feature gate becomes a runtime-filtered event,
/// which costs several nanoseconds in either profile.
const FRAME_TRACE_TOLERANCE_FRACTION: f64 = 0.25;

/// The arithmetic every loop performs, so that the loops differ only in whether
/// they contain a log statement.
#[inline(always)]
fn work(accumulator: u64, index: u64) -> u64 {
    accumulator.wrapping_add(black_box(index) ^ 0x9e37_79b9)
}

#[inline(never)]
fn without_logging(iterations: u64) -> u64 {
    let mut accumulator = 0_u64;
    for index in 0..iterations {
        accumulator = work(accumulator, index);
    }
    black_box(accumulator)
}

/// Byte-for-byte identical to [`without_logging`]; the difference between the
/// two is this test's noise floor.
#[inline(never)]
fn without_logging_again(iterations: u64) -> u64 {
    let mut accumulator = 0_u64;
    for index in 0..iterations {
        accumulator = work(accumulator, index);
    }
    black_box(accumulator)
}

#[inline(never)]
fn with_frame_trace(iterations: u64) -> u64 {
    let mut accumulator = 0_u64;
    for index in 0..iterations {
        accumulator = work(accumulator, index);
        clipped_logging::trace_frame!(frame_index = index, accumulator, "frame acquired");
    }
    black_box(accumulator)
}

#[inline(never)]
fn with_disabled_debug(iterations: u64) -> u64 {
    let mut accumulator = 0_u64;
    for index in 0..iterations {
        accumulator = work(accumulator, index);
        tracing::debug!(frame_index = index, accumulator, "frame acquired");
    }
    black_box(accumulator)
}

/// Nanoseconds per iteration for each loop, in the order given, measured by
/// running every loop once per round and keeping each loop's fastest round.
fn nanoseconds_per_iteration(loops: &[fn(u64) -> u64]) -> Vec<f64> {
    // One untimed round so that the caches and the branch predictor are in the
    // same state for the first timed round as for the last.
    for body in loops {
        black_box(body(ITERATIONS / 10));
    }

    let mut best = vec![Duration::MAX; loops.len()];
    for round in 0..ROUNDS {
        for position in 0..loops.len() {
            // Rotate the order every round. Whichever loop runs first in a
            // round is consistently the fastest — the processor has just been
            // handed a burst of work and has not yet settled — and rotating
            // gives every loop that position an equal number of times, so the
            // advantage cancels instead of landing on whichever loop happened
            // to be written first.
            let index = (round + position) % loops.len();
            let start = Instant::now();
            black_box(loops[index](ITERATIONS));
            let elapsed = start.elapsed();
            best[index] = best[index].min(elapsed);
        }
    }

    best.iter()
        .map(|elapsed| elapsed.as_secs_f64() * 1e9 / ITERATIONS as f64)
        .collect()
}

#[test]
fn disabled_logging_costs_nothing_measurable_in_a_hot_loop() {
    // A subscriber has to exist for `tracing::debug!` to take its real
    // rejection path. `info` is the level a shipped build runs at, and the
    // events this test emits are all below it, so nothing is written anywhere.
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::sink)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("no other test in this binary installs a subscriber");

    let measurements = nanoseconds_per_iteration(&[
        without_logging,
        without_logging_again,
        with_frame_trace,
        with_disabled_debug,
    ]);
    let (baseline, control) = (measurements[0], measurements[1]);
    let (frame_trace, disabled_debug) = (measurements[2], measurements[3]);

    let noise_floor = (control - baseline).abs();
    let frame_trace_overhead = frame_trace - baseline;
    let disabled_debug_overhead = disabled_debug - baseline;

    eprintln!(
        "iterations per round: {ITERATIONS}, rounds: {ROUNDS}, \
         frame-tracing feature: {}\n\
         baseline                  {baseline:.3} ns/iteration\n\
         baseline (repeated)       {control:.3} ns/iteration  \
         (noise floor {noise_floor:.3})\n\
         trace_frame!              {frame_trace:.3} ns/iteration  \
         (overhead {frame_trace_overhead:+.3})\n\
         disabled tracing::debug!  {disabled_debug:.3} ns/iteration  \
         (overhead {disabled_debug_overhead:+.3})",
        clipped_logging::FRAME_TRACING
    );

    // On a machine slower than this test's author's, every figure here scales
    // together, so both tolerances have a floor that scales with the baseline.
    let disabled_debug_tolerance = DISABLED_DEBUG_TOLERANCE_NS.max(baseline);

    // With the feature on, `trace_frame!` is an ordinary `TRACE` event that
    // the subscriber rejects at runtime, so it costs what a disabled `debug!`
    // costs and is held to that bound instead. That build exists to be run by
    // hand during a debugging session, not to be shipped.
    let frame_trace_tolerance = if clipped_logging::FRAME_TRACING {
        disabled_debug_tolerance
    } else {
        noise_floor + FRAME_TRACE_TOLERANCE_NS.max(baseline * FRAME_TRACE_TOLERANCE_FRACTION)
    };
    assert!(
        frame_trace_overhead < frame_trace_tolerance,
        "trace_frame! cost {frame_trace_overhead:.3} ns/iteration, above the \
         {frame_trace_tolerance:.3} ns tolerance, against a {noise_floor:.3} \
         ns noise floor"
    );

    assert!(
        disabled_debug_overhead < disabled_debug_tolerance,
        "a disabled tracing::debug! cost {disabled_debug_overhead:.3} \
         ns/iteration, above the {disabled_debug_tolerance:.3} ns tolerance, \
         against a {noise_floor:.3} ns noise floor"
    );
}
