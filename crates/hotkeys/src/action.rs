//! What a hotkey asks the application to do, and what is behind each one today.

use core::fmt;

/// Something a global hotkey can ask the application to do.
///
/// The set is SPEC.md section 34, and it is deliberately closed: a hotkey is a
/// key combination pointed at one of *these*, not at an arbitrary command
/// string. The configuration UI ([issue
/// #54](https://github.com/wildware-uk/clipped/issues/54)) enumerates
/// [`ACTIONS`] to build its screen, so an action that exists here and is bound
/// to nothing still appears — with the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HotkeyAction {
    /// Write the last few minutes of the replay buffer to a clip.
    SaveReplay,
    /// Mark this moment in the recording without saving a clip.
    AddBookmark,
    /// Write a still image of what is on screen.
    TakeScreenshot,
    /// Start recording, or stop the recording in progress.
    ///
    /// One action rather than two, because it is one key: which of the two it
    /// means is decided by whether a recording is running, and only the process
    /// that owns the session knows that.
    ToggleRecording,
    /// Mute the microphone, whatever it was before.
    ///
    /// Distinct from [`Self::ToggleMicrophone`] on purpose. This is the key
    /// somebody hits when a housemate walks in: it has one outcome, and pressing
    /// it twice in a panic does not unmute.
    ///
    /// Nothing in any build can perform it yet — `clipped-audio` captures a
    /// microphone but cannot mute one mid-recording, which is [issue
    /// #234](https://github.com/wildware-uk/clipped/issues/234).
    MuteMicrophone,
    /// Mute the microphone if it is live, unmute it if it is muted. As
    /// [`Self::MuteMicrophone`], nothing performs it yet.
    ToggleMicrophone,
    /// Show or hide the in-game overlay.
    OpenOverlay,
}

/// Every action, in the order the configuration UI should list them.
///
/// An action's position here is also the identifier it is registered with, so
/// the order is load-bearing rather than cosmetic —
/// [`HotkeyAction::index`] and the tests around it are what hold the two
/// together when a variant is added.
pub const ACTIONS: [HotkeyAction; 7] = [
    HotkeyAction::SaveReplay,
    HotkeyAction::AddBookmark,
    HotkeyAction::TakeScreenshot,
    HotkeyAction::ToggleRecording,
    HotkeyAction::MuteMicrophone,
    HotkeyAction::ToggleMicrophone,
    HotkeyAction::OpenOverlay,
];

impl HotkeyAction {
    /// The action's stable name, for configuration files and logs.
    ///
    /// These match the command names on the recorder's control protocol
    /// (`docs/ipc.md`) where the same thing exists on both, so that a log line
    /// about `save_replay` means the same thing wherever it was written.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SaveReplay => "save_replay",
            Self::AddBookmark => "add_bookmark",
            Self::TakeScreenshot => "take_screenshot",
            Self::ToggleRecording => "toggle_recording",
            Self::MuteMicrophone => "mute_microphone",
            Self::ToggleMicrophone => "toggle_microphone",
            Self::OpenOverlay => "open_overlay",
        }
    }

    /// The action's name in the words a person reads, for the UI and for an
    /// error a user is shown (AGENTS.md section 28).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SaveReplay => "Save replay",
            Self::AddBookmark => "Add bookmark",
            Self::TakeScreenshot => "Take screenshot",
            Self::ToggleRecording => "Start or stop recording",
            Self::MuteMicrophone => "Mute microphone",
            Self::ToggleMicrophone => "Toggle microphone",
            Self::OpenOverlay => "Open overlay",
        }
    }

    /// The action with that [`name`](Self::name), if it is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        ACTIONS.iter().copied().find(|action| action.name() == name)
    }

    /// The action's position in [`ACTIONS`].
    ///
    /// This is also the identifier the combination is registered with on
    /// Windows, which is why it is defined by an exhaustive match rather than
    /// by searching [`ACTIONS`]: adding a variant then has to say where it goes,
    /// and `every_action_is_listed_once_and_indexed_by_its_position` fails if
    /// the two disagree.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::SaveReplay => 0,
            Self::AddBookmark => 1,
            Self::TakeScreenshot => 2,
            Self::ToggleRecording => 3,
            Self::MuteMicrophone => 4,
            Self::ToggleMicrophone => 5,
            Self::OpenOverlay => 6,
        }
    }

    /// The action at that position in [`ACTIONS`], if there is one.
    #[must_use]
    pub fn from_index(index: usize) -> Option<Self> {
        ACTIONS.get(index).copied()
    }

    /// The subsystem this action needs and no build of Clipped has yet, if that
    /// is why nothing handles it.
    ///
    /// [`None`] does not mean the action works: it means the missing piece is a
    /// handler from the process that owns the session, not a subsystem nobody
    /// has written. Starting a recording and muting a microphone are both
    /// things this build can do; whether *this* process was given a handler for
    /// them is a separate question, and [`crate::Unhandled`] is what answers it.
    ///
    /// Saving a replay is the one to read carefully. The buffer and the save
    /// exist — [issue #37](https://github.com/wildware-uk/clipped/issues/37)
    /// wrote `clipped_replay::save_clip`, and `clipped_session` can fill a
    /// buffer while it records. What no build has is a *recording that is
    /// running one*: nothing starts a buffered session and nothing carries a
    /// save request to it, which is [issue
    /// #38](https://github.com/wildware-uk/clipped/issues/38). So this stays
    /// [`Some`] and names what is genuinely absent, rather than naming a buffer
    /// that has been built.
    #[must_use]
    pub const fn planned_subsystem(self) -> Option<PlannedSubsystem> {
        match self {
            Self::SaveReplay => Some(PlannedSubsystem::new(
                "a recording with a replay buffer",
                "M3",
                38,
            )),
            Self::AddBookmark => Some(PlannedSubsystem::new("bookmarks", "M8", 64)),
            Self::TakeScreenshot => Some(PlannedSubsystem::new("screenshot capture", "M8", 67)),
            Self::OpenOverlay => Some(PlannedSubsystem::new("the in-game overlay", "M5", 53)),
            Self::ToggleRecording | Self::MuteMicrophone | Self::ToggleMicrophone => None,
        }
    }
}

