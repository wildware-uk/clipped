//! What Clipped observed about capturing one game, as opposed to what the user
//! chose.
//!
//! # Why this is not a [`SettingKey`](super::SettingKey)
//!
//! Every other value in this module is one a person set. This one is written on
//! their behalf, when a recording ends, and that difference is the whole reason
//! it is a section of its own rather than a ninth per-game setting:
//!
//! - **A settings screen would offer to reset it.**
//!   [`Resolved::is_overridden`](super::Resolved::is_overridden) is true for a
//!   value the game's own layer holds, and that is what enables the Reset
//!   control. A remembered method stored in a game's [`Preferences`] would draw
//!   as a choice the user had made, with a Reset for a choice they never made —
//!   the control that silently means something else that AGENTS.md section 27
//!   and `super::value`'s own documentation are about.
//! - **Saving the settings screen would erase it.**
//!   [`Configuration::set_game`](super::Configuration::set_game) replaces a
//!   game's layer with what the screen built, so a memory living inside
//!   `Preferences` would be lost every time the user saved anything for that
//!   game.
//! - **It does not inherit.** AGENTS.md section 30 is about a value a user set
//!   globally and may override for one game. "Windows Graphics Capture could
//!   not capture Counter-Strike here" says nothing about Minecraft: whether a
//!   backend can capture a target depends on that target — a fullscreen
//!   exclusive swap chain, a window with display affinity set — so there is no
//!   global layer for it to fall through to, and inventing one would let a
//!   single unlucky game downgrade every other.
//!
//! # What it is for
//!
//! A recording that starts on Windows Graphics Capture and falls back to
//! Desktop Duplication loses a second or two doing it. Without this, the next
//! recording of the same game loses the same second or two in the same way,
//! for ever ([issue #286](https://github.com/wildware-uk/clipped/issues/286)).
//! With it, the next recording *starts* on the method that worked.
//!
//! It is a preference and never a pin. `clipped_capture::CaptureFallback::start_preferring`
//! applies it by asking one candidate first; a remembered method this machine
//! can no longer offer, or that fails to start, falls back exactly as if
//! nothing had been remembered.
//!
//! # Why it is forgotten
//!
//! Drivers update, Windows updates, and a monitor gets unplugged. A method that
//! could not capture a game last month may capture it perfectly today, and a
//! memory nothing ever revisits is a permanent downgrade bought with one bad
//! afternoon. So a memory is preferred only while it is younger than
//! [`MEMORY_LIFETIME`]; after that the published preference order is tried
//! afresh and whatever that recording ends on is remembered anew.
//!
//! The stamp is *when the answer last changed or was last re-established*, and
//! not when it was last confirmed. That distinction is the difference between a
//! memory that expires and one that never does:
//! [`Configuration::remember_capture_method`](super::Configuration::remember_capture_method)
//! leaves a fresh memory of the same method alone, so a game recorded every day
//! on Desktop Duplication still re-tries Windows Graphics Capture a fortnight
//! later. It writes when the method differs *or* when the memory had already
//! expired, so the fortnight starts again from a genuine re-trial rather than
//! from every recording.

use core::fmt;
use core::time::Duration;
use std::collections::BTreeMap;
use std::time::SystemTime;

use clipped_capture::CaptureMethod;
use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::config::game::GameKey;

/// The key one entry writes the method under.
const METHOD: &str = "method";

/// The key one entry writes the moment under.
const SINCE: &str = "since";

/// How long a remembered capture method is preferred before the ordinary
/// preference order is tried again.
///
/// A fortnight. What it costs when it expires is one fall back — the second or
/// two this whole feature exists to save — so the price of re-trying is at most
/// that, amortised over two weeks of recordings. What it buys is that a machine
/// whose graphics driver was fixed does not go on recording through the worse
/// backend until somebody notices and deletes a file. Shorter would pay the fall
/// back too often; much longer and "my capture never went back to normal"
/// becomes a support question rather than something that fixes itself.
pub const MEMORY_LIFETIME: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// The capture method a recording of one game was last observed to end on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureMemory {
    method: CaptureMethod,
    since: SystemTime,
}

impl CaptureMemory {
    /// A memory of `method`, established at `since`.
    #[must_use]
    pub const fn new(method: CaptureMethod, since: SystemTime) -> Self {
        Self { method, since }
    }

    /// The method that worked.
    #[must_use]
    pub const fn method(&self) -> CaptureMethod {
        self.method
    }

    /// When this answer was established, which is not when it was last
    /// confirmed — see the module documentation.
    #[must_use]
    pub const fn since(&self) -> SystemTime {
        self.since
    }

    /// Whether this memory is old enough that the preference order should be
    /// tried again.
    ///
    /// A stamp in the *future* counts as expired. That is a clock that went
    /// backwards rather than a memory worth trusting, and treating it as expired
    /// is what makes the next recording re-stamp it instead of leaving a value
    /// that cannot expire until the machine catches up with it.
    #[must_use]
    pub fn is_expired(&self, now: SystemTime) -> bool {
        now.duration_since(self.since)
            .map_or(true, |age| age >= MEMORY_LIFETIME)
    }
}

impl fmt::Display for CaptureMemory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.method)
    }
}

