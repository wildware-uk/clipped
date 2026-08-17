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
//!
//! Rebinding ([`HotkeyLoop::rebind`]) is the same thread affinity applied to a
//! second kind of request: a caller cannot call `RegisterHotKey` for this
//! thread from anywhere else, so a rebind is posted to the loop as a message —
//! [`REBIND`] — carrying the request on the heap, and the caller's thread
//! blocks on a reply the loop thread sends back once it has acted on it.

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
use crate::action::{HotkeyAction, ACTIONS};
use crate::bindings::Bindings;
use crate::dispatch::{Dispatcher, HotkeyPress};
use crate::hotkey::Hotkey;

/// The message that ends the loop.
///
/// `WM_APP` and above is the range Windows documents as an application's own,
/// so this cannot collide with a message Windows itself defines.
const STOP: u32 = WM_APP + 1;

/// The message that carries a rebind request. See [`HotkeyLoop::rebind`].
const REBIND: u32 = WM_APP + 2;

/// The lowest identifier a rebind's probe may claim.
///
/// Every identifier below this is one `run`'s initial pass may give an action
/// in [`ACTIONS`] — one per action, by position, as
/// [`HotkeyAction::index`](crate::action::HotkeyAction::index) documents — so
/// starting a rebind's own identifiers here means a probe can never collide
/// with the very registration it exists to leave untouched until the new
/// combination is proven free.
const SCRATCH_ID_START: i32 = ACTIONS.len() as i32;

/// The top of the range `RegisterHotKey` documents as valid for a null window:
/// `0x0000` through `0xBFFF`.
const SCRATCH_ID_END: i32 = 0xBFFF;

/// The thread that owns every registration, and its address.
#[derive(Debug)]
pub(super) struct HotkeyLoop {
    /// Where [`STOP`] and [`REBIND`] are posted. Valid for as long as the
    /// thread is alive, which is until [`stop`](Self::stop) joins it.
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

/// What the loop thread reports back once it has registered everything.
struct Ready {
    thread_id: u32,
    outcomes: Vec<RegistrationOutcome>,
}

/// One [`HotkeyLoop::rebind`] request, boxed and posted to the loop thread as
/// an opaque `LPARAM`.
///
/// A `Sender` rather than a `SyncSender` because exactly one reply is ever
/// sent and the caller is already blocked in `recv`, so nothing is gained by
/// bounding a channel that never holds more than one message.
struct RebindRequest {
    action: HotkeyAction,
    hotkey: Option<Hotkey>,
    reply: Sender<Result<(), Option<ConflictCause>>>,
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

