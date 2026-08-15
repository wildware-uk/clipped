//! Who starts the recorder, who notices when it has gone, and how each side is
//! kept to one.
//!
//! The rest of this crate is what the two processes *say* to each other. This
//! module is the other half of the same seam: whether the other one is running
//! at all. [ADR 0002](../../../docs/adr/0002-separate-recorder-process.md) made
//! the recorder a process with its own lifetime and listed the consequence in as
//! many words — "something has to supervise the recorder: start it if it is not
//! running, notice if it dies, and decide whether to restart it mid-session"
//! ([issue #106](https://github.com/wildware-uk/clipped/issues/106)).
//! [ADR 0006](../../../docs/adr/0006-recorder-lifetime-and-supervision.md) is
//! the decision this module implements.
//!
//! # The shape of it
//!
//! ```text
//!  desktop application                         recorder
//!  ───────────────────                         ────────
//!  claim Local\clipped-desktop.<session> ──▶   (one UI per sign-in session)
//!  connect to the endpoint ──────────────▶     already listening? attach
//!    nothing there → spawn detached ─────▶     serve, owning the endpoint
//!  subscribe to events ──────────────────▶     status, errors
//!    the connection ends ◀───────────────      the recorder is gone
//!    reattach, or restart with backoff
//!  the window closes  ────────────────────     keeps recording
//! ```
//!
//! Three properties are worth stating because each is a decision rather than an
//! implementation detail.
//!
//! **The recorder is started detached and is never stopped by accident.** It is
//! spawned with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` and with its
//! standard streams pointed at nothing, so closing, quitting or killing the
//! process that started it does not reach it. [`RecorderLink::stop`] stops
//! *watching*; it does not stop the recorder. That is the whole of AGENTS.md
//! section 17 as it applies here: a UI failure must not destroy an active
//! recording, and the surest way to guarantee that is for the UI to own no part
//! of the recorder's lifetime after the moment it starts it.
//!
//! Stopping it **on purpose** is a different thing and used to be impossible
//! ([issue #220](https://github.com/wildware-uk/clipped/issues/220)): a process
//! with no console cannot be sent Ctrl+C, so the only way to end a recorder
//! started here was Task Manager. [`RecorderLink::shut_down_recorder`] is the
//! way, and it goes over the protocol rather than at the process — which is
//! what makes "finish the recording first" possible at all.
//!
//! **One recorder per endpoint is enforced by the endpoint**, not by anything
//! here. `FILE_FLAG_FIRST_PIPE_INSTANCE` means the second `serve` on a name
//! fails at once (`transport/windows.rs`), so two supervisors that decide at the
//! same instant that nothing is running produce one serving recorder and one
//! that exits saying the name was taken. [`ensure_recorder`] therefore does not
//! need a lock of its own, and it reports which of the two happened rather than
//! papering over it — see [`AttachmentOrigin::LostTheRace`].
//!
//! **One UI per sign-in session is a separate problem with a separate answer.**
//! Nothing about a pipe stops a second window opening, and a second window would
//! be a second supervisor with its own restart budget. [`SingleInstance`] is a
//! named mutex, which is the mechanism Windows releases when a process dies
//! however it died — so a UI that crashed does not lock the next launch out.
//!
//! # What "recovery" means here, and what it does not
//!
//! When a recorder is killed while recording, the file it was writing is a
//! playable recording of everything up to a second or so before the kill
//! ([ADR 0001](../../../docs/adr/0001-mkv-archival-container.md),
//! `docs/muxing.md`, which measures the loss). Recovery in this module is
//! therefore: **notice, name the file, and say it was not resumed**
//! ([`RecorderLinkEvent::RecordingInterrupted`]). A replacement recorder is a
//! new process with no capture session, no encoder and no open container; it
//! cannot continue writing a file another process left, and resuming into a
//! *second* file would be a recording the user did not ask for, beginning at a
//! moment they cannot see. Repairing the interrupted file's index is a library
//! concern and is not attempted here.
//!
//! # Threading
//!
//! [`ensure_recorder`] blocks on the thread that calls it, for at most
//! [`SupervisorSettings::startup_timeout`]. [`RecorderLink::start`] returns at
//! once and does its work on a thread of its own, which is where every
//! connection this module makes lives — none of it belongs on a thread drawing a
//! window (`client.rs`).

