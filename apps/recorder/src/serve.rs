//! The `serve` subcommand: the recorder as a service the desktop application
//! drives.
//!
//! `record` is one recording and then the process exits. `serve` is the shape
//! the recorder actually runs in — started at login, outliving every window the
//! user opens, and told what to do over the protocol in `clipped-ipc`
//! ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md),
//! `docs/ipc.md`).
//!
//! This module is the *application* behind that protocol. `clipped-ipc` owns
//! the transport, the framing, the handshake and the dispatch; what is here is
//! the part that knows what a recording is: [`RecorderService`] answers the
//! commands, and [`RecordingState`] owns the one recording this process will
//! run at a time and the thread it runs on.
//!
//! # What it records
//!
//! What the window asks for, and — with `--watch-for-games` — what a game
//! launching asks for. The second is the shape a shipped build runs in: one
//! process holds the launch watcher, the control protocol and the global
//! hotkeys, so a bookmark, a screenshot and a stop reach a recording nobody had
//! to start (`crate::watch::AutomaticRecorder`, [`RecordingState::adopt`],
//! [issue #421](https://github.com/wildware-uk/clipped/issues/421)).
//!
//! Both kinds of recording live in the same [`RecordingState`] and are acted on
//! by the same commands. What differs is who owns the *session*: a recording the
//! window asked for is the whole of its own, and an automatic one belongs to a
//! sitting the session manager on the watcher's thread owns.
//!
//! # What it can actually do
//!
//! Start a recording, stop one, mark a moment in one, say what it is doing,
//! read the recording library, and copy a finished recording into MP4. The
//! recording is the one `record` makes, through the same `clipped-session`
//! call, validated by the same code (AGENTS.md section 55). Every other command
//! in the protocol belongs to a subsystem that is not built, and `clipped-ipc`
//! refuses those before they reach this module at all, with the milestone and
//! issue that build them.
//!
//! # What a recording is filed under
//!
//! The same thing `watch` files one under. This process loads the user's game
//! catalogue at start-up and every recording it starts asks it about the
//! window's process, so a sitting somebody recorded by pointing at
//! Counter-Strike lands beside the sittings detection recorded of it, and a
//! window that is not a game is recorded and filed under nothing rather than
//! under a guess ([`RecordingState::begin`], `docs/sessions.md`, issue #403).
//!
//! The last two are answered from files rather than from anything this process
//! is doing, on the connection thread the command arrived on: a library read is
//! a bounded query over local data (`crate::library`) and an export is a copy of
//! coded packets between two containers (`crate::export`). Neither shares a
//! lock, a queue or a file with a recording.
//!
//! # Bookmarks, and what they are not allowed to cost
//!
//! `add_bookmark` is answered on the connection thread it arrived on. It reads
//! one relaxed atomic — [`clipped_session::RecordingProgress`], which the
//! recording publishes once per encoded frame — appends to a `Vec` behind the
//! log's own mutex, and writes a small file. **None of that is on the recording
//! thread**: `clipped_session::record` is handed the progress handle and never
//! the bookmark log, so there is no lock, queue or file a bookmark shares with
//! capture (AGENTS.md section 20, `docs/bookmarks.md`).
//!
//! The one piece of state a bookmark and a recording *do* share is
//! [`RecordingState::current`], which the recording thread touches exactly twice
//! — when it is stored and when its outcome is — and never per frame. The
//! bookmark takes what it needs from it and lets go before it writes anything.
//!
//! # Threads
//!
//! ```text
//!  main thread            connection threads         recording thread
//!  ───────────            ──────────────────         ────────────────
//!  accept ──────────────▶ read, dispatch ──────────▶ capture, encode, mux
//!                         write the reply            publish what changed
//!  Ctrl+C: stop the listener, then stop the recording and wait for its file
//! ```
//!
//! A command never runs on the recording thread and the recording never runs on
//! a connection thread. That is not tidiness: `clipped_session::record` blocks
//! for the length of the recording, so answering `get_status` on the thread that
//! started it would mean the UI froze for as long as the recording lasted.
//!
//! # Shutting down
//!
//! Ctrl+C stops the listener and then stops the recording, in that order, and
//! waits for the file to be finalised before the process exits. The recording
//! is the only thing here that must survive its process ending correctly
//! (AGENTS.md section 17); connection threads own nothing and are left to go
//! with the process.
//!
//! The `shutdown` command takes the same path and deliberately not a second one:
//! `clipped-ipc` answers it by stopping the listener, so everything below
//! `server.serve` in [`run`] happens exactly as it does for Ctrl+C. That is what
//! makes a recorder started detached — with no console to receive Ctrl+C —
//! stoppable at all ([issue #220](https://github.com/wildware-uk/clipped/issues/220)).

use std::error::Error;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime};

use clipped_game_detection::catalogue::Catalogue;
use clipped_game_detection::launcher::Launchers;
use clipped_ipc::{
    features, ActiveRecording, AddBookmark, BookmarkSummary, Command, CommandHandler, EndReason,
    Endpoint, EventPublisher, HotkeyBinding, ProtocolError, RecorderStatus, RecordingSummary,
    ReplaySummary, Reply, SaveReplay, ScreenshotSummary, Server, ServerError, SessionSummary,
    StartRecording, StopRecording, TakeScreenshot, TransportError, Watching,
};
use clipped_ipc::{ErrorCode, PeerIdentity};
use clipped_logging::RedactedPath;
use clipped_session::automatic::{ManualSession, RecordedProcess, RecordingOutcome};
use clipped_session::bookmarks::{BookmarkError, BookmarkLog, BookmarkRequest};
use clipped_session::config::Configuration;
use clipped_session::screenshot::{
    Screenshot, ScreenshotError, ScreenshotFormat, ScreenshotRequests, ScreenshotSettings,
    StillFrame,
};
use clipped_session::{
    CaptureAccounting, CaptureTargetSettings, RecordingProgress, RecordingReport,
    RecordingSettings, ReplayRecording, ReplaySaveError,
};
use clipped_windows::WindowInfo;

use crate::cli::{RecordArgs, ServeArgs};
use crate::config::{ConfigError, RecordingConfig};
use crate::library::{LibraryIndexer, LibraryReader};
use crate::record::{resolve_window, settings_for};

/// What this build tells every client it can do.
///
/// A UI reads this rather than inferring capability from a version number, so
/// that a control whose command would be refused is never offered at all
/// (AGENTS.md section 27).
///
/// **Everything here is a fact about the build**, which is why
/// [`features::AUTOMATIC`] is deliberately not in the list: since issue #421 a
/// plain `serve` and a `serve --watch-for-games` are the same binary and differ
/// in what *this* recorder does, so it is added by
/// [`RecorderService::features`] from the recorder's own claim instead
/// ([issue #587](https://github.com/wildware-uk/clipped/issues/587)).
fn features_of_this_build() -> Vec<String> {
    vec![
        features::RECORDING.to_owned(),
        features::STATUS_EVENTS.to_owned(),
        features::BOOKMARKS.to_owned(),
        features::SCREENSHOTS.to_owned(),
        // The window checks for this before it draws a library screen at all,
        // so that a recorder built before issue #301 is told apart from a
        // library with nothing in it.
        features::LIBRARY.to_owned(),
        // And for this before it draws an Export control, so that nobody is
        // asked to choose a file name for a file an older recorder was never
        // going to write (issue #399).
        features::EXPORT.to_owned(),
        // And for this before it *subscribes* to the `exports` event stream.
        // Unlike every other name in this list, the cost of guessing wrong is
        // not an unusable button: an event stream a recorder does not have is
        // refused by name and the refusal takes the whole events connection
        // with it, so a window that assumed progress would lose its status
        // subscription too (issue #446).
        features::EXPORT_PROGRESS.to_owned(),
        // And for this before it draws a hotkey list, so that a recorder built
        // before issue #232 — which registered nothing at all — is told apart
        // from a machine on which every combination registered cleanly. The two
        // are opposite answers and the second is what an empty list looks like.
        // And for this before it draws a player, so that a recorder built
        // before issue #304 — which has no `open_playback` and would refuse the
        // request — is told apart from a recording that will not play.
        features::PLAYBACK.to_owned(),
        // And for this before it draws a tile that would hold a picture, so
        // that a recorder built before issue #448 — which has no
        // `open_preview` and would refuse the request once a row — is told
        // apart from a library whose pictures have simply not been made yet.
        // Those are the two answers a blank grid can mean, and only one of
        // them is worth waiting for.
        features::PREVIEWS.to_owned(),
        features::HOTKEYS.to_owned(),
        // And for this before it offers "Save Replay": a recorder built before
        // issue #38 parses `save_replay` and always refuses it, so the feature
        // is what tells an unusable button from a working one. Whether *this*
        // recording has a buffer to save from is
        // `ActiveRecording::replay_seconds`.
        features::REPLAY.to_owned(),
        // Every command behind it is one this build performs: the settings are
        // read and written through `clipped_session::config` and the
        // microphones are enumerated by `clipped-audio` (issue #51).
        features::SETTINGS.to_owned(),
        // Separate from the settings themselves, because a window that cannot
        // get a level should still draw the list of microphones rather than
        // refusing the whole screen (`clipped_ipc::features`, issue #109). This
        // build opens the endpoint and listens, on Windows; a build without an
        // audio backend does not claim it, so a window there draws the list and
        // says why there is no meter instead of showing one stuck at zero.
        #[cfg(windows)]
        features::MICROPHONE_LEVEL.to_owned(),
        // And this before it draws a start-at-login switch, so that a recorder
        // built before issue #308 — which has the settings commands and neither
        // of these two — is told apart from a recorder that is simply not set
        // to start at sign-in. Those are opposite answers, and the second is
        // what an unanswered command looks like if nobody checks.
        features::STARTUP.to_owned(),
        // And this before a Diagnostics screen draws anything against the
        // capture backend or the encoder. A recorder built before issue #302
        // has no `get_diagnostics` at all, and "Clipped found no encoder on
        // this machine" and "Clipped never asked" are opposite answers — which
        // is the worst pair of readings to confuse on the one screen whose
        // whole subject is what is and is not known.
        features::DIAGNOSTICS.to_owned(),
    ]
}

/// Why `serve` did not serve.
#[derive(Debug)]
pub enum ServeError {
    /// The endpoint could not be taken — most often because a recorder is
    /// already running.
    Endpoint(TransportError),
    /// Accepting connections failed.
    Serving(ServerError),
    /// The Ctrl+C handler could not be installed, so the recorder could not be
    /// stopped without killing it — and killing it during a recording is the
    /// failure the whole shutdown path exists to prevent.
    Shutdown(crate::shutdown::CtrlCError),
}

impl fmt::Display for ServeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(error) => write!(formatter, "{error}"),
            Self::Serving(error) => write!(formatter, "{error}"),
            Self::Shutdown(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for ServeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Endpoint(error) => Some(error),
            Self::Serving(error) => Some(error),
            Self::Shutdown(error) => Some(error),
        }
    }
}

/// Listens on the endpoint and serves the protocol until Ctrl+C.
///
/// Prints one line to standard output when it is ready to be connected to:
///
/// ```text
/// ready endpoint=\\.\pipe\clipped-recorder.1
/// ```
///
/// That line exists for whatever started the recorder — the supervisor in
/// [issue #106](https://github.com/wildware-uk/clipped/issues/106), and the
/// tests today. It is the one thing this subcommand writes to standard output;
/// everything else is a diagnostic and goes to standard error
/// (`docs/recorder-cli.md`).
///
/// # Errors
///
/// [`ServeError::Endpoint`] if the endpoint could not be taken,
/// [`ServeError::Shutdown`] if the process could not be made interruptible, and
/// [`ServeError::Serving`] if accepting connections failed part way through.
pub fn run(args: &ServeArgs) -> Result<(), ServeError> {
    let endpoint = match &args.endpoint {
        Some(name) => Endpoint::named(name),
        None => Endpoint::for_this_session(),
    }
    .map_err(ServeError::Endpoint)?;

    // Before anything measures a window. Without it every size the recorder
    // sees is the compatibility fiction Windows tells a DPI-unaware process,
    // and it cannot be turned on later — it is a property of the process, set
    // once (`crate::record`).
    crate::record::enable_dpi_awareness();

    let mut listener =
        clipped_ipc::transport::Listener::bind(&endpoint).map_err(ServeError::Endpoint)?;

    let events = EventPublisher::new();
    let service = Arc::new(RecorderService::new(events.clone()));

    let signal = crate::shutdown::ShutdownSignal::new();
    // The handler first, then re-enabling Ctrl+C, for the reason `record`
    // documents: a recorder started in a process group of its own inherits
    // Ctrl+C disabled, and turning it on before there is a handler would open a
    // window in which it terminates the process with default handling — with a
    // recording open.
    crate::shutdown::install_ctrl_c_handler(&signal).map_err(ServeError::Shutdown)?;
    crate::shutdown::allow_ctrl_c();

    let stopper = listener.stopper();
    let interrupted = signal.clone();
    let watching = thread::Builder::new()
        .name("clipped-serve-shutdown".to_owned())
        .spawn(move || {
            interrupted.wait();
            stopper.stop();
        })
        .expect("a thread can be started to watch for Ctrl+C");

    // After the endpoint has been taken and before the ready line, which is the
    // ordering the "exactly one process registers" requirement rests on:
    // `Listener::bind` above is what makes a second recorder exit, so by the
    // time anything here asks Windows for a combination this process is
    // demonstrably the only recorder in the session (ADR 0009, issue #232).
    //
    // Before the ready line as well, so that a window connecting the instant it
    // sees that line already has an answer to `get_hotkeys` rather than a race
    // with registration.
    let (hotkeys, registered) = crate::hotkeys::start(
        &(Arc::clone(&service) as Arc<dyn CommandHandler>),
        &service.configuration(),
    );
    service.publish_hotkeys(registered);

    // After the hotkeys, because this is the process that has them: one
    // process watches for games, serves the protocol and owns the
    // combinations, which is what makes `Ctrl`+`F9` reach a recording nobody
    // had to start (ADR 0009, issue #421). Started before the ready line so
    // that a window connecting the instant it appears is told about a game
    // already being recorded rather than finding out a moment later.
    let automatic = args
        .watch_for_games
        .then(|| crate::watch::AutomaticRecorder::start(&service));

    println!("ready endpoint={}", endpoint.path());
    // Flushed explicitly: whatever started this process is very likely blocked
    // reading that line, and standard output to a pipe is buffered.
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    // After the ready line and before the accept loop. The index is brought up
    // to date on a thread of its own, so nothing here waits for it: a window
    // that connects immediately is answered immediately, and the run catches up
    // on everything produced while nothing was indexing (`crate::library`,
    // issue #402).
    service.start_indexing();

    let server = Server::new(Arc::clone(&service), events.clone(), identity());
    let outcome = server.serve(&mut listener).map_err(ServeError::Serving);

    // The hotkeys first, and deliberately before the recording is stopped: a
    // press arriving while the recording was being finalised would ask this
    // service for something halfway through happening. Stopping gives every
    // combination back to Windows and waits for the handler that is running, so
    // by the line below no press can be in flight.
    hotkeys.stop();

    // Then the automatic recorder, before the line below: it owns a recording
    // of its own and a session record that has to be closed and written out,
    // and stopping it waits for both (AGENTS.md section 17). Nothing new can
    // start after this, because the process watcher has gone with it.
    if let Some(automatic) = automatic {
        automatic.stop();
    }

    // The listener has stopped, so nothing new can arrive. What is left is the
    // recording, which is the only thing in this process that must be finished
    // properly rather than abandoned (AGENTS.md section 17).
    service.shut_down();
    events.close();

    // Raised whether the loop ended through Ctrl+C or through a failure, so
    // that the watching thread is never left blocked on a signal nobody will
    // send.
    signal.request();
    let _ = watching.join();

    outcome
}

/// The catalogue every recording this process makes is attributed through, or
/// an empty one.
///
/// The same loader `watch` uses, so a game the user registered, renamed or
/// excluded means the same thing to a recording they started from the window as
/// it does to one detection started (AGENTS.md section 55, issue #45).
/// `crate::replay` reads it through here too, for the same reason and with the
/// same fallback: a sitting is filed under the game it is of whichever
/// subcommand made it.
///
/// **A catalogue that cannot be read does not stop a recording**, which is the
/// difference between this and `watch`. `watch` has nothing to do without a
/// catalogue and refuses to start; here the user has pointed at a window and
/// pressed record, and refusing them their footage over a malformed line in
/// their games file would be protecting the wrong thing (AGENTS.md sections 16
/// and 17).
///
/// What is lost instead is attribution, and the *empty* catalogue is why: the
/// failure is almost always in the user's own overlay, which is where their
/// exclusions and renames live, and falling back to the shipped data would file
/// recordings under games they told Clipped to leave alone. Every sitting made
/// while their file is unreadable is `unattributed`, which is honest, and the
/// error names the file so they can fix it.
pub(crate) fn catalogue_for_recordings() -> Catalogue {
    match crate::watch::load_catalogue() {
        Ok(catalogue) => catalogue,
        Err(error) => {
            tracing::error!(
                %error,
                "the game catalogue could not be read, so recordings made from the window will \
                 not be filed under a game until it is fixed"
            );
            eprintln!(
                "Your games file could not be read, so recordings will not be filed under a \
                 game: {error}"
            );
            Catalogue::default()
        }
    }
}

