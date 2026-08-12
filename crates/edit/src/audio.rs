//! The mix: what each audio track of the exported clip is made of, and how
//! loud it is.
//!
//! This is the payoff for the multi-track recording the whole product is built
//! around (SPEC.md sections 11 to 13). The recording already holds game audio,
//! system audio, the microphone and any voice-chat applications as separate
//! streams, so discovering afterwards that Discord was too loud costs a slider
//! and not a re-record ([issue
//! #85](https://github.com/wildware-uk/clipped/issues/85)).
//!
//! # A track of the clip, not a track of a recording
//!
//! An [`AudioTrack`] here is a track of the *output*, and it lists which stream
//! of which source feeds it. That indirection is what makes combining
//! recordings work: two sessions recorded on different days may carry Discord
//! on different stream indices, and joining them ([issue
//! #88](https://github.com/wildware-uk/clipped/issues/88)) has to put both
//! under one slider called "Discord" rather than under two. Naming the streams
//! explicitly also means the model never has to guess from a track name, which
//! is a guess that would be wrong for exactly the users who route their own
//! applications to their own tracks (SPEC.md section 12).

use core::time::Duration;

use serde::{Deserialize, Serialize};

use crate::source::SourceId;
use crate::time::duration_nanos;

/// The quietest gain that is worth distinguishing from silence.
///
/// Below about -60 dB nothing is audible under game audio, and the number is
/// here so that a slider dragged to its bottom stop produces a value the model
/// accepts rather than one it refuses.
pub const MINIMUM_GAIN_DB: f64 = -60.0;

/// The loudest boost an edit may apply.
///
/// A quiet microphone is the reason to allow any boost at all; +12 dB is four
/// times the amplitude, which is as far as a recording can usually be pushed
/// before its noise floor arrives with it.
pub const MAXIMUM_GAIN_DB: f64 = 12.0;

/// Which stream of which source feeds an output track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackInput {
    /// The document's source.
    pub source: SourceId,
    /// The audio stream index within that recording, numbered as the container
    /// numbers its audio streams: 0 for the first audio track, not for the
    /// video one.
    pub stream: u16,
}

impl TrackInput {
    /// Feeds `stream` of `source` into a track.
    #[must_use]
    pub const fn new(source: SourceId, stream: u16) -> Self {
        Self { source, stream }
    }
}

/// One audio track of the exported clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioTrack {
    /// What the track is called, in the export and in the editor.
    ///
    /// Unique within a document, and what the user sees beside the slider.
    /// `clipped-muxer` writes a name into every track it records for the same
    /// reason: a file with four unnamed audio tracks has to be identified by
    /// listening to it.
    pub name: String,
    /// The streams that feed it, at most one per source.
    pub inputs: Vec<TrackInput>,
    /// The level, in decibels, with `0.0` meaning "as recorded".
    #[serde(default)]
    pub gain_db: f64,
    /// Whether the track is silenced.
    #[serde(default)]
    pub muted: bool,
    /// Whether the track is soloed, which silences every track that is not.
    #[serde(default)]
    pub soloed: bool,
    /// How long the track takes to reach its level at the start of the clip.
    #[serde(default, with = "duration_nanos")]
    pub fade_in: Duration,
    /// How long the track takes to reach silence at the end of the clip.
    #[serde(default, with = "duration_nanos")]
    pub fade_out: Duration,
}

impl AudioTrack {
    /// A track named `name`, fed by `inputs`, at the level it was recorded.
    #[must_use]
    pub fn new(name: impl Into<String>, inputs: Vec<TrackInput>) -> Self {
        Self {
            name: name.into(),
            inputs,
            gain_db: 0.0,
            muted: false,
            soloed: false,
            fade_in: Duration::ZERO,
            fade_out: Duration::ZERO,
        }
    }

    /// The same track at `gain_db`.
    #[must_use]
    pub fn at_gain_db(mut self, gain_db: f64) -> Self {
        self.gain_db = gain_db;
        self
    }

    /// The same track, silenced.
    #[must_use]
    pub fn muted(mut self) -> Self {
        self.muted = true;
        self
    }

    /// The same track, soloed.
    #[must_use]
    pub fn soloed(mut self) -> Self {
        self.soloed = true;
        self
    }

    /// The same track, fading in and out over the given lengths.
    #[must_use]
    pub fn with_fades(mut self, fade_in: Duration, fade_out: Duration) -> Self {
        self.fade_in = fade_in;
        self.fade_out = fade_out;
        self
    }

    /// Whether the gain is a number an exporter can use.
    #[must_use]
    pub fn has_usable_gain(&self) -> bool {
        self.gain_db.is_finite()
            && self.gain_db >= MINIMUM_GAIN_DB
            && self.gain_db <= MAXIMUM_GAIN_DB
    }
}

