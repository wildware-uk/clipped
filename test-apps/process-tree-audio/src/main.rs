//! A test subject whose child process is what plays the tone.
//!
//! See the library documentation (`src/lib.rs`) for what this is for and the
//! protocol it speaks. In short:
//!
//! ```text
//! process-tree-audio                    # the parent: silent, waits for "spawn"
//! process-tree-audio --play             # a player: renders a tone at once
//! process-tree-audio --play --frequency 1373 --seconds 10
//! ```
//!
//! A parent starts a player as its own child when it is sent `spawn` on
//! standard input, which is how "the game spawned a helper that makes the
//! noise" happens at a moment a test chooses. Closing standard input ends the
//! parent, and the parent ends its child on the way out — on every path,
//! including a panic, because a test application left rendering a tone on a
//! shared machine is somebody's afternoon.

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use clap::Parser;
use clipped_process_tree_audio::{tone, AMPLITUDE, FREQUENCY};

/// A controlled subject for process-scoped audio capture tests.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Options {
    /// Render the tone in this process, rather than waiting to start a child
    /// that does.
    #[arg(long)]
    play: bool,

    /// The tone to play, in hertz.
    #[arg(long, default_value_t = FREQUENCY)]
    frequency: f32,

    /// The peak amplitude to play it at, as a fraction of full scale.
    #[arg(long, default_value_t = AMPLITUDE)]
    amplitude: f32,

    /// Stop after this many seconds. Without it, the run ends when its standard
    /// input closes.
    #[arg(long)]
    seconds: Option<f64>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse();
    let limit = options.seconds.map(Duration::from_secs_f64);

    if options.play {
        return play(&options, limit);
    }
    parent(&options, limit)
}

/// The parent: plays nothing, and starts a player when it is asked to.
fn parent(options: &Options, limit: Option<Duration>) -> Result<(), Box<dyn std::error::Error>> {
    announce(&format!("ready pid={} role=parent", std::process::id()))?;

    let mut child = None;
    for line in commands(limit) {
        match line.trim() {
            "spawn" => {
                if child.is_none() {
                    child = Some(spawn_player(options)?);
                }
            }
            "stop" => break,
            // A blank line is somebody pressing return, not a command.
            "" => {}
            other => announce(&format!("ignored command={other}"))?,
        }
    }

    if let Some(mut child) = child {
        // The child has no reason to outlive its parent, and a test that
        // panicked is exactly when one would.
        let _ = child.kill();
        let _ = child.wait();
    }
    announce("stopped")
}

/// Starts a player as this process's own child and reports what it said.
fn spawn_player(options: &Options) -> Result<Child, Box<dyn std::error::Error>> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--play")
        .args(["--frequency", &options.frequency.to_string()])
        .args(["--amplitude", &options.amplitude.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    // The child's first line is its own `ready` or `unavailable`, and it is
    // passed straight through: a test that asked for a tone has to be able to
    // tell "playing" from "this machine cannot", and the child is the only one
    // that knows.
    let stdout = child.stdout.take().expect("standard output was piped");
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    announce(&format!("child pid={} {}", child.id(), first.trim()))?;

    // Whatever it says afterwards is drained, because a child whose standard
    // output nobody reads eventually blocks trying to write to it.
    std::thread::spawn(move || {
        for line in reader.lines() {
            if line.is_err() {
                break;
            }
        }
    });

    Ok(child)
}

/// The player: renders the tone until it is told to stop.
fn play(options: &Options, limit: Option<Duration>) -> Result<(), Box<dyn std::error::Error>> {
    let running = Arc::new(AtomicBool::new(true));
    // Standard input is the stop signal, and reading it has to happen off the
    // rendering thread: a player that stopped feeding the endpoint while it
    // waited for a line would produce a gap in the tone rather than a tone.
    std::thread::spawn({
        let running = Arc::clone(&running);
        move || {
            let mut line = String::new();
            let mut input = BufReader::new(std::io::stdin());
            loop {
                line.clear();
                match input.read_line(&mut line) {
                    // End of file: the test has gone, and so should this.
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.trim() == "stop" => break,
                    Ok(_) => {}
                }
            }
            running.store(false, Ordering::Relaxed);
        }
    });

    let frequency = options.frequency;
    let played = tone::play(
        frequency,
        options.amplitude,
        limit,
        &running,
        |rate, channels| {
            let _ = announce(&format!(
                "ready pid={} role=player frequency={frequency} amplitude={} \
                 rate={rate} channels={channels}",
                std::process::id(),
                options.amplitude
            ));
        },
    );

    match played {
        Ok(played) => announce(&format!("stopped frames={}", played.frames)),
        Err(reason) => {
            // Not a failure of this program: a machine with no output device
            // cannot play a tone, and the test that started it skips rather
            // than waiting for a sound that is never coming (AGENTS.md
            // section 25).
            announce(&format!("unavailable reason={reason}"))?;
            Ok(())
        }
    }
}

/// Writes one protocol line and flushes it.
///
/// Flushed every time because the reader on the other end is a test waiting for
/// this exact line before it does anything, and a line sitting in a buffer is a
/// test that times out.
fn announce(line: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}

/// The commands arriving on standard input, ending at end of file or after
/// `limit`.
///
/// `limit` is a stop for a run nobody is driving — someone trying the
/// application by hand — and is enforced by a thread that ends the process,
/// because there is no way to interrupt a blocking read of standard input.
fn commands(limit: Option<Duration>) -> impl Iterator<Item = String> {
    if let Some(limit) = limit {
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            let _ = announce("stopped");
            std::process::exit(0);
        });
    }
    BufReader::new(std::io::stdin())
        .lines()
        .map_while(Result::ok)
}
