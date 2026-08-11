//! Measures what the process watcher costs and how fast it is.
//!
//! The two numbers `docs/game-detection.md` quotes come from here, and this
//! exists so that they can be taken again on a different machine rather than
//! trusted (AGENTS.md section 19).
//!
//! ```text
//! cargo run -p clipped-game-detection --example process_watch_probe -- latency 20
//! cargo run -p clipped-game-detection --example process_watch_probe -- idle 300
//! cargo run -p clipped-game-detection --example process_watch_probe -- start 20
//! ```
//!
//! `latency` starts a `cmd.exe` it can end when it chooses, and times the gap
//! between the process existing and the watcher saying so, in both directions.
//! `idle` runs the watcher with nothing happening and reports the processor
//! time this process used over the interval. `start` times the one-off cost of
//! building a watcher, which is the baseline snapshot — one `OpenProcess` per
//! process on the machine — and the subscription together.
//!
//! What `idle` measures is *this* process. The WMI service does the polling
//! this design moved out of here, and its cost lands in `WmiPrvSE.exe` and in
//! the `Winmgmt` service host; measuring those needs a performance counter and
//! is described in `docs/game-detection.md` rather than done here, so that this
//! program does not quietly report someone else's work as its own.

#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use clipped_game_detection::{Next, ProcessWatcher, WatchConfig, WatchEvent};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// How long the probe waits for one event before giving up on it.
const PATIENCE: Duration = Duration::from_secs(30);

fn main() {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments.next().unwrap_or_else(|| "latency".to_owned());
    let count: u32 = arguments
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10);

    // The notification interval is the one setting worth sweeping: it is what
    // trades detection latency against the work the WMI service does, and the
    // table in `docs/game-detection.md` was taken by running this at several
    // values.
    let config = match arguments.next().and_then(|value| value.parse::<u64>().ok()) {
        Some(seconds) => WatchConfig {
            notification_interval: Duration::from_secs(seconds),
            ..WatchConfig::default()
        },
        None => WatchConfig::default(),
    };

    match mode.as_str() {
        "latency" => latency(count, config),
        "idle" => idle(count, config),
        "start" => start_cost(count, config),
        other => {
            eprintln!(
                "unknown mode {other}; expected `latency <runs>`, `idle <seconds>` \
                 or `start <runs>`"
            );
            std::process::exit(2);
        }
    }
}

/// Times building a watcher: the baseline snapshot and the subscription.
///
/// The baseline opens every process on the machine to ask what it is running,
/// which is the most expensive thing the watcher ever does — once. It is paid
/// while the application is starting anyway, and this is what says whether that
/// is a fair thing to claim (AGENTS.md section 7).
fn start_cost(runs: u32, config: WatchConfig) {
    let mut timings = Vec::new();
    let mut processes = 0;

    for run in 1..=runs {
        let began = Instant::now();
        let watcher = ProcessWatcher::start(config).expect("the watcher starts");
        let elapsed = began.elapsed();

        processes = watcher.already_running().len();
        let resolved = watcher
            .already_running()
            .iter()
            .filter(|process| process.image_path.is_some())
            .count();
        // Dropping the watcher waits for its source threads, which takes a
        // notification interval and is not what is being timed, so the clock is
        // read before it goes.
        drop(watcher);

        println!("run {run:>3}: start {elapsed:>12.3?}  ({processes} processes, {resolved} paths)");
        timings.push(elapsed);
    }

    println!("processes in the baseline: {processes}");
    report("start", &mut timings);
}

/// Times detection of a process starting and of the same process exiting.
fn latency(runs: u32, config: WatchConfig) {
    let mut watcher = ProcessWatcher::start(config).expect("the watcher starts");
    describe(&watcher, config);

    let mut launches = Vec::new();
    let mut exits = Vec::new();

    for run in 1..=runs {
        let subject = start_subject();
        let pid = subject.id();
        let started = Instant::now();
        wait_for(&mut watcher, |event| match event {
            WatchEvent::Launched(launch) => {
                launch.processes.iter().any(|process| process.pid == pid)
            }
            _ => false,
        });
        let launch = started.elapsed();

        end_subject(subject);
        let ended = Instant::now();
        wait_for(&mut watcher, |event| match event {
            WatchEvent::Exited(exit) => exit.process.pid == pid,
            _ => false,
        });
        let exit = ended.elapsed();

        println!("run {run:>3}: launch {launch:>12.3?}  exit {exit:>12.3?}");
        launches.push(launch);
        exits.push(exit);
    }

    report("launch", &mut launches);
    report("exit", &mut exits);
}