impl fmt::Display for HotkeyAction {
    /// The label, so that `{action}` in a message meant for a person reads as
    /// one.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// A subsystem an action needs, which milestone builds it and which issue
/// tracks it.
///
/// The shape is deliberately the one `clipped-ipc` refuses an unbuilt command
/// with (`ProtocolError::not_implemented`, `docs/ipc.md`): a UI that can say
/// "Save replay — not in this build (M3, issue #38)" is telling the truth,
/// where a key that silently does nothing is not (AGENTS.md sections 27 and
/// 54). This crate does not depend on `clipped-ipc` to say it — both sit at the
/// bottom of the stack and neither may name the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedSubsystem {
    subsystem: &'static str,
    milestone: &'static str,
    tracking_issue: u32,
}

impl PlannedSubsystem {
    /// Names a subsystem, the milestone that builds it and the issue that
    /// tracks it.
    #[must_use]
    const fn new(subsystem: &'static str, milestone: &'static str, tracking_issue: u32) -> Self {
        Self {
            subsystem,
            milestone,
            tracking_issue,
        }
    }

    /// What is missing, in the words a person would use.
    #[must_use]
    pub const fn subsystem(&self) -> &'static str {
        self.subsystem
    }

    /// The milestone that builds it.
    #[must_use]
    pub const fn milestone(&self) -> &'static str {
        self.milestone
    }

    /// The issue that tracks it.
    #[must_use]
    pub const fn tracking_issue(&self) -> u32 {
        self.tracking_issue
    }
}

impl fmt::Display for PlannedSubsystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} arrives in {} (issue #{})",
            self.subsystem, self.milestone, self.tracking_issue
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{HotkeyAction, ACTIONS};

    /// The one that has teeth when a variant is added.
    ///
    /// A new action forces an arm in `index`, because that match is exhaustive.
    /// If the arm reuses an index, the first assertion fails; if it takes the
    /// next free one without the variant being added to [`ACTIONS`], the second
    /// does. Either way the two cannot drift, and the identifier a combination
    /// is registered with stays unique — which is the property that actually
    /// matters, because Windows keys its hotkey table on it.
    #[test]
    fn every_action_is_listed_once_and_indexed_by_its_position() {
        for (position, action) in ACTIONS.iter().enumerate() {
            assert_eq!(
                action.index(),
                position,
                "{action} is at position {position} of ACTIONS but reports index {}",
                action.index()
            );
        }

        for action in ACTIONS {
            assert!(
                action.index() < ACTIONS.len(),
                "{action} has an index outside ACTIONS, so it was given an \
                 identifier without being added to the list"
            );
        }
    }

    #[test]
    fn names_are_unique_and_round_trip() {
        for action in ACTIONS {
            assert_eq!(
                HotkeyAction::from_name(action.name()),
                Some(action),
                "{action} does not parse back from its own name"
            );
        }

        let mut names: Vec<&str> = ACTIONS.iter().map(|action| action.name()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(count, names.len(), "two actions share a name: {names:?}");
    }

    /// The sentence a user is shown, held to what the build can actually do.
    ///
    /// It names issue #38 and not #37 deliberately. #37 built the replay buffer
    /// and its save; what is still missing is a recording running one and a
    /// route from this key to it, and that is #38. Naming the finished issue
    /// would tell a user that a shipped feature is unbuilt (AGENTS.md sections
    /// 27 and 54).
    #[test]
    fn an_unbuilt_action_says_which_milestone_and_issue_builds_it() {
        let planned = HotkeyAction::SaveReplay
            .planned_subsystem()
            .expect("no build runs a recording with a replay buffer yet");

        assert_eq!(planned.milestone(), "M3");
        assert_eq!(planned.tracking_issue(), 38);
        assert_eq!(
            planned.to_string(),
            "a recording with a replay buffer arrives in M3 (issue #38)"
        );
    }

    #[test]
    fn an_action_whose_subsystem_exists_names_no_missing_one() {
        // Recording exists (M1). What a build may be missing is a handler, and
        // that is a different sentence — see `Unhandled`.
        assert_eq!(HotkeyAction::ToggleRecording.planned_subsystem(), None);
    }
}
