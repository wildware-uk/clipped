//! `fullscreen-dx11`: the video pattern, covering a whole display.
//!
//! # Why this is a separate binary
//!
//! Exclusive fullscreen is the one presentation
//! [issue #12](https://github.com/wildware-uk/clipped/issues/12) could not
//! verify capture against, because there was nothing to point a capture at that
//! would take a display the way a game does. This is that subject.
//!
//! It is a second binary rather than a flag on `video-pattern` because taking a
//! display away from whoever is using it is not something a mistyped argument
//! should be able to do. Running this is a decision: it changes the display
//! mode, it hides whatever was on that display, and if it exits badly the
//! desktop is left rearranged. Everything else — the window, the swap chain,
//! the pattern, the run loop, the protocol — is the same code as
//! `video-pattern`, from `clipped-video-pattern`.
//!
//! # What Windows may refuse
//!
//! `SetFullscreenState` only succeeds for a window Windows is willing to give
//! the foreground to, and it will not give the foreground to a process the user
//! has not interacted with — which is most of the time when a test starts it.
//! The application says which it got in its `ready` line
//! (`exclusive=yes` or `exclusive=no`) and carries on either way, as a
//! borderless window covering the display. A test reads that field rather than
//! assuming (AGENTS.md section 16).
//!
//! # Giving the display back
//!
//! Every way of stopping — the deadline, standard input closing, Ctrl-C, the
//! window being closed — leaves exclusive fullscreen before the swap chain is
//! released, which is what puts the display back the way it was. That is why
//! the Ctrl-C handler in `clipped-video-pattern` exists.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use clipped_video_pattern::{MonitorChoice, Options, Presentation};

/// A test application that covers a display with a deterministic pattern.
#[derive(Debug, Parser)]
#[command(
    name = "fullscreen-dx11",
    about = "Covers a display with a deterministic test pattern, exclusively where Windows allows",
    long_about = "Presents the same machine-verifiable pattern as `video-pattern`, but over a \
                  whole display: either as a borderless window covering it, or - where Windows \
                  grants the foreground - by taking the display exclusively through DXGI, the \
                  way a game in fullscreen mode does.\n\n\
                  Prints one `ready` line naming its window handle, its exact pixel size and \
                  whether the display was actually granted exclusively, then renders until its \
                  deadline, until standard input closes, until Ctrl-C, or until the window is \
                  closed. The display is always given back before the process exits."
)]
struct Arguments {
    /// Frames to present per second.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=1000))]
    fps: u32,

    /// How long to render for. Zero renders until stopped.
    ///
    /// The default is deliberately short. A display held exclusively is a
    /// display nobody else can use.
    #[arg(long, default_value_t = 30)]
    seconds: u64,

    /// Whether to ask for the display exclusively, or merely cover it.
    #[arg(long, value_enum, default_value_t = Mode::Exclusive)]
    mode: Mode,

    /// Which display to cover.
    ///
    /// `auto` prefers a display that is not the primary one, so that a run does
    /// not take the desktop away from whoever is at the keyboard.
    #[arg(long, value_enum, default_value_t = Monitor::Auto)]
    monitor: Monitor,

    /// Keep rendering when standard input closes.
    #[arg(long)]
    ignore_stdin: bool,
}

/// How the display is taken.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    /// Ask DXGI for the display itself, as a game in fullscreen mode does.
    Exclusive,
    /// Cover the display with a borderless window, as a game in "fullscreen
    /// (windowed)" mode does.
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

fn main() -> ExitCode {
    let arguments = Arguments::parse();

    let options = Options {
        fps: arguments.fps,
        duration: (arguments.seconds > 0).then(|| Duration::from_secs(arguments.seconds)),
        presentation: match arguments.mode {
            Mode::Exclusive => Presentation::FullscreenExclusive,
            Mode::Borderless => Presentation::FullscreenBorderless,
        },
        monitor: match arguments.monitor {
            Monitor::Auto => MonitorChoice::Auto,
            Monitor::Primary => MonitorChoice::Primary,
        },
        // Ignored by a fullscreen presentation, which takes the display's size.
        size: Options::default().size,
        stop_on_stdin_end: !arguments.ignore_stdin,
        // This application takes over a display; it does not also take over the
        // speakers. The sounded subject is `video-pattern --tone`, which is
        // what the absolute A/V offset measurement runs (docs/av-sync.md).
        tone: false,
    };

    match clipped_video_pattern::run(options) {
        Ok(summary) => {
            eprintln!(
                "[info] presented {} frames {}, stopped because of the {}",
                summary.frames,
                if summary.exclusive {
                    "with the display held exclusively"
                } else {
                    "in a window covering the display"
                },
                summary.reason
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("fullscreen-dx11 failed: {error}");
            ExitCode::FAILURE
        }
    }
}
