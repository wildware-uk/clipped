//! What the tray shows, derived from what the application actually knows.
//!
//! This module is a function and some data. It takes the recorder link's state
//! and the last window the user was in, and returns the mark on the icon, the
//! tooltip, and the label and enabled state of every menu item. It touches
//! nothing: no Tauri types, no Windows, no I/O. That is deliberate — it is the
//! part of the tray with rules in it, and rules that cannot be tested are rules
//! nobody can change safely.
//!
//! # The two rules everything here follows
//!
//! **Nothing is shown that the application does not know** (AGENTS.md section
//! 27). The status line is the link's own state, in words. There is no
//! "buffering" mark, because the recorder cannot report one: the replay buffer
//! exists as a crate and no `serve` runs it, so a tray that showed buffering
//! would be showing something nobody measured.
//!
//! **Nothing is offered that would do nothing.** Every item is either something
//! this build can perform or is disabled with the reason in its own label. Save
//! Replay and Add Bookmark are commands the protocol defines and this build
//! refuses, and their labels carry the subsystem and the issue *from the
//! protocol's own typed refusal* rather than from a sentence typed here — so
//! the day the recorder gains one, this stops claiming it has not.

use clipped_ipc::{RecorderLinkState, RecorderStatus, UnbuiltCommand};

use crate::foreground::ForegroundWindow;
use crate::tray_icon::TrayMark;

/// One line of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuEntry {
    /// What it says.
    pub(crate) label: String,
    /// Whether it can be clicked.
    ///
    /// A disabled entry always says why in its own label. A menu item has
    /// nowhere else to put a reason — there is no tooltip and no help text in a
    /// notification-area menu — so "greyed out with no explanation", which
    /// AGENTS.md section 27 calls out by name, is avoided the only way it can be.
    pub(crate) enabled: bool,
}

impl MenuEntry {
    /// An item that can be clicked.
    fn live(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            enabled: true,
        }
    }

    /// An item that cannot, and says why.
    fn refused(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            enabled: false,
        }
    }

    /// The label for a command the protocol defines and this build refuses.
    ///
    /// Built from [`UnbuiltCommand`]'s own subsystem and tracking issue, which
    /// are the same two facts the recorder puts in the `not_implemented` refusal
    /// it would send. One source, so the menu cannot go on claiming a subsystem
    /// is missing after it has been built.
    fn unbuilt(name: &str, command: UnbuiltCommand) -> Self {
        Self::refused(format!(
            "{name} — needs {} (#{})",
            command.subsystem(),
            command.tracking_issue()
        ))
    }
}

/// What clicking [`TrayModel::record`] should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordAction {
    /// Start recording the process behind the last window the user was in.
    Start {
        /// Which process, as `start_recording`'s `pid` parameter.
        process_id: u32,
    },
    /// Stop whatever is running.
    Stop,
    /// Neither: the item is disabled, and its label says why.
    Nothing,
}

/// What the tray looks like and what its items do, right now.
///
/// Compared rather than rebuilt blindly: the menu is only replaced when this
/// changes, so following the foreground window does not mean rebuilding a menu
/// on every alt-tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrayModel {
    /// The mark on the icon.
    pub(crate) mark: TrayMark,
    /// The icon's tooltip: the same state in words, for a pointer and for a
    /// screen reader.
    pub(crate) tooltip: String,
    /// The first line of the menu, which SPEC.md section 33 asks for.
    pub(crate) status: MenuEntry,
    /// Save Replay.
    pub(crate) save_replay: MenuEntry,
    /// Add Bookmark.
    pub(crate) add_bookmark: MenuEntry,
    /// Start or Stop Recording, whichever applies.
    pub(crate) record: MenuEntry,
    /// What that item does.
    pub(crate) record_action: RecordAction,
    /// Open Library.
    pub(crate) library: MenuEntry,
    /// Settings.
    pub(crate) settings: MenuEntry,
    /// Exit.
    pub(crate) exit: MenuEntry,
    /// Whether exiting will stop a recording and finish its file.
    ///
    /// The whole of the "the user must not silently lose footage" requirement in
    /// the interface: when this is true the item does not say "Exit", it says
    /// what it is about to do, and the shutdown sent afterwards carries the
    /// permission that says the same thing to the recorder.
    pub(crate) exit_finalises_a_recording: bool,
}

