//! Automatic recording: the policy between "a game started" and "record it".
//!
//! Everything under this module already existed separately.
//! `clipped_game_detection::ProcessWatcher` reports what started and what
//! stopped ([#41]); `Catalogue::match_process` says whether a process is a game
//! ([#42]); [`crate::record`] records a window to a playable file ([#126]).
//! What was missing, and what is here, is the *policy* joining them and the
//! session model around it ([#46]) — which is the thing the product exists to
//! do (SPEC.md section 2, "Automatic").
//!
//! [#41]: https://github.com/wildware-uk/clipped/issues/41
//! [#42]: https://github.com/wildware-uk/clipped/issues/42
//! [#46]: https://github.com/wildware-uk/clipped/issues/46
//! [#126]: https://github.com/wildware-uk/clipped/issues/126
//!
//! # The shape of it: judge here, act there
//!
//! ```text
//!  ProcessWatcher ──▶ WatchEvent ──▶ SessionManager ──▶ SessionAction ──▶ the driver
//!                                     Catalogue          start / stop      resolve a window,
//!                                     the session        a recording       run clipped_session::record
//! ```
//!
//! [`SessionManager`] touches no window, opens no encoder and starts no thread.
//! It is a state machine over `(event, wall-clock reading)` that returns
//! [`SessionAction`]s, exactly as `clipped_game_detection`'s launch debounce is
//! a state machine over `(event, now)`, and for the same reason: every rule
//! below — a crash, a fast restart, a second game, a suspend — is a rule about
//! *timing and identity* and can be tested against constructed launches on any
//! machine, with no game, no GPU and no waiting. `apps/recorder`'s `watch`
//! subcommand is the driver that turns the actions into recordings.
//!
//! # The rules, and why each one is what it is
//!
//! **A process the catalogue does not claim is not a game.** Nothing is
//! recorded, at `debug`, and the overwhelming majority of launches on a Windows
//! machine end here.
//!
//! **A tie is recorded and left unattributed.** `Match::Ambiguous` is a real
//! answer: several catalogue entries claim the executable equally well and the
//! catalogue deliberately does not guess between them. Not recording would lose
//! footage that cannot be made again; guessing would file somebody's session
//! under the wrong game, silently. So the session is recorded, filed under
//! [`session::UNATTRIBUTED`], and every candidate is written into its sidecar
//! for a person — or M6's library — to resolve later.
//!
//! **The game exiting ends the recording, however it exits.** A crash is not a
//! special case here: the process vanishes, the watcher reports it, the stop
//! signal is raised and `clipped_session::record` finalises the container on
//! its way out (AGENTS.md section 17, [ADR 0001]). In practice the recording has
//! usually ended already, because the window went first.
//!
//! **A fast restart is one session with two recordings.** The session stays
//! open for [`AutomaticSettings::restart_grace`] after the game exits, and the
//! same game launching inside that window rejoins it. Two files, because a
//! container cannot span a gap and there is one encoder — but one session, so
//! the library is not fragmented by somebody alt-F4ing and coming straight
//! back. A relaunch after the grace, or of a different game, is a new session.
//!
//! **`child_processes` extends a session's life and never its capture.** The
//! catalogue carries a game's known helpers and deliberately does not match on
//! them, because a crash handler is not the game. Here they keep the session
//! *open* — the grace period does not start while one of them from the same
//! launch is still running — and they are never a capture target, so a game
//! that quit to its crash reporter does not have the crash reporter recorded.
//!
//! **A second game during an active session is noted, not recorded.** There is
//! one encoder and one capture target. Pre-empting would truncate a recording
//! the user is in the middle of, so the launch is written into the active
//! session's events and remembered; when that session ends, the most recently
//! deferred game that is *still running* becomes a session of its own and is
//! recorded from that moment. One that exited in the meantime is forgotten.
//!
//! **A suspend ends the recording rather than putting a hole in it.** The
//! manager is driven by a loop that calls it at least once a second, so a
//! wall-clock jump of [`AutomaticSettings::suspend_gap`] means the machine
//! slept. A file whose timestamps span eight hours of nothing is not a
//! recording anybody can use, so the current one is finished and — if the game
//! is still running — another begins in the same session. A session that was
//! inside its restart grace is closed outright: hours passed, not seconds.
//!
//! This is inferred from the clock rather than from `WM_POWERBROADCAST`.
//! Subscribing to the power notification would give the manager warning
//! *before* the suspend and let it finish the file first, which is better, and
//! it needs a message loop this crate does not have; it is
//! [issue #240](https://github.com/wildware-uk/clipped/issues/240).
//!
//! **A game already running when the recorder starts is not recorded.** Full
//! Session means "start when the game starts", and joining halfway would
//! produce a session that began at an arbitrary moment. The driver says so at
//! `info` rather than leaving the user to wonder
//! ([`SessionManager::already_running_games`]).
//!
//! # The one capture mode
//!
//! Full Session (SPEC.md section 7): start when the game starts, stop when it
//! exits, keep everything. The other three are not offered here rather than
//! being offered and doing nothing (AGENTS.md section 27). Match Recording
//! needs the highlight provider API (M9); Highlights Only and Manual/Replay
//! Buffer need a replay buffer that can be *saved* from, which is
//! [issue #37](https://github.com/wildware-uk/clipped/issues/37) and
//! [issue #38](https://github.com/wildware-uk/clipped/issues/38).
//!
//! Per-game settings are not applied here either. A catalogue entry can carry
//! `default_settings`, and `clipped_game_detection` is explicit that nothing
//! interprets them because per-game configuration is M7 (SPEC.md section 31).
//! Reading them here would be building a later milestone's work.
//!
//! [ADR 0001]: ../../../docs/adr/0001-mkv-archival-container.md

