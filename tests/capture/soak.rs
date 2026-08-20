//! Hours of recording, and whether the recorder is holding more at the end.
//!
//! [Issue #105](https://github.com/wildware-uk/clipped/issues/105), and
//! AGENTS.md sections 58 and 59. Many faults appear only after hours, and the
//! recorder is expected to stay resident for days — so what this asks is not
//! "did it record" but **"is it holding more than it was"**.
//!
//! # Why repeated recordings rather than one long one
//!
//! Two reasons, and the second is the one that matters.
//!
//! **Disk.** A recording is written at the encoder's bitrate for as long as it
//! runs — around 8 GB an hour at 1440p60 — so a three-hour single recording
//! needs 24 GB of somebody's drive, and the machine this was written on had
//! 63 GB free on the volume `%TEMP%` is. A soak that fills a disk has measured
//! the disk. Each cycle here deletes its own file, so the space used is flat
//! whatever the duration.
//!
//! **Where leaks live.** A single long recording exercises the writer and
//! almost nothing else: one capture session opened, one encoder opened, one
//! muxer opened. Starting and stopping is where a texture, a thread or a
//! handle goes unreleased, and repeating it is what finds that. #598's scratch
//! directories and #443's encoder bridge were both found by counting across
//! repetitions rather than by looking at one run.
//!
//! # What it does not do yet
//!
//! #105's scope also names repeated replay saves, repeated game launches,
//! audio device changes and a large library. This covers the recording cycle
//! only. The others need a running `watch`, a way to change the default
//! endpoint without disturbing whoever owns the machine, and a library to
//! build — each worth its own pass, and none of them made easier by being
//! bolted onto this one.
//!
//! # Running it
//!
//! ```text
//! # The default: a few minutes, enough to see the shape.
//! cargo test -p clipped-video-pattern --test soak -- --ignored --nocapture
//!
//! # A real soak. Hours, and the figure this issue wants.
//! CLIPPED_SOAK_MINUTES=180 cargo test -p clipped-video-pattern --test soak -- \
//!     --ignored --nocapture
//! ```

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::path::PathBuf;
use std::time::Instant;

use clipped_session::{
    record_into, CaptureTargetSettings, RecordingOutputs, RecordingSettings, StopSignal,
};
use clipped_test_exclusion::{Exclusive, Resource};
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::resources::Held;

/// The rate the subject presents at, and the rate each recording asks for.
const FPS: u32 = 60;

/// How long each recording in the cycle runs.
///
/// Short, because the cycle is the subject: what is being repeated is opening
/// a capture session, an encoder and a muxer and then closing all three. A
/// longer recording per cycle buys more encoder time and fewer of the
/// transitions this is looking at.
const RECORD_FOR: Duration = Duration::from_secs(6);

/// How long to run for when nobody says.
///
/// Minutes rather than hours, so that the test is runnable as part of the
/// hardware sweep and still shows the shape. `CLIPPED_SOAK_MINUTES` is how the
/// real soak is asked for, and the report says which of the two this was.
const DEFAULT_MINUTES: u64 = 4;

/// The environment variable that lengthens the run.
const SOAK_MINUTES: &str = "CLIPPED_SOAK_MINUTES";

/// How long the subject is given to put its window up and say so.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Cycles ignored at the start before the baseline is taken.
///
/// The first recordings on a process are not representative: FFmpeg resolves
/// and loads, the encoder's driver initialises, allocators reach a working
/// size. Measuring from cycle zero would report all of that as a leak, which
/// is how a soak comes to fail on a healthy build.
const WARM_UP_CYCLES: u32 = 3;

/// Where the run stops being allowed to grow, as a fraction of the measured
/// cycles.
///
/// **The statistic this test turns on, and the one an earlier version got
/// wrong.** A recording pipeline acquires things once and keeps them —
/// allocator arenas, a thread pool, driver caches — so a healthy process
/// climbs for a while and then stops. Dividing total growth by the number of
/// cycles calls that climb a rate, and reports a plateau as a leak.
///
/// Measured over 119 cycles: the handle count rose to +38 by cycle 30 and then
/// did not move again — +38, +38, +38, +38, +36, +38, +36, +38 at cycles 40
/// through 110. A four-minute run of 37 cycles saw only the climb, divided by
/// 37, and reported "1.03 handles per recording", which read as a leak and was
/// the first thirty cycles of a process settling.
///
/// So the question is not how much it grew. It is **whether it is still
/// growing**, which is what the last third answers.
const SETTLED_AFTER: f64 = 2.0 / 3.0;

