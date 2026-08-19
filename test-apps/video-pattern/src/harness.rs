//! Driving a test application from a test.
//!
//! A capture test needs a subject that is running, on screen, and identifiable
//! before it starts capturing, and needs it to be gone afterwards whether the
//! test passed, failed or panicked. That is fiddly enough to be worth writing
//! once: [`TestApp`] starts the process, waits for its `ready` line, exposes the
//! window handle and the exact pattern size, and kills the process on [`Drop`].
//!
//! ```no_run
//! use std::time::Duration;
//! use clipped_video_pattern::harness::TestApp;
//!
//! // In a test inside this package, write `env!("CARGO_BIN_EXE_video-pattern")`
//! // here: Cargo sets that to the binary it just built. It is spelt out as a
//! // path here only because a documentation example is compiled without it.
//! let app = TestApp::start(
//!     "target/debug/video-pattern.exe",
//!     ["--fps", "30", "--seconds", "30", "--mode", "borderless"],
//!     Duration::from_secs(20),
//! )?;
//!
//! // Capture app.window() here, and look for a pattern of app.client_size().
//!
//! app.stop(Duration::from_secs(5))?;
//! # Ok::<(), clipped_video_pattern::harness::HarnessError>(())
//! ```
//!
//! # Ownership
//!
//! [`TestApp`] owns the child process and its pipes. `Drop` closes standard
//! input — which is how the application is asked to stop cleanly — waits
//! briefly, and kills the process if it has not gone. There is no path, panic
//! included, on which the application outlives the test that started it
//! (AGENTS.md sections 25 and 58).

use core::fmt;
use core::time::Duration;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Instant;

use crate::child_output::{self, Lines, NoLine};

/// How long [`Drop`] gives the application to stop cleanly before killing it.
const DROP_GRACE: Duration = Duration::from_secs(3);

/// How long the reader thread is given to finish once the application has
/// exited.
///
/// It is reading a pipe whose only writer has gone, so this is the time for a
/// thread to be scheduled and to see end of file — microseconds on an idle
/// machine, and this much only because the machine running a capture test is
/// allowed to be busy.
const OUTPUT_DRAIN: Duration = Duration::from_secs(5);

/// A running test application.
#[derive(Debug)]
pub struct TestApp {
    child: Child,
    /// Every line the application prints after the `ready` one, as its reader
    /// thread receives them. Reading them all is not curiosity: an unread pipe
    /// makes the application's own writes fail, and its last line is the frame
    /// count a test cross-checks its capture against.
    lines: Lines,
    window: usize,
    client: (u32, u32),
    presentation: String,
    exclusive: bool,
    monitor: String,
    tone: Tone,
    steady_tone: SteadyTone,
    /// A `stopped` line that arrived while [`TestApp::tones`] was draining.
    ///
    /// [`TestApp::stop`] reads the last line the application printed, and a
    /// test that drains the output during the run would otherwise take that
    /// line out of the channel and leave the stop reporting that the
    /// application never said how it went.
    stopped_early: Option<String>,
}

/// What a run said about its sound.
///
/// Three states rather than two, because "this run was not asked for a tone"
/// and "this run was asked for one and this machine cannot play it" are
/// different things to a test: the first is every capture test in this
/// workspace, and the second is a reason to skip a measurement rather than to
/// wait for a sound that is never coming.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tone {
    /// The run was not asked for a tone, and is silent.
    Off,
    /// The run was asked for a tone and the machine could not play one. The
    /// application says why on standard error.
    Unavailable,
    /// The run is playing tones on this plan.
    Playing(TonePlan),
}

/// What a run said about its *continuous* tone, which is the other kind of
/// sound this application makes (`--steady-tone`).
///
/// Separate from [`Tone`] rather than a fourth variant of it, because the two
/// are separate options and a run can be asked for either. Three states for the
/// same reason [`Tone`] has three: a driver has to be able to tell a run that
/// was never asked for a sound from a run that was asked and could not make
/// one, and only the second is a reason to skip a measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SteadyTone {
    /// The run was not asked for a continuous tone, and is silent.
    Off,
    /// The run was asked for one and the machine could not play it. The
    /// application says why on standard error.
    Unavailable,
    /// The run is holding this frequency, in hertz, for its whole length.
    Playing(f32),
}

