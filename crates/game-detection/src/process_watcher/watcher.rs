//! The watcher itself: a source, a debounce, and a thread that owns neither.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use clipped_logging::RedactedPath;

use super::config::WatchConfig;
use super::debounce::LaunchDebouncer;
use super::error::{SourceError, WatchError};
use super::process::ProcessSnapshot;
use super::source::{EventSource, SourceMessage};
use super::windows::{baseline, Source};
use super::{Next, WatchEvent};

/// Reports processes starting and stopping, without polling for them.
///
/// # Using it
///
/// ```no_run
/// use std::time::Duration;
///
/// use clipped_game_detection::{Next, ProcessWatcher, WatchConfig, WatchEvent};
///
/// let mut watcher = ProcessWatcher::start(WatchConfig::default())?;
/// loop {
///     match watcher.next_event(Duration::from_secs(1)) {
///         Next::Event(WatchEvent::Launched(launch)) => {
///             println!("{} started", launch.newest().image_name);
///         }
///         Next::Event(WatchEvent::Exited(exit)) => {
///             println!("{} exited", exit.process.image_name);
///         }
///         Next::Event(other) => println!("{other:?}"),
///         // Nothing happened within the timeout, which is the usual answer.
///         Next::Idle => {}
///         // Every source is gone. Nothing further will ever arrive, so the
///         // loop ends rather than asking a watcher that has no answer left.
///         Next::Finished => break,
///     }
/// }
/// # Ok::<(), clipped_game_detection::WatchError>(())
/// ```
///
/// # Threading
///
/// The watcher is driven by whichever thread calls [`Self::next_event`], and it
/// is not `Sync`: one thread owns it. Behind it are one or two source threads
/// that do nothing but block on Windows and forward what it says; all
/// interpretation happens on the caller's thread, so a slow consumer delays
/// events rather than corrupting them (AGENTS.md section 20).
///
/// Dropping the watcher stops the source threads and waits for them, which
/// takes up to [`WatchConfig::notification_interval`] because the call they are
/// blocked in cannot be interrupted.
///
/// # What it costs when nothing is happening
///
/// Nothing on this thread: [`Self::next_event`] blocks until the source has
/// something or until the debounce has something due, and when neither is true
/// it does not wake at all. The measured idle cost of the whole arrangement,
/// including the work WMI does on its own account, is in
/// `docs/game-detection.md`.
#[derive(Debug)]
pub struct ProcessWatcher {
    config: WatchConfig,
    debouncer: LaunchDebouncer,
    source: Source,
    already_running: Vec<ProcessSnapshot>,
    /// Events produced by a drain and not yet handed to the caller.
    ready: VecDeque<WatchEvent>,
    /// Set once no source remains; the watcher then reports nothing further.
    stopped: bool,
}

impl ProcessWatcher {
    /// Starts watching.
    ///
    /// Takes a snapshot of what is already running before subscribing, so that
    /// a game started before Clipped still has an executable name to report
    /// when it exits. A process that starts in the gap between the snapshot and
    /// the subscription — a few tens of milliseconds — is invisible to this
    /// watcher for its lifetime. That is the one race here, it is bounded, and
    /// it is preferred to the alternative ordering, which produces a spurious
    /// exit for a process that is still running.
    ///
    /// # Errors
    ///
    /// [`WatchError::Baseline`] if the process table cannot be read at all, and
    /// [`WatchError::NoSource`] if neither the WMI subscription nor the
    /// snapshot fallback can be started. Both mean no detection is possible and
    /// should be reported to the user rather than retried in a loop.
    pub fn start(config: WatchConfig) -> Result<Self, WatchError> {
        let already_running = baseline().map_err(WatchError::Baseline)?;

        let mut debouncer = LaunchDebouncer::new(config);
        debouncer.seed(&already_running);

        let known: Vec<u32> = already_running.iter().map(|process| process.pid).collect();
        let source = Source::start(config, &known)?;

        tracing::info!(
            source = source.kind.as_str(),
            already_running = already_running.len(),
            "process watcher started"
        );

        Ok(Self {
            config,
            debouncer,
            source,
            already_running,
            ready: VecDeque::new(),
            stopped: false,
        })
    }

    /// Which mechanism is delivering events.
    #[must_use]
    pub fn source(&self) -> EventSource {
        self.source.kind
    }

    /// Why the preferred source was not used, if it was not.
    ///
    /// [`None`] when [`Self::source`] is the preferred one. Worth showing on a
    /// diagnostics screen: a machine on the fallback has slower and coarser
    /// detection, and the reason is the actionable part.
    #[must_use]
    pub fn declined_source(&self) -> Option<&SourceError> {
        self.source.declined.as_ref()
    }

