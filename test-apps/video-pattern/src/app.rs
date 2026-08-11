//! Running the test application: what it is asked for, what it announces, and
//! how it is stopped.
//!
//! # The contract with whatever launched it
//!
//! The application is meant to be driven by a test as often as by a person, so
//! everything a driver needs is on standard output, one line at a time, flushed
//! as it is written:
//!
//! ```text
//! ready hwnd=0x00000000000a06f2 client=1280x720 fps=30 presentation=borderless \
//!     exclusive=no monitor=\\.\DISPLAY1 tone=off
//! stopped frames=901 reason=deadline
//! ```
//!
//! A driver waits for the `ready` line, captures `hwnd`, and knows the exact
//! pixel size of the pattern it is about to look for. Nothing else is printed to
//! standard output; warnings and diagnostics go to standard error, so a driver
//! parsing the protocol cannot be derailed by one.
//!
//! A `--tone` run says so in the `ready` line and then prints one line per tone
//! as it presents the frame that tone belongs to:
//!
//! ```text
//! ready hwnd=0x… client=1280x720 fps=30 presentation=borderless exclusive=no \
//!     monitor=\\.\DISPLAY1 tone=yes tone-hz=997 tone-ms=30 tone-first=60 tone-every=150
//! tone index=0 frame=60 onset=31415926535 present=31415928111 skew=1576
//! ```
//!
//! `onset` is where the endpoint's own clock puts the tone's half-amplitude
//! point and `present` is the performance counter immediately after the frame
//! was handed to the compositor — both in nanoseconds on the same counter a
//! capture backend stamps frames with. `skew` is `present − onset`: how far
//! apart the two halves of the event were **at the source**, which is what a
//! measurement of the recording has to subtract before it can call what is left
//! the recorder's. It is not assumed to be zero, because a scheduler does not
//! promise that.
//!
//! A tone with no moment says which of the two reasons it has: `onset=none` is
//! one the render thread refused to place and did not play, and `onset=pending`
//! is one it had not reported by the time the frame went to the compositor —
//! probably played, but with nothing to measure it from. They are counted
//! separately because one is a missing sound and the other is a missing report.
//!
//! # Stopping, and never being left behind
//!
//! Three things end a run, and all of them end it the same way — the loop
//! stops, [`crate::render::PatternWindow`]'s `Drop` gives back the display and
//! the window, and the process exits zero:
//!
//! - the `--seconds` deadline, which is the backstop that means a forgotten
//!   process eventually goes away on its own;
//! - standard input closing or carrying the word `stop`, which is how a test
//!   asks for a clean stop and, more importantly, what happens automatically if
//!   the test process dies — Windows does not kill a child when its parent
//!   goes, and a test application left rendering on somebody's second monitor
//!   is exactly the mess this project's testing rules exist to prevent. A run
//!   started by a script that has no standard input to give would end
//!   immediately, so [`Options::stop_on_stdin_end`] (`--ignore-stdin`) turns
//!   that signal off;
//! - Ctrl-C, or the window being closed, for a person running it by hand.
//!
//! The Ctrl-C handler is why there is a process-wide flag here rather than only
//! an owned one: Windows calls the handler on a thread of its own with no
//! context argument, so the flag it sets has to be reachable from a function
//! pointer. It is an `AtomicBool` and nothing else shares it (AGENTS.md
//! section 20).

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use windows::core::BOOL;
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT};

use crate::pattern::{self, PatternError};
use crate::render::{handle_value, PatternWindow, Placement};
use crate::tone::{Emitted, ToneOutput};

/// Set by the console control handler; read by the run loop.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// How many seconds after the run starts the first tone sounds.
///
/// Long enough for whatever is recording to have found the window and settled,
/// short enough that a thirty-second run still gets several tones.
const TONE_LEAD_SECONDS: u32 = 2;

/// How many frames before the one it belongs to a tone is asked for.
///
/// It has to be more than the render thread keeps queued in front of the
/// endpoint — sixty milliseconds — or the moment asked for would already have
/// been played past and the tone would be reported missed. Six frames is two
/// hundred milliseconds at 30 fps, which clears that comfortably while keeping
/// the prediction of when the frame will be presented short enough to be worth
/// making: the error in it is only whatever the loop's pacing does over six
/// frames, and it is measured and announced rather than assumed away.
const REQUEST_LEAD_FRAMES: u32 = 6;

