//! Staying attached to a recorder, and noticing when there is not one.
//!
//! [`RecorderLink`] is the long-lived half of supervision. It attaches to the
//! recorder, keeps a subscription to its `status` and `errors` streams, and when
//! that subscription ends it decides — through [`RestartPolicy`] — whether to
//! try again. Everything it knows is published as a
//! [`RecorderLinkState`], which is what a window renders.
//!
//! # The state is never guessed
//!
//! [`RecorderLinkState`] has four variants and no catch-all, for the reason
//! [`RecorderStatus`] has none: a state a client cannot determine is a state it
//! must not invent (AGENTS.md section 27, `docs/ipc.md`). "The recorder is not
//! reachable" is [`RecorderLinkState::Unavailable`] with the reason in it, not
//! an idle recorder with nothing recorded.
//!
//! # Stopping
//!
//! Two different things, and keeping them apart is the whole design.
//!
//! [`RecorderLink::stop`] stops the *watching*. It does not stop the recorder,
//! which is the entire point of the arrangement: a window that closes must
//! leave a recording running
//! ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)).
//!
//! [`RecorderLink::shut_down_recorder`] stops the *recorder*, on purpose, over
//! the protocol, and finishes any recording first
//! ([issue #220](https://github.com/wildware-uk/clipped/issues/220)). It is what
//! a tray menu's Exit calls, and it is deliberately the only thing here that can
//! end a recording.
//!
//! It also does not join the watching thread. Reads on this transport have no
//! deadline (`docs/ipc.md`, "A client that connects and then stalls"), so a
//! thread blocked waiting for the next event cannot be woken from outside; it
//! ends when the connection does, and it checks the stop flag before doing
//! anything else. In a process that is exiting — the only place `stop` is
//! meaningfully called — that thread goes with it and costs nothing.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{ensure_recorder, Attachment, SupervisorError, SupervisorSettings};
use crate::client::{Client, ClientError, EventClient};
use crate::command::{Command, Reply, Shutdown};
use crate::error::ProtocolError;
use crate::hotkeys::{HotkeyBinding, HotkeyState};
use crate::message::{Event, EventStream};
use crate::status::{ActiveRecording, RecorderStatus};

/// How long a subscription is given to be accepted.
///
/// Short: the recorder has just answered a probe on the same endpoint, so this
/// covers the moment between one connection being accepted and the next
/// instance being created, not a recorder that is starting.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a command is given to reach a recorder.
///
/// The same reasoning as [`SUBSCRIBE_TIMEOUT`]: this is the wait for a
/// *connection* to a recorder believed to be there, not for one that is starting
/// and not for the command itself, which has no deadline. How long a recorder
/// takes to finish a file after a shutdown is a separate wait again, and is
/// [`super::wait_for_recorder_to_exit`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Where the link with the recorder stands.
///
/// Serialisable because this is what crosses into a window. It is deliberately
/// not part of the wire protocol in `docs/ipc.md`: it describes the *connection*
/// to a recorder rather than anything a recorder says, and no recorder ever
/// sends one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "link", rename_all = "snake_case")]
pub enum RecorderLinkState {
    /// Looking for a recorder, or waiting for one that has just been started.
    ///
    /// The state a link is in before it has an answer, and the only one that
    /// means "not known yet".
    Connecting,
    /// Attached to a recorder, which last said this.
    Attached {
        /// The recorder being talked to, as the pipe reports it.
        ///
        /// Carried so that a replacement is visible as one. Without it, a
        /// recorder that crashed and was restarted between two reads of the
        /// state looks exactly like one that never went — and "your recorder
        /// was replaced" is not something a UI may keep to itself
        /// (AGENTS.md section 27).
        recorder_process_id: u32,
        /// What that recorder can do, from its handshake.
        ///
        /// A control that maps to a feature-gated command asks this before it
        /// draws itself. Without it the control is drawn against an older
        /// recorder, the user chooses a file name, and *then* the command is
        /// refused with `unknown_command` — a refusal that arrives after the
        /// only part of the interaction that cost them anything, which is what
        /// AGENTS.md section 27 forbids
        /// ([issue #447](https://github.com/wildware-uk/clipped/issues/447)).
        ///
        /// It describes the recorder named above, not whatever is listening
        /// now: both travel together so a replacement cannot be read with the
        /// previous build's capabilities.
        features: Vec<String>,
        /// What the recorder is doing.
        status: RecorderStatus,
    },
    /// Not attached, and waiting before trying again.
    ///
    /// Trying again attaches to a recorder if one is listening and starts one
    /// if none is, so this covers both "the connection dropped" and "the
    /// recorder is gone". Which of the two it was is not knowable at the moment
    /// the connection ends — that is what the next attempt finds out — and
    /// naming one would be a guess.
    Reconnecting {
        /// Which consecutive attempt this is, counting from one.
        attempt: u32,
        /// How many will be made before the link gives up.
        attempts_allowed: u32,
        /// How long the wait before this attempt is.
        delay_ms: u64,
        /// Why the previous attachment ended, as a sentence.
        reason: String,
    },
    /// Nothing is attached, and nothing further will be tried on its own.
    ///
    /// Reached by exhausting the restart policy, or at once for a failure no
    /// restart could fix. [`RecorderLink::retry`] is the way out, and is what a
    /// "Try again" control calls (AGENTS.md section 45).
    Unavailable {
        /// What happened, as a sentence a person can act on.
        reason: String,
    },
}