/// The most committed memory a run may gain in total, however many cycles it
/// ran for.
///
/// **A cap rather than a rate, and that is the point.** What separates a leak
/// from a process settling is whether the growth *scales with the work*: a leak
/// of any size reaches any cap if the run is long enough, and a one-off step
/// does not move however long you wait.
///
/// The one-off step here is real and was measured twice: committed memory sits
/// near 99 MB for about sixty cycles, steps once to about 109 MB, and holds
/// there. Over 60 cycles that was +11.0 MB and over 119 cycles +10.0 MB — the
/// same step, not a rate, which is exactly what a cap admits and a per-cycle
/// threshold cannot.
///
/// So the sensitivity of this test comes from **the length of the run**, not
/// from the tightness of the number: at 32 MB, a leak of 512 KB per recording
/// is caught within about sixty cycles, and one of 32 KB per recording within a
/// thousand — which is what `CLIPPED_SOAK_MINUTES=180` is for.
const TOTAL_PRIVATE_BYTES: f64 = 32.0 * 1024.0 * 1024.0;

/// The most handles a run may gain in total, however many cycles it ran for.
///
/// The same reasoning and a much smaller number, because handles do not drift:
/// a process that closes what it opens returns to the same count. Measured, the
/// count rises to +38 by cycle 30 and then does not move again — +38 at cycle
/// 40 and +38 at cycle 110, with the settled part of a run moving by nothing at
/// all. 96 is that step with room, and a leak of one handle per recording
/// reaches it inside a hundred cycles.
const TOTAL_HANDLES: i64 = 96;

/// A stop signal a scope can raise.
#[derive(Debug, Default)]
struct Flag(AtomicBool);

impl Flag {
    fn raise(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl StopSignal for Flag {
    fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A directory of this test's own.
fn scratch() -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "clipped-soak-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
    directory
}

/// How long the run was asked for.
fn asked_for() -> (Duration, bool) {
    match std::env::var(SOAK_MINUTES).ok().and_then(|value| {
        value
            .parse::<u64>()
            .ok()
            .filter(|minutes| *minutes > 0 && *minutes < 24 * 60)
    }) {
        Some(minutes) => (Duration::from_secs(minutes * 60), true),
        None => (Duration::from_secs(DEFAULT_MINUTES * 60), false),
    }
}

#[test]
#[ignore = "records for minutes or hours through a real encoder; see the module documentation"]
fn recording_over_and_over_does_not_leave_the_process_holding_more() {
    // One capture measurement at a time (issue #194). A soak is the longest
    // holder of it in the repository, which is deliberate: a soak sharing the
    // machine measures the sharing.
    let _measuring = Exclusive::acquire(Resource::CaptureMeasurement)
        .unwrap_or_else(|contended| panic!("{contended}"));

    let (soak_for, was_asked) = asked_for();

    let app = TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--mode",
            "borderless",
            "--fps",
            &FPS.to_string(),
            // Beyond any soak this test will run, so the subject outlives the
            // recordings rather than the other way round.
            "--seconds",
            "86400",
        ],
        READY_TIMEOUT,
    )
    .expect("the video pattern application should start and announce itself");

    let (width, height) = app.client_size();
    let target = CaptureTargetSettings::window(app.window() as u64, width, height);
    let directory = scratch();

    let started = Instant::now();
    let mut cycles = 0u32;
    let mut frames = 0u64;
    let mut baseline: Option<Held> = None;
    let mut settled: Option<(u32, Held)> = None;
    let mut latest = Held::now();

    println!("\n=== soak: {} minutes ===", soak_for.as_secs() / 60);
    println!("before any recording: {latest}");