/// How this recorder introduces itself in every handshake.
fn identity() -> PeerIdentity {
    PeerIdentity {
        name: env!("CARGO_PKG_NAME").to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// The recorder behind the protocol.
#[derive(Debug)]
pub struct RecorderService {
    recordings: Arc<RecordingState>,
    /// The settings file, and the configuration in force (`crate::settings`,
    /// issue #51).
    ///
    /// Shared with [`RecordingState`] rather than copied into it, so that a
    /// setting saved from the window is what the *next* recording is made with
    /// — without a restart, and without anything re-reading a file underneath a
    /// recording that is already running.
    settings: Arc<crate::settings::SettingsFile>,
    /// The recording library, which this process reads on the window's behalf
    /// because the window cannot (`crate::library`, issue #301).
    library: LibraryReader,
    /// What puts a recording *into* that library (`crate::library`, issue #402).
    ///
    /// Held here as well as by [`RecordingState`] so that start-up can ask for
    /// the first run and shutdown can stop one, neither of which is a recording's
    /// business.
    indexer: Arc<LibraryIndexer>,
    /// What became of the global hotkeys, once `serve` has registered them
    /// (`crate::hotkeys`, issue #232).
    ///
    /// Filled in after this service exists rather than built with it, because
    /// the registration's handlers call *into* this service: a press is turned
    /// into the same [`Command`] the window would have sent, so the service has
    /// to exist before there is anything to register. Set once and never
    /// changed — rebinding without a restart is
    /// [issue #233](https://github.com/wildware-uk/clipped/issues/233).
    ///
    /// The [`Err`] is a registration that did not happen at all, kept as a
    /// sentence rather than collapsed into an empty list: "this recorder
    /// registered no hotkeys" and "every hotkey registered cleanly" are opposite
    /// answers (AGENTS.md section 27).
    hotkeys: OnceLock<Result<Vec<HotkeyBinding>, String>>,
    /// Where an export says how far it has got (`crate::export`, issue #446).
    ///
    /// A second handle on the publisher [`RecordingState`] holds, rather than a
    /// path through it: an export is not a recording and has no business
    /// reaching through the recording state to say so. Cloning is what
    /// [`EventPublisher`] is for — the subscriber list is shared, so both
    /// handles publish to the same windows.
    events: EventPublisher,
}

impl RecorderService {
    /// A service with nothing recording, over the library at Clipped's usual
    /// place.
    #[must_use]
    pub fn new(events: EventPublisher) -> Self {
        // The same file `watch` reads, so that "what does this record at" has
        // one answer whichever subcommand is asking (AGENTS.md sections 30 and
        // 55). Read once here and held, because from now on the window changes
        // it through this process (`crate::settings`, issue #51).
        let settings = Arc::new(crate::settings::SettingsFile::for_this_user());
        Self::over(
            events,
            LibraryReader::for_this_user(),
            // The storage limits come from the same settings file the recording
            // settings do. Without them the indexer sweeps nothing, which is
            // what an unconfigured machine gets (issue #111).
            LibraryIndexer::for_this_user()
                .with_storage(settings.configuration().storage().clone()),
            settings,
            // And the same catalogue `watch` matches processes against, read
            // once for the same reason (issue #403).
            catalogue_for_recordings(),
            // And the launchers, so that a recording started from the window is
            // filed under the game whichever shop installed it, rather than
            // only when the catalogue knows the executable's name (issue #522).
            Launchers::discover(),
        )
    }

    /// The same, over a library and a catalogue named by the caller, and the
    /// shipped settings.
    ///
    /// For tests, which must not read, create or index the library of whoever is
    /// running them, must not be told what to record by their settings file, and
    /// must not be told what a game is by their games file either (AGENTS.md
    /// section 25).
    #[must_use]
    pub fn with_library(
        events: EventPublisher,
        library: LibraryReader,
        indexer: LibraryIndexer,
        catalogue: Catalogue,
    ) -> Self {
        Self::over(
            events,
            library,
            indexer,
            // Nowhere, deliberately: a service built for a test must not read
            // or write the settings of whoever is running it (AGENTS.md
            // section 25). A test that is *about* the settings uses
            // [`Self::with_settings`].
            Arc::new(crate::settings::SettingsFile::nowhere()),
            catalogue,
            // A handler built for a test is told nothing about this machine's
            // launchers, for the reason its catalogue is a fixture (AGENTS.md
            // section 25).
            Launchers::none(),
        )
    }

    fn over(
        events: EventPublisher,
        library: LibraryReader,
        indexer: LibraryIndexer,
        settings: Arc<crate::settings::SettingsFile>,
        catalogue: Catalogue,
        launchers: Launchers,
    ) -> Self {
        let indexer = Arc::new(indexer);
        Self {
            recordings: Arc::new(RecordingState::new(
                events.clone(),
                Arc::clone(&indexer),
                Arc::clone(&settings),
                catalogue,
                launchers,
            )),
            settings,
            library,
            indexer,
            hotkeys: OnceLock::new(),
            events,
        }
    }

    /// The same, over a settings file the caller names.
    ///
    /// For the tests that are *about* the settings, which need a file of their
    /// own rather than the one belonging to whoever is running them.
    #[must_use]
    pub fn with_settings(
        events: EventPublisher,
        library: LibraryReader,
        indexer: LibraryIndexer,
        catalogue: Catalogue,
        settings: crate::settings::SettingsFile,
    ) -> Self {
        Self::over(
            events,
            library,
            indexer,
            Arc::new(settings),
            catalogue,
            Launchers::none(),
        )
    }

    /// The settings in force.
    ///
    /// `serve` asks for them to resolve the hotkey bindings, so that "what is
    /// Save Replay bound to" has one answer in this process (AGENTS.md section
    /// 30). A recording asks when it starts, and never again while it runs
    /// (issue #61).
    #[must_use]
    pub fn configuration(&self) -> Configuration {
        self.settings.configuration()
    }

    /// The one recording this process runs at a time.
    ///
    /// For the automatic recorder, which hands its recordings over to it so
    /// that a bookmark, a screenshot and a stop reach a recording nobody asked
    /// for through the same commands they reach one somebody did
    /// (`RecordingState::adopt`, issue #421).
    pub(crate) fn recordings(&self) -> &Arc<RecordingState> {
        &self.recordings
    }

    /// Records what became of the global hotkeys, so `get_hotkeys` can answer.
    ///
    /// Called once, by `serve`, immediately after registering them. A second
    /// call is ignored rather than overwriting the first: nothing re-registers
    /// today, and a report that could change under a window reading it would be
    /// a promise this build cannot keep (issue #233).
    pub fn publish_hotkeys(&self, registered: Result<Vec<HotkeyBinding>, String>) {
        if self.hotkeys.set(registered).is_err() {
            tracing::warn!("the hotkey registration was reported twice; the first one stands");
        }
    }

    /// Where every global hotkey stands.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Internal`] when no hotkey was registered, carrying the
    /// sentence that says why — a refusal rather than an empty list, because an
    /// empty list reads as "nothing conflicted".
    fn hotkeys(&self) -> Result<Vec<HotkeyBinding>, ProtocolError> {
        match self.hotkeys.get() {
            Some(Ok(hotkeys)) => Ok(hotkeys.clone()),
            Some(Err(reason)) => Err(ProtocolError::new(ErrorCode::Internal, reason.clone())),
            // Reachable only from a `RecorderService` nothing registered hotkeys
            // for, which today means a test. Saying so beats an empty list that
            // would be drawn as seven working hotkeys.
            None => Err(ProtocolError::new(
                ErrorCode::Internal,
                "this recorder has not registered any global hotkeys",
            )),
        }
    }

    /// Brings the index up to date with whatever happened while nothing was
    /// indexing, on a thread of its own.
    ///
    /// Called once `serve` is listening, so that a window connecting is never
    /// waiting on a walk of the recordings folder. What it catches is everything
    /// no run has seen: sessions `watch` recorded in a process of its own, files
    /// copied onto the machine, a library file the user deleted.
    pub fn start_indexing(&self) {
        self.indexer.start();
    }

    /// Stops anything still running, and waits for its file to be finished.
    pub fn shut_down(&self) {
        match self.recordings.stop(None) {
            Ok(summary) => tracing::info!(
                output = %RedactedPath::new(PathBuf::from(&summary.output)),
                frames = summary.frames_encoded,
                "the recording was stopped and finished because the recorder is shutting down"
            ),
            // `not_recording` is the ordinary case: a recorder shutting down
            // with nothing recording.
            Err(error) if error.code == ErrorCode::NotRecording => {}
            Err(error) => tracing::error!(%error, "the recording did not stop cleanly"),
        }

        // After the recording, deliberately: stopping it above wrote the
        // session's final record, so a run that is still going has the whole
        // sitting to find.
        //
        // A run in progress is *cancelled* rather than waited for, and a
        // recording stopped by this shutdown may therefore not reach the index
        // before the process ends. That costs nothing: what makes a session
        // findable is its sidecar, which is already on disk, and the run at the
        // next start-up indexes it. A recorder that would not close until it had
        // walked a large library would be a far worse thing to ship
        // (`clipped_library::index`, AGENTS.md section 17).
        self.indexer.shut_down();
    }
}

impl CommandHandler for RecorderService {
    fn call(&self, command: Command) -> Result<Reply, ProtocolError> {
        match command {
            Command::Ping => Ok(Reply::Pong),
            Command::GetStatus => Ok(Reply::Status {
                status: self.recordings.status(),
            }),
            Command::StartRecording(start) => {
                let started = self.recordings.start(&start)?;
                Ok(started)
            }
            Command::StopRecording(StopRecording { recording_id }) => Ok(Reply::RecordingStopped {
                summary: self.recordings.stop(recording_id.as_deref())?,
            }),
            Command::AddBookmark(request) => Ok(Reply::BookmarkAdded {
                bookmark: self.recordings.bookmark(&request, SystemTime::now())?,
            }),
            Command::TakeScreenshot(request) => Ok(Reply::ScreenshotTaken {
                screenshot: self.recordings.screenshot(&request, SystemTime::now())?,
            }),
            // On the connection thread it arrived on, like a bookmark and a
            // screenshot, and for the same reason: what it takes from the
            // recording state is a handle and a session, both of which it
            // copies out under the lock and lets go of before it writes
            // anything (`RecordingState::save_replay`).
            Command::SaveReplay(request) => Ok(Reply::ReplaySaved {
                clip: self.recordings.save_replay(&request, SystemTime::now())?,
            }),
            // Answered from the index rather than from anything this process is
            // doing, and deliberately on the connection thread: a library read
            // is a bounded query over local data and shares nothing with a
            // recording (`crate::library`).
            Command::LibrarySessions(request) => Ok(Reply::LibrarySessions {
                page: self.library.sessions(&request)?,
            }),
            Command::LibraryGames => Ok(Reply::LibraryGames {
                games: self.library.games()?,
            }),
            Command::LibraryEvents(request) => Ok(Reply::LibraryEvents {
                lane: self.library.events(&request)?,
            }),
            // A read like the three above it, answered on the connection thread
            // for the same reason: it is one statement over the index and
            // shares nothing with a recording.
            Command::LibraryTrash(_) => Ok(Reply::LibraryTrash {
                trash: self.library.trash()?,
            }),
            // These two write, and are still answered here: the trash's every
            // statement is its own transaction and a rename is a filesystem
            // call, so neither holds the database's one writer for longer than
            // a row update (`clipped_library::trash`).
            Command::RestoreFromTrash(request) => Ok(Reply::Restored {
                restored: self.library.restore(&request)?,
            }),
            Command::EmptyTrash(request) => Ok(Reply::TrashEmptied {
                emptied: self.library.empty(&request)?,
            }),
            // One row update, under the same argument again: a favourite mark
            // is a single `UPDATE` against a primary key and touches no file at
            // all (`clipped_library::favourites`).
            Command::SetFavourite(request) => Ok(Reply::Favourited {
                mark: self.library.set_favourite(&request, SystemTime::now())?,
            }),
            // The same again: one `UPDATE` against a primary key, and one read
            // back to answer with what is true rather than with what was asked
            // for (`clipped_library::locks`).
            Command::SetLock(request) => Ok(Reply::Locked {
                lock: self.library.set_lock(&request, SystemTime::now())?,
            }),
            // Also on the connection thread: reading a handful of manifests and
            // one settings file is bounded local work that shares nothing with
            // a recording, which is the same argument the library reads above
            // are answered here under.
            Command::Plugins => {
                let (installed, refused) = crate::plugins::declarations().map_err(|error| {
                    ProtocolError::new(
                        ErrorCode::Internal,
                        format!("the installed plugins could not be read: {error}"),
                    )
                })?;
                Ok(Reply::Plugins { installed, refused })
            }
            // Also on the connection thread, and for the same reason a library
            // read is: it touches no recording, takes no lock a recording takes
            // and opens a file of its own (`crate::export`). It is the slower
            // of the two by far — a copy of a long recording is bounded by the
            // disk — and it still may not run on the recording thread, which is
            // the one thing that must never wait.
            Command::ExportRecording(request) => Ok(Reply::RecordingExported {
                export: crate::export::export(&request, &self.events)?,
            }),
            // A read of the recording, and at most one copy of it: the same
            // shape as an export and on the same thread, for the same reasons
            // (`crate::playback`). Ordinarily it writes nothing at all — the
            // window plays the recording itself — and only choosing a sound
            // track other than the one a media element would reach costs a
            // pass over the file (issue #304).
            Command::OpenPlayback(request) => Ok(Reply::PlaybackOpened {
                playback: crate::playback::open(&request)?,
            }),
            // Answered by the indexer, because the indexer is what holds the
            // two services that make these (`crate::preview`, issue #448).
            Command::OpenPreview(request) => Ok(Reply::PreviewOpened {
                preview: self.indexer.preview(&request)?,
            }),
            // Answered from what registration produced when this process
            // started, which is a clone of a small `Vec` and touches nothing a
            // recording touches (`crate::hotkeys`, issue #232).
            Command::GetHotkeys => Ok(Reply::Hotkeys {
                hotkeys: self.hotkeys()?,
            }),
            // On the connection thread, like a library read, and for a stronger
            // version of the same reason: neither half touches a recording. The
            // capture account is one clone out of a mutex the recording thread
            // wrote once before its first frame, and the capability report is a
            // cached reading that never opens an encoder session — so a window
            // asking this during a recording costs that recording nothing
            // (`crate::diagnostics`, AGENTS.md sections 17 and 20, issue #302).
            Command::GetDiagnostics => Ok(Reply::Diagnostics {
                diagnostics: crate::diagnostics::diagnostics(
                    self.recordings.capture_account().as_ref(),
                )?,
            }),
            // Answered on the connection thread, like a library read: reading
            // or writing one small file shares nothing with a recording, and
            // the only lock it takes is the settings file's own
            // (`crate::settings`, issue #51).
            Command::GetSettings => Ok(Reply::Settings {
                settings: self.settings.view()?,
            }),
            // Answered with the settings as they now stand, so the window draws
            // what was saved rather than what it hoped had been.
            Command::ApplySettings(request) => Ok(Reply::Settings {
                settings: self.settings.apply(&request)?,
            }),
            // Asked of Windows each time rather than answered from a list read
            // at start-up: a microphone plugged in while the window is open is
            // one somebody is about to choose (issue #308).
            Command::GetAudioDevices => Ok(Reply::AudioDevices {
                devices: crate::settings::audio_devices()?,
            }),
            // The device is opened, listened to and closed inside this call.
            // Nothing is held between questions, so a window that is killed
            // while somebody is choosing leaves no capture behind and no
            // microphone-in-use indicator (AGENTS.md section 58,
            // `clipped_session::microphone_level`).
            Command::GetMicrophoneLevel(request) => Ok(Reply::MicrophoneLevel {
                level: crate::settings::microphone_level(&request)?,
            }),
            // One registry value, read and written by the same code the
            // `start-at-login` subcommand runs (`crate::start_at_login`, issue
            // #308). It is answered here rather than in the window because the
            // value names the executable to run, and that executable is this
            // process — a window writing a path it guessed at would leave a
            // startup entry pointing at nothing.
            Command::GetStartAtLogin => Ok(Reply::StartAtLogin {
                start_at_login: crate::start_at_login::current()?,
            }),
            // Answered with the arrangement as it now stands, read back out of
            // the registry, for the reason `apply_settings` is answered with
            // the settings as they now stand.
            Command::SetStartAtLogin(request) => Ok(Reply::StartAtLogin {
                start_at_login: crate::start_at_login::set(&request)?,
            }),
            // Also answered by `clipped-ipc` before dispatch, for the opposite
            // reason: what a shutdown ends is the accept loop, which belongs to
            // the server rather than to this service. It stops the listener,
            // `run` below then stops any recording and waits for its file, and
            // the process exits — the same path Ctrl+C takes
            // (`crates/ipc/src/server.rs`, issue #220). Reaching here would be a
            // bug in that dispatch.
            Command::Shutdown(_) => Err(ProtocolError::new(
                ErrorCode::Internal,
                "`shutdown` is answered by the protocol layer and should not have reached the \
                 recorder",
            )),
        }
    }

    fn status(&self) -> RecorderStatus {
        self.recordings.status()
    }

    /// What this build can do, plus the one capability that is a fact about
    /// *this recorder* rather than about the binary.
    ///
    /// [`features::AUTOMATIC`] says the recorder records games by itself, and
    /// since [issue #421](https://github.com/wildware-uk/clipped/issues/421)
    /// both kinds live in one binary: a plain `serve` will never record
    /// anything it was not asked for, and a `serve --watch-for-games` will
    /// record the next game to launch. A window has no other way to tell them
    /// apart before it draws a screen that says one of those two things
    /// ([issue #587](https://github.com/wildware-uk/clipped/issues/587)).
    ///
    /// It is answered from [`RecordingState::watches_for_games`] — the same
    /// claim [`RecorderStatus::Watching`] is answered from, and literally the
    /// same field — rather than from the `--watch-for-games` flag, so that a
    /// recorder asked to watch whose detection could not start does not
    /// advertise a capability its status denies.
    fn features(&self) -> Vec<String> {
        let mut features = features_of_this_build();
        if self.recordings.watches_for_games() {
            features.push(features::AUTOMATIC.to_owned());
        }
        features
    }
}

/// The one recording this process runs at a time, and the thread it runs on.
///
/// One at a time is a decision rather than a limitation of the code: a second
/// recording means a second encoder session and a second capture loop competing
/// with the game the first one is recording, and nothing in the product asks
/// for it. A second `start_recording` is refused with
/// [`ErrorCode::AlreadyRecording`] rather than queued.
#[derive(Debug)]
pub(crate) struct RecordingState {
    current: Mutex<Option<Running>>,
    /// Signalled when a recording's thread has stored its outcome.
    finished: Condvar,
    events: EventPublisher,
    next_id: AtomicU64,
    /// Asked to bring the library up to date once a recording's session record
    /// is final (`crate::library`, issue #402).
    indexer: Arc<LibraryIndexer>,
    /// The user's settings, shared with the service that can change them
    /// (`crate::settings`, issue #51).
    ///
    /// Asked when a recording starts and never while one is running: what a
    /// recording is made with belongs to the moment it started, so a setting
    /// saved half way through reaches the *next* recording rather than the
    /// encoder that is running (`clipped_session::config`, issue #61).
    settings: Arc<crate::settings::SettingsFile>,
    /// What Clipped knows about games, as it stood when this process started
    /// ([`catalogue_for_recordings`]).
    ///
    /// Asked once per recording — when [`Self::begin`] opens its session — and
    /// never per frame. Held for the same reason the settings are: the answer
    /// belongs to the moment the recording started, and a games file edited
    /// while a recording runs does not change what that recording is of.
    catalogue: Catalogue,
    /// The launchers installed on this machine, as they stood when this process
    /// started ([`launchers_for_recordings`]).
    ///
    /// Read once, for the reason the catalogue is: asking six providers costs a
    /// registry walk and six directory reads, and a recording resolves what it
    /// is of when it starts. A game installed while this recorder is running is
    /// therefore identified by the catalogue's name and path rungs until it is
    /// restarted, which is the trade
    /// [`Launchers`](clipped_game_detection::launcher::Launchers) documents.
    launchers: Launchers,
    /// What the launch watcher is doing, when this recorder has one.
    ///
    /// [`None`] is what a plain `serve` reports for ever, and it is why this is
    /// an `Option` of a `Watching` rather than a `Watching` with an empty
    /// sitting: the question `get_status` answers is whether **this** recorder
    /// will record a game on its own, not whether the build could
    /// ([issue #584](https://github.com/wildware-uk/clipped/issues/584)). A
    /// `serve --watch-for-games` whose detection could not be started answers
    /// [`None`] too, because it will not record anything either.
    ///
    /// # Locks
    ///
    /// **The inner of this state's two locks.** A thread that holds both takes
    /// [`Self::current`] first, and [`Self::status`] is the reason there is an
    /// order to keep at all: it answers about a recording and a watcher in one
    /// breath. [`WatchingForGames`] is the only writer and lets this one go
    /// before it asks for a status to publish, so nothing ever wants
    /// `current` while holding this.
    ///
    /// It is held for a clone or a store of a small value and never across a
    /// file, a capture, a lookup or an event publication (AGENTS.md section
    /// 20). The writer is the launch watcher's own thread
    /// (`crate::watch::watch_for_games`), which writes once per pass of a loop
    /// that turns over about once a second; the readers are connection threads
    /// answering `get_status`. Neither is a capture thread and neither waits on
    /// the other for longer than a `memcpy`.
    watching: Mutex<Option<Watching>>,
}

/// A recording that has been started.
///
/// It stays here after it has finished, holding its outcome, until whoever
/// stops it collects it — which is what lets `stop_recording` return the real
/// report of a recording that had already ended by itself.
#[derive(Debug)]
struct Running {
    id: String,
    output: PathBuf,
    target: String,
    started: Instant,
    stop: crate::shutdown::ShutdownSignal,
    thread: Option<JoinHandle<()>>,
    /// Where the recording has reached on its own timeline.
    ///
    /// The only honest source for a bookmark's offset: it is the media
    /// timestamp of the last frame that reached the file, which is what a
    /// bookmark has to name (`clipped_session::RecordingProgress`).
    progress: RecordingProgress,
    /// The bookmarks taken in this recording, and the file they live in.
    ///
    /// Shared so that the connection thread answering `add_bookmark` holds it
    /// without holding [`RecordingState::current`] while it writes.
    bookmarks: Arc<BookmarkLog>,
    /// Where a `take_screenshot` asks this recording for one of the frames it
    /// has already captured.
    ///
    /// Cloned rather than borrowed, for the same reason `bookmarks` is: the
    /// connection thread waits on it, and it must not be holding
    /// [`RecordingState::current`] while it does — the recording thread stores
    /// its outcome through that same mutex.
    screenshots: ScreenshotRequests,
    /// Which capture backend this recording settled on, and everything it fell
    /// past to get there.
    ///
    /// Cloned rather than borrowed, like `screenshots` and for the same reason:
    /// the recording thread writes it and connection threads read it, and
    /// neither may hold [`RecordingState::current`] to do so. It is the only
    /// route `clipped_capture::CaptureStatus` has out of the capture thread
    /// (`clipped_session::CaptureAccounting`, issue #302).
    capture: CaptureAccounting,
    /// The session this recording is the whole of, and its record on disk.
    ///
    /// [`Some`] for the whole life of the recording. It is an [`Option`] for one
    /// reason and no other: closing a session consumes it
    /// ([`ManualSession::finish`]), and the outcome that closes this one arrives
    /// while the recording is still held here.
    ///
    /// A recording started by this state cannot be made without one —
    /// [`RecordingState::begin`] builds the session before it builds this — and
    /// that is the point. Issue #402 was a `serve` that wrote files and no
    /// session record at all, so there was nothing for the library to index; the
    /// session being a field of the recording rather than a step somebody has to
    /// remember is what stops that returning.
    ///
    /// Behind a mutex of its own, and shared, because a `save_replay` arriving
    /// on a connection thread enters its clip in it: it takes this handle from
    /// under [`RecordingState::current`], lets that lock go, and only then
    /// writes anything — exactly as `add_bookmark` does with the bookmark log.
    session: Option<Arc<Mutex<ManualSession>>>,
    /// That same sitting as the protocol describes one, taken when it was
    /// opened.
    ///
    /// **Where the game a recording is of comes from on a status.**
    /// [`Running::target`] is a capture selector — `process 4242` — and a
    /// window cannot turn one into "Counter-Strike 2" without the catalogue;
    /// the session asked the catalogue when it opened, so this is that answer
    /// (`clipped_ipc::ActiveRecording::session`, issue #241).
    ///
    /// A copy rather than a view of [`Self::session`], and it cannot go stale:
    /// everything on it is fixed for the life of the recording — the
    /// identifier, the game, when the sitting started and the one file being
    /// written — and the fields that do change are the ones only a *finished*
    /// recording has, by which time the status is no longer `recording`. It is
    /// also what keeps `get_status` off that mutex, which the recording thread
    /// holds while it writes the session's record to disk (AGENTS.md section
    /// 20).
    ///
    /// [`None`] for a recording the automatic recorder handed over. That one
    /// belongs to a sitting its session manager owns, which may already hold
    /// earlier files of the same sitting and is the one on
    /// [`RecordingState::watching`]; [`status_of`] is where the two answers
    /// meet.
    sitting: Option<Box<SessionSummary>>,
    /// The rolling window of the last few minutes, when this recording was
    /// asked for one.
    ///
    /// [`None`] for an ordinary recording, and that is what `save_replay`
    /// refuses on: a buffer costs memory in proportion to its duration
    /// (`docs/replay-buffer.md`), so one is kept only when somebody asked for
    /// it. Shared for the reason the session is — a save runs on the connection
    /// thread while this one carries on recording.
    replay: Option<Arc<ReplayRecording>>,
    /// Present when the automatic recorder started this recording, and the flag
    /// its driver reads.
    ///
    /// [`RecordingState::stop`] raises it before it raises the stop signal, and
    /// the driver's loop turns it into
    /// [`SessionManager::asked_to_stop_recording`](clipped_session::automatic::SessionManager::asked_to_stop_recording).
    /// Without it the file would stop and the session would start another
    /// recording of the same game five seconds later, because the game is still
    /// running and the manager cannot tell a stop somebody asked for from a
    /// window that went (issue #421).
    ///
    /// [`None`] for a recording `start_recording` asked for: there is no
    /// session policy behind one, and the recording ending *is* the session
    /// ending.
    asked_to_stop: Option<Arc<AtomicBool>>,
    /// [`None`] while it is still recording.
    outcome: Option<Result<RecordingReport, String>>,
}

impl Running {
    /// The same recording, keeping a rolling window to save replays from.
    ///
    /// A step after construction rather than a further argument to
    /// [`RecordingState::begin`], because it is the one thing about a recording
    /// that is optional: `start` attaches a buffer when `start_recording` asked
    /// for one and does not otherwise.
    #[must_use]
    fn with_replay(mut self, replay: Option<Arc<ReplayRecording>>) -> Self {
        self.replay = replay;
        self
    }

    /// The same recording, keeping whatever [`replay_asked`] said it should.
    ///
    /// This is where a `replay` that named no length becomes a duration, and it
    /// is here rather than beside the rest of the request because *here* is the
    /// first moment the length exists: [`ManualSession::start`] has asked the
    /// catalogue what game the window is and folded that game's settings over
    /// the global ones, so `replay_window_seconds` has the answer that applies
    /// to this recording rather than the one that applies to nothing in
    /// particular (AGENTS.md section 30, `docs/configuration.md`).
    ///
    /// # Errors
    ///
    /// Only for a configured window `clipped-replay` will not take, which a
    /// `Configuration` built through its own API cannot hold — every way of
    /// setting `replay_window_seconds` checks the same bounds. It is a
    /// `Result` so that a settings file which somehow carries one is refused
    /// with the buffer's own sentence rather than silently recorded without a
    /// buffer, which is the failure this whole issue was.
    fn with_replay_asked(self, asked: ReplayAsked) -> Result<Self, ProtocolError> {
        let replay = match asked {
            ReplayAsked::Nothing => None,
            ReplayAsked::Named(replay) => Some(replay),
            ReplayAsked::Configured => {
                let window = *self.resolved_settings().replay_window().value();
                tracing::info!(
                    window_seconds = window.as_secs_f64(),
                    "this recording keeps the replay window the settings ask for, because the \
                     request asked for a buffer without naming a length"
                );
                Some(replay_of(window)?)
            }
        };

        Ok(self.with_replay(replay))
    }

    /// The session this recording is the whole of, while it is still running.
    ///
    /// # Panics
    ///
    /// If the recording has already ended: [`RecordingState::finish`] takes the
    /// session to close it, and nothing asks a recording that has stopped what
    /// it is recording into.
    ///
    /// And on a recording [`RecordingState::adopt`] handed over, which holds no
    /// session of its own — the automatic recorder's manager owns that sitting.
    /// The one caller is `save_replay`, which is reached only by a recording
    /// with a replay buffer, and an adopted recording never has one
    /// ([issue #427](https://github.com/wildware-uk/clipped/issues/427) is what
    /// would give it one, and would have to give it a session here too).
    fn session(&self) -> &Arc<Mutex<ManualSession>> {
        self.session
            .as_ref()
            .expect("a recording that has not ended still holds its session")
    }

    /// The settings this recording's session resolved, copied out of it.
    ///
    /// A copy rather than a borrow because the session is behind a mutex and
    /// this is read once, before the recording thread starts.
    fn resolved_settings(&self) -> clipped_session::config::ResolvedSettings {
        self.session()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .settings()
            .clone()
    }
}

impl RecordingState {
    fn new(
        events: EventPublisher,
        indexer: Arc<LibraryIndexer>,
        settings: Arc<crate::settings::SettingsFile>,
        catalogue: Catalogue,
        launchers: Launchers,
    ) -> Self {
        Self {
            current: Mutex::new(None),
            finished: Condvar::new(),
            events,
            next_id: AtomicU64::new(1),
            indexer,
            settings,
            catalogue,
            launchers,
            // Nothing is watching until something says it is, which is what a
            // `serve` with no `--watch-for-games` reports for its whole life.
            watching: Mutex::new(None),
        }
    }

    /// A recording of `window`, and the session record it opens.
    ///
    /// **The only way a [`Running`] is built**, in the recorder and in its tests
    /// alike, and that is the whole point of it existing. Issue #402 was a
    /// `serve` that produced files and no session record, so nothing could index
    /// them; issue #403 was a `serve` that asked nothing about the window it was
    /// pointed at, so every sitting it produced was filed under no game. Both
    /// answers are settled *here*, in the one place a recording of this state
    /// comes from, so writing either defect again means deleting a line the
    /// tests are looking at rather than forgetting one somewhere else.
    ///
    /// The thread is filled in by the caller, because what a recording is made
    /// with is resolved from the settings the session resolved, and there is
    /// nothing to spawn until that answer exists. The stop signal is made here
    /// and handed out by cloning, so a recording and the signal that ends it
    /// cannot be two different signals.
    fn begin(
        &self,
        id: String,
        output: PathBuf,
        target: String,
        window: &WindowInfo,
        // The settings this recording is made with, resolved by its caller at
        // the moment it started rather than read again here: one recording
        // reads the file once (issue #51).
        configuration: &Configuration,
        now: SystemTime,
    ) -> Running {
        // Asked of Windows here rather than carried on `WindowInfo`, which
        // describes what enumeration saw and never opens a process twice
        // (`clipped_windows::process`). It is one process handle per recording,
        // and it is what most catalogue entries need: Counter-Strike 2 is
        // `cs2.exe` *in the directory Steam installs it into*, and an entry
        // qualified by a path cannot match a process nobody could locate
        // (`clipped_game_detection::catalogue::matching`).
        let image_path = clipped_windows::process_image_path(window.process_id())
            .map(|path| path.to_string_lossy().into_owned());
        let process = RecordedProcess::new(
            window.process_id(),
            window.process_name().unwrap_or_default(),
        )
        .with_image_path(image_path.as_deref());

        // The directory is the recording's own, which is what "beside its
        // recordings" means for an output the caller may have named itself.
        let session = ManualSession::start(
            output.parent().unwrap_or_else(|| Path::new(".")),
            output.clone(),
            configuration,
            &self.catalogue,
            &self.launchers,
            process,
            now,
        );

        // Taken here, from the session that has just asked the catalogue what
        // this window is, so that the status can name the game without asking
        // anything twice and without reaching into the session while a
        // recording thread is writing to it.
        let sitting = Box::new(crate::watch::sitting_of(session.session()));

        Running {
            id,
            bookmarks: Arc::new(BookmarkLog::for_recording(&output)),
            output,
            target,
            started: Instant::now(),
            stop: crate::shutdown::ShutdownSignal::new(),
            thread: None,
            progress: RecordingProgress::new(),
            screenshots: ScreenshotRequests::new(),
            capture: CaptureAccounting::new(),
            session: Some(Arc::new(Mutex::new(session))),
            sitting: Some(sitting),
            // Attached afterwards by [`Running::with_replay`], because it is the
            // one thing about a recording that is optional.
            replay: None,
            // Nothing to tell: this recording is the whole of its session.
            asked_to_stop: None,
            outcome: None,
        }
    }

    /// Makes a recording the automatic recorder started reachable over the
    /// protocol, for as long as it runs.
    ///
    /// **This is the whole of issue #421.** `serve` answers `add_bookmark`,
    /// `take_screenshot`, `stop_recording` and `get_status` against
    /// [`Self::current`], and until now the only recordings in there were the
    /// ones `start_recording` asked for — so the recordings a user is most
    /// likely to want to bookmark, the ones nobody had to start, were the ones
    /// nothing could bookmark. Handing one over here is what a press of
    /// `Ctrl`+`F9` reaches, through exactly the same [`Self::bookmark`] a button
    /// reaches: there is one implementation of what a bookmark is, and it cannot
    /// drift between the two ways of starting a recording (AGENTS.md section
    /// 55).
    ///
    /// What is deliberately *not* handed over is the session record. An
    /// automatic recording belongs to a sitting the
    /// [`SessionManager`](clipped_session::automatic::SessionManager) on the
    /// driver's thread owns — it may be the second file of one, and it is that
    /// manager that writes the sidecar — so [`Running::session`] is [`None`]
    /// here and this state closes nothing. A recording started from the window
    /// is the whole of its own session, which is why that one carries a
    /// [`ManualSession`].
    ///
    /// # Errors
    ///
    /// The sentence for a recorder that is already recording. One at a time is
    /// this process's rule whoever asked for the recording, and a game launching
    /// while the user is recording something by hand does not take the encoder
    /// away from them.
    pub(crate) fn adopt(
        self: &Arc<Self>,
        output: &Path,
        target: String,
        progress: &RecordingProgress,
        stop: &crate::shutdown::ShutdownSignal,
        asked_to_stop: &Arc<AtomicBool>,
    ) -> Result<Adopted, String> {
        let mut current = self
            .current
            .lock()
            .map_err(|_| "the recording state was poisoned by an earlier panic".to_owned())?;
        if let Some(running) = current.as_ref().filter(|running| running.outcome.is_none()) {
            return Err(format!(
                "this recorder is already recording {}, and it records one thing at a time",
                running.target
            ));
        }

        let id = format!("r-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let screenshots = ScreenshotRequests::new();
        let capture = CaptureAccounting::new();
        *current = Some(Running {
            id: id.clone(),
            bookmarks: Arc::new(BookmarkLog::for_recording(output)),
            output: output.to_path_buf(),
            target,
            started: Instant::now(),
            stop: stop.clone(),
            // The driver owns it, and joins it itself: what it gets back is a
            // `RecordingOutcome` its session manager needs, which is not
            // something this state has any use for.
            thread: None,
            progress: progress.clone(),
            screenshots: screenshots.clone(),
            capture: capture.clone(),
            session: None,
            // And no copy of one either: the sitting this recording belongs to
            // is the driver's, reported on to `watching` once a pass, and it is
            // the one `status_of` puts on the status. Taking a copy here would
            // be a second answer that stopped changing the moment the recording
            // started — which is the moment the sitting gains this file.
            sitting: None,
            // No buffer, so `save_replay` refuses an automatic recording in the
            // recorder's own words.
            // [Issue #427](https://github.com/wildware-uk/clipped/issues/427)
            // gave the recordings the *window* starts one, through
            // [`StartRecording::replay`](clipped_ipc::StartRecording::replay);
            // an automatic recording is one nobody asked for, and whether it
            // should spend roughly 140 MiB a minute on a buffer nobody asked
            // for is a decision about memory that issue did not make.
            replay: None,
            asked_to_stop: Some(Arc::clone(asked_to_stop)),
            outcome: None,
        });
        // The watcher's state read under `current`, which is the order every
        // thread that takes both of this state's locks uses. It cannot change
        // the answer here — a recording has just been stored, and a recorder
        // that is recording is recording — but reading it is what keeps
        // [`status_of`] the one place that decides.
        let status = status_of(current.as_ref(), self.watching_now());
        drop(current);

        tracing::info!(
            recording = id,
            output = %RedactedPath::new(output),
            "a recording detection started can be reached over the protocol"
        );
        self.events
            .publish(&clipped_ipc::Event::StatusChanged { status });

        Ok(Adopted {
            state: Arc::clone(self),
            id,
            screenshots,
            capture,
            released: false,
        })
    }

    /// Reports that this recorder is watching for games, until the guard is
    /// dropped.
    ///
    /// **This is the producer of [`RecorderStatus::Watching`]**, and the whole
    /// of [issue #584](https://github.com/wildware-uk/clipped/issues/584). The
    /// state, the tray's rendering of it and the hotkey's refusal in it were all
    /// built on top of a recorder that could never enter it, so a recorder
    /// waiting for a game to launch answered `idle` — the same word as one that
    /// will never record anything, which is exactly what
    /// [issue #241](https://github.com/wildware-uk/clipped/issues/241) added the
    /// state to stop.
    ///
    /// It is claimed by [`crate::watch::AutomaticRecorder::start`] the moment
    /// it has somewhere to record to and a thread to watch on — before the
    /// ready line, so a window connecting the instant it sees one is told what
    /// this recorder is rather than told `idle` and corrected a moment later.
    /// Detection that then fails to start drops the guard, and the recorder goes
    /// back to answering `idle`, because it will not record anything either
    /// (`crate::watch::watch_for_games`).
    pub(crate) fn watch_for_games(self: &Arc<Self>) -> WatchingForGames {
        self.watching_is(Some(Watching::default()));
        tracing::info!("this recorder is watching for games, and says so when it is asked");
        WatchingForGames {
            state: Arc::clone(self),
        }
    }

    /// The sitting the launch watcher is holding, or [`None`] for one watching
    /// for anything at all.
    ///
    /// A sitting outlives the recording it is made of: a game that exits keeps
    /// its sitting open for the restart grace, so that the same game launching
    /// again rejoins it (`docs/sessions.md`). Carrying it here is what stops a
    /// window blanking the game's name for those few seconds and then filling it
    /// in again — the flicker [`Watching::session`] exists to prevent.
    ///
    /// **Nothing is invented for a watcher with no sitting.** An absent sitting
    /// is absent from the wire, so a recorder waiting for its first game of the
    /// day is `{"state":"watching"}` and nothing more (`docs/ipc.md`).
    ///
    /// Ignored when nothing is watching, which is a driver reporting after its
    /// guard has gone rather than something to complain about.
    pub(crate) fn sitting_is(&self, session: Option<Box<SessionSummary>>) {
        let changed = {
            let mut watching = self.watching_lock();
            match watching.as_mut() {
                None => false,
                Some(held) if held.session == session => false,
                Some(held) => {
                    held.session = session;
                    true
                }
            }
        };

        if changed {
            self.publish_what_the_watcher_changed();
        }
    }

    /// Says a sitting is over, with the files it produced.
    ///
    /// **This is the producer of
    /// [`Event::SessionEnded`](clipped_ipc::Event::SessionEnded)**, and the
    /// second acceptance criterion of
    /// [issue #241](https://github.com/wildware-uk/clipped/issues/241). The
    /// event was defined, carried by the schema and parsed by the desktop's
    /// TypeScript, and nothing ever sent one.
    ///
    /// It carries the sitting rather than an identifier because the files are
    /// the point: a window is told a sitting is over at the moment it can offer
    /// to open it, and the library has not necessarily indexed any of it yet
    /// (`docs/library.md`). Both kinds of sitting end through here — the one a
    /// `start_recording` was the whole of, closed by [`Self::finish`], and the
    /// one a driver's session manager owns, closed by that manager
    /// (`crate::watch`) — so there is one thing a client subscribes to rather
    /// than one per way of starting a recording (AGENTS.md section 55).
    ///
    /// The status is left to the caller. A driver's sitting is on
    /// [`Self::watching`] and comes off it through [`Self::sitting_is`]; a
    /// `start_recording`'s sitting is part of the recording and goes when the
    /// recording does, which [`Self::finish`] already publishes.
    pub(crate) fn sitting_ended(&self, session: SessionSummary) {
        tracing::info!(
            session = session.session_id,
            recordings = session.recordings.len(),
            "telling every subscriber that a sitting ended"
        );
        self.events
            .publish(&clipped_ipc::Event::SessionEnded { session });
    }

    /// Stores what the watcher is doing and tells every subscriber, if it moved.
    fn watching_is(&self, watching: Option<Watching>) {
        let changed = {
            let mut held = self.watching_lock();
            let changed = *held != watching;
            *held = watching;
            changed
        };

        if changed {
            self.publish_what_the_watcher_changed();
        }
    }

    /// Tells every subscriber what this recorder is doing now.
    ///
    /// Called with no lock of this state held, because [`Self::status`] takes
    /// both of them.
    ///
    /// **Published while a recording is running as well.** Issue #584 kept
    /// quiet then, and was right to: the sitting was invisible under a
    /// `recording` status, so every one of these would have been an identical
    /// event. It is not invisible any more — a recording carries the sitting it
    /// belongs to (issue #241) — so the sitting gaining this recording's own
    /// file is a real change to what a window is drawing, and a subscriber told
    /// nothing would show "the first file of this sitting" for as long as the
    /// second one took to record.
    ///
    /// It is still not a stream. Both callers compare before they publish, so
    /// this is reached only when the stored value actually moved, and a driver
    /// reporting the same sitting once a second reaches it once.
    fn publish_what_the_watcher_changed(&self) {
        self.events.publish(&clipped_ipc::Event::StatusChanged {
            status: self.status(),
        });
    }

    /// How the recording in progress is capturing, or [`None`] when there is
    /// none.
    ///
    /// Two ways to get [`None`], and both are the honest answer rather than a
    /// gap: nothing is being recorded, so there is no capture backend running at
    /// all; or a recording has started and has not opened its backend yet, which
    /// is a few milliseconds and is when "not chosen yet" is true
    /// (`clipped_session::CaptureAccounting`, issue #302).
    ///
    /// A clone of a small value out of the lock, released before the caller does
    /// anything with it, which is the discipline every reader of this state
    /// keeps (AGENTS.md section 20).
    fn capture_account(&self) -> Option<clipped_session::CaptureAccount> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|running| running.outcome.is_none())
            .and_then(|running| running.capture.account())
    }

    /// What the watcher is doing, as [`status_of`] needs it.
    ///
    /// Read through a poisoned lock for the reason [`Self::status`] is: what is
    /// behind it is an owned value a panic cannot leave half written, and the
    /// worst a poisoned read can be is out of date.
    fn watching_now(&self) -> Option<Watching> {
        self.watching_lock().clone()
    }

    /// Whether this recorder will record a game by itself.
    ///
    /// **The same claim [`RecorderStatus::Watching`] is answered from** — the
    /// same field, read through the same lock — rather than a second flag
    /// beside it, so that a recorder cannot advertise
    /// [`features::AUTOMATIC`] and then report a status that says it will never
    /// record anything ([issue #587](https://github.com/wildware-uk/clipped/issues/587)).
    ///
    /// Deliberately **not** `matches!(self.status(), Watching(_))`. A recorder
    /// that is watching *and* recording reports `recording`, because that is
    /// the thing a window has to be able to see and stop ([`status_of`]) — and
    /// it goes on watching throughout. Reading the status here would make the
    /// capability appear and disappear with every recording, which is a window
    /// watching a control it drew from the welcome stop applying to the
    /// recorder it drew it for.
    pub(crate) fn watches_for_games(&self) -> bool {
        self.watching_lock().is_some()
    }

    fn watching_lock(&self) -> MutexGuard<'_, Option<Watching>> {
        self.watching
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Asks for the library index to be brought up to date, on its own thread.
    ///
    /// The automatic recorder calls it when a session ends, because the session
    /// record it has just written is what the index reads. A recording
    /// `start_recording` made asks for the same run from [`Self::finish`], where
    /// this state is what closed the session; an automatic session is closed by
    /// its manager, so there is nowhere else this could be asked from
    /// (`crate::library`, issue #402).
    pub(crate) fn index_now(&self) {
        self.indexer.request();
    }

    /// What Clipped knows about games, as it stood when this process started.
    pub(crate) fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }

    /// The settings in force, from the one state this process keeps them in.
    ///
    /// For the automatic recorder, which resolves each game's settings through
    /// a session manager of its own (`crate::watch`, issue #421). It reads them
    /// from here rather than opening the settings file a second time, so that
    /// "what did the user configure" has one answer in this process however a
    /// recording was started — the same reason a recording the window asked for
    /// reads them from here (AGENTS.md sections 30 and 55, issue #51).
    pub(crate) fn configuration(&self) -> Configuration {
        self.settings.configuration()
    }

    /// Validates the request, resolves the target, opens a session and starts
    /// recording.
    fn start(self: &Arc<Self>, request: &StartRecording) -> Result<Reply, ProtocolError> {
        let args = record_args(request)?;
        // Read once, here, and from the state `apply_settings` writes rather
        // than from the file: everything this recording is made with — where it
        // goes, and what it is made at — comes from the settings as they stand
        // at the moment it starts, and nothing re-reads them afterwards
        // (issue #61).
        //
        // The *one* state, not a start-up snapshot and not a second read of the
        // file. A recording the window started goes where the settings screen
        // said (issue #307), and it has to be the answer the screen last saved
        // rather than the answer the file held when this process started, or
        // the screen is a control that does nothing until the recorder is
        // restarted (`crate::settings`, issue #51).
        let configuration = self.settings.configuration();
        let config = RecordingConfig::resolve(&args, configuration.storage().recording_directory())
            .map_err(invalid_parameters)?;
        // Before the window is resolved and before anything is created: a
        // duration no buffer can hold is a parameter to fix, and finding that
        // out after a capture session has opened would be finding it out late
        // (AGENTS.md section 45). A `replay` that named no length is only
        // *recognised* here — the length it means is the one the session
        // resolves, below.
        let asked_replay = replay_asked(request)?;
        let window = resolve_window(&config.target).map_err(unrecordable_target)?;
        let asked_for = settings_for(&config, &window);

        let mut current = self.lock()?;
        if current
            .as_ref()
            .is_some_and(|running| running.outcome.is_none())
        {
            return Err(ProtocolError::new(
                ErrorCode::AlreadyRecording,
                "this recorder records one thing at a time, and it is already recording",
            ));
        }

        let id = format!("r-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        // `serve` starts recordings and never a buffered capture: every
        // `start_recording` names an output, and `--no-recording` is a
        // `clipped-recorder replay` argument (ADR 0018). Should the protocol
        // ever ask for one, `recording_started` and `recording_stopped` need a
        // shape for a sitting with no file rather than an empty string here.
        let output = asked_for
            .output()
            .expect("a recording started over the protocol always names a file")
            .to_path_buf();
        let target = config.target.to_string();

        tracing::info!(
            recording = id,
            target = target,
            output = %RedactedPath::new(&output),
            "starting a recording because the desktop application asked for one"
        );

        // The session opens with the recording and before the encoder, so that
        // a recorder killed during this recording still leaves something saying
        // what the file beside it is (AGENTS.md section 17). `begin` is the only
        // place a recording of this state is built, which is what makes that —
        // and asking the catalogue what the window is — unforgettable rather
        // than remembered.
        let mut running = self
            .begin(
                id.clone(),
                output.clone(),
                target,
                &window,
                &configuration,
                SystemTime::now(),
            )
            // After the session, because the length of a buffer nobody named is
            // `replay_window_seconds` folded for the game the catalogue just
            // made of this window (issue #427).
            .with_replay_asked(asked_replay)?;

        // What the request asked for, then what the user configured laid over
        // it — `apply_configured_to` and not `apply_to`, for the reason `watch`
        // gives at the same call: `apply_to` would put the shipped default over
        // every parameter the request named, so a `start_recording` asking for
        // 144 frames per second would record at 60 on every machine with no
        // settings file. Two callers, one rule (AGENTS.md section 55).
        let settings = running.resolved_settings().apply_configured_to(asked_for);

        running.thread = Some(spawn_recording(
            self,
            &id,
            settings,
            running.stop.clone(),
            RecordingChannels {
                progress: running.progress.clone(),
                screenshots: running.screenshots.clone(),
                capture: running.capture.clone(),
                replay: running.replay.clone(),
            },
        ));

        *current = Some(running);
        // The watcher's state read under `current`, which is the order every
        // thread that takes both of this state's locks uses. It cannot change
        // the answer here — a recording has just been stored, and a recorder
        // that is recording is recording — but reading it is what keeps
        // [`status_of`] the one place that decides.
        let status = status_of(current.as_ref(), self.watching_now());
        drop(current);

        self.events
            .publish(&clipped_ipc::Event::StatusChanged { status });

        Ok(Reply::RecordingStarted {
            recording_id: id,
            output: output.to_string_lossy().into_owned(),
        })
    }

    /// Stops the recording and waits for its file to be finished.
    ///
    /// `id` names the recording to stop. [`None`] means whatever is running,
    /// which is what a tray menu wants; naming one is what a window that had a
    /// particular recording on screen does, so that a recording which ended by
    /// itself in the meantime cannot have its successor stopped instead.
    fn stop(&self, id: Option<&str>) -> Result<RecordingSummary, ProtocolError> {
        let stop = {
            let current = self.lock()?;
            let Some(running) = current.as_ref() else {
                return Err(nothing_to_stop());
            };
            if let Some(wanted) = id {
                if wanted != running.id {
                    return Err(ProtocolError::new(
                        ErrorCode::NotRecording,
                        format!("recording `{wanted}` is not the one this recorder is running"),
                    ));
                }
            }
            // Before the signal is raised, and deliberately: the driver reads
            // this flag once round its loop *before* it collects a finished
            // recording, so a stop that is seen at all is seen before the
            // outcome it produced. Raising the signal first would let the
            // recording end, be collected and be followed by another one of the
            // same game — which is the stop undoing itself (issue #421).
            if let Some(asked_to_stop) = &running.asked_to_stop {
                asked_to_stop.store(true, Ordering::SeqCst);
            }
            running.stop.clone()
        };

        // Polled by the capture loop between frames, so the recording stops at
        // a frame boundary and the session flushes the encoder and closes the
        // container before its thread ends.
        stop.request();

        let mut current = self.lock()?;
        loop {
            match current.as_ref() {
                None => return Err(nothing_to_stop()),
                Some(running) if running.outcome.is_some() => break,
                Some(_) => {
                    current = self
                        .finished
                        .wait(current)
                        .map_err(|_| poisoned("waiting for the recording to finish"))?;
                }
            }
        }

        let mut running = current.take().expect("the loop only leaves through a Some");
        drop(current);

        if let Some(thread) = running.thread.take() {
            let _ = thread.join();
        }

        match running
            .outcome
            .expect("the loop only leaves once an outcome is stored")
        {
            Ok(report) => Ok(summarise(&report)),
            Err(message) => Err(ProtocolError::new(ErrorCode::RecordingFailed, message)),
        }
    }

    /// Marks the moment the recording has reached, and writes it down.
    ///
    /// `now` is the wall clock, passed in so that this is testable without one
    /// (AGENTS.md section 25). It is only what the bookmark is *stamped* with;
    /// where the bookmark lands comes from the recording's own clock.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotRecording`] when nothing is being recorded, or when a
    /// named recording is not the one running;
    /// [`ErrorCode::InvalidParameters`] for a label, colour, duration or lead
    /// the recorder will not store; and [`ErrorCode::Internal`] when the
    /// bookmark was taken and could not be saved — which names the file, because
    /// a full or disconnected drive is something only the user can fix
    /// (AGENTS.md section 45).
    fn bookmark(
        &self,
        request: &AddBookmark,
        now: SystemTime,
    ) -> Result<BookmarkSummary, ProtocolError> {
        let wanted = bookmark_request(request)?;

        // Everything this needs comes out of the lock here, and the lock is
        // released before the file is written: the recording thread stores its
        // outcome through this same mutex, and a bookmark that held it across a
        // disk write would make a recording's end wait on one.
        let (recording_id, progress, bookmarks) = {
            let current = self.lock()?;
            let Some(running) = current.as_ref().filter(|running| running.outcome.is_none()) else {
                return Err(nothing_to_bookmark());
            };
            if let Some(named) = &request.recording_id {
                if named != &running.id {
                    return Err(ProtocolError::new(
                        ErrorCode::NotRecording,
                        format!("recording `{named}` is not the one this recorder is running"),
                    ));
                }
            }
            (
                running.id.clone(),
                running.progress.clone(),
                Arc::clone(&running.bookmarks),
            )
        };

        let Some(position) = progress.position() else {
            // The recording has been started and has not yet put a frame in its
            // file — the encoder is still opening, or the window has not drawn.
            // There is no moment to mark, and marking zero would put the
            // bookmark at a place the user was not looking at.
            return Err(ProtocolError::new(
                ErrorCode::NotRecording,
                "the recording has not captured its first frame yet, so there is no moment to \
                 mark",
            ));
        };

        let taken = bookmarks
            .add(&wanted, position, now)
            .map_err(|error| bookmark_not_saved(&error))?;

        tracing::info!(
            recording = recording_id,
            at_seconds = taken.at().as_secs_f64(),
            lead_seconds = taken.lead().as_secs_f64(),
            labelled = taken.label().is_some(),
            bookmarks = bookmarks.count(),
            "a moment was bookmarked in the recording"
        );

        Ok(BookmarkSummary {
            recording_id,
            at_seconds: taken.at().as_secs_f64(),
            pressed_at_seconds: position.as_secs_f64(),
            lead_seconds: taken.lead().as_secs_f64(),
            label: taken.label().map(str::to_owned),
            colour: taken.colour().map(str::to_owned),
            duration_seconds: taken.duration().map(|span| span.as_secs_f64()),
            bookmarks_file: bookmarks.path().to_string_lossy().into_owned(),
            bookmarks_in_recording: u32::try_from(bookmarks.count()).unwrap_or(u32::MAX),
        })
    }

    /// Saves the last few seconds of the recording's replay buffer as a clip.
    ///
    /// Everything it needs comes out of [`Self::current`] here, and the lock is
    /// released before the clip is written: the recording thread stores its
    /// outcome through that same mutex, and a save that held it across a file
    /// write would make a recording's end wait on one — the rule `add_bookmark`
    /// and `take_screenshot` already follow.
    ///
    /// `now` is the wall clock, passed in so that this is testable without one
    /// (AGENTS.md section 25). It is what the clip is stamped with in the
    /// session's record; where the clip *begins and ends* comes from the
    /// recording's own timeline.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotRecording`] when nothing is being recorded, when a named
    /// recording is not the one running, or when the recording that is running
    /// keeps no replay buffer — which is a different sentence, because the
    /// answer to it is to start one that does;
    /// [`ErrorCode::InvalidParameters`] for a duration that is not a number of
    /// seconds; and [`ErrorCode::Internal`] when the clip could not be written,
    /// which names the file, because a full drive or a name already taken is
    /// something only the user can fix (AGENTS.md section 45).
    fn save_replay(
        &self,
        request: &SaveReplay,
        now: SystemTime,
    ) -> Result<ReplaySummary, ProtocolError> {
        let destination = destination_for(request)?;
        // Both parameters are read before the recording state is touched, for
        // the reason `take_screenshot` reads its format first: what the caller
        // got wrong is the caller's to fix, and it is the same answer whether
        // anything is being recorded or not.
        let asked_for = requested_length(request)?;

        let (recording_id, replay, session) = {
            let current = self.lock()?;
            let Some(running) = current.as_ref().filter(|running| running.outcome.is_none()) else {
                return Err(nothing_to_save());
            };
            if let Some(named) = &request.recording_id {
                if named != &running.id {
                    return Err(ProtocolError::new(
                        ErrorCode::NotRecording,
                        format!("recording `{named}` is not the one this recorder is running"),
                    ));
                }
            }
            let Some(replay) = running.replay.clone() else {
                return Err(ProtocolError::new(
                    ErrorCode::NotRecording,
                    "this recording is not keeping a replay buffer; start one with `replay`, or \
                     with `replay_seconds` to choose the length, to be able to save from it",
                ));
            };
            (running.id.clone(), replay, Arc::clone(running.session()))
        };

        // Nothing asked for means the whole window the recording was started
        // with, which is what a hotkey press means.
        let keep = asked_for.unwrap_or_else(|| replay.window());

        let saved = crate::replay::save(&replay, &session, keep, destination, now)
            .map_err(replay_not_saved)?;
        let clip = &saved.clip;

        tracing::info!(
            recording = recording_id,
            path = %RedactedPath::new(clip.path()),
            seconds = clip.duration().as_secs_f64(),
            requested_seconds = clip.requested_length().as_secs_f64(),
            complete = clip.is_complete(),
            "a replay clip was saved because the desktop application asked for one"
        );

        Ok(ReplaySummary {
            path: clip.path().to_string_lossy().into_owned(),
            recording_id,
            requested_seconds: clip.requested_length().as_secs_f64(),
            duration_seconds: clip.duration().as_secs_f64(),
            source_start_seconds: clip.covered().start().as_secs_f64(),
            source_end_seconds: clip.covered().end().as_secs_f64(),
            leading_slack_seconds: clip.leading_slack().as_secs_f64(),
            complete: clip.is_complete(),
            shortfall_seconds: clip.shortfall().as_secs_f64(),
            bytes: clip.byte_len(),
        })
    }

    /// Saves a still image of what is being captured.
    ///
    /// Two paths, and which one is taken is not the caller's choice:
    ///
    /// - **A recording is running.** The picture comes from a frame that
    ///   recording already captured. It costs the capture thread one texture
    ///   copy and cannot interrupt the recording
    ///   (`clipped_session::screenshot`).
    /// - **Nothing is running.** A capture is opened for the target the request
    ///   names, one frame is taken and it is shut down. Far more expensive, and
    ///   the reason the request carries a target at all.
    ///
    /// Encoding and writing happen on this thread — the connection thread the
    /// command arrived on — and never on a capture thread (AGENTS.md section
    /// 20). `now` is passed in so that the file's name is testable without a
    /// wall clock (AGENTS.md section 25).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] for a format this build will not write
    /// or a target that cannot be parsed; [`ErrorCode::NotRecording`] when a
    /// named recording is not the one running; [`ErrorCode::TargetNotFound`]
    /// when nothing is being recorded and the request names no window that
    /// exists; and [`ErrorCode::Internal`] when the picture was taken and could
    /// not be saved — which names the file, because a full or disconnected
    /// drive is something only the user can fix (AGENTS.md section 45).
    fn screenshot(
        &self,
        request: &TakeScreenshot,
        now: SystemTime,
    ) -> Result<ScreenshotSummary, ProtocolError> {
        let settings = screenshot_settings(request)?;

        // Everything this needs comes out of the lock here, and the lock is
        // released before anything waits or writes: the recording thread stores
        // its outcome through this same mutex, and a screenshot that held it
        // across a frame wait would make a recording's end wait on one.
        let running = {
            let current = self.lock()?;
            match current.as_ref().filter(|running| running.outcome.is_none()) {
                Some(running) => {
                    if let Some(named) = &request.recording_id {
                        if named != &running.id {
                            return Err(ProtocolError::new(
                                ErrorCode::NotRecording,
                                format!(
                                    "recording `{named}` is not the one this recorder is running"
                                ),
                            ));
                        }
                    }
                    Some((running.id.clone(), running.screenshots.clone()))
                }
                None => {
                    if let Some(named) = &request.recording_id {
                        return Err(ProtocolError::new(
                            ErrorCode::NotRecording,
                            format!("recording `{named}` is not the one this recorder is running"),
                        ));
                    }
                    None
                }
            }
        };

        let (recording_id, still, position) = match running {
            Some((id, requests)) => {
                let served = requests.take().map_err(screenshot_failed)?;
                (Some(id), served.still, served.position)
            }
            None => (None, self.photograph_target(request)?, None),
        };

        let screenshot = clipped_session::screenshot::write(
            &still, &settings,
            // The game a screenshot belongs to is the session's, and a session
            // `serve` opened now has one where the catalogue claims the window
            // (issue #403). Reading it here is deliberately *not* part of that
            // ticket: a screenshot taken with nothing recording has no session
            // at all, so filing screenshots by game is a decision about both
            // cases rather than about this one, and it is issue #334. What
            // changed is that #334 now has something to read (AGENTS.md
            // section 40).
            "", now, position,
        )
        .map_err(screenshot_failed)?;

        Ok(summarise_screenshot(&screenshot, recording_id))
    }

    /// Opens a capture of the target the request names, for one frame.
    ///
    /// Only reached when nothing is being recorded.
    fn photograph_target(&self, request: &TakeScreenshot) -> Result<StillFrame, ProtocolError> {
        let target = screenshot_target(request)?;
        clipped_session::screenshot::capture_still(&target).map_err(screenshot_failed)
    }

    /// What the recorder is doing.
    ///
    /// Two questions in one answer — what is being recorded, and whether
    /// anything is watching for a game to record — which is why it takes both
    /// of this state's locks, and takes them in that order. Nothing else takes
    /// them the other way round (see [`Self::watching`]).
    ///
    /// Deliberately reads through a poisoned lock. A panic while the state was
    /// held does not stop the recording thread — `clipped_session::record` is
    /// running on a thread of its own and the file is still growing — so
    /// answering "idle" would be telling the UI that nothing is being recorded
    /// while a recording continues, which is the failure AGENTS.md sections 15
    /// and 54 are about. Nothing here is left half-written by a panic in the
    /// Rust sense: the state is an `Option<Running>` of owned values, so the
    /// worst a poisoned read can be is out of date, and a stale answer about a
    /// real recording beats a confident answer about a fictional idle one.
    fn status(&self) -> RecorderStatus {
        let current = self.current.lock().unwrap_or_else(|poisoned| {
            tracing::warn!(
                "the recording state was poisoned by an earlier panic; reporting what it holds"
            );
            poisoned.into_inner()
        });
        let watching = self.watching_now();
        status_of(current.as_ref(), watching)
    }

    /// Records a thread's outcome and tells every subscriber what changed.
    ///
    /// Called from the recording thread, so it does the least it can: stores
    /// the outcome, wakes anybody waiting for it, and publishes. The report
    /// stays in [`Running`] until somebody collects it, because a recording that
    /// ended by itself is still a recording whose figures `stop_recording`
    /// should be able to return.
    fn finish(&self, id: &str, outcome: Result<RecordingReport, String>) {
        let failure = outcome.as_ref().err().cloned();
        // Built before the lock is taken, because it is the only part of
        // closing the session that costs anything: what happens under the lock
        // is a `take` and a store, exactly as it was before a session existed.
        let for_the_session = session_outcome(&outcome);
        let mut ending = None;

        match self.current.lock() {
            Ok(mut current) => {
                if let Some(running) = current.as_mut() {
                    if running.id == id {
                        ending = running.session.take();
                        running.outcome = Some(outcome);
                    }
                }
            }
            Err(_) => tracing::error!(
                recording = id,
                "the recording state was poisoned, so this recording's outcome was lost"
            ),
        }
        self.finished.notify_all();

        if let Some(session) = ending {
            // The sidecar is written here, with the lock released, for the
            // reason `add_bookmark` releases it before writing one: this state
            // is what a bookmark, a screenshot and `get_status` all take, and
            // holding it across a file write would make each of them wait on a
            // disk (AGENTS.md section 20).
            //
            // Nothing depends on this finishing before `stop_recording` is
            // answered. What makes the recording *findable* is the index, and
            // the run that fills it is asked for below — after the record it
            // reads is on disk, which is the ordering that does matter.
            //
            // Through the mutex rather than by taking the session out of it: a
            // `save_replay` that is still writing a clip holds this lock, and
            // waiting for it is what stops the session's last write racing the
            // clip's entry in it (`ManualSession::finish_in_place`).
            let sitting = {
                let mut session = session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                crate::watch::sitting_of(
                    session.finish_in_place(&for_the_session, SystemTime::now()),
                )
            };
            let ended = sitting.session_id.clone();

            // Said before the index run is asked for, because the event carries
            // the sitting's files precisely so that a window need not wait for
            // one: the recording is on disk and playable now, and the row that
            // will describe it is minutes of walking away on a full folder
            // (`clipped_ipc::Event::SessionEnded`, issue #241). A sitting a
            // `start_recording` was the whole of ends here; an automatic one
            // ends in its manager, and says so from there (`crate::watch`).
            self.sitting_ended(sitting);

            // Off this thread and on to the indexer's: this is the recording
            // thread, and walking the recordings folder here would hold up the
            // reply to whoever asked for the stop.
            tracing::info!(
                recording = id,
                session = ended,
                "asking for the library to be brought up to date, because a session ended"
            );
            self.indexer.request();
        }

        if let Some(message) = failure {
            tracing::error!(recording = id, message, "a recording ended in a failure");
            self.events.publish(&clipped_ipc::Event::RecordingFailed {
                recording_id: id.to_owned(),
                error: ProtocolError::new(ErrorCode::RecordingFailed, message),
            });
        }

        self.events.publish(&clipped_ipc::Event::StatusChanged {
            status: self.status(),
        });
    }

    /// The recording state, or an error a client can be told about.
    fn lock(&self) -> Result<MutexGuard<'_, Option<Running>>, ProtocolError> {
        self.current
            .lock()
            .map_err(|_| poisoned("reading the recording state"))
    }
}

