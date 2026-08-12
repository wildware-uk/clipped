//! Turning a position on the edited timeline into a position in a recording.
//!
//! This is the arithmetic the whole document exists to make possible, and the
//! one piece of it that must give the same answer in the editor's preview and
//! in the exporter, on every machine, for ever. It is therefore integer
//! arithmetic over the segment list and nothing else: no floats, no
//! accumulated offsets, no dependence on the order edits were applied in.
//!
//! ```text
//!   output   0s        8s              20s          24s
//!            ├─ seg 0 ─┼───── seg 1 ────┼── seg 2 ───┤
//!               A 30s─38s   A 92s─104s     B 5s─9s
//! ```
//!
//! Segments are laid end to end in the order they appear in the document.
//! There are no gaps: a gap is either black frames nobody asked for, or a
//! second way of expressing the same edit, and neither is worth the ambiguity.

use crate::segment::Segment;
use crate::source::SourceId;
use crate::time::{OutputTime, SourceTime};

/// Where a position on the edited timeline comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Which segment of the document covers it.
    pub segment: usize,
    /// Which source that segment draws from.
    pub source: SourceId,
    /// Where in that recording the material is.
    pub source_time: SourceTime,
    /// Where the segment starts on the edited timeline, so that a caller
    /// stepping through a segment does not have to add the lengths up again.
    pub segment_start: OutputTime,
}

/// How long `segments` last in total, or `None` if one of them cannot be read.
///
/// `None` rather than a guess: a segment with a zero denominator or a
/// backwards span has no length, and an exporter that silently treated it as
/// zero would write a file quietly missing a piece of the clip. Validation
/// refuses those documents, so this answers for anything read or written by
/// this crate.
pub(crate) fn total_output_nanos(segments: &[Segment]) -> Option<u64> {
    let mut total = 0_u64;
    for segment in segments {
        total = total.checked_add(segment.output_nanos()?)?;
    }
    Some(total)
}

/// Finds the material at `at`, or `None` if the clip has already ended.
///
/// Linear over the segments, and deliberately so: a clip is a handful of
/// segments, and an index keyed on cumulative offsets would be a second
/// representation of the timeline that could disagree with the first. [Issue
/// #89](https://github.com/wildware-uk/clipped/issues/89) may cache the offsets
/// while it walks a whole export, which is a different thing from the document
/// storing them.
pub(crate) fn locate(segments: &[Segment], at: OutputTime) -> Option<Placement> {
    let mut segment_start = 0_u64;

    for (index, segment) in segments.iter().enumerate() {
        let length = segment.output_nanos()?;
        let end = segment_start.checked_add(length)?;
        if at.as_nanos() < end {
            let into_segment = at.as_nanos().checked_sub(segment_start)?;
            let into_source = segment.speed.source_nanos(into_segment)?;
            let source_time = segment.span.start().checked_add_nanos(into_source)?;
            return Some(Placement {
                segment: index,
                source: segment.source,
                source_time,
                segment_start: OutputTime::from_nanos(segment_start),
            });
        }
        segment_start = end;
    }

    None
}

