//! The edits themselves: trimming the ends, splitting at the playhead and
//! deleting a section.
//!
//! Every one of them is about the distinction [`crate::time`] draws. A user
//! points at a moment on the **edited timeline** — the playhead, a selection
//! they dragged — and what has to change is which **source** material the
//! document names and where that material lands in output time afterwards.
//! Deleting eight seconds out of the middle does not move a single frame in the
//! recording; it moves every frame after the cut eight seconds earlier in the
//! export. Getting that backwards produces an export that plays the wrong
//! material, and it looks like a bug in [issue
//! #89](https://github.com/wildware-uk/clipped/issues/89) rather than a bug
//! here.
//!
//! # One primitive
//!
//! All four operations are the same piece of arithmetic: put a boundary at an
//! output time, then keep some of what is either side of it.
//!
//! ```text
//!   before   ├──── segment 0 ────┼──── segment 1 ────┤
//!                        ▲ at
//!   divide   ├── 0a ─────┼─ 0b ──┼──── segment 1 ────┤
//!
//!   split         keep everything          →  0a 0b 1
//!   trim start    keep from the boundary   →  0b 1
//!   trim end      keep up to the boundary  →  0a
//!   delete        divide twice, drop the middle
//! ```
//!
//! [`divide`] is therefore the only place that converts an output time into a
//! source time for an edit, which is what makes "split at ten seconds then
//! delete the first ten" and "delete the first ten seconds" produce the same
//! document. It is also why a boundary that already exists costs nothing: the
//! operation finds it rather than inserting a second one, and the document
//! comes back unchanged.
//!
//! # What moves and what does not
//!
//! | | Source time | Output time |
//! | --- | --- | --- |
//! | Split | unchanged | unchanged |
//! | Trim start | unchanged | everything kept moves earlier by the trim |
//! | Trim end | unchanged | unchanged |
//! | Delete section | unchanged | everything after the cut moves earlier by its length |
//!
//! Overlays are timed in output time, so they are carried through the same
//! mapping the material is; the rules are on [`remapped`]. Audio fades are
//! measured from the ends of the clip, so a clip that got shorter can leave
//! fades that no longer fit, and [`clamp_fades`] shortens them rather than
//! letting the operation fail.
//!
//! # Refusals
//!
//! An operation is a `Result`, and a refused one changes nothing: [`apply`]
//! takes `&self` and builds a new document, so there is no half-applied state
//! to recover from. What is refused is listed on
//! [`OperationRefused`](crate::OperationRefused), and the last check is that
//! the result validates — so no operation can produce a document that could not
//! be saved.
//!
//! [`apply`]: EditDocument::apply

use core::time::Duration;

use crate::audio::AudioTrack;
use crate::document::EditDocument;
use crate::error::{DocumentProblem, OperationRefused};
use crate::overlay::TextOverlay;
use crate::segment::Segment;
use crate::time::{OutputSpan, OutputTime, SourceSpan};

/// A change to the material of a clip.
///
/// Positions are on the **edited timeline**, because that is what the user is
/// looking at: the playhead sits at a moment of the clip, not at a moment of
/// one of the recordings behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditOperation {
    /// Throw away everything before `at`, and move what is left to the start.
    TrimStart {
        /// The new beginning of the clip, on the edited timeline.
        at: OutputTime,
    },
    /// Throw away everything from `at` onwards.
    TrimEnd {
        /// The new end of the clip, on the edited timeline.
        at: OutputTime,
    },
    /// Turn the segment covering `at` into two, keeping all the material.
    ///
    /// A split changes nothing about what plays or when. It is what every
    /// later per-segment edit — a speed change, a crop, a rotation — needs in
    /// order to apply to part of a clip rather than all of it.
    Split {
        /// Where the new boundary goes, on the edited timeline.
        at: OutputTime,
    },
    /// Throw away `range` and join what is either side of it.
    DeleteSection {
        /// The part of the edited timeline to remove.
        range: OutputSpan,
    },
}

impl EditDocument {
    /// The document with `operation` applied, or why it was refused.
    ///
    /// The original is untouched: a refusal cannot leave a half-edited
    /// document, and the caller decides what to do with the result. [Undo and
    /// redo](crate::EditHistory) are built on exactly this.
    ///
    /// The document is validated before the operation and again after it, so a
    /// document that could not have been saved cannot come back out of one.
    ///
    /// # Errors
    ///
    /// [`OperationRefused`], which names the time or the range at fault.
    pub fn apply(&self, operation: EditOperation) -> Result<Self, OperationRefused> {
        self.validate().map_err(OperationRefused::Unreadable)?;
        let clip_nanos = self
            .output_duration_nanos()
            .ok_or(OperationRefused::Unreadable(
                DocumentProblem::TimelineTooLong,
            ))?;

        let edited = match operation {
            EditOperation::TrimStart { at } => trim_start(self, at, clip_nanos),
            EditOperation::TrimEnd { at } => trim_end(self, at, clip_nanos),
            EditOperation::Split { at } => split(self, at, clip_nanos),
            EditOperation::DeleteSection { range } => delete_section(self, range, clip_nanos),
        }?;

        edited
            .validate()
            .map_err(OperationRefused::WouldBreakTheDocument)?;
        Ok(edited)
    }
}