    while started.elapsed() < soak_for {
        // The same path a real recording takes, and a file that is deleted
        // immediately: what is being measured is what the process kept, not
        // what it wrote.
        let output = directory.join("cycle.mkv");
        let settings = RecordingSettings::new(target, output.clone())
            .with_framerate(FPS)
            .with_overwrite(true);

        let stop = Flag::default();
        let report = std::thread::scope(|scope| {
            let recorder =
                scope.spawn(|| record_into(&settings, &stop, &RecordingOutputs::default()));
            std::thread::sleep(RECORD_FOR);
            stop.raise();
            recorder
                .join()
                .expect("the recording thread does not panic")
                .expect("a window that is drawing can be recorded on this machine")
        });

        frames += report.frames_encoded();
        let _ = std::fs::remove_file(&output);
        cycles += 1;
        latest = Held::now();

        // Where the run is expected to have stopped climbing. Taken by elapsed
        // time rather than by a cycle count, because the number of cycles is
        // not known until the clock runs out.
        if settled.is_none()
            && baseline.is_some()
            && started.elapsed().as_secs_f64() >= soak_for.as_secs_f64() * SETTLED_AFTER
        {
            settled = Some((cycles, latest));
            println!("settled reading at cycle {cycles}: {latest}");
        }

        if cycles == WARM_UP_CYCLES {
            baseline = Some(latest);
            println!("baseline after {WARM_UP_CYCLES} warm-up cycles: {latest}");
        } else if cycles % 10 == 0 {
            let elapsed = started.elapsed().as_secs_f64() / 60.0;
            match baseline {
                Some(base) => println!(
                    "cycle {cycles:>4} ({elapsed:>5.1} min): {latest} | since baseline: {}",
                    latest.since(&base)
                ),
                None => println!("cycle {cycles:>4} ({elapsed:>5.1} min): {latest}"),
            }
        }
    }

    let Some(base) = baseline else {
        panic!(
            "the soak ran {cycles} cycle(s), which is fewer than the {WARM_UP_CYCLES} it warms \
             up for, so nothing was measured. Ask for longer with {SOAK_MINUTES}"
        );
    };

    let measured = cycles - WARM_UP_CYCLES;
    let growth = latest.since(&base);
    let minutes = started.elapsed().as_secs_f64() / 60.0;

    println!(
        "\n=== soak result ===\n\
         asked for          : {}\n\
         ran for            : {minutes:.1} min\n\
         cycles             : {cycles} ({measured} measured, {WARM_UP_CYCLES} warm-up)\n\
         frames encoded     : {frames}\n\
         at the start       : {base}\n\
         at the end         : {latest}\n\
         growth             : {growth}\n\
         per measured cycle : private {:+.1} KB, handles {:+.2}\n",
        if was_asked {
            format!("{SOAK_MINUTES}={}", soak_for.as_secs() / 60)
        } else {
            format!("the {DEFAULT_MINUTES}-minute default")
        },
        growth.private_bytes() as f64 / measured.max(1) as f64 / 1024.0,
        growth.handles() as f64 / f64::from(measured.max(1)),
    );

    assert!(
        measured >= 10,
        "only {measured} cycle(s) were measured, which is too few to say anything about a trend;          ask for longer with {SOAK_MINUTES}"
    );

    let Some((settled_at, settled_held)) = settled else {
        panic!(
            "the run never reached its settled point, so there is nothing to compare the end              against; ask for longer with {SOAK_MINUTES}"
        );
    };

    // The question the whole test turns on: is it *still* growing? Total growth
    // includes the climb every process makes while it settles, and dividing
    // that by the cycle count reports a plateau as a leak — which is exactly
    // what an earlier version of this test did.
    let after_settling = latest.since(&settled_held);
    let settled_cycles = cycles - settled_at;

    println!(
        "settled at cycle {settled_at}, and over the {settled_cycles} cycle(s) since: {}",
        after_settling
    );

    assert!(
        settled_cycles >= 5,
        "only {settled_cycles} cycle(s) ran after the settled point, which is too few to say          whether anything is still growing; ask for longer with {SOAK_MINUTES}"
    );

    assert!(
        (growth.private_bytes() as f64) <= TOTAL_PRIVATE_BYTES,
        "committed memory grew by {:.1} MB across {measured} recordings, past the {:.0} MB a run          may gain however long it is. A cap is the test rather than a rate because a leak scales          with the work and a process settling does not — so this is growth that kept going.          Since the settled reading at cycle {settled_at}: {after_settling}",
        growth.private_bytes() as f64 / (1024.0 * 1024.0),
        TOTAL_PRIVATE_BYTES / (1024.0 * 1024.0)
    );

    assert!(
        growth.handles() <= TOTAL_HANDLES,
        "the process gained {} handles across {measured} recordings, past the {TOTAL_HANDLES} a          run may gain however long it is — and handles do not drift the way memory does. Since          the settled reading at cycle {settled_at}: {after_settling}",
        growth.handles()
    );

    let _ = std::fs::remove_dir_all(&directory);
}
