//! What is worth interrupting somebody for, what it says, and what it offers.
//!
//! This module is a function and some data. It takes what the recorder link
//! reports and returns either nothing or one [`Notification`]: a title, a
//! sentence and an action. It touches nothing — no Tauri, no Windows, no I/O —
//! for the reason [`crate::tray_model`] does not: it is the part of the
//! notification system with rules in it, and rules that cannot be tested are
//! rules nobody can change safely. [`crate::notifications`] is the machinery
//! that shows the toast and performs the action.
//!
//! # What is notified, and what deliberately is not
//!
//! A recorder runs for days. A notification per recording started would be
//! intolerable, and Windows would collapse or rate-limit them anyway — so the
//! rule this module encodes is that **only failures interrupt anybody**:
//!
//! | What the link reports | Notified | Why |
//! | --- | --- | --- |
//! | `State(Connecting)` | no | Transient, and the tray's icon already says it. |
//! | `State(Attached { Idle })` | no | Starting and stopping are the ordinary course of a day. |
//! | `State(Attached { Recording })` | no | As above, and it is what the tray icon's mark is for. |
//! | `State(Reconnecting)` | no | A blip that usually fixes itself within a second; a toast for each one is the nuisance. |
//! | `State(Unavailable)` | **yes** | The link has given up. Nothing is being recorded and nothing further will be tried unless asked. |
//! | `RecordingInterrupted` | **yes** | A recorder died mid-recording. There is a file, and nothing else will ever tell the user where. |
//! | `RecordingFailed` | **yes** | A recording ended because something went wrong, and the state that follows is only "idle". |
//!
//! Every row is an event that exists today: they are the three variants of
//! [`RecorderLinkEvent`] and the four of [`RecorderLinkState`], and there are no
//! others. "Replay saved", "bookmark added" and "screenshot taken" are in issue
//! #110's scope and are **not** here, because no such event exists — the replay
//! buffer's save is issue #37, and notifying about something no subsystem
//! reports would be the invented state AGENTS.md section 27 forbids.
//!
//! Issue #110 also asks for non-critical notifications to be suppressed during
//! gameplay. There are none: every category above is a failure, and the moment a
//! user most needs to know that nothing is being recorded is while they are
//! playing. So the suppression rule is satisfied by the set being empty, and a
//! critical notification is never withheld.
//!
//! # How this relates to the tray and to the window
//!
//! Three surfaces, and each says something the others cannot.
//!
//! - The **tray** (issue #50) carries *state*: what is happening now, as an icon
//!   mark, a tooltip and a menu. It is always there and it never interrupts.
//! - The **window** carries *sentences*: the status block renders the link's
//!   state, an interrupted recording's file, and whatever the tray had to
//!   report. It says more than a toast can, and it says nothing at all while it
//!   is hidden.
//! - A **notification** is the third: the only one that reaches somebody who has
//!   closed the window to the tray and is in a game, which is exactly when the
//!   recorder is doing its job. It duplicates neither of the others, because it
//!   is used only for the things they cannot deliver in time.
//!
//! # Nothing is announced twice
//!
//! Two rules, and both are about not being a nuisance:
//!
//! - A state that has not changed raises nothing. The link republishes whole
//!   states rather than deltas, and an identical one is not news.
//! - The state the window *opened* in raises nothing either. A notification is
//!   for something that happened while you were away; the state Clipped was
//!   already in when it started is on screen in front of you. Without this, a
//!   machine with no recorder installed (issue #226) would toast on every
//!   launch.

use clipped_ipc::{
    ActiveRecording, ProtocolError, RecorderLinkEvent, RecorderLinkState, RecorderStatus,
};
use serde::Deserialize;

/// The version this build writes and understands in the settings file.
pub(crate) const SETTINGS_VERSION: u32 = 1;

/// What a notification is about.
///
/// The unit the user switches off. There are three because there are three
/// things worth interrupting anybody for; a fourth category means a fourth real
/// event, not a fourth wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotificationCategory {
    /// A recording ended because something went wrong. The recorder is still
    /// running.
    RecordingFailed,
    /// A recorder stopped while it was recording, without being asked to.
    RecordingInterrupted,
    /// The link gave up: nothing is being recorded and nothing further will be
    /// tried on its own.
    RecorderUnavailable,
}

