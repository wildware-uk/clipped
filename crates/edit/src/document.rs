//! The document itself: what an edit is made of, and what makes one readable.
//!
//! # Lifecycle
//!
//! A document is built (or read), edited by the operations M11's other tickets
//! own, validated, and written back as text somebody else stores. Validation is
//! not optional at either end: [`EditDocument::read`] runs it before returning
//! a document, and [`EditDocument::write`] runs it before producing any text.
//! Refusing to *write* a broken document is the more valuable half — it means
//! nothing unreadable can reach the database in the first place, so an edit a
//! user made last year still opens.
//!
//! The builders here do not validate, because a half-built document is a normal
//! intermediate state: a source is added before the segment that plays it.
//!
//! # Threading
//!
//! Plain data. `EditDocument` is `Send` and `Sync`, owns everything it refers
//! to, and has no interior mutability, so the editor's thread and an export
//! running on another can hold their own clones and neither can surprise the
//! other.

use serde::{Deserialize, Serialize};

use crate::audio::{self, AudioTrack, TrackOutput};
use crate::error::{DocumentProblem, EditDocumentError};
use crate::framing::AspectRatio;
use crate::overlay::TextOverlay;
use crate::schema::{self, Loaded, SCHEMA_VERSION};
use crate::segment::Segment;
use crate::source::{RecordingId, Source, SourceId};
use crate::time::{OutputTime, SourceSpan};
use crate::timeline::{self, Placement};

/// A non-destructive edit: which recordings to play, which parts, and how.
///
/// Nothing in here refers to a file. The recordings are named by the library's
/// identifiers, and everything else is a description of what to do with them
/// when something eventually renders the clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditDocument {
    /// The format's version, not Clipped's.
    ///
    /// Written first so that a human opening the text sees it first, and read
    /// out of the raw JSON before anything else is trusted (see
    /// [`crate::schema`]).
    schema_version: u32,
    /// What the user called the clip.
    pub title: String,
    /// The shape of the exported file, or the sources' own shape when absent.
    #[serde(default)]
    pub aspect_ratio: Option<AspectRatio>,
    /// The recordings this edit draws on.
    pub sources: Vec<Source>,
    /// The material, in the order it plays.
    pub segments: Vec<Segment>,
    /// The audio tracks of the exported clip.
    #[serde(default)]
    pub audio_tracks: Vec<AudioTrack>,
    /// Text drawn over the picture.
    #[serde(default)]
    pub overlays: Vec<TextOverlay>,
}