/// Something the link noticed.
///
/// Delivered on the channel [`RecorderLink::start`] returns, in the order they
/// happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RecorderLinkEvent {
    /// The link's state changed. Carries the whole state rather than a delta,
    /// so a consumer that missed one recovers on the next.
    State(RecorderLinkState),
    /// A recorder stopped while it was recording, without being asked to.
    ///
    /// The file it names is a finished, playable recording of everything up to
    /// roughly a second before the recorder died
    /// ([ADR 0001](../../../docs/adr/0001-mkv-archival-container.md),
    /// `docs/muxing.md` measures the bound). It was **not** resumed: a
    /// replacement recorder is a new process with no capture session and no open
    /// container, and it cannot continue writing a file another process left.
    ///
    /// This exists so that a UI can say the recording exists rather than leaving
    /// the user to find it, which is the whole of what recovery means here.
    RecordingInterrupted(ActiveRecording),
    /// The recorder reported that a recording failed. The recorder itself is
    /// still running.
    RecordingFailed {
        /// Which recording.
        recording_id: String,
        /// What the recorder said failed.
        error: ProtocolError,
    },
    /// Windows refused one or more of the recorder's global hotkeys, most often
    /// because another application already owns the combination.
    ///
    /// Asked for once per attachment rather than waited for: the recorder
    /// registers its combinations before it announces itself
    /// ([ADR 0009](../../../docs/adr/0009-the-recorder-registers-global-hotkeys.md)),
    /// so `get_hotkeys` has a settled answer the moment there is anything to ask.
    ///
    /// Carries every refused binding rather than a count, because what a person
    /// needs is *which* combination and *which* action
    /// ([issue #417](https://github.com/wildware-uk/clipped/issues/417)) — and
    /// the recorder's own sentence for why, which
    /// [`HotkeyState::Conflict`](crate::HotkeyState::Conflict) already carries.
    ///
    /// Sent on **every** attachment, including a reconnection to the same
    /// recorder. Whether that is worth interrupting anybody for a second time is
    /// not a question this crate can answer — it does not know what the user has
    /// already been told — so the link reports the fact and the consumer decides.
    /// The desktop application's `NotificationPolicy` is where "once, and not on
    /// every reconnection" is enforced.
    HotkeysUnavailable {
        /// The bindings whose state is
        /// [`Conflict`](crate::HotkeyState::Conflict). Never empty: a link with
        /// nothing to report sends no event at all.
        conflicts: Vec<HotkeyBinding>,
    },
}

/// What asking the recorder to exit produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ShutdownOutcome {
    /// The recorder accepted and is winding up.
    ShuttingDown {
        /// The recording it will finish before it exits, if there was one.
        ///
        /// The file is a real file at a real path, and naming it is the whole of
        /// what a UI can usefully say about it (the same reasoning as
        /// [`RecorderLinkEvent::RecordingInterrupted`], and the opposite
        /// situation: this one was asked for).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finalising: Option<ActiveRecording>,
    },
    /// There was no recorder listening, so there was nothing to stop.
    ///
    /// Not an error: "exit" with nothing running has already happened. It is
    /// reported rather than smoothed into success so that a caller can tell
    /// "stopped it" from "there was nothing there", which are different things
    /// to say to a user.
    NothingRunning,
}

/// Why a command sent through the link did not produce a reply.
///
/// One type for every command a link sends rather than one per command: the
/// things that can go wrong — the recorder said no, the recorder could not be
/// reached, there was never a recorder — do not depend on which command it was.
#[derive(Debug)]
pub enum RecorderCallError {
    /// The recorder refused, and this is the refusal to render.
    ///
    /// For a shutdown,
    /// [`ErrorCode::AlreadyRecording`](crate::ErrorCode::AlreadyRecording) is
    /// the interesting one and is not a failure: it means a recording is
    /// running and the request did not say it could be finished, which is the
    /// recorder protecting it. Ask again with
    /// [`Shutdown::finalise_recording`] once the user has said so.
    ///
    /// [`ErrorCode::UnknownCommand`](crate::ErrorCode::UnknownCommand) is the
    /// other one to expect: a recorder built before a command existed, still
    /// running from before an update.
    Refused(ProtocolError),
    /// The recorder could not be reached, or went away part way through.
    Unreachable(ClientError),
    /// The recorder answered a different command's reply.
    ///
    /// A bug on one side or the other, and worth telling from a refusal, which
    /// is the recorder working correctly.
    Unexpected(String),
    /// This link never had a recorder to talk to.
    ///
    /// [`RecorderLink::started_unavailable`] produces such a link: it was made
    /// for a window that could not name an endpoint or an executable at all.
    NoRecorderConfigured,
}

impl std::fmt::Display for RecorderCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(error) => write!(formatter, "{error}"),
            Self::Unreachable(error) => {
                write!(formatter, "the recorder could not be reached: {error}")
            }
            Self::Unexpected(what) => {
                write!(formatter, "the recorder answered with {what} instead")
            }
            Self::NoRecorderConfigured => {
                formatter.write_str("this window never had a recorder to talk to")
            }
        }
    }
}