/// Where `segment` starts on the edited timeline.
pub(crate) fn segment_start_nanos(segments: &[Segment], segment: usize) -> Option<u64> {
    if segment >= segments.len() {
        return None;
    }
    total_output_nanos(&segments[..segment])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{SourceSpan, Speed};

    fn segment(source: u32, start_nanos: u64, end_nanos: u64) -> Segment {
        Segment::new(
            SourceId::new(source),
            SourceSpan::new(
                SourceTime::from_nanos(start_nanos),
                SourceTime::from_nanos(end_nanos),
            )
            .expect("the test span ends after it starts"),
        )
    }

    /// Eight seconds of source 0, then twelve more from further into it, then
    /// four seconds of source 1: the picture in this module's documentation.
    fn three_segments() -> Vec<Segment> {
        vec![
            segment(0, 30_000_000_000, 38_000_000_000),
            segment(0, 92_000_000_000, 104_000_000_000),
            segment(1, 5_000_000_000, 9_000_000_000),
        ]
    }

    #[test]
    fn the_clip_is_as_long_as_its_segments_added_up() {
        assert_eq!(
            total_output_nanos(&three_segments()),
            Some(24_000_000_000),
            "8s + 12s + 4s"
        );
        assert_eq!(total_output_nanos(&[]), Some(0));
    }

    #[test]
    fn a_position_inside_the_first_segment_maps_straight_into_the_recording() {
        let placement = locate(&three_segments(), OutputTime::from_nanos(2_000_000_000))
            .expect("two seconds in is inside the clip");

        assert_eq!(placement.segment, 0);
        assert_eq!(placement.source, SourceId::new(0));
        assert_eq!(
            placement.source_time,
            SourceTime::from_nanos(32_000_000_000)
        );
        assert_eq!(placement.segment_start, OutputTime::ZERO);
    }

    #[test]
    fn the_material_a_cut_removed_is_not_reachable_from_the_output() {
        // The cut is at eight seconds: everything from 38s to 92s of the
        // recording is gone from the clip, and the frame after the cut is the
        // one at 92s. This is the assertion that a cut is a cut.
        let segments = three_segments();
        let before =
            locate(&segments, OutputTime::from_nanos(7_999_999_999)).expect("just before the cut");
        let after =
            locate(&segments, OutputTime::from_nanos(8_000_000_000)).expect("just after the cut");

        assert_eq!(before.segment, 0);
        assert_eq!(
            before.source_time,
            SourceTime::from_nanos(37_999_999_999),
            "the last nanosecond of the first segment"
        );
        assert_eq!(after.segment, 1);
        assert_eq!(
            after.source_time,
            SourceTime::from_nanos(92_000_000_000),
            "and then the material jumps"
        );
    }

    #[test]
    fn a_position_in_a_later_segment_finds_the_other_recording() {
        let placement = locate(&three_segments(), OutputTime::from_nanos(21_000_000_000))
            .expect("twenty-one seconds in is inside the clip");

        assert_eq!(placement.segment, 2);
        assert_eq!(placement.source, SourceId::new(1));
        assert_eq!(placement.source_time, SourceTime::from_nanos(6_000_000_000));
        assert_eq!(
            placement.segment_start,
            OutputTime::from_nanos(20_000_000_000)
        );
    }

    #[test]
    fn the_end_of_the_clip_is_past_the_end() {
        let segments = three_segments();
        assert!(locate(&segments, OutputTime::from_nanos(23_999_999_999)).is_some());
        assert!(
            locate(&segments, OutputTime::from_nanos(24_000_000_000)).is_none(),
            "the timeline is half-open at its end, like every span in it"
        );
        assert!(locate(&[], OutputTime::ZERO).is_none());
    }

    #[test]
    fn speed_stretches_output_time_without_moving_the_material() {
        // Four seconds of material at half speed is eight seconds of output,
        // and half way through the output is a quarter of the way through the
        // material.
        let segments = vec![segment(0, 10_000_000_000, 14_000_000_000)
            .at_speed(Speed::new(1, 2).expect("a valid speed"))];

        assert_eq!(total_output_nanos(&segments), Some(8_000_000_000));

        let placement = locate(&segments, OutputTime::from_nanos(4_000_000_000))
            .expect("four seconds in is inside the clip");
        assert_eq!(
            placement.source_time,
            SourceTime::from_nanos(12_000_000_000)
        );

        assert!(locate(&segments, OutputTime::from_nanos(8_000_000_000)).is_none());
    }

    #[test]
    fn a_sped_up_segment_shortens_the_clip_and_runs_through_the_material_faster() {
        let segments =
            vec![segment(0, 0, 12_000_000_000).at_speed(Speed::new(3, 1).expect("a valid speed"))];

        assert_eq!(total_output_nanos(&segments), Some(4_000_000_000));

        let placement = locate(&segments, OutputTime::from_nanos(1_000_000_000))
            .expect("one second in is inside the clip");
        assert_eq!(placement.source_time, SourceTime::from_nanos(3_000_000_000));
    }

    #[test]
    fn an_unreadable_segment_makes_the_timeline_unreadable_rather_than_wrong() {
        let mut segments = three_segments();
        segments[1].speed = serde_json::from_str(r#"{"numerator":1,"denominator":0}"#)
            .expect("the shape is right even though the value is not");

        assert_eq!(total_output_nanos(&segments), None);
        assert_eq!(
            locate(&segments, OutputTime::from_nanos(10_000_000_000)),
            None
        );
        assert!(
            locate(&segments, OutputTime::ZERO).is_some(),
            "the segments before the broken one are still readable"
        );
    }

    #[test]
    fn a_segment_reports_where_it_starts_on_the_timeline() {
        let segments = three_segments();

        assert_eq!(segment_start_nanos(&segments, 0), Some(0));
        assert_eq!(segment_start_nanos(&segments, 1), Some(8_000_000_000));
        assert_eq!(segment_start_nanos(&segments, 2), Some(20_000_000_000));
        assert_eq!(segment_start_nanos(&segments, 3), None);
    }
}