/// Reports what the watcher costs while nothing at all is happening.
fn idle(seconds: u32, config: WatchConfig) {
    let mut watcher = ProcessWatcher::start(config).expect("the watcher starts");
    describe(&watcher, config);

    let duration = Duration::from_secs(u64::from(seconds));
    let before = processor_time();
    let start = Instant::now();
    let mut events = 0_u32;

    while start.elapsed() < duration {
        match watcher.next_event(Duration::from_secs(1)) {
            Next::Event(_) => events += 1,
            Next::Idle => {}
            Next::Finished => {
                eprintln!("the watcher lost every source it had; the measurement is void");
                std::process::exit(1);
            }
        }
    }

    let used = processor_time().saturating_sub(before);
    let elapsed = start.elapsed();
    #[allow(
        clippy::cast_precision_loss,
        reason = "milliseconds against seconds: the ratio is what matters"
    )]
    let share = used.as_secs_f64() / elapsed.as_secs_f64() * 100.0;

    println!("idle for {elapsed:.1?}");
    println!("  events seen:    {events}");
    println!("  processor time: {:.1?}", used);
    println!("  one core:       {share:.4}%");
}

/// Prints the smallest, median and largest of a set of measurements.
fn report(name: &str, measurements: &mut [Duration]) {
    if measurements.is_empty() {
        return;
    }
    measurements.sort_unstable();

    let median = measurements[measurements.len() / 2];
    let total: Duration = measurements.iter().sum();
    println!(
        "{name}: n={} min {:.3?} median {:.3?} max {:.3?} mean {:.3?}",
        measurements.len(),
        measurements[0],
        median,
        measurements[measurements.len() - 1],
        total / u32::try_from(measurements.len()).unwrap_or(1),
    );
}

/// Says what is being measured, so a pasted result is interpretable.
fn describe(watcher: &ProcessWatcher, config: WatchConfig) {
    println!("source:               {}", watcher.source());
    if let Some(declined) = watcher.declined_source() {
        println!("preferred source:     declined ({declined})");
    }
    println!(
        "notification interval {:?}, quiet period {:?}, exit settle {:?}",
        config.notification_interval, config.launch_quiet_period, config.exit_settle_period
    );
    println!("already running:      {}", watcher.already_running().len());
}

/// Waits for an event the predicate accepts.
fn wait_for(watcher: &mut ProcessWatcher, mut accepts: impl FnMut(&WatchEvent) -> bool) {
    let start = Instant::now();
    while start.elapsed() < PATIENCE {
        let event = match watcher.next_event(Duration::from_millis(100)) {
            Next::Event(event) => event,
            Next::Idle => continue,
            Next::Finished => panic!("the watcher had already finished"),
        };
        if let WatchEvent::Stopped(reason) = &event {
            panic!("the watcher lost every source it had: {reason}");
        }
        if accepts(&event) {
            return;
        }
    }
    panic!("nothing matching arrived within {PATIENCE:?}");
}

/// Starts a process this probe can end when it chooses.
fn start_subject() -> Child {
    Command::new("cmd.exe")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("cmd.exe is present on every Windows machine")
}

/// Ends the subject and waits for Windows to agree that it has gone.
fn end_subject(mut subject: Child) {
    drop(subject.stdin.take());
    subject
        .wait()
        .expect("the subject exits when its input closes");
}

/// Processor time this process has used, kernel and user together.
fn processor_time() -> Duration {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();

    // SAFETY: the process handle is the current-process pseudo-handle, which is
    // always valid and is not owned; all four out parameters are live locals of
    // the type the signature names.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    }
    .expect("a process can always read its own times");

    hundred_nanoseconds(kernel) + hundred_nanoseconds(user)
}

/// A `FILETIME` interval as a duration. Its unit is 100 nanoseconds.
fn hundred_nanoseconds(time: FILETIME) -> Duration {
    let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    Duration::from_nanos(ticks.saturating_mul(100))
}