    /// Unregisters `action`'s current combination, if it has one, and
    /// registers `hotkey` in its place — or, with `hotkey` of [`None`],
    /// unregisters it and registers nothing.
    ///
    /// `RegisterHotKey` and `UnregisterHotKey` are bound to the calling
    /// thread, and that thread is the loop's, not this one's (see the module
    /// doc comment). So this posts [`REBIND`] carrying the request on the
    /// heap and blocks on a reply the loop thread sends back after
    /// [`handle_rebind`] has acted on it — the same shape [`stop`](Self::stop)
    /// uses for [`STOP`], with a reply added because a rebind, unlike a stop,
    /// has an outcome the caller needs.
    ///
    /// A refusal never costs `action` the combination it already had:
    /// [`handle_rebind`] proves `hotkey` is free before releasing the
    /// previous registration, so a caller reading `Err` from this knows the
    /// old combination, if there was one, is still registered.
    ///
    /// # Errors
    ///
    /// `Err(Some(cause))` if Windows refused `hotkey`. `Err(None)` if the
    /// loop thread could not be reached at all — either the post itself
    /// failed, or the thread ended before replying, which only the
    /// `GetMessageW` failure in [`pump`] can cause outside a
    /// [`stop`](Self::stop) that this call raced.
    pub(super) fn rebind(
        &self,
        action: HotkeyAction,
        hotkey: Option<Hotkey>,
    ) -> Result<(), Option<ConflictCause>> {
        let (reply, response) = mpsc::channel();
        let request = Box::new(RebindRequest {
            action,
            hotkey,
            reply,
        });
        let pointer = Box::into_raw(request);

        // SAFETY: `pointer` was produced by `Box::into_raw` immediately
        // above and is handed to the loop thread as an opaque `LPARAM`.
        // `REBIND` is handled in exactly one place, `handle_rebind`, which
        // reclaims it with `Box::from_raw` exactly once — so ownership moves
        // from this thread to that one without ever being shared.
        let posted = unsafe {
            PostThreadMessageW(self.thread_id, REBIND, WPARAM(0), LPARAM(pointer as isize))
        };
        if let Err(error) = posted {
            // The message never reached the loop thread, so `handle_rebind`
            // will never run to reclaim the box. Reclaim and drop it here
            // instead of leaking it.
            // SAFETY: `pointer` still points at the box `Box::into_raw`
            // produced above, and the failed post means nothing else has
            // touched or freed it.
            drop(unsafe { Box::from_raw(pointer) });
            tracing::error!(
                action = action.name(),
                %error,
                "the hotkey thread could not be asked to rebind",
            );
            return Err(None);
        }

        // `handle_rebind` always sends a reply, whether the rebind succeeded
        // or Windows refused it. A disconnected channel here means the loop
        // thread ended without running it at all.
        response.recv().unwrap_or_else(|_| {
            tracing::error!(
                action = action.name(),
                "the hotkey thread ended before answering a rebind request",
            );
            Err(None)
        })
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

    let mut registered: Vec<(i32, HotkeyAction, Hotkey)> = Vec::new();
    let mut outcomes = Vec::with_capacity(wanted.len());
    for &(action, hotkey) in wanted {
        let identifier = canonical_identifier(action);
        let cause = register(identifier, action, hotkey).err();
        if cause.is_none() {
            registered.push((identifier, action, hotkey));
        }
        outcomes.push(RegistrationOutcome { action, cause });
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
        unregister_all(&registered);
        return;
    }

    let mut next_scratch_id = SCRATCH_ID_START;
    pump(&mut registered, &mut next_scratch_id, &dispatcher);
    unregister_all(&registered);
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

/// Asks Windows for one combination under `identifier`.
///
/// `MOD_NOREPEAT` is not in the `MOD_*` set a user chooses from: it is set on
/// every registration and is not configurable. Without it, holding `Ctrl+F10`
/// down produces a `WM_HOTKEY` at the keyboard's repeat rate — thirty saved
/// replays for one long press. It is taken from the `windows` crate rather than
/// written as a literal, because this file is `cfg(windows)`-only, so the reason
/// the constants in `crate::hotkey` are literals (that crate module compiles off
/// Windows) does not apply here.
fn register(identifier: i32, action: HotkeyAction, hotkey: Hotkey) -> Result<(), ConflictCause> {
    let modifiers = HOT_KEY_MODIFIERS(hotkey.modifiers().bits() | MOD_NOREPEAT.0);

    // SAFETY: no pointers are involved. A null window is the documented way to
    // have `WM_HOTKEY` posted to this thread's queue instead of to a window,
    // and this thread is the one that pumps that queue and the one that
    // unregisters. `identifier` is either an action's fixed position in
    // `ACTIONS` (the initial registration in `run`) or a scratch identifier
    // `next_scratch` has proven is not already in `registered` (a rebind's
    // probe) — either way it is unique among this thread's live registrations
    // and well inside the 0x0000-0xBFFF a thread may use.
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
/// observable in a log; and it is the same call [`rebind_action`] uses to give
/// up one binding's *previous* combination while the thread carries on running
/// ([issue #233](https://github.com/wildware-uk/clipped/issues/233)).
fn unregister_all(registered: &[(i32, HotkeyAction, Hotkey)]) {
    for &(identifier, action, hotkey) in registered {
        unregister_one(identifier, action, hotkey);
    }
}

/// Gives one registered combination back.
///
/// # Panics
///
/// Never; a failure is logged and otherwise ignored (see [`unregister_all`]).
fn unregister_one(identifier: i32, action: HotkeyAction, hotkey: Hotkey) {
    // SAFETY: `identifier` is one this thread registered with `register` —
    // either the action's canonical identifier from `run`'s initial pass or a
    // scratch identifier from a rebind — and this is the thread that
    // registered it, which is what `UnregisterHotKey` requires.
    if let Err(error) = unsafe { UnregisterHotKey(None, identifier) } {
        // Nothing can be done about it, and it must not stop the rest being
        // released - but a combination Windows still thinks Clipped owns is
        // exactly the state that produces a mysterious conflict later.
        tracing::warn!(action = action.name(), hotkey = %hotkey, %error, "hotkey could not be released");
    }
}

/// The identifier `run`'s initial pass registers `action` under: its position
/// in [`ACTIONS`], which is unique by construction.
///
/// This is only the *starting* identifier. [`rebind_action`] moves a
/// rebound action onto a scratch identifier instead of reusing this one, so
/// after a rebind an action's live identifier is no longer necessarily its
/// position in `ACTIONS` — [`pump`] matches a `WM_HOTKEY` against whatever
/// `registered` actually holds rather than recomputing this.
fn canonical_identifier(action: HotkeyAction) -> i32 {
    i32::try_from(action.index()).unwrap_or(i32::MAX)
}

/// The next identifier a rebind's probe may use, skipping any identifier
/// currently in `registered`.
///
/// Wrapping from [`SCRATCH_ID_END`] back to [`SCRATCH_ID_START`] would take
/// billions of rebinds in one process to matter, and the skip means it cannot
/// matter even then: at most `ACTIONS.len()` entries are ever live at once, so
/// the loop below claims a free identifier in at most that many steps.
fn next_scratch(next: &mut i32, registered: &[(i32, HotkeyAction, Hotkey)]) -> i32 {
    loop {
        let candidate = *next;
        *next = if candidate >= SCRATCH_ID_END {
            SCRATCH_ID_START
        } else {
            candidate + 1
        };
        if registered
            .iter()
            .all(|&(identifier, _, _)| identifier != candidate)
        {
            return candidate;
        }
    }
}

/// Handles one [`RebindRequest`], on the loop thread and nowhere else.
///
/// Reclaims the request, acts on it and replies — the whole of what
/// `HotkeyLoop::rebind` is waiting for.
fn handle_rebind(
    registered: &mut Vec<(i32, HotkeyAction, Hotkey)>,
    next_scratch_id: &mut i32,
    lparam: LPARAM,
) {
    // SAFETY: `lparam.0` is a pointer `HotkeyLoop::rebind` produced with
    // `Box::into_raw` and posted as this message's `LPARAM`. `REBIND` is
    // handled only here, so this is the one place that pointer is read, and
    // `Box::from_raw` here is the one place it is reclaimed.
    let request = unsafe { Box::from_raw(lparam.0 as *mut RebindRequest) };
    let RebindRequest {
        action,
        hotkey,
        reply,
    } = *request;

    let outcome = rebind_action(registered, next_scratch_id, action, hotkey);

    // `HotkeyLoop::rebind` always waits on `response.recv()` before this
    // fires, so a disconnected channel here would mean the caller gave up —
    // which nothing in this crate does — and is not this thread's problem
    // either way.
    let _ = reply.send(outcome);
}

/// Points `action` at `hotkey`, or at nothing if `hotkey` is [`None`],
/// updating `registered` to match whatever Windows ends up actually holding.
///
/// The order is what keeps a refusal cheap: `hotkey` is registered — under a
/// scratch identifier from [`next_scratch`], never `action`'s own — *before*
/// `action`'s previous registration is touched. If Windows refuses it, this
/// returns having changed nothing, and the caller still holds whatever it held
/// before. Only once the new combination is proven free is the old one
/// released and the new one kept as `action`'s live registration.
fn rebind_action(
    registered: &mut Vec<(i32, HotkeyAction, Hotkey)>,
    next_scratch_id: &mut i32,
    action: HotkeyAction,
    hotkey: Option<Hotkey>,
) -> Result<(), Option<ConflictCause>> {
    let previous = registered.iter().position(|&(_, bound, _)| bound == action);

    let Some(wanted) = hotkey else {
        if let Some(index) = previous {
            let (identifier, _, old_hotkey) = registered.remove(index);
            unregister_one(identifier, action, old_hotkey);
        }
        return Ok(());
    };

    if let Some(index) = previous {
        let (_, _, old_hotkey) = registered[index];
        if old_hotkey == wanted {
            // Already bound to it. Asking Windows to register a combination
            // this thread already holds under a different identifier would
            // itself be refused as taken — by this thread — so there is
            // nothing to do.
            return Ok(());
        }
    }

    let scratch_id = next_scratch(next_scratch_id, registered);
    register(scratch_id, action, wanted).map_err(Some)?;

    if let Some(index) = previous {
        let (identifier, _, old_hotkey) = registered.remove(index);
        unregister_one(identifier, action, old_hotkey);
    }
    registered.push((scratch_id, action, wanted));
    Ok(())
}

/// Receives presses and rebind requests until asked to stop.
fn pump(
    registered: &mut Vec<(i32, HotkeyAction, Hotkey)>,
    next_scratch_id: &mut i32,
    dispatcher: &Dispatcher,
) {
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
        if message.message == REBIND {
            handle_rebind(registered, next_scratch_id, message.lParam);
            continue;
        }
        if message.message != WM_HOTKEY {
            continue;
        }

        // `wParam` is the identifier the combination was registered with.
        let identifier = i32::try_from(message.wParam.0).unwrap_or(i32::MAX);
        let Some(&(_, action, hotkey)) = registered
            .iter()
            .find(|&&(candidate, _, _)| candidate == identifier)
        else {
            tracing::warn!(
                identifier,
                "a WM_HOTKEY arrived for an identifier this build never registered",
            );
            continue;
        };

        dispatcher.press(HotkeyPress::new(action, hotkey, at));
    }
}
