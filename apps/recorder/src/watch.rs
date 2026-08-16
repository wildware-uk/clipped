//! The `watch` subcommand: record games as they start, without being asked.
//!
//! This is the mode the product is for (SPEC.md sections 2 and 7). It joins
//! three things that already existed and were not connected:
//! `clipped_game_detection::ProcessWatcher` says what started and stopped, its
//! `Catalogue` says whether that is a game, and `clipped_session::record`
//! records a window to a playable file. `clipped_session::automatic` holds the
//! policy between them; this module is the part that has to touch the machine.
//!
//! # The division of labour, and why it is here
//!
//! ```text
//!  this module                       clipped_session::automatic
//!  ───────────                       ──────────────────────────
//!  wait on the watcher      ────▶    observe / poll
//!  resolve a process to a   ◀────    StartRecording
//!    window, run the session
//!  raise a stop signal      ◀────    StopRecording
//!  report the outcome       ────▶    recording_finished
//!  print the summary        ◀────    SessionEnded
//! ```
//!
//! Everything on the right is a decision about timing and identity and is
//! tested without a machine. Everything on the left needs a desktop, and is
//! deliberately small enough to be read in one sitting.
//!
//! # Threads
//!
//! ```text
//!  the command's thread                     recording thread
//!  ────────────────────                     ────────────────
//!  wait on the process watcher              wait for the game's window
//!  drive the session manager  ──spawn──▶    capture, encode, mux
//!  collect a finished recording ◀──────     return what it turned out to be
//!  take what the plugins said   ◀──────     plugin thread
//!                                           poll the supervisor, drain events
//! ```
//!
//! One recording at a time, which the session manager already guarantees:
//! there is one encoder and one capture target. The recording runs on a thread
//! of its own because `clipped_session::record` blocks for the length of it,
//! and this thread has to keep waiting on the watcher — otherwise a game
//! exiting would not be noticed until the recording it should have ended had
//! ended by itself.
//!
//! The third thread is `clipped_session::plugins`, which starts the highlight
//! plugins that support the game being recorded and is deliberately neither of
//! the other two: the recording thread may not wait on a plugin (AGENTS.md
//! section 20) and neither may this one, which has a process watcher to answer.
//! What crosses back is what the plugins reported and what went wrong with them,
//! taken here once round the loop and put in front of the user.
//!
//! # Plugins, and which of them runs
//!
//! `run` reads the plugins directory at start-up and says what is installed and
//! what was refused. Which of those *start* comes from the settings file's
//! `plugins` section — the plugins the user enabled, and the consent token each
//! was enabled with (`clipped_session::config::plugins`,
//! [issue #282](https://github.com/wildware-uk/clipped/issues/282)). It is read
//! once, beside the plugins themselves.
//!
//! A plugin the file does not mention is off, and enabling one uninvited is not
//! an option: it would make `docs/privacy.md`'s register false, and all three
//! bundled plugins open a loopback socket. A plugin whose declaration no longer
//! matches the token beside it is refused and *reported* — the user agreed to
//! something else, and is the only one who can agree to the new thing.
//!
//! So each recording names the installed plugins that claim the game it is of
//! and did not start, with which of the three reasons it was, rather than
//! leaving a silence (`report_plugin_not_started`).
//!
//! Nothing writes that section yet
//! ([issue #281](https://github.com/wildware-uk/clipped/issues/281) is the
//! screen that would), so a build whose settings nobody has hand-edited starts
//! no plugin.
//!
//! # Stopping
//!
//! Ctrl+C, exactly as `record` and `serve`: the signal reaches the recording
//! loop between frames, `clipped_session` flushes the encoder and closes the
//! container, and the session is written out and reported before the process
//! exits. Nothing is killed.

use std::error::Error;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use clipped_game_detection::catalogue::{Catalogue, OverlayStatus};
use clipped_game_detection::launcher::Launchers;
use clipped_game_detection::{
    Next, ProcessWatcher, WatchConfig, WatchError as DetectionError, WatchEvent,
};
use clipped_logging::RedactedPath;
use clipped_plugins::{
    discover, InstalledPlugin, ObservedProcess, SessionDetails, SupervisionEvent, SupervisionPolicy,
};
use clipped_session::automatic::{
    AutomaticSettings, RecordingId, RecordingOutcome, RecordingOutcomeSummary, RecordingRequest,
    Session, SessionAction, SessionManager,
};
use clipped_session::config::{
    Configuration, ConfigurationError, ConfigurationStore, NotStarted, PluginConsents,
};
use clipped_session::plugins::{installed_but_not_enabled, PluginOutcome, SessionPlugins};
use clipped_session::{
    RecordingOutputs, RecordingProgress, RecordingReport, RecordingSettings, SessionError,
};
use clipped_windows::WindowInfo;

use crate::cli::{RecordArgs, WatchArgs};
use crate::config::{CaptureTarget, RecordingConfig};
use crate::record::{choose_window, settings_for, RecordError};
use crate::shutdown::{install_ctrl_c_handler, CtrlCError, ShutdownSignal};

/// How long the loop waits on the watcher before letting the clock move on.
///
/// The session manager's suspend rule is stated against a promise that it is
/// called at least once a second, and this is that promise. It costs nothing:
/// `ProcessWatcher::next_event` sleeps for the timeout rather than polling.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How often the recording thread looks for the game's window.
///
/// Twice a second, for at most [`WatchArgs::window_timeout`]. Enumerating the
/// desktop is not free, so this is deliberately not a tight loop — and it is
/// bounded at both ends, which is what keeps it from being the filesystem-style
/// polling AGENTS.md section 18 rules out.
const WINDOW_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long the loop pauses while it is waiting only for a recording to finish.
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Why `watch` did not watch, or did not finish.
#[derive(Debug)]
pub enum WatchCommandError {
    /// The directory recordings would go in cannot be used.
    OutputDirectory {
        /// The directory.
        directory: PathBuf,
        /// Why it cannot be used.
        source: std::io::Error,
    },
    /// There is no home directory, and none was named.
    NoOutputDirectory,
    /// The game catalogue could not be read.
    Catalogue(clipped_game_detection::catalogue::CatalogueError),
    /// The Ctrl+C handler could not be installed, so a recording could not be
    /// stopped cleanly.
    Shutdown(CtrlCError),
    /// Process detection could not be started at all.
    Detection(DetectionError),
    /// Process detection stopped while watching, so no further game can be
    /// noticed.
    DetectionStopped {
        /// What the watcher said went wrong.
        reason: String,
    },
}

impl fmt::Display for WatchCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputDirectory { directory, source } => write!(
                formatter,
                "recordings cannot be written to {}: {source}",
                directory.display()
            ),
            Self::NoOutputDirectory => formatter.write_str(
                "there is no home directory to put recordings in, so an output directory is \
                 required",
            ),
            Self::Catalogue(error) => write!(formatter, "{error}"),
            Self::Shutdown(error) => write!(formatter, "{error}"),
            Self::Detection(error) => write!(
                formatter,
                "games cannot be detected on this machine, so nothing could be recorded \
                 automatically: {error}"
            ),
            Self::DetectionStopped { reason } => write!(
                formatter,
                "game detection stopped and no source is left, so no further game would have \
                 been noticed: {reason}"
            ),
        }
    }
}

impl Error for WatchCommandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::OutputDirectory { source, .. } => Some(source),
            Self::Catalogue(error) => Some(error),
            Self::Shutdown(error) => Some(error),
            Self::Detection(error) => Some(error),
            Self::NoOutputDirectory | Self::DetectionStopped { .. } => None,
        }
    }
}

