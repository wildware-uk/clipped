//! The Clipped desktop application.
//!
//! This binary is a window and a supervisor, and nothing else. It opens a
//! WebView2 host, serves the built React interface into it, makes sure a
//! recorder is running, and gets out of the way. It owns no capture, no encoding
//! and no session state: the recorder (`apps/recorder`) is a separate process
//! for the reason `docs/adr/0002-separate-recorder-process.md` gives — closing or
//! crashing this window must not interrupt a recording.
//!
//! # What it does at startup, in order
//!
//! 1. **Claims the single-instance name.** A second launch finds it taken, says
//!    so and exits without touching the recorder. Two windows would be two
//!    supervisors, each with a restart budget of its own.
//! 2. **Starts a [`RecorderLink`].** That attaches to a recorder if one is
//!    listening and starts one — detached, so it outlives this process — if none
//!    is. Everything about that decision, including the restart policy, lives in
//!    `clipped_ipc::supervisor` and `docs/adr/0006-recorder-lifetime-and-supervision.md`.
//! 3. **Puts Clipped in the notification area** and follows the link's state
//!    with it. The tray is the first part of this interface that *acts* on the
//!    recorder rather than only watching it (SPEC.md section 33, issue #50);
//!    [`tray`] and [`tray_model`] are where that lives.
//! 4. **Opens the window**, which reads the link's state through the
//!    `recorder_link_state` command and follows it through the
//!    `recorder-link` event.
//!
//! # What the window can ask this process to do
//!
//! `#[tauri::command]`s, and no other way in. Two are about this process —
//! [`recorder_link_state`] and [`startup_notice`]; two read the recording
//! library through the recorder — [`library_sessions`] and [`library_games`]
//! (issue #301); four are the record control — [`record_target`] says what
//! would be recorded, [`recorder_status`] asks the recorder what it is doing,
//! and [`start_recording`] and [`stop_recording`] do the two things the button
//! does (issue #389); three act on a recording the library listed —
//! [`export_recording`], [`open_recording`] and [`reveal_recording`]
//! (issue #399); and [`open_playback`] is what puts a recording on the screen
//! (issue #304).
//!
//! All but `record_target`, `open_recording` and `reveal_recording` are a round
//! trip over the control protocol, and each returns either the recorder's own
//! answer or a [`RecorderProblem`] carrying the recorder's own words. **The
//! window keeps no recording state of its own**: it asks [`recorder_status`]
//! and draws the answer, so a recorder that has died stops being reported as
//! recording rather than going on being claimed (`docs/desktop-ui.md`,
//! AGENTS.md section 27).
//!
//! # Why opening and revealing are commands rather than a permission
//!
//! `capabilities/default.json` is the whole of the window's privilege, and
//! playing a recording adds **nothing** to it: the `clip` URI scheme is
//! registered by this process rather than granted to the interface, and it
//! serves only what the recorder has already answered `open_playback` with
//! ([`playback`], issue #304).
//!
//! Two lines of it are dialogs: `dialog:allow-save`, which the export added so
//! that the interface can ask the operating system where an export should go,
//! and `dialog:allow-open`, which the settings screen added so that it can ask
//! which folder to record into (issue #51). Both answer with a path a person
//! chose and neither reaches a file: `tauri-plugin-fs` is present as a
//! dependency of the dialog plugin and is **not registered**, so none of its
//! commands exists to be permitted.
//!
//! Opening a recording and revealing it in Explorer could have been the same
//! shape — `tauri-plugin-opener` has commands the interface can call — and are
//! deliberately not. The permission that would allow it is
//! `opener:allow-open-path` over a **scope**, and there is no scope that would
//! work: a recording lives wherever the recorder's output directory points,
//! which is a setting, so the scope would have to be every path on the machine.
//! Granting the webview "open anything with its default application" to open
//! one MKV is the opposite of what that file's own comment asks for. Two
//! commands here mean the window can ask *this process* to open a file, and
//! this process is the one that decides.
//!
//! # Closing, and quitting
//!
//! They are different things now, which is what SPEC.md section 33 asks for.
//! Closing the window hides it: the recorder is a separate process and goes on
//! recording, and the tray is where the application still is. Quitting is the
//! tray's Exit, and it is the **only** path that stops the recorder — over the
//! protocol, so that a recording is finished and its file closed rather than
//! abandoned ([issue #220](https://github.com/wildware-uk/clipped/issues/220),
//! AGENTS.md section 17).
//!
//! Nothing else here stops the recorder, on any path, including the window
//! crashing. That is still the decision it always was.
//!
//! **All of that depends on there being a tray.** A build that could not add
//! its icon has no Exit and nothing to restore from, so refusing to close the
//! window would leave the application with no way back and no way out; without
//! one, closing the window closes it, and [`no_tray_notice`] is what the user
//! is told about that before they try.
//!
//! # Why this crate links `clipped-ipc` and nothing else
//!
//! A webview cannot open a named pipe, so the Tauri host is the protocol's
//! client. `clipped-ipc` is the crate both ends are meant to use — it depends on
//! no other crate in this workspace, so linking it brings in the messages and
//! the transport and not the recording engine.
//! `tests/integration/tests/workspace_layering.rs` enforces exactly that: it is
//! the only member this manifest may name, and it must itself name none.

// A release build must not open a console window behind the application. The
// debug build keeps one, because that is where `tracing` output and a panic
// backtrace are read from during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod foreground;
mod notification_policy;
mod notifications;
mod playback;
mod this_application;
mod toast;
mod tray;
mod tray_icon;
mod tray_model;

use std::path::PathBuf;

use clipped_ipc::supervisor::{claim_instance, InstanceClaim};
use clipped_ipc::{
    Endpoint, PeerIdentity, RecorderLink, RecorderLinkEvent, RecorderLinkState, SupervisorSettings,
};
use tauri::{Emitter as _, Manager as _, WindowEvent};

use crate::notification_policy::NotificationPreferences;

/// The single-instance name this application claims, before the session
/// namespace is applied.
///
/// Windows scopes `Local\` names to the sign-in session, so two people signed in
/// at once each get their own window and their own recorder — the same reasoning
/// that puts a session discriminator in the endpoint name.
const INSTANCE_NAME: &str = "clipped-desktop";

/// The recorder executable, expected beside this one.
const RECORDER_EXECUTABLE: &str = "clipped-recorder.exe";

/// An environment variable naming the recorder to start instead.
///
/// For running the window against a recorder built somewhere else — a `cargo
/// build` target directory rather than an installation — without having to copy
/// a binary about. It names an executable and nothing else; there is no way to
/// pass arguments through it.
const RECORDER_OVERRIDE: &str = "CLIPPED_RECORDER_EXE";

/// The event the window listens for.
const LINK_EVENT: &str = "recorder-link";

