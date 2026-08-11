//! The half of the hotkey service that only Windows can answer: registering a
//! real combination, losing one to another owner, and being woken by a real
//! keystroke.
//!
//! The dispatch rules — what a slow handler delays, what an unhandled action
//! reports, what happens when a queue fills — are unit tests in
//! `src/dispatch.rs`, because none of them needs a keyboard. What is here is
//! everything that would still be unproven with those passing:
//!
//! | Test | What it decides |
//! | --- | --- |
//! | `a_combination_another_process_owns_is_reported_as_a_conflict…` | That a taken combination produces a conflict naming it, and that the other bindings survive it |
//! | `stopping_the_service_gives_its_combinations_back` | That a stopped service's combinations can be taken again — by *something*; see the test, which is careful about what it claims |
//! | `a_registered_hotkey_fires_when_the_combination_is_actually_pressed` | That `RegisterHotKey`, the message loop, the virtual-key mapping and the dispatcher agree, against a keystroke Windows delivered |
//! | `a_second_hotkey_arrives_while_the_first_handler_is_still_busy` | The acceptance criterion, through the real message loop: a handler that blocks for a second costs the *next* press nothing |
//! | `hotkeys_fire_while_a_fullscreen_subject_holds_a_display` | `#[ignore]`d: the same, while `test-apps/fullscreen-dx11` covers or exclusively owns a display |
//!
//! # These tests type
//!
//! They synthesise keystrokes with `SendInput`, which is input into whatever
//! session `cargo test` was run in. Two things make that acceptable where
//! `tests/capture` deliberately refuses to do it: the combinations are
//! `Ctrl+Alt+Shift+F13` upwards, which no application binds and no text field
//! receives, and pressing a key is the entire behaviour under test — a hotkey
//! test that never presses a key is a test of a data structure.
//!
//! The combination is chosen from this process's identifier, so that two
//! checkouts running the suite at the same time do not fight over one
//! registration, and every injection is serialised through [`KEYBOARD`] so that
//! two of these tests cannot interleave half a chord each.
//!
//! # When a machine cannot run one
//!
//! A window station that refuses to register a hotkey, and input that Windows
//! declines to inject, are both reported and skipped — and both **fail** under
//! `CLIPPED_REQUIRE_HOTKEYS`, so a machine that is supposed to prove this
//! cannot quietly stop proving it (AGENTS.md section 54). A combination that
//! registered and a keystroke Windows accepted and then no press is never a
//! skip: that is the defect these tests exist to catch.

#![cfg(windows)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use clipped_hotkeys::{
    BindingState, Bindings, ConflictCause, Handlers, Hotkey, HotkeyAction, HotkeyEvent,
    HotkeyService, PressOutcome,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, MOUSEEVENTF_MOVE, MOUSEINPUT, VIRTUAL_KEY, VK_CONTROL, VK_F13, VK_F14, VK_F15,
    VK_F16, VK_F17, VK_F18, VK_F19, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// Turns "this machine could not run the test" from a pass into a failure, the
/// way `CLIPPED_REQUIRE_CAPTURE` does for the capture suite.
const REQUIRE_HOTKEYS: &str = "CLIPPED_REQUIRE_HOTKEYS";

/// How long a press is given to travel from `SendInput` to a handler.
///
/// Two orders of magnitude more than it takes: the interesting failure is "it
/// never arrives", not "it was slow", and a generous bound keeps a loaded CI
/// runner out of the results.
const DELIVERY: Duration = Duration::from_secs(2);

/// How long the slow handler in the concurrency test blocks for.
const SLOW: Duration = Duration::from_secs(1);

/// How long the fullscreen subject is left rendering before a key is pressed at
/// it, so that the press meets an application that is actually presenting
/// rather than one that has just finished its mode change.
const SETTLE: Duration = Duration::from_secs(2);

/// Serialises injected keystrokes.
///
/// Keyboard state is global to the session: two tests pressing chords at once
/// would release each other's modifiers halfway through, and the failure would
/// look exactly like a hotkey that does not work.
static KEYBOARD: Mutex<()> = Mutex::new(());

