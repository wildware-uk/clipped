//! What tracking a game's process tree costs, measured rather than assumed.
//!
//! ```text
//! cargo run --release -p clipped-windows --example process_tree_probe -- 60
//! cargo run --release -p clipped-windows --example process_tree_probe -- 60 250
//! ```
//!
//! The first argument is how long to run for, in seconds; the second is the
//! rescan interval in milliseconds, which defaults to
//! [`ProcessTree::DEFAULT_RESCAN_INTERVAL`]. A run reports how long a scan
//! takes, what a call that the rate limiter turns away costs, and how much
//! processor time the whole run spent — the numbers `docs/audio-routing.md`
//! records (AGENTS.md sections 18 and 19).
//!
//! The subject is a chain of three processes the probe starts itself, so the
//! tree gains and loses members during the run rather than being one process
//! that never changes, and the measurement needs no game installed.
//!
//! Read the processor time as the *whole* cost of tracking: this crate spends
//! nothing anywhere else. Unlike the process watcher's WMI subscription, which
//! does its work inside a service and never appears in Clipped's own process
//! (`docs/game-detection.md`), a scan is a call this process makes on its own
//! thread and pays for itself.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clipped_windows::ProcessTree;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// How many rate-limited calls the second measurement makes.
const TURNED_AWAY_CALLS: u32 = 1_000_000;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let seconds: u64 = arguments
        .next()
        .unwrap_or_else(|| "60".to_owned())
        .parse()
        .expect("the first argument is a number of seconds");
    let interval = arguments
        .next()
        .map_or(ProcessTree::DEFAULT_RESCAN_INTERVAL, |milliseconds| {
            Duration::from_millis(
                milliseconds
                    .parse()
                    .expect("the second argument is a rescan interval in milliseconds"),
            )
        });

    let mut chain = Command::new("cmd.exe")
        .args(["/c", "set /p go= & cmd.exe /c more"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("cmd.exe is on every Windows installation");
    let mut input = chain.stdin.take().expect("standard input was piped");

    let mut tree = ProcessTree::rooted_at(chain.id())
        .expect("a process this probe started can be opened")
        .with_rescan_interval(interval);

    println!(
        "tracking process {} for {seconds}s, rescanning every {} ms",
        chain.id(),
        interval.as_millis()
    );

    // A tree that never changed would measure the cheapest possible case, so
    // the chain grows two processes half way through the run.
    let started = Instant::now();
    let grow_at = started + Duration::from_secs(seconds / 2);
    let mut grown = false;

    let cpu_before = processor_time();
    let mut scans: Vec<Duration> = Vec::new();
    let mut joined = 0_usize;
    let mut exited = 0_usize;

    while started.elapsed() < Duration::from_secs(seconds) {
        if !grown && Instant::now() >= grow_at {
            input.write_all(b"go\r\n").expect("the chain is still open");
            input.flush().expect("the chain is still open");
            grown = true;
        }

        // One millisecond more than the interval, so that every call here is a
        // scan: a sleep never returns early, so the rate limiter can never turn
        // one of these away and every duration below is a real scan.
        std::thread::sleep(interval + Duration::from_millis(1));

        let before = Instant::now();
        let change = tree
            .refresh()
            .expect("the process table can always be read");
        scans.push(before.elapsed());

        joined += change.joined().len();
        exited += change.exited().len();
        if !change.refused().is_empty() {
            println!("refused: {:?}", change.refused());
        }
    }
    let cpu = processor_time() - cpu_before;
    let elapsed = started.elapsed();

    // What a caller pays for asking more often than the interval allows, which
    // is what an audio thread calling `refresh` on every packet would be doing.
    let mut idle = tree.with_rescan_interval(Duration::from_secs(3_600));
    let before = Instant::now();
    for _ in 0..TURNED_AWAY_CALLS {
        idle.refresh().expect("no scan, so nothing to fail");
    }
    let turned_away = before.elapsed() / TURNED_AWAY_CALLS;

    drop(input);
    let _ = chain.kill();
    let _ = chain.wait();

    scans.sort_unstable();
    let median = scans.get(scans.len() / 2).copied().unwrap_or_default();

    println!("ran for {:.1}s", elapsed.as_secs_f64());
    println!("scans: {}", scans.len());
    println!("members joined: {joined}, exited: {exited}");
    println!(
        "scan: min {:.3} ms, median {:.3} ms, max {:.3} ms",
        milliseconds(scans.first().copied()),
        median.as_secs_f64() * 1_000.0,
        milliseconds(scans.last().copied()),
    );
    println!(
        "a call the rate limiter turns away: {} ns",
        turned_away.as_nanos()
    );
    println!(
        "processor time: {:.1} ms over {:.1}s, or {:.4}% of one core",
        cpu.as_secs_f64() * 1_000.0,
        elapsed.as_secs_f64(),
        cpu.as_secs_f64() / elapsed.as_secs_f64() * 100.0,
    );
}

/// A duration in milliseconds, or zero if there was none.
fn milliseconds(value: Option<Duration>) -> f64 {
    value.unwrap_or_default().as_secs_f64() * 1_000.0
}

/// How much processor time this process has used, kernel and user together.
fn processor_time() -> Duration {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();

    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid
    // and needs no closing, and all four out parameters are live `FILETIME`s
    // for the duration of the call.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    }
    .expect("a process can always ask about itself");

    // File times count 100-nanosecond ticks; these two are elapsed processor
    // time rather than a moment.
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}

/// A `FILETIME` as the number it is.
fn ticks(value: FILETIME) -> u64 {
    u64::from(value.dwHighDateTime) << 32 | u64::from(value.dwLowDateTime)
}