fn main() {
    // Before anything else. A second launch that has already started a recorder
    // has failed the requirement whatever it does next, so this is the first
    // statement in the program.
    match claim_instance(INSTANCE_NAME) {
        Ok(InstanceClaim::Claimed(claim)) => {
            // Held for the life of the process. It is released when this
            // process ends, however it ends.
            std::mem::forget(claim);
        }
        Ok(InstanceClaim::AlreadyRunning) => {
            // A release build has no console, so this line is for a developer
            // rather than for a user: to somebody double-clicking the icon, a
            // second launch appears to do nothing at all. Raising the window
            // that is already open needs a channel to the instance holding the
            // name, and is
            // [issue #225](https://github.com/wildware-uk/clipped/issues/225).
            // What matters here is only that this launch starts no recorder.
            eprintln!(
                "Clipped is already running in this session. Bringing that window to the front \
                 is issue #225; for now, find it in the taskbar."
            );
            return;
        }
        Err(error) => {
            eprintln!("Clipped could not check whether it was already running: {error}");
            return;
        }
    }

    let (link, events) = match supervisor_settings() {
        Ok(settings) => RecorderLink::start(settings),
        Err(reason) => {
            // Not fatal: a window that cannot find the recorder is still worth
            // opening, because saying so is the whole of what it can usefully
            // do (AGENTS.md section 27). A dead link that reports the reason is
            // the honest shape of that.
            RecorderLink::started_unavailable(reason)
        }
    };

    tauri::Builder::default()
        .manage(link)
        // The interface calls neither of these directly. `opener` is reached
        // from `open_recording` and `reveal_recording` below, and `dialog` is
        // reached from the interface through `dialog:allow-save` and
        // `dialog:allow-open` and nothing else — see the header, and
        // `capabilities/default.json`.
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            recorder_link_state,
            startup_notice,
            library_sessions,
            library_events,
            library_games,
            library_trash,
            restore_from_trash,
            empty_trash,
            set_favourite,
            set_lock,
            record_target,
            recorder_status,
            recorder_hotkeys,
            recorder_settings,
            apply_recorder_settings,
            audio_devices,
            microphone_level,
            start_at_login,
            set_start_at_login,
            start_recording,
            stop_recording,
            export_recording,
            open_playback,
            open_recording,
            reveal_recording
        ])
        // The one thing this window may receive bytes over, and it serves
        // nothing until the recorder has answered `open_playback` for a
        // recording (`playback`). Asynchronous because reading four mebibytes
        // off a disk may not happen on the thread drawing the window, and
        // because a media element has several of these in flight at once.
        .register_asynchronous_uri_scheme_protocol(
            playback::SCHEME,
            |_context, request, responder| {
                std::thread::Builder::new()
                    .name("clipped-playback".to_owned())
                    .spawn(move || responder.respond(playback::handle(&request)))
                    .map_or_else(
                        |error| eprintln!("a recording could not be served to the window: {error}"),
                        |_| (),
                    );
            },
        )
        .setup(move |app| {
            let handle = app.handle().clone();

            // On the main thread, and before the tray: an out-of-context window
            // event hook delivers through the hooking thread's message queue,
            // which for a Tauri application is this one. Doing it first means
            // the tray's first menu already knows what to offer to record.
            foreground::follow_the_foreground_window();

            // Which notifications the user wants, shared by the thread that
            // decides and the `apply_settings` command that changes them. It
            // starts as "everything on" and is replaced the moment the link
            // attaches and the recorder can be asked (`notifications::refresh`,
            // issue #252).
            let notification_preferences = NotificationPreferences::default();
            app.manage(notification_preferences.clone());

            // Before the tray, because both report a startup failure through the
            // same one-sentence notice and a missing tray is the more important
            // of the two: it changes what closing the window does.
            let mut notifier = notifications::install(
                &handle,
                &app.state::<RecorderLink>(),
                notification_preferences,
            );

            if let Err(error) = tray::install(&handle, &app.state::<RecorderLink>()) {
                // Not fatal. A window that shows the recorder's state is still
                // worth having, and saying what is missing beats exiting
                // (AGENTS.md section 16) — but *only* if what is missing is
                // actually said, and if closing the window still works without
                // it. Both of those are below: `startup_notice` is read by the
                // window when it mounts, and `on_window_event` asks whether
                // there is a tray before it refuses to close.
                set_startup_notice(&no_tray_notice(&error.to_string()));
            }

            // A thread of its own because the link's channel blocks, and the
            // one thing that may never block is the thread running the window.
            std::thread::Builder::new()
                .name("clipped-recorder-link-events".to_owned())
                .spawn(move || {
                    while let Ok(event) = events.recv() {
                        // The tray first, because it is on screen whether or
                        // not the window is, and it is drawn from the state
                        // rather than from the event.
                        if let RecorderLinkEvent::State(state) = &event {
                            tray::refresh(&handle, state);
                        }

                        // Then whatever is worth interrupting the user for,
                        // which is a short list and deliberately so
                        // (`notification_policy`, issue #110). This thread owns
                        // the policy because it is the only thing that consults
                        // it; showing a toast is a WinRT call and a tenth of a
                        // second at worst, and nothing here is drawing a window.
                        notifier.consider(&handle, &event);

                        if let Err(error) = handle.emit(LINK_EVENT, &event) {
                            // The window has gone; the recorder has not, and
                            // this thread has nothing left to do.
                            eprintln!("the recorder link event could not be delivered: {error}");
                            break;
                        }
                    }
                })?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing the window minimises to the tray (SPEC.md section
                // 33). Quitting is the tray's Exit, and it is deliberately the
                // only thing that stops the recorder: a window closed by
                // accident must never end a recording.
                //
                // **Only when there is a tray to minimise to.** Refusing to
                // close with no icon to restore from and no Exit to quit with
                // would strand the application with no way back and no way out,
                // which is the opposite of the useful action AGENTS.md section
                // 45 asks for. Without one the window closes the way any window
                // does and the recorder is left running, exactly as ADR 0002
                // requires of every path but Exit; `no_tray_notice` is what the
                // user was told about that when the window opened.
                if !tray::installed(window.app_handle()) {
                    return;
                }

                api.prevent_close();
                if let Err(error) = window.hide() {
                    eprintln!("the Clipped window could not be hidden: {error}");
                }
            }
        })
        .run(tauri::generate_context!())
        // There is no interface to report this in: the failure is that the
        // window could not be created, which on Windows almost always means
        // the WebView2 runtime is missing. Panicking with that sentence is more
        // use than a silent exit code.
        .expect("failed to open the Clipped window; check that the WebView2 runtime is installed");
}

/// Where the link stands, for the window to draw.
///
/// The window asks once when it mounts and follows the `recorder-link` event
/// afterwards. Both carry the whole state rather than a delta, so a window that
/// missed an event recovers on the next one.
#[tauri::command]
fn recorder_link_state(link: tauri::State<'_, RecorderLink>) -> RecorderLinkState {
    link.state()
}

/// A request to the recorder that did not produce the reply it asked for.
///
/// The window cannot open `library.db`, may not link `clipped-library` and
/// cannot open a named pipe from a webview, so every question it has for the
/// recorder is a round trip
/// ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md),
/// [issue #301](https://github.com/wildware-uk/clipped/issues/301),
/// [issue #389](https://github.com/wildware-uk/clipped/issues/389)). Four
/// different things can stop one, and the screen has to tell them apart because
/// the useful action is different in each case (AGENTS.md section 45):
///
/// - the recorder **refused**, and [`Self::code`] is its own protocol code —
///   `library_unavailable` for an index that could not be read,
///   `invalid_parameters` for a search that does not parse, `target_not_found`
///   for a window that has since closed, `already_recording` for a second
///   start, `unknown_command` for a recorder built before a command existed;
/// - there is **no recorder to ask**, or it went away mid-question;
/// - this build was started with no recorder configured at all;
/// - the recorder answered a different command's reply, which is a bug rather
///   than a refusal.
///
/// None of them is an empty library, and none of them is a recording that
/// started; none may be drawn as one (AGENTS.md section 27).
///
/// **The sentence is the recorder's own** wherever there is one. The window
/// invents no wording for a refusal it did not make.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct RecorderProblem {
    /// The recorder's protocol code, or one of the three below for a question
    /// that never reached it.
    ///
    /// Those are outside the protocol's own vocabulary deliberately: a code
    /// the recorder could also send would leave the window unable to tell "the
    /// recorder said no" from "there was no recorder".
    code: String,
    /// One sentence for a person, which is the part that is always shown.
    message: String,
}

/// The code a question that never reached a recorder carries.
const RECORDER_UNREACHABLE: &str = "recorder_unreachable";

/// The code a build with no recorder configured carries.
const NO_RECORDER_CONFIGURED: &str = "no_recorder_configured";

/// The code a reply that answered a different question carries.
const UNEXPECTED_REPLY: &str = "unexpected_reply";

impl From<clipped_ipc::RecorderCallError> for RecorderProblem {
    fn from(error: clipped_ipc::RecorderCallError) -> Self {
        match error {
            clipped_ipc::RecorderCallError::Refused(refusal) => Self {
                code: refusal.code.as_str().to_owned(),
                message: refusal.message,
            },
            clipped_ipc::RecorderCallError::Unreachable(error) => Self {
                code: RECORDER_UNREACHABLE.to_owned(),
                message: format!("the recorder could not be reached: {error}"),
            },
            clipped_ipc::RecorderCallError::Unexpected(what) => Self {
                code: UNEXPECTED_REPLY.to_owned(),
                message: what,
            },
            clipped_ipc::RecorderCallError::NoRecorderConfigured => Self {
                code: NO_RECORDER_CONFIGURED.to_owned(),
                message: "this build has no recorder to ask, so nothing can be read or recorded"
                    .to_owned(),
            },
        }
    }
}

/// A reply that was not the one the command asked for.
///
/// It cannot happen against a recorder that speaks this protocol version, and
/// is reported rather than ignored: a window that quietly drew an empty library
/// because it got a `pong` would be hiding a real fault (AGENTS.md section 15).
fn wrong_reply(command: &str) -> RecorderProblem {
    RecorderProblem {
        code: UNEXPECTED_REPLY.to_owned(),
        message: format!("the recorder answered `{command}` with something else"),
    }
}

/// One page of the recording library.
///
/// `async` so that Tauri runs it on the async runtime rather than on the thread
/// drawing the window: [`RecorderLink::call`] opens a pipe and waits for an
/// answer, and a window that froze while a library page was fetched would be the
/// exact failure ADR 0002's two processes exist to prevent.
#[tauri::command(async)]
fn library_sessions(
    link: tauri::State<'_, RecorderLink>,
    limit: Option<u32>,
    after: Option<String>,
    query: Option<String>,
) -> Result<clipped_ipc::LibrarySessionPage, RecorderProblem> {
    let reply = link.call(&clipped_ipc::Command::LibrarySessions(
        clipped_ipc::LibrarySessions {
            limit,
            after,
            query,
        },
    ))?;

    match reply {
        clipped_ipc::Reply::LibrarySessions { page } => Ok(page),
        _ => Err(wrong_reply("library_sessions")),
    }
}

/// What is waiting in the trash (SPEC.md section 19, issue #450).
///
/// The whole trash rather than a page: it is what somebody deleted and has not
/// emptied, bounded by the retention period rather than by the size of the
/// library.
///
/// `async` for the reason [`library_sessions`] is.
#[tauri::command(async)]
fn library_trash(
    link: tauri::State<'_, RecorderLink>,
) -> Result<clipped_ipc::TrashListing, RecorderProblem> {
    match link.call(&clipped_ipc::Command::LibraryTrash(
        clipped_ipc::LibraryTrash {},
    ))? {
        clipped_ipc::Reply::LibraryTrash { trash } => Ok(trash),
        _ => Err(wrong_reply("library_trash")),
    }
}

/// Puts one thing back where it was (issue #450).
#[tauri::command(async)]
fn restore_from_trash(
    link: tauri::State<'_, RecorderLink>,
    kind: String,
    id: i64,
) -> Result<clipped_ipc::RestoredItem, RecorderProblem> {
    match link.call(&clipped_ipc::Command::RestoreFromTrash(
        clipped_ipc::RestoreFromTrash { kind, id },
    ))? {
        clipped_ipc::Reply::Restored { restored } => Ok(restored),
        _ => Err(wrong_reply("restore_from_trash")),
    }
}

