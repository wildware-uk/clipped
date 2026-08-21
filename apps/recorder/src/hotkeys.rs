//! The global hotkeys, registered by this process, and what a press turns into.
//!
//! # Why the recorder and not the window
//!
//! `RegisterHotKey` gives a combination to exactly one process, so somebody had
//! to choose which one. It is this one, and
//! [ADR 0009](../../../docs/adr/0009-the-recorder-registers-global-hotkeys.md)
//! is the argument: the recorder is what starts at login and outlives every
//! window (ADR 0002), it is what can act on a press without a round trip, and
//! its exclusivity is already decided by the endpoint — `serve` binds the named
//! pipe before this module registers anything, so a second recorder has already
//! exited by the time it could have taken a combination away from the first.
//!
//! The desktop application registers nothing. It reads this through
//! `get_hotkeys` and changes a binding through `apply_settings`, which saves
//! the combination and then rebinds the running service
//! ([`RegisteredHotkeys::apply`], issue #233) — so a combination changed from
//! the window takes effect on the next press rather than the next start. What
//! [issue #54](https://github.com/wildware-uk/clipped/issues/54) still adds is
//! a control that *captures* a combination instead of one that is typed.
//!
//! # What a press is
//!
//! **The same command the window would have sent.** A press is turned into a
//! [`Command`] and handed to the same [`CommandHandler`] the protocol dispatches
//! to, so there is one implementation of "add a bookmark" and a press cannot
//! drift from the button (AGENTS.md section 55).
//!
//! That is why what this module holds is a [`CommandHandler`] and not a
//! `RecorderService`: the trait *is* the seam being described, so naming it
//! keeps the claim honest rather than aspirational — nothing here can reach past
//! the protocol into the recording state, because it has no way to.
//!
//! The one difference is where the answer goes. A command that arrived over IPC
//! is answered to the client that asked; a press has no client, so its outcome
//! goes to the log — as an `info` for what happened and a `warn` for what did
//! not, never nothing (AGENTS.md sections 15 and 54).
//!
//! ```text
//!   the window            a key press
//!       │                      │
//!   start_recording        WM_HOTKEY ──▶ handler thread
//!       │                      │              │
//!       └──────▶ RecorderService::call ◀──────┘
//! ```
//!
//! # What a press records
//!
//! The window it was pressed in front of. A key press carries no target, which
//! is why "Start or stop recording" could only ever *stop* one
//! ([issue #416](https://github.com/wildware-uk/clipped/issues/416)); the
//! recorder now answers the same question the Record button answers, from
//! `clipped_windows::foreground_target` and at the moment of the press. It
//! sends the same `start_recording` the window sends — see
//! [`start_what_is_in_front`] — and refuses, naming what was in front instead,
//! when what is there is Clipped's own window, part of the shell, or nothing.
//!
//! # Threads
//!
//! `clipped_hotkeys` gives each handled action a thread of its own and never
//! runs a handler on the thread that received the press (`docs/hotkeys.md`). So
//! a handler here may take as long as stopping a recording takes — which is a
//! flush, an encoder drain and a container trailer — without delaying the next
//! press or any other action. Nothing here runs on a capture thread, and nothing
//! here is called from one.

use std::sync::{Arc, Mutex};

use clipped_hotkeys::{
    ActionStatus, BindingState, Handlers, Hotkey, HotkeyAction, HotkeyService, Registration,
    Unhandled,
};
use clipped_ipc::{
    AddBookmark, Command, CommandHandler, ErrorCode, HotkeyBinding, HotkeyState, ProtocolError,
    RecorderStatus, Reply, SaveReplay, StartRecording, StopRecording, TakeScreenshot,
};
use clipped_session::config::Configuration;
use clipped_windows::{ForegroundTarget, WindowsError};

/// How this process finds out what the user was looking at when they pressed a
/// key.
///
/// A function rather than a call, for one reason: what has the foreground on a
/// machine running the test suite is not something a test may decide, and the
/// question this ticket is about — which command a press produces when a game
/// is in front — has no answer at all without being able to say what is in
/// front. Production passes [`what_is_in_front`], which is the real one and the
/// only one in the shipped binary.
pub(crate) type Foreground = Arc<dyn Fn() -> Result<ForegroundTarget, WindowsError> + Send + Sync>;

/// Asking Windows, at the moment of the press.
///
/// See `clipped_windows::foreground`: a hotkey press raises no window, so
/// unlike the desktop application's tray menu the recorder can ask when it is
/// asked rather than following the foreground with a hook it would have to run
/// for the life of the process.
pub(crate) fn what_is_in_front() -> Foreground {
    Arc::new(clipped_windows::foreground_target)
}

/// The hotkey service this process is running, and what it registered.
///
/// Dropping the last handle gives every combination back to Windows and waits
/// for a handler that is still running, so a recorder that ends — by request,
/// by Ctrl+C or by a panic unwinding out of `serve` — never leaves a
/// combination registered that nothing is listening for (AGENTS.md section 58).
///
/// # Why this is shared rather than owned by `serve`
///
/// Because a binding has to be changeable from a connection thread. `serve`
/// holds one handle so that it can stop the service in the right order at
/// shutdown, and `RecorderService` holds another so that `apply_settings` can
/// call [`Self::apply`] on the thread the window's request arrived on
/// ([issue #233](https://github.com/wildware-uk/clipped/issues/233)).
///
/// # Threads
///
/// `RegisterHotKey` and `UnregisterHotKey` are bound to the thread that called
/// them, which is the message loop `clipped_hotkeys` runs and not any thread
/// here. Nothing in this module reaches that thread itself:
/// [`HotkeyService::rebind`] posts the request to it and waits for the answer,
/// which is why a connection thread may ask for a rebind at all (AGENTS.md
/// section 20, `crates/hotkeys/src/service/windows.rs`).
///
/// The wait is bounded by what the loop thread does with one message, which is
/// two Win32 calls; it holds this lock while it waits, so a `get_hotkeys`
/// arriving mid-rebind is answered after it rather than during it. That is the
/// point rather than a cost — the answer is read *out of* the live
/// registration, so a reader can never be shown a combination Windows has not
/// been asked for.
#[derive(Debug, Clone)]
pub struct RegisteredHotkeys {
    state: Arc<Mutex<Registered>>,
}

/// What this process is holding, or the sentence saying why it is holding
/// nothing.
#[derive(Debug)]
enum Registered {
    /// Running, and this is what Windows gave it.
    Running(HotkeyService),
    /// Nothing is registered. The string is the sentence `get_hotkeys` answers
    /// with — a refusal rather than an empty list, because an empty list reads
    /// as "nothing conflicted" (AGENTS.md section 27).
    Nothing(String),
}