/// Keeps everything from `at` onwards, and moves it to the start of the clip.
fn trim_start(
    document: &EditDocument,
    at: OutputTime,
    clip_nanos: u64,
) -> Result<EditDocument, OperationRefused> {
    let at_nanos = at.as_nanos();
    if at_nanos > clip_nanos {
        return Err(OperationRefused::OutsideTheClip {
            at_nanos,
            clip_nanos,
        });
    }
    // Trimming defines the range that is *kept*, and a kept range that ends
    // where it starts is not a range at all (`OutputSpan` refuses one for the
    // same reason). Emptying a clip deliberately is `DeleteSection`.
    if at_nanos == clip_nanos {
        return Err(OperationRefused::NothingWouldRemain);
    }

    let (divided, boundary) = divide(&document.segments, at_nanos)?;
    let mut edited = document.clone();
    edited.segments = divided[boundary..].to_vec();
    // What survived started at `at` and now starts at zero.
    settle(&mut edited, |nanos| nanos.saturating_sub(at_nanos));
    Ok(edited)
}

/// Keeps everything before `at`.
fn trim_end(
    document: &EditDocument,
    at: OutputTime,
    clip_nanos: u64,
) -> Result<EditDocument, OperationRefused> {
    let at_nanos = at.as_nanos();
    if at_nanos > clip_nanos {
        return Err(OperationRefused::OutsideTheClip {
            at_nanos,
            clip_nanos,
        });
    }
    if at_nanos == 0 {
        return Err(OperationRefused::NothingWouldRemain);
    }

    let (divided, boundary) = divide(&document.segments, at_nanos)?;
    let mut edited = document.clone();
    edited.segments = divided[..boundary].to_vec();
    // Nothing moved: the clip only got shorter at its end, so an overlay is
    // where it was unless it ran past the new end.
    settle(&mut edited, |nanos| nanos.min(at_nanos));
    Ok(edited)
}

/// Puts a boundary at `at` without changing anything that plays.
fn split(
    document: &EditDocument,
    at: OutputTime,
    clip_nanos: u64,
) -> Result<EditDocument, OperationRefused> {
    let at_nanos = at.as_nanos();
    // The end of the clip is not a position a segment covers — the timeline is
    // half-open — so there is nothing there to split.
    if at_nanos >= clip_nanos {
        return Err(OperationRefused::OutsideTheClip {
            at_nanos,
            clip_nanos,
        });
    }

    let (divided, _) = divide(&document.segments, at_nanos)?;
    let mut edited = document.clone();
    edited.segments = divided;
    // No `settle`: output time is untouched, so every overlay and every fade
    // is still exactly where the user put it.
    Ok(edited)
}

/// Removes `range` from the middle and joins what is either side of it.
fn delete_section(
    document: &EditDocument,
    range: OutputSpan,
    clip_nanos: u64,
) -> Result<EditDocument, OperationRefused> {
    let start_nanos = range.start().as_nanos();
    let end_nanos = range.end().as_nanos();
    if end_nanos > clip_nanos {
        return Err(OperationRefused::OutsideTheClip {
            at_nanos: end_nanos,
            clip_nanos,
        });
    }

    let (divided, first) = divide(&document.segments, start_nanos)?;
    // Dividing again at a later time can only insert a boundary at or after
    // the first one, so `first` still indexes the segment that starts at
    // `start_nanos`.
    let (divided, last) = divide(&divided, end_nanos)?;

    let mut segments = divided[..first].to_vec();
    segments.extend_from_slice(&divided[last..]);

    let mut edited = document.clone();
    edited.segments = segments;
    let removed_nanos = end_nanos - start_nanos;
    settle(&mut edited, |nanos| {
        if nanos <= start_nanos {
            nanos
        } else if nanos < end_nanos {
            // A moment inside the cut collapses onto the join.
            start_nanos
        } else {
            nanos - removed_nanos
        }
    });
    Ok(edited)
}

