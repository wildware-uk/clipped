//! The service itself: what is registered, what Windows refused, and what a
//! press turns into.
//!
//! The Win32 calls are all in `service/windows.rs` — one file, so that a future
//! port has a marked surface and the rest of the crate stays free of platform
//! conditionals (AGENTS.md section 5). What is here is the part with
//! no operating system in it: the report a settings screen renders, and the
//! ownership rules that stop a registration outliving the service that made it.

#[cfg_attr(not(windows), path = "unsupported.rs")]
#[cfg_attr(windows, path = "windows.rs")]
mod platform;

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::Receiver;

use crate::action::{HotkeyAction, ACTIONS};
use crate::bindings::Bindings;
use crate::dispatch::{Dispatcher, Handlers, HotkeyEvent};
use crate::hotkey::Hotkey;

/// Global hotkeys, registered with Windows and dispatched to handlers.
///
/// Starting one registers every binding it was given, reports what happened to
/// each (see [`registration`](Self::registration)) and then delivers presses
/// until it is dropped or [`stop`](Self::stop)ped. A registration belongs to
/// the service: dropping it gives every combination back to the system, so
/// there is no path — a panic included — on which Clipped keeps holding a
/// combination it is no longer listening for (AGENTS.md section 58).
#[derive(Debug)]
pub struct HotkeyService {
    registration: Registration,
    /// [`None`] once [`stop`](Self::stop) has taken it.
    running: Option<platform::HotkeyLoop>,
}

impl HotkeyService {
    /// Registers `bindings` and starts delivering presses to `handlers`.
    ///
    /// A combination another application already owns does not stop the others
    /// being registered: every binding is attempted, and the outcome of each is
    /// in [`registration`](Self::registration). That is deliberate — one
    /// conflicting hotkey should cost the user that hotkey, not all of them.
    ///
    /// The returned [`Receiver`] carries one [`HotkeyEvent`] per press,
    /// produced on the thread that received it. Reading it is optional; a
    /// caller that drops it loses the events and nothing else.
    ///
    /// # Errors
    ///
    /// [`HotkeyError::Unsupported`] off Windows, and
    /// [`HotkeyError::ThreadStart`] if the message-loop thread could not be
    /// created. Neither is a conflict: a conflict is a successful start with a
    /// [`BindingState::Conflict`] in the registration.
    pub fn start(
        bindings: &Bindings,
        handlers: Handlers,
    ) -> Result<(Self, Receiver<HotkeyEvent>), HotkeyError> {
        let handled: BTreeSet<HotkeyAction> = handlers.handled().collect();
        let (dispatcher, events) = Dispatcher::start(handlers);

        let (running, outcomes) = platform::HotkeyLoop::start(bindings, dispatcher)?;

        // Every binding is attempted and reported, so the outcomes that carry a
        // cause are exactly the combinations Windows would not give this
        // process; the rest of the report is a function of what was asked for.
        let refused: BTreeMap<HotkeyAction, ConflictCause> = outcomes
            .iter()
            .filter_map(|outcome| outcome.cause.map(|cause| (outcome.action, cause)))
            .collect();

        let registration = Registration::of(bindings, &handled, &refused);
        for conflict in registration.conflicts() {
            tracing::warn!(
                action = conflict.action.name(),
                hotkey = %conflict.hotkey,
                "{conflict}",
            );
        }

        Ok((
            Self {
                registration,
                running: Some(running),
            },
            events,
        ))
    }

    /// What became of every action: what it is bound to, whether Windows
    /// accepted it, and whether anything in this process handles it.
    ///
    /// This is the whole of what the hotkey configuration screen ([issue
    /// #54](https://github.com/wildware-uk/clipped/issues/54)) needs to render
    /// itself, and it is a plain value: no callbacks, no polling.
    #[must_use]
    pub const fn registration(&self) -> &Registration {
        &self.registration
    }