impl NotificationCategory {
    /// Every category, in the order the settings file lists them.
    ///
    /// The single list. Tests walk it rather than repeating it, so a category
    /// added without a switch to turn it off fails a test rather than reaching a
    /// user as a notification nothing can silence.
    pub(crate) const ALL: [Self; 3] = [
        Self::RecordingFailed,
        Self::RecordingInterrupted,
        Self::RecorderUnavailable,
    ];

    /// The name this category has in the settings file.
    ///
    /// Stable: renaming one would silently re-enable a category somebody had
    /// switched off (AGENTS.md section 43).
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::RecordingFailed => "recording_failed",
            Self::RecordingInterrupted => "recording_interrupted",
            Self::RecorderUnavailable => "recorder_unavailable",
        }
    }
}

/// What a notification offers the user to *do*.
///
/// Every notification has one. A failure that arrives with nothing to act on is
/// the message AGENTS.md section 45 exists to prevent, and a button that would
/// do nothing is worse than no button at all (section 27) — which is why
/// [`RetryRecorder`](Self::RetryRecorder) is only ever chosen for a link that
/// has a recorder to retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotificationAction {
    /// Show a recording in File Explorer, with the file itself selected.
    ///
    /// The path is a file the recorder finished writing. Whatever ended a
    /// recording, what was written before it is a playable file
    /// ([ADR 0001](../../../docs/adr/0001-mkv-archival-container.md)), so this
    /// is a real recording and not a fragment.
    ShowFile {
        /// The recording, in full. Never abbreviated: it is the only thing here
        /// anybody can act on.
        path: String,
    },
    /// Look for a recorder again, with a fresh restart budget.
    ///
    /// `RecorderLink::retry`, which is what a "Try again" control calls. It does
    /// nothing to a link that never had a recorder to talk to, so
    /// [`NotificationPolicy`] only offers it when there is one.
    RetryRecorder,
    /// Raise the window carrying a sentence.
    ///
    /// The fallback when there is nothing more specific to offer. It is a thing
    /// that happens rather than a button that does not: the window comes to the
    /// front with the message in its status block, which is the only surface
    /// Clipped has that can hold a paragraph.
    OpenClipped {
        /// What the window is to say when it arrives.
        notice: String,
    },
}

impl NotificationAction {
    /// The text on the toast's button.
    ///
    /// A verb and an object, in the style AGENTS.md section 28 asks for, and
    /// never "Explore" or "Learn more" (section 29).
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::ShowFile { .. } => "Show the file",
            Self::RetryRecorder => "Try again",
            Self::OpenClipped { .. } => "Open Clipped",
        }
    }
}

/// One notification, ready to be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notification {
    /// Which switch turns it off.
    pub(crate) category: NotificationCategory,
    /// Two or three words: what happened.
    pub(crate) title: String,
    /// One or two sentences: what it means, and where the thing it is about is.
    pub(crate) body: String,
    /// What the user can do about it.
    pub(crate) action: NotificationAction,
}

/// Which notifications the user wants.
///
/// Read from `notifications.json` in Clipped's configuration directory;
/// [`crate::notifications`] is what finds and reads it. Every category defaults
/// to on, because all three are failures and a user who has not said otherwise
/// wants to be told that nothing is being recorded.
///
/// `#[serde(default)]` is the whole of the compatibility policy for this file: a
/// file written before a category existed is missing its field and gets the
/// default, and a file written by a newer Clipped carries fields this build
/// ignores. [`Self::from_json`] refuses a `version` from the future rather than
/// guessing at what its fields mean (AGENTS.md sections 30 and 43).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub(crate) struct NotificationSettings {
    /// The shape of this file, so a later change can migrate rather than
    /// misread.
    version: u32,
    /// Whether a recording that failed is worth a notification.
    recording_failed: bool,
    /// Whether a recorder that died mid-recording is.
    recording_interrupted: bool,
    /// Whether a link that gave up looking for a recorder is.
    recorder_unavailable: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            recording_failed: true,
            recording_interrupted: true,
            recorder_unavailable: true,
        }
    }
}

impl NotificationSettings {
    /// Whether this category may interrupt the user.
    pub(crate) const fn allows(self, category: NotificationCategory) -> bool {
        match category {
            NotificationCategory::RecordingFailed => self.recording_failed,
            NotificationCategory::RecordingInterrupted => self.recording_interrupted,
            NotificationCategory::RecorderUnavailable => self.recorder_unavailable,
        }
    }