/// Which frames of a run carry a tone.
///
/// Arithmetic rather than a list, so that a test knows every frame that will
/// carry one before the run starts — which is what lets it decode those frames
/// out of a capture instead of decoding all of them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TonePlan {
    /// The tone's frequency in hertz.
    pub frequency: f32,
    /// How long each tone lasts.
    pub length: Duration,
    /// The counter of the frame the first tone belongs to.
    pub first_frame: u32,
    /// Frames from one tone's frame to the next.
    pub frame_interval: u32,
}

impl TonePlan {
    /// How many frames it is from `counter` to the next frame carrying a tone,
    /// counting the frame itself as zero.
    ///
    /// [`None`] once there are no more, which cannot happen: the plan repeats
    /// for as long as the run does.
    #[must_use]
    pub fn frames_until(&self, counter: u32) -> Option<u32> {
        if self.frame_interval == 0 {
            return None;
        }
        match self.first_frame.checked_sub(counter) {
            Some(until) => Some(until),
            None => Some(
                (self.frame_interval - (counter - self.first_frame) % self.frame_interval)
                    % self.frame_interval,
            ),
        }
    }
}

/// One tone the application announced as it presented the frame it belongs to.
///
/// Both moments are nanoseconds on the Windows performance counter, which is
/// the clock a capture backend stamps frames with and WASAPI reports positions
/// on (`docs/av-sync.md`), so they can be compared with a recording's timestamps
/// directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToneEvent {
    /// Which tone of the run this is, counting from zero.
    pub index: u32,
    /// The counter of the frame it belongs to.
    pub frame: u32,
    /// Where the endpoint's own clock puts the tone's half-amplitude point, or
    /// why there is no such moment.
    pub onset: Onset,
    /// The counter reading immediately after the frame was handed to the
    /// compositor.
    ///
    /// How far this is from [`Onset::At`] is how far apart the two halves of
    /// the event were **at the source**, which a measurement of a recording has
    /// to subtract before calling what is left the recorder's: nothing makes a
    /// thread present a frame at exactly the moment an endpoint plays a sample.
    pub present_nanos: u64,
}

/// What the application knew about a tone's sound as it presented the frame.
///
/// Three states rather than an [`Option`], because the two ways of having no
/// moment are different faults and counting them together hides one of them: a
/// tone that was never played is a hole in the subject's sound, and a tone
/// whose placement had not been reported yet is a late report of a sound that
/// probably *was* played. A run whose tones are all [`Onset::Unreported`] has a
/// reporting problem; a run whose tones are all [`Onset::NotPlaced`] has an
/// audio one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Onset {
    /// The endpoint's clock puts the half-amplitude point here, in nanoseconds
    /// on the performance counter.
    At(u64),
    /// The render thread could not put the tone at the moment it was asked for
    /// — the moment had already been queued past — so it did not play it. A
    /// tone that was not played is reported as one rather than played late
    /// (AGENTS.md section 54).
    NotPlaced,
    /// The render thread had not said what it did with the tone by the time the
    /// frame was presented. The sound may well have been made; what is missing
    /// is the report of where it was put, so nothing can be measured from it.
    Unreported,
}