/// The tray, as it should look given what the application knows.
pub(crate) fn tray_model(
    link: &RecorderLinkState,
    foreground: Option<&ForegroundWindow>,
) -> TrayModel {
    let recording = match link {
        RecorderLinkState::Attached {
            status: RecorderStatus::Recording(active),
            ..
        } => Some(active),
        _ => None,
    };

    let (mark, status, tooltip) = describe(link);
    let (record, record_action) = record_entry(link, recording.is_some(), foreground);

    TrayModel {
        mark,
        tooltip,
        // Not a control. It is the sentence SPEC.md section 33 puts at the top
        // of the menu, and there is nothing to click.
        status: MenuEntry::refused(status),
        save_replay: MenuEntry::unbuilt("Save Replay", UnbuiltCommand::SaveReplay),
        add_bookmark: MenuEntry::unbuilt("Add Bookmark", UnbuiltCommand::AddBookmark),
        record,
        record_action,
        // Both of these open the window at a screen. Neither screen is written
        // yet, and each one says so and names the issue that builds it — which
        // is a thing that happens rather than a control that does nothing.
        library: MenuEntry::live("Open Library"),
        settings: MenuEntry::live("Settings"),
        exit: MenuEntry::live(if recording.is_some() {
            "Stop recording and exit"
        } else {
            "Exit"
        }),
        exit_finalises_a_recording: recording.is_some(),
    }
}

/// The mark, the menu's first line and the tooltip, for one link state.
///
/// One arm per state and no default, so a state added to `RecorderLinkState`
/// stops this compiling rather than being drawn as whatever the last arm
/// happened to be.
fn describe(link: &RecorderLinkState) -> (TrayMark, String, String) {
    match link {
        RecorderLinkState::Connecting => (
            TrayMark::Connecting,
            "Looking for the recorder".to_owned(),
            "Clipped — looking for the recorder".to_owned(),
        ),
        RecorderLinkState::Attached {
            status: RecorderStatus::Idle,
            ..
        } => (
            TrayMark::Idle,
            "Not recording".to_owned(),
            "Clipped — not recording".to_owned(),
        ),
        RecorderLinkState::Attached {
            status: RecorderStatus::Recording(active),
            ..
        } => (
            TrayMark::Recording,
            format!("Recording {}", active.target),
            format!("Clipped — recording {}", active.target),
        ),
        RecorderLinkState::Reconnecting {
            attempt,
            attempts_allowed,
            ..
        } => (
            TrayMark::Connecting,
            format!("Reconnecting — attempt {attempt} of {attempts_allowed}"),
            format!(
                "Clipped — reconnecting to the recorder, attempt {attempt} of {attempts_allowed}"
            ),
        ),
        // The reason is a whole sentence and belongs where there is room for
        // it, which is the window. The menu says the state and the tooltip
        // says where to look.
        RecorderLinkState::Unavailable { reason } => (
            TrayMark::Unavailable,
            "No recorder".to_owned(),
            format!("Clipped — no recorder. {reason}"),
        ),
    }
}

/// What the window is told when Exit could not reach the recorder.
///
/// Exit is the only path that stops the recorder, so a shutdown that could not
/// be delivered leaves a recorder running that nothing on screen accounts for.
/// A release build has no console, so saying it to standard error says it to
/// nobody; this is the sentence that goes to the window instead
/// (AGENTS.md sections 17 and 45).
///
/// Two halves, and both are needed. What is at stake comes from the last state
/// the link published — the only thing this window still knows about the
/// recorder — and is deliberately different for "it was recording" and "there
/// is no way to tell", because claiming the second is safe would be inventing a
/// state nobody measured (AGENTS.md section 27). What to do about it names Task
/// Manager, because with the endpoint unreachable that is genuinely the only
/// thing left, and a message with no action in it is the failure section 45
/// describes.
pub(crate) fn could_not_reach_the_recorder(link: &RecorderLinkState, error: &str) -> String {
    let at_stake = match link {
        RecorderLinkState::Attached {
            status: RecorderStatus::Recording(active),
            ..
        } => format!(
            "It was last recording {} to {}, and that recording is still running.",
            active.target, active.output
        ),
        _ => "Clipped cannot tell whether it is still recording.".to_owned(),
    };

    format!(
        "Clipped has not exited, because {error}. {at_stake} Choose Exit again to close this \
         window anyway — the recorder is a separate process and will go on running until it is \
         stopped, which without Clipped means ending clipped-recorder.exe in Task Manager."
    )
}