    /// Reads the settings file's contents.
    ///
    /// A leading byte-order mark is dropped first. JSON has no such thing and
    /// serde rejects it, but this file is edited by hand on Windows and both
    /// Notepad and `Out-File -Encoding utf8` under Windows PowerShell put one
    /// there — so refusing it would mean a file that looks exactly right and
    /// does not work. That was not a guess: writing the file that way is what
    /// the first end-to-end run of this feature did, and the notification it was
    /// meant to switch off arrived (`docs/desktop-ui.md`).
    ///
    /// # Errors
    ///
    /// A sentence for the user when the file is not JSON, is not an object of
    /// the expected shape, or says it was written by a later version of Clipped
    /// than this one. In every case the caller falls back to the defaults and
    /// says so: silently ignoring a settings file somebody edited is worse than
    /// either outcome (AGENTS.md section 15).
    pub(crate) fn from_json(json: &str) -> Result<Self, String> {
        let json = json.strip_prefix('\u{feff}').unwrap_or(json);
        let settings: Self =
            serde_json::from_str(json).map_err(|error| format!("it could not be read: {error}"))?;

        if settings.version > SETTINGS_VERSION {
            return Err(format!(
                "it says version {} and this build of Clipped understands version {SETTINGS_VERSION}",
                settings.version
            ));
        }

        Ok(settings)
    }
}

/// Decides what the user is told, and remembers enough not to repeat itself.
///
/// One of these exists per run of the application, owned by the thread that
/// reads the recorder link's events, which is the only thing that consults it.
/// That is deliberate: it holds mutable state, and a lock around it would be a
/// lock nothing else ever wanted.
#[derive(Debug)]
pub(crate) struct NotificationPolicy {
    /// Which categories the user wants.
    settings: NotificationSettings,
    /// Whether this link has a recorder to look for again.
    ///
    /// False for a link that never had one — no endpoint could be named or no
    /// executable found — where "Try again" would do nothing at all.
    can_retry: bool,
    /// The last state seen, so that an unchanged one is not announced.
    last_state: RecorderLinkState,
    /// The last recording the recorder said it was making.
    ///
    /// Kept so that a `recording_failed` event, which carries an identifier and
    /// no path, can name the file. **Never cleared when the recorder goes
    /// idle**: the idle status and the failure are two messages and the recorder
    /// decides their order, so clearing on idle would lose the path exactly when
    /// it is wanted. The identifier is what makes that safe — a failure is only
    /// matched to a recording whose `recording_id` it names, so a stale one is
    /// ignored rather than misreported.
    recording: Option<ActiveRecording>,
}

impl NotificationPolicy {
    /// A policy that starts from the state the application opened in.
    ///
    /// `opening_state` is not announced. See the module documentation: a
    /// notification is for something that happened while the user was away.
    pub(crate) fn new(
        settings: NotificationSettings,
        can_retry: bool,
        opening_state: &RecorderLinkState,
    ) -> Self {
        Self {
            settings,
            can_retry,
            last_state: opening_state.clone(),
            recording: recording_in(opening_state).cloned(),
        }
    }

    /// What, if anything, to show the user for this event.
    pub(crate) fn decide(&mut self, event: &RecorderLinkEvent) -> Option<Notification> {
        // The bookkeeping happens whether or not the category is switched on, so
        // that switching one off cannot leave this unable to name a file later.
        let notification = self.consider(event);
        notification.filter(|notification| self.settings.allows(notification.category))
    }

    /// The decision itself, before the user's switches are applied.
    fn consider(&mut self, event: &RecorderLinkEvent) -> Option<Notification> {
        match event {
            RecorderLinkEvent::State(state) => self.state_changed(state),
            RecorderLinkEvent::RecordingInterrupted(active) => Some(recording_interrupted(active)),
            RecorderLinkEvent::RecordingFailed {
                recording_id,
                error,
            } => Some(self.recording_failed(recording_id, error)),
        }
    }

    /// The link is somewhere new — or says it is.
    fn state_changed(&mut self, state: &RecorderLinkState) -> Option<Notification> {
        if *state == self.last_state {
            return None;
        }
        self.last_state = state.clone();

        if let Some(active) = recording_in(state) {
            self.recording = Some(active.clone());
        }

        match state {
            RecorderLinkState::Unavailable { reason } => Some(self.recorder_unavailable(reason)),
            // Connecting, attached and reconnecting are the ordinary course of a
            // day. The tray shows all three and none of them is worth a toast.
            RecorderLinkState::Connecting
            | RecorderLinkState::Attached { .. }
            | RecorderLinkState::Reconnecting { .. } => None,
        }
    }