impl RegisteredHotkeys {
    /// Gives every combination back and waits for the handler that is running.
    ///
    /// Called before the recorder stops the recording it is making, so that a
    /// press cannot arrive while the process is winding up and ask for a
    /// recording that is halfway through being finished.
    ///
    /// Takes `&self` rather than consuming, because `serve` is not the only
    /// holder any more: the service holds a handle too, and stopping is a thing
    /// one of them does rather than the last one to let go.
    pub fn stop(&self) {
        let stopped = std::mem::replace(
            &mut *self.locked(),
            Registered::Nothing("this recorder is shutting down".to_owned()),
        );
        if let Registered::Running(service) = stopped {
            service.stop();
        }
    }

    /// Where every global hotkey stands, read out of the live registration.
    ///
    /// Not a stored copy. Before [issue
    /// #233](https://github.com/wildware-uk/clipped/issues/233) the report was
    /// published once and kept, which was safe only because nothing could
    /// change what was registered; now that something can, a second copy would
    /// be a second thing to keep in step. Asking the service each time makes
    /// that impossible rather than merely unlikely.
    ///
    /// # Errors
    ///
    /// The sentence to show when nothing is registered at all, which is not the
    /// same as every combination having been refused: a refusal is a
    /// [`HotkeyState::Conflict`] in an otherwise ordinary report.
    pub fn report(&self) -> Result<Vec<HotkeyBinding>, String> {
        match &*self.locked() {
            Registered::Running(service) => Ok(report_of(service.registration())),
            Registered::Nothing(reason) => Err(reason.clone()),
        }
    }

    /// Points every action at what the settings now say, without a restart.
    ///
    /// Called by `apply_settings` after the save, for the reason the storage
    /// limits are pushed to the indexer there: a combination saved from the
    /// window and not carried here is a control whose effect waits for a
    /// restart, with nothing on screen saying so (AGENTS.md section 27).
    ///
    /// **Only what changed is touched.** An action already bound to what the
    /// settings ask for is left alone rather than unregistered and registered
    /// again, so saving the resolution does not briefly drop `Ctrl`+`F10`.
    ///
    /// A combination Windows refuses is reported and *kept as it was*
    /// ([`HotkeyService::rebind`] registers the new one before releasing the
    /// old), so a save that names an impossible combination costs the user the
    /// change and not the binding they had. The refusal reaches them through
    /// the report: the next `get_hotkeys` shows the action still on its old
    /// combination.
    pub fn apply(&self, configuration: &Configuration) {
        let wanted = match configuration.resolve_hotkeys() {
            Ok(resolved) => resolved,
            Err(error) => {
                // The saved file points one combination at two actions.
                // `apply_settings` refuses such a change, so this is a file
                // edited by hand underneath a running recorder; changing part
                // of the set would leave a keyboard whose behaviour depends on
                // which action was reached first, exactly as `start` refuses
                // to.
                tracing::error!(
                    %error,
                    "no hotkey was rebound, because the settings file now points one combination \
                     at two actions"
                );
                return;
            }
        };

        let mut state = self.locked();
        let Registered::Running(service) = &mut *state else {
            return;
        };

        let held: Vec<(HotkeyAction, Option<Hotkey>)> = service
            .registration()
            .statuses()
            .iter()
            .map(|status| (status.action(), status.binding()))
            .collect();
        let changed: Vec<(HotkeyAction, Option<Hotkey>)> = held
            .iter()
            .filter(|&&(action, bound)| bound != wanted.binding(action).get())
            .map(|&(action, _)| (action, wanted.binding(action).get()))
            .collect();

        // Two passes, and the first one exists for one case: swapping two
        // actions' combinations. `HotkeyService::rebind` refuses a combination
        // another Clipped action still holds — the same rule `Bindings::bind`
        // enforces — so the action being moved *off* a combination has to let
        // go before the action moving onto it can ask for it. Only an action
        // that is itself changing is released, and only when something else
        // wants what it holds, so the ordinary case of one changed binding
        // releases nothing early and keeps the refusal guarantee above.
        //
        // The holder is always one of the changing actions: `wanted` resolved
        // without a conflict, so nothing that keeps its combination can be
        // holding one another action is moving onto. The check is what makes
        // that an assumption this code does not act on rather than one it
        // relies on.
        for &(action, hotkey) in &changed {
            let Some(hotkey) = hotkey else { continue };
            let holder = held.iter().find_map(|&(other, bound)| {
                (other != action && bound == Some(hotkey)).then_some(other)
            });
            if let Some(holder) = holder {
                if changed.iter().any(|&(moving, _)| moving == holder) {
                    Self::rebind(service, holder, None);
                }
            }
        }

        for &(action, hotkey) in &changed {
            Self::rebind(service, action, hotkey);
        }
    }

