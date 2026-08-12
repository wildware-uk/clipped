//! The mix: what each audio track of the exported clip is made of, how loud it
//! is, and how it fades.
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
//!
//! # What is an edit, and what is only listening
//!
//! Three of the four controls describe the clip: a **level**, a **mute** and a
//! **fade** are all things the exported file must carry, so all three are
//! fields of the track and all three are saved.
//!
//! **Solo is not.** Soloing a track is how a user listens to part of their own
//! mix while they work, and it is undone by clicking it again a second later.
//! It is therefore [`Solo`] — a value the editor holds beside the document,
//! never inside it — and the exporter is never given one. Two consequences,
//! both deliberate:
//!
//! - A clip cannot be exported with three of its four tracks silently missing
//!   because somebody left a solo on before pressing Export. The document says
//!   what the file contains, and everything in the document reaches the file.
//! - "Two tracks are soloed" cannot arise. A solo names **at most one track**,
//!   so soloing a second track moves the solo rather than adding to it, and
//!   there is no rule to guess about what several of them mean.
//!
//! `docs/editing.md` argues both at length; this is the implementation of it.

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
///
/// Every field here is part of the edit, so every field is saved and every
/// field reaches the export. Solo is [not one of them](self#what-is-an-edit-and-what-is-only-listening).
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
    /// Whether the track is silenced for good, in the export as well as the
    /// preview.
    #[serde(default)]
    pub muted: bool,
    /// How long the track takes to reach its level at the start of the clip.
    ///
    /// A length of **output** time, measured from the start of the clip, not a
    /// position in any recording: see [`fade_amplitude`].
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

    /// Whether the track carries exactly the samples that were recorded.
    ///
    /// The model's half of the question [issue
    /// #89](https://github.com/wildware-uk/clipped/issues/89) asks about every
    /// track: a level of `0.0` dB with no mute and no fade changes no sample,
    /// so the recorded stream can be muxed into the export without a decoder.
    /// Anything else has to be produced sample by sample.
    ///
    /// It answers about the *mix* only, exactly as
    /// [`Segment::is_untransformed`](crate::Segment::is_untransformed) answers
    /// about a segment's own transformations. Whether a copy is possible at all
    /// also depends on the codecs in the file, which this crate never opens,
    /// and on the track being fed by a single stream.
    #[must_use]
    pub fn is_unmixed(&self) -> bool {
        self.gain_db == 0.0 && !self.muted && self.fade_in.is_zero() && self.fade_out.is_zero()
    }

    /// How much of the clip the two fades occupy, or `None` past `u64`.
    #[must_use]
    pub(crate) fn total_fade_nanos(&self) -> Option<u64> {
        nanos(self.fade_in).checked_add(nanos(self.fade_out))
    }
}

/// A [`Duration`] as nanoseconds, saturating where it cannot be one.
///
/// Saturating rather than refusing because every caller is asking a question
/// about a clip, and a clip is never five hundred and eighty-four years long:
/// a fade that says it is compares as longer than the clip either way, which is
/// what validation is about to refuse.
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Which track, if any, the editor is listening to on its own.
///
/// **Playback state, not an edit.** It is held beside a document rather than
/// inside one, is never written to storage, and never reaches an export — the
/// reasoning is [on this module](self#what-is-an-edit-and-what-is-only-listening).
///
/// At most one track, so the question "what do two solos mean?" has no answer
/// to get wrong: [`toggled`](Self::toggled) moves the solo to whichever track
/// was clicked, and clicking the soloed one again clears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Solo {
    /// The track being listened to, by its index in the document.
    listening_to: Option<usize>,
}

impl Solo {
    /// Listening to the whole mix, which is where an editor starts.
    pub const NONE: Self = Self { listening_to: None };

    /// Listening to `track` alone.
    #[must_use]
    pub const fn on(track: usize) -> Self {
        Self {
            listening_to: Some(track),
        }
    }

    /// The track being listened to on its own, if there is one.
    #[must_use]
    pub const fn track(self) -> Option<usize> {
        self.listening_to
    }

    /// Whether anything at all is soloed.
    #[must_use]
    pub const fn is_listening(self) -> bool {
        self.listening_to.is_some()
    }

    /// Whether the solo silences `track`, which is true of every other track.
    #[must_use]
    pub const fn silences(self, track: usize) -> bool {
        match self.listening_to {
            None => false,
            Some(soloed) => soloed != track,
        }
    }

