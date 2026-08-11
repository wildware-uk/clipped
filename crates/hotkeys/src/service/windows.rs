//! The Windows half: `RegisterHotKey`, and the message loop the presses arrive
//! on.
//!
//! Every Win32 call in this crate is in this file.
//!
//! # Why `RegisterHotKey` and not a keyboard hook
//!
//! The alternative is `SetWindowsHookEx(WH_KEYBOARD_LL, …)`, which sees *every*
//! keystroke on the machine — including the ones typed into a password box —
//! and is what a keylogger is built from. Anti-cheat software treats a process
//! that installs one as hostile, and it is the same judgement a user would make
//! if they looked. AGENTS.md section 34 is explicit that user account safety
//! comes before richer behaviour, and a recorder that gets somebody banned from
//! their game has failed at something more important than a hotkey.
//!
//! `RegisterHotKey` tells Windows about *one combination* and receives nothing
//! else. It is the documented, supported route, it needs no elevation, and it
//! delivers while another application has the foreground — which is the whole
//! requirement. `docs/hotkeys.md` is the full argument, including what this
//! choice cannot do.
//!
//! # Threading
//!
//! `RegisterHotKey` with a null window posts `WM_HOTKEY` to *the calling
//! thread's* message queue, and `UnregisterHotKey` must be called from the same
//! thread. So one thread owns all of it: it registers, pumps messages, and
//! unregisters on the way out. Nothing else in the process may call either
//! function, and nothing else has to — the thread is started, addressed by
//! thread identifier and stopped by [`HotkeyLoop`].
//!
//! That thread runs no handlers. It hands each press to
//! [`Dispatcher::press`](crate::dispatch::Dispatcher::press), which is a lookup
//! and a non-blocking send, and goes straight back to `GetMessageW`. A handler
//! that takes a second cannot cost it a press.

use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_APP, WM_HOTKEY, WM_USER,
};

use super::{ConflictCause, HotkeyError, RegistrationOutcome};
use crate::action::HotkeyAction;
use crate::bindings::Bindings;
use crate::dispatch::{Dispatcher, HotkeyPress};
use crate::hotkey::Hotkey;

/// The message that ends the loop.
///
/// `WM_APP` and above is the range Windows documents as an application's own,
/// so this cannot collide with a message Windows itself defines.
const STOP: u32 = WM_APP + 1;

/// The thread that owns every registration, and its address.
#[derive(Debug)]
pub(super) struct HotkeyLoop {
    /// Where [`STOP`] is posted. Valid for as long as the thread is alive,
    /// which is until [`stop`](Self::stop) joins it.
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

/// What the loop thread reports back once it has registered everything.
struct Ready {
    thread_id: u32,
    outcomes: Vec<RegistrationOutcome>,
}

impl HotkeyLoop {
    /// Starts the thread, registers `bindings` on it, and returns once every
    /// registration has been attempted.
    ///
    /// Returning only after registration is what makes the report the caller
    /// gets complete, and what stops a press arriving before the caller has
    /// been told whether the key even works.
    pub(super) fn start(
        bindings: &Bindings,
        dispatcher: Dispatcher,
    ) -> Result<(Self, Vec<RegistrationOutcome>), HotkeyError> {
        let wanted: Vec<(HotkeyAction, Hotkey)> = bindings.iter().collect();
        let (ready, started) = mpsc::channel();

        let handle = thread::Builder::new()
            .name("clipped-hotkeys".to_owned())
            .spawn(move || run(&wanted, dispatcher, &ready))
            .map_err(HotkeyError::ThreadStart)?;

        match started.recv() {
            Ok(Ready {
                thread_id,
                outcomes,
            }) => Ok((
                Self {
                    thread_id,
                    handle: Some(handle),
                },
                outcomes,
            )),
            // The thread ended without reporting, which it has no path to do
            // except by panicking. Swallowing that would leave the caller with
            // a service that registers nothing and explains nothing.
            Err(_) => match handle.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => unreachable!("the loop reports before it returns"),
            },
        }
    }