impl std::error::Error for RecorderCallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused(error) => Some(error),
            Self::Unreachable(error) => Some(error),
            Self::Unexpected(_) | Self::NoRecorderConfigured => None,
        }
    }
}

/// A supervised attachment to the recorder.
///
/// Created by [`start`](Self::start), which returns at once; the work happens on
/// a thread of its own.
#[derive(Debug)]
pub struct RecorderLink {
    shared: Arc<Shared>,
    /// What the watching thread was given, kept so that
    /// [`shut_down_recorder`](Self::shut_down_recorder) can open a connection of
    /// its own. [`None`] for a link that never had a recorder to watch.
    settings: Option<Arc<SupervisorSettings>>,
}

impl RecorderLink {
    /// Attaches to the recorder and keeps doing so, reporting on a channel.
    ///
    /// The receiver may be dropped: the link keeps its state either way, and
    /// [`state`](Self::state) is the whole of it. A consumer that stops reading
    /// loses events rather than blocking the link, in the same way a subscriber
    /// to the recorder does (`docs/ipc.md`).
    #[must_use]
    pub fn start(settings: SupervisorSettings) -> (Self, Receiver<RecorderLinkEvent>) {
        let (sender, receiver) = mpsc::channel();
        let shared = Arc::new(Shared::new());
        let settings = Arc::new(settings);

        let watching = Arc::clone(&shared);
        let watched = Arc::clone(&settings);
        thread::Builder::new()
            .name("clipped-recorder-link".to_owned())
            .spawn(move || supervise(&watched, &watching, &sender))
            .expect("a thread can be started to watch the recorder");

        (
            Self {
                shared,
                settings: Some(settings),
            },
            receiver,
        )
    }

    /// A link that watches nothing, because there was nothing to watch.
    ///
    /// For a caller that could not describe a recorder at all — no endpoint, no
    /// executable — and still has a window to draw. The state is
    /// [`RecorderLinkState::Unavailable`] from the start and never changes,
    /// which is the honest shape of "this application cannot reach a recorder
    /// and here is why" (AGENTS.md section 27). No thread is started.
    #[must_use]
    pub fn started_unavailable(reason: String) -> (Self, Receiver<RecorderLinkEvent>) {
        let (sender, receiver) = mpsc::channel();
        let shared = Arc::new(Shared::new());
        let state = RecorderLinkState::Unavailable { reason };

        shared.set_state(state.clone());
        // The receiver is live, so this cannot fail; it is sent rather than only
        // stored so that a window which listens before it asks still hears.
        let _ = sender.send(RecorderLinkEvent::State(state));

        (
            Self {
                shared,
                settings: None,
            },
            receiver,
        )
    }

    /// Where the link stands right now.
    #[must_use]
    pub fn state(&self) -> RecorderLinkState {
        self.shared.state()
    }

    /// Tries again after the link gave up, with a fresh budget.
    ///
    /// The action behind a "Try again" control. It does nothing to a link that
    /// has not given up; a link that is already attached or already waiting does
    /// not need telling.
    pub fn retry(&self) {
        self.shared.request_retry();
    }

    /// Stops watching the recorder. **Does not stop the recorder.**
    ///
    /// See the module documentation for why the watching thread is not joined.
    pub fn stop(&self) {
        self.shared.request_stop();
    }

    /// Where the recorder listens, for a caller that has to wait for it to go.
    ///
    /// [`None`] for a link that never had one
    /// ([`started_unavailable`](Self::started_unavailable)).
    #[must_use]
    pub fn endpoint(&self) -> Option<&crate::transport::Endpoint> {
        self.settings.as_ref().map(|settings| &settings.endpoint)
    }

