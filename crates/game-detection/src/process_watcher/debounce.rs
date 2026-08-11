//! Turning raw process events into launches.
//!
//! This is the whole of the watcher's judgement, and it is a pure state machine
//! over `(event, now)` with no threads, no channels and no Windows in it — so
//! the cases that matter are exercised against constructed process trees rather
//! than against a game somebody has to own (AGENTS.md section 25).
//!
//! # The rules
//!
//! 1. A process that starts joins the launch its parent belongs to, if that
//!    launch is still being collected. Otherwise it opens a new one. This is
//!    why the events carry a parent: without it there is no way to tell a
//!    launcher starting a game from two games starting at once.
//! 2. Collecting a launch keeps it open for [`WatchConfig::launch_quiet_period`]
//!    past its most recent member, up to
//!    [`WatchConfig::max_launch_window`] from its first.
//! 3. A process that exits before its launch is reported is removed from it.
//!    Neither its start nor its exit is ever reported — this is what makes a
//!    launcher that hands over to the game and disappears invisible. A launch
//!    that loses every member is dropped entirely.
//! 4. A process that exits after its launch was reported is reported as gone,
//!    with the executable and parent it was reported with. Nothing looks the
//!    process up at that point, because there is nothing left to look up.
//! 5. Exits are gathered for [`WatchConfig::exit_settle_period`] and reported
//!    children first, so that a parent and a child dying together arrive in an
//!    order that means something.
//! 6. A process identifier that starts again while the watcher still believes
//!    it is alive means Windows reused it. The old one is reported gone first.

use std::collections::HashMap;
use std::time::Instant;

use super::config::WatchConfig;
use super::process::{LaunchGroup, LaunchId, ProcessExit, ProcessSnapshot};
use super::source::SourceEvent;
use super::WatchEvent;

/// A launch that is still being collected.
#[derive(Debug)]
struct PendingLaunch {
    /// The identifier its members will be reported with.
    id: LaunchId,
    /// When its first member started, which fixes the cap on collection.
    opened_at: Instant,
    /// When it will be reported, if nothing else joins it.
    quiet_until: Instant,
    /// Its members, oldest first.
    members: Vec<ProcessSnapshot>,
}

impl PendingLaunch {
    /// Whether `pid` is one of this launch's members.
    fn contains(&self, pid: u32) -> bool {
        self.members.iter().any(|member| member.pid == pid)
    }
}

/// A process the watcher has reported and still believes is running.
#[derive(Debug)]
struct LiveProcess {
    process: ProcessSnapshot,
    launch: LaunchId,
}

/// An exit waiting for its batch to be complete.
#[derive(Debug)]
struct PendingExit {
    process: ProcessSnapshot,
    launch: LaunchId,
    settle_at: Instant,
}

/// Collects raw process events into launches and exits.
#[derive(Debug)]
pub(crate) struct LaunchDebouncer {
    config: WatchConfig,
    /// Launches still being collected, oldest first.
    pending: Vec<PendingLaunch>,
    /// Everything reported and believed to be running, by process identifier.
    live: HashMap<u32, LiveProcess>,
    /// Exits waiting out [`WatchConfig::exit_settle_period`].
    exiting: Vec<PendingExit>,
    /// The identifier the next launch will take.
    next_launch: LaunchId,
}

impl LaunchDebouncer {
    /// A debouncer that has seen nothing yet.
    pub(crate) fn new(config: WatchConfig) -> Self {
        Self {
            config,
            pending: Vec::new(),
            live: HashMap::new(),
            exiting: Vec::new(),
            next_launch: LaunchId::first(),
        }
    }

    /// Records processes that were already running as
    /// [`LaunchId::ALREADY_RUNNING`].
    ///
    /// Nothing is reported for them — they did not start, they were there — but
    /// they are remembered, so that a game running before Clipped opened still
    /// reports its exit with a name attached.
    ///
    /// Processes already known are left alone. A source takes its baseline and
    /// begins delivering events at slightly different moments, and the event is
    /// the better record: it was observed, where the baseline only says the
    /// process existed by the time somebody looked.
    pub(crate) fn seed(&mut self, running: &[ProcessSnapshot]) {
        for process in running {
            if self.knows(process.pid) {
                continue;
            }
            self.live.insert(
                process.pid,
                LiveProcess {
                    process: process.clone(),
                    launch: LaunchId::ALREADY_RUNNING,
                },
            );
        }
    }