    /// Gives every combination back to Windows and waits for the handler that
    /// is running, if any.
    ///
    /// Waiting is the point rather than an oversight: a replay being written
    /// when the user quits should finish being written (AGENTS.md section 17).
    /// It happens on the caller's thread, never on a capture thread.
    pub fn stop(mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

impl Drop for HotkeyService {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

/// One binding's fate, and one row of the configuration screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionStatus {
    action: HotkeyAction,
    binding: Option<Hotkey>,
    state: BindingState,
    handled: bool,
}

impl ActionStatus {
    /// The action.
    #[must_use]
    pub const fn action(&self) -> HotkeyAction {
        self.action
    }

    /// What it is bound to, if anything.
    #[must_use]
    pub const fn binding(&self) -> Option<Hotkey> {
        self.binding
    }

    /// Whether Windows accepted the binding, refused it, or was never asked.
    #[must_use]
    pub const fn state(&self) -> &BindingState {
        &self.state
    }

    /// Whether anything in this process handles the action.
    ///
    /// A bound, registered hotkey with no handler still does nothing when
    /// pressed — it reports [`crate::Unhandled`] — and a screen that showed it
    /// as working would be lying (AGENTS.md section 27).
    #[must_use]
    pub const fn is_handled(&self) -> bool {
        self.handled
    }
}

/// What Windows said about one binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingState {
    /// The action has no combination, so nothing was registered.
    Unbound,
    /// Windows accepted it and presses are being delivered.
    Bound,
    /// Windows refused it. See [`Conflict`].
    Conflict(Conflict),
}

/// A combination Windows would not give Clipped.
///
/// The [`Display`](fmt::Display) is written to be shown to a user as it is
/// (AGENTS.md section 45): it names the hotkey, names the action that wanted
/// it, says who is likely to have it and says what to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    action: HotkeyAction,
    hotkey: Hotkey,
    cause: ConflictCause,
}

impl Conflict {
    /// The action that wanted the combination.
    #[must_use]
    pub const fn action(&self) -> HotkeyAction {
        self.action
    }

    /// The combination that could not be registered.
    #[must_use]
    pub const fn hotkey(&self) -> Hotkey {
        self.hotkey
    }

    /// What Windows said, for a log or a diagnostics bundle.
    #[must_use]
    pub const fn cause(&self) -> ConflictCause {
        self.cause
    }
}

impl fmt::Display for Conflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} could not be Clipped's shortcut for {}: ",
            self.hotkey, self.action,
        )?;

        match self.cause {
            // Clipped is named first because it is the one culprit the reader
            // would not think of and the one they can do something about
            // immediately. A recorder already running holds these combinations,
            // and the sentence that listed only overlays sent somebody looking
            // for a Discord setting to change (issue #232).
            ConflictCause::AlreadyRegistered => formatter.write_str(
                "another application already uses it, and that includes another copy of \
                 Clipped: only one process can hold a combination. Discord, Steam, \
                 NVIDIA's overlay and GeForce Experience all claim function-key \
                 combinations too, and Windows reserves a few of its own. Choose a \
                 different combination, or close the application that has this one and \
                 try again",
            ),
            ConflictCause::Refused { code } => write!(
                formatter,
                "Windows refused it ({code:#010X}). Choose a different combination; if no \
                 combination works, Clipped is running somewhere it cannot receive input, \
                 such as a service or a locked session",
            ),
        }
    }
}

impl core::error::Error for Conflict {}

/// Why Windows refused a combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictCause {
    /// `ERROR_HOTKEY_ALREADY_REGISTERED`: something else on this machine — or
    /// another copy of Clipped — holds the combination.
    AlreadyRegistered,
    /// Windows refused it for another reason, carrying the `HRESULT` for the
    /// diagnostics (AGENTS.md section 45: the code belongs in the log, not in
    /// the sentence the user reads).
    Refused {
        /// The `HRESULT` `RegisterHotKey` failed with.
        code: u32,
    },
}

/// Every action's status, in the order [`ACTIONS`] lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    statuses: Vec<ActionStatus>,
}

