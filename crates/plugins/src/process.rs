//! One running plugin: the child process, and the thread that reads it.
//!
//! # Nothing here ever waits for a plugin
//!
//! Not when it is started, not when it is stopped, and not when it is killed.
//! Every function in this module returns immediately, and everything that takes
//! time — a plugin that has not said hello yet, a plugin that has been asked to
//! stop and has not — is a state `crate::supervisor` looks at again next time it
//! is polled. That is what keeps AGENTS.md section 20 true at the only place it
//! could be broken: a session that stops a plugin at the end of a recording must
//! not spend two seconds waiting for it before finalising the file.
//!
//! # The reader thread, and why it is not joined
//!
//! A pipe has no timed read, so reading a plugin's output means a thread
//! blocked on `read`. It ends when the plugin's standard output closes, which
//! happens when the plugin exits — including when it is killed, which is the
//! whole reason a plugin is a *process*: a hung thread inside this process
//! could never be reclaimed, and a hung process can be terminated by the
//! operating system.
//!
//! It is never joined, deliberately. Joining would make this process's shutdown
//! depend on a pipe closing, and a plugin that left a child of its own holding
//! that pipe would hang the recorder — which is precisely the failure this
//! design exists to prevent. Instead the thread reports that it has finished
//! ([`ReaderSnapshot::reader_finished`]), and the tests assert that it does.

use core::time::Duration;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use clipped_events::EventSource;

use crate::discovery::EnabledPlugin;
use crate::inbox::EventInbox;
use crate::manifest::{ContractVersion, CONTRACT};
use crate::report::{
    read_report, HostCommand, PluginReport, SessionDetails, SessionTimeline, MAX_PROBLEM_BYTES,
};

/// The longest line this build will read from a plugin.
///
/// An event's payload is capped at 4 KiB by `crates/events`, so a legitimate
/// line is a fraction of this. The cap exists because a line is read into
/// memory before it can be parsed, and "read until a newline" from a program
/// that never sends one is an allocation with no upper bound.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// How many of a plugin's problem messages are kept.
///
/// The most recent few, because they are shown to a user and a plugin repeating
/// itself must not grow this process (AGENTS.md section 59).
const KEPT_PROBLEMS: usize = 4;

/// A plugin that is running.
#[derive(Debug)]
pub(crate) struct PluginProcess {
    child: Child,
    /// Kept so that `detach` can be written and the plugin's standard input can
    /// then be closed — a plugin that ignores commands still reads end of file.
    stdin: Option<ChildStdin>,
    reader: JoinHandle<()>,
    shared: Arc<Mutex<ReaderState>>,
    started_at: Instant,
    /// When the plugin was asked to stop, if it has been.
    stopping_since: Option<Instant>,
}

impl PluginProcess {
    /// Starts `plugin` and tells it about `session`.
    ///
    /// Returns as soon as the process exists. The plugin's `hello` has not
    /// arrived yet and is not waited for; `crate::supervisor` notices its
    /// absence.
    ///
    /// # Errors
    ///
    /// Whatever the operating system said about starting the executable.
    pub(crate) fn spawn(
        plugin: &EnabledPlugin,
        session: &SessionDetails,
        timeline: SessionTimeline,
        inbox: EventInbox,
        now: Instant,
    ) -> io::Result<Self> {
        let installed = plugin.installed();
        let mut command = Command::new(installed.executable());
        command
            .current_dir(installed.directory())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Standard error is deliberately **not** a pipe. A pipe nobody
            // drains fills, and a plugin that then blocks writing to it is a
            // hang this host would have caused. Inheriting sends a plugin's own
            // diagnostics wherever the recorder's go, which for a detached
            // recorder is nowhere and for a developer is the console.
            .stderr(Stdio::inherit());

        // A plugin is a console program, and a game is in the foreground. Without
        // this, starting one flashes a console window over the game.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .expect("standard output was asked for as a pipe");
        let mut stdin = child
            .stdin
            .take()
            .expect("standard input was asked for as a pipe");

        // Around two hundred bytes into a pipe buffer of several kilobytes, so
        // this cannot block on a plugin that has not started reading yet.
        let attach = HostCommand::Attach {
            contract: CONTRACT,
            session: session.clone(),
        };
        if let Err(error) = stdin.write_all(attach.to_line().as_bytes()) {
            tracing::warn!(
                plugin = %plugin.id(),
                %error,
                "the plugin closed its standard input before it was told about the session"
            );
        }
        let _ = stdin.flush();

        let shared = Arc::new(Mutex::new(ReaderState::new(now)));
        let reader = spawn_reader(
            plugin.id().as_source().clone(),
            BufReader::new(stdout),
            Arc::clone(&shared),
            inbox,
            timeline,
        );

        Ok(Self {
            child,
            stdin: Some(stdin),
            reader,
            shared,
            started_at: now,
            stopping_since: None,
        })
    }

