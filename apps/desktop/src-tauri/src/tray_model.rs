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
//! Replay is a command the protocol defines and this build refuses, and its
//! label carries the subsystem and the issue *from the protocol's own typed
//! refusal* rather than from a sentence typed here — so the day the recorder
//! gains one, this stops claiming it has not. Add Bookmark is what that looked
//! like the day it happened ([issue
//! #64](https://github.com/wildware-uk/clipped/issues/64)): the refusal it
//! quoted no longer exists, so the item is a control, disabled while there is
//! no recording to put a bookmark in — or while the attached recorder never
//! said it could make one ([issue
//! #447](https://github.com/wildware-uk/clipped/issues/447)).
//!
//! That second condition is about the *recorder*, not the build. An installed
//! Clipped can find an older recorder still running from a previous version,
//! and every item here maps to a command that recorder may not have. It says so
//! in its welcome, and asking is the difference between a control that refuses
//! before it is pressed and one that refuses after.

use clipped_ipc::{features, ActiveRecording, RecorderLinkState, RecorderStatus};

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
        save_replay: replay_entry(link, recording),
        add_bookmark: bookmark_entry(link, recording.is_some()),
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
        RecorderLinkState::Attached { status, .. } => describe_status(status),
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

/// The same three things, for what an attached recorder says it is doing.
///
/// One arm per status and no default, for the reason [`describe`] has one per
/// link state: a fourth state added to `RecorderStatus` is a protocol version
/// bump, and it must stop this compiling rather than be drawn as whichever arm
/// happened to be last.
fn describe_status(status: &RecorderStatus) -> (TrayMark, String, String) {
    match status {
        RecorderStatus::Idle => (
            TrayMark::Idle,
            "Not recording".to_owned(),
            "Clipped — not recording".to_owned(),
        ),
        // Nothing is being recorded, so the mark is the idle one: a badge of
        // its own would be a tray design decision, and issue #584's scope was
        // the protocol having a word for the state rather than the tray
        // gaining a fourth mark for it.
        //
        // The sentence is not the idle one, though. A sitting still open in
        // its restart grace has a game name, and dropping it for those seconds
        // is exactly the flicker `Watching::session` exists to prevent.
        RecorderStatus::Watching(_) => match game_in(status) {
            Some(game) => (
                TrayMark::Idle,
                format!("Watching — {game}"),
                format!("Clipped — watching for {game}"),
            ),
            None => (
                TrayMark::Idle,
                "Watching for a game".to_owned(),
                "Clipped — watching for a game".to_owned(),
            ),
        },
        // **The game, when the recording's sitting names one.** This said
        // `Recording process 4242` until
        // [issue #588](https://github.com/wildware-uk/clipped/issues/588),
        // which is the line above going from a game's name while the recorder
        // waited to a process identifier the moment it started recording it —
        // backwards, and precisely the sentence issue #241 put
        // `ActiveRecording::session` on the wire to make sayable. The tray knew
        // the game and said the selector, which is withholding rather than
        // lying and is no more allowed (AGENTS.md section 27).
        RecorderStatus::Recording(active) => {
            let of = game_in(status).unwrap_or(&active.target);
            let line = match sitting_position(status) {
                // "the second file of this sitting", which is what stops the
                // next file of a game somebody is still playing looking like an
                // unrelated recording. Said only when there is more than one,
                // because "file 1 of this sitting" is noise.
                Some(index) => format!("Recording {of}, file {index} of this sitting"),
                None => format!("Recording {of}"),
            };
            let tooltip = format!("Clipped — recording {of}");
            (TrayMark::Recording, line, tooltip)
        }
    }
}

/// The game the recorder's sitting is of, when the catalogue named one.
///
/// Asked of the whole status rather than of each arm, through
/// `RecorderStatus::session`, because a sitting spans two of them: one being
/// recorded is on `Recording` and one waiting out its restart grace is on
/// `Watching`, and they are the same sitting a few seconds apart. Matching on
/// the state twice here would be a second answer to a question the protocol
/// crate already answers (AGENTS.md section 55).
///
/// [`None`] for a sitting the catalogue would not attribute, and for a recording
/// that belongs to no sitting at all. Neither is a name to invent: what the
/// caller falls back to is the selector the user actually gave.
fn game_in(status: &RecorderStatus) -> Option<&str> {
    status
        .session()
        .and_then(|sitting| sitting.game_name.as_deref())
}