/// The segment list with a boundary at `at_nanos`, and where that boundary is.
///
/// The returned index is the position of the first segment at or after
/// `at_nanos`: `0` for the start of the clip, `segments.len()` for its end, and
/// otherwise the piece that begins there. A boundary that already exists is
/// found rather than inserted, so dividing twice at the same time produces the
/// same list as dividing once — which is what makes the operations above
/// order-independent where they should be.
///
/// The conversion from output time to source time is
/// [`Speed::source_nanos`](crate::Speed::source_nanos), the same call
/// [`EditDocument::locate`] makes, so a boundary lands exactly where the frame
/// the user is looking at comes from. Its truncation can put the cut on one of
/// the segment's own ends, and then there is nothing to divide: the boundary is
/// the one already there.
///
/// `Err` only for a document whose segments cannot be measured, which
/// validation has already refused by the time an operation calls this.
fn divide(segments: &[Segment], at_nanos: u64) -> Result<(Vec<Segment>, usize), OperationRefused> {
    let mut divided: Vec<Segment> = Vec::with_capacity(segments.len() + 1);
    let mut boundary: Option<usize> = None;
    let mut start_nanos: u64 = 0;

    for segment in segments {
        let end_nanos = segment
            .output_nanos()
            .and_then(|length| start_nanos.checked_add(length))
            .ok_or(OperationRefused::Unreadable(
                DocumentProblem::TimelineTooLong,
            ))?;

        if boundary.is_none() && at_nanos <= start_nanos {
            boundary = Some(divided.len());
        } else if boundary.is_none() && at_nanos < end_nanos {
            let cut = segment
                .speed
                .source_nanos(at_nanos - start_nanos)
                .and_then(|into_source| segment.span.start().checked_add_nanos(into_source))
                .ok_or(OperationRefused::Unreadable(
                    DocumentProblem::TimelineTooLong,
                ))?;

            match (
                SourceSpan::new(segment.span.start(), cut),
                SourceSpan::new(cut, segment.span.end()),
            ) {
                (Some(before), Some(after)) => {
                    let mut left = segment.clone();
                    left.span = before;
                    divided.push(left);
                    boundary = Some(divided.len());
                    let mut right = segment.clone();
                    right.span = after;
                    divided.push(right);
                    start_nanos = end_nanos;
                    continue;
                }
                // The cut rounded onto this segment's own start, so the
                // boundary is the one in front of it.
                (None, _) => boundary = Some(divided.len()),
                // And onto its end, so the boundary is the one behind it.
                (_, None) => boundary = Some(divided.len() + 1),
            }
        }

        divided.push(segment.clone());
        start_nanos = end_nanos;
    }

    if boundary.is_none() && at_nanos <= start_nanos {
        boundary = Some(divided.len());
    }
    let boundary = boundary.ok_or(OperationRefused::OutsideTheClip {
        at_nanos,
        clip_nanos: start_nanos,
    })?;
    Ok((divided, boundary))
}

/// Carries everything timed in output time through the operation's mapping.
///
/// `map` says where a moment of the old timeline is on the new one. Both ends
/// of every overlay go through it, and a fade that no longer fits the clip is
/// shortened. Segments are already the operation's own business by the time
/// this runs.
fn settle(edited: &mut EditDocument, map: impl Fn(u64) -> u64) {
    edited.overlays = edited
        .overlays
        .iter()
        .filter_map(|overlay| remapped(overlay, &map))
        .collect();

    // An unmeasurable timeline leaves the fades alone: the operation's final
    // validation is what reports it, rather than this quietly clamping every
    // fade to nothing on the way past.
    if let Some(clip_nanos) = edited.output_duration_nanos() {
        clamp_fades(&mut edited.audio_tracks, clip_nanos);
    }
}

/// An overlay moved onto the new timeline, or `None` if nothing of it is left.
///
/// Both ends go through the same mapping the material did, so text stays over
/// the moment it was put on:
///
/// - An overlay wholly inside a deleted section, or wholly outside a trim, maps
///   to an empty range and goes with the material it was over.
/// - One that straddles a cut keeps the part that survived. An overlay across a
///   deleted section is shortened by the length of the section, which is the
///   only answer that leaves it over the same frames either side of the join.
fn remapped(overlay: &TextOverlay, map: &impl Fn(u64) -> u64) -> Option<TextOverlay> {
    let when = OutputSpan::new(
        OutputTime::from_nanos(map(overlay.when.start().as_nanos())),
        OutputTime::from_nanos(map(overlay.when.end().as_nanos())),
    )?;
    let mut moved = overlay.clone();
    moved.when = when;
    Some(moved)
}