/// This recorder's claim to be watching for games, for as long as it is
/// watching.
///
/// A guard rather than a pair of calls, for the reason [`Adopted`] is one: the
/// clearing half is what must not be forgotten. A recorder left claiming to be
/// watching after its watcher thread has gone — because detection stopped,
/// because the loop returned, or because it panicked — would tell every window
/// that the next game to launch will be recorded, and nothing would record it
/// (AGENTS.md sections 27 and 54).
#[derive(Debug)]
pub(crate) struct WatchingForGames {
    state: Arc<RecordingState>,
}

impl Drop for WatchingForGames {
    fn drop(&mut self) {
        self.state.watching_is(None);
    }
}

/// A recording the automatic recorder handed over, for as long as it runs.
///
/// Handing it back is what takes it out of the recorder's status and stores its
/// outcome, and it happens on every path out of a recording including a panic:
/// [`Drop`] releases one that was never released deliberately. A recording left
/// in [`RecordingState::current`] with no outcome would leave `stop_recording`
/// waiting for one for ever, which would cost the user their ability to stop the
/// recorder (AGENTS.md section 17).
#[derive(Debug)]
pub(crate) struct Adopted {
    state: Arc<RecordingState>,
    id: String,
    screenshots: ScreenshotRequests,
    capture: CaptureAccounting,
    released: bool,
}

