//! When a plugin is in trouble, and what is done about it.
//!
//! Everything here is a decision made from numbers — how long since the plugin
//! last said anything, how many of its events were dropped, how many times it
//! has already been replaced — and none of it touches a process. That split is
//! deliberate: the rules are the part worth arguing about and the part worth
//! testing, and they are tested here without a plugin, a thread or a wait
//! (`crate::supervisor` is what applies them).
//!
//! # The three ways a plugin misbehaves, and the answer to each
//!
//! | | What it looks like | What the host does |
//! | --- | --- | --- |
//! | It crashes | The process exits without being asked to | Replace it, with a widening delay, a bounded number of times |
//! | It hangs | No report of any kind for [`silence_timeout`](SupervisionPolicy::silence_timeout) | Kill it, then treat it as a crash |
//! | It floods | Events dropped past [`dropped_event_budget`](SupervisionPolicy::dropped_event_budget) | Stop it and disable it: a plugin outrunning the recording is not going to stop of its own accord |
//!
//! None of the three reaches a recording, whatever the host decides, because
//! nothing in a recording waits on a plugin (`crate::inbox`). What these rules
//! decide is whether the *plugin* comes back, and they exist because the
//! recorder runs for days: a plugin that is replaced for ever burns CPU beside
//! a game and fills a log with one line (AGENTS.md section 59), and a plugin
//! that is never replaced turns one bad minute into a session with no events.
//!
//! # Why this is not `clipped_ipc::RestartPolicy`
//!
//! That type supervises *the recorder*, from the desktop application, and the
//! shape of the two policies is genuinely the same: bounded attempts, widening
//! delays, a counter that resets once the thing has stayed up. What differs is
//! everything around it — a plugin can hang while still running, can flood, and
//! is stopped for reasons a recorder has no equivalent of — and reusing the
//! type would mean this crate depending on the control protocol between the
//! recorder and the window in order to borrow four numbers. The four numbers
//! are copied, deliberately, and named the same so a reader can see that they
//! are the same idea (AGENTS.md section 55).

use core::fmt;
use core::time::Duration;
use std::time::Instant;

/// How patient the host is with a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisionPolicy {
    /// How long a plugin may say nothing at all before it is treated as hung.
    ///
    /// A plugin with nothing to report says `alive` (`crate::report`), so
    /// silence here means the plugin is not running its own loop: deadlocked,
    /// blocked on a socket that will never answer, or waiting on a game that
    /// has stopped talking. It is the failure a separate process is chosen
    /// for — a hung *thread* could never be reclaimed, and a hung process can
    /// be killed.
    pub silence_timeout: Duration,
    /// How long a freshly started plugin has to introduce itself.
    ///
    /// Separate from the silence timeout because the two failures are
    /// different: this one is a plugin that cannot start — a missing runtime, a
    /// game directory that is not there — and it is worth reporting as that
    /// rather than as a hang.
    pub hello_timeout: Duration,
    /// How many events may be dropped before the plugin is called a flood.
    ///
    /// Dropping happens when the queue is full, which happens when a plugin
    /// produces events faster than a recording drains them. A game does not do
    /// that; a plugin in a loop does.
    pub dropped_event_budget: u64,
    /// How many unreadable lines and refused events are tolerated.
    ///
    /// Not zero, because a plugin printing its own diagnostics on standard
    /// output is a mistake rather than a menace, and one refused event should
    /// not cost a whole integration. Not unbounded, because a plugin that
    /// cannot produce a single readable line is not an integration at all.
    pub protocol_fault_budget: u32,
    /// How many consecutive replacements to start before giving up.
    pub attempts: u32,
    /// How long to wait before the first replacement.
    pub first_delay: Duration,
    /// The longest wait between replacements, once doubling has reached it.
    pub maximum_delay: Duration,
    /// How long a plugin must run before the attempt counter is reset.
    ///
    /// Without this, a plugin that fails once an hour would exhaust its
    /// attempts in an afternoon and never start again, which for a recorder
    /// that runs for days is the wrong end of the trade.
    pub settled_after: Duration,
    /// How long a plugin has to exit after being asked to, before it is killed.
    pub stop_grace: Duration,
}

impl Default for SupervisionPolicy {
    /// Ten seconds of silence, three replacements, and a flood budget of a
    /// hundred events.
    ///
    /// The silence timeout is the number most worth explaining: it is long
    /// enough that a plugin doing something slow between heartbeats — reading a
    /// large log file, waiting on a game that has just been minimised — is not
    /// killed for it, and short enough that a session does not spend a match
    /// attached to something that stopped working.
    fn default() -> Self {
        Self {
            silence_timeout: Duration::from_secs(10),
            hello_timeout: Duration::from_secs(5),
            dropped_event_budget: 100,
            protocol_fault_budget: 32,
            attempts: 3,
            first_delay: Duration::from_secs(1),
            maximum_delay: Duration::from_secs(30),
            settled_after: Duration::from_secs(60),
            stop_grace: Duration::from_secs(2),
        }
    }
}

