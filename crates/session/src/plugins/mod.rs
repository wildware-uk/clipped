//! The plugins attached to one recording, and the thread that runs them.
//!
//! `crates/plugins` is the contract: what a plugin is, how one is started, and
//! what keeps a bad one away from a recording. It deliberately starts nothing by
//! itself — [`PluginSupervisor`] is a state machine over a clock reading its
//! owner supplies. This module is that owner
//! ([issue #338](https://github.com/wildware-uk/clipped/issues/338)): it creates
//! the supervisor, attaches the plugins that support the game being recorded,
//! polls it about once a second, drains the events it produces and stops
//! everything when the recording ends.
//!
//! # The shape of it
//!
//! ```text
//!  capture thread                  this module's thread            plugin process
//!  ──────────────                  ────────────────────            ──────────────
//!  first kept frame                                    ┌──▶  spawn
//!    ├─ CaptureClock::start_at ──▶ the epoch arrives ───┘      {"report":"hello"}
//!    └─ RecordingProgress            attach(timeline)
//!                                    drain the inbox   ◀──────  {"report":"event",…}
//!  encode, mux, repeat               poll(now)      ── once a second
//!                                    detach          ──────────▶ exit, or be killed
//!
//!  the driver's thread
//!  ───────────────────
//!  take_events()   ── the seam issue #71 attaches to
//!  take_reports()  ── what went wrong, put in front of the user
//!  finish()        ── at the end of the recording
//! ```
//!
//! **The capture thread appears once, and it does not call a plugin.** All it
//! does is publish the moment its timeline began
//! ([`RecordingProgress::timeline_began`]), which is one `OnceLock` store and
//! cannot block. Everything else — starting a plugin, reading it, timing it,
//! killing it — happens on the thread this module creates, which nothing in a
//! recording ever waits for (AGENTS.md section 20). That is the whole of "a
//! misbehaving plugin cannot cost a recording anything", and it is a property of
//! the shape rather than of the plugins.
//!
//! # One timeline, taken beside the capture epoch
//!
//! A plugin says **how long ago** something happened, never when
//! (`docs/plugin-api.md`), and the subtraction that turns that into a position
//! in the file is [`SessionTimeline`] — the one clock conversion `crates/events`
//! and `crates/plugins` between them allow, bounded to one function on purpose.
//! This module does not add a second one. It waits for the recording to publish
//! the reading of this process's monotonic clock taken *beside* the capture
//! epoch, builds exactly one [`SessionTimeline`] from it, and hands that same
//! value to every plugin it attaches.
//!
//! The consequence is worth stating: **a plugin starts when the recording's
//! first frame does**, not when the game launched. A recording that never
//! produces a frame — no window appeared, the encoder refused — starts no
//! plugin at all, because there is no timeline for its events to sit on. A
//! plugin attached to a match already in progress can still describe it:
//! [`SessionTimeline`] places a moment before the epoch at a negative position,
//! deliberately.
//!
//! One [`SessionPlugins`] belongs to one *recording*, not to one session. A
//! session that records twice — the same game relaunched inside its restart
//! grace — starts its plugins twice, because each recording has an epoch of its
//! own and a plugin's events have to be placed on the file they belong to.
//!
//! # What a session does with a plugin nobody has enabled
//!
//! **Nothing, and it says which one** ([`installed_but_not_enabled`]).
//!
//! [`SessionPlugins::start`] takes [`EnabledPlugin`]s, and the only way to
//! obtain one is [`InstalledPlugin::enable`] with the consent token the user's
//! consent was recorded against. Nothing records that yet
//! ([issue #282](https://github.com/wildware-uk/clipped/issues/282) is the
//! configuration API's job, and a settings store here would be the second one
//! AGENTS.md section 30 warns about), so today no shipped path produces an
//! `EnabledPlugin` and a session attaches none.
//!
//! Enabling one on the user's behalf was the alternative, and it is refused:
//! `docs/privacy.md` requires that network access is opted into by a deliberate
//! action, and all three bundled plugins open a loopback socket. A recorder that
//! started them because they were on disk would make that register entry false.
//! What a session does instead is name every installed plugin that supports the
//! game and say it is not enabled, so that a user who installed one is told why
//! it is not running rather than left to guess (AGENTS.md section 27).
//!
//! # What is not here
//!
//! **Persisting the events.** [`SessionPlugins::take_events`] and
//! [`PluginOutcome::events`] are where a drained event is handed over, and
//! nothing in this workspace takes it yet:
//! [issue #71](https://github.com/wildware-uk/clipped/issues/71) is the ticket
//! that writes them to `clipped-storage` against the recording they belong to.
//! Until it lands, an event reported during a recording reaches the log and the
//! recording's outcome, and no further — which the driver says out loud rather
//! than implying a feature that works.