    /// A recording ended because something went wrong.
    ///
    /// The file is named when the failure is for the recording this window last
    /// saw the recorder making, and is not otherwise. The recorder's own
    /// identifier is what decides that: guessing that the last recording seen
    /// was the one that failed would put a path in front of the user that might
    /// belong to a different file (AGENTS.md section 27).
    fn recording_failed(&self, recording_id: &str, error: &ProtocolError) -> Notification {
        let file = self
            .recording
            .as_ref()
            .filter(|active| active.recording_id == recording_id)
            .map(|active| active.output.clone());

        let title = "Recording failed".to_owned();
        let said = as_sentence(&error.message);

        match file {
            Some(path) => Notification {
                category: NotificationCategory::RecordingFailed,
                title,
                body: format!("{said} What was recorded before it failed is at {path}."),
                action: NotificationAction::ShowFile { path },
            },
            // No path to offer, so the honest action is the window, which has
            // room for the whole refusal and is where Clipped is driven from.
            None => Notification {
                category: NotificationCategory::RecordingFailed,
                title: title.clone(),
                body: said.clone(),
                action: NotificationAction::OpenClipped {
                    notice: format!("{title}. {said}"),
                },
            },
        }
    }

    /// The link has given up looking for a recorder.
    fn recorder_unavailable(&self, reason: &str) -> Notification {
        let title = "Recorder unavailable".to_owned();
        let said = as_sentence(reason);

        Notification {
            category: NotificationCategory::RecorderUnavailable,
            title: title.clone(),
            body: said.clone(),
            action: if self.can_retry {
                NotificationAction::RetryRecorder
            } else {
                NotificationAction::OpenClipped {
                    notice: format!("{title}. {said}"),
                }
            },
        }
    }
}

/// A recorder died with a recording open.
///
/// The wording is the window's, in
/// `apps/desktop/src/useRecorderLink.ts::describeInterruption`, deliberately:
/// two surfaces describing the same event in two vocabularies is how a user ends
/// up believing they are two events.
fn recording_interrupted(active: &ActiveRecording) -> Notification {
    Notification {
        category: NotificationCategory::RecordingInterrupted,
        title: "Recording interrupted".to_owned(),
        body: format!(
            "{} was not resumed. The file is at {}.",
            active.target, active.output
        ),
        action: NotificationAction::ShowFile {
            path: active.output.clone(),
        },
    }
}

/// The recording a link state describes, if it describes one.
fn recording_in(state: &RecorderLinkState) -> Option<&ActiveRecording> {
    match state {
        RecorderLinkState::Attached {
            status: RecorderStatus::Recording(active),
            ..
        } => Some(active),
        _ => None,
    }
}

