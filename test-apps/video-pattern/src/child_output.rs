//! Reading a test application's standard output without ever blocking for
//! ever.
//!
//! # Why a thread and a channel rather than a read
//!
//! A test application announces itself on standard output and a test waits for
//! that line before it does anything. Waiting for it with [`BufRead::read_line`]
//! is the obvious thing and is wrong twice over:
//!
//! - **A child that never writes anything blocks the test for ever**, and there
//!   is no portable way to put a deadline on a pipe read. A test that hangs
//!   reports nothing, which is worse than one that fails: it is a build nobody
//!   can read and, on a machine somebody is using, a test application still
//!   rendering a tone into their speakers.
//! - **The pipe has to keep being read for the whole run.** A child whose
//!   standard output fills up blocks in `write` and stops doing whatever else it
//!   was doing — the subject freezes because the test stopped listening.
//!
//! So the pipe is drained by a thread of its own from the moment the process
//! starts, and the test takes lines out of a channel with
//! [`Receiver::recv_timeout`]. Every wait a harness makes is then bounded by
//! construction rather than by a deadline that is only looked at between lines.
//!
//! # Who uses it
//!
//! Both test-application harnesses: [`crate::harness::TestApp`] here, and
//! `clipped_process_tree_audio::harness::ToneSubject` for the audio subject.
//! They word their failures differently, because a window that never appeared
//! and a tone that never started are different sentences, so this module
//! answers [`NoLine`] and leaves the wording to them. The mechanism is shared,
//! which is the part that must not exist twice (AGENTS.md section 55).

use core::time::Duration;
use std::io::{BufRead, BufReader};
use std::process::ChildStdout;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;

/// The lines a child has printed, as they arrive.
pub type Lines = Receiver<std::io::Result<String>>;

/// Why there is no next line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoLine {
    /// The pipe could not be read, in the operating system's words.
    Unreadable(String),
    /// Nothing arrived within the time the caller was prepared to wait.
    Silent(Duration),
    /// Standard output reached end of file: the process has gone, or closed it.
    Ended,
}

/// Starts draining `stdout` on a thread of its own.
///
/// The thread ends when the pipe closes, which happens when the application
/// exits — every harness in this workspace guarantees that it does, on the way
/// out of a test that passed, failed or panicked.
///
/// # Errors
///
/// The reader thread could not be started.
pub fn reading(stdout: ChildStdout) -> std::io::Result<Lines> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("test-app-output".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    // The test has gone. Its harness's `Drop` is dealing with
                    // the process.
                    break;
                }
            }
        })?;
    Ok(receiver)
}

/// Takes the next line the application printed, giving up after `timeout`.
///
/// # Errors
///
/// [`NoLine`], for the caller to word.
pub fn next_line_within(lines: &Lines, timeout: Duration) -> Result<String, NoLine> {
    match lines.recv_timeout(timeout) {
        Ok(Ok(line)) => Ok(line.trim_end().to_owned()),
        Ok(Err(source)) => Err(NoLine::Unreadable(source.to_string())),
        Err(RecvTimeoutError::Timeout) => Err(NoLine::Silent(timeout)),
        Err(RecvTimeoutError::Disconnected) => Err(NoLine::Ended),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long enough that a loaded machine cannot mistake scheduling for a line,
    /// short enough that the test which proves the bound is not itself a wait.
    const PATIENCE: Duration = Duration::from_millis(200);

    #[test]
    fn a_line_that_has_arrived_is_taken_and_its_ending_trimmed() {
        let (sender, lines) = mpsc::channel();
        sender
            .send(Ok("ready pid=1234 role=parent\r\n".to_owned()))
            .expect("the receiver is alive");

        assert_eq!(
            next_line_within(&lines, PATIENCE).as_deref(),
            Ok("ready pid=1234 role=parent")
        );
    }

    #[test]
    fn a_child_that_says_nothing_gives_up_rather_than_waiting_for_ever() {
        // The whole reason this module exists. The sender is held, so the
        // channel is neither closed nor written to — a running process that has
        // announced nothing, which is the shape of every hang this replaced.
        let (_sender, lines) = mpsc::channel::<std::io::Result<String>>();

        let started = std::time::Instant::now();
        let outcome = next_line_within(&lines, PATIENCE);

        assert_eq!(outcome, Err(NoLine::Silent(PATIENCE)));
        assert!(
            started.elapsed() < PATIENCE * 10,
            "the wait was not bounded: it took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_child_that_has_gone_is_told_apart_from_one_that_is_merely_quiet() {
        // Different sentences for the caller: a process that exited without
        // announcing itself is a defect in the subject, and one that is still
        // running and silent is a machine that is too slow or a subject that is
        // wedged. A harness that reported both the same way would send whoever
        // read it to the wrong place.
        let (sender, lines) = mpsc::channel::<std::io::Result<String>>();
        drop(sender);

        assert_eq!(next_line_within(&lines, PATIENCE), Err(NoLine::Ended));
    }

    #[test]
    fn a_pipe_that_could_not_be_read_says_so_in_the_systems_words() {
        let (sender, lines) = mpsc::channel();
        sender
            .send(Err(std::io::Error::other("the pipe broke")))
            .expect("the receiver is alive");

        assert_eq!(
            next_line_within(&lines, PATIENCE),
            Err(NoLine::Unreadable("the pipe broke".to_owned()))
        );
    }
}
