//! Driving the subject from a test.
//!
//! A capture test needs a subject that is running and identifiable before it
//! starts capturing, and needs it gone afterwards whether the test passed,
//! failed or panicked — with nothing left rendering a tone on a machine other
//! people are using. That is fiddly enough to be worth writing once.
//!
//! ```no_run
//! use core::time::Duration;
//! use clipped_process_tree_audio::harness::ToneSubject;
//!
//! // In a test inside this package, write `env!("CARGO_BIN_EXE_process-tree-audio")`
//! // here: Cargo sets that to the binary it just built.
//! let mut parent = ToneSubject::start("target/debug/process-tree-audio.exe", &[])?;
//! let tree_root = parent.pid();
//!
//! // Start capturing `tree_root` here, and only then ask for the child that
//! // makes the noise.
//! let child = parent.spawn_child(Duration::from_secs(10))?;
//!
//! parent.stop();
//! # Ok::<(), String>(())
//! ```
//!
//! # Skipping
//!
//! Every test here needs a machine that can play a sound, and every one of them
//! makes one. [`suppressed`] and [`skipped`] are the two environment variables
//! that decide what happens on a machine that cannot, or that has been asked
//! for quiet — they live here rather than in a test file because there is more
//! than one test file and a second copy of a skip is a skip that stops matching
//! (AGENTS.md section 55).
//!
//! # Ownership
//!
//! [`ToneSubject`] owns the process and its pipes. [`Drop`] closes standard
//! input — which is how the application is asked to stop — waits briefly, and
//! kills it if it has not gone. There is no path, panic included, on which the
//! subject outlives the test that started it (AGENTS.md sections 25 and 58).
//!
//! # Every wait here is bounded
//!
//! Nothing in this file blocks on a pipe. The subject's standard output is
//! drained by a thread from the moment it starts and read through
//! [`clipped_video_pattern::child_output`], the same mechanism the video
//! subject's harness uses, so a subject that starts and then says nothing — a
//! player wedged in the audio endpoint it is opening, say — fails its test in
//! seconds rather than hanging the run. That distinction is not theoretical
//! here: an unbounded `child.wait()` in `plugins/cs2/tests/plugin_process.rs`
//! once cost a six-hour continuous-integration job.

use core::time::Duration;
use std::ffi::OsStr;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use clipped_video_pattern::child_output::{self, Lines, NoLine};

/// How long [`Drop`] gives the subject to stop cleanly before killing it.
const DROP_GRACE: Duration = Duration::from_secs(2);

/// How long [`ToneSubject::start`] waits for the subject's first line.
///
/// Generous, because what a player does before it can announce anything is open
/// a shared-mode audio endpoint, and a machine that is cold, busy or already
/// recording can take seconds over it. Bounded all the same: a subject that
/// never announces itself is a test that never finishes, and the seconds saved
/// by a tighter number are not worth a run that never ends.
pub const ANNOUNCEMENT_PATIENCE: Duration = Duration::from_secs(30);

/// A running subject.
#[derive(Debug)]
pub struct ToneSubject {
    process: Child,
    output: Lines,
    pid: u32,
    /// What the subject said about its own sound, if it is a player.
    tone: Option<PlayingTone>,
}

/// What a player reported once its tone was reaching the endpoint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayingTone {
    /// The frequency it is rendering, in hertz.
    pub frequency: f32,
    /// The endpoint's sample rate.
    pub rate: u32,
    /// The endpoint's channel count.
    pub channels: u16,
}

impl ToneSubject {
    /// Starts the subject and waits for it to announce itself.
    ///
    /// # Errors
    ///
    /// A sentence saying why there is nothing to capture: the process would not
    /// start, it said nothing, or — for a player — this machine cannot play a
    /// tone at all, which is a reason to skip a test rather than to fail one.
    pub fn start(executable: impl AsRef<OsStr>, arguments: &[&str]) -> Result<Self, String> {
        Self::start_within(executable, arguments, ANNOUNCEMENT_PATIENCE)
    }

    /// Starts the subject and waits `patience` for it to announce itself.
    ///
    /// [`start`](Self::start) with the wait named. Every test takes the
    /// default; this exists for the one that proves the wait is really bounded,
    /// which has to point the harness at a program that announces nothing and
    /// cannot spend [`ANNOUNCEMENT_PATIENCE`] finding that out.
    ///
    /// # Errors
    ///
    /// [`start`](Self::start)'s, plus: the subject printed nothing within
    /// `patience`. The process is stopped before that is reported — a subject
    /// nobody is listening to any more must not be left running, least of all
    /// one holding an audio endpoint (AGENTS.md section 58).
    pub fn start_within(
        executable: impl AsRef<OsStr>,
        arguments: &[&str],
        patience: Duration,
    ) -> Result<Self, String> {
        let mut process = Command::new(executable)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("the test subject would not start: {error}"))?;
        let stdout = process
            .stdout
            .take()
            .expect("standard output was asked for");
        let output = match child_output::reading(stdout) {
            Ok(output) => output,
            Err(error) => {
                // The process is this function's to clean up until the struct
                // that owns it exists.
                let _ = process.kill();
                let _ = process.wait();
                return Err(format!("the subject's output could not be read: {error}"));
            }
        };