/// One sentence, from a message written as a clause.
///
/// The recorder's own messages are lower-case fragments — "the disk the
/// recording was being written to is full" — because they are written to be
/// embedded. A toast puts one under a bold title with nothing around it, so it
/// is capitalised and stopped here rather than by asking the protocol to change
/// how it writes (AGENTS.md sections 28 and 44).
fn as_sentence(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut characters = trimmed.chars();
    let mut sentence: String = characters
        .next()
        .map(|first| first.to_uppercase().to_string())
        .unwrap_or_default();
    sentence.push_str(characters.as_str());

    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipped_ipc::ErrorCode;

    /// The state a healthy attached recorder is in.
    fn idle() -> RecorderLinkState {
        RecorderLinkState::Attached {
            recorder_process_id: 4_242,
            status: RecorderStatus::Idle,
        }
    }

    fn active(recording_id: &str) -> ActiveRecording {
        ActiveRecording {
            recording_id: recording_id.to_owned(),
            output: r"D:\clips\cs2-2026-08-11.mkv".to_owned(),
            target: "process cs2.exe".to_owned(),
            elapsed_ms: 90_000,
        }
    }

    fn recording(recording_id: &str) -> RecorderLinkState {
        RecorderLinkState::Attached {
            recorder_process_id: 4_242,
            status: RecorderStatus::Recording(active(recording_id)),
        }
    }

    fn unavailable(reason: &str) -> RecorderLinkState {
        RecorderLinkState::Unavailable {
            reason: reason.to_owned(),
        }
    }

    fn failed(recording_id: &str, message: &str) -> RecorderLinkEvent {
        RecorderLinkEvent::RecordingFailed {
            recording_id: recording_id.to_owned(),
            error: ProtocolError::new(ErrorCode::RecordingFailed, message),
        }
    }

    /// A policy that has been running since the application was connecting.
    fn policy() -> NotificationPolicy {
        NotificationPolicy::new(
            NotificationSettings::default(),
            true,
            &RecorderLinkState::Connecting,
        )
    }

    #[test]
    fn the_ordinary_course_of_a_day_interrupts_nobody() {
        // The nuisance rule, as a test. A recorder runs for days; a toast when a
        // recording starts, when it stops, and on every reconnection blip would
        // make the whole system something a user turns off, taking the failures
        // with it.
        let mut policy = policy();

        for state in [
            idle(),
            recording("r-1"),
            idle(),
            RecorderLinkState::Reconnecting {
                attempt: 1,
                attempts_allowed: 5,
                delay_ms: 500,
                reason: "the connection ended".to_owned(),
            },
            RecorderLinkState::Connecting,
        ] {
            assert_eq!(
                policy.decide(&RecorderLinkEvent::State(state.clone())),
                None,
                "{state:?} is not worth interrupting anybody for"
            );
        }
    }

    #[test]
    fn a_failed_recording_names_the_file_it_left_and_offers_to_show_it() {
        // The recording_failed event carries an identifier and no path, so the
        // path can only come from the status that preceded it. Without it the
        // user is told a recording failed and left to guess whether anything
        // was written.
        let mut policy = policy();
        policy.decide(&RecorderLinkEvent::State(recording("r-1")));

        let notification = policy
            .decide(&failed(
                "r-1",
                "the disk the recording was being written to is full",
            ))
            .expect("a failed recording is worth a notification");

        assert_eq!(notification.category, NotificationCategory::RecordingFailed);
        assert_eq!(notification.title, "Recording failed");
        assert!(
            notification.body.starts_with("The disk"),
            "the recorder's own sentence, made into one: {}",
            notification.body
        );
        assert!(
            notification.body.contains(r"D:\clips\cs2-2026-08-11.mkv"),
            "the file has to be named: {}",
            notification.body
        );
        assert_eq!(
            notification.action,
            NotificationAction::ShowFile {
                path: r"D:\clips\cs2-2026-08-11.mkv".to_owned()
            }
        );
    }

    #[test]
    fn a_failure_for_a_recording_this_window_never_saw_claims_no_file() {
        // The window may have attached after the recording started, or missed
        // the status. Naming the last file seen would name a different
        // recording's file, which is worse than naming none.
        let mut policy = policy();
        policy.decide(&RecorderLinkEvent::State(recording("r-1")));

        let notification = policy
            .decide(&failed("r-2", "the encoder stopped responding"))
            .expect("a failed recording is still worth a notification");

        assert!(
            !notification.body.contains(r"D:\clips"),
            "no file may be claimed for a recording this window never saw: {}",
            notification.body
        );
        assert_eq!(
            notification.action,
            NotificationAction::OpenClipped {
                notice: "Recording failed. The encoder stopped responding.".to_owned()
            }
        );
    }

    #[test]
    fn the_recorder_going_idle_does_not_cost_a_failure_its_file() {
        // The recorder sends the idle status and the failure as two messages and
        // decides their order. A policy that forgot the recording on idle would
        // name the file or not depending on which arrived first.
        let mut policy = policy();
        policy.decide(&RecorderLinkEvent::State(recording("r-1")));
        policy.decide(&RecorderLinkEvent::State(idle()));

        let notification = policy
            .decide(&failed("r-1", "the encoder stopped responding"))
            .expect("a failed recording is worth a notification");

        assert_eq!(
            notification.action,
            NotificationAction::ShowFile {
                path: r"D:\clips\cs2-2026-08-11.mkv".to_owned()
            },
            "the file is still the file: {}",
            notification.body
        );
    }

    #[test]
    fn an_interrupted_recording_says_where_the_file_is_and_that_it_was_not_resumed() {
        let mut policy = policy();

        let notification = policy
            .decide(&RecorderLinkEvent::RecordingInterrupted(active("r-1")))
            .expect("a recorder that died mid-recording is worth a notification");

        assert_eq!(
            notification.category,
            NotificationCategory::RecordingInterrupted
        );
        assert_eq!(notification.title, "Recording interrupted");
        assert!(
            notification.body.contains("was not resumed"),
            "recovery names the file; it does not resume the recording: {}",
            notification.body
        );
        assert!(
            notification.body.contains(r"D:\clips\cs2-2026-08-11.mkv"),
            "{}",
            notification.body
        );
        assert_eq!(
            notification.action,
            NotificationAction::ShowFile {
                path: r"D:\clips\cs2-2026-08-11.mkv".to_owned()
            }
        );
    }

    #[test]
    fn a_link_that_gave_up_offers_to_try_again() {
        let mut policy = policy();

        let notification = policy
            .decide(&RecorderLinkEvent::State(unavailable(
                "the recorder could not be started",
            )))
            .expect("a link that gave up is worth a notification");

        assert_eq!(
            notification.category,
            NotificationCategory::RecorderUnavailable
        );
        assert_eq!(notification.title, "Recorder unavailable");
        assert_eq!(notification.action, NotificationAction::RetryRecorder);
    }

    #[test]
    fn a_link_that_never_had_a_recorder_does_not_offer_a_retry_that_would_do_nothing() {
        // `RecorderLink::retry` does nothing to a link with no settings behind
        // it, and a button that does nothing is the failure AGENTS.md section 27
        // names. The window at least holds the reason.
        let mut policy = NotificationPolicy::new(
            NotificationSettings::default(),
            false,
            &RecorderLinkState::Connecting,
        );

        let notification = policy
            .decide(&RecorderLinkEvent::State(unavailable(
                "the recorder endpoint could not be named",
            )))
            .expect("it is still worth a notification");

        assert_eq!(
            notification.action,
            NotificationAction::OpenClipped {
                notice: "Recorder unavailable. The recorder endpoint could not be named."
                    .to_owned()
            }
        );
    }

    #[test]
    fn the_state_the_application_opened_in_is_not_announced() {
        // An installation with no recorder beside it (issue #226) starts
        // unavailable and stays there. The window is open, showing exactly that,
        // and a toast on every launch saying what is already on screen is how a
        // user learns to ignore them.
        let opening = unavailable("Clipped could not find clipped-recorder.exe");
        let mut policy = NotificationPolicy::new(NotificationSettings::default(), true, &opening);

        assert_eq!(
            policy.decide(&RecorderLinkEvent::State(opening)),
            None,
            "the state the window opened in is already on screen"
        );
    }

    #[test]
    fn a_state_republished_unchanged_is_announced_once() {
        let mut policy = policy();
        let gone = unavailable("the recorder could not be started");

        assert!(
            policy
                .decide(&RecorderLinkEvent::State(gone.clone()))
                .is_some(),
            "the first one is news"
        );
        assert_eq!(
            policy.decide(&RecorderLinkEvent::State(gone)),
            None,
            "the same state again is not"
        );
    }

    #[test]
    fn giving_up_again_after_trying_again_is_announced_again() {
        // The other half of the rule above, and the one that matters: the user
        // asked for a retry, it went connecting, and it failed again. Suppressing
        // that would leave a "Try again" that appears to have worked.
        let mut policy = policy();
        let gone = unavailable("the recorder could not be started");

        policy.decide(&RecorderLinkEvent::State(gone.clone()));
        policy.decide(&RecorderLinkEvent::State(RecorderLinkState::Connecting));

        assert!(
            policy.decide(&RecorderLinkEvent::State(gone)).is_some(),
            "a second failure after a retry is a second thing that happened"
        );
    }

    #[test]
    fn every_category_can_be_switched_off_and_switching_one_off_leaves_the_others() {
        // Acceptance criterion: disabled categories produce nothing. Walked over
        // `ALL` so that a category added without a switch fails here.
        for category in NotificationCategory::ALL {
            let off = NotificationSettings::from_json(&format!(
                r#"{{"version":1,"{}":false}}"#,
                category.key()
            ))
            .expect("the settings parse");

            let mut raised = Vec::new();
            for event in every_notifiable_event() {
                let mut policy = NotificationPolicy::new(off, true, &RecorderLinkState::Connecting);
                // The failure needs the status that names its recording first.
                policy.decide(&RecorderLinkEvent::State(recording("r-1")));
                if let Some(notification) = policy.decide(&event) {
                    raised.push(notification.category);
                }
            }

            assert!(
                !raised.contains(&category),
                "{} was switched off and still notified",
                category.key()
            );
            for other in NotificationCategory::ALL {
                if other != category {
                    assert!(
                        raised.contains(&other),
                        "switching off {} also silenced {}",
                        category.key(),
                        other.key()
                    );
                }
            }
        }
    }

    /// One event for each category, so the tests above cannot drift from
    /// [`NotificationCategory::ALL`].
    fn every_notifiable_event() -> Vec<RecorderLinkEvent> {
        vec![
            failed("r-1", "the encoder stopped responding"),
            RecorderLinkEvent::RecordingInterrupted(active("r-1")),
            RecorderLinkEvent::State(unavailable("the recorder could not be started")),
        ]
    }

    #[test]
    fn every_notification_carries_something_to_do() {
        // Acceptance criterion: an error notification leads to an action rather
        // than only a message.
        for event in every_notifiable_event() {
            let mut policy = policy();
            policy.decide(&RecorderLinkEvent::State(recording("r-1")));

            let notification = policy
                .decide(&event)
                .unwrap_or_else(|| panic!("{event:?} should notify"));

            assert!(
                !notification.action.label().is_empty(),
                "{event:?} offers nothing to do"
            );
            assert!(!notification.title.is_empty(), "{event:?} has no title");
            assert!(!notification.body.is_empty(), "{event:?} says nothing");
        }
    }

    #[test]
    fn the_settings_default_to_telling_the_user_everything() {
        let settings = NotificationSettings::default();
        for category in NotificationCategory::ALL {
            assert!(
                settings.allows(category),
                "{} should default to on",
                category.key()
            );
        }
    }

    #[test]
    fn a_settings_file_missing_a_category_gets_the_default_for_it() {
        // The additive half of the compatibility policy: a file written before a
        // category existed must not silence it.
        let settings = NotificationSettings::from_json(r#"{"version":1,"recording_failed":false}"#)
            .expect("a partial file is a valid file");

        assert!(!settings.allows(NotificationCategory::RecordingFailed));
        assert!(settings.allows(NotificationCategory::RecordingInterrupted));
        assert!(settings.allows(NotificationCategory::RecorderUnavailable));
    }

    #[test]
    fn a_field_this_build_has_never_heard_of_is_ignored_rather_than_fatal() {
        let settings = NotificationSettings::from_json(
            r#"{"version":1,"recording_failed":false,"replay_saved":true}"#,
        )
        .expect("an unknown field must not cost the whole file");

        assert!(!settings.allows(NotificationCategory::RecordingFailed));
    }

    #[test]
    fn a_settings_file_from_a_later_clipped_is_refused_rather_than_guessed_at() {
        let error = NotificationSettings::from_json(r#"{"version":2,"recording_failed":false}"#)
            .expect_err("version 2 may mean something else by these fields");

        assert!(error.contains("version 2"), "{error}");
        assert!(error.contains("version 1"), "{error}");
    }

    #[test]
    fn a_settings_file_a_windows_editor_saved_is_still_a_settings_file() {
        // Notepad and Windows PowerShell's `Out-File -Encoding utf8` both write
        // a byte-order mark, which is not JSON. The first end-to-end run of this
        // feature switched a category off exactly that way, and the notification
        // arrived anyway.
        let settings =
            NotificationSettings::from_json("\u{feff}{\"version\":1,\"recording_failed\":false}")
                .expect("a byte-order mark must not cost the whole file");

        assert!(!settings.allows(NotificationCategory::RecordingFailed));
    }

    #[test]
    fn a_settings_file_that_is_not_json_is_refused_rather_than_ignored() {
        let error = NotificationSettings::from_json("recording_failed = false")
            .expect_err("an INI file is not a settings file");
        assert!(!error.is_empty(), "the user has to be told what is wrong");
    }

    #[test]
    fn a_clause_from_the_recorder_becomes_a_sentence() {
        assert_eq!(
            as_sentence("the disk the recording was being written to is full"),
            "The disk the recording was being written to is full."
        );
        assert_eq!(
            as_sentence("NVENC would not open."),
            "NVENC would not open.",
            "a message that is already a sentence is left alone"
        );
        assert_eq!(as_sentence("  "), "", "there is nothing to capitalise");
    }
}