impl TestApp {
    /// Starts `executable` with `arguments` and waits for it to be on screen.
    ///
    /// Returns once the application has printed its `ready` line, which it does
    /// after the window exists, the swap chain is presenting and — for an
    /// exclusive fullscreen run — the display has been asked for. Capturing
    /// before that line is capturing a window that may not exist yet.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] if the process could not be started, said nothing
    /// within `ready_timeout`, exited before announcing itself, or announced
    /// something this version cannot parse. Every one of those is reported
    /// rather than waited out, because a capture test blocked on a subject that
    /// never appeared is the least informative failure available.
    pub fn start<Argument: AsRef<OsStr>>(
        executable: impl AsRef<OsStr>,
        arguments: impl IntoIterator<Item = Argument>,
        ready_timeout: Duration,
    ) -> Result<Self, HarnessError> {
        let executable = executable.as_ref().to_owned();
        let mut child = Command::new(&executable)
            .args(arguments)
            // Standard input is a pipe so that closing it stops the
            // application; standard error is inherited so that its warnings
            // land in the test's output where somebody will read them.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|source| HarnessError::Spawn {
                executable: executable.to_string_lossy().into_owned(),
                source,
            })?;

        let stdout = child.stdout.take().ok_or(HarnessError::NoPipe)?;
        let lines = match child_output::reading(stdout) {
            Ok(lines) => lines,
            Err(source) => {
                // The application is this function's to clean up until the
                // struct that owns it exists.
                let _ = child.kill();
                let _ = child.wait();
                return Err(HarnessError::Spawn {
                    executable: "the reader thread".to_owned(),
                    source,
                });
            }
        };

        match Self::from_ready_line(child, lines, ready_timeout) {
            Ok(app) => Ok(app),
            Err((mut child, error)) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }

    /// Waits for the `ready` line and builds the handle, or gives the child
    /// back so the caller can kill it.
    fn from_ready_line(
        child: Child,
        lines: Lines,
        ready_timeout: Duration,
    ) -> Result<Self, (Child, HarnessError)> {
        let line = match next_line_within(&lines, ready_timeout) {
            Ok(line) => line,
            Err(error) => return Err((child, error)),
        };
        let fields = match parse_ready(&line) {
            Ok(fields) => fields,
            Err(error) => return Err((child, error)),
        };
        Ok(Self {
            child,
            lines,
            window: fields.window,
            client: fields.client,
            presentation: fields.presentation,
            exclusive: fields.exclusive,
            monitor: fields.monitor,
            tone: fields.tone,
            steady_tone: fields.steady_tone,
            stopped_early: None,
        })
    }

    /// The window handle, as the number a capture target is built from.
    #[must_use]
    pub const fn window(&self) -> usize {
        self.window
    }

    /// The pattern's exact size in physical pixels.
    #[must_use]
    pub const fn client_size(&self) -> (u32, u32) {
        self.client
    }

    /// How the application says it is presenting.
    #[must_use]
    pub fn presentation(&self) -> &str {
        &self.presentation
    }

    /// Whether the application was granted the display exclusively.
    ///
    /// Windows refuses this to a process the user has not interacted with, so a
    /// test asserting on exclusive fullscreen has to read this rather than
    /// assume it (AGENTS.md section 16).
    #[must_use]
    pub const fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    /// The display it was placed on, as `\\.\DISPLAY1`.
    #[must_use]
    pub fn monitor(&self) -> &str {
        &self.monitor
    }

    /// What the run said about its sound, from the `ready` line.
    #[must_use]
    pub const fn tone(&self) -> Tone {
        self.tone
    }

    /// What the run said about its continuous tone, from the `ready` line.
    #[must_use]
    pub const fn steady_tone(&self) -> SteadyTone {
        self.steady_tone
    }

    /// Takes every tone the application has announced since this was last
    /// called.
    ///
    /// Drained rather than waited on: the application announces a tone as it
    /// presents the frame the tone belongs to, so a test that wants to decode
    /// that frame has to be capturing at the time rather than reading the
    /// output afterwards.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] if the pipe failed or if a `tone` line is not one this
    /// version understands. Lines that are neither a tone nor the `stopped`
    /// summary are ignored, because standard output is a protocol this test
    /// harness is allowed not to have caught up with.
    pub fn tones(&mut self) -> Result<Vec<ToneEvent>, HarnessError> {
        let mut tones = Vec::new();
        while let Ok(line) = self.lines.try_recv() {
            let line = line.map_err(|source| HarnessError::Stop {
                detail: format!("could not read the test application's output: {source}"),
            })?;
            let line = line.trim_end();
            if line.starts_with("stopped ") {
                self.stopped_early = Some(line.to_owned());
            } else if line.starts_with("tone ") {
                tones.push(parse_tone(line)?);
            }
        }
        Ok(tones)
    }

    /// Asks the application to stop, and waits up to `timeout` for it to go.
    ///
    /// Closing standard input is the request; the application's own watcher
    /// turns that into a clean shutdown that gives back the display before the
    /// process exits, which matters after an exclusive fullscreen run.
    ///
    /// Returns what the application said it did, which is what a capture test
    /// compares its own frame accounting against.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Stop`] if the application had to be killed, exited
    /// unsuccessfully, could not be waited for, or never said how it went. The
    /// application is gone either way — this reports that it did not go
    /// quietly, which is a defect in the application worth failing a test over.
    pub fn stop(mut self, timeout: Duration) -> Result<Stopped, HarnessError> {
        self.request_stop();

        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) if status.success() => return self.final_summary(OUTPUT_DRAIN),
                Ok(Some(status)) => {
                    return Err(HarnessError::Stop {
                        detail: format!("the test application exited with {status}"),
                    })
                }
                Ok(None) if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return Err(HarnessError::Stop {
                        detail: format!(
                            "the test application was still running {:.1}s after it was \
                             asked to stop, and had to be killed",
                            timeout.as_secs_f64()
                        ),
                    });
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(source) => {
                    return Err(HarnessError::Stop {
                        detail: format!("could not wait for the test application: {source}"),
                    })
                }
            }
        }
    }

    /// Closes standard input, which the application reads as "stop".
    fn request_stop(&mut self) {
        drop(self.child.stdin.take());
    }

    /// The `stopped` line the application prints last.
    ///
    /// Called after the process has exited, which is not the same thing as the
    /// reader thread having finished: the child exiting and the thread reading
    /// the last bytes out of the pipe and sending them are separate events, in
    /// separate processes, with nothing ordering them. So the drain waits for
    /// the channel to *disconnect* — which happens when the reader thread drops
    /// its sender, and it does that only after the pipe has reached end of file
    /// — rather than taking what happens to have arrived. Draining with
    /// `try_recv` instead loses the last line on a machine loaded enough to
    /// have not scheduled the reader yet, and reports a clean run as an
    /// application that may have kept the display.
    ///
    /// The wait is bounded so that a reader thread which somehow never finishes
    /// fails the test rather than hanging it.
    fn final_summary(&self, timeout: Duration) -> Result<Stopped, HarnessError> {
        let deadline = Instant::now() + timeout;
        let mut last = self.stopped_early.clone();
        loop {
            match self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) => {
                    let line = line.map_err(|source| HarnessError::Stop {
                        detail: format!("could not read the test application's output: {source}"),
                    })?;
                    if line.trim_end().starts_with("stopped ") {
                        last = Some(line);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(HarnessError::Stop {
                        detail: format!(
                            "the test application exited but the thread reading its output \
                             had not finished {:.1}s later, so its last line cannot be \
                             accounted for",
                            timeout.as_secs_f64()
                        ),
                    })
                }
            }
        }

        let line = last.ok_or_else(|| HarnessError::Stop {
            detail: "the test application exited without printing a `stopped` line, so \
                     there is no telling whether it gave the display back"
                .to_owned(),
        })?;
        parse_stopped(&line)
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.request_stop();

        let deadline = Instant::now() + DROP_GRACE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }

        // Whether the test passed or panicked, nothing of it may still be
        // rendering on somebody's second monitor a minute later.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The fields of a `ready` line.