/// Watches for games and records them until Ctrl+C.
///
/// Standard output is left empty, as it is for `record`: what this command
/// produces is files, and everything it says goes to standard error and to the
/// log (`docs/recorder-cli.md`).
///
/// # Errors
///
/// [`WatchCommandError`], which names what could not be set up or what stopped
/// working. A session that was recording when something failed has still been
/// finalised and written out: the failure paths below all go through the same
/// shutdown as Ctrl+C.
pub fn run(args: &WatchArgs) -> Result<(), WatchCommandError> {
    // Read here as well as in `Driver::new`, which reads it for the per-game
    // settings it resolves. Two reads of a small file at start-up, rather than
    // handing the driver a `Configuration` - `Driver::new` takes the path on
    // purpose, so that the whole path from a file on disk to the settings a
    // recording starts with is one thing a test can exercise.
    let settings_file = ConfigurationStore::default_path();
    let configuration = load_configuration(settings_file.as_deref());
    let directory = output_directory(args, configuration.storage().recording_directory())?;
    let catalogue = load_catalogue()?;

    // A property of the process, set once, and before anything measures a
    // window (`crate::record`).
    crate::record::enable_dpi_awareness();

    let signal = ShutdownSignal::new();
    // The handler first, then Ctrl+C back on, for the reason `record`
    // documents: a recorder started in a process group of its own inherits
    // Ctrl+C disabled.
    install_ctrl_c_handler(&signal).map_err(WatchCommandError::Shutdown)?;
    crate::shutdown::allow_ctrl_c();

    let mut watcher =
        ProcessWatcher::start(WatchConfig::default()).map_err(WatchCommandError::Detection)?;

    // Two things on disk are read at start-up: the user's settings and the
    // plugins directory. Which file and which directory is all this function
    // decides — `default_path` and `installed_plugins` — because what is in
    // them, and what to do when one cannot be read, is `Driver`'s, and that is
    // what lets both be tested against what a test wrote rather than against
    // whatever is in this machine's user directory (`clipped_logging::
    // directories` separates the two the same way).
    let mut driver = Driver::new(
        catalogue,
        // The third thing read from disk at start-up: which shops are installed
        // and what each of them says it has. Once, here, for the reason the
        // other two are read once — and `Launchers` documents what a game
        // installed afterwards costs (issue #522).
        Launchers::discover(),
        AutomaticSettings::new(directory.clone()),
        ConfigurationStore::default_path().as_deref(),
        RecordingPlan::from(args),
        installed_plugins(),
    );

    announce(&directory, &watcher, &driver.manager);

    let stopped_by = driver.watch(&mut watcher, &signal);

    eprintln!("Automatic recording stopped.");
    match stopped_by {
        Some(reason) => Err(WatchCommandError::DetectionStopped { reason }),
        None => Ok(()),
    }
}

/// Where recordings and session records go.
fn output_directory(
    args: &WatchArgs,
    configured: Option<&Path>,
) -> Result<PathBuf, WatchCommandError> {
    // Three layers, top down: the flag, then the settings file, then the videos
    // folder this build would pick on its own. The middle one is step 3 of
    // SPEC.md section 45 - a directory chosen once in the settings screen has to
    // be the one an automatic recording lands in, since nobody is at a command
    // line when a game launches (issue #307).
    let directory = match (&args.output_directory, configured) {
        (Some(named), _) => named.clone(),
        (None, Some(chosen)) => chosen.to_path_buf(),
        (None, None) => {
            crate::config::default_output_directory().ok_or(WatchCommandError::NoOutputDirectory)?
        }
    };

    // Created here rather than on the first recording, because this command
    // runs for days before it writes anything and "the drive you named is not
    // there" is not a thing to find out at the moment a game launches
    // (AGENTS.md section 17).
    std::fs::create_dir_all(&directory).map_err(|source| WatchCommandError::OutputDirectory {
        directory: directory.clone(),
        source,
    })?;
    Ok(directory)
}

/// The catalogue, with whatever happened to the user's overlay reported.
///
/// `pub(crate)` for `serve`, which needs the same catalogue and the same report
/// of what happened to the user's file: a game they registered, renamed or
/// excluded has to mean the same thing to a recording they started from the
/// window as it does to one detection started (AGENTS.md section 55, issue
/// #403). What differs is what the two do with a failure, which is the caller's
/// decision and not this function's — see `serve::catalogue_for_recordings`.
pub(crate) fn load_catalogue() -> Result<Catalogue, WatchCommandError> {
    let loaded = Catalogue::load().map_err(WatchCommandError::Catalogue)?;

    match loaded.overlay() {
        OverlayStatus::NoUserDirectory | OverlayStatus::Absent { .. } => {}
        OverlayStatus::Loaded { path, entries } => {
            tracing::info!(
                overlay = %RedactedPath::new(path),
                entries,
                "the user's own game entries were loaded"
            );
        }
        // The user's file was rewritten and a copy kept, which they should be
        // told about rather than discover (AGENTS.md section 56).
        OverlayStatus::Migrated {
            path,
            from,
            to,
            backup,
            entries,
        } => {
            tracing::warn!(
                overlay = %RedactedPath::new(path),
                from,
                to,
                backup = %RedactedPath::new(backup),
                entries,
                "your games file was written for an older Clipped and has been converted"
            );
            eprintln!(
                "Your games file was converted from version {from} to {to}. The original was \
                 kept at {}.",
                backup.display()
            );
        }
        // `OverlayStatus` is `#[non_exhaustive]`. A state added later is one
        // this command has nothing specific to say about; the catalogue itself
        // has already refused anything it could not read.
        _ => {}
    }

    Ok(loaded.into_catalogue())
}

/// The user's settings from `path`, or the shipped defaults when there are
/// none to read.
///
/// `None` is a machine that describes no per-user directory at all, which
/// [`ConfigurationStore::default_path`] documents as a supported state.
///
/// A missing file is the ordinary case rather than a failure: somebody who has
/// never changed a setting has no settings file, and writing one on first run
/// would put a file on their disk for nothing. A file that exists but cannot be
/// read is reported and then ignored — a recorder that refuses to record
/// because a preference is malformed has chosen the wrong thing to protect
/// (AGENTS.md sections 16 and 45).
///
/// **Neither case is written back over.** Nothing here calls
/// [`ConfigurationStore::store`], which is what stops a build that cannot read
/// a newer settings file from replacing it with what this one understood
/// (AGENTS.md section 56; the same defect was found in #108 during review).
///
/// `pub` for one reason, and it is a test: `tests/unreadable_settings.rs` has
/// to be a binary of its own to observe the report this makes — installing a
/// second subscriber in a process makes `tracing` abandon its cached
/// per-callsite decisions, so a subscriber that shares a process with the rest
/// of this crate's tests sees nothing (`crates/logging/tests/frame_tracing.rs`
/// is split for the same reason). Nothing outside this crate should call it;
/// the whole library target is documented as not being a public API.
pub fn load_configuration(path: Option<&Path>) -> Configuration {
    let Some(path) = path else {
        return Configuration::defaults();
    };

    let mut store = ConfigurationStore::at(path);
    match store.load() {
        Ok(_) => store.current().clone(),
        Err(error) => {
            report_unreadable_settings(&error);
            Configuration::defaults()
        }
    }
}

/// Says, once, that a settings file could not be read.
///
/// One function rather than two statements at the call site, so that "it was
/// reported" is one thing to find and one thing to change.
///
/// The same sentence goes to both places. The log is where it is found months
/// later and the console is where somebody who started `watch` in a terminal
/// sees it now, and a diagnostic that only one of them carries is one half of
/// the users never see (`docs/logging.md`, AGENTS.md section 45).
///
/// Both halves are held by a test, and they have to be different tests because
/// they are observed in different places:
///
/// - the log, by `tests/unreadable_settings.rs`, which drives
///   [`load_configuration`] into a subscriber of its own;
/// - the console, by `command_line.rs`'s
///   `watch_says_on_the_console_that_a_settings_file_it_cannot_read_was_left_alone`,
///   which starts the built `watch` over an unreadable file and reads its
///   standard error. No subscriber can see an `eprintln!`, however well the
///   first test is written, so until the second existed the line below could be
///   deleted with every test on the branch still green.
fn report_unreadable_settings(error: &ConfigurationError) {
    let sentence = unreadable_settings_sentence(error);
    tracing::warn!(
        %error,
        report = sentence.as_str(),
        "the settings file could not be read, so this run uses the shipped defaults"
    );
    eprintln!("{sentence}");
}

/// What somebody is told when their settings file cannot be read.
///
/// Three things, because all three are what they need to know: that their
/// settings are not in force, that recording is happening anyway, and that
/// their file is still theirs — a user who reads "settings not applied" and
/// nothing else has no way to know whether the recorder has just overwritten
/// what it could not read (AGENTS.md sections 45 and 56).
fn unreadable_settings_sentence(error: &ConfigurationError) -> String {
    format!(
        "Settings not applied: {error}. Recording with Clipped's defaults; your settings file \
         has been left as it is."
    )
}