use core::time::Duration;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Instant;

use clipped_events::GameEvent;
use clipped_plugins::{
    EnabledPlugin, EventReceiver, InboxStats, InstalledPlugin, ObservedProcess, PluginHealth,
    PluginState, PluginSupervisor, SessionDetails, SessionTimeline, SupervisionEvent,
    SupervisionPolicy,
};

use crate::progress::RecordingProgress;

/// How often the supervisor is polled.
///
/// `clipped_plugins::PluginSupervisor::poll` asks for "about once a second", and
/// this is that promise. Everything time-based about a plugin — one that has not
/// said hello, one that has gone quiet, one whose replacement is due — happens
/// there, so a slower loop would mean a hung plugin holding a game's port for
/// longer than it has to. A faster one would buy nothing: the queue between a
/// plugin and this thread is bounded and never blocks either end.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How often the runner looks for the recording's epoch before it has one.
///
/// Faster than [`POLL_INTERVAL`], because this one is a *start-up* delay: it is
/// how much later than the first frame a plugin is attached, and a plugin that
/// missed the first second of a match missed the first second of a match. It
/// costs a relaxed load of an atomic ten times a second while a recording is
/// opening its encoder.
const EPOCH_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How often plugins are polled once they have been asked to finish.
///
/// A plugin that ignores `detach` is killed by the poll after
/// `SupervisionPolicy::stop_grace`, so polling at the ordinary rate would add up
/// to a second to the end of every recording for nothing.
const STOPPING_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The most events kept for a caller that has not taken them.
///
/// The queue a plugin delivers into is bounded and counted
/// (`clipped_plugins::EventInbox`); this is the same rule applied to what has
/// already been drained. A game reports events in ones and twos a minute, so a
/// caller draining this at the end of a recording is nowhere near it, and a
/// recorder that runs for days (AGENTS.md section 59) must not grow without a
/// ceiling because something upstream went wrong. What is lost past it is
/// counted in [`PluginOutcome::events_lost`] rather than quietly discarded: a
/// timeline missing marks has to say so.
pub const MAX_KEPT_EVENTS: usize = 4096;