struct Ready {
    window: usize,
    client: (u32, u32),
    presentation: String,
    exclusive: bool,
    monitor: String,
    tone: Tone,
    steady_tone: SteadyTone,
}

/// Parses `ready hwnd=0x… client=1280x720 …`.
fn parse_ready(line: &str) -> Result<Ready, HarnessError> {
    let rest = line
        .strip_prefix("ready ")
        .ok_or_else(|| HarnessError::Protocol {
            line: line.to_owned(),
            detail: "the first line was not a `ready` line".to_owned(),
        })?;

    let fields: HashMap<&str, &str> = rest
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect();

    let field = |name: &str| -> Result<String, HarnessError> {
        fields
            .get(name)
            .map(|value| (*value).to_owned())
            .ok_or_else(|| HarnessError::Protocol {
                line: line.to_owned(),
                detail: format!("no `{name}` field"),
            })
    };
    let protocol = |detail: String| HarnessError::Protocol {
        line: line.to_owned(),
        detail,
    };

    let handle = field("hwnd")?;
    let digits = handle
        .strip_prefix("0x")
        .ok_or_else(|| protocol("the window handle is not hexadecimal".to_owned()))?;
    let window = usize::from_str_radix(digits, 16)
        .map_err(|source| protocol(format!("the window handle does not parse: {source}")))?;
    if window == 0 {
        return Err(protocol("the window handle is null".to_owned()));
    }

    let client = field("client")?;
    let (width, height) = client
        .split_once('x')
        .ok_or_else(|| protocol("the client size is not WIDTHxHEIGHT".to_owned()))?;
    let client = (
        width
            .parse()
            .map_err(|_| protocol(format!("the client width does not parse: {width}")))?,
        height
            .parse()
            .map_err(|_| protocol(format!("the client height does not parse: {height}")))?,
    );

    let tone = match fields.get("tone").copied() {
        Some("yes") => {
            let number = |name: &str| -> Result<u32, HarnessError> {
                let value = field(name)?;
                value
                    .parse()
                    .map_err(|_| protocol(format!("`{name}` does not parse: {value}")))
            };
            let frequency = field("tone-hz")?;
            Tone::Playing(TonePlan {
                frequency: frequency
                    .parse()
                    .map_err(|_| protocol(format!("`tone-hz` does not parse: {frequency}")))?,
                length: Duration::from_millis(u64::from(number("tone-ms")?)),
                first_frame: number("tone-first")?,
                frame_interval: number("tone-every")?,
            })
        }
        Some("no") => Tone::Unavailable,
        // An application old enough not to have the field at all is a silent
        // one, which is what `off` says.
        Some("off") | None => Tone::Off,
        Some(other) => return Err(protocol(format!("`tone={other}` is not a state"))),
    };

    let steady_tone =
        match fields.get("steady-tone").copied() {
            Some("yes") => {
                let frequency = field("steady-tone-hz")?;
                SteadyTone::Playing(frequency.parse().map_err(|_| {
                    protocol(format!("`steady-tone-hz` does not parse: {frequency}"))
                })?)
            }
            Some("no") => SteadyTone::Unavailable,
            Some("off") | None => SteadyTone::Off,
            Some(other) => return Err(protocol(format!("`steady-tone={other}` is not a state"))),
        };

    Ok(Ready {
        window,
        client,
        presentation: field("presentation")?,
        exclusive: field("exclusive")? == "yes",
        monitor: field("monitor").unwrap_or_default(),
        tone,
        steady_tone,
    })
}