mod clock;
mod session;
mod settings;
mod sidecar;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipped_game_detection::catalogue::{Catalogue, Match, ProcessCandidate};
use clipped_game_detection::{LaunchGroup, ProcessExit, ProcessSnapshot, WatchEvent};

pub use session::{
    GameIdentity, RecordingOutcomeSummary, Session, SessionEndReason, SessionEvent,
    SessionEventKind, SessionId, SessionRecording, UNATTRIBUTED,
};
pub use settings::{
    AutomaticSettings, DEFAULT_MAX_RECORDINGS_PER_SESSION, DEFAULT_RECORDING_RESTART_DELAY,
    DEFAULT_RESTART_GRACE, DEFAULT_SUSPEND_GAP,
};
pub use sidecar::SCHEMA_VERSION as SESSION_SCHEMA_VERSION;

use crate::report::RecordingReport;

/// Which recording of which session.
///
/// A session can contain several recordings, and the driver reports back about
/// one of them after the manager may already have moved on, so an outcome names
/// what it is the outcome *of* rather than being assumed to be the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingId {
    /// The session it belongs to.
    pub session: SessionId,
    /// Which recording of that session, counting from one.
    pub index: u32,
}

/// Why a recording is being asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// The game's process has gone.
    GameExited,
    /// The machine was suspended and resumed, and a file with an hours-long
    /// hole in it is worse than two files.
    SystemResumed,
    /// The recorder is shutting down.
    RecorderStopping,
}

impl StopCause {
    /// The token this cause is logged as.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::GameExited => "game-exited",
            Self::SystemResumed => "system-resumed",
            Self::RecorderStopping => "recorder-stopping",
        }
    }
}

impl std::fmt::Display for StopCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.token())
    }
}

/// A recording the manager wants started.
///
/// It names a *process*, not a window. Resolving one to the other needs the
/// desktop, and a game reported as launched may not have put a window on screen
/// yet — so the wait for one belongs to the driver, which can do it without
/// blocking this state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingRequest {
    /// What to report the outcome against.
    pub recording: RecordingId,
    /// Which game, as far as the catalogue could say.
    pub game: GameIdentity,
    /// The process whose window should be recorded.
    pub process_id: u32,
    /// That process's executable file name, for diagnostics.
    pub image_name: String,
    /// Where the recording goes.
    pub output: PathBuf,
}

