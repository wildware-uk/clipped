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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime};

use clipped_game_detection::catalogue::Catalogue;
use clipped_game_detection::launcher::Launchers;
use clipped_ipc::{
    features, ActiveRecording, AddBookmark, BookmarkSummary, Command, CommandHandler, EndReason,
    Endpoint, EventPublisher, HotkeyBinding, ProtocolError, RecorderStatus, RecordingSummary,
    ReplaySummary, Reply, SaveReplay, ScreenshotSummary, Server, ServerError, StartRecording,
    StopRecording, TakeScreenshot, TransportError,
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
    CaptureTargetSettings, RecordingProgress, RecordingReport, RecordingSettings, ReplayRecording,
    ReplaySaveError,
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
        // And for this before it draws a hotkey list, so that a recorder built
        // before issue #232 — which registered nothing at all — is told apart
        // from a machine on which every combination registered cleanly. The two
        // are opposite answers and the second is what an empty list looks like.
        features::HOTKEYS.to_owned(),
        // And for this before it offers "Save Replay": a recorder built before
        // issue #38 parses `save_replay` and always refuses it, so the feature
        // is what tells an unusable button from a working one. Whether *this*
        // recording has a buffer to save from is
        // `ActiveRecording::replay_seconds`.
        features::REPLAY.to_owned(),
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
        service.configuration(),
    );
    service.publish_hotkeys(registered);

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
}

impl RecorderService {
    /// A service with nothing recording, over the library at Clipped's usual
    /// place.
    #[must_use]
    pub fn new(events: EventPublisher) -> Self {
        Self::over(
            events,
            LibraryReader::for_this_user(),
            // The storage limits come from the same settings file the recording
            // settings do, read once here. Without them the indexer sweeps
            // nothing, which is what an unconfigured machine gets (issue #111).
            LibraryIndexer::for_this_user().with_storage(
                crate::watch::load_configuration(
                    clipped_session::config::ConfigurationStore::default_path().as_deref(),
                )
                .storage()
                .clone(),
            ),
            // The same file `watch` reads, through the same function, so that
            // "what does this record at" has one answer whichever subcommand is
            // asking (AGENTS.md sections 30 and 55). Read once, here: a
            // recording resolves what it is made with when it starts, and
            // nothing re-reads a file underneath a running encoder (issue #61).
            crate::watch::load_configuration(
                clipped_session::config::ConfigurationStore::default_path().as_deref(),
            ),
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
            Configuration::defaults(),
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
        configuration: Configuration,
        catalogue: Catalogue,
        launchers: Launchers,
    ) -> Self {
        let indexer = Arc::new(indexer);
        Self {
            recordings: Arc::new(RecordingState::new(
                events,
                Arc::clone(&indexer),
                configuration,
                catalogue,
                launchers,
            )),
            library,
            indexer,
            hotkeys: OnceLock::new(),
        }
    }

    /// The settings this process resolved when it started.
    ///
    /// Read once, here, for the reason [`Self::new`] gives: a recording resolves
    /// what it is made with when it starts, and nothing re-reads a file
    /// underneath a running encoder. `serve` asks for it to resolve the hotkey
    /// bindings, so that "what is Save Replay bound to" has one answer in this
    /// process (AGENTS.md section 30).
    #[must_use]
    pub fn configuration(&self) -> &Configuration {
        &self.recordings.configuration
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
                export: crate::export::export(&request)?,
            }),
            // Answered from what registration produced when this process
            // started, which is a clone of a small `Vec` and touches nothing a
            // recording touches (`crate::hotkeys`, issue #232).
            Command::GetHotkeys => Ok(Reply::Hotkeys {
                hotkeys: self.hotkeys()?,
            }),
            // Refused by `clipped-ipc` before dispatch, so that no handler can
            // answer a command whose subsystem does not exist (AGENTS.md
            // section 54). Reaching here would be a bug in that refusal.
            Command::Unbuilt(unbuilt) => Err(unbuilt.refusal()),
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

