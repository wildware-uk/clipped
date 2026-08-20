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
//! The whole measurement is repeated up to [`ATTEMPTS`] times and the *lowest*
//! overhead is the one asserted on, which is the same argument as keeping the
//! fastest round, one level up. Keeping the fastest round protects against a
//! scheduling event landing inside one round; it cannot protect against a host
//! that is contended for the whole of one measurement, which is what a shared
//! CI runner does. That produced a failure on `main`
//! ([issue #631](https://github.com/wildware-uk/clipped/issues/631)): a
//! disabled `debug!` measured at 14.589 ns against a 9.931 ns bound, when five
//! runs on an idle machine put it at 2.543 to 2.593 ns and the noise floor of
//! that CI run was 1.787 ns against a local 0.18. An attempt is only ever made
//! slower by the machine, so the lowest of several is the honest estimate and a
//! regression still shows in every one of them.
//!
//! That was not enough on its own, and this file said so before it happened:
//! the same failure recurred at 15.699 ns against 10.145
//! ([issue #703](https://github.com/wildware-uk/clipped/issues/703)), on a pull
//! request that changed only TypeScript, and the same commit passed when it was
//! re-run. Three attempts of a whole measurement cannot help when the host is
//! contended across all three. The bound on a disabled `debug!` now also scales
//! with the measured noise floor, which is the one number that says how well
//! this machine could measure anything just now; the arithmetic and the three
//! datasets behind it are at the assertion.
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

/// How many times the whole measurement may be repeated before it is believed.
///
/// Keeping the fastest of [`ROUNDS`] handles a scheduling event inside a round.
/// It does nothing about a host contended for the whole of one loop's rounds,
/// which is what a shared runner does and what failed `main` in
/// [issue #631](https://github.com/wildware-uk/clipped/issues/631). Repeating
/// the measurement and keeping the lowest overhead is the same reasoning
/// applied to the thing above a round.
///
/// Three, because one attempt costs under a second and the failure being
/// guarded against is transient: a run contended through all three attempts is
/// a machine that could not have measured anything, and a regression is present
/// in every attempt so the lowest still shows it.
const ATTEMPTS: usize = 3;

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

/// How many times the measured noise floor a disabled `debug!` may cost, when
/// that is more than [`DISABLED_DEBUG_TOLERANCE_NS`] and more than the baseline.
///
/// The noise floor is two identical loops measured against each other, so it is
/// this run's own answer to "how well could this machine measure anything just
/// now". Scaling by it is what lets one bound serve a quiet desktop and a
/// hosted runner without being chosen for either — the arithmetic, and the
/// three datasets behind the number, are at the assertion.
const DISABLED_DEBUG_NOISE_MULTIPLE: f64 = 15.0;

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

    // The lowest overhead of several whole measurements, for the reason the
    // module documentation gives: a contended host inflates an entire attempt,
    // which keeping the fastest round cannot undo. Each attempt is scored by the
    // disabled `debug!` overhead, because that is the tighter of the two
    // assertions and the one issue #631 failed on — and the attempt that wins
    // brings its own baseline, control and `trace_frame!` figures with it, so
    // every number asserted on comes from one self-consistent measurement
    // rather than from the best of each column taken separately.
    let mut best: Option<Vec<f64>> = None;
    for _ in 0..ATTEMPTS {
        let attempt = nanoseconds_per_iteration(&[
            without_logging,
            without_logging_again,
            with_frame_trace,
            with_disabled_debug,
        ]);
        let overhead = attempt[3] - attempt[0];
        let better = match &best {
            Some(kept) => overhead < kept[3] - kept[0],
            None => true,
        };
        if better {
            best = Some(attempt);
        }
    }
    let measurements = best.expect("ATTEMPTS is not zero, so an attempt was kept");

    let (baseline, control) = (measurements[0], measurements[1]);
    let (frame_trace, disabled_debug) = (measurements[2], measurements[3]);

    let noise_floor = (control - baseline).abs();
    let frame_trace_overhead = frame_trace - baseline;
    let disabled_debug_overhead = disabled_debug - baseline;

    eprintln!(
        "iterations per round: {ITERATIONS}, rounds: {ROUNDS}, \
         attempts: {ATTEMPTS}, frame-tracing feature: {}\n\
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
    //
    // And a third floor that scales with the *noise*, which is what a contended
    // runner actually moves (issue #703). Measured, on this test's own output:
    //
    //                   baseline    noise    overhead   ovh/base   ovh/noise
    //   idle, 5 runs       3.768    0.179       2.650      0.703       14.80
    //   CI (#631)          9.931    1.787      14.589      1.469        8.16
    //   CI (#703)         10.145    2.021      15.699      1.547        7.77
    //
    // The overhead is a *smaller* multiple of the noise floor when the host is
    // contended, not a larger one, so a bound that scales with the noise grows
    // faster than the thing it is bounding. That is the direction a tolerance
    // wants to be wrong in.
    //
    // Fifteen because it gives the same headroom - about 1.9x - on an idle
    // machine and on both CI runs that failed, so no configuration is being
    // privileged. On a quiet machine the noise term is 2.7 ns and the constant
    // still binds; it is only on a runner that cannot measure that this opens
    // up, which is exactly when it should.
    //
    // What this gives up: on a contended runner the bound is around 30 ns, so a
    // regression smaller than that would not be caught *there*. It is caught on
    // any machine quiet enough to measure, and the regressions this exists for -
    // an eagerly evaluated argument, a lock, an allocation - are larger than
    // that anyway. A bound tight enough to catch 5 ns on a contended host is a
    // bound that fails when nothing is wrong, which is what these two issues
    // are.
    let disabled_debug_tolerance = DISABLED_DEBUG_TOLERANCE_NS
        .max(baseline)
        .max(noise_floor * DISABLED_DEBUG_NOISE_MULTIPLE);

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