/// What the manager wants done.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionAction {
    /// Start recording. The driver reports back through
    /// [`SessionManager::recording_finished`].
    StartRecording(RecordingRequest),

    /// Raise the stop signal of a running recording.
    ///
    /// The recording finalises its file on the way out; the driver still
    /// reports its outcome afterwards.
    StopRecording {
        /// Which recording.
        recording: RecordingId,
        /// Why.
        cause: StopCause,
    },

    /// A session is finished. Nothing further will be added to it.
    ///
    /// Handed over rather than kept, because the recorder runs for days and a
    /// list of every session since login is memory nobody asked for
    /// (AGENTS.md section 59). Its sidecar has already been written.
    SessionEnded(Box<Session>),
}

/// What a recording turned out to be, as the driver reports it.
#[derive(Debug)]
pub enum RecordingOutcome {
    /// It ran, and the file was finalised.
    Recorded(Box<RecordingReport>),

    /// No capturable window belonging to the process appeared in time, so
    /// nothing was recorded and no file was written.
    NoWindow {
        /// What the desktop said, so a user can act on it.
        detail: String,
    },

    /// The recording failed. Whatever had been written was still finalised,
    /// unless it failed before its first packet.
    Failed {
        /// What went wrong.
        detail: String,
    },
}

impl RecordingOutcome {
    /// The form a session keeps.
    fn summarise(&self) -> RecordingOutcomeSummary {
        match self {
            Self::Recorded(report) => RecordingOutcomeSummary::of(report),
            Self::NoWindow { detail } => RecordingOutcomeSummary::NoWindow {
                detail: detail.clone(),
            },
            Self::Failed { detail } => RecordingOutcomeSummary::Failed {
                detail: detail.clone(),
            },
        }
    }
}

/// Decides when a session starts, when it stops, and what it contains.
///
/// # Threading
///
/// One owner. The manager is not `Sync` in spirit even where it is in fact:
/// every method takes `&mut self`, and it is designed to be driven by the same
/// thread that waits on the process watcher. Nothing here blocks, allocates on
/// a capture path or touches the filesystem except to rewrite a session's small
/// sidecar when the session changes.
///
/// # Time
///
/// Every method takes the current wall-clock reading rather than reading a
/// clock itself, which is what makes a fast restart, a grace period and a
/// suspend testable without waiting for any of them to happen (AGENTS.md
/// section 25). The caller is expected to call [`Self::poll`] or
/// [`Self::observe`] at least once a second; the suspend rule is stated in
/// terms of that promise.
#[derive(Debug)]
pub struct SessionManager {
    catalogue: Catalogue,
    settings: AutomaticSettings,
    active: Option<Active>,
    /// Games that launched while a session was already recording, newest last.
    deferred: Vec<MatchedLaunch>,
    last_observed: Option<SystemTime>,
    stopping: bool,
}

/// The session currently open, and everything the policy needs about it.
#[derive(Debug)]
struct Active {
    session: Session,
    subject: Subject,
    /// Lower-cased executable names the catalogue lists as this game's
    /// children. Never a capture target; see the module documentation.
    child_names: BTreeSet<String>,
    /// Those of them that are still running, from the launch that started this
    /// session.
    live_children: BTreeSet<u32>,
    /// The recording that is running, if one is.
    recording: Option<u32>,
    /// Set once a stop has been asked for and before the outcome arrives, so a
    /// second reason to stop does not produce a second request.
    stopping: bool,
    /// When the next recording of this session may start.
    pending_start: Option<SystemTime>,
    /// When the session ran out of live processes, which starts the grace.
    idle_since: Option<SystemTime>,
    /// How many recordings the session has started.
    started_recordings: u32,
    /// Set when a recording found no window, so the manager stops looking for
    /// one. Cleared when the game relaunches, which is a new process and a new
    /// question.
    no_window: bool,
    /// Set once the cap has been reported, so it is reported once.
    limit_reported: bool,
}