    /// The state after the solo button on `track` is pressed.
    ///
    /// Pressing it on the soloed track clears the solo; pressing it on any
    /// other track moves the solo there. There is deliberately no way to reach
    /// a state with two soloed tracks.
    #[must_use]
    pub const fn toggled(self, track: usize) -> Self {
        match self.listening_to {
            Some(soloed) if soloed == track => Self::NONE,
            _ => Self::on(track),
        }
    }
}

/// What a track contributes to the export, before its fades are applied.
///
/// Returned rather than a bare `f64` so that "silent" cannot be mistaken for
/// "no gain applied", which is the mistake that exports a muted microphone at
/// full volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackOutput {
    /// Nothing. The track is muted, or the editor is soloing another one.
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

    /// The level as a multiplier on the recorded samples.
    ///
    /// `1.0` is the level as recorded, `0.0` is silence, and the conversion is
    /// the ordinary one: an amplitude ratio of `10^(dB/20)`, so -6 dB is very
    /// nearly half. Decibels are what the user drags and what the document
    /// stores; a multiplier is what anything rendering samples actually needs,
    /// and having one function for it means the preview and the export cannot
    /// each round their own way.
    #[must_use]
    pub fn amplitude(self) -> f64 {
        match self {
            Self::Silent => 0.0,
            Self::Audible { gain_db } => amplitude_from_db(gain_db),
        }
    }
}

/// An amplitude multiplier from a level in decibels.
fn amplitude_from_db(gain_db: f64) -> f64 {
    10.0_f64.powf(gain_db / 20.0)
}

/// What `track` contributes to the **export**.
///
/// Mute and the level, and nothing else: an export is never handed a [`Solo`],
/// so nothing about how the user was listening can reach the file.
pub(crate) fn resolve(track: &AudioTrack) -> TrackOutput {
    if track.muted {
        return TrackOutput::Silent;
    }
    TrackOutput::Audible {
        gain_db: track.gain_db,
    }
}

/// What `track` contributes to the **preview**, given the editor's `solo`.
///
/// The rules [issue #85](https://github.com/wildware-uk/clipped/issues/85) asks
/// to be predictable and documented:
///
/// - **Mute wins.** A muted track is silent whatever else is set, including on
///   itself: soloing a muted track does not unmute it. Solo is a way of
///   listening to part of a mix, not a second mute button with the opposite
///   sense, and a control that quietly undoes another control is the kind of
///   surprise AGENTS.md section 27 is about.
/// - **Solo is exclusive.** While a track is soloed, every other track is
///   silent — in the preview, and only there.
/// - **Solo affects nothing when nothing is soloed**, so the common case is
///   just mute and gain, and the preview matches the export exactly.
pub(crate) fn monitor(track: &AudioTrack, index: usize, solo: Solo) -> TrackOutput {
    if solo.silences(index) {
        return TrackOutput::Silent;
    }
    resolve(track)
}

