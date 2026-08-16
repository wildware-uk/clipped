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
//! `get_hotkeys` and, once [issue
//! #54](https://github.com/wildware-uk/clipped/issues/54) lands, writes the
//! bindings into the settings file this reads at start-up.
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
//! # Threads
//!
//! `clipped_hotkeys` gives each handled action a thread of its own and never
//! runs a handler on the thread that received the press (`docs/hotkeys.md`). So
//! a handler here may take as long as stopping a recording takes — which is a
//! flush, an encoder drain and a container trailer — without delaying the next
//! press or any other action. Nothing here runs on a capture thread, and nothing
//! here is called from one.

use std::sync::Arc;

use clipped_hotkeys::{
    ActionStatus, BindingState, Handlers, Hotkey, HotkeyAction, HotkeyService, Registration,
    Unhandled,
};
use clipped_ipc::{
    AddBookmark, Command, CommandHandler, ErrorCode, HotkeyBinding, HotkeyState, ProtocolError,
    RecorderStatus, Reply, SaveReplay, StopRecording, TakeScreenshot,
};
use clipped_session::config::Configuration;

/// The hotkey service this process is running, and what it registered.
///
/// Dropping it gives every combination back to Windows and waits for a handler
/// that is still running, so a recorder that ends — by request, by Ctrl+C or by
/// a panic unwinding out of `serve` — never leaves a combination registered that
/// nothing is listening for (AGENTS.md section 58).
#[derive(Debug)]
pub struct RegisteredHotkeys {
    /// [`None`] when the service could not be started at all, which is not the
    /// same as every combination being refused: see [`start`].
    service: Option<HotkeyService>,
}

impl RegisteredHotkeys {
    /// Gives every combination back and waits for the handler that is running.
    ///
    /// Called before the recorder stops the recording it is making, so that a
    /// press cannot arrive while the process is winding up and ask for a
    /// recording that is halfway through being finished.
    pub fn stop(mut self) {
        if let Some(service) = self.service.take() {
            service.stop();
        }
    }
}

/// Registers the user's hotkeys and starts delivering presses to `recorder`.
///
/// Returns the running service and the report `get_hotkeys` answers with. The
/// report is produced on **every** path, including the ones where nothing was
/// registered, because "the recorder could not register its hotkeys" and "every
/// hotkey registered cleanly" are opposite answers and an empty list would be
/// drawn as the second (AGENTS.md section 27).
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
) -> (RegisteredHotkeys, Result<Vec<HotkeyBinding>, String>) {
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
            return (
                RegisteredHotkeys { service: None },
                Err(format!(
                    "No hotkey is registered: {error} Fix the `hotkeys` section of the settings \
                     file and restart Clipped."
                )),
            );
        }
    };

    let handlers = handlers_for(recorder);

    match HotkeyService::start(&bindings, handlers) {
        Ok((service, events)) => {
            // The events channel is the *other* way to learn what a press did,
            // and this process does not use it: every outcome is already logged
            // by `perform` on the handler's own thread, with the action and the
            // recorder's own sentence. Dropping the receiver costs the events
            // and nothing else, which `HotkeyService::start` documents.
            drop(events);

            let report = report_of(service.registration());
            tracing::info!(
                registered = service.registration().bound().count(),
                conflicts = service.registration().conflicts().count(),
                "the global hotkeys were registered"
            );

            (
                RegisteredHotkeys {
                    service: Some(service),
                },
                Ok(report),
            )
        }
        Err(error) => {
            tracing::error!(%error, "no global hotkey was registered");
            (
                RegisteredHotkeys { service: None },
                Err(format!("No hotkey is registered: {error}.")),
            )
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
fn handlers_for(recorder: &Arc<dyn CommandHandler>) -> Handlers {
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
        handlers = handlers.on(action, move |press| {
            perform(&recorder, press.action(), press.hotkey())
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
fn perform(recorder: &Arc<dyn CommandHandler>, action: HotkeyAction, hotkey: Hotkey) {
    match command_for(recorder.as_ref(), action) {
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
/// The refusal for a press that cannot become a command at all, which today is
/// exactly one case: starting a recording, which needs a window to record and a
/// key press does not carry one ([issue
/// #416](https://github.com/wildware-uk/clipped/issues/416)).
fn command_for(
    recorder: &dyn CommandHandler,
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
            RecorderStatus::Idle => Err(ProtocolError::new(
                ErrorCode::NotRecording,
                "nothing is being recorded, and a hotkey does not say which window to record. \
                 Start the recording from the Clipped window or the tray",
            )),
            // Watching is not idle, and the idle refusal would be a lie here:
            // it tells somebody to start a recording from the window, when a
            // watching recorder is going to start one itself the moment a game
            // appears. Whether this key should *also* start one early is issue
            // #421's question, and answering it here would be answering it in
            // the wrong place.
            RecorderStatus::Watching(_) => Err(ProtocolError::new(
                ErrorCode::NotRecording,
                "nothing is being recorded yet. Clipped is watching for a game and will start \
                 recording on its own when one appears",
            )),
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

    use clipped_hotkeys::{Bindings, Hotkey, HotkeyAction, HotkeyService, Registration, ACTIONS};
    use clipped_ipc::{
        ActiveRecording, Command, CommandHandler, ErrorCode, HotkeyState, ProtocolError,
        RecorderStatus, Reply, SaveReplay,
    };
    use clipped_session::config::{Configuration, HotkeyOverride, HotkeyOverrides};

    use super::{command_for, handlers_for, perform, report_of, row_for, start};

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

        let handled: Vec<HotkeyAction> = handlers_for(&handler(&recorder)).handled().collect();

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

        for action in handlers_for(&recorder).handled() {
            assert!(
                command_for(recorder.as_ref(), action).is_ok(),
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
    fn pressed(recorder: &Arc<AskedRecorder>, action: HotkeyAction) -> Vec<Command> {
        let mut handlers = handlers_for(&handler(recorder));

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

        match pressed(&recorder, HotkeyAction::SaveReplay).as_slice() {
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

        match pressed(&recorder, HotkeyAction::AddBookmark).as_slice() {
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

        match pressed(&recorder, HotkeyAction::TakeScreenshot).as_slice() {
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

        match pressed(&recorder, HotkeyAction::ToggleRecording).as_slice() {
            [Command::StopRecording(stop)] => assert_eq!(
                stop.recording_id, None,
                "a toggle stops whatever is running, which is all a key press can mean",
            ),
            other => panic!("a toggle while recording must send `stop_recording`, not {other:?}"),
        }
    }

    /// AGENTS.md section 54: the half that is not built refuses by name rather
    /// than guessing at a window.
    #[test]
    fn a_toggle_press_with_nothing_recording_is_refused_in_words_that_say_why() {
        let recorder = Arc::new(AskedRecorder::idle());
        let asked = handler(&recorder);

        let refusal = command_for(asked.as_ref(), HotkeyAction::ToggleRecording)
            .expect_err("nothing is being recorded, so there is nothing to stop");
        assert_eq!(refusal.code, ErrorCode::NotRecording);
        assert!(
            refusal.message.contains("which window to record"),
            "the refusal has to name what is missing rather than say something failed: {}",
            refusal.message,
        );

        perform(&asked, HotkeyAction::ToggleRecording, a_combination());
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

        let (registered, report) = start(&handler(&recorder), &configuration);
        let row = report
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
        let (service, _events) =
            HotkeyService::start(&Bindings::defaults(), handlers_for(&handler(&recorder)))
                .expect("the hotkey service starts");
        let registration = service.registration().clone();
        service.stop();
        registration
    }
}