/// Shortens fades that no longer fit the clip they are on.
///
/// A fade is a length at an end of the clip, so trimming or deleting can leave
/// a pair that adds up to more than what is left — which validation refuses.
/// Refusing the whole operation for it would mean a user could not trim a clip
/// they had already faded, so the fades give way instead. The fade *in* is kept
/// in preference to the fade out because it is the one the viewer hears first,
/// and because a clip cut short at its end is exactly the case where the fade
/// out was going to be re-dragged anyway.
fn clamp_fades(tracks: &mut [AudioTrack], clip_nanos: u64) {
    let clip = Duration::from_nanos(clip_nanos);
    for track in tracks {
        if track.fade_in > clip {
            track.fade_in = clip;
        }
        let remaining = clip - track.fade_in;
        if track.fade_out > remaining {
            track.fade_out = remaining;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::TrackInput;
    use crate::overlay::TextOverlay;
    use crate::source::{RecordingId, Source, SourceId};
    use crate::time::{SourceTime, Speed};

    fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
        SourceSpan::new(
            SourceTime::from_nanos(start_nanos),
            SourceTime::from_nanos(end_nanos),
        )
        .expect("the test span ends after it starts")
    }

    fn when(start_nanos: u64, end_nanos: u64) -> OutputSpan {
        OutputSpan::new(
            OutputTime::from_nanos(start_nanos),
            OutputTime::from_nanos(end_nanos),
        )
        .expect("the test span ends after it starts")
    }

    const SECOND: u64 = 1_000_000_000;

    /// Three ten-second segments of one recording, laid end to end.
    ///
    /// The material is deliberately not contiguous — 0s–10s, 100s–110s and
    /// 200s–210s of the same recording — so that a source time identifies which
    /// segment it came from on sight, and an operation that kept the wrong
    /// piece cannot be mistaken for one that kept the right one.
    fn three_segments() -> EditDocument {
        let source = SourceId::new(0);
        EditDocument::new("Ace")
            .with_source(Source::new(source, RecordingId::new("rec-1")))
            .with_segment(Segment::new(source, span(0, 10 * SECOND)))
            .with_segment(Segment::new(source, span(100 * SECOND, 110 * SECOND)))
            .with_segment(Segment::new(source, span(200 * SECOND, 210 * SECOND)))
    }

    /// Where every whole second of the clip comes from, as an export would read
    /// it: `(which recording, source time in seconds)`.
    fn transcript(document: &EditDocument) -> Vec<(u32, u64)> {
        let duration = document
            .output_duration_nanos()
            .expect("a valid document has a duration");
        (0..duration / SECOND)
            .map(|second| {
                let placement = document
                    .locate(OutputTime::from_nanos(second * SECOND))
                    .expect("a position inside the clip is placed");
                (
                    placement.source.get(),
                    placement.source_time.as_nanos() / SECOND,
                )
            })
            .collect()
    }

    #[test]
    fn trimming_the_start_drops_the_material_before_it_and_moves_the_rest_earlier() {
        let document = three_segments();
        let trimmed = document
            .apply(EditOperation::TrimStart {
                at: OutputTime::from_nanos(4 * SECOND),
            })
            .expect("four seconds in is inside the clip");

        assert_eq!(trimmed.output_duration_nanos(), Some(26 * SECOND));
        // The first segment now starts four seconds into the recording, and the
        // material that used to be at four seconds is at zero.
        assert_eq!(trimmed.segments[0].span, span(4 * SECOND, 10 * SECOND));
        assert_eq!(trimmed.segments.len(), 3, "only the first was shortened");
        assert_eq!(
            transcript(&trimmed),
            transcript(&document)[4..],
            "the clip is the tail of the one it was cut from, frame for frame"
        );
    }

    #[test]
    fn trimming_the_end_keeps_the_material_before_it_where_it_was() {
        let document = three_segments();
        let trimmed = document
            .apply(EditOperation::TrimEnd {
                at: OutputTime::from_nanos(23 * SECOND),
            })
            .expect("twenty-three seconds in is inside the clip");

        assert_eq!(trimmed.output_duration_nanos(), Some(23 * SECOND));
        assert_eq!(trimmed.segments.len(), 3);
        assert_eq!(
            trimmed.segments[2].span,
            span(200 * SECOND, 203 * SECOND),
            "the last segment lost its tail and kept its head"
        );
        assert_eq!(transcript(&trimmed), transcript(&document)[..23]);
    }

    #[test]
    fn trimming_an_end_to_a_segment_boundary_drops_whole_segments() {
        let document = three_segments();

        let start = document
            .apply(EditOperation::TrimStart {
                at: OutputTime::from_nanos(10 * SECOND),
            })
            .expect("the boundary between the first two segments");
        assert_eq!(start.segments.len(), 2);
        assert_eq!(start.segments[0].span, span(100 * SECOND, 110 * SECOND));

        let end = document
            .apply(EditOperation::TrimEnd {
                at: OutputTime::from_nanos(20 * SECOND),
            })
            .expect("the boundary between the last two segments");
        assert_eq!(end.segments.len(), 2);
        assert_eq!(end.segments[1].span, span(100 * SECOND, 110 * SECOND));
    }

    #[test]
    fn splitting_makes_two_segments_that_play_exactly_what_the_one_did() {
        let document = three_segments();
        let split = document
            .apply(EditOperation::Split {
                at: OutputTime::from_nanos(13 * SECOND),
            })
            .expect("thirteen seconds in is inside the clip");

        assert_eq!(split.segments.len(), 4);
        assert_eq!(split.segments[1].span, span(100 * SECOND, 103 * SECOND));
        assert_eq!(split.segments[2].span, span(103 * SECOND, 110 * SECOND));
        assert_eq!(
            split.output_duration_nanos(),
            document.output_duration_nanos()
        );
        assert_eq!(
            transcript(&split),
            transcript(&document),
            "a split changes where the boundaries are, not what plays"
        );
    }

    #[test]
    fn splitting_exactly_on_a_boundary_finds_it_rather_than_adding_a_second_one() {
        let document = three_segments();

        for at_nanos in [0, 10 * SECOND, 20 * SECOND] {
            let split = document
                .apply(EditOperation::Split {
                    at: OutputTime::from_nanos(at_nanos),
                })
                .expect("a boundary is inside the clip");
            assert_eq!(
                split, document,
                "the boundary at {at_nanos} ns already exists, so the document is unchanged"
            );
        }
    }

    #[test]
    fn splitting_at_the_end_of_the_clip_is_refused_rather_than_producing_an_empty_piece() {
        let document = three_segments();

        assert_eq!(
            document.apply(EditOperation::Split {
                at: OutputTime::from_nanos(30 * SECOND)
            }),
            Err(OperationRefused::OutsideTheClip {
                at_nanos: 30 * SECOND,
                clip_nanos: 30 * SECOND,
            })
        );
    }

    #[test]
    fn deleting_a_section_joins_what_is_left_and_moves_it_earlier() {
        let document = three_segments();
        let edited = document
            .apply(EditOperation::DeleteSection {
                range: when(4 * SECOND, 7 * SECOND),
            })
            .expect("the range is inside the clip");

        assert_eq!(edited.output_duration_nanos(), Some(27 * SECOND));
        assert_eq!(edited.segments.len(), 4);
        assert_eq!(edited.segments[0].span, span(0, 4 * SECOND));
        assert_eq!(
            edited.segments[1].span,
            span(7 * SECOND, 10 * SECOND),
            "the material after the cut is where it always was in the recording"
        );

        // The whole point, stated as the export would see it: what plays after
        // the join is what used to play three seconds later.
        let before = transcript(&document);
        let expected: Vec<(u32, u64)> = before[..4]
            .iter()
            .chain(before[7..].iter())
            .copied()
            .collect();
        assert_eq!(transcript(&edited), expected);
    }

    #[test]
    fn deleting_exactly_one_segment_removes_that_segment_and_nothing_else() {
        let document = three_segments();
        let edited = document
            .apply(EditOperation::DeleteSection {
                range: when(10 * SECOND, 20 * SECOND),
            })
            .expect("the middle segment");

        assert_eq!(edited.segments.len(), 2);
        assert_eq!(edited.segments[0].span, span(0, 10 * SECOND));
        assert_eq!(edited.segments[1].span, span(200 * SECOND, 210 * SECOND));
        assert_eq!(edited.output_duration_nanos(), Some(20 * SECOND));
    }

    #[test]
    fn deleting_across_two_segments_leaves_a_piece_of_each() {
        let document = three_segments();
        let edited = document
            .apply(EditOperation::DeleteSection {
                range: when(7 * SECOND, 24 * SECOND),
            })
            .expect("from inside the first segment to inside the third");

        assert_eq!(
            edited.segments.len(),
            2,
            "the head of the first and the tail of the third; the middle is gone"
        );
        assert_eq!(edited.segments[0].span, span(0, 7 * SECOND));
        assert_eq!(edited.segments[1].span, span(204 * SECOND, 210 * SECOND));
        assert_eq!(edited.output_duration_nanos(), Some(13 * SECOND));

        let after_the_join = edited
            .locate(OutputTime::from_nanos(7 * SECOND))
            .expect("the join is inside the shorter clip");
        let first_one_kept = document
            .locate(OutputTime::from_nanos(24 * SECOND))
            .expect("the end of the deleted range is inside the original");
        assert_eq!(
            (after_the_join.source, after_the_join.source_time),
            (first_one_kept.source, first_one_kept.source_time),
            "the frame after the join is the first one the deletion did not take"
        );
    }

    #[test]
    fn deleting_everything_leaves_an_empty_clip_rather_than_a_broken_one() {
        let document = three_segments();
        let edited = document
            .apply(EditOperation::DeleteSection {
                range: when(0, 30 * SECOND),
            })
            .expect("a user may delete all of it");

        assert!(edited.segments.is_empty());
        assert_eq!(edited.output_duration_nanos(), Some(0));
        assert_eq!(
            edited.sources, document.sources,
            "the recordings stay declared: undo has to be able to put the material back, \
             and an unused source harms nothing"
        );
        edited.validate().expect("an empty clip is valid");
    }

    #[test]
    fn a_trim_that_would_leave_nothing_is_refused() {
        let document = three_segments();

        assert_eq!(
            document.apply(EditOperation::TrimStart {
                at: OutputTime::from_nanos(30 * SECOND)
            }),
            Err(OperationRefused::NothingWouldRemain)
        );
        assert_eq!(
            document.apply(EditOperation::TrimEnd {
                at: OutputTime::ZERO
            }),
            Err(OperationRefused::NothingWouldRemain)
        );
    }

    #[test]
    fn a_time_past_the_end_of_the_clip_is_refused_by_every_operation() {
        let document = three_segments();
        let past_the_end = OutputTime::from_nanos(30 * SECOND + 1);
        let refused = Err(OperationRefused::OutsideTheClip {
            at_nanos: 30 * SECOND + 1,
            clip_nanos: 30 * SECOND,
        });

        assert_eq!(
            document.apply(EditOperation::TrimStart { at: past_the_end }),
            refused
        );
        assert_eq!(
            document.apply(EditOperation::TrimEnd { at: past_the_end }),
            refused
        );
        assert_eq!(
            document.apply(EditOperation::Split { at: past_the_end }),
            refused
        );
        assert_eq!(
            document.apply(EditOperation::DeleteSection {
                range: when(29 * SECOND, 30 * SECOND + 1)
            }),
            refused
        );
    }

    #[test]
    fn an_operation_on_a_document_that_cannot_be_read_is_refused_before_it_starts() {
        let mut document = three_segments();
        document.segments[1].speed = serde_json::from_str(r#"{"numerator":1,"denominator":0}"#)
            .expect("the shape is right even though the value is not");

        assert_eq!(
            document.apply(EditOperation::Split {
                at: OutputTime::from_nanos(SECOND)
            }),
            Err(OperationRefused::Unreadable(
                DocumentProblem::UnusableSpeed { segment: 1 }
            ))
        );
    }

    #[test]
    fn an_overlay_inside_a_deleted_section_goes_with_the_material_it_was_over() {
        let document = three_segments()
            .with_overlay(TextOverlay::new("before", when(0, 3 * SECOND)))
            .with_overlay(TextOverlay::new("inside", when(11 * SECOND, 13 * SECOND)))
            .with_overlay(TextOverlay::new("across", when(8 * SECOND, 22 * SECOND)))
            .with_overlay(TextOverlay::new("after", when(25 * SECOND, 28 * SECOND)));

        let edited = document
            .apply(EditOperation::DeleteSection {
                range: when(10 * SECOND, 20 * SECOND),
            })
            .expect("the middle segment");

        let surviving: Vec<(&str, u64, u64)> = edited
            .overlays
            .iter()
            .map(|overlay| {
                (
                    overlay.text.as_str(),
                    overlay.when.start().as_nanos() / SECOND,
                    overlay.when.end().as_nanos() / SECOND,
                )
            })
            .collect();

        assert_eq!(
            surviving,
            vec![
                ("before", 0, 3),
                // "inside" was only ever on screen over material that is gone.
                ("across", 8, 12),
                ("after", 15, 18),
            ]
        );
        edited
            .validate()
            .expect("no overlay is left running past the end of a shorter clip");
    }

    #[test]
    fn an_overlay_moves_with_the_clip_when_the_start_is_trimmed_and_is_clamped_at_the_end() {
        let document = three_segments()
            .with_overlay(TextOverlay::new("dropped", when(0, 2 * SECOND)))
            .with_overlay(TextOverlay::new("straddling", when(2 * SECOND, 8 * SECOND)));

        let trimmed = document
            .apply(EditOperation::TrimStart {
                at: OutputTime::from_nanos(5 * SECOND),
            })
            .expect("five seconds in is inside the clip");
        let overlays: Vec<(&str, u64, u64)> = trimmed
            .overlays
            .iter()
            .map(|overlay| {
                (
                    overlay.text.as_str(),
                    overlay.when.start().as_nanos(),
                    overlay.when.end().as_nanos(),
                )
            })
            .collect();
        assert_eq!(overlays, vec![("straddling", 0, 3 * SECOND)]);

        let cut_short = document
            .apply(EditOperation::TrimEnd {
                at: OutputTime::from_nanos(4 * SECOND),
            })
            .expect("four seconds in is inside the clip");
        assert_eq!(cut_short.overlays.len(), 2);
        assert_eq!(
            cut_short.overlays[1].when,
            when(2 * SECOND, 4 * SECOND),
            "an overlay running past the new end is clamped to it rather than refused"
        );
        cut_short.validate().expect("and the result is valid");
    }

    #[test]
    fn a_fade_longer_than_what_is_left_is_shortened_rather_than_refusing_the_trim() {
        let document = three_segments().with_audio_track(
            AudioTrack::new("Game", vec![TrackInput::new(SourceId::new(0), 0)])
                .with_fades(Duration::from_secs(6), Duration::from_secs(8)),
        );

        let trimmed = document
            .apply(EditOperation::TrimEnd {
                at: OutputTime::from_nanos(10 * SECOND),
            })
            .expect("a clip with fades can still be trimmed");

        assert_eq!(trimmed.audio_tracks[0].fade_in, Duration::from_secs(6));
        assert_eq!(
            trimmed.audio_tracks[0].fade_out,
            Duration::from_secs(4),
            "the fade in is kept and the fade out gives way"
        );
        trimmed.validate().expect("the fades now fit the clip");

        let emptied = document
            .apply(EditOperation::DeleteSection {
                range: when(0, 30 * SECOND),
            })
            .expect("deleting everything");
        assert_eq!(emptied.audio_tracks[0].fade_in, Duration::ZERO);
        assert_eq!(emptied.audio_tracks[0].fade_out, Duration::ZERO);
    }

    #[test]
    fn a_split_inside_a_sped_up_segment_lands_where_that_output_moment_comes_from() {
        // Twelve seconds of material at double speed is six seconds of output,
        // so two seconds into the output is four seconds into the material.
        let source = SourceId::new(0);
        let document = EditDocument::new("Ace")
            .with_source(Source::new(source, RecordingId::new("rec-1")))
            .with_segment(
                Segment::new(source, span(0, 12 * SECOND))
                    .at_speed(Speed::new(2, 1).expect("a valid speed")),
            );

        let split = document
            .apply(EditOperation::Split {
                at: OutputTime::from_nanos(2 * SECOND),
            })
            .expect("two seconds in is inside the clip");

        assert_eq!(split.segments[0].span, span(0, 4 * SECOND));
        assert_eq!(split.segments[1].span, span(4 * SECOND, 12 * SECOND));
        assert_eq!(
            split.segments[1].speed, document.segments[0].speed,
            "both halves keep the presentation the segment had"
        );
        assert_eq!(split.output_duration_nanos(), Some(6 * SECOND));
        assert_eq!(transcript(&split), transcript(&document));
    }

    #[test]
    fn a_cut_that_rounds_onto_a_segments_own_start_leaves_the_segment_whole() {
        // Ten nanoseconds of material stretched over ten milliseconds of
        // output. Anything in the first millionth of that maps to source offset
        // zero, so the cut lands on the segment's own start and there is
        // nothing to divide: the boundary is the one already in front of it.
        // Getting that index wrong is a trim that silently drops the segment,
        // and it is only reachable at speeds this extreme, which is exactly why
        // it is asserted rather than reasoned about.
        let source = SourceId::new(0);
        let document = EditDocument::new("Ace")
            .with_source(Source::new(source, RecordingId::new("rec-1")))
            .with_segment(
                Segment::new(source, span(0, 10))
                    .at_speed(Speed::new(1, 1_000_000).expect("a valid speed")),
            );
        assert_eq!(document.output_duration_nanos(), Some(10_000_000));

        let at = OutputTime::from_nanos(500_000);
        assert_eq!(
            document
                .apply(EditOperation::TrimStart { at })
                .expect("half a millisecond in is inside the clip"),
            document,
            "the material before the cut is under a nanosecond of source, so all of it stays"
        );
        assert_eq!(
            document
                .apply(EditOperation::Split { at })
                .expect("half a millisecond in is inside the clip"),
            document,
            "and there is no boundary to add there"
        );
    }

    #[test]
    fn a_split_keeps_the_crop_and_the_rotation_of_the_segment_it_divides() {
        use crate::framing::{CropRect, Rotation};

        let source = SourceId::new(0);
        let document = EditDocument::new("Ace")
            .with_source(Source::new(source, RecordingId::new("rec-1")))
            .with_segment(
                Segment::new(source, span(0, 10 * SECOND))
                    .cropped_to(CropRect::new(0.1, 0.2, 0.5, 0.6).expect("a valid crop"))
                    .rotated(Rotation::Clockwise90),
            );

        let split = document
            .apply(EditOperation::Split {
                at: OutputTime::from_nanos(4 * SECOND),
            })
            .expect("four seconds in is inside the clip");

        for piece in &split.segments {
            assert_eq!(piece.crop, document.segments[0].crop);
            assert_eq!(piece.rotation, document.segments[0].rotation);
        }
    }

    #[test]
    fn two_splits_give_the_same_document_in_either_order() {
        let document = three_segments();
        let first = OutputTime::from_nanos(4 * SECOND);
        let second = OutputTime::from_nanos(23 * SECOND);

        let one_way = document
            .apply(EditOperation::Split { at: first })
            .and_then(|split| split.apply(EditOperation::Split { at: second }))
            .expect("both times are inside the clip");
        let other_way = document
            .apply(EditOperation::Split { at: second })
            .and_then(|split| split.apply(EditOperation::Split { at: first }))
            .expect("both times are inside the clip");

        assert_eq!(one_way, other_way);
        assert_eq!(one_way.segments.len(), 5);
    }

    #[test]
    fn splitting_before_a_deletion_and_after_it_agree_once_the_time_is_mapped() {
        // The same two decisions in either order: divide at four seconds, and
        // delete ten to twenty. Done the other way round the split is at four
        // seconds either way, because four seconds is before the cut and a
        // deletion only moves what is after it.
        let document = three_segments();
        let removal = EditOperation::DeleteSection {
            range: when(10 * SECOND, 20 * SECOND),
        };

        let split_first = document
            .apply(EditOperation::Split {
                at: OutputTime::from_nanos(4 * SECOND),
            })
            .and_then(|split| split.apply(removal))
            .expect("both operations are inside the clip");
        let delete_first = document
            .apply(removal)
            .and_then(|edited| {
                edited.apply(EditOperation::Split {
                    at: OutputTime::from_nanos(4 * SECOND),
                })
            })
            .expect("both operations are inside the clip");

        assert_eq!(split_first, delete_first);
    }

    #[test]
    fn deleting_two_sections_gives_the_same_document_in_either_order() {
        // The later range measured on the timeline it is dragged on: doing the
        // earlier deletion first moves the later one earlier by its length.
        let document = three_segments();
        let earlier = when(2 * SECOND, 5 * SECOND);
        let later = when(21 * SECOND, 26 * SECOND);
        let later_after_the_earlier_one = when(18 * SECOND, 23 * SECOND);

        let earlier_first = document
            .apply(EditOperation::DeleteSection { range: earlier })
            .and_then(|edited| {
                edited.apply(EditOperation::DeleteSection {
                    range: later_after_the_earlier_one,
                })
            })
            .expect("both ranges are inside the clip");
        let later_first = document
            .apply(EditOperation::DeleteSection { range: later })
            .and_then(|edited| edited.apply(EditOperation::DeleteSection { range: earlier }))
            .expect("both ranges are inside the clip");

        assert_eq!(earlier_first, later_first);
        assert_eq!(earlier_first.output_duration_nanos(), Some(22 * SECOND));
    }

    #[test]
    fn a_deletion_is_a_trim_when_it_reaches_an_end_of_the_clip() {
        let document = three_segments();

        let from_the_start = document
            .apply(EditOperation::DeleteSection {
                range: when(0, 6 * SECOND),
            })
            .expect("a deletion that starts at the start");
        let trimmed = document
            .apply(EditOperation::TrimStart {
                at: OutputTime::from_nanos(6 * SECOND),
            })
            .expect("the same thing said the other way");
        assert_eq!(from_the_start, trimmed);

        let to_the_end = document
            .apply(EditOperation::DeleteSection {
                range: when(26 * SECOND, 30 * SECOND),
            })
            .expect("a deletion that reaches the end");
        let cut_short = document
            .apply(EditOperation::TrimEnd {
                at: OutputTime::from_nanos(26 * SECOND),
            })
            .expect("the same thing said the other way");
        assert_eq!(to_the_end, cut_short);
    }

    #[test]
    fn no_operation_at_any_moment_of_the_clip_can_produce_a_document_that_would_not_save() {
        // The guarantee stated as a sweep: every operation, at every tenth of a
        // second of a clip that uses speed, overlays and fades. Whatever comes
        // back must be a document that could be written to the database.
        let source = SourceId::new(0);
        let document = EditDocument::new("Ace")
            .with_source(Source::new(source, RecordingId::new("rec-1")))
            .with_segment(Segment::new(source, span(0, 5 * SECOND)))
            .with_segment(
                Segment::new(source, span(20 * SECOND, 32 * SECOND))
                    .at_speed(Speed::new(3, 2).expect("a valid speed")),
            )
            .with_segment(Segment::new(source, span(60 * SECOND, 63 * SECOND)))
            .with_audio_track(
                AudioTrack::new("Game", vec![TrackInput::new(source, 0)])
                    .with_fades(Duration::from_secs(3), Duration::from_secs(4)),
            )
            .with_overlay(TextOverlay::new("Ace", when(SECOND, 9 * SECOND)));
        let clip_nanos = document
            .output_duration_nanos()
            .expect("the fixture is readable");

        for step in 0..=clip_nanos / (SECOND / 10) {
            let at = OutputTime::from_nanos(step * SECOND / 10);
            for operation in [
                EditOperation::TrimStart { at },
                EditOperation::TrimEnd { at },
                EditOperation::Split { at },
            ] {
                if let Ok(edited) = document.apply(operation) {
                    edited.write().unwrap_or_else(|error| {
                        panic!(
                            "{operation:?} at {at:?} produced a document that cannot be \
                             saved: {error}"
                        )
                    });
                }
            }
            if let Some(range) = OutputSpan::new(at, OutputTime::from_nanos(clip_nanos)) {
                if let Ok(edited) = document.apply(EditOperation::DeleteSection { range }) {
                    edited.write().unwrap_or_else(|error| {
                        panic!(
                            "deleting {range:?} produced a document that cannot be saved: \
                             {error}"
                        )
                    });
                }
            }
        }
    }
}
