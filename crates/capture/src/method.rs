//! The capture methods this project recognises, and the user's choice between
//! them.

use core::fmt;

/// A way of getting frames out of a running application or a display.
///
/// This is the vocabulary SPEC.md section 8 uses, in the order it prefers, and
/// it is the value the product shows as `Current method: ...`. It names a
/// *technique*, not an implementation: a variant existing here does not mean a
/// backend for it exists in this build. Ask a
/// [`BackendDeclaration`](crate::BackendDeclaration) for that.
///
/// The snake_case form used in logs is [`log_value`](Self::log_value), which is
/// deliberately identical to the `capture_backend` field vocabulary in
/// `clipped_logging::CaptureBackend`, so that a log line and the UI agree about
/// which method is running. The two enumerations are not shared — a capture
/// method is a technique this crate selects between, a `capture_backend` field
/// value is a word the log format has committed to, and they answer to
/// different documents — so `log_values_match_the_logging_field_vocabulary`
/// asserts the correspondence against the real `clipped_logging` enumeration,
/// which this crate takes as a dev-dependency for that test alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum CaptureMethod {
    /// Frames taken from the game's own presentation, by hooking the graphics
    /// API inside the game process — what OBS calls "game capture".
    ///
    /// **Nothing in this workspace implements this, and it may never be
    /// implemented here.** The usual technique is DLL injection into the game
    /// process, which AGENTS.md section 34 rules out: a recorder that injects
    /// into an online game risks the user's account, and no capture quality is
    /// worth that. The variant exists because SPEC.md section 8 puts the method
    /// at the top of the preference order, and an honest preference order has
    /// to name the thing it prefers. If a way of doing it that a user's
    /// anti-cheat would welcome ever appears, it has a name and a place to slot
    /// into; until then nothing registers a candidate for it, so
    /// [`select`](crate::select) never reaches it and no recording can use it.
    GameCapture,
    /// Windows Graphics Capture: the `Windows.Graphics.Capture` API, which
    /// captures a window or a display from the compositor.
    ///
    /// Implemented by [issue #12](https://github.com/wildware-uk/clipped/issues/12).
    WindowsGraphicsCapture,
    /// DXGI Desktop Duplication: a duplicate of a whole display output, cropped
    /// to the target where a window was asked for.
    ///
    /// Implemented by [issue #13](https://github.com/wildware-uk/clipped/issues/13).
    DesktopDuplication,
}

impl CaptureMethod {
    /// Every method, most preferred first (SPEC.md section 8).
    ///
    /// This is the published order — what the settings screen lists and what
    /// the documentation quotes. It is not what [`select`](crate::select)
    /// walks: selection sorts the candidates it was given by `preference_rank`
    /// (private, just below), so a method cannot be invisible to selection by
    /// being absent from an array. `preference_rank` will not compile unless
    /// every method holds its own slot here, so the array is complete and in
    /// the order the ranks say.
    pub const PREFERENCE_ORDER: [Self; 3] = [
        Self::GameCapture,
        Self::WindowsGraphicsCapture,
        Self::DesktopDuplication,
    ];

    /// Where this method comes in the preference order, `0` being the most
    /// preferred.
    ///
    /// The `match` is exhaustive, so a new variant does not compile until its
    /// author has decided where in the preference order it belongs, and each
    /// rank is checked at compile time against the slot of
    /// [`PREFERENCE_ORDER`](Self::PREFERENCE_ORDER) it names. Every method
    /// therefore occupies its own position in that array, or the crate does not
    /// build: the two ways of asking "which methods are there, and in what
    /// order?" cannot drift apart.
    ///
    /// That is worth the machinery because they were previously able to
    /// disagree in silence. Selection walked the array, so a registered,
    /// [`Available`](crate::Availability::Available) candidate for a method
    /// missing from it was never asked under
    /// [`Automatic`](CaptureMethodSetting::Automatic) — the caller got "no
    /// capture backend can capture this target" with an empty list of
    /// candidates — while [`Forced`](CaptureMethodSetting::Forced) selected the
    /// very same candidate happily.
    pub(crate) const fn preference_rank(self) -> usize {
        match self {
            Self::GameCapture => const { Self::rank(Self::GameCapture, 0) },
            Self::WindowsGraphicsCapture => const { Self::rank(Self::WindowsGraphicsCapture, 1) },
            Self::DesktopDuplication => const { Self::rank(Self::DesktopDuplication, 2) },
        }
    }