    /// What the reader has seen so far.
    pub(crate) fn snapshot(&self) -> ReaderSnapshot {
        let state = self.shared.lock().unwrap_or_else(|poisoned| {
            // The reader thread panicking would be a bug in this crate rather
            // than in a plugin, and losing every plugin over it would be the
            // wrong answer to it.
            poisoned.into_inner()
        });
        ReaderSnapshot {
            started_at: self.started_at,
            last_report: state.last_report,
            hello: state.hello,
            dropped: state.dropped,
            faults: state.faults,
            problems: state.problems.clone(),
            reader_finished: self.reader.is_finished(),
        }
    }

    /// Whether the process has ended, and how, without waiting for it.
    pub(crate) fn exit_status(&mut self) -> Option<ExitStatus> {
        match self.child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                tracing::warn!(%error, "a plugin's exit status could not be read");
                None
            }
        }
    }

    /// Asks the plugin to finish, and does not wait for it.
    ///
    /// Writes `detach` and closes the plugin's standard input, which is the
    /// second half of the same message: a plugin that never reads a command
    /// still reads end of file.
    pub(crate) fn ask_to_stop(&mut self, now: Instant) {
        if self.stopping_since.is_some() {
            return;
        }
        self.stopping_since = Some(now);
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(HostCommand::Detach.to_line().as_bytes());
            let _ = stdin.flush();
        }
        self.stdin = None;
    }

    /// Whether a plugin that was asked to stop has had long enough.
    pub(crate) fn outstayed_its_welcome(&self, now: Instant, grace: Duration) -> bool {
        self.stopping_since
            .is_some_and(|since| now.duration_since(since) >= grace)
    }

    /// Ends the process now.
    ///
    /// Kills and reaps it. Neither call waits on the plugin's cooperation: this
    /// is `TerminateProcess`, and the reader thread ends when the pipe the
    /// dead process held closes.
    pub(crate) fn kill(&mut self) {
        self.stdin = None;
        if let Err(error) = self.child.kill() {
            // `InvalidInput` means it had already exited, which is not a
            // problem worth a line in a log.
            if error.kind() != io::ErrorKind::InvalidInput {
                tracing::warn!(%error, "a plugin could not be stopped");
            }
        }
        let _ = self.child.wait();
    }
}

impl Drop for PluginProcess {
    /// A supervisor that is dropped leaves no plugins running.
    ///
    /// The recorder may be shutting down, or the session may have ended in a
    /// way nobody planned. Either way a plugin outliving the process that
    /// started it is a process nobody owns, holding a port a game will want
    /// again.
    fn drop(&mut self) {
        if self.exit_status().is_none() {
            self.kill();
        }
    }
}

/// What the reader thread has seen, as of one moment.
#[derive(Debug, Clone)]
pub(crate) struct ReaderSnapshot {
    /// When the process was started.
    pub(crate) started_at: Instant,
    /// When it last said anything this build could read.
    pub(crate) last_report: Instant,
    /// The contract version it introduced itself with, once it has.
    pub(crate) hello: Option<ContractVersion>,
    /// Events of its that the recording could not take.
    pub(crate) dropped: u64,
    /// Lines that could not be read, and events that were refused.
    pub(crate) faults: u32,
    /// The most recent things it said were wrong.
    pub(crate) problems: Vec<String>,
    /// Whether the thread reading it has finished, which means its standard
    /// output has closed.
    pub(crate) reader_finished: bool,
}