/// Reads the `capture` section, keeping what it can read and dropping what it
/// cannot.
///
/// Dropping is the right answer here and would be the wrong answer for
/// `plugins` or `storage`, and the difference is whose value it is. Everything
/// else in a settings file is something a person decided, so an entry this
/// build cannot read is kept untouched or the file is refused outright
/// (AGENTS.md section 56). This section is Clipped's own note about a machine,
/// and it can be made again by recording the game once. Refusing the whole file
/// over it would cost the user every setting they *did* choose, and keeping an
/// entry nothing can interpret would mean writing back a memory no recording
/// could ever confirm or correct.
pub(crate) fn read(section: Option<Map<String, Value>>) -> BTreeMap<GameKey, CaptureMemory> {
    let mut memories = BTreeMap::new();
    let Some(section) = section else {
        return memories;
    };

    for (name, value) in section {
        let Ok(game) = GameKey::parse(&name) else {
            continue;
        };
        let Some(entry) = value.as_object() else {
            continue;
        };
        let Some(method) = entry
            .get(METHOD)
            .and_then(Value::as_str)
            .and_then(CaptureMethod::from_log_value)
        else {
            continue;
        };
        let Some(since) = entry.get(SINCE).and_then(Value::as_str).and_then(moment) else {
            continue;
        };
        memories.insert(game, CaptureMemory::new(method, since));
    }
    memories
}

/// Writes the `capture` section.
pub(crate) fn write<'memories>(
    memories: impl Iterator<Item = (&'memories GameKey, &'memories CaptureMemory)>,
) -> Map<String, Value> {
    let mut section = Map::new();
    for (game, memory) in memories {
        let mut entry = Map::new();
        entry.insert(
            METHOD.to_owned(),
            // The same word the log line and the diagnostics bundle use for the
            // same method, so a support bundle showing "capture_backend=
            // desktop_duplication" and a settings file saying the same thing are
            // searchable as one string (AGENTS.md section 55).
            Value::from(memory.method().log_value()),
        );
        entry.insert(
            SINCE.to_owned(),
            Value::from(crate::automatic::rfc3339(memory.since())),
        );
        section.insert(game.as_str().to_owned(), Value::Object(entry));
    }
    section
}

/// The moment an RFC 3339 stamp names, or [`None`] for text that is not one.
fn moment(text: &str) -> Option<SystemTime> {
    OffsetDateTime::parse(text, &Rfc3339)
        .ok()
        .map(SystemTime::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moment() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725)
    }

    #[test]
    fn a_memory_is_preferred_right_up_to_its_lifetime_and_not_past_it() {
        let memory = CaptureMemory::new(CaptureMethod::DesktopDuplication, moment());

        assert!(!memory.is_expired(moment()));
        assert!(!memory.is_expired(moment() + MEMORY_LIFETIME - Duration::from_secs(1)));
        assert!(memory.is_expired(moment() + MEMORY_LIFETIME));
    }

    #[test]
    fn a_stamp_in_the_future_is_expired_rather_than_trusted_for_ever() {
        // A clock that went backwards — a laptop resuming with a bad RTC, a
        // machine whose time synchronised late. Treating this as fresh would
        // pin the memory until the wall clock caught up with a stamp that may
        // be years away.
        let memory = CaptureMemory::new(CaptureMethod::DesktopDuplication, moment());
        assert!(memory.is_expired(moment() - Duration::from_secs(1)));
    }

    #[test]
    fn a_memory_survives_being_written_and_read_back() {
        let game = GameKey::parse("counter-strike-2").expect("a game key");
        let mut memories = BTreeMap::new();
        memories.insert(
            game.clone(),
            CaptureMemory::new(
                CaptureMethod::DesktopDuplication,
                moment_at_second_resolution(),
            ),
        );

        let read_back = read(Some(write(memories.iter())));

        assert_eq!(read_back, memories);
    }

    #[test]
    fn an_entry_this_build_cannot_read_is_dropped_rather_than_costing_the_whole_file() {
        // The section is Clipped's own note and can be made again; the settings
        // beside it in the same file cannot.
        let mut section = Map::new();
        section.insert(
            "counter-strike-2".to_owned(),
            serde_json::json!({ "method": "quantum_capture", "since": "2026-08-16T10:00:00Z" }),
        );
        section.insert(
            "half-life-3".to_owned(),
            serde_json::json!({ "method": "desktop_duplication", "since": "the third" }),
        );
        section.insert(
            "Portal 3".to_owned(),
            serde_json::json!({ "method": "desktop_duplication", "since": "2026-08-16T10:00:00Z" }),
        );
        section.insert(
            "minecraft".to_owned(),
            serde_json::json!({ "method": "desktop_duplication", "since": "2026-08-16T10:00:00Z" }),
        );

        let read_back = read(Some(section));

        assert_eq!(
            read_back.keys().map(GameKey::as_str).collect::<Vec<_>>(),
            ["minecraft"],
            "an unreadable entry should be dropped and a readable one beside it kept"
        );
    }

    /// A moment with no sub-second part, which is all an RFC 3339 stamp written
    /// at second resolution can carry back.
    fn moment_at_second_resolution() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725)
    }
}