/// Parses `tone index=0 frame=60 onset=61420657101866 present=61420657564400 skew=462534`.
///
/// `onset` is `none` for a tone the render thread refused to place and
/// `pending` for one it had not reported by the time the frame was presented
/// ([`Onset`]).
///
/// `skew` is not read: it is `present − onset`, printed so that a person reading
/// the output does not have to subtract two eleven-digit numbers, and a test
/// that took the application's arithmetic on trust would be checking one number
/// against itself.
fn parse_tone(line: &str) -> Result<ToneEvent, HarnessError> {
    let fields: HashMap<&str, &str> = line
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect();
    let protocol = |detail: String| HarnessError::Protocol {
        line: line.to_owned(),
        detail,
    };

    let number = |name: &str| -> Result<u64, HarnessError> {
        let value = fields
            .get(name)
            .ok_or_else(|| protocol(format!("no `{name}` field")))?;
        value
            .parse()
            .map_err(|_| protocol(format!("`{name}` does not parse: {value}")))
    };

    let onset = fields
        .get("onset")
        .ok_or_else(|| protocol("no `onset` field".to_owned()))?;

    Ok(ToneEvent {
        index: u32::try_from(number("index")?)
            .map_err(|_| protocol("the tone index is not a `u32`".to_owned()))?,
        frame: u32::try_from(number("frame")?)
            .map_err(|_| protocol("the frame counter is not a `u32`".to_owned()))?,
        onset: match *onset {
            "none" => Onset::NotPlaced,
            "pending" => Onset::Unreported,
            _ => Onset::At(number("onset")?),
        },
        present_nanos: number("present")?,
    })
}