/// The reader thread's side of a running plugin.
#[derive(Debug)]
struct ReaderState {
    last_report: Instant,
    hello: Option<ContractVersion>,
    dropped: u64,
    faults: u32,
    problems: Vec<String>,
}

impl ReaderState {
    fn new(now: Instant) -> Self {
        Self {
            last_report: now,
            hello: None,
            dropped: 0,
            faults: 0,
            problems: Vec::new(),
        }
    }
}

/// Starts the thread that turns a plugin's output into events.
fn spawn_reader(
    source: EventSource,
    mut output: BufReader<std::process::ChildStdout>,
    shared: Arc<Mutex<ReaderState>>,
    inbox: EventInbox,
    timeline: SessionTimeline,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("plugin-{}", source.as_str()))
        .spawn(move || loop {
            match read_line(&mut output, MAX_LINE_BYTES) {
                Ok(Line::End) => break,
                Ok(Line::Text(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    // The moment the report arrived, read here rather than
                    // anywhere later: everything downstream is arithmetic on
                    // this one reading (`crate::report`).
                    let received = timeline.now();
                    handle_line(&source, &line, received, &shared, &inbox);
                }
                Ok(Line::TooLong { bytes }) => {
                    tracing::warn!(
                        plugin = %source,
                        bytes,
                        "a plugin sent a line longer than this build will read, and it was \
                         discarded"
                    );
                    fault(&shared);
                }
                Err(error) => {
                    tracing::debug!(plugin = %source, %error, "a plugin's output ended");
                    break;
                }
            }
        })
        .expect("a thread can be started for a plugin that has already been started")
}

/// Interprets one line, and delivers whatever it turned out to be.
fn handle_line(
    source: &EventSource,
    line: &str,
    received: clipped_events::EventTime,
    shared: &Arc<Mutex<ReaderState>>,
    inbox: &EventInbox,
) {
    let report = match read_report(line) {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(plugin = %source, %error, "a plugin sent something this build could not read");
            fault(shared);
            return;
        }
    };

    let mut state = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.last_report = Instant::now();

    match report {
        PluginReport::Hello { contract } => state.hello = Some(contract),
        PluginReport::Alive => {}
        PluginReport::Problem { message } => {
            let message: String = message.chars().take(MAX_PROBLEM_BYTES).collect();
            tracing::info!(plugin = %source, problem = %message, "a plugin reported a problem");
            if state.problems.len() == KEPT_PROBLEMS {
                state.problems.remove(0);
            }
            state.problems.push(message);
        }
        PluginReport::Event(event) => match event.into_event(source, received) {
            Ok(event) => {
                if inbox.deliver(event).was_dropped() {
                    state.dropped += 1;
                }
            }
            Err(refusal) => {
                tracing::warn!(plugin = %source, %refusal, "a plugin's event was refused");
                state.faults = state.faults.saturating_add(1);
            }
        },
    }
}

/// Records one thing this build could not use.
fn fault(shared: &Arc<Mutex<ReaderState>>) {
    let mut state = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.faults = state.faults.saturating_add(1);
}

/// One line, or the end of the output.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// A line, without its newline.
    Text(String),
    /// A line longer than the limit. It has been discarded, up to and including
    /// its newline, so reading can continue with the next one.
    TooLong {
        /// How much was read before it was given up on.
        bytes: usize,
    },
    /// The output has closed.
    End,
}