    /// Everything that was running when the watcher started.
    ///
    /// These are not reported as launches — they did not start — but they are
    /// remembered, so their exits are reported. A caller that wants to know
    /// whether a game was already running asks here.
    #[must_use]
    pub fn already_running(&self) -> &[ProcessSnapshot] {
        &self.already_running
    }

    /// The next event, waiting up to `timeout` for one.
    ///
    /// [`Next::Idle`] means nothing happened in that time, which is the usual
    /// answer and is not a failure. [`Next::Finished`] means every source is
    /// gone and nothing further will ever arrive; [`WatchEvent::Stopped`] is
    /// delivered once before it, carrying the reason.
    ///
    /// The wait is not a poll: this thread sleeps until the source has
    /// something to say or until a debounce window closes, whichever is sooner,
    /// and if neither is pending it sleeps for the whole timeout. A finished
    /// watcher waits too. Answering instantly would be cheaper for this call
    /// and ruinous for the process — the loop above is written to run for days,
    /// and a call that always returns immediately turns it into a busy-wait on
    /// a processor from the moment WMI goes away.
    pub fn next_event(&mut self, timeout: Duration) -> Next {
        let deadline = Instant::now() + timeout;

        loop {
            if let Some(event) = self.ready.pop_front() {
                return Next::Event(event);
            }

            let now = Instant::now();
            let due = self.debouncer.drain(now);
            if !due.is_empty() {
                for event in &due {
                    record(event);
                }
                self.ready.extend(due);
                continue;
            }

            if now >= deadline {
                return if self.stopped {
                    Next::Finished
                } else {
                    Next::Idle
                };
            }

            let wake = self
                .debouncer
                .next_deadline()
                .map_or(deadline, |next| next.min(deadline));
            let waiting = wake.saturating_duration_since(now);

            if self.stopped {
                // Sleeping rather than waiting on the source: there is no
                // source. Its channel is disconnected, so a receive would
                // return at once and this loop would spin — which is the whole
                // point of doing it here instead.
                std::thread::sleep(waiting);
            } else {
                self.wait(waiting);
            }
        }
    }

    /// Waits for one message from the source and folds it into the debounce.
    fn wait(&mut self, duration: Duration) {
        use std::sync::mpsc::RecvTimeoutError;

        match self.source.events.recv_timeout(duration) {
            Ok(SourceMessage::Event(event)) => self.debouncer.observe(event, Instant::now()),
            Ok(SourceMessage::Lost(reason)) => self.replace_source(reason),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // Every source thread has ended without saying why, which is
                // what a panic in one of them looks like. Treated as a loss:
                // the alternative is a watcher that silently never reports
                // another game.
                self.replace_source(SourceError::new(
                    "process event source",
                    "the source ended without reporting a reason",
                ));
            }
        }
    }

    /// Falls back to the fallback source, or gives up and says so.
    fn replace_source(&mut self, reason: SourceError) {
        if self.source.kind == EventSource::SnapshotPolling {
            tracing::error!(error = %reason, "the process watcher has no source left");
            self.stopped = true;
            self.ready.push_back(WatchEvent::Stopped(reason));
            return;
        }

        let from = self.source.kind;
        let known = self.debouncer.known_pids();
        match Source::start_polling(self.config, &known) {
            Ok(source) => {
                let to = source.kind;
                // Replacing the source drops the old one, which stops its
                // threads and drops the channel they were writing to, so
                // nothing from the failed source can arrive after this point.
                self.source = source;
                tracing::warn!(
                    from = from.as_str(),
                    to = to.as_str(),
                    error = %reason,
                    "the process event source changed"
                );
                self.ready
                    .push_back(WatchEvent::SourceChanged { from, to, reason });
            }
            Err(error) => {
                tracing::error!(error = %error, "the process watcher has no source left");
                self.stopped = true;
                self.ready.push_back(WatchEvent::Stopped(reason));
            }
        }
    }
}

/// Writes what happened to the diagnostics.
///
/// The one place in the watcher that logs a process, so there is one place to
/// check that it does so safely. An executable path is a user path — it carries
/// the account name and the shape of somebody's games library — so it goes
/// through [`RedactedPath`], which keeps the file name and a stable digest of
/// the rest (docs/logging.md). The launch identifier is what ties the lines for
/// one game together.
///
/// At `debug` and not `info`, because nothing in this module knows what a game
/// is: these fire for Windows' own background churn, which an idle measurement
/// put at roughly six hundred lines an hour before a single game is detected.
/// The hourly-rotated logs exist to diagnose recordings, and that volume would
/// dominate them (AGENTS.md section 35). Whichever layer decides that a launch
/// *is* a game is the one with something worth an `info` line to say.
fn record(event: &WatchEvent) {
    match event {
        WatchEvent::Launched(launch) => {
            let newest = launch.newest();
            tracing::debug!(
                launch = launch.id.get(),
                processes = launch.processes.len(),
                pid = newest.pid,
                parent_pid = newest.parent_pid,
                image = %redacted(newest),
                "process launch detected"
            );
        }
        WatchEvent::Exited(exit) => {
            tracing::debug!(
                launch = exit.launch.get(),
                pid = exit.process.pid,
                image = %redacted(&exit.process),
                "watched process exited"
            );
        }
        // Both are logged where they are decided, with detail this function
        // does not have.
        WatchEvent::SourceChanged { .. } | WatchEvent::Stopped(_) => {}
    }
}

