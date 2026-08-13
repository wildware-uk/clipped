//! Who starts plugins, who notices when one has gone wrong, and what a
//! recording is allowed to depend on.
//!
//! # The shape of it
//!
//! ```text
//!  recording session                     supervisor                    plugin process
//!  ─────────────────                     ──────────                    ──────────────
//!  attach(enabled, session) ──────────▶  spawn ─────────────────────▶  {"report":"hello"}
//!                                        reader thread  ◀───────────   {"report":"event",…}
//!  drain the inbox  ◀───────────────────  bounded queue
//!  poll(now)        ──────────────────▶  exited? silent? flooding?
//!                                        kill / replace / disable ──▶
//!  detach()         ──────────────────▶  detach, then close its input
//! ```
//!
//! **The session's arrow points one way.** It hands over a plugin and then
//! drains a queue; it never calls into a plugin, never waits for one, and is
//! never given the chance to. That is the whole of "a misbehaving plugin cannot
//! affect capture" (AGENTS.md sections 16, 17 and 20), and it is a property of
//! the shape rather than of the plugins.
//!
//! # Polling, and why there is no thread doing it
//!
//! Everything time-based — a plugin that has not said hello, one that has gone
//! quiet, one whose replacement is due — happens in [`PluginSupervisor::poll`],
//! which the owner calls with a clock reading. **Call it about once a second,
//! from any thread that is not the capture thread.** A supervisor that is not
//! polled still costs a recording nothing: events keep arriving, the queue keeps
//! bounding them, and what does not happen is a hung plugin being reclaimed.
//!
//! It is not a thread of its own because a supervision thread would be a second
//! clock, a second place to reason about shutdown, and a thread that outlives
//! whatever created it. `clipped_session::automatic` decided the same thing for
//! the same reason: the rules are a state machine over a clock reading the
//! caller supplies, which is also what makes every rule here testable without
//! waiting for one (`crate::supervision`).
//!
//! One thread per *running plugin* is unavoidable and is the reader
//! (`crate::process`): a pipe has no timed read.

use core::time::Duration;
use std::time::Instant;

use crate::discovery::EnabledPlugin;
use crate::inbox::{inbox, EventInbox, EventReceiver, InboxStats, DEFAULT_CAPACITY};
use crate::manifest::PluginId;
use crate::process::PluginProcess;
use crate::report::{SessionDetails, SessionTimeline};
use crate::supervision::{PluginTrouble, Recovery, RestartLedger, SupervisionPolicy};

/// Runs the plugins attached to one recording session.
#[derive(Debug)]
pub struct PluginSupervisor {
    policy: SupervisionPolicy,
    inbox: EventInbox,
    plugins: Vec<SupervisedPlugin>,
    reportable: Vec<SupervisionEvent>,
}

impl PluginSupervisor {
    /// A supervisor and the end of the queue a recording drains.
    ///
    /// The receiver is returned rather than held so that the recording owns it:
    /// a session that has finished with plugins drops the receiver, and every
    /// subsequent delivery is counted and discarded rather than kept for
    /// nobody.
    #[must_use]
    pub fn new(policy: SupervisionPolicy) -> (Self, EventReceiver) {
        Self::with_capacity(policy, DEFAULT_CAPACITY)
    }

    /// As [`new`](Self::new), with a queue of `capacity` events.
    #[must_use]
    pub fn with_capacity(policy: SupervisionPolicy, capacity: usize) -> (Self, EventReceiver) {
        let (inbox, receiver) = inbox(capacity);
        (
            Self {
                policy,
                inbox,
                plugins: Vec::new(),
                reportable: Vec::new(),
            },
            receiver,
        )
    }

    /// Starts `plugin` against a session.
    ///
    /// Returns as soon as the process has been created; whether it started
    /// properly is reported by [`poll`](Self::poll), because "the executable is
    /// missing" and "the plugin never introduced itself" are the same kind of
    /// fact as "it stopped an hour later" and belong in one place.
    ///
    /// The caller decides which plugins to attach, using
    /// [`EnabledPlugin::supports`]. Attaching one that does not support the
    /// process is allowed and is nobody's error: a plugin's manifest is what
    /// answers that question, and a caller that has already answered it should
    /// not be made to answer it twice.
    pub fn attach(
        &mut self,
        plugin: EnabledPlugin,
        session: SessionDetails,
        timeline: SessionTimeline,
        now: Instant,
    ) {
        let mut supervised = SupervisedPlugin {
            id: plugin.id().clone(),
            plugin,
            session,
            timeline,
            state: PluginState::Starting,
            ledger: RestartLedger::default(),
            process: None,
            dropped_before: 0,
            faults_before: 0,
            problems_reported: 0,
        };
        Supervising {
            policy: &self.policy,
            inbox: &self.inbox,
            reportable: &mut self.reportable,
        }
        .start(&mut supervised, now);
        self.plugins.push(supervised);
    }