/// Takes the next line the application printed, giving up after `timeout`.
///
/// The wait itself is [`child_output::next_line_within`]; what is here is this
/// harness's wording for the three ways it can come to nothing.
fn next_line_within(lines: &Lines, timeout: Duration) -> Result<String, HarnessError> {
    child_output::next_line_within(lines, timeout).map_err(|reason| HarnessError::NoOutput {
        detail: match reason {
            NoLine::Unreadable(source) => {
                format!("could not read from the test application: {source}")
            }
            NoLine::Silent(timeout) => format!(
                "the test application printed nothing within {:.1}s",
                timeout.as_secs_f64()
            ),
            NoLine::Ended => "the test application exited without announcing itself".to_owned(),
        },
    })
}

/// What the application said it did, from its last line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    /// Frames it presented, which is one more than the last counter it drew.
    pub frames: u32,
    /// Why it stopped, in the application's own words: `deadline`,
    /// `stop-requested`, `interrupted` or `window-closed`.
    pub reason: String,
}

/// Parses `stopped frames=901 reason=deadline`.
fn parse_stopped(line: &str) -> Result<Stopped, HarnessError> {
    let fields: HashMap<&str, &str> = line
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect();
    let protocol = |detail: String| HarnessError::Protocol {
        line: line.to_owned(),
        detail,
    };

    let frames = fields
        .get("frames")
        .ok_or_else(|| protocol("no `frames` field".to_owned()))?;
    let frames = frames
        .parse()
        .map_err(|_| protocol(format!("the frame count does not parse: {frames}")))?;

    Ok(Stopped {
        frames,
        reason: (*fields
            .get("reason")
            .ok_or_else(|| protocol("no `reason` field".to_owned()))?)
        .to_owned(),
    })
}

/// Why a test could not get a subject to point a capture at.
#[derive(Debug)]
#[non_exhaustive]
pub enum HarnessError {
    /// The process could not be started.
    Spawn {
        /// What was being started.
        executable: String,
        /// Why it could not be.
        source: std::io::Error,
    },
    /// The process was started without the pipe the protocol needs.
    NoPipe,
    /// Nothing usable arrived on standard output.
    NoOutput {
        /// What happened instead.
        detail: String,
    },
    /// The first line was not the `ready` line this version understands.
    Protocol {
        /// The line as received.
        line: String,
        /// What was wrong with it.
        detail: String,
    },
    /// The application did not stop when it was asked to.
    Stop {
        /// What happened.
        detail: String,
    },
}