    /// Ends the loop, which unregisters everything and drains the handlers.
    pub(super) fn stop(mut self) {
        // SAFETY: `PostThreadMessageW` needs a thread identifier and a message,
        // neither of which is a pointer. The thread is still alive — nothing
        // but this function joins it, and it is called once — and it has a
        // message queue, because it created one before reporting the identifier
        // this call uses.
        let posted = unsafe { PostThreadMessageW(self.thread_id, STOP, WPARAM(0), LPARAM(0)) };
        if let Err(error) = posted {
            // The thread is gone. Joining below still reaps it, and the
            // registrations went with it, so there is nothing to recover -
            // but a stop that did not stop anything should not be silent.
            tracing::warn!(%error, "the hotkey thread could not be asked to stop");
        }

        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                tracing::error!("the hotkey thread panicked; its combinations were released");
            }
        }
    }
}

impl Drop for HotkeyLoop {
    /// Only reached if [`stop`](Self::stop) was not called — a panic between
    /// starting the loop and building the service. The thread must not outlive
    /// its owner in that case either.
    fn drop(&mut self) {
        if self.handle.is_some() {
            let orphaned = Self {
                thread_id: self.thread_id,
                handle: self.handle.take(),
            };
            orphaned.stop();
        }
    }
}

/// The loop thread, start to finish.
fn run(wanted: &[(HotkeyAction, Hotkey)], dispatcher: Dispatcher, ready: &Sender<Ready>) {
    create_message_queue();

    let mut registered: Vec<(HotkeyAction, Hotkey)> = Vec::new();
    let mut outcomes = Vec::with_capacity(wanted.len());
    for &(action, hotkey) in wanted {
        let cause = register(action, hotkey).err();
        if cause.is_none() {
            registered.push((action, hotkey));
        }
        outcomes.push(RegistrationOutcome {
            action,
            hotkey,
            cause,
        });
    }

    // SAFETY: `GetCurrentThreadId` takes nothing and cannot fail.
    let thread_id = unsafe { GetCurrentThreadId() };

    if ready
        .send(Ready {
            thread_id,
            outcomes,
        })
        .is_err()
    {
        // The caller gave up between spawning this thread and hearing back.
        // Give the combinations straight back rather than holding them for a
        // service nobody has.
        unregister(&registered);
        return;
    }

    pump(&registered, &dispatcher);
    unregister(&registered);
    dispatcher.shutdown();
}

/// Forces this thread to have a message queue.
///
/// `PostThreadMessageW` fails against a thread that has never called a message
/// function, and the caller learns this thread's identifier the moment
/// registration finishes — so the queue has to exist before that, not at the
/// first `GetMessageW`.
fn create_message_queue() {
    let mut message = MSG::default();
    // SAFETY: `message` is a live, correctly sized `MSG` for the duration of
    // the call. `PM_NOREMOVE` means nothing is taken off the queue, and the
    // `WM_USER` filter means nothing is matched; the call is made for its
    // documented side effect of creating the queue.
    // Whether a message matched is not the question — the filter is chosen so
    // that none can — so the return value is deliberately discarded.
    let _matched = unsafe { PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE) };
}

/// Asks Windows for one combination.
///
/// `MOD_NOREPEAT` is not in the `MOD_*` set a user chooses from: it is set on
/// every registration and is not configurable. Without it, holding `Ctrl+F10`
/// down produces a `WM_HOTKEY` at the keyboard's repeat rate — thirty saved
/// replays for one long press. It is taken from the `windows` crate rather than
/// written as a literal, because this file is `cfg(windows)`-only, so the reason
/// the constants in `crate::hotkey` are literals (that crate module compiles off
/// Windows) does not apply here.
fn register(action: HotkeyAction, hotkey: Hotkey) -> Result<(), ConflictCause> {
    let modifiers = HOT_KEY_MODIFIERS(hotkey.modifiers().bits() | MOD_NOREPEAT.0);
    let identifier = identifier(action);

    // SAFETY: no pointers are involved. A null window is the documented way to
    // have `WM_HOTKEY` posted to this thread's queue instead of to a window,
    // and this thread is the one that pumps that queue and the one that
    // unregisters. The identifier is the action's index, which is unique and
    // well inside the 0x0000-0xBFFF a thread may use.
    let outcome =
        unsafe { RegisterHotKey(None, identifier, modifiers, hotkey.key().virtual_key()) };

    match outcome {
        Ok(()) => {
            tracing::info!(action = action.name(), hotkey = %hotkey, "hotkey registered");
            Ok(())
        }
        Err(error) => {
            let code = error.code().0 as u32;
            tracing::warn!(
                action = action.name(),
                hotkey = %hotkey,
                code = format_args!("{code:#010X}"),
                "hotkey could not be registered",
            );
            Err(
                if code
                    == windows::core::HRESULT::from_win32(ERROR_HOTKEY_ALREADY_REGISTERED.0).0
                        as u32
                {
                    ConflictCause::AlreadyRegistered
                } else {
                    ConflictCause::Refused { code }
                },
            )
        }
    }
}

