//! A key combination: what it is made of, how it is written down, and which
//! combinations are refused before Windows is ever asked.
//!
//! # Why the virtual-key codes are literals here
//!
//! Every code below is a documented Win32 constant, and this module names them
//! as numbers rather than importing `windows::Win32::UI::Input::KeyboardAndMouse`
//! for one reason: the workspace compiles and unit-tests off Windows so that a
//! contributor's other machine is not useless to them, and the `windows`
//! dependency is `cfg(windows)`-only. A copied constant is a constant that can
//! be wrong, so `virtual_key_codes_match_the_windows_headers` holds every one of
//! them against the real header on Windows — where it matters — rather than
//! trusting the comment beside it.

use core::fmt;
use core::str::FromStr;

/// The modifier keys held down with the main key.
///
/// The bit values are the Win32 `MOD_*` flags, so that the conversion at the
/// registration call is the identity. `virtual_key_codes_match_the_windows_headers`
/// checks that claim against the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    /// No modifier at all.
    pub const NONE: Self = Self(0);
    /// The Alt key. `MOD_ALT`.
    pub const ALT: Self = Self(0x0001);
    /// The Ctrl key. `MOD_CONTROL`.
    pub const CONTROL: Self = Self(0x0002);
    /// The Shift key. `MOD_SHIFT`.
    pub const SHIFT: Self = Self(0x0004);
    /// The Windows key. `MOD_WIN`.
    pub const WIN: Self = Self(0x0008);

    /// Whether every modifier in `other` is also in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any of Ctrl, Alt or Win is held.
    ///
    /// Shift is deliberately not one of them: `Shift+A` is how a capital A is
    /// typed, so a combination whose only modifier is Shift is as ruinous to
    /// bind system-wide as a bare letter. See [`Hotkey::new`].
    #[must_use]
    const fn has_command_modifier(self) -> bool {
        self.0 & (Self::ALT.0 | Self::CONTROL.0 | Self::WIN.0) != 0
    }

    /// The `MOD_*` bits, for the registration call.
    #[must_use]
    pub(crate) const fn bits(self) -> u32 {
        self.0 as u32
    }
}

impl core::ops::BitOr for Modifiers {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl core::ops::BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

impl fmt::Display for Modifiers {
    /// The held modifiers in the canonical order, each followed by `+`, so that
    /// `write!("{modifiers}{key}")` is the whole combination.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (modifier, name) in [
            (Self::CONTROL, "Ctrl"),
            (Self::ALT, "Alt"),
            (Self::SHIFT, "Shift"),
            (Self::WIN, "Win"),
        ] {
            if self.contains(modifier) {
                write!(formatter, "{name}+")?;
            }
        }
        Ok(())
    }
}

/// One main key, guaranteed to be a key Clipped can register.
///
/// The private virtual-key code is why the guarantee holds: every way of making
/// a [`Key`] goes through this module, so there is no `Key` that names nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(u16);

/// The keys with a fixed name, and the [`Key`] each name is.
///
/// Letters, digits and function keys are ranges and are handled arithmetically;
/// this is everything else Clipped will bind. It is a short list on purpose: a
/// hotkey a game might also want (Tab, Escape, Enter, the arrows) is a hotkey
/// that takes a key away from the game it is meant to be used in, and the
/// modifiers are what make the useful ones safe.
///
/// The codes live on the associated constants below and are named from here
/// rather than repeated, so there is one copy of each number to be wrong about.
const NAMED_KEYS: [(&str, Key); 9] = [
    ("Space", Key::SPACE),
    ("PageUp", Key::PAGE_UP),
    ("PageDown", Key::PAGE_DOWN),
    ("End", Key::END),
    ("Home", Key::HOME),
    ("PrintScreen", Key::PRINT_SCREEN),
    ("Insert", Key::INSERT),
    ("Delete", Key::DELETE),
    ("Pause", Key::PAUSE),
];