/// What a track contributes to the export, once mute and solo are resolved.
///
/// Returned rather than a bare `f64` so that "silent" cannot be mistaken for
/// "no gain applied", which is the mistake that exports a muted microphone at
/// full volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackOutput {
    /// Nothing. The track is muted, or something else is soloed.
    Silent,
    /// Audible, at this level in decibels.
    Audible {
        /// The level to apply, in decibels.
        gain_db: f64,
    },
}

impl TrackOutput {
    /// Whether anything is heard.
    #[must_use]
    pub const fn is_audible(self) -> bool {
        matches!(self, Self::Audible { .. })
    }

    /// The level, or `None` when the track is silent.
    #[must_use]
    pub const fn gain_db(self) -> Option<f64> {
        match self {
            Self::Silent => None,
            Self::Audible { gain_db } => Some(gain_db),
        }
    }
}

/// Resolves `track` against whether anything in the document is soloed.
///
/// The rules, which [issue
/// #85](https://github.com/wildware-uk/clipped/issues/85) asks to be
/// predictable and documented, are the ones every mixing desk uses:
///
/// - **Mute wins.** A muted track is silent whatever else is set, including on
///   itself: soloing a muted track does not unmute it. Solo is a way of
///   listening to part of a mix, not a second mute button with the opposite
///   sense, and a control that quietly undoes another control is the kind of
///   surprise AGENTS.md section 27 is about.
/// - **Solo is exclusive.** If any track in the document is soloed, every track
///   that is not soloed is silent.
/// - **Solo affects nothing when nobody is soloed**, so the common case — no
///   solo anywhere — is just mute and gain.
pub(crate) fn resolve(track: &AudioTrack, any_soloed: bool) -> TrackOutput {
    if track.muted || (any_soloed && !track.soloed) {
        return TrackOutput::Silent;
    }
    TrackOutput::Audible {
        gain_db: track.gain_db,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str) -> AudioTrack {
        AudioTrack::new(name, vec![TrackInput::new(SourceId::new(0), 0)])
    }

    #[test]
    fn a_plain_track_plays_at_the_level_it_was_recorded() {
        assert_eq!(
            resolve(&track("Game"), false),
            TrackOutput::Audible { gain_db: 0.0 }
        );
    }

    #[test]
    fn a_muted_track_is_silent_even_when_it_is_the_one_soloed() {
        let both = track("Microphone").muted().soloed();
        assert_eq!(resolve(&both, true), TrackOutput::Silent);
        assert_eq!(resolve(&both, false), TrackOutput::Silent);
    }

    #[test]
    fn soloing_one_track_silences_the_others() {
        let soloed = track("Game").soloed();
        let other = track("Discord");

        assert!(resolve(&soloed, true).is_audible());
        assert_eq!(resolve(&other, true), TrackOutput::Silent);
    }

    #[test]
    fn solo_changes_nothing_when_nothing_is_soloed() {
        let quiet = track("Discord").at_gain_db(-8.5);
        assert_eq!(
            resolve(&quiet, false),
            TrackOutput::Audible { gain_db: -8.5 }
        );
    }

    #[test]
    fn a_silent_track_reports_no_level_at_all() {
        assert_eq!(TrackOutput::Silent.gain_db(), None);
        assert!(!TrackOutput::Silent.is_audible());
        assert_eq!(TrackOutput::Audible { gain_db: -3.0 }.gain_db(), Some(-3.0));
    }

    #[test]
    fn a_gain_outside_the_usable_range_is_rejected() {
        assert!(track("Game").has_usable_gain());
        assert!(track("Game").at_gain_db(MINIMUM_GAIN_DB).has_usable_gain());
        assert!(track("Game").at_gain_db(MAXIMUM_GAIN_DB).has_usable_gain());
        assert!(!track("Game").at_gain_db(-60.1).has_usable_gain());
        assert!(!track("Game").at_gain_db(12.1).has_usable_gain());
        assert!(!track("Game").at_gain_db(f64::NAN).has_usable_gain());
        assert!(!track("Game")
            .at_gain_db(f64::NEG_INFINITY)
            .has_usable_gain());
    }

    #[test]
    fn fades_are_written_as_nanoseconds_like_every_other_time() {
        let faded = track("Game").with_fades(Duration::from_millis(500), Duration::from_secs(1));
        let json = serde_json::to_value(&faded).expect("it serialises");

        assert_eq!(json["fade_in"], 500_000_000_u64);
        assert_eq!(json["fade_out"], 1_000_000_000_u64);
        assert_eq!(
            serde_json::from_value::<AudioTrack>(json).expect("it reads back"),
            faded
        );
    }

    #[test]
    fn the_optional_parts_of_a_track_may_be_left_out_of_the_document() {
        let read: AudioTrack =
            serde_json::from_str(r#"{"name":"Game","inputs":[{"source":0,"stream":0}]}"#)
                .expect("gain, mute, solo and fades all have defaults");

        assert_eq!(read, track("Game"));
    }
}