    /// Looks at every plugin, and acts.
    ///
    /// Everything the supervisor has to say comes out of here: a plugin that
    /// became ready, one that reported a problem, one being replaced, one being
    /// given up on. Call it about once a second.
    pub fn poll(&mut self, now: Instant) -> Vec<SupervisionEvent> {
        // Destructured rather than borrowed through `self`, so that examining
        // one plugin can also append to the supervisor's report: the borrows
        // are disjoint and the compiler can see that they are.
        let Self {
            policy,
            inbox,
            plugins,
            reportable,
        } = self;
        for plugin in plugins.iter_mut() {
            Supervising {
                policy,
                inbox,
                reportable,
            }
            .examine(plugin, now);
        }
        std::mem::take(&mut self.reportable)
    }

    /// Asks every plugin to finish.
    ///
    /// Returns at once. A plugin that has not exited by the time
    /// [`SupervisionPolicy::stop_grace`] has passed is killed at the next
    /// [`poll`](Self::poll), and dropping the supervisor kills anything still
    /// running — a plugin must not outlive the session that started it, whether
    /// or not that session ended tidily.
    pub fn detach(&mut self, now: Instant) {
        for plugin in &mut self.plugins {
            if let Some(process) = plugin.process.as_mut() {
                process.ask_to_stop(now);
                plugin.state = PluginState::Stopping;
            } else {
                plugin.state = PluginState::Stopped;
            }
        }
    }

