//! Global hotkeys: combinations that reach Clipped while a game has the
//! foreground, and the threads their handlers run on.
//!
//! # Responsibilities
//!
//! - The set of actions a hotkey can trigger (SPEC.md section 34) and the
//!   combinations bound to them, defaulting to the two SPEC.md names.
//! - Registering those combinations with Windows, and reporting each one that
//!   another application already owns.
//! - Delivering a press to a handler without making any other thread wait.
//!
//! # Not responsible for
//!
//! Doing any of it. Saving a replay, taking a screenshot and starting a
//! recording all live elsewhere; this crate calls a handler the owning process
//! supplies. An action with no handler is reported as [`Unhandled`] rather than
//! quietly doing nothing (AGENTS.md section 54). It also does not own the
//! configuration screen — [issue
//! #54](https://github.com/wildware-uk/clipped/issues/54) builds that, out of
//! [`Registration`] and [`Bindings`], and it does not persist anything: the
//! configuration API is [issue
//! #108](https://github.com/wildware-uk/clipped/issues/108).
//!
//! # Position in the architecture
//!
//! Layer 0, beside `clipped-windows` and `clipped-ipc`. It depends on no other
//! crate in the workspace, deliberately: a hotkey service that reached into the
//! recording engine could not be linked by the desktop application, and the
//! direction of the dependency is the wrong way round anyway — the process that
//! owns a session plugs a handler in here, not the other way about.
//!
//! # The mechanism, and what it cannot do
//!
//! `RegisterHotKey`. The alternative — a low-level keyboard hook — sees every
//! keystroke on the machine, is what a keylogger is built from, and is treated
//! as hostile by anti-cheat software (AGENTS.md section 34). `RegisterHotKey`
//! tells Windows about one combination and receives nothing else.
//!
//! Being honest about the price of that choice, because it is the criterion the
//! whole feature is judged on:
//!
//! | Case | Does a hotkey fire? |
//! | --- | --- |
//! | Borderless-fullscreen game in the foreground | Yes — measured, see `docs/hotkeys.md` |
//! | Exclusive-fullscreen (DXGI) game in the foreground | Yes — measured, see `docs/hotkeys.md` |
//! | Another application already owns the combination | **No.** Reported as a [`Conflict`], never silently |
//! | An elevated application has the foreground and Clipped is not elevated | Not measured. Windows' integrity rules apply |
//! | The secure desktop: the UAC prompt, the lock screen, Ctrl+Alt+Del | **No.** Nothing but Windows runs there |
//! | A game that acquires the keyboard through legacy exclusive DirectInput | Not measured; historically this suppresses hotkeys |
//!
//! `docs/hotkeys.md` carries the evidence for the measured rows and the
//! reasoning for the rest.
//!
//! # Threading
//!
//! Three kinds of thread, and one rule.
//!
//! ```text
//!   caller's thread          hotkey thread              one thread per
//!   ───────────────          ─────────────              handled action
//!   HotkeyService::start ──▶ RegisterHotKey             ─────────────────
//!                            GetMessageW  ──WM_HOTKEY──▶ press ──▶ handler
//!   HotkeyService::stop  ──▶ UnregisterHotKey
//! ```
//!
//! **A hotkey press never makes another thread wait.** The hotkey thread does a
//! map lookup and a non-blocking send and returns to `GetMessageW`; a handler
//! that takes a second to save a replay delays neither the next press nor any
//! other action's handler, and nothing in this crate is ever called from a
//! capture or encode thread (AGENTS.md section 20). `src/dispatch.rs` documents
//! the queues, and what happens when one fills.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::mpsc::RecvTimeoutError;
//! use std::time::Duration;
//!
//! use clipped_hotkeys::{Bindings, Handlers, HotkeyAction, HotkeyService, PressOutcome};
//!
//! // Ctrl+F10 saves a replay and Ctrl+F9 bookmarks, per SPEC.md.
//! let bindings = Bindings::defaults();
//!
//! // Only the actions this process can actually perform get a handler. The
//! // rest report themselves as unhandled when pressed.
//! let handlers = Handlers::new().on(HotkeyAction::ToggleRecording, |press| {
//!     println!("{} was pressed", press.hotkey());
//! });
//!
//! let (hotkeys, events) = HotkeyService::start(&bindings, handlers)?;
//!
//! // A combination another application owns is reported, not swallowed.
//! for conflict in hotkeys.registration().conflicts() {
//!     eprintln!("{conflict}");
//! }
//!
//! match events.recv_timeout(Duration::from_secs(60)) {
//!     Ok(event) => println!("{event}"),
//!     Err(RecvTimeoutError::Timeout) => println!("nobody pressed anything"),
//!     Err(RecvTimeoutError::Disconnected) => println!("the service stopped"),
//! }
//!
//! hotkeys.stop();
//! # Ok::<(), clipped_hotkeys::HotkeyError>(())
//! ```
//!
//! # What exists today
//!
//! The service, its registration report and the dispatch model
//! ([issue #39](https://github.com/wildware-uk/clipped/issues/39)), and the
//! caller: `clipped-recorder serve` starts one and turns a press into the
//! command the desktop application would have sent
//! ([issue #232](https://github.com/wildware-uk/clipped/issues/232),
//! `apps/recorder/src/hotkeys.rs`,
//! [ADR 0009](../../../docs/adr/0009-the-recorder-registers-global-hotkeys.md)).
//! The screen that *binds* a combination is
//! [issue #54](https://github.com/wildware-uk/clipped/issues/54), and changing
//! one without restarting the service is
//! [issue #233](https://github.com/wildware-uk/clipped/issues/233).

mod action;
mod bindings;
mod dispatch;
mod hotkey;
mod service;

pub use action::{HotkeyAction, PlannedSubsystem, ACTIONS};
pub use bindings::{BindError, Bindings};
pub use dispatch::{
    Handlers, HotkeyEvent, HotkeyPress, PressOutcome, Unhandled, PRESSES_PER_ACTION,
};
pub use hotkey::{Hotkey, InvalidHotkey, Key, Modifiers};
pub use service::{
    ActionStatus, BindingState, Conflict, ConflictCause, HotkeyError, HotkeyService, Registration,
};