/// How many seconds between one tone and the next.
///
/// Five seconds is far enough apart that a detector looking for one cannot
/// confuse it with its neighbour at any offset worth measuring, and close enough
/// together that a ninety-second run yields seventeen independent readings
/// rather than one.
const TONE_INTERVAL_SECONDS: u32 = 5;

/// How the window is put on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    /// An ordinary window with a title bar and a border. A capture of it
    /// includes both, which is why a decoder has to find the pattern rather
    /// than assume it starts at the top-left corner of the frame.
    Windowed,
    /// A borderless window of the requested size. A capture of it is exactly
    /// the pattern, which is what makes it the mode automated tests use.
    Borderless,
    /// A borderless window covering a whole display: what a game means by
    /// "fullscreen (windowed)", and still composed by the desktop compositor.
    FullscreenBorderless,
    /// A window covering a whole display that then asks DXGI for the display
    /// itself, through `SetFullscreenState`: what a game means by "fullscreen
    /// (exclusive)". Windows can refuse, and the application says which it got.
    FullscreenExclusive,
}

impl Presentation {
    /// The word this presentation is named by on the command line and in the
    /// `ready` line.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::Borderless => "borderless",
            Self::FullscreenBorderless => "fullscreen-borderless",
            Self::FullscreenExclusive => "fullscreen-exclusive",
        }
    }

    /// Whether this presentation covers a whole display.
    const fn is_fullscreen(self) -> bool {
        matches!(self, Self::FullscreenBorderless | Self::FullscreenExclusive)
    }
}

impl fmt::Display for Presentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Which display to put the window on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorChoice {
    /// The first display that is not the primary one, falling back to the
    /// primary when there is only one.
    ///
    /// The default, and the reason for it is not cosmetic: the person running a
    /// capture test is usually working on their primary display, an
    /// always-on-top test window over their work gets minimised, and a
    /// minimised window stops being composed — so the run stops measuring
    /// capture and starts measuring Alt-Tab.
    #[default]
    Auto,
    /// The primary display, whatever else is attached. For running by hand on a
    /// machine with one monitor, or for deliberately testing the primary one.
    Primary,
}

/// What a run was asked for.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Frames presented per second.
    pub fps: u32,
    /// How long to run for. [`None`] runs until stopped.
    pub duration: Option<Duration>,
    /// How the window is presented.
    pub presentation: Presentation,
    /// Which display it goes on.
    pub monitor: MonitorChoice,
    /// The client size in physical pixels. Ignored by the fullscreen
    /// presentations, which take the display's size.
    pub size: (u32, u32),
    /// Whether standard input closing stops the run.
    ///
    /// True by default, and that default is the safety property: a test that
    /// dies without killing its child leaves a pipe closed behind it, and the
    /// application notices and goes. Set it false for a run started by a script
    /// that has no standard input to give — the run then ends at its deadline,
    /// at Ctrl-C, or when the window is closed.
    pub stop_on_stdin_end: bool,
    /// Whether to play a short tone at the moment a named frame is presented.
    ///
    /// False by default, and deliberately so: this application is started by
    /// tests that have nothing to do with sound, and a test suite that makes a
    /// noise nobody asked for is one somebody turns off. See [`crate::tone`]
    /// for what the tone is and what its announced moment means.
    pub tone: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fps: 30,
            duration: Some(Duration::from_secs(60)),
            presentation: Presentation::Borderless,
            monitor: MonitorChoice::Auto,
            // Comfortably above the pattern's minimum, a common capture size,
            // and small enough to leave a second monitor usable behind it.
            size: (1280, 720),
            stop_on_stdin_end: true,
            tone: false,
        }
    }
}

/// Why a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The `--seconds` deadline was reached.
    Deadline,
    /// Standard input closed, or carried `stop`.
    Requested,
    /// Ctrl-C, or Ctrl-Break.
    Interrupted,
    /// The window was closed.
    WindowClosed,
}