    /// What every attached plugin is doing.
    #[must_use]
    pub fn health(&self) -> Vec<PluginHealth> {
        self.plugins
            .iter()
            .map(|plugin| {
                let snapshot = plugin.process.as_ref().map(PluginProcess::snapshot);
                PluginHealth {
                    plugin: plugin.id.clone(),
                    state: plugin.state.clone(),
                    dropped: plugin.dropped_before
                        + snapshot.as_ref().map_or(0, |snapshot| snapshot.dropped),
                    faults: plugin.faults_before
                        + snapshot.as_ref().map_or(0, |snapshot| snapshot.faults),
                    still_reading: snapshot
                        .as_ref()
                        .is_some_and(|snapshot| !snapshot.reader_finished),
                    problems: snapshot
                        .map(|snapshot| snapshot.problems)
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// What the queue has seen, across every plugin.
    #[must_use]
    pub fn inbox_stats(&self) -> InboxStats {
        self.inbox.stats()
    }
}

/// The supervisor's own state, borrowed apart so that one plugin can be
/// examined while the report is being written.
#[derive(Debug)]
struct Supervising<'a> {
    policy: &'a SupervisionPolicy,
    inbox: &'a EventInbox,
    reportable: &'a mut Vec<SupervisionEvent>,
}

impl Supervising<'_> {
    /// Starts, or restarts, one plugin's process.
    fn start(&mut self, plugin: &mut SupervisedPlugin, now: Instant) {
        match PluginProcess::spawn(
            &plugin.plugin,
            &plugin.session,
            plugin.timeline,
            self.inbox.clone(),
            now,
        ) {
            Ok(process) => {
                tracing::info!(plugin = %plugin.id, "plugin started");
                plugin.process = Some(process);
                plugin.state = PluginState::Starting;
                plugin.ledger.started(now);
            }
            Err(error) => {
                plugin.process = None;
                self.give_up_or_replace(
                    plugin,
                    PluginTrouble::CouldNotStart {
                        because: error.to_string(),
                    },
                    now,
                    Restartable::Yes,
                );
            }
        }
    }

    /// One plugin's turn.
    fn examine(&mut self, plugin: &mut SupervisedPlugin, now: Instant) {
        match plugin.state {
            PluginState::Disabled { .. } | PluginState::Stopped => {}
            PluginState::WaitingToRestart { at } => {
                if now >= at {
                    self.start(plugin, now);
                }
            }
            PluginState::Stopping => {
                let finished = if let Some(process) = plugin.process.as_mut() {
                    if process.exit_status().is_some() {
                        true
                    } else if process.outstayed_its_welcome(now, self.policy.stop_grace) {
                        tracing::info!(
                            plugin = %plugin.id,
                            "plugin did not finish when asked, and was stopped"
                        );
                        process.kill();
                        true
                    } else {
                        false
                    }
                } else {
                    true
                };
                if finished {
                    plugin.process = None;
                    plugin.state = PluginState::Stopped;
                }
            }
            PluginState::Starting | PluginState::Running => self.examine_running(plugin, now),
        }
    }

    /// A plugin that is supposed to be working.
    fn examine_running(&mut self, plugin: &mut SupervisedPlugin, now: Instant) {
        let Some(process) = plugin.process.as_mut() else {
            plugin.state = PluginState::Stopped;
            return;
        };

        let snapshot = process.snapshot();
        for problem in snapshot.problems.iter().skip(plugin.problems_reported) {
            self.reportable.push(SupervisionEvent::Problem {
                plugin: plugin.id.clone(),
                message: problem.clone(),
            });
        }
        plugin.problems_reported = snapshot.problems.len();

        // A plugin that has already exited is reported as having exited, before
        // anything else: "it stopped" is a better answer than "it went quiet",
        // and both are true of a process that is no longer there.
        if let Some(status) = process.exit_status() {
            let trouble = PluginTrouble::Exited {
                code: status.code(),
            };
            self.stop_and_recover(plugin, trouble, now, Restartable::Yes);
            return;
        }

        // What the running executable says, rather than only what its manifest
        // said: an update can replace one without the other.
        if let Some(contract) = snapshot.hello {
            if !contract.is_supported() {
                let trouble = PluginTrouble::WrongContract {
                    contract: contract.number(),
                };
                self.stop_and_recover(plugin, trouble, now, Restartable::No);
                return;
            }
            if plugin.state == PluginState::Starting {
                plugin.state = PluginState::Running;
                self.reportable.push(SupervisionEvent::Ready {
                    plugin: plugin.id.clone(),
                });
            }
        } else if now.duration_since(snapshot.started_at) >= self.policy.hello_timeout {
            let trouble = PluginTrouble::NeverStarted {
                waited: self.policy.hello_timeout,
            };
            self.stop_and_recover(plugin, trouble, now, Restartable::Yes);
            return;
        }

        if snapshot.dropped > self.policy.dropped_event_budget {
            // Not restartable, deliberately. A plugin that outruns a recording
            // does it because of what it is, and a replacement floods the same
            // queue a second later — while the events being lost are the ones
            // this whole subsystem exists to record.
            let trouble = PluginTrouble::Flooded {
                dropped: plugin.dropped_before + snapshot.dropped,
            };
            self.stop_and_recover(plugin, trouble, now, Restartable::No);
            return;
        }

        if snapshot.faults > self.policy.protocol_fault_budget {
            let trouble = PluginTrouble::Unreadable {
                faults: plugin.faults_before + snapshot.faults,
            };
            self.stop_and_recover(plugin, trouble, now, Restartable::No);
            return;
        }

        // Silence is only asked of a plugin that has introduced itself. Before
        // that there is exactly one question — has it started? — and
        // `hello_timeout` is the budget for it. A plugin that has never spoken
        // is not one that stopped speaking, and judging the same interval by
        // both numbers charges a slow start to whichever of the two happens to
        // be smaller: on a loaded machine that reports a plugin the operating
        // system took a moment to load as one that hung (#405).
        if snapshot.hello.is_some() {
            let quiet_for = now.duration_since(snapshot.last_report);
            if quiet_for >= self.policy.silence_timeout {
                // The case a separate process is chosen for: this kills
                // something that will never answer, which is not possible for a
                // thread.
                let trouble = PluginTrouble::Silent { quiet_for };
                self.stop_and_recover(plugin, trouble, now, Restartable::Yes);
            }
        }
    }

    /// Ends a plugin's process and decides whether it comes back.
    fn stop_and_recover(
        &mut self,
        plugin: &mut SupervisedPlugin,
        trouble: PluginTrouble,
        now: Instant,
        restartable: Restartable,
    ) {
        if let Some(mut process) = plugin.process.take() {
            // Killed before its counters are read, rather than after: the
            // reader thread is still working while this runs, and reading first
            // would leave whatever it did in between attributed to nobody.
            // Anything already buffered in the pipe is still lost to this
            // count, which is why what a plugin is charged with can be slightly
            // less than what the queue as a whole recorded.
            process.kill();
            let snapshot = process.snapshot();
            plugin.dropped_before += snapshot.dropped;
            plugin.faults_before += snapshot.faults;
            plugin.problems_reported = 0;
        }
        self.give_up_or_replace(plugin, trouble, now, restartable);
    }

    /// The restart decision, and the report of it.
    fn give_up_or_replace(
        &mut self,
        plugin: &mut SupervisedPlugin,
        trouble: PluginTrouble,
        now: Instant,
        restartable: Restartable,
    ) {
        let recovery = match restartable {
            Restartable::No => Recovery::GiveUp,
            Restartable::Yes => plugin.ledger.troubled(now, self.policy),
        };

        match recovery {
            Recovery::Replace { attempt, after } => {
                tracing::warn!(
                    plugin = %plugin.id,
                    %trouble,
                    attempt,
                    "plugin is being started again"
                );
                plugin.state = PluginState::WaitingToRestart { at: now + after };
                self.reportable.push(SupervisionEvent::Restarting {
                    plugin: plugin.id.clone(),
                    trouble,
                    attempt,
                    after,
                });
            }
            Recovery::GiveUp => {
                tracing::warn!(plugin = %plugin.id, %trouble, "plugin disabled");
                plugin.state = PluginState::Disabled {
                    trouble: trouble.clone(),
                };
                self.reportable.push(SupervisionEvent::Disabled {
                    plugin: plugin.id.clone(),
                    trouble,
                });
            }
        }
    }
}

/// Whether a kind of trouble is worth another attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restartable {
    /// A crash, a hang, or a plugin that could not be started: possibly
    /// transient — a game that had not finished starting, a port that was
    /// briefly taken.
    Yes,
    /// A flood, an unreadable stream, or the wrong contract version: a
    /// replacement does exactly the same thing.
    No,
}

/// What a supervisor has to say about a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionEvent {
    /// It introduced itself, and is running.
    Ready {
        /// Which plugin.
        plugin: PluginId,
    },
    /// It said something is wrong that the user can act on.
    Problem {
        /// Which plugin.
        plugin: PluginId,
        /// What it said, in its own words.
        message: String,
    },
    /// It went wrong and is being started again.
    Restarting {
        /// Which plugin.
        plugin: PluginId,
        /// What went wrong.
        trouble: PluginTrouble,
        /// Which consecutive attempt this will be.
        attempt: u32,
        /// How long until it is.
        after: Duration,
    },
    /// It went wrong and is not being started again.
    Disabled {
        /// Which plugin.
        plugin: PluginId,
        /// What went wrong, in the words the user is shown.
        trouble: PluginTrouble,
    },
}