    /// Asks the recorder to finish what it is doing and exit.
    ///
    /// This is the action behind a tray menu's Exit, and the answer to
    /// [issue #220](https://github.com/wildware-uk/clipped/issues/220). It sends
    /// `shutdown` on a control connection of its own; the recorder stops
    /// listening, stops any recording and waits for its file to be finalised,
    /// and then exits (`docs/ipc.md`).
    ///
    /// `finalise_recording` is the caller's answer to "may this end a
    /// recording". `false` — the safe default — is refused with
    /// [`RecorderCallError::Refused`] carrying `already_recording` while
    /// something is being recorded, so that a caller which has not put the
    /// question to the user cannot answer it for them.
    ///
    /// **Nothing here waits for the recorder to be gone.** The reply says the
    /// shutdown was accepted; [`super::wait_for_recorder_to_exit`] is how a
    /// caller finds out that it finished, and the two are separate because the
    /// wait is as long as finalising a file and a caller may want to say so.
    ///
    /// # Watching stops with it
    ///
    /// A shutdown that is accepted also calls [`stop`](Self::stop), because a
    /// link that kept watching would see the recorder go, decide it had crashed
    /// and start a replacement — undoing the thing that was just asked for. The
    /// order is deliberately reply-then-stop rather than the reverse: a refused
    /// shutdown leaves the link watching exactly as it was. The gap between the
    /// two is microseconds, against the recorder's own sequence of closing its
    /// listener, finalising a file and only then ending its event subscriptions,
    /// followed by this link's own backoff before any replacement — so there is
    /// no window in which a replacement could be started.
    ///
    /// # Errors
    ///
    /// [`RecorderCallError::Refused`] if the recorder said no — most usefully
    /// `already_recording`, and `unknown_command` from a recorder built before
    /// this command existed; [`RecorderCallError::Unreachable`] if it could not
    /// be reached; [`RecorderCallError::Unexpected`] if it answered a different
    /// command's reply; [`RecorderCallError::NoRecorderConfigured`] for a link
    /// that never had a recorder to talk to.
    ///
    /// A recorder that is not listening at all is **not** an error: there is
    /// nothing to stop, which is [`ShutdownOutcome::NothingRunning`].
    pub fn shut_down_recorder(
        &self,
        finalise_recording: bool,
    ) -> Result<ShutdownOutcome, RecorderCallError> {
        let outcome = match self.call(&Command::Shutdown(Shutdown { finalise_recording })) {
            Ok(Reply::ShuttingDown { finalising }) => ShutdownOutcome::ShuttingDown { finalising },
            Ok(other) => return Err(RecorderCallError::Unexpected(format!("`{other:?}`"))),
            // Nothing is listening. There is no recorder to stop, which is the
            // state the caller was asking for rather than a failure.
            Err(RecorderCallError::Unreachable(ClientError::Transport(
                crate::transport::TransportError::NotListening { .. },
            ))) => {
                self.stop();
                return Ok(ShutdownOutcome::NothingRunning);
            }
            Err(error) => return Err(error),
        };

        tracing::info!(
            finalising = matches!(
                &outcome,
                ShutdownOutcome::ShuttingDown {
                    finalising: Some(_)
                }
            ),
            "the recorder accepted a shutdown"
        );

        // Before this returns, so that the watching thread cannot notice the
        // recorder going and start a replacement.
        self.stop();
        self.shared.set_state(RecorderLinkState::Unavailable {
            reason: "The recorder was asked to exit.".to_owned(),
        });

        Ok(outcome)
    }

    /// Sends one command to the recorder and waits for its reply.
    ///
    /// A control connection of its own, opened and closed around the one
    /// command. The link's own connection carries events and is read by a thread
    /// blocked on it, and a control connection is request-then-response in
    /// strict alternation (`docs/ipc.md`) — so sharing one would mean
    /// interrupting a blocking read. Opening a pipe costs one `CreateFile`, and
    /// the recorder serves eight connections.
    ///
    /// **Blocks** until the recorder answers, which for `stop_recording` is
    /// until the file has been finalised. Never call it on a thread that is
    /// drawing a window.
    ///
    /// # Errors
    ///
    /// [`RecorderCallError::Refused`] carries the recorder's own refusal, which
    /// is what a UI renders; [`RecorderCallError::Unreachable`] means there was
    /// no recorder to ask or it went away;
    /// [`RecorderCallError::NoRecorderConfigured`] means this link never had
    /// one.
    pub fn call(&self, command: &Command) -> Result<Reply, RecorderCallError> {
        let Some(settings) = self.settings.as_ref() else {
            return Err(RecorderCallError::NoRecorderConfigured);
        };

        let mut client = Client::connect(
            &settings.endpoint,
            &settings.client.name,
            &settings.client.version,
            CONNECT_TIMEOUT,
        )
        .map_err(RecorderCallError::Unreachable)?;

        match client.call(command) {
            Ok(reply) => Ok(reply),
            Err(ClientError::Refused(refusal)) => Err(RecorderCallError::Refused(refusal)),
            Err(error) => Err(RecorderCallError::Unreachable(error)),
        }
    }
}

impl Drop for RecorderLink {
    /// A dropped link stops watching, so a link that goes out of scope does not
    /// leave a thread reconnecting to a recorder nobody is listening to.
    fn drop(&mut self) {
        self.stop();
    }
}

/// The state the link and its watching thread share.
#[derive(Debug)]
struct Shared {
    state: Mutex<RecorderLinkState>,
    control: Mutex<Control>,
    changed: Condvar,
}

/// What the owner has asked the watching thread to do.
#[derive(Debug, Default)]
struct Control {
    stopping: bool,
    retry_requested: bool,
}

impl Shared {
    fn new() -> Self {
        Self {
            state: Mutex::new(RecorderLinkState::Connecting),
            control: Mutex::new(Control::default()),
            changed: Condvar::new(),
        }
    }

    /// Reads through a poisoned lock deliberately, for the reason
    /// `serve.rs` does: a panic elsewhere must not turn "attached and
    /// recording" into "unknown". The state is an owned value, so the worst a
    /// poisoned read can be is out of date.
    fn state(&self) -> RecorderLinkState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_state(&self, state: RecorderLinkState) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
    }

    fn is_stopping(&self) -> bool {
        self.control
            .lock()
            .map(|control| control.stopping)
            .unwrap_or(true)
    }

    fn request_stop(&self) {
        if let Ok(mut control) = self.control.lock() {
            control.stopping = true;
        }
        self.changed.notify_all();
    }

    fn request_retry(&self) {
        if let Ok(mut control) = self.control.lock() {
            control.retry_requested = true;
        }
        self.changed.notify_all();
    }

    /// Waits for `delay`, returning `true` if a stop was asked for instead.
    fn sleep_unless_stopping(&self, delay: Duration) -> bool {
        let deadline = Instant::now() + delay;
        let Ok(mut control) = self.control.lock() else {
            return true;
        };

        while !control.stopping {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let Ok((next, _)) = self.changed.wait_timeout(control, remaining) else {
                return true;
            };
            control = next;
        }

        true
    }

    /// Waits until a retry is asked for, returning `true` if a stop came first.
    fn wait_for_retry(&self) -> bool {
        let Ok(mut control) = self.control.lock() else {
            return true;
        };

        while !control.stopping && !control.retry_requested {
            let Ok(next) = self.changed.wait(control) else {
                return true;
            };
            control = next;
        }

        if control.stopping {
            return true;
        }
        control.retry_requested = false;
        false
    }
}

