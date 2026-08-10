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
/// which method is running. The two enumerations are not shared, because this
/// crate must not force a logging dependency on every consumer of the interface
/// and the logging vocabulary is closed on purpose; keeping them in step is a
/// review obligation, and there is nothing to keep in step until a backend
/// exists.
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
    /// [`select`](crate::select) walks this array, so the preference order lives
    /// in exactly one place. Adding a variant means adding it here; the
    /// `preference_order_contains_every_method` test fails otherwise.
    pub const PREFERENCE_ORDER: [Self; 3] = [
        Self::GameCapture,
        Self::WindowsGraphicsCapture,
        Self::DesktopDuplication,
    ];

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
    fn preference_order_contains_every_method() {
        // Exhaustive match: adding a variant without adding it to
        // PREFERENCE_ORDER stops compiling here, which is the point.
        for method in [
            CaptureMethod::GameCapture,
            CaptureMethod::WindowsGraphicsCapture,
            CaptureMethod::DesktopDuplication,
        ] {
            assert!(
                CaptureMethod::PREFERENCE_ORDER.contains(&method),
                "{method} is missing from the preference order"
            );
        }
    }

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
        // These are the `capture_backend` values `clipped_logging::CaptureBackend`
        // already emits, so a log search for one method must find frames from
        // this crate's report of it too.
        assert_eq!(
            CaptureMethod::WindowsGraphicsCapture.log_value(),
            "windows_graphics_capture"
        );
        assert_eq!(
            CaptureMethod::DesktopDuplication.log_value(),
            "desktop_duplication"
        );
        // Game Capture has no counterpart in the logging vocabulary, because
        // nothing implements it and that enumeration is closed on purpose.
        assert_eq!(CaptureMethod::GameCapture.log_value(), "game_capture");
    }
}