    fn features(&self) -> Vec<String> {
        features_of_this_build()
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
struct RecordingState {
    current: Mutex<Option<Running>>,
    /// Signalled when a recording's thread has stored its outcome.
    finished: Condvar,
    events: EventPublisher,
    next_id: AtomicU64,
    /// Asked to bring the library up to date once a recording's session record
    /// is final (`crate::library`, issue #402).
    indexer: Arc<LibraryIndexer>,
    /// The user's settings, as they stood when this process started
    /// (`RecorderService::new`).
    ///
    /// Held rather than re-read, exactly as `watch` holds them: a recording
    /// resolves what it is made with when it starts, and nothing re-reads a file
    /// underneath a running encoder (`clipped_session::config`, issue #61).
    configuration: Configuration,
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
    /// The rolling window of the last few minutes, when this recording was
    /// asked for one.
    ///
    /// [`None`] for an ordinary recording, and that is what `save_replay`
    /// refuses on: a buffer costs memory in proportion to its duration
    /// (`docs/replay-buffer.md`), so one is kept only when somebody asked for
    /// it. Shared for the reason the session is — a save runs on the connection
    /// thread while this one carries on recording.
    replay: Option<Arc<ReplayRecording>>,
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

    /// The session this recording is the whole of, while it is still running.
    ///
    /// # Panics
    ///
    /// If the recording has already ended: [`RecordingState::finish`] takes the
    /// session to close it, and nothing asks a recording that has stopped what
    /// it is recording into.
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
        configuration: Configuration,
        catalogue: Catalogue,
        launchers: Launchers,
    ) -> Self {
        Self {
            current: Mutex::new(None),
            finished: Condvar::new(),
            events,
            next_id: AtomicU64::new(1),
            indexer,
            configuration,
            catalogue,
            launchers,
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
            &self.configuration,
            &self.catalogue,
            &self.launchers,
            process,
            now,
        );

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
            session: Some(Arc::new(Mutex::new(session))),
            // Attached afterwards by [`Running::with_replay`], because it is the
            // one thing about a recording that is optional.
            replay: None,
            outcome: None,
        }
    }