/// The function keys these tests choose from. `F13` upwards, because no
/// keyboard has them and nothing binds them.
const SPARE_FUNCTION_KEYS: [VIRTUAL_KEY; 12] = [
    VK_F13, VK_F14, VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24,
];

/// Reports that the test could not run here, and fails under
/// [`REQUIRE_HOTKEYS`].
fn skipped(reason: &str) {
    assert!(
        std::env::var_os(REQUIRE_HOTKEYS).is_none_or(|value| value.is_empty()),
        "{REQUIRE_HOTKEYS} is set, so this must not be skipped: {reason}"
    );
    // Through `stderr()` rather than `eprintln!`, which libtest captures: a
    // skip nobody sees is the failure this exists to prevent.
    let _ = writeln!(std::io::stderr(), "SKIPPED (hotkeys): {reason}");
}

/// A combination for one test to use, distinct per test and per process.
///
/// The virtual-key code comes from the Windows header rather than from
/// `clipped-hotkeys`, deliberately: the name is what the library is given and
/// the code is what the keystroke is made of, so a wrong entry in the library's
/// own mapping shows up here as a hotkey that never fires rather than as two
/// copies of the same mistake agreeing with each other.
fn combination(test: u32) -> (Hotkey, VIRTUAL_KEY) {
    let index = (std::process::id() + test) as usize % SPARE_FUNCTION_KEYS.len();
    let key = SPARE_FUNCTION_KEYS[index];
    let name = format!("Ctrl+Alt+Shift+F{}", index + 13);
    let hotkey = name
        .parse()
        .unwrap_or_else(|error| panic!("{name} should parse: {error}"));
    (hotkey, key)
}

