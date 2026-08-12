//! A piece of a recording, and where it sits in the clip.
//!
//! # Why an edit is a list of segments and not a list of cuts
//!
//! AGENTS.md section 57 gives `cuts` as one of the things an edit is made of,
//! and the obvious reading is a list of removed ranges over a source. That
//! shape was rejected here for two reasons.
//!
//! It is order-dependent. "Remove 10s to 20s, then remove 15s to 25s" means
//! something different depending on whether the second range is measured
//! against the original recording or against the result of the first removal,
//! and every reader of the document has to make the same choice as every writer
//! or the export does not match the preview.
//!
//! And it assumes one source. [Issue
//! #88](https://github.com/wildware-uk/clipped/issues/88) joins material from
//! several recordings, and there is no single timeline for a removal to be
//! measured against.
//!
//! So a cut is stored as its *result*: the segments either side of it, in the
//! order they play. Deleting a section is expressible ([issue
//! #84](https://github.com/wildware-uk/clipped/issues/84) turns one segment
//! into two), and so is joining unrelated recordings, and reading the document
//! is arithmetic rather than replay.

use serde::{Deserialize, Serialize};

use crate::framing::{CropRect, Rotation};
use crate::source::SourceId;
use crate::time::{SourceSpan, Speed};

/// One run of material from one source, and how it is presented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    /// Which of the document's sources the material comes from.
    pub source: SourceId,
    /// Which part of that recording, on the recording's own timeline.
    pub span: SourceSpan,
    /// How fast it plays. Defaults to [`Speed::NORMAL`].
    #[serde(default)]
    pub speed: Speed,
    /// The part of the frame to keep, or the whole frame when absent.
    #[serde(default)]
    pub crop: Option<CropRect>,
    /// How far the picture is turned, applied after the crop.
    #[serde(default)]
    pub rotation: Rotation,
}

impl Segment {
    /// A segment playing `span` of `source` unchanged.
    #[must_use]
    pub fn new(source: SourceId, span: SourceSpan) -> Self {
        Self {
            source,
            span,
            speed: Speed::NORMAL,
            crop: None,
            rotation: Rotation::None,
        }
    }

    /// The same segment at `speed`.
    #[must_use]
    pub fn at_speed(mut self, speed: Speed) -> Self {
        self.speed = speed;
        self
    }

    /// The same segment cropped to `crop`.
    #[must_use]
    pub fn cropped_to(mut self, crop: CropRect) -> Self {
        self.crop = Some(crop);
        self
    }

    /// The same segment turned by `rotation`.
    #[must_use]
    pub fn rotated(mut self, rotation: Rotation) -> Self {
        self.rotation = rotation;
        self
    }

    /// How much of the edited timeline this segment occupies.
    ///
    /// `None` when the segment could not be read: an empty or backwards span,
    /// a speed with a zero in it, or a length that does not fit in `u64`
    /// nanoseconds. Every one of those is refused by
    /// [`EditDocument::validate`](crate::EditDocument::validate), so a document
    /// that was read or written by this crate always answers.
    #[must_use]
    pub fn output_nanos(&self) -> Option<u64> {
        if !self.span.is_valid() {
            return None;
        }
        self.speed.output_nanos(self.span.duration_nanos())
    }

    /// Whether an exporter could copy this segment's video without re-encoding.
    ///
    /// Only about *this* segment's own transformations. Whether a whole export
    /// can be a stream copy depends on the sources agreeing with each other and
    /// with the output settings, which is [issue
    /// #89](https://github.com/wildware-uk/clipped/issues/89)'s question and
    /// needs the files themselves to answer.
    #[must_use]
    pub fn is_untransformed(&self) -> bool {
        self.speed.is_normal()
            && self.rotation == Rotation::None
            && self.crop.is_none_or(|crop| crop == CropRect::FULL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::SourceTime;

    fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
        SourceSpan::new(
            SourceTime::from_nanos(start_nanos),
            SourceTime::from_nanos(end_nanos),
        )
        .expect("the test span ends after it starts")
    }

    #[test]
    fn a_plain_segment_lasts_exactly_as_long_as_the_material_it_names() {
        let segment = Segment::new(SourceId::new(0), span(1_000, 5_000));
        assert_eq!(segment.output_nanos(), Some(4_000));
        assert!(segment.is_untransformed());
    }

    #[test]
    fn speed_changes_how_long_a_segment_lasts_in_the_output() {
        let segment = Segment::new(SourceId::new(0), span(0, 8_000))
            .at_speed(Speed::new(4, 1).expect("a valid speed"));

        assert_eq!(segment.output_nanos(), Some(2_000));
        assert!(
            !segment.is_untransformed(),
            "a speed change is a re-encode, and an exporter has to know"
        );
    }

    #[test]
    fn a_full_frame_crop_is_not_a_transformation() {
        let segment = Segment::new(SourceId::new(0), span(0, 1_000)).cropped_to(CropRect::FULL);
        assert!(segment.is_untransformed());

        let cropped = Segment::new(SourceId::new(0), span(0, 1_000))
            .cropped_to(CropRect::new(0.1, 0.1, 0.5, 0.5).expect("a valid crop"));
        assert!(!cropped.is_untransformed());
    }

    #[test]
    fn rotation_counts_as_a_transformation() {
        let segment = Segment::new(SourceId::new(0), span(0, 1_000)).rotated(Rotation::Clockwise90);
        assert!(!segment.is_untransformed());
    }

    #[test]
    fn a_segment_with_an_unusable_speed_has_no_length() {
        let mut segment = Segment::new(SourceId::new(0), span(0, 1_000));
        // Only reachable through a document that was never validated, which is
        // exactly the case this has to answer rather than divide by zero in.
        segment.speed = serde_json::from_str(r#"{"numerator":0,"denominator":1}"#)
            .expect("the shape is right even though the value is not");

        assert_eq!(segment.output_nanos(), None);
    }

    #[test]
    fn the_optional_parts_of_a_segment_may_be_left_out_of_the_document() {
        let segment: Segment =
            serde_json::from_str(r#"{"source":2,"span":{"start":100,"end":200}}"#)
                .expect("speed, crop and rotation all have defaults");

        assert_eq!(segment.source, SourceId::new(2));
        assert_eq!(segment.speed, Speed::NORMAL);
        assert_eq!(segment.crop, None);
        assert_eq!(segment.rotation, Rotation::None);
    }
}