/// Says what is about to happen, and what will not.
///
/// The last part matters: a game that is already running will not be recorded,
/// and a user who is told nothing would reasonably conclude the recorder is
/// broken (AGENTS.md section 27).
fn announce(directory: &Path, watcher: &ProcessWatcher, manager: &SessionManager) {
    tracing::info!(
        directory = %RedactedPath::new(directory),
        source = watcher.source().as_str(),
        "watching for games"
    );
    eprintln!(
        "Watching for games. Recordings go to {}. Press Ctrl+C to stop.",
        directory.display()
    );

    report_interrupted_recordings(directory);

    if let Some(declined) = watcher.declined_source() {
        tracing::warn!(
            error = %declined,
            "the preferred process event source was not available; detection is slower and \
             coarser than usual"
        );
    }

    for game in manager.already_running_games(watcher.already_running()) {
        tracing::info!(
            game = game.slug(),
            "a game is already running; automatic recording starts with the next launch of it"
        );
        eprintln!(
            "{} is already running, so it is not being recorded. Automatic recording starts \
             when a game launches.",
            game.display_name()
        );
    }
}

/// Reads the plugins directory, and says what is there.
///
/// Nothing is skipped silently: a directory that is not a usable plugin is
/// reported with the reason it was refused, because a user who dropped one in
/// and cannot see it needs to be told that its manifest names an executable
/// which is not there (AGENTS.md section 15).
///
/// A machine with no plugins directory has no plugins, which is every machine
/// until somebody installs one, and is not worth a word.
fn installed_plugins() -> Vec<InstalledPlugin> {
    let Some(directory) = crate::config::plugins_directory() else {
        return Vec::new();
    };

    let discovery = discover(&directory);
    for rejected in &discovery.rejected {
        tracing::warn!(
            plugin = %RedactedPath::new(&rejected.directory),
            reason = %rejected.reason,
            "something under the plugins directory is not a usable plugin"
        );
        eprintln!("A plugin could not be read: {}", rejected.reason);
    }
    for plugin in &discovery.installed {
        tracing::info!(
            plugin = %plugin.id(),
            name = plugin.manifest().name(),
            "a plugin is installed"
        );
    }
    discovery.installed
}

/// Says whether a previous run left footage nobody has claimed.
///
/// This is the moment to ask. A recorder that was killed left a file that plays
/// and a session record that never says the recording ended
/// (`clipped_session::automatic::recovery`), and nothing else in the product
/// will mention it: the library indexes finished recordings and the file simply
/// sits there. Startup is also the only moment at which the question is
/// unambiguous — a recording that is running right now looks exactly the same
/// from the outside, and this runs before this process has started one.
///
/// It reports and does nothing else. Adopting or discarding is
/// `clipped-recorder recover`, deliberately, because one of the two deletes
/// footage (AGENTS.md section 56).
fn report_interrupted_recordings(directory: &Path) {
    let found = match clipped_session::automatic::recovery::interrupted_recordings(directory) {
        Ok(found) => found,
        // Never fatal. Watching for games is what this command is for, and a
        // directory listing that failed is not a reason to refuse to record.
        Err(error) => {
            tracing::warn!(
                directory = %RedactedPath::new(directory),
                %error,
                "the recordings directory could not be checked for interrupted recordings"
            );
            return;
        }
    };

    if found.is_empty() {
        return;
    }

    let with_footage = found
        .iter()
        .filter(|recording| recording.has_footage())
        .count();
    tracing::warn!(
        interrupted = found.len(),
        with_footage,
        "a previous run left recordings that were never closed off"
    );
    eprintln!(
        "{} recording{} from an earlier run {} never finished, and {} still {} footage. \
         Run `clipped-recorder recover` to keep or discard {}.",
        found.len(),
        if found.len() == 1 { "" } else { "s" },
        if found.len() == 1 { "was" } else { "were" },
        with_footage,
        if with_footage == 1 { "has" } else { "have" },
        if found.len() == 1 { "it" } else { "them" }
    );
}

/// The command's own state: the policy, what every recording is made with, and
/// the one recording that may be running.
#[derive(Debug)]
struct Driver {
    manager: SessionManager,
    plan: RecordingPlan,
    /// What was found under the plugins directory when this command started.
    ///
    /// Read once. A plugin appearing while a game is being recorded is not a
    /// plugin this run starts: discovery reads manifests off a disk somebody
    /// else is writing to, and doing it again every second would be the
    /// filesystem polling AGENTS.md section 18 rules out.
    installed_plugins: Vec<InstalledPlugin>,
    /// Which of those the user enabled, and what they agreed to.
    ///
    /// Read once at start-up, beside the plugins themselves and for the same
    /// reason.
    plugin_consents: PluginConsents,
    /// The zero every event of the open session is stamped against.
    ///
    /// Taken from the first recording that produces a frame and held until the
    /// session ends. A session that writes several files stamps all their
    /// events against this one origin, which is the only thing
    /// `clipped_library::events` can place against — it sorts a session's
    /// recordings on one axis and asks which contains a moment, and both
    /// operations are nonsense if every file has its own zero
    /// ([issue #488](https://github.com/wildware-uk/clipped/issues/488)).
    session_epoch: Option<Instant>,
    running: Option<Running>,
}

/// A recording in progress.
#[derive(Debug)]
struct Running {
    id: RecordingId,
    stop: ShutdownSignal,
    thread: JoinHandle<RecordingOutcome>,
    /// The plugins attached to this recording, on a thread of their own.
    ///
    /// Dropped when the recording is collected, which stops every one of them
    /// whether or not this loop asked politely first.
    plugins: SessionPlugins,
    /// This recording's account of its own timeline.
    ///
    /// Kept so that the driver can read the epoch back once the first frame has
    /// fixed it, and hold it as the *session's* zero for every later recording
    /// of the same session.
    progress: RecordingProgress,
}

impl Driver {
    /// A driver with nothing running, recording each game at whatever the
    /// user's settings say it should be recorded at.
    ///
    /// `settings_file` is where those settings are kept — read here rather than
    /// taken as a [`Configuration`] so that the whole path from a file on disk
    /// to the settings a recording is started with is one thing a test can
    /// exercise, including the two cases where there is nothing to read
    /// ([`load_configuration`]).
    ///
    /// `installed_plugins` is the other half of the same idea and is handed in
    /// rather than discovered here, because [`installed_plugins`] reads a real
    /// directory and reports what it refused: a test that builds a driver must
    /// not have its result depend on what is installed on the machine running
    /// it (AGENTS.md section 25).
    fn new(
        catalogue: Catalogue,
        launchers: Launchers,
        settings: AutomaticSettings,
        settings_file: Option<&Path>,
        plan: RecordingPlan,
        installed_plugins: Vec<InstalledPlugin>,
    ) -> Self {
        // Per-game settings reach a recording through the manager: it resolves
        // them when it asks for one, and `attempt` lays the answer over what
        // the command line asked for (issue #61).
        let configuration = load_configuration(settings_file);
        // Taken before the configuration moves into the manager. Which plugins
        // the user enabled is not a per-game setting the manager resolves, and
        // `attach_plugins` runs on a path that must not go looking for a
        // settings file.
        let plugin_consents = configuration.plugins().clone();
        let manager = SessionManager::new(catalogue, settings)
            .with_configuration(configuration)
            // Without this the launcher rung never fires and a game is
            // identified by its executable's name and path alone, which is what
            // detection was before the providers existed (issue #522).
            .with_launchers(launchers);
        Self {
            manager,
            plan,
            installed_plugins,
            plugin_consents,
            session_epoch: None,
            running: None,
        }
    }

    /// The loop. Returns the reason detection stopped, if that is why it ended.
    fn watch(&mut self, watcher: &mut ProcessWatcher, signal: &ShutdownSignal) -> Option<String> {
        let mut stopping = false;
        let mut detection_stopped: Option<String> = None;

        loop {
            // Before anything else, and every time round: what a plugin had to
            // say is only useful while the recording it belongs to is running,
            // and a `PluginTrouble` nobody reads is one that was logged and
            // forgotten (AGENTS.md section 45).
            self.report_plugin_activity();

            if let Some(finished) = self.collect_finished() {
                let actions =
                    self.manager
                        .recording_finished(&finished.0, finished.1, SystemTime::now());
                self.apply(actions);
            }

            if !stopping && (signal.is_requested() || detection_stopped.is_some()) {
                stopping = true;
                let actions = self.manager.shut_down(SystemTime::now());
                self.apply(actions);
            }

            if stopping {
                if self.running.is_none() {
                    return detection_stopped;
                }
                // Nothing new can arrive, so this waits on the recording alone
                // rather than on the watcher's timeout — which would add a
                // second to every shutdown for no benefit.
                thread::sleep(SHUTDOWN_POLL_INTERVAL);
                continue;
            }

            match watcher.next_event(POLL_INTERVAL) {
                Next::Event(event) => {
                    report_watcher_event(&event, &mut detection_stopped);
                    let actions = self.manager.observe(&event, SystemTime::now());
                    self.apply(actions);
                }
                Next::Idle => {
                    let actions = self.manager.poll(SystemTime::now());
                    self.apply(actions);
                }
                // The watcher has already delivered `WatchEvent::Stopped` with
                // the reason, so `detection_stopped` is set by the time this
                // arrives; this is the answer to every call after it.
                Next::Finished => {
                    detection_stopped
                        .get_or_insert_with(|| "the process watcher finished".to_owned());
                }
            }
        }
    }