    /// Every process identifier the watcher is currently accounting for.
    ///
    /// This is what a replacement source is given so that its first pass
    /// reports what changed rather than announcing the whole machine.
    pub(crate) fn known_pids(&self) -> Vec<u32> {
        let live = self.live.keys().copied();
        let collecting = self
            .pending
            .iter()
            .flat_map(|launch| launch.members.iter().map(|member| member.pid));
        let leaving = self.exiting.iter().map(|exit| exit.process.pid);
        live.chain(collecting).chain(leaving).collect()
    }

    /// Whether this identifier is anywhere in the machine's state.
    fn knows(&self, pid: u32) -> bool {
        self.live.contains_key(&pid)
            || self.pending.iter().any(|launch| launch.contains(pid))
            || self.exiting.iter().any(|exit| exit.process.pid == pid)
    }

    /// Takes one raw event. Nothing is reported until [`Self::drain`].
    pub(crate) fn observe(&mut self, event: SourceEvent, now: Instant) {
        match event {
            SourceEvent::Started(process) => self.observe_start(process, now),
            SourceEvent::Exited { pid } => self.observe_exit(pid, now),
        }
    }

    fn observe_start(&mut self, process: ProcessSnapshot, now: Instant) {
        // Rule 6: this identifier cannot belong to two live processes.
        self.retire(process.pid, now);

        // Rule 1: join the parent's launch if it is still being collected.
        if let Some(launch) = self
            .pending
            .iter_mut()
            .find(|launch| launch.contains(process.parent_pid))
        {
            // Rule 2. The cap is what stops a launcher that starts a helper
            // every second from deferring its own launch indefinitely.
            let cap = launch.opened_at + self.config.max_launch_window;
            launch.quiet_until = (now + self.config.launch_quiet_period).min(cap);
            launch.members.push(process);
            return;
        }

        let id = self.next_launch;
        self.next_launch = id.next();
        self.pending.push(PendingLaunch {
            id,
            opened_at: now,
            quiet_until: now + self.config.launch_quiet_period,
            members: vec![process],
        });
    }

    fn observe_exit(&mut self, pid: u32, now: Instant) {
        // Rule 3: a member of a launch that has not been reported leaves
        // without trace.
        if self.forget_pending(pid) {
            return;
        }

        // Rule 4, delayed by rule 5. An exit for something the watcher never
        // reported — a process that was running before the baseline was taken,
        // or one it has already reported gone — is not news, and falls through.
        if let Some(live) = self.live.remove(&pid) {
            self.exiting.push(PendingExit {
                process: live.process,
                launch: live.launch,
                settle_at: now + self.config.exit_settle_period,
            });
        }
    }

    /// Removes `pid` from the launch collecting it, dropping an emptied launch.
    ///
    /// Reports whether it was there.
    fn forget_pending(&mut self, pid: u32) -> bool {
        let Some(index) = self.pending.iter().position(|launch| launch.contains(pid)) else {
            return false;
        };

        let launch = &mut self.pending[index];
        launch.members.retain(|member| member.pid != pid);
        if launch.members.is_empty() {
            self.pending.remove(index);
        }
        true
    }

    /// Handles an identifier being reused while the watcher still believes it
    /// is alive.
    fn retire(&mut self, pid: u32, now: Instant) {
        if self.forget_pending(pid) {
            return;
        }

        if let Some(live) = self.live.remove(&pid) {
            // Reported immediately rather than after the settle period: this
            // exit is already old news — it happened at some point before the
            // reuse — and it has to precede the launch that reused the
            // identifier.
            self.exiting.push(PendingExit {
                process: live.process,
                launch: live.launch,
                settle_at: now,
            });
        }
    }