/// `VK_F1`. The function keys run from here to `VK_F24` without a gap.
const VK_F1: u16 = 0x70;
/// How many function keys Windows has virtual-key codes for.
const FUNCTION_KEYS: u8 = 24;
/// `VK_A`. `A` to `Z` are contiguous, and are the ASCII codes of the capitals.
const VK_A: u16 = 0x41;
/// `VK_0`. `0` to `9` are contiguous, and are the ASCII codes of the digits.
const VK_0: u16 = 0x30;

impl Key {
    /// The Space bar.
    pub const SPACE: Self = Self(0x20);
    /// Page Up.
    pub const PAGE_UP: Self = Self(0x21);
    /// Page Down.
    pub const PAGE_DOWN: Self = Self(0x22);
    /// End.
    pub const END: Self = Self(0x23);
    /// Home.
    pub const HOME: Self = Self(0x24);
    /// Print Screen.
    pub const PRINT_SCREEN: Self = Self(0x2C);
    /// Insert.
    pub const INSERT: Self = Self(0x2D);
    /// Delete.
    pub const DELETE: Self = Self(0x2E);
    /// Pause.
    pub const PAUSE: Self = Self(0x13);

    /// `F1` to `F24`, by number.
    ///
    /// [`None`] for anything outside that range, which is every function key
    /// Windows has no code for.
    #[must_use]
    pub const fn function(number: u8) -> Option<Self> {
        if number == 0 || number > FUNCTION_KEYS {
            return None;
        }
        Some(Self(VK_F1 + (number as u16) - 1))
    }

    /// A letter key, given either case.
    ///
    /// [`None`] for anything that is not an ASCII letter. There is one key for
    /// `a` and `A`: which of the two a keypress produces is the Shift state,
    /// and Shift is a modifier here rather than part of the key.
    #[must_use]
    pub const fn letter(letter: char) -> Option<Self> {
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        Some(Self(
            VK_A + (letter.to_ascii_uppercase() as u16) - b'A' as u16,
        ))
    }

    /// A digit key from the top row, `0` to `9`.
    ///
    /// The numeric keypad is deliberately absent: its codes are different, and
    /// a binding made on a keyboard with a keypad that silently does nothing on
    /// a keyboard without one is worse than one that could not be made.
    #[must_use]
    pub const fn digit(digit: u8) -> Option<Self> {
        if digit > 9 {
            return None;
        }
        Some(Self(VK_0 + digit as u16))
    }

    /// The key written that way, however it is cased, spaced or hyphenated:
    /// `F10`, `f10`, `PageUp`, `page up` and `page-up` are all accepted.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        let wanted = normalise(name);

        if let Some(number) = wanted.strip_prefix('f') {
            if let Ok(number) = number.parse::<u8>() {
                return Self::function(number);
            }
        }

        let mut characters = wanted.chars();
        if let (Some(single), None) = (characters.next(), characters.next()) {
            if single.is_ascii_alphabetic() {
                return Self::letter(single);
            }
            if let Some(digit) = single.to_digit(10) {
                // `to_digit` bounds this at 9, so the cast cannot truncate.
                #[allow(clippy::cast_possible_truncation)]
                return Self::digit(digit as u8);
            }
        }