impl SupervisionPolicy {
    /// Never replace a plugin; report the loss and stop there.
    #[must_use]
    pub fn never_restart() -> Self {
        Self {
            attempts: 0,
            ..Self::default()
        }
    }

    /// How long to wait before consecutive attempt number `attempt`, counting
    /// from one, or [`None`] once the attempts are used up.
    ///
    /// The delay doubles, capped at [`maximum_delay`](Self::maximum_delay).
    /// Doubling is what separates "the game was still starting" from "this
    /// plugin cannot run on this machine": the first is fixed by the first
    /// attempt, and the second is not retried forty times a second while the
    /// message about it is being read.
    #[must_use]
    pub fn delay_before(&self, attempt: u32) -> Option<Duration> {
        if attempt == 0 || attempt > self.attempts {
            return None;
        }
        let delay = self
            .first_delay
            .checked_mul(2_u32.saturating_pow((attempt - 1).min(31)))
            .unwrap_or(self.maximum_delay);
        Some(delay.min(self.maximum_delay))
    }
}

/// What went wrong with a plugin.
///
/// Every variant is something the user can be shown and something a log can be
/// searched for. None of them is "an error occurred".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginTrouble {
    /// It stopped without being asked to.
    Exited {
        /// Its exit code, when the operating system had one to give.
        code: Option<i32>,
    },
    /// It said nothing at all for longer than the policy allows, and was
    /// killed.
    Silent {
        /// How long it had been quiet.
        quiet_for: Duration,
    },
    /// It never introduced itself, and was killed.
    NeverStarted {
        /// How long it was given.
        waited: Duration,
    },
    /// It speaks a contract version this build does not.
    ///
    /// Checked against what the running executable says rather than only
    /// against its manifest, because an update can replace one without the
    /// other.
    WrongContract {
        /// What it said it speaks.
        contract: u32,
    },
    /// It produced events faster than the recording could take them, and was
    /// stopped.
    Flooded {
        /// How many events were lost.
        dropped: u64,
    },
    /// It produced more lines this build could not read, or events it was not
    /// allowed to send, than the policy allows.
    Unreadable {
        /// How many.
        faults: u32,
    },
    /// It could not be started at all.
    CouldNotStart {
        /// What the operating system said, already rendered: an `io::Error` is
        /// not comparable and this type is compared in tests and in the
        /// interface.
        because: String,
    },
}

impl fmt::Display for PluginTrouble {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited { code: Some(code) } => {
                write!(formatter, "the plugin stopped on its own, with code {code}")
            }
            Self::Exited { code: None } => formatter.write_str("the plugin stopped on its own"),
            Self::Silent { quiet_for } => write!(
                formatter,
                "the plugin said nothing for {quiet_for:?} and was stopped"
            ),
            Self::NeverStarted { waited } => write!(
                formatter,
                "the plugin did not start within {waited:?} and was stopped"
            ),
            Self::WrongContract { contract } => write!(
                formatter,
                "the plugin speaks plugin contract {contract}, which this build of Clipped does \
                 not"
            ),
            Self::Flooded { dropped } => write!(
                formatter,
                "the plugin reported events faster than they could be recorded, and {dropped} \
                 were lost before it was stopped"
            ),
            Self::Unreadable { faults } => write!(
                formatter,
                "the plugin sent {faults} things this build could not use, and was stopped"
            ),
            Self::CouldNotStart { because } => {
                write!(formatter, "the plugin could not be started: {because}")
            }
        }
    }
}

impl core::error::Error for PluginTrouble {}

/// What to do about a plugin that is in trouble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recovery {
    /// Start it again, after a delay.
    Replace {
        /// Which consecutive attempt this is, counting from one.
        attempt: u32,
        /// How long to wait first.
        after: Duration,
    },
    /// Leave it stopped and say why.
    GiveUp,
}

/// How many times a plugin has been replaced, and how long the current one has
/// been up.
///
/// Pure arithmetic over a clock reading the caller supplies, so the rules above
/// are tested by passing times rather than by waiting for them.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RestartLedger {
    attempts_used: u32,
    running_since: Option<Instant>,
}

impl RestartLedger {
    /// Records that a plugin has just been started.
    pub(crate) fn started(&mut self, now: Instant) {
        self.running_since = Some(now);
    }