/// The watching thread.
fn supervise(
    settings: &SupervisorSettings,
    shared: &Arc<Shared>,
    sender: &Sender<RecorderLinkEvent>,
) {
    let mut attempt: u32 = 0;
    // `Some` once an attachment has ended, holding why. Every loss costs a
    // delay before the next attempt, which is what stops a recorder that
    // accepts a connection and immediately drops it from being retried in a
    // tight loop.
    let mut loss: Option<String> = None;

    publish(shared, sender, RecorderLinkState::Connecting);

    loop {
        if shared.is_stopping() {
            return;
        }

        if let Some(reason) = loss.take() {
            let Some(delay) = settings.restart.delay_before(attempt + 1) else {
                publish(
                    shared,
                    sender,
                    RecorderLinkState::Unavailable {
                        reason: gave_up(settings, &reason),
                    },
                );
                if shared.wait_for_retry() {
                    return;
                }
                attempt = 0;
                publish(shared, sender, RecorderLinkState::Connecting);
                continue;
            };

            attempt += 1;
            tracing::warn!(
                endpoint = %settings.endpoint,
                attempt,
                delay_ms = delay.as_millis(),
                reason,
                "the link with the recorder ended; trying again"
            );
            publish(
                shared,
                sender,
                RecorderLinkState::Reconnecting {
                    attempt,
                    attempts_allowed: settings.restart.attempts,
                    delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    reason,
                },
            );
            if shared.sleep_unless_stopping(delay) {
                return;
            }
        }

        let attachment = match ensure_recorder(settings) {
            Ok(attachment) => attachment,
            Err(error) if worth_trying_again(&error) => {
                loss = Some(error.to_string());
                continue;
            }
            Err(error) => {
                publish(
                    shared,
                    sender,
                    RecorderLinkState::Unavailable {
                        reason: error.to_string(),
                    },
                );
                if shared.wait_for_retry() {
                    return;
                }
                attempt = 0;
                loss = None;
                publish(shared, sender, RecorderLinkState::Connecting);
                continue;
            }
        };

        let attached_at = Instant::now();
        let reason = follow(settings, shared, sender, &attachment);

        if shared.is_stopping() {
            return;
        }

        // A recorder that stayed up is not in a crash loop, whatever happened
        // afterwards, so the budget starts again rather than being spent down
        // over days by one loss a day.
        if attached_at.elapsed() >= settings.restart.settled_after {
            attempt = 0;
        }
        loss = Some(reason);
    }
}

/// Subscribes and reads events until the subscription ends, returning why.
///
/// The last status seen is what makes [`RecorderLinkEvent::RecordingInterrupted`]
/// possible: when the connection ends while the recorder was recording, this is
/// the only place that still knows what it was recording.
fn follow(
    settings: &SupervisorSettings,
    shared: &Arc<Shared>,
    sender: &Sender<RecorderLinkEvent>,
    attachment: &Attachment,
) -> String {
    let mut events = match EventClient::subscribe(
        &settings.endpoint,
        &settings.client.name,
        &settings.client.version,
        vec![EventStream::Status, EventStream::Errors],
        SUBSCRIBE_TIMEOUT,
    ) {
        Ok(events) => events,
        Err(error) => {
            return format!("the recorder would not deliver its status: {error}");
        }
    };

    tracing::info!(
        endpoint = %settings.endpoint,
        recorder = attachment.recorder_process_id,
        origin = ?attachment.origin,
        "attached to the recorder"
    );

    report_hotkey_conflicts(settings, sender);

    let mut recording: Option<ActiveRecording> = None;

    loop {
        match events.next_event() {
            Ok(Event::StatusChanged { status }) => {
                recording = match &status {
                    RecorderStatus::Recording(active) => Some(active.clone()),
                    RecorderStatus::Idle => None,
                };
                publish(
                    shared,
                    sender,
                    RecorderLinkState::Attached {
                        recorder_process_id: attachment.recorder_process_id,
                        features: attachment.features.clone(),
                        status,
                    },
                );
            }
            Ok(Event::RecordingFailed {
                recording_id,
                error,
            }) => {
                // The recording failed; the recorder did not. Forwarded rather
                // than folded into the state, because the state that follows it
                // is "idle", and "idle" alone would tell the user their
                // recording simply ended.
                send(
                    sender,
                    RecorderLinkEvent::RecordingFailed {
                        recording_id,
                        error,
                    },
                );
            }
            // An event invented after this build was compiled. Ignored rather
            // than treated as a fault, which is what `docs/ipc.md`'s
            // compatibility policy requires of a reader.
            Ok(Event::Other(_)) => {}
            Err(error) => {
                if let Some(active) = recording {
                    tracing::warn!(
                        recording = active.recording_id,
                        "the recorder stopped while it was recording; the file it left is \
                         playable and was not resumed"
                    );
                    send(sender, RecorderLinkEvent::RecordingInterrupted(active));
                }
                return format!(
                    "the connection to the recorder (process {}) ended: {error}",
                    attachment.recorder_process_id
                );
            }
        }
    }
}