impl Registration {
    /// The report for `bindings`, given which actions have handlers and which
    /// combinations the system refused.
    ///
    /// This is the construction [`HotkeyService::start`] performs, and there is
    /// only this one. It is public because **a conflict is the case that
    /// matters and the one no test could otherwise reach**: whether Windows
    /// refuses `Ctrl`+`F10` depends on what else is installed on the machine, so
    /// a test that arranged a real one would be a test that passed or failed by
    /// accident. Everything built *from* a registration — the lines
    /// `clipped-recorder replay` prints before it starts recording, the rows the
    /// settings screen draws — has a conflict path, and this is how each of them
    /// is exercised without one (`docs/testing.md`).
    ///
    /// An action with no binding is [`BindingState::Unbound`] whatever `refused`
    /// says about it: nothing was registered for it, so nothing can have been
    /// refused.
    #[must_use]
    pub fn of(
        bindings: &Bindings,
        handled: &BTreeSet<HotkeyAction>,
        refused: &BTreeMap<HotkeyAction, ConflictCause>,
    ) -> Self {
        Self {
            statuses: ACTIONS
                .iter()
                .map(|&action| {
                    let binding = bindings.binding(action);
                    ActionStatus {
                        action,
                        binding,
                        state: match (binding, refused.get(&action)) {
                            (None, _) => BindingState::Unbound,
                            (Some(hotkey), Some(&cause)) => BindingState::Conflict(Conflict {
                                action,
                                hotkey,
                                cause,
                            }),
                            (Some(_), None) => BindingState::Bound,
                        },
                        handled: handled.contains(&action),
                    }
                })
                .collect(),
        }
    }

    /// Every action, bound or not.
    #[must_use]
    pub fn statuses(&self) -> &[ActionStatus] {
        &self.statuses
    }

    /// The status of one action.
    #[must_use]
    pub fn status(&self, action: HotkeyAction) -> Option<&ActionStatus> {
        self.statuses
            .iter()
            .find(|status| status.action() == action)
    }

    /// Every combination Windows refused.
    ///
    /// Empty is the normal case. A caller that shows nothing when this is
    /// non-empty has silently lost the user a hotkey, which is the failure this
    /// whole type exists to prevent.
    pub fn conflicts(&self) -> impl Iterator<Item = &Conflict> {
        self.statuses
            .iter()
            .filter_map(|status| match &status.state {
                BindingState::Conflict(conflict) => Some(conflict),
                BindingState::Bound | BindingState::Unbound => None,
            })
    }

    /// The actions whose combinations are registered and are being delivered.
    pub fn bound(&self) -> impl Iterator<Item = &ActionStatus> {
        self.statuses
            .iter()
            .filter(|status| matches!(status.state, BindingState::Bound))
    }
}

/// Why a hotkey service could not be started at all.
///
/// A refused *combination* is not one of these: that is a successful start with
/// a [`Conflict`] in the registration, because the other combinations still
/// work.
#[derive(Debug)]
pub enum HotkeyError {
    /// This platform has no global hotkeys. Clipped targets Windows; the crate
    /// still builds elsewhere so that its logic can be tested there.
    Unsupported,
    /// The message-loop thread could not be created.
    ThreadStart(std::io::Error),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => {
                formatter.write_str("global hotkeys need Windows, and this is not Windows")
            }
            Self::ThreadStart(error) => write!(
                formatter,
                "the thread global hotkeys are received on could not be started: {error}",
            ),
        }
    }
}

impl core::error::Error for HotkeyError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Unsupported => None,
            Self::ThreadStart(error) => Some(error),
        }
    }
}

