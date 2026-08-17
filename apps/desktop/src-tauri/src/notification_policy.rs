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
//! | `HotkeysUnavailable` | **yes**, once | Windows refused a combination, so a control the user believes in does nothing. Only the first time a given set is seen; a reconnection reporting the same refusals is not news. |
//!
//! Every row is an event that exists today: they are the four variants of
//! [`RecorderLinkEvent`] and the four of [`RecorderLinkState`], and there are no
//! others. "Replay saved", "bookmark added" and "screenshot taken" are in issue
//! #110's scope and are **not** here, because no such event exists — the replay
//! buffer's save is written (issue #37) but no build runs a recording with a
//! buffer to save from (issue #38), and notifying about something no subsystem
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
//!   machine whose recorder is missing would toast on every launch.

use std::sync::{Arc, Mutex, PoisonError};

use clipped_ipc::{
    ActiveRecording, HotkeyBinding, HotkeyState, ProtocolError, RecorderLinkEvent,
    RecorderLinkState, RecorderStatus, SettingsView,
};

/// What a notification is about.
///
/// The unit the user switches off. There are four because there are four things
/// worth interrupting anybody for; a fifth category means a fifth real event,
/// not a fifth wording.
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
    /// Windows refused a global hotkey, so pressing it does nothing.
    ///
    /// The odd one out, and deliberately still here: it is not a recording that
    /// failed but a control that is not there, and the way somebody finds out
    /// otherwise is by pressing it in a game and watching nothing happen
    /// ([issue #417](https://github.com/wildware-uk/clipped/issues/417)).
    HotkeyUnavailable,
}

impl NotificationCategory {
    /// Every category, in the order the settings file lists them.
    ///
    /// The single list. Tests walk it rather than repeating it, so a category
    /// added without a switch to turn it off fails a test rather than reaching a
    /// user as a notification nothing can silence.
    pub(crate) const ALL: [Self; 4] = [
        Self::RecordingFailed,
        Self::RecordingInterrupted,
        Self::RecorderUnavailable,
        Self::HotkeyUnavailable,
    ];

    /// The name this category has in the settings file.
    ///
    /// The recorder's own spelling, in the `notifications` section of
    /// `settings.json` (`clipped_session::config::notifications`). It is
    /// repeated here rather than imported because this window may link one crate
    /// of that workspace, `clipped-ipc`, and a settings key crosses that
    /// protocol as text; `settingsConformance.test.ts` is what holds the two
    /// lists equal, in both directions.
    ///
    /// Stable: renaming one would silently re-enable a category somebody had
    /// switched off (AGENTS.md section 43).
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::RecordingFailed => "recording_failed",
            Self::RecordingInterrupted => "recording_interrupted",
            Self::RecorderUnavailable => "recorder_unavailable",
            Self::HotkeyUnavailable => "hotkey_unavailable",
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
    /// Raise the window on the screen that lists the hotkeys and what Windows
    /// said about each.
    ///
    /// The Settings screen, which has drawn that list since
    /// [issue #232](https://github.com/wildware-uk/clipped/issues/232). It is a
    /// real destination rather than a button that apologises: the row for the
    /// refused combination is already there, with the recorder's own sentence
    /// beside it.
    OpenHotkeySettings,
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
            Self::OpenHotkeySettings => "Change the hotkey",
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
/// Every category defaults to on, because all four are failures and a user who
/// has not said otherwise wants to be told that nothing is being recorded.
///
/// # Where these come from
///
/// The `notifications` section of `%LOCALAPPDATA%\Clipped\settings.json`, which
/// the **recorder** owns — its defaults, its validation and its migrations are
/// `clipped_session::config` (`docs/configuration.md`) — asked for over the
/// protocol with `get_settings` and changed with `apply_settings`
/// (`crate::notifications::refresh`).
///
/// It is asked for rather than read because this window may link one crate of
/// the repository's workspace, `clipped-ipc`, and
/// `tests/integration/tests/workspace_layering.rs` enforces it: naming
/// `clipped-session` here would put the recording engine inside the window's
/// process, which is the separation ADR 0002 exists to make. Until
/// [issue #252](https://github.com/wildware-uk/clipped/issues/252) the way round
/// that was a `notifications.json` of this window's own — a second store of user
/// preferences with a second version field, a second missing-key policy and a
/// second reader, which is the duplication AGENTS.md section 55 forbids.
/// `crate::notifications::migrate_legacy_switches` is what carries one of those
/// files into the settings file and removes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotificationSettings {
    /// One switch per category, indexed as [`NotificationCategory::ALL`] orders
    /// them — so a category added there has somewhere to be stored without
    /// anybody remembering to add it.
    enabled: [bool; NotificationCategory::ALL.len()],
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: [true; NotificationCategory::ALL.len()],
        }
    }
}