/// The plugins running for one recording.
///
/// Created when a recording is asked for and finished when it ends. Dropping one
/// without calling [`finish`](Self::finish) still stops every plugin it started:
/// a plugin outliving the recording that started it is a process nobody owns,
/// holding a port a game will want again.
///
/// # Threading
///
/// One thread, created here, which owns the [`PluginSupervisor`] and is the only
/// thread that ever touches it. Every method on this type takes a lock held for
/// the length of a `Vec` move and nothing else, so a caller — the recorder's
/// watch loop — cannot be delayed by a plugin however badly it behaves. Nothing
/// in the capture path holds one of these at all.
#[derive(Debug)]
pub struct SessionPlugins {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl SessionPlugins {
    /// Starts the plugins in `plugins` that support the process being recorded.
    ///
    /// Returns at once; the process for each plugin is created on the thread
    /// this starts, and whether it started properly is reported through
    /// [`take_reports`](Self::take_reports). A plugin whose manifest does not
    /// claim `session.process` is skipped and logged rather than refused, which
    /// is what lets a caller hand over everything the user enabled without
    /// filtering it first.
    ///
    /// `progress` is the recording's own account of its timeline. Plugins are
    /// attached when it publishes an epoch, which is the moment the first frame
    /// reaches the file; see the module documentation for why that wait exists
    /// and what it costs.
    #[must_use]
    pub fn start(
        plugins: Vec<EnabledPlugin>,
        session: SessionDetails,
        progress: &RecordingProgress,
        policy: SupervisionPolicy,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        let runner = Runner {
            shared: Arc::clone(&shared),
            progress: progress.clone(),
            session,
            policy,
        };

        let thread = thread::Builder::new()
            .name("clipped-session-plugins".to_owned())
            .spawn(move || runner.run(plugins))
            .expect("a thread can be started to run a recording's plugins on");

        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// Everything the plugins have reported since this was last called.
    ///
    /// **This is the seam [issue
    /// #71](https://github.com/wildware-uk/clipped/issues/71) attaches to.** The
    /// events are already placed on the recording's timeline and attributed to
    /// the plugin that reported them; what is missing is the write to
    /// `clipped-storage` against the recording they belong to. Nothing in this
    /// workspace calls this in anger yet.
    ///
    /// Events from several plugins are interleaved by arrival and are **not** in
    /// timeline order; sort by `timing().at()` (`crates/events`).
    #[must_use]
    pub fn take_events(&self) -> Vec<GameEvent> {
        core::mem::take(&mut self.shared.collected().events)
    }

    /// Everything the supervisor has had to say since this was last called.
    ///
    /// A plugin that became ready, one that reported a problem the user can act
    /// on, one being replaced, one being given up on. Draining this is how a
    /// `clipped_plugins::PluginTrouble` reaches a person rather than being
    /// logged and forgotten (AGENTS.md section 45).
    #[must_use]
    pub fn take_reports(&self) -> Vec<SupervisionEvent> {
        core::mem::take(&mut self.shared.collected().reports)
    }

    /// Stops every plugin and reports what they did.
    ///
    /// Writes `detach`, closes each plugin's standard input and kills anything
    /// still running once `SupervisionPolicy::stop_grace` has passed, then
    /// returns. **Bounded**: the longest this can take is the stop grace plus
    /// one poll, whatever the plugins do, because nothing here waits on one.
    #[must_use]
    pub fn finish(mut self) -> PluginOutcome {
        self.stop_and_join();

        let mut collected = self.shared.collected();
        PluginOutcome {
            events: core::mem::take(&mut collected.events),
            reports: core::mem::take(&mut collected.reports),
            events_lost: collected.events_lost,
            health: core::mem::take(&mut collected.health),
            inbox: collected.inbox,
        }
    }

    /// Asks the thread to finish, and waits for it.
    fn stop_and_join(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.shared.ask_to_stop();
        if thread.join().is_err() {
            // The runner catches nothing, so this is a panic in this crate
            // rather than in a plugin. Every plugin it started is dead all the
            // same: the supervisor was owned by that thread and dropping one
            // kills anything still running (`clipped_plugins`).
            tracing::error!("the thread running this recording's plugins ended unexpectedly");
        }
    }
}

impl Drop for SessionPlugins {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// What one recording's plugins came to.
#[derive(Debug)]
pub struct PluginOutcome {
    /// The events nobody took while the recording ran, in arrival order.
    pub events: Vec<GameEvent>,
    /// What the supervisor had to say and nobody took.
    pub reports: Vec<SupervisionEvent>,
    /// Events dropped because [`MAX_KEPT_EVENTS`] was reached. A recording with
    /// a non-zero count here has an incomplete timeline and has to say so.
    pub events_lost: u64,
    /// What each attached plugin was doing at the end, including how many of its
    /// events the recording could not take.
    ///
    /// Read *after* every plugin was asked to stop, so the state here is what
    /// became of it rather than why: a plugin the supervisor gave up on has
    /// already been stopped by the time this is taken, and the reason it was
    /// given up on is the [`SupervisionEvent::Disabled`] in
    /// [`reports`](Self::reports).
    pub health: Vec<PluginHealth>,
    /// What the queue between the plugins and the recording saw.
    pub inbox: InboxStats,
}

impl PluginOutcome {
    /// Whether anything about this recording's plugins is worth telling the user
    /// about.
    ///
    /// True when an event was lost, either at the queue or after it. Both mean
    /// the same thing — the timeline of this recording is missing marks — and
    /// both are the kind of quiet incompleteness AGENTS.md section 27 forbids
    /// leaving to be discovered.
    #[must_use]
    pub const fn lost_anything(&self) -> bool {
        self.events_lost > 0 || self.inbox.dropped > 0
    }
}

/// The installed plugins that support `process` and cannot be started.
///
/// Every one of them, today. Starting a plugin needs an
/// [`EnabledPlugin`], which only [`InstalledPlugin::enable`] produces and which
/// only recorded consent can justify; nothing records consent yet
/// ([issue #282](https://github.com/wildware-uk/clipped/issues/282)). This is
/// how a driver names the plugins a user installed and would reasonably expect
/// to be running, instead of ignoring them silently — see the module
/// documentation for why the alternative was refused.
#[must_use]
pub fn installed_but_not_enabled<'a>(
    installed: &'a [InstalledPlugin],
    process: &ObservedProcess,
) -> Vec<&'a InstalledPlugin> {
    installed
        .iter()
        .filter(|plugin| plugin.supports(process))
        .collect()
}

/// The state shared between the runner's thread and its owner.
#[derive(Debug, Default)]
struct Shared {
    collected: Mutex<Collected>,
    stopping: Mutex<bool>,
    wake: Condvar,
}

impl Shared {
    fn collected(&self) -> MutexGuard<'_, Collected> {
        // A poisoned lock here would mean this module panicked while holding it,
        // which is a bug in this crate. Losing a recording's events over it
        // would be the wrong answer to it.
        self.collected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ask_to_stop(&self) {
        let mut stopping = self
            .stopping
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *stopping = true;
        self.wake.notify_all();
    }

