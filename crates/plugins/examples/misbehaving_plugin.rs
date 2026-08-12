//! A plugin that goes wrong on purpose, so that the isolation rules have a real
//! subject to be tested against.
//!
//! Issue #69's first acceptance criterion is that a plugin which panics, hangs
//! or floods events cannot stall or stop a recording, **tested**. A fake that
//! merely pretends to hang would only prove that the pretence was ignored; this
//! is a real process, doing the real thing, read over a real pipe, and killed
//! by the real supervisor.
//!
//! # How it is told what to do
//!
//! By the name it was installed under. A plugin's manifest names the executable
//! in its own directory (`crate::manifest`), so a test copies this binary in as
//! `hang.exe`, `flood.exe` or `crash.exe` and the behaviour follows the name.
//! The alternative — a field in the manifest carrying arguments — would be a
//! permanent hole in the contract for the sake of a test fixture.
//!
//! | Installed as | What it does |
//! | --- | --- |
//! | `…crash…` | Says hello, then panics |
//! | `…hang…` | Says hello, then never says anything again |
//! | `…flood…` | Says hello, then reports events as fast as the pipe will take them |
//! | `…garbage…` | Says hello, then prints things that are not reports |
//! | `…newer…` | Claims a contract version this build does not speak |
//! | `…quiet…` | Says nothing at all, ever, including hello |
//!
//! Each mode has a safety valve: none of them runs for more than a minute, so a
//! test that fails to kill one does not leave it behind on a shared machine.

use std::io::{self, Write};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use clipped_events::EventKind;
use clipped_plugins::{hello, write_report, PluginReport, ReportedEvent};
use serde_json::Map;

/// Nothing here outlives this, whatever the host does or fails to do.
const SAFETY_VALVE: Duration = Duration::from_secs(60);

/// The most events the flood produces before giving up on being stopped.
const FLOOD_LIMIT: usize = 200_000;

fn main() {
    let installed_as = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        })
        .unwrap_or_default();

    let mut output = io::stdout().lock();
    if !installed_as.contains("quiet") {
        let contract = if installed_as.contains("newer") {
            // A version this build does not speak, which the host has to notice
            // from the running executable and not only from the manifest.
            r#"{"report":"hello","contract":99}"#.to_owned() + "\n"
        } else {
            write_report(&hello())
        };
        let _ = output.write_all(contract.as_bytes());
        let _ = output.flush();
    }

    if installed_as.contains("crash") {
        panic!("this plugin is deliberately broken");
    }

    let started = Instant::now();

    if installed_as.contains("flood") {
        for _ in 0..FLOOD_LIMIT {
            let event = ReportedEvent {
                kind: EventKind::Kill,
                ago_ns: 0,
                precision_ns: 0,
                confidence: 1.0,
                data: Map::new(),
            };
            if output
                .write_all(write_report(&PluginReport::Event(event)).as_bytes())
                .is_err()
            {
                break;
            }
            if started.elapsed() > SAFETY_VALVE {
                break;
            }
        }
        process::exit(0);
    }

    if installed_as.contains("garbage") {
        while started.elapsed() < SAFETY_VALVE {
            if writeln!(output, "this is not a report, and never will be").is_err() {
                break;
            }
            let _ = output.flush();
            thread::sleep(Duration::from_millis(1));
        }
        process::exit(0);
    }

    // `hang`, `quiet` and `newer` all sit here: the first two so that the host
    // has to notice silence, and the third so that it has to notice the version
    // rather than waiting for one.
    while started.elapsed() < SAFETY_VALVE {
        thread::sleep(Duration::from_millis(50));
    }
}