/// A process's executable, safe to log.
///
/// Falls back to the file name when there is no path, which is already just a
/// name; redacting it keeps the digest meaningful and costs nothing.
fn redacted(process: &ProcessSnapshot) -> RedactedPath {
    process
        .image_path
        .as_ref()
        .map_or_else(|| RedactedPath::new(&process.image_name), RedactedPath::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Short enough that the test is quick, long enough that a spin is
    /// unmistakable against it.
    const TIMEOUT: Duration = Duration::from_millis(200);

    /// Settings that keep these tests quick.
    ///
    /// The debounce windows are short because a machine running a test suite
    /// starts processes constantly, and everything the source said before it
    /// died is still owed to the caller; a default 2.5-second quiet period would
    /// have the drain below take that long for no benefit.
    fn config() -> WatchConfig {
        WatchConfig {
            notification_interval: Duration::from_secs(1),
            launch_quiet_period: Duration::from_millis(100),
            max_launch_window: Duration::from_millis(300),
            exit_settle_period: Duration::from_millis(50),
        }
    }

    /// A watcher whose sources have all been lost, reached the way a real one
    /// reaches that state.
    ///
    /// Nothing is faked and no private field is set to get there: sources are
    /// declared lost until none is left, which walks
    /// [`ProcessWatcher::replace_source`] down to the branch that gives up.
    ///
    /// How many there are to lose depends on the machine — one where WMI does
    /// not answer starts on the fallback already — so the loop stops when the
    /// watcher says it has, rather than counting.
    fn exhausted_watcher() -> ProcessWatcher {
        let mut watcher =
            ProcessWatcher::start(config()).expect("a watcher can be started on this machine");

        let mut stopped = false;
        for _ in 0..2 {
            watcher.replace_source(SourceError::new("test", "the source was lost"));
            match watcher.next_event(TIMEOUT) {
                Next::Event(WatchEvent::SourceChanged { .. }) => {}
                Next::Event(WatchEvent::Stopped(_)) => {
                    stopped = true;
                    break;
                }
                other => panic!("losing a source reported {other:?}"),
            }
        }
        assert!(
            stopped,
            "the watcher never ran out of sources, so nothing below is being tested"
        );

        // Whatever the sources reported while they still worked is owed to the
        // caller and is delivered even now, so it is drained here rather than
        // left to arrive in the middle of a measurement. Nothing can join it:
        // a stopped watcher never reads from a source again.
        while watcher.debouncer.next_deadline().is_some() || !watcher.ready.is_empty() {
            watcher.next_event(TIMEOUT);
        }

        watcher
    }

    #[test]
    fn a_watcher_with_no_source_left_waits_out_its_timeout_rather_than_spinning() {
        // A recorder runs for days. When WMI goes away, the consumer loop in
        // this module's own documentation must not become a busy-wait that
        // burns a processor for the rest of the session: a watcher that has
        // stopped is worse than useless if it also costs a core.
        let mut watcher = exhausted_watcher();

        let window = Duration::from_millis(800);
        let start = Instant::now();
        let mut answers = 0_u32;
        while start.elapsed() < window {
            assert_eq!(watcher.next_event(TIMEOUT), Next::Finished);
            answers += 1;
        }

        // Four full timeouts fit in the window; anything much above that is a
        // call returning without waiting at all.
        assert!(
            answers <= 6,
            "a stopped watcher answered {answers} times in {window:?}: it is spinning, not waiting"
        );
    }

    #[test]
    fn a_finished_watcher_is_not_confused_with_a_quiet_one() {
        // The two answers call for opposite responses — keep waiting, or tell
        // the user detection has stopped — so they must not be spelled the same
        // way. Before this distinction existed both were `None`.
        let mut live =
            ProcessWatcher::start(config()).expect("a watcher can be started on this machine");

        // An idle machine may genuinely start something during the wait, so the
        // assertion is that a live watcher never says `Finished`, not that it
        // says `Idle`.
        assert_ne!(
            live.next_event(TIMEOUT),
            Next::Finished,
            "a watcher with a working source has not finished"
        );

        assert_eq!(exhausted_watcher().next_event(TIMEOUT), Next::Finished);
    }
}