impl Adopted {
    /// Where a `take_screenshot` asks this recording for a frame it has already
    /// captured.
    ///
    /// Handed to [`clipped_session::RecordingOutputs`] by the driver, which is
    /// what makes the still come from the recording rather than from a second
    /// capture of the same window (`RecordingState::screenshot`).
    pub(crate) fn screenshots(&self) -> &ScreenshotRequests {
        &self.screenshots
    }

    /// Where this recording says which capture backend it is using.
    ///
    /// Handed to [`clipped_session::RecordingOutputs`] by the driver, like the
    /// screenshots above: a recording detection started has to reach the
    /// Diagnostics screen the same way one the window started does, or the
    /// answer would depend on who asked for the recording (issue #302).
    pub(crate) fn capture(&self) -> &CaptureAccounting {
        &self.capture
    }

    /// Hands the recording back with what became of it.
    pub(crate) fn finished(mut self, outcome: &RecordingOutcome) {
        self.release(report_of(outcome));
    }

    fn release(&mut self, outcome: Result<RecordingReport, String>) {
        if self.released {
            return;
        }
        self.released = true;
        self.state.finish(&self.id, outcome);
    }
}

impl Drop for Adopted {
    fn drop(&mut self) {
        self.release(Err(
            "the automatic recording ended without reporting an outcome".to_owned(),
        ));
    }
}

/// What a recording turned out to be, in the vocabulary this state keeps.
///
/// The inverse of [`session_outcome`], which is what the session below is told.
/// There are two vocabularies because there are two questions: a session asks
/// what the sitting got, and this asks what to answer the client that is waiting
/// on `stop_recording` with.
fn report_of(outcome: &RecordingOutcome) -> Result<RecordingReport, String> {
    match outcome {
        RecordingOutcome::Recorded(report) => Ok((**report).clone()),
        RecordingOutcome::NoWindow { detail } | RecordingOutcome::Failed { detail } => {
            Err(detail.clone())
        }
    }
}

/// Everything a recording publishes through, cloned out of the [`Running`] it
/// belongs to.
///
/// One struct rather than four parameters because they are one thing: the set of
/// places a recording writes to that are not its file. Each is a handle over
/// shared state, so cloning costs a reference count and the recording thread and
/// the connection threads reading them never wait on each other for longer than
/// a `memcpy` (AGENTS.md section 20).
struct RecordingChannels {
    /// Where the recording has reached on its own timeline.
    progress: RecordingProgress,
    /// Where a `take_screenshot` asks it for a frame it already has.
    screenshots: ScreenshotRequests,
    /// Where it says which capture backend it settled on (issue #302).
    capture: CaptureAccounting,
    /// The rolling window a `save_replay` saves from, when it has one.
    replay: Option<Arc<ReplayRecording>>,
}

/// Runs one recording on a thread of its own.
///
/// The panic guard is the reason this is not two lines. `clipped_session`
/// finalises the file on every path out including a panic, so a panic here does
/// not cost the recording — but a thread that dies without storing an outcome
/// would leave `stop_recording` waiting for one for ever, which would cost the
/// user their ability to stop the recorder.
fn spawn_recording(
    state: &Arc<RecordingState>,
    id: &str,
    settings: RecordingSettings,
    stop: crate::shutdown::ShutdownSignal,
    channels: RecordingChannels,
) -> JoinHandle<()> {
    let state = Arc::clone(state);
    let id = id.to_owned();
    let RecordingChannels {
        progress,
        screenshots,
        capture,
        replay,
    } = channels;

    thread::Builder::new()
        .name("clipped-recording".to_owned())
        .spawn(move || {
            let mut outputs = clipped_session::RecordingOutputs::default()
                .with_progress(&progress)
                .with_screenshots(&screenshots)
                // Where the capture backend this recording settles on leaves the
                // capture thread. Without it `get_diagnostics` would have
                // nothing to report while a recording was running, which is the
                // only time there is a backend to report (issue #302).
                .with_capture_account(&capture);
            // The buffer is filled from the packets this recording produces —
            // one encoder, two consumers — and the handle outlives the
            // recording because whoever saves from it is on another thread
            // (`clipped_session::replay`).
            if let Some(replay) = replay.as_deref() {
                outputs = outputs.with_replay(replay);
            }
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                clipped_session::record_into(&settings, &stop, &outputs)
            }));

            let outcome = match outcome {
                Ok(Ok(report)) => Ok(report),
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => Err(
                    "the recording thread panicked; the file was finalised before it did"
                        .to_owned(),
                ),
            };

            state.finish(&id, outcome);
        })
        .expect("a thread can be started to record on")
}

/// What a finished recording is, in the vocabulary a session records.
///
/// The same three answers `watch` reports through
/// [`RecordingOutcome`](clipped_session::automatic::RecordingOutcome), because
/// they are the same three things that can happen and a session should not be
/// able to tell which subcommand produced it.
///
/// There is no `NoWindow` here and there cannot be: `start_recording` resolves
/// the window before it answers, so a recording that started had one. That is
/// the one difference between the two drivers, and it is a difference in what
/// can happen rather than in how it is written down — `watch` waits for a
/// window belonging to a game it has only just seen launch, and this waits for
/// nothing because the user is looking at the window.
fn session_outcome(outcome: &Result<RecordingReport, String>) -> RecordingOutcome {
    match outcome {
        Ok(report) => RecordingOutcome::Recorded(Box::new(report.clone())),
        Err(detail) => RecordingOutcome::Failed {
            detail: detail.clone(),
        },
    }
}

/// The status a recording state describes.
///
/// `watching` is what the launch watcher is doing, and is [`None`] when nothing
/// is watching — a plain `serve`, or one whose detection could not be started.
/// It is only ever the answer when nothing is being recorded: a recorder that is
/// both watching and recording is **recording**, because that is the thing a
/// window has to be able to see and stop.
///
/// Its *sitting* is read either way, though, and that is the one place the two
/// arms meet: a recording the watcher started has no sitting of its own and
/// belongs to the one the watcher is holding.
fn status_of(running: Option<&Running>, watching: Option<Watching>) -> RecorderStatus {
    match running {
        Some(running) if running.outcome.is_none() => RecorderStatus::Recording(ActiveRecording {
            recording_id: running.id.clone(),
            output: running.output.to_string_lossy().into_owned(),
            target: running.target.clone(),
            elapsed_ms: u64::try_from(running.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            // What a window reads before offering "Save Replay" for this
            // recording: the feature says the build has the command, and this
            // says there is a buffer to save from.
            replay_seconds: running
                .replay
                .as_ref()
                .map(|replay| u32::try_from(replay.window().as_secs()).unwrap_or(u32::MAX)),
            // **The game this recording is of**, which is the first acceptance
            // criterion of
            // [issue #241](https://github.com/wildware-uk/clipped/issues/241):
            // `target` is a capture selector and a window cannot turn `process
            // 4242` into "Counter-Strike 2" without the catalogue, which lives
            // here. Both kinds of recording carry it, from the sitting each one
            // actually belongs to:
            //
            // - one `start_recording` asked for is the whole of its own
            //   sitting, copied when [`RecordingState::begin`] opened it;
            // - one the watcher handed over belongs to the sitting its driver
            //   is in, which is the one on `watching` — and which may already
            //   hold the earlier files of the same sitting, so that the second
            //   file of a sitting stops looking like an unrelated recording.
            //
            // In that order, and the order is the point: a `start_recording`
            // arriving while this recorder is watching must not claim the game
            // the watcher was in the middle of, which would be a window naming
            // a game nobody asked to record.
            session: running
                .sitting
                .clone()
                .or_else(|| watching.and_then(|watching| watching.session)),
        }),
        // Nothing is being recorded, so what this recorder is depends on
        // whether it will record the next game to launch by itself. Those are
        // two different answers to "what are you doing", and answering `idle`
        // to both is the defect issue #584 is about — a recorder about to
        // record a game, and one that will never record anything, told a
        // window the same thing.
        _ => watching.map_or(RecorderStatus::Idle, RecorderStatus::Watching),
    }
}

/// Turns the protocol's parameters into the arguments `record` would have been
/// given.
///
/// Deliberately routed through [`RecordArgs`] rather than validated again here.
/// Every rule about what a recording may be asked for — a resolution's bounds,
/// a frame rate's range, an output path that must end in `.mkv` and must not
/// already exist — lives in [`crate::options`] and [`crate::config`], and a
/// second copy of them reachable only over IPC is a second set of answers to the
/// same question (AGENTS.md section 55).
fn record_args(request: &StartRecording) -> Result<RecordArgs, ProtocolError> {
    Ok(RecordArgs {
        window: request.window.clone(),
        process: request.process.clone(),
        pid: request.pid,
        output: request.output.as_ref().map(PathBuf::from),
        overwrite: request.overwrite,
        resolution: parsed(&request.resolution, "resolution")?.unwrap_or_default(),
        framerate: parsed(&request.framerate.map(|rate| rate.to_string()), "framerate")?
            .unwrap_or(crate::options::Framerate::DEFAULT),
        codec: chosen(&request.codec, "codec")?.unwrap_or_default(),
        encoder: chosen(&request.encoder, "encoder")?.unwrap_or_default(),
        microphone: parsed(&request.microphone, "microphone")?.unwrap_or_default(),
        system_audio: parsed(&request.system_audio, "system_audio")?.unwrap_or_default(),
    })
}

/// Parses an optional textual parameter through the same `FromStr` the command
/// line uses.
fn parsed<T>(value: &Option<String>, name: &str) -> Result<Option<T>, ProtocolError>
where
    T: std::str::FromStr,
    T::Err: fmt::Display,
{
    match value {
        None => Ok(None),
        Some(text) => text
            .parse()
            .map(Some)
            .map_err(|error| parameter_error(name, &error)),
    }
}

/// Reads an optional parameter that is one of a fixed set of names, through the
/// same list `--codec` and `--encoder` offer.
///
/// `clap`'s own value enumeration, so the accepted spellings cannot drift
/// between the command line and the protocol, and an unknown one is refused
/// with the same message the command line would give.
fn chosen<T: clap::ValueEnum>(
    value: &Option<String>,
    name: &str,
) -> Result<Option<T>, ProtocolError> {
    match value {
        None => Ok(None),
        Some(text) => T::from_str(text, true)
            .map(Some)
            .map_err(|error| parameter_error(name, &error)),
    }
}

/// A refusal naming the parameter that was wrong and why.
fn parameter_error(name: &str, reason: &dyn fmt::Display) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::InvalidParameters,
        format!("`{name}` was not usable: {reason}"),
    )
}