/// Destroys everything in the trash, confirmed against the listing shown.
///
/// Both numbers are the listing the user was looking at. The recorder refuses if
/// the trash has gained anything since, because the alternative is deleting
/// something they never saw — which is why this takes two numbers rather than a
/// boolean (issue #450).
#[tauri::command(async)]
fn empty_trash(
    link: tauri::State<'_, RecorderLink>,
    items: u64,
    bytes: u64,
) -> Result<clipped_ipc::TrashEmptied, RecorderProblem> {
    match link.call(&clipped_ipc::Command::EmptyTrash(clipped_ipc::EmptyTrash {
        items,
        bytes,
    }))? {
        clipped_ipc::Reply::TrashEmptied { emptied } => Ok(emptied),
        _ => Err(wrong_reply("empty_trash")),
    }
}

/// Marks one thing a favourite, or clears the mark (issue #58).
///
/// The target takes two fields because the schema does: a sitting is addressed
/// by the identifier the recorder generated, which is text, and a recording or
/// clip by the integer key the index gave it. `kind` says which to read.
///
/// `favourite` is the state to be in rather than a toggle, so two windows open
/// on one library cannot disagree about which way a star points.
#[tauri::command(async)]
fn set_favourite(
    link: tauri::State<'_, RecorderLink>,
    kind: String,
    session_id: String,
    id: i64,
    favourite: bool,
) -> Result<clipped_ipc::FavouriteMark, RecorderProblem> {
    match link.call(&clipped_ipc::Command::SetFavourite(
        clipped_ipc::SetFavourite {
            kind,
            session_id,
            id,
            favourite,
        },
    ))? {
        clipped_ipc::Reply::Favourited { mark } => Ok(mark),
        _ => Err(wrong_reply("set_favourite")),
    }
}

/// Locks one thing against automatic cleanup, or unlocks it (issue #472).
///
/// A lock protects against automatic cleanup and nothing else: a locked
/// recording is deleted by a manual delete exactly as an unlocked one is.
#[tauri::command(async)]
fn set_lock(
    link: tauri::State<'_, RecorderLink>,
    kind: String,
    session_id: String,
    id: i64,
    locked: bool,
) -> Result<clipped_ipc::LockMark, RecorderProblem> {
    match link.call(&clipped_ipc::Command::SetLock(clipped_ipc::SetLock {
        kind,
        session_id,
        id,
        locked,
    }))? {
        clipped_ipc::Reply::Locked { lock } => Ok(lock),
        _ => Err(wrong_reply("set_lock")),
    }
}

/// What the library holds per game (SPEC.md section 17).
#[tauri::command(async)]
fn library_games(
    link: tauri::State<'_, RecorderLink>,
) -> Result<Vec<clipped_ipc::LibraryGame>, RecorderProblem> {
    match link.call(&clipped_ipc::Command::LibraryGames)? {
        clipped_ipc::Reply::LibraryGames { games } => Ok(games),
        _ => Err(wrong_reply("library_games")),
    }
}

/// The marks on one recording's timeline.
///
/// Placed in that recording's file by the recorder, because placing needs the
/// recording's span on its session's timeline and this process has no way to
/// know it — the window is given the number it draws at rather than one it
/// would have to work out (`docs/ipc.md`, `library_events`).
///
/// `async` for the reason [`library_sessions`] is: the call opens a pipe and
/// waits, and a window that froze while a lane was fetched is what ADR 0002's
/// two processes exist to prevent.
#[tauri::command(async)]
fn library_events(
    link: tauri::State<'_, RecorderLink>,
    recording: String,
) -> Result<clipped_ipc::LibraryEventLane, RecorderProblem> {
    let reply = link.call(&clipped_ipc::Command::LibraryEvents(
        clipped_ipc::LibraryEvents { recording },
    ))?;

    match reply {
        clipped_ipc::Reply::LibraryEvents { lane } => Ok(lane),
        _ => Err(wrong_reply("library_events")),
    }
}

/// What the record control would record, if it were pressed now.
///
/// The application the user was last in, which is the same answer the tray's
/// Start Recording gives and comes from the same place ([`foreground`]). A
/// window has a screen to name it on, so it does — a button reading "Record"
/// with no subject records something the user did not choose.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct RecordTarget {
    /// The process `start_recording` would be given.
    process_id: u32,
    /// The executable's file name, such as `cs2.exe`, for the button to name.
    ///
    /// The executable rather than the window title, for the reason
    /// [`foreground::ForegroundWindow`] gives: a title is user content, and the
    /// surest way to put somebody's document name on screen and into a
    /// screenshot of a bug report (AGENTS.md section 13).
    process_name: String,
}

/// What the record control would record, or [`None`] if nothing would.
///
/// [`None`] is a real state rather than a failure: a machine just signed into
/// has had nothing in front of it, and a build whose foreground hook could not
/// be installed never will. The window says so and disables the control, which
/// is what the tray's menu item does with the same answer.
///
/// Not `async`: it reads a value this process already holds and opens no pipe.
#[tauri::command]
fn record_target() -> Option<RecordTarget> {
    foreground::last_seen().map(|window| RecordTarget {
        process_id: window.process_id,
        process_name: window.process_name,
    })
}

/// What the recorder is doing, asked of the recorder.
///
/// **This is where the window's recording state comes from**, and the reason it
/// is a command of its own rather than a field the window keeps. The link's
/// state carries a status too, but the recorder publishes `status_changed` when
/// a recording starts and when it ends and at no point between
/// (`apps/recorder/src/serve.rs`), so the `elapsed_ms` in it is the elapsed time
/// at the moment the recording started and never moves. Counting up from it in
/// the window would be a figure nobody measured, and — worse — a window that had
/// decided it was recording would go on saying so after the recorder died
/// (AGENTS.md section 27, issue #389).
///
/// So the window asks, repeatedly, and draws the answer. A recorder that has
/// stopped, crashed or refused stops answering `recording`, and the window
/// follows it down within one interval.
#[tauri::command(async)]
fn recorder_status(
    link: tauri::State<'_, RecorderLink>,
) -> Result<clipped_ipc::RecorderStatus, RecorderProblem> {
    match link.call(&clipped_ipc::Command::GetStatus)? {
        clipped_ipc::Reply::Status { status } => Ok(status),
        _ => Err(wrong_reply("get_status")),
    }
}

/// Where every global hotkey stands, asked of the recorder.
///
/// The window registers none of them. `RegisterHotKey` gives a combination to
/// exactly one process and that process is the recorder, because it is the one
/// that outlives this window and the one that can act on a press
/// ([ADR 0009](../../../../docs/adr/0009-the-recorder-registers-global-hotkeys.md)).
/// So this window is not where a conflict is discovered — the recorder found out
/// when it started, which may have been days ago — and asking is the only way to
/// see it.
///
/// Asking matters more here than it does for most commands. A combination
/// another application owns is a hotkey that does nothing, and without this it
/// would exist only as a line in the recorder's log; interrupting somebody with
/// it is [issue #417](https://github.com/wildware-uk/clipped/issues/417), and
/// this is what that would read.
///
/// `async` for the reason [`recorder_status`] is: it opens a pipe and waits for
/// an answer, and the thread drawing the window may not.
#[tauri::command(async)]
fn recorder_hotkeys(
    link: tauri::State<'_, RecorderLink>,
) -> Result<Vec<clipped_ipc::HotkeyBinding>, RecorderProblem> {
    match link.call(&clipped_ipc::Command::GetHotkeys)? {
        clipped_ipc::Reply::Hotkeys { hotkeys } => Ok(hotkeys),
        _ => Err(wrong_reply("get_hotkeys")),
    }
}

/// Every setting, what it resolves to, and whether anything reads it.
///
/// Asked of the recorder rather than read from `settings.json`, because the
/// recorder owns that file — its versioning, its migrations and its validation
/// live in `clipped_session::config`, which this window may not link
/// (`tests/integration/tests/workspace_layering.rs`). A window that read the
/// file itself would be a second implementation of all three, against the file
/// somebody's settings live in (AGENTS.md section 55, issue #252).
///
/// `async` for the reason [`recorder_status`] is: it opens a pipe and waits for
/// an answer, and the thread drawing the window may not.
#[tauri::command(async)]
fn recorder_settings(
    link: tauri::State<'_, RecorderLink>,
    notifications: tauri::State<'_, NotificationPreferences>,
) -> Result<clipped_ipc::SettingsView, RecorderProblem> {
    match link.call(&clipped_ipc::Command::GetSettings)? {
        clipped_ipc::Reply::Settings { settings } => {
            notifications.adopt(&settings);
            Ok(settings)
        }
        _ => Err(wrong_reply("get_settings")),
    }
}

/// Changes settings, and answers with the settings as they now stand.
///
/// The answer is the recorder's, not this window's idea of what it sent: a value
/// the recorder refused, or one another window changed a moment earlier, would
/// otherwise be drawn as saved. `null` clears a setting, which is Reset.
///
/// A refusal carries the recorder's own sentence, naming the setting, the value
/// and what would have been accepted — the same sentence somebody hand-editing
/// the file would get, because it is the same validation (AGENTS.md section 45).
///
/// Four of those settings are this process's to act on rather than the
/// recorder's — the notification switches — so what comes back is handed to
/// [`NotificationPreferences`] on the way out. Without that, switching a
/// category off would save perfectly and go on notifying until Clipped was
/// restarted, which is the control that silently does nothing of AGENTS.md
/// section 27 (issue #252).
#[tauri::command(async)]
fn apply_recorder_settings(
    link: tauri::State<'_, RecorderLink>,
    notifications: tauri::State<'_, NotificationPreferences>,
    values: std::collections::BTreeMap<String, Option<String>>,
) -> Result<clipped_ipc::SettingsView, RecorderProblem> {
    match link.call(&clipped_ipc::Command::ApplySettings(
        clipped_ipc::ApplySettings { values },
    ))? {
        clipped_ipc::Reply::Settings { settings } => {
            notifications.adopt(&settings);
            Ok(settings)
        }
        _ => Err(wrong_reply("apply_settings")),
    }
}