    /// One rebind, with its outcome in the log either way (AGENTS.md section
    /// 15).
    fn rebind(service: &mut HotkeyService, action: HotkeyAction, hotkey: Option<Hotkey>) {
        match service.rebind(action, hotkey) {
            Ok(()) => tracing::info!(
                action = action.name(),
                hotkey = hotkey.map_or_else(|| "none".to_owned(), |hotkey| hotkey.to_string()),
                "a hotkey was rebound from the settings without restarting the recorder"
            ),
            Err(error) => tracing::warn!(
                action = action.name(),
                hotkey = hotkey.map_or_else(|| "none".to_owned(), |hotkey| hotkey.to_string()),
                "a hotkey could not be rebound, so it is still on the combination it had: {error}"
            ),
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Registered> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn of(state: Registered) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

/// Registers the user's hotkeys and starts delivering presses to `recorder`.
///
/// Returns the running service, which [`RegisteredHotkeys::report`] answers
/// `get_hotkeys` out of. An answer is available on **every** path, including
/// the ones where nothing was registered, because "the recorder could not
/// register its hotkeys" and "every hotkey registered cleanly" are opposite
/// answers and an empty list would be drawn as the second (AGENTS.md section
/// 27).
///
/// # Errors
///
/// Never as a `Result`: a failure is the [`Err`] half of the report, which is
/// the sentence the window shows. There is nothing here worth failing `serve`
/// over — a recorder with no hotkeys still records, and one that refused to
/// start because Windows would not give it `Ctrl`+`F10` would be a far worse
/// thing to ship.
pub fn start(
    recorder: &Arc<dyn CommandHandler>,
    configuration: &Configuration,
) -> RegisteredHotkeys {
    let bindings = match configuration.resolve_hotkeys() {
        Ok(resolved) => resolved.bindings().clone(),
        Err(error) => {
            // Only reachable from a settings file somebody edited by hand:
            // `HotkeyOverrides::set` refuses to produce a set that resolves to
            // two actions on one combination. Registering the ones that do not
            // collide would leave the user with a keyboard whose behaviour
            // depends on which action `BTreeMap` happened to reach first, so
            // nothing is registered and the file is named as the thing to fix.
            tracing::error!(
                %error,
                "no hotkey was registered, because the settings file points one combination at \
                 two actions"
            );
            return RegisteredHotkeys::of(Registered::Nothing(format!(
                "No hotkey is registered: {error} Fix the `hotkeys` section of the settings file \
                 and restart Clipped."
            )));
        }
    };

    let handlers = handlers_for(recorder, &what_is_in_front());

    match HotkeyService::start(&bindings, handlers) {
        Ok((service, events)) => {
            // The events channel is the *other* way to learn what a press did,
            // and this process does not use it: every outcome is already logged
            // by `perform` on the handler's own thread, with the action and the
            // recorder's own sentence. Dropping the receiver costs the events
            // and nothing else, which `HotkeyService::start` documents.
            drop(events);

            tracing::info!(
                registered = service.registration().bound().count(),
                conflicts = service.registration().conflicts().count(),
                "the global hotkeys were registered"
            );

            RegisteredHotkeys::of(Registered::Running(service))
        }
        Err(error) => {
            tracing::error!(%error, "no global hotkey was registered");
            RegisteredHotkeys::of(Registered::Nothing(format!(
                "No hotkey is registered: {error}."
            )))
        }
    }
}

/// A handler for every action this build can perform, and none for the rest.
///
/// The absence is the point. An action with no handler is reported as
/// [`Unhandled`] when it is pressed and shown as unavailable before it is, and
/// both carry the milestone and issue that would build it. A handler that
/// swallowed the press to make the key look alive is what AGENTS.md section 54
/// forbids.
///
/// `pub(crate)` for one caller and it is a test: `crate::watch` presses the
/// bookmark key against a real recording detection started, which is issue
/// #421's acceptance criterion and cannot be asserted from here — this module
/// has no way to make a recording. Pressing through the handlers this registers,
/// rather than calling [`perform`], is what makes it the real path.
pub(crate) fn handlers_for(
    recorder: &Arc<dyn CommandHandler>,
    foreground: &Foreground,
) -> Handlers {
    let mut handlers = Handlers::new();
    for action in [
        // `Ctrl`+`F10` is the reason this list exists at all (SPEC.md section
        // 7). It was the one action here with nothing behind it until issue
        // #38 built the buffer, the save and the command; leaving it out now
        // would be the recorder going on refusing a key it can perform.
        HotkeyAction::SaveReplay,
        HotkeyAction::AddBookmark,
        HotkeyAction::TakeScreenshot,
        HotkeyAction::ToggleRecording,
    ] {
        let recorder = Arc::clone(recorder);
        let foreground = Arc::clone(foreground);
        handlers = handlers.on(action, move |press| {
            perform(&recorder, &foreground, press.action(), press.hotkey());
        });
    }
    handlers
}

/// Runs one press, on that action's own handler thread.
///
/// Reports what happened either way. A press the recorder could not act on is
/// the whole failure mode of a hotkey — the key does nothing and nothing says
/// why — so the refusal is logged at `warn` with the recorder's own sentence,
/// which is the same one the window would have been shown had it asked
/// (AGENTS.md section 15).
fn perform(
    recorder: &Arc<dyn CommandHandler>,
    foreground: &Foreground,
    action: HotkeyAction,
    hotkey: Hotkey,
) {
    match command_for(recorder.as_ref(), foreground, action) {
        Ok(command) => {
            let name = command.name();
            match recorder.as_ref().call(command) {
                Ok(reply) => tracing::info!(
                    action = action.name(),
                    hotkey = %hotkey,
                    command = name,
                    outcome = %described(&reply),
                    "a hotkey was pressed and the recorder acted on it"
                ),
                Err(refusal) => tracing::warn!(
                    action = action.name(),
                    hotkey = %hotkey,
                    command = name,
                    code = refusal.code.as_str(),
                    "a hotkey was pressed and the recorder refused it: {}",
                    refusal.message,
                ),
            }
        }
        Err(refusal) => tracing::warn!(
            action = action.name(),
            hotkey = %hotkey,
            code = refusal.code.as_str(),
            "a hotkey was pressed and there was nothing to send: {}",
            refusal.message,
        ),
    }
}

/// The command one action asks the recorder for.
///
/// # Errors
///
/// The refusal for a press that cannot become a command at all: today, a toggle
/// pressed with nothing in front worth recording, which names what it found
/// instead ([issue #416](https://github.com/wildware-uk/clipped/issues/416)).
fn command_for(
    recorder: &dyn CommandHandler,
    foreground: &Foreground,
    action: HotkeyAction,
) -> Result<Command, ProtocolError> {
    match action {
        // Nothing named at all: keep the duration the recording's buffer was
        // started with, out of whatever is being recorded, and put the clip
        // where that recording's clips go. Somebody pressing the key mid-fight
        // has said everything they are going to say (`clipped_ipc::SaveReplay`).
        // A recording that keeps no buffer is refused in the recorder's own
        // words, which is more use than a guess at a duration.
        HotkeyAction::SaveReplay => Ok(Command::SaveReplay(SaveReplay::default())),
        // No recording named, which means "whatever is running" — the same
        // thing the tray's menu sends, and the only thing a key press can mean.
        HotkeyAction::AddBookmark => Ok(Command::AddBookmark(AddBookmark::default())),
        // No target either: with a recording running the picture comes from a
        // frame it already captured, and with nothing running the recorder
        // refuses in its own words, which is more use than a guess at a window.
        HotkeyAction::TakeScreenshot => Ok(Command::TakeScreenshot(TakeScreenshot::default())),
        HotkeyAction::ToggleRecording => match recorder.status() {
            RecorderStatus::Recording(_) => Ok(Command::StopRecording(StopRecording::default())),
            // What the user was looking at when the key went down, which is the
            // same thing the window's Record button offers and the same command
            // it sends (issue #416).
            RecorderStatus::Idle => start_what_is_in_front(foreground),
            // The same thing, and this is where issue #421 left it: a watching
            // recorder was told to refuse, because the only sentence available
            // was "start it from the window" and a recorder that is about to
            // start one itself should not be saying that. Whether the key
            // should *also* start one early was left open, and it is answered
            // here because #416 is what makes it answerable — the recorder can
            // now say which window, so a press has something to record.
            //
            // Starting is the answer, for the reason the whole ticket exists:
            // watching is not recording, and a key that refuses every press
            // while nothing is being recorded is a key that does nothing
            // (AGENTS.md section 27). The game a user reaches for the keyboard
            // over is the one the catalogue did not recognise. Nothing is at
            // risk either way — there is no footage to lose while nothing is
            // being recorded (AGENTS.md section 56) — and if a game launches
            // into a recording this press started, the watcher is refused with
            // the recorder's own "one at a time" sentence, which is what it
            // already gets when the window starts one.
            RecorderStatus::Watching(_) => start_what_is_in_front(foreground),
        },
        // Not reachable: `handlers_for` registers the three above and nothing
        // else, so a press of any other action never reaches a handler and is
        // reported as `Unhandled` instead. Kept as a refusal rather than an
        // `unreachable!` because the way this becomes wrong is somebody adding
        // an action to that list and not to this match, and a panic on a
        // handler thread would take that action's hotkey out for the rest of
        // the session.
        other => Err(ProtocolError::new(
            ErrorCode::Internal,
            format!(
                "{} was given a hotkey handler and no command to send, which is a defect in the \
                 recorder",
                other.label()
            ),
        )),
    }
}

/// `start_recording` for whatever the user was looking at, or the refusal that
/// says what was in front instead.
///
/// # Why this is the same thing the window sends
///
/// Byte for byte the request `apps/desktop/src-tauri/src/main.rs`'s
/// `recording_request` builds: the process identifier and a replay buffer, and
/// nothing else. That is the third acceptance criterion of
/// [issue #416](https://github.com/wildware-uk/clipped/issues/416) — the Record
/// button and the hotkey have to agree about what the target is — and it is
/// worth spelling out that "agree" is doing two jobs here:
///
/// - **The same field.** A process identifier, not a window handle and not an
///   executable name, so the recorder resolves the window through the one
///   `resolve_window` both paths already go through (AGENTS.md section 55).
/// - **The same recording.** `replay: true` is what the window asks for
///   ([issue #427](https://github.com/wildware-uk/clipped/issues/427)), so a
///   recording started with the key keeps a buffer and `Ctrl`+`F10` works
///   against it. Without it, the first thing a user did after starting a
///   recording from the keyboard would be refused — one hotkey quietly
///   disabling another (AGENTS.md section 27).
///
/// # Errors
///
/// The refusal for a press with nothing sensible in front. It names what *was*
/// in front rather than only that something was missing, because "the key does
/// nothing" is the failure this whole module exists to prevent, and a log line
/// saying "no target" is one nobody can act on (AGENTS.md section 15).
fn start_what_is_in_front(foreground: &Foreground) -> Result<Command, ProtocolError> {
    match foreground() {
        Ok(ForegroundTarget::Recordable(window)) => Ok(Command::StartRecording(StartRecording {
            pid: Some(window.process_id()),
            replay: true,
            ..StartRecording::default()
        })),
        Ok(ForegroundTarget::NothingToRecord(reason)) => Err(ProtocolError::new(
            // The two codes `crate::serve::unrecordable_target` already uses
            // for the same distinction — nothing to record, against one thing
            // that cannot be recorded as it is — so a client branching on the
            // code reads a refused press the way it reads a refused
            // `start_recording`.
            match reason {
                clipped_windows::NotRecordable::NotCapturable { .. } => {
                    ErrorCode::TargetNotCapturable
                }
                _ => ErrorCode::TargetNotFound,
            },
            format!(
                "nothing was recorded, because {reason}. Bring what you want recorded to the \
                 front and press the key again"
            ),
        )),
        // Windows refusing to describe the window it has just named as the
        // foreground one. Reported rather than guessed past: the alternative is
        // recording something the user did not choose.
        Err(error) => Err(ProtocolError::new(
            ErrorCode::TargetNotFound,
            format!(
                "nothing was recorded, because Windows would not say what is in front: {error}"
            ),
        )),
    }
}

/// What a reply says happened, in the few words a log line wants.
///
/// Deliberately not the whole reply: a `bookmark_added` carries the file it was
/// written to, and a recorder's log must not accumulate the paths of everything
/// a user records (AGENTS.md section 13, `docs/logging.md`).
fn described(reply: &Reply) -> &'static str {
    match reply {
        Reply::ReplaySaved { .. } => "a replay clip was saved",
        Reply::BookmarkAdded { .. } => "the moment was marked",
        Reply::ScreenshotTaken { .. } => "a screenshot was written",
        Reply::RecordingStarted { .. } => "a recording of what was in front was started",
        Reply::RecordingStopped { .. } => "the recording was stopped and its file finished",
        _ => "done",
    }
}

/// Every action, as the protocol reports it.
///
/// The whole of [`clipped_hotkeys::ACTIONS`], including the ones bound to
/// nothing: a screen that was sent only the bound ones could not offer the rest,
/// and an action missing from the list is indistinguishable from one this
/// recorder has never heard of.
fn report_of(registration: &Registration) -> Vec<HotkeyBinding> {
    registration.statuses().iter().map(row_for).collect()
}

/// One action's row.
fn row_for(status: &ActionStatus) -> HotkeyBinding {
    let action = status.action();

    HotkeyBinding {
        action: action.name().to_owned(),
        label: action.label().to_owned(),
        hotkey: status.binding().map(|hotkey| hotkey.to_string()),
        state: match status.state() {
            BindingState::Unbound => HotkeyState::Unbound,
            BindingState::Bound => HotkeyState::Registered,
            // The conflict's own sentence, carried through verbatim: it names
            // the combination, the action that wanted it, who is likely to have
            // it and what to do next, and only the process that asked Windows
            // knows any of that (AGENTS.md section 45).
            BindingState::Conflict(conflict) => HotkeyState::Conflict {
                reason: conflict.to_string(),
            },
        },
        handled: status.is_handled(),
        // The same sentence a press would produce, from the same type, so the
        // reason a key does nothing cannot differ depending on whether the user
        // pressed it or read about it.
        unavailable: (!status.is_handled()).then(|| Unhandled::for_action(action).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::test_support::Scratch;

    use clipped_hotkeys::{Bindings, Hotkey, HotkeyAction, HotkeyService, Registration, ACTIONS};
    use clipped_ipc::{
        ActiveRecording, Command, CommandHandler, ErrorCode, HotkeyState, ProtocolError,
        RecorderStatus, Reply, SaveReplay, StartRecording,
    };
    use clipped_session::config::{Configuration, HotkeyOverride, HotkeyOverrides};

    use clipped_windows::{
        ForegroundTarget, NotRecordable, PixelSize, WindowGeometry, WindowInfo, WindowsError,
        DEFAULT_DPI,
    };

    use super::{command_for, handlers_for, perform, report_of, row_for, start, Foreground};

    /// The process the game in these tests is running as.
    const A_GAME: u32 = 4_242;

    /// A foreground that answers with the window in front of a written-down
    /// desktop, rather than with whatever is in front of whoever is running the
    /// suite (AGENTS.md section 25).
    fn in_front_of_the_user(target: ForegroundTarget) -> Foreground {
        Arc::new(move || Ok(target.clone()))
    }

    /// A game, in front, that this recorder can record.
    fn a_game_in_front() -> Foreground {
        in_front_of_the_user(ForegroundTarget::Recordable(Box::new(WindowInfo::new(
            clipped_windows::WindowHandle::from_raw(0x1234),
            "Counter-Strike 2".to_owned(),
            A_GAME,
            Some("cs2.exe".to_owned()),
            WindowGeometry::new(
                PixelSize::new(2560, 1440),
                DEFAULT_DPI,
                clipped_windows::MonitorHandle::from_raw(1),
            ),
            false,
            None,
        ))))
    }

    /// Nothing in front worth recording.
    fn nothing_in_front(reason: NotRecordable) -> Foreground {
        in_front_of_the_user(ForegroundTarget::NothingToRecord(reason))
    }

    /// A recorder that records what it was asked, and answers.
    ///
    /// The seam this module is built on. A test that wanted a real
    /// `RecorderService` to see one of these commands would need a window to
    /// record and a library to write into; what it actually needs to know is
    /// which command a press produced, which is what this keeps.
    #[derive(Debug)]
    struct AskedRecorder {
        asked: Mutex<Vec<Command>>,
        status: RecorderStatus,
    }

    impl AskedRecorder {
        fn idle() -> Self {
            Self {
                asked: Mutex::new(Vec::new()),
                status: RecorderStatus::Idle,
            }
        }

        /// Watching for games, with none running.
        ///
        /// A state the protocol has a word for and no recorder reports yet:
        /// `serve --watch-for-games` still answers `Idle` while it waits, which
        /// is the half of [issue
        /// #241](https://github.com/wildware-uk/clipped/issues/241) that is
        /// still open. It is written down here because the arm exists and
        /// because what a press does in it is a decision, not an accident.
        fn watching() -> Self {
            Self {
                asked: Mutex::new(Vec::new()),
                status: RecorderStatus::Watching(clipped_ipc::Watching {
                    session: None,
                    pending: None,
                }),
            }
        }

        fn recording() -> Self {
            Self {
                asked: Mutex::new(Vec::new()),
                status: RecorderStatus::Recording(ActiveRecording {
                    recording_id: "r-1".to_owned(),
                    output: r"D:\clips\session.mkv".to_owned(),
                    target: "process `cs2.exe`".to_owned(),
                    elapsed_ms: 4_000,
                    // Whether this recording keeps a buffer changes what
                    // `save_replay` answers, not which command a press sends,
                    // and it is the command these tests are about.
                    replay_seconds: Some(300),
                    // Which command a press sends does not depend on whether
                    // the recording belongs to a sitting, and these tests are
                    // about the command.
                    session: None,
                }),
            }
        }

        fn asked(&self) -> Vec<Command> {
            self.asked.lock().expect("nothing panicked").clone()
        }
    }

    impl CommandHandler for AskedRecorder {
        fn call(&self, command: Command) -> Result<Reply, ProtocolError> {
            self.asked.lock().expect("nothing panicked").push(command);
            Ok(Reply::Pong)
        }

        fn status(&self) -> RecorderStatus {
            self.status.clone()
        }

        fn features(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// The combination a press in these tests carries.
    ///
    /// Nothing here registers anything, so which one it is only matters to the
    /// log line it appears in.
    fn a_combination() -> Hotkey {
        "Ctrl+F8".parse().expect("Ctrl+F8 is a hotkey")
    }

    fn handler(recorder: &Arc<AskedRecorder>) -> Arc<dyn CommandHandler> {
        Arc::clone(recorder) as Arc<dyn CommandHandler>
    }

    /// The list this ticket is judged on: what a press can actually do in this
    /// build.
    #[test]
    fn the_actions_with_handlers_are_the_ones_this_recorder_can_perform() {
        let recorder = Arc::new(AskedRecorder::idle());

        let handled: Vec<HotkeyAction> = handlers_for(&handler(&recorder), &a_game_in_front())
            .handled()
            .collect();

        assert_eq!(
            handled,
            vec![
                HotkeyAction::SaveReplay,
                HotkeyAction::AddBookmark,
                HotkeyAction::TakeScreenshot,
                HotkeyAction::ToggleRecording,
            ],
            "the handled actions are the ones the recorder answers a command for",
        );
    }

    /// Every action given a handler has to become a command, or its key is one
    /// that does nothing — which is the defect this whole ticket is about.
    #[test]
    fn every_action_with_a_handler_becomes_a_command_while_a_recording_is_running() {
        let recorder = Arc::new(AskedRecorder::recording());
        let recorder = handler(&recorder);
        let foreground = a_game_in_front();

        for action in handlers_for(&recorder, &foreground).handled() {
            assert!(
                command_for(recorder.as_ref(), &foreground, action).is_ok(),
                "{action} has a handler and no command, so pressing its key would do nothing",
            );
        }
    }

    /// Presses `action`'s key through the handler `handlers_for` registered for
    /// it, and answers with what the recorder was asked for.
    ///
    /// The three tests below go through this and **not** through [`perform`]
    /// directly, which is the whole point of them. `perform` is told which
    /// action to run, so a test that calls it proves only that `perform` maps an
    /// action it was handed to a command. What it cannot see is
    /// [`handlers_for`] registering a closure against the wrong action — and
    /// that is not a dead key but a key that does something other than what its
    /// row on the settings screen says it does.
    fn pressed(
        recorder: &Arc<AskedRecorder>,
        foreground: &Foreground,
        action: HotkeyAction,
    ) -> Vec<Command> {
        let mut handlers = handlers_for(&handler(recorder), foreground);

        handlers
            .press(action, a_combination())
            .unwrap_or_else(|unhandled| {
                panic!("this build performs {action}, so its key must have a handler: {unhandled}")
            });

        recorder.asked()
    }

    /// The key SPEC.md section 7 names, and the one this recorder refused to
    /// perform until issue #38 built the buffer behind it.
    #[test]
    fn the_handler_registered_for_save_replay_saves_out_of_whatever_is_running() {
        let recorder = Arc::new(AskedRecorder::recording());

        match pressed(&recorder, &a_game_in_front(), HotkeyAction::SaveReplay).as_slice() {
            [Command::SaveReplay(request)] => {
                assert_eq!(
                    request,
                    &SaveReplay::default(),
                    "a press names no recording, no duration and no file: it keeps what the \
                     recording's buffer was started with, where that recording's clips go",
                );
            }
            other => panic!("a replay press must send one `save_replay`, not {other:?}"),
        }
    }

    #[test]
    fn the_handler_registered_for_add_bookmark_bookmarks_whatever_is_running() {
        let recorder = Arc::new(AskedRecorder::recording());

        match pressed(&recorder, &a_game_in_front(), HotkeyAction::AddBookmark).as_slice() {
            [Command::AddBookmark(request)] => assert_eq!(
                request.recording_id, None,
                "a key press means whatever is running, because it cannot mean anything else",
            ),
            other => panic!("a bookmark press must send one `add_bookmark`, not {other:?}"),
        }
    }

    #[test]
    fn the_handler_registered_for_take_screenshot_takes_a_screenshot() {
        let recorder = Arc::new(AskedRecorder::recording());

        match pressed(&recorder, &a_game_in_front(), HotkeyAction::TakeScreenshot).as_slice() {
            [Command::TakeScreenshot(request)] => assert_eq!(
                request.pid, None,
                "a key press names no window: the picture comes from a frame already captured",
            ),
            other => panic!("a screenshot press must send one `take_screenshot`, not {other:?}"),
        }
    }

    /// The half of `toggle_recording` this build performs, and the reason the
    /// action is given a handler at all.
    #[test]
    fn the_handler_registered_for_toggle_recording_stops_the_recording_that_is_running() {
        let recorder = Arc::new(AskedRecorder::recording());

        match pressed(&recorder, &a_game_in_front(), HotkeyAction::ToggleRecording).as_slice() {
            [Command::StopRecording(stop)] => assert_eq!(
                stop.recording_id, None,
                "a toggle stops whatever is running, which is all a key press can mean",
            ),
            other => panic!("a toggle while recording must send `stop_recording`, not {other:?}"),
        }
    }

    /// The other half, and the whole of issue #416: a press with nothing
    /// running records what the user was looking at.
    ///
    /// Through the handler `handlers_for` registered rather than through
    /// [`command_for`], for the reason [`pressed`] gives: what is being asserted
    /// is that pressing *this* key sends *this* command.
    #[test]
    fn the_handler_registered_for_toggle_recording_starts_a_recording_of_what_is_in_front() {
        let recorder = Arc::new(AskedRecorder::idle());

        match pressed(&recorder, &a_game_in_front(), HotkeyAction::ToggleRecording).as_slice() {
            [Command::StartRecording(request)] => {
                assert_eq!(
                    request.pid,
                    Some(A_GAME),
                    "a toggle with nothing running records the window the user is in",
                );
                // The window's `recording_request` builds exactly this, and the
                // agreement is the third acceptance criterion. A press that
                // named the window by title or by executable would resolve
                // differently — a title matches on a substring — and a press
                // that left `replay` false would start a recording the replay
                // hotkey then refuses to save from.
                assert_eq!(
                    request,
                    &StartRecording {
                        pid: Some(A_GAME),
                        replay: true,
                        ..StartRecording::default()
                    },
                    "the hotkey has to ask for the same recording the Record button asks for",
                );
            }
            other => panic!("a toggle while idle must send one `start_recording`, not {other:?}"),
        }
    }

    /// AGENTS.md section 54, and issue #416's second acceptance criterion: with
    /// nothing sensible in front, the press refuses and says what was there
    /// instead rather than recording something nobody chose.
    #[test]
    fn a_toggle_press_with_nothing_worth_recording_in_front_says_what_was_there_instead() {
        let recorder = Arc::new(AskedRecorder::idle());
        let asked = handler(&recorder);

        // Every way the foreground can fail to be a target, each with the word
        // a user would recognise in it. `Clipped` is the one issue #416 calls
        // out by name: recording the Clipped window because somebody pressed
        // the key while looking at it is worse than refusing.
        let refusals = [
            (NotRecordable::Nothing, "no window has the foreground"),
            (
                NotRecordable::ShellSurface {
                    class: "Shell_TrayWnd".to_owned(),
                },
                "taskbar",
            ),
            (
                NotRecordable::Clipped {
                    process_name: "clipped-desktop.exe".to_owned(),
                },
                "Clipped's own window",
            ),
            (NotRecordable::NoProcess, "no process"),
        ];

        for (reason, expected) in refusals {
            let foreground = nothing_in_front(reason.clone());

            let refusal = command_for(asked.as_ref(), &foreground, HotkeyAction::ToggleRecording)
                .expect_err("there is nothing in front to record");
            assert_eq!(refusal.code, ErrorCode::TargetNotFound, "{reason:?}");
            assert!(
                refusal.message.contains(expected),
                "the refusal has to say what was in front instead of {expected:?}: {}",
                refusal.message,
            );

            perform(
                &asked,
                &foreground,
                HotkeyAction::ToggleRecording,
                a_combination(),
            );
        }

        assert!(
            recorder.asked().is_empty(),
            "a press that cannot become a command must send nothing rather than guess at a window",
        );
    }

    /// A watching recorder is not a recording one, and a key that refused every
    /// press while nothing was being recorded would be a key that does nothing.
    #[test]
    fn a_toggle_press_while_watching_for_games_records_what_is_in_front_too() {
        let recorder = Arc::new(AskedRecorder::watching());

        // Issue #421 left this refusing, because the only sentence available
        // then was "start it from the window": the recorder could not say which
        // window itself. It can now, so the press does what it does when the
        // recorder is idle — the game somebody reaches for the keyboard over is
        // the one the catalogue did not recognise.
        match pressed(&recorder, &a_game_in_front(), HotkeyAction::ToggleRecording).as_slice() {
            [Command::StartRecording(request)] => assert_eq!(request.pid, Some(A_GAME)),
            other => panic!("a toggle while watching must start a recording, not {other:?}"),
        }
    }

    /// The same press against a **real** recorder that is really watching.
    ///
    /// The test above proves what the arm does; this proves a press can reach
    /// it. Until issue #584 no recorder could report
    /// [`RecorderStatus::Watching`] at all, so that arm had been dead code since
    /// it landed and a stand-in recorder was the only thing that had ever
    /// entered it. What makes this the real path is that the status comes from
    /// `crate::serve`'s own state, through the same `CommandHandler::status`
    /// [`command_for`] calls.
    ///
    /// The assertion that would fail without the producer is the **status**, not
    /// the command: issue #583 decided that a toggle while watching does what a
    /// toggle while idle does, so the two arms send the same request and the
    /// command alone cannot tell them apart. What can is which state the press
    /// read, and whether the foreground was asked at all — a foreground is only
    /// resolved by the two arms that start a recording.
    #[test]
    fn a_toggle_press_reaches_the_watching_arm_of_a_recorder_that_is_really_watching() {
        let directory = scratch("watching-toggle");
        let service = a_recorder(&directory);
        let watching = service.recordings().watch_for_games();
        let recorder = Arc::clone(&service) as Arc<dyn CommandHandler>;

        assert_eq!(
            recorder.status(),
            RecorderStatus::Watching(clipped_ipc::Watching {
                session: None,
                pending: None
            }),
            "a recorder watching for games has to say so, or a press reads `idle` and this test \
             is about a different arm",
        );

        // Asked only from `start_what_is_in_front`, which only the `Idle` and
        // `Watching` arms reach: a refusing arm would leave this `false`.
        let asked = Arc::new(Mutex::new(false));
        let seen = Arc::clone(&asked);
        let foreground: Foreground = Arc::new(move || {
            *seen.lock().expect("nothing panicked") = true;
            Ok(ForegroundTarget::Recordable(Box::new(WindowInfo::new(
                clipped_windows::WindowHandle::from_raw(0x1234),
                "Counter-Strike 2".to_owned(),
                A_GAME,
                Some("cs2.exe".to_owned()),
                WindowGeometry::new(
                    PixelSize::new(2560, 1440),
                    DEFAULT_DPI,
                    clipped_windows::MonitorHandle::from_raw(1),
                ),
                false,
                None,
            ))))
        });

        match command_for(
            recorder.as_ref(),
            &foreground,
            HotkeyAction::ToggleRecording,
        ) {
            Ok(Command::StartRecording(request)) => assert_eq!(
                request.pid,
                Some(A_GAME),
                "a toggle while watching records what is in front, exactly as one while idle does",
            ),
            other => panic!("a toggle while watching must start a recording, not {other:?}"),
        }

        // And through the handler the registration wires up, which is what a
        // key press actually goes through. The recorder refuses the request —
        // there is no window belonging to process 4242 on this machine — and
        // the refusal is logged rather than thrown, which is what a press with
        // no client to answer does.
        handlers_for(&recorder, &foreground)
            .press(HotkeyAction::ToggleRecording, a_combination())
            .expect("this build performs Start or stop recording, so its key has a handler");

        assert!(
            *asked.lock().expect("nothing panicked"),
            "a press while watching has to ask what is in front: an arm that refused would send \
             nothing and record nothing while nothing was being recorded",
        );
        assert_eq!(
            recorder.status(),
            RecorderStatus::Watching(clipped_ipc::Watching {
                session: None,
                pending: None
            }),
            "and a refused press leaves the recorder watching, rather than claiming a recording \
             it did not start",
        );

        drop(watching);
    }

    /// A recorder over a library, a settings file and a games file of this
    /// test's own, never the ones belonging to whoever is running the suite
    /// (AGENTS.md section 25).
    fn a_recorder(directory: &std::path::Path) -> Arc<crate::serve::RecorderService> {
        Arc::new(crate::serve::RecorderService::with_library(
            clipped_ipc::EventPublisher::new(),
            crate::library::LibraryReader::at(Some(directory.join("library.db"))),
            crate::library::LibraryIndexer::at(
                Some(directory.join("library.db")),
                vec![directory.to_path_buf()],
            ),
            clipped_game_detection::catalogue::Catalogue::default(),
        ))
    }

    /// A directory of this test's own, removed again when the test that made it
    /// passes.
    ///
    /// This used to return a bare path and nothing ever removed it
    /// ([issue #598](https://github.com/wildware-uk/clipped/issues/598)). See
    /// [`Scratch`] for what the returned value does and how to hold it.
    fn scratch(name: &str) -> Scratch {
        Scratch::new(&format!("hotkeys-{name}"))
    }

    /// A window that is in front and cannot be captured is a different answer
    /// from no window at all, and it is the one `resolve_window` would give.
    #[test]
    fn a_toggle_press_at_a_window_that_cannot_be_captured_is_refused_with_that_reason() {
        let recorder = Arc::new(AskedRecorder::idle());
        let asked = handler(&recorder);
        let foreground = nothing_in_front(NotRecordable::NotCapturable {
            process_name: Some("player.exe".to_owned()),
            exclusion: clipped_windows::Exclusion::ContentProtected,
        });

        let refusal = command_for(asked.as_ref(), &foreground, HotkeyAction::ToggleRecording)
            .expect_err("a window excluded from capture is not something to record");

        assert_eq!(
            refusal.code,
            ErrorCode::TargetNotCapturable,
            "a window that is there and cannot be recorded has its own code, because the two ask \
             different things of the user",
        );
        assert!(
            refusal.message.contains("player.exe") && refusal.message.contains("record black"),
            "{}",
            refusal.message,
        );
    }

    /// What a press does when Windows itself will not answer.
    ///
    /// Refused, and said out loud. Guessing past it — recording the last thing
    /// seen, or the first window in the list — is recording something the user
    /// did not choose.
    #[test]
    fn a_toggle_press_windows_will_not_answer_is_refused_rather_than_guessed_at() {
        let recorder = Arc::new(AskedRecorder::idle());
        let asked = handler(&recorder);
        // Which failure it is does not matter and cannot be chosen from here:
        // the constructors are `clipped_windows`'s own. What matters is that
        // Windows would not describe the window it had just named as the
        // foreground one, and that the press then sends nothing.
        let foreground: Foreground = Arc::new(|| {
            Err(WindowsError::WindowGone {
                handle: clipped_windows::WindowHandle::from_raw(0x1234),
            })
        });

        let refusal = command_for(asked.as_ref(), &foreground, HotkeyAction::ToggleRecording)
            .expect_err("a foreground nobody can read is not a target");
        assert!(
            refusal.message.contains("would not say what is in front"),
            "{}",
            refusal.message,
        );

        perform(
            &asked,
            &foreground,
            HotkeyAction::ToggleRecording,
            a_combination(),
        );
        assert!(
            recorder.asked().is_empty(),
            "a press that cannot become a command must send nothing rather than guess at one",
        );
    }

    /// The row the settings screen draws for an action nothing performs. It has
    /// to name the milestone and the issue, because "nothing happened" is what
    /// the user would otherwise have to work out for themselves.
    #[test]
    fn an_action_this_build_cannot_perform_is_reported_as_unavailable_and_says_which_issue() {
        let registration = a_registration();

        let row = row_for(
            registration
                .status(HotkeyAction::OpenOverlay)
                .expect("every action has a row"),
        );

        assert_eq!(row.action, "open_overlay");
        assert!(!row.handled);
        let reason = row.unavailable.expect("an unhandled action says why");
        assert!(reason.contains("Open overlay"), "{reason}");
        assert!(reason.contains("M5"), "{reason}");
        assert!(reason.contains("#53"), "{reason}");

        let row = row_for(
            registration
                .status(HotkeyAction::AddBookmark)
                .expect("every action has a row"),
        );
        assert!(
            row.handled && row.unavailable.is_none(),
            "the recorder adds bookmarks, so that row must not read as unavailable",
        );
        assert_eq!(row.hotkey.as_deref(), Some("Ctrl+F9"));

        // The row this ticket turned over. Save replay was the example above
        // until issue #38 built it, and a build that went on reporting it as
        // unavailable would be telling a user a shipped feature is missing —
        // invisibly, because the sentence still reads plausibly (AGENTS.md
        // sections 27 and 54).
        let row = row_for(
            registration
                .status(HotkeyAction::SaveReplay)
                .expect("every action has a row"),
        );
        assert!(
            row.handled && row.unavailable.is_none(),
            "the recorder saves replays, so that row must not read as unavailable: {row:?}",
        );
        assert_eq!(row.hotkey.as_deref(), Some("Ctrl+F10"));
    }

    /// The whole list, every time. A screen sent only the bound actions could
    /// not offer the rest.
    #[test]
    fn every_action_is_reported_whether_or_not_it_is_bound() {
        let rows = report_of(&a_registration());

        assert_eq!(rows.len(), ACTIONS.len());
        let unbound = rows
            .iter()
            .filter(|row| matches!(row.state, HotkeyState::Unbound))
            .count();
        assert!(
            unbound > 0 && unbound < rows.len(),
            "the defaults bind some actions and not others, so both kinds of row must appear: \
             {rows:?}",
        );
    }

    /// The wire the settings file reaches Windows down.
    ///
    /// `start` is the only production caller of
    /// [`Configuration::resolve_hotkeys`], and the one line joining it to
    /// `HotkeyService::start` is the whole of "Clipped reads your hotkeys". A
    /// `start` that resolved the configuration and then registered
    /// [`Bindings::defaults`] anyway would throw every override in
    /// `settings.json` away in silence — the settings screen would still say the
    /// file is read, the recorder would still report a full table, and the only
    /// symptom would be that the combination the user chose does nothing and the
    /// one they replaced still works.
    ///
    /// Asserted through `start` and not through `resolve_hotkeys`, because
    /// `resolve_hotkeys` has its own tests in `clipped_session` and they pass
    /// either way: what is unguarded is this process handing the answer on.
    ///
    /// The row is read back from the report rather than from the service,
    /// because the report is also what `get_hotkeys` answers the settings screen
    /// with, so one assertion covers both. It carries the combination that was
    /// *asked for* whether or not Windows granted it, which is what keeps this
    /// test from depending on what else on the machine holds a function key.
    /// A configuration binding each action in `bindings` and nothing else.
    fn configured(bindings: &[(HotkeyAction, &str)]) -> Configuration {
        let mut overrides = HotkeyOverrides::none();
        for action in ACTIONS {
            let binding = bindings.iter().find(|(bound, _)| *bound == action).map_or(
                HotkeyOverride::Unbound,
                |(_, text)| {
                    HotkeyOverride::Bound(text.parse().expect("a combination this test writes"))
                },
            );
            overrides
                .set(action, Some(binding))
                .expect("this test does not write a combination twice");
        }
        let mut configuration = Configuration::defaults();
        configuration.set_hotkeys(overrides);
        configuration
    }

    /// Two combinations nothing else on the machine will have.
    ///
    /// `F13` upwards, chosen from this process's identifier, exactly as
    /// `crates/hotkeys/tests/windows_hotkeys.rs` chooses: no keyboard has these
    /// keys, nothing binds them, and two checkouts running the suite at once do
    /// not fight over one registration. In particular they are **not** the
    /// shipped defaults, which the recorder the person at the keyboard is
    /// running already holds.
    fn two_spare_combinations() -> (String, String) {
        let first = std::process::id() % 12;
        let second = (first + 1) % 12;
        (
            format!("Ctrl+Alt+Shift+F{}", first + 13),
            format!("Ctrl+Alt+Shift+F{}", second + 13),
        )
    }

    /// Two actions trading combinations in one save, which is the one case that
    /// cannot be done a binding at a time.
    ///
    /// `HotkeyService::rebind` refuses a combination another Clipped action
    /// still holds — the same rule a fresh set of bindings is validated by — so
    /// pointing Save replay at what Add bookmark has, while Add bookmark still
    /// has it, is refused and the save reaches the file and stops. The settings
    /// screen sends both keys in one `apply_settings`, so this is a swap a user
    /// can ask for in one press of Save.
    ///
    /// Asserted over what was *asked of Windows* rather than over what Windows
    /// granted, for the reason `the_combination_registered_is_the_one_the_settings_file_names`
    /// is: whether a machine running the suite can register anything is not
    /// something this test may depend on. A refused rebind leaves the old
    /// combination in the report, so the assertion still fails when the release
    /// pass is removed.
    #[test]
    fn two_actions_can_trade_combinations_in_one_save() {
        let (first, second) = two_spare_combinations();
        let recorder = Arc::new(AskedRecorder::idle());
        let registered = start(
            &handler(&recorder),
            &configured(&[
                (HotkeyAction::SaveReplay, &first),
                (HotkeyAction::AddBookmark, &second),
            ]),
        );

        registered.apply(&configured(&[
            (HotkeyAction::SaveReplay, &second),
            (HotkeyAction::AddBookmark, &first),
        ]));

        let report = registered.report();
        // Before the assertions, so that a failure does not leave the
        // combinations registered for the rest of the suite.
        registered.stop();

        let report = report.expect("the service started, so there is a report");
        assert_eq!(
            row(&report, "save_replay").hotkey.as_deref(),
            Some(second.as_str()),
            "Save replay did not take the combination Add bookmark was moving off, which is the \
             swap being refused rather than performed: {report:?}",
        );
        assert_eq!(
            row(&report, "add_bookmark").hotkey.as_deref(),
            Some(first.as_str()),
            "Add bookmark did not take the combination Save replay gave up: {report:?}",
        );
    }

    /// One action's row.
    fn row<'a>(
        report: &'a [clipped_ipc::HotkeyBinding],
        action: &str,
    ) -> &'a clipped_ipc::HotkeyBinding {
        report
            .iter()
            .find(|row| row.action == action)
            .unwrap_or_else(|| panic!("`{action}` should be in the report: {report:?}"))
    }

    #[test]
    fn the_combination_registered_is_the_one_the_settings_file_names() {
        let recorder = Arc::new(AskedRecorder::idle());
        let mut overrides = HotkeyOverrides::none();
        overrides
            .set(
                HotkeyAction::AddBookmark,
                Some(HotkeyOverride::Bound(
                    "Ctrl+Shift+F7".parse().expect("Ctrl+Shift+F7 is a hotkey"),
                )),
            )
            .expect("nothing else is bound to Ctrl+Shift+F7");
        let mut configuration = Configuration::defaults();
        configuration.set_hotkeys(overrides);

        let registered = start(&handler(&recorder), &configuration);
        let row = registered
            .report()
            .as_ref()
            .expect("the service starts, so the report is the list of rows")
            .iter()
            .find(|row| row.action == "add_bookmark")
            .cloned()
            .expect("every action has a row");
        // Before the assertion, so that a failure does not leave the
        // combination registered for the rest of the suite.
        registered.stop();

        assert_eq!(
            row.hotkey.as_deref(),
            Some("Ctrl+Shift+F7"),
            "the recorder registered a combination the settings file does not name, so the \
             overrides in it were thrown away and the user's own hotkey does nothing",
        );
    }

    /// A real registration of the shipped defaults, through the production
    /// handlers, so `handled` is what this build handles rather than what a
    /// fixture claims.
    ///
    /// The service is stopped before the registration is read, so that no test
    /// leaves `Ctrl`+`F10` taken from the rest of the suite.
    fn a_registration() -> Registration {
        let recorder = Arc::new(AskedRecorder::idle());
        let (service, _events) = HotkeyService::start(
            &Bindings::defaults(),
            handlers_for(&handler(&recorder), &a_game_in_front()),
        )
        .expect("the hotkey service starts");
        let registration = service.registration().clone();
        service.stop();
        registration
    }
}