        let mut subject = Self {
            process,
            output,
            pid: 0,
            tone: None,
        };

        // Every failure from here leaves through `subject`, so `Drop` stops the
        // process: no early return abandons a running player.
        let announcement = subject
            .next_line(patience)
            .map_err(|reason| silence(&reason))?;
        if let Some(reason) = announcement.strip_prefix("unavailable reason=") {
            return Err(reason.to_owned());
        }
        subject.pid = field(&announcement, "pid")
            .ok_or_else(|| format!("the subject announced itself as {announcement:?}"))?
            .parse()
            .map_err(|error| format!("the subject's identifier was not a number: {error}"))?;
        subject.tone = playing_tone(&announcement);

        Ok(subject)
    }

    /// The subject's process identifier.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// What this subject is playing, if it is a player.
    #[must_use]
    pub fn tone(&self) -> Option<PlayingTone> {
        self.tone
    }

    /// Asks a parent to start the child that makes the noise, and waits for it.
    ///
    /// Answers the child's process identifier and what it is playing. The wait
    /// is bounded by `patience`, because a test that hangs reports nothing.
    ///
    /// # Errors
    ///
    /// A sentence saying why there is no child playing anything, which for a
    /// machine that cannot render a tone is a reason to skip.
    pub fn spawn_child(&mut self, patience: Duration) -> Result<(u32, PlayingTone), String> {
        self.request_child("spawn", patience)
    }

    /// Asks a parent to start another child, playing `frequency`.
    ///
    /// The same thing as [`spawn_child`](Self::spawn_child) with the tone
    /// named, and a parent takes as many as it is sent. It is what a test asks
    /// for when a tree is **already** making a noise and something new has to
    /// join it: the joiner's audio can only be found in a track that already
    /// holds its sibling's if the two are at different frequencies.
    ///
    /// # Errors
    ///
    /// [`spawn_child`](Self::spawn_child)'s.
    pub fn spawn_child_playing(
        &mut self,
        frequency: f32,
        patience: Duration,
    ) -> Result<(u32, PlayingTone), String> {
        self.request_child(&format!("spawn {frequency}"), patience)
    }

    /// Sends one spawn command and waits for the child it produces.
    fn request_child(
        &mut self,
        command: &str,
        patience: Duration,
    ) -> Result<(u32, PlayingTone), String> {
        self.send(command)?;

        // The deadline bounds the whole exchange rather than each line: a
        // parent printing something that is not a `child` line every few
        // milliseconds must not extend the wait for ever, and one printing
        // nothing at all must not stop the deadline being looked at. Both come
        // from asking `next_line` for only the time that is left.
        let deadline = Instant::now() + patience;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            // Not merely tidiness before a zero-length wait: a subject printing
            // lines that are not `child` lines faster than they are read would
            // otherwise hand this loop a line on every turn for ever, and the
            // deadline it has already passed would never be acted on.
            if left.is_zero() {
                return Err(no_child(patience));
            }
            let line = match self.next_line(left) {
                Ok(line) => line,
                // Running out of time is this wait's failure and is worded as
                // one. A subject that stopped, or whose pipe broke, failed in
                // its own right and says so in its own words.
                Err(NoLine::Silent(_)) => return Err(no_child(patience)),
                Err(reason) => return Err(silence(&reason)),
            };
            let Some(rest) = line.strip_prefix("child ") else {
                continue;
            };
            if let Some(reason) = rest.split_once("unavailable reason=") {
                return Err(reason.1.to_owned());
            }
            let pid = field(rest, "pid")
                .ok_or_else(|| format!("the child was announced as {rest:?}"))?
                .parse()
                .map_err(|error| format!("the child's identifier was not a number: {error}"))?;
            let tone = playing_tone(rest)
                .ok_or_else(|| format!("the child did not say what it is playing: {rest:?}"))?;
            return Ok((pid, tone));
        }
    }

    /// Stops the subject and waits for it to go.
    pub fn stop(&mut self) {
        // Closing standard input is the documented way to ask: the subject sees
        // end of file and ends its child with it.
        let _ = self.process.stdin.take();
        let deadline = Instant::now() + DROP_GRACE;
        while Instant::now() < deadline {
            match self.process.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.process.kill();
        let _ = self.process.wait();
    }

    /// Sends one command.
    fn send(&mut self, command: &str) -> Result<(), String> {
        use std::io::Write as _;

        let input = self
            .process
            .stdin
            .as_mut()
            .ok_or_else(|| "the subject has already been stopped".to_owned())?;
        writeln!(input, "{command}")
            .and_then(|()| input.flush())
            .map_err(|error| format!("the subject stopped listening: {error}"))
    }

    /// The next line the subject printed, waiting no longer than `timeout`.
    ///
    /// The reason is left unworded because the callers word it differently: a
    /// subject that never announced itself and a parent that never started a
    /// child are different failures to read at three in the morning.
    fn next_line(&mut self, timeout: Duration) -> Result<String, NoLine> {
        child_output::next_line_within(&self.output, timeout)
    }
}