/// Gives every registered combination back.
///
/// **No test can prove this call does anything**, and that is worth writing
/// down rather than discovering later: Windows releases a thread's hotkeys when
/// the thread terminates, and this thread terminates immediately afterwards. A
/// mutation that skipped every call here left
/// `stopping_the_service_gives_its_combinations_back` passing, because the
/// combination *was* available again — Windows had released it.
///
/// It is here anyway, for two reasons. It releases at a defined point in the
/// shutdown rather than at thread teardown, which is what makes a stop
/// observable in a log; and it is the call a rebind will need
/// ([issue #233](https://github.com/wildware-uk/clipped/issues/233)), where the
/// thread does *not* end and Windows would release nothing.
fn unregister(registered: &[(HotkeyAction, Hotkey)]) {
    for &(action, hotkey) in registered {
        // SAFETY: the same null window and the same identifier the
        // registration used, from the thread that made it, which is what
        // `UnregisterHotKey` requires.
        if let Err(error) = unsafe { UnregisterHotKey(None, identifier(action)) } {
            // Nothing can be done about it, and it must not stop the rest being
            // released - but a combination Windows still thinks Clipped owns is
            // exactly the state that produces a mysterious conflict later.
            tracing::warn!(action = action.name(), hotkey = %hotkey, %error, "hotkey could not be released");
        }
    }
}

/// The identifier a combination is registered under: the action's position in
/// [`crate::ACTIONS`], which is unique by construction.
fn identifier(action: HotkeyAction) -> i32 {
    i32::try_from(action.index()).unwrap_or(i32::MAX)
}

/// Receives presses until asked to stop.
fn pump(registered: &[(HotkeyAction, Hotkey)], dispatcher: &Dispatcher) {
    loop {
        let mut message = MSG::default();
        // SAFETY: `message` is a live `MSG`. A null window and a zero filter
        // ask for every message posted to this thread, which is what a thread
        // with no windows of its own wants.
        let received = unsafe { GetMessageW(&mut message, None, 0, 0) };
        let at = Instant::now();

        match received.0 {
            -1 => {
                // Documented as "an error occurred". There is no error a
                // message loop can recover from here, and spinning on it would
                // burn a core.
                tracing::error!(
                    error = %windows::core::Error::from_thread(),
                    "the hotkey message loop failed; hotkeys have stopped",
                );
                return;
            }
            0 => return, // WM_QUIT.
            _ => {}
        }

        if message.message == STOP {
            return;
        }
        if message.message != WM_HOTKEY {
            continue;
        }

        // `wParam` is the identifier the combination was registered with.
        let Some(action) = HotkeyAction::from_index(message.wParam.0) else {
            tracing::warn!(
                identifier = message.wParam.0,
                "a WM_HOTKEY arrived for an identifier this build never registered",
            );
            continue;
        };
        let Some(&(_, hotkey)) = registered.iter().find(|(bound, _)| *bound == action) else {
            tracing::warn!(
                action = action.name(),
                "a WM_HOTKEY arrived for an action whose registration was refused",
            );
            continue;
        };

        dispatcher.press(HotkeyPress::new(action, hotkey, at));
    }
}