mod instance;
mod link;
mod platform;

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use instance::{claim_instance, InstanceClaim, InstanceError, SingleInstance};
pub use link::{
    RecorderCallError, RecorderLink, RecorderLinkEvent, RecorderLinkState, ShutdownOutcome,
};

use crate::client::{Client, ClientError};
use crate::error::ProtocolError;
use crate::message::PeerIdentity;
use crate::transport::{Endpoint, TransportError};

/// The subcommand the recorder is started with.
const SERVE: &str = "serve";

/// How long a probe waits to find out whether a recorder is already listening.
///
/// Zero: the endpoint either exists or it does not, and `connect` answers that
/// in one `CreateFile`. Waiting would only be useful if a recorder were about to
/// appear, and the case where one is about to appear is the one this module
/// creates for itself and waits for explicitly.
const PROBE: Duration = Duration::ZERO;

/// How long to wait between attempts to reach a recorder that is starting.
const STARTUP_POLL: Duration = Duration::from_millis(25);

/// `STATUS_DLL_NOT_FOUND`: a library the executable imports was not found, so
/// Windows ended the process before its first instruction.
///
/// This is the exit code of a recorder installed on a machine that is missing
/// something it needs beside it, or missing the Microsoft Visual C++
/// redistributable from the operating system
/// ([issue #407](https://github.com/wildware-uk/clipped/issues/407),
/// [ADR 0007](../../../docs/adr/0007-visual-c-runtime-linkage.md)). The failure
/// is entirely the loader's: `main` never runs, so no log file is opened and
/// nothing the recorder could have said is said.
const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;

/// `STATUS_ENTRYPOINT_NOT_FOUND`: a library was found but does not contain a
/// function the executable imports.
///
/// The same failure with a different cause — a half-replaced install, or a
/// recorder beside FFmpeg libraries from a different pin — and the same
/// symptoms, which is why it is reported the same way. Windows shows a user the
/// same dialog for both.
const STATUS_ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;

/// What a supervisor needs in order to find, start and watch a recorder.
#[derive(Debug, Clone)]
pub struct SupervisorSettings {
    /// Where the recorder listens.
    ///
    /// Normally [`Endpoint::for_this_session`]. It is passed to the recorder as
    /// `--endpoint` rather than left to the default, so that the name the
    /// supervisor connects to and the name the recorder binds are the same
    /// string rather than two computations of it.
    pub endpoint: Endpoint,
    /// The `clipped-recorder` executable to start.
    ///
    /// Resolved by the caller, because where it is depends on how the
    /// application was installed and this crate has no business guessing.
    pub executable: PathBuf,
    /// How this process introduces itself in a handshake.
    pub client: PeerIdentity,
    /// How long a recorder that has just been started is given to begin
    /// listening before it is called a failure.
    pub startup_timeout: Duration,
    /// What to do when a recorder stops without being asked to.
    pub restart: RestartPolicy,
}

impl SupervisorSettings {
    /// Settings for `executable` at `endpoint`, with the defaults for
    /// everything else.
    #[must_use]
    pub fn new(endpoint: Endpoint, executable: PathBuf, client: PeerIdentity) -> Self {
        Self {
            endpoint,
            executable,
            client,
            startup_timeout: Duration::from_secs(10),
            restart: RestartPolicy::default(),
        }
    }
}

/// What to do when a recorder stops without being asked to.
///
/// The shape of this type is the decision: a bounded number of attempts, each
/// further apart than the last, and a counter that resets once a recorder has
/// stayed up long enough to be called working. A recorder that fails on startup
/// — a corrupt configuration, a graphics device that is not there — fails again
/// immediately, and a supervisor with no bound would restart it for ever,
/// burning CPU beside a game and filling a log with the same line. Staying down
/// and saying so is the better failure (AGENTS.md sections 16 and 45): it is
/// visible, and it leaves the user an action.
///
/// A failure that is *certain* to repeat spends none of the budget:
/// [`SupervisorError::NotLoadable`] and its neighbours are reported at the first
/// attempt (`supervisor::link::worth_trying_again`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    /// How many consecutive replacements to start before giving up.
    pub attempts: u32,
    /// How long to wait before the first replacement.
    pub first_delay: Duration,
    /// The longest wait between replacements, once doubling has reached it.
    pub maximum_delay: Duration,
    /// How long a recorder must stay reachable before the attempt counter is
    /// reset.
    ///
    /// Without this, a machine that loses its recorder once a day would exhaust
    /// [`attempts`](Self::attempts) after four days and never restart again. A
    /// recorder that ran for a minute is not in a crash loop.
    pub settled_after: Duration,
}