/// What one attached plugin is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHealth {
    /// Which plugin.
    pub plugin: PluginId,
    /// What it is doing.
    pub state: PluginState,
    /// How many of its events the recording could not take, across every
    /// attempt. A session with a non-zero count here has an incomplete
    /// timeline and must say so rather than look complete.
    pub dropped: u64,
    /// How many of its lines could not be read, or events were refused.
    pub faults: u32,
    /// Whether a thread is still reading its output.
    ///
    /// Normally the same thing as "it is running". It is reported separately
    /// because the one case where the two differ is worth being able to see: a
    /// plugin that has exited while something it started still holds its
    /// standard output. Nothing waits on that thread (`crate::process`), so it
    /// costs a recording nothing, but it is the difference between a plugin
    /// that tidied up after itself and one that left a process behind.
    pub still_reading: bool,
    /// The most recent problems it reported.
    pub problems: Vec<String>,
}

/// What a supervised plugin is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Started, and has not introduced itself yet.
    Starting,
    /// Running, and has.
    Running,
    /// Stopped, and due to be started again.
    WaitingToRestart {
        /// When.
        at: Instant,
    },
    /// Asked to finish.
    Stopping,
    /// Finished.
    Stopped,
    /// Stopped, and not coming back. The reason is what the user is shown.
    Disabled {
        /// Why.
        trouble: PluginTrouble,
    },
}

/// One plugin, and everything the supervisor remembers about it.
#[derive(Debug)]
struct SupervisedPlugin {
    id: PluginId,
    plugin: EnabledPlugin,
    session: SessionDetails,
    timeline: SessionTimeline,
    state: PluginState,
    ledger: RestartLedger,
    process: Option<PluginProcess>,
    /// Counts from processes that have already ended, so that a plugin cannot
    /// escape its budgets by being restarted.
    dropped_before: u64,
    faults_before: u32,
    /// How many of the current process's problems have already been reported.
    problems_reported: usize,
}

#[cfg(test)]
mod tests {
    use clipped_events::{EventKind, GameEvent};

    use super::*;
    use crate::fixture::{install_example, session, until, TemporaryDirectory, PATIENCE};

    /// The longest a recording may spend *draining its plugins' events*,
    /// whatever they are doing.
    ///
    /// Loose on purpose, and it does not need to be tight: a drain that had
    /// gone wrong would not be *slower*, it would be **unbounded** — a loop
    /// that empties a queue a flooding plugin is refilling does not finish
    /// while the flood lasts. Half a second separates that from a machine
    /// running eight tests and a flooding process at once, where a thread can
    /// be descheduled for tens of milliseconds through nobody's fault
    /// (AGENTS.md section 25).
    ///
    /// The sharp assertion beside this one is that a drain never returns more
    /// than a queue's worth, which is not a measurement and cannot flake.
    const A_DRAIN_IS_NEVER_LONGER_THAN: Duration = Duration::from_millis(500);

