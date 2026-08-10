//! Choosing a capture backend, and saying which one was chosen.

use core::fmt;
use std::error::Error;

use crate::{
    Availability, BackendDeclaration, CaptureMethod, CaptureMethodSetting, TargetKind,
    TargetProperties, Unavailable,
};

/// Why a candidate was not chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Rejection {
    /// The backend declared that it cannot capture this kind of target.
    TargetKindUnsupported {
        /// The kind it was asked about.
        kind: TargetKind,
    },
    /// The backend was asked about this target and declined.
    Unavailable(Unavailable),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetKindUnsupported { kind } => write!(f, "cannot capture a {kind}"),
            Self::Unavailable(reason) => write!(f, "{reason}"),
        }
    }
}

/// What happened to one candidate during selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// It was chosen.
    Selected,
    /// It was passed over.
    Rejected(Rejection),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Selected => f.write_str("selected"),
            Self::Rejected(rejection) => write!(f, "{rejection}"),
        }
    }
}

/// One candidate that selection actually looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Considered {
    method: CaptureMethod,
    outcome: Outcome,
}

impl Considered {
    /// The method the candidate implements.
    #[must_use]
    pub const fn method(&self) -> CaptureMethod {
        self.method
    }

    /// Whether it was chosen, and if not, why not.
    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        self.outcome
    }
}

impl fmt::Display for Considered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.method, self.outcome)
    }
}

/// The backend that will capture, and the reasoning that got there.
///
/// The two things a user sees come straight off this (SPEC.md section 8):
///
/// ```text
/// Capture method: Automatic
/// Current method: Windows Graphics Capture
/// ```
///
/// — [`setting`](Self::setting) is the first line and [`method`](Self::method)
/// the second. [`considered`](Self::considered) is the third thing, for the
/// diagnostics screen and the session log: it is the answer to "why is it using
/// *that* one?", which is otherwise a question nobody can answer from a bug
/// report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    setting: CaptureMethodSetting,
    method: CaptureMethod,
    considered: Vec<Considered>,
}

impl Selection {
    /// What the user asked for: `Automatic`, or a method they pinned.
    #[must_use]
    pub const fn setting(&self) -> CaptureMethodSetting {
        self.setting
    }

    /// The method that was chosen, to be reported as `Current method`.
    #[must_use]
    pub const fn method(&self) -> CaptureMethod {
        self.method
    }

    /// Every candidate selection examined, in the order it examined them,
    /// ending with the one it chose.
    ///
    /// Candidates less preferred than the winner were never asked, and so do
    /// not appear; neither do methods with no registered candidate, because
    /// "there is no Game Capture backend" is a fact about the build rather than
    /// about this target.
    #[must_use]
    pub fn considered(&self) -> &[Considered] {
        &self.considered
    }
}