/// The microphones this machine has, asked of the recorder.
///
/// The window cannot enumerate them: `clipped-audio` is in the recorder's
/// process, and a name in the settings file is matched against the endpoints
/// present when a recording starts — so the list the user chooses from has to be
/// the recorder's own or it would be a list of devices that may not be there
/// (issue #308).
#[tauri::command(async)]
fn audio_devices(
    link: tauri::State<'_, RecorderLink>,
) -> Result<clipped_ipc::AudioDevices, RecorderProblem> {
    match link.call(&clipped_ipc::Command::GetAudioDevices)? {
        clipped_ipc::Reply::AudioDevices { devices } => Ok(devices),
        _ => Err(wrong_reply("get_audio_devices")),
    }
}

/// What the microphone a setting names is hearing right now.
///
/// The half of choosing a microphone a list of names cannot answer: which of
/// this machine's three input devices can actually hear the person choosing
/// (SPEC.md section 45, step 3). Asked repeatedly while a meter is on screen,
/// and each call opens the endpoint and closes it again — so a window that is
/// killed mid-choice leaves no capture running and no microphone-in-use
/// indicator behind (`clipped_session::microphone_level`).
///
/// The value crosses as the settings file's own spelling of a microphone, not as
/// a device name, because the question is what the choice being looked at would
/// record. The recorder resolves it with the code a recording resolves it with,
/// so the meter and the recording cannot be pointed at different endpoints.
#[tauri::command(async)]
fn microphone_level(
    link: tauri::State<'_, RecorderLink>,
    microphone: String,
) -> Result<clipped_ipc::MicrophoneLevel, RecorderProblem> {
    match link.call(&clipped_ipc::Command::GetMicrophoneLevel(
        clipped_ipc::MicrophoneLevelRequest { microphone },
    ))? {
        clipped_ipc::Reply::MicrophoneLevel { level } => Ok(level),
        _ => Err(wrong_reply("get_microphone_level")),
    }
}

/// Whether the recorder starts when this user signs in.
///
/// Not a setting, so not `get_settings`: it is a `Run` value Windows reads at
/// sign-in rather than a key in `settings.json`. The window cannot read the
/// registry — and would not want to, because what it would have to *write* is
/// the recorder's own executable path, which only the recorder knows
/// (issue #308).
#[tauri::command(async)]
fn start_at_login(
    link: tauri::State<'_, RecorderLink>,
) -> Result<clipped_ipc::StartAtLogin, RecorderProblem> {
    match link.call(&clipped_ipc::Command::GetStartAtLogin)? {
        clipped_ipc::Reply::StartAtLogin { start_at_login } => Ok(start_at_login),
        _ => Err(wrong_reply("get_start_at_login")),
    }
}

/// Turns starting at login on or off, and answers with where it now stands.
///
/// The answer is the registry's, not this window's idea of what it sent: a
/// write the registry refused would otherwise be drawn as a switch that moved.
#[tauri::command(async)]
fn set_start_at_login(
    link: tauri::State<'_, RecorderLink>,
    enabled: bool,
) -> Result<clipped_ipc::StartAtLogin, RecorderProblem> {
    match link.call(&clipped_ipc::Command::SetStartAtLogin(
        clipped_ipc::SetStartAtLogin { enabled },
    ))? {
        clipped_ipc::Reply::StartAtLogin { start_at_login } => Ok(start_at_login),
        _ => Err(wrong_reply("set_start_at_login")),
    }
}

/// A recording that started, in the form the window renders.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct StartedRecording {
    /// Identifies it to [`stop_recording`].
    recording_id: String,
    /// The file it is writing, which is the one thing worth saying about a
    /// recording that has just begun and the one thing nothing else will say.
    output: String,
}

/// What this application asks the recorder to record, wherever it is asked
/// from.
///
/// One function because there are two controls — the window's Record button and
/// the tray's Start Recording — and they must not drift. Issue #427 is what
/// drift here looks like: the tray's Save Replay item was correct, enabled
/// exactly when the running recording had a buffer, and dark for ever, because
/// the two places that started a recording both forgot to ask for one.
///
/// **`pid` rather than "whatever is in front now"** deliberately, and it is the
/// same reasoning `StopRecording::recording_id` carries: the control names what
/// it will record, and the foreground can change between the label being drawn
/// and the button being pressed. Sending the identifier that was on screen means
/// a press cannot record an application the user was never offered.
///
/// **`replay` rather than `replay_seconds`.** A recording started here keeps a
/// replay buffer, so Save Replay is a control rather than a label, and how long
/// it keeps is `replay_window_seconds` — a setting, resolved by the recorder for
/// the game it turns out to be recording. This process cannot read it and should
/// not: it may link `clipped-ipc` and nothing else of the workspace
/// (`tests/integration/tests/workspace_layering.rs`), and a length invented here
/// would be a duration nobody chose (AGENTS.md sections 30 and 55).
///
/// **Nothing else is sent.** Resolution, frame rate, codec, encoder and the
/// audio devices are the recorder's own settings for the same reason; the output
/// path is likewise the recorder's timestamped default, which is what
/// `clipped-recorder record` writes with no `--output`.
pub(crate) fn recording_request(process_id: u32) -> clipped_ipc::StartRecording {
    clipped_ipc::StartRecording {
        pid: Some(process_id),
        replay: true,
        ..clipped_ipc::StartRecording::default()
    }
}

/// Records the process the window named.
///
/// The request is [`recording_request`], which is also what the tray sends.
///
/// `async` for the reason [`recorder_status`] is, and more so: this one waits
/// for a capture target to be resolved and an encoder session to be opened.
#[tauri::command(async)]
fn start_recording(
    link: tauri::State<'_, RecorderLink>,
    process_id: u32,
) -> Result<StartedRecording, RecorderProblem> {
    let reply = link.call(&clipped_ipc::Command::StartRecording(recording_request(
        process_id,
    )))?;

    match reply {
        clipped_ipc::Reply::RecordingStarted {
            recording_id,
            output,
        } => Ok(StartedRecording {
            recording_id,
            output,
        }),
        _ => Err(wrong_reply("start_recording")),
    }
}

/// Stops the recording the window had on screen, and waits for its file.
///
/// `recording_id` is what the window last saw [`recorder_status`] report, so
/// that a recording which ended by itself in the meantime cannot have its
/// successor stopped instead — the race `StopRecording::recording_id` exists
/// for, and a real one when the recorded window closed at the same moment the
/// user pressed the button.
///
/// [`None`] means "whatever is running", which is what the tray sends and what
/// this command sends when the window has no particular recording on screen. It
/// is passed through rather than substituted with something: a command that
/// invented an identifier would stop a recording nobody asked about.
///
/// The reply is the finished recording, and it arrives **after the file has been
/// finalised** — `stop_recording` does not answer until the muxer has closed the
/// container (`docs/ipc.md`). A window that drew "stopped" before that would be
/// pointing at a file that was not yet playable, which is the one claim this
/// control must not make (AGENTS.md section 22).
#[tauri::command(async)]
fn stop_recording(
    link: tauri::State<'_, RecorderLink>,
    recording_id: Option<String>,
) -> Result<clipped_ipc::RecordingSummary, RecorderProblem> {
    let reply = link.call(&clipped_ipc::Command::StopRecording(
        clipped_ipc::StopRecording { recording_id },
    ))?;

    match reply {
        clipped_ipc::Reply::RecordingStopped { summary } => Ok(summary),
        _ => Err(wrong_reply("stop_recording")),
    }
}

/// Copies a recording into MP4, and waits for the file.
///
/// The window has read `source` out of the library and the person at the
/// keyboard has chosen `destination` through a Save As dialog
/// (`recordingActions.ts`). Both are passed on exactly as given: a command that
/// substituted a destination of its own would write somewhere nobody chose, and
/// one that resolved the source would export a file the library never listed.
///
/// Nothing here checks either path. That is not an oversight — the recorder is
/// the process that will open them, and a check here would be a second answer to
/// a question only the muxer can settle, made a moment earlier and therefore
/// against a different state of the disk (AGENTS.md section 55). What the window
/// gets back is the recorder's own refusal: `destination_exists` for a file that
/// is already there, which the interface offers "choose another name" on, and
/// `export_failed` carrying the muxer's own sentence about a recording MP4
/// cannot hold.
///
/// The reply arrives **after the MP4's index has been written**, so a window
/// that has been told the export finished is pointing at a playable file — the
/// same promise `stop_recording` makes (AGENTS.md section 22).
///
/// `async` for the reason [`recorder_status`] is, and much more so: a copy of a
/// long recording is bounded by the disk, and the thread drawing the window may
/// not wait on one.
#[tauri::command(async)]
fn export_recording(
    link: tauri::State<'_, RecorderLink>,
    source: String,
    destination: String,
) -> Result<clipped_ipc::ExportSummary, RecorderProblem> {
    let reply = link.call(&clipped_ipc::Command::ExportRecording(
        clipped_ipc::ExportRecording {
            source,
            destination,
        },
    ))?;

    match reply {
        clipped_ipc::Reply::RecordingExported { export } => Ok(export),
        _ => Err(wrong_reply("export_recording")),
    }
}