    /// The longest a supervision poll may take.
    ///
    /// Deliberately much larger, and deliberately a separate number: a poll can
    /// start a replacement plugin, and starting a process takes as long as the
    /// operating system takes. What it may never do is *wait for a plugin* —
    /// which is what this catches, since every way a plugin can make something
    /// wait lasts far longer than this.
    const A_POLL_IS_NEVER_LONGER_THAN: Duration = Duration::from_secs(5);

    /// A supervisor with a policy tuned to fail fast, so that the rules can be
    /// watched in a test rather than in an afternoon.
    ///
    /// **Nothing here is a budget for starting a process.** `hello_timeout` and
    /// `silence_timeout` are as long as these tests are willing to wait for a
    /// child process at all, so a machine that takes a moment over
    /// `CreateProcess`, a loader run and a first write on a pipe fails the wait
    /// below with a sentence saying so, rather than being reported as a plugin
    /// that could not start. Four hundred milliseconds used to be the budget,
    /// and a CI runner exceeded it twice in a row while starting the flooding
    /// plugin, which turned a test about flooding into a test about how busy
    /// the runner was (#405).
    ///
    /// Every test that shares this is about something a plugin does *after* it
    /// has introduced itself. The two that are about these numbers —
    /// `a_plugin_that_never_introduces_itself_is_reported_as_that` and
    /// `a_plugin_that_hangs_is_killed_rather_than_waited_for` — shorten the one
    /// they are about, and each says why a short one cannot give it the wrong
    /// answer.
    fn impatient() -> SupervisionPolicy {
        SupervisionPolicy {
            silence_timeout: PATIENCE,
            hello_timeout: PATIENCE,
            dropped_event_budget: 4,
            protocol_fault_budget: 5,
            attempts: 2,
            first_delay: Duration::from_millis(10),
            maximum_delay: Duration::from_millis(20),
            settled_after: Duration::from_secs(60),
            stop_grace: Duration::from_millis(200),
        }
    }

    /// One turn of the loop a recording session runs, with the plugin work in
    /// it timed.
    ///
    /// Everything a plugin costs a recording happens here — draining the queue
    /// and polling the supervisor — so timing exactly this is what makes "a
    /// misbehaving plugin cannot stall a recording" a measurement rather than
    /// an assertion about the code's shape.
    fn a_turn_of_the_recording_loop(
        supervisor: &mut PluginSupervisor,
        receiver: &EventReceiver,
        events: &mut Vec<GameEvent>,
        reported: &mut Vec<SupervisionEvent>,
    ) {
        let before_draining = Instant::now();
        let drained = receiver.drain();
        let draining = before_draining.elapsed();
        assert!(
            drained.len() <= receiver.capacity(),
            "a drain must cost at most one queue, and this one cost {}",
            drained.len()
        );
        assert!(
            draining < A_DRAIN_IS_NEVER_LONGER_THAN,
            "a recording spent {draining:?} taking events from its plugins"
        );
        events.extend(drained);

        let before_polling = Instant::now();
        reported.extend(supervisor.poll(Instant::now()));
        let polling = before_polling.elapsed();
        assert!(
            polling < A_POLL_IS_NEVER_LONGER_THAN,
            "a supervision poll took {polling:?}, which is long enough to have waited for a plugin"
        );
    }

    /// Runs a session's loop until `done` is satisfied, timing every turn.
    fn record_until(
        supervisor: &mut PluginSupervisor,
        receiver: &EventReceiver,
        what: &str,
        mut done: impl FnMut(&[GameEvent], &[SupervisionEvent]) -> bool,
    ) -> (Vec<GameEvent>, Vec<SupervisionEvent>) {
        let mut events = Vec::new();
        let mut reported = Vec::new();
        until(what, || {
            a_turn_of_the_recording_loop(supervisor, receiver, &mut events, &mut reported);
            done(&events, &reported)
        });
        (events, reported)
    }

    fn disabled_with(reported: &[SupervisionEvent]) -> Option<PluginTrouble> {
        reported.iter().find_map(|event| match event {
            SupervisionEvent::Disabled { trouble, .. } => Some(trouble.clone()),
            _ => None,
        })
    }

    /// Whether a plugin was replaced **because it flooded**.
    ///
    /// The decision this crate refuses to make, and deliberately not "whether it
    /// was replaced at all": a plugin that was slow to start and replaced for
    /// that is a supervisor doing its job, and a test that cannot tell the two
    /// apart fails on a busy machine while saying something untrue about the
    /// rule it is named for (#405).
    fn restarted_for_flooding(reported: &[SupervisionEvent]) -> bool {
        reported.iter().any(|event| {
            matches!(
                event,
                SupervisionEvent::Restarting {
                    trouble: PluginTrouble::Flooded { .. },
                    ..
                }
            )
        })
    }

