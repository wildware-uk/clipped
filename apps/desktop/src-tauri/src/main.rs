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
mod tray;
mod tray_icon;
mod tray_model;

use std::path::PathBuf;

use clipped_ipc::supervisor::{claim_instance, InstanceClaim};
use clipped_ipc::{
    Endpoint, PeerIdentity, RecorderLink, RecorderLinkEvent, RecorderLinkState, SupervisorSettings,
};
use tauri::{Emitter as _, Manager as _, WindowEvent};

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
        .invoke_handler(tauri::generate_handler![recorder_link_state])
        .setup(move |app| {
            let handle = app.handle().clone();

            // On the main thread, and before the tray: an out-of-context window
            // event hook delivers through the hooking thread's message queue,
            // which for a Tauri application is this one. Doing it first means
            // the tray's first menu already knows what to offer to record.
            foreground::follow_the_foreground_window();

            if let Err(error) = tray::install(&handle, &app.state::<RecorderLink>()) {
                // Not fatal. A window that shows the recorder's state is still
                // worth having, and saying what is missing beats exiting
                // (AGENTS.md section 16).
                eprintln!("Clipped could not add its notification-area icon: {error}");
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
/// **The installer does not put one there yet**
/// ([issue #226](https://github.com/wildware-uk/clipped/issues/226)), so an
/// installed build reports that the recorder is missing — correctly, and every
/// time. In development the recorder is in the workspace's own target
/// directory, which is what [`RECORDER_OVERRIDE`] is for.
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