impl StopReason {
    /// The word printed in the `stopped` line.
    const fn label(self) -> &'static str {
        match self {
            Self::Deadline => "deadline",
            Self::Requested => "stop-requested",
            Self::Interrupted => "interrupted",
            Self::WindowClosed => "window-closed",
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// What a run did.
#[derive(Debug, Clone, Copy)]
pub struct Summary {
    /// Frames presented, which is also the counter drawn into the last one plus
    /// one.
    pub frames: u32,
    /// Why it stopped.
    pub reason: StopReason,
    /// Whether the display was held exclusively.
    pub exclusive: bool,
}

/// Runs the test application until something stops it.
///
/// # Errors
///
/// [`AppError`] if the window, the graphics device or a frame could not be
/// made. A test application that cannot draw what it promised has to fail
/// loudly: a capture test would otherwise report an empty capture as a capture
/// defect (AGENTS.md section 54).
pub(crate) fn run(options: Options) -> Result<Summary, AppError> {
    if let Err(error) = clipped_windows::enable_per_monitor_dpi_awareness() {
        // Not fatal, and worth saying: on a scaled display a DPI-unaware
        // process is shown a fiction, so the pattern would be drawn at one size
        // and captured at another, and every cell edge would land between two
        // pixels.
        eprintln!("[warn] could not become per-monitor DPI aware: {error}");
    }

    let (placement, monitor) = placement_for(options)?;
    let mut window = PatternWindow::open(placement)?;

    if options.presentation == Presentation::FullscreenExclusive {
        // Whether Windows granted it is reported in the `ready` line below,
        // through `window.is_exclusive()`. A refusal is a legitimate outcome —
        // Windows does not give the foreground to a process the user has not
        // interacted with — and the application carries on covering the
        // display as a borderless window.
        let _granted = window.take_display_exclusively()?;
    }

    let stop = install_stop_signals(options.stop_on_stdin_end);

    // Opened before the `ready` line, because that line has to say whether this
    // run will actually make a sound: a driver measuring an offset against a
    // tone needs to know there will not be one rather than wait for it.
    let sound = options.tone.then(ToneOutput::start).and_then(|started| {
        started
            .inspect_err(|reason| eprintln!("[warn] this run will be silent: {reason}"))
            .ok()
    });

    let interval = Duration::from_secs(1) / options.fps.max(1);
    let mut tones = Tones::planned(sound.is_some(), options.fps, window.presented());

    let (width, height) = window.size();
    announce(&format!(
        "ready hwnd=0x{:016x} client={}x{} fps={} presentation={} exclusive={} monitor={} {}",
        handle_value(window.handle()),
        width,
        height,
        options.fps,
        options.presentation,
        if window.is_exclusive() { "yes" } else { "no" },
        monitor,
        tones.announcement(options.tone),
    ));

    let started = Instant::now();
    let mut next_present = Instant::now();

    let reason = loop {
        if window.pump_messages() {
            break StopReason::WindowClosed;
        }
        if INTERRUPTED.load(Ordering::Relaxed) {
            break StopReason::Interrupted;
        }
        if stop.load(Ordering::Relaxed) {
            break StopReason::Requested;
        }
        if options
            .duration
            .is_some_and(|duration| started.elapsed() >= duration)
        {
            break StopReason::Deadline;
        }

        // The counter is the window's, not this loop's: a fullscreen run
        // presents warm-up frames before the loop starts, and they are frames
        // the application presented and a capture can see. Counting them here
        // is what keeps `Summary::frames` the count of every frame presented
        // and the last counter drawn plus one.
        window.present_next()?;
        // Immediately after the present, because this is the video half of the
        // event a tone is the audio half of, and every microsecond between the
        // two readings is an error in the source moment being announced.
        tones.presented(sound.as_ref(), window.presented(), interval);

        // Paced against a fixed schedule rather than by sleeping for an
        // interval after each frame, so that the time spent drawing does not
        // accumulate into a drift a capture test would read as the source
        // running slow.
        next_present += interval;
        let now = Instant::now();
        if next_present > now {
            thread::sleep(next_present - now);
        } else {
            next_present = now;
            thread::yield_now();
        }
    };

    let exclusive = window.is_exclusive();
    let frames = window.presented();
    // Explicit, before the summary is printed: the `stopped` line means the
    // display has been given back and the window has gone, which is what lets a
    // test treat the line as permission to stop worrying about this process.
    drop(window);

    announce(&format!("stopped frames={frames} reason={reason}"));

    Ok(Summary {
        frames,
        reason,
        exclusive,
    })
}

/// Which frames carry a tone, and the announcements that go with them.
///
/// The plan is arithmetic on the frame counter rather than a list of moments,
/// so that a driver reading the `ready` line knows every frame a tone belongs
/// to in advance and can decode exactly those frames out of its capture. A
/// silent run has no plan and this does nothing.
#[derive(Debug)]
struct Tones {
    /// The counter of the frame the first tone belongs to, or [`None`] when
    /// this run is silent.
    first_frame: Option<u32>,
    /// Frames from one tone's frame to the next.
    every_frames: u32,
    /// What the render thread reported about the tones it placed, accumulated
    /// because a tone is reported when it is *placed* — one queue length before
    /// it sounds — rather than when it is heard.
    emitted: Vec<Emitted>,
}

impl Tones {
    /// Decides which frames carry a tone, given the frame the run loop is about
    /// to present.
    fn planned(sounding: bool, fps: u32, base_frame: u32) -> Self {
        let per_second = fps.max(1);
        let every_frames = (TONE_INTERVAL_SECONDS * per_second).max(1);
        let lead_frames = TONE_LEAD_SECONDS * per_second;

        Self {
            first_frame: sounding.then(|| base_frame.saturating_add(lead_frames)),
            every_frames,
            emitted: Vec::new(),
        }
    }

    /// The `ready` line's tone fields.
    ///
    /// Three states rather than two, because "this run was not asked for a
    /// tone" and "this run was asked for one and this machine cannot play it"
    /// are different things to a driver: the first is what every other test
    /// gets, and the second is a reason to skip a measurement rather than to
    /// wait for a sound that is never coming (AGENTS.md section 16).
    fn announcement(&self, requested: bool) -> String {
        match self.first_frame {
            Some(first) => format!(
                "tone=yes tone-hz={} tone-ms={} tone-first={first} tone-every={}",
                crate::tone::FREQUENCY,
                crate::tone::LENGTH.as_millis(),
                self.every_frames,
            ),
            None if requested => "tone=no".to_owned(),
            None => "tone=off".to_owned(),
        }
    }

    /// Deals with the frame just presented: asks for a tone that is coming, and
    /// announces one that has just sounded.
    ///
    /// `presented` is the window's count of frames presented, so the frame that
    /// has just gone to the compositor carries the counter one below it.
    ///
    /// The request happens [`REQUEST_LEAD_FRAMES`] before the frame it is for,
    /// against the moment that frame is *about to* be presented rather than
    /// against a schedule laid down at the start of the run. That is the whole
    /// difference between a tone that stays with its frame and one that drifts
    /// away from it: this loop paces itself against a fixed schedule and gives
    /// up on it whenever it falls behind — a busy compositor, a debug build, a
    /// machine somebody is using — and a tone anchored at the start would still
    /// be following the schedule the frames had abandoned.
    fn presented(&mut self, sound: Option<&ToneOutput>, presented: u32, interval: Duration) {
        let (Some(sound), Some(first), Some(frame)) =
            (sound, self.first_frame, presented.checked_sub(1))
        else {
            return;
        };
        let interval_nanos = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);

        // The frame this one is REQUEST_LEAD_FRAMES in front of, if that frame
        // carries a tone.
        let coming = frame.saturating_add(REQUEST_LEAD_FRAMES);
        if coming >= first && (coming - first) % self.every_frames == 0 {
            sound.request(
                crate::tone::now_nanos()
                    + u64::from(REQUEST_LEAD_FRAMES).saturating_mul(interval_nanos),
            );
        }

        if frame < first || (frame - first) % self.every_frames != 0 {
            return;
        }
        let present_nanos = crate::tone::now_nanos();

        let index = (frame - first) / self.every_frames;
        self.emitted.extend(sound.emitted());
        let placed = self.emitted.iter().find(|tone| tone.index == index);

        // Three outcomes rather than two, because "the render thread refused to
        // place this tone" and "the render thread has not said yet" are
        // different things to whoever is counting: the first is a sound that
        // was never made, and the second is a sound that probably was. Both are
        // said out loud rather than left out, because a driver counting tones
        // would otherwise be waiting for a line that is not coming.
        match placed.map(|tone| tone.midpoint_nanos) {
            Some(Some(onset)) => announce(&format!(
                "tone index={index} frame={frame} onset={onset} present={present_nanos} \
                 skew={}",
                present_nanos as i64 - onset as i64,
            )),
            Some(None) => announce(&format!(
                "tone index={index} frame={frame} onset=none present={present_nanos} skew=none"
            )),
            None => announce(&format!(
                "tone index={index} frame={frame} onset=pending present={present_nanos} skew=none"
            )),
        }
    }
}

/// Prints one line of the protocol, flushed.
///
/// Flushing matters: a driver blocks reading this line, and standard output to
/// a pipe is block-buffered, so an unflushed `ready` line is a test that waits
/// for the whole run before it starts.
fn announce(line: &str) {
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = writeln!(stdout, "{line}").and_then(|()| stdout.flush()) {
        // Not fatal and not silent: whoever was reading has gone, which the run
        // loop finds out for itself when standard input closes.
        eprintln!("[warn] could not write \"{line}\" to standard output: {error}");
    }
}

/// Where the window goes, and the display it is on, from the options and the
/// displays actually attached.
fn placement_for(options: Options) -> Result<(Placement, String), AppError> {
    let monitors = clipped_windows::enumerate_monitors().map_err(AppError::Monitors)?;

    let chosen = match options.monitor {
        MonitorChoice::Primary => monitors
            .iter()
            .find(|monitor| monitor.is_primary())
            .or_else(|| monitors.first()),
        MonitorChoice::Auto => monitors
            .iter()
            .find(|monitor| !monitor.is_primary())
            .or_else(|| monitors.first()),
    }
    .ok_or(AppError::NoDisplays)?;

    let bounds = chosen.bounds();
    let (x, y, width, height) = if options.presentation.is_fullscreen() {
        (
            bounds.left(),
            bounds.top(),
            bounds.size().width(),
            bounds.size().height(),
        )
    } else {
        // A small inset rather than the corner, so that the window is
        // obviously a window and does not sit under a taskbar.
        const INSET: i32 = 64;
        (
            bounds.left() + INSET,
            bounds.top() + INSET,
            options.size.0,
            options.size.1,
        )
    };

    if width < pattern::MIN_WIDTH || height < pattern::MIN_HEIGHT {
        return Err(AppError::PatternDoesNotFit { width, height });
    }

    Ok((
        Placement {
            x,
            y,
            width,
            height,
            presentation: options.presentation,
        },
        chosen.device_name().to_owned(),
    ))
}

/// Installs the console control handler and the standard-input watcher, and
/// returns the flag they set.
///
/// The watcher thread is detached deliberately. It blocks in a read on standard
/// input that cannot be cancelled, so joining it would mean a run that only
/// ends when somebody types something; the process exiting is what ends it, and
/// it owns nothing that needs releasing.
fn install_stop_signals(watch_stdin: bool) -> Arc<AtomicBool> {
    // SAFETY: `console_handler` is a plain `extern "system"` function with the
    // signature `PHANDLER_ROUTINE` names, and it touches only a `static`
    // atomic, so it is safe to call from the thread Windows raises it on.
    if let Err(error) = unsafe { SetConsoleCtrlHandler(Some(console_handler), true) } {
        eprintln!("[warn] Ctrl-C will not stop this cleanly: {error}");
    }

    let stop = Arc::new(AtomicBool::new(false));
    if !watch_stdin {
        return stop;
    }

    let watcher = Arc::clone(&stop);
    let spawned = thread::Builder::new()
        .name("stdin-stop".to_owned())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match stdin.lock().read_line(&mut line) {
                    // End of input: whoever launched this closed the pipe, or
                    // has gone entirely.
                    Ok(0) => break,
                    Ok(_) if line.trim().eq_ignore_ascii_case("stop") => break,
                    Ok(_) => {}
                    Err(error) => {
                        eprintln!("[warn] stopped watching standard input: {error}");
                        break;
                    }
                }
            }
            watcher.store(true, Ordering::Relaxed);
        });

    if let Err(error) = spawned {
        eprintln!(
            "[warn] no standard-input watcher ({error}); this run will only stop at its \
             deadline"
        );
    }

    stop
}