    #[test]
    fn a_supervisor_with_no_plugins_has_nothing_to_say() {
        let (mut supervisor, receiver) = PluginSupervisor::new(SupervisionPolicy::default());
        assert!(supervisor.poll(Instant::now()).is_empty());
        assert!(supervisor.health().is_empty());
        assert!(receiver.drain().is_empty());
        assert_eq!(supervisor.inbox_stats(), InboxStats::default());
    }

    #[test]
    fn a_plugin_that_panics_is_replaced_and_then_left_alone() {
        let root = TemporaryDirectory::new("crash");
        let plugin = install_example(&root, "crasher", "misbehaving_plugin", "crash-plugin");
        let (mut supervisor, receiver) = PluginSupervisor::new(impatient());
        supervisor.attach(
            plugin,
            session(),
            SessionTimeline::starting_now(),
            Instant::now(),
        );

        let (_, reported) = record_until(
            &mut supervisor,
            &receiver,
            "a plugin that panics to be replaced twice and then disabled",
            |_, reported| disabled_with(reported).is_some(),
        );

        let attempts: Vec<u32> = reported
            .iter()
            .filter_map(|event| match event {
                SupervisionEvent::Restarting { attempt, .. } => Some(*attempt),
                _ => None,
            })
            .collect();
        assert_eq!(
            attempts,
            vec![1, 2],
            "a crashing plugin is given the attempts the policy allows, and no more"
        );
        assert!(
            matches!(
                disabled_with(&reported),
                Some(PluginTrouble::Exited { code: Some(_) })
            ),
            "expected the panic to be reported as an exit, got {reported:?}"
        );
        assert!(matches!(
            supervisor.health()[0].state,
            PluginState::Disabled { .. }
        ));
    }

    #[test]
    fn a_plugin_that_hangs_is_killed_rather_than_waited_for() {
        // The failure a plugin is a *process* for. Nothing here waits on it:
        // the recording's loop is timed on every turn, and the plugin is
        // reclaimed by the operating system, which is not possible for a thread
        // that has stopped answering.
        //
        // The one test that is about the silence budget, so it is the one that
        // shortens it. It can: silence is asked only of a plugin that has
        // already introduced itself (`Supervising::examine_running`), so the
        // time this plugin spends being loaded is charged to `hello_timeout`
        // and not to this number, and a machine having a bad afternoon delays
        // the answer instead of changing it.
        let root = TemporaryDirectory::new("hang");
        let plugin = install_example(&root, "hanger", "misbehaving_plugin", "hang-plugin");
        let quick_to_notice_silence = SupervisionPolicy {
            silence_timeout: Duration::from_millis(400),
            ..impatient()
        };
        let (mut supervisor, receiver) = PluginSupervisor::new(quick_to_notice_silence);
        supervisor.attach(
            plugin,
            session(),
            SessionTimeline::starting_now(),
            Instant::now(),
        );

        let (_, reported) = record_until(
            &mut supervisor,
            &receiver,
            "a plugin that says hello and then hangs to be stopped",
            |_, reported| disabled_with(reported).is_some(),
        );

        assert!(
            reported
                .iter()
                .any(|event| matches!(event, SupervisionEvent::Ready { .. })),
            "it started properly before it stopped answering: {reported:?}"
        );
        assert!(
            matches!(disabled_with(&reported), Some(PluginTrouble::Silent { .. })),
            "expected silence to be what it was stopped for, got {reported:?}"
        );
    }