    /// Sleeps for at most `interval`, and answers whether the runner should
    /// stop.
    ///
    /// A condition variable rather than a sleep and a flag, so that finishing a
    /// recording does not wait out whatever is left of a poll interval.
    fn rest(&self, interval: Duration) -> bool {
        let stopping = self
            .stopping
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *stopping {
            return true;
        }
        let (stopping, _) = self
            .wake
            .wait_timeout(stopping, interval)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *stopping
    }
}

/// What the runner has gathered, waiting for its owner to take it.
#[derive(Debug, Default)]
struct Collected {
    events: Vec<GameEvent>,
    reports: Vec<SupervisionEvent>,
    events_lost: u64,
    health: Vec<PluginHealth>,
    inbox: InboxStats,
    /// The thread every supervision poll has happened on.
    ///
    /// Kept so that "a plugin is never polled on the thread that is recording"
    /// is something a test can assert rather than something a comment claims
    /// (`tests/plugins_during_a_recording.rs`, AGENTS.md section 20).
    polled_on: Option<ThreadId>,
    /// How many turns the runner has taken.
    polls: u64,
}

/// The thread that owns the supervisor.
#[derive(Debug)]
struct Runner {
    shared: Arc<Shared>,
    progress: RecordingProgress,
    session: SessionDetails,
    policy: SupervisionPolicy,
}

impl Runner {
    /// The whole of the thread's life.
    fn run(self, plugins: Vec<EnabledPlugin>) {
        let (mut supervisor, receiver) = PluginSupervisor::new(self.policy);

        let Some(epoch) = self.wait_for_the_recordings_timeline() else {
            tracing::info!(
                session = self.session.session.as_str(),
                plugins = plugins.len(),
                "the recording ended before it produced a frame, so no plugin was started: a \
                 plugin's events are positions in a file, and this recording has no timeline to \
                 place them on"
            );
            return;
        };

        // The one conversion, built once, from the reading taken beside the
        // capture epoch. Every plugin below is given this same value.
        let timeline = SessionTimeline::starting_at(epoch);
        let attached = self.attach_all(&mut supervisor, plugins, timeline);

        while !self.shared.rest(POLL_INTERVAL) {
            self.turn(&mut supervisor, &receiver);
        }

        self.finish(&mut supervisor, &receiver, attached);
    }