    /// Everything that is due at `now`, in the order a consumer should see it.
    ///
    /// Exits come before launches. That is what makes rule 6 hold — the stale
    /// process is gone before its identifier is used again — and it is the
    /// order a session layer wants anyway: stop what has ended before starting
    /// what has begun.
    pub(crate) fn drain(&mut self, now: Instant) -> Vec<WatchEvent> {
        let mut events = Vec::new();

        let mut due: Vec<PendingExit> = Vec::new();
        let mut waiting = Vec::with_capacity(self.exiting.len());
        for exit in self.exiting.drain(..) {
            if exit.settle_at <= now {
                due.push(exit);
            } else {
                waiting.push(exit);
            }
        }
        self.exiting = waiting;

        order_children_first(&mut due);
        events.extend(due.into_iter().map(|exit| {
            WatchEvent::Exited(ProcessExit {
                process: exit.process,
                launch: exit.launch,
            })
        }));

        let mut ready = Vec::new();
        let mut collecting = Vec::with_capacity(self.pending.len());
        for launch in self.pending.drain(..) {
            if launch.quiet_until <= now {
                ready.push(launch);
            } else {
                collecting.push(launch);
            }
        }
        self.pending = collecting;

        ready.sort_by_key(|launch| launch.id);
        for launch in ready {
            for member in &launch.members {
                self.live.insert(
                    member.pid,
                    LiveProcess {
                        process: member.clone(),
                        launch: launch.id,
                    },
                );
            }
            events.push(WatchEvent::Launched(LaunchGroup {
                id: launch.id,
                processes: launch.members,
            }));
        }

        events
    }

    /// The earliest moment [`Self::drain`] could produce anything.
    ///
    /// [`None`] when nothing is outstanding, which means the watcher can block
    /// on its source indefinitely rather than waking up to find nothing to do.
    /// This is the reason the debounce costs no CPU while it waits.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let launches = self.pending.iter().map(|launch| launch.quiet_until);
        let exits = self.exiting.iter().map(|exit| exit.settle_at);
        launches.chain(exits).min()
    }
}