    /// Decides what happens to a plugin that has just failed.
    pub(crate) fn troubled(&mut self, now: Instant, policy: &SupervisionPolicy) -> Recovery {
        let settled = self
            .running_since
            .is_some_and(|since| now.duration_since(since) >= policy.settled_after);
        if settled {
            self.attempts_used = 0;
        }
        self.running_since = None;

        let attempt = self.attempts_used + 1;
        match policy.delay_before(attempt) {
            Some(after) => {
                self.attempts_used = attempt;
                Recovery::Replace { attempt, after }
            }
            None => Recovery::GiveUp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SupervisionPolicy {
        SupervisionPolicy {
            attempts: 3,
            first_delay: Duration::from_secs(1),
            maximum_delay: Duration::from_secs(4),
            settled_after: Duration::from_secs(60),
            ..SupervisionPolicy::default()
        }
    }

    #[test]
    fn the_delay_doubles_and_is_capped() {
        let policy = policy();
        assert_eq!(policy.delay_before(1), Some(Duration::from_secs(1)));
        assert_eq!(policy.delay_before(2), Some(Duration::from_secs(2)));
        assert_eq!(
            policy.delay_before(3),
            Some(Duration::from_secs(4)),
            "doubling stops at the maximum"
        );
        assert_eq!(policy.delay_before(4), None, "the attempts are used up");
        assert_eq!(policy.delay_before(0), None);
    }

    #[test]
    fn a_plugin_that_keeps_failing_is_left_stopped() {
        // The behaviour AGENTS.md sections 16 and 45 ask for: a permanent
        // failure is reported and stays reported, rather than being retried
        // beside a game for the rest of the evening.
        let policy = policy();
        let mut ledger = RestartLedger::default();
        let start = Instant::now();

        for expected in 1..=3 {
            ledger.started(start);
            assert!(matches!(
                ledger.troubled(start, &policy),
                Recovery::Replace { attempt, .. } if attempt == expected
            ));
        }
        ledger.started(start);
        assert_eq!(ledger.troubled(start, &policy), Recovery::GiveUp);
    }

    #[test]
    fn a_plugin_that_ran_for_a_while_starts_its_budget_again() {
        // Without this, a plugin that fails once an hour would be permanently
        // disabled by teatime, which for a recorder that runs for days is the
        // wrong end of the trade.
        let policy = policy();
        let mut ledger = RestartLedger::default();
        let start = Instant::now();

        for _ in 0..3 {
            ledger.started(start);
            assert!(matches!(
                ledger.troubled(start, &policy),
                Recovery::Replace { .. }
            ));
        }

        ledger.started(start);
        let much_later = start + policy.settled_after + Duration::from_secs(1);
        assert!(
            matches!(
                ledger.troubled(much_later, &policy),
                Recovery::Replace { attempt: 1, .. }
            ),
            "a plugin that stayed up for a minute is not in a crash loop"
        );
    }

    #[test]
    fn a_plugin_that_has_not_run_long_enough_keeps_counting() {
        let policy = policy();
        let mut ledger = RestartLedger::default();
        let start = Instant::now();

        ledger.started(start);
        assert!(matches!(
            ledger.troubled(start + Duration::from_secs(1), &policy),
            Recovery::Replace { attempt: 1, .. }
        ));
        ledger.started(start);
        assert!(
            matches!(
                ledger.troubled(start + Duration::from_secs(59), &policy),
                Recovery::Replace { attempt: 2, .. }
            ),
            "just under the settling time is still a crash loop"
        );
    }

    #[test]
    fn a_policy_can_refuse_to_replace_anything() {
        let mut ledger = RestartLedger::default();
        let now = Instant::now();
        ledger.started(now);
        assert_eq!(
            ledger.troubled(now, &SupervisionPolicy::never_restart()),
            Recovery::GiveUp
        );
    }

    #[test]
    fn every_trouble_says_what_happened_in_words() {
        // These are shown to the user and searched for in logs, so none of them
        // may render as "an error occurred" (AGENTS.md sections 15 and 45).
        let troubles = [
            PluginTrouble::Exited { code: Some(3) },
            PluginTrouble::Exited { code: None },
            PluginTrouble::Silent {
                quiet_for: Duration::from_secs(10),
            },
            PluginTrouble::NeverStarted {
                waited: Duration::from_secs(5),
            },
            PluginTrouble::WrongContract { contract: 2 },
            PluginTrouble::Flooded { dropped: 100 },
            PluginTrouble::Unreadable { faults: 32 },
            PluginTrouble::CouldNotStart {
                because: "the file was not found".to_owned(),
            },
        ];
        for trouble in troubles {
            let said = trouble.to_string();
            assert!(
                said.contains("plugin") && said.len() > 20,
                "`{said}` does not say what happened"
            );
        }
    }
}