        NAMED_KEYS
            .iter()
            .find(|(candidate, _)| normalise(candidate) == wanted)
            .map(|&(_, key)| key)
    }

    /// How the key is written down.
    #[must_use]
    pub fn name(self) -> String {
        match self.0 {
            code if (VK_F1..VK_F1 + FUNCTION_KEYS as u16).contains(&code) => {
                format!("F{}", code - VK_F1 + 1)
            }
            code if (VK_A..VK_A + 26).contains(&code) => {
                char::from(u8::try_from(code).unwrap_or(b'?')).to_string()
            }
            code if (VK_0..VK_0 + 10).contains(&code) => (code - VK_0).to_string(),
            code => NAMED_KEYS
                .iter()
                .find(|(_, candidate)| candidate.0 == code)
                .map_or_else(
                    // Unreachable: every constructor is above, and each one
                    // produces a code this match covers. Stated rather than
                    // panicked over, because a name is not worth a crash.
                    || format!("VK_{code:#04X}"),
                    |(name, _)| (*name).to_owned(),
                ),
        }
    }

    /// Every key that can be bound, for the configuration UI to offer.
    ///
    /// Function keys first, because they are what a game is least likely to
    /// want and what a recorder is most likely to be bound to.
    pub fn assignable() -> impl Iterator<Item = Self> {
        (1..=FUNCTION_KEYS)
            .filter_map(Self::function)
            .chain((b'A'..=b'Z').filter_map(|letter| Self::letter(char::from(letter))))
            .chain((0..=9).filter_map(Self::digit))
            .chain(NAMED_KEYS.iter().map(|&(_, key)| key))
    }

    /// Whether this key types a character, and so must not be bound without one
    /// of Ctrl, Alt or Win.
    #[must_use]
    const fn types_a_character(self) -> bool {
        (self.0 >= VK_A && self.0 < VK_A + 26)
            || (self.0 >= VK_0 && self.0 < VK_0 + 10)
            || self.0 == Self::SPACE.0
    }

    /// The virtual-key code, for the registration call.
    #[must_use]
    pub(crate) const fn virtual_key(self) -> u32 {
        self.0 as u32
    }
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name())
    }
}

/// Lower-cases a token and drops the separators people write between words, so
/// that `Page Up`, `page-up` and `PAGEUP` are one name.
fn normalise(token: &str) -> String {
    token
        .chars()
        .filter(|character| !matches!(character, ' ' | '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// A key combination Clipped can register.
///
/// Written and parsed as `Ctrl+F10`: [`Display`](fmt::Display) and
/// [`FromStr`] are inverses, which is what a configuration file and a
/// settings screen both need. There is no `serde` derive here on purpose —
/// nothing persists a binding yet, and a string that round-trips is what the
/// configuration API ([issue
/// #108](https://github.com/wildware-uk/clipped/issues/108)) will want anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hotkey {
    modifiers: Modifiers,
    key: Key,
}

impl Hotkey {
    /// A combination of modifiers and a key.
    ///
    /// # Errors
    ///
    /// [`InvalidHotkey::TypingKey`] when the key types a character and none of
    /// Ctrl, Alt or Win is held. Registering `A`, or `Shift+A`, takes that
    /// keystroke away from every other application on the machine — including
    /// the one the user is typing their password into — and a recorder that
    /// let a user do that by mistake would be indistinguishable from a broken
    /// keyboard. Function keys and the navigation cluster are not restricted:
    /// a bare `F9` is a normal thing to bind and takes nothing away from
    /// typing.
    pub const fn new(modifiers: Modifiers, key: Key) -> Result<Self, InvalidHotkey> {
        if key.types_a_character() && !modifiers.has_command_modifier() {
            return Err(InvalidHotkey::TypingKey);
        }
        Ok(Self { modifiers, key })
    }

    /// The modifiers held with the key.
    #[must_use]
    pub const fn modifiers(self) -> Modifiers {
        self.modifiers
    }

    /// The main key.
    #[must_use]
    pub const fn key(self) -> Key {
        self.key
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{}", self.modifiers, self.key)
    }
}

impl FromStr for Hotkey {
    type Err = InvalidHotkey;

    /// Parses `Ctrl+F10`, in any order and any case.
    ///
    /// `Ctrl`, `Control`, `Alt`, `Shift`, `Win`, `Windows` and `Super` are the
    /// modifier names; everything else is the main key, and there must be
    /// exactly one of it.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut modifiers = Modifiers::NONE;
        let mut key = None;

        for token in text.split('+').map(str::trim).filter(|t| !t.is_empty()) {
            match normalise(token).as_str() {
                "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
                "alt" => modifiers |= Modifiers::ALT,
                "shift" => modifiers |= Modifiers::SHIFT,
                "win" | "windows" | "super" => modifiers |= Modifiers::WIN,
                _ => {
                    let parsed = Key::named(token)
                        .ok_or_else(|| InvalidHotkey::UnknownKey(token.to_owned()))?;
                    if key.replace(parsed).is_some() {
                        return Err(InvalidHotkey::TwoKeys);
                    }
                }
            }
        }

        Self::new(modifiers, key.ok_or(InvalidHotkey::NoKey)?)
    }
}

/// Why a combination cannot be used.
///
/// Every variant is a sentence a settings screen can show as it is (AGENTS.md
/// sections 28 and 45): it says what is wrong and what to do instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidHotkey {
    /// The text named no main key at all, only modifiers.
    NoKey,
    /// The text named two main keys.
    TwoKeys,
    /// The text named something that is not a key Clipped can bind.
    UnknownKey(String),
    /// The key types a character and was given no command modifier.
    TypingKey,
}

