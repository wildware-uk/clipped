//! The notification-area icon and its menu.
//!
//! SPEC.md section 33 asks for a tray with the current status, Save Replay, Add
//! Bookmark, Start/Stop Recording, Open Library, Settings and Exit, and for
//! closing the window to minimise to it rather than to quit. This module is
//! that, and it is the first part of Clipped's interface that *does* anything:
//! until now the window could only watch.
//!
//! # What is here and what is next door
//!
//! [`crate::tray_model`] decides what the tray should look like and what each
//! item does. This module is the machinery: it builds the menu, replaces it when
//! the model changes, and turns a click into a command on the recorder. Keeping
//! the two apart is what makes the rules testable — `tray_model` has no Tauri in
//! it and runs under `cargo test`.
//!
//! # Threads
//!
//! Menu events arrive on the thread running the window, and every action here is
//! a round trip to another process over a named pipe. So each one is answered on
//! a thread of its own: a `stop_recording` does not return until the file has
//! been finalised, and a window frozen for the length of that is a window the
//! user will conclude has crashed.
//!
//! The tray itself may only be touched from the main thread, which is what
//! [`tauri::AppHandle::run_on_main_thread`] is for. The link's event thread
//! therefore computes the new model where it is and hands the drawing back.
//!
//! # Explorer restarting
//!
//! When Explorer restarts it broadcasts `TaskbarCreated`, and an application
//! that does not re-add its icon loses it silently for the rest of the session.
//! Nothing here handles that, because `tray-icon` — the crate behind Tauri's
//! tray — registers the message and re-adds the icon itself. That is a claim
//! worth checking rather than believing, and `docs/desktop-ui.md` records what
//! restarting Explorer actually did.

use std::sync::{Arc, Mutex};

use clipped_ipc::supervisor::{wait_for_recorder_to_exit, RecorderCallError, ShutdownOutcome};
use clipped_ipc::{Command, RecorderLink, RecorderLinkState, Reply, StartRecording, StopRecording};
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter as _, Manager as _, Runtime};

use crate::foreground;
use crate::tray_model::{tray_model, MenuEntry, RecordAction, TrayModel};

/// The tray icon's identifier, so that the one built here can be found again.
const TRAY_ID: &str = "clipped-tray";

/// The menu item identifiers.
///
/// Stable across rebuilds of the menu, because the handler matches on them and
/// the menu is replaced whenever the state changes. The Start/Stop item keeps
/// one identifier for both jobs: which one it is doing is a property of the
/// model, and reading it there is one place to be right rather than two.
mod ids {
    /// The status line. Never enabled; it is a sentence, not a control.
    pub(super) const STATUS: &str = "status";
    /// Save Replay.
    pub(super) const SAVE_REPLAY: &str = "save-replay";
    /// Add Bookmark.
    pub(super) const ADD_BOOKMARK: &str = "add-bookmark";
    /// Start or Stop Recording.
    pub(super) const RECORD: &str = "record";
    /// Open Library.
    pub(super) const LIBRARY: &str = "library";
    /// Settings.
    pub(super) const SETTINGS: &str = "settings";
    /// Exit.
    pub(super) const EXIT: &str = "exit";
}

/// The event the window listens for when the tray sends it somewhere.
pub(crate) const NAVIGATE_EVENT: &str = "tray-navigate";

/// The event the window listens for when something the tray did failed.
pub(crate) const NOTICE_EVENT: &str = "tray-notice";

/// The label of the window this application has.
const MAIN_WINDOW: &str = "main";

/// How long the recorder is given to finish and exit before the window stops
/// waiting for it.
///
/// Long enough to finalise a container — `docs/muxing.md` measures that in
/// hundreds of milliseconds — with room for a disk that is busy. When it runs
/// out the window exits anyway and says so: the recorder is detached, so it goes
/// on finishing the file whether or not anything is watching, and refusing to
/// close the window would leave the user with no way out at all.
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// The tray, and the model it was last drawn from.
///
/// Held in Tauri's state so that a menu event can ask what the item it belongs
/// to was for.
pub(crate) struct Tray {
    icon: TrayIcon,
    model: Mutex<TrayModel>,
}

