//! What the recorder's global hotkeys are doing, as the window reads it.
//!
//! # Why this is on the protocol at all
//!
//! `RegisterHotKey` gives a combination to exactly one process, and
//! [ADR 0009](../../../docs/adr/0009-the-recorder-registers-global-hotkeys.md)
//! made that process the recorder: it is the one that outlives every window, and
//! the one that can act on a press. So the window is not the process that finds
//! out that Discord already owns `Ctrl`+`F10` — the recorder is, at the moment it
//! registers, which may be days before anybody opens a window.
//!
//! A combination the user cannot have is exactly the failure a hotkey has: the
//! key does nothing and nothing says why. Writing it to the recorder's log
//! satisfies nobody, so `get_hotkeys` is the question the window asks and this is
//! the answer (AGENTS.md sections 27 and 45).
//!
//! # Why it is asked for rather than pushed
//!
//! Registration happens once, when the recorder starts. An event announcing a
//! conflict would be published before any window existed to hear it, and the
//! window that opened an hour later would show nothing — which is the state this
//! whole reply exists to prevent. A question has no such race: whenever the
//! window asks, the answer is the current one.
//!
//! # Why these types do not come from `clipped-hotkeys`
//!
//! This crate depends on no other crate of the workspace and may not start
//! (ADR 0002, `tests/integration/tests/workspace_layering.rs`): the desktop
//! application links it, and nothing of the recording engine may come with it.
//! So these are wire types, and the recorder maps its own
//! `clipped_hotkeys::Registration` onto them — the same relationship
//! [`RecorderStatus`](crate::RecorderStatus) has with a recording.

use serde::{Deserialize, Serialize};

/// One action, what it is bound to, and whether pressing it would do anything.
///
/// One row of the hotkey list a window draws. Every field answers a question
/// somebody looking at that row is asking, and the two that matter most are the
/// two a naive implementation leaves out: [`state`](Self::state), because a
/// binding Windows refused is not a binding, and
/// [`unavailable`](Self::unavailable), because a *registered* combination whose
/// action nothing performs is still a key that does nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    /// The action's stable name, such as `save_replay`.
    ///
    /// The same name the command it performs has where there is one, so a log
    /// line about `add_bookmark` means one thing whichever side wrote it.
    pub action: String,
    /// The action's name in the words a person reads, such as `Save replay`.
    ///
    /// Sent rather than derived, so that the list of actions has one home. A
    /// window that kept its own table of labels would be a second answer to
    /// "what is this action called", and would silently show nothing at all for
    /// an action a newer recorder had added.
    pub label: String,
    /// The combination, written as `Ctrl+F10`. Absent when the action is bound
    /// to nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    /// What Windows said about it.
    pub state: HotkeyState,
    /// Whether anything in the recorder performs the action.
    ///
    /// A registered combination with no handler still does nothing when pressed.
    /// Drawing such a row as working would be the lie AGENTS.md section 27 is
    /// about, so this is a field rather than an inference from
    /// [`state`](Self::state).
    pub handled: bool,
    /// Why pressing it would do nothing, when that is the case.
    ///
    /// Present exactly when [`handled`](Self::handled) is `false`, and it is the
    /// recorder's own sentence: "Save replay is not in this build: a recording
    /// with a replay buffer arrives in M3 (issue #38)". Absent for an action the
    /// recorder performs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// What Windows said about one binding.
///
/// **Closed, and deliberately without a catch-all**, for the reason
/// [`RecorderStatus`](crate::RecorderStatus) has none (`docs/ipc.md`): a state a
/// client cannot read is one it would otherwise draw as something else, and the
/// something else here is "this hotkey works". An unreadable frame is the honest
/// outcome; an event a client cannot read is information it does without, and
/// this is not an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HotkeyState {
    /// The action has no combination, so nothing was registered.
    Unbound,
    /// Windows accepted it, and presses are being delivered.
    Registered,
    /// Windows refused it, most often because another application owns it.
    Conflict {
        /// What to tell the user, in the recorder's own words: what failed, who
        /// is likely to have it, and what to do next (AGENTS.md section 45).
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{HotkeyBinding, HotkeyState};

    #[test]
    fn an_unbound_action_leaves_the_combination_out_rather_than_sending_an_empty_one() {
        let row = HotkeyBinding {
            action: "open_overlay".to_owned(),
            label: "Open overlay".to_owned(),
            hotkey: None,
            state: HotkeyState::Unbound,
            handled: false,
            unavailable: Some("the in-game overlay arrives in M5 (issue #53)".to_owned()),
        };

        let json = serde_json::to_string(&row).expect("it serialises");
        assert_eq!(
            json,
            r#"{"action":"open_overlay","label":"Open overlay","state":{"state":"unbound"},"handled":false,"unavailable":"the in-game overlay arrives in M5 (issue #53)"}"#,
        );
        assert_eq!(
            serde_json::from_str::<HotkeyBinding>(&json).expect("and deserialises"),
            row,
        );
    }

    /// The row this whole type exists for: a hotkey the user cannot have,
    /// carrying the sentence that says why.
    #[test]
    fn a_refused_combination_carries_the_reason_across_the_wire() {
        let row = HotkeyBinding {
            action: "save_replay".to_owned(),
            label: "Save replay".to_owned(),
            hotkey: Some("Ctrl+F10".to_owned()),
            state: HotkeyState::Conflict {
                reason: "another application already uses it".to_owned(),
            },
            handled: false,
            unavailable: None,
        };

        let json = serde_json::to_string(&row).expect("it serialises");
        let back: HotkeyBinding = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, row);
        assert!(
            json.contains("another application already uses it"),
            "a conflict that arrives without its reason is a log line with extra steps: {json}",
        );
    }

    /// `docs/ipc.md`: an unknown *state* must make the frame unreadable rather
    /// than be smoothed into something plausible.
    #[test]
    fn a_state_this_build_has_never_heard_of_is_refused_rather_than_guessed() {
        let json = r#"{"action":"save_replay","label":"Save replay","state":{"state":"pending"},"handled":true}"#;

        assert!(
            serde_json::from_str::<HotkeyBinding>(json).is_err(),
            "a state this build cannot read must not parse as one it can",
        );
    }
}
