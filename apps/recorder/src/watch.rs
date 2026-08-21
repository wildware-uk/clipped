//! Recording games as they start, without being asked.
//!
//! This is the mode the product is for (SPEC.md sections 2 and 7), and it runs
//! in two places. `clipped-recorder watch` is the terminal-facing command;
//! [`AutomaticRecorder`] is the same loop on a thread of `serve`'s, which is
//! what a shipped build runs. They share every line below — the difference is
//! that a recording made under `serve` is handed to the recording state the
//! protocol answers against, so a bookmark, a screenshot and a stop reach it
//! ([issue #421](https://github.com/wildware-uk/clipped/issues/421),
//! `docs/sessions.md`). A recording made by `watch` is reachable by nothing,
//! because that command serves no protocol and registers no hotkey.
//!
//! It joins three things that already existed and were not connected:
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    rfc3339, AutomaticSettings, GameIdentity, RecordingId, RecordingOutcome,
    RecordingOutcomeSummary, RecordingRequest, Session, SessionAction, SessionManager,
    SessionRecording,
};
use clipped_session::config::{
    Configuration, ConfigurationError, ConfigurationStore, NotStarted, PluginConsents,
};
use clipped_session::plugins::{installed_but_not_enabled, PluginOutcome, SessionPlugins};
use clipped_session::{
    RecordingOutputs, RecordingProgress, RecordingReport, RecordingSettings, SessionError,
};
use clipped_windows::WindowInfo;

use crate::cli::{RecordArgs, WatchArgs, DEFAULT_WINDOW_TIMEOUT_SECONDS};
use crate::config::{CaptureTarget, RecordingConfig};
use crate::record::{choose_window, settings_for, RecordError};
use crate::serve::{Adopted, RecorderService, RecordingState, WatchingForGames};
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
    // Read before anything else, because where recordings go is one of the
    // things it says: a directory somebody picked in the settings screen is
    // where this run writes, without `--output-directory` and without a flag
    // they would have to remember (issue #307).
    //
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
        configuration,
        RecordingPlan::from(args),
        installed_plugins(),
        // Nothing. This command serves no protocol and registers no hotkey, so
        // there is nothing for a recording of it to be reachable *from*: a
        // bookmark, a screenshot and a stop all arrive as commands, and no
        // command can arrive here. That is the whole of issue #421, and the
        // answer to it is `serve --watch-for-games` rather than a control
        // endpoint of this command's own (`docs/sessions.md`).
        None,
    );

    announce(&directory, &watcher, &driver.manager);

    let stopped_by = driver.watch(&mut watcher, &signal);

    eprintln!("Automatic recording stopped.");
    match stopped_by {
        Some(reason) => Err(WatchCommandError::DetectionStopped { reason }),
        None => Ok(()),
    }
}

/// The same watching, inside a process that is doing something else.
///
/// `serve --watch-for-games` is the shape a shipped build runs: the desktop
/// supervisor starts it, `start-at-login` writes it into the `Run` key, and it
/// serves the control protocol and owns the global hotkeys. Before issue #421 it
/// did not watch for games, and `watch` — which did — served no protocol and
/// registered no hotkey, so the recordings a user is most likely to want to
/// bookmark were the ones nothing could bookmark.
///
/// Joining the two here rather than giving `watch` a control endpoint of its own
/// is what keeps [ADR 0009](../../../docs/adr/0009-the-recorder-registers-global-hotkeys.md)
/// true: exactly one process registers the combinations, and it is the one whose
/// endpoint already decided that it is the only recorder in the session. Two
/// recorders both wanting the keys is the arrangement that ADR rules out.
///
/// # Threads
///
/// One, of its own, running exactly the loop `watch` runs on its main thread.
/// The process watcher is started on it rather than handed to it, because a
/// failure to start detection must not stop a recorder that still has a window
/// to serve — `watch` has nothing left to do without detection and says so by
/// failing; here it is reported and the protocol carries on.
#[derive(Debug)]
pub(crate) struct AutomaticRecorder {
    stop: ShutdownSignal,
    /// [`None`] when there was nothing to start — a recordings directory that
    /// could not be made, which is reported rather than fatal.
    thread: Option<JoinHandle<()>>,
}