/// Asks the recorder which of its hotkeys Windows refused, and says so.
///
/// On a control connection of its own, for the reason
/// [`RecorderLink::call`] opens one: the event subscription this link is about
/// to read from is a stream the caller blocks on, and a control connection is
/// request-then-response in strict alternation (`docs/ipc.md`). Opening a pipe
/// costs one `CreateFile`, and the recorder serves eight connections.
///
/// **Nothing here fails the attachment.** A recorder too old to know
/// `get_hotkeys`, one that refuses it, or one that went away between attaching
/// and being asked, all mean the same thing to this function: it has nothing to
/// report. Losing the link over a question about hotkeys would trade a
/// convenience for the thing the link exists to do (AGENTS.md section 16).
fn report_hotkey_conflicts(settings: &SupervisorSettings, sender: &Sender<RecorderLinkEvent>) {
    let mut client = match Client::connect(
        &settings.endpoint,
        &settings.client.name,
        &settings.client.version,
        CONNECT_TIMEOUT,
    ) {
        Ok(client) => client,
        Err(error) => {
            tracing::debug!(
                %error,
                "the recorder could not be asked which hotkeys it holds, so a combination \
                 another application owns will only be visible on the settings screen"
            );
            return;
        }
    };

    let hotkeys = match client.call(&Command::GetHotkeys) {
        Ok(Reply::Hotkeys { hotkeys }) => hotkeys,
        Ok(_) => {
            tracing::debug!("the recorder answered `get_hotkeys` with something else");
            return;
        }
        Err(error) => {
            tracing::debug!(
                %error,
                "the recorder would not say which hotkeys it holds"
            );
            return;
        }
    };

    let conflicts: Vec<HotkeyBinding> = hotkeys
        .into_iter()
        .filter(|binding| matches!(binding.state, HotkeyState::Conflict { .. }))
        .collect();

    // No event for a recorder that got everything it asked for. An empty list
    // travelling the channel would be a consumer's problem to filter, and a
    // notification policy that received one would have to know it means "all
    // well" rather than "something is wrong" (AGENTS.md section 27).
    if conflicts.is_empty() {
        return;
    }

    tracing::warn!(
        refused = conflicts.len(),
        actions = ?conflicts
            .iter()
            .map(|binding| binding.action.as_str())
            .collect::<Vec<&str>>(),
        "Windows refused some of the recorder's global hotkeys; pressing them will do nothing"
    );
    send(sender, RecorderLinkEvent::HotkeysUnavailable { conflicts });
}

/// Whether trying the same thing again could plausibly work.
///
/// The distinction matters because the alternative to retrying is telling the
/// user, and telling the user four backoff delays late is worse than telling
/// them at once. A missing executable does not appear because it was asked for
/// again, and a recorder that speaks a different protocol version will speak
/// the same one next time — and the only way to change that is to stop it,
/// which may be to stop a recording.
///
/// A recorder Windows would not load is in the same class as a missing one, and
/// for the same reason: the library it is missing does not arrive because the
/// recorder was started a second time. Retrying it would spend four backoff
/// delays before showing a message that was already true at the first attempt,
/// and every one of those attempts leaves a process in the user's event log
/// (`SupervisorError::NotLoadable`, issue #407).
fn worth_trying_again(error: &SupervisorError) -> bool {
    match error {
        SupervisorError::Spawn { .. }
        | SupervisorError::NeverListened { .. }
        | SupervisorError::Connect { .. } => true,
        SupervisorError::ExecutableMissing { .. }
        | SupervisorError::NotLoadable { .. }
        | SupervisorError::Incompatible { .. }
        | SupervisorError::Unsupported => false,
    }
}

/// The sentence shown when the restart policy is exhausted.
///
/// Two wordings, because a policy of no attempts is a deliberate arrangement
/// rather than a budget that ran out, and "0 attempts failed" would read as a
/// bug in the message rather than as the setting it is.
fn gave_up(settings: &SupervisorSettings, reason: &str) -> String {
    if settings.restart.attempts == 0 {
        return format!(
            "{reason}. This link starts no replacement, so nothing is being recorded until a \
             recorder is started."
        );
    }

    format!(
        "{reason}. {} attempts to reach or start a recorder failed, so nothing is being \
         recorded and no more will be made without being asked.",
        settings.restart.attempts
    )
}

/// Stores a state and tells whoever is listening.
fn publish(shared: &Arc<Shared>, sender: &Sender<RecorderLinkEvent>, state: RecorderLinkState) {
    shared.set_state(state.clone());
    send(sender, RecorderLinkEvent::State(state));
}