impl fmt::Display for InvalidHotkey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoKey => formatter
                .write_str("a hotkey needs a key as well as its modifiers, such as Ctrl+F10"),
            Self::TwoKeys => formatter.write_str(
                "a hotkey has one key and any number of modifiers, such as Ctrl+Shift+F10",
            ),
            Self::UnknownKey(token) => write!(
                formatter,
                "`{token}` is not a key Clipped can bind. Use a function key (F1 to F24), \
                 a letter, a digit, or one of Space, Insert, Delete, Home, End, PageUp, \
                 PageDown, PrintScreen and Pause",
            ),
            Self::TypingKey => formatter.write_str(
                "a letter, digit or Space needs Ctrl, Alt or the Windows key with it, \
                 otherwise binding it would take that key away from everything you type in",
            ),
        }
    }
}

impl core::error::Error for InvalidHotkey {}

#[cfg(test)]
mod tests {
    use super::{Hotkey, InvalidHotkey, Key, Modifiers};

    #[test]
    fn the_spec_defaults_are_written_and_parsed_the_same_way() {
        for text in ["Ctrl+F10", "Ctrl+F9"] {
            let hotkey: Hotkey = text.parse().expect("a documented default parses");
            assert_eq!(hotkey.to_string(), text);
        }
    }

    #[test]
    fn modifiers_are_written_in_one_order_whatever_order_they_were_given_in() {
        let parsed: Hotkey = "shift+win+alt+ctrl+f10"
            .parse()
            .expect("all four modifiers");

        assert_eq!(parsed.to_string(), "Ctrl+Alt+Shift+Win+F10");
    }

    #[test]
    fn a_key_can_be_written_with_spaces_hyphens_or_neither() {
        for text in ["Ctrl+PageUp", "Ctrl+page up", "Ctrl+PAGE-UP"] {
            let parsed: Hotkey = text.parse().unwrap_or_else(|_| panic!("{text} parses"));
            assert_eq!(parsed.key(), Key::PAGE_UP, "{text}");
        }
    }

    #[test]
    fn every_assignable_key_survives_being_written_down_and_read_back() {
        for key in Key::assignable() {
            let name = key.name();
            assert_eq!(
                Key::named(&name),
                Some(key),
                "{name} does not parse back to the key it was written from"
            );
        }
    }

    #[test]
    fn an_unknown_key_says_what_can_be_bound_instead() {
        let error = "Ctrl+Squirrel"
            .parse::<Hotkey>()
            .expect_err("there is no Squirrel key");

        assert_eq!(error, InvalidHotkey::UnknownKey("Squirrel".to_owned()));
        let message = error.to_string();
        assert!(message.contains("Squirrel"), "{message}");
        assert!(message.contains("F1 to F24"), "{message}");
    }

    #[test]
    fn modifiers_alone_and_two_keys_are_both_refused() {
        assert_eq!(
            "Ctrl+Shift".parse::<Hotkey>(),
            Err(InvalidHotkey::NoKey),
            "modifiers with no key",
        );
        assert_eq!(
            "Ctrl+F9+F10".parse::<Hotkey>(),
            Err(InvalidHotkey::TwoKeys),
            "two keys",
        );
    }