    /// Validates the request, resolves the target, opens a session and starts
    /// recording.
    fn start(self: &Arc<Self>, request: &StartRecording) -> Result<Reply, ProtocolError> {
        let args = record_args(request)?;
        let config = RecordingConfig::resolve(&args).map_err(invalid_parameters)?;
        // Before the window is resolved and before anything is created: a
        // duration no buffer can hold is a parameter to fix, and finding that
        // out after a capture session has opened would be finding it out late
        // (AGENTS.md section 45).
        let replay = replay_for(request)?;
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
        let output = asked_for.output().to_path_buf();
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
                SystemTime::now(),
            )
            .with_replay(replay);

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
            running.progress.clone(),
            running.screenshots.clone(),
            running.replay.clone(),
        ));

        *current = Some(running);
        let status = status_of(current.as_ref());
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
                    "this recording is not keeping a replay buffer; start one with                      `replay_seconds` to be able to save from it",
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
        status_of(current.as_ref())
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
            let ended = {
                let mut session = session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                session
                    .finish_in_place(&for_the_session, SystemTime::now())
                    .id()
                    .as_str()
                    .to_owned()
            };

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
    progress: RecordingProgress,
    screenshots: ScreenshotRequests,
    replay: Option<Arc<ReplayRecording>>,
) -> JoinHandle<()> {
    let state = Arc::clone(state);
    let id = id.to_owned();

    thread::Builder::new()
        .name("clipped-recording".to_owned())
        .spawn(move || {
            let mut outputs = clipped_session::RecordingOutputs::default()
                .with_progress(&progress)
                .with_screenshots(&screenshots);
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
fn status_of(running: Option<&Running>) -> RecorderStatus {
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
        }),
        _ => RecorderStatus::Idle,
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

/// The replay buffer a `start_recording` asked for, if it asked for one.
///
/// The bound is `clipped-replay`'s own and the message is its own, so that the
/// duration a window may ask for over the protocol and the one
/// `replay --duration` accepts are the same range with the same explanation
/// (AGENTS.md section 55).
fn replay_for(request: &StartRecording) -> Result<Option<Arc<ReplayRecording>>, ProtocolError> {
    let Some(seconds) = request.replay_seconds else {
        return Ok(None);
    };

    ReplayRecording::new(std::time::Duration::from_secs(u64::from(seconds)))
        .map(|replay| Some(Arc::new(replay)))
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
        output: report.output().to_string_lossy().into_owned(),
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
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    use clipped_game_detection::catalogue::EntrySource;
    use clipped_session::bookmarks::{BookmarkFile, DEFAULT_LEAD};
    use clipped_windows::{MonitorHandle, PixelSize, WindowGeometry, WindowHandle};

    use super::*;

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

    /// A directory of this test's own; several of these run at once.
    fn scratch(name: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let directory = std::env::temp_dir().join(format!(
            "clipped-serve-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
        directory
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
        Arc::new(RecordingState::new(
            EventPublisher::new(),
            Arc::new(indexer_over(directory)),
            Configuration::defaults(),
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
                moment(),
            )
            .with_replay(replay);
        if let Some(position) = position {
            running.progress.reached(position);
        }
        running
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

        let _ = std::fs::remove_dir_all(&directory);
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

        let _ = std::fs::remove_dir_all(&directory);
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

        let _ = std::fs::remove_dir_all(&directory);
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

        let _ = std::fs::remove_dir_all(&directory);
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

        let _ = std::fs::remove_dir_all(&directory);
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
        assert!(replay_for(&StartRecording::default())
            .expect("no buffer asked for")
            .is_none());

        let asked = replay_for(&StartRecording {
            replay_seconds: Some(60),
            ..StartRecording::default()
        })
        .expect("a minute is in range")
        .expect("a buffer was asked for");
        assert_eq!(asked.window(), Duration::from_secs(60));

        let error = replay_for(&StartRecording {
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

        let _ = std::fs::remove_dir_all(&directory);
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

        let _ = std::fs::remove_dir_all(&directory);
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

        let _ = std::fs::remove_dir_all(&directory);
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

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A service over a library holding one sitting whose file has gone.
    ///
    /// The library is built here rather than reconciled, because what is under
    /// test is the path from a command to a reply, not indexing.
    fn service_over_a_library(name: &str) -> RecorderService {
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

        RecorderService::with_library(
            EventPublisher::new(),
            LibraryReader::at(Some(path)),
            indexer_over(&directory),
            Catalogue::default(),
        )
    }

    #[test]
    fn the_library_commands_are_answered_from_the_index_through_the_real_dispatch() {
        // Deliberately through `CommandHandler::call` rather than through
        // `LibraryReader` beside it: what issue #301 is about is a command
        // reaching the index at all, and a reader that works while nothing
        // routes a command to it is the gap this ticket exists to close. A
        // command wired to the wrong handler, or refused before dispatch, fails
        // here and nowhere else.
        let service = service_over_a_library("library");

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
    fn a_recording_with_no_events_is_answered_as_none_rather_than_refused() {
        // "None" and "nobody asked" are different things to draw, and the
        // Editor screen says them differently. An empty lane is the first; a
        // refusal would make the window guess at the second.
        let service = service_over_a_library("library-events-none");

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
        let service = service_over_a_library("library-events-bad-id");

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
        let _ = std::fs::remove_dir_all(&directory);
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
        let _ = std::fs::remove_dir_all(&directory);
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

        let _ = std::fs::remove_dir_all(&directory);
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
        let _ = std::fs::remove_dir_all(&directory);
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
        let _ = std::fs::remove_dir_all(&directory);
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
        let _ = std::fs::remove_dir_all(&directory);
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
    fn an_export_is_routed_to_the_muxer_through_the_real_dispatch() {
        // Deliberately through `CommandHandler::call` rather than through
        // `crate::export` beside it. What issue #399 is about is a command
        // reaching the muxer at all; an export function that works while
        // nothing routes a command to it is exactly the gap this ticket exists
        // to close, and a command wired to the wrong handler — or left in
        // `UNBUILT_COMMANDS` and refused before dispatch — fails here and
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

        let _ = std::fs::remove_dir_all(&directory);
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
}
