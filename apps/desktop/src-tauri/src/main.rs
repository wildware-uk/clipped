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
        .invoke_handler(tauri::generate_handler![
            recorder_link_state,
            startup_notice,
            library_sessions,
            library_games
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // On the main thread, and before the tray: an out-of-context window
            // event hook delivers through the hooking thread's message queue,
            // which for a Tauri application is this one. Doing it first means
            // the tray's first menu already knows what to offer to record.
            foreground::follow_the_foreground_window();

            // Before the tray, because both report a startup failure through the
            // same one-sentence notice and a missing tray is the more important
            // of the two: it changes what closing the window does.
            let mut notifier = notifications::install(&handle, &app.state::<RecorderLink>());

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

/// A library read that did not happen, in the form the window renders.
///
/// The window cannot open `library.db` and may not link `clipped-library`, so
/// every question about the library is a round trip to the recorder
/// ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md),
/// [issue #301](https://github.com/wildware-uk/clipped/issues/301)). Three
/// different things can stop one, and the screen has to tell them apart:
///
/// - the recorder **refused**, and [`Self::code`] is its own protocol code —
///   `library_unavailable` for an index that could not be read,
///   `invalid_parameters` for a search that does not parse, `unknown_command`
///   for a recorder built before this command existed;
/// - there is **no recorder to ask**, or it went away mid-question;
/// - this build was started with no recorder configured at all.
///
/// None of them is an empty library, and none of them may be drawn as one
/// (AGENTS.md section 27).
#[derive(Debug, Clone, serde::Serialize)]
struct LibraryProblem {
    /// The recorder's protocol code, or one of the two below for a question
    /// that never reached it.
    ///
    /// Both are outside the protocol's own vocabulary deliberately: a code the
    /// recorder could also send would leave the window unable to tell "the
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

impl From<clipped_ipc::RecorderCallError> for LibraryProblem {
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
                message: "this build has no recorder to ask, so the library cannot be read"
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
fn wrong_reply(command: &str) -> LibraryProblem {
    LibraryProblem {
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
) -> Result<clipped_ipc::LibrarySessionPage, LibraryProblem> {
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

/// What the library holds per game (SPEC.md section 17).
#[tauri::command(async)]
fn library_games(
    link: tauri::State<'_, RecorderLink>,
) -> Result<Vec<clipped_ipc::LibraryGame>, LibraryProblem> {
    match link.call(&clipped_ipc::Command::LibraryGames)? {
        clipped_ipc::Reply::LibraryGames { games } => Ok(games),
        _ => Err(wrong_reply("library_games")),
    }
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

#[cfg(test)]
mod tests {
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
        let refused = LibraryProblem::from(clipped_ipc::RecorderCallError::Refused(
            clipped_ipc::ProtocolError::new(
                clipped_ipc::ErrorCode::LibraryUnavailable,
                "the recording library could not be opened",
            ),
        ));
        assert_eq!(refused.code, "library_unavailable");
        assert_eq!(refused.message, "the recording library could not be opened");

        let missing = LibraryProblem::from(clipped_ipc::RecorderCallError::NoRecorderConfigured);
        assert_eq!(missing.code, NO_RECORDER_CONFIGURED);
        assert_ne!(
            missing.code, "library_unavailable",
            "a question that never reached a recorder must not look like a refusal it made"
        );
    }

    /// A recorder that answers the two library commands and remembers exactly
    /// what it was asked.
    #[derive(Debug, Default)]
    struct AskedRecorder {
        /// Every command it was sent, in order.
        asked: std::sync::Mutex<Vec<clipped_ipc::Command>>,
        /// What to answer both commands with instead of a reply.
        refusal: Option<clipped_ipc::ProtocolError>,
    }

    impl AskedRecorder {
        /// A recorder that refuses every library question.
        fn refusing(refusal: clipped_ipc::ProtocolError) -> Self {
            Self {
                asked: std::sync::Mutex::new(Vec::new()),
                refusal: Some(refusal),
            }
        }

        /// What it has been asked so far.
        fn asked(&self) -> Vec<clipped_ipc::Command> {
            self.asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
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
                other => Err(clipped_ipc::ProtocolError::new(
                    clipped_ipc::ErrorCode::UnknownCommand,
                    format!("this test recorder answers the library commands only, not {other:?}"),
                )),
            }
        }

        fn status(&self) -> clipped_ipc::RecorderStatus {
            clipped_ipc::RecorderStatus::Idle
        }

        fn features(&self) -> Vec<String> {
            vec![clipped_ipc::features::LIBRARY.to_owned()]
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
    struct FakeRecorder {
        endpoint: Endpoint,
        handler: std::sync::Arc<AskedRecorder>,
        events: clipped_ipc::EventPublisher,
        serving: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeRecorder {
        /// Starts one on a pipe named after `test`, serving on a thread.
        fn listening(test: &str, handler: AskedRecorder) -> Self {
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
        fn window(&self) -> tauri::App<tauri::test::MockRuntime> {
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

            tauri::test::mock_builder()
                .manage(link)
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("a mock application builds")
        }
    }

    impl Drop for FakeRecorder {
        /// Over the protocol, the way the real recorder is stopped, so a test
        /// does not leave a pipe listening for the rest of the run.
        fn drop(&mut self) {
            if let Ok(mut client) = clipped_ipc::Client::connect(
                &self.endpoint,
                "clipped-desktop-test",
                "0.0.0",
                std::time::Duration::from_secs(5),
            ) {
                let _ = client.call(&clipped_ipc::Command::Shutdown(
                    clipped_ipc::Shutdown::default(),
                ));
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