    /// `rank`, rejected unless
    /// [`PREFERENCE_ORDER`](Self::PREFERENCE_ORDER)`[rank]` is `method`.
    ///
    /// Called only from inside `const` blocks in
    /// [`preference_rank`](Self::preference_rank), which is what makes both
    /// checks compile errors rather than panics. The comparison is on the
    /// discriminants because `PartialEq` is not usable in a `const fn`; these
    /// are fieldless variants, so the cast is the whole value.
    const fn rank(method: Self, rank: usize) -> usize {
        assert!(
            rank < Self::PREFERENCE_ORDER.len(),
            "a capture method's rank must be a position in PREFERENCE_ORDER: \
             add the method to that array"
        );
        assert!(
            Self::PREFERENCE_ORDER[rank] as usize == method as usize,
            "a capture method's rank must be its own position in \
             PREFERENCE_ORDER, not another method's"
        );
        rank
    }

    /// How this method is written in a structured log field.
    ///
    /// Stable, snake_case and machine-searchable, unlike
    /// [`Display`](fmt::Display), which is the user-facing label and may be
    /// translated one day.
    #[must_use]
    pub const fn log_value(self) -> &'static str {
        match self {
            Self::GameCapture => "game_capture",
            Self::WindowsGraphicsCapture => "windows_graphics_capture",
            Self::DesktopDuplication => "desktop_duplication",
        }
    }
}

impl fmt::Display for CaptureMethod {
    /// The label shown to a user, as in `Current method: Game Capture`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GameCapture => "Game Capture",
            Self::WindowsGraphicsCapture => "Windows Graphics Capture",
            Self::DesktopDuplication => "Desktop Duplication",
        })
    }
}

/// What the user asked for, as opposed to what they got.
///
/// SPEC.md section 8 shows both at once:
///
/// ```text
/// Capture method: Automatic
/// Current method: Game Capture
/// ```
///
/// The first line is this type; the second is
/// [`Selection::method`](crate::Selection::method). Most users never change the
/// first, which is why [`Automatic`](Self::Automatic) is the [`Default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CaptureMethodSetting {
    /// Let [`select`](crate::select) choose, following
    /// [`CaptureMethod::PREFERENCE_ORDER`].
    #[default]
    Automatic,
    /// Use this method and no other.
    ///
    /// A forced method that cannot capture the target is an error rather than a
    /// quiet fall back to a different one: the user asked a specific question
    /// and deserves the answer, and silently ignoring the setting is how people
    /// end up debugging the wrong backend.
    Forced(CaptureMethod),
}

impl fmt::Display for CaptureMethodSetting {
    /// The label shown to a user, as in `Capture method: Automatic`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Automatic => f.write_str("Automatic"),
            Self::Forced(method) => write!(f, "{method}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_order_matches_the_specification() {
        // SPEC.md section 8: Game Capture, then Windows Graphics Capture, then
        // Desktop Duplication.
        assert_eq!(
            CaptureMethod::PREFERENCE_ORDER,
            [
                CaptureMethod::GameCapture,
                CaptureMethod::WindowsGraphicsCapture,
                CaptureMethod::DesktopDuplication,
            ]
        );
    }

    #[test]
    fn labels_are_the_ones_the_product_shows() {
        assert_eq!(CaptureMethod::GameCapture.to_string(), "Game Capture");
        assert_eq!(
            CaptureMethod::WindowsGraphicsCapture.to_string(),
            "Windows Graphics Capture"
        );
        assert_eq!(
            CaptureMethod::DesktopDuplication.to_string(),
            "Desktop Duplication"
        );
        assert_eq!(CaptureMethodSetting::default().to_string(), "Automatic");
        assert_eq!(
            CaptureMethodSetting::Forced(CaptureMethod::DesktopDuplication).to_string(),
            "Desktop Duplication"
        );
    }

    #[test]
    fn log_values_match_the_logging_field_vocabulary() {
        // Compared against the real enumeration, not against string literals
        // copied out of it: a log search for `capture_backend` has to find both
        // this crate's report of the method and the session's log lines, and
        // two hand-maintained lists of the same words drift.
        for method in CaptureMethod::PREFERENCE_ORDER {
            // Exhaustive on purpose: a new method has to state its counterpart
            // here, or say why it has none.
            let logged = match method {
                // Game Capture has no counterpart, and that is the right
                // answer rather than an omission. `clipped_logging`'s
                // vocabulary names backends that can appear in a session log,
                // nothing implements Game Capture, and AGENTS.md section 34
                // rules out the usual way of doing so; a value no code can
                // emit would be a word in the log format promising a backend
                // that does not exist. A build that ever implements it adds
                // the variant there in the same change.
                CaptureMethod::GameCapture => {
                    assert_eq!(method.log_value(), "game_capture");
                    continue;
                }
                CaptureMethod::WindowsGraphicsCapture => {
                    clipped_logging::CaptureBackend::WindowsGraphicsCapture
                }
                CaptureMethod::DesktopDuplication => {
                    clipped_logging::CaptureBackend::DesktopDuplication
                }
            };

            assert_eq!(
                method.log_value(),
                logged.to_string(),
                "{method} is logged as one word by this crate and another by \
                 clipped-logging, so no log search finds both"
            );
        }
    }
}