/// Which file of its sitting the running recording is, when it is not the first.
///
/// `SessionSummary::recordings` includes the one being written and is in the
/// order they were recorded, so the count *is* the position. [`None`] for a
/// sitting on its first file, and for a recording carrying no sitting: there is
/// nothing to say in either case, and "file 1 of this sitting" would be a
/// sentence that only ever adds words.
fn sitting_position(status: &RecorderStatus) -> Option<usize> {
    status
        .session()
        .map(|sitting| sitting.recordings.len())
        .filter(|recordings| *recordings > 1)
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
            status: status @ RecorderStatus::Recording(active),
            ..
        } => format!(
            // The game rather than the capture selector, for the reason
            // `describe_status` names it: somebody about to go looking for a
            // recording in Task Manager is better served by "Counter-Strike 2"
            // than by `process 4242`, and this window knows which it is.
            "It was last recording {} to {}, and that recording is still running.",
            game_in(status).unwrap_or(&active.target),
            active.output
        ),
        _ => "Clipped cannot tell whether it is still recording.".to_owned(),
    };

    format!(
        "Clipped has not exited, because {error}. {at_stake} Choose Exit again to close this \
         window anyway — the recorder is a separate process and will go on running until it is \
         stopped, which without Clipped means ending clipped-recorder.exe in Task Manager."
    )
}

/// The Save Replay item.
///
/// Live only while a recording with a **replay buffer** is running, which is
/// two conditions rather than one: `save_replay` is refused with
/// `not_recording` when nothing is being recorded, and refused again — with a
/// different sentence — when the recording that is running keeps no buffer to
/// save from ([issue #38](https://github.com/wildware-uk/clipped/issues/38),
/// `docs/ipc.md`). Offering a control whose command is about to be refused is
/// what AGENTS.md section 27 rules out, so each refusal is a label.
///
/// It said `needs a recording with a replay buffer (#38)` until that issue,
/// which built the buffer's driver and the command. When it is live, clicking
/// it now saves a clip — `tray::save_replay` — which it did not until
/// [issue #427](https://github.com/wildware-uk/clipped/issues/427).
///
/// A recording started **from this window** asks for a buffer since #427:
/// `crate::recording_request` asks for one without naming a length, and the
/// recorder answers with `replay_window_seconds` resolved for the game it is
/// recording — a duration somebody chose, which this process could not have
/// read for itself.
///
/// It does not follow that the item is always live against such a recording,
/// and the difference is the reason this reads the status rather than
/// remembering what it asked for. `replay_window_seconds` may be `0`, which
/// means keep no buffer
/// ([issue #539](https://github.com/wildware-uk/clipped/issues/539)) — a user
/// who has declined the buffer's continuous write gets a recording that keeps
/// none, `replay_seconds` absent from its status, and this item refused in
/// words. So the middle refusal covers two honest cases: a recording somebody
/// else started without a buffer — `clipped-recorder record`, or a client of
/// its own — and one that was offered a buffer and declined it. What it is no
/// longer is a stale claim about the build, which is the whole difference.
///
/// The capability is asked first, and is a different question again: a recorder
/// that never advertised `replay` has no `save_replay` command, so no recording
/// it makes can ever be savable. Asked before the two conditions above because
/// it outlives them — the others change with the next recording, and this one
/// does not change until the recorder is replaced.
fn replay_entry(link: &RecorderLinkState, recording: Option<&ActiveRecording>) -> MenuEntry {
    if attached_without(link, features::REPLAY) {
        return MenuEntry::refused("Save Replay — this recorder cannot save replays");
    }

    match recording {
        Some(active) if active.replay_seconds.is_some() => MenuEntry::live("Save Replay"),
        Some(_) => {
            MenuEntry::refused("Save Replay — this recording is not keeping a replay buffer")
        }
        None => MenuEntry::refused("Save Replay — nothing is being recorded"),
    }
}