/// The fade envelope of `track` at `at_nanos` of a clip lasting `clip_nanos`.
///
/// A multiplier between `0.0` and `1.0` to apply on top of the track's level.
/// The curve is defined here, once, so that a preview and an export cannot draw
/// two different ones: it rises **linearly in amplitude** from zero across
/// `fade_in`, and falls linearly to zero across `fade_out`.
///
/// ```text
///        ╱▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔╲
///       ╱                       ╲
///   0s ╯   fade_in      fade_out ╰ clip
/// ```
///
/// Both lengths are **output** time measured from the ends of the clip, which
/// is the only anchor that survives editing: material moves in output time
/// every time a section is deleted, so a fade pinned to material would be a
/// fade that wanders off the start of the clip the first time the user trims
/// it. The consequence is the honest one — trimming a clip changes which
/// material the fade covers, and not the shape of the fade.
///
/// Fades that overlap are refused by
/// [`EditDocument::validate`](crate::EditDocument::validate), but the two
/// factors are multiplied rather than assumed exclusive, so this stays a real
/// answer between zero and one for a document that has not been through it.
pub(crate) fn fade_amplitude(track: &AudioTrack, at_nanos: u64, clip_nanos: u64) -> f64 {
    if at_nanos >= clip_nanos {
        // The timeline is half-open: the clip's own duration is the first
        // moment that is past the end of it, and nothing plays there.
        return 0.0;
    }

    let mut amplitude = 1.0;
    let fade_in = nanos(track.fade_in);
    if at_nanos < fade_in {
        amplitude *= at_nanos as f64 / fade_in as f64;
    }
    let fade_out = nanos(track.fade_out);
    let remaining = clip_nanos - at_nanos;
    if remaining <= fade_out {
        amplitude *= remaining as f64 / fade_out as f64;
    }
    amplitude
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str) -> AudioTrack {
        AudioTrack::new(name, vec![TrackInput::new(SourceId::new(0), 0)])
    }

    const SECOND: u64 = 1_000_000_000;

    #[test]
    fn a_plain_track_plays_at_the_level_it_was_recorded() {
        assert_eq!(
            resolve(&track("Game")),
            TrackOutput::Audible { gain_db: 0.0 }
        );
    }

    #[test]
    fn a_muted_track_is_silent_even_when_it_is_the_one_soloed() {
        let muted = track("Microphone").muted();

        assert_eq!(monitor(&muted, 0, Solo::on(0)), TrackOutput::Silent);
        assert_eq!(monitor(&muted, 0, Solo::NONE), TrackOutput::Silent);
        assert_eq!(resolve(&muted), TrackOutput::Silent);
    }

    #[test]
    fn soloing_one_track_silences_the_others_in_the_preview_only() {
        let soloed = track("Game").at_gain_db(-4.0);
        let other = track("Discord");
        let solo = Solo::on(0);

        assert_eq!(
            monitor(&soloed, 0, solo),
            TrackOutput::Audible { gain_db: -4.0 }
        );
        assert_eq!(monitor(&other, 1, solo), TrackOutput::Silent);
        assert!(
            resolve(&other).is_audible(),
            "the export is never given a solo, so the other track is still in the file"
        );
    }

    #[test]
    fn solo_changes_nothing_when_nothing_is_soloed() {
        let quiet = track("Discord").at_gain_db(-8.5);

        assert_eq!(
            monitor(&quiet, 2, Solo::NONE),
            TrackOutput::Audible { gain_db: -8.5 }
        );
        assert_eq!(monitor(&quiet, 2, Solo::NONE), resolve(&quiet));
    }

    #[test]
    fn a_solo_names_one_track_so_two_of_them_cannot_be_asked_about() {
        // The whole reason solo is not a field on a track: pressing solo on a
        // second track moves it rather than adding to it, and there is no state
        // in which two tracks are soloed for a rule to have to arbitrate.
        let solo = Solo::NONE.toggled(0);
        assert_eq!(solo.track(), Some(0));
        assert!(solo.is_listening());

        let moved = solo.toggled(2);
        assert_eq!(
            moved.track(),
            Some(2),
            "the solo moved to the track clicked"
        );
        assert!(moved.silences(0), "and the one it left is silent again");
        assert!(!moved.silences(2));

        assert_eq!(moved.toggled(2), Solo::NONE, "clicking it again clears it");
        assert!(!Solo::NONE.silences(0));
        assert_eq!(Solo::default(), Solo::NONE);
    }

    #[test]
    fn a_silent_track_reports_no_level_at_all() {
        assert_eq!(TrackOutput::Silent.gain_db(), None);
        assert!(!TrackOutput::Silent.is_audible());
        assert_eq!(TrackOutput::Silent.amplitude(), 0.0);
        assert_eq!(TrackOutput::Audible { gain_db: -3.0 }.gain_db(), Some(-3.0));
    }

    #[test]
    fn a_level_in_decibels_becomes_the_multiplier_every_renderer_needs() {
        let amplitude = |gain_db| TrackOutput::Audible { gain_db }.amplitude();

        assert!((amplitude(0.0) - 1.0).abs() < 1e-12, "as recorded is one");
        assert!(
            (amplitude(-6.020_599_913_279_624) - 0.5).abs() < 1e-9,
            "-6 dB is half the amplitude"
        );
        assert!(
            (amplitude(MAXIMUM_GAIN_DB) - 3.981_071_705_534_972).abs() < 1e-9,
            "and +12 dB is four times it"
        );
        assert!(
            amplitude(MINIMUM_GAIN_DB) < 0.001_1 && amplitude(MINIMUM_GAIN_DB) > 0.000_9,
            "the quietest level the model allows is a thousandth of the recording"
        );
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
                .expect("gain, mute and fades all have defaults");

        assert_eq!(read, track("Game"));
    }

    #[test]
    fn a_solo_has_nowhere_to_be_written_in_a_track() {
        let text = serde_json::to_string(&track("Game")).expect("it serialises");
        assert!(
            !text.contains("solo"),
            "a solo saved with the clip is a solo that reaches the export: {text}"
        );

        let error = serde_json::from_str::<AudioTrack>(
            r#"{"name":"Game","inputs":[{"source":0,"stream":0}],"soloed":true}"#,
        )
        .expect_err("a track carrying a solo is not a track this build understands");
        assert!(error.to_string().contains("soloed"), "{error}");
    }

    #[test]
    fn a_fade_rises_from_silence_and_falls_back_to_it() {
        let faded = track("Game").with_fades(Duration::from_secs(2), Duration::from_secs(4));
        let clip = 20 * SECOND;
        let at = |at_nanos| fade_amplitude(&faded, at_nanos, clip);

        assert_eq!(at(0), 0.0, "the clip opens in silence");
        assert!(
            (at(SECOND) - 0.5).abs() < 1e-12,
            "halfway up after a second"
        );
        assert_eq!(at(2 * SECOND), 1.0, "and at its level when the fade ends");
        assert_eq!(at(10 * SECOND), 1.0, "the middle is untouched");

        assert_eq!(at(16 * SECOND), 1.0, "the fade out starts at its level");
        assert!((at(18 * SECOND) - 0.5).abs() < 1e-12);
        assert!(at(clip - 1) < 1e-8, "and reaches silence at the end");
        assert_eq!(at(clip), 0.0, "past the end there is nothing to hear");
    }

    #[test]
    fn a_track_with_no_fades_is_at_its_level_from_the_first_moment_to_the_last() {
        let plain = track("Game");
        let clip = 5 * SECOND;

        assert_eq!(fade_amplitude(&plain, 0, clip), 1.0);
        assert_eq!(fade_amplitude(&plain, clip - 1, clip), 1.0);
        assert_eq!(fade_amplitude(&plain, clip, clip), 0.0);
    }

    #[test]
    fn fades_that_overlap_dip_in_the_middle_rather_than_adding_up_past_the_level() {
        // Unreachable through `validate`, which refuses fades longer than the
        // clip — but reachable in a document that has not been through it, and
        // a multiplier over 1.0 there would be an export that clips where the
        // preview did not.
        let both = track("Game").with_fades(Duration::from_secs(8), Duration::from_secs(8));
        let clip = 10 * SECOND;

        let middle = fade_amplitude(&both, 5 * SECOND, clip);
        assert!(
            (middle - 0.625 * 0.625).abs() < 1e-12,
            "the two envelopes multiply: {middle}"
        );
        for step in 0..=100 {
            let amplitude = fade_amplitude(&both, clip * step / 100, clip);
            assert!(
                (0.0..=1.0).contains(&amplitude),
                "a fade multiplier is never outside 0..=1, and is {amplitude} at step {step}"
            );
        }
    }

    #[test]
    fn fades_that_exactly_meet_hand_over_at_the_join() {
        let both = track("Game").with_fades(Duration::from_secs(5), Duration::from_secs(5));
        let clip = 10 * SECOND;

        assert!((fade_amplitude(&both, 5 * SECOND, clip) - 1.0).abs() < 1e-12);
        assert!((fade_amplitude(&both, 4 * SECOND, clip) - 0.8).abs() < 1e-12);
        assert!((fade_amplitude(&both, 6 * SECOND, clip) - 0.8).abs() < 1e-12);
    }

    #[test]
    fn a_track_is_unmixed_only_when_nothing_at_all_is_done_to_it() {
        assert!(track("Game").is_unmixed());
        assert!(!track("Game").at_gain_db(-0.5).is_unmixed());
        assert!(!track("Game").muted().is_unmixed());
        assert!(!track("Game")
            .with_fades(Duration::from_nanos(1), Duration::ZERO)
            .is_unmixed());
        assert!(!track("Game")
            .with_fades(Duration::ZERO, Duration::from_nanos(1))
            .is_unmixed());
    }

    #[test]
    fn the_fades_of_a_track_are_measured_together() {
        let faded = track("Game").with_fades(Duration::from_secs(2), Duration::from_secs(3));
        assert_eq!(faded.total_fade_nanos(), Some(5 * SECOND));

        let absurd = track("Game").with_fades(Duration::MAX, Duration::from_secs(1));
        assert_eq!(
            absurd.total_fade_nanos(),
            None,
            "a fade longer than an edit can represent is reported rather than wrapped"
        );
    }
}