impl NotificationSettings {
    /// Whether this category may interrupt the user.
    pub(crate) const fn allows(self, category: NotificationCategory) -> bool {
        self.enabled[category as usize]
    }

    /// The switches the recorder has just reported.
    ///
    /// A category the view does not mention is **on**, which covers the two ways
    /// that happens and wants the same answer for both: a recorder older than
    /// this window, which has no such setting, and a value neither side could
    /// make sense of. Everything is a failure, so the safe fallback is being
    /// told about it — a window that read silence as "switched off" would stop
    /// saying that nothing is being recorded.
    pub(crate) fn from_view(view: &SettingsView) -> Self {
        let mut settings = Self::default();
        for category in NotificationCategory::ALL {
            if let Some(entry) = view
                .settings
                .iter()
                .find(|entry| entry.key == category.key())
            {
                // The words the settings file spells a boolean in, which is what
                // every value on this protocol is (`clipped_ipc::settings`).
                settings.enabled[category as usize] = !entry.value.eq_ignore_ascii_case("false");
            }
        }
        settings
    }
}

/// The switches, shared between the window's Save and the thread that decides.
///
/// Two threads need them and neither owns them. The recorder link's event thread
/// consults them for every notification, and a `#[tauri::command]` running on
/// the window's thread replaces them the moment somebody changes one on the
/// Settings screen — which is what makes a switch take effect immediately rather
/// than at the next launch, and therefore what stops it being a control that
/// silently does nothing until Clipped is restarted (AGENTS.md section 27).
///
/// A lock rather than a channel because the read is the hot side: it happens
/// once per link event and is uncontended almost always, and a write is a person
/// pressing Save.
#[derive(Debug, Clone, Default)]
pub(crate) struct NotificationPreferences(Arc<Mutex<NotificationSettings>>);

impl NotificationPreferences {
    /// The switches as they stand.
    pub(crate) fn current(&self) -> NotificationSettings {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the switches from what the recorder has just answered.
    ///
    /// The whole view rather than a delta, for the reason the reply to
    /// `apply_settings` is the settings as they now stand: what a window draws,
    /// and what it acts on, is what the recorder holds rather than what the
    /// window hoped had been saved (`crates/ipc/src/settings.rs`).
    pub(crate) fn adopt(&self, view: &SettingsView) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            NotificationSettings::from_view(view);
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
    ///
    /// Shared rather than owned, because the switches are the one thing here
    /// that somebody can change while this is running: the Settings screen saves
    /// one and the very next notification has to honour it.
    preferences: NotificationPreferences,
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
    /// The refused combinations the user has already been told about.
    ///
    /// The link reports conflicts on **every** attachment, because it cannot
    /// know what has already been said; this is where issue #417's "once, and
    /// not on every reconnection" is kept. A recorder that loses its connection
    /// and comes back with the same combinations still refused is the ordinary
    /// case, and it is not news.
    ///
    /// Compared by combination and action rather than by count, so that a
    /// *different* hotkey being refused after the user changed one is news
    /// again. Sorted, because the order the recorder lists them in is not part
    /// of the fact.
    reported_conflicts: Option<Vec<(String, String)>>,
}

impl NotificationPolicy {
    /// A policy that starts from the state the application opened in.
    ///
    /// `opening_state` is not announced. See the module documentation: a
    /// notification is for something that happened while the user was away.
    pub(crate) fn new(
        preferences: NotificationPreferences,
        can_retry: bool,
        opening_state: &RecorderLinkState,
    ) -> Self {
        Self {
            preferences,
            can_retry,
            last_state: opening_state.clone(),
            recording: recording_in(opening_state).cloned(),
            reported_conflicts: None,
        }
    }

    /// What, if anything, to show the user for this event.
    pub(crate) fn decide(&mut self, event: &RecorderLinkEvent) -> Option<Notification> {
        // The bookkeeping happens whether or not the category is switched on, so
        // that switching one off cannot leave this unable to name a file later.
        let notification = self.consider(event);
        let settings = self.preferences.current();
        notification.filter(|notification| settings.allows(notification.category))
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
            RecorderLinkEvent::HotkeysUnavailable { conflicts } => {
                self.hotkeys_unavailable(conflicts)
            }
            // Deliberately no notification. An export is something the person
            // asked for and is watching a bar for on the screen they asked from
            // (issue #446); a desktop notification for each percentage point,
            // or even one at the end of a copy somebody is sitting in front of,
            // is the interruption AGENTS.md section 28 exists to prevent. It is
            // matched rather than fallen through so that the next event added
            // to this enumeration has to be thought about here.
            RecorderLinkEvent::ExportProgress(_) => None,
        }
    }