/// Windows' console control handler: records the interruption and reports it
/// handled, so the process is not torn down before the display is given back.
extern "system" fn console_handler(event: u32) -> BOOL {
    if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
        INTERRUPTED.store(true, Ordering::Relaxed);
        return true.into();
    }
    false.into()
}

/// Everything that can stop a test application from doing its job.
#[derive(Debug)]
#[non_exhaustive]
pub enum AppError {
    /// A Windows API call failed, named by the call.
    Windows {
        /// The API that failed.
        operation: &'static str,
        /// What Windows said.
        source: windows::core::Error,
    },
    /// The displays could not be enumerated, so there is nowhere to put a
    /// window.
    Monitors(clipped_windows::WindowsError),
    /// Windows reported no displays at all.
    NoDisplays,
    /// The surface is too small for a decodable pattern.
    PatternDoesNotFit {
        /// The width available in pixels.
        width: u32,
        /// The height available in pixels.
        height: u32,
    },
    /// The pattern could not be drawn into the surface Windows provided.
    Pattern(PatternError),
}

impl AppError {
    /// A failure from `operation`.
    pub(crate) const fn windows(operation: &'static str, source: windows::core::Error) -> Self {
        Self::Windows { operation, source }
    }
}

impl From<PatternError> for AppError {
    fn from(error: PatternError) -> Self {
        Self::Pattern(error)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows { operation, source } => {
                write!(formatter, "{operation} failed: {source}")
            }
            Self::Monitors(source) => write!(formatter, "could not list the displays: {source}"),
            Self::NoDisplays => formatter.write_str(
                "Windows reported no displays, so there is nowhere to show a test pattern",
            ),
            Self::PatternDoesNotFit { width, height } => write!(
                formatter,
                "the test pattern needs at least {}x{} pixels and {width}x{height} was \
                 available; ask for a bigger window with --size",
                pattern::MIN_WIDTH,
                pattern::MIN_HEIGHT
            ),
            Self::Pattern(source) => write!(formatter, "the frame could not be drawn: {source}"),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Windows { source, .. } => Some(source),
            Self::Monitors(source) => Some(source),
            Self::Pattern(source) => Some(source),
            Self::NoDisplays | Self::PatternDoesNotFit { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocol_words_are_the_ones_a_driver_parses() {
        // `harness::TestApp` and docs/testing.md both name these. Changing one
        // of them without the other is a test that hangs waiting for a line
        // that will never come, which is a worse hour than this test is.
        assert_eq!(Presentation::Windowed.label(), "windowed");
        assert_eq!(Presentation::Borderless.label(), "borderless");
        assert_eq!(
            Presentation::FullscreenBorderless.label(),
            "fullscreen-borderless"
        );
        assert_eq!(
            Presentation::FullscreenExclusive.label(),
            "fullscreen-exclusive"
        );
        assert_eq!(StopReason::Deadline.label(), "deadline");
        assert_eq!(StopReason::Requested.label(), "stop-requested");
        assert_eq!(StopReason::Interrupted.label(), "interrupted");
        assert_eq!(StopReason::WindowClosed.label(), "window-closed");
    }

    #[test]
    fn a_silent_run_announces_that_it_is_silent_and_plans_no_tones() {
        // The default every other capture test gets. A plan here would be a
        // sound on somebody's machine from a test that never asked for one.
        let tones = Tones::planned(false, 30, 0);
        assert_eq!(tones.first_frame, None);
        assert_eq!(tones.announcement(false), "tone=off");
        assert_eq!(
            tones.announcement(true),
            "tone=no",
            "a run that asked for a tone and cannot play one has to say which of the two \
             it is, or a driver waits for a sound that is never coming"
        );
    }

    #[test]
    fn the_tone_plan_is_arithmetic_a_driver_can_reproduce() {
        // The `ready` line is the whole protocol for this: a driver reads the
        // first frame and the interval and knows every frame that carries a
        // tone, so it can decode exactly those out of a capture rather than
        // decoding all of them.
        let tones = Tones::planned(true, 30, 0);
        assert_eq!(
            tones.announcement(true),
            "tone=yes tone-hz=997 tone-ms=30 tone-first=60 tone-every=150",
            "at 30 fps the first tone is the frame two seconds in and there is one every \
             five seconds after it"
        );

        // A run that has already presented warm-up frames counts from where it
        // is, not from zero, or the first tone would be announced for a frame
        // that has already gone.
        let after_warm_up = Tones::planned(true, 60, 10);
        assert_eq!(after_warm_up.first_frame, Some(10 + 2 * 60));
        assert_eq!(after_warm_up.every_frames, 5 * 60);
    }

    #[test]
    fn the_default_run_is_the_one_a_capture_test_wants() {
        let options = Options::default();
        assert_eq!(options.presentation, Presentation::Borderless);
        assert_eq!(options.monitor, MonitorChoice::Auto);
        assert!(
            !options.tone,
            "the application is silent unless a run asks for a tone: a test suite that \
             makes a noise nobody asked for is one somebody turns off"
        );
        assert!(options.size.0 >= pattern::MIN_WIDTH);
        assert!(options.size.1 >= pattern::MIN_HEIGHT);
        assert!(
            options.duration.is_some(),
            "a default run has to end on its own, or a forgotten test application \
             renders until somebody notices it"
        );
    }
}