/// Written by hand because [`TrayIcon`] has no [`Debug`] of its own, and the
/// crate's `missing_debug_implementations` lint is not worth turning off for
/// one type. The model is the half worth printing anyway; the icon is an
/// operating-system handle.
impl std::fmt::Debug for Tray {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Tray")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Tray {
    /// What the tray is showing.
    fn model(&self) -> TrayModel {
        self.model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Puts Clipped in the notification area.
///
/// # Errors
///
/// Whatever Tauri said. A tray that could not be created is reported and is not
/// fatal: the window still opens and still shows the recorder's state, which is
/// a worse application than the one intended but a better one than none.
pub(crate) fn install(app: &AppHandle, link: &RecorderLink) -> tauri::Result<()> {
    let model = tray_model(&link.state(), foreground::last_seen().as_ref());
    let menu = build_menu(app, &model)?;

    let icon = TrayIconBuilder::with_id(TRAY_ID)
        .icon(model.mark.image())
        .tooltip(&model.tooltip)
        .menu(&menu)
        // The menu is the right button's. The left button raises the window,
        // which is what a user who has just closed it to the tray expects, and
        // what the platform convention is.
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            let app = tray.app_handle();
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => show_window(app),
                // The pointer has arrived at the icon and the menu is about to
                // be asked for. This is when what it should say is decided:
                // "Start Recording — cs2.exe" depends on which window the user
                // was last in, and rebuilding the menu every time that changed
                // would be work on every alt-tab, beside a game, for a menu
                // nobody is looking at (AGENTS.md section 18).
                TrayIconEvent::Enter { .. } => {
                    if let Some(link) = app.try_state::<RecorderLink>() {
                        refresh(app, &link.state());
                    }
                }
                _ => {}
            }
        })
        .build(app)?;

    app.manage(Arc::new(Tray {
        icon,
        model: Mutex::new(model),
    }));

    Ok(())
}

/// Redraws the tray, if what it should show has changed.
///
/// Called whenever the link says something and whenever the foreground window
/// moves. Both happen often and neither usually changes the menu, so the model
/// is compared first: replacing a menu the user may have open is not free, and
/// rebuilding one on every alt-tab would be work beside a game for no reason
/// (AGENTS.md section 18).
pub(crate) fn refresh(app: &AppHandle, link: &RecorderLinkState) {
    let Some(tray) = app.try_state::<Arc<Tray>>() else {
        // No tray was installed. Nothing to redraw, and the window is still
        // showing the same state through its own channel.
        return;
    };
    let tray = Arc::clone(&tray);

    let model = tray_model(link, foreground::last_seen().as_ref());
    if model == tray.model() {
        return;
    }

    let handle = app.clone();
    // The tray belongs to the main thread; this is called from the link's own.
    let drawn = app.run_on_main_thread(move || {
        if let Err(error) = draw(&handle, &tray, model) {
            eprintln!("the tray could not be redrawn: {error}");
        }
    });

    if let Err(error) = drawn {
        eprintln!("the tray could not be redrawn: {error}");
    }
}

/// Applies a model to the tray. Main thread only.
fn draw(app: &AppHandle, tray: &Tray, model: TrayModel) -> tauri::Result<()> {
    let menu = build_menu(app, &model)?;
    tray.icon.set_menu(Some(menu))?;
    tray.icon.set_icon(Some(model.mark.image()))?;
    tray.icon.set_tooltip(Some(&model.tooltip))?;

    *tray
        .model
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = model;

    Ok(())
}

/// The menu, in the order SPEC.md section 33 gives.
fn build_menu<R: Runtime, M: tauri::Manager<R>>(
    app: &M,
    model: &TrayModel,
) -> tauri::Result<Menu<R>> {
    let item = |id: &str, entry: &MenuEntry| {
        MenuItem::with_id(app, id, &entry.label, entry.enabled, None::<&str>)
    };

    let status = item(ids::STATUS, &model.status)?;
    let save_replay = item(ids::SAVE_REPLAY, &model.save_replay)?;
    let add_bookmark = item(ids::ADD_BOOKMARK, &model.add_bookmark)?;
    let record = item(ids::RECORD, &model.record)?;
    let library = item(ids::LIBRARY, &model.library)?;
    let settings = item(ids::SETTINGS, &model.settings)?;
    let exit = item(ids::EXIT, &model.exit)?;

    Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &save_replay,
            &add_bookmark,
            &record,
            &PredefinedMenuItem::separator(app)?,
            &library,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &exit,
        ],
    )
}

/// Somebody clicked something.
///
/// Everything that talks to the recorder is handed to a thread, because this one
/// is drawing the window.
fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let app = app.clone();

    match event.id.as_ref() {
        ids::LIBRARY => open_screen(&app, "/library"),
        ids::SETTINGS => open_screen(&app, "/settings"),
        ids::RECORD => {
            std::thread::spawn(move || record(&app));
        }
        ids::EXIT => {
            std::thread::spawn(move || exit(&app));
        }
        // The status line, the two unbuilt commands and the separators. All are
        // disabled, so Windows does not raise an event for them at all; this
        // arm exists so that an item added without a handler is visible here
        // rather than silently doing nothing.
        other => eprintln!("the tray has no action for `{other}`"),
    }
}

/// Starts or stops a recording, whichever the item was offering.
fn record(app: &AppHandle) {
    let Some(tray) = app.try_state::<Arc<Tray>>() else {
        return;
    };
    let Some(link) = app.try_state::<RecorderLink>() else {
        return;
    };

    let outcome = match tray.model().record_action {
        RecordAction::Start { process_id } => start_recording(&link, process_id),
        RecordAction::Stop => stop_recording(&link),
        // Only reachable if the item was clicked between the model changing and
        // the menu being redrawn.
        RecordAction::Nothing => Err("There is nothing to record.".to_owned()),
    };

    if let Err(message) = outcome {
        report(app, &message);
    }
}