impl AutomaticRecorder {
    /// Starts watching for games on a thread of its own.
    ///
    /// Never fails, for the reason `crate::hotkeys::start` never fails: a
    /// recorder that refused to serve the desktop application because it could
    /// not create a folder would be a far worse thing to ship than one that
    /// records nothing automatically and says why.
    pub(crate) fn start(service: &Arc<RecorderService>) -> Self {
        let stop = ShutdownSignal::new();
        let recordings = Arc::clone(service.recordings());
        let catalogue = recordings.catalogue().clone();

        let directory = match recordings_directory(
            None,
            service.configuration().storage().recording_directory(),
        ) {
            Ok(directory) => directory,
            Err(error) => {
                tracing::error!(
                    %error,
                    "games will not be recorded automatically, because the folder they would be \
                     written to could not be used"
                );
                eprintln!("Games will not be recorded automatically: {error}");
                return Self { stop, thread: None };
            }
        };

        let plugins = installed_plugins();
        let signal = stop.clone();
        // Claimed here rather than on the thread below, and that is what makes
        // `serve --watch-for-games` answer `watching` to a window that connects
        // the instant it sees the ready line: this call happens before that line
        // is printed, and the thread's first pass may not (issue #584). What is
        // being claimed is settled by now — there is somewhere to record to and
        // a thread about to watch — and the guard is handed to that thread, so
        // detection failing to start takes the claim down with it.
        let watching = recordings.watch_for_games();
        let thread = thread::Builder::new()
            .name("clipped-automatic-recorder".to_owned())
            .spawn(move || {
                watch_for_games(
                    &directory,
                    catalogue,
                    plugins,
                    &recordings,
                    &signal,
                    watching,
                );
            })
            .expect("a thread can be started to watch for games on");

        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// Stops watching, and waits for the recording it is making to be finished.
    ///
    /// The wait is the point: the session's last file is being finalised and its
    /// record written, and a recorder that exited without waiting would leave a
    /// recording somebody was making of a game they are still playing
    /// (AGENTS.md section 17).
    pub(crate) fn stop(mut self) {
        self.stop.request();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AutomaticRecorder {
    /// The same wait, for a `serve` that is unwinding out of a panic. A watcher
    /// thread left running would go on recording into a process that is gone.
    fn drop(&mut self) {
        self.stop.request();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The loop, on the automatic recorder's own thread.
///
/// `watching` is this recorder's claim to be watching for games, and it is taken
/// by value because the loop ending is the end of the claim: detection that
/// could not be started, detection that stopped, and a shutdown all drop it
/// here, and the recorder answers `idle` again — which is the truth, because
/// none of the three will record the next game to launch (issue #584).
fn watch_for_games(
    directory: &Path,
    catalogue: Catalogue,
    plugins: Vec<InstalledPlugin>,
    recordings: &Arc<RecordingState>,
    signal: &ShutdownSignal,
    watching: WatchingForGames,
) {
    let mut watcher = match ProcessWatcher::start(WatchConfig::default()) {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::error!(
                %error,
                "games will not be recorded automatically, because this machine could not be \
                 watched for them"
            );
            eprintln!("Games will not be recorded automatically: {error}");
            return;
        }
    };

    let mut driver = Driver::new(
        catalogue,
        // Discovered here rather than taken from the service, which has a set
        // of its own: `Launchers` is not `Clone` and the session manager owns
        // what it is given. It is a registry walk and six directory reads,
        // once, on a thread nothing is waiting on (issue #522).
        Launchers::discover(),
        AutomaticSettings::new(directory.to_path_buf()),
        // The settings the service holds, not a second read of the file — and
        // only the value this driver *starts* with.
        //
        // Taking them from the one state this process keeps them in is
        // necessary and was never sufficient. The manager owns the
        // configuration it resolves per-game settings from, so what is passed
        // here is a copy, and a copy goes stale the moment the Settings screen
        // saves. Until issue #51 nothing replaced it: a microphone chosen in
        // the window reached automatic recordings only after the recorder was
        // restarted, which is exactly what SPEC.md section 45 rules out.
        //
        // `Driver::take_the_settings_the_user_saved` is what keeps it current
        // from here on, once a pass, through `SessionManager::set_configuration`.
        // This line is only where it begins (`crate::settings`, issues #51 and
        // #421).
        recordings.configuration(),
        RecordingPlan::default(),
        plugins,
        Some(Arc::clone(recordings)),
    );

    announce(directory, &watcher, &driver.manager);

    let stopped = driver.watch(&mut watcher, signal);

    // Before the reporting below, and after every path out of the loop: from
    // this line on nothing will record a game that launches, and `get_status`
    // answers `idle` again rather than promising one (issue #584).
    drop(watching);

    if let Some(reason) = stopped {
        tracing::error!(
            reason,
            "games are no longer being recorded automatically, because detection stopped"
        );
        eprintln!("Games are no longer being recorded automatically: {reason}");
    }
}

/// Where recordings and session records go.
///
/// Three answers in order: what this run was told on its command line, what the
/// user configured, and the Clipped folder of their videos directory. The flag
/// wins over the setting because it is what somebody typed for this run
/// (`docs/configuration.md`).
fn output_directory(
    args: &WatchArgs,
    configured: Option<&Path>,
) -> Result<PathBuf, WatchCommandError> {
    recordings_directory(args.output_directory.as_deref(), configured)
}

/// The same, for a caller with no command line to read a flag from.
///
/// `serve --watch-for-games` is that caller: it has the settings file and
/// nothing else, and a directory chosen once in the settings screen has to mean
/// the same thing to it as it does to `watch` (AGENTS.md section 55).
fn recordings_directory(
    named: Option<&Path>,
    configured: Option<&Path>,
) -> Result<PathBuf, WatchCommandError> {
    let directory = chosen_recordings_directory(named, configured)?;

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

/// Which directory those three layers name, without making it.
///
/// Split out because a directory saved from the Settings screen has to be
/// resolved *again* while the recorder is running, and the answer has to be the
/// one this run started with when nothing has changed — a second rule for
/// "where do recordings go" is the scattering AGENTS.md section 30 forbids, and
/// two rules that disagreed would have the settings screen offer to move
/// recordings that were never anywhere else (issue #609).
fn chosen_recordings_directory(
    named: Option<&Path>,
    configured: Option<&Path>,
) -> Result<PathBuf, WatchCommandError> {
    // Three layers, top down: the flag, then the settings file, then the videos
    // folder this build would pick on its own. The middle one is step 3 of
    // SPEC.md section 45 - a directory chosen once in the settings screen has to
    // be the one an automatic recording lands in, since nobody is at a command
    // line when a game launches (issue #307).
    Ok(match (named, configured) {
        (Some(named), _) => named.to_path_buf(),
        (None, Some(chosen)) => chosen.to_path_buf(),
        (None, None) => {
            crate::config::default_output_directory().ok_or(WatchCommandError::NoOutputDirectory)?
        }
    })
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
pub(crate) fn report_unreadable_settings(error: &ConfigurationError) {
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
    /// The settings generation the manager's configuration was taken at, or
    /// [`None`] before the first pass has taken one.
    ///
    /// Compared against `RecordingState::settings_generation` once a pass. When
    /// they differ the user has saved something since the manager was last
    /// told, and only then is the settings lock taken and the configuration
    /// cloned — see [`Self::take_the_settings_the_user_saved`].
    ///
    /// [`None`] rather than the generation read beside the configuration in
    /// [`Self::new`], so that there is no window between the two reads for a
    /// save to fall down. The first pass refreshes unconditionally, which
    /// re-sets the configuration the manager already has, and after that the
    /// comparison is exact.
    settings_generation: Option<u64>,
    /// Where a recording this driver starts makes itself reachable over the
    /// protocol, when there is a protocol.
    ///
    /// [`Some`] under `serve --watch-for-games`, and [`None`] under `watch`,
    /// which serves none. It is what a bookmark, a screenshot and a stop arrive
    /// through, and handing the recording over is all it takes — there is one
    /// implementation of each of the three, in `crate::serve`, and it does not
    /// know or care which of the two started the recording it is acting on
    /// (issue #421, AGENTS.md section 55).
    recordings: Option<Arc<RecordingState>>,
    /// The overlay this driver watches for edits, or [`None`] when the machine
    /// has no user directory to keep one in.
    ///
    /// Held rather than asked for each pass so that a test can point a driver at
    /// a scratch file: the real answer is the user's own
    /// `%LOCALAPPDATA%\Clipped\games.toml`, and a test that stat-ed *that*
    /// would be reading somebody's real catalogue (AGENTS.md section 25).
    overlay: Option<PathBuf>,
    /// When the overlay was last modified, as this driver last saw it.
    ///
    /// `None` inside the `Some` is a file that is not there, which is an answer
    /// rather than an absence: an overlay appearing is an edit like any other,
    /// and comparing `None` against `Some(_)` is what notices it. The outer
    /// [`Option`] is "this driver has not looked yet", so the first pass loads
    /// rather than assuming what it started with is current.
    overlay_seen: Option<Option<SystemTime>>,
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
    /// Raised by `stop_recording` when somebody stopped this recording.
    ///
    /// The one thing the protocol cannot do to a recording on its own. Stopping
    /// it is a signal the recorder already holds; what it cannot decide is
    /// whether the *session* should start another recording of a game that is
    /// still running, which is the manager's to decide and this thread's to tell
    /// it (issue #421).
    asked_to_stop: Arc<AtomicBool>,
}

impl Driver {
    /// A driver with nothing running, recording each game at whatever the
    /// user's settings say it should be recorded at.
    ///
    /// `configuration` is what [`load_configuration`] read from the user's
    /// settings file — handed in rather than read here because `run` has
    /// already read it to work out where recordings go, and one process reading
    /// one file once is what stops the two answers disagreeing. A test that is
    /// about the whole path from a file on disk to the settings a recording is
    /// started with calls [`load_configuration`] itself and passes the result,
    /// including the two cases where there is nothing to read.
    ///
    /// `installed_plugins` is the other half of the same idea and is handed in
    /// rather than discovered here, because [`installed_plugins`] reads a real
    /// directory and reports what it refused: a test that builds a driver must
    /// not have its result depend on what is installed on the machine running
    /// it (AGENTS.md section 25).
    ///
    /// `recordings` is where a recording this driver starts is handed over so
    /// that the protocol can reach it, and is [`None`] for a process that serves
    /// no protocol. It is an argument rather than something set afterwards
    /// because forgetting it is the defect issue #421 is about, and an argument
    /// is what makes every caller answer the question.
    fn new(
        catalogue: Catalogue,
        launchers: Launchers,
        settings: AutomaticSettings,
        configuration: Configuration,
        plan: RecordingPlan,
        installed_plugins: Vec<InstalledPlugin>,
        recordings: Option<Arc<RecordingState>>,
    ) -> Self {
        // Per-game settings reach a recording through the manager: it resolves
        // them when it asks for one, and `attempt` lays the answer over what
        // the command line asked for (issue #61).
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
            settings_generation: None,
            overlay: clipped_game_detection::catalogue::overlay_path(),
            overlay_seen: None,
            recordings,
        }
    }

    /// The loop. Returns the reason detection stopped, if that is why it ended.
    fn watch(&mut self, watcher: &mut ProcessWatcher, signal: &ShutdownSignal) -> Option<String> {
        let mut stopping = false;
        let mut detection_stopped: Option<String> = None;

        loop {
            // Before anything in this pass can ask the manager for a recording:
            // an action produced below carries the settings the manager
            // resolved, so a save that has landed since the last pass has to be
            // in the manager before `observe` or `poll` are called, not after.
            self.take_the_settings_the_user_saved();
            self.take_the_catalogue_the_user_edited();

            // The sitting this recorder is in, before anything in this pass can
            // change it, so that what the protocol reports is never more than
            // one pass — about a second — behind what the manager holds. A
            // status is a copy of the recorder's state and can be stale by the
            // time it is drawn whatever this does (`clipped_ipc::status`); what
            // it must not be is *absent*, which is what it was before issue
            // #584.
            self.report_the_sitting();

            // And where they are going, which the pass above may just have
            // changed and which the manager itself changes when it closes a
            // sitting. A window asking for the settings compares it against
            // what the file holds, which is how it can say that a directory
            // somebody saved a minute ago is saved and not yet in use
            // (issue #609).
            self.say_where_recordings_are_going();

            // First of all, and before the recording that is running can be
            // collected: `stop_recording` raises the flag *before* it raises
            // the stop signal, so a pass that sees the recording finished has
            // already had a pass at the flag. Reading it after collecting the
            // outcome would let the manager schedule another recording of the
            // same game before it was told the user had asked for the stop.
            self.take_the_stop_the_user_asked_for();

            // Then what a plugin had to say, every time round: it is only
            // useful while the recording it belongs to is running, and a
            // `PluginTrouble` nobody reads is one that was logged and forgotten
            // (AGENTS.md section 45).
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

    /// Gives the manager the settings as the Settings screen last saved them,
    /// if it has not already got them.
    ///
    /// # Why this exists
    ///
    /// The manager owns the configuration it resolves each game's settings
    /// from, and it is handed one copy when this driver is built. Nothing used
    /// to replace that copy, so a microphone saved from the window meant one
    /// thing to a recording the window asked for — which reads the settings
    /// state at the moment it starts (`crate::serve::RecordingState::start`) —
    /// and another to a recording detection started, until the recorder was
    /// restarted. SPEC.md section 45 says in as many words that an MVP
    /// requiring a restart is not finished, and this is the seam that was
    /// missing: `set_configuration` existed and had no production caller.
    ///
    /// # The next recording, not the running one
    ///
    /// Deliberately the next one. `SessionManager::set_configuration` says the
    /// same thing from its own side: a recording resolves its settings once,
    /// when it starts, and nothing here reaches into one that is running. A
    /// user who changes the frame rate mid-game gets it on the next recording
    /// rather than a re-opened encoder halfway through a file — which would
    /// mean a file whose second half does not match its first, and is a
    /// different question from the one the MVP asks. This call cannot do
    /// otherwise even by accident: the running recording's settings were
    /// resolved and handed to its thread before this ran, and `self.running` is
    /// not touched here.
    ///
    /// # What it costs
    ///
    /// One relaxed atomic load per pass — about one a second — and nothing
    /// else on the passes where nothing was saved, which is nearly all of them.
    /// The settings lock is taken and the configuration cloned only on a pass
    /// that follows a save. Neither happens on a capture thread: this is the
    /// watcher's loop, which already sleeps a second at a time waiting on
    /// process events, and the recording's own thread never reads settings
    /// after it starts (AGENTS.md section 20).
    ///
    /// # What it does not touch
    ///
    /// `installed_plugins` and `plugin_consents`, both of which are read once
    /// at start-up and documented as such on the fields. Consents are cloned
    /// out of the configuration *before* it moves into the manager precisely so
    /// that `attach_plugins` never goes looking for a settings file, and
    /// replacing what the manager holds leaves that clone alone. A plugin
    /// enabled from the window still arrives on the next run of the recorder,
    /// which is what the plugins are documented to do and is not what issue #51
    /// is about.
    ///
    /// Per-game overrides survive, because what is replaced is the whole
    /// `Configuration` — global settings and per-game layers together, as
    /// `apply_settings` last wrote them — rather than a global half laid over
    /// per-game entries this driver had been carrying (AGENTS.md section 30).
    ///
    /// # The recording directory travels the same pass, and lands differently
    ///
    /// Where automatic recordings are written is **not** in the `Configuration`
    /// this replaces: it is frozen into the manager's `AutomaticSettings` by
    /// [`recordings_directory`] before this thread starts. It rides this same
    /// generation check — one mechanism, not two — and is handed over by
    /// [`Self::take_the_directory_the_user_saved`], which is where the
    /// difference between the two is: a configuration reaches the next
    /// *recording*, and a directory reaches the next *sitting*. A session's
    /// record is written next to the files it names, so a directory that moved
    /// half way through a sitting would leave the record in one folder and some
    /// of its own recordings in another (AGENTS.md section 56, issue #609).
    ///
    /// Nothing happens under `watch`, which serves no protocol: there is no
    /// Settings screen to save from, so the start-up configuration is still the
    /// only one there has ever been.
    /// Re-reads the catalogue when the user's overlay has changed.
    ///
    /// A user adds a game, renames one or excludes one by editing
    /// `games.toml` (`docs/game-detection.md`). Until this, that reached
    /// detection only when the recorder was restarted: `SessionManager::new`
    /// was handed a catalogue and held it for the life of the driver, so
    /// somebody who excluded a game watched it go on being recorded
    /// ([issue #245](https://github.com/wildware-uk/clipped/issues/245)).
    ///
    /// **The modification time and not a generation.** The settings are written
    /// through this process, so a counter it owns is the honest signal there;
    /// the overlay is written by a text editor, and the filesystem's own answer
    /// is the only thing that knows. One `metadata` call a pass, which is the
    /// same order of cost as the settings' atomic load and is not on any capture
    /// thread (AGENTS.md section 20).
    ///
    /// **A file that is not there is an answer.** An overlay appearing is an
    /// edit, and `None` compared against `Some(_)` is what notices it. So is one
    /// being deleted, which puts the shipped catalogue back.
    ///
    /// A read that fails leaves the catalogue alone and says so once per change
    /// rather than once a pass: a half-written file being saved is a state that
    /// lasts milliseconds, and a driver that dropped every game because it
    /// caught one would be worse than one that waited for the next pass.
    fn take_the_catalogue_the_user_edited(&mut self) {
        let Some(overlay) = self.overlay.clone() else {
            return;
        };

        // Read before the catalogue, for the reason the settings' generation is:
        // an edit landing between the two must leave this driver believing it is
        // behind — one redundant reload on the next pass — rather than believing
        // it is current with a catalogue that predates the edit.
        let modified = std::fs::metadata(&overlay)
            .and_then(|data| data.modified())
            .ok();
        if self.overlay_seen == Some(modified) {
            return;
        }
        self.overlay_seen = Some(modified);

        match Catalogue::load_with_overlay_at(&overlay) {
            Ok(loaded) => {
                let catalogue = loaded.into_catalogue();
                tracing::info!(
                    overlay = %RedactedPath::new(&overlay),
                    entries = catalogue.entries().len(),
                    "the game catalogue was re-read after the user's own file changed"
                );
                self.manager.set_catalogue(catalogue);
            }
            Err(error) => tracing::warn!(
                overlay = %RedactedPath::new(&overlay),
                %error,
                "the user's game file changed and could not be read, so detection is still \
                 using the catalogue it had; it will be tried again when the file changes again"
            ),
        }
    }

    fn take_the_settings_the_user_saved(&mut self) {
        let Some(recordings) = self.recordings.as_ref() else {
            return;
        };

        // Before the configuration is asked for, never after. A save landing
        // between the two must leave this driver believing it is behind — one
        // redundant refresh on the next pass, costing a clone — rather than
        // believing it is current with a configuration that predates the save,
        // which would strand that setting until the next save after it.
        let generation = recordings.settings_generation();
        if self.settings_generation == Some(generation) {
            return;
        }

        let configuration = recordings.configuration();
        let configured = configuration
            .storage()
            .recording_directory()
            .map(Path::to_path_buf);
        self.manager.set_configuration(configuration);
        self.take_the_directory_the_user_saved(configured.as_deref());
        self.settings_generation = Some(generation);
    }

    /// Gives the manager the recording directory the Settings screen last
    /// saved.
    ///
    /// # Where it lands, and when
    ///
    /// [`SessionManager::set_recording_directory`] decides that, and the rule is
    /// **between sittings, never during one**: a sitting's session record is
    /// written next to the recordings it names, and a directory that moved half
    /// way through would separate the two — silently, because every file is
    /// still on disk and nothing is left able to say which sitting they
    /// belonged to (AGENTS.md section 56). With no sitting open it is in force
    /// at once, which is the case whenever nobody is playing; with one open it
    /// is held until the manager closes that sitting.
    ///
    /// # Why the folder is made here
    ///
    /// For the reason [`recordings_directory`] makes it at start-up: this
    /// recorder may run for days before it writes anything, and "the drive you
    /// named is not there" is not a thing to find out at the moment a game
    /// launches (AGENTS.md section 17). Doing it when the save arrives is as
    /// close to the moment somebody could act on the failure as this loop gets.
    ///
    /// **A folder that cannot be made is still taken.** The alternative is a
    /// setting the user saved, saw accepted, and which silently does not apply
    /// — and the failure is already handled where it belongs: a recording
    /// starting into a directory that is missing, is not a directory, or cannot
    /// be written to fails naming it (`crate::config::ConfigError`), which is a
    /// reported failure rather than a control that lies.
    ///
    /// # Threads
    ///
    /// The watcher's own loop, on a pass that follows a save and on no other:
    /// one relaxed atomic load is what gets this far, so the directory read and
    /// the `create_dir_all` happen once per save rather than once a second. No
    /// capture thread reaches here, and a recording in progress is untouched --
    /// it was given its output path when it started (AGENTS.md section 20).
    fn take_the_directory_the_user_saved(&mut self, configured: Option<&Path>) {
        // Resolved by the same three layers `run` resolved it by, so that a
        // settings file which says nothing lands back on the same default
        // rather than looking like a change. `None` for the flag: the recorder
        // that serves a Settings screen has no `--output-directory` of its own,
        // and one that did would be honouring what somebody typed for this run
        // over what a window saved.
        let directory = match chosen_recordings_directory(None, configured) {
            Ok(directory) => directory,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the recording directory that was saved could not be resolved, so automatic \
                     recordings keep going where they were going"
                );
                return;
            }
        };

        // Handed over whether or not it looks like a change, and the manager
        // decides. It is the one that knows where recordings are going *and*
        // where a sitting is holding them back from going, and a second copy of
        // that rule here got it wrong in exactly one case: somebody who picks a
        // folder mid-game and then picks the old one back would have had the
        // first choice arrive anyway when the sitting ended, because from out
        // here the second save looks like no change at all (AGENTS.md
        // section 30).
        if let Err(error) = std::fs::create_dir_all(&directory) {
            tracing::warn!(
                directory = %RedactedPath::new(&directory),
                %error,
                "the recording directory that was saved could not be created; it is still what \
                 automatic recordings will be started into, and one that cannot be written to \
                 fails naming it"
            );
        }

        if !self.manager.set_recording_directory(directory.clone()) {
            eprintln!(
                "Recordings will go to {} from the next session. The sitting that is open keeps \
                 its recordings where they are.",
                directory.display()
            );
        }
    }

    /// Puts where automatic recordings are going on the state the protocol
    /// reads.
    ///
    /// It is what the Settings screen's "recordings still go to ..." sentence is
    /// answered from (`crate::settings::note_where_recordings_still_go`), and
    /// it has to come from here rather than from the settings file, because the
    /// file holds what was *saved* and this holds what is in *force* — which
    /// are the same thing except during the one sitting that separates them.
    ///
    /// Once a pass, like [`Self::report_the_sitting`], and for the same reason:
    /// the manager moves the directory itself when a sitting ends, so nothing
    /// else knows the moment it happened. It is one small lock and a path
    /// comparison on a loop that turns over about once a second, on no capture
    /// thread.
    fn say_where_recordings_are_going(&self) {
        let Some(recordings) = self.recordings.as_ref() else {
            return;
        };
        recordings.automatic_recordings_go_to(self.manager.recording_directory());
    }

    /// Puts the sitting this recorder is in on the status the protocol reports,
    /// or takes it off when there is none.
    ///
    /// A sitting outlives the recording it is made of: a game that exits keeps
    /// its sitting open for the restart grace, so the same game launching again
    /// rejoins it rather than fragmenting one sitting into two. During that
    /// period this recorder is watching *and* in a sitting, and a window that
    /// dropped the game's name for those few seconds would flicker between
    /// "Counter-Strike 2" and "watching for games" and back — which is what
    /// `clipped_ipc::Watching::session` exists to prevent (issue #241).
    ///
    /// Reported while a recording is running as well as between recordings,
    /// deliberately. It is invisible then — a recorder that is recording reports
    /// `recording` — but it means the sitting is already there the instant the
    /// recording ends, rather than arriving on the pass after it and blanking
    /// the game's name in between.
    ///
    /// Nothing happens under `watch`, which serves no protocol and hands its
    /// recordings nowhere.
    fn report_the_sitting(&self) {
        let Some(recordings) = self.recordings.as_ref() else {
            return;
        };

        recordings.sitting_is(
            self.manager
                .active_session()
                .map(|session| Box::new(sitting_of(session))),
        );
    }

    /// Tells the session manager that somebody stopped the recording, if they
    /// did.
    ///
    /// The stop itself has already happened — `stop_recording` raised the
    /// recording's own signal and is waiting for the file — so what is left is
    /// the decision this thread owns: whether the sitting starts another
    /// recording of a game that is still running. It does not, until the game
    /// relaunches (`SessionManager::asked_to_stop_recording`, issue #421).
    ///
    /// Taken rather than read, so that one stop is reported once.
    fn take_the_stop_the_user_asked_for(&mut self) {
        let asked = self
            .running
            .as_ref()
            .is_some_and(|running| running.asked_to_stop.swap(false, Ordering::SeqCst));
        if asked {
            self.manager.asked_to_stop_recording();
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
                SessionAction::RememberCaptureMethod { game, method } => {
                    // The manager has already applied it to the configuration
                    // it holds, so the next recording of this game prefers it
                    // whether or not this write happens. What this does is make
                    // it outlive the process.
                    //
                    // Through the settings file service rather than a store of
                    // this driver's own: it is the one thing in the process
                    // holding a `ConfigurationStore`, and going around it would
                    // mean two writers racing over one file — including with
                    // the Settings screen (`crate::settings`). A `watch` run
                    // with no window attached has none, which is why this is a
                    // `debug` line and not a warning: nothing was lost that the
                    // next recording of this session needs.
                    match self.recordings.as_ref() {
                        Some(recordings) => recordings.remember_capture_method(&game, method),
                        None => tracing::debug!(
                            game = game.as_str(),
                            capture_backend = method.log_value(),
                            "there is no settings file to record the capture method in, so it \
                             will be preferred for the rest of this run and not after it"
                        ),
                    }
                }
                SessionAction::SessionEnded(session) => {
                    report_session(&session);
                    // The sidecar is on disk by now — the manager wrote it
                    // before it handed the session over — so this is the first
                    // moment the library has a complete sitting to index. A
                    // recording `start_recording` made asks for the same run
                    // from `RecordingState::finish`; an automatic session is
                    // closed here, so this is where it is asked for
                    // (`crate::library`, issue #402). Without it the sitting
                    // somebody has just finished playing would not appear in
                    // the window until the recorder was restarted.
                    if let Some(recordings) = self.recordings.as_ref() {
                        // Before the index run and not after it: the event
                        // carries the sitting's files precisely because the
                        // library may not have indexed one of them yet, so a
                        // window is told a sitting is over at the moment it
                        // can offer to open it rather than when a walk of the
                        // recordings folder finishes (`clipped_ipc::Event`,
                        // issue #241).
                        recordings.sitting_ended(sitting_of(&session));
                        recordings.index_now();
                    }
                    // And the sitting comes off the status in the same breath.
                    // The loop reports it once round, so leaving it would be up
                    // to a second of a window naming a game nobody is playing,
                    // after it has been told that sitting ended.
                    self.report_the_sitting();
                }
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

        // Made here, beside the stop signal, because the two answer one
        // question between them: the signal ends the file, and this says the
        // session should not start another. Shared with the recording's thread
        // so that whoever adopts the recording can raise it.
        let asked_to_stop = Arc::new(AtomicBool::new(false));
        let reachable = self.recordings.as_ref().map(|recordings| Reachable {
            recordings: Arc::clone(recordings),
            asked_to_stop: Arc::clone(&asked_to_stop),
        });

        let thread = thread::Builder::new()
            .name("clipped-automatic-recording".to_owned())
            .spawn(move || {
                record_process(
                    &request,
                    &plan,
                    &signal,
                    &recording_progress,
                    reachable.as_ref(),
                )
            })
            .expect("a thread can be started to record on");

        self.running = Some(Running {
            id,
            stop,
            thread,
            plugins,
            progress,
            asked_to_stop,
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

/// One sitting, as the control protocol describes one.
///
/// The live counterpart of what the library will hold once the sitting has been
/// written down and indexed, and it deliberately carries the same field names
/// for the same facts: they are the same facts about the same sitting a few
/// seconds apart (`clipped_ipc::SessionSummary`). Everything the recorder learns
/// afterwards — a row identifier, a size on disk, a favourite — is absent,
/// because none of it is known while the recording is still being written.
///
/// The mapping is here, in the recorder, for the reason
/// `crate::library`'s is: `clipped-session` is a domain crate that does not link
/// the protocol, and the process that owns the state is the one that answers for
/// it ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)).
pub(crate) fn sitting_of(session: &Session) -> clipped_ipc::SessionSummary {
    clipped_ipc::SessionSummary {
        session_id: session.id().as_str().to_owned(),
        game_id: attributed(session.game()).map(|(game_id, _)| game_id.to_owned()),
        game_name: attributed(session.game()).map(|(_, name)| name.to_owned()),
        // The same stamp the sidecar carries, from the same formatter, so that
        // a window which saw this sitting live recognises it when the library
        // hands it back (AGENTS.md section 55).
        started_at: rfc3339(session.started_at()),
        // Absent, because a sitting on a *status* is one the recorder is still
        // in: the manager holds it until it closes it, and closing it is what
        // hands it over. It is mapped rather than written as `None` so that
        // this says what the session says — and once the sitting is over, the
        // same mapping describes it on a `SessionEnded` event, which is the
        // whole reason there is one shape and not two
        // (`clipped_ipc::SessionSummary`).
        ended_at: session.ended_at().map(rfc3339),
        // From the session's own events, which is where the reason it ended
        // was written down: the sidecar carries that event and the library
        // reads the same word back out of it, so a window told a sitting ended
        // because the game exited and a library row saying so cannot disagree
        // (`Session::end_reason`, AGENTS.md section 55). Absent while the
        // sitting is open, where inventing one would be a screen saying why a
        // sitting ended that has not.
        end_reason: session.end_reason().map(|reason| reason.token().to_owned()),
        recordings: session.recordings().iter().map(file_of).collect(),
    }
}

/// The game a sitting is filed under, when the catalogue attributed one.
///
/// [`None`] for a tie and for a window the catalogue claims nothing about, which
/// is what `game_id` and `game_name` being absent means on the wire: the sitting
/// is filed under no game rather than under a guess (`docs/sessions.md`).
/// Deliberately not [`GameIdentity::slug`] and
/// [`GameIdentity::display_name`], which answer `unattributed` and "an
/// unidentified window" — words for a log line, and a name a screen would draw
/// as though the catalogue had said it.
const fn attributed(game: &GameIdentity) -> Option<(&String, &String)> {
    match game {
        GameIdentity::Known { game_id, name } => Some((game_id, name)),
        GameIdentity::Ambiguous { .. } | GameIdentity::Unidentified => None,
    }
}

/// One file of a sitting, as the protocol describes it.
fn file_of(recording: &SessionRecording) -> clipped_ipc::SessionRecording {
    clipped_ipc::SessionRecording {
        session_index: recording.index(),
        output: recording.output().to_string_lossy().into_owned(),
        // Absent while it is still being written, which is the answer for the
        // last file of a sitting that is still open. A recording that produced
        // nothing is listed all the same, carrying `no-window` or `failed`: a
        // sitting whose recording failed is not a sitting with one fewer
        // recording (AGENTS.md section 27).
        outcome: recording
            .outcome()
            .map(|outcome| outcome.token().to_owned()),
        // **Why the file ended, and not only that it did.** A recording
        // somebody stopped answers this in the reply to their stop; a recording
        // that ended by itself has no reply, and this event is the only thing
        // the recorder sends about it. Without the reason on here, a window
        // watching a recording finish because its window was dragged to a new
        // size cannot tell that from one that ran to the end — which is the
        // silence [issue #625](https://github.com/wildware-uk/clipped/issues/625)
        // is about. The word is the sidecar's own and the index's own
        // (`EndReason::token`, `clipped_ipc::LibraryRecording::end_reason`), so
        // the announcement and the library row a minute later say the same
        // thing (AGENTS.md section 55).
        end_reason: recording.outcome().and_then(ended_because),
        duration_ms: recording.outcome().and_then(recorded_length),
    }
}

/// Why a finished recording ended, for one that reached an ending.
///
/// [`None`] for `no-window` and `failed`: neither ever opened a file it could
/// finish, so there is no ending to give a reason for, and inventing one would
/// be a window drawing a sentence about a recording that never happened
/// (AGENTS.md section 27).
fn ended_because(outcome: &RecordingOutcomeSummary) -> Option<String> {
    match outcome {
        RecordingOutcomeSummary::Recorded { end_reason, .. } => Some(end_reason.token().to_owned()),
        RecordingOutcomeSummary::NoWindow { .. } | RecordingOutcomeSummary::Failed { .. } => None,
    }
}

/// How long a finished recording covers, for one that produced a file.
///
/// The span between the first and last timestamps written, which is what a
/// player would show — not the wall-clock time between starting and stopping,
/// which is longer by however long the encoder took to produce its first frame.
fn recorded_length(outcome: &RecordingOutcomeSummary) -> Option<u64> {
    match outcome {
        RecordingOutcomeSummary::Recorded { duration, .. } => {
            Some(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        }
        RecordingOutcomeSummary::NoWindow { .. } | RecordingOutcomeSummary::Failed { .. } => None,
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
    quality_preset: crate::options::PresetSelection,
    framerate: crate::options::Framerate,
    codec: crate::options::VideoCodec,
    encoder: crate::options::EncoderSelection,
    microphone: crate::options::AudioDeviceSelection,
    system_audio: crate::options::AudioDeviceSelection,
    window_timeout: Duration,
}

/// What a recording needs in order to be reachable over the protocol.
///
/// Carried to the recording's own thread, because the moment a recording
/// becomes reachable is the moment there is a window to record — not the moment
/// the game launched. A recorder that reported itself as recording while it was
/// still waiting for a game to draw would be refusing `start_recording` for two
/// minutes over a window that may never appear, and answering `get_status` with
/// a recording that does not exist (AGENTS.md section 54).
#[derive(Debug)]
struct Reachable {
    recordings: Arc<RecordingState>,
    asked_to_stop: Arc<AtomicBool>,
}

impl Reachable {
    /// Says a recording has been started and is waiting for `game` to draw.
    ///
    /// Paired with [`Self::window_appeared`], which every path out of the wait
    /// calls ([issue #739](https://github.com/wildware-uk/clipped/issues/739)).
    fn waiting_for_a_window(&self, game: &str) {
        self.recordings.waiting_for_a_window(Some(game.to_owned()));
    }

    /// Says the wait is over, however it ended.
    fn window_appeared(&self) {
        self.recordings.waiting_for_a_window(None);
    }

    /// Hands the recording over, or says why it could not be.
    fn adopt(
        &self,
        output: &Path,
        target: String,
        progress: &RecordingProgress,
        stop: &ShutdownSignal,
        settings: Vec<clipped_session::config::EffectiveSetting>,
    ) -> Result<Adopted, String> {
        self.recordings.adopt(
            output,
            target,
            progress,
            stop,
            &self.asked_to_stop,
            settings,
        )
    }
}

impl Default for RecordingPlan {
    /// What `serve --watch-for-games` records at, having no command line to be
    /// told by.
    ///
    /// The same values `watch`'s own flags default to, and the same ones
    /// `serve` already gives a `start_recording` that names nothing
    /// (`crate::serve::record_args`): the shipped defaults of the types
    /// themselves. A recording made automatically and one made from the window
    /// are the same recording, and this is where that stays true for the
    /// parameters neither of them names (AGENTS.md section 55).
    fn default() -> Self {
        Self {
            resolution: crate::options::Resolution::default(),
            quality_preset: crate::options::PresetSelection::default(),
            framerate: crate::options::Framerate::DEFAULT,
            codec: crate::options::VideoCodec::default(),
            encoder: crate::options::EncoderSelection::default(),
            microphone: crate::options::AudioDeviceSelection::default(),
            system_audio: crate::options::AudioDeviceSelection::default(),
            window_timeout: Duration::from_secs(u64::from(DEFAULT_WINDOW_TIMEOUT_SECONDS)),
        }
    }
}

impl RecordingPlan {
    fn from(args: &WatchArgs) -> Self {
        Self {
            resolution: args.resolution,
            quality_preset: args.quality_preset,
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
            quality_preset: self.quality_preset,
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
    reachable: Option<&Reachable>,
) -> RecordingOutcome {
    let outcome = attempt(request, plan, stop, progress, reachable);

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
    reachable: Option<&Reachable>,
) -> RecordingOutcome {
    attempt_with(
        request,
        plan,
        stop,
        progress,
        reachable,
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
    reachable: Option<&Reachable>,
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
    // No configured directory to fall back on, and none needed: an automatic
    // recording's output is decided by the session manager, under the directory
    // `run` resolved, and `plan.args_for` always names it — so the settings file
    // has already been consulted where that directory was chosen
    // (`output_directory`).
    let config = match RecordingConfig::resolve(&args, None) {
        Ok(config) => config,
        Err(error) => return failure(&error, game),
    };

    // Said before the wait and taken back after it, so a window can tell a
    // recording that is starting from a recorder that is idle. They are the same
    // status otherwise, and the interval is not short: a real Garry's Mod launch
    // spent 48 seconds here while the window said the recorder would record the
    // game "if it starts" ([issue
    // #739](https://github.com/wildware-uk/clipped/issues/739)).
    //
    // Announced here rather than inside `find_window` because this is the layer
    // that has both the game's name and the way back to the recorder's state —
    // `find_window` is a function of a target and a timeout, deliberately, so it
    // can be tested against desktops a test constructed.
    if let Some(reachable) = reachable {
        reachable.waiting_for_a_window(game);
    }
    let found = find_window(&config.target, plan.window_timeout, stop);
    // On every path out, including the failure: a recorder that gave up waiting
    // must not go on saying it is about to record something.
    if let Some(reachable) = reachable {
        reachable.window_appeared();
    }

    let window = match found {
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
        .apply_configured_to(settings_for(&config, &window))
        // Not part of `apply_configured_to`, and deliberately: it is not a
        // setting. It is what a previous recording of this game was observed to
        // end on, and it only changes which capture candidate is asked first
        // (issue #286).
        .with_remembered_capture_method(request.remembered_capture_method);

    // Here, and not when the recording was asked for: this is the moment there
    // is something a bookmark could mark. From now until the recording ends,
    // `add_bookmark`, `take_screenshot`, `stop_recording` and `get_status` all
    // reach it — through the one implementation of each, which does not know
    // this recording was started by anything unusual (issue #421).
    let adopted = match reachable
        .map(|reachable| {
            reachable.adopt(
                &request.output,
                request.game.display_name().to_owned(),
                progress,
                stop,
                // What this recording is *running with*, which is the command
                // line with this game's configured settings laid over it,
                // not the configuration on its own, which says nothing about
                // the flags no layer overrode (issue #61, criterion 3).
                clipped_session::config::effective_settings(&settings, &request.settings),
            )
        })
        .transpose()
    {
        Ok(adopted) => adopted,
        // One recording at a time is this process's rule whoever asked for it.
        // Somebody recording by hand keeps the encoder, and the sitting records
        // that it got no footage rather than silently getting none.
        Err(refusal) => return failure(&refusal, game),
    };

    // `record_into` rather than `record`, for the outputs this command needs:
    // where the recording's timeline begins, which is what places a plugin's
    // event inside the file (`clipped_session::plugins`), and — once the
    // recording is reachable — where a `take_screenshot` asks it for a frame it
    // has already captured. Both are stores the capture thread cannot wait on.
    let mut outputs = RecordingOutputs::default().with_progress(progress);
    if let Some(adopted) = adopted.as_ref() {
        outputs = outputs.with_screenshots(adopted.screenshots());
        // And where it says which capture backend it settled on, so that a
        // recording detection started reaches the Diagnostics screen the same
        // way one the window started does (issue #302). Only when the recording
        // is reachable: a recording nothing can ask about has nobody to tell.
        outputs = outputs.with_capture_account(adopted.capture());
    }
    let outcome =
        match std::panic::catch_unwind(AssertUnwindSafe(|| record(&settings, stop, &outputs))) {
            Ok(Ok(report)) => RecordingOutcome::Recorded(Box::new(report)),
            Ok(Err(error)) => session_failure(&error, game, &config.output),
            Err(_) => failure(
                &"the recording thread panicked; the file was finalised before it did",
                game,
            ),
        };

    // Handed back before this thread returns, so that the recorder's status,
    // and anything waiting on `stop_recording`, learn what became of it. A path
    // that skipped this would leave a recording nobody could stop; `Adopted`
    // releases itself on drop for exactly that reason.
    if let Some(adopted) = adopted {
        adopted.finished(&outcome);
    }
    outcome
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::Duration;

    use clipped_game_detection::catalogue::EntrySource;
    use clipped_game_detection::{LaunchGroup, LaunchId, ProcessSnapshot};
    use clipped_ipc::{ApplySettings, CommandHandler, EventPublisher, RecorderStatus};

    use crate::library::LibraryIndexer;
    use clipped_session::config::{
        AudioDeviceSetting, GameKey, Preferences, ResolutionSetting, SettingSource,
    };
    use clipped_session::{
        AudioSourceSetting, CodecPreference, EncoderPreference, UnavailableChoice,
    };

    use super::*;
    use crate::test_support::Scratch;

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
    ///
    /// The removal is [`Scratch`]'s rather than this type's own. What it had
    /// was `let _ = fs::remove_dir_all(…)` in [`Drop`]: that takes a failing
    /// test's evidence with it, and it cannot report a removal that did not
    /// happen — the defect PR #597 was written to expose (issue #598).
    struct TestDirectory(Scratch);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = Scratch::new(&format!("watch-{label}"));
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

    /// A command line that asks for something no default would produce, so that
    /// a settings file that quietly replaced it is visible in an assertion.
    fn args() -> WatchArgs {
        WatchArgs {
            quality_preset: crate::options::PresetSelection::default(),
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

    /// A recorder over a library, an index and a catalogue of this test's own.
    ///
    /// Never `RecorderService::new`: that one reads the settings, the games
    /// file and the recording library of whoever is running the tests
    /// (AGENTS.md section 25).
    fn a_recorder(directory: &TestDirectory) -> Arc<RecorderService> {
        a_recorder_publishing_to(directory, &clipped_ipc::EventPublisher::new())
    }

    /// The same, publishing through a publisher the caller keeps hold of.
    ///
    /// For the one test that has to read what was published rather than what
    /// was decided: the publisher has to be the same value on both sides of the
    /// pipe (`crate::test_events`).
    fn a_recorder_publishing_to(
        directory: &TestDirectory,
        events: &clipped_ipc::EventPublisher,
    ) -> Arc<RecorderService> {
        Arc::new(RecorderService::with_library(
            events.clone(),
            crate::library::LibraryReader::at(Some(directory.recordings().join("library.db"))),
            crate::library::LibraryIndexer::at(
                Some(directory.recordings().join("library.db")),
                vec![directory.recordings()],
            ),
            Catalogue::default(),
        ))
    }

    #[test]
    fn pressing_the_bookmark_hotkey_during_an_automatic_recording_marks_the_moment() {
        // Issue #421's acceptance criterion, end to end and through every real
        // link: the attempt that a launch produces, the moment it hands the
        // recording over, the handler `serve` registers for `add_bookmark`, the
        // command that handler sends, and the recorder answering it against the
        // recording detection started.
        //
        // The press is `handlers_for(...).press(...)` and not `perform(...)`,
        // for the reason `crate::hotkeys`'s own tests give: `perform` is told
        // which action to run, so calling it proves only that an action maps to
        // a command. What is being asserted here is that the key wired to Add
        // bookmark reaches a recording nobody asked for.
        let directory = TestDirectory::new("hotkey-bookmark");
        let service = a_recorder(&directory);
        let request = recording_asked_for(&directory, &directory.settings_file());
        let reachable = Reachable {
            recordings: Arc::clone(service.recordings()),
            asked_to_stop: Arc::new(AtomicBool::new(false)),
        };
        let progress = RecordingProgress::new();

        let mut pressed = None;
        let outcome = attempt_with(
            &request,
            &RecordingPlan::from(&args()),
            &ShutdownSignal::new(),
            &progress,
            Some(&reachable),
            |_, _, _| Ok(window()),
            |_settings, _stop, outputs| {
                // Inside the recording: this is the only moment at which there
                // is anything to bookmark, and it is where a key press lands.
                assert!(
                    outputs.screenshots.is_some(),
                    "a recording that can be reached must serve a screenshot from a frame it \
                     already has, rather than opening a second capture of the same window",
                );
                progress.reached(Duration::from_secs(120));

                let recorder = Arc::clone(&service) as Arc<dyn CommandHandler>;
                pressed = Some(
                    // The real resolver, because this presses the bookmark key
                    // and nothing on that path asks what is in front. A press
                    // that *did* would be reaching Windows from a test, which
                    // `crate::hotkeys`'s own tests are what cover.
                    crate::hotkeys::handlers_for(&recorder, &crate::hotkeys::what_is_in_front())
                        .press(
                            clipped_hotkeys::HotkeyAction::AddBookmark,
                            "Ctrl+F9".parse().expect("Ctrl+F9 is a hotkey"),
                        )
                        .map(|()| recorder.status()),
                );
                Err(SessionError::TargetHasNoPixels)
            },
        );

        assert!(
            matches!(outcome, RecordingOutcome::Failed { .. }),
            "the stand-in engine reports a failure: {outcome:?}"
        );
        let status = pressed
            .expect("the recording ran")
            .expect("this build performs Add bookmark, so its key has a handler");
        let RecorderStatus::Recording(active) = status else {
            panic!("the recording was still running when the key was pressed");
        };
        assert_eq!(
            active.target,
            request.game.display_name(),
            "the window has to be told which game is being recorded",
        );

        let read = clipped_session::bookmarks::BookmarkFile::for_recording(&request.output)
            .expect("a press writes the bookmark down beside the recording");
        assert_eq!(read.bookmarks.len(), 1, "one press, one mark: {read:?}");
        assert_eq!(
            read.bookmarks[0].at().as_secs_f64(),
            120.0 - clipped_session::bookmarks::DEFAULT_LEAD.as_secs_f64(),
            "and it lands where a bookmark of a window-started recording lands: the moment \
             before the press, by the same lead",
        );

        assert!(
            matches!(service.status(), RecorderStatus::Idle),
            "and the recording is handed back when it ends, so nothing goes on claiming it",
        );
    }

    /*
     * The interval a window used to describe as an idle recorder.
     *
     * A game is recognised, a recording is started for it, and the recorder
     * waits for the game to draw. On a real Garry's Mod launch that was 48
     * seconds, and `get_status` answered `watching` throughout -- the same
     * answer a recorder with nothing to do gives (issue #739).
     *
     * Asserted *during* the wait rather than around it, which is what
     * `attempt_with` taking its window-finder as an argument is for: the
     * closure is the wait, so asking the recorder what it is doing from inside
     * it is asking at the only moment the answer matters. A test that looked
     * before and after would pass on a build that never set the state at all.
     */
    #[test]
    fn a_recording_waiting_for_its_game_to_draw_says_so_while_it_waits() {
        let directory = TestDirectory::new("waiting-says-so");
        let service = a_recorder(&directory);
        let request = recording_asked_for(&directory, &directory.settings_file());
        let reachable = Reachable {
            recordings: Arc::clone(service.recordings()),
            asked_to_stop: Arc::new(AtomicBool::new(false)),
        };
        let watching = service.recordings().watch_for_games();

        let mut during = None;
        let outcome = attempt_with(
            &request,
            &RecordingPlan::from(&args()),
            &ShutdownSignal::new(),
            &RecordingProgress::new(),
            Some(&reachable),
            |_, _, _| {
                during = Some(service.status());
                Ok(window())
            },
            |_settings, _stop, _outputs| Err(SessionError::TargetHasNoPixels),
        );

        let during = during.expect("the window was looked for");
        let RecorderStatus::Watching(waiting) = during else {
            panic!("a recorder waiting for a game's window is watching, not {during:?}");
        };
        let pending = waiting
            .pending
            .expect("a recording has been started and is waiting for the game to draw");
        assert_eq!(
            pending.game_name,
            request.game.display_name(),
            "the wait names the game as the catalogue names it, because a window cannot turn a \
             process identifier into one"
        );

        // And it is taken back. A recorder that went on saying it was about to
        // record something it is already recording is the same untruth pointing
        // the other way.
        let RecorderStatus::Watching(after) = service.status() else {
            panic!("this recorder is still watching once the attempt is over");
        };
        assert!(
            after.pending.is_none(),
            "the wait is over, so nothing is waiting for a window: {:?}",
            after.pending
        );
        drop(watching);
        // Cleared on the failure path too, which is the one that matters most:
        // a recorder that gave up waiting must not go on saying it is about to
        // record something.
        assert!(
            matches!(outcome, RecordingOutcome::Failed { .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_recording_nothing_can_reach_is_still_made_and_still_reports_its_own_timeline() {
        // The other side of the seam, which is what `watch` is. Handing over
        // nowhere has to leave the recording exactly as it was — the same
        // settings, the same progress, the same outcome — because the whole of
        // this change is meant to be reachability and not a second kind of
        // recording (AGENTS.md section 55).
        let directory = TestDirectory::new("unreachable");
        let request = recording_asked_for(&directory, &directory.settings_file());
        let progress = RecordingProgress::new();

        let mut screenshots_offered = None;
        let outcome = attempt_with(
            &request,
            &RecordingPlan::from(&args()),
            &ShutdownSignal::new(),
            &progress,
            None,
            |_, _, _| Ok(window()),
            |_settings, _stop, outputs| {
                screenshots_offered = Some(outputs.screenshots.is_some());
                assert!(
                    outputs.progress.is_some(),
                    "the timeline is what places a plugin's event, and belongs to every recording",
                );
                Err(SessionError::TargetHasNoPixels)
            },
        );

        assert_eq!(
            screenshots_offered,
            Some(false),
            "nothing can ask this recording for a screenshot, so it is not offered a way to \
             serve one",
        );
        assert!(
            matches!(outcome, RecordingOutcome::Failed { .. }),
            "{outcome:?}"
        );
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

    /// The exit of a process that was launched by [`launch`].
    ///
    /// The fields are `clipped_game_detection`'s own and public, because an exit
    /// is remembered rather than looked up: by the time anything learns a
    /// process has gone there is nothing left to ask about it.
    fn exit(pid: u32, image_name: &str) -> WatchEvent {
        WatchEvent::Exited(clipped_game_detection::ProcessExit {
            process: ProcessSnapshot::new(pid, 4, None, image_name),
            launch: LaunchId::ALREADY_RUNNING,
        })
    }

    /// A driver that hands its recordings to `service`, over the one game in
    /// [`GAMES`].
    fn a_driver(directory: &TestDirectory, service: &Arc<RecorderService>) -> Driver {
        Driver::new(
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue"),
            Launchers::none(),
            AutomaticSettings::new(directory.recordings()),
            load_configuration(Some(&directory.settings_file())),
            RecordingPlan::from(&args()),
            Vec::new(),
            Some(Arc::clone(service.recordings())),
        )
    }

    /// Issue #584's second acceptance criterion, and the reason
    /// `Watching::session` is on the wire at all.
    ///
    /// The manager is driven through the real events — a launch, the game
    /// exiting, and the poll that closes the sitting once its restart grace has
    /// run out — because those are the three moments the answer changes, and
    /// each is a wall-clock reading this test hands over rather than waits for.
    /// What is asserted is what a window would draw at each of them.
    #[test]
    fn the_sitting_a_watching_recorder_is_in_travels_on_its_status_until_the_grace_runs_out() {
        let directory = TestDirectory::new("watching-sitting");
        let service = a_recorder(&directory);
        let watching = service.recordings().watch_for_games();
        let driver = &mut a_driver(&directory, &service);

        driver.report_the_sitting();
        assert_eq!(
            service.status(),
            RecorderStatus::Watching(clipped_ipc::Watching {
                session: None,
                pending: None
            }),
            "a recorder watching for anything at all is `watching` and nothing more: an empty \
             sitting invented to fill the field would be a game name a window could draw",
        );

        let launched = SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725);
        // The manager is driven directly rather than through `Driver::apply`,
        // which would try to record: what this test is about is the sitting the
        // manager holds, and making a recording of it would need a desktop, a
        // GPU and an encoder. The recording it asks for is reported finished
        // below, which is the half the sitting's policy actually turns on.
        let asked = driver
            .manager
            .observe(&launch(4242, "test-game.exe"), launched)
            .into_iter()
            .find_map(|action| match action {
                SessionAction::StartRecording(request) => Some(request.recording),
                _ => None,
            })
            .expect("a launch of a game in the catalogue asks for a recording");
        driver.report_the_sitting();

        let sitting = the_sitting(&service);
        assert_eq!(
            (sitting.game_id.as_deref(), sitting.game_name.as_deref()),
            (Some("test-game"), Some("Test Game")),
            "the game is the whole reason a sitting is on the wire: `target` is a capture \
             selector and a window cannot turn one into a name without the catalogue",
        );
        assert_eq!(
            sitting.ended_at, None,
            "a sitting on a status is one the recorder is still in",
        );
        assert_eq!(
            sitting.recordings.len(),
            1,
            "the file this sitting has asked for is part of it: {:?}",
            sitting.recordings,
        );
        assert_eq!(
            sitting.recordings[0].outcome, None,
            "and it is still being written, which is what an absent outcome means",
        );

        // The game goes, and the recording it was of ends. This is the restart
        // grace, and the whole of what the field exists for: the recorder is
        // watching *and* in a sitting, and a window that dropped the name for
        // those seconds would flicker between "Test Game" and "watching for
        // games" and back.
        let exited = launched + Duration::from_secs(60);
        let _ = driver.manager.observe(&exit(4242, "test-game.exe"), exited);
        let _ = driver.manager.recording_finished(
            &asked,
            RecordingOutcome::Failed {
                detail: "the stand-in engine in this test records nothing".to_owned(),
            },
            exited,
        );
        driver.report_the_sitting();

        let in_the_grace = the_sitting(&service);
        assert_eq!(
            in_the_grace.session_id, sitting.session_id,
            "the same sitting, so that a relaunch inside the grace rejoins what is on screen",
        );
        assert_eq!(in_the_grace.game_name.as_deref(), Some("Test Game"));
        assert_eq!(
            (
                in_the_grace.recordings[0].outcome.as_deref(),
                in_the_grace.recordings[0].duration_ms,
            ),
            (Some("failed"), None),
            "a recording that produced no file is listed all the same, and says so: a sitting              whose recording failed is not a sitting with one fewer recording",
        );

        // And out the other side of it. `DEFAULT_RESTART_GRACE` is a minute and
        // `DEFAULT_SUSPEND_GAP` is ninety seconds, so this reading closes the
        // sitting by the grace rather than by the suspend rule.
        let _ = driver.manager.poll(
            exited + clipped_session::automatic::DEFAULT_RESTART_GRACE + Duration::from_secs(1),
        );
        driver.report_the_sitting();
        assert_eq!(
            service.status(),
            RecorderStatus::Watching(clipped_ipc::Watching {
                session: None,
                pending: None
            }),
            "once the sitting is over the recorder is watching for anything again, and a window \
             that went on showing the game would be naming one nobody is playing",
        );

        // And the claim goes back when the watcher does, which is what stops a
        // recorder whose detection has stopped promising to record the next
        // game to launch.
        drop(watching);
        assert_eq!(
            service.status(),
            RecorderStatus::Idle,
            "nothing is watching now, and `idle` is what a recorder that will record nothing by \
             itself answers",
        );
    }

    /// Issue #241's second acceptance criterion, for the sitting a driver owns.
    ///
    /// `Event::SessionEnded` was defined, mirrored in the desktop's TypeScript
    /// and carried by the schema, and **nothing ever sent one**: the driver's
    /// `SessionEnded` arm printed a summary to the console and asked for an
    /// index run, and said nothing to any window.
    ///
    /// It reads what was *published*, through a real server over a real pipe
    /// (`crate::test_events`), because a test that called the publisher itself
    /// is precisely the test this defect would have passed. The manager is
    /// driven through the real events — a launch, the game exiting, and the
    /// poll that closes the sitting once its restart grace has run out — and
    /// what it decides goes through `Driver::apply`, which is the arm being
    /// asserted. Only the recording is left out: making one would need a
    /// desktop, a GPU and an encoder.
    #[test]
    fn a_sitting_that_ends_is_announced_with_the_files_it_produced() {
        let directory = TestDirectory::new("sitting-ended");
        let events = clipped_ipc::EventPublisher::new();
        let service = a_recorder_publishing_to(&directory, &events);
        let subscribed = crate::test_events::Subscribed::to(&events, &service, "sitting-ended");
        let watching = service.recordings().watch_for_games();
        let driver = &mut a_driver(&directory, &service);

        let launched = SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725);
        let asked = driver
            .manager
            .observe(&launch(4242, "test-game.exe"), launched)
            .into_iter()
            .find_map(|action| match action {
                SessionAction::StartRecording(request) => Some(request.recording),
                _ => None,
            })
            .expect("a launch of a game in the catalogue asks for a recording");
        driver.report_the_sitting();

        // The game goes, and the recording it was of ends. The sitting stays
        // open through its restart grace, which is why the end of a sitting is
        // not the end of a recording and needs an event of its own.
        let exited = launched + Duration::from_secs(60);
        let _ = driver.manager.observe(&exit(4242, "test-game.exe"), exited);
        let _ = driver.manager.recording_finished(
            &asked,
            RecordingOutcome::Failed {
                detail: "the stand-in engine in this test records nothing".to_owned(),
            },
            exited,
        );
        driver.report_the_sitting();

        // And out the other side of the grace, which is where the manager
        // closes the sitting and hands it over.
        let actions = driver.manager.poll(
            exited + clipped_session::automatic::DEFAULT_RESTART_GRACE + Duration::from_secs(1),
        );
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, SessionAction::SessionEnded(_))),
            "the manager should close the sitting once its restart grace has run out: {actions:?}",
        );
        driver.apply(actions);

        let event = subscribed.wait_for("a sitting ending", |event| {
            matches!(event, clipped_ipc::Event::SessionEnded { .. })
        });
        let clipped_ipc::Event::SessionEnded { session } = event else {
            unreachable!("`wait_for` matched a sitting ending")
        };

        assert_eq!(
            session.game_name.as_deref(),
            Some("Test Game"),
            "a window is told which sitting ended, in the words it was shown while it ran",
        );
        assert!(
            session.ended_at.is_some(),
            "what makes a sitting over is `ended_at`, and this one is: {session:?}",
        );
        assert_eq!(
            session.end_reason.as_deref(),
            Some("game-exited"),
            "and why it ended, from the session's own record of it",
        );
        assert_eq!(
            session.recordings.len(),
            1,
            "the files it produced are the point of the event: {:?}",
            session.recordings,
        );
        assert_eq!(
            session.recordings[0].outcome.as_deref(),
            Some("failed"),
            "a recording that produced no file is listed all the same, saying so",
        );

        assert_eq!(
            service.status(),
            RecorderStatus::Watching(clipped_ipc::Watching {
                session: None,
                pending: None
            }),
            "and the sitting comes off the status in the same breath: a window that went on \
             naming the game after being told the sitting ended would be showing two answers",
        );

        drop(subscribed);
        drop(watching);
        service.shut_down();
    }

    /// The sitting on the recorder's status, or the reason there is none.
    fn the_sitting(service: &Arc<RecorderService>) -> clipped_ipc::SessionSummary {
        match service.status() {
            RecorderStatus::Watching(watching) => *watching
                .session
                .expect("a recorder in a sitting carries it on its status"),
            other => panic!("expected a watching recorder, got {other:?}"),
        }
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
            // Read here rather than inside the driver, which is the whole of
            // what `run` does: from the file on disk to the settings a
            // recording is started with.
            load_configuration(Some(settings_file)),
            RecordingPlan::from(&args()),
            // No plugins: what a settings file does to a recording is what
            // these tests are about, and reading this machine's plugins
            // directory would make them depend on it (AGENTS.md section 25).
            Vec::new(),
            // And nowhere to hand a recording over to: what these tests are
            // about is the settings a recording is made with, which is the
            // same question whether or not anything can reach it.
            None,
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
            None,
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
    fn a_setting_saved_while_the_recorder_runs_reaches_the_driver_on_its_next_pass() {
        // The seam four fixes rest on, asserted for the first time.
        //
        // `tests/integration/tests/settings_reach_the_running_recorder.rs`
        // checks that somebody *answered* the propagation question for every
        // setting, and its own documentation is explicit that it cannot check
        // the propagation itself — "nothing static can". Nothing dynamic did
        // either: the only driver test with a settings file builds one with
        // `recordings: None`, so `take_the_settings_the_user_saved` returned at
        // its first line and the refresh ran in no test at all (issue #648).
        //
        // A regression here would look exactly like the four defects that guard
        // exists for — #608, #623, #647 and the hotkeys one — and every answer
        // in that file would still read as true while being false in fact.
        let directory = TestDirectory::new("saved-mid-run");
        let settings_file = directory.settings_file();
        ConfigurationStore::at(&settings_file)
            .store(Configuration::defaults())
            .expect("the settings file can be written");

        let settings = Arc::new(crate::settings::SettingsFile::at(&settings_file));
        let recordings = Arc::new(crate::serve::RecordingState::new(
            EventPublisher::new(),
            Arc::new(LibraryIndexer::at(
                Some(directory.recordings().join("library.db")),
                vec![directory.recordings()],
            )),
            Arc::clone(&settings),
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue"),
            Launchers::none(),
            // No games file: these are about the settings seam, not the
            // catalogue, and must not be able to write one (AGENTS.md
            // section 25).
            None,
        ));

        let mut driver = Driver::new(
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue"),
            Launchers::none(),
            AutomaticSettings::new(directory.recordings()),
            // What the driver started with: the defaults, before any save.
            Configuration::defaults(),
            RecordingPlan::from(&args()),
            Vec::new(),
            Some(Arc::clone(&recordings)),
        );

        // The driver has to have looked once, or the first pass after a save is
        // indistinguishable from the first pass ever.
        driver.take_the_settings_the_user_saved();
        assert_eq!(
            driver.manager.configuration().global().framerate(),
            None,
            "nothing has been saved yet"
        );

        let mut values = BTreeMap::new();
        values.insert("framerate".to_owned(), Some("120".to_owned()));
        settings
            .apply(&ApplySettings { game: None, values })
            .expect("120 is a framerate the settings file accepts");

        driver.take_the_settings_the_user_saved();

        assert_eq!(
            driver.manager.configuration().global().framerate(),
            Some(120),
            "a setting saved while the recorder runs has to reach the manager on its next pass, \
             or the next automatically-started recording is made with what the recorder started \
             with (issue #51, SPEC.md section 45)"
        );
    }

    #[test]
    fn a_pass_with_nothing_saved_does_not_re_read_the_settings() {
        // The other half, and what the generation is for: without it this would
        // clone the configuration once a second for the life of the process,
        // which is the cost `RecordingState::settings_generation` documents
        // avoiding (AGENTS.md section 20).
        let directory = TestDirectory::new("nothing-saved");
        let settings_file = directory.settings_file();
        ConfigurationStore::at(&settings_file)
            .store(Configuration::defaults())
            .expect("the settings file can be written");

        let settings = Arc::new(crate::settings::SettingsFile::at(&settings_file));
        let recordings = Arc::new(crate::serve::RecordingState::new(
            EventPublisher::new(),
            Arc::new(LibraryIndexer::at(
                Some(directory.recordings().join("library.db")),
                vec![directory.recordings()],
            )),
            Arc::clone(&settings),
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue"),
            Launchers::none(),
            // No games file: these are about the settings seam, not the
            // catalogue, and must not be able to write one (AGENTS.md
            // section 25).
            None,
        ));

        let mut driver = Driver::new(
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue"),
            Launchers::none(),
            AutomaticSettings::new(directory.recordings()),
            Configuration::defaults(),
            RecordingPlan::from(&args()),
            Vec::new(),
            Some(Arc::clone(&recordings)),
        );

        driver.take_the_settings_the_user_saved();
        let generation = recordings.settings_generation();

        driver.take_the_settings_the_user_saved();

        assert_eq!(
            recordings.settings_generation(),
            generation,
            "a pass that read the settings again would have moved nothing, but this asserts the \
             cheap half was cheap: the generation is what the driver compares, and nothing else \
             may touch it"
        );
    }

    #[test]
    fn a_game_added_to_the_users_own_file_reaches_detection_without_a_restart() {
        // Issue #245. `SessionManager::new` was handed a catalogue and held it
        // for the life of the driver, so somebody who added a game to
        // `games.toml` — the documented way to add one
        // (`docs/game-detection.md`) — watched it go on not being recorded
        // until they restarted the recorder.
        //
        // The overlay is pointed at a scratch file rather than the real one: a
        // test that stat-ed `%LOCALAPPDATA%\Clipped\games.toml` would be
        // reading somebody's own catalogue (AGENTS.md section 25).
        let directory = TestDirectory::new("overlay-added");
        let overlay = directory.recordings().join("games.toml");

        let mut driver = Driver::new(
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue"),
            Launchers::none(),
            AutomaticSettings::new(directory.recordings()),
            Configuration::defaults(),
            RecordingPlan::from(&args()),
            Vec::new(),
            None,
        );
        driver.overlay = Some(overlay.clone());

        // The first pass, with no overlay there: the driver looks, finds
        // nothing, and holds the shipped catalogue.
        driver.take_the_catalogue_the_user_edited();
        assert!(
            driver
                .manager
                .catalogue()
                .find_by_id("a-game-i-added")
                .is_none(),
            "nothing has been added yet"
        );

        fs::write(
            &overlay,
            "schema_version = 1\n\n\
             [[game]]\n\
             game_id = \"a-game-i-added\"\n\
             name = \"A game I added\"\n\n\
             [[game.executables]]\n\
             name = \"mine.exe\"\n",
        )
        .expect("the scratch overlay can be written");

        driver.take_the_catalogue_the_user_edited();

        assert!(
            driver
                .manager
                .catalogue()
                .find_by_id("a-game-i-added")
                .is_some(),
            "a game added to the user's own file has to reach detection on the next pass, or \
             they are told to restart a recorder that is running (issue #245)"
        );
    }

    #[test]
    fn a_game_taken_out_of_the_users_own_file_stops_being_detected() {
        // The other direction, and the one that matters more: an exclusion or a
        // deletion the driver never re-read is a game somebody believes is no
        // longer being recorded and which is still being recorded.
        //
        // A file going away is an edit like any other — `None` compared against
        // `Some(_)` — and this is what proves the comparison is on the answer
        // rather than on the file existing.
        let directory = TestDirectory::new("overlay-removed");
        let overlay = directory.recordings().join("games.toml");
        fs::write(
            &overlay,
            "schema_version = 1\n\n\
             [[game]]\n\
             game_id = \"a-game-i-added\"\n\
             name = \"A game I added\"\n\n\
             [[game.executables]]\n\
             name = \"mine.exe\"\n",
        )
        .expect("the scratch overlay can be written");

        let mut driver = Driver::new(
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue"),
            Launchers::none(),
            AutomaticSettings::new(directory.recordings()),
            Configuration::defaults(),
            RecordingPlan::from(&args()),
            Vec::new(),
            None,
        );
        driver.overlay = Some(overlay.clone());

        driver.take_the_catalogue_the_user_edited();
        assert!(
            driver
                .manager
                .catalogue()
                .find_by_id("a-game-i-added")
                .is_some(),
            "the overlay was there on the first pass"
        );

        fs::remove_file(&overlay).expect("the scratch overlay can be removed");

        driver.take_the_catalogue_the_user_edited();

        assert!(
            driver
                .manager
                .catalogue()
                .find_by_id("a-game-i-added")
                .is_none(),
            "a file going away is an edit too, and the shipped catalogue has to come back"
        );
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
            quality_preset: crate::options::PresetSelection::default(),
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