    /// The outcome of the recording that has just finished, if one has.
    fn collect_finished(&mut self) -> Option<(RecordingId, RecordingOutcome)> {
        if !self
            .running
            .as_ref()
            .is_some_and(|running| running.thread.is_finished())
        {
            return None;
        }

        let running = self.running.take().expect("just checked");

        // The plugins first: the recording has ended, so what they were
        // attached to is gone, and stopping them is bounded by the supervision
        // policy however badly they behave.
        report_plugin_outcome(&running.id, running.plugins.finish());

        let outcome = running.thread.join().unwrap_or_else(|_| {
            // `spawn_recording` catches the panic itself, so this is a panic in
            // the catch or a thread that was killed; either way the file has
            // been finalised by `clipped_session` and the session must not be
            // left waiting for an outcome that will never come.
            RecordingOutcome::Failed {
                detail: "the recording thread ended without reporting an outcome".to_owned(),
            }
        });
        Some((running.id, outcome))
    }

    /// Takes the session's zero from the first recording that produces a frame,
    /// and lets it go when the session does.
    ///
    /// Reading it is one atomic load from a `OnceLock` the capture thread has
    /// already written; nothing waits, and nothing is asked of the recording.
    ///
    /// The clearing half is the one that matters. A zero held past the end of a
    /// session would stamp the next session's events against a previous
    /// session's first frame -- every event minutes or hours out, and every
    /// number still a plausible one, which is the failure mode
    /// [issue #488](https://github.com/wildware-uk/clipped/issues/488) is about.
    fn hold_the_sessions_epoch(&mut self) {
        if self.manager.active_session().is_none() {
            self.session_epoch = None;
            return;
        }

        if self.session_epoch.is_some() {
            return;
        }

        if let Some(running) = self.running.as_ref() {
            self.session_epoch = running.progress.timeline_epoch();
        }
    }

    /// Tells the session where the running recording starts on its timeline.
    ///
    /// The difference between the session's zero and this recording's, which is
    /// a number only the driver holds -- the session manager never sees an
    /// `Instant` and the recording never sees the session's. Nanoseconds,
    /// because that is what an `EventTime` is.
    ///
    /// Done once per recording, as soon as both epochs exist, rather than when
    /// the recording ends: a recording that fails still occupied a span, and a
    /// span nobody wrote down is one every event of that file is placed against
    /// nothing by (`clipped_library::events`).
    fn place_the_running_recording(&mut self) {
        let (Some(session_epoch), Some(running)) = (self.session_epoch, self.running.as_ref())
        else {
            return;
        };
        let Some(recording_epoch) = running.progress.timeline_epoch() else {
            return;
        };
        let id = running.id.clone();

        // Saturating, and signed for the same reason `EventTime` is: a
        // recording cannot start before its session, but a clock that is not
        // guaranteed monotonic across a suspend could say so, and a value
        // pinned at the end of the range is at least visibly wrong.
        let starts_at = recording_epoch
            .saturating_duration_since(session_epoch)
            .as_nanos();
        let starts_at = i64::try_from(starts_at).unwrap_or(i64::MAX);

        if self.manager.place_recording(&id, starts_at) {
            tracing::debug!(
                session = id.session.as_str(),
                index = id.index,
                starts_at_nanos = starts_at,
                "this recording's place on the session's timeline was recorded"
            );
        }
    }

    /// Puts what this recording's plugins have said in front of the user, and
    /// what they reported on the session.
    fn report_plugin_activity(&mut self) {
        self.hold_the_sessions_epoch();
        self.place_the_running_recording();

        let Some(running) = self.running.as_ref() else {
            return;
        };
        for report in running.plugins.take_reports() {
            report_plugin_event(&report);
        }

        let session = running.id.session.clone();
        let index = running.id.index;
        let events = running.plugins.take_events();
        if events.is_empty() {
            return;
        }

        // Onto the session, which writes them to its sidecar, which the library
        // indexer turns into rows (issue #71). Drained on this thread and handed
        // over here rather than delivered by the plugin thread, because the
        // session manager is not shared: one owner, one place it is mutated.
        let offered = events.len();
        let kept = self.manager.record_game_events(events);
        if kept == offered {
            tracing::debug!(
                session = session.as_str(),
                index,
                events = kept,
                "this recording's plugins reported events"
            );
            return;
        }

        // Said rather than swallowed. The only way to get here is a plugin
        // still draining after its session closed, and an event that is
        // silently discarded is indistinguishable from one that was never
        // reported (AGENTS.md section 54).
        tracing::warn!(
            session = session.as_str(),
            index,
            offered,
            kept,
            "a plugin reported events after the session closed, so they were dropped"
        );
    }

    /// Carries out what the session manager decided.
    fn apply(&mut self, actions: Vec<SessionAction>) {
        for action in actions {
            match action {
                SessionAction::StartRecording(request) => self.start(request),
                SessionAction::StopRecording { recording, cause } => match &self.running {
                    Some(running) if running.id == recording => {
                        tracing::info!(
                            session = recording.session.as_str(),
                            index = recording.index,
                            cause = cause.token(),
                            "stopping the recording"
                        );
                        running.stop.request();
                    }
                    // The recording it names has already finished and its
                    // outcome is on its way, which is the ordinary race between
                    // a window closing and the watcher noticing the process go.
                    _ => tracing::debug!(
                        session = recording.session.as_str(),
                        index = recording.index,
                        cause = cause.token(),
                        "a stop arrived for a recording that is no longer running"
                    ),
                },
                SessionAction::SessionEnded(session) => report_session(&session),
            }
        }
    }

    /// Starts a recording on a thread of its own.
    fn start(&mut self, request: RecordingRequest) {
        if self.running.is_some() {
            // The session manager runs one recording at a time; reaching here
            // would be a bug in it rather than something to paper over.
            tracing::error!(
                session = request.recording.session.as_str(),
                index = request.recording.index,
                "a recording was asked for while another was still running, and was not started"
            );
            return;
        }

        // Not "Recording …" — nothing is being recorded yet. A game reported as
        // launched has usually not drawn anything, and the search for its
        // window can take up to `--window-timeout` and can fail. The line that
        // says a recording started is printed by the recording thread once
        // there is a window to record (`record_process`), so the console never
        // claims a recording that never happened (AGENTS.md section 27).
        eprintln!(
            "{} started. Looking for its window.",
            request.game.display_name()
        );

        let stop = ShutdownSignal::new();
        let id = request.recording.clone();
        let plan = self.plan.clone();
        let signal = stop.clone();

        // The recording's own account of its timeline, which is the only thing
        // the capture thread gives a plugin: the instant its first frame fixed
        // the epoch. Everything else about a plugin happens on the thread
        // `SessionPlugins` starts (AGENTS.md section 20).
        let progress = RecordingProgress::new();
        let plugins = self.attach_plugins(&request, &progress);
        let recording_progress = progress.clone();

        let thread = thread::Builder::new()
            .name("clipped-automatic-recording".to_owned())
            .spawn(move || record_process(&request, &plan, &signal, &recording_progress))
            .expect("a thread can be started to record on");

        self.running = Some(Running {
            id,
            stop,
            thread,
            plugins,
            progress,
        });
    }

