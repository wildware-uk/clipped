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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clipped_ipc::supervisor::{wait_for_recorder_to_exit, RecorderCallError, ShutdownOutcome};
use clipped_ipc::{
    AddBookmark, Command, RecorderLink, RecorderLinkState, Reply, SaveReplay, StopRecording,
};
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter as _, Manager as _, Runtime};

use crate::foreground;
use crate::tray_model::{
    could_not_reach_the_recorder, tray_model, MenuEntry, RecordAction, TrayModel,
};

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
/// # A failure here changes what closing the window does
///
/// The tray is where the application lives when its window is closed, so
/// "closing minimises to the tray" is only true when there *is* one. A build
/// that could not add its icon and went on refusing to close its window would
/// leave the user with no way back and no way out — no icon to restore from and
/// no Exit to quit with — which is the opposite of what AGENTS.md section 45
/// asks for.
///
/// So the tray is optional and [`installed`] is what says whether there is one.
/// Without it the window closes the way any window does, the recorder is left
/// running exactly as
/// [ADR 0002](../../../docs/adr/0002-separate-recorder-process.md) requires, and
/// [`crate::startup_notice`] tells the user both of those things.
///
/// # Errors
///
/// Whatever Tauri said, for the caller to put in front of the user.
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

/// Whether this application has a notification-area icon.
///
/// The one fact `main.rs` needs in order to decide what closing the window
/// means, and it is read from the tray itself rather than from a flag beside it:
/// [`install`] manages the [`Tray`] only when it built one, so there is no
/// second answer to keep in step.
pub(crate) fn installed(app: &AppHandle) -> bool {
    app.try_state::<Arc<Tray>>().is_some()
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

/// Every item the menu is built from, in order, with the identifier it carries.
///
/// The single list of what is in the menu. `build_menu` needs Tauri and a
/// desktop; this does not, so it is what
/// `every_identifier_the_menu_is_built_from_has_an_action` walks — an item added
/// below and forgotten in [`action_for`] fails a test rather than reaching a
/// user as a menu entry that visibly does nothing. Separators are not here
/// because they carry no identifier and nothing can click one.
fn menu_entries(model: &TrayModel) -> [(&'static str, &MenuEntry); 7] {
    [
        (ids::STATUS, &model.status),
        (ids::SAVE_REPLAY, &model.save_replay),
        (ids::ADD_BOOKMARK, &model.add_bookmark),
        (ids::RECORD, &model.record),
        (ids::LIBRARY, &model.library),
        (ids::SETTINGS, &model.settings),
        (ids::EXIT, &model.exit),
    ]
}

/// The menu, in the order SPEC.md section 33 gives.
fn build_menu<R: Runtime, M: tauri::Manager<R>>(
    app: &M,
    model: &TrayModel,
) -> tauri::Result<Menu<R>> {
    let item = |(id, entry): (&str, &MenuEntry)| {
        MenuItem::with_id(app, id, &entry.label, entry.enabled, None::<&str>)
    };

    let [status, save_replay, add_bookmark, record, library, settings, exit] = menu_entries(model);
    let status = item(status)?;
    let save_replay = item(save_replay)?;
    let add_bookmark = item(add_bookmark)?;
    let record = item(record)?;
    let library = item(library)?;
    let settings = item(settings)?;
    let exit = item(exit)?;

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

/// What a menu identifier means.
///
/// Split out of [`on_menu_event`] so that the mapping can be tested without a
/// desktop: what a click does is a rule, and a rule nobody can run is a rule
/// nobody can change safely. What is *not* covered here is Tauri delivering the
/// event at all, which needs a real tray and a real click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    /// Raise the window and send it to a screen.
    Navigate(&'static str),
    /// Start or stop a recording, whichever the model is offering.
    Record,
    /// Mark this moment in the recording that is running.
    Bookmark,
    /// Write out the last stretch of the running recording's replay buffer.
    SaveReplay,
    /// Stop the recorder and then this process.
    Exit,
    /// A line of the menu that is not a control.
    ///
    /// The status line, which is a sentence. It is disabled, so Windows raises
    /// no event for it; naming it anyway is what tells an item with no handler
    /// apart from an item with nothing to do.
    Inert,
    /// An identifier this build does not know.
    Unknown,
}

/// What the item with this identifier does.
fn action_for(id: &str) -> MenuAction {
    match id {
        ids::LIBRARY => MenuAction::Navigate("/library"),
        ids::SETTINGS => MenuAction::Navigate("/settings"),
        ids::RECORD => MenuAction::Record,
        ids::ADD_BOOKMARK => MenuAction::Bookmark,
        ids::SAVE_REPLAY => MenuAction::SaveReplay,
        ids::EXIT => MenuAction::Exit,
        ids::STATUS => MenuAction::Inert,
        _ => MenuAction::Unknown,
    }
}

/// Somebody clicked something.
///
/// Everything that talks to the recorder is handed to a thread, because this one
/// is drawing the window.
fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let app = app.clone();

    match action_for(event.id.as_ref()) {
        MenuAction::Navigate(path) => open_screen(&app, path),
        MenuAction::Record => {
            std::thread::spawn(move || record(&app));
        }
        MenuAction::Bookmark => {
            std::thread::spawn(move || bookmark(&app));
        }
        MenuAction::SaveReplay => {
            std::thread::spawn(move || save_replay(&app));
        }
        MenuAction::Exit => {
            std::thread::spawn(move || exit(&app));
        }
        MenuAction::Inert => {}
        // An item was added to the menu and not to `action_for`.
        // `every_menu_identifier_has_an_action` is what stops that reaching a
        // user, and this is what happens if it ever does: a menu item that
        // silently did nothing would be indistinguishable from a click that
        // missed, and a line to standard error is a line to nobody in a release
        // build, which has no console (AGENTS.md section 45).
        MenuAction::Unknown => report(
            &app,
            "Clipped does not know what that menu item does. This is a fault in Clipped rather \
             than anything you did; please report it.",
        ),
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
///
/// The request is [`crate::recording_request`], which is the same one the
/// window's Record button sends — including the replay buffer, so that Save
/// Replay below is live against a recording this menu started (issue #427).
fn start_recording(link: &RecorderLink, process_id: u32) -> Result<(), String> {
    let command = Command::StartRecording(crate::recording_request(process_id));

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

/// Marks this moment in the recording that is running.
///
/// Deliberately without a recording identifier, for the same reason
/// [`stop_recording`] is: the tray is marking "the recording", and it has no
/// particular one on screen. It sends no label and no colour either — the menu
/// item is one click and there is nowhere in a notification-area menu to type,
/// so the bookmark is the bare mark a hotkey would take. Naming it is what the
/// timeline does afterwards ([issue
/// #65](https://github.com/wildware-uk/clipped/issues/65)).
///
/// Feedback is the whole point of answering at all: a mark that is taken
/// silently is indistinguishable from a click that missed. Where it landed is
/// what is worth saying, because it is not where the click was — the recorder
/// stamps a bookmark a few seconds earlier to allow for reaction time
/// (`docs/bookmarks.md`).
fn bookmark(app: &AppHandle) {
    let Some(link) = app.try_state::<RecorderLink>() else {
        return;
    };

    let outcome = match link.call(&Command::AddBookmark(AddBookmark::default())) {
        Ok(Reply::BookmarkAdded { bookmark }) => {
            tracing_line(&format!(
                "bookmarked {:.1}s into the recording",
                bookmark.at_seconds
            ));
            Ok(())
        }
        Ok(other) => Err(format!("The recorder answered a bookmark with {other:?}.")),
        Err(error) => Err(format!("The moment could not be bookmarked. {error}")),
    };

    if let Err(message) = outcome {
        report(app, &message);
    }
}

/// Writes out the last stretch of the running recording's replay buffer.
///
/// Nothing is named, for the reason [`bookmark`] names nothing: the item is one
/// click in a menu with nowhere to type, so it saves *the* recording's buffer,
/// for the length that buffer was started with, into the place the recorder
/// puts a clip. Every one of those is a decision the recorder already has an
/// answer to, and a second answer here would be one the user never chose
/// (`docs/ipc.md` on `save_replay`'s three optional fields).
///
/// # Why the clip's path is said and the bookmark's offset is not
///
/// A bookmark goes *into* the recording somebody is already making. A replay is
/// a **new file**, in a place the user did not name, and a file whose location
/// nobody is told is a file nobody finds. So the outcome is spelled in full,
/// with the length, and with whether the buffer held everything that was asked
/// for — a clip that is shorter than the window is not a failure, it is a
/// buffer that had not been filling for long enough, and saying so is the
/// difference between a working feature and a mysterious one.
///
/// The item is only enabled while a recording is keeping a buffer, so the
/// refusals below are for the click that lands between the menu being drawn and
/// the recording ending. They are reported rather than swallowed: the user
/// asked for a file and there is none.
fn save_replay(app: &AppHandle) {
    let Some(link) = app.try_state::<RecorderLink>() else {
        return;
    };

    let outcome = match link.call(&Command::SaveReplay(SaveReplay::default())) {
        Ok(Reply::ReplaySaved { clip }) => {
            let complete = if clip.complete {
                String::new()
            } else {
                format!(
                    ", {:.1}s short of the buffer's window",
                    clip.shortfall_seconds
                )
            };
            tracing_line(&format!(
                "saved {:.1}s of replay to {}{complete}",
                clip.duration_seconds, clip.path
            ));
            Ok(())
        }
        Ok(other) => Err(format!("The recorder answered a replay with {other:?}.")),
        Err(error) => Err(format!("The replay could not be saved. {error}")),
    };

    if let Err(message) = outcome {
        report(app, &message);
    }
}

/// Opens the window at a screen.
///
/// Reachable from [`crate::notifications`] as well as from the menu: a
/// notification's action is the same "raise the window somewhere useful" the
/// tray performs, and a second implementation of it would be a second answer to
/// how a screen is opened (AGENTS.md section 55).
pub(crate) fn open_screen(app: &AppHandle, path: &str) {
    show_window(app);
    if let Err(error) = app.emit(NAVIGATE_EVENT, path) {
        eprintln!("the window could not be sent to {path}: {error}");
    }
}

/// Whether Exit has already said that it could not reach the recorder.
///
/// The second click is the user answering it. See [`exit`].
static UNREACHABLE_ALREADY_REPORTED: AtomicBool = AtomicBool::new(false);

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
///
/// # When the recorder cannot be reached at all
///
/// The dangerous case, and the reason this is not simply "close anyway". Exit is
/// the only path that stops the recorder; a shutdown that could not be delivered
/// therefore leaves a recorder running, quite possibly recording, with the one
/// thing that could have said so about to disappear. Saying it to standard error
/// says it to nobody — a release build has no console.
///
/// So the first Exit does not exit. It raises the window carrying
/// [`could_not_reach_the_recorder`], which names what was being recorded and
/// where the file is, and says that choosing Exit again will close the window
/// regardless. The second one does exactly that.
///
/// Two clicks rather than one because both of the simple answers are wrong:
/// closing silently is the recording-safety failure AGENTS.md section 17 puts
/// above almost everything, and refusing for ever is a user trapped in an
/// application that will not close (section 45). This way nothing is lost
/// quietly and nothing is inescapable. It also clears itself in the ordinary
/// case — a recorder that has genuinely gone is not *unreachable*, it is not
/// listening, which is [`ShutdownOutcome::NothingRunning`] and exits first time.
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
                    tracing_line(&format!(
                        "the recorder had not finished within {EXIT_WAIT:?}; it is detached and \
                         goes on finishing its file after this window closes"
                    ));
                }
            }
        }
        Ok(ShutdownOutcome::NothingRunning) => {}
        Err(RecorderCallError::Refused(refusal)) => {
            report(app, &format!("Clipped did not exit. {}", refusal.message));
            return;
        }
        // This link never had a recorder to talk to: no endpoint could be named
        // or no executable found, and it has started nothing. There is no
        // recording anywhere to be left behind, so there is nothing to warn
        // about and no reason to make the user click twice.
        Err(RecorderCallError::NoRecorderConfigured) => {}
        Err(error) => {
            if !UNREACHABLE_ALREADY_REPORTED.swap(true, Ordering::SeqCst) {
                report(
                    app,
                    &could_not_reach_the_recorder(&link.state(), &error.to_string()),
                );
                return;
            }
            // Said once already, and the user has chosen Exit again.
            tracing_line(&format!(
                "closing anyway; the recorder could not be asked to exit: {error}"
            ));
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
/// command comes back. So the window comes up carrying the sentence, which is
/// the surface Clipped has that can hold one (AGENTS.md section 45).
///
/// [`crate::notifications`] uses it too, for the same reason and in two places:
/// when a toast could not be shown at all, and when a notification's action
/// resolves to "open Clipped" because there was nothing more specific to offer.
pub(crate) fn report(app: &AppHandle, message: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_identifier_the_menu_is_built_from_has_an_action() {
        // The fallthrough arm, as a test rather than as a reading. Walked from
        // the list `build_menu` itself walks, so this cannot go on agreeing with
        // a menu that has changed.
        let model = crate::tray_model::tray_model(&RecorderLinkState::Connecting, None);

        for (id, entry) in menu_entries(&model) {
            assert_ne!(
                action_for(id),
                MenuAction::Unknown,
                "`{id}` (`{}`) is in the menu and nothing here knows what it does",
                entry.label
            );
        }
    }

    #[test]
    fn each_item_does_the_one_thing_its_label_offers() {
        // Which is not obvious from the identifiers: Open Library and Settings
        // differ only by the path they carry, and Start and Stop Recording are
        // deliberately one identifier whose meaning is the model's.
        assert_eq!(
            action_for(ids::LIBRARY),
            MenuAction::Navigate("/library"),
            "Open Library has to reach the library and not some other screen"
        );
        assert_eq!(action_for(ids::SETTINGS), MenuAction::Navigate("/settings"));
        assert_eq!(action_for(ids::RECORD), MenuAction::Record);
        assert_eq!(action_for(ids::EXIT), MenuAction::Exit);
        // Issue #64 turned this from a line that never did anything into one
        // that sends a command. An item left `Inert` after the recorder gained
        // the command would be enabled in the menu and silently do nothing when
        // clicked, which is the failure AGENTS.md section 27 names.
        assert_eq!(action_for(ids::ADD_BOOKMARK), MenuAction::Bookmark);
        // And issue #427 is the same thing happening to Save Replay: the
        // recorder has had the command since issue #38, and until now the item
        // was `Inert` — so a recording started with a buffer drew an enabled
        // control that did nothing at all when clicked.
        assert_eq!(action_for(ids::SAVE_REPLAY), MenuAction::SaveReplay);
    }

    #[test]
    fn the_line_that_is_not_a_control_is_named_rather_than_left_over() {
        // The status line is a sentence, and it is disabled, so Windows raises
        // no event for it. Naming it anyway is what makes `Unknown` mean
        // "somebody added an item and forgot" rather than "the line that never
        // does anything".
        assert_eq!(action_for(ids::STATUS), MenuAction::Inert);
    }

    #[test]
    fn the_tray_starts_a_recording_it_will_be_able_to_save_a_replay_from() {
        // The other half of issue #427, and the half no model test can see. The
        // three refusals below are a *function of the recording*, so a Start
        // Recording that did not ask for a replay buffer leaves Save Replay
        // permanently disabled with "this recording is not keeping a replay
        // buffer" — an honest label on a control nobody can ever reach, which
        // is what this menu drew for months while every test in this file
        // passed.
        //
        // Driven over a real pipe rather than by reading the request back,
        // because "the tray builds the right struct" is not the claim: the
        // claim is that the recorder is asked for a buffer when somebody clicks
        // Start Recording in this menu.
        let recorder = crate::tests::FakeRecorder::listening(
            "tray-record-start",
            crate::tests::AskedRecorder::default(),
        );
        let app = recorder.window();

        start_recording(app.state::<RecorderLink>().inner(), 4_242)
            .expect("the recorder answered the start");

        assert_eq!(
            recorder.handler.asked(),
            vec![Command::StartRecording(clipped_ipc::StartRecording {
                pid: Some(4_242),
                replay: true,
                ..clipped_ipc::StartRecording::default()
            })],
            "Start Recording has to ask for the process the menu named and for a replay buffer, \
             or Save Replay below it can never be enabled"
        );
    }

    #[test]
    fn every_enabled_item_the_model_can_produce_sends_something() {
        // The property behind the two tests above, and the one that catches the
        // next item added: an item the tray is willing to *enable* must have an
        // action that is not `Inert`. Walked over every link state, because
        // which items are enabled is exactly what the state decides.
        let recording = RecorderLinkState::Attached {
            recorder_process_id: 7,
            features: clipped_ipc::features::ALL
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            status: clipped_ipc::RecorderStatus::Recording(clipped_ipc::ActiveRecording {
                recording_id: "r-1".to_owned(),
                output: r"D:\clips\session.mkv".to_owned(),
                target: "process `cs2.exe`".to_owned(),
                elapsed_ms: 1_000,
                replay_seconds: Some(300),
                session: None,
            }),
        };

        for link in [RecorderLinkState::Connecting, recording] {
            let model = crate::tray_model::tray_model(&link, None);
            for (id, entry) in menu_entries(&model) {
                if entry.enabled {
                    assert_ne!(
                        action_for(id),
                        MenuAction::Inert,
                        "`{id}` (`{}`) can be clicked and does nothing",
                        entry.label
                    );
                }
            }
        }
    }

    #[test]
    fn an_identifier_the_menu_never_built_is_not_quietly_ignored() {
        assert_eq!(action_for("no-such-item"), MenuAction::Unknown);
        assert_eq!(action_for(""), MenuAction::Unknown);
    }
}