impl fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { executable, source } => {
                write!(formatter, "could not start {executable}: {source}")
            }
            Self::NoPipe => {
                formatter.write_str("the test application was started without a standard output")
            }
            Self::NoOutput { detail } => formatter.write_str(detail),
            Self::Protocol { line, detail } => write!(
                formatter,
                "the test application said `{line}`, which this test cannot use: {detail}"
            ),
            Self::Stop { detail } => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for HarnessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            Self::NoPipe | Self::NoOutput { .. } | Self::Protocol { .. } | Self::Stop { .. } => {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ready_line_is_read_into_the_fields_a_test_needs() {
        let ready = parse_ready(
            "ready hwnd=0x00000000000a06f2 client=1280x720 fps=30 \
             presentation=borderless exclusive=no monitor=\\\\.\\DISPLAY1",
        )
        .expect("this is the line the application prints");

        assert_eq!(ready.window, 0x000a_06f2);
        assert_eq!(ready.client, (1280, 720));
        assert_eq!(ready.presentation, "borderless");
        assert!(!ready.exclusive);
        assert_eq!(ready.monitor, "\\\\.\\DISPLAY1");
    }

    #[test]
    fn a_steady_tone_run_is_read_back_as_the_frequency_it_is_holding() {
        let ready = parse_ready(
            "ready hwnd=0x1 client=1280x720 fps=60 presentation=borderless exclusive=no \
             monitor=\\\\.\\DISPLAY1 tone=off steady-tone=yes steady-tone-hz=997 \
             steady-tone-rate=48000 steady-tone-channels=2",
        )
        .expect("this is the line a --steady-tone run prints");

        assert_eq!(ready.steady_tone, SteadyTone::Playing(997.0));
        assert_eq!(
            ready.tone,
            Tone::Off,
            "the two kinds of sound are separate options and must not be read as each other"
        );
    }

    #[test]
    fn a_run_that_could_not_hold_a_tone_is_not_read_as_one_that_was_never_asked() {
        // The distinction the whole three-state enumeration exists for. An
        // isolation test measuring a track for a frequency has to skip on the
        // first and would otherwise measure silence and blame the routing
        // (AGENTS.md section 54).
        let refused = parse_ready(
            "ready hwnd=0x1 client=1280x720 fps=60 presentation=borderless exclusive=no \
             monitor=\\\\.\\DISPLAY1 tone=off steady-tone=no",
        )
        .expect("this is the line a machine with no output endpoint prints");
        assert_eq!(refused.steady_tone, SteadyTone::Unavailable);

        let silent = parse_ready(
            "ready hwnd=0x1 client=1280x720 fps=30 presentation=borderless exclusive=no \
             monitor=\\\\.\\DISPLAY1",
        )
        .expect("a `ready` line without the steady-tone fields is still a `ready` line");
        assert_eq!(silent.steady_tone, SteadyTone::Off);
    }

    #[test]
    fn a_run_that_says_nothing_about_sound_is_read_as_a_silent_one() {
        // The line every other capture test's application prints. It has to
        // keep parsing, and it has to mean silence rather than an unknown.
        let ready = parse_ready(
            "ready hwnd=0x1 client=1280x720 fps=30 presentation=borderless exclusive=no \
             monitor=\\\\.\\DISPLAY1",
        )
        .expect("a `ready` line without the tone fields is still a `ready` line");
        assert_eq!(ready.tone, Tone::Off);

        let refused = parse_ready(
            "ready hwnd=0x1 client=1280x720 fps=30 presentation=borderless exclusive=no \
             monitor=\\\\.\\DISPLAY1 tone=no",
        )
        .expect("this is the line the application prints when it cannot play a tone");
        assert_eq!(
            refused.tone,
            Tone::Unavailable,
            "a machine that cannot play the tone has to be distinguishable from one that \
             was never asked to, or a measurement waits for a sound that is not coming"
        );
    }

    #[test]
    fn a_sounded_run_announces_which_frames_carry_a_tone() {
        let ready = parse_ready(
            "ready hwnd=0x1 client=1280x720 fps=30 presentation=borderless exclusive=no \
             monitor=\\\\.\\DISPLAY1 tone=yes tone-hz=997 tone-ms=30 tone-first=60 \
             tone-every=150",
        )
        .expect("this is the line a --tone run prints");

        let Tone::Playing(plan) = ready.tone else {
            panic!(
                "a run announcing tone=yes is playing tones, and read as {:?}",
                ready.tone
            );
        };
        assert!((plan.frequency - 997.0).abs() < f32::EPSILON);
        assert_eq!(plan.length, Duration::from_millis(30));

        // The arithmetic the capture test uses to decide which frames to
        // decode: it follows the counter and has to know, from any frame, how
        // far the next tone is.
        assert_eq!(plan.frames_until(0), Some(60));
        assert_eq!(plan.frames_until(60), Some(0));
        assert_eq!(plan.frames_until(61), Some(149));
        assert_eq!(plan.frames_until(209), Some(1));
    }

    #[test]
    fn a_tone_line_carries_both_moments_of_the_event() {
        // The pair the whole absolute measurement rests on: where the endpoint
        // put the sound, and where the application handed over the picture.
        let tone = parse_tone(
            "tone index=2 frame=360 onset=61430657118666 \
                               present=61430657907400 skew=788734",
        )
        .expect("this is the line the application prints per tone");

        assert_eq!(tone.index, 2);
        assert_eq!(tone.frame, 360);
        assert_eq!(tone.onset, Onset::At(61_430_657_118_666));
        assert_eq!(tone.present_nanos, 61_430_657_907_400);
        let Onset::At(onset) = tone.onset else {
            panic!("this line announced a moment, and read as {:?}", tone.onset);
        };
        assert_eq!(
            tone.present_nanos as i64 - onset as i64,
            788_734,
            "the two moments have to be read exactly, because a measurement subtracts one \
             from the other; the line's own `skew` field is ignored rather than trusted"
        );
    }

    #[test]
    fn a_tone_that_was_not_played_is_read_as_one_that_was_not_played() {
        // The case that must never be read as "played at zero": a tone the
        // application could not place at the moment it wanted.
        let tone = parse_tone("tone index=0 frame=60 onset=none present=61420657564400 skew=none")
            .expect("this is the line the application prints for a tone it did not play");
        assert_eq!(tone.onset, Onset::NotPlaced);

        // And the other way of having no moment, which is a different fault:
        // the sound was probably made and the report of where it went had not
        // arrived when the frame was presented.
        let pending =
            parse_tone("tone index=0 frame=60 onset=pending present=61420657564400 skew=none")
                .expect("this is the line the application prints for an unreported tone");
        assert_eq!(pending.onset, Onset::Unreported);

        for line in [
            "tone frame=60 onset=1 present=2",
            "tone index=0 onset=1 present=2",
            "tone index=0 frame=60 present=2",
            "tone index=0 frame=60 onset=1",
            "tone index=0 frame=60 onset=soon present=2",
        ] {
            assert!(
                parse_tone(line).is_err(),
                "`{line}` should not have been accepted"
            );
        }
    }

    #[test]
    fn an_exclusive_run_is_recognised() {
        let ready = parse_ready(
            "ready hwnd=0x1 client=2560x1440 fps=60 presentation=fullscreen-exclusive \
             exclusive=yes monitor=\\\\.\\DISPLAY2",
        )
        .expect("this is the line the application prints");
        assert!(ready.exclusive);
    }

    #[test]
    fn the_last_line_says_how_many_frames_the_application_presented() {
        let stopped = parse_stopped("stopped frames=901 reason=deadline")
            .expect("this is the line the application prints last");
        assert_eq!(stopped.frames, 901);
        assert_eq!(stopped.reason, "deadline");

        for line in [
            "stopped reason=deadline",
            "stopped frames=lots reason=deadline",
            "stopped frames=1",
        ] {
            assert!(
                parse_stopped(line).is_err(),
                "`{line}` should not have been accepted"
            );
        }
    }

    #[test]
    fn a_line_that_is_not_the_protocol_is_refused_rather_than_half_understood() {
        // Every one of these has to be an error rather than a default, because
        // the alternative is a capture test pointed at window zero, which fails
        // ten seconds later with a message about capture.
        for line in [
            "starting up",
            "ready client=1280x720 presentation=borderless exclusive=no",
            "ready hwnd=00000000000a06f2 client=1280x720 presentation=borderless exclusive=no",
            "ready hwnd=0x0 client=1280x720 presentation=borderless exclusive=no",
            "ready hwnd=0xzz client=1280x720 presentation=borderless exclusive=no",
            "ready hwnd=0x1 client=1280 presentation=borderless exclusive=no",
            "ready hwnd=0x1 client=1280x720 exclusive=no",
        ] {
            assert!(
                parse_ready(line).is_err(),
                "`{line}` should not have been accepted"
            );
        }
    }
}