    /// Starts the plugins for the game this recording is of.
    ///
    /// Only the ones the user enabled, and only while what they agreed to still
    /// matches what the plugin declares. Everything not started is said, with
    /// which of the three reasons it was: a plugin that quietly does not run is
    /// worse than one that says why (AGENTS.md section 27).
    fn attach_plugins(
        &self,
        request: &RecordingRequest,
        progress: &RecordingProgress,
    ) -> SessionPlugins {
        let session = SessionDetails {
            session: request.recording.session.as_str().to_owned(),
            process: ObservedProcess::new(&request.image_name, request.process_id),
        };

        // Narrowed to this game first, so that nothing is said about a plugin
        // for a game nobody is playing.
        let supporting: Vec<InstalledPlugin> =
            installed_but_not_enabled(&self.installed_plugins, &session.process)
                .into_iter()
                .cloned()
                .collect();
        let (enabled, refused) = self.plugin_consents.enable_all(supporting);

        for problem in &refused {
            report_plugin_not_started(problem, request);
        }

        SessionPlugins::start(
            enabled,
            session,
            progress,
            self.session_epoch,
            SupervisionPolicy::default(),
        )
    }
}

/// Says why an installed plugin for this game did not start.
///
/// Three different things to tell somebody, so they are told apart. Only the
/// lapse interrupts: a plugin nobody has enabled, or one that is turned off, is
/// doing what the user asked, and a console line every time they launch a game
/// would be nagging. A lapse is the one they have to act on, because a plugin
/// they *did* enable has stopped running and will not start again until they
/// look at what changed.
fn report_plugin_not_started(problem: &NotStarted, request: &RecordingRequest) {
    match problem {
        NotStarted::NeverEnabled { plugin } => tracing::info!(
            plugin,
            game = request.game.slug(),
            "a plugin supports this game and has not been enabled, so it was not started"
        ),
        NotStarted::TurnedOff { plugin } => tracing::info!(
            plugin,
            game = request.game.slug(),
            "a plugin supports this game and is turned off, so it was not started"
        ),
        NotStarted::ConsentLapsed {
            plugin,
            agreed_to,
            now_declares,
        } => {
            tracing::warn!(
                plugin,
                game = request.game.slug(),
                agreed_to,
                now_declares,
                "a plugin asks for network access other than what was agreed to, so it was not                  started"
            );
            eprintln!(
                "{plugin} is enabled but now asks for different network access, so it is not                  running.
  you agreed to: {agreed_to}
  it now asks for: {now_declares}"
            );
        }
    }
}

/// Puts one thing the supervisor said in front of the user.
///
/// A plugin's trouble is reported rather than logged and forgotten, because an
/// integration that silently never works is worse than one that says why
/// (AGENTS.md section 45). Only the two that are worth interrupting somebody for
/// reach the console: a plugin saying something is wrong that they can act on,
/// and a plugin being given up on. A replacement is a log line, because the
/// recorder handling it is the point.
fn report_plugin_event(report: &SupervisionEvent) {
    match report {
        SupervisionEvent::Ready { plugin } => {
            tracing::info!(%plugin, "a plugin is running");
        }
        SupervisionEvent::Problem { plugin, message } => {
            tracing::warn!(%plugin, problem = %message, "a plugin reported a problem");
            eprintln!("{plugin}: {message}");
        }
        SupervisionEvent::Restarting {
            plugin,
            trouble,
            attempt,
            after,
        } => {
            tracing::warn!(
                %plugin,
                %trouble,
                attempt,
                after_seconds = after.as_secs_f32(),
                "a plugin is being started again"
            );
        }
        SupervisionEvent::Disabled { plugin, trouble } => {
            tracing::warn!(%plugin, %trouble, "a plugin was stopped for the rest of this recording");
            eprintln!("{plugin} was stopped for the rest of this recording: {trouble}");
            eprintln!("  The recording itself is unaffected.");
        }
    }
}

/// Says what a recording's plugins came to, once it has ended.
fn report_plugin_outcome(recording: &RecordingId, outcome: PluginOutcome) {
    for report in &outcome.reports {
        report_plugin_event(report);
    }
    if outcome.health.is_empty() {
        return;
    }

    tracing::info!(
        session = recording.session.as_str(),
        index = recording.index,
        plugins = outcome.health.len(),
        events = outcome.events.len(),
        dropped = outcome.inbox.dropped,
        "this recording's plugins have finished"
    );

    // A timeline missing marks has to say so rather than look complete
    // (AGENTS.md section 27).
    if outcome.lost_anything() {
        eprintln!(
            "Some events reported during this recording were lost, so its timeline is \
             incomplete."
        );
    }
}

/// Everything every automatic recording is made with.
///
/// The global settings, once. Per-game overrides are M7 (SPEC.md section 31)
/// and are deliberately not read here: a catalogue entry can carry
/// `default_settings` and nothing in this build interprets them.
#[derive(Debug, Clone)]
struct RecordingPlan {
    resolution: crate::options::Resolution,
    framerate: crate::options::Framerate,
    codec: crate::options::VideoCodec,
    encoder: crate::options::EncoderSelection,
    microphone: crate::options::AudioDeviceSelection,
    system_audio: crate::options::AudioDeviceSelection,
    window_timeout: Duration,
}

impl RecordingPlan {
    fn from(args: &WatchArgs) -> Self {
        Self {
            resolution: args.resolution,
            framerate: args.framerate,
            codec: args.codec,
            encoder: args.encoder,
            microphone: args.microphone.clone(),
            system_audio: args.system_audio.clone(),
            window_timeout: Duration::from_secs(u64::from(args.window_timeout)),
        }
    }

    /// The arguments `record` would have been given for this recording.
    ///
    /// Routed through [`RecordArgs`] and [`RecordingConfig::resolve`] rather
    /// than validated again here, for the reason `serve` gives for doing the
    /// same: every rule about what a recording may be is in `crate::options`
    /// and `crate::config`, and a second copy reachable only from this
    /// subcommand would be a second set of answers to the same question
    /// (AGENTS.md section 55).
    fn args_for(&self, process_id: u32, output: &Path) -> RecordArgs {
        RecordArgs {
            window: None,
            process: None,
            pid: Some(process_id),
            output: Some(output.to_path_buf()),
            // Never. A session's files are named after the moment it started,
            // so an existing one means something else is writing there — and a
            // recording cannot be made again (AGENTS.md section 56).
            overwrite: false,
            resolution: self.resolution,
            framerate: self.framerate,
            codec: self.codec,
            encoder: self.encoder,
            microphone: self.microphone.clone(),
            system_audio: self.system_audio.clone(),
        }
    }
}

/// Waits for the game's window, then records it.
///
/// Runs on the recording thread. The panic guard is there for the same reason
/// `serve`'s is: `clipped_session` finalises the file on every path out
/// including a panic, so a panic here does not cost the recording — but a
/// thread that died without reporting an outcome would leave the session
/// waiting for one for ever.
fn record_process(
    request: &RecordingRequest,
    plan: &RecordingPlan,
    stop: &ShutdownSignal,
    progress: &RecordingProgress,
) -> RecordingOutcome {
    let outcome = attempt(request, plan, stop, progress);

    // An attempt that produced nothing is said out loud. The summary printed
    // when the session ends counts files, so a recording that never happened
    // would otherwise be an absence the user has to notice rather than a
    // sentence they can read — and "why was my game not recorded" deserves an
    // answer at the moment it is known (AGENTS.md section 27).
    //
    // A *failure* is said by whichever function diagnosed it, because only
    // that function knows what to suggest doing about it: `failure` for the
    // ones that happen before a session is reached and `session_failure` for
    // the ones the pipeline reports (`crate::record::report_failure`).
    if let RecordingOutcome::NoWindow { detail } = &outcome {
        eprintln!(
            "Nothing was recorded of {}: {detail}",
            request.game.display_name()
        );
    }
    outcome
}

/// The attempt itself, with nothing printed.
fn attempt(
    request: &RecordingRequest,
    plan: &RecordingPlan,
    stop: &ShutdownSignal,
    progress: &RecordingProgress,
) -> RecordingOutcome {
    attempt_with(
        request,
        plan,
        stop,
        progress,
        wait_for_window,
        |settings, stop, outputs| clipped_session::record_into(settings, stop, outputs),
    )
}

