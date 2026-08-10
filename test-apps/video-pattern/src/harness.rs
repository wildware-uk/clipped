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
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

/// How long [`Drop`] gives the application to stop cleanly before killing it.
const DROP_GRACE: Duration = Duration::from_secs(3);

/// A running test application.
#[derive(Debug)]
pub struct TestApp {
    child: Child,
    /// Every line the application prints after the `ready` one, as its reader
    /// thread receives them. Reading them all is not curiosity: an unread pipe
    /// makes the application's own writes fail, and its last line is the frame
    /// count a test cross-checks its capture against.
    lines: Receiver<std::io::Result<String>>,
    window: usize,
    client: (u32, u32),
    presentation: String,
    exclusive: bool,
    monitor: String,
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
        let lines = match read_lines(stdout) {
            Ok(lines) => lines,
            Err(error) => {
                // The application is this function's to clean up until the
                // struct that owns it exists.
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
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
        lines: Receiver<std::io::Result<String>>,
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
                Ok(Some(status)) if status.success() => return self.final_summary(),
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
    /// Called after the process has exited, so its reader thread has finished
    /// and every line it ever wrote is already in the channel.
    fn final_summary(&self) -> Result<Stopped, HarnessError> {
        let mut last = None;
        while let Ok(line) = self.lines.try_recv() {
            let line = line.map_err(|source| HarnessError::Stop {
                detail: format!("could not read the test application's output: {source}"),
            })?;
            if line.starts_with("stopped ") {
                last = Some(line);
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

    Ok(Ready {
        window,
        client,
        presentation: field("presentation")?,
        exclusive: field("exclusive")? == "yes",
        monitor: field("monitor").unwrap_or_default(),
    })
}

/// Reads the application's standard output on a thread of its own.
///
/// A thread, rather than reading in the test, for two reasons. A child that
/// never writes anything would otherwise block the test for ever, and there is
/// no portable way to put a deadline on a pipe read. And the pipe has to keep
/// being read for the whole run: a child whose standard output fills up blocks
/// in `write`, and this one would then stop presenting frames — the subject
/// would freeze because the test stopped listening.
///
/// The thread ends when the pipe closes, which happens when the application
/// exits; [`TestApp`]'s `Drop` guarantees that it does.
fn read_lines(stdout: ChildStdout) -> Result<Receiver<std::io::Result<String>>, HarnessError> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("test-app-output".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    // The test has gone. `Drop` is dealing with the process.
                    break;
                }
            }
        })
        .map_err(|source| HarnessError::Spawn {
            executable: "the reader thread".to_owned(),
            source,
        })?;
    Ok(receiver)
}

/// Takes the next line the application printed, giving up after `timeout`.
fn next_line_within(
    lines: &Receiver<std::io::Result<String>>,
    timeout: Duration,
) -> Result<String, HarnessError> {
    match lines.recv_timeout(timeout) {
        Ok(Ok(line)) => Ok(line.trim_end().to_owned()),
        Ok(Err(source)) => Err(HarnessError::NoOutput {
            detail: format!("could not read from the test application: {source}"),
        }),
        Err(RecvTimeoutError::Timeout) => Err(HarnessError::NoOutput {
            detail: format!(
                "the test application printed nothing within {:.1}s",
                timeout.as_secs_f64()
            ),
        }),
        Err(RecvTimeoutError::Disconnected) => Err(HarnessError::NoOutput {
            detail: "the test application exited without announcing itself".to_owned(),
        }),
    }
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
