//! What can be wrong with a document, said in a way somebody can act on.
//!
//! Two levels, because they have two audiences. [`DocumentProblem`] is about
//! the *contents* — a segment naming a source that is not declared — and is
//! produced by validation, which runs on every read and every write; the editor
//! can put it in front of a user. [`EditDocumentError`] is about the document
//! as a whole — it is not JSON, it is from a newer build, a migration refused —
//! and the important thing about most of its variants is what they imply the
//! caller must **not** do next, which is write anything back.

use core::fmt;

use crate::source::SourceId;

/// Something wrong with a document's contents.
///
/// Each variant names where the problem is, because a clip with nine segments
/// and four audio tracks gives "invalid document" nowhere to start.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DocumentProblem {
    /// Two sources were declared with the same identifier.
    DuplicateSource {
        /// The identifier used twice.
        id: SourceId,
    },
    /// A source was declared without saying which recording it is.
    SourceWithoutRecording {
        /// The source in question.
        id: SourceId,
    },
    /// A segment refers to a source the document does not declare.
    UnknownSource {
        /// Which segment, numbered from zero as the document holds them.
        segment: usize,
        /// The source it asked for.
        source: SourceId,
    },
    /// A segment's span is empty or backwards.
    EmptySpan {
        /// Which segment.
        segment: usize,
    },
    /// A segment's speed has a zero in it.
    UnusableSpeed {
        /// Which segment.
        segment: usize,
    },
    /// A segment is so short, or so fast, that it contributes no output at all.
    SegmentProducesNoOutput {
        /// Which segment.
        segment: usize,
    },
    /// The segments add up to more than `u64` nanoseconds.
    TimelineTooLong,
    /// A segment's crop is not a rectangle inside the frame.
    UnusableCrop {
        /// Which segment.
        segment: usize,
    },
    /// An audio track has no name to show beside its slider.
    TrackWithoutName {
        /// Which track, numbered from zero as the document holds them.
        track: usize,
    },
    /// Two audio tracks have the same name.
    DuplicateTrackName {
        /// The name used twice.
        name: String,
    },
    /// An audio track is fed by nothing, so it can only produce silence.
    TrackWithoutInputs {
        /// The track's name.
        name: String,
    },
    /// An audio track draws on a source the document does not declare.
    TrackFromUnknownSource {
        /// The track's name.
        name: String,
        /// The source it asked for.
        source: SourceId,
    },
    /// One recording's stream feeds more than one track of the export.
    StreamUsedTwice {
        /// The source the stream belongs to.
        source: SourceId,
        /// The stream index.
        stream: u16,
    },
    /// An audio track's level is not a number an exporter can apply.
    UnusableGain {
        /// The track's name.
        name: String,
        /// The level found.
        gain_db: f64,
    },
    /// A track's fades are longer than the clip they are on.
    FadesLongerThanTheClip {
        /// The track's name.
        name: String,
    },
    /// An overlay says nothing.
    EmptyOverlay {
        /// Which overlay, numbered from zero as the document holds them.
        overlay: usize,
    },
    /// An overlay's timing range is empty or backwards.
    EmptyOverlayRange {
        /// Which overlay.
        overlay: usize,
    },
    /// An overlay is on screen after the clip has ended.
    OverlayPastTheEnd {
        /// Which overlay.
        overlay: usize,
        /// Where it ends, in nanoseconds on the edited timeline.
        ends_at_nanos: u64,
        /// How long the clip is, in nanoseconds.
        clip_nanos: u64,
    },
    /// An overlay is positioned off the frame.
    OverlayOffTheFrame {
        /// Which overlay.
        overlay: usize,
    },
    /// An overlay's text height is invisible or absurd.
    UnusableOverlayHeight {
        /// Which overlay.
        overlay: usize,
        /// The height found, as a percentage of the frame.
        height_percent: u8,
    },
    /// The output aspect ratio has a zero in it.
    UnusableAspectRatio,
}