impl Default for RestartPolicy {
    /// Four attempts at 1, 2, 4 and 8 seconds, resetting after a minute up.
    ///
    /// Fifteen seconds in total, which is short enough that a transient failure
    /// is over before a user has read the message about it, and few enough that
    /// a permanent one is reported rather than retried into the evening.
    fn default() -> Self {
        Self {
            attempts: 4,
            first_delay: Duration::from_secs(1),
            maximum_delay: Duration::from_secs(30),
            settled_after: Duration::from_secs(60),
        }
    }
}

impl RestartPolicy {
    /// Never start a replacement; report the loss and stop there.
    pub const NEVER: Self = Self {
        attempts: 0,
        first_delay: Duration::ZERO,
        maximum_delay: Duration::ZERO,
        settled_after: Duration::ZERO,
    };

    /// How long to wait before consecutive attempt number `attempt`, counting
    /// from one, or [`None`] once the attempts are used up.
    ///
    /// The delay doubles and is capped at
    /// [`maximum_delay`](Self::maximum_delay). Doubling is what separates "the
    /// recorder was momentarily not there" from "the recorder cannot start":
    /// the first case is fixed by the first attempt and the user sees a second
    /// of nothing, and the second case is not being retried forty times a
    /// second while the message about it is being read.
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

/// Which recorder this process ended up talking to, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// The process identifier of the recorder serving the endpoint.
    ///
    /// Read from the pipe itself rather than from anything this process
    /// remembers, so it names the recorder that is actually being talked to.
    pub recorder_process_id: u32,
    /// What that recorder said it can do, from its `Welcome`.
    ///
    /// Carried rather than looked up later, because it describes **this**
    /// recorder: a window that asked again after a restart could be told about
    /// a different build from the one it is attached to. See
    /// [`crate::features`] for the names.
    pub features: Vec<String>,
    /// How this process came to be talking to it.
    pub origin: AttachmentOrigin,
}

/// How a supervisor came to be talking to the recorder it is talking to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentOrigin {
    /// A recorder was already listening. This process started nothing.
    ///
    /// The ordinary case for a second window, and for the first window on a
    /// machine where the recorder starts at login.
    Existing,
    /// This process started the recorder it is talking to.
    Started,
    /// This process started a recorder, and another that was starting at the
    /// same moment took the endpoint first.
    ///
    /// The one started here exited saying the endpoint was taken, which is the
    /// transport refusing to let two recorders serve one name
    /// (`transport/windows.rs`), and the one being talked to is the winner. It
    /// is reported rather than smoothed over because "a recorder was started
    /// here" and "a recorder is running" are different facts, and a supervisor
    /// that conflated them would count somebody else's recorder as its own.
    LostTheRace {
        /// The process that was started here and exited.
        started_process_id: u32,
    },
}