impl EditDocument {
    /// An empty document titled `title`.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            title: title.into(),
            aspect_ratio: None,
            sources: Vec::new(),
            segments: Vec::new(),
            audio_tracks: Vec::new(),
            overlays: Vec::new(),
        }
    }

    /// A document playing `span` of one recording: the shape of an instant
    /// clip.
    ///
    /// SPEC.md section 20 describes creating a clip by dragging a range on the
    /// timeline and storing it "without initially copying/re-encoding video".
    /// That is this: one source, one segment, no rendering, and it is a full
    /// edit document from the moment it is created, so opening it in the editor
    /// ([issue #91](https://github.com/wildware-uk/clipped/issues/91)) needs no
    /// conversion step that could lose something.
    #[must_use]
    pub fn from_recording(
        title: impl Into<String>,
        recording: RecordingId,
        span: SourceSpan,
    ) -> Self {
        let source = SourceId::new(0);
        let mut document = Self::new(title);
        document.sources.push(Source::new(source, recording));
        document.segments.push(Segment::new(source, span));
        document
    }

    /// The format version this document is in.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The same document with `source` declared.
    #[must_use]
    pub fn with_source(mut self, source: Source) -> Self {
        self.sources.push(source);
        self
    }

    /// The same document with `segment` appended to the timeline.
    #[must_use]
    pub fn with_segment(mut self, segment: Segment) -> Self {
        self.segments.push(segment);
        self
    }

    /// The same document with `track` in its mix.
    #[must_use]
    pub fn with_audio_track(mut self, track: AudioTrack) -> Self {
        self.audio_tracks.push(track);
        self
    }

    /// The same document with `overlay` drawn over it.
    #[must_use]
    pub fn with_overlay(mut self, overlay: TextOverlay) -> Self {
        self.overlays.push(overlay);
        self
    }

    /// The same document exported at `aspect_ratio`.
    #[must_use]
    pub fn with_aspect_ratio(mut self, aspect_ratio: AspectRatio) -> Self {
        self.aspect_ratio = Some(aspect_ratio);
        self
    }

    /// Which recording a source refers to, if the document declares it.
    #[must_use]
    pub fn source(&self, id: SourceId) -> Option<&Source> {
        self.sources.iter().find(|source| source.id == id)
    }

    /// How long the clip is.
    ///
    /// `None` only for a document that has never been validated and holds a
    /// segment that cannot be read; anything from [`read`](Self::read) answers.
    #[must_use]
    pub fn output_duration_nanos(&self) -> Option<u64> {
        timeline::total_output_nanos(&self.segments)
    }

    /// Where `at` on the edited timeline comes from, or `None` past the end.
    ///
    /// The question an export ([issue
    /// #89](https://github.com/wildware-uk/clipped/issues/89)) and a preview
    /// ([issue #83](https://github.com/wildware-uk/clipped/issues/83)) both
    /// ask, and the reason they cannot disagree.
    #[must_use]
    pub fn locate(&self, at: OutputTime) -> Option<Placement> {
        timeline::locate(&self.segments, at)
    }

    /// Where a segment begins on the edited timeline.
    #[must_use]
    pub fn segment_start(&self, segment: usize) -> Option<OutputTime> {
        timeline::segment_start_nanos(&self.segments, segment).map(OutputTime::from_nanos)
    }

    /// Whether any track is soloed, which changes what every other one does.
    #[must_use]
    pub fn any_soloed(&self) -> bool {
        self.audio_tracks.iter().any(|track| track.soloed)
    }

    /// What audio track `index` contributes, once mute and solo are resolved.
    ///
    /// `None` when there is no such track. The rules are documented on
    /// [`crate::audio`].
    #[must_use]
    pub fn track_output(&self, index: usize) -> Option<TrackOutput> {
        let any_soloed = self.any_soloed();
        self.audio_tracks
            .get(index)
            .map(|track| audio::resolve(track, any_soloed))
    }

    /// The overlays on screen at `at`.
    pub fn overlays_at(&self, at: OutputTime) -> impl Iterator<Item = &TextOverlay> {
        self.overlays
            .iter()
            .filter(move |overlay| overlay.when.contains(at))
    }

    /// Reads a document, converting one written by an older build.
    ///
    /// # Errors
    ///
    /// [`EditDocumentError`], and the caller must **write nothing back** for
    /// any of them. A document from a newer build, or one this build cannot
    /// convert, is left exactly as it is.
    pub fn read(text: &str) -> Result<Loaded, EditDocumentError> {
        schema::read(text, SCHEMA_VERSION, schema::MIGRATIONS)
    }

    /// Writes the document as the text somebody else stores.
    ///
    /// Validates first, so that nothing unreadable can be persisted.
    ///
    /// # Errors
    ///
    /// [`EditDocumentError::Invalid`] if the document says something
    /// impossible.
    pub fn write(&self) -> Result<String, EditDocumentError> {
        self.validate()?;
        schema::write(self)
    }

    /// Checks everything that makes a document readable.
    ///
    /// The first problem found rather than all of them: they are usually
    /// consequences of each other, and the editor has one place to put a
    /// message.
    ///
    /// # Errors
    ///
    /// The first [`DocumentProblem`] in the document.
    pub fn validate(&self) -> Result<(), DocumentProblem> {
        self.validate_sources()?;
        self.validate_segments()?;
        let clip_nanos = self
            .output_duration_nanos()
            .ok_or(DocumentProblem::TimelineTooLong)?;
        self.validate_audio(clip_nanos)?;
        self.validate_overlays(clip_nanos)?;

        if let Some(aspect_ratio) = self.aspect_ratio {
            if !aspect_ratio.is_valid() {
                return Err(DocumentProblem::UnusableAspectRatio);
            }
        }
        Ok(())
    }

    fn validate_sources(&self) -> Result<(), DocumentProblem> {
        let mut seen: Vec<SourceId> = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            if seen.contains(&source.id) {
                return Err(DocumentProblem::DuplicateSource { id: source.id });
            }
            if source.recording.is_empty() {
                return Err(DocumentProblem::SourceWithoutRecording { id: source.id });
            }
            seen.push(source.id);
        }
        Ok(())
    }

    fn validate_segments(&self) -> Result<(), DocumentProblem> {
        for (index, segment) in self.segments.iter().enumerate() {
            if self.source(segment.source).is_none() {
                return Err(DocumentProblem::UnknownSource {
                    segment: index,
                    source: segment.source,
                });
            }
            if !segment.span.is_valid() {
                return Err(DocumentProblem::EmptySpan { segment: index });
            }
            if !segment.speed.is_valid() {
                return Err(DocumentProblem::UnusableSpeed { segment: index });
            }
            if segment.crop.is_some_and(|crop| !crop.is_valid()) {
                return Err(DocumentProblem::UnusableCrop { segment: index });
            }
            match segment.output_nanos() {
                None => return Err(DocumentProblem::TimelineTooLong),
                Some(0) => {
                    return Err(DocumentProblem::SegmentProducesNoOutput { segment: index });
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    fn validate_audio(&self, clip_nanos: u64) -> Result<(), DocumentProblem> {
        let mut names: Vec<&str> = Vec::with_capacity(self.audio_tracks.len());
        let mut streams: Vec<(SourceId, u16)> = Vec::new();

        for (index, track) in self.audio_tracks.iter().enumerate() {
            if track.name.trim().is_empty() {
                return Err(DocumentProblem::TrackWithoutName { track: index });
            }
            if names.contains(&track.name.as_str()) {
                return Err(DocumentProblem::DuplicateTrackName {
                    name: track.name.clone(),
                });
            }
            names.push(&track.name);

            if track.inputs.is_empty() {
                return Err(DocumentProblem::TrackWithoutInputs {
                    name: track.name.clone(),
                });
            }
            for input in &track.inputs {
                if self.source(input.source).is_none() {
                    return Err(DocumentProblem::TrackFromUnknownSource {
                        name: track.name.clone(),
                        source: input.source,
                    });
                }
                let stream = (input.source, input.stream);
                if streams.contains(&stream) {
                    return Err(DocumentProblem::StreamUsedTwice {
                        source: input.source,
                        stream: input.stream,
                    });
                }
                streams.push(stream);
            }

            if !track.has_usable_gain() {
                return Err(DocumentProblem::UnusableGain {
                    name: track.name.clone(),
                    gain_db: track.gain_db,
                });
            }

            let fades = u64::try_from(track.fade_in.as_nanos())
                .ok()
                .and_then(|fade_in| {
                    u64::try_from(track.fade_out.as_nanos())
                        .ok()
                        .and_then(|fade_out| fade_in.checked_add(fade_out))
                });
            if fades.is_none_or(|fades| fades > clip_nanos) {
                return Err(DocumentProblem::FadesLongerThanTheClip {
                    name: track.name.clone(),
                });
            }
        }
        Ok(())
    }

    fn validate_overlays(&self, clip_nanos: u64) -> Result<(), DocumentProblem> {
        for (index, overlay) in self.overlays.iter().enumerate() {
            if overlay.text.trim().is_empty() {
                return Err(DocumentProblem::EmptyOverlay { overlay: index });
            }
            if !overlay.when.is_valid() {
                return Err(DocumentProblem::EmptyOverlayRange { overlay: index });
            }
            if overlay.when.end().as_nanos() > clip_nanos {
                return Err(DocumentProblem::OverlayPastTheEnd {
                    overlay: index,
                    ends_at_nanos: overlay.when.end().as_nanos(),
                    clip_nanos,
                });
            }
            if !overlay.position.is_valid() {
                return Err(DocumentProblem::OverlayOffTheFrame { overlay: index });
            }
            if !overlay.has_usable_height() {
                return Err(DocumentProblem::UnusableOverlayHeight {
                    overlay: index,
                    height_percent: overlay.height_percent,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;
    use crate::audio::TrackInput;
    use crate::framing::CropRect;
    use crate::overlay::OverlayPosition;
    use crate::time::{OutputSpan, SourceTime, Speed};

    fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
        SourceSpan::new(
            SourceTime::from_nanos(start_nanos),
            SourceTime::from_nanos(end_nanos),
        )
        .expect("the test span ends after it starts")
    }

    fn output_span(start_nanos: u64, end_nanos: u64) -> OutputSpan {
        OutputSpan::new(
            OutputTime::from_nanos(start_nanos),
            OutputTime::from_nanos(end_nanos),
        )
        .expect("the test span ends after it starts")
    }

    /// A ten-second clip of one recording, which every test below starts from.
    fn simple() -> EditDocument {
        EditDocument::from_recording("Ace", RecordingId::new("rec-1"), span(0, 10_000_000_000))
    }

    #[test]
    fn an_instant_clip_is_one_source_and_one_segment() {
        let document = simple();

        assert_eq!(document.sources.len(), 1);
        assert_eq!(document.segments.len(), 1);
        assert_eq!(document.schema_version(), SCHEMA_VERSION);
        assert_eq!(document.output_duration_nanos(), Some(10_000_000_000));
        document.validate().expect("an instant clip is valid");
    }

    #[test]
    fn an_empty_document_is_valid_and_lasts_no_time() {
        let document = EditDocument::new("Untitled");

        document
            .validate()
            .expect("a clip somebody has deleted everything from is not corrupt");
        assert_eq!(document.output_duration_nanos(), Some(0));
        assert_eq!(document.locate(OutputTime::ZERO), None);
    }

    #[test]
    fn a_segment_may_not_play_a_recording_the_edit_never_declared() {
        let document = simple().with_segment(Segment::new(SourceId::new(9), span(0, 1_000)));

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::UnknownSource {
                segment: 1,
                source: SourceId::new(9),
            })
        );
    }

    #[test]
    fn two_sources_may_not_share_a_number() {
        let document =
            simple().with_source(Source::new(SourceId::new(0), RecordingId::new("rec-2")));

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::DuplicateSource {
                id: SourceId::new(0)
            })
        );
    }

    #[test]
    fn a_source_has_to_say_which_recording_it_is() {
        let document = EditDocument::new("Ace")
            .with_source(Source::new(SourceId::new(0), RecordingId::new("  ")));

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::SourceWithoutRecording {
                id: SourceId::new(0)
            })
        );
    }

    #[test]
    fn a_segment_that_contributes_no_output_is_refused() {
        // A hundred nanoseconds at a thousand times speed rounds to nothing,
        // and a segment nobody can see is a segment the editor would show and
        // the export would not.
        let document = EditDocument::new("Ace")
            .with_source(Source::new(SourceId::new(0), RecordingId::new("rec-1")))
            .with_segment(
                Segment::new(SourceId::new(0), span(0, 100))
                    .at_speed(Speed::new(1_000, 1).expect("a valid speed")),
            );

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::SegmentProducesNoOutput { segment: 0 })
        );
    }

    #[test]
    fn an_unusable_speed_is_named_as_a_speed_rather_than_as_a_length() {
        let mut document = simple();
        document.segments[0].speed = serde_json::from_str(r#"{"numerator":0,"denominator":1}"#)
            .expect("the shape is right even though the value is not");

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::UnusableSpeed { segment: 0 })
        );
    }

    #[test]
    fn a_crop_outside_the_frame_is_refused() {
        let mut document = simple();
        document.segments[0].crop = Some(CropRect {
            x: 0.9,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        });

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::UnusableCrop { segment: 0 })
        );
    }

    #[test]
    fn an_audio_track_needs_a_name_that_is_not_shared() {
        let document = simple()
            .with_audio_track(AudioTrack::new(
                "Game",
                vec![TrackInput::new(SourceId::new(0), 0)],
            ))
            .with_audio_track(AudioTrack::new(
                "Game",
                vec![TrackInput::new(SourceId::new(0), 1)],
            ));

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::DuplicateTrackName {
                name: "Game".to_owned()
            })
        );

        let unnamed = simple().with_audio_track(AudioTrack::new(
            "   ",
            vec![TrackInput::new(SourceId::new(0), 0)],
        ));
        assert_eq!(
            unnamed.validate(),
            Err(DocumentProblem::TrackWithoutName { track: 0 })
        );
    }

    #[test]
    fn an_audio_track_fed_by_nothing_is_refused_rather_than_silently_silent() {
        let document = simple().with_audio_track(AudioTrack::new("Game", Vec::new()));

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::TrackWithoutInputs {
                name: "Game".to_owned()
            })
        );
    }

    #[test]
    fn one_recorded_stream_may_not_feed_two_tracks_of_the_export() {
        let document = simple()
            .with_audio_track(AudioTrack::new(
                "Game",
                vec![TrackInput::new(SourceId::new(0), 0)],
            ))
            .with_audio_track(AudioTrack::new(
                "Also game",
                vec![TrackInput::new(SourceId::new(0), 0)],
            ));

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::StreamUsedTwice {
                source: SourceId::new(0),
                stream: 0,
            })
        );
    }

    #[test]
    fn an_audio_track_may_not_draw_on_a_recording_the_edit_never_declared() {
        let document = simple().with_audio_track(AudioTrack::new(
            "Discord",
            vec![TrackInput::new(SourceId::new(4), 2)],
        ));

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::TrackFromUnknownSource {
                name: "Discord".to_owned(),
                source: SourceId::new(4),
            })
        );
    }

    #[test]
    fn a_level_no_exporter_could_apply_is_refused() {
        let document = simple().with_audio_track(
            AudioTrack::new("Game", vec![TrackInput::new(SourceId::new(0), 0)])
                .at_gain_db(f64::NAN),
        );

        assert!(matches!(
            document.validate(),
            Err(DocumentProblem::UnusableGain { .. })
        ));
    }

    #[test]
    fn fades_may_not_be_longer_than_the_clip_they_are_on() {
        let document = simple().with_audio_track(
            AudioTrack::new("Game", vec![TrackInput::new(SourceId::new(0), 0)])
                .with_fades(Duration::from_secs(6), Duration::from_secs(6)),
        );

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::FadesLongerThanTheClip {
                name: "Game".to_owned()
            })
        );

        let fits = simple().with_audio_track(
            AudioTrack::new("Game", vec![TrackInput::new(SourceId::new(0), 0)])
                .with_fades(Duration::from_secs(5), Duration::from_secs(5)),
        );
        fits.validate().expect("fades that exactly fill the clip");
    }

    #[test]
    fn an_overlay_may_not_outlast_the_clip() {
        let document = simple().with_overlay(TextOverlay::new(
            "Ace",
            output_span(9_000_000_000, 11_000_000_000),
        ));

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::OverlayPastTheEnd {
                overlay: 0,
                ends_at_nanos: 11_000_000_000,
                clip_nanos: 10_000_000_000,
            })
        );
    }

    #[test]
    fn an_overlay_ending_exactly_at_the_end_of_the_clip_is_on_screen_for_its_last_moment() {
        // The boundary the test above does not reach, and the one a `>=` would
        // get wrong. Ranges are half-open, so an overlay ending at the clip's
        // duration ends where the clip does and still covers its final moment;
        // one nanosecond past that is the first value the model refuses. Both
        // sides are asserted here because a validator a nanosecond too strict
        // rejects exactly the overlay the editor produces when a user drags a
        // title to the end of the timeline.
        let clip_nanos = 10_000_000_000;

        let exactly = simple().with_overlay(TextOverlay::new(
            "Ace",
            output_span(9_000_000_000, clip_nanos),
        ));
        exactly
            .validate()
            .expect("an overlay ending where the clip ends is inside the clip");
        assert_eq!(
            exactly
                .overlays_at(OutputTime::from_nanos(clip_nanos - 1))
                .count(),
            1,
            "and it is still on screen for the clip's last nanosecond"
        );

        let one_nanosecond_over = simple().with_overlay(TextOverlay::new(
            "Ace",
            output_span(9_000_000_000, clip_nanos + 1),
        ));
        assert_eq!(
            one_nanosecond_over.validate(),
            Err(DocumentProblem::OverlayPastTheEnd {
                overlay: 0,
                ends_at_nanos: clip_nanos + 1,
                clip_nanos,
            }),
            "and one nanosecond past the end is the first value refused"
        );
    }

    #[test]
    fn an_overlay_has_to_say_something_somewhere_on_the_frame() {
        let empty = simple().with_overlay(TextOverlay::new(" ", output_span(0, 1_000)));
        assert_eq!(
            empty.validate(),
            Err(DocumentProblem::EmptyOverlay { overlay: 0 })
        );

        let off_frame = simple().with_overlay(
            TextOverlay::new("Ace", output_span(0, 1_000)).at(OverlayPosition { x: 1.5, y: 0.5 }),
        );
        assert_eq!(
            off_frame.validate(),
            Err(DocumentProblem::OverlayOffTheFrame { overlay: 0 })
        );

        let invisible =
            simple().with_overlay(TextOverlay::new("Ace", output_span(0, 1_000)).sized(0));
        assert_eq!(
            invisible.validate(),
            Err(DocumentProblem::UnusableOverlayHeight {
                overlay: 0,
                height_percent: 0,
            })
        );
    }

    #[test]
    fn an_aspect_ratio_with_a_zero_in_it_is_refused() {
        let mut document = simple();
        document.aspect_ratio = serde_json::from_str(r#"{"width":16,"height":0}"#)
            .expect("the shape is right even though the value is not");

        assert_eq!(
            document.validate(),
            Err(DocumentProblem::UnusableAspectRatio)
        );
    }

    #[test]
    fn the_overlays_on_screen_at_a_moment_are_the_ones_whose_range_covers_it() {
        let document = simple()
            .with_overlay(TextOverlay::new("first", output_span(0, 3_000_000_000)))
            .with_overlay(TextOverlay::new(
                "second",
                output_span(2_000_000_000, 5_000_000_000),
            ));

        let at = |nanos| {
            document
                .overlays_at(OutputTime::from_nanos(nanos))
                .map(|overlay| overlay.text.as_str())
                .collect::<Vec<_>>()
        };

        assert_eq!(at(1_000_000_000), vec!["first"]);
        assert_eq!(at(2_500_000_000), vec!["first", "second"]);
        assert_eq!(at(3_000_000_000), vec!["second"]);
        assert!(at(5_000_000_000).is_empty());
    }

    #[test]
    fn the_mix_resolves_solo_across_the_whole_document() {
        let document = simple()
            .with_audio_track(
                AudioTrack::new("Game", vec![TrackInput::new(SourceId::new(0), 0)])
                    .at_gain_db(-4.0),
            )
            .with_audio_track(
                AudioTrack::new("Discord", vec![TrackInput::new(SourceId::new(0), 1)]).soloed(),
            );

        assert!(document.any_soloed());
        assert_eq!(document.track_output(0), Some(TrackOutput::Silent));
        assert_eq!(
            document.track_output(1),
            Some(TrackOutput::Audible { gain_db: 0.0 })
        );
        assert_eq!(document.track_output(2), None);
    }

    #[test]
    fn writing_a_broken_document_fails_instead_of_storing_it() {
        let document = simple().with_segment(Segment::new(SourceId::new(9), span(0, 1_000)));

        let error = document
            .write()
            .expect_err("an unreadable document must not reach the database");
        assert!(matches!(
            error,
            EditDocumentError::Invalid(DocumentProblem::UnknownSource { segment: 1, .. })
        ));
    }
}