/// Orders a batch of exits so that a process is reported before its parent.
///
/// Only relationships *within the batch* count: a parent that is not exiting
/// too is irrelevant to the order. Depth is counted by walking the parent
/// links present in the batch, bounded by the batch length so that a cycle —
/// which Windows should not produce, and which would otherwise hang here —
/// stops rather than spins.
fn order_children_first(exits: &mut [PendingExit]) {
    let parents: HashMap<u32, u32> = exits
        .iter()
        .map(|exit| (exit.process.pid, exit.process.parent_pid))
        .collect();

    let depth = |pid: u32| -> usize {
        let mut depth = 0;
        let mut current = pid;
        while let Some(&parent) = parents.get(&current) {
            if parent == current || depth >= parents.len() {
                break;
            }
            match parents.contains_key(&parent) {
                true => {
                    depth += 1;
                    current = parent;
                }
                false => break,
            }
        }
        depth
    };

    exits.sort_by_key(|exit| (std::cmp::Reverse(depth(exit.process.pid)), exit.process.pid));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    /// A start event for a process with no path, which is the harder case: the
    /// tests care about identifiers and parents, and a name is enough to tell
    /// the members of a launch apart when one fails.
    fn started(pid: u32, parent_pid: u32, name: &str) -> SourceEvent {
        SourceEvent::Started(ProcessSnapshot::new(
            pid,
            parent_pid,
            Some(PathBuf::from(format!(r"C:\Games\{name}"))),
            name,
        ))
    }

    fn exited(pid: u32) -> SourceEvent {
        SourceEvent::Exited { pid }
    }

    /// Settings with round numbers, so that a test's arithmetic is readable.
    fn config() -> WatchConfig {
        WatchConfig {
            notification_interval: Duration::from_secs(1),
            launch_quiet_period: Duration::from_millis(1_000),
            max_launch_window: Duration::from_millis(5_000),
            exit_settle_period: Duration::from_millis(200),
        }
    }

    fn launches(events: &[WatchEvent]) -> Vec<&LaunchGroup> {
        events
            .iter()
            .filter_map(|event| match event {
                WatchEvent::Launched(group) => Some(group),
                _ => None,
            })
            .collect()
    }

    fn exits(events: &[WatchEvent]) -> Vec<&ProcessExit> {
        events
            .iter()
            .filter_map(|event| match event {
                WatchEvent::Exited(exit) => Some(exit),
                _ => None,
            })
            .collect()
    }

    fn names(group: &LaunchGroup) -> Vec<&str> {
        group
            .processes
            .iter()
            .map(|process| process.image_name.as_str())
            .collect()
    }

    #[test]
    fn a_launcher_and_the_game_it_starts_are_one_launch() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        // Steam (pid 100) is already running and is not part of this.
        debouncer.observe(started(200, 100, "launcher.exe"), start);
        debouncer.observe(
            started(201, 200, "game.exe"),
            start + Duration::from_millis(400),
        );

        // Nothing yet: the launch is still quiet-waiting on its newest member.
        assert!(debouncer
            .drain(start + Duration::from_millis(1_100))
            .is_empty());

        let events = debouncer.drain(start + Duration::from_millis(1_500));
        let groups = launches(&events);
        assert_eq!(groups.len(), 1, "expected one launch, got {events:#?}");
        assert_eq!(names(groups[0]), ["launcher.exe", "game.exe"]);
        assert_eq!(groups[0].newest().pid, 201);
        assert_eq!(groups[0].root().pid, 200);
    }

    #[test]
    fn a_launcher_that_hands_over_and_exits_is_never_reported() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(200, 100, "launcher.exe"), start);
        debouncer.observe(
            started(201, 200, "game.exe"),
            start + Duration::from_millis(100),
        );
        // The launcher hands over and disappears, inside the window.
        debouncer.observe(exited(200), start + Duration::from_millis(300));

        let events = debouncer.drain(start + Duration::from_millis(2_000));
        let groups = launches(&events);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            names(groups[0]),
            ["game.exe"],
            "the launcher exited before the launch was reported and is not news"
        );
        assert!(
            exits(&events).is_empty(),
            "and neither is its exit: {events:#?}"
        );
    }

    #[test]
    fn a_game_that_re_executes_itself_is_one_launch() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(300, 100, "game.exe"), start);
        // The first copy starts a second one and stands aside, which is what a
        // title that re-launches itself under a different bitness or with a
        // patched command line does.
        debouncer.observe(
            started(301, 300, "game.exe"),
            start + Duration::from_millis(250),
        );
        debouncer.observe(exited(300), start + Duration::from_millis(260));

        let events = debouncer.drain(start + Duration::from_millis(2_000));
        let groups = launches(&events);
        assert_eq!(groups.len(), 1, "expected one launch, got {events:#?}");
        assert_eq!(groups[0].processes.len(), 1);
        assert_eq!(
            groups[0].newest().pid,
            301,
            "the surviving copy is what was launched"
        );
    }

    #[test]
    fn two_unrelated_games_starting_at_once_are_two_launches() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        // Two storefronts, two games, the same instant.
        debouncer.observe(started(400, 100, "first.exe"), start);
        debouncer.observe(started(500, 101, "second.exe"), start);

        let events = debouncer.drain(start + Duration::from_millis(1_100));
        let groups = launches(&events);
        assert_eq!(groups.len(), 2, "expected two launches, got {events:#?}");
        assert_eq!(names(groups[0]), ["first.exe"]);
        assert_eq!(names(groups[1]), ["second.exe"]);
        assert_ne!(
            groups[0].id, groups[1].id,
            "unrelated launches must not share an identifier"
        );
    }

    #[test]
    fn a_process_that_starts_and_exits_inside_the_window_is_never_reported() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        // An installer's helper, a shader-cache job, a crash reporter.
        debouncer.observe(started(600, 100, "helper.exe"), start);
        debouncer.observe(exited(600), start + Duration::from_millis(400));

        let events = debouncer.drain(start + Duration::from_millis(5_000));
        assert!(events.is_empty(), "expected silence, got {events:#?}");
        assert_eq!(debouncer.next_deadline(), None);
    }

    #[test]
    fn a_parent_and_child_dying_together_are_reported_child_first() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(700, 100, "launcher.exe"), start);
        debouncer.observe(
            started(701, 700, "game.exe"),
            start + Duration::from_millis(100),
        );
        debouncer.observe(
            started(702, 701, "child.exe"),
            start + Duration::from_millis(200),
        );
        assert_eq!(
            launches(&debouncer.drain(start + Duration::from_secs(2))).len(),
            1
        );

        let dying = start + Duration::from_secs(3);
        // Delivered parent-first, which is the order that must not survive.
        debouncer.observe(exited(700), dying);
        debouncer.observe(exited(701), dying);
        debouncer.observe(exited(702), dying);

        let events = debouncer.drain(dying + Duration::from_millis(250));
        let gone: Vec<u32> = exits(&events).iter().map(|exit| exit.process.pid).collect();
        assert_eq!(
            gone,
            [702, 701, 700],
            "a process must be reported gone before its parent"
        );
    }

    #[test]
    fn an_exit_carries_what_the_process_was_reported_with() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(800, 100, "game.exe"), start);
        let launched = debouncer.drain(start + Duration::from_secs(2));
        let id = launches(&launched)[0].id;

        debouncer.observe(exited(800), start + Duration::from_secs(3));
        let events = debouncer.drain(start + Duration::from_millis(3_500));
        let gone = exits(&events);

        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].launch, id, "the exit belongs to its launch");
        assert_eq!(gone[0].process.pid, 800);
        assert_eq!(gone[0].process.parent_pid, 100);
        assert_eq!(
            gone[0].process.image_path,
            Some(PathBuf::from(r"C:\Games\game.exe")),
            "nothing can look the executable up once the process has gone"
        );
    }

    #[test]
    fn the_quiet_period_is_measured_from_the_newest_member() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(900, 100, "launcher.exe"), start);
        debouncer.observe(
            started(901, 900, "wrapper.exe"),
            start + Duration::from_millis(900),
        );

        assert!(
            debouncer
                .drain(start + Duration::from_millis(1_800))
                .is_empty(),
            "the launch was still collecting 900ms after its newest member"
        );
        assert_eq!(
            launches(&debouncer.drain(start + Duration::from_millis(1_950))).len(),
            1
        );
    }

    #[test]
    fn a_launch_is_reported_once_the_cap_is_reached_however_busy_it_is() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(1_000, 100, "launcher.exe"), start);
        // A helper every 500ms forever: without the cap the quiet period is
        // never reached and the launch is never reported.
        for (pid, step) in (1_001..).zip(1..=20) {
            let now = start + Duration::from_millis(500 * step);
            debouncer.observe(started(pid, 1_000, "helper.exe"), now);
        }

        let events = debouncer.drain(start + Duration::from_millis(5_000));
        let groups = launches(&events);
        assert_eq!(
            groups.len(),
            1,
            "the cap must report the launch at max_launch_window"
        );
        assert_eq!(
            groups[0].root().pid,
            1_000,
            "and it is still the same launch"
        );
    }

    #[test]
    fn a_child_starting_after_its_launch_was_reported_is_a_new_launch() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(1_100, 100, "launcher.exe"), start);
        assert_eq!(
            launches(&debouncer.drain(start + Duration::from_millis(1_100))).len(),
            1
        );

        // The launcher was reported two seconds ago; a game started from its
        // menu now is a separate launch, not an extension of the old one.
        // Without this, every game ever started from one Steam session would
        // fold into a single launch that never ends.
        debouncer.observe(
            started(1_101, 1_100, "game.exe"),
            start + Duration::from_secs(2),
        );
        let events = debouncer.drain(start + Duration::from_millis(3_500));
        let groups = launches(&events);
        assert_eq!(groups.len(), 1);
        assert_eq!(names(groups[0]), ["game.exe"]);
    }

    #[test]
    fn a_reused_identifier_reports_the_old_process_gone_first() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(1_200, 100, "first.exe"), start);
        assert_eq!(
            launches(&debouncer.drain(start + Duration::from_millis(1_100))).len(),
            1
        );

        // Windows hands 1200 out again without the watcher having seen the
        // first one go, which is what a missed deletion event looks like.
        debouncer.observe(
            started(1_200, 100, "second.exe"),
            start + Duration::from_secs(2),
        );
        let events = debouncer.drain(start + Duration::from_millis(3_500));

        assert!(
            matches!(&events[0], WatchEvent::Exited(exit) if exit.process.image_name == "first.exe"),
            "the stale process must be reported gone before the identifier is reused: {events:#?}"
        );
        let groups = launches(&events);
        assert_eq!(groups.len(), 1);
        assert_eq!(names(groups[0]), ["second.exe"]);
    }

    #[test]
    fn a_process_that_was_already_running_still_reports_its_exit() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.seed(&[ProcessSnapshot::new(
            1_300,
            100,
            Some(PathBuf::from(r"C:\Games\running.exe")),
            "running.exe",
        )]);
        assert!(
            debouncer.drain(start).is_empty(),
            "a process that was already there did not start"
        );

        debouncer.observe(exited(1_300), start);
        let events = debouncer.drain(start + Duration::from_millis(250));
        let gone = exits(&events);

        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].process.image_name, "running.exe");
        assert!(
            gone[0].launch.is_already_running(),
            "it belongs to no launch this watcher saw"
        );
    }

    #[test]
    fn the_baseline_never_overwrites_something_already_observed() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(1_400, 100, "game.exe"), start);
        // A baseline arriving while a launch is still being collected — the
        // source took its snapshot after its first events — must not turn that
        // launch into a process that was merely already there.
        debouncer.seed(&[ProcessSnapshot::new(1_400, 100, None, "game.exe")]);

        let events = debouncer.drain(start + Duration::from_millis(1_100));
        let launched = launches(&events);
        assert_eq!(launched.len(), 1, "{events:#?}");
        let launch = launched[0].id;

        // And a baseline arriving after the launch was reported — which is what
        // a source restart delivers — must not detach the process from it. If
        // it did, the exit below would be attributed to no launch at all, and a
        // session keyed on the launch would never learn that its game had gone.
        debouncer.seed(&[ProcessSnapshot::new(1_400, 100, None, "game.exe")]);
        debouncer.observe(exited(1_400), start + Duration::from_secs(2));

        let events = debouncer.drain(start + Duration::from_millis(2_500));
        let gone = exits(&events);
        assert_eq!(gone.len(), 1, "{events:#?}");
        assert_eq!(
            gone[0].launch, launch,
            "the process still belongs to the launch it was reported with"
        );
    }

    #[test]
    fn an_exit_for_a_process_never_seen_is_ignored() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(exited(1_500), start);
        debouncer.observe(exited(1_500), start);

        assert!(debouncer.drain(start + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn the_deadline_is_the_earliest_thing_outstanding() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        assert_eq!(
            debouncer.next_deadline(),
            None,
            "an idle watcher has nothing to wake up for"
        );

        debouncer.observe(started(1_600, 100, "game.exe"), start);
        assert_eq!(
            debouncer.next_deadline(),
            Some(start + Duration::from_millis(1_000))
        );

        debouncer.drain(start + Duration::from_millis(1_100));
        debouncer.observe(exited(1_600), start + Duration::from_millis(1_200));
        assert_eq!(
            debouncer.next_deadline(),
            Some(start + Duration::from_millis(1_400)),
            "the settle period on the exit is now the earliest deadline"
        );
    }

    #[test]
    fn exits_are_reported_before_launches_in_the_same_drain() {
        let start = Instant::now();
        let mut debouncer = LaunchDebouncer::new(config());

        debouncer.observe(started(1_700, 100, "first.exe"), start);
        debouncer.drain(start + Duration::from_millis(1_100));

        let later = start + Duration::from_secs(2);
        debouncer.observe(exited(1_700), later);
        debouncer.observe(started(1_701, 100, "second.exe"), later);

        let events = debouncer.drain(later + Duration::from_millis(1_100));
        assert!(
            matches!(
                events.as_slice(),
                [WatchEvent::Exited(_), WatchEvent::Launched(_)]
            ),
            "stop what has ended before starting what has begun: {events:#?}"
        );
    }
}