/// The attempt, against a given way of finding the window and making the
/// recording.
///
/// The two arguments are the only parts of an attempt that need a desktop and a
/// GPU; everything between them — validating the plan, laying this game's
/// configured settings over it, and turning what came back into an outcome — is
/// this command's own logic, and taking them as arguments is what lets that
/// logic be tested on a machine that can capture nothing. `attempt` is this with
/// the real pair, and is the only caller outside the tests — the same shape
/// [`crate::shutdown::run_until_shutdown`] takes the recording itself as, and
/// which its own tests drive with a closure that records nothing.
fn attempt_with<FindWindow, Record>(
    request: &RecordingRequest,
    plan: &RecordingPlan,
    stop: &ShutdownSignal,
    progress: &RecordingProgress,
    find_window: FindWindow,
    record: Record,
) -> RecordingOutcome
where
    FindWindow: FnOnce(&CaptureTarget, Duration, &ShutdownSignal) -> Result<WindowInfo, String>,
    Record: FnOnce(
        &RecordingSettings,
        &ShutdownSignal,
        &RecordingOutputs<'_>,
    ) -> Result<RecordingReport, SessionError>,
{
    let game = request.game.display_name();
    let args = plan.args_for(request.process_id, &request.output);
    // `plan.args_for` always names an output file, so the settings file has
    // already been consulted where that directory was chosen (`output_directory`).
    let config = match RecordingConfig::resolve(&args, None) {
        Ok(config) => config,
        Err(error) => return failure(&error, game),
    };

    let window = match find_window(&config.target, plan.window_timeout, stop) {
        Ok(window) => window,
        Err(detail) => return RecordingOutcome::NoWindow { detail },
    };

    // Here, and not when the recording was asked for: this is the first moment
    // at which there is something to record.
    eprintln!(
        "Recording {} to {}.",
        request.game.display_name(),
        request.output.display()
    );

    // What the command line asked for, then what this game was configured for
    // laid over it. Resolved once, when the recording started; `request` has
    // been carrying the answer since then (issue #61).
    //
    // `apply_configured_to` and not `apply_to`: only settings a user configured
    // replace what this command was asked for on its command line. `apply_to`
    // would put the shipped default over every flag nobody has a settings file
    // for, which is `watch --framerate 144` recording at 60.
    let settings = request
        .settings
        .apply_configured_to(settings_for(&config, &window));
    // `record_into` rather than `record`, for the one output this command needs:
    // where the recording's timeline begins. That is what places a plugin's
    // event inside the file (`clipped_session::plugins`), and publishing it is
    // one `OnceLock` store on the first frame — nothing the capture thread can
    // wait on.
    let outputs = RecordingOutputs::default().with_progress(progress);
    match std::panic::catch_unwind(AssertUnwindSafe(|| record(&settings, stop, &outputs))) {
        Ok(Ok(report)) => RecordingOutcome::Recorded(Box::new(report)),
        Ok(Err(error)) => session_failure(&error, game, &config.output),
        Err(_) => failure(
            &"the recording thread panicked; the file was finalised before it did",
            game,
        ),
    }
}

/// A failure the recording pipeline diagnosed, put to the user.
///
/// The same treatment `record` gives the same failures
/// (`crate::record::report_failure`), because an automatic recording is where
/// they matter most: nobody is watching a terminal, so the one line that
/// reaches the console has to say what happened to the footage and what to do
/// (AGENTS.md section 45).
///
/// The string kept for the session's record carries both the headline and the
/// technical words: it is written into the sidecar and read months later
/// (`docs/sessions.md`).
fn session_failure(
    error: &clipped_session::SessionError,
    game: &str,
    output: &Path,
) -> RecordingOutcome {
    let failure = clipped_session::RecordingFailure::of(error, output);

    tracing::error!(
        failure = failure.kind().token(),
        output = %RedactedPath::new(output),
        detail = failure.detail(),
        "an automatic recording failed"
    );

    eprintln!("Recording {game} failed. {}", failure.headline());
    eprintln!("{}", failure.footage_sentence(output));
    for action in failure.actions() {
        eprintln!("  - {action}");
    }

    RecordingOutcome::Failed {
        detail: format!("{}: {}", failure.headline(), failure.detail()),
    }
}

/// A failure before a recording session was reached, as the session records it.
fn failure(error: &dyn fmt::Display, game: &str) -> RecordingOutcome {
    eprintln!("Recording {game} failed: {error}");
    RecordingOutcome::Failed {
        detail: error.to_string(),
    }
}

/// Looks for a capturable window belonging to the game, until there is one.
///
/// A game reported as launched has usually not drawn anything yet: the watcher
/// reports a launch a few seconds after the process starts, and a game can take
/// a minute to reach a window while it compiles shaders. Giving up at the first
/// look would mean recording almost nothing automatically.
///
/// Returns the desktop's own explanation when it gives up, so that "why was my
/// game not recorded" has an answer — including the case where the process has
/// several capturable windows and `clipped_windows::resolve` will not choose
/// between them.
fn wait_for_window(
    target: &CaptureTarget,
    timeout: Duration,
    stop: &ShutdownSignal,
) -> Result<WindowInfo, String> {
    wait_for_window_on(
        &mut || clipped_windows::enumerate_windows().map_err(RecordError::from),
        target,
        timeout,
        stop,
    )
}

/// The same, looking at whatever desktop `describe` reports each time.
///
/// Split from [`wait_for_window`] around the one syscall, for the same reason
/// [`crate::record::choose_window`] is split from `resolve_window`: what is left
/// is every rule about when to keep waiting and when to give up, and it can be
/// exercised against desktops a test constructed — including the one this
/// function exists for, a window that is minimised on one look and drawing on
/// the next (issue #383).
fn wait_for_window_on(
    describe: &mut dyn FnMut() -> Result<Vec<WindowInfo>, RecordError>,
    target: &CaptureTarget,
    timeout: Duration,
    stop: &ShutdownSignal,
) -> Result<WindowInfo, String> {
    let deadline = Instant::now() + timeout;
    let mut waited = false;

    loop {
        let refusal = match describe().and_then(|desktop| choose_window(&desktop, target)) {
            Ok(window) => return Ok(window),
            Err(RecordError::Resolution(error)) => error.to_string(),
            // Waited out for the same reason a window that has not appeared yet
            // is: a game that starts minimised, or one somebody minimised while
            // it was loading, is a window that is about to be recordable. Giving
            // up at the first look would refuse a recording that the next second
            // could have made, and carrying on regardless would record nothing
            // at all (issue #383). If it is still minimised when the timeout
            // runs out, this reason is what the console line says.
            Err(minimised @ RecordError::TargetMinimised { .. }) => minimised.to_string(),
            Err(other) => return Err(other.to_string()),
        };

        if stop.is_requested() {
            return Err(format!("stopped before a window appeared: {refusal}"));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no window to record appeared within {}s: {refusal}",
                timeout.as_secs()
            ));
        }
        if !waited {
            waited = true;
            tracing::debug!(target = %target, "waiting for the game to put a window on screen");
        }
        thread::sleep(WINDOW_POLL_INTERVAL);
    }
}

/// Puts what the watcher said about itself where somebody will see it.
fn report_watcher_event(event: &WatchEvent, detection_stopped: &mut Option<String>) {
    match event {
        WatchEvent::SourceChanged { from, to, reason } => {
            tracing::warn!(
                from = from.as_str(),
                to = to.as_str(),
                error = %reason,
                "game detection changed to a different event source"
            );
            eprintln!(
                "Game detection is now using {} rather than {}, which is slower to notice a \
                 launch.",
                to.as_str(),
                from.as_str()
            );
        }
        WatchEvent::Stopped(reason) => {
            tracing::error!(error = %reason, "game detection has stopped");
            eprintln!("Game detection has stopped: {reason}");
            *detection_stopped = Some(reason.to_string());
        }
        WatchEvent::Launched(_) | WatchEvent::Exited(_) => {}
        // `WatchEvent` is `#[non_exhaustive]`; a kind added later is not
        // something this loop can act on, and the watcher logs it itself.
        _ => {}
    }
}