    /// The rule that stops a settings screen taking the keyboard away.
    #[test]
    fn a_typing_key_needs_ctrl_alt_or_win_and_shift_does_not_count() {
        let letter = Key::letter('a').expect("A is a letter");

        assert_eq!(
            Hotkey::new(Modifiers::NONE, letter),
            Err(InvalidHotkey::TypingKey),
            "a bare letter would swallow every A typed on this machine",
        );
        assert_eq!(
            Hotkey::new(Modifiers::SHIFT, letter),
            Err(InvalidHotkey::TypingKey),
            "Shift+A is how a capital A is typed",
        );
        assert!(
            Hotkey::new(Modifiers::CONTROL, letter).is_ok(),
            "Ctrl+A is a normal hotkey",
        );
        assert!(
            Hotkey::new(Modifiers::NONE, Key::function(9).expect("F9 exists")).is_ok(),
            "a bare function key takes nothing away from typing",
        );
        assert!(
            Hotkey::new(Modifiers::NONE, Key::SPACE).is_err(),
            "Space types a character too",
        );
    }

    #[test]
    fn keys_outside_the_ranges_windows_has_codes_for_are_not_keys() {
        assert_eq!(Key::function(0), None);
        assert_eq!(Key::function(25), None);
        assert_eq!(Key::digit(10), None);
        assert_eq!(Key::letter('£'), None);
        assert_eq!(Key::named("F25"), None);
    }

    /// The one that catches a copied constant going stale.
    ///
    /// This module writes the virtual-key codes and the `MOD_*` bits as
    /// literals, so that it compiles on a machine with no Windows headers. That
    /// is only safe if something holds the literals against the headers on the
    /// platform they describe, which is this.
    #[cfg(windows)]
    #[test]
    fn virtual_key_codes_match_the_windows_headers() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_0, VK_A, VK_DELETE, VK_END, VK_F1, VK_F24,
            VK_HOME, VK_INSERT, VK_NEXT, VK_PAUSE, VK_PRIOR, VK_SNAPSHOT, VK_SPACE, VK_Z,
        };

        assert_eq!(Modifiers::ALT.bits(), MOD_ALT.0);
        assert_eq!(Modifiers::CONTROL.bits(), MOD_CONTROL.0);
        assert_eq!(Modifiers::SHIFT.bits(), MOD_SHIFT.0);
        assert_eq!(Modifiers::WIN.bits(), MOD_WIN.0);

        assert_eq!(
            Key::function(1).map(Key::virtual_key),
            Some(u32::from(VK_F1.0))
        );
        assert_eq!(
            Key::function(24).map(Key::virtual_key),
            Some(u32::from(VK_F24.0)),
            "the function keys are only contiguous if F24 lands where the header says",
        );
        assert_eq!(
            Key::letter('A').map(Key::virtual_key),
            Some(u32::from(VK_A.0))
        );
        assert_eq!(
            Key::letter('z').map(Key::virtual_key),
            Some(u32::from(VK_Z.0)),
            "the letters are only contiguous if Z lands where the header says",
        );
        assert_eq!(Key::digit(0).map(Key::virtual_key), Some(u32::from(VK_0.0)));

        for (key, expected) in [
            (Key::SPACE, VK_SPACE),
            (Key::PAGE_UP, VK_PRIOR),
            (Key::PAGE_DOWN, VK_NEXT),
            (Key::END, VK_END),
            (Key::HOME, VK_HOME),
            (Key::PRINT_SCREEN, VK_SNAPSHOT),
            (Key::INSERT, VK_INSERT),
            (Key::DELETE, VK_DELETE),
            (Key::PAUSE, VK_PAUSE),
        ] {
            assert_eq!(
                key.virtual_key(),
                u32::from(expected.0),
                "{} is not the code the header gives it",
                key.name(),
            );
        }
    }
}