/// The process a session is of.
#[derive(Debug)]
struct Subject {
    pid: u32,
    image_name: String,
    alive: bool,
}

/// A launch the catalogue recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchedLaunch {
    game: GameIdentity,
    pid: u32,
    image_name: String,
    child_names: BTreeSet<String>,
    child_pids: BTreeSet<u32>,
}

impl SessionManager {
    /// A manager with nothing running.
    #[must_use]
    pub fn new(catalogue: Catalogue, settings: AutomaticSettings) -> Self {
        Self {
            catalogue,
            settings,
            active: None,
            deferred: Vec::new(),
            last_observed: None,
            stopping: false,
        }
    }

    /// The session that is open, if one is.
    #[must_use]
    pub fn active_session(&self) -> Option<&Session> {
        self.active.as_ref().map(|active| &active.session)
    }

    /// Whether a recording is running.
    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.recording.is_some())
    }

    /// Which of the processes that were already running are games.
    ///
    /// None of them will be recorded — see the module documentation — and this
    /// is how the driver can say so instead of leaving a user to wonder why the
    /// game they were already playing is not being captured (AGENTS.md
    /// section 27).
    #[must_use]
    pub fn already_running_games(&self, processes: &[ProcessSnapshot]) -> Vec<GameIdentity> {
        let mut found: Vec<GameIdentity> = Vec::new();
        for process in processes {
            if let Some((game, _)) = self.identify(process) {
                if !found.contains(&game) {
                    found.push(game);
                }
            }
        }
        found
    }

    /// Folds one watcher event in.
    #[must_use]
    pub fn observe(&mut self, event: &WatchEvent, now: SystemTime) -> Vec<SessionAction> {
        let mut actions = self.advance(now);
        if self.stopping {
            return actions;
        }

        match event {
            WatchEvent::Launched(group) => self.launched(group, now, &mut actions),
            WatchEvent::Exited(exit) => self.exited(exit, now, &mut actions),
            // The watcher talking about itself. Detection continuing on a
            // slower source changes nothing about an open session, and a
            // watcher that has stopped is reported to the user by the driver,
            // which is the only part that can do anything about it.
            _ => {}
        }

        actions
    }

    /// Lets time pass, when nothing happened.
    ///
    /// This is what closes a restart grace period, releases a delayed restart
    /// and notices a suspend, so a driver that only called [`Self::observe`]
    /// would sit on a session that should have ended for as long as the machine
    /// stayed quiet.
    #[must_use]
    pub fn poll(&mut self, now: SystemTime) -> Vec<SessionAction> {
        self.advance(now)
    }

    /// Tells the manager what a recording turned out to be.
    ///
    /// An outcome for a recording the manager has moved past is ignored rather
    /// than applied to its successor.
    #[must_use]
    pub fn recording_finished(
        &mut self,
        recording: &RecordingId,
        outcome: RecordingOutcome,
        now: SystemTime,
    ) -> Vec<SessionAction> {
        let mut actions = Vec::new();
        let expected = self.active.as_ref().is_some_and(|active| {
            active.session.id() == &recording.session && active.recording == Some(recording.index)
        });

        if !expected {
            tracing::debug!(
                session = recording.session.as_str(),
                index = recording.index,
                "an outcome arrived for a recording this session is no longer running"
            );
            actions.extend(self.advance(now));
            return actions;
        }

        let stopping = self.stopping;
        let delay = self.settings.recording_restart_delay();
        let summary = outcome.summarise();
        let produced_a_file = summary.produced_a_file();
        let outcome_token = summary.token();

        let active = self
            .active
            .as_mut()
            .expect("the outcome was checked against an active session");
        active.recording = None;
        active.stopping = false;
        if matches!(summary, RecordingOutcomeSummary::NoWindow { .. }) {
            active.no_window = true;
        }
        active.session.end_recording(recording.index, summary, now);
        persist(self.settings.directory(), &active.session);

        tracing::info!(
            session = active.session.id().as_str(),
            index = recording.index,
            outcome = outcome_token,
            produced_a_file,
            "a recording of the session ended"
        );

        if !stopping {
            if active.subject.alive && !active.no_window {
                // The game is still running, so the session is not over: the
                // window went, or changed size, or the machine slept. Another
                // recording follows, after long enough that an exit the watcher
                // has not reported yet has time to arrive.
                active.pending_start = Some(now + delay);
            } else if !active.subject.alive && active.live_children.is_empty() {
                active.idle_since = Some(now);
            }
        }

        if stopping {
            self.close_active(SessionEndReason::RecorderStopping, now, &mut actions);
        }
        actions.extend(self.advance(now));
        actions
    }

    /// Stops everything, because the recorder is stopping.
    ///
    /// Any running recording is asked to stop; the session is closed as soon as
    /// its outcome arrives, or at once if nothing was running. Nothing further
    /// is started afterwards.
    #[must_use]
    pub fn shut_down(&mut self, now: SystemTime) -> Vec<SessionAction> {
        self.stopping = true;
        self.deferred.clear();

        let mut actions = Vec::new();
        let mut stop = None;
        // Whether a recording is still owed an outcome — which is not the same
        // question as whether this shutdown is the thing that stops it. A stop
        // asked for a moment earlier, because the game exited or the machine
        // resumed, has already reached the recording and its outcome is still
        // on its way. Closing the session now would throw that outcome away and
        // leave behind a record saying a recording began and never ended, which
        // is precisely what M6's indexer would later have to reconcile against
        // a file that is sitting there, finalised and perfectly playable.
        let mut owed_an_outcome = false;
        if let Some(active) = self.active.as_mut() {
            active.pending_start = None;
            if let Some(index) = active.recording {
                owed_an_outcome = true;
                if !active.stopping {
                    active.stopping = true;
                    stop = Some(RecordingId {
                        session: active.session.id().clone(),
                        index,
                    });
                }
            }
        }

        if let Some(recording) = stop {
            actions.push(SessionAction::StopRecording {
                recording,
                cause: StopCause::RecorderStopping,
            });
        } else if !owed_an_outcome {
            self.close_active(SessionEndReason::RecorderStopping, now, &mut actions);
        }
        actions
    }

    /// Everything that depends only on the clock having moved.
    fn advance(&mut self, now: SystemTime) -> Vec<SessionAction> {
        let mut actions = Vec::new();

        let gap = self
            .last_observed
            .and_then(|last| now.duration_since(last).ok());
        self.last_observed = Some(now);
        if let Some(gap) = gap.filter(|gap| *gap >= self.settings.suspend_gap()) {
            self.resumed(gap, now, &mut actions);
        }
        if self.stopping {
            return actions;
        }

        let grace = self.settings.restart_grace();
        let expired = self
            .active
            .as_ref()
            .and_then(|active| active.idle_since)
            .is_some_and(|idle| now.duration_since(idle).unwrap_or_default() >= grace);
        if expired {
            self.close_active(SessionEndReason::GameExited, now, &mut actions);
        }

        let due = self
            .active
            .as_ref()
            .and_then(|active| active.pending_start)
            .is_some_and(|due| now >= due);
        if due {
            if let Some(action) = self.begin_recording(now) {
                actions.push(action);
            }
        }

        actions
    }

    /// What a suspend and resume does to whatever was going on.
    fn resumed(&mut self, gap: Duration, now: SystemTime, actions: &mut Vec<SessionAction>) {
        tracing::warn!(
            gap_seconds = gap.as_secs(),
            "the wall clock jumped further than this loop can account for; treating it as a \
             suspend and resume"
        );
        // A launch remembered before a suspend says nothing about what is
        // running now, and the exits that happened during the suspend arrive
        // as a batch the manager cannot order against it.
        self.deferred.clear();

        let mut close = false;
        if let Some(active) = self.active.as_mut() {
            active
                .session
                .record(now, SessionEventKind::SystemResumed { gap });
            match active.recording {
                Some(index) if !active.stopping => {
                    active.stopping = true;
                    actions.push(SessionAction::StopRecording {
                        recording: RecordingId {
                            session: active.session.id().clone(),
                            index,
                        },
                        cause: StopCause::SystemResumed,
                    });
                }
                // A session waiting for the same game to come back was waiting
                // for seconds, and hours went past.
                None if active.idle_since.is_some() => close = true,
                _ => {}
            }
            persist(self.settings.directory(), &active.session);
        }

        if close {
            self.close_active(SessionEndReason::SystemResumed, now, actions);
        }
    }

    /// A launch the watcher reported.
    fn launched(&mut self, group: &LaunchGroup, now: SystemTime, actions: &mut Vec<SessionAction>) {
        let Some(matched) = self.match_launch(group) else {
            tracing::debug!(
                launch = group.id.get(),
                processes = group.processes.len(),
                image = group.newest().image_name.as_str(),
                "a launch matched no game in the catalogue"
            );
            return;
        };

        let rejoins = self.active.as_ref().is_some_and(|active| {
            !active.subject.alive && active.session.game().is_same_game_as(&matched.game)
        });

        if rejoins {
            self.rejoin(matched, now, actions);
            return;
        }

        if self.active.is_some() {
            self.defer(matched, now);
            return;
        }

        self.start_session(matched, now, actions);
    }

    /// The same game, back inside its session's restart grace.
    fn rejoin(
        &mut self,
        matched: MatchedLaunch,
        now: SystemTime,
        actions: &mut Vec<SessionAction>,
    ) {
        let already_recording = {
            let active = self
                .active
                .as_mut()
                .expect("a rejoin is only decided against an active session");
            tracing::info!(
                session = active.session.id().as_str(),
                game = matched.game.slug(),
                pid = matched.pid,
                "the same game launched again while its session was still open, so this is one \
                 session with another recording in it rather than two sessions"
            );
            active
                .session
                .record(now, SessionEventKind::GameRelaunched { pid: matched.pid });
            active.subject = Subject {
                pid: matched.pid,
                image_name: matched.image_name,
                alive: true,
            };
            active.child_names = matched.child_names;
            active.live_children = matched.child_pids;
            active.idle_since = None;
            active.no_window = false;
            persist(self.settings.directory(), &active.session);
            active.recording.is_some()
        };

        // A recording that is still finalising owns the encoder. Its outcome
        // arriving is what starts the next one, in `recording_finished`.
        if !already_recording {
            if let Some(action) = self.begin_recording(now) {
                actions.push(action);
            }
        }
    }

    /// A game that started while another session was running.
    fn defer(&mut self, matched: MatchedLaunch, now: SystemTime) {
        let active = self
            .active
            .as_mut()
            .expect("a deferral is only decided against an active session");
        tracing::info!(
            session = active.session.id().as_str(),
            game = matched.game.slug(),
            pid = matched.pid,
            "a second game started while a session was already running; there is one encoder and \
             one capture target, so the session in progress keeps them"
        );
        active.session.record(
            now,
            SessionEventKind::AnotherGameStarted {
                game_id: matched.game.slug().to_owned(),
                pid: matched.pid,
            },
        );
        persist(self.settings.directory(), &active.session);

        self.deferred.retain(|waiting| waiting.pid != matched.pid);
        self.deferred.push(matched);
    }

    /// A process the watcher reported as gone.
    ///
    /// Every process this manager is tracking hears about the exit, not only
    /// the ones belonging to the session that happens to be open. A process
    /// exits once and is reported once, so a deferred game's helper that is not
    /// accounted for here can never be accounted for at all: it would be
    /// promoted with the game, hours later, as a live child that is already
    /// dead and can never be seen to die. The session it belonged to would then
    /// never reach its grace period, never end, and — because a session that
    /// never ends defers every game after it — the recorder would silently stop
    /// recording anything for the rest of its run.
    fn exited(&mut self, exit: &ProcessExit, now: SystemTime, actions: &mut Vec<SessionAction>) {
        let pid = exit.process.pid;
        self.deferred.retain(|waiting| waiting.pid != pid);
        for waiting in &mut self.deferred {
            waiting.child_pids.remove(&pid);
        }

        let mut stop = None;
        if let Some(active) = self.active.as_mut() {
            if active.subject.pid == pid && active.subject.alive {
                tracing::info!(
                    session = active.session.id().as_str(),
                    pid,
                    "the game this session is of has exited"
                );
                active.subject.alive = false;
                active.pending_start = None;
                active
                    .session
                    .record(now, SessionEventKind::GameExited { pid });
                persist(self.settings.directory(), &active.session);

                match active.recording {
                    Some(index) if !active.stopping => {
                        active.stopping = true;
                        stop = Some(RecordingId {
                            session: active.session.id().clone(),
                            index,
                        });
                    }
                    Some(_) => {}
                    None if active.live_children.is_empty() => active.idle_since = Some(now),
                    None => {}
                }
            } else if active.live_children.remove(&pid)
                && !active.subject.alive
                && active.recording.is_none()
                && active.live_children.is_empty()
                && active.idle_since.is_none()
            {
                active.idle_since = Some(now);
            }
        }

        if let Some(recording) = stop {
            actions.push(SessionAction::StopRecording {
                recording,
                cause: StopCause::GameExited,
            });
        }
    }

    /// Opens a session for a recognised launch and records it.
    fn start_session(
        &mut self,
        matched: MatchedLaunch,
        now: SystemTime,
        actions: &mut Vec<SessionAction>,
    ) {
        let mut session = Session::new(matched.game, now);
        session.record(
            now,
            SessionEventKind::Started {
                pid: matched.pid,
                image_name: matched.image_name.clone(),
            },
        );

        tracing::info!(
            session = session.id().as_str(),
            game = session.game().slug(),
            pid = matched.pid,
            image = matched.image_name.as_str(),
            "a game started, so a session started"
        );

        self.active = Some(Active {
            session,
            subject: Subject {
                pid: matched.pid,
                image_name: matched.image_name,
                alive: true,
            },
            child_names: matched.child_names,
            live_children: matched.child_pids,
            recording: None,
            stopping: false,
            pending_start: None,
            idle_since: None,
            started_recordings: 0,
            no_window: false,
            limit_reported: false,
        });

        if let Some(action) = self.begin_recording(now) {
            actions.push(action);
        }
    }

    /// Asks for the next recording of the open session.
    fn begin_recording(&mut self, now: SystemTime) -> Option<SessionAction> {
        let limit = self.settings.max_recordings_per_session();
        let active = self.active.as_mut()?;
        active.pending_start = None;

        if active.started_recordings >= limit {
            if !active.limit_reported {
                active.limit_reported = true;
                tracing::warn!(
                    session = active.session.id().as_str(),
                    limit,
                    "this session has started as many recordings as it is allowed; no more will \
                     be started for it"
                );
                active
                    .session
                    .record(now, SessionEventKind::RecordingLimitReached { limit });
                persist(self.settings.directory(), &active.session);
            }
            return None;
        }

        active.started_recordings += 1;
        let index = active.started_recordings;
        let output = active
            .session
            .recording_path(self.settings.directory(), index);
        active.session.begin_recording(index, output.clone(), now);
        active.recording = Some(index);
        persist(self.settings.directory(), &active.session);

        tracing::info!(
            session = active.session.id().as_str(),
            index,
            pid = active.subject.pid,
            "starting a recording for the session"
        );

        Some(SessionAction::StartRecording(RecordingRequest {
            recording: RecordingId {
                session: active.session.id().clone(),
                index,
            },
            game: active.session.game().clone(),
            process_id: active.subject.pid,
            image_name: active.subject.image_name.clone(),
            output,
        }))
    }

    /// Ends the open session and hands it over.
    fn close_active(
        &mut self,
        reason: SessionEndReason,
        now: SystemTime,
        actions: &mut Vec<SessionAction>,
    ) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        active.session.end(reason, now);
        persist(self.settings.directory(), &active.session);
        tracing::info!(
            session = active.session.id().as_str(),
            game = active.session.game().slug(),
            reason = reason.token(),
            recordings = active.session.recordings().len(),
            "the session ended"
        );
        actions.push(SessionAction::SessionEnded(Box::new(active.session)));

        if !self.stopping {
            // The most recently deferred game, because it is the one the user
            // most recently chose to launch. Anything that exited while it
            // waited has already been dropped.
            if let Some(next) = self.deferred.pop() {
                self.start_session(next, now, actions);
            }
        }
    }

    /// Which game, if any, a launch is.
    ///
    /// Newest process first: a launch chain cannot have a child start before
    /// its parent, so the last member to start is the best single guess at the
    /// thing that was launched (`LaunchGroup::newest`), and searching backwards
    /// from it finds the game rather than the launcher that started it.
    fn match_launch(&self, group: &LaunchGroup) -> Option<MatchedLaunch> {
        for process in group.processes.iter().rev() {
            let Some((game, child_names)) = self.identify(process) else {
                continue;
            };

            let child_pids = group
                .processes
                .iter()
                .filter(|member| member.pid != process.pid)
                .filter(|member| child_names.contains(&member.image_name.to_lowercase()))
                .map(|member| member.pid)
                .collect();

            return Some(MatchedLaunch {
                game,
                pid: process.pid,
                image_name: process.image_name.clone(),
                child_names,
                child_pids,
            });
        }
        None
    }

    /// What the catalogue makes of one process.
    fn identify(&self, process: &ProcessSnapshot) -> Option<(GameIdentity, BTreeSet<String>)> {
        let path = process
            .image_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());

        let mut candidate = ProcessCandidate::new(&process.image_name);
        if let Some(path) = path.as_deref() {
            candidate = candidate.with_path(path);
        }

        match self.catalogue.match_process(&candidate) {
            Match::None => None,
            Match::One { entry, .. } => Some((
                GameIdentity::Known {
                    game_id: entry.game_id().as_str().to_owned(),
                    name: entry.name().to_owned(),
                },
                lowered(entry.child_processes()),
            )),
            // The union of the candidates' children, because the question a
            // child answers here is only "is anything of this game still
            // running", and any of the tied entries could be the one.
            Match::Ambiguous { entries, .. } => Some((
                GameIdentity::Ambiguous {
                    candidates: entries
                        .iter()
                        .map(|entry| entry.game_id().as_str().to_owned())
                        .collect(),
                },
                entries
                    .iter()
                    .flat_map(|entry| lowered(entry.child_processes()))
                    .collect(),
            )),
        }
    }
}

/// Executable names as they are compared: lower-cased, as Windows compares
/// them.
fn lowered(names: &[String]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_lowercase()).collect()
}

/// Writes a session's sidecar, or says why it could not.
///
/// A failure here never fails a recording. The video is what cannot be made
/// again; a metadata write that failed is a warning and a session that is only
/// in memory until the next change (AGENTS.md section 17).
fn persist(directory: &Path, session: &Session) {
    if let Err(error) = sidecar::write(directory, session) {
        tracing::warn!(
            session = session.id().as_str(),
            %error,
            "the session's record could not be written; the recordings themselves are unaffected"
        );
    }
}

#[cfg(test)]
mod tests;
