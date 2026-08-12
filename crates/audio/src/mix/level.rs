//! How loud one source is in the compatibility mix.

use core::fmt;

/// The level one source is mixed at, as a linear amplitude multiplier.
///
/// Linear rather than decibels because that is what the mixing loop multiplies
/// by, and converting per buffer would be arithmetic in a hot path to express a
/// number that only changes when somebody moves a slider. A user interface that
/// works in decibels builds one of these with [`from_decibels`](Self::from_decibels).
///
/// # This is the *only* place a level is applied
///
/// A level belongs to the mix and to nothing else (AGENTS.md section 21): the
/// isolated track a source gets carries the samples the device produced, at the
/// amplitude it produced them, whatever the user has done to this slider. That
/// is what makes the isolated tracks worth having — somebody who turns the game
/// down to hear themselves talk has not thrown the game's audio away, they have
/// changed one track of the recording.
///
/// # Boosting
///
/// Levels above unity are allowed, because a microphone that is genuinely too
/// quiet against a game is a real complaint and refusing to boost it would make
/// the mix useless in exactly the case it exists for. Nothing here clips as a
/// result: the mixer's limiter holds the mix under full scale however hard the
/// sources are driven into it.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Level(f32);

impl Level {
    /// The source is mixed at exactly the amplitude it was captured at.
    pub const UNITY: Self = Self(1.0);

    /// The source contributes nothing to the mix.
    ///
    /// A muted source is still recorded on its own track. Muting is a decision
    /// about what the mix sounds like, not about what the recording contains,
    /// and a user who muted the microphone in the mix and then found their
    /// commentary missing from every track would have lost something nobody can
    /// give back (AGENTS.md section 56).
    pub const SILENT: Self = Self(0.0);

    /// The loudest a source may be mixed at: +18 dB.
    ///
    /// A bound rather than an opinion. There is no amount of gain that makes a
    /// microphone Windows had muted audible, and an unbounded multiplier turns
    /// a mistyped configuration value into a mix made of one source.
    pub const MAX_LINEAR: f32 = 8.0;

    /// Takes a linear multiplier.
    ///
    /// Returns [`None`] for anything that is not a finite number between zero
    /// and [`MAX_LINEAR`](Self::MAX_LINEAR) inclusive. Refused rather than
    /// clamped: a level that arrived as `NaN` is a bug somewhere upstream, and
    /// silently treating it as unity would hide it in a recording rather than
    /// in a log.
    #[must_use]
    pub fn linear(multiplier: f32) -> Option<Self> {
        if multiplier.is_finite() && (0.0..=Self::MAX_LINEAR).contains(&multiplier) {
            Some(Self(multiplier))
        } else {
            None
        }
    }

    /// Takes a level in decibels relative to the captured amplitude, so `0.0` is
    /// [`UNITY`](Self::UNITY) and `-6.0` is about half.
    ///
    /// Anything at or below [`SILENCE_FLOOR_DB`](Self::SILENCE_FLOOR_DB) is
    /// [`SILENT`](Self::SILENT) — there is no decibel value for silence, and a
    /// slider dragged to its bottom has to produce one. Above
    /// [`MAX_LINEAR`](Self::MAX_LINEAR) the result is [`None`], for the reason
    /// [`linear`](Self::linear) gives.
    #[must_use]
    pub fn from_decibels(decibels: f32) -> Option<Self> {
        if !decibels.is_finite() {
            return None;
        }
        if decibels <= Self::SILENCE_FLOOR_DB {
            return Some(Self::SILENT);
        }
        Self::linear(10.0_f32.powf(decibels / 20.0))
    }

    /// The level at which a source is treated as muted rather than very quiet:
    /// −96 dB, which is the noise floor of 16-bit audio.
    pub const SILENCE_FLOOR_DB: f32 = -96.0;

    /// The multiplier the mixing loop uses.
    #[must_use]
    pub const fn as_linear(self) -> f32 {
        self.0
    }

    /// Whether this source contributes nothing at all, which is what lets the
    /// mixer skip work it could not hear.
    #[must_use]
    pub fn is_silent(self) -> bool {
        self.0 == 0.0
    }
}

impl Default for Level {
    /// [`UNITY`](Self::UNITY): a source nobody has expressed an opinion about is
    /// mixed as it was captured.
    fn default() -> Self {
        Self::UNITY
    }
}

impl fmt::Display for Level {
    /// In decibels, because that is how a person reads a level.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_silent() {
            return formatter.write_str("silent");
        }
        write!(formatter, "{:+.1} dB", 20.0 * self.0.log10())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_in_decibels_is_the_multiplier_a_mix_applies() {
        assert_eq!(Level::from_decibels(0.0), Some(Level::UNITY));

        let half = Level::from_decibels(-6.020_6).expect("−6 dB is a level");
        assert!(
            (half.as_linear() - 0.5).abs() < 1e-4,
            "−6 dB should halve the amplitude, got {}",
            half.as_linear()
        );

        let boost = Level::from_decibels(6.020_6).expect("+6 dB is a level");
        assert!((boost.as_linear() - 2.0).abs() < 1e-4, "{boost}");
    }

    #[test]
    fn the_bottom_of_a_slider_is_silence_rather_than_a_very_small_number() {
        // There is no decibel value for silence, so a slider dragged to its
        // bottom would otherwise leave the source audible in the mix at some
        // tiny amplitude — a mute control that does not mute (AGENTS.md
        // section 27).
        assert_eq!(Level::from_decibels(-96.0), Some(Level::SILENT));
        assert_eq!(Level::from_decibels(-120.0), Some(Level::SILENT));
        assert!(Level::SILENT.is_silent());
        assert!(!Level::UNITY.is_silent());
    }

    #[test]
    fn a_level_that_is_not_a_number_is_refused_rather_than_treated_as_unity() {
        assert_eq!(Level::linear(f32::NAN), None);
        assert_eq!(Level::linear(f32::INFINITY), None);
        assert_eq!(Level::linear(-0.5), None);
        assert_eq!(Level::from_decibels(f32::NAN), None);

        // And the bound is real: a mistyped configuration value must not become
        // a mix made of one source.
        assert!(Level::linear(Level::MAX_LINEAR).is_some());
        assert_eq!(Level::linear(Level::MAX_LINEAR + 0.1), None);
        assert_eq!(Level::from_decibels(30.0), None);
    }

    #[test]
    fn a_level_prints_the_way_a_person_reads_one() {
        assert_eq!(Level::UNITY.to_string(), "+0.0 dB");
        assert_eq!(Level::SILENT.to_string(), "silent");
        assert_eq!(
            Level::linear(0.5).expect("a real level").to_string(),
            "-6.0 dB"
        );
    }
}
