//! `video-pattern`: a window that renders a deterministic, verifiable pattern.
//!
//! Run it by hand to have something to point a capture at, or start it from a
//! test through [`clipped_video_pattern::harness::TestApp`]. `docs/testing.md`
//! is the guide.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use clipped_video_pattern::{MonitorChoice, Options, Presentation};

/// A window rendering a deterministic test pattern for capture tests.
#[derive(Debug, Parser)]
#[command(
    name = "video-pattern",
    about = "Renders a deterministic, machine-verifiable video pattern for capture tests",
    long_about = "Renders a window whose every frame carries its own frame counter, a \
                  checksum, a background colour and a marker position, so that a captured \
                  frame can be identified and checked programmatically rather than by eye.\n\n\
                  Prints one `ready` line naming its window handle and exact pixel size, then \
                  renders until its deadline, until standard input closes, until Ctrl-C, or \
                  until the window is closed."
)]
struct Arguments {
    /// Frames to present per second.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=1000))]
    fps: u32,

    /// How long to render for. Zero renders until stopped.
    #[arg(long, default_value_t = 60)]
    seconds: u64,

    /// Client size, as WIDTHxHEIGHT physical pixels.
    #[arg(long, default_value = "1280x720", value_parser = parse_size)]
    size: (u32, u32),

    /// How the window is presented.
    #[arg(long, value_enum, default_value_t = Mode::Borderless)]
    mode: Mode,

    /// Keep rendering when standard input closes.
    ///
    /// Standard input closing normally stops the application, which is what
    /// stops a test application outliving the test that started it. Pass this
    /// for a run started by a script that has no standard input to give.
    #[arg(long)]
    ignore_stdin: bool,

    /// Which display to render on.
    ///
    /// `auto` prefers a display that is not the primary one, so that the window
    /// is not sitting on top of whoever is at the keyboard.
    #[arg(long, value_enum, default_value_t = Monitor::Auto)]
    monitor: Monitor,
}

/// The presentations this binary offers.
///
/// Fullscreen is deliberately absent: taking over a display is what
/// `fullscreen-dx11` is for, and a test application that could do it by
/// accident — through a mistyped flag on a machine somebody is working at — is
/// not one this project wants.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    /// An ordinary window, with a title bar and a border around the pattern.
    Windowed,
    /// A borderless window whose pixels are exactly the pattern.
    Borderless,
}

/// Which display to use.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Monitor {
    /// A display that is not the primary one, when there is one.
    Auto,
    /// The primary display.
    Primary,
}

fn parse_size(value: &str) -> Result<(u32, u32), String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got `{value}`"))?;
    let width = width
        .parse()
        .map_err(|_| format!("`{width}` is not a width"))?;
    let height = height
        .parse()
        .map_err(|_| format!("`{height}` is not a height"))?;
    Ok((width, height))
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();

    let options = Options {
        fps: arguments.fps,
        duration: (arguments.seconds > 0).then(|| Duration::from_secs(arguments.seconds)),
        presentation: match arguments.mode {
            Mode::Windowed => Presentation::Windowed,
            Mode::Borderless => Presentation::Borderless,
        },
        monitor: match arguments.monitor {
            Monitor::Auto => MonitorChoice::Auto,
            Monitor::Primary => MonitorChoice::Primary,
        },
        size: arguments.size,
        stop_on_stdin_end: !arguments.ignore_stdin,
    };

    match clipped_video_pattern::run(options) {
        Ok(summary) => {
            eprintln!(
                "[info] presented {} frames, stopped because of the {}",
                summary.frames, summary.reason
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("video-pattern failed: {error}");
            ExitCode::FAILURE
        }
    }
}