/// Nothing could be selected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectionError {
    /// Two candidates claimed the same method.
    ///
    /// A registry bug. Refused rather than resolved arbitrarily, because the
    /// two would be indistinguishable in the report and in the logs, and a
    /// recording using the wrong one of them is a fault nobody can diagnose.
    DuplicateCandidate {
        /// The method claimed twice.
        method: CaptureMethod,
    },
    /// The user pinned a method that no candidate implements.
    ForcedMethodMissing {
        /// The method they asked for.
        method: CaptureMethod,
    },
    /// The user pinned a method that cannot capture this target.
    ///
    /// Deliberately not a fall back to something else: an explicit setting is
    /// obeyed or reported, never quietly overridden (SPEC.md section 8 makes
    /// falling back the behaviour of `Automatic`, which is what a user who
    /// wants it should choose).
    ForcedMethodRejected {
        /// The method they asked for.
        method: CaptureMethod,
        /// Why it cannot capture this target.
        rejection: Rejection,
    },
    /// Automatic selection ran out of candidates.
    NoBackendAvailable {
        /// Every candidate that was tried, and why each declined.
        ///
        /// Empty when there were none to try, which the message says in those
        /// words: "no capture backend can capture this target" on its own is
        /// the one report nobody can diagnose, because it does not distinguish
        /// a build with no backends in it from backends that all declined
        /// (AGENTS.md section 15).
        considered: Vec<Considered>,
    },
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateCandidate { method } => {
                write!(f, "two capture backends both claim to be {method}")
            }
            Self::ForcedMethodMissing { method } => write!(
                f,
                "{method} was requested, but this build has no {method} backend"
            ),
            Self::ForcedMethodRejected { method, rejection } => {
                write!(f, "{method} was requested, but it {rejection}")
            }
            Self::NoBackendAvailable { considered } if considered.is_empty() => f.write_str(
                "no capture backend can capture this target: this build registered \
                 no capture backends at all",
            ),
            Self::NoBackendAvailable { considered } => {
                f.write_str("no capture backend can capture this target")?;
                for candidate in considered {
                    write!(f, "; {candidate}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for SelectionError {}

/// Chooses the backend that will capture `target`.
///
/// # The policy
///
/// With [`CaptureMethodSetting::Automatic`], the candidates are sorted by
/// preference — Game Capture, then Windows Graphics Capture, then Desktop
/// Duplication (SPEC.md section 8) — and the first one that passes both tests
/// wins:
///
/// 1. its [`BackendCapabilities`](crate::BackendCapabilities) say it can
///    address this kind of target at all, and
/// 2. it answers [`Availability::Available`] when asked about this target.
///
/// A method with no registered candidate is skipped, because there is nothing
/// to ask. Every candidate that *was* registered is examined: the order comes
/// from sorting the candidates by
/// `CaptureMethod::preference_rank`, whose `match` is
/// exhaustive, rather than from walking a separate list of methods a new one
/// could be missing from. With
/// [`CaptureMethodSetting::Forced`], the named candidate faces the same two
/// tests and there is no fall back: failing either is an error the user can
/// read.
///
/// # Why it is a free function over declarations
///
/// Nothing here touches a GPU, a window or a clock: the inputs are declared
/// capabilities and observed target properties, and the output depends on
/// nothing else. So the choice is reproducible, it can be logged as a decision
/// rather than a mystery, and it is testable on a build machine with no
/// display — which the tests in this module do.
///
/// It is also the seam for automatic fallback after a mid-recording failure
/// ([issue #97](https://github.com/wildware-uk/clipped/issues/97), milestone
/// M13). That is out of scope here and no part of it is built, but because
/// selection is a pure function of the candidate list, falling back needs no
/// new interface: call this again with the failed method removed from the
/// slice. What M13 has to add is the part that is genuinely missing — deciding
/// *when* a backend has failed, and remembering the answer per game.
///
/// # Errors
///
/// [`SelectionError`] when the candidate list is malformed, when a forced
/// method is missing or unusable, or when no candidate can capture the target.
///
/// # Example
///
/// ```
/// # use clipped_capture::{
/// #     Availability, BackendCapabilities, BackendDeclaration, CaptureMethod,
/// #     CaptureMethodSetting, FrameSize, TargetKind, TargetProperties, select,
/// # };
/// # #[derive(Debug)]
/// # struct Wgc;
/// # impl BackendDeclaration for Wgc {
/// #     fn method(&self) -> CaptureMethod { CaptureMethod::WindowsGraphicsCapture }
/// #     fn capabilities(&self) -> BackendCapabilities {
/// #         BackendCapabilities::new(true, true).with_occlusion_independent(true)
/// #     }
/// #     fn availability(&self, _: &TargetProperties) -> Availability { Availability::Available }
/// # }
/// let size = FrameSize::new(2560, 1440).expect("a real window has a size");
/// let target = TargetProperties::new(TargetKind::Window, size);
/// let selection = select(&[&Wgc], &target, CaptureMethodSetting::Automatic)?;
///
/// assert_eq!(
///     format!("Capture method: {}\nCurrent method: {}", selection.setting(), selection.method()),
///     "Capture method: Automatic\nCurrent method: Windows Graphics Capture",
/// );
/// # Ok::<(), clipped_capture::SelectionError>(())
/// ```
pub fn select(
    candidates: &[&dyn BackendDeclaration],
    target: &TargetProperties,
    setting: CaptureMethodSetting,
) -> Result<Selection, SelectionError> {
    for (index, candidate) in candidates.iter().enumerate() {
        if candidates[..index]
            .iter()
            .any(|earlier| earlier.method() == candidate.method())
        {
            return Err(SelectionError::DuplicateCandidate {
                method: candidate.method(),
            });
        }
    }

    match setting {
        CaptureMethodSetting::Forced(method) => {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.method() == method)
                .ok_or(SelectionError::ForcedMethodMissing { method })?;

            match consider(*candidate, target) {
                Outcome::Selected => Ok(Selection {
                    setting,
                    method,
                    considered: vec![Considered {
                        method,
                        outcome: Outcome::Selected,
                    }],
                }),
                Outcome::Rejected(rejection) => {
                    Err(SelectionError::ForcedMethodRejected { method, rejection })
                }
            }
        }
        CaptureMethodSetting::Automatic => {
            // Ordering the candidates, rather than walking a list of methods
            // and looking each one up, is what makes it impossible for a
            // registered candidate to be silently skipped: every candidate is
            // in this vector, and `preference_rank` is an exhaustive match, so
            // a method that exists has a place in the order or the crate does
            // not compile.
            let mut ordered: Vec<&dyn BackendDeclaration> = candidates.to_vec();
            ordered.sort_by_key(|candidate| candidate.method().preference_rank());

            let mut considered = Vec::new();

            for candidate in ordered {
                let method = candidate.method();
                let outcome = consider(candidate, target);
                considered.push(Considered { method, outcome });

                if outcome == Outcome::Selected {
                    return Ok(Selection {
                        setting,
                        method,
                        considered,
                    });
                }
            }

            Err(SelectionError::NoBackendAvailable { considered })
        }
    }
}

/// Applies both tests to one candidate.
fn consider(candidate: &dyn BackendDeclaration, target: &TargetProperties) -> Outcome {
    if !candidate.capabilities().captures(target.kind()) {
        return Outcome::Rejected(Rejection::TargetKindUnsupported {
            kind: target.kind(),
        });
    }
    match candidate.availability(target) {
        Availability::Available => Outcome::Selected,
        Availability::Unavailable(reason) => Outcome::Rejected(Rejection::Unavailable(reason)),
    }
}

#[cfg(test)]
mod tests {
    //! The backends in this module are fakes, and that is deliberate.
    //!
    //! What is under test here is the *selection policy* — the preference
    //! order, the two tests a candidate has to pass, and what is reported when
    //! none does. That policy is pure logic over declared capabilities and
    //! observed target properties, so a fake that declares capabilities is not
    //! a stand-in for capture: it is exactly the input the policy consumes.
    //! Real capture is tested against real Windows APIs in
    //! [issue #12](https://github.com/wildware-uk/clipped/issues/12) and
    //! [issue #13](https://github.com/wildware-uk/clipped/issues/13), and no
    //! fake here is production code or a placeholder for any.

    use super::*;
    use crate::{BackendCapabilities, FrameSize};

    /// A backend declaration with no backend behind it.
    #[derive(Debug)]
    struct FakeDeclaration {
        method: CaptureMethod,
        capabilities: BackendCapabilities,
        availability: Availability,
        declines_protected_content: bool,
    }

    impl FakeDeclaration {
        /// An available backend for `method` that can capture anything.
        fn available(method: CaptureMethod) -> Self {
            Self {
                method,
                capabilities: BackendCapabilities::new(true, true),
                availability: Availability::Available,
                declines_protected_content: false,
            }
        }

        fn unavailable(method: CaptureMethod, reason: Unavailable) -> Self {
            Self {
                availability: Availability::Unavailable(reason),
                ..Self::available(method)
            }
        }

        fn monitors_only(method: CaptureMethod) -> Self {
            Self {
                capabilities: BackendCapabilities::new(false, true),
                ..Self::available(method)
            }
        }

        fn windows_only(method: CaptureMethod) -> Self {
            Self {
                capabilities: BackendCapabilities::new(true, false),
                ..Self::available(method)
            }
        }

        fn declining_protected_content(method: CaptureMethod) -> Self {
            Self {
                declines_protected_content: true,
                ..Self::available(method)
            }
        }
    }

    impl BackendDeclaration for FakeDeclaration {
        fn method(&self) -> CaptureMethod {
            self.method
        }

        fn capabilities(&self) -> BackendCapabilities {
            self.capabilities
        }

        fn availability(&self, target: &TargetProperties) -> Availability {
            if self.declines_protected_content && target.is_content_protected() {
                return Availability::Unavailable(Unavailable::UnsupportedTarget {
                    reason: "the window has opted out of being captured",
                });
            }
            self.availability
        }
    }

    fn window() -> TargetProperties {
        TargetProperties::new(
            TargetKind::Window,
            FrameSize::new(1920, 1080).expect("1920x1080 is a valid size"),
        )
    }

    fn monitor() -> TargetProperties {
        TargetProperties::new(
            TargetKind::Monitor,
            FrameSize::new(3840, 2160).expect("3840x2160 is a valid size"),
        )
    }

    #[test]
    fn automatic_takes_the_most_preferred_available_backend() {
        // All three available: SPEC.md section 8 says Game Capture wins. No
        // Game Capture backend is registered in a real build, which is what
        // the next test is about.
        let game = FakeDeclaration::available(CaptureMethod::GameCapture);
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);
        let duplication = FakeDeclaration::available(CaptureMethod::DesktopDuplication);

        // Registration order is not preference order: the policy must sort by
        // the specified hierarchy, not by however the registry was built.
        let selection = select(
            &[&duplication, &wgc, &game],
            &window(),
            CaptureMethodSetting::Automatic,
        )
        .expect("three available backends must yield a selection");

        assert_eq!(selection.method(), CaptureMethod::GameCapture);
        assert_eq!(selection.setting(), CaptureMethodSetting::Automatic);
        assert_eq!(
            selection.considered(),
            [Considered {
                method: CaptureMethod::GameCapture,
                outcome: Outcome::Selected,
            }]
            .as_slice(),
            "less preferred backends should not even be asked"
        );
    }

    #[test]
    fn a_preferred_backend_that_is_not_implemented_is_passed_over() {
        // The real Game Capture situation: the method is in the preference
        // order, and nothing implements it (AGENTS.md section 34).
        let game =
            FakeDeclaration::unavailable(CaptureMethod::GameCapture, Unavailable::NotImplemented);
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);

        let selection = select(&[&game, &wgc], &window(), CaptureMethodSetting::Automatic)
            .expect("Windows Graphics Capture is available");

        assert_eq!(selection.method(), CaptureMethod::WindowsGraphicsCapture);
        assert_eq!(
            selection.considered(),
            [
                Considered {
                    method: CaptureMethod::GameCapture,
                    outcome: Outcome::Rejected(Rejection::Unavailable(Unavailable::NotImplemented)),
                },
                Considered {
                    method: CaptureMethod::WindowsGraphicsCapture,
                    outcome: Outcome::Selected,
                },
            ]
            .as_slice(),
            "the report must record that Game Capture was tried and why it was not used"
        );
    }

    #[test]
    fn a_method_with_no_registered_candidate_is_skipped_silently() {
        // Game Capture is registered nowhere in this workspace, so it should
        // not appear in the report at all: it is a fact about the build, not
        // about this target.
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);

        let selection = select(&[&wgc], &window(), CaptureMethodSetting::Automatic)
            .expect("Windows Graphics Capture is available");

        assert_eq!(selection.method(), CaptureMethod::WindowsGraphicsCapture);
        assert_eq!(selection.considered().len(), 1);
        assert_eq!(
            selection.considered()[0].method(),
            CaptureMethod::WindowsGraphicsCapture
        );
    }

    #[test]
    fn a_backend_that_cannot_address_the_target_kind_is_passed_over() {
        // The preferred candidate captures windows and nothing else, which is
        // the shape of a real game-capture hook: it attaches to a process, so a
        // whole display is not something it can be pointed at.
        let hook = FakeDeclaration::windows_only(CaptureMethod::GameCapture);
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);

        let for_window = select(&[&hook, &wgc], &window(), CaptureMethodSetting::Automatic)
            .expect("the preferred backend captures windows");
        assert_eq!(for_window.method(), CaptureMethod::GameCapture);

        // Same registry, pointed at a monitor: the preferred candidate is not
        // eligible at all and must be passed over rather than asked.
        let for_monitor = select(&[&hook, &wgc], &monitor(), CaptureMethodSetting::Automatic)
            .expect("Windows Graphics Capture captures monitors");
        assert_eq!(for_monitor.method(), CaptureMethod::WindowsGraphicsCapture);
        assert_eq!(
            for_monitor.considered()[0].outcome(),
            Outcome::Rejected(Rejection::TargetKindUnsupported {
                kind: TargetKind::Monitor
            })
        );
    }

    #[test]
    fn target_properties_reach_the_backend_being_asked() {
        // A window that has opted out of capture: the backend that honours the
        // opt-out declines it, and selection moves on rather than recording a
        // black rectangle (issue #97).
        let wgc =
            FakeDeclaration::declining_protected_content(CaptureMethod::WindowsGraphicsCapture);
        let duplication = FakeDeclaration::available(CaptureMethod::DesktopDuplication);
        let protected = window().with_content_protected(true);

        let ordinary = select(
            &[&wgc, &duplication],
            &window(),
            CaptureMethodSetting::Automatic,
        )
        .expect("an ordinary window is capturable");
        assert_eq!(ordinary.method(), CaptureMethod::WindowsGraphicsCapture);

        let selection = select(
            &[&wgc, &duplication],
            &protected,
            CaptureMethodSetting::Automatic,
        )
        .expect("Desktop Duplication does not object");
        assert_eq!(selection.method(), CaptureMethod::DesktopDuplication);
        assert_eq!(
            selection.considered()[0].outcome(),
            Outcome::Rejected(Rejection::Unavailable(Unavailable::UnsupportedTarget {
                reason: "the window has opted out of being captured",
            }))
        );
    }

    #[test]
    fn when_nothing_can_capture_the_target_every_reason_is_reported() {
        let wgc = FakeDeclaration::unavailable(
            CaptureMethod::WindowsGraphicsCapture,
            Unavailable::UnsupportedSystem {
                requirement: "Windows 10 build 1903 or later",
            },
        );
        let duplication = FakeDeclaration::monitors_only(CaptureMethod::DesktopDuplication);

        let error = select(
            &[&wgc, &duplication],
            &window(),
            CaptureMethodSetting::Automatic,
        )
        .expect_err("neither backend can capture this window");

        let SelectionError::NoBackendAvailable { considered } = &error else {
            panic!("expected NoBackendAvailable, got {error:?}");
        };
        assert_eq!(
            considered,
            &[
                Considered {
                    method: CaptureMethod::WindowsGraphicsCapture,
                    outcome: Outcome::Rejected(Rejection::Unavailable(
                        Unavailable::UnsupportedSystem {
                            requirement: "Windows 10 build 1903 or later",
                        }
                    )),
                },
                Considered {
                    method: CaptureMethod::DesktopDuplication,
                    outcome: Outcome::Rejected(Rejection::TargetKindUnsupported {
                        kind: TargetKind::Window,
                    }),
                },
            ]
        );
        assert_eq!(
            error.to_string(),
            "no capture backend can capture this target; \
             Windows Graphics Capture: requires Windows 10 build 1903 or later; \
             Desktop Duplication: cannot capture a window",
            "the message has to say what was tried, or a bug report says only 'it failed'"
        );
    }

    #[test]
    fn an_empty_registry_is_not_a_selection() {
        let error = select(&[], &window(), CaptureMethodSetting::Automatic)
            .expect_err("there is nothing to select");
        assert_eq!(
            error,
            SelectionError::NoBackendAvailable {
                considered: Vec::new()
            }
        );
        assert_eq!(
            error.to_string(),
            "no capture backend can capture this target: this build registered \
             no capture backends at all",
            "with nothing tried there are no reasons to list, so the message \
             has to name the situation itself or the bug report says only \
             'it failed'"
        );
    }

    #[test]
    fn automatic_selection_reaches_every_method_there_is() {
        // The regression this catches: a method that `select` never asks
        // about. An available candidate for it is registered on its own, so
        // the only way it is not selected is that automatic selection cannot
        // see it — which is what happened when selection walked a hand-written
        // array and a method was missing from it. Forced selection, which
        // looks the candidate up directly, agrees; the two paths disagreeing
        // about which backends exist is the silent half of that fault.
        for method in CaptureMethod::PREFERENCE_ORDER {
            let candidate = FakeDeclaration::available(method);

            let automatic = select(&[&candidate], &window(), CaptureMethodSetting::Automatic)
                .unwrap_or_else(|error| panic!("{method} is available: {error}"));
            assert_eq!(automatic.method(), method);

            let forced = select(
                &[&candidate],
                &window(),
                CaptureMethodSetting::Forced(method),
            )
            .unwrap_or_else(|error| panic!("{method} is available: {error}"));
            assert_eq!(
                forced.method(),
                automatic.method(),
                "the two selection paths must agree about which backends exist"
            );
        }
    }

    #[test]
    fn a_forced_method_beats_the_preference_order() {
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);
        let duplication = FakeDeclaration::available(CaptureMethod::DesktopDuplication);

        let selection = select(
            &[&wgc, &duplication],
            &monitor(),
            CaptureMethodSetting::Forced(CaptureMethod::DesktopDuplication),
        )
        .expect("the forced backend is available");

        assert_eq!(selection.method(), CaptureMethod::DesktopDuplication);
        assert_eq!(
            selection.setting(),
            CaptureMethodSetting::Forced(CaptureMethod::DesktopDuplication)
        );
    }

    #[test]
    fn a_forced_method_that_cannot_run_is_reported_rather_than_replaced() {
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);
        let duplication = FakeDeclaration::monitors_only(CaptureMethod::DesktopDuplication);

        let error = select(
            &[&wgc, &duplication],
            &window(),
            CaptureMethodSetting::Forced(CaptureMethod::DesktopDuplication),
        )
        .expect_err("the forced backend cannot capture a window");

        assert_eq!(
            error,
            SelectionError::ForcedMethodRejected {
                method: CaptureMethod::DesktopDuplication,
                rejection: Rejection::TargetKindUnsupported {
                    kind: TargetKind::Window
                },
            },
            "an explicit setting must not be silently swapped for a different backend"
        );
    }

    #[test]
    fn a_forced_method_nothing_implements_says_so() {
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);

        let error = select(
            &[&wgc],
            &window(),
            CaptureMethodSetting::Forced(CaptureMethod::GameCapture),
        )
        .expect_err("no Game Capture backend exists");

        assert_eq!(
            error,
            SelectionError::ForcedMethodMissing {
                method: CaptureMethod::GameCapture
            }
        );
        assert_eq!(
            error.to_string(),
            "Game Capture was requested, but this build has no Game Capture backend"
        );
    }

    #[test]
    fn two_candidates_for_one_method_are_refused() {
        let first = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);
        let second = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);

        let error = select(
            &[&first, &second],
            &window(),
            CaptureMethodSetting::Automatic,
        )
        .expect_err("the report could not say which one ran");

        assert_eq!(
            error,
            SelectionError::DuplicateCandidate {
                method: CaptureMethod::WindowsGraphicsCapture
            }
        );
    }

    #[test]
    fn the_status_the_product_shows_comes_straight_off_the_selection() {
        // SPEC.md section 8's two lines, which is the whole of what a user
        // needs to understand about capture APIs.
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);
        let selection = select(&[&wgc], &window(), CaptureMethodSetting::Automatic)
            .expect("Windows Graphics Capture is available");

        assert_eq!(
            format!(
                "Capture method: {}\nCurrent method: {}",
                selection.setting(),
                selection.method()
            ),
            "Capture method: Automatic\nCurrent method: Windows Graphics Capture"
        );
    }

    #[test]
    fn re_selecting_without_a_failed_method_is_the_fallback_seam() {
        // Runtime fallback belongs to issue #97 and is not built. This test
        // records the property that lets it be built without changing this
        // interface: selection is a pure function of the candidate list, so
        // "fall back" is "ask again without that candidate".
        let wgc = FakeDeclaration::available(CaptureMethod::WindowsGraphicsCapture);
        let duplication = FakeDeclaration::available(CaptureMethod::DesktopDuplication);
        let registry: [&dyn BackendDeclaration; 2] = [&wgc, &duplication];

        let first = select(&registry, &monitor(), CaptureMethodSetting::Automatic)
            .expect("Windows Graphics Capture is available");
        assert_eq!(first.method(), CaptureMethod::WindowsGraphicsCapture);

        let remaining: Vec<&dyn BackendDeclaration> = registry
            .into_iter()
            .filter(|candidate| candidate.method() != first.method())
            .collect();
        let second = select(&remaining, &monitor(), CaptureMethodSetting::Automatic)
            .expect("Desktop Duplication is available");
        assert_eq!(second.method(), CaptureMethod::DesktopDuplication);
    }
}