impl fmt::Display for DocumentProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSource { id } => {
                write!(formatter, "two sources are both numbered {id}")
            }
            Self::SourceWithoutRecording { id } => {
                write!(formatter, "source {id} does not say which recording it is")
            }
            Self::UnknownSource { segment, source } => write!(
                formatter,
                "segment {segment} plays source {source}, which this edit does not declare"
            ),
            Self::EmptySpan { segment } => write!(
                formatter,
                "segment {segment} ends before or exactly where it starts, so it plays nothing"
            ),
            Self::UnusableSpeed { segment } => write!(
                formatter,
                "segment {segment} has a speed with a zero in it, which has no playback rate"
            ),
            Self::SegmentProducesNoOutput { segment } => write!(
                formatter,
                "segment {segment} is too short at its speed to contribute any output"
            ),
            Self::TimelineTooLong => formatter.write_str(
                "the segments add up to more time than an edit can represent (584 years)",
            ),
            Self::UnusableCrop { segment } => write!(
                formatter,
                "segment {segment} is cropped to a rectangle that is empty or outside the frame"
            ),
            Self::TrackWithoutName { track } => {
                write!(formatter, "audio track {track} has no name")
            }
            Self::DuplicateTrackName { name } => {
                write!(formatter, "two audio tracks are both called `{name}`")
            }
            Self::TrackWithoutInputs { name } => write!(
                formatter,
                "audio track `{name}` is fed by no recording, so it could only be silent"
            ),
            Self::TrackFromUnknownSource { name, source } => write!(
                formatter,
                "audio track `{name}` draws on source {source}, which this edit does not declare"
            ),
            Self::StreamUsedTwice { source, stream } => write!(
                formatter,
                "stream {stream} of source {source} feeds more than one audio track, so the \
                 same audio would be exported twice"
            ),
            Self::UnusableGain { name, gain_db } => write!(
                formatter,
                "audio track `{name}` is set to {gain_db} dB, which is outside {} to {} dB",
                crate::audio::MINIMUM_GAIN_DB,
                crate::audio::MAXIMUM_GAIN_DB
            ),
            Self::FadesLongerThanTheClip { name } => write!(
                formatter,
                "audio track `{name}` fades in and out for longer than the clip lasts"
            ),
            Self::EmptyOverlay { overlay } => {
                write!(formatter, "overlay {overlay} has no text")
            }
            Self::EmptyOverlayRange { overlay } => write!(
                formatter,
                "overlay {overlay} ends before or exactly where it starts, so it never appears"
            ),
            Self::OverlayPastTheEnd {
                overlay,
                ends_at_nanos,
                clip_nanos,
            } => write!(
                formatter,
                "overlay {overlay} runs to {ends_at_nanos} ns, past the end of a clip that \
                 lasts {clip_nanos} ns"
            ),
            Self::OverlayOffTheFrame { overlay } => {
                write!(formatter, "overlay {overlay} is positioned off the frame")
            }
            Self::UnusableOverlayHeight {
                overlay,
                height_percent,
            } => write!(
                formatter,
                "overlay {overlay} is {height_percent}% of the frame's height, which is outside \
                 {} to {}%",
                crate::overlay::MINIMUM_TEXT_HEIGHT_PERCENT,
                crate::overlay::MAXIMUM_TEXT_HEIGHT_PERCENT
            ),
            Self::UnusableAspectRatio => {
                formatter.write_str("the output aspect ratio has a zero in it")
            }
        }
    }
}

impl core::error::Error for DocumentProblem {}

/// Why a document could not be read or written.
#[derive(Debug)]
#[non_exhaustive]
pub enum EditDocumentError {
    /// The text is not JSON at all.
    Syntax {
        /// What the parser said.
        message: String,
    },
    /// The document does not say which schema version it is.
    SchemaVersionMissing,
    /// The document was written by a newer build of Clipped.
    ///
    /// **Nothing may be written back.** An older Clipped rewriting a document
    /// it does not understand is how a user loses the edit they made on the
    /// machine that was up to date (AGENTS.md sections 43 and 56).
    SchemaTooNew {
        /// The version the document carries.
        found: u32,
        /// The newest version this build understands.
        supported: u32,
    },
    /// The document is older than this build and there is no route forward.
    MigrationMissing {
        /// The version reached before the chain ran out.
        from: u32,
        /// The version it needed to reach.
        to: u32,
    },
    /// A migration step refused the document.
    MigrationFailed {
        /// The version being migrated from.
        from: u32,
        /// The version being migrated to.
        to: u32,
        /// What the step said.
        reason: String,
    },
    /// The document is JSON of the right version but not the right shape.
    Shape {
        /// What the deserialiser said.
        message: String,
    },
    /// The document is the right shape but says something impossible.
    Invalid(DocumentProblem),
}

impl fmt::Display for EditDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { message } => {
                write!(formatter, "this edit is not readable as JSON: {message}")
            }
            Self::SchemaVersionMissing => formatter.write_str(
                "this edit has no `schema_version`, so there is no way to tell what it is",
            ),
            Self::SchemaTooNew { found, supported } => write!(
                formatter,
                "this edit was saved by a newer version of Clipped (format {found}; this build \
                 reads up to {supported}). Update Clipped to open it. Nothing has been changed."
            ),
            Self::MigrationMissing { from, to } => write!(
                formatter,
                "this edit is in format {from} and this build has no way to convert it to {to}; \
                 it has been left exactly as it was"
            ),
            Self::MigrationFailed { from, to, reason } => write!(
                formatter,
                "this edit could not be converted from format {from} to {to}: {reason}; it has \
                 been left exactly as it was"
            ),
            Self::Shape { message } => {
                write!(formatter, "this edit is not shaped like one: {message}")
            }
            Self::Invalid(problem) => write!(formatter, "this edit cannot be used: {problem}"),
        }
    }
}

impl core::error::Error for EditDocumentError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Invalid(problem) => Some(problem),
            _ => None,
        }
    }
}

impl From<DocumentProblem> for EditDocumentError {
    fn from(problem: DocumentProblem) -> Self {
        Self::Invalid(problem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_document_tells_the_user_what_to_do_and_that_nothing_was_lost() {
        let message = EditDocumentError::SchemaTooNew {
            found: 4,
            supported: 1,
        }
        .to_string();

        assert!(message.contains("Update Clipped"), "{message}");
        assert!(message.contains("Nothing has been changed"), "{message}");
    }

    #[test]
    fn a_problem_names_where_it_is() {
        let message = DocumentProblem::UnknownSource {
            segment: 2,
            source: SourceId::new(7),
        }
        .to_string();

        assert!(message.contains("segment 2"), "{message}");
        assert!(message.contains("source 7"), "{message}");
    }

    #[test]
    fn a_problem_reaches_the_top_through_the_error_it_is_wrapped_in() {
        let error = EditDocumentError::from(DocumentProblem::UnusableAspectRatio);

        assert!(error.to_string().contains("aspect ratio"), "{error}");
        assert!(
            core::error::Error::source(&error).is_some(),
            "the problem should still be reachable as a source"
        );
    }
}