/// Reads one line, refusing to allocate more than `limit` for it.
///
/// `BufRead::read_line` has no limit, which is the whole reason this exists: a
/// plugin that prints a gigabyte before its first newline would otherwise be a
/// gigabyte in this process.
fn read_line(reader: &mut impl BufRead, limit: usize) -> io::Result<Line> {
    let mut line: Vec<u8> = Vec::new();
    let mut discarded = 0_usize;

    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok(if line.is_empty() && discarded == 0 {
                Line::End
            } else if discarded > 0 {
                Line::TooLong {
                    bytes: discarded + line.len(),
                }
            } else {
                Line::Text(String::from_utf8_lossy(&line).into_owned())
            });
        }

        match available.iter().position(|byte| *byte == b'\n') {
            Some(end) => {
                let taken = end + 1;
                if discarded == 0 && line.len() + end <= limit {
                    line.extend_from_slice(&available[..end]);
                    reader.consume(taken);
                    // A line written on Windows arrives with a carriage return.
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return Ok(Line::Text(String::from_utf8_lossy(&line).into_owned()));
                }
                reader.consume(taken);
                return Ok(Line::TooLong {
                    bytes: discarded + line.len() + end,
                });
            }
            None => {
                let length = available.len();
                if line.len() + length > limit {
                    // Past the limit: stop keeping it, and keep reading until
                    // the newline so that the *next* line is still readable.
                    discarded += line.len() + length;
                    line.clear();
                } else {
                    line.extend_from_slice(available);
                }
                reader.consume(length);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{install_example, session, until, TemporaryDirectory};

    #[test]
    fn killing_a_hung_plugin_ends_the_thread_that_was_reading_it() {
        // The claim the process boundary is chosen for, checked rather than
        // asserted: a plugin that stops answering is reclaimed *and so is the
        // thread reading it*, because that thread is blocked on a pipe the
        // dead process was holding. A hung plugin inside this process would
        // leave a thread nothing could ever end, and the recorder runs for days
        // (AGENTS.md sections 58 and 59).
        let root = TemporaryDirectory::new("reader");
        let plugin = install_example(&root, "hanger", "misbehaving_plugin", "hang-plugin");
        let (inbox, receiver) = crate::inbox::inbox(4);
        let mut process = PluginProcess::spawn(
            &plugin,
            &session(),
            crate::report::SessionTimeline::starting_now(),
            inbox,
            Instant::now(),
        )
        .expect("the plugin can be started");

        until("the plugin to introduce itself", || {
            process.snapshot().hello.is_some()
        });
        assert!(
            !process.snapshot().reader_finished,
            "it is hung, not finished: its output is still open"
        );

        process.kill();
        until("the thread reading the plugin to end", || {
            process.snapshot().reader_finished
        });
        assert!(
            process.exit_status().is_some(),
            "and the process itself is gone"
        );
        drop(receiver);
    }

    #[test]
    fn lines_are_read_one_at_a_time_without_their_endings() {
        let mut reader = io::Cursor::new(b"first\nsecond\r\n".to_vec());
        assert_eq!(
            read_line(&mut reader, 64).expect("a line"),
            Line::Text("first".to_owned())
        );
        assert_eq!(
            read_line(&mut reader, 64).expect("a line"),
            Line::Text("second".to_owned()),
            "a carriage return is part of the line ending, not of the JSON"
        );
        assert_eq!(read_line(&mut reader, 64).expect("the end"), Line::End);
    }

    #[test]
    fn a_line_without_an_ending_is_still_read() {
        let mut reader = io::Cursor::new(b"{\"report\":\"alive\"}".to_vec());
        assert_eq!(
            read_line(&mut reader, 64).expect("a line"),
            Line::Text(r#"{"report":"alive"}"#.to_owned())
        );
    }

    #[test]
    fn an_endless_line_is_discarded_rather_than_allocated() {
        // The failure this exists for: a plugin that never sends a newline must
        // not be a plugin that owns as much of this process as it likes.
        let mut torrent = vec![b'x'; 4096];
        torrent.push(b'\n');
        torrent.extend_from_slice(b"{\"report\":\"alive\"}\n");
        let mut reader = io::Cursor::new(torrent);

        let read = read_line(&mut reader, 512).expect("a line");
        assert!(
            matches!(read, Line::TooLong { bytes } if bytes >= 512),
            "expected the line to be given up on, got {read:?}"
        );
        assert_eq!(
            read_line(&mut reader, 512).expect("a line"),
            Line::Text(r#"{"report":"alive"}"#.to_owned()),
            "and the plugin is still readable afterwards"
        );
    }
}