/// What happened to one binding when it was registered.
///
/// The platform module's answer, before it becomes an [`ActionStatus`]. It does
/// not carry the combination: the action names it in the [`Bindings`] the
/// registration was asked for, and a second copy here would be a second thing
/// that could disagree with what was actually registered.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RegistrationOutcome {
    /// The action whose combination this is.
    pub(crate) action: HotkeyAction,
    /// [`None`] if Windows accepted it.
    pub(crate) cause: Option<ConflictCause>,
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{BindingState, Conflict, ConflictCause, Registration};
    use crate::action::HotkeyAction;
    use crate::bindings::Bindings;

    fn conflict(cause: ConflictCause) -> Conflict {
        Conflict {
            action: HotkeyAction::SaveReplay,
            hotkey: "Ctrl+F10".parse().expect("Ctrl+F10 is a hotkey"),
            cause,
        }
    }

    /// AGENTS.md section 45: what a user is shown has to be actionable.
    #[test]
    fn a_taken_combination_names_the_hotkey_the_action_and_what_to_do() {
        let message = conflict(ConflictCause::AlreadyRegistered).to_string();

        assert!(message.contains("Ctrl+F10"), "{message}");
        assert!(message.contains("Save replay"), "{message}");
        assert!(
            message.contains("another application already uses it"),
            "{message}",
        );
        assert!(
            message.contains("Choose a different combination"),
            "the message has to say what to do next: {message}",
        );
    }

    /// The one culprit a list of overlays cannot cover, because it is us.
    ///
    /// A recorder that finds a combination taken by another Clipped recorder —
    /// which is what a second `serve` on its own endpoint is — used to be told
    /// to go and look at Discord's settings. The cause this type documents has
    /// always included "another copy of Clipped"; the sentence the user reads
    /// is what did not.
    #[test]
    fn a_taken_combination_admits_that_clipped_itself_may_be_holding_it() {
        let message = conflict(ConflictCause::AlreadyRegistered).to_string();

        assert!(
            message.contains("another copy of Clipped"),
            "a user whose combination is held by Clipped must not be sent looking through \
             another application's settings: {message}",
        );
    }

    #[test]
    fn a_refusal_that_is_not_a_conflict_carries_the_code_for_the_log() {
        let message = conflict(ConflictCause::Refused { code: 0x8007_0005 }).to_string();

        assert!(message.contains("0x80070005"), "{message}");
        assert!(
            message.contains("Choose a different combination"),
            "{message}"
        );
    }

    /// The three states one report can hold at once, from the one construction
    /// `HotkeyService::start` uses.
    #[test]
    fn a_report_says_bound_refused_or_never_asked_for_each_action() {
        let registration = Registration::of(
            &Bindings::defaults(),
            &BTreeSet::from([HotkeyAction::SaveReplay]),
            &BTreeMap::from([(HotkeyAction::SaveReplay, ConflictCause::AlreadyRegistered)]),
        );

        let refused = registration
            .status(HotkeyAction::SaveReplay)
            .expect("every action has a row");
        assert!(matches!(refused.state(), BindingState::Conflict(_)));
        assert_eq!(refused.binding().expect("Ctrl+F10").to_string(), "Ctrl+F10");
        assert!(refused.is_handled());

        let accepted = registration
            .status(HotkeyAction::AddBookmark)
            .expect("every action has a row");
        assert_eq!(accepted.state(), &BindingState::Bound);
        assert!(
            !accepted.is_handled(),
            "only `save_replay` was given a handler here",
        );

        let never_asked = registration
            .status(HotkeyAction::TakeScreenshot)
            .expect("every action has a row");
        assert_eq!(never_asked.state(), &BindingState::Unbound);

        let conflicts: Vec<&Conflict> = registration.conflicts().collect();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].action(), HotkeyAction::SaveReplay);
        assert_eq!(conflicts[0].hotkey().to_string(), "Ctrl+F10");
        assert_eq!(
            registration.bound().count(),
            1,
            "a refused combination is not a bound one",
        );
    }

    /// Nothing was registered for it, so nothing can have been refused.
    #[test]
    fn an_unbound_action_is_never_reported_as_a_conflict() {
        let registration = Registration::of(
            &Bindings::empty(),
            &BTreeSet::new(),
            &BTreeMap::from([(HotkeyAction::SaveReplay, ConflictCause::AlreadyRegistered)]),
        );

        assert_eq!(registration.conflicts().count(), 0);
        assert_eq!(
            registration
                .status(HotkeyAction::SaveReplay)
                .expect("every action has a row")
                .state(),
            &BindingState::Unbound,
        );
    }

    #[test]
    fn a_state_is_only_a_conflict_when_windows_refused_something() {
        assert_ne!(
            BindingState::Unbound,
            BindingState::Conflict(conflict(ConflictCause::AlreadyRegistered)),
        );
    }
}