impl Drop for ToneSubject {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What a missing line means, for a caller with nothing more specific to say.
fn silence(reason: &NoLine) -> String {
    match reason {
        NoLine::Unreadable(error) => format!("the subject's output could not be read: {error}"),
        NoLine::Silent(timeout) => format!(
            "the subject said nothing within {:.1}s",
            timeout.as_secs_f64()
        ),
        NoLine::Ended => "the subject stopped without saying anything".to_owned(),
    }
}

/// What a parent that did not start a child in time is reported as.
fn no_child(patience: Duration) -> String {
    format!(
        "the subject did not start a child within {:.1}s",
        patience.as_secs_f64()
    )
}

/// The value of `name=` in one of the subject's lines.
fn field<'line>(line: &'line str, name: &str) -> Option<&'line str> {
    line.split_whitespace()
        .filter_map(|part| part.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// The environment variable that turns "this machine cannot do this" from a
/// skip into a failure.
const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

/// The environment variable that asks the tests which make a noise not to.
const SKIP_AUDIO: &str = "CLIPPED_SKIP_AUDIO";

/// Whether `name` is set to anything at all.
fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Whether the caller should skip because the machine has been asked for quiet.
///
/// Consulted before anything is started, which is the difference between this
/// and [`skipped`]: by the time a test has discovered it cannot run, it has
/// already made whatever noise it was going to make.
///
/// # Panics
///
/// When `CLIPPED_SKIP_AUDIO` and `CLIPPED_REQUIRE_AUDIO` are both set. One says
/// these tests must not run and the other says they must not be skipped; there
/// is no behaviour that satisfies both, so neither is guessed at.
#[must_use]
pub fn suppressed() -> bool {
    if !is_set(SKIP_AUDIO) {
        return false;
    }
    assert!(
        !is_set(REQUIRE_AUDIO),
        "{SKIP_AUDIO} and {REQUIRE_AUDIO} are both set. One says these tests must not run and \
         the other says they must not be skipped; there is no behaviour that satisfies both, so \
         neither is being guessed at."
    );
    skipped(&format!("{SKIP_AUDIO} is set"));
    true
}

/// Reports that the test could not run here.
///
/// Written through `std::io::stderr()` rather than with `eprintln!` because
/// libtest captures the macros, and a skip nobody can see is how a test quietly
/// stops testing anything.
///
/// # Panics
///
/// When `CLIPPED_REQUIRE_AUDIO` is set, which is what a run whose numbers are
/// being recorded should use: a test that decided for itself it could not run
/// reads as a pass, and this is the only thing that stops it.
pub fn skipped(reason: &str) {
    use std::io::Write as _;

    assert!(
        !is_set(REQUIRE_AUDIO),
        "{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}"
    );
    let _ = writeln!(std::io::stderr(), "SKIPPED (audio): {reason}");
}

/// What a `role=player` line says is being played.
fn playing_tone(line: &str) -> Option<PlayingTone> {
    if field(line, "role") != Some("player") {
        return None;
    }
    Some(PlayingTone {
        frequency: field(line, "frequency")?.parse().ok()?,
        rate: field(line, "rate")?.parse().ok()?,
        channels: field(line, "channels")?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_players_announcement_is_read_back_as_what_it_is_playing() {
        let line = "ready pid=1234 role=player frequency=997 amplitude=0.04 rate=48000 \
                    channels=2";

        assert_eq!(field(line, "pid"), Some("1234"));
        assert_eq!(
            playing_tone(line),
            Some(PlayingTone {
                frequency: 997.0,
                rate: 48_000,
                channels: 2,
            })
        );
    }

    #[test]
    fn a_parent_is_not_playing_anything() {
        // The distinction the whole subject is built on: the parent is silent
        // and its child is not. A harness that read the parent as a player
        // would have a test asserting a tone against a process that never made
        // one.
        assert_eq!(playing_tone("ready pid=1234 role=parent"), None);
        assert_eq!(field("ready pid=1234 role=parent", "role"), Some("parent"));
    }

    #[test]
    fn a_missing_field_is_missing_rather_than_guessed_at() {
        assert_eq!(field("ready pid=1234", "frequency"), None);
        assert_eq!(playing_tone("ready pid=1234 role=player"), None);
        assert_eq!(field("", "pid"), None);
    }
}