    /// Waits for the recording to say where its timeline begins.
    ///
    /// [`None`] when the recording ended without producing a frame, which is
    /// also how a recording that failed to open an encoder ends.
    fn wait_for_the_recordings_timeline(&self) -> Option<Instant> {
        loop {
            if let Some(epoch) = self.progress.timeline_epoch() {
                return Some(epoch);
            }
            if self.shared.rest(EPOCH_POLL_INTERVAL) {
                return self.progress.timeline_epoch();
            }
        }
    }

    /// Starts the plugins that claim the process being recorded.
    fn attach_all(
        &self,
        supervisor: &mut PluginSupervisor,
        plugins: Vec<EnabledPlugin>,
        timeline: SessionTimeline,
    ) -> usize {
        let mut attached = 0;
        for plugin in plugins {
            if !plugin.supports(&self.session.process) {
                tracing::debug!(
                    plugin = %plugin.id(),
                    process = self.session.process.executable(),
                    "an enabled plugin does not support the game being recorded, so it was not \
                     started"
                );
                continue;
            }
            tracing::info!(
                plugin = %plugin.id(),
                session = self.session.session.as_str(),
                "attaching a plugin to this recording"
            );
            supervisor.attach(plugin, self.session.clone(), timeline, Instant::now());
            attached += 1;
        }
        attached
    }

    /// One turn: take what the plugins produced, and let the supervisor act.
    ///
    /// Both halves are bounded however badly a plugin behaves —
    /// `EventReceiver::drain` returns at most one queue's worth and
    /// `PluginSupervisor::poll` never waits on a process — which is what makes
    /// this thread's cost a fixed one rather than a plugin's to decide.
    fn turn(&self, supervisor: &mut PluginSupervisor, receiver: &EventReceiver) {
        let drained = receiver.drain();
        let reports = supervisor.poll(Instant::now());

        let mut collected = self.shared.collected();
        collected.polled_on = Some(thread::current().id());
        collected.polls += 1;
        collected.inbox = supervisor.inbox_stats();
        collected.reports.extend(reports);

        for event in drained {
            if collected.events.len() >= MAX_KEPT_EVENTS {
                if collected.events_lost == 0 {
                    tracing::warn!(
                        session = self.session.session.as_str(),
                        kept = MAX_KEPT_EVENTS,
                        "this recording's plugins have reported more events than are being taken \
                         from them, and the ones past the limit are being lost; the recording \
                         itself is unaffected"
                    );
                }
                collected.events_lost += 1;
            } else {
                collected.events.push(event);
            }
        }
    }

    /// Stops every plugin, and does not wait for one that will not stop.
    fn finish(&self, supervisor: &mut PluginSupervisor, receiver: &EventReceiver, attached: usize) {
        supervisor.detach(Instant::now());

        // A plugin that has not exited by the time the grace has passed is
        // killed by the poll that notices, so this is bounded by the policy
        // rather than by the plugins.
        let deadline = Instant::now() + self.policy.stop_grace + POLL_INTERVAL;
        loop {
            self.turn(supervisor, receiver);
            if all_stopped(supervisor) || Instant::now() >= deadline {
                break;
            }
            thread::sleep(STOPPING_POLL_INTERVAL);
        }

        let health = supervisor.health();
        let still_running = health
            .iter()
            .filter(|plugin| !is_stopped(&plugin.state))
            .count();
        if still_running > 0 {
            // Dropping the supervisor kills them, which happens as this returns.
            tracing::warn!(
                session = self.session.session.as_str(),
                still_running,
                "plugins did not finish when this recording ended and are being stopped"
            );
        }
        tracing::info!(
            session = self.session.session.as_str(),
            attached,
            "this recording's plugins have finished"
        );

        let mut collected = self.shared.collected();
        collected.inbox = supervisor.inbox_stats();
        collected.health = health;
    }
}

/// Whether every plugin has finished for good.
fn all_stopped(supervisor: &PluginSupervisor) -> bool {
    supervisor
        .health()
        .iter()
        .all(|plugin| is_stopped(&plugin.state))
}

/// Whether one plugin has finished for good.
fn is_stopped(state: &PluginState) -> bool {
    matches!(state, PluginState::Stopped | PluginState::Disabled { .. })
}

#[cfg(test)]
mod tests;