/// Why a recorder could not be found or started.
#[derive(Debug)]
pub enum SupervisorError {
    /// There is no recorder executable where the settings said there would be.
    ///
    /// Checked before anything is spawned so that the message names the path
    /// rather than reporting a spawn failure from the operating system.
    ExecutableMissing {
        /// Where it was looked for.
        path: PathBuf,
    },
    /// Windows refused to load the recorder, so it never ran.
    ///
    /// Distinct from [`NeverListened`](Self::NeverListened), which it would
    /// otherwise be indistinguishable from — the process starts and is gone by
    /// the next poll — because the two need opposite things said about them. A
    /// recorder that started and exited has a log file to look in; a recorder
    /// the loader refused has none, because the refusal happens before its first
    /// instruction. Reporting the second as the first sends a user to an empty
    /// log directory and shows them an exit status of `-1073741515`
    /// ([issue #407](https://github.com/wildware-uk/clipped/issues/407)).
    ///
    /// It is not retried either: a library that is not on the machine does not
    /// arrive because the recorder was started again
    /// (`supervisor::link::worth_trying_again`).
    NotLoadable {
        /// The recorder Windows would not load.
        path: PathBuf,
        /// The `STATUS_` code it was ended with, kept so that the message names
        /// what a user would otherwise have to look up.
        status: u32,
    },
    /// The recorder executable could not be started.
    Spawn {
        /// What was being started.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// A recorder was started and never began listening.
    NeverListened {
        /// Where it was expected.
        endpoint: String,
        /// How long it was given.
        waited: Duration,
        /// Its exit code, if it exited rather than merely failing to listen.
        exit: Option<i32>,
    },
    /// A recorder is listening and will not speak to this build.
    ///
    /// Version skew: the recorder has been running since before an update, or
    /// the recorder was updated and this window was not. The refusal names
    /// every version the recorder does speak, so the message can say which side
    /// is behind (`docs/ipc.md`).
    Incompatible {
        /// The recorder's own refusal.
        refusal: ProtocolError,
    },
    /// Talking to the endpoint failed for a reason other than nothing being
    /// there.
    Connect {
        /// Where.
        endpoint: String,
        /// What went wrong.
        ///
        /// Boxed because a `ClientError` carries a whole refusal — a code, a
        /// message and its machine-readable detail — and this error is returned
        /// from every function in this module.
        source: Box<ClientError>,
    },
    /// This build cannot start a detached process, because it is not a Windows
    /// build.
    Unsupported,
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExecutableMissing { path } => write!(
                formatter,
                "the recorder was not found at {}, so no recording can be started",
                path.display()
            ),
            Self::NotLoadable { path, status } => write!(
                formatter,
                "Windows would not load the recorder at {} ({}, 0x{status:08X}), so it never ran \
                 and has logged nothing: a library it needs is missing or is the wrong build. \
                 Reinstalling Clipped replaces the libraries that belong beside it; a Clipped \
                 built elsewhere may also need the Microsoft Visual C++ 2015-2022 \
                 redistributable.",
                path.display(),
                loader_status_name(*status)
            ),
            Self::Spawn { path, source } => write!(
                formatter,
                "the recorder at {} could not be started: {source}",
                path.display()
            ),
            Self::NeverListened {
                endpoint,
                waited,
                exit: Some(code),
            } => write!(
                formatter,
                "the recorder exited with status {code} within {waited:?} without listening on \
                 {endpoint}; its diagnostics are in the Clipped log directory"
            ),
            Self::NeverListened {
                endpoint,
                waited,
                exit: None,
            } => write!(
                formatter,
                "the recorder was started but was not listening on {endpoint} after {waited:?}"
            ),
            Self::Incompatible { refusal } => write!(
                formatter,
                "the recorder that is running will not talk to this build: {refusal}"
            ),
            Self::Connect { endpoint, source } => {
                write!(formatter, "{endpoint} could not be reached: {source}")
            }
            Self::Unsupported => {
                formatter.write_str("this build cannot start a recorder; it is not a Windows build")
            }
        }
    }
}

impl std::error::Error for SupervisorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            Self::Incompatible { refusal } => Some(refusal),
            Self::Connect { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Whether an exit status is Windows saying it would not load the executable,
/// and which refusal it was.
///
/// The loader ends a process it refuses with the `NTSTATUS` describing why,
/// which arrives here as an exit code like any other: `STATUS_DLL_NOT_FOUND`
/// reaches [`std::process::ExitStatus::code`] as `-1073741515`, because the
/// status has its top bit set and the code is signed. That is the whole of the
/// difference between "the recorder failed" and "the recorder was never given
/// the chance to", and without recognising it the supervisor reports the second
/// as the first (issue #407).
///
/// Only the two loader refusals are recognised. A recorder that reached `main`
/// and exited is a different thing to say, whatever number it exited with.
fn loader_refusal(code: Option<i32>) -> Option<u32> {
    let status = code? as u32;
    match status {
        STATUS_DLL_NOT_FOUND | STATUS_ENTRYPOINT_NOT_FOUND => Some(status),
        _ => None,
    }
}

/// The name Windows gives a loader refusal, for a message that can be searched
/// for.
fn loader_status_name(status: u32) -> &'static str {
    match status {
        STATUS_ENTRYPOINT_NOT_FOUND => "STATUS_ENTRYPOINT_NOT_FOUND",
        // Every value reaching here has been through `loader_refusal`, and the
        // only other one it admits is this.
        _ => "STATUS_DLL_NOT_FOUND",
    }
}