/// Sends an event, ignoring a receiver that has gone.
///
/// Deliberately not an error: the receiver being dropped is a window that
/// closed, and the link's job is to keep watching the recorder rather than to
/// care whether anybody is reading (AGENTS.md section 15 — the failure is
/// intentionally irrelevant, and this is the note saying so).
fn send(sender: &Sender<RecorderLinkEvent>, event: RecorderLinkEvent) {
    let _ = sender.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_no_retry_could_fix_is_not_retried() {
        assert!(!worth_trying_again(&SupervisorError::ExecutableMissing {
            path: std::path::PathBuf::from("nowhere.exe")
        }));
        assert!(!worth_trying_again(&SupervisorError::Incompatible {
            refusal: ProtocolError::new(
                crate::error::ErrorCode::UnsupportedProtocolVersion,
                "a different version"
            )
        }));
        assert!(!worth_trying_again(&SupervisorError::NotLoadable {
            path: std::path::PathBuf::from("clipped-recorder.exe"),
            status: 0xC000_0135
        }));
        assert!(worth_trying_again(&SupervisorError::NeverListened {
            endpoint: r"\\.\pipe\x".to_owned(),
            waited: Duration::from_secs(1),
            exit: Some(1)
        }));
    }

    /// A recorder that answers `get_hotkeys` with whatever it was built with.
    #[cfg(windows)]
    struct RecorderHolding(Vec<HotkeyBinding>);

    #[cfg(windows)]
    impl crate::server::CommandHandler for RecorderHolding {
        fn call(&self, command: Command) -> Result<Reply, ProtocolError> {
            match command {
                Command::GetHotkeys => Ok(Reply::Hotkeys {
                    hotkeys: self.0.clone(),
                }),
                _ => Ok(Reply::Pong),
            }
        }

        fn status(&self) -> RecorderStatus {
            RecorderStatus::Idle
        }

        fn features(&self) -> Vec<String> {
            vec![crate::features::HOTKEYS.to_owned()]
        }
    }

    /// A binding in the state Windows leaves one it would not give out.
    #[cfg(windows)]
    fn refused(action: &str, combination: &str) -> HotkeyBinding {
        HotkeyBinding {
            action: action.to_owned(),
            label: "Save replay".to_owned(),
            hotkey: Some(combination.to_owned()),
            state: HotkeyState::Conflict {
                reason: format!("{combination} is already registered by another application"),
            },
            handled: true,
            unavailable: None,
        }
    }

    /// A binding Windows accepted.
    #[cfg(windows)]
    fn registered(action: &str, combination: &str) -> HotkeyBinding {
        HotkeyBinding {
            action: action.to_owned(),
            label: "Add bookmark".to_owned(),
            hotkey: Some(combination.to_owned()),
            state: HotkeyState::Registered,
            handled: true,
            unavailable: None,
        }
    }

    /// Starts a recorder holding `hotkeys`, attaches a link, and returns what the
    /// link reported.
    #[cfg(windows)]
    fn what_the_link_reports(label: &str, hotkeys: Vec<HotkeyBinding>) -> Vec<RecorderLinkEvent> {
        let endpoint = crate::transport::Endpoint::named(&format!(
            "clipped-link-hotkeys-{label}.{}",
            std::process::id()
        ))
        .expect("the generated name is valid");
        let mut listener =
            crate::transport::Listener::bind(&endpoint).expect("nothing else has this name");
        let events_published = crate::server::EventPublisher::new();
        let server = crate::server::Server::new(
            Arc::new(RecorderHolding(hotkeys)),
            events_published.clone(),
            crate::message::PeerIdentity {
                name: "clipped-recorder".to_owned(),
                version: "0.0.0-test".to_owned(),
            },
        );
        let serving = thread::spawn(move || {
            let _ = server.serve(&mut listener);
        });

        let settings_endpoint = endpoint.clone();
        let settings = SupervisorSettings {
            restart: crate::supervisor::RestartPolicy::NEVER,
            ..SupervisorSettings::new(
                endpoint,
                std::env::temp_dir().join("clipped-no-such-recorder.exe"),
                crate::message::PeerIdentity {
                    name: "clipped-ipc-test".to_owned(),
                    version: "0.0.0".to_owned(),
                },
            )
        };
        let (link, events) = RecorderLink::start(settings);

        // Everything the link says in the first moments of an attachment. The
        // hotkey question is asked once it is attached, so the state changes
        // arrive around it and the order is not this test's business.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut reported = Vec::new();
        while Instant::now() < deadline {
            match events.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => {
                    let attached = matches!(
                        event,
                        RecorderLinkEvent::State(RecorderLinkState::Attached { .. })
                    );
                    reported.push(event);
                    // Attached is published from the first status the
                    // subscription delivers, which is after the hotkey question
                    // has been asked and answered.
                    if attached {
                        break;
                    }
                }
                Err(_) => continue,
            }
        }

        link.stop();
        drop(link);

        // Stopped over the protocol, which is what ends the accept loop: a
        // listener left serving makes the join below never return, and the
        // desktop application's own fake recorder carries the same note because
        // the first version of it hung a whole test binary that way. This
        // recorder answers `status` with `Idle`, so a bare shutdown is not
        // refused.
        if let Ok(mut client) = Client::connect(
            &settings_endpoint,
            "clipped-ipc-test",
            "0.0.0",
            Duration::from_secs(5),
        ) {
            let _ = client.call(&Command::Shutdown(Shutdown {
                finalise_recording: true,
            }));
        }
        events_published.close();
        let _ = serving.join();
        reported
    }

    #[cfg(windows)]
    #[test]
    fn a_hotkey_windows_refused_is_reported_the_moment_the_link_attaches() {
        // Issue #417. The recorder has known which of its combinations Windows
        // refused since issue #232, and answered `get_hotkeys` about it — but
        // nothing asked unless somebody opened Settings, so a user found out
        // that Ctrl+F10 belongs to another application by pressing it in a game
        // and watching nothing happen.
        let reported = what_the_link_reports(
            "refused",
            vec![
                registered("add_bookmark", "Ctrl+F9"),
                refused("save_replay", "Ctrl+F10"),
            ],
        );

        let conflicts = reported
            .iter()
            .find_map(|event| match event {
                RecorderLinkEvent::HotkeysUnavailable { conflicts } => Some(conflicts),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the link never reported the refused hotkey: {reported:#?}"));

        assert_eq!(
            conflicts.len(),
            1,
            "only the refused combination is a conflict; the registered one is not: {conflicts:#?}"
        );
        assert_eq!(conflicts[0].action, "save_replay");
        assert_eq!(conflicts[0].hotkey.as_deref(), Some("Ctrl+F10"));
    }

    #[cfg(windows)]
    #[test]
    fn a_recorder_that_got_every_combination_it_asked_for_reports_nothing() {
        // The other half, and the reason the event carries a non-empty list: an
        // event meaning "all well" would have every consumer checking a length
        // before deciding whether something was wrong (AGENTS.md section 27).
        let reported = what_the_link_reports(
            "granted",
            vec![
                registered("add_bookmark", "Ctrl+F9"),
                registered("save_replay", "Ctrl+F10"),
            ],
        );

        assert!(
            !reported
                .iter()
                .any(|event| matches!(event, RecorderLinkEvent::HotkeysUnavailable { .. })),
            "nothing was refused, so nothing should have been reported: {reported:#?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_link_that_gave_up_says_so_and_tries_again_when_asked() {
        // The state a window shows when the recorder cannot be started, and the
        // way out of it. Nothing external is needed: the endpoint is a name
        // nothing is listening on and the executable is a path that does not
        // exist, which is the failure `worth_trying_again` refuses to retry — so
        // the link reaches `Unavailable` at once and stays there until told.
        let settings = SupervisorSettings {
            restart: crate::supervisor::RestartPolicy::NEVER,
            ..SupervisorSettings::new(
                crate::transport::Endpoint::named(&format!(
                    "clipped-link-test.{}",
                    std::process::id()
                ))
                .expect("the generated name is valid"),
                std::env::temp_dir().join("clipped-no-such-recorder.exe"),
                crate::message::PeerIdentity {
                    name: "clipped-ipc-test".to_owned(),
                    version: "0.0.0".to_owned(),
                },
            )
        };

        let (link, events) = RecorderLink::start(settings);
        let reason = wait_for_unavailable(&events);
        assert!(
            reason.contains("clipped-no-such-recorder.exe"),
            "the state a window shows has to say what went wrong: {reason}"
        );

        link.retry();
        assert!(
            matches!(
                events
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the link answers a retry"),
                RecorderLinkEvent::State(RecorderLinkState::Connecting)
            ),
            "a retry should start the link looking again, not leave it where it was"
        );
        // And it gives up again rather than looping, because nothing changed.
        wait_for_unavailable(&events);
    }

    /// Reads events until the link reports it has given up, and returns why.
    #[cfg(windows)]
    fn wait_for_unavailable(events: &Receiver<RecorderLinkEvent>) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("the link publishes a state within the timeout")
            {
                RecorderLinkEvent::State(RecorderLinkState::Unavailable { reason }) => {
                    return reason
                }
                _ => continue,
            }
        }
    }

    #[test]
    fn every_link_state_survives_the_journey_into_a_window() {
        // The states cross a process boundary into a webview, so each has to
        // serialise and come back as itself. A state that does not is a window
        // that shows nothing at the moment it most needs to show something.
        for state in [
            RecorderLinkState::Connecting,
            RecorderLinkState::Attached {
                recorder_process_id: 4_242,
                // Two of them, and not the whole list: a window is told what
                // *this* recorder can do, which is a subset for anything but
                // the newest build, and the round trip has to keep the subset
                // rather than a flag saying "some".
                features: vec![
                    crate::features::RECORDING.to_owned(),
                    crate::features::LIBRARY.to_owned(),
                ],
                status: RecorderStatus::Idle,
            },
            RecorderLinkState::Reconnecting {
                attempt: 2,
                attempts_allowed: 4,
                delay_ms: 2_000,
                reason: "the connection ended".to_owned(),
            },
            RecorderLinkState::Unavailable {
                reason: "the recorder was not found".to_owned(),
            },
        ] {
            let json = serde_json::to_string(&state).expect("it serialises");
            let back: RecorderLinkState = serde_json::from_str(&json).expect("and comes back");
            assert_eq!(back, state, "{json}");
        }
    }
}
