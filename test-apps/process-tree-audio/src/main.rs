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
//! noise" happens at a moment a test chooses. `spawn 1699` names the tone,
//! and a parent takes as many of them as it is sent: "the game spawned a
//! *second* helper, an hour in, and it plays something else" is the same
//! sentence with a frequency in it, and is the only way a test can tell the
//! joiner's audio from the audio that was already there. Closing standard
//! input ends the parent, and the parent ends its children on the way out — on
//! every path, including a panic, because a test application left rendering a
//! tone on a shared machine is somebody's afternoon.

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use clap::Parser;
use clipped_video_pattern::child_output::{self, Lines, NoLine};
use clipped_video_pattern::steady_tone::{self, AMPLITUDE, FREQUENCY};

/// How long a parent waits for a player it started to say what it is doing.
///
/// Bounded for the reason the harness's waits are: what a player does before it
/// can announce anything is open a shared-mode audio endpoint, and a parent
/// blocked for ever on a player that never came back would be a parent that
/// stops answering its own standard input — a test hanging on a subject that is
/// still running, which is the worst of both.
///
/// Shorter than the ten seconds every test gives `spawn_child`, so that a
/// player which cannot report is reported *by its parent*, with a reason,
/// rather than by the test merely running out of patience.
const PLAYER_PATIENCE: Duration = Duration::from_secs(8);

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

    // Each child is kept with the lines it is printing: dropping the receiver
    // would end the thread draining its standard output, and a child whose
    // output nobody reads eventually blocks trying to write to it.
    let mut children: Vec<(Child, Lines)> = Vec::new();
    for line in commands(limit) {
        let command = line.trim();
        if command == "stop" {
            break;
        }
        // A blank line is somebody pressing return, not a command.
        if command.is_empty() {
            continue;
        }
        match spawn_request(command, options.frequency) {
            Some(frequency) => children.push(spawn_player(options, frequency)?),
            None => announce(&format!("ignored command={command}"))?,
        }
    }

    for (mut child, _lines) in children {
        // A child has no reason to outlive its parent, and a test that
        // panicked is exactly when one would. The wait is after the kill, which
        // is what keeps it bounded: it reaps a process that is already being
        // terminated rather than waiting on one that is running.
        let _ = child.kill();
        let _ = child.wait();
    }
    announce("stopped")
}

/// The frequency a `spawn` command asks for, or [`None`] if this is not one.
///
/// `spawn` on its own means the frequency this run was started with, which is
/// what a test asking for one child wants and is the only form that existed
/// when there could only be one. `spawn 1699` names a frequency, and is how a
/// test gets a **second** child playing something the first is not: a process
/// that joined the tree while a capture was running cannot be told from the
/// sibling that was already playing unless the two make different sounds
/// (`tests/mid_recording_joiner.rs`).
fn spawn_request(command: &str, default: f32) -> Option<f32> {
    let rest = command.strip_prefix("spawn")?;
    if rest.is_empty() {
        return Some(default);
    }
    // A command word ends at whitespace: `spawner` is not `spawn`, and reading
    // it as one would start a player nobody asked for.
    let argument = rest.strip_prefix(char::is_whitespace)?.trim();
    if argument.is_empty() {
        return Some(default);
    }
    argument.parse().ok()
}

/// Starts a player as this process's own child and reports what it said.
fn spawn_player(
    options: &Options,
    frequency: f32,
) -> Result<(Child, Lines), Box<dyn std::error::Error>> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--play")
        .args(["--frequency", &frequency.to_string()])
        .args(["--amplitude", &options.amplitude.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    // Drained on a thread from here on, so that a child which keeps talking
    // never blocks writing to a pipe nobody is reading.
    let stdout = child.stdout.take().expect("standard output was piped");
    let lines = child_output::reading(stdout)?;

    // The child's first line is its own `ready` or `unavailable`, and it is
    // passed straight through: a test that asked for a tone has to be able to
    // tell "playing" from "this machine cannot", and the child is the only one
    // that knows. A child that says neither within `PLAYER_PATIENCE` is
    // reported as unavailable in the same shape and killed, because a wedged
    // player is not a reason to stop answering the test.
    match child_output::next_line_within(&lines, PLAYER_PATIENCE) {
        Ok(first) => announce(&format!("child pid={} {first}", child.id()))?,
        Err(reason) => {
            let detail = match reason {
                NoLine::Silent(patience) => format!(
                    "the player said nothing within {:.1}s",
                    patience.as_secs_f64()
                ),
                NoLine::Ended => "the player exited without saying anything".to_owned(),
                NoLine::Unreadable(error) => {
                    format!("the player'''s output could not be read: {error}")
                }
            };
            let pid = child.id();
            let _ = child.kill();
            let _ = child.wait();
            announce(&format!("child pid={pid} unavailable reason={detail}"))?;
        }
    }

    Ok((child, lines))
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
    let played = steady_tone::play(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_spawn_asks_for_the_frequency_this_run_was_started_with() {
        // The form every test used before there could be more than one child,
        // and the one `harness::ToneSubject::spawn_child` still sends.
        assert_eq!(spawn_request("spawn", 997.0), Some(997.0));
    }

    #[test]
    fn a_spawn_with_a_frequency_asks_for_that_one() {
        // The whole of "a process joined the tree playing something else": a
        // joiner that sounded like its sibling would be invisible in the
        // measurement.
        assert_eq!(spawn_request("spawn 1699", 997.0), Some(1699.0));
        assert_eq!(spawn_request("spawn 1699.5", 997.0), Some(1699.5));
    }

    #[test]
    fn a_command_that_merely_begins_with_spawn_is_not_one() {
        // A command word ends at whitespace. Matching on the prefix alone would
        // have `spawner` start a player, which is a tone nobody asked for
        // playing on somebody's machine.
        assert_eq!(spawn_request("spawner", 997.0), None);
        assert_eq!(spawn_request("spawn-two", 997.0), None);
        assert_eq!(spawn_request("respawn", 997.0), None);
    }

    #[test]
    fn a_spawn_whose_frequency_is_not_a_number_is_refused_rather_than_guessed_at() {
        // Refused, not defaulted: a test that mistyped a frequency should see
        // `ignored command=` rather than a child playing the wrong tone, which
        // would fail an isolation assertion for a reason that has nothing to do
        // with the capture.
        assert_eq!(spawn_request("spawn hello", 997.0), None);
        assert_eq!(spawn_request("spawn 1699 1373", 997.0), None);
    }
}