/// Says what a finished session came to.
fn report_session(session: &Session) {
    let files: Vec<&clipped_session::automatic::SessionRecording> = session
        .recordings()
        .iter()
        .filter(|recording| {
            recording
                .outcome()
                .is_some_and(RecordingOutcomeSummary::produced_a_file)
        })
        .collect();
    let seconds: f64 = files
        .iter()
        .filter_map(|recording| match recording.outcome() {
            Some(RecordingOutcomeSummary::Recorded { duration, .. }) => {
                Some(duration.as_secs_f64())
            }
            _ => None,
        })
        .sum();

    eprintln!(
        "Session {} of {}: {} recording{} totalling {seconds:.0}s.",
        session.id(),
        session.game().display_name(),
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    for recording in files {
        eprintln!("  {}", recording.output().display());
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use clipped_game_detection::catalogue::EntrySource;
    use clipped_game_detection::{LaunchGroup, LaunchId, ProcessSnapshot};
    use clipped_session::config::{
        AudioDeviceSetting, GameKey, Preferences, ResolutionSetting, SettingSource,
    };
    use clipped_session::{
        AudioSourceSetting, CodecPreference, EncoderPreference, UnavailableChoice,
    };

    use super::*;

    /// One game, which is all any test here needs to launch.
    const GAMES: &str = r#"
schema_version = 1

[[game]]
game_id = "test-game"
name = "Test Game"
[[game.executables]]
name = "test-game.exe"
"#;

    /// A directory of this test's own, removed when it is dropped.
    ///
    /// The workspace has no `tempfile` dependency and this is not enough reason
    /// to add one; `crate::config` and `clipped_session::automatic`'s tests
    /// build the same thing from `std::env::temp_dir` (AGENTS.md sections 10
    /// and 55).
    #[derive(Debug)]
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "clipped-watch-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("recordings")).expect("the directory can be created");
            fs::create_dir_all(path.join("settings")).expect("the directory can be created");
            Self(path)
        }

        /// Where recordings and session records go.
        fn recordings(&self) -> PathBuf {
            self.0.join("recordings")
        }

        /// The settings file this run would read, whether or not it exists.
        ///
        /// In a directory of its own, as it is on a real machine: settings live
        /// under `%LOCALAPPDATA%\Clipped` and recordings do not
        /// ([`ConfigurationStore::default_path`]).
        fn settings_file(&self) -> PathBuf {
            self.0
                .join("settings")
                .join(clipped_session::config::FILE_NAME)
        }

        /// Everything in the settings directory, sorted, so that a test can say
        /// what was written as well as what was not.
        fn settings_directory_entries(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.0.join("settings"))
                .expect("the directory can be listed")
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A command line that asks for something no default would produce, so that
    /// a settings file that quietly replaced it is visible in an assertion.
    fn args() -> WatchArgs {
        WatchArgs {
            output_directory: None,
            window_timeout: 30,
            resolution: crate::options::Resolution::Fixed {
                width: 1920,
                height: 1080,
            },
            framerate: "144".parse().expect("a valid framerate"),
            codec: crate::options::VideoCodec::Av1,
            encoder: crate::options::EncoderSelection::Nvenc,
            microphone: crate::options::AudioDeviceSelection::Disabled,
            system_audio: crate::options::AudioDeviceSelection::Named("Speakers".to_owned()),
        }
    }

    /// A launch of one process.
    ///
    /// [`LaunchId::ALREADY_RUNNING`] is the only identifier constructible
    /// outside `clipped_game_detection`, and nothing in the policy reads it —
    /// a launch is identified by the processes in it.
    fn launch(pid: u32, image_name: &str) -> WatchEvent {
        WatchEvent::Launched(LaunchGroup {
            id: LaunchId::ALREADY_RUNNING,
            processes: vec![ProcessSnapshot::new(pid, 4, None, image_name)],
        })
    }

    /// The recording a launch of `test-game.exe` asks for, with the settings
    /// resolved from `settings_file` — the whole of what `run` sets up, from
    /// the file on disk to the request handed to the recording thread.
    fn recording_asked_for(directory: &TestDirectory, settings_file: &Path) -> RecordingRequest {
        let catalogue =
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue");
        let mut driver = Driver::new(
            catalogue,
            // And no launchers, for the same reason as the plugins below.
            Launchers::none(),
            AutomaticSettings::new(directory.recordings()),
            Some(settings_file),
            RecordingPlan::from(&args()),
            // No plugins: what a settings file does to a recording is what
            // these tests are about, and reading this machine's plugins
            // directory would make them depend on it (AGENTS.md section 25).
            Vec::new(),
        );

        driver
            .manager
            .observe(&launch(4242, "test-game.exe"), SystemTime::UNIX_EPOCH)
            .into_iter()
            .find_map(|action| match action {
                SessionAction::StartRecording(request) => Some(request),
                _ => None,
            })
            .expect("a launch of a game in the catalogue asks for a recording")
    }

    /// A window that is already there, so that an attempt reaches the point of
    /// starting a recording without a desktop.
    fn window() -> WindowInfo {
        WindowInfo::new(
            clipped_windows::WindowHandle::from_raw(0x0001_04ac),
            "Test Game".to_owned(),
            4242,
            Some("test-game.exe".to_owned()),
            clipped_windows::WindowGeometry::new(
                clipped_windows::PixelSize::new(2560, 1440),
                96,
                clipped_windows::MonitorHandle::from_raw(1),
            ),
            false,
            None,
        )
    }

    /// What the recording engine was actually asked to record.
    ///
    /// Runs the real attempt — the plan, the window and this game's configured
    /// settings, composed by the code `run` reaches — against a window that is
    /// already there and an engine that records nothing and reports a failure.
    fn settings_the_recording_was_started_with(request: &RecordingRequest) -> RecordingSettings {
        let mut started: Option<RecordingSettings> = None;
        let outcome = attempt_with(
            request,
            &RecordingPlan::from(&args()),
            &ShutdownSignal::new(),
            &RecordingProgress::new(),
            |_, _, _| Ok(window()),
            |settings, _, _| {
                started = Some(settings.clone());
                Err(SessionError::TargetHasNoPixels)
            },
        );

        assert!(
            matches!(outcome, RecordingOutcome::Failed { .. }),
            "the stand-in engine reports a failure: {outcome:?}"
        );
        started.expect("the recording engine was asked to record something")
    }

    #[test]
    fn a_games_own_settings_reach_the_recording_that_is_started_for_it() {
        // The point of #61, end to end and in one test: a settings file on
        // disk, the manager that resolves it for the game that launched, and
        // the settings the recording engine is handed. Every link between them
        // is one this PR added, and dropping any of them leaves the command
        // line's 1080p144 in the assertions below.
        let directory = TestDirectory::new("configured");
        let mut configuration = Configuration::defaults();
        let mut preferences = Preferences::none();
        preferences
            .set_resolution(Some(ResolutionSetting::Fixed {
                width: 2560,
                height: 1440,
            }))
            .expect("1440p is in range");
        preferences.set_framerate(Some(60)).expect("60 is in range");
        preferences
            .set_microphone(Some(AudioDeviceSetting::Named("Yeti".to_owned())))
            .expect("a device name in range");
        configuration.set_game(
            GameKey::parse("test-game").expect("the fixture's identifier is a key"),
            preferences,
        );
        ConfigurationStore::at(directory.settings_file())
            .store(configuration)
            .expect("the settings file can be written");

        let request = recording_asked_for(&directory, &directory.settings_file());
        assert_eq!(
            request.settings.framerate().source(),
            SettingSource::Game,
            "the request must carry what was resolved for this game"
        );

        let settings = settings_the_recording_was_started_with(&request);
        assert_eq!(
            settings.resolution(),
            ResolutionSetting::Fixed {
                width: 2560,
                height: 1440
            },
            "the game's configured resolution must reach the recording"
        );
        assert_eq!(
            settings.framerate(),
            60,
            "the game's configured frame rate must reach the recording"
        );
        assert_eq!(
            settings.microphone(),
            &AudioSourceSetting::Named("Yeti".to_owned()),
            "the game's configured microphone must reach the recording"
        );
        assert_eq!(
            settings.unavailable_choice(),
            UnavailableChoice::Substitute,
            "a configured resolution this machine cannot produce substitutes rather than losing \
             the recording"
        );

        // And what the file says nothing about is still what the command line
        // asked for, rather than what Clipped ships with.
        assert_eq!(
            settings.codec(),
            CodecPreference::Fixed(clipped_encoder::Codec::Av1)
        );
        assert_eq!(
            settings.encoder(),
            EncoderPreference::Fixed(clipped_encoder::EncoderKind::Nvenc)
        );
        assert_eq!(
            settings.system_audio(),
            &AudioSourceSetting::Named("Speakers".to_owned())
        );
    }

    #[test]
    fn no_settings_file_records_at_what_was_asked_for_and_writes_nothing() {
        // The ordinary case: somebody who has never changed a setting. The
        // recording happens at the command line's settings, and no file is
        // invented on their disk to say so.
        let directory = TestDirectory::new("absent");
        let settings_file = directory.settings_file();

        let request = recording_asked_for(&directory, &settings_file);
        let settings = settings_the_recording_was_started_with(&request);

        assert_eq!(settings.framerate(), 144);
        assert_eq!(
            settings.resolution(),
            ResolutionSetting::Fixed {
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(
            settings.microphone(),
            &AudioSourceSetting::Off,
            "`--microphone none` must not be turned back on by a settings file that is not there"
        );
        assert_eq!(
            settings.unavailable_choice(),
            UnavailableChoice::Refuse,
            "nothing was configured, so nothing substitutes for what the command line named"
        );

        assert!(
            !settings_file.exists(),
            "a first run must not write a settings file: {}",
            settings_file.display()
        );
        assert!(
            directory.settings_directory_entries().is_empty(),
            "nothing may be written where the settings live: {:?}",
            directory.settings_directory_entries()
        );
    }

    #[test]
    fn a_settings_file_that_cannot_be_read_is_ignored_and_never_written_over() {
        // AGENTS.md section 56, and the data-loss defect found in #108: a file
        // this build cannot read is one whose contents are not known to be
        // worthless — it may have been written by a newer Clipped. It is
        // reported, the shipped defaults stand, the recording still happens,
        // and the file is left exactly as it was found.
        let directory = TestDirectory::new("unreadable");
        let settings_file = directory.settings_file();
        let as_found = "{ \"schema_version\": 99, \"this build\": cannot read this";
        fs::write(&settings_file, as_found).expect("the file can be written");

        let request = recording_asked_for(&directory, &settings_file);
        let settings = settings_the_recording_was_started_with(&request);

        assert_eq!(
            settings.framerate(),
            144,
            "an unreadable settings file must leave the recording as it was asked for rather \
             than stop it"
        );
        assert_eq!(settings.microphone(), &AudioSourceSetting::Off);

        assert_eq!(
            fs::read_to_string(&settings_file).expect("the file is still there"),
            as_found,
            "a settings file this build cannot read must never be replaced by what it understood"
        );
        assert_eq!(
            directory.settings_directory_entries(),
            vec![clipped_session::config::FILE_NAME.to_owned()],
            "no temporary file, no backup and no rewrite: the directory is as it was found"
        );
    }

    #[test]
    fn the_sentence_about_an_unreadable_settings_file_says_it_was_left_alone() {
        // The wording, not the wiring. Somebody who is told only "settings not
        // applied" has no way to know whether the recorder has just overwritten
        // a file it could not read.
        let error = ConfigurationError::Read {
            path: PathBuf::from(r"D:\settings.json"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        let sentence = unreadable_settings_sentence(&error);

        assert!(sentence.contains("settings.json"), "{sentence}");
        assert!(sentence.contains("defaults"), "{sentence}");
        assert!(
            sentence.contains("left as it is"),
            "the sentence must say the file was not written over: {sentence}"
        );
    }

    #[test]
    fn the_loop_promises_the_session_manager_what_its_suspend_rule_assumes() {
        // The manager reads a wall-clock jump as a suspend, and the size it
        // reads as one is chosen against this interval. A loop that waited
        // longer than the manager's threshold would have every quiet stretch
        // look like a resume.
        assert!(POLL_INTERVAL <= Duration::from_secs(1));
        assert!(clipped_session::automatic::DEFAULT_SUSPEND_GAP > POLL_INTERVAL * 10);
    }

    #[test]
    fn every_recording_setting_reaches_the_recording_it_is_made_with() {
        // A setting that parses and is then dropped is a control that silently
        // does nothing (AGENTS.md section 27), and this mapping is the only
        // place one could be lost between `watch` and the session.
        let args = WatchArgs {
            output_directory: None,
            window_timeout: 30,
            resolution: crate::options::Resolution::Fixed {
                width: 1920,
                height: 1080,
            },
            framerate: "144".parse().expect("a valid framerate"),
            codec: crate::options::VideoCodec::Av1,
            encoder: crate::options::EncoderSelection::Nvenc,
            microphone: crate::options::AudioDeviceSelection::Disabled,
            system_audio: crate::options::AudioDeviceSelection::Named("Speakers".to_owned()),
        };

        let plan = RecordingPlan::from(&args);
        assert_eq!(plan.window_timeout, Duration::from_secs(30));

        let record = plan.args_for(4242, Path::new(r"D:\clips\session.mkv"));
        assert_eq!(record.pid, Some(4242));
        assert_eq!(
            record.output.as_deref(),
            Some(Path::new(r"D:\clips\session.mkv"))
        );
        assert!(
            !record.overwrite,
            "an automatic recording must never replace a file it found"
        );
        assert_eq!(record.resolution, args.resolution);
        assert_eq!(record.framerate, args.framerate);
        assert_eq!(record.codec, args.codec);
        assert_eq!(record.encoder, args.encoder);
        assert_eq!(record.microphone, args.microphone);
        assert_eq!(record.system_audio, args.system_audio);
    }

    /// The game's window, drawing or minimised.
    ///
    /// The size is the same either way: Windows keeps answering for a minimised
    /// window's geometry, so nothing downstream can tell the two apart by shape
    /// — the flag is the whole of the difference (issue #383).
    fn game_window(minimised: bool) -> WindowInfo {
        WindowInfo::new(
            clipped_windows::WindowHandle::from_raw(0x0001_04ac),
            "Counter-Strike 2".to_owned(),
            4242,
            Some("cs2.exe".to_owned()),
            clipped_windows::WindowGeometry::new(
                clipped_windows::PixelSize::new(2560, 1440),
                96,
                clipped_windows::MonitorHandle::from_raw(1),
            ),
            minimised,
            None,
        )
    }

    #[test]
    fn a_game_that_starts_minimised_is_waited_for_rather_than_given_up_on() {
        // The case `watch` exists for and the one nobody is watching a console
        // for: a game launched to the taskbar, or minimised while it compiles
        // shaders. Giving up at the first look would silently skip the session,
        // and the automatic recorder would have nothing to say about why.
        let looks = std::cell::Cell::new(0u32);
        let mut desktop = || {
            looks.set(looks.get() + 1);
            Ok(vec![game_window(looks.get() == 1)])
        };

        let window = wait_for_window_on(
            &mut desktop,
            &CaptureTarget::ProcessName("cs2.exe".to_owned()),
            Duration::from_secs(5),
            &ShutdownSignal::new(),
        )
        .expect("a window that was restored on the second look is a window to record");

        assert_eq!(window.title(), "Counter-Strike 2");
        assert!(
            !window.is_minimised(),
            "the window handed to the recording is the restored one"
        );
        assert_eq!(
            looks.get(),
            2,
            "the first look refused it and the wait should have taken a second"
        );
    }

    #[test]
    fn a_game_still_minimised_when_the_wait_runs_out_is_given_up_on_with_that_as_the_reason() {
        // The other end of the same rule. Waiting is not waiting for ever, and
        // the reason the console prints has to be the real one: "no window
        // appeared" for a window that is there and minimised would send
        // somebody looking for a game-detection fault.
        let mut desktop = || Ok(vec![game_window(true)]);

        let refusal = wait_for_window_on(
            &mut desktop,
            &CaptureTarget::ProcessName("cs2.exe".to_owned()),
            Duration::ZERO,
            &ShutdownSignal::new(),
        )
        .expect_err("a window that is still minimised is not one to record");

        assert!(
            refusal.contains("minimised") && refusal.contains("Counter-Strike 2"),
            "the reason has to name the window and say what is wrong with it: {refusal}"
        );
    }

    #[test]
    fn a_target_that_names_no_window_at_all_is_also_waited_for() {
        // The wait this function was written for in the first place, kept here
        // so that the minimised case cannot be made to pass by breaking it: a
        // game that has not drawn yet reaches the same loop by a different
        // error, and it is the ordinary one.
        let looks = std::cell::Cell::new(0u32);
        let mut desktop = || {
            looks.set(looks.get() + 1);
            Ok(if looks.get() == 1 {
                Vec::new()
            } else {
                vec![game_window(false)]
            })
        };

        wait_for_window_on(
            &mut desktop,
            &CaptureTarget::ProcessName("cs2.exe".to_owned()),
            Duration::from_secs(5),
            &ShutdownSignal::new(),
        )
        .expect("a window that appeared on the second look is a window to record");

        assert_eq!(looks.get(), 2);
    }
}