    #[test]
    fn a_plugin_that_floods_is_bounded_counted_and_stopped_for_good() {
        // Three things at once: the queue holds the line, the loss is counted
        // rather than hidden, and a flood is not something a replacement fixes.
        //
        // The restart budget is opened wide on purpose, and it is what gives
        // the third claim its teeth: this supervisor would replace this plugin
        // twenty times over if the rule allowed it, so "it was never restarted
        // for flooding" is the rule holding rather than a budget running out.
        // It also leaves room for the restart this test must *not* care
        // about — a start slow enough to be given up on, which is a decision
        // the supervisor is right to make and which a busy runner produces
        // (#405) — to happen without becoming a different answer.
        let root = TemporaryDirectory::new("flood");
        let plugin = install_example(&root, "flooder", "misbehaving_plugin", "flood-plugin");
        let eager_to_replace = SupervisionPolicy {
            attempts: 20,
            ..impatient()
        };
        let (mut supervisor, receiver) = PluginSupervisor::with_capacity(eager_to_replace, 8);
        supervisor.attach(
            plugin,
            session(),
            SessionTimeline::starting_now(),
            Instant::now(),
        );

        let (events, reported) = record_until(
            &mut supervisor,
            &receiver,
            "a plugin that floods to be stopped",
            // Stopping on the wrong answer as well as on the right one: a
            // supervisor that decided to replace a flooding plugin should fail
            // this test at the moment it decides, rather than by running out of
            // patience half a minute later with nothing said about why.
            |_, reported| disabled_with(reported).is_some() || restarted_for_flooding(reported),
        );

        assert!(
            !restarted_for_flooding(&reported),
            "a plugin that outruns a recording is not worth restarting: {reported:?}"
        );
        assert!(
            matches!(
                disabled_with(&reported),
                Some(PluginTrouble::Flooded { dropped }) if dropped > 0
            ),
            "expected a flood, got {reported:?}"
        );

        // Stopped *for good*, which is the half of that claim a single report
        // cannot make: polled again with the clock an hour on — past any delay
        // a replacement could have been waiting out — it has nothing more to
        // say and nothing has been started in its place.
        let much_later = Instant::now() + Duration::from_secs(3600);
        assert_eq!(
            supervisor.poll(much_later),
            Vec::new(),
            "a plugin disabled for flooding had something more to say an hour later"
        );
        assert!(
            matches!(
                supervisor.health()[0].state,
                PluginState::Disabled {
                    trouble: PluginTrouble::Flooded { .. }
                }
            ),
            "an hour later it is still disabled, and still for flooding: {:?}",
            supervisor.health()[0].state
        );

        let stats = supervisor.inbox_stats();
        let charged = supervisor.health()[0].dropped;
        assert!(stats.dropped > 0, "the loss is counted: {stats:?}");
        assert!(
            charged > 0 && charged <= stats.dropped,
            "the loss is attributed to the plugin that caused it: it was charged {charged} of \
             the {} the queue lost",
            stats.dropped
        );
        assert!(
            events.iter().all(|event| event.kind() == &EventKind::Kill),
            "what did get through is still well-formed"
        );
    }

    #[test]
    fn a_plugin_that_never_introduces_itself_is_reported_as_that() {
        // The one test that is about the start-up budget, so it is the one that
        // shortens it. It is also the only test that can afford to: this plugin
        // never introduces itself *at all*, so "slow to start" and "never
        // started" are the same plugin here and no amount of load on the
        // machine can turn one into the other (#405).
        let root = TemporaryDirectory::new("quiet");
        let plugin = install_example(&root, "quiet-one", "misbehaving_plugin", "quiet-plugin");
        let quick_to_give_up_on_starting = SupervisionPolicy {
            hello_timeout: Duration::from_millis(400),
            ..impatient()
        };
        let (mut supervisor, receiver) = PluginSupervisor::new(quick_to_give_up_on_starting);
        supervisor.attach(
            plugin,
            session(),
            SessionTimeline::starting_now(),
            Instant::now(),
        );

        let (_, reported) = record_until(
            &mut supervisor,
            &receiver,
            "a plugin that never says hello to be given up on",
            |_, reported| disabled_with(reported).is_some(),
        );
        assert!(
            matches!(
                disabled_with(&reported),
                Some(PluginTrouble::NeverStarted { .. })
            ),
            "expected a plugin that never started, got {reported:?}"
        );
    }

    #[test]
    fn a_plugin_that_has_not_started_is_never_reported_as_one_that_went_quiet() {
        // Two budgets, one interval, and the interval belongs to exactly one of
        // them. A plugin that has said nothing at all has not *gone* quiet, and
        // the difference is what the user is shown: `NeverStarted` names a
        // plugin that could not run and `Silent` names one that ran and stopped
        // answering (AGENTS.md section 45). Under a policy whose silence budget
        // is the shorter of the two the answer must still be the first one —
        // which is also what lets every test above hold the start-up budget
        // open without a short silence budget quietly taking its place (#405).
        let root = TemporaryDirectory::new("quiet-not-silent");
        let plugin = install_example(&root, "quiet-one", "misbehaving_plugin", "quiet-plugin");
        let quicker_to_call_it_silence = SupervisionPolicy {
            silence_timeout: Duration::from_millis(50),
            hello_timeout: Duration::from_millis(400),
            ..impatient()
        };
        let (mut supervisor, receiver) = PluginSupervisor::new(quicker_to_call_it_silence);
        supervisor.attach(
            plugin,
            session(),
            SessionTimeline::starting_now(),
            Instant::now(),
        );

        let (_, reported) = record_until(
            &mut supervisor,
            &receiver,
            "a plugin that never says hello to be given up on",
            |_, reported| disabled_with(reported).is_some(),
        );

        let called_silent = reported.iter().any(|event| {
            matches!(
                event,
                SupervisionEvent::Restarting {
                    trouble: PluginTrouble::Silent { .. },
                    ..
                } | SupervisionEvent::Disabled {
                    trouble: PluginTrouble::Silent { .. },
                    ..
                }
            )
        });
        assert!(
            !called_silent,
            "a plugin that has never spoken was reported as one that stopped speaking: {reported:?}"
        );
        assert!(
            matches!(
                disabled_with(&reported),
                Some(PluginTrouble::NeverStarted { waited }) if waited == Duration::from_millis(400)
            ),
            "expected the start-up budget to be what it was given up on, got {reported:?}"
        );
    }