    /// Windows refused one or more global hotkeys.
    ///
    /// Answers `None` for a set the user has already been told about, which is
    /// what makes a reconnection to the same recorder silent.
    fn hotkeys_unavailable(&mut self, conflicts: &[HotkeyBinding]) -> Option<Notification> {
        let mut seen: Vec<(String, String)> = conflicts
            .iter()
            .map(|binding| {
                (
                    binding.hotkey.clone().unwrap_or_default(),
                    binding.action.clone(),
                )
            })
            .collect();
        seen.sort();

        if self.reported_conflicts.as_ref() == Some(&seen) {
            return None;
        }
        self.reported_conflicts = Some(seen);

        // The recorder's own sentence for the first one, because it is the one
        // that says who is likely to have the combination and what to do — and
        // repeating it for each of several would make a toast nobody reads
        // (AGENTS.md sections 28 and 45).
        let first = conflicts.first()?;
        let combination = first.hotkey.as_deref().unwrap_or("a combination");
        let said = match &first.state {
            HotkeyState::Conflict { reason } => as_sentence(reason),
            // Not reachable through the link, which filters to conflicts before
            // it sends. Stated rather than unwrapped so that a future caller
            // passing something else gets a true sentence instead of a panic.
            _ => format!("{combination} could not be registered."),
        };

        let body = if conflicts.len() == 1 {
            format!("{combination} does nothing: {said}")
        } else {
            format!(
                "{combination} does nothing: {said} {} more of Clipped's hotkeys were refused too.",
                conflicts.len() - 1
            )
        };

        Some(Notification {
            category: NotificationCategory::HotkeyUnavailable,
            title: format!("{} is unavailable", first.label),
            body,
            action: NotificationAction::OpenHotkeySettings,
        })
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
                action: NotificationAction::OpenClipped {
                    notice: format!("{title}. {said}"),
                },
                title,
                body: said,
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
    // `trimmed` is not empty, so there is a first character; `.next()` is
    // unwrapped through the loop rather than through a default that could only
    // ever produce the empty string already returned above.
    let mut sentence = String::new();
    for first in characters.by_ref().take(1) {
        sentence.extend(first.to_uppercase());
    }
    sentence.push_str(characters.as_str());

    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipped_ipc::{ErrorCode, SettingEntry};

    /// The settings as the recorder answers `get_settings`, with the categories
    /// in `switched_off` set to `false` and the rest to `true`.
    ///
    /// Built as a whole `SettingsView` rather than by reaching into
    /// [`NotificationSettings`], deliberately: what these tests are about is
    /// whether a switch somebody moved on the Settings screen reaches the
    /// decision, and the recorder's answer is the only thing that carries it.
    /// A test that set the booleans directly would still pass if
    /// [`NotificationSettings::from_view`] read the wrong key.
    fn answered(switched_off: &[NotificationCategory]) -> SettingsView {
        SettingsView {
            file: r"C:\Users\alex\AppData\Local\Clipped\settings.json".to_owned(),
            settings: NotificationCategory::ALL
                .into_iter()
                .map(|category| SettingEntry {
                    key: category.key().to_owned(),
                    label: category.key().to_owned(),
                    value: if switched_off.contains(&category) {
                        "false".to_owned()
                    } else {
                        "true".to_owned()
                    },
                    overridden: switched_off.contains(&category),
                    choices: vec!["true".to_owned(), "false".to_owned()],
                    accepted: "true or false".to_owned(),
                    applies: true,
                    unavailable: None,
                })
                .collect(),
        }
    }

    /// Preferences holding what a recorder answering `answered` would give.
    fn switches(switched_off: &[NotificationCategory]) -> NotificationPreferences {
        let preferences = NotificationPreferences::default();
        preferences.adopt(&answered(switched_off));
        preferences
    }

    /// The state a healthy attached recorder is in.
    fn idle() -> RecorderLinkState {
        RecorderLinkState::Attached {
            recorder_process_id: 4_242,
            features: Vec::new(),
            status: RecorderStatus::Idle,
        }
    }

    fn active(recording_id: &str) -> ActiveRecording {
        ActiveRecording {
            recording_id: recording_id.to_owned(),
            output: r"D:\clips\cs2-2026-08-11.mkv".to_owned(),
            target: "process cs2.exe".to_owned(),
            elapsed_ms: 90_000,
            replay_seconds: None,
            session: None,
        }
    }

    fn recording(recording_id: &str) -> RecorderLinkState {
        RecorderLinkState::Attached {
            recorder_process_id: 4_242,
            features: Vec::new(),
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
        NotificationPolicy::new(switches(&[]), true, &RecorderLinkState::Connecting)
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
        let mut policy =
            NotificationPolicy::new(switches(&[]), false, &RecorderLinkState::Connecting);

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
        // An installation whose recorder has been deleted starts unavailable
        // and stays there. The window is open, showing exactly that,
        // and a toast on every launch saying what is already on screen is how a
        // user learns to ignore them.
        let opening = unavailable("Clipped could not find clipped-recorder.exe");
        let mut policy = NotificationPolicy::new(switches(&[]), true, &opening);

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
            let off = switches(&[category]);

            let mut raised = Vec::new();
            for event in every_notifiable_event() {
                let mut policy =
                    NotificationPolicy::new(off.clone(), true, &RecorderLinkState::Connecting);
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
            RecorderLinkEvent::HotkeysUnavailable {
                conflicts: vec![refused("save_replay", "Save replay", "Ctrl+F10")],
            },
        ]
    }

    /// A binding Windows would not give this recorder.
    fn refused(action: &str, label: &str, combination: &str) -> HotkeyBinding {
        HotkeyBinding {
            action: action.to_owned(),
            label: label.to_owned(),
            hotkey: Some(combination.to_owned()),
            state: HotkeyState::Conflict {
                reason: format!(
                    "{combination} is already registered by another application, most likely one \
                     running in the background"
                ),
            },
            handled: true,
            unavailable: None,
        }
    }

    #[test]
    fn every_notification_carries_an_action_with_what_performing_it_needs() {
        // Acceptance criterion: an error notification leads to an action rather
        // than only a message.
        //
        // The previous version of this test asserted `action.label()` was not
        // empty, which proved nothing twice over. A label is a compile-time
        // constant, so the assertion could not fail; and every case it ran
        // resolved to `ShowFile` or `RetryRecorder`, so it never reached
        // `OpenClipped` at all — setting that variant's label to `""` left all
        // of these tests passing. Both faults are fixed here: the matrix reaches
        // all three variants and says so at the end, and what is asserted is
        // whether the action arrives carrying the thing
        // `crate::notifications::perform` needs to act on. An action without
        // that is a button that does nothing (AGENTS.md section 27).
        let mut reached = Vec::new();

        for can_retry in [true, false] {
            for known_recording in [true, false] {
                for event in every_notifiable_event() {
                    let mut policy = NotificationPolicy::new(
                        switches(&[]),
                        can_retry,
                        &RecorderLinkState::Connecting,
                    );
                    if known_recording {
                        policy.decide(&RecorderLinkEvent::State(recording("r-1")));
                    }

                    let notification = policy
                        .decide(&event)
                        .unwrap_or_else(|| panic!("{event:?} should notify"));

                    assert!(!notification.title.is_empty(), "{event:?} has no title");
                    assert!(!notification.body.is_empty(), "{event:?} says nothing");

                    match &notification.action {
                        NotificationAction::ShowFile { path } => assert!(
                            notification.body.contains(path.as_str()) && !path.is_empty(),
                            "Show the file has no file to show, or one the notification never \
                             named: {notification:?}"
                        ),
                        NotificationAction::RetryRecorder => assert!(
                            can_retry,
                            "Try again was offered to a link `retry` would do nothing to: \
                             {notification:?}"
                        ),
                        NotificationAction::OpenClipped { notice } => assert!(
                            notice.contains(&notification.title) && notice.len() > 1,
                            "Open Clipped would raise the window carrying nothing that says what \
                             happened: {notification:?}"
                        ),
                        // Carries nothing, because the destination is the whole
                        // of it. What it needs instead is for the notification
                        // to have named the combination — a toast that sends
                        // somebody to a settings screen without saying which
                        // hotkey is the vague message AGENTS.md section 28 is
                        // about.
                        NotificationAction::OpenHotkeySettings => assert!(
                            notification.body.contains("Ctrl+F10"),
                            "Change the hotkey never said which hotkey: {notification:?}"
                        ),
                    }

                    reached.push(notification.action.label());
                }
            }
        }

        // The half that the old test was missing. Without it a variant can stop
        // being produced, or stop having a label, and every assertion above
        // still passes because nothing ever reaches it.
        for label in [
            "Show the file",
            "Try again",
            "Open Clipped",
            "Change the hotkey",
        ] {
            assert!(
                reached.contains(&label),
                "no case in this test produces {label}, so nothing here says whether it works: \
                 {reached:?}"
            );
        }
    }

    /// Attaching to a recorder that reports the same refusals again.
    ///
    /// What the link really does on a reconnection: `follow` asks `get_hotkeys`
    /// on every attachment, so the policy sees this event as often as the
    /// connection drops.
    fn attached_again(
        policy: &mut NotificationPolicy,
        conflicts: Vec<HotkeyBinding>,
    ) -> Option<Notification> {
        policy.decide(&RecorderLinkEvent::State(RecorderLinkState::Reconnecting {
            attempt: 1,
            attempts_allowed: 5,
            delay_ms: 250,
            reason: "the pipe closed".to_owned(),
        }));
        policy.decide(&RecorderLinkEvent::HotkeysUnavailable { conflicts })
    }

    #[test]
    fn a_refused_hotkey_says_which_combination_which_action_and_where_to_change_it() {
        // Issue #417's second acceptance criterion. Somebody who never opens
        // Settings finds out that Ctrl+F10 is Discord's by pressing it in a game
        // and watching nothing happen; the whole point of the notification is
        // that it says which key and which action, and offers the screen where
        // that can be changed.
        let mut policy =
            NotificationPolicy::new(switches(&[]), true, &RecorderLinkState::Connecting);

        let notification = policy
            .decide(&RecorderLinkEvent::HotkeysUnavailable {
                conflicts: vec![refused("save_replay", "Save replay", "Ctrl+F10")],
            })
            .expect("a refused hotkey is worth telling somebody about");

        assert_eq!(
            notification.category,
            NotificationCategory::HotkeyUnavailable
        );
        assert!(
            notification.title.contains("Save replay"),
            "the notification never named the action: {notification:?}"
        );
        assert!(
            notification.body.contains("Ctrl+F10"),
            "the notification never named the combination: {notification:?}"
        );
        assert!(
            notification.body.contains("another application"),
            "the recorder's own explanation was dropped on the way: {notification:?}"
        );
        assert_eq!(notification.action, NotificationAction::OpenHotkeySettings);
    }

    #[test]
    fn a_refused_hotkey_is_notified_once_and_not_again_on_every_reconnection() {
        // Issue #417's first acceptance criterion, and the reason the policy
        // remembers rather than the link: `follow` asks `get_hotkeys` on every
        // attachment because it cannot know what has already been said, so a
        // recorder that drops its connection twice an hour would otherwise toast
        // twice an hour about a combination the user has already decided to live
        // with.
        let conflicts = vec![refused("save_replay", "Save replay", "Ctrl+F10")];
        let mut policy =
            NotificationPolicy::new(switches(&[]), true, &RecorderLinkState::Connecting);

        assert!(
            policy
                .decide(&RecorderLinkEvent::HotkeysUnavailable {
                    conflicts: conflicts.clone()
                })
                .is_some(),
            "the first time a hotkey is refused is news"
        );

        for attempt in 1..=3 {
            assert!(
                attached_again(&mut policy, conflicts.clone()).is_none(),
                "reconnection {attempt} announced the same refused hotkey again"
            );
        }
    }

    #[test]
    fn a_different_hotkey_being_refused_is_news_again() {
        // The other half of remembering: the set is compared by combination and
        // action, not by "have we ever said anything about hotkeys". Somebody
        // who changed Ctrl+F10 to Ctrl+F11 and hit a *different* application's
        // combination has a new problem, and a policy that only remembered
        // "already told them once" would leave them with a control that silently
        // does nothing.
        let mut policy =
            NotificationPolicy::new(switches(&[]), true, &RecorderLinkState::Connecting);

        policy
            .decide(&RecorderLinkEvent::HotkeysUnavailable {
                conflicts: vec![refused("save_replay", "Save replay", "Ctrl+F10")],
            })
            .expect("the first refusal is news");

        let second = attached_again(
            &mut policy,
            vec![refused("save_replay", "Save replay", "Ctrl+F11")],
        )
        .expect("a combination that was not refused before is news");

        assert!(
            second.body.contains("Ctrl+F11"),
            "the second notification is about the new combination: {second:?}"
        );
    }

    #[test]
    fn a_refused_hotkey_can_be_switched_off_on_its_own() {
        // Issue #417's third acceptance criterion. The generic
        // `every_category_can_be_switched_off_and_switching_one_off_leaves_the_others`
        // walks `ALL` and covers this too; it is asserted here as well because
        // that test would still pass if this category never produced a
        // notification at all.
        let mut policy = NotificationPolicy::new(
            switches(&[NotificationCategory::HotkeyUnavailable]),
            true,
            &RecorderLinkState::Connecting,
        );

        assert!(
            policy
                .decide(&RecorderLinkEvent::HotkeysUnavailable {
                    conflicts: vec![refused("save_replay", "Save replay", "Ctrl+F10")],
                })
                .is_none(),
            "a switched-off category must not interrupt anybody"
        );
    }

    #[test]
    fn a_switch_saved_while_clipped_is_running_reaches_the_very_next_notification() {
        // The half a settings screen depends on. The thread that decides holds
        // the policy and the thread that saves is the window's, so a switch that
        // only took effect at the next launch would be a control that does
        // nothing for the rest of the session (AGENTS.md section 27).
        let preferences = NotificationPreferences::default();
        let mut policy =
            NotificationPolicy::new(preferences.clone(), true, &RecorderLinkState::Connecting);

        assert!(
            policy
                .decide(&RecorderLinkEvent::RecordingInterrupted(active("r-1")))
                .is_some(),
            "everything is on until somebody says otherwise"
        );

        // What `apply_settings` answers with, adopted the way
        // `crate::main::apply_recorder_settings` adopts it.
        preferences.adopt(&answered(&[NotificationCategory::RecordingInterrupted]));

        assert_eq!(
            policy.decide(&RecorderLinkEvent::RecordingInterrupted(active("r-2"))),
            None,
            "the switch was saved and the notification arrived anyway",
        );
    }

    #[test]
    fn the_settings_default_to_telling_the_user_everything() {
        // Before the recorder has answered — and for a recorder too old to have
        // these settings at all — every category is on. Silence would be the
        // wrong way to fail: all four are failures.
        let settings = NotificationSettings::default();
        for category in NotificationCategory::ALL {
            assert!(
                settings.allows(category),
                "{} should default to on",
                category.key()
            );
        }

        assert_eq!(
            NotificationPreferences::default().current(),
            settings,
            "a window that has not asked yet must not have silenced anything",
        );
    }

    #[test]
    fn a_category_the_recorder_did_not_send_is_left_on_rather_than_read_as_off() {
        // A recorder older than this window has no such setting, and a window
        // that read its silence as "switched off" would stop telling somebody
        // that nothing is being recorded.
        let mut view = answered(&NotificationCategory::ALL);
        view.settings
            .retain(|entry| entry.key != NotificationCategory::RecorderUnavailable.key());

        let settings = NotificationSettings::from_view(&view);

        assert!(
            settings.allows(NotificationCategory::RecorderUnavailable),
            "a category nothing was said about must not be silenced",
        );
        assert!(
            !settings.allows(NotificationCategory::RecordingFailed),
            "and the categories that were sent are still read",
        );
    }

    #[test]
    fn every_category_is_read_from_the_key_the_settings_file_spells_it_with() {
        // The join between the two halves of this: the recorder writes these
        // keys into `settings.json` and this window matches on them. A rename on
        // either side silently switches a category back on, which is why they
        // are held equal by `settingsConformance.test.ts` as well.
        for category in NotificationCategory::ALL {
            let settings = NotificationSettings::from_view(&answered(&[category]));

            assert!(
                !settings.allows(category),
                "{} was switched off in the recorder's answer and read as on",
                category.key(),
            );
            for other in NotificationCategory::ALL {
                if other != category {
                    assert!(
                        settings.allows(other),
                        "{} was switched off by {}'s entry",
                        other.key(),
                        category.key(),
                    );
                }
            }
        }
    }

    #[test]
    fn a_value_neither_side_can_make_sense_of_leaves_the_category_on() {
        // The same rule as an absent key, and the same reason. `false` is the
        // only thing that silences anything.
        let mut view = answered(&[]);
        for entry in &mut view.settings {
            entry.value = "off".to_owned();
        }

        let settings = NotificationSettings::from_view(&view);
        for category in NotificationCategory::ALL {
            assert!(
                settings.allows(category),
                "{} was silenced by a value that is not `false`",
                category.key(),
            );
        }
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