/// A configuration failure, as something the desktop application can render.
fn invalid_parameters(error: ConfigError) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParameters, error.to_string())
}

/// The refusal for a stop with nothing to stop.
fn nothing_to_stop() -> ProtocolError {
    ProtocolError::new(ErrorCode::NotRecording, "nothing is being recorded")
}

/// What a `start_recording` asked its recording to keep.
///
/// Two of the three answers are settled by the request alone; the third is not,
/// which is the whole reason this is an enum rather than an
/// `Option<Arc<ReplayRecording>>`. A request that asks for a buffer without
/// naming a length is asking for `replay_window_seconds`, and that setting
/// inherits per game — so its answer does not exist until the session has asked
/// the catalogue what game the window is ([`Running::with_replay_asked`]).
#[derive(Debug, Clone)]
enum ReplayAsked {
    /// No buffer. The request named no length and asked for none.
    Nothing,
    /// A buffer at the length this recorder has configured for this game.
    Configured,
    /// A buffer at the length the request named, already checked.
    Named(Arc<ReplayRecording>),
}

/// What a `start_recording` asks its recording to keep, checked as a parameter.
///
/// Called before the window is resolved and before anything is created, so that
/// a duration no buffer can hold is a parameter to fix rather than something
/// found out after a capture session has opened (AGENTS.md section 45). What it
/// cannot check here is the *configured* window, which no request carries and
/// which the session resolves — hence [`ReplayAsked::Configured`].
///
/// A named length wins over `replay`, because a caller that sent a number has
/// already answered the question `replay` asks the recorder.
fn replay_asked(request: &StartRecording) -> Result<ReplayAsked, ProtocolError> {
    if let Some(seconds) = request.replay_seconds {
        let named = replay_of(std::time::Duration::from_secs(u64::from(seconds)))?;
        return Ok(ReplayAsked::Named(named));
    }

    Ok(if request.replay {
        ReplayAsked::Configured
    } else {
        ReplayAsked::Nothing
    })
}

/// A replay buffer of `window`, refused in `clipped-replay`'s own words.
///
/// The bound is that crate's own and the message is its own, so that the
/// duration a window may ask for over the protocol, the one a settings file may
/// carry and the one `replay --duration` accepts are the same range with the
/// same explanation (AGENTS.md section 55).
fn replay_of(window: std::time::Duration) -> Result<Arc<ReplayRecording>, ProtocolError> {
    ReplayRecording::new(window)
        .map(Arc::new)
        .map_err(|error| ProtocolError::new(ErrorCode::InvalidParameters, error.to_string()))
}

/// How much a `save_replay` asked to keep, if it said.
///
/// A JSON number can be negative, infinite or not a number at all, and none of
/// those is a duration. Refusing it as a *parameter* is what tells a window it
/// sent something wrong rather than that the recorder could not do it
/// (AGENTS.md section 15).
fn requested_length(request: &SaveReplay) -> Result<Option<std::time::Duration>, ProtocolError> {
    let Some(seconds) = request.duration_seconds else {
        return Ok(None);
    };

    std::time::Duration::try_from_secs_f64(seconds)
        .map(Some)
        .map_err(|_| {
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                format!(
                    "`duration_seconds` has to be a number of seconds that is not negative, and                      {seconds} is not one"
                ),
            )
        })
}

/// Where a `save_replay` was told to put its clip, if it named anywhere.
///
/// A blank path is refused here rather than reaching the writer, which would
/// report it as a file that could not be created — a message about the wrong
/// thing. A path that is already *taken* is deliberately not refused here: the
/// writer refuses it, because whether a file exists is a question about the
/// disk at the moment of writing rather than at the moment of asking
/// (AGENTS.md section 56).
fn destination_for(request: &SaveReplay) -> Result<Option<PathBuf>, ProtocolError> {
    match request.output.as_deref() {
        None => Ok(None),
        Some(path) if path.trim().is_empty() => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            "`output` has to be a path to write the clip to; leave it out to have the clip \
             named after the session",
        )),
        Some(path) => Ok(Some(PathBuf::from(path))),
    }
}

/// The refusal for a replay with nothing to save.
///
/// A separate sentence from [`nothing_to_stop`] for the reason
/// [`nothing_to_bookmark`] is: somebody pressed a key and needs to know why
/// nothing happened (AGENTS.md section 45).
fn nothing_to_save() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::NotRecording,
        "nothing is being recorded, so there is no replay buffer to save from",
    )
}

/// A replay that could not be saved, as something the desktop application can
/// render.
///
/// The code says what the caller can do about it: a recording with no buffer,
/// or a buffer with nothing in it yet, is [`ErrorCode::NotRecording`] — nothing
/// is wrong, and the answer is to wait or to start a recording that keeps one —
/// and a disk that refused is [`ErrorCode::Internal`] with the file named in the
/// message.
fn replay_not_saved(error: ReplaySaveError) -> ProtocolError {
    let code = match &error {
        ReplaySaveError::NotBuffering | ReplaySaveError::NothingBuffered(_) => {
            ErrorCode::NotRecording
        }
        ReplaySaveError::NotWritten { .. } => ErrorCode::Internal,
        // `ReplaySaveError` is `#[non_exhaustive]`, and a variant added there is
        // a decision to make here rather than a silent `Internal`. It cannot be
        // caught by the compiler across a crate boundary, so it is caught by the
        // message instead.
        _ => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
}

/// The refusal for a bookmark with nothing to mark.
///
/// A separate sentence from [`nothing_to_stop`] because it answers a different
/// question: somebody pressed the bookmark key and needs to know why nothing
/// happened, and "nothing is being recorded" on its own reads like a fault
/// (AGENTS.md section 45).
fn nothing_to_bookmark() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::NotRecording,
        "nothing is being recorded, so there is no recording to mark a moment in",
    )
}

/// Reads the protocol's parameters into what `clipped-session` accepts.
///
/// Every bound — how long a label may be, how far before the press a bookmark
/// may be stamped — belongs to `clipped_session::bookmarks`, because that is
/// what has to store the result. A second set of limits here would be a second
/// set of answers to one question (AGENTS.md section 55).
fn bookmark_request(request: &AddBookmark) -> Result<BookmarkRequest, ProtocolError> {
    let mut wanted = BookmarkRequest::new()
        .with_label(request.label.clone())
        .and_then(|wanted| wanted.with_colour(request.colour.clone()))
        .map_err(invalid_bookmark)?;

    if let Some(seconds) = request.duration_seconds {
        wanted = wanted
            .with_duration(Some(bookmark_seconds(seconds, "duration_seconds")?))
            .map_err(invalid_bookmark)?;
    }
    if let Some(seconds) = request.lead_seconds {
        wanted = wanted
            .with_lead(bookmark_seconds(seconds, "lead_seconds")?)
            .map_err(invalid_bookmark)?;
    }
    Ok(wanted)
}

/// A duration from a figure on the wire, which may be anything a JSON number
/// can be.
fn bookmark_seconds(value: f64, name: &'static str) -> Result<std::time::Duration, ProtocolError> {
    std::time::Duration::try_from_secs_f64(value).map_err(|_| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "`{name}` has to be a number of seconds that is not negative, and {value} is \
                     not one"
            ),
        )
    })
}

/// A bookmark the recorder will not store, as something the UI can render.
fn invalid_bookmark(error: BookmarkError) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParameters, error.to_string())
}

/// A bookmark that was taken and could not be written down.
fn bookmark_not_saved(error: &BookmarkError) -> ProtocolError {
    ProtocolError::new(ErrorCode::Internal, error.to_string())
}

/// Where a screenshot goes and what it is saved as, from the request.
///
/// The directory is the recorder's default until the settings API is read at
/// the moment a command arrives ([issue
/// #61](https://github.com/wildware-uk/clipped/issues/61)); saying so here is
/// better than inventing a per-request one nothing would remember.
fn screenshot_settings(request: &TakeScreenshot) -> Result<ScreenshotSettings, ProtocolError> {
    let directory = clipped_session::screenshot::default_directory().ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::Internal,
            "this account has no home directory, so there is nowhere to put a screenshot",
        )
    })?;

    let format = match &request.format {
        None => ScreenshotFormat::default(),
        Some(name) => ScreenshotFormat::from_name(name).ok_or_else(|| {
            let known: Vec<&str> = ScreenshotFormat::ALL
                .iter()
                .map(|format| format.name())
                .collect();
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                format!("`{name}` is not a screenshot format; this build writes {known:?}"),
            )
        })?,
    };

    // Asked of the linked FFmpeg rather than assumed, because the answer for
    // lossless WebP depends on how it was built. Refusing here names the format
    // instead of writing a file with the wrong contents (AGENTS.md section 54).
    if !format.is_available() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!("this build of Clipped cannot write {format}"),
        ));
    }

    Ok(ScreenshotSettings::new(directory).with_format(format))
}

/// The window a screenshot with no recording behind it photographs.
fn screenshot_target(request: &TakeScreenshot) -> Result<CaptureTargetSettings, ProtocolError> {
    if request.window.is_none() && request.process.is_none() && request.pid.is_none() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            "nothing is being recorded, so a screenshot has to say which window to photograph: \
             send `window`, `process` or `pid`",
        ));
    }

    // The same resolution a recording goes through, so "which window is
    // `cs2.exe`" has one answer in this process (AGENTS.md section 55).
    let target = crate::config::target_from(
        request.window.as_deref(),
        request.process.as_deref(),
        request.pid,
    )
    .map_err(invalid_parameters)?;
    let window = resolve_window(&target).map_err(unrecordable_target)?;

    let size = window.geometry().client_size();
    Ok(
        CaptureTargetSettings::window(window.handle().as_u64(), size.width(), size.height())
            .content_protected(window.is_content_protected())
            .minimised(window.is_minimised()),
    )
}

/// The refusal for a target that cannot be recorded or photographed.
///
/// Shared by `start_recording` and `take_screenshot` so that the two answer the
/// same question the same way, and so that the desktop application can branch on
/// the code rather than on the sentence:
///
/// - [`ErrorCode::TargetNotFound`] — nothing matched, or several things did.
///   Choose something else.
/// - [`ErrorCode::TargetNotCapturable`] — one window matched and cannot be
///   recorded as it is. Change the window; the message names it and says how
///   ([issue #383](https://github.com/wildware-uk/clipped/issues/383)).
///
/// The message is the error's own words, carried through verbatim, because the
/// window this refusal is about is the window the user is looking at and only
/// this process knows what it is called.
fn unrecordable_target(error: crate::record::RecordError) -> ProtocolError {
    match error {
        crate::record::RecordError::Resolution(resolution) => {
            ProtocolError::new(ErrorCode::TargetNotFound, resolution.to_string())
        }
        minimised @ crate::record::RecordError::TargetMinimised { .. } => {
            ProtocolError::new(ErrorCode::TargetNotCapturable, minimised.to_string())
        }
        other => ProtocolError::new(ErrorCode::Internal, other.to_string()),
    }
}

/// The refusal for a screenshot that could not be taken or saved.
///
/// The code says what the caller can do about it: a format or a target it got
/// wrong is [`ErrorCode::InvalidParameters`], a window that stopped drawing is
/// [`ErrorCode::TargetNotFound`], and a disk that refused is
/// [`ErrorCode::Internal`] with the file named in the message.
fn screenshot_failed(error: ScreenshotError) -> ProtocolError {
    let code = match &error {
        ScreenshotError::FormatUnavailable { .. } => ErrorCode::InvalidParameters,
        ScreenshotError::NoFrame { .. } => ErrorCode::TargetNotFound,
        ScreenshotError::Capture(_) => ErrorCode::TargetNotFound,
        ScreenshotError::Copy(_)
        | ScreenshotError::Encode { .. }
        | ScreenshotError::DirectoryNotCreated { .. }
        | ScreenshotError::NotWritten { .. }
        | ScreenshotError::NoFreeName { .. }
        | ScreenshotError::NotCaptured { .. } => ErrorCode::Internal,
        // `ScreenshotError` is `#[non_exhaustive]`, and a variant added there
        // is a decision to make here rather than a silent `Internal`. It cannot
        // be caught by the compiler across a crate boundary, so it is caught by
        // the message instead.
        _ => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
}

/// A screenshot, as the protocol reports it.
fn summarise_screenshot(
    screenshot: &Screenshot,
    recording_id: Option<String>,
) -> ScreenshotSummary {
    ScreenshotSummary {
        path: screenshot.path().to_string_lossy().into_owned(),
        format: screenshot.format().name().to_owned(),
        width: screenshot.width(),
        height: screenshot.height(),
        bytes: screenshot.bytes(),
        recording_id,
        at_seconds: screenshot.position().map(|at| at.as_secs_f64()),
    }
}

/// The refusal for a lock a panic left poisoned.
fn poisoned(what: &str) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::Internal,
        format!("the recorder failed while {what}; its diagnostics have the detail"),
    )
}

/// A finished recording, as the protocol reports it.
fn summarise(report: &RecordingReport) -> RecordingSummary {
    let (width, height) = report.size();

    RecordingSummary {
        // Empty only for a capture that wrote no file, which `serve` never
        // starts — see `start_recording` above.
        output: report
            .output()
            .map_or_else(String::new, |output| output.to_string_lossy().into_owned()),
        duration_ms: u64::try_from(report.duration().as_millis()).unwrap_or(u64::MAX),
        // The protocol's own three words where they line up, and
        // `EndReason::Other` where they do not. The disk guard's reasons
        // (#103) are newer than the protocol's vocabulary, and inventing a
        // mapping onto `Stopped` would tell the desktop application that
        // somebody chose to stop a recording their drive stopped — which is
        // the difference between a notification and a lie. `Other` carries the
        // word verbatim, which is what that variant is for (docs/ipc.md);
        // promoting these two to named variants is
        // [issue #284](https://github.com/wildware-uk/clipped/issues/284).
        end_reason: match report.end_reason() {
            clipped_session::EndReason::Stopped => EndReason::Stopped,
            clipped_session::EndReason::TargetLost => EndReason::TargetLost,
            clipped_session::EndReason::TargetResized => EndReason::TargetResized,
            other => EndReason::Other(other.token().replace('-', "_")),
        },
        frames_encoded: report.frames_encoded(),
        frames_skipped_for_rate: report.frames_skipped_for_rate(),
        frames_dropped_writer_behind: report.frames_dropped_writer_behind(),
        sustained_framerate: report.sustained_framerate(),
        encoder: encoder_token(report.encoder()).to_owned(),
        codec: codec_token(report.codec()).to_owned(),
        width,
        height,
    }
}

/// The name `--encoder` accepts for an encoder.
///
/// The protocol and the command line use the same tokens deliberately: a
/// support request that says `encoder=nvenc` should mean the same thing
/// whichever of the two produced it.
fn encoder_token(encoder: clipped_encoder::EncoderKind) -> &'static str {
    match encoder {
        clipped_encoder::EncoderKind::Nvenc => "nvenc",
        clipped_encoder::EncoderKind::Amf => "amf",
        clipped_encoder::EncoderKind::QuickSync => "quicksync",
        clipped_encoder::EncoderKind::Software => "software",
    }
}