    #[test]
    fn a_plugin_speaking_another_contract_version_is_not_run() {
        // Checked against the running executable and not only against its
        // manifest, because an update can replace one without the other.
        let root = TemporaryDirectory::new("newer");
        let plugin = install_example(
            &root,
            "from-the-future",
            "misbehaving_plugin",
            "newer-plugin",
        );
        let (mut supervisor, receiver) = PluginSupervisor::new(impatient());
        supervisor.attach(
            plugin,
            session(),
            SessionTimeline::starting_now(),
            Instant::now(),
        );

        let (_, reported) = record_until(
            &mut supervisor,
            &receiver,
            "a plugin from a newer contract to be refused",
            |_, reported| disabled_with(reported).is_some(),
        );
        assert_eq!(
            disabled_with(&reported),
            Some(PluginTrouble::WrongContract { contract: 99 })
        );
    }

    #[test]
    fn a_plugin_printing_rubbish_is_tolerated_up_to_a_budget() {
        // A plugin logging to its standard output is a mistake, not a menace,
        // and one bad line must not cost an integration. Enough of them is a
        // plugin that is not one.
        let root = TemporaryDirectory::new("garbage");
        let plugin = install_example(&root, "noisy", "misbehaving_plugin", "garbage-plugin");
        let (mut supervisor, receiver) = PluginSupervisor::new(impatient());
        supervisor.attach(
            plugin,
            session(),
            SessionTimeline::starting_now(),
            Instant::now(),
        );

        let (_, reported) = record_until(
            &mut supervisor,
            &receiver,
            "a plugin printing rubbish to be stopped",
            |_, reported| disabled_with(reported).is_some(),
        );
        assert!(
            matches!(
                disabled_with(&reported),
                Some(PluginTrouble::Unreadable { faults }) if faults > 5
            ),
            "expected an unreadable plugin, got {reported:?}"
        );
    }

    #[test]
    fn a_working_plugin_reports_an_event_the_host_places_and_attributes() {
        // The end-to-end path, against the worked example in
        // `examples/example_plugin.rs`: a plugin says what happened and how
        // long ago, and what comes out is a `GameEvent` on the recording's
        // timeline, attributed to the plugin's own identifier.
        let root = TemporaryDirectory::new("working");
        let plugin = install_example(&root, "acme-cs2", "example_plugin", "example-plugin");
        let (mut supervisor, receiver) = PluginSupervisor::new(impatient());

        // A timeline that started two seconds ago, so that an event 480 ms
        // before the report is comfortably inside the recording.
        let started = Instant::now() - Duration::from_secs(2);
        supervisor.attach(
            plugin,
            session(),
            SessionTimeline::starting_at(started),
            Instant::now(),
        );

        let (events, reported) = record_until(
            &mut supervisor,
            &receiver,
            "the example plugin to report a kill",
            |events, _| !events.is_empty(),
        );

        assert!(reported
            .iter()
            .any(|event| matches!(event, SupervisionEvent::Ready { .. })));
        let kill = &events[0];
        assert_eq!(kill.kind(), &EventKind::Kill);
        assert_eq!(
            kill.source().as_str(),
            "acme-cs2",
            "the source is the manifest's identifier, which the plugin never sent"
        );
        assert_eq!(kill.timing().latency(), Duration::from_millis(480));
        assert_eq!(kill.timing().precision(), Duration::from_millis(100));
        assert_eq!(kill.data().fields()["weapon"], serde_json::json!("ak47"));

        // Placed on the recording's timeline, and placed *earlier than the
        // report that carried it*. The second half is the one worth checking:
        // the queue was drained within a few milliseconds of the report
        // arriving, so an event that was not moved back by the 480 ms the
        // plugin said it was late would sit within a few milliseconds of now.
        let at = kill.timing().at().as_media_nanos();
        let now = SessionTimeline::starting_at(started).now().as_media_nanos();
        assert!(
            at > 1_000_000_000,
            "the kill belongs on this recording's timeline, and was placed at {at}ns"
        );
        assert!(
            at <= now - 400_000_000,
            "the kill happened 480 ms before it was reported, and was placed at {at}ns with the \
             recording at {now}ns"
        );

        // And it stops when the session does.
        supervisor.detach(Instant::now());
        until("the plugin to finish when the session does", || {
            supervisor.poll(Instant::now());
            supervisor.health()[0].state == PluginState::Stopped
        });
    }
}
