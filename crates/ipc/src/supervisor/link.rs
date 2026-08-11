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
//! [`RecorderLink::stop`] stops the *watching*. It does not stop the recorder,
//! which is the entire point of the arrangement: a window that closes must
//! leave a recording running
//! ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)).
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
use crate::client::EventClient;
use crate::error::ProtocolError;
use crate::message::{Event, EventStream};
use crate::status::{ActiveRecording, RecorderStatus};

/// How long a subscription is given to be accepted.
///
/// Short: the recorder has just answered a probe on the same endpoint, so this
/// covers the moment between one connection being accepted and the next
/// instance being created, not a recorder that is starting.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(2);

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

impl RecorderLinkState {
    /// The recorder's own state, when there is a recorder to have one.
    ///
    /// [`None`] in every state but [`Attached`](Self::Attached), because in the
    /// others there is nothing this process knows about what is being recorded.
    #[must_use]
    pub const fn status(&self) -> Option<&RecorderStatus> {
        match self {
            Self::Attached { status, .. } => Some(status),
            _ => None,
        }
    }

    /// The recorder being talked to, when there is one.
    #[must_use]
    pub const fn recorder_process_id(&self) -> Option<u32> {
        match self {
            Self::Attached {
                recorder_process_id,
                ..
            } => Some(*recorder_process_id),
            _ => None,
        }
    }
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
}

/// A supervised attachment to the recorder.
///
/// Created by [`start`](Self::start), which returns at once; the work happens on
/// a thread of its own.
#[derive(Debug)]
pub struct RecorderLink {
    shared: Arc<Shared>,
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

        let watching = Arc::clone(&shared);
        thread::Builder::new()
            .name("clipped-recorder-link".to_owned())
            .spawn(move || supervise(&settings, &watching, &sender))
            .expect("a thread can be started to watch the recorder");

        (Self { shared }, receiver)
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

        (Self { shared }, receiver)
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

/// Whether trying the same thing again could plausibly work.
///
/// The distinction matters because the alternative to retrying is telling the
/// user, and telling the user four backoff delays late is worse than telling
/// them at once. A missing executable does not appear because it was asked for
/// again, and a recorder that speaks a different protocol version will speak
/// the same one next time — and the only way to change that is to stop it,
/// which may be to stop a recording.
fn worth_trying_again(error: &SupervisorError) -> bool {
    match error {
        SupervisorError::Spawn { .. }
        | SupervisorError::NeverListened { .. }
        | SupervisorError::Connect { .. } => true,
        SupervisorError::ExecutableMissing { .. }
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
    fn only_an_attached_link_reports_a_recorder_status() {
        assert_eq!(RecorderLinkState::Connecting.status(), None);
        assert_eq!(
            RecorderLinkState::Unavailable {
                reason: "no recorder".to_owned()
            }
            .status(),
            None,
            "a link that cannot reach the recorder knows nothing about what it is doing"
        );
        assert_eq!(
            RecorderLinkState::Attached {
                recorder_process_id: 4_242,
                status: RecorderStatus::Idle
            }
            .status(),
            Some(&RecorderStatus::Idle)
        );
    }

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
        assert!(worth_trying_again(&SupervisorError::NeverListened {
            endpoint: r"\\.\pipe\x".to_owned(),
            waited: Duration::from_secs(1),
            exit: Some(1)
        }));
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