/// The Add Bookmark item.
///
/// Live only while something is being recorded, because a bookmark is an offset
/// *into a recording* and there is nothing to put one in otherwise — the
/// recorder refuses `add_bookmark` with `not_recording` in exactly that case
/// (`docs/bookmarks.md`), and offering a control whose command is about to be
/// refused is what AGENTS.md section 27 rules out.
///
/// It was a `not in this build` refusal until
/// [issue #64](https://github.com/wildware-uk/clipped/issues/64), which is the
/// ticket that built the bookmark store and the command. A recorder from before
/// that issue is still a recorder this window can attach to, and it says so by
/// not advertising `bookmarks` — which is asked first, because "this recorder
/// has no bookmarks at all" and "there is nothing to bookmark right now" send a
/// user to two different places.
fn bookmark_entry(link: &RecorderLinkState, recording: bool) -> MenuEntry {
    if attached_without(link, features::BOOKMARKS) {
        return MenuEntry::refused("Add Bookmark — this recorder cannot mark a moment");
    }

    if recording {
        MenuEntry::live("Add Bookmark")
    } else {
        MenuEntry::refused("Add Bookmark — nothing is being recorded")
    }
}

/// Whether there is a recorder attached and it did **not** advertise `feature`.
///
/// The question `features::BOOKMARKS` and `features::SCREENSHOTS` are
/// documented as existing for: a UI asks it *before* drawing the control,
/// because a recorder built before the command existed refuses it with
/// [`ErrorCode::UnknownCommand`](clipped_ipc::ErrorCode::UnknownCommand) — a
/// refusal that arrives after the only part of the interaction that cost the
/// user anything ([issue #447](https://github.com/wildware-uk/clipped/issues/447)).
///
/// Deliberately not "does this recorder do X", which would also be false while
/// there is no recorder at all. The items asking this already have a better
/// sentence for that case — "nothing is being recorded" — and replacing it with
/// a claim about a recorder that is not there would be worse than the one it
/// replaced.
fn attached_without(link: &RecorderLinkState, feature: &str) -> bool {
    match link {
        RecorderLinkState::Attached { features, .. } => {
            !features.iter().any(|advertised| advertised == feature)
        }
        _ => false,
    }
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
    use clipped_ipc::{ActiveRecording, SessionRecording, SessionSummary, Watching};

    /// A recorder of this build: one that advertises everything this build's
    /// own `serve` does.
    ///
    /// `features::ALL` rather than a list typed here, so a capability added to
    /// the protocol does not quietly leave these cases testing an older
    /// recorder than the one they are about.
    fn attached(status: RecorderStatus) -> RecorderLinkState {
        attached_with(
            features::ALL
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            status,
        )
    }

    /// A recorder advertising exactly `features` and nothing else.
    fn attached_with(features: Vec<String>, status: RecorderStatus) -> RecorderLinkState {
        RecorderLinkState::Attached {
            recorder_process_id: 4_242,
            features,
            status,
        }
    }

    /// Everything this build's recorder can do, except one thing.
    fn everything_but(missing: &str) -> Vec<String> {
        features::ALL
            .iter()
            .filter(|name| **name != missing)
            .map(|name| (*name).to_owned())
            .collect()
    }

    fn recording() -> RecorderLinkState {
        attached(RecorderStatus::Recording(active(None)))
    }

    /// A recording that is keeping `seconds` of replay buffer.
    fn recording_with_replay(seconds: u32) -> RecorderLinkState {
        attached(RecorderStatus::Recording(active(Some(seconds))))
    }

    fn active(replay_seconds: Option<u32>) -> ActiveRecording {
        ActiveRecording {
            recording_id: "r-1".to_owned(),
            output: r"D:\clips\session.mkv".to_owned(),
            target: "process `cs2.exe`".to_owned(),
            elapsed_ms: 4_200,
            replay_seconds,
            session: None,
        }
    }

    /// A sitting the catalogue attributed, holding `files` recordings.
    ///
    /// The one being written is one of them, which is what makes the count the
    /// position of the file the recorder is on.
    fn sitting(files: usize) -> Box<SessionSummary> {
        Box::new(SessionSummary {
            session_id: "cs2-20260816-201400".to_owned(),
            game_id: Some("counter-strike-2".to_owned()),
            game_name: Some("Counter-Strike 2".to_owned()),
            started_at: "2026-08-16T20:14:00+01:00".to_owned(),
            recordings: (1..=files)
                .map(|index| SessionRecording {
                    session_index: u32::try_from(index).expect("a small count"),
                    output: format!(r"D:\clips\cs2-{index}.mkv"),
                    ..SessionRecording::default()
                })
                .collect(),
            ..SessionSummary::default()
        })
    }

    /// A sitting the catalogue would not attribute: it claimed nothing, or
    /// reported a tie, and the sitting is filed under no game rather than a
    /// guess.
    fn unattributed_sitting() -> Box<SessionSummary> {
        Box::new(SessionSummary {
            session_id: "unknown-20260816-201400".to_owned(),
            started_at: "2026-08-16T20:14:00+01:00".to_owned(),
            recordings: vec![SessionRecording::default()],
            ..SessionSummary::default()
        })
    }

    /// A recording that belongs to `session`.
    fn active_in(session: Option<Box<SessionSummary>>) -> ActiveRecording {
        ActiveRecording {
            session,
            ..active(None)
        }
    }

    fn game() -> ForegroundWindow {
        ForegroundWindow {
            process_id: 4_242,
            process_name: "cs2.exe".to_owned(),
        }
    }

    /// Every state the link can be in, for the tests that walk all of them.
    ///
    /// Two of them are attached *and recording* and differ only in what the
    /// recorder said it could do, because that is the difference the properties
    /// below have to hold across: an older recorder mid-recording is the case
    /// where a control looks most clearly clickable and is most clearly not.
    ///
    /// Both watching states are here as well, and a recording that carries the
    /// sitting it belongs to. A recorder that watches for games is not
    /// recording, so every property about a control that needs a recording has
    /// to hold across it — and until issue #588 no watching state was in this
    /// list at all, so nothing here had ever been asked.
    fn every_link_state() -> Vec<RecorderLinkState> {
        vec![
            RecorderLinkState::Connecting,
            attached(RecorderStatus::Idle),
            attached(RecorderStatus::Watching(Watching { session: None })),
            attached(RecorderStatus::Watching(Watching {
                session: Some(sitting(1)),
            })),
            recording(),
            attached(RecorderStatus::Recording(active_in(Some(sitting(2))))),
            attached_with(Vec::new(), RecorderStatus::Recording(active(Some(30)))),
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
        // does something. Each of the two that depend on a recording is enabled
        // exactly when its command would be performed rather than refused.
        for link in every_link_state() {
            for foreground in [None, Some(game())] {
                let model = tray_model(&link, foreground.as_ref());

                assert_eq!(
                    model.save_replay.enabled,
                    matches!(
                        &link,
                        RecorderLinkState::Attached {
                            status: RecorderStatus::Recording(active),
                            features,
                            ..
                        } if active.replay_seconds.is_some()
                            && features.iter().any(|name| name == features::REPLAY)
                    ),
                    "{link:?}: a replay comes out of a buffer, so the item may only be clickable \
                     while a recording is keeping one"
                );
                assert_eq!(
                    model.add_bookmark.enabled,
                    matches!(
                        &link,
                        RecorderLinkState::Attached {
                            status: RecorderStatus::Recording(_),
                            features,
                            ..
                        } if features.iter().any(|name| name == features::BOOKMARKS)
                    ),
                    "{link:?}: a bookmark is an offset into a recording, so the item may only be \
                     clickable while there is one"
                );
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
    fn save_replay_stopped_claiming_the_feature_is_unbuilt_and_became_a_control() {
        // Issue #38 built the `replay` subcommand and the `save_replay`
        // command, so the menu must stop saying "needs a recording with a
        // replay buffer (#38)". A ticket closed while the product still says
        // the feature is unbuilt is the failure AGENTS.md sections 27 and 54
        // name, and it is invisible from the recorder's side — the refusal
        // simply stops being sent while the menu goes on quoting it.
        let idle = tray_model(&attached(RecorderStatus::Idle), Some(&game()));
        assert!(!idle.save_replay.label.contains("#38"), "{idle:?}");
        assert_eq!(
            idle.save_replay.label,
            "Save Replay — nothing is being recorded"
        );

        // A recording with no buffer — `clipped-recorder record`, or a client
        // of its own — is a different sentence again: something *is* being
        // recorded, and there is still nothing to save (#427).
        let plain = tray_model(&recording(), Some(&game()));
        assert_eq!(
            plain.save_replay.label,
            "Save Replay — this recording is not keeping a replay buffer"
        );
        assert!(!plain.save_replay.enabled);

        // And one that is keeping a buffer is a control.
        let buffered = tray_model(&recording_with_replay(300), Some(&game()));
        assert_eq!(buffered.save_replay.label, "Save Replay");
        assert!(buffered.save_replay.enabled);
    }

    #[test]
    fn add_bookmark_stopped_claiming_the_feature_is_unbuilt_and_became_a_control() {
        // Issue #64 built the bookmark store and the `add_bookmark` command, so
        // the menu must stop saying "needs bookmarks (#64)". A ticket closed
        // while the product still says the feature is unbuilt is the failure
        // AGENTS.md sections 27 and 54 name, and it is invisible from the
        // recorder's side — the refusal simply stops being sent while the menu
        // goes on quoting it.
        let idle = tray_model(&attached(RecorderStatus::Idle), Some(&game()));
        assert!(!idle.add_bookmark.label.contains("#64"), "{idle:?}");
        assert_eq!(
            idle.add_bookmark.label,
            "Add Bookmark — nothing is being recorded"
        );
        assert!(!idle.add_bookmark.enabled);

        let recording = tray_model(&recording(), Some(&game()));
        assert_eq!(recording.add_bookmark.label, "Add Bookmark");
        assert!(recording.add_bookmark.enabled);
    }

    #[test]
    fn a_recorder_that_never_said_it_could_bookmark_is_not_offered_a_bookmark_control() {
        // The version-skew case, and the one the protocol's own documentation
        // says `features::BOOKMARKS` exists for. An installed Clipped can find
        // a recorder from before issue #64 still running, and everything about
        // it looks right: attached, recording, a game in front. Drawing the
        // control on the strength of that means the user presses it and *then*
        // gets `unknown_command`, having marked a moment that was never marked.
        let older = attached_with(
            everything_but(features::BOOKMARKS),
            RecorderStatus::Recording(active(None)),
        );

        let model = tray_model(&older, Some(&game()));

        assert!(!model.add_bookmark.enabled);
        assert_eq!(
            model.add_bookmark.label, "Add Bookmark — this recorder cannot mark a moment",
            "the reason has to name the recorder rather than the recording: it is what a restart \
             fixes, and 'nothing is being recorded' would be a lie about a recording that is \
             running"
        );
    }

    #[test]
    fn a_recorder_that_never_said_it_could_save_replays_is_not_offered_the_control() {
        // The same skew, on the item where it is easiest to hide: a recording
        // *with* a buffer, on a recorder with no `save_replay` command. Both
        // conditions the label already knew about are satisfied, so without the
        // capability check this is a live, clickable Save Replay that refuses.
        let older = attached_with(
            everything_but(features::REPLAY),
            RecorderStatus::Recording(active(Some(300))),
        );

        let model = tray_model(&older, Some(&game()));

        assert!(!model.save_replay.enabled);
        assert_eq!(
            model.save_replay.label,
            "Save Replay — this recorder cannot save replays"
        );
    }

    #[test]
    fn a_recorder_that_is_not_there_is_not_described_as_one_that_lacks_a_capability() {
        // The ordering the two items above depend on. "This recorder cannot
        // mark a moment" said while there is no recorder at all is a claim
        // about something that does not exist, and it would replace two
        // sentences that are true and useful.
        for link in [
            RecorderLinkState::Connecting,
            RecorderLinkState::Unavailable {
                reason: "the recorder was not found".to_owned(),
            },
        ] {
            let model = tray_model(&link, Some(&game()));

            assert_eq!(
                model.add_bookmark.label, "Add Bookmark — nothing is being recorded",
                "{link:?}"
            );
            assert_eq!(
                model.save_replay.label, "Save Replay — nothing is being recorded",
                "{link:?}"
            );
        }
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
    fn a_recording_that_knows_its_game_is_named_by_it_rather_than_by_a_process_id() {
        // Issue #588's first half. `ActiveRecording::session` has carried the
        // game since issue #241 (PR #586) and this arm went on formatting
        // `target`, which is a capture selector — so the line went from
        // "Watching — Counter-Strike 2" while the recorder waited to
        // "Recording process 4242" the moment it started recording that very
        // game. Backwards, and withholding something the window was holding
        // (AGENTS.md section 27).
        let model = tray_model(
            &attached(RecorderStatus::Recording(active_in(Some(sitting(1))))),
            Some(&game()),
        );

        assert_eq!(model.mark, TrayMark::Recording);
        assert_eq!(model.status.label, "Recording Counter-Strike 2");
        assert_eq!(model.tooltip, "Clipped — recording Counter-Strike 2");
        assert!(
            !model.status.label.contains("cs2.exe"),
            "the selector is what the window could not turn into a game name; having been given \
             the name, it must not go back to it: {}",
            model.status.label,
        );
    }

    #[test]
    fn the_second_file_of_a_sitting_says_which_file_of_the_sitting_it_is() {
        // The sentence issue #241 exists to make sayable, and the reason the
        // sitting carries `recordings` at all: a game restarted inside its
        // grace period keeps one sitting, and the file that follows would
        // otherwise look like an unrelated recording of the same game.
        let second = tray_model(
            &attached(RecorderStatus::Recording(active_in(Some(sitting(2))))),
            None,
        );
        assert_eq!(
            second.status.label,
            "Recording Counter-Strike 2, file 2 of this sitting"
        );

        // And a sitting on its first file says nothing about it, because "file
        // 1 of this sitting" only ever adds words.
        let first = tray_model(
            &attached(RecorderStatus::Recording(active_in(Some(sitting(1))))),
            None,
        );
        assert_eq!(first.status.label, "Recording Counter-Strike 2");
    }

    #[test]
    fn a_recording_with_no_game_behind_it_still_says_what_it_is_recording() {
        // The fallback, and the half that stops the change above being a screen
        // that goes blank. Two recordings have no game name to show: one
        // carrying no sitting at all, and one whose sitting the catalogue would
        // not attribute — it claimed nothing, or reported a tie, and inventing
        // a name for it is precisely what `game_name` is optional to prevent.
        // Both show the selector the user actually gave, which is what this
        // line said before issue #588.
        let no_sitting = tray_model(&recording(), None);
        assert_eq!(no_sitting.status.label, "Recording process `cs2.exe`");
        assert_eq!(no_sitting.tooltip, "Clipped — recording process `cs2.exe`");

        let unattributed = tray_model(
            &attached(RecorderStatus::Recording(active_in(Some(
                unattributed_sitting(),
            )))),
            None,
        );
        assert_eq!(unattributed.status.label, "Recording process `cs2.exe`");
    }

    #[test]
    fn a_watching_recorder_names_the_game_whose_sitting_is_still_open() {
        // The arm this one was already right about, asserted here because
        // nothing had ever asked it: the whole point of issue #588 is that the
        // two lines stopped agreeing, and a test of one of them is not a test
        // of that.
        let waiting = tray_model(
            &attached(RecorderStatus::Watching(Watching {
                session: Some(sitting(1)),
            })),
            Some(&game()),
        );
        assert_eq!(waiting.status.label, "Watching — Counter-Strike 2");
        assert_eq!(waiting.mark, TrayMark::Idle, "nothing is being recorded");

        let anything = tray_model(
            &attached(RecorderStatus::Watching(Watching { session: None })),
            Some(&game()),
        );
        assert_eq!(anything.status.label, "Watching for a game");
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

        // And it names the game when the recording knows one, for the reason
        // the status line does: somebody who is about to be sent to Task
        // Manager is better served by the name of the game than by a selector.
        let named = could_not_reach_the_recorder(
            &attached(RecorderStatus::Recording(active_in(Some(sitting(1))))),
            "the pipe was busy",
        );
        assert!(named.contains("Counter-Strike 2"), "{named}");
        assert!(named.contains(r"D:\clips\session.mkv"), "{named}");
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