/// A recording the recorder has opened for playback, as the window needs it.
///
/// The **address** rather than the path: what the recorder answered with is
/// registered with the `clip` scheme and the window is handed a number that
/// stands for it, so the interface never holds a file name it could ask for
/// something else with (`playback`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct OpenedPlayback {
    /// Where to point a `<video>`.
    url: String,
    /// The source stream index whose sound is in it, if it has any.
    audio_track: Option<usize>,
    /// Every sound track of the recording, for the selector.
    audio_tracks: Vec<clipped_ipc::PlaybackTrack>,
    /// Whether a copy had to be made to carry the chosen track.
    prepared: bool,
}

/// Opens a recording for playback, and registers what came back.
///
/// Two things happen here and both matter. The **recorder** decides what may be
/// played: it opens the recording, lists its sound tracks and answers with a
/// file — the recording itself when a media element would already play the
/// chosen track, and a copy carrying that track alone when it would not, which
/// is the whole reason this is a round trip rather than a path the window
/// already has (`clipped_ipc::playback`, issue #304). Then **this process**
/// registers that file with the `clip` scheme and hands back an address.
///
/// The window is never given a path and can never name one: a number nothing
/// has registered is a 404, so the reach it gains is exactly the recordings the
/// recorder has vouched for in this session (`playback`).
///
/// `async` for the reason [`export_recording`] is: preparing a track is a pass
/// over the recording, and the thread drawing the window may not wait on one.
#[tauri::command(async)]
fn open_playback(
    link: tauri::State<'_, RecorderLink>,
    source: String,
    audio_track: Option<usize>,
) -> Result<OpenedPlayback, RecorderProblem> {
    let reply = link.call(&clipped_ipc::Command::OpenPlayback(
        clipped_ipc::OpenPlayback {
            source,
            audio_track,
        },
    ))?;

    match reply {
        clipped_ipc::Reply::PlaybackOpened { playback } => Ok(OpenedPlayback {
            url: playback::url_for(std::path::Path::new(&playback.path)),
            audio_track: playback.audio_track,
            audio_tracks: playback.audio_tracks,
            prepared: playback.prepared,
        }),
        _ => Err(wrong_reply("open_playback")),
    }
}

/// Something this process was asked to do with a file, and could not.
///
/// A shape of its own rather than a [`RecorderProblem`], because no recorder was
/// involved and saying one refused would send somebody looking at the wrong
/// thing. It carries the same two fields so that the interface reads every
/// failure the same way (`library.ts`, `asProblem`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct FileProblem {
    /// [`FILE_MISSING`] or [`SHELL_REFUSED`].
    code: String,
    /// One sentence for a person, which is the part that is always shown.
    message: String,
}

/// The code a file that is not where the library said it was carries.
const FILE_MISSING: &str = "file_missing";

/// The code Windows refusing the request carries.
const SHELL_REFUSED: &str = "shell_refused";

/// Checks the file is still there, and says so usefully if it is not.
///
/// Worth doing before either action below, because the alternative is Windows's
/// own answer to opening a path that does not exist, which is a dialog this
/// process did not raise and cannot word. The library already knows a file can
/// go — it records `missing_since` for one it could not find — but that is
/// as of the last reconciliation, and a file deleted since then is a real case
/// (`docs/library.md`, AGENTS.md section 16).
///
/// The check is not a guarantee and is not meant to be: the file can go between
/// this and the shell opening it. What it buys is that the common failure gets
/// the sentence it deserves.
fn still_there(path: &str) -> Result<PathBuf, FileProblem> {
    let file = PathBuf::from(path);
    if matches!(file.try_exists(), Ok(true)) {
        return Ok(file);
    }
    Err(FileProblem {
        code: FILE_MISSING.to_owned(),
        message: format!(
            "{} is not there any more. It may have been moved or deleted, or the drive it is on \
             may not be connected.",
            file.file_name().map_or_else(
                || path.to_owned(),
                |name| name.to_string_lossy().into_owned()
            )
        ),
    })
}

/// Opens a recording in whatever application the user opens video with.
///
/// Still here now that the window plays a recording itself
/// ([`open_playback`], issue #304), and deliberately: the window plays one
/// track at a time and offers no scrubbing beyond what a `<video>` gives, and
/// somebody who wants their own player, frame stepping or a second monitor
/// should have the file. Watching in Clipped and watching in VLC are different
/// requests.
///
/// No default application is named, so this is whatever the user chose for
/// `.mkv`. A machine with nothing associated is Windows's "how do you want to
/// open this file" prompt, which is the right answer and not a failure.
///
/// `async` because opening an application is a shell call that can block for as
/// long as the shell takes.
#[tauri::command(async)]
fn open_recording<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
) -> Result<(), FileProblem> {
    use tauri_plugin_opener::OpenerExt as _;

    let file = still_there(&path)?;
    app.opener()
        .open_path(file.to_string_lossy(), None::<&str>)
        .map_err(|error| FileProblem {
            code: SHELL_REFUSED.to_owned(),
            message: format!("Windows would not open that recording: {error}"),
        })
}

/// Shows a recording in Explorer, with the file selected.
///
/// The permanent answer to "where did it go?". Selected rather than merely
/// opening the folder, because a recordings folder holds hundreds of files and
/// an unselected one is a folder somebody now has to search
/// (SPEC.md section 17).
///
/// `async` for the reason [`open_recording`] is.
#[tauri::command(async)]
fn reveal_recording<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
) -> Result<(), FileProblem> {
    use tauri_plugin_opener::OpenerExt as _;

    let file = still_there(&path)?;
    app.opener()
        .reveal_item_in_dir(&file)
        .map_err(|error| FileProblem {
            code: SHELL_REFUSED.to_owned(),
            message: format!("Windows would not show that recording in Explorer: {error}"),
        })
}

/// Something that went wrong before the window existed to be told about it.
///
/// Written during `setup` and read by the window when it mounts. It cannot be
/// an event: the window subscribes from React, which has not run yet when the
/// tray is built, so an event sent then would be sent to nobody. It cannot be
/// standard error either — a release build has no console — and this is exactly
/// the class of failure a user has to know about, so it waits to be asked for
/// (AGENTS.md section 45).
static STARTUP_NOTICE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Records something for the window to say when it opens.
fn set_startup_notice(notice: &str) {
    // Through a poisoned lock deliberately: a panic elsewhere must not be what
    // decides the user is never told (the same reasoning as `RecorderLink`'s
    // own state).
    *STARTUP_NOTICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(notice.to_owned());
    // A debug build has a console, and this belongs in it beside whatever else
    // was written as the application came up.
    eprintln!("clipped: {notice}");
}