/// Makes sure a recorder is listening on the endpoint, starting one if none is.
///
/// This is the whole of "deciding that no recorder is running and doing
/// something about it" that `docs/ipc.md` leaves to this issue. It attaches by
/// preference and starts by necessity, because a recorder that is already
/// running may be recording, and the only safe thing to do with a recording
/// somebody else's window started is to leave it alone.
///
/// The connection it makes to find out is dropped before returning. It is a
/// probe, not the link: [`RecorderLink`] opens its own.
///
/// # Errors
///
/// [`SupervisorError::ExecutableMissing`] or [`SupervisorError::Spawn`] if a
/// recorder had to be started and could not be, [`SupervisorError::NeverListened`]
/// if one was started and never appeared, [`SupervisorError::Incompatible`] if
/// the recorder that is running speaks a protocol version this build does not,
/// and [`SupervisorError::Connect`] for any other failure to reach the endpoint
/// — including one belonging to another account.
pub fn ensure_recorder(settings: &SupervisorSettings) -> Result<Attachment, SupervisorError> {
    if let Some((recorder_process_id, features)) = probe(settings)? {
        tracing::debug!(
            endpoint = %settings.endpoint,
            recorder = recorder_process_id,
            "a recorder is already listening, so this process started none"
        );
        return Ok(Attachment {
            recorder_process_id,
            features,
            origin: AttachmentOrigin::Existing,
        });
    }

    start_and_wait(settings)
}

/// The recorder serving the endpoint -- its process identifier and what it can
/// do -- or [`None`] if nothing is.
///
/// The features come from the same handshake as the identifier, because that is
/// the only place they are said: a recorder describes itself once, in its
/// `Welcome`. Reading them here rather than asking again later is what makes
/// them describe *this* recorder rather than whatever is listening by the time
/// somebody looks ([issue #447](https://github.com/wildware-uk/clipped/issues/447)).
///
/// A refused handshake still means a recorder is there, and is reported as
/// [`SupervisorError::Incompatible`] rather than as an absence: starting a
/// second recorder because the first one is too old would produce two recorders
/// racing for one endpoint, and the one that lost would be the one that is
/// recording.
fn probe(settings: &SupervisorSettings) -> Result<Option<(u32, Vec<String>)>, SupervisorError> {
    match Client::connect(
        &settings.endpoint,
        &settings.client.name,
        &settings.client.version,
        PROBE,
    ) {
        Ok(client) => match client.recorder_process_id() {
            Ok(process_id) => Ok(Some((process_id, client.welcome().features.clone()))),
            Err(error) => Err(SupervisorError::Connect {
                endpoint: settings.endpoint.path(),
                source: Box::new(ClientError::Transport(TransportError::Platform(error))),
            }),
        },
        Err(ClientError::Transport(TransportError::NotListening { .. })) => Ok(None),
        Err(ClientError::Refused(refusal)) => Err(SupervisorError::Incompatible { refusal }),
        Err(source) => Err(SupervisorError::Connect {
            endpoint: settings.endpoint.path(),
            source: Box::new(source),
        }),
    }
}

/// Waits for the endpoint to stop answering, and says whether it did.
///
/// This is the only proof available that a recorder asked to exit has finished.
/// The reply to `shutdown` is written *before* the recorder winds up — it has to
/// be, or it could not be written at all — so it says the shutdown was accepted
/// and nothing more. The endpoint disappearing is the recorder's last act: the
/// listener is closed, then any recording is stopped and its file finalised,
/// then the process ends and the operating system releases the pipe.
///
/// The wait therefore has to allow for finalising a file, which is why the
/// caller chooses the timeout rather than this function. `false` means the
/// recorder is still there after `timeout` — worth reporting, and not worth
/// killing anything over: a recorder still closing a container is doing the one
/// thing that must not be interrupted (AGENTS.md section 17).
///
/// It polls, because a process that is not this one's child cannot be waited
/// on and the pipe carries no closing event. Every 25 ms, off any path that
/// matters, for as long as somebody is watching a progress message.
#[must_use]
pub fn wait_for_recorder_to_exit(endpoint: &Endpoint, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;

    loop {
        match crate::transport::connect(endpoint, Duration::ZERO) {
            // Nothing is listening: the recorder has gone.
            Err(TransportError::NotListening { .. }) => return true,
            // Anything else — a connection that opened, or a failure that is
            // not an absence — means something is still there to answer.
            Ok(connection) => drop(connection),
            Err(_) => {}
        }

        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(STARTUP_POLL);
    }
}