/// The Start/Stop Recording item, and what it does.
///
/// Three ways it can be disabled, and each says which:
///
/// - there is no recorder to ask;
/// - there is one, nothing is recording, and nothing has been in front of this
///   window to record — a machine just signed into, or one where the foreground
///   hook could not be installed;
/// - nothing at all is wrong and it is live.
fn record_entry(
    link: &RecorderLinkState,
    recording: bool,
    foreground: Option<&ForegroundWindow>,
) -> (MenuEntry, RecordAction) {
    if recording {
        return (MenuEntry::live("Stop Recording"), RecordAction::Stop);
    }

    if !matches!(link, RecorderLinkState::Attached { .. }) {
        return (
            MenuEntry::refused("Start Recording — no recorder"),
            RecordAction::Nothing,
        );
    }

    match foreground {
        Some(window) => (
            MenuEntry::live(format!("Start Recording — {}", window.process_name)),
            RecordAction::Start {
                process_id: window.process_id,
            },
        ),
        None => (
            MenuEntry::refused("Start Recording — nothing in front to record"),
            RecordAction::Nothing,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipped_ipc::ActiveRecording;

    fn attached(status: RecorderStatus) -> RecorderLinkState {
        RecorderLinkState::Attached {
            recorder_process_id: 4_242,
            status,
        }
    }

    fn recording() -> RecorderLinkState {
        attached(RecorderStatus::Recording(ActiveRecording {
            recording_id: "r-1".to_owned(),
            output: r"D:\clips\session.mkv".to_owned(),
            target: "process `cs2.exe`".to_owned(),
            elapsed_ms: 4_200,
        }))
    }

    fn game() -> ForegroundWindow {
        ForegroundWindow {
            process_id: 4_242,
            process_name: "cs2.exe".to_owned(),
        }
    }

    /// Every state the link can be in, for the tests that walk all of them.
    fn every_link_state() -> Vec<RecorderLinkState> {
        vec![
            RecorderLinkState::Connecting,
            attached(RecorderStatus::Idle),
            recording(),
            RecorderLinkState::Reconnecting {
                attempt: 2,
                attempts_allowed: 4,
                delay_ms: 2_000,
                reason: "the connection ended".to_owned(),
            },
            RecorderLinkState::Unavailable {
                reason: "the recorder was not found at C:\\clipped-recorder.exe".to_owned(),
            },
        ]
    }

    #[test]
    fn no_enabled_item_has_nothing_behind_it() {
        // The acceptance criterion, as a property rather than as five
        // assertions: whatever the application knows, an item a user can click
        // does something. Save Replay and Add Bookmark are the ones with no
        // subsystem, and they are never enabled in any state.
        for link in every_link_state() {
            for foreground in [None, Some(game())] {
                let model = tray_model(&link, foreground.as_ref());

                assert!(!model.save_replay.enabled, "{link:?}");
                assert!(!model.add_bookmark.enabled, "{link:?}");
                assert!(!model.status.enabled, "the status line is not a control");
                assert_eq!(
                    model.record.enabled,
                    model.record_action != RecordAction::Nothing,
                    "{link:?} with foreground {foreground:?}: an enabled Start/Stop Recording has \
                     to have something to do, and a disabled one must not"
                );
            }
        }
    }

    #[test]
    fn every_disabled_item_says_why_in_its_own_label() {
        // A menu item has no tooltip and no help text, so the label is the only
        // place a reason can go. "Greyed out with no explanation" is the failure
        // AGENTS.md section 27 names.
        for link in every_link_state() {
            for foreground in [None, Some(game())] {
                let model = tray_model(&link, foreground.as_ref());
                for entry in [&model.save_replay, &model.add_bookmark, &model.record] {
                    if !entry.enabled {
                        // The dash is not the point; what follows it is. A label
                        // that merely *ends* in an em dash is as unexplained as
                        // one without it, so both sides are held to being words.
                        let (offer, reason) = entry.label.split_once('—').unwrap_or_else(|| {
                            panic!("`{}` is disabled and does not say why", entry.label)
                        });
                        assert!(
                            !offer.trim().is_empty(),
                            "`{}` says a reason and does not say what for",
                            entry.label
                        );
                        assert!(
                            !reason.trim().is_empty(),
                            "`{}` is disabled and the reason after the dash is empty",
                            entry.label
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_unbuilt_items_name_the_subsystem_and_the_issue_that_builds_it() {
        let model = tray_model(&attached(RecorderStatus::Idle), Some(&game()));

        // #38 and not #37: #37 built the replay buffer and its save, so a menu
        // that still named it would be telling a user a shipped feature is
        // unbuilt. What is missing is a recording running one.
        assert_eq!(
            model.save_replay.label,
            "Save Replay — needs a recording with a replay buffer (#38)"
        );
        assert_eq!(
            model.add_bookmark.label,
            "Add Bookmark — needs bookmarks (#64)"
        );
    }

    #[test]
    fn a_recorder_that_is_recording_offers_to_stop_it_and_says_so_on_the_icon() {
        let model = tray_model(&recording(), Some(&game()));

        assert_eq!(model.mark, TrayMark::Recording);
        assert_eq!(model.status.label, "Recording process `cs2.exe`");
        assert_eq!(model.record.label, "Stop Recording");
        assert_eq!(model.record_action, RecordAction::Stop);
    }

    #[test]
    fn exiting_while_recording_says_that_is_what_it_will_do() {
        // The whole of "the user must not silently lose footage" as the menu
        // expresses it: the item stops saying "Exit" and says what it is about
        // to do, and the flag beside it is what makes the shutdown carry the
        // permission the recorder insists on.
        let recording_model = tray_model(&recording(), None);
        assert_eq!(recording_model.exit.label, "Stop recording and exit");
        assert!(recording_model.exit_finalises_a_recording);

        let idle_model = tray_model(&attached(RecorderStatus::Idle), None);
        assert_eq!(idle_model.exit.label, "Exit");
        assert!(!idle_model.exit_finalises_a_recording);
    }

    #[test]
    fn exit_is_always_offered_because_it_is_the_only_way_out() {
        // Including when the recorder is unreachable: the window still has to
        // be closable, and a shutdown sent to nothing reports that nothing was
        // running rather than failing.
        for link in every_link_state() {
            assert!(tray_model(&link, None).exit.enabled, "{link:?}");
        }
    }

    #[test]
    fn starting_a_recording_names_what_it_would_record_rather_than_only_offering_to() {
        let model = tray_model(&attached(RecorderStatus::Idle), Some(&game()));

        assert_eq!(model.record.label, "Start Recording — cs2.exe");
        assert_eq!(
            model.record_action,
            RecordAction::Start { process_id: 4_242 },
            "the item has to carry the process it named, or it would record something else"
        );
    }

    #[test]
    fn a_state_with_no_recorder_never_offers_to_start_one_recording() {
        for link in [
            RecorderLinkState::Connecting,
            RecorderLinkState::Unavailable {
                reason: "no recorder".to_owned(),
            },
            RecorderLinkState::Reconnecting {
                attempt: 1,
                attempts_allowed: 4,
                delay_ms: 1_000,
                reason: "the connection ended".to_owned(),
            },
        ] {
            let model = tray_model(&link, Some(&game()));
            assert!(!model.record.enabled, "{link:?}");
            assert_eq!(model.record_action, RecordAction::Nothing, "{link:?}");
        }
    }

    #[test]
    fn an_exit_that_could_not_reach_the_recorder_names_the_recording_it_is_leaving_behind() {
        // The failure this exists to prevent: the window goes, a recording goes
        // on, and nothing anywhere says so. The sentence has to carry the file,
        // because it is the one thing nothing else will ever mention.
        let said = could_not_reach_the_recorder(&recording(), "the pipe was busy");

        assert!(said.contains(r"D:\clips\session.mkv"), "{said}");
        assert!(said.contains("process `cs2.exe`"), "{said}");
        assert!(said.contains("still running"), "{said}");
        assert!(said.contains("the pipe was busy"), "{said}");
    }

    #[test]
    fn an_exit_that_could_not_reach_the_recorder_never_claims_it_was_not_recording() {
        // Every state but "attached and recording" leaves this window unable to
        // tell, and the one thing it may not do is say the reassuring half of
        // that as though it knew (AGENTS.md section 27).
        for link in every_link_state() {
            let said = could_not_reach_the_recorder(&link, "the recorder went away");
            let recording = matches!(
                link,
                RecorderLinkState::Attached {
                    status: RecorderStatus::Recording(_),
                    ..
                }
            );

            assert_eq!(
                said.contains("cannot tell whether it is still recording"),
                !recording,
                "{link:?} said `{said}`"
            );
            // And whatever it could tell, it always ends with something the
            // user can actually do (AGENTS.md section 45).
            assert!(said.contains("Exit again"), "{link:?} said `{said}`");
            assert!(said.contains("Task Manager"), "{link:?} said `{said}`");
        }
    }

    #[test]
    fn the_mark_and_the_words_never_disagree() {
        // The icon is the only part of this a user sees without opening
        // anything, so it has to be the same statement as the tooltip. A mark
        // that said "recording" beside a tooltip that said "not recording"
        // would be the kind of drift nobody notices until a bug report.
        for link in every_link_state() {
            let model = tray_model(&link, None);
            let says_recording =
                model.tooltip.contains("recording") && !model.tooltip.contains("not recording");
            assert_eq!(
                model.mark == TrayMark::Recording,
                says_recording,
                "{link:?} draws {:?} and says `{}`",
                model.mark,
                model.tooltip
            );
        }
    }
}
