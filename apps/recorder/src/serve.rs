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
//! Start a recording, stop one, mark a moment in one, and say what it is doing.
//! The recording is the one `record` makes, through the same `clipped-session`
//! call, validated by the same code (AGENTS.md section 55). Every other command
//! in the protocol belongs to a subsystem that is not built, and `clipped-ipc`
//! refuses those before they reach this module at all, with the milestone and
//! issue that build them.
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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime};

use clipped_ipc::{
    features, ActiveRecording, AddBookmark, BookmarkSummary, Command, CommandHandler, EndReason,
    Endpoint, EventPublisher, ProtocolError, RecorderStatus, RecordingSummary, Reply, Server,
    ServerError, StartRecording, StopRecording, TransportError,
};
use clipped_ipc::{ErrorCode, PeerIdentity};
use clipped_logging::RedactedPath;
use clipped_session::bookmarks::{BookmarkError, BookmarkLog, BookmarkRequest};
use clipped_session::{RecordingProgress, RecordingReport, RecordingSettings};

use crate::cli::{RecordArgs, ServeArgs};
use crate::config::{ConfigError, RecordingConfig};
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

    println!("ready endpoint={}", endpoint.path());
    // Flushed explicitly: whatever started this process is very likely blocked
    // reading that line, and standard output to a pipe is buffered.
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    let server = Server::new(Arc::clone(&service), events.clone(), identity());
    let outcome = server.serve(&mut listener).map_err(ServeError::Serving);

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
}

impl RecorderService {
    /// A service with nothing recording.
    #[must_use]
    pub fn new(events: EventPublisher) -> Self {
        Self {
            recordings: Arc::new(RecordingState::new(events)),
        }
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
    /// [`None`] while it is still recording.
    outcome: Option<Result<RecordingReport, String>>,
}

impl RecordingState {
    fn new(events: EventPublisher) -> Self {
        Self {
            current: Mutex::new(None),
            finished: Condvar::new(),
            events,
            next_id: AtomicU64::new(1),
        }
    }

    /// Validates the request, resolves the target, and starts recording.
    fn start(self: &Arc<Self>, request: &StartRecording) -> Result<Reply, ProtocolError> {
        let args = record_args(request)?;
        let config = RecordingConfig::resolve(&args).map_err(invalid_parameters)?;
        let window = resolve_window(&config.target).map_err(|error| match error {
            crate::record::RecordError::Resolution(resolution) => {
                ProtocolError::new(ErrorCode::TargetNotFound, resolution.to_string())
            }
            other => ProtocolError::new(ErrorCode::Internal, other.to_string()),
        })?;
        let settings = settings_for(&config, &window);

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
        let stop = crate::shutdown::ShutdownSignal::new();
        let output = settings.output().to_path_buf();
        let target = config.target.to_string();

        tracing::info!(
            recording = id,
            target = target,
            output = %RedactedPath::new(&output),
            "starting a recording because the desktop application asked for one"
        );

        let progress = RecordingProgress::new();
        let thread = spawn_recording(self, &id, settings, stop.clone(), progress.clone());

        *current = Some(Running {
            id: id.clone(),
            output: output.clone(),
            target,
            started: Instant::now(),
            stop,
            thread: Some(thread),
            progress,
            bookmarks: Arc::new(BookmarkLog::for_recording(&output)),
            outcome: None,
        });
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

        match self.current.lock() {
            Ok(mut current) => {
                if let Some(running) = current.as_mut() {
                    if running.id == id {
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
) -> JoinHandle<()> {
    let state = Arc::clone(state);
    let id = id.to_owned();

    thread::Builder::new()
        .name("clipped-recording".to_owned())
        .spawn(move || {
            let outputs = clipped_session::RecordingOutputs::default().with_progress(&progress);
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

/// The status a recording state describes.
fn status_of(running: Option<&Running>) -> RecorderStatus {
    match running {
        Some(running) if running.outcome.is_none() => RecorderStatus::Recording(ActiveRecording {
            recording_id: running.id.clone(),
            output: running.output.to_string_lossy().into_owned(),
            target: running.target.clone(),
            elapsed_ms: u64::try_from(running.started.elapsed().as_millis()).unwrap_or(u64::MAX),
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

    use clipped_session::bookmarks::{BookmarkFile, DEFAULT_LEAD};

    use super::*;

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

    /// A state holding a recording that has reached `position`.
    ///
    /// Built by hand rather than by starting one: what is under test is the
    /// bookmark path, which needs a recording position and an output path and
    /// nothing else. Capturing a real window would need a desktop, a GPU and an
    /// encoder to test arithmetic and a file write.
    fn recording_at(output: &Path, position: Option<Duration>) -> Arc<RecordingState> {
        let state = Arc::new(RecordingState::new(EventPublisher::new()));
        let progress = RecordingProgress::new();
        if let Some(position) = position {
            progress.reached(position);
        }

        *state.current.lock().expect("a fresh lock") = Some(Running {
            id: "r-1".to_owned(),
            output: output.to_path_buf(),
            target: "process cs2.exe".to_owned(),
            started: Instant::now(),
            stop: crate::shutdown::ShutdownSignal::new(),
            thread: None,
            progress,
            bookmarks: Arc::new(BookmarkLog::for_recording(output)),
            outcome: None,
        });
        state
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

        let idle = Arc::new(RecordingState::new(EventPublisher::new()));
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

    #[test]
    fn a_recorder_that_can_bookmark_says_so_in_its_handshake() {
        // A UI decides whether to offer the control by asking here rather than
        // by inferring it from a version, so a build that stores bookmarks and
        // does not advertise them is one whose tray never offers the item.
        assert!(features_of_this_build().contains(&clipped_ipc::features::BOOKMARKS.to_owned()));
    }
}