/// Starts a recorder and waits for it to be reachable.
fn start_and_wait(settings: &SupervisorSettings) -> Result<Attachment, SupervisorError> {
    if !settings.executable.is_file() {
        return Err(SupervisorError::ExecutableMissing {
            path: settings.executable.clone(),
        });
    }

    let mut child = platform::spawn_detached(
        &settings.executable,
        &[SERVE, "--endpoint", settings.endpoint.name()],
    )?;
    let started_process_id = child.id();

    tracing::info!(
        endpoint = %settings.endpoint,
        recorder = started_process_id,
        "no recorder was listening, so one was started"
    );

    let deadline = Instant::now() + settings.startup_timeout;
    loop {
        if let Some((recorder_process_id, features)) = probe(settings)? {
            // The recorder answering may not be the one started here: two
            // supervisors deciding at the same instant that nothing is running
            // is a real race, and the endpoint is what resolves it rather than
            // anything in this process.
            let origin = if recorder_process_id == started_process_id {
                AttachmentOrigin::Started
            } else {
                tracing::info!(
                    endpoint = %settings.endpoint,
                    recorder = recorder_process_id,
                    started = started_process_id,
                    "another recorder took the endpoint first, so the one started here stopped"
                );
                AttachmentOrigin::LostTheRace { started_process_id }
            };

            return Ok(Attachment {
                recorder_process_id,
                features,
                origin,
            });
        }

        // Checked after the probe rather than before it, so that a recorder
        // which listened and then exited between two polls is still found: the
        // probe is the question being asked, and the exit is only how the wait
        // ends early.
        let exit = child.try_wait().ok().flatten();
        if let Some(status) = exit {
            // A recorder Windows would not load is reported as that rather than
            // as one that started and vanished, because they need opposite
            // things said about them and only this one has an action attached
            // (issue #407).
            if let Some(refusal) = loader_refusal(status.code()) {
                return Err(SupervisorError::NotLoadable {
                    path: settings.executable.clone(),
                    status: refusal,
                });
            }

            return Err(SupervisorError::NeverListened {
                endpoint: settings.endpoint.path(),
                waited: settings.startup_timeout,
                exit: status.code(),
            });
        }

        if Instant::now() >= deadline {
            // Left running rather than killed. It may be a recorder that is
            // slow to start rather than one that is broken, and killing a
            // process that is about to own recordings, on a timeout, is the
            // wrong way round.
            return Err(SupervisorError::NeverListened {
                endpoint: settings.endpoint.path(),
                waited: settings.startup_timeout,
                exit: None,
            });
        }

        std::thread::sleep(STARTUP_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_policy_backs_off_and_then_gives_up() {
        let policy = RestartPolicy::default();

        assert_eq!(policy.delay_before(1), Some(Duration::from_secs(1)));
        assert_eq!(policy.delay_before(2), Some(Duration::from_secs(2)));
        assert_eq!(policy.delay_before(3), Some(Duration::from_secs(4)));
        assert_eq!(policy.delay_before(4), Some(Duration::from_secs(8)));
        assert_eq!(
            policy.delay_before(5),
            None,
            "a restart loop that never ends is worse than a recorder that is down and says so"
        );
    }

    #[test]
    fn the_delay_never_exceeds_the_maximum() {
        let policy = RestartPolicy {
            attempts: 20,
            first_delay: Duration::from_secs(1),
            maximum_delay: Duration::from_secs(5),
            settled_after: Duration::from_secs(60),
        };

        for attempt in 1..=20 {
            let delay = policy.delay_before(attempt).expect("within the attempts");
            assert!(
                delay <= Duration::from_secs(5),
                "attempt {attempt} would wait {delay:?}"
            );
        }
        // And it does reach the cap rather than stopping short of it.
        assert_eq!(policy.delay_before(20), Some(Duration::from_secs(5)));
    }

    #[test]
    fn a_policy_that_never_restarts_offers_no_first_attempt() {
        assert_eq!(RestartPolicy::NEVER.delay_before(1), None);
    }

    #[test]
    fn attempt_zero_is_not_an_attempt() {
        // Attempts are counted from one, because "the first replacement" is
        // attempt one in every message this produces. Asking for attempt zero
        // is a caller's bug, and answering it with the first delay would hide
        // an off-by-one that quietly doubled the budget.
        assert_eq!(RestartPolicy::default().delay_before(0), None);
    }
}