/// What the window asks for when it mounts.
#[tauri::command]
fn startup_notice() -> Option<String> {
    STARTUP_NOTICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The sentence shown when there is no notification-area icon.
///
/// It says all three things the next few minutes depend on: that the icon is
/// missing, that closing the window therefore quits rather than minimising, and
/// that quitting leaves the recorder running — because with no tray there is no
/// Exit, and Exit is the only thing that stops a recorder (`tray::exit`,
/// ADR 0002).
fn no_tray_notice(error: &str) -> String {
    format!(
        "Clipped could not add its notification-area icon: {error}. Closing this window will \
         therefore quit Clipped instead of minimising to the tray, and quitting leaves the \
         recorder running — the tray's Exit is the only thing that stops one. Restarting Clipped \
         is worth trying; if the icon is still missing, end clipped-recorder.exe in Task Manager \
         to stop a recording."
    )
}

/// What the supervisor is told about this machine.
fn supervisor_settings() -> Result<SupervisorSettings, String> {
    let endpoint = Endpoint::for_this_session()
        .map_err(|error| format!("the recorder endpoint could not be named: {error}"))?;

    Ok(SupervisorSettings::new(
        endpoint,
        recorder_executable()?,
        PeerIdentity {
            name: env!("CARGO_PKG_NAME").to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    ))
}

/// The recorder this application starts.
///
/// Beside this executable, because that is where an installation puts it, unless
/// [`RECORDER_OVERRIDE`] names another. Nothing searches the `PATH`: a recorder
/// found on the `PATH` could be any build of any age, and the one thing a
/// supervisor must not do is start a recorder it cannot account for.
///
/// The installer puts one there: `bundle.resources` in `tauri.conf.json`
/// collects the recorder and the FFmpeg libraries beside this executable
/// (`docs/packaging.md`, issue #226). In development they are in the workspace's
/// own target directory instead, which is what [`RECORDER_OVERRIDE`] is for.
fn recorder_executable() -> Result<PathBuf, String> {
    if let Some(named) = std::env::var_os(RECORDER_OVERRIDE) {
        return Ok(PathBuf::from(named));
    }

    let beside = std::env::current_exe()
        .map_err(|error| format!("Clipped could not find its own location: {error}"))?
        .parent()
        .ok_or_else(|| "Clipped is not in a directory".to_owned())?
        .join(RECORDER_EXECUTABLE);

    Ok(beside)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_missing_tray_tells_the_user_what_closing_the_window_will_now_do() {
        // The trap this replaced: `on_window_event` refused to close whether or
        // not there was a tray, so a failed install left a window that would
        // not close and no icon to quit from. Closing now works — and a user
        // who is not told will close it expecting to minimise and quit instead,
        // with a recorder still running.
        let said = no_tray_notice("the shell would not accept the icon");

        assert!(
            said.contains("the shell would not accept the icon"),
            "{said}"
        );
        assert!(
            said.contains("quit Clipped instead of minimising"),
            "{said}"
        );
        assert!(said.contains("leaves the recorder running"), "{said}");
        assert!(said.contains("Task Manager"), "{said}");
    }

    #[test]
    fn a_library_question_that_never_reached_a_recorder_is_not_a_refusal_it_made() {
        // The distinction the Library screen turns on. A window that could not
        // tell "the recorder said the index is unreadable" from "there was no
        // recorder" would show one sentence for both, and the useful action is
        // different in each case (AGENTS.md section 45).
        let refused = RecorderProblem::from(clipped_ipc::RecorderCallError::Refused(
            clipped_ipc::ProtocolError::new(
                clipped_ipc::ErrorCode::LibraryUnavailable,
                "the recording library could not be opened",
            ),
        ));
        assert_eq!(refused.code, "library_unavailable");
        assert_eq!(refused.message, "the recording library could not be opened");

        let missing = RecorderProblem::from(clipped_ipc::RecorderCallError::NoRecorderConfigured);
        assert_eq!(missing.code, NO_RECORDER_CONFIGURED);
        assert_ne!(
            missing.code, "library_unavailable",
            "a question that never reached a recorder must not look like a refusal it made"
        );
    }

    /// A recorder that answers the two library commands and remembers exactly
    /// what it was asked.
    #[derive(Debug, Default)]
    pub(crate) struct AskedRecorder {
        /// Every command it was sent, in order.
        asked: std::sync::Mutex<Vec<clipped_ipc::Command>>,
        /// What to answer every command with instead of a reply.
        refusal: Option<clipped_ipc::ProtocolError>,
    }

    impl AskedRecorder {
        /// A recorder that refuses everything it is asked.
        fn refusing(refusal: clipped_ipc::ProtocolError) -> Self {
            Self {
                asked: std::sync::Mutex::new(Vec::new()),
                refusal: Some(refusal),
            }
        }

        /// What it has been asked so far.
        pub(crate) fn asked(&self) -> Vec<clipped_ipc::Command> {
            self.asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        /// Waits until the link has asked its own start-up question.
        ///
        /// `RecorderLink` asks `get_hotkeys` once it has attached, on its
        /// watching thread, so that a combination Windows refused reaches the
        /// user as a notification rather than only as a line in a log
        /// ([issue #417](https://github.com/wildware-uk/clipped/issues/417)).
        ///
        /// That question races every command a test sends afterwards: it can
        /// arrive before one, after one, or between two. Waiting for it here and
        /// then forgetting it is what keeps `asked()` a statement about *the
        /// window* rather than about which thread got there first — the
        /// alternative is nine assertions that pass or fail on timing.
        fn wait_for_the_links_own_question(&self) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if self
                    .asked()
                    .iter()
                    .any(|command| matches!(command, clipped_ipc::Command::GetHotkeys))
                {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!(
                "the link never asked this recorder which hotkeys it holds, so either it did not \
                 attach or it stopped asking"
            );
        }

        /// Forgets everything asked so far.
        fn forget(&self) {
            self.asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
    }

    impl clipped_ipc::CommandHandler for AskedRecorder {
        fn call(
            &self,
            command: clipped_ipc::Command,
        ) -> Result<clipped_ipc::Reply, clipped_ipc::ProtocolError> {
            self.asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(command.clone());

            if let Some(refusal) = &self.refusal {
                return Err(refusal.clone());
            }

            match command {
                clipped_ipc::Command::LibrarySessions(_) => {
                    Ok(clipped_ipc::Reply::LibrarySessions { page: a_page() })
                }
                clipped_ipc::Command::LibraryGames => Ok(clipped_ipc::Reply::LibraryGames {
                    games: vec![a_game()],
                }),
                clipped_ipc::Command::GetStatus => Ok(clipped_ipc::Reply::Status {
                    status: a_running_recording(),
                }),
                clipped_ipc::Command::GetHotkeys => Ok(clipped_ipc::Reply::Hotkeys {
                    hotkeys: vec![a_hotkey_the_user_cannot_have()],
                }),
                clipped_ipc::Command::StartRecording(_) => {
                    Ok(clipped_ipc::Reply::RecordingStarted {
                        recording_id: "rec-1".to_owned(),
                        output: r"D:\clips\clipped-cs2-2026-08-12T09-15-00.mkv".to_owned(),
                    })
                }
                clipped_ipc::Command::StopRecording(_) => {
                    Ok(clipped_ipc::Reply::RecordingStopped {
                        summary: a_summary(),
                    })
                }
                clipped_ipc::Command::ExportRecording(request) => {
                    Ok(clipped_ipc::Reply::RecordingExported {
                        // Built from the request rather than from a constant,
                        // so that a command which sent the wrong paths — or the
                        // right ones the wrong way round — shows up in what
                        // comes back.
                        export: an_export(&request),
                    })
                }
                other => Err(clipped_ipc::ProtocolError::new(
                    clipped_ipc::ErrorCode::UnknownCommand,
                    format!(
                        "this test recorder answers the library and recording commands only, not \
                         {other:?}"
                    ),
                )),
            }
        }

        fn status(&self) -> clipped_ipc::RecorderStatus {
            a_running_recording()
        }

        fn features(&self) -> Vec<String> {
            vec![clipped_ipc::features::LIBRARY.to_owned()]
        }
    }

    /// The status [`AskedRecorder`] answers `get_status` with.
    fn a_running_recording() -> clipped_ipc::RecorderStatus {
        clipped_ipc::RecorderStatus::Recording(clipped_ipc::ActiveRecording {
            recording_id: "rec-1".to_owned(),
            output: r"D:\clips\clipped-cs2-2026-08-12T09-15-00.mkv".to_owned(),
            target: "process `cs2.exe`".to_owned(),
            elapsed_ms: 754_000,
            // A recording this application started keeps a buffer, at the
            // window the recorder's settings resolved (#427), so the status it
            // reports one from carries the length rather than nothing.
            replay_seconds: Some(300),
            session: None,
        })
    }

    /// The row [`AskedRecorder`] answers `get_hotkeys` with.
    ///
    /// A conflict deliberately, because that is the row this command exists to
    /// carry: an empty answer and a table of registered rows both read as "no
    /// hotkey has a problem", and the one thing the window cannot find out for
    /// itself is that Windows refused one.
    fn a_hotkey_the_user_cannot_have() -> clipped_ipc::HotkeyBinding {
        clipped_ipc::HotkeyBinding {
            action: "save_replay".to_owned(),
            label: "Save replay".to_owned(),
            hotkey: Some("Ctrl+F10".to_owned()),
            state: clipped_ipc::HotkeyState::Conflict {
                reason: "Ctrl+F10 could not be Clipped's shortcut for Save replay: another \
                         application already uses it"
                    .to_owned(),
            },
            handled: false,
            unavailable: Some(
                "Save replay is not in this build: it arrives in M3 (issue #38)".to_owned(),
            ),
        }
    }

    /// The summary [`AskedRecorder`] answers `stop_recording` with.
    fn a_summary() -> clipped_ipc::RecordingSummary {
        clipped_ipc::RecordingSummary {
            output: r"D:\clips\clipped-cs2-2026-08-12T09-15-00.mkv".to_owned(),
            duration_ms: 754_000,
            end_reason: clipped_ipc::EndReason::Stopped,
            frames_encoded: 45_240,
            frames_skipped_for_rate: 0,
            frames_dropped_writer_behind: 0,
            sustained_framerate: Some(59.98),
            encoder: "nvenc".to_owned(),
            codec: "av1".to_owned(),
            width: 2_560,
            height: 1_392,
        }
    }

    /// The export [`AskedRecorder`] answers `export_recording` with.
    fn an_export(request: &clipped_ipc::ExportRecording) -> clipped_ipc::ExportSummary {
        clipped_ipc::ExportSummary {
            source: request.source.clone(),
            destination: request.destination.clone(),
            duration_ms: 754_000,
            packets: 45_240,
            bytes: 9_811_204_112,
            elapsed_ms: 4_182,
            lossless: true,
            losses: Vec::new(),
        }
    }

    /// The page [`AskedRecorder`] answers `library_sessions` with.
    fn a_page() -> clipped_ipc::LibrarySessionPage {
        clipped_ipc::LibrarySessionPage {
            sessions: vec![clipped_ipc::LibrarySession {
                session_id: "counter-strike-2-20260811-201400".to_owned(),
                game_name: Some("Counter-Strike 2".to_owned()),
                started_at: "2026-08-11T20:14:00+01:00".to_owned(),
                ..clipped_ipc::LibrarySession::default()
            }],
            next_cursor: Some(
                "2026-08-11T20:14:00+01:00|counter-strike-2-20260811-201400".to_owned(),
            ),
        }
    }

    /// The game [`AskedRecorder`] answers `library_games` with.
    fn a_game() -> clipped_ipc::LibraryGame {
        clipped_ipc::LibraryGame {
            game_id: Some("counter-strike-2".to_owned()),
            name: Some("Counter-Strike 2".to_owned()),
            sessions: 3,
            recordings: 7,
            ..clipped_ipc::LibraryGame::default()
        }
    }

    /// A recorder listening on a named pipe of this test's own.
    ///
    /// The protocol's own [`clipped_ipc::Server`] behind a real pipe rather
    /// than a stub in place of [`RecorderLink`], because the link is a concrete
    /// type with no seam in it: the only way to find out what the window
    /// actually put on the wire is to be the thing at the other end of it. That
    /// is the point of these tests — the TypeScript suite stubs `invoke` and so
    /// stops at the window's edge, and the recorder's own tests start at its
    /// dispatch, leaving the two commands that join them covered by nothing.
    pub(crate) struct FakeRecorder {
        endpoint: Endpoint,
        pub(crate) handler: std::sync::Arc<AskedRecorder>,
        events: clipped_ipc::EventPublisher,
        serving: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeRecorder {
        /// Starts one on a pipe named after `test`, serving on a thread.
        pub(crate) fn listening(test: &str, handler: AskedRecorder) -> Self {
            let endpoint = Endpoint::named(&format!("clipped-{test}.{}", std::process::id()))
                .expect("the generated name is valid");
            let mut listener = clipped_ipc::transport::Listener::bind(&endpoint)
                .expect("nothing else has this name");

            let handler = std::sync::Arc::new(handler);
            let events = clipped_ipc::EventPublisher::new();
            let server = clipped_ipc::Server::new(
                std::sync::Arc::clone(&handler),
                events.clone(),
                PeerIdentity {
                    name: "clipped-recorder".to_owned(),
                    version: "0.0.0-test".to_owned(),
                },
            );

            let serving = std::thread::spawn(move || {
                let _ = server.serve(&mut listener);
            });

            Self {
                endpoint,
                handler,
                events,
                serving: Some(serving),
            }
        }

        /// A link pointed at this recorder, inside an application that manages
        /// it — which is the only way to obtain the [`tauri::State`] a
        /// `#[tauri::command]` is handed.
        ///
        /// The executable named is one that does not exist, and is never
        /// reached: a recorder is already listening on the endpoint, so the
        /// supervisor attaches rather than starting one. `RestartPolicy::NEVER`
        /// makes sure a test cannot leave something trying again in the
        /// background.
        pub(crate) fn window(&self) -> tauri::App<tauri::test::MockRuntime> {
            let settings = SupervisorSettings {
                restart: clipped_ipc::RestartPolicy::NEVER,
                ..SupervisorSettings::new(
                    self.endpoint.clone(),
                    std::env::temp_dir().join("clipped-no-such-recorder.exe"),
                    PeerIdentity {
                        name: "clipped-desktop-test".to_owned(),
                        version: "0.0.0".to_owned(),
                    },
                )
            };
            let (link, _events) = RecorderLink::start(settings);

            // Settled before the test looks at anything. See
            // `wait_for_the_links_own_question`: the link asks the recorder
            // which hotkeys it holds as soon as it attaches, and a test about
            // what the window asked should not depend on whether that landed
            // first.
            self.handler.wait_for_the_links_own_question();
            self.handler.forget();

            tauri::test::mock_builder()
                .manage(link)
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("a mock application builds")
        }
    }

    impl Drop for FakeRecorder {
        /// Over the protocol, the way the real recorder is stopped, so a test
        /// does not leave a pipe listening for the rest of the run.
        ///
        /// `finalise_recording: true`, and it has to be. This recorder answers
        /// `status` with a recording in progress — which is what the record
        /// control's cases are about — and the protocol refuses a bare
        /// `shutdown` while something is being recorded, with `already_recording`
        /// (`crates/ipc/src/server.rs`, issue #220). A refused shutdown leaves
        /// the listener serving and the `join` below never returns: the first
        /// version of this hung the whole test binary that way.
        fn drop(&mut self) {
            if let Ok(mut client) = clipped_ipc::Client::connect(
                &self.endpoint,
                "clipped-desktop-test",
                "0.0.0",
                std::time::Duration::from_secs(5),
            ) {
                let _ = client.call(&clipped_ipc::Command::Shutdown(clipped_ipc::Shutdown {
                    finalise_recording: true,
                }));
            }
            self.events.close();
            if let Some(serving) = self.serving.take() {
                let _ = serving.join();
            }
        }
    }

    #[test]
    fn the_library_page_command_asks_the_recorder_for_the_page_it_was_asked_for() {
        // The middle hop of the round trip this feature is: window → Tauri
        // command → control protocol → recorder. A `library_sessions` that sent
        // `library_games`, or that dropped `limit`, `after` or `query` on the
        // way through, would show the newest page for ever — the search box
        // typed into and nothing happening, the second page of the library
        // being the first one again — and every other test in the repository
        // would still be green.
        let recorder = FakeRecorder::listening("library-page", AskedRecorder::default());
        let window = recorder.window();

        let page = library_sessions(
            window.state::<RecorderLink>(),
            Some(7),
            Some("2026-08-11T20:14:00+01:00|counter-strike-2-20260810-193000".to_owned()),
            Some("game:cs2 tag:clutch -favourite".to_owned()),
        )
        .expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::LibrarySessions(
                clipped_ipc::LibrarySessions {
                    limit: Some(7),
                    after: Some(
                        "2026-08-11T20:14:00+01:00|counter-strike-2-20260810-193000".to_owned()
                    ),
                    query: Some("game:cs2 tag:clutch -favourite".to_owned()),
                }
            )],
            "the command the recorder received has to be the one the window was asked for, \
             carrying every parameter it was given"
        );
        assert_eq!(
            page,
            a_page(),
            "and its reply has to reach the caller whole"
        );
    }

    #[test]
    fn a_page_asked_for_with_no_parameters_asks_the_recorder_for_none() {
        // What a window sends when a Library screen first opens. `None` has to
        // stay `None`: a command that substituted a limit of its own would put
        // a second page size in the system, and the recorder's is the one that
        // knows what a frame can carry.
        let recorder = FakeRecorder::listening("library-page-default", AskedRecorder::default());
        let window = recorder.window();

        library_sessions(window.state::<RecorderLink>(), None, None, None)
            .expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::LibrarySessions(
                clipped_ipc::LibrarySessions::default()
            )]
        );
    }

    #[test]
    fn the_per_game_command_asks_the_recorder_for_the_games_rather_than_a_page() {
        // The two library commands take the same link and answer shapes that
        // both start with `Library`, so sending the wrong one is a one-word
        // mistake that compiles.
        let recorder = FakeRecorder::listening("library-games", AskedRecorder::default());
        let window = recorder.window();

        let games = library_games(window.state::<RecorderLink>()).expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::LibraryGames]
        );
        assert_eq!(games, vec![a_game()]);
    }

    #[test]
    fn a_refusal_from_the_recorder_reaches_the_window_as_its_own_code() {
        // The other half of what these commands do, over the real transport:
        // an unreadable library has to arrive at the window carrying
        // `library_unavailable`, because that is what the screen tells apart
        // from an empty library (AGENTS.md section 27).
        let recorder = FakeRecorder::listening(
            "library-refused",
            AskedRecorder::refusing(clipped_ipc::ProtocolError::new(
                clipped_ipc::ErrorCode::LibraryUnavailable,
                "the recording library could not be opened: the drive is not connected",
            )),
        );
        let window = recorder.window();

        let problem = library_sessions(window.state::<RecorderLink>(), None, None, None)
            .expect_err("an unreadable library is a refusal");

        assert_eq!(problem.code, "library_unavailable");
        assert!(
            problem.message.contains("the drive is not connected"),
            "the recorder's own sentence is the one worth showing: {}",
            problem.message
        );
    }

    #[test]
    fn the_record_button_asks_the_recorder_to_record_the_process_it_was_given() {
        // The middle hop of the round trip the record control is: window → Tauri
        // command → control protocol → recorder. A `start_recording` that sent
        // `take_screenshot`, or that dropped the process identifier and let the
        // recorder pick its own target, would record the wrong application — or
        // nothing — while the window said it had started, and every TypeScript
        // test in the repository would still be green, because they stub
        // `invoke` (issue #389).
        //
        // `replay: true` is on the wire for the same kind of reason (issue
        // #427). A recording started here that did not ask for a buffer is one
        // the tray's Save Replay item can never be enabled against, and nothing
        // about that failure is visible: the item is correctly disabled, the
        // recording is fine, and the control is dead for ever.
        let recorder = FakeRecorder::listening("record-start", AskedRecorder::default());
        let window = recorder.window();

        let started =
            start_recording(window.state::<RecorderLink>(), 4_242).expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::StartRecording(
                clipped_ipc::StartRecording {
                    pid: Some(4_242),
                    replay: true,
                    ..clipped_ipc::StartRecording::default()
                }
            )],
            "the command the recorder received has to be `start_recording` for the process the \
             window named, keeping a replay buffer, and nothing else"
        );
        assert_eq!(
            recording_request(4_242).replay_seconds,
            None,
            "and it must not name a length: how long a buffer keeps is a setting, and this \
             process cannot read one"
        );
        assert_eq!(
            started,
            StartedRecording {
                recording_id: "rec-1".to_owned(),
                output: r"D:\clips\clipped-cs2-2026-08-12T09-15-00.mkv".to_owned(),
            },
            "and its reply has to reach the caller whole — the file is what the window shows"
        );
    }

    #[test]
    fn the_stop_button_names_the_recording_the_window_had_on_screen() {
        // `recording_id` is the whole safety property of a stop: a recording
        // that ended by itself between the window drawing it and the button
        // being pressed must not have its successor stopped instead
        // (`StopRecording::recording_id`, `docs/ipc.md`). A command that dropped
        // it would send "stop whatever is running", and the failure would only
        // ever show up as somebody's next recording ending early.
        let recorder = FakeRecorder::listening("record-stop", AskedRecorder::default());
        let window = recorder.window();

        let summary = stop_recording(window.state::<RecorderLink>(), Some("rec-1".to_owned()))
            .expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::StopRecording(
                clipped_ipc::StopRecording {
                    recording_id: Some("rec-1".to_owned()),
                }
            )],
            "the identifier the window was showing has to be the one the recorder is given"
        );
        assert_eq!(
            summary,
            a_summary(),
            "and the finished recording has to reach the caller whole"
        );
    }

    #[test]
    fn a_stop_with_no_recording_named_stops_whatever_is_running() {
        // What the window sends when it has no particular recording on screen,
        // and what the tray sends always. `None` has to stay `None`: a command
        // that substituted an identifier of its own would stop a recording
        // nobody asked about.
        let recorder = FakeRecorder::listening("record-stop-any", AskedRecorder::default());
        let window = recorder.window();

        stop_recording(window.state::<RecorderLink>(), None).expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::StopRecording(
                clipped_ipc::StopRecording::default()
            )]
        );
    }

    #[test]
    fn the_windows_recording_state_is_asked_of_the_recorder() {
        // The acceptance criterion this command exists for. The window's
        // "recording" and its elapsed time come from `get_status` and from
        // nothing else — not from a flag set when the button was pressed, and
        // not from a timer the window keeps. A `recorder_status` that sent
        // `ping` and answered from the link's cached state would leave a window
        // claiming to record after the recorder had died, which is the specific
        // failure issue #389 names.
        let recorder = FakeRecorder::listening("record-status", AskedRecorder::default());
        let window = recorder.window();

        let status =
            recorder_status(window.state::<RecorderLink>()).expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::GetStatus],
            "the state the window draws has to be one the recorder was asked for"
        );
        assert_eq!(
            status,
            a_running_recording(),
            "and the recorder's own status has to reach the caller whole, elapsed time included"
        );
    }

    #[test]
    fn the_hotkey_table_the_window_draws_is_the_recorders_own_answer() {
        // The wire this command is: `get_hotkeys` out, the recorder's rows
        // back, unchanged. A `recorder_hotkeys` that answered with an empty list
        // would draw an empty hotkey table, and an empty table reads as "no
        // hotkey has a problem" — the exact thing the conflict row exists to
        // prevent (AGENTS.md section 27, issue #232). The window registers
        // nothing itself, so there is no second place this could come from and
        // nothing that would notice it had gone.
        let recorder = FakeRecorder::listening("recorder-hotkeys", AskedRecorder::default());
        let window = recorder.window();

        let hotkeys =
            recorder_hotkeys(window.state::<RecorderLink>()).expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::GetHotkeys],
            "where the hotkeys stand has to be asked of the recorder that registered them"
        );
        assert_eq!(
            hotkeys,
            vec![a_hotkey_the_user_cannot_have()],
            "and the recorder's rows have to reach the window whole, the conflict included"
        );
    }

    #[test]
    fn a_recording_that_cannot_start_says_why_in_the_recorders_own_words() {
        // Issue #389's fourth acceptance criterion, over the real transport: a
        // recording that cannot start says why, in the words the protocol
        // returned (AGENTS.md section 45). A command that mapped every failure
        // to one sentence of its own would leave the window unable to tell a
        // window that has closed from an encoder that is busy.
        let recorder = FakeRecorder::listening(
            "record-refused",
            AskedRecorder::refusing(clipped_ipc::ProtocolError::new(
                clipped_ipc::ErrorCode::TargetNotFound,
                "no visible window belongs to process 4242; it may have closed",
            )),
        );
        let window = recorder.window();

        let problem = start_recording(window.state::<RecorderLink>(), 4_242)
            .expect_err("a target that has gone is a refusal");

        assert_eq!(problem.code, "target_not_found");
        assert!(
            problem.message.contains("it may have closed"),
            "the recorder's own sentence is the one worth showing: {}",
            problem.message
        );
    }

    #[test]
    fn the_export_command_asks_the_recorder_for_the_two_files_the_window_named() {
        // The middle hop of the round trip an export is: window → Tauri command
        // → control protocol → recorder. An `export_recording` that swapped the
        // two paths would ask the recorder to read the MP4 the user had just
        // named and write over the recording; one that substituted a
        // destination of its own would write somewhere nobody chose. Both
        // compile, both leave every TypeScript test green — they stub `invoke`
        // — and both are caught here and nowhere else (issue #399).
        let recorder = FakeRecorder::listening("export", AskedRecorder::default());
        let window = recorder.window();

        let summary = export_recording(
            window.state::<RecorderLink>(),
            r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
            r"E:\share\ace on mirage.mp4".to_owned(),
        )
        .expect("the recorder answered");

        assert_eq!(
            recorder.handler.asked(),
            vec![clipped_ipc::Command::ExportRecording(
                clipped_ipc::ExportRecording {
                    source: r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
                    destination: r"E:\share\ace on mirage.mp4".to_owned(),
                }
            )],
            "the recorder has to be given the recording the window listed and the destination \
             the person chose, the right way round"
        );
        assert_eq!(
            summary.source, r"D:\clips\cs2-20260811-201400-1.mkv",
            "and the recorder's own summary has to reach the caller whole"
        );
        assert_eq!(summary.destination, r"E:\share\ace on mirage.mp4");
        assert!(summary.lossless);
    }

    #[test]
    fn an_export_onto_a_file_that_exists_reaches_the_window_as_its_own_code_and_sentence() {
        // Issue #399's fifth and sixth acceptance criteria at the window's
        // edge. `destination_exists` is what the interface offers "choose
        // another name" on, and the sentence is the recorder's — a command that
        // flattened every export failure into one message would leave somebody
        // unable to tell a name that is taken from a recording MP4 cannot hold
        // (AGENTS.md section 45).
        let recorder = FakeRecorder::listening(
            "export-refused",
            AskedRecorder::refusing(clipped_ipc::ProtocolError::new(
                clipped_ipc::ErrorCode::DestinationExists,
                "there is already a file at ace on mirage.mp4, and Clipped does not overwrite \
                 one; choose another name",
            )),
        );
        let window = recorder.window();

        let problem = export_recording(
            window.state::<RecorderLink>(),
            r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
            r"E:\share\ace on mirage.mp4".to_owned(),
        )
        .expect_err("a destination that is taken is a refusal");

        assert_eq!(problem.code, "destination_exists");
        assert!(
            problem.message.contains("choose another name"),
            "the recorder's own sentence is the one worth showing: {}",
            problem.message
        );
    }

    /// An application with the opener plugin in it, which is what
    /// [`open_recording`] and [`reveal_recording`] are handed.
    ///
    /// No window is ever asked for, so nothing is drawn: `mock_builder` builds
    /// on `MockRuntime`, and these commands only need the handle the plugin
    /// hangs off.
    fn window_with_the_opener() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .plugin(tauri_plugin_opener::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("a mock application builds")
    }

    #[test]
    fn a_recording_that_is_not_there_is_never_handed_to_the_shell_by_either_command() {
        // Through the commands themselves rather than through `still_there`
        // beside them, because a check that exists and is not called is exactly
        // the defect this is for. Without it the failure is Windows's own
        // dialog, raised by a process the user did not ask and worded by
        // nobody — and the library cannot cover it: `missing_since` is as of the
        // last reconciliation, and a file deleted since then is a real case
        // (AGENTS.md sections 16 and 45).
        //
        // Only the refusing direction goes through the commands, and
        // deliberately: the accepting one ends in `ShellExecute`, and a test
        // that launched somebody's media player would be a test that opened a
        // window on a build agent. What holds the check honest in the other
        // direction is `a_file_that_is_there_is_not_refused_as_missing` below.
        let gone = std::env::temp_dir().join("clipped-no-such-recording-399.mkv");
        let _ = std::fs::remove_file(&gone);
        let window = window_with_the_opener();

        for problem in [
            open_recording(window.handle().clone(), gone.to_string_lossy().into_owned())
                .expect_err("a file that is not there cannot be opened"),
            reveal_recording(window.handle().clone(), gone.to_string_lossy().into_owned())
                .expect_err("a file that is not there cannot be revealed"),
        ] {
            assert_eq!(
                problem.code, FILE_MISSING,
                "a file that was never handed to the shell must not be reported as one the shell \
                 refused: {problem:?}"
            );
            assert!(
                problem
                    .message
                    .contains("clipped-no-such-recording-399.mkv"),
                "the sentence has to name the file: {}",
                problem.message
            );
        }
    }

    #[test]
    fn a_file_that_is_there_is_not_refused_as_missing() {
        // The other direction, and what makes the test above mean something:
        // without it a check that refused everything would pass just as well,
        // and Open would never work for anybody.
        let directory = std::env::temp_dir().join(format!(
            "clipped-desktop-open-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
        let recording = directory.join("match.mkv");
        std::fs::write(&recording, b"a recording").expect("the file is written");

        assert_eq!(
            still_there(&recording.to_string_lossy()).expect("the file is there"),
            recording
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_startup_failure_waits_for_the_window_to_ask_for_it() {
        // Nothing is listening for an event while `setup` runs, so the notice
        // is kept until the window mounts and asks. A notice that was only
        // emitted would be a notice nobody received.
        assert_eq!(startup_notice(), None, "nothing has gone wrong yet");

        set_startup_notice("the icon could not be added");
        assert_eq!(
            startup_notice().as_deref(),
            Some("the icon could not be added"),
            "the window has to be able to ask for it after the fact"
        );
    }
}