/// One keyboard event.
fn key_event(key: VIRTUAL_KEY, released: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: if released {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Presses `Ctrl+Alt+Shift+key` and lets it go.
///
/// Returns whether Windows accepted every event. It refuses when the session
/// will not take injected input at all — a locked workstation, or a process at
/// a lower integrity level than the foreground window — and that is a machine
/// that cannot run the test rather than a defect in the service.
fn press(key: VIRTUAL_KEY, _keyboard: &MutexGuard<'_, ()>) -> bool {
    let chord = [
        key_event(VK_CONTROL, false),
        key_event(VK_MENU, false),
        key_event(VK_SHIFT, false),
        key_event(key, false),
        key_event(key, true),
        key_event(VK_SHIFT, true),
        key_event(VK_MENU, true),
        key_event(VK_CONTROL, true),
    ];

    // SAFETY: `chord` is a live slice of correctly initialised `INPUT`s and the
    // size passed is the size of the type they are. `SendInput` copies them.
    let sent = unsafe { SendInput(&chord, i32::try_from(size_of::<INPUT>()).unwrap_or(0)) };
    sent as usize == chord.len()
}

/// A mouse movement of zero pixels.
///
/// It moves no cursor and types into nothing. It is here because Windows grants
/// an exclusive-fullscreen transition only to a process asking for it while a
/// process that has synthesised an input event is still alive
/// (`tests/capture/README.md`) — and it also wakes a display the idle timeout
/// has turned off. This test process is that process, and it stays alive for
/// the whole run.
fn nudge_the_session(_keyboard: &MutexGuard<'_, ()>) {
    let movement = [INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }];

    // SAFETY: as `press` above — a live slice of initialised `INPUT`s and the
    // size of that type.
    let sent = unsafe { SendInput(&movement, i32::try_from(size_of::<INPUT>()).unwrap_or(0)) };
    if sent == 0 {
        let _ = writeln!(
            std::io::stderr(),
            "[info] Windows would not accept a synthetic mouse move; an exclusive \
             transition is unlikely to be granted",
        );
    }
}

/// Bindings with one action pointed at one combination.
fn binding(action: HotkeyAction, hotkey: Hotkey) -> Bindings {
    let mut bindings = Bindings::empty();
    bindings
        .bind(action, hotkey)
        .expect("one binding cannot collide with another");
    bindings
}

/// Whether the service registered `action`, saying loudly why not if it did
/// not.
fn is_bound(service: &HotkeyService, action: HotkeyAction) -> bool {
    let status = service
        .registration()
        .status(action)
        .expect("every action has a status");

    match status.state() {
        BindingState::Bound => true,
        BindingState::Conflict(conflict) => {
            skipped(&format!(
                "this machine would not register the test's own combination: {conflict}",
            ));
            false
        }
        BindingState::Unbound => panic!("the test bound {action}, so it cannot be unbound"),
    }
}

/// Waits for the next press to be reported.
fn next_event(events: &Receiver<HotkeyEvent>) -> Option<HotkeyEvent> {
    match events.recv_timeout(DELIVERY) {
        Ok(event) => Some(event),
        Err(RecvTimeoutError::Timeout) => None,
        Err(RecvTimeoutError::Disconnected) => panic!("the hotkey service stopped on its own"),
    }
}

/// The conflict half of the acceptance criteria, against Windows rather than
/// against a fixture: a combination that is genuinely taken.
#[test]
fn a_combination_another_registration_owns_is_reported_as_a_conflict_naming_it() {
    let (hotkey, _) = combination(0);

    let (owner, _owner_events) =
        HotkeyService::start(&binding(HotkeyAction::SaveReplay, hotkey), Handlers::new())
            .expect("starting a hotkey service");
    if !is_bound(&owner, HotkeyAction::SaveReplay) {
        return;
    }

    // A second service, wanting the same combination for a different action,
    // and one it can have.
    let (spare, _) = combination(1);
    let mut wanted = binding(HotkeyAction::TakeScreenshot, hotkey);
    wanted
        .bind(HotkeyAction::AddBookmark, spare)
        .expect("a second, free combination");

    let (contender, _events) =
        HotkeyService::start(&wanted, Handlers::new()).expect("starting a second hotkey service");

    let conflicts: Vec<_> = contender.registration().conflicts().collect();
    assert_eq!(
        conflicts.len(),
        1,
        "exactly the taken combination should have been refused: {:?}",
        contender.registration(),
    );
    let conflict = conflicts[0];
    assert_eq!(conflict.hotkey(), hotkey);
    assert_eq!(conflict.action(), HotkeyAction::TakeScreenshot);
    assert_eq!(
        conflict.cause(),
        ConflictCause::AlreadyRegistered,
        "a combination another registration holds is `ERROR_HOTKEY_ALREADY_REGISTERED`, \
         and reporting it as anything else would lose the one message that helps",
    );

    let message = conflict.to_string();
    assert!(
        message.contains(&hotkey.to_string()),
        "the failure has to name the hotkey: {message}",
    );
    assert!(
        message.contains("Take screenshot"),
        "the failure has to name the action: {message}",
    );

    // The one that matters for the user: losing one combination must not cost
    // them the others.
    assert!(
        matches!(
            contender
                .registration()
                .status(HotkeyAction::AddBookmark)
                .map(clipped_hotkeys::ActionStatus::state),
            Some(BindingState::Bound),
        ),
        "the free combination should still have been registered: {:?}",
        contender.registration(),
    );
    assert_eq!(
        contender.registration().bound().count(),
        1,
        "one of the two was refused, so exactly one should be bound: {:?}",
        contender.registration(),
    );

    contender.stop();
    owner.stop();
}

/// That a stopped service's combinations are available again — which is what a
/// user notices, and what a restarted Clipped depends on.
///
/// What it deliberately does **not** claim is that `UnregisterHotKey` is what
/// released them. Windows releases a thread's hotkeys when the thread ends, and
/// the hotkey thread ends here; a mutation that removed every `UnregisterHotKey`
/// call left this test passing. `src/service/windows.rs` says why the call is
/// there anyway.
#[test]
fn stopping_the_service_gives_its_combinations_back() {
    let (hotkey, _) = combination(2);

    let (first, _events) =
        HotkeyService::start(&binding(HotkeyAction::SaveReplay, hotkey), Handlers::new())
            .expect("starting a hotkey service");
    if !is_bound(&first, HotkeyAction::SaveReplay) {
        return;
    }
    first.stop();

    let (second, _events) =
        HotkeyService::start(&binding(HotkeyAction::SaveReplay, hotkey), Handlers::new())
            .expect("starting a second hotkey service");

    assert!(
        matches!(
            second
                .registration()
                .status(HotkeyAction::SaveReplay)
                .map(clipped_hotkeys::ActionStatus::state),
            Some(BindingState::Bound),
        ),
        "the combination the first service held was not released: {:?}",
        second.registration(),
    );
    second.stop();
}

/// The end-to-end one: a keystroke Windows delivered reaches a handler.
#[test]
fn a_registered_hotkey_fires_when_the_combination_is_actually_pressed() {
    let (hotkey, key) = combination(3);
    let (ran, handled) = mpsc::channel();

    let (service, events) = HotkeyService::start(
        &binding(HotkeyAction::ToggleRecording, hotkey),
        Handlers::new().on(HotkeyAction::ToggleRecording, move |press| {
            ran.send(press.hotkey()).expect("the test is listening");
        }),
    )
    .expect("starting a hotkey service");
    if !is_bound(&service, HotkeyAction::ToggleRecording) {
        return;
    }

    // What the configuration screen shows beside each row, and the difference
    // between "this key works" and "this key is registered and does nothing".
    let statuses = service.registration().statuses();
    assert_eq!(
        statuses.len(),
        clipped_hotkeys::ACTIONS.len(),
        "every action gets a row, bound or not",
    );
    assert!(
        statuses
            .iter()
            .find(|status| status.action() == HotkeyAction::ToggleRecording)
            .is_some_and(clipped_hotkeys::ActionStatus::is_handled),
        "the action a handler was registered for should say so: {statuses:?}",
    );
    assert!(
        statuses
            .iter()
            .filter(|status| status.action() != HotkeyAction::ToggleRecording)
            .all(|status| !status.is_handled()),
        "no other action was given a handler: {statuses:?}",
    );

    let keyboard = KEYBOARD.lock().expect("the keyboard lock is not poisoned");
    if !press(key, &keyboard) {
        skipped("this session would not accept synthetic keystrokes");
        return;
    }
    drop(keyboard);

    let event = next_event(&events).unwrap_or_else(|| {
        panic!("{hotkey} was registered and pressed, and no press was reported")
    });
    assert_eq!(event.press().action(), HotkeyAction::ToggleRecording);
    assert_eq!(event.press().hotkey(), hotkey);
    assert_eq!(*event.outcome(), PressOutcome::Dispatched);

    assert_eq!(
        handled
            .recv_timeout(DELIVERY)
            .expect("the handler runs on its own thread"),
        hotkey,
        "the handler was given a different combination from the one pressed",
    );

    service.stop();
}

/// "Handler dispatch never blocks", proven where it has to be true: the Windows
/// message loop.
///
/// The unit tests show the dispatcher does not block its caller. This shows the
/// caller is the message loop and that the loop keeps receiving — a service
/// that ran handlers inline would pass every unit test in `src/dispatch.rs` and
/// fail this.
#[test]
fn a_second_hotkey_arrives_while_the_first_handler_is_still_busy() {
    let (slow, slow_key) = combination(4);
    let (quick, quick_key) = combination(5);
    let (finished, finishes) = mpsc::channel();
    let quickly = finished.clone();

    let mut bindings = binding(HotkeyAction::SaveReplay, slow);
    bindings
        .bind(HotkeyAction::AddBookmark, quick)
        .expect("two different combinations");

    let (service, events) = HotkeyService::start(
        &bindings,
        Handlers::new()
            .on(HotkeyAction::SaveReplay, move |_| {
                std::thread::sleep(SLOW);
                finished.send(HotkeyAction::SaveReplay).ok();
            })
            .on(HotkeyAction::AddBookmark, move |_| {
                quickly.send(HotkeyAction::AddBookmark).ok();
            }),
    )
    .expect("starting a hotkey service");
    if !is_bound(&service, HotkeyAction::SaveReplay)
        || !is_bound(&service, HotkeyAction::AddBookmark)
    {
        return;
    }

    let keyboard = KEYBOARD.lock().expect("the keyboard lock is not poisoned");
    let sent = press(slow_key, &keyboard) && press(quick_key, &keyboard);
    drop(keyboard);
    if !sent {
        skipped("this session would not accept synthetic keystrokes");
        return;
    }

    let started = Instant::now();
    let first =
        next_event(&events).unwrap_or_else(|| panic!("{slow} was pressed and not reported"));
    let second =
        next_event(&events).unwrap_or_else(|| panic!("{quick} was pressed and not reported"));
    let both_reported = started.elapsed();

    assert_eq!(first.press().action(), HotkeyAction::SaveReplay);
    assert_eq!(second.press().action(), HotkeyAction::AddBookmark);
    assert!(
        both_reported < SLOW,
        "the second press took {both_reported:?} to be reported, so the message \
         loop was inside the first handler",
    );

    let first_finished = finishes
        .recv_timeout(SLOW)
        .expect("the quick handler should finish long before the slow one");
    assert_eq!(
        first_finished,
        HotkeyAction::AddBookmark,
        "the bookmark handler waited for the replay handler",
    );

    let then = finishes
        .recv_timeout(SLOW * 3)
        .expect("the slow handler still finishes");
    assert_eq!(then, HotkeyAction::SaveReplay);

    service.stop();
}

/// The subject: `test-apps/fullscreen-dx11`, covering or owning a display.
struct Subject {
    child: Child,
    /// Held open because closing it is how the application is told to stop.
    stdin: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    /// Whether Windows granted the exclusive transition, from the `ready` line.
    exclusive: bool,
    /// The window it drew, from the `ready` line.
    window: HWND,
}

impl Subject {
    /// Starts the application in `mode` and returns once it has announced
    /// itself.
    ///
    /// The binary is found beside this test rather than through
    /// `CARGO_BIN_EXE_`, which Cargo only sets for a test in the binary's own
    /// package — and `clipped-hotkeys` must not depend on a test application:
    /// `tests/integration/tests/workspace_layering.rs` forbids it, in either
    /// direction.
    fn start(mode: &str, seconds: u32) -> Result<Self, String> {
        let binary = std::env::current_exe()
            .map_err(|error| format!("this test has no path: {error}"))?
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("this test is not inside a target directory")?
            .join("fullscreen-dx11.exe");
        if !binary.exists() {
            return Err(format!(
                "{} does not exist. Build it first: \
                 cargo build -p clipped-fullscreen-dx11",
                binary.display()
            ));
        }

        let mut child = Command::new(&binary)
            .args(["--mode", mode, "--seconds", &seconds.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{} would not start: {error}", binary.display()))?;

        let stdin = child.stdin.take();
        let mut output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or("the subject was started without a pipe to read")?,
        );

        let mut ready = String::new();
        output
            .read_line(&mut ready)
            .map_err(|error| format!("the subject said nothing: {error}"))?;
        let _ = writeln!(std::io::stderr(), "[subject] {}", ready.trim_end());

        let field = |name: &str| -> Option<String> {
            ready
                .split_whitespace()
                .find_map(|part| part.strip_prefix(name).map(str::to_owned))
        };
        let handle = field("hwnd=").ok_or_else(|| format!("no window in `{ready}`"))?;
        let handle = i64::from_str_radix(handle.trim_start_matches("0x"), 16)
            .map_err(|error| format!("`{handle}` is not a window handle: {error}"))?;

        Ok(Self {
            child,
            stdin,
            output,
            exclusive: field("exclusive=").as_deref() == Some("yes"),
            window: HWND(handle as *mut core::ffi::c_void),
        })
    }
}

impl Drop for Subject {
    /// Closes standard input, which is how the application is asked to give the
    /// display back, and kills it if it will not. There is no path — a panicking
    /// assertion included — on which a test leaves a display taken.
    fn drop(&mut self) {
        drop(self.stdin.take());

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }

        let mut last = String::new();
        let _ = self.output.read_line(&mut last);
        if !last.trim().is_empty() {
            let _ = writeln!(std::io::stderr(), "[subject] {}", last.trim_end());
        }
    }
}

/// The acceptance criterion this crate exists for: a hotkey fires while a game
/// has the display.
///
/// It is `#[ignore]`d because it takes a display away from whoever is at the
/// machine for the length of the run, and because it types.
///
/// ```text
/// $env:CLIPPED_REQUIRE_HOTKEYS = "1"
/// cargo build -p clipped-fullscreen-dx11
/// cargo test -p clipped-hotkeys --test windows_hotkeys -- --ignored --nocapture --test-threads=1
/// ```
///
/// **Run it from an interactive shell.** Windows grants the exclusive
/// transition only while a process that has synthesised an input event is still
/// running; this test is that process — it nudges the session before starting
/// the subject — but a machine nobody has touched may still refuse, and a
/// refusal is reported rather than asserted away. `exclusive=no` in the output
/// means the run covered a borderless fullscreen window and says nothing about
/// the exclusive case.
#[test]
#[ignore = "takes over a display and synthesises keystrokes; see the doc comment"]
fn hotkeys_fire_while_a_fullscreen_subject_holds_a_display() {
    for mode in ["borderless", "exclusive"] {
        let (hotkey, key) = combination(6);
        let (ran, handled) = mpsc::channel();

        // The service starts first, which is the order a user has: the recorder
        // is running, and then the game starts.
        let (service, events) = HotkeyService::start(
            &binding(HotkeyAction::ToggleRecording, hotkey),
            Handlers::new().on(HotkeyAction::ToggleRecording, move |press| {
                ran.send(press.at()).expect("the test is listening");
            }),
        )
        .expect("starting a hotkey service");
        assert!(
            is_bound(&service, HotkeyAction::ToggleRecording),
            "{hotkey} could not be registered, so this run proves nothing",
        );

        let keyboard = KEYBOARD.lock().expect("the keyboard lock is not poisoned");
        nudge_the_session(&keyboard);
        drop(keyboard);

        let subject = match Subject::start(mode, 30) {
            Ok(subject) => subject,
            Err(reason) => {
                skipped(&reason);
                return;
            }
        };

        // Let the subject settle into the display and present some frames. The
        // `ready` line comes as soon as the transition is done, and a key
        // pressed in the same instant would be a weaker claim than one pressed
        // at a game that is actually rendering.
        std::thread::sleep(SETTLE);

        // SAFETY: `GetForegroundWindow` takes nothing and returns a handle that
        // may be null; it is only compared here.
        let foreground = unsafe { GetForegroundWindow() };
        let _ = writeln!(
            std::io::stderr(),
            "[info] mode={mode} exclusive={} subject-has-foreground={} hotkey={hotkey}",
            self::yes_or_no(subject.exclusive),
            self::yes_or_no(foreground == subject.window),
        );
        assert_eq!(
            foreground, subject.window,
            "the subject does not have the foreground, so this run says nothing \
             about a hotkey reaching past a game that does",
        );

        let keyboard = KEYBOARD.lock().expect("the keyboard lock is not poisoned");
        let sent = press(key, &keyboard);
        drop(keyboard);
        assert!(
            sent,
            "Windows would not accept the keystroke, so nothing was pressed",
        );

        let pressed = Instant::now();
        let event = next_event(&events).unwrap_or_else(|| {
            panic!(
                "{hotkey} was registered and pressed while a {mode} fullscreen \
                 application (exclusive={}) held the display, and no press was reported",
                self::yes_or_no(subject.exclusive),
            )
        });
        assert_eq!(*event.outcome(), PressOutcome::Dispatched);
        let ran_at = handled
            .recv_timeout(DELIVERY)
            .expect("the handler runs on its own thread");

        let _ = writeln!(
            std::io::stderr(),
            "[result] mode={mode} exclusive={} delivered=yes latency={:?} handler={:?}",
            self::yes_or_no(subject.exclusive),
            event.press().at().duration_since(pressed),
            ran_at.duration_since(pressed),
        );

        if mode == "exclusive" && !subject.exclusive {
            let _ = writeln!(
                std::io::stderr(),
                "\n*** NOT EXERCISED: Windows refused the exclusive transition, so this \
                 run covered a borderless window over the display and says nothing about \
                 exclusive fullscreen. tests/capture/README.md has what decides it. ***\n",
            );
            assert!(
                std::env::var_os(REQUIRE_HOTKEYS).is_none_or(|value| value.is_empty()),
                "{REQUIRE_HOTKEYS} is set, so this run had to reach exclusive fullscreen \
                 and Windows refused the transition",
            );
        }

        drop(subject);
        service.stop();
    }
}

/// For the `exclusive=` and `subject-has-foreground=` fields, which follow the
/// test applications' own vocabulary (`docs/testing.md`).
fn yes_or_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