/// The name `--codec` accepts for a codec.
fn codec_token(codec: clipped_encoder::Codec) -> &'static str {
    match codec {
        clipped_encoder::Codec::H264 => "h264",
        clipped_encoder::Codec::Hevc => "hevc",
        clipped_encoder::Codec::Av1 => "av1",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use clipped_game_detection::catalogue::EntrySource;
    use clipped_session::bookmarks::{BookmarkFile, DEFAULT_LEAD};
    use clipped_windows::{MonitorHandle, PixelSize, WindowGeometry, WindowHandle};

    use super::*;
    use crate::test_support::Scratch;

    /// A catalogue that claims **this test process** as a game, by the name and
    /// the directory its executable really has.
    ///
    /// Built from `current_exe` rather than written out, and qualified by a
    /// path, because that is what makes the lookup real: the recorder is told
    /// to record a window belonging to a live process, and the only way the
    /// qualifier can be checked is if the recorder really asked Windows where
    /// that process's image lives. A build that dropped the image path answers
    /// `unattributed` here and passes nothing.
    ///
    /// The shipped catalogue is deliberately not used: it would make these
    /// tests depend on which games Clipped happens to ship, and the entries
    /// that matter — Counter-Strike 2 among them — are path-qualified against
    /// install directories no test machine has (AGENTS.md section 25).
    fn catalogue_claiming_this_process() -> Catalogue {
        let executable = std::env::current_exe().expect("a test process can name its executable");
        let name = this_executable_name();
        let directory = executable
            .parent()
            .and_then(Path::file_name)
            .expect("an executable is in a directory")
            .to_string_lossy()
            .into_owned();

        Catalogue::parse(
            &format!(
                "schema_version = 1\n\n[[game]]\ngame_id = \"a-test-game\"\nname = \"A Test \
                 Game\"\n[[game.executables]]\nname = \"{name}\"\npath_contains = \
                 \"{directory}\"\n"
            ),
            EntrySource::Seed,
        )
        .expect("the fixture is a valid catalogue")
    }

    /// The file name of the executable these tests are running as.
    fn this_executable_name() -> String {
        std::env::current_exe()
            .expect("a test process can name its executable")
            .file_name()
            .expect("an executable has a file name")
            .to_string_lossy()
            .into_owned()
    }

    /// A window of this test's process, as the recorder would have resolved one.
    ///
    /// Constructed rather than enumerated — there is no desktop here — but its
    /// process identifier is a real one, which is the half of the lookup a
    /// fabricated number cannot exercise.
    fn window_of(process_id: u32, image_name: &str) -> WindowInfo {
        WindowInfo::new(
            WindowHandle::from_raw(0x0001_04ac),
            "A Test Game".to_owned(),
            process_id,
            Some(image_name.to_owned()),
            WindowGeometry::new(PixelSize::new(2560, 1440), 96, MonitorHandle::from_raw(1)),
            false,
            None,
        )
    }

    /// A directory of this test's own, removed again when the test that made it
    /// passes; several of these run at once.
    ///
    /// Every test here used to end with `let _ = fs::remove_dir_all(&directory)`
    /// instead, which is two defects at once and 1,787 `clipped-serve-*`
    /// directories on one machine
    /// ([issue #598](https://github.com/wildware-uk/clipped/issues/598)). A
    /// test that fails never reaches its last line, so exactly the runs worth
    /// diagnosing left nothing to diagnose *and* left the directory; and the
    /// service under test holds the library database open until it is dropped
    /// at the end of the test body, so on Windows the removal was refused —
    /// silently, because its result went to `_`.
    fn scratch(name: &str) -> Scratch {
        Scratch::new(&format!("serve-{name}"))
    }

    fn moment() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725)
    }

    /// An indexer over a library and recording folder of this test's own.
    ///
    /// Never the real ones: an indexer pointed at `for_this_user` would walk the
    /// recordings of whoever is running the tests and write to their library
    /// (AGENTS.md section 25).
    fn indexer_over(directory: &Path) -> LibraryIndexer {
        LibraryIndexer::at(
            Some(directory.join("library.db")),
            vec![directory.to_path_buf()],
        )
    }

    /// A recording state with nothing recording, over a scratch library and a
    /// catalogue that claims nothing.
    fn idle_state(directory: &Path) -> Arc<RecordingState> {
        state_over(directory, Catalogue::default())
    }

    /// The same, over a catalogue the caller chose.
    fn state_over(directory: &Path, catalogue: Catalogue) -> Arc<RecordingState> {
        state_configured(directory, catalogue, Configuration::defaults())
    }

    /// The same again, over settings the caller chose.
    ///
    /// Never the user's own file: `ConfigurationStore::default_path` is
    /// whoever is running the tests, and a test that read it would pass or fail
    /// on their settings (AGENTS.md section 25).
    fn state_configured(
        directory: &Path,
        catalogue: Catalogue,
        configuration: Configuration,
    ) -> Arc<RecordingState> {
        // A settings file inside the scratch directory, never the user's own:
        // `ConfigurationStore::default_path` is whoever is running the tests,
        // and a test that read it would pass or fail on their settings
        // (AGENTS.md section 25). Written and then read back rather than held in
        // memory, so that what the caller configured reaches a recording by the
        // path a real one takes.
        let path = directory.join("settings.json");
        clipped_session::config::ConfigurationStore::at(&path)
            .store(configuration)
            .expect("a scratch settings file can be written");

        Arc::new(RecordingState::new(
            EventPublisher::new(),
            Arc::new(indexer_over(directory)),
            Arc::new(crate::settings::SettingsFile::at(path)),
            catalogue,
            // Not the machine's: a test that asked what is installed here would
            // answer differently on another machine (AGENTS.md section 25).
            Launchers::none(),
        ))
    }

    /// A state holding a recording that has reached `position`.
    ///
    /// Built by hand rather than by starting one: capturing a real window would
    /// need a desktop, a GPU and an encoder. Everything the real `start` builds
    /// is built here, **including the session** — it is not optional there and
    /// it is not skipped here, so a test that exercises the end of a recording
    /// exercises the end of a session record too.
    fn recording_at(output: &Path, position: Option<Duration>) -> Arc<RecordingState> {
        let directory = output.parent().expect("the output is in a directory");
        let state = idle_state(directory);
        let running = started_recording(&state, output, position);
        *state.current.lock().expect("a fresh lock") = Some(running);
        state
    }

    /// A recording as `start` builds one, through the same constructor.
    ///
    /// Never a `Running { … }` literal, deliberately: a test that assembled the
    /// fields by hand could leave out the session, or the catalogue lookup that
    /// gives it a game, and would then be testing a recording the recorder
    /// cannot make.
    fn started_recording(
        state: &RecordingState,
        output: &Path,
        position: Option<Duration>,
    ) -> Running {
        started_recording_with(state, output, position, None)
    }

    /// The same, with or without a replay buffer attached.
    fn started_recording_with(
        state: &RecordingState,
        output: &Path,
        position: Option<Duration>,
        replay: Option<Arc<ReplayRecording>>,
    ) -> Running {
        let running = state
            .begin(
                "r-1".to_owned(),
                output.to_path_buf(),
                "process cs2.exe".to_owned(),
                &window_of(4_242, "cs2.exe"),
                &Configuration::defaults(),
                moment(),
            )
            .with_replay(replay);
        if let Some(position) = position {
            running.progress.reached(position);
        }
        running
    }

    /// The distinction issue #584 is about, in the one place that decides it.
    ///
    /// Three answers from one state, and the middle one is the one that did not
    /// exist: a recorder watching for a game to launch answered `idle`, which is
    /// what a recorder that will never record anything answers.
    #[test]
    fn watching_for_games_is_a_different_answer_from_idle_and_from_recording() {
        let directory = scratch("watching-or-idle");
        let output = directory.join("clipped-cs2.mkv");
        let state = idle_state(&directory);

        assert_eq!(
            state.status(),
            RecorderStatus::Idle,
            "nothing is watching and nothing is recording, which is what a plain `serve` is for \
             its whole life",
        );

        let watching = state.watch_for_games();
        assert_eq!(
            state.status(),
            RecorderStatus::Watching(Watching { session: None }),
            "and now the next game to launch will be recorded, which a window cannot be told by \
             the same word as `idle`",
        );

        // A recorder that is watching *and* recording is recording: that is the
        // thing a window has to be able to see and stop, and it is the answer
        // whether the recording was asked for or started by the watcher itself.
        let running = started_recording(&state, &output, Some(Duration::from_secs(30)));
        *state.current.lock().expect("a fresh lock") = Some(running);
        assert!(
            matches!(state.status(), RecorderStatus::Recording(_)),
            "a watching recorder that is recording reports the recording: {:?}",
            state.status(),
        );

        drop(watching);
        assert!(
            matches!(state.status(), RecorderStatus::Recording(_)),
            "and the watcher going does not change what is being recorded",
        );

        *state.current.lock().expect("a fresh lock") = None;
        assert_eq!(
            state.status(),
            RecorderStatus::Idle,
            "with the watcher gone and nothing recording, this recorder will record nothing by \
             itself, and `idle` is the honest word for that",
        );
    }

    /// A sitting reported by a driver whose recorder is not watching is not put
    /// on a status.
    ///
    /// `watch` serves no protocol, and a driver that outlived its guard — a
    /// shutdown, or detection that stopped — has nothing to say either. The
    /// alternative is a recorder claiming a sitting while answering `idle`,
    /// which no client could read at all: an idle status has nowhere to carry
    /// one.
    #[test]
    fn a_sitting_reported_with_nothing_watching_is_ignored_rather_than_stored() {
        let directory = scratch("sitting-unwatched");
        let state = idle_state(&directory);

        state.sitting_is(Some(Box::new(SessionSummary {
            session_id: "test-game-20260811-143205".to_owned(),
            game_name: Some("Test Game".to_owned()),
            ..SessionSummary::default()
        })));

        assert_eq!(state.status(), RecorderStatus::Idle);
    }

    /// Issue #241's first acceptance criterion, for a recording nobody asked
    /// for.
    ///
    /// The sitting is the only thing that knows the game — `target` is a
    /// capture selector, and turning `process 4242` into "Counter-Strike 2"
    /// needs the catalogue — and `status_of` set this field to `None` for every
    /// recording, so a window could name the game a recorder was *waiting* for
    /// and not the one it was recording.
    #[test]
    fn a_recording_the_watcher_started_carries_the_sitting_it_is_part_of() {
        let directory = scratch("adopted-sitting");
        let output = directory.join("test-game-20260811-201400-02.mkv");
        let state = idle_state(&directory);
        let _watching = state.watch_for_games();

        let (_adopted, _progress, _stop, _asked) = adopted_recording(&state, &output, None);
        match state.status() {
            RecorderStatus::Recording(active) => assert_eq!(
                active.session, None,
                "a watcher with no sitting invents none: an empty sitting on a recording would \
                 be a game name with nothing behind it",
            ),
            other => panic!("the recorder should be recording, not {other:?}"),
        }

        // What the driver reports once a pass, including while this recording
        // runs, so the sitting is on the status the moment a window asks.
        state.sitting_is(Some(Box::new(SessionSummary {
            session_id: "test-game-20260811-201400".to_owned(),
            game_id: Some("test-game".to_owned()),
            game_name: Some("Test Game".to_owned()),
            started_at: "2026-08-11T20:14:00+01:00".to_owned(),
            recordings: vec![
                clipped_ipc::SessionRecording {
                    session_index: 1,
                    output: directory
                        .join("test-game-20260811-201400-01.mkv")
                        .to_string_lossy()
                        .into_owned(),
                    outcome: Some("recorded".to_owned()),
                    duration_ms: Some(600_000),
                },
                clipped_ipc::SessionRecording {
                    session_index: 2,
                    output: output.to_string_lossy().into_owned(),
                    ..clipped_ipc::SessionRecording::default()
                },
            ],
            ..SessionSummary::default()
        })));

        let session = match state.status() {
            RecorderStatus::Recording(active) => *active
                .session
                .expect("a recording the watcher started belongs to the sitting it is holding"),
            other => panic!("the recorder should be recording, not {other:?}"),
        };
        assert_eq!(
            session.game_name.as_deref(),
            Some("Test Game"),
            "this is the whole of the criterion: a recording that can say which game it is of",
        );
        assert_eq!(
            session.recordings.len(),
            2,
            "and which file of the sitting it is, so the second one stops looking like an \
             unrelated recording: {:?}",
            session.recordings,
        );
    }

    /// The other kind of recording, and the reason the two are told apart.
    ///
    /// A `start_recording` is the whole of its own sitting, which the catalogue
    /// attributed when it opened. Reading the *watcher's* sitting for it would
    /// put the game somebody was playing an hour ago on a recording of
    /// something else — a window naming a game nobody asked to record.
    #[test]
    fn a_recording_the_window_asked_for_carries_its_own_sitting_and_not_the_watchers() {
        let directory = scratch("asked-for-sitting");
        let output = directory.join("clipped-20260813-120000.mkv");
        let state = state_over(&directory, catalogue_claiming_this_process());
        let _watching = state.watch_for_games();
        state.sitting_is(Some(Box::new(SessionSummary {
            session_id: "some-other-game-20260811-201400".to_owned(),
            game_name: Some("Some Other Game".to_owned()),
            ..SessionSummary::default()
        })));

        let running = state.begin(
            "r-1".to_owned(),
            output.clone(),
            format!("process {}", this_executable_name()),
            &window_of(std::process::id(), &this_executable_name()),
            &Configuration::defaults(),
            moment(),
        );
        *state.current.lock().expect("a fresh lock") = Some(running);

        let session = match state.status() {
            RecorderStatus::Recording(active) => *active
                .session
                .expect("every recording this recorder makes opens a sitting"),
            other => panic!("the recorder should be recording, not {other:?}"),
        };
        assert_eq!(
            session.game_name.as_deref(),
            Some("A Test Game"),
            "the game is the one the catalogue gave *this* recording's sitting",
        );
        assert!(
            session.session_id.starts_with("a-test-game-"),
            "and its sitting is the one the recording opened: {}",
            session.session_id,
        );
        assert_eq!(
            session.recordings.len(),
            1,
            "which holds the file being written and nothing else: {:?}",
            session.recordings,
        );
        assert_eq!(session.recordings[0].output, output.to_string_lossy());
        assert_eq!(
            session.ended_at, None,
            "a sitting on a status is one the recorder is still in",
        );
    }

    /// The sitting reaching a window **while the recording is still running**.
    ///
    /// Issue #584 kept quiet about the watcher's changes under a recording, and
    /// was right to while the sitting was invisible then. It is on the recording
    /// now, and the change that matters most happens exactly there: the sitting
    /// gains this recording's own file a moment after it starts. A subscriber
    /// told nothing would draw "the first file of this sitting" for as long as
    /// the second one took to record, and `get_status` would disagree with the
    /// event stream about the same recorder.
    #[test]
    fn a_sitting_that_changes_under_a_running_recording_is_published_rather_than_held_back() {
        let directory = scratch("sitting-under-recording");
        let output = directory.join("test-game-20260811-201400-01.mkv");

        let events = EventPublisher::new();
        let service = Arc::new(RecorderService::with_library(
            events.clone(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory),
            Catalogue::default(),
        ));
        let subscribed = crate::test_events::Subscribed::to(&events, &service, "sitting-recording");

        let state = Arc::clone(service.recordings());
        let _watching = state.watch_for_games();
        let (adopted, _progress, _stop, _asked) = adopted_recording(&state, &output, None);

        state.sitting_is(Some(Box::new(SessionSummary {
            session_id: "test-game-20260811-201400".to_owned(),
            game_name: Some("Test Game".to_owned()),
            recordings: vec![clipped_ipc::SessionRecording {
                session_index: 1,
                output: output.to_string_lossy().into_owned(),
                ..clipped_ipc::SessionRecording::default()
            }],
            ..SessionSummary::default()
        })));

        let event = subscribed.wait_for("the recording's sitting", |event| {
            matches!(
                event,
                clipped_ipc::Event::StatusChanged {
                    status: RecorderStatus::Recording(active),
                } if active.session.is_some()
            )
        });
        let clipped_ipc::Event::StatusChanged { status } = event else {
            unreachable!("`wait_for` matched a status change")
        };
        let RecorderStatus::Recording(active) = status else {
            unreachable!("`wait_for` matched a recording")
        };
        assert_eq!(
            active
                .session
                .expect("`wait_for` matched a recording with a sitting")
                .game_name
                .as_deref(),
            Some("Test Game"),
        );

        drop(subscribed);
        // Handed back before the recorder is shut down, because a shutdown
        // stops whatever is running and waits for its file: this recording is a
        // stand-in with no thread behind it, and nothing else would ever store
        // an outcome for it.
        drop(adopted);
        service.shut_down();
    }

    /// Issue #241's second acceptance criterion, for the sitting a
    /// `start_recording` was the whole of.
    ///
    /// `Event::SessionEnded` was defined, mirrored in the desktop's TypeScript
    /// and carried by the schema, and **nothing ever sent one**. This subscribes
    /// the way the desktop application does — a real server over a real pipe —
    /// because a test that called the publisher itself is exactly the test that
    /// would have passed all along.
    #[test]
    fn a_sitting_a_recording_was_the_whole_of_is_announced_with_the_file_it_produced() {
        let directory = scratch("session-ended");
        let output = directory.join("clipped-20260813-120000.mkv");
        std::fs::write(&output, [0u8; 4096]).expect("the recording can be written");

        let events = EventPublisher::new();
        let service = Arc::new(RecorderService::with_library(
            events.clone(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory),
            catalogue_claiming_this_process(),
        ));
        let subscribed = crate::test_events::Subscribed::to(&events, &service, "session-ended");

        let running = service.recordings.begin(
            "r-1".to_owned(),
            output.clone(),
            format!("process {}", this_executable_name()),
            &window_of(std::process::id(), &this_executable_name()),
            &Configuration::defaults(),
            moment(),
        );
        *service.recordings.current.lock().expect("a fresh lock") = Some(running);
        service
            .recordings
            .finish("r-1", Err("the encoder went".to_owned()));

        let event = subscribed.wait_for("a sitting ending", |event| {
            matches!(event, clipped_ipc::Event::SessionEnded { .. })
        });
        let clipped_ipc::Event::SessionEnded { session } = event else {
            unreachable!("`wait_for` matched a sitting ending")
        };

        assert_eq!(
            session.game_name.as_deref(),
            Some("A Test Game"),
            "the sitting is the one the catalogue attributed when the recording opened it",
        );
        assert!(
            session.ended_at.is_some(),
            "what makes a sitting over is `ended_at`, and this one is: {session:?}",
        );
        assert_eq!(
            session.end_reason.as_deref(),
            Some("recording-ended"),
            "a sitting somebody's recording was the whole of ends when that recording does",
        );
        assert_eq!(
            session.recordings.len(),
            1,
            "and it carries the files it produced, which is why the event exists: {:?}",
            session.recordings,
        );
        assert_eq!(session.recordings[0].output, output.to_string_lossy());
        assert_eq!(
            session.recordings[0].outcome.as_deref(),
            Some("failed"),
            "a recording that produced nothing is listed all the same, saying so",
        );

        drop(subscribed);
        service.shut_down();
    }

    #[test]
    fn a_replay_asked_for_with_nothing_recording_is_refused_by_its_own_sentence() {
        // Somebody pressed the key. "Nothing is being recorded" on its own
        // reads like a fault, so the refusal says what there was to save from
        // (AGENTS.md section 45).
        let directory = scratch("no-recording");
        let state = idle_state(&directory);

        let error = state
            .save_replay(&SaveReplay::default(), moment())
            .expect_err("there is no recording");

        assert_eq!(error.code, ErrorCode::NotRecording);
        assert!(
            error.message.contains("replay buffer"),
            "the refusal has to say what is missing: {}",
            error.message
        );
    }

    #[test]
    fn a_recording_started_without_a_buffer_says_so_rather_than_writing_nothing() {
        // The refusal that matters most, because it is the one a user can act
        // on: this recording keeps no history, and the answer is to start one
        // that does. A build that answered "nothing is being recorded" while a
        // recording ran would be telling them something false.
        let directory = scratch("no-buffer");
        let output = directory.join("clipped-cs2.mkv");
        let state = recording_at(&output, Some(Duration::from_secs(30)));

        let error = state
            .save_replay(&SaveReplay::default(), moment())
            .expect_err("this recording keeps no buffer");

        assert_eq!(error.code, ErrorCode::NotRecording);
        assert!(
            error.message.contains("replay_seconds"),
            "the refusal has to name what to ask for instead: {}",
            error.message
        );
        assert!(
            matches!(state.status(), RecorderStatus::Recording(active) if active.replay_seconds.is_none()),
            "and the status has to say the same thing, so a window never offers the control"
        );
    }

    #[test]
    fn a_recording_with_a_buffer_says_how_much_it_keeps_and_refuses_until_it_has_some() {
        // The other half: a recording that *does* keep a buffer advertises the
        // window on its status, and a save before the encoder has produced
        // anything is refused rather than writing an empty file — which is the
        // first second of every buffered recording.
        let directory = scratch("with-buffer");
        let output = directory.join("clipped-cs2.mkv");
        let state = idle_state(&directory);
        let replay = Arc::new(
            ReplayRecording::new(Duration::from_secs(90)).expect("ninety seconds is in range"),
        );
        let running = started_recording_with(
            &state,
            &output,
            Some(Duration::from_secs(30)),
            Some(Arc::clone(&replay)),
        );
        *state.current.lock().expect("a fresh lock") = Some(running);

        let RecorderStatus::Recording(active) = state.status() else {
            panic!("the recorder is recording");
        };
        assert_eq!(
            active.replay_seconds,
            Some(90),
            "a window reads this before it offers Save Replay for this recording"
        );

        let error = state
            .save_replay(&SaveReplay::default(), moment())
            .expect_err("the encoder has produced nothing yet");
        assert_eq!(error.code, ErrorCode::NotRecording);
    }

    #[test]
    fn a_replay_meant_for_a_recording_that_has_ended_is_not_taken_out_of_its_successor() {
        // The same race `stop_recording` and `add_bookmark` name a recording
        // for: the window had one on screen, it ended, and the save must not
        // land in whatever is running now.
        let directory = scratch("named-recording");
        let output = directory.join("clipped-cs2.mkv");
        let state = recording_at(&output, Some(Duration::from_secs(30)));

        let error = state
            .save_replay(
                &SaveReplay {
                    recording_id: Some("r-99".to_owned()),
                    ..SaveReplay::default()
                },
                moment(),
            )
            .expect_err("r-99 is not the recording this recorder is running");

        assert_eq!(error.code, ErrorCode::NotRecording);
        assert!(error.message.contains("r-99"), "{}", error.message);
    }

    #[test]
    fn a_duration_that_is_not_a_number_of_seconds_is_a_parameter_to_fix() {
        // A JSON number can be negative or infinite, and neither is a duration.
        // It is `invalid_parameters` rather than `not_recording` because the
        // caller sent something wrong, which is a different thing to tell a
        // window (AGENTS.md section 15).
        let directory = scratch("bad-duration");
        let output = directory.join("clipped-cs2.mkv");
        let state = recording_at(&output, Some(Duration::from_secs(30)));

        for seconds in [-1.0, f64::NAN, f64::INFINITY] {
            let error = state
                .save_replay(
                    &SaveReplay {
                        duration_seconds: Some(seconds),
                        ..SaveReplay::default()
                    },
                    moment(),
                )
                .expect_err("that is not a number of seconds");
            assert_eq!(
                error.code,
                ErrorCode::InvalidParameters,
                "{seconds} should be refused as a parameter: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_blank_output_is_refused_before_it_reaches_the_writer() {
        // A path of spaces would surface as "the clip could not be created",
        // which is a message about the wrong thing.
        let error = destination_for(&SaveReplay {
            output: Some("   ".to_owned()),
            ..SaveReplay::default()
        })
        .expect_err("a blank path is not a path");

        assert_eq!(error.code, ErrorCode::InvalidParameters);
        assert!(error.message.contains("output"), "{}", error.message);

        assert_eq!(
            destination_for(&SaveReplay::default()).expect("nothing named is not an error"),
            None,
            "leaving it out means the session names the clip"
        );
    }

    #[test]
    fn a_replay_buffer_a_start_asked_for_is_bounded_by_the_buffers_own_rules() {
        // The duration a window may ask for over the protocol and the one
        // `replay --duration` accepts are the same range with the same
        // explanation, because both come from `clipped-replay` (AGENTS.md
        // section 55).
        assert!(
            matches!(
                replay_asked(&StartRecording::default()).expect("no buffer asked for"),
                ReplayAsked::Nothing
            ),
            "a request that says nothing about a replay asks for no buffer"
        );

        let ReplayAsked::Named(asked) = replay_asked(&StartRecording {
            replay_seconds: Some(60),
            ..StartRecording::default()
        })
        .expect("a minute is in range") else {
            panic!("a length that was named is a length this recording keeps");
        };
        assert_eq!(asked.window(), Duration::from_secs(60));

        let error = replay_asked(&StartRecording {
            replay_seconds: Some(4 * 3600),
            ..StartRecording::default()
        })
        .expect_err("four hours is not a supported window");
        assert_eq!(error.code, ErrorCode::InvalidParameters);
        assert!(
            error.message.contains("30.0s") && error.message.contains("1800.0s"),
            "the refusal has to name the bounds: {}",
            error.message
        );

        // Both at once is a caller that has already answered the question
        // `replay` asks, and the number it sent is the one it gets.
        let ReplayAsked::Named(both) = replay_asked(&StartRecording {
            replay: true,
            replay_seconds: Some(90),
            ..StartRecording::default()
        })
        .expect("ninety seconds is in range") else {
            panic!("a length that was named wins over the configured one");
        };
        assert_eq!(both.window(), Duration::from_secs(90));
    }

    #[test]
    fn a_start_that_asks_for_a_buffer_without_a_length_keeps_the_one_the_settings_chose() {
        // Issue #427's first criterion, from the settings file to the buffer.
        // The desktop window cannot read a setting — it may link `clipped-ipc`
        // and nothing else of this workspace
        // (`tests/integration/tests/workspace_layering.rs`) — so it asks for a
        // buffer without naming a length, and this is the recorder answering
        // with `replay_window_seconds`. A `with_replay_asked` that invented a
        // constant, or that read the global layer instead of the recording's,
        // would give every recording five minutes however the user had set it,
        // and the tray's Save Replay would look exactly as correct as it does
        // now.
        let directory = scratch("configured-window");
        let output = directory.join("clipped-20260813-120000.mkv");

        let mut configuration = Configuration::defaults();
        let mut global = clipped_session::config::Preferences::default();
        global
            .set_replay_window(Some(Duration::from_secs(90)))
            .expect("ninety seconds is a window a buffer will take");
        configuration.set_global(global);

        let state = state_configured(&directory, Catalogue::default(), configuration.clone());
        let running = state
            .begin(
                "r-1".to_owned(),
                output.clone(),
                "process cs2.exe".to_owned(),
                &window_of(4_242, "cs2.exe"),
                &configuration,
                moment(),
            )
            .with_replay_asked(ReplayAsked::Configured)
            .expect("the configured window is one a buffer will take");

        assert_eq!(
            running
                .replay
                .as_ref()
                .expect("a recording that asked for a buffer has one")
                .window(),
            Duration::from_secs(90),
            "the buffer keeps what the settings file said, not what this file said"
        );

        // And the same recording with nothing asked for keeps nothing, so the
        // configured window is applied because it was asked for rather than
        // because it exists — the protocol's promise to every other client
        // (`StartRecording::replay_seconds`).
        let ordinary = state
            .begin(
                "r-2".to_owned(),
                output,
                "process cs2.exe".to_owned(),
                &window_of(4_242, "cs2.exe"),
                &configuration,
                moment(),
            )
            .with_replay_asked(ReplayAsked::Nothing)
            .expect("nothing asked for is not a failure");
        assert!(
            ordinary.replay.is_none(),
            "a recording nobody asked to keep a buffer keeps none, however the settings read"
        );
    }

    #[test]
    fn the_buffer_a_recording_keeps_is_the_one_configured_for_the_game_it_is_of() {
        // The half above cannot see: `replay_window_seconds` inherits per game
        // (AGENTS.md section 30), so the length has to come from the fold the
        // *session* resolved rather than from `resolve_global`. The catalogue
        // claims this test's own process, so the recording is of a known game
        // and that game's layer is the one that must win.
        let directory = scratch("per-game-window");
        let output = directory.join("clipped-20260813-120000.mkv");

        let mut configuration = Configuration::defaults();
        let mut global = clipped_session::config::Preferences::default();
        global
            .set_replay_window(Some(Duration::from_secs(600)))
            .expect("ten minutes is a window a buffer will take");
        configuration.set_global(global);

        let mut for_the_game = clipped_session::config::Preferences::default();
        for_the_game
            .set_replay_window(Some(Duration::from_secs(45)))
            .expect("forty-five seconds is a window a buffer will take");
        configuration.set_game(
            clipped_session::config::GameKey::parse("a-test-game")
                .expect("the catalogue's identifier is a settings key"),
            for_the_game,
        );

        let state = state_configured(
            &directory,
            catalogue_claiming_this_process(),
            configuration.clone(),
        );
        let running = state
            .begin(
                "r-1".to_owned(),
                output,
                format!("process {}", this_executable_name()),
                &window_of(std::process::id(), &this_executable_name()),
                &configuration,
                moment(),
            )
            .with_replay_asked(ReplayAsked::Configured)
            .expect("the configured window is one a buffer will take");

        assert_eq!(
            running
                .replay
                .as_ref()
                .expect("a recording that asked for a buffer has one")
                .window(),
            Duration::from_secs(45),
            "the game's own replay window has to beat the global one, or per-game settings stop \
             at the buffer"
        );
    }

    #[test]
    fn a_bookmark_lands_before_the_request_and_is_on_disk_when_the_reply_is_sent() {
        // Both halves of the ticket in one test: where the mark goes, and the
        // fact that it is written *before* the caller is told it was taken. A
        // recorder killed a moment later has already saved it.
        let directory = scratch("taken");
        let output = directory.join("clipped-cs2.mkv");
        let state = recording_at(&output, Some(Duration::from_secs(120)));

        let summary = state
            .bookmark(&AddBookmark::default(), moment())
            .expect("a bookmark can be taken while a recording is running");

        assert_eq!(summary.recording_id, "r-1");
        assert_eq!(summary.pressed_at_seconds, 120.0);
        assert_eq!(summary.lead_seconds, DEFAULT_LEAD.as_secs_f64());
        assert_eq!(summary.at_seconds, 120.0 - DEFAULT_LEAD.as_secs_f64());
        assert_eq!(summary.bookmarks_in_recording, 1);

        let read = BookmarkFile::for_recording(&output)
            .expect("the bookmark is on disk by the time the reply is built");
        assert_eq!(read.bookmarks.len(), 1);
        assert_eq!(
            read.bookmarks[0].at().as_secs_f64(),
            summary.at_seconds,
            "the reply and the file have to agree about where the mark is"
        );
        assert_eq!(
            summary.bookmarks_file,
            output
                .with_file_name("clipped-cs2.bookmarks.json")
                .to_string_lossy()
        );
    }

    #[test]
    fn every_parameter_a_caller_sends_reaches_the_bookmark_that_is_written() {
        let directory = scratch("parameters");
        let output = directory.join("clipped-cs2.mkv");
        let state = recording_at(&output, Some(Duration::from_secs(60)));

        let summary = state
            .bookmark(
                &AddBookmark {
                    recording_id: Some("r-1".to_owned()),
                    label: Some("triple kill".to_owned()),
                    colour: Some("#ffcc00".to_owned()),
                    duration_seconds: Some(12.5),
                    lead_seconds: Some(3.25),
                },
                moment(),
            )
            .expect("a fully specified bookmark can be taken");

        assert_eq!(summary.at_seconds, 56.75);
        assert_eq!(summary.lead_seconds, 3.25);
        assert_eq!(summary.label.as_deref(), Some("triple kill"));
        assert_eq!(summary.colour.as_deref(), Some("#ffcc00"));
        assert_eq!(summary.duration_seconds, Some(12.5));

        let written = &BookmarkFile::for_recording(&output)
            .expect("it was written")
            .bookmarks[0];
        assert_eq!(written.at(), Duration::from_millis(56_750));
        assert_eq!(written.lead(), Duration::from_millis(3_250));
        assert_eq!(written.label(), Some("triple kill"));
        assert_eq!(written.colour(), Some("#ffcc00"));
        assert_eq!(written.duration(), Some(Duration::from_millis(12_500)));
    }

    #[test]
    fn a_bookmark_with_nothing_to_mark_is_refused_rather_than_written_somewhere() {
        // Three ways there is nothing to mark, and each is a different sentence
        // because each has a different answer for the user.
        let directory = scratch("refused");
        let output = directory.join("clipped-cs2.mkv");

        let idle = idle_state(&directory);
        let error = idle
            .bookmark(&AddBookmark::default(), moment())
            .expect_err("nothing is being recorded");
        assert_eq!(error.code, ErrorCode::NotRecording);

        let starting = recording_at(&output, None);
        let error = starting
            .bookmark(&AddBookmark::default(), moment())
            .expect_err("no frame has reached the file yet");
        assert_eq!(error.code, ErrorCode::NotRecording);
        assert!(
            error.message.contains("first frame"),
            "the refusal should say what is missing: {}",
            error.message
        );
        assert!(
            BookmarkFile::for_recording(&output).is_err(),
            "a refused bookmark must not leave a file behind"
        );

        let running = recording_at(&output, Some(Duration::from_secs(30)));
        let error = running
            .bookmark(
                &AddBookmark {
                    recording_id: Some("r-9".to_owned()),
                    ..AddBookmark::default()
                },
                moment(),
            )
            .expect_err("r-9 is not the recording that is running");
        assert_eq!(error.code, ErrorCode::NotRecording);
        assert!(error.message.contains("r-9"), "{}", error.message);
    }

    #[test]
    fn a_bookmark_the_recorder_cannot_store_is_refused_with_which_field_was_wrong() {
        let directory = scratch("invalid");
        let output = directory.join("clipped-cs2.mkv");
        let state = recording_at(&output, Some(Duration::from_secs(30)));

        for (request, expected) in [
            (
                AddBookmark {
                    label: Some("x".repeat(1_000)),
                    ..AddBookmark::default()
                },
                "label",
            ),
            (
                AddBookmark {
                    lead_seconds: Some(-1.0),
                    ..AddBookmark::default()
                },
                "lead_seconds",
            ),
            (
                AddBookmark {
                    lead_seconds: Some(600.0),
                    ..AddBookmark::default()
                },
                "lead",
            ),
            (
                AddBookmark {
                    duration_seconds: Some(f64::NAN),
                    ..AddBookmark::default()
                },
                "duration_seconds",
            ),
        ] {
            let error = state
                .bookmark(&request, moment())
                .expect_err("the recorder cannot store this bookmark");
            assert_eq!(error.code, ErrorCode::InvalidParameters, "{request:?}");
            assert!(
                error.message.contains(expected),
                "the refusal should name the field that was wrong: {}",
                error.message
            );
        }

        assert!(
            BookmarkFile::for_recording(&output).is_err(),
            "a bookmark the recorder refused must not have been written"
        );
    }

    /// A service over a library holding one sitting whose file has gone.
    ///
    /// The library is built here rather than reconciled, because what is under
    /// test is the path from a command to a reply, not indexing.
    fn service_over_a_library(name: &str) -> (Scratch, RecorderService) {
        let directory = scratch(name);
        let path = directory.join("library.db");
        {
            let database = clipped_storage::Database::open(&path).expect("a database opens");
            let connection = database.connection();
            connection
                .execute(
                    "INSERT INTO games (game_id, name, first_seen_at) \
                     VALUES ('cs2', 'Counter-Strike 2', '2026-08-11T20:14:00+01:00')",
                    [],
                )
                .expect("a game inserts");
            connection
                .execute(
                    "INSERT INTO sessions (session_id, game_id, started_at) \
                     VALUES ('cs2-20260811-201400', 'cs2', '2026-08-11T20:14:00+01:00')",
                    [],
                )
                .expect("a session inserts");
            connection
                .execute(
                    "INSERT INTO recordings \
                        (session_id, session_index, path, started_at, size_bytes, missing_since) \
                     VALUES ('cs2-20260811-201400', 1, 'D:\\clips\\gone.mkv', \
                             '2026-08-11T20:14:00+01:00', 1024, '2026-08-12T09:00:00+01:00')",
                    [],
                )
                .expect("a recording inserts");
        }

        // The directory comes back with the service. The service holds a
        // `LibraryReader`, which caches the database it opens, so the directory
        // has to outlive it — and a tuple's bindings are dropped in reverse, so
        // `let (directory, service) = …` at the call site drops the service
        // first, which is the order Windows insists on.
        let service = RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(path)),
            indexer_over(&directory),
            Catalogue::default(),
        );
        (directory, service)
    }

    #[test]
    fn the_library_commands_are_answered_from_the_index_through_the_real_dispatch() {
        // Deliberately through `CommandHandler::call` rather than through
        // `LibraryReader` beside it: what issue #301 is about is a command
        // reaching the index at all, and a reader that works while nothing
        // routes a command to it is the gap this ticket exists to close. A
        // command wired to the wrong handler, or refused before dispatch, fails
        // here and nowhere else.
        let (_directory, service) = service_over_a_library("library");

        let Reply::LibrarySessions { page } = service
            .call(Command::LibrarySessions(
                clipped_ipc::LibrarySessions::default(),
            ))
            .expect("the library reads")
        else {
            panic!("`library_sessions` was answered with something else");
        };
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(
            page.sessions[0].recordings[0].missing_since.as_deref(),
            Some("2026-08-12T09:00:00+01:00"),
            "a recording whose file has gone has to reach the window saying so"
        );

        let Reply::LibraryGames { games } =
            service.call(Command::LibraryGames).expect("the games read")
        else {
            panic!("`library_games` was answered with something else");
        };
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].missing, 1);
    }

    #[test]
    fn the_marks_of_a_recording_reach_the_window_placed_in_its_file() {
        // Issue #329. The window cannot work out where a mark goes: that needs
        // the recording's span, which it has no way to know. So the recorder
        // answers with the offset into the file, and this is the wiring that
        // proves the command reaches the index rather than merely compiling.
        let directory = scratch("library-events");
        let path = directory.join("library.db");
        let recording = {
            let database = clipped_storage::Database::open(&path).expect("a database opens");
            let connection = database.connection();
            connection
                .execute_batch(
                    "INSERT INTO games (game_id, name, first_seen_at)                      VALUES ('cs2', 'Counter-Strike 2', '2026-08-11T20:14:00+01:00');                      INSERT INTO sessions (session_id, game_id, started_at)                      VALUES ('cs2-20260811-201400', 'cs2', '2026-08-11T20:14:00+01:00');                      INSERT INTO recordings (session_id, session_index, path, started_at,                          duration_seconds, starts_at_nanos)                      VALUES ('cs2-20260811-201400', 1, 'one.mkv',                              '2026-08-11T20:14:00+01:00', 600.0, 60000000000);",
                )
                .expect("a session with one recording inserts");
            let recording = connection.last_insert_rowid();

            // The recording starts a minute into the session, so a kill at 64 s
            // on the session's timeline is 4 s into *this file*. Getting that
            // subtraction backwards is the whole failure this answers.
            connection
                .execute(
                    "INSERT INTO game_events                         (session_id, recording_id, at_nanos, kind, source, document)                      VALUES ('cs2-20260811-201400', ?1, 64000000000,                              'acme-cs2.flashbang_blinded_five', 'acme-cs2', '{}')",
                    clipped_storage::rusqlite::params![recording],
                )
                .expect("an event inserts");
            recording
        };

        let service = RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(path)),
            indexer_over(&directory),
            Catalogue::default(),
        );

        let Reply::LibraryEvents { lane } = service
            .call(Command::LibraryEvents(clipped_ipc::LibraryEvents {
                recording: recording.to_string(),
            }))
            .expect("the events read")
        else {
            panic!("`library_events` was answered with something else");
        };

        assert_eq!(lane.marks.len(), 1);
        assert_eq!(
            lane.marks[0].at, 4_000_000_000,
            "the mark is 4 s into the file and was placed at {}ns, which is what a mark drawn              against the session's timeline rather than the file's would look like",
            lane.marks[0].at
        );
        assert_eq!(
            lane.marks[0].kind, "acme-cs2.flashbang_blinded_five",
            "a kind this build has never met has to arrive and be drawn"
        );
        assert_eq!(lane.marks[0].source, "acme-cs2");
    }

    #[test]
    fn the_preview_command_is_answered_with_the_picture_of_the_recording_it_named() {
        // The middle hop, which nothing else covers: `preview::open`'s own tests
        // start at the service and the window's tests stop at `invoke`, so a
        // dispatch that answered `open_preview` with the wrong command, or with
        // the wrong recording's picture, would pass both. That is the failure
        // `library_sessions` had once and `start_recording` had once, and this
        // is where it would land now (issue #448).
        let directory = scratch("open-preview");
        let recording = directory.join("cs2-20260811-201400-1.mkv");
        std::fs::write(&recording, b"a stand-in for a recording").expect("it can be written");
        let thumbnails = directory.join("thumbnails");
        std::fs::create_dir_all(&thumbnails).expect("the cache directory can be made");
        store_a_thumbnail(&thumbnails, &recording, b"the picture of that recording");

        let service = RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory).with_preview_caches(thumbnails, directory.join("waveforms")),
            Catalogue::default(),
        );

        let Reply::PreviewOpened { preview } = service
            .call(Command::OpenPreview(clipped_ipc::OpenPreview {
                source: recording.to_string_lossy().into_owned(),
                kind: clipped_ipc::PreviewKind::Thumbnail,
                buckets: None,
            }))
            .expect("a thumbnail that is there is not a refusal")
        else {
            panic!("`open_preview` was answered with something else");
        };

        assert_eq!(preview.state, clipped_ipc::PreviewState::Ready);
        assert_eq!(
            preview
                .picture
                .expect("a ready thumbnail carries a picture")
                .bytes,
            clipped_ipc::base64(b"the picture of that recording"),
            "the window would have been handed somebody else's frame"
        );
    }

    #[test]
    fn this_build_says_it_can_be_asked_for_a_preview() {
        // A window checks this before it draws a tile that would hold a picture,
        // so a recorder that answers `open_preview` and does not advertise it
        // would have a library screen with no pictures in it and nothing
        // anywhere reporting a problem (`clipped_ipc::features`, issue #448).
        assert!(
            features_of_this_build().contains(&clipped_ipc::features::PREVIEWS.to_owned()),
            "this build answers `open_preview` and must say so: {:?}",
            features_of_this_build()
        );
    }

    /// Writes a thumbnail cache entry, as `docs/thumbnails.md` specifies one.
    ///
    /// The same fixture `crate::preview`'s tests use, written out again here
    /// rather than shared because the two modules test different things with it
    /// and a `#[cfg(test)]` helper does not cross a module boundary without
    /// being made part of the crate's surface.
    fn store_a_thumbnail(cache: &Path, recording: &Path, picture: &[u8]) {
        let identity = clipped_library::thumbnail::SourceIdentity::of(recording)
            .expect("the stand-in recording can be stat-ed");
        let key = identity.cache_key();
        std::fs::write(cache.join(format!("{key}.jpg")), picture)
            .expect("the picture can be written");
        let sidecar = serde_json::json!({
            "version": 1,
            "recording": recording.to_string_lossy(),
            "size_bytes": identity.size(),
            "modified_nanos": identity.modified_nanos(),
            "image": {
                "file": format!("{key}.jpg"),
                "width": 640,
                "height": 360,
                "at_seconds": 12.5,
                "blank": false
            }
        });
        std::fs::write(
            cache.join(format!("{key}.json")),
            serde_json::to_vec(&sidecar).expect("the sidecar serialises"),
        )
        .expect("the sidecar can be written");
    }

    #[test]
    fn a_recording_with_no_events_is_answered_as_none_rather_than_refused() {
        // "None" and "nobody asked" are different things to draw, and the
        // Editor screen says them differently. An empty lane is the first; a
        // refusal would make the window guess at the second.
        let (_directory, service) = service_over_a_library("library-events-none");

        let Reply::LibraryEvents { lane } = service
            .call(Command::LibraryEvents(clipped_ipc::LibraryEvents {
                recording: "1".to_owned(),
            }))
            .expect("a recording with no events is not a failure")
        else {
            panic!("`library_events` was answered with something else");
        };

        assert!(lane.marks.is_empty());
    }

    #[test]
    fn an_identifier_that_is_not_a_recording_is_the_callers_mistake() {
        let (_directory, service) = service_over_a_library("library-events-bad-id");

        let refusal = service
            .call(Command::LibraryEvents(clipped_ipc::LibraryEvents {
                recording: "the third one".to_owned(),
            }))
            .expect_err("that is not an identifier this library uses");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
    }

    #[test]
    fn a_library_that_cannot_be_read_is_refused_through_the_dispatch_rather_than_drawn_as_empty() {
        let directory = scratch("library-unreadable");
        let path = directory.join("library.db");
        std::fs::write(&path, b"not a database").expect("the file is written");
        let service = RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(path)),
            indexer_over(&directory),
            Catalogue::default(),
        );

        let refusal = service
            .call(Command::LibrarySessions(
                clipped_ipc::LibrarySessions::default(),
            ))
            .expect_err("an unreadable library is not an empty one");

        assert_eq!(refusal.code, ErrorCode::LibraryUnavailable);
    }

    /// How long a test waits for the indexer thread. Generous: it is a walk of
    /// a directory holding two files, and a bound tight enough to trip on a
    /// busy machine is a failure nobody can tell from a real one.
    const INDEXING_PATIENCE: Duration = Duration::from_secs(30);

    /// A service over an empty library and an empty recordings folder, both of
    /// this test's own, with a recording in progress.
    ///
    /// The recording is put in by hand for the reason `recording_at` gives —
    /// there is no window, GPU or encoder here — and it carries the session the
    /// real `start` builds, because that is the thing under test.
    fn service_recording_into(directory: &Path, output: &Path) -> RecorderService {
        service_recording_into_over(directory, output, Catalogue::default())
    }

    /// The same, over a catalogue the caller chose.
    fn service_recording_into_over(
        directory: &Path,
        output: &Path,
        catalogue: Catalogue,
    ) -> RecorderService {
        let service = RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(directory),
            catalogue,
        );

        let running = started_recording(&service.recordings, output, None);
        *service.recordings.current.lock().expect("a fresh lock") = Some(running);
        service
    }

    /// The sessions the window would be shown, through the real dispatch.
    fn library_of(service: &RecorderService) -> Vec<clipped_ipc::LibrarySession> {
        let Reply::LibrarySessions { page } = service
            .call(Command::LibrarySessions(
                clipped_ipc::LibrarySessions::default(),
            ))
            .expect("the library reads")
        else {
            panic!("`library_sessions` was answered with something else");
        };
        page.sessions
    }

    #[test]
    fn a_recording_made_from_the_window_is_in_the_library_when_the_window_next_asks() {
        // Issue #402's acceptance criterion, end to end and in one process: a
        // recording started over the protocol, finished, and then found by the
        // command the Library screen sends — with nothing restarted in between.
        //
        // The three links it holds together are the three that were missing.
        // `serve` writes a session record at all; something calls
        // `clipped_library::index::reconcile`; and the reply the window gets is
        // built from what that run wrote. Break any one of them — drop the
        // `session` from `Running`, take the `indexer.request()` out of
        // `finish`, never start the indexer thread — and this fails.
        let directory = scratch("indexed");
        let output = directory.join("clipped-20260813-120000.mkv");
        std::fs::write(&output, [0u8; 4096]).expect("the recording can be written");

        let service = service_recording_into(&directory, &output);
        assert!(
            library_of(&service).is_empty(),
            "nothing has indexed yet, so there is nothing for the library to show — and \
             without this the assertions below could be about a row that was already there"
        );

        // The run start-up asks for is drained first, so that what is asserted
        // below is the run *this recording* asked for. Without it a build that
        // never asked for one after a recording could still pass, on the timing
        // of a thread.
        service.start_indexing();
        assert!(service.indexer.settled_within(INDEXING_PATIENCE));
        let runs_before = service.indexer.runs();

        // What the recording thread does when its recording ends. The outcome
        // is a failure rather than a report because `RecordingReport`'s fields
        // belong to `clipped-session` and cannot be built from here; what
        // differs is one column of one row, and the recorded case is held by
        // `clipped_session::automatic`'s own comparison of the two session
        // records.
        service
            .recordings
            .finish("r-1", Err("the encoder went".to_owned()));

        assert!(
            service.indexer.settled_within(INDEXING_PATIENCE),
            "the indexer never finished the run the recording asked for"
        );
        assert!(
            service.indexer.runs() > runs_before,
            "a recording that finished has to ask for the library to be brought up to date"
        );

        let sessions = library_of(&service);
        assert_eq!(
            sessions.len(),
            1,
            "a recording made from the window has to reach the library: {sessions:?}"
        );
        assert_eq!(sessions[0].recordings.len(), 1);
        assert_eq!(
            sessions[0].recordings[0].path,
            output.to_string_lossy(),
            "the row has to name the file that was recorded"
        );
        assert_eq!(
            sessions[0].recordings[0].size_bytes,
            Some(4096),
            "the size comes from the file on disk, not from the session record"
        );
        assert_eq!(
            sessions[0].game_id, None,
            "the catalogue was asked about this window and claimed nothing, and the library \
             says so rather than inventing a game"
        );
        assert_eq!(
            sessions[0].end_reason.as_deref(),
            Some("recording-ended"),
            "the row has to be the *finished* session record rather than the one written when \
             the recording started"
        );
        assert!(sessions[0].ended_at.is_some());

        service.shut_down();
    }

    #[test]
    fn a_recording_of_a_window_the_catalogue_claims_reaches_the_library_under_that_game() {
        // Issue #403's first acceptance criterion, along the whole path it has
        // to survive: the window's process, the catalogue lookup, the session
        // record, the index, and the reply the Library screen is drawn from. A
        // sitting that arrives here with no game is a sitting the screen groups
        // apart from the ones `watch` recorded of the same game, which is the
        // defect.
        //
        // The window is this test process's own, so the image path the recorder
        // reads is a real one and the catalogue's path qualifier is really
        // checked (`catalogue_claiming_this_process`). Take the image path out
        // of `begin`, hand `ManualSession` an empty catalogue, or stop asking
        // it at all, and this fails while the test above still passes.
        let directory = scratch("attributed");
        let output = directory.join("clipped-20260813-120000.mkv");
        std::fs::write(&output, [0u8; 4096]).expect("the recording can be written");

        let service = RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory),
            catalogue_claiming_this_process(),
        );
        let running = service.recordings.begin(
            "r-1".to_owned(),
            output.clone(),
            format!("process {}", this_executable_name()),
            &window_of(std::process::id(), &this_executable_name()),
            &Configuration::defaults(),
            moment(),
        );
        *service.recordings.current.lock().expect("a fresh lock") = Some(running);

        service.start_indexing();
        assert!(service.indexer.settled_within(INDEXING_PATIENCE));
        service
            .recordings
            .finish("r-1", Err("the encoder went".to_owned()));
        assert!(
            service.indexer.settled_within(INDEXING_PATIENCE),
            "the indexer never finished the run the recording asked for"
        );

        let sessions = library_of(&service);
        assert_eq!(sessions.len(), 1, "{sessions:?}");
        assert_eq!(
            sessions[0].game_id.as_deref(),
            Some("a-test-game"),
            "a recording of a window the catalogue claims has to be filed under that game: \
             {sessions:?}"
        );
        assert_eq!(sessions[0].game_name.as_deref(), Some("A Test Game"));
        assert!(
            sessions[0].session_id.starts_with("a-test-game-"),
            "the sitting is named after the game as one `watch` recorded would be: {}",
            sessions[0].session_id
        );

        // And the Library screen's own grouping, which is what the user sees:
        // one game, with this sitting in it, rather than a row of sittings
        // attributed to nothing.
        let Reply::LibraryGames { games } =
            service.call(Command::LibraryGames).expect("the games read")
        else {
            panic!("`library_games` was answered with something else");
        };
        assert_eq!(games.len(), 1, "{games:?}");
        assert_eq!(games[0].game_id.as_deref(), Some("a-test-game"));
        assert_eq!(games[0].sessions, 1);

        service.shut_down();
    }

    #[test]
    fn the_recorder_the_window_drives_holds_the_catalogue_watch_would_have_loaded() {
        // The question the test above cannot ask, because it supplies the
        // catalogue itself: does the recorder this product actually builds have
        // one? `RecorderService::new` is what `serve` calls, and a build that
        // loaded no catalogue — or loaded the shipped data while ignoring the
        // user's own file — files recordings under nothing, or under games
        // somebody excluded, however well everything below it works.
        //
        // The answer is compared against the loader rather than against a
        // fixture, so this holds on any machine: one that has a games file of
        // its own, one that has none, and one whose file cannot be read.
        let service = RecorderService::new(EventPublisher::new());

        match crate::watch::load_catalogue() {
            Ok(expected) => {
                assert!(
                    !expected.entries().is_empty(),
                    "the shipped catalogue is not empty, so this comparison is worth making"
                );
                assert_eq!(
                    service.recordings.catalogue, expected,
                    "`serve` has to file recordings through the same catalogue `watch` matches \
                     processes against"
                );
            }
            // The documented fallback rather than an untested branch: a games
            // file that cannot be read costs attribution and must cost nothing
            // else (`catalogue_for_recordings`).
            Err(_) => assert!(
                service.recordings.catalogue.entries().is_empty(),
                "a catalogue that could not be read must not be replaced by the shipped one, \
                 which would ignore what the user decided"
            ),
        }
    }

    #[test]
    fn a_recording_started_while_the_catalogue_is_unreadable_is_still_made_and_still_filed() {
        // The third acceptance criterion. An empty catalogue is what a games
        // file nobody can read leaves behind, and the recording has to go on
        // regardless: the person pressed record, and their footage is what
        // cannot be made again (AGENTS.md sections 16 and 17).
        let directory = scratch("no-catalogue");
        let output = directory.join("clipped-20260813-120000.mkv");
        let state = state_over(&directory, Catalogue::default());

        let running = state.begin(
            "r-1".to_owned(),
            output.clone(),
            format!("process {}", this_executable_name()),
            &window_of(std::process::id(), &this_executable_name()),
            &Configuration::defaults(),
            moment(),
        );

        let session = running.session().lock().expect("a fresh lock");
        assert_eq!(
            session.session().game(),
            &clipped_session::automatic::GameIdentity::Unidentified
        );
        assert!(
            session.sidecar_path().is_file(),
            "the sitting still has a record, so the recording still reaches the library"
        );
        drop(session);
    }

    #[test]
    fn a_recording_that_ends_closes_its_session_record_and_leaves_it_final() {
        // The half of the path above that the library test cannot distinguish
        // from luck: the record on disk is opened when the recording starts and
        // *closed* when it ends, so what the index reads is a finished sitting
        // rather than one that says a recording began and never ended. It is
        // written with the recording state released, for the reason
        // `add_bookmark` releases it — the write is on the recording thread and
        // must not make a bookmark or a `get_status` wait on a disk.
        let directory = scratch("ordering");
        let output = directory.join("clipped-20260813-120000.mkv");
        std::fs::write(&output, [0u8; 16]).expect("the recording can be written");

        let service = service_recording_into(&directory, &output);
        let sidecar = std::fs::read_dir(&directory)
            .expect("the directory can be listed")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().ends_with(".session.json"))
            .expect("the session's record is written when the recording starts");

        let opened = std::fs::read_to_string(&sidecar).expect("it can be read");
        assert!(
            opened.contains("\"ended_at\": null"),
            "a session that is still recording has not ended: {opened}"
        );

        service
            .recordings
            .finish("r-1", Err("the encoder went".to_owned()));

        let closed = std::fs::read_to_string(&sidecar).expect("it can be read");
        assert!(
            closed.contains("recording-ended"),
            "the session has to be closed once its recording is: {closed}"
        );

        service.shut_down();
    }

    #[test]
    fn a_session_recorded_before_this_process_started_is_indexed_at_start_up() {
        // The other moment reconciliation runs, and the reason it has to: a
        // sitting `watch` recorded in a process of its own, or a recording made
        // by a build that was killed before its own run, is in the folder and in
        // no index. Nothing but start-up would ever look at it.
        let directory = scratch("start-up");
        let output = directory.join("clipped-earlier.mkv");
        std::fs::write(&output, [0u8; 2048]).expect("the recording can be written");
        let earlier = ManualSession::start(
            &directory,
            output.clone(),
            &Configuration::defaults(),
            &Catalogue::default(),
            &Launchers::none(),
            RecordedProcess::new(7, "cs2.exe"),
            moment(),
        );
        let _ = earlier.finish(
            &RecordingOutcome::Failed {
                detail: "before this process existed".to_owned(),
            },
            moment(),
        );

        let service = RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory),
            Catalogue::default(),
        );
        service.start_indexing();
        assert!(
            service.indexer.settled_within(INDEXING_PATIENCE),
            "the run start-up asks for never finished"
        );

        assert_eq!(
            library_of(&service).len(),
            1,
            "a sitting recorded before this process started has to be picked up"
        );

        service.shut_down();
    }

    #[test]
    fn a_recording_with_no_session_record_is_left_where_it_is_and_never_invented_into_a_row() {
        // What happens to the files a user upgrading already has: a build whose
        // `serve` wrote no session record left `.mkv` files nothing describes.
        // The library refuses to guess what they are — inventing a session would
        // be inventing a game, a start time and a sitting nobody recorded — and
        // it refuses just as firmly to tidy them up. They are reported at every
        // run (`crate::library::report_unindexed`) until issue #272 offers to
        // recover them.
        let directory = scratch("orphan");
        let orphan = directory.join("clipped-20260812-171203.mkv");
        std::fs::write(&orphan, [0u8; 1024]).expect("the recording can be written");

        let service = RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory),
            Catalogue::default(),
        );
        service.start_indexing();
        assert!(service.indexer.settled_within(INDEXING_PATIENCE));
        assert!(
            service.indexer.runs() >= 1,
            "the file was never looked at, so this test proves nothing about it"
        );

        assert!(
            library_of(&service).is_empty(),
            "a file no session record names must not become a row the library made up"
        );
        assert!(
            orphan.is_file(),
            "indexing must never move, rename or delete a recording"
        );
        assert_eq!(
            std::fs::metadata(&orphan).expect("it is still there").len(),
            1024,
            "and must never write to one"
        );

        service.shut_down();
    }

    #[test]
    fn a_build_that_can_measure_a_microphone_says_so_apart_from_the_settings() {
        // Its own capability, and the reason is what a window does without it:
        // a settings screen that cannot get a level still has a working list of
        // devices, so it must draw the chooser and leave out the meter rather
        // than refuse the screen (`clipped_ipc::features`, issue #109).
        let features = features_of_this_build();
        assert!(features.contains(&clipped_ipc::features::SETTINGS.to_owned()));
        assert_eq!(
            features.contains(&clipped_ipc::features::MICROPHONE_LEVEL.to_owned()),
            cfg!(windows),
            "a build with no audio backend must not claim it can listen: {features:?}",
        );
    }

    #[test]
    fn only_a_recorder_that_is_watching_advertises_that_it_records_games_by_itself() {
        // Issue #587. Every other name in the welcome is a fact about the
        // build; this one is a fact about the recorder, and the difference is
        // the whole reason the feature exists. `features_of_this_build` never
        // listed it, so no window could tell a recorder that records by itself
        // from one that never will.
        //
        // The guard is what decides it, which is what makes the third
        // assertion worth making: detection that starts and then stops takes
        // the claim with it, exactly as it takes `RecorderStatus::Watching`
        // (`WatchingForGames`, issue #584). A recorder that went on advertising
        // it would be telling every window that the next game to launch will be
        // recorded, with nothing left to record it (AGENTS.md sections 27 and
        // 54).
        //
        // Over the real protocol, both directions, in
        // `tests/ipc_protocol.rs::only_a_recorder_that_records_games_by_itself_advertises_that_it_does`.
        let service = RecorderService::new(EventPublisher::new());
        let automatic = clipped_ipc::features::AUTOMATIC.to_owned();

        assert!(
            !service.features().contains(&automatic),
            "nothing has asked this recorder to watch, so it will record nothing it is not asked \
             for: {:?}",
            service.features(),
        );

        let watching = service.recordings.watch_for_games();
        assert!(
            service.features().contains(&automatic),
            "a recorder watching for games records them by itself, and a window has no other way \
             to find that out: {:?}",
            service.features(),
        );

        drop(watching);
        assert!(
            !service.features().contains(&automatic),
            "a watcher that has gone must take the claim with it: {:?}",
            service.features(),
        );
    }

    #[test]
    fn recording_no_microphone_is_refused_a_level_rather_than_answered_with_silence() {
        // The distinction the meter exists to draw, at its own boundary. `none`
        // is a setting somebody chose and a reading of zero is a microphone
        // that heard nothing, and a screen given the second for the first would
        // draw a dead meter over a deliberate choice (AGENTS.md section 27).
        //
        // Needs no audio device: `none` names no endpoint, so nothing is opened
        // before the refusal.
        let directory = scratch("microphone-level-none");
        let service = RecorderService::with_settings(
            EventPublisher::new(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory),
            Catalogue::default(),
            crate::settings::SettingsFile::at(directory.join("settings.json")),
        );

        let error = service
            .call(Command::GetMicrophoneLevel(
                clipped_ipc::MicrophoneLevelRequest {
                    microphone: "none".to_owned(),
                },
            ))
            .expect_err("`none` has no level to report");

        assert_eq!(error.code, ErrorCode::InvalidParameters);
        assert!(
            error.message.contains("no microphone"),
            "the refusal has to say why there is no level: {}",
            error.message,
        );

        service.shut_down();
    }

    #[test]
    fn a_microphone_the_settings_file_would_refuse_is_refused_in_its_own_words() {
        // The value is parsed by the settings file's own parser, so a value
        // this can be asked about is exactly a value that could be saved
        // (`crate::settings::microphone_level`). A second, looser parser here
        // would let a window meter something it could never write down.
        //
        // Needs no audio device: the value never resolves to an endpoint.
        let directory = scratch("microphone-level-refused");
        let service = RecorderService::with_settings(
            EventPublisher::new(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory),
            Catalogue::default(),
            crate::settings::SettingsFile::at(directory.join("settings.json")),
        );

        let error = service
            .call(Command::GetMicrophoneLevel(
                clipped_ipc::MicrophoneLevelRequest {
                    // Blank is the shortest thing the file refuses, and it is
                    // refused by `AudioDeviceSetting::named` rather than by
                    // anything written here.
                    microphone: "name:".to_owned(),
                },
            ))
            .expect_err("a blank device name is not a value the settings file holds");

        assert_eq!(error.code, ErrorCode::InvalidParameters);

        service.shut_down();
    }

    #[test]
    fn a_recorder_that_can_read_the_library_says_so_in_its_handshake() {
        // The same rule bookmarks and screenshots follow: the window asks here
        // before it draws a library screen, so a build that can read the index
        // and does not advertise it is one whose Library screen stays empty for
        // no reason anybody can see.
        assert!(features_of_this_build().contains(&clipped_ipc::features::LIBRARY.to_owned()));
    }

    #[test]
    fn a_setting_saved_through_the_protocol_is_what_the_next_recording_is_made_with() {
        // Through `CommandHandler::call` rather than through `crate::settings`
        // beside it, for the reason the export case below gives: what issue #51
        // is about is a change from the window reaching the settings a
        // *recording* resolves from. Until it landed, this service held a
        // `Configuration` copied out of the file when the process started, so a
        // setting saved from the window reached the next recording only after a
        // restart — which is not what "close the window and recording works
        // from then on" means (SPEC.md section 45).
        let directory = scratch("settings-dispatch");
        let service = RecorderService::with_settings(
            EventPublisher::new(),
            LibraryReader::at(Some(directory.join("library.db"))),
            indexer_over(&directory),
            Catalogue::default(),
            crate::settings::SettingsFile::at(directory.join("settings.json")),
        );

        let mut values = std::collections::BTreeMap::new();
        values.insert("microphone".to_owned(), Some("name:Shure MV7".to_owned()));
        let reply = service
            .call(Command::ApplySettings(clipped_ipc::ApplySettings {
                values,
            }))
            .expect("a device name is a value the settings file can hold");

        let Reply::Settings { settings } = reply else {
            panic!("apply_settings answered with something other than the settings");
        };
        assert!(settings
            .settings
            .iter()
            .any(|entry| entry.key == "microphone" && entry.value == "name:Shure MV7"));

        // The settings a recording starting now resolves from — the state the
        // recordings share, rather than a snapshot taken at start-up.
        assert_eq!(
            service
                .recordings
                .settings
                .configuration()
                .resolve_global()
                .written_value(clipped_session::config::SettingKey::Microphone),
            "name:Shure MV7",
        );
    }

    #[test]
    fn an_export_is_routed_to_the_muxer_through_the_real_dispatch() {
        // Deliberately through `CommandHandler::call` rather than through
        // `crate::export` beside it. What issue #399 is about is a command
        // reaching the muxer at all; an export function that works while
        // nothing routes a command to it is exactly the gap this ticket exists
        // to close, and a command wired to the wrong handler — or refused
        // before dispatch as one this build does not perform — fails here and
        // nowhere else.
        //
        // The source is deliberately not media: what is under test is the
        // route, and a refusal in the muxer's own words is proof the muxer was
        // reached. Whether the copy is a real MP4 that decodes is
        // `apps/recorder/tests/ipc_protocol.rs`, over a real recorder process
        // and a real file.
        let directory = scratch("export-dispatch");
        let source = directory.join("match.mkv");
        std::fs::write(&source, b"this is not media").expect("the source is written");
        let service = RecorderService::new(EventPublisher::new());

        let refusal = service
            .call(Command::ExportRecording(clipped_ipc::ExportRecording {
                source: source.to_string_lossy().into_owned(),
                destination: directory.join("match.mp4").to_string_lossy().into_owned(),
            }))
            .expect_err("a file that is not media cannot be remuxed");

        assert_eq!(refusal.code, ErrorCode::ExportFailed);
        assert!(
            refusal.message.contains("match.mkv"),
            "the muxer's own sentence has to survive the dispatch: {}",
            refusal.message
        );
    }

    #[test]
    fn a_recorder_that_can_export_says_so_in_its_handshake() {
        // The same rule the library follows: the window asks here before it
        // draws an Export control, so a build that can copy a recording into
        // MP4 and does not advertise it is one whose library offers no way to
        // share anything, for no reason anybody can see.
        assert!(features_of_this_build().contains(&clipped_ipc::features::EXPORT.to_owned()));
    }

    #[test]
    fn a_recorder_that_can_bookmark_says_so_in_its_handshake() {
        // A UI decides whether to offer the control by asking here rather than
        // by inferring it from a version, so a build that stores bookmarks and
        // does not advertise them is one whose tray never offers the item.
        assert!(features_of_this_build().contains(&clipped_ipc::features::BOOKMARKS.to_owned()));
    }

    #[test]
    fn a_minimised_window_is_refused_over_ipc_as_a_target_that_cannot_be_captured() {
        // The code is what the desktop application branches on, and it has to
        // be this one: `internal` is "the recorder has a bug", which a window
        // the user minimised is not, and `target_not_found` would be a lie —
        // the window was found. `target_not_capturable` is the code that means
        // "change the window", and the sentence naming the window is carried
        // through verbatim because only this process knows what it is called
        // (issue #383).
        let refusal = unrecordable_target(crate::record::RecordError::TargetMinimised {
            window: "Clipped video pattern (video-pattern.exe)".to_owned(),
        });

        assert_eq!(
            refusal.code,
            ErrorCode::TargetNotCapturable,
            "the desktop cannot tell the user to restore a window it was given \
             {:?} for",
            refusal.code
        );
        assert!(
            refusal
                .message
                .contains("Clipped video pattern (video-pattern.exe)"),
            "the refusal must name the window: {}",
            refusal.message
        );
    }

    #[test]
    fn a_recording_that_failed_for_some_other_reason_is_still_an_internal_refusal() {
        // The other direction, and what makes the test above mean something:
        // without it a mapping that answered `target_not_capturable` to
        // everything would pass just as well, and the desktop would tell
        // somebody whose disk filled up to restore their window.
        let refusal = unrecordable_target(crate::record::RecordError::Session(
            clipped_session::SessionError::NoFrames,
        ));

        assert_eq!(refusal.code, ErrorCode::Internal);
    }

    /// A recording the automatic recorder would have handed over, and the two
    /// things its driver keeps a hold of.
    ///
    /// Through `adopt`, never a `Running { … }` literal, for the reason
    /// `started_recording` gives about the other kind: a test that assembled
    /// the fields itself would be testing a recording this recorder cannot
    /// make.
    fn adopted_recording(
        state: &Arc<RecordingState>,
        output: &Path,
        position: Option<Duration>,
    ) -> (
        Adopted,
        RecordingProgress,
        crate::shutdown::ShutdownSignal,
        Arc<AtomicBool>,
    ) {
        let progress = RecordingProgress::new();
        if let Some(position) = position {
            progress.reached(position);
        }
        let stop = crate::shutdown::ShutdownSignal::new();
        let asked_to_stop = Arc::new(AtomicBool::new(false));
        let adopted = state
            .adopt(
                output,
                "A Test Game".to_owned(),
                &progress,
                &stop,
                &asked_to_stop,
            )
            .expect("nothing else is being recorded");

        (adopted, progress, stop, asked_to_stop)
    }

    #[test]
    fn a_recording_detection_started_is_marked_by_the_same_bookmark_a_button_takes() {
        // Issue #421's first acceptance criterion, at the layer that decides
        // it. `add_bookmark` is answered against whatever is in `current`, and
        // before this a recording `watch` made was never in there — so a
        // recording nobody had to start was one nothing could mark. There is no
        // second bookmark implementation here and there must not be: this is
        // `RecordingState::bookmark`, the one a press and a button both reach.
        let directory = scratch("adopted-bookmark");
        let output = directory.join("clipped-a-test-game.mkv");
        let state = idle_state(&directory);
        let (adopted, _progress, _stop, _asked) =
            adopted_recording(&state, &output, Some(Duration::from_secs(120)));

        let RecorderStatus::Recording(active) = state.status() else {
            panic!("a recording that has been handed over is one this recorder is running");
        };
        assert_eq!(active.target, "A Test Game");
        assert_eq!(active.output, output.to_string_lossy());
        assert!(
            active.replay_seconds.is_none(),
            "an automatic recording keeps no buffer, so nothing may offer Save Replay for it"
        );

        let summary = state
            .bookmark(&AddBookmark::default(), moment())
            .expect("a bookmark can be taken in a recording detection started");

        assert_eq!(summary.recording_id, active.recording_id);
        assert_eq!(summary.pressed_at_seconds, 120.0);
        assert_eq!(summary.at_seconds, 120.0 - DEFAULT_LEAD.as_secs_f64());
        let read = BookmarkFile::for_recording(&output)
            .expect("the bookmark is on disk by the time the reply is built");
        assert_eq!(read.bookmarks.len(), 1);

        drop(adopted);
    }

    #[test]
    fn a_recording_handed_back_leaves_the_recorder_idle_however_it_ended() {
        // The half that keeps the recorder honest afterwards. A recording left
        // in `current` with no outcome would have `stop_recording` waiting for
        // one for ever, and `get_status` claiming a recording that ended
        // minutes ago — so `Adopted` releases itself on drop, which is the path
        // a panicking recording thread takes.
        let directory = scratch("adopted-dropped");
        let output = directory.join("clipped-a-test-game.mkv");
        let state = idle_state(&directory);
        let (adopted, _progress, _stop, _asked) =
            adopted_recording(&state, &output, Some(Duration::from_secs(5)));

        drop(adopted);

        assert!(
            matches!(state.status(), RecorderStatus::Idle),
            "a recording nobody is making must not be reported as one that is"
        );
        let error = state
            .bookmark(&AddBookmark::default(), moment())
            .expect_err("the recording has ended, so there is no moment to mark");
        assert_eq!(error.code, ErrorCode::NotRecording);
    }

    #[test]
    fn a_game_launching_while_somebody_is_recording_by_hand_is_refused_the_encoder() {
        // One recording at a time is this process's rule whoever asked for it,
        // and the person who pressed record is the one looking at the screen.
        // The refusal is what the session's record keeps, so the sitting says
        // it got no footage rather than silently having none.
        let directory = scratch("adopted-busy");
        let manual = directory.join("clipped-cs2.mkv");
        let automatic = directory.join("clipped-a-test-game.mkv");
        let state = recording_at(&manual, Some(Duration::from_secs(5)));

        let refusal = state
            .adopt(
                &automatic,
                "A Test Game".to_owned(),
                &RecordingProgress::new(),
                &crate::shutdown::ShutdownSignal::new(),
                &Arc::new(AtomicBool::new(false)),
            )
            .expect_err("this recorder is already recording something");

        assert!(
            refusal.contains("already recording"),
            "the sitting's record has to say why it got nothing: {refusal}"
        );
    }

    #[test]
    fn stopping_an_automatic_recording_tells_its_driver_before_it_stops_the_file() {
        // The ordering the whole stop rests on. The driver reads this flag once
        // round its loop *before* it collects a finished recording, so raising
        // the stop signal first would let the recording end, be collected, and
        // be followed by another recording of the same game five seconds later
        // — a Stop button that undoes itself (AGENTS.md section 27).
        let directory = scratch("adopted-stop");
        let output = directory.join("clipped-a-test-game.mkv");
        let state = idle_state(&directory);
        let (adopted, _progress, stop, asked_to_stop) =
            adopted_recording(&state, &output, Some(Duration::from_secs(5)));

        let stopping = {
            let state = Arc::clone(&state);
            thread::spawn(move || state.stop(None))
        };

        // The signal is raised second, so seeing it means the flag has already
        // been raised. A build that raised them the other way round fails here
        // rather than intermittently.
        while !stop.is_requested() {
            thread::yield_now();
        }
        assert!(
            asked_to_stop.load(Ordering::SeqCst),
            "the driver has to be told the user asked, before the recording it made can end"
        );

        adopted.finished(&RecordingOutcome::Failed {
            detail: "the stand-in recording reports a failure".to_owned(),
        });
        let outcome = stopping.join().expect("the stopping thread does not panic");
        assert_eq!(
            outcome.expect_err("the recording failed").code,
            ErrorCode::RecordingFailed,
            "and whoever asked for the stop is told what became of the file"
        );
    }

    #[test]
    fn a_replay_asked_for_of_an_automatic_recording_is_refused_rather_than_taken() {
        // An automatic recording keeps no buffer: `start_recording`'s `replay`
        // is what asks for one (issue #427) and nothing asks on detection's
        // behalf, so `save_replay` has to refuse it in the same words it
        // refuses a window-started recording without one, and must not reach
        // for a session it does not have.
        let directory = scratch("adopted-replay");
        let output = directory.join("clipped-a-test-game.mkv");
        let state = idle_state(&directory);
        let (adopted, _progress, _stop, _asked) =
            adopted_recording(&state, &output, Some(Duration::from_secs(30)));

        let error = state
            .save_replay(&SaveReplay::default(), moment())
            .expect_err("this recording keeps no buffer");

        assert_eq!(error.code, ErrorCode::NotRecording);
        assert!(
            error.message.contains("replay_seconds"),
            "the refusal has to name what to ask for instead: {}",
            error.message
        );

        drop(adopted);
    }
}