/// Asks the recorder to record the process the menu named.
fn start_recording(link: &RecorderLink, process_id: u32) -> Result<(), String> {
    let command = Command::StartRecording(StartRecording {
        pid: Some(process_id),
        ..StartRecording::default()
    });

    match link.call(&command) {
        // Where the file is, is the one thing worth knowing and the one thing
        // nothing else will tell them.
        Ok(Reply::RecordingStarted { output, .. }) => {
            tracing_line(&format!("recording started: {output}"));
            Ok(())
        }
        Ok(other) => Err(format!("The recorder answered a start with {other:?}.")),
        Err(error) => Err(format!("Recording could not be started. {error}")),
    }
}

/// Asks the recorder to stop whatever is running.
///
/// Deliberately without a recording identifier: the tray is stopping "the
/// recording", and it has no particular one on screen (`docs/ipc.md`).
fn stop_recording(link: &RecorderLink) -> Result<(), String> {
    match link.call(&Command::StopRecording(StopRecording::default())) {
        Ok(Reply::RecordingStopped { summary }) => {
            tracing_line(&format!("recording finished: {}", summary.output));
            Ok(())
        }
        Ok(other) => Err(format!("The recorder answered a stop with {other:?}.")),
        Err(error) => Err(format!("The recording could not be stopped. {error}")),
    }
}

/// Opens the window at a screen.
fn open_screen(app: &AppHandle, path: &str) {
    show_window(app);
    if let Err(error) = app.emit(NAVIGATE_EVENT, path) {
        eprintln!("the window could not be sent to {path}: {error}");
    }
}

/// Stops the recorder and then this process.
///
/// The order matters and is the whole of the third acceptance criterion on
/// issue #50. The recorder is asked to exit **and waited for**, because a window
/// that vanished the instant the user clicked Exit would leave a recorder
/// finalising a file with nothing on screen to say so.
///
/// A refusal is not a failure to hide. If the recording started between the menu
/// being drawn and the item being clicked, the recorder refuses a shutdown that
/// was not told it could end one — and the honest response is to say so and stay
/// open, rather than to send the permission the user was never asked for.
fn exit(app: &AppHandle) {
    let Some(link) = app.try_state::<RecorderLink>() else {
        app.exit(0);
        return;
    };

    let finalise = app
        .try_state::<Arc<Tray>>()
        .is_some_and(|tray| tray.model().exit_finalises_a_recording);

    match link.shut_down_recorder(finalise) {
        Ok(ShutdownOutcome::ShuttingDown { finalising }) => {
            if let Some(active) = &finalising {
                tracing_line(&format!("finishing {} before exiting", active.output));
            }
            if let Some(endpoint) = link.endpoint() {
                if !wait_for_recorder_to_exit(endpoint, EXIT_WAIT) {
                    eprintln!(
                        "the recorder had not finished within {EXIT_WAIT:?}; it is detached and \
                         goes on finishing its file after this window closes"
                    );
                }
            }
        }
        Ok(ShutdownOutcome::NothingRunning) => {}
        Err(RecorderCallError::Refused(refusal)) => {
            report(app, &format!("Clipped did not exit. {}", refusal.message));
            return;
        }
        Err(error) => {
            // The recorder could not be reached to be stopped. Closing the
            // window anyway is right — it owns no recording — but the user is
            // told, because a recorder nobody can reach is one that will still
            // be running after this window has gone.
            eprintln!("the recorder could not be asked to exit: {error}");
        }
    }

    app.exit(0);
}

/// Brings the window back, wherever it was.
pub(crate) fn show_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };

    for outcome in [window.show(), window.unminimize(), window.set_focus()] {
        if let Err(error) = outcome {
            eprintln!("the Clipped window could not be brought to the front: {error}");
        }
    }
}

/// Shows the window and tells it something the user has to read.
///
/// A tray menu has nowhere to report a failure: the menu is gone by the time the
/// command comes back, and a notification-area balloon is not something this
/// application has. So the window comes up carrying the sentence, which is the
/// only surface Clipped has that can hold one (AGENTS.md section 45).
fn report(app: &AppHandle, message: &str) {
    show_window(app);
    if let Err(error) = app.emit(NOTICE_EVENT, message) {
        eprintln!("{message} (and the window could not be told: {error})");
    }
}

/// One line to standard error, which in a debug build is a console.
///
/// The window has no log view yet (issue #101 builds Diagnostics), and a
/// successful action is not something to interrupt anybody with — the tray's own
/// state changes to say it happened.
fn tracing_line(what: &str) {
    eprintln!("clipped: {what}");
}
