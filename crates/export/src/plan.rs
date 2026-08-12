//! Deciding *how* an edit can be rendered, before anything is written.
//!
//! An export is one of two very different operations, and which one it is
//! decides how long it takes and what it costs in quality:
//!
//! - A **stream copy** moves the coded packets the recording already holds into
//!   a new container, cutting between them. No decoder runs, no encoder runs,
//!   and the pictures in the result are the pictures in the recording, bit for
//!   bit. It takes about as long as reading the file.
//! - A **re-encode** decodes, transforms and encodes every frame. It is the
//!   only way to change what a frame looks like — or to begin a segment
//!   anywhere but a keyframe — and it costs both time and a generation of
//!   quality.
//!
//! Nothing here writes anything. [`ExportPlan::of`] answers the question from
//! the document and a description of the recordings, so that a caller can say
//! what an export will cost before somebody waits for it (AGENTS.md section
//! 45), and so that the decision itself is testable without a recording.
//!
//! # When a copy is possible
//!
//! Every one of these, and the plan names the ones that failed:
//!
//! | Condition | Why |
//! | --- | --- |
//! | One recording | Two recordings are two sets of stream parameters, and one container header |
//! | Every segment untransformed | A speed, a crop or a rotation is a new picture |
//! | No overlays | Text over the picture is a new picture |
//! | The aspect ratio matches the source | A different shape is a new picture |
//! | Codecs the container writer can describe | A track it cannot describe cannot be declared |
//! | The stream is not reordered | The tail of a segment is cut in presentation order |
//! | Every segment starts on a keyframe | A decoder cannot start anywhere else |
//! | Every audio track is one stream at its recorded level | Anything else is a mix, which has to be produced |
//!
//! # The keyframe rule, and what is deliberately not done about it
//!
//! `docs/editing.md` settles this and this crate obeys it:
//!
//! > A keyframe is a re-encode decision, not a timing one. Stream-copying a
//! > segment can only begin at a keyframe, so a copy would have to move the cut
//! > back to the previous one — up to a whole group of pictures of material the
//! > user deleted. That is a visible difference from the preview, so it is not
//! > something an exporter may do quietly.
//!
//! So a segment whose first frame is not a keyframe makes the export a
//! re-encode. It is never quietly moved to the previous keyframe, and the
//! blocker carries both times so that a caller can say how far off it was.

use core::fmt;

use clipped_edit::{
    AspectRatio, EditDocument, OutputTime, RecordingId, SourceId, SourceSpan, SourceTime,
    TrackOutput,
};
use clipped_muxer::{AudioCodec, VideoCodec};

use crate::source::{IndexedFrame, SourceProfile, SourceStream, StreamFormat};

/// How an edit has to be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportMethod {
    /// The coded packets are copied. Fast, and lossless.
    StreamCopy,
    /// Every frame has to be decoded, transformed and encoded again.
    Reencode,
}

impl ExportMethod {
    /// Whether the coded packets survive unchanged.
    #[must_use]
    pub const fn is_copy(self) -> bool {
        matches!(self, Self::StreamCopy)
    }
}

impl fmt::Display for ExportMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StreamCopy => "stream copy",
            Self::Reencode => "re-encode",
        })
    }
}

/// Why an audio track cannot be copied as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MixReason {
    /// The track is fed by more than one recorded stream, so it is a sum.
    SeveralInputs,
    /// The track plays at something other than the level it was recorded at.
    Level,
    /// The track is muted, and silence has to be produced rather than copied.
    Silenced,
    /// The track fades in or out, which changes every sample it covers.
    Fades,
}

impl fmt::Display for MixReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SeveralInputs => "it is fed by more than one recorded stream",
            Self::Level => "it does not play at the level it was recorded at",
            Self::Silenced => "it is silent, and silence has to be produced rather than copied",
            Self::Fades => "it fades, which changes every sample it covers",
        })
    }
}

/// One reason this edit cannot be exported by copying its packets.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopyBlocker {
    /// The edit draws on more than one recording.
    SeveralRecordings {
        /// How many.
        recordings: usize,
    },
    /// The recording has no picture track to copy.
    NoVideo,
    /// The container writer has no name for the recording's video codec.
    VideoCodecNotDescribable {
        /// What the codec is called, as FFmpeg names it.
        codec: String,
    },
    /// The container writer has no name for one of the audio codecs.
    AudioCodecNotDescribable {
        /// Which audio stream of the recording, numbered from zero.
        stream: u16,
        /// What the codec is called, as FFmpeg names it.
        codec: String,
    },
    /// A segment changes what its pictures look like.
    SegmentTransformed {
        /// Which segment of the document.
        segment: usize,
    },
    /// A segment covers no pictures at all.
    SegmentHasNoFrames {
        /// Which segment of the document.
        segment: usize,
    },
    /// A segment's first picture is not one a decoder can start at.
    SegmentDoesNotStartOnAKeyframe {
        /// Which segment of the document.
        segment: usize,
        /// Where the segment's first picture is, in the recording.
        frame_nanos: u64,
        /// The keyframe a copy would have had to move the cut back to, when
        /// there is one before it.
        previous_keyframe_nanos: Option<u64>,
    },
    /// The recording's pictures are stored in a different order from the one
    /// they are shown in.
    ReorderedStream,
    /// The clip has text drawn over it.
    Overlays {
        /// How many.
        overlays: usize,
    },
    /// The clip is to be exported at a shape the recording is not.
    AspectRatioDiffers {
        /// What the document asks for.
        wanted: AspectRatio,
        /// The recording's picture width.
        source_width: u32,
        /// The recording's picture height.
        source_height: u32,
    },
    /// An audio track of the output is a mix rather than one recorded stream.
    TrackNeedsMixing {
        /// What the track is called in the document.
        name: String,
        /// What makes it a mix.
        reason: MixReason,
    },
}

impl fmt::Display for CopyBlocker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SeveralRecordings { recordings } => write!(
                formatter,
                "the clip draws on {recordings} recordings, which cannot share one set of \
                 stream descriptions"
            ),
            Self::NoVideo => formatter.write_str("the recording has no picture track"),
            Self::VideoCodecNotDescribable { codec } => write!(
                formatter,
                "the recording's picture is {codec}, which Clipped's container writer cannot \
                 describe"
            ),
            Self::AudioCodecNotDescribable { stream, codec } => write!(
                formatter,
                "audio stream {stream} is {codec}, which Clipped's container writer cannot \
                 describe"
            ),
            Self::SegmentTransformed { segment } => write!(
                formatter,
                "segment {segment} is sped up, cropped or rotated, which makes new pictures"
            ),
            Self::SegmentHasNoFrames { segment } => write!(
                formatter,
                "segment {segment} covers no pictures of the recording at all"
            ),
            Self::SegmentDoesNotStartOnAKeyframe {
                segment,
                frame_nanos,
                previous_keyframe_nanos,
            } => {
                write!(
                    formatter,
                    "segment {segment} starts at {:.3}s, which is not a picture a decoder can \
                     start at",
                    seconds(*frame_nanos)
                )?;
                match previous_keyframe_nanos {
                    Some(keyframe) => write!(
                        formatter,
                        "; a copy would have had to begin {:.3}s earlier and show material the \
                         cut removed",
                        seconds(frame_nanos.saturating_sub(*keyframe))
                    ),
                    None => formatter.write_str("; there is no keyframe before it"),
                }
            }
            Self::ReorderedStream => formatter.write_str(
                "the recording's pictures are stored out of the order they are shown in, and a \
                 copy cuts the end of a segment in the order they are shown",
            ),
            Self::Overlays { overlays } => write!(
                formatter,
                "{overlays} pieces of text are drawn over the picture, which makes new pictures"
            ),
            Self::AspectRatioDiffers {
                wanted,
                source_width,
                source_height,
            } => write!(
                formatter,
                "the clip is to be {}:{} and the recording is {source_width}x{source_height}",
                wanted.width, wanted.height
            ),
            Self::TrackNeedsMixing { name, reason } => {
                write!(formatter, "the audio track '{name}' is a mix: {reason}")
            }
        }
    }
}

/// A nanosecond count as seconds, for a message.
fn seconds(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000_000.0
}

/// One segment of the document, placed on the output timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedSegment {
    /// Which segment of the document this is.
    pub segment: usize,
    /// Which of the document's sources it draws on.
    pub source: SourceId,
    /// The material it plays, on the recording's own timeline.
    pub span: SourceSpan,
    /// Where it begins on the exported file's timeline.
    pub output_start: OutputTime,
    /// The first picture it shows, where the recording has one in its span.
    pub opening_frame: Option<IndexedFrame>,
    /// How many pictures it shows.
    pub frames: usize,
}

impl PlannedSegment {
    /// Where a source time inside this segment lands on the output timeline.
    ///
    /// The one conversion a copy makes, and the reason it is here rather than
    /// in the writing loop: it is the arithmetic that decides whether the
    /// exported file matches the timeline, so it is testable on its own.
    ///
    /// [`None`] for a time outside the segment's span, which is a caller
    /// asking about material this segment does not play.
    #[must_use]
    pub fn output_of(&self, at: SourceTime) -> Option<OutputTime> {
        let into_segment = at.nanos_since(self.span.start())?;
        self.output_start.checked_add_nanos(into_segment)
    }
}

/// One audio track of the exported file, and the recorded stream it copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAudioTrack {
    /// What the track is called in the export.
    pub name: Option<String>,
    /// Which of the recording's audio streams feeds it, numbered from zero.
    pub stream: u16,
}

/// What exporting an edit would do, worked out without writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPlan {
    method: ExportMethod,
    blockers: Vec<CopyBlocker>,
    segments: Vec<PlannedSegment>,
    audio: Vec<PlannedAudioTrack>,
    recording: Option<RecordingId>,
    output_nanos: u64,
}

impl ExportPlan {
    /// Works out how `document` would be rendered from `profiles`.
    ///
    /// `profiles` describes each recording the document names.
    ///
    /// # Errors
    ///
    /// [`PlanError`] for a document that cannot be read at all, or one naming a
    /// recording that was not described. Everything else — every reason a copy
    /// is impossible — is a [`CopyBlocker`] in the plan rather than a failure,
    /// because "this will be a re-encode, and here is why" is an answer.
    pub fn of(
        document: &EditDocument,
        profiles: &[(RecordingId, SourceProfile)],
    ) -> Result<Self, PlanError> {
        document.validate().map_err(PlanError::Document)?;
        let output_nanos = document
            .output_duration_nanos()
            .ok_or(PlanError::TimelineUnreadable)?;

        let recordings = recordings_of(document);
        let mut blockers = Vec::new();

        // The one recording a copy could draw on, or nothing when there is not
        // exactly one. Every check below that needs the file uses it, so a
        // multi-recording edit collects one blocker rather than a cascade.
        let single = match recordings.len() {
            1 => Some(recordings[0].clone()),
            count => {
                blockers.push(CopyBlocker::SeveralRecordings { recordings: count });
                None
            }
        };

        let profile = match &single {
            Some(recording) => Some(profile_of(profiles, recording).ok_or_else(|| {
                PlanError::RecordingNotDescribed {
                    recording: recording.clone(),
                }
            })?),
            None => {
                // Still checked, so that a caller is told about a missing
                // description rather than about the joining it was going to be
                // told about anyway.
                for recording in &recordings {
                    if profile_of(profiles, recording).is_none() {
                        return Err(PlanError::RecordingNotDescribed {
                            recording: recording.clone(),
                        });
                    }
                }
                None
            }
        };

        let segments = place_segments(document, profile)?;
        let audio = plan_audio(document, profile, &mut blockers)?;

        if !document.overlays.is_empty() {
            blockers.push(CopyBlocker::Overlays {
                overlays: document.overlays.len(),
            });
        }

        if let Some(profile) = profile {
            check_video(profile, document.aspect_ratio, &mut blockers);
            check_segments(document, profile, &segments, &mut blockers);
        }

        let method = if blockers.is_empty() {
            ExportMethod::StreamCopy
        } else {
            ExportMethod::Reencode
        };

        Ok(Self {
            method,
            blockers,
            segments,
            audio,
            recording: single,
            output_nanos,
        })
    }

    /// How the export would be made.
    #[must_use]
    pub const fn method(&self) -> ExportMethod {
        self.method
    }

    /// Every reason a copy is not possible, in the order they were found.
    ///
    /// Empty exactly when the method is a copy. All of them rather than the
    /// first, because a caller offering to re-encode is explaining a decision
    /// and one of three reasons is a worse explanation than three.
    #[must_use]
    pub fn blockers(&self) -> &[CopyBlocker] {
        &self.blockers
    }

    /// The segments, in the order they play, placed on the output timeline.
    #[must_use]
    pub fn segments(&self) -> &[PlannedSegment] {
        &self.segments
    }

    /// The audio tracks the export would write.
    #[must_use]
    pub fn audio_tracks(&self) -> &[PlannedAudioTrack] {
        &self.audio
    }

    /// The one recording this edit draws on, when it draws on one.
    #[must_use]
    pub const fn recording(&self) -> Option<&RecordingId> {
        self.recording.as_ref()
    }

    /// How long the exported clip is, in nanoseconds.
    #[must_use]
    pub const fn output_nanos(&self) -> u64 {
        self.output_nanos
    }

    /// How many pictures the export would write.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.segments.iter().map(|segment| segment.frames).sum()
    }
}

impl fmt::Display for ExportPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:.3}s in {} segments by {}",
            seconds(self.output_nanos),
            self.segments.len(),
            self.method
        )?;
        for blocker in &self.blockers {
            write!(formatter, "; {blocker}")?;
        }
        Ok(())
    }
}

/// The recordings a document draws on, each once, in the order it declares
/// them.
fn recordings_of(document: &EditDocument) -> Vec<RecordingId> {
    let mut recordings: Vec<RecordingId> = Vec::new();
    for segment in &document.segments {
        let Some(source) = document.source(segment.source) else {
            continue;
        };
        if !recordings.contains(&source.recording) {
            recordings.push(source.recording.clone());
        }
    }
    recordings
}

/// The description of one recording, where it was given.
fn profile_of<'a>(
    profiles: &'a [(RecordingId, SourceProfile)],
    recording: &RecordingId,
) -> Option<&'a SourceProfile> {
    profiles
        .iter()
        .find(|(named, _)| named == recording)
        .map(|(_, profile)| profile)
}

/// Lays the document's segments end to end on the output timeline.
fn place_segments(
    document: &EditDocument,
    profile: Option<&SourceProfile>,
) -> Result<Vec<PlannedSegment>, PlanError> {
    let mut placed = Vec::with_capacity(document.segments.len());
    let mut output_nanos = 0_u64;

    for (index, segment) in document.segments.iter().enumerate() {
        let length = segment
            .output_nanos()
            .ok_or(PlanError::TimelineUnreadable)?;
        let opening =
            profile.and_then(|profile| profile.frames().first_at_or_after(segment.span.start()));
        let frames = profile.map_or(0, |profile| {
            profile
                .frames()
                .frames_in(segment.span.start(), segment.span.end())
                .count()
        });

        placed.push(PlannedSegment {
            segment: index,
            source: segment.source,
            span: segment.span,
            output_start: OutputTime::from_nanos(output_nanos),
            opening_frame: opening.filter(|frame| segment.span.contains(frame.presentation)),
            frames,
        });

        output_nanos = output_nanos
            .checked_add(length)
            .ok_or(PlanError::TimelineUnreadable)?;
    }

    Ok(placed)
}

/// Whether the recording's picture can be copied at all, and at the shape asked
/// for.
fn check_video(
    profile: &SourceProfile,
    aspect_ratio: Option<AspectRatio>,
    blockers: &mut Vec<CopyBlocker>,
) {
    let Some(video) = profile.video() else {
        blockers.push(CopyBlocker::NoVideo);
        return;
    };
    let StreamFormat::Video {
        codec,
        width,
        height,
        ..
    } = video.format()
    else {
        blockers.push(CopyBlocker::NoVideo);
        return;
    };

    if codec.is_none() {
        blockers.push(CopyBlocker::VideoCodecNotDescribable {
            codec: video.codec_name().to_owned(),
        });
    }

    if let Some(wanted) = aspect_ratio {
        // Cross-multiplied rather than divided: two integer ratios are equal
        // when their cross products are, and a floating comparison of 16/9
        // against 2560/1440 is a comparison that can go either way on a
        // different machine (AGENTS.md section 25).
        let matches = u64::from(wanted.width) * u64::from(*height)
            == u64::from(wanted.height) * u64::from(*width);
        if !matches {
            blockers.push(CopyBlocker::AspectRatioDiffers {
                wanted,
                source_width: *width,
                source_height: *height,
            });
        }
    }

    if profile.frames().is_reordered() {
        blockers.push(CopyBlocker::ReorderedStream);
    }
}

/// Whether every segment can begin where a decoder can.
fn check_segments(
    document: &EditDocument,
    profile: &SourceProfile,
    segments: &[PlannedSegment],
    blockers: &mut Vec<CopyBlocker>,
) {
    for (placed, segment) in segments.iter().zip(&document.segments) {
        if !segment.is_untransformed() {
            blockers.push(CopyBlocker::SegmentTransformed {
                segment: placed.segment,
            });
            continue;
        }

        let Some(opening) = placed.opening_frame else {
            blockers.push(CopyBlocker::SegmentHasNoFrames {
                segment: placed.segment,
            });
            continue;
        };

        if !opening.keyframe {
            let previous = profile
                .frames()
                .frames()
                .iter()
                .filter(|frame| frame.keyframe && frame.presentation < opening.presentation)
                .map(|frame| frame.presentation.as_nanos())
                .max();
            blockers.push(CopyBlocker::SegmentDoesNotStartOnAKeyframe {
                segment: placed.segment,
                frame_nanos: opening.presentation.as_nanos(),
                previous_keyframe_nanos: previous,
            });
        }
    }
}

/// Works out which recorded stream each output audio track copies.
///
/// A document that declares no tracks at all carries the recording's audio
/// **as it was recorded** — every stream, in the container's own order, with
/// its own name. That is what an edit which says nothing about audio means:
/// an instant clip (`EditDocument::from_recording`) declares one source and one
/// segment and no mix, and a clip of a match that arrived silent would be a
/// worse answer than one that sounds like the recording (AGENTS.md section 54).
fn plan_audio(
    document: &EditDocument,
    profile: Option<&SourceProfile>,
    blockers: &mut Vec<CopyBlocker>,
) -> Result<Vec<PlannedAudioTrack>, PlanError> {
    if document.audio_tracks.is_empty() {
        let Some(profile) = profile else {
            return Ok(Vec::new());
        };
        return Ok(profile
            .audio()
            .iter()
            .enumerate()
            .map(|(index, stream)| PlannedAudioTrack {
                name: stream.name().map(str::to_owned),
                // The document's own numbering: 0 for the first audio stream.
                stream: u16::try_from(index).unwrap_or(u16::MAX),
            })
            .collect());
    }

    let mut planned = Vec::with_capacity(document.audio_tracks.len());
    for (index, track) in document.audio_tracks.iter().enumerate() {
        let mut reason = None;
        if track.inputs.len() > 1 {
            reason = Some(MixReason::SeveralInputs);
        } else if document.track_output(index) == Some(TrackOutput::Silent) {
            reason = Some(MixReason::Silenced);
        } else if track.gain_db != 0.0 {
            reason = Some(MixReason::Level);
        } else if !track.fade_in.is_zero() || !track.fade_out.is_zero() {
            reason = Some(MixReason::Fades);
        }

        if let Some(reason) = reason {
            blockers.push(CopyBlocker::TrackNeedsMixing {
                name: track.name.clone(),
                reason,
            });
            continue;
        }

        let input = track
            .inputs
            .first()
            .ok_or_else(|| PlanError::Document(unreachable_track_without_inputs()))?;

        if let Some(profile) = profile {
            match profile.audio_stream(input.stream) {
                None => {
                    return Err(PlanError::AudioStreamMissing {
                        track: track.name.clone(),
                        stream: input.stream,
                        available: profile.audio().len(),
                    })
                }
                Some(stream) => {
                    if let StreamFormat::Audio { codec: None, .. } = stream.format() {
                        blockers.push(CopyBlocker::AudioCodecNotDescribable {
                            stream: input.stream,
                            codec: stream.codec_name().to_owned(),
                        });
                        continue;
                    }
                }
            }
        }

        planned.push(PlannedAudioTrack {
            name: Some(track.name.clone()),
            stream: input.stream,
        });
    }

    Ok(planned)
}

/// The problem a track with no inputs is, phrased the way the document model
/// phrases it.
///
/// Unreachable: `EditDocument::validate` runs first and refuses exactly this.
/// Written as a value rather than as a panic because an export must not end in
/// one (AGENTS.md section 15).
fn unreachable_track_without_inputs() -> clipped_edit::DocumentProblem {
    clipped_edit::DocumentProblem::TrackWithoutInputs {
        name: String::new(),
    }
}

/// Why a plan could not be made at all.
///
/// Distinct from [`CopyBlocker`], which is a reason the export will be slower
/// rather than a reason there will not be one.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PlanError {
    /// The document says something impossible.
    Document(clipped_edit::DocumentProblem),
    /// A segment's length could not be worked out, so the timeline has none.
    TimelineUnreadable,
    /// The document names a recording that was not described.
    RecordingNotDescribed {
        /// The recording that is missing.
        recording: RecordingId,
    },
    /// An audio track draws on a stream the recording does not have.
    AudioStreamMissing {
        /// The track that asked for it.
        track: String,
        /// The stream it asked for.
        stream: u16,
        /// How many audio streams the recording actually has.
        available: usize,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Document(problem) => write!(formatter, "the clip cannot be read: {problem}"),
            Self::TimelineUnreadable => {
                formatter.write_str("the clip's timeline has no length that can be worked out")
            }
            Self::RecordingNotDescribed { recording } => write!(
                formatter,
                "the clip plays the recording {} and nothing said where it is",
                recording.as_str()
            ),
            Self::AudioStreamMissing {
                track,
                stream,
                available,
            } => write!(
                formatter,
                "the audio track '{track}' plays recorded stream {stream}, and the recording \
                 has {available}"
            ),
        }
    }
}

impl std::error::Error for PlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Document(problem) => Some(problem),
            _ => None,
        }
    }
}

/// Whether a stream describes audio the container writer knows.
///
/// Used by [`crate::render`] as well, which is why it is here beside the
/// decision that depends on it rather than duplicated there.
#[must_use]
pub(crate) fn audio_codec_of(stream: &SourceStream) -> Option<AudioCodec> {
    match stream.format() {
        StreamFormat::Audio { codec, .. } => *codec,
        _ => None,
    }
}

/// Whether a stream describes video the container writer knows.
#[must_use]
pub(crate) fn video_codec_of(stream: &SourceStream) -> Option<VideoCodec> {
    match stream.format() {
        StreamFormat::Video { codec, .. } => *codec,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use clipped_edit::{
        AudioTrack, RecordingId, Rotation, Segment, Source, SourceId, Speed, TextOverlay,
        TrackInput,
    };

    use super::*;
    use crate::source::{SourceStream, VideoFrameIndex};

    const SECOND: u64 = 1_000_000_000;

    fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
        SourceSpan::new(
            SourceTime::from_nanos(start_nanos),
            SourceTime::from_nanos(end_nanos),
        )
        .expect("the test span ends after it starts")
    }

    /// Ten seconds of 10 fps video, a keyframe every second.
    fn frames() -> VideoFrameIndex {
        VideoFrameIndex::new(
            (0..100)
                .map(|number| {
                    let at = SourceTime::from_nanos(number * SECOND / 10);
                    IndexedFrame {
                        presentation: at,
                        decode: at,
                        keyframe: number % 10 == 0,
                    }
                })
                .collect(),
        )
    }

    /// A recording of that video with two named audio streams.
    fn profile() -> SourceProfile {
        SourceProfile::new(
            vec![
                SourceStream::video(0, Some(VideoCodec::H264), 1920, 1080, "h264")
                    .with_extradata(vec![1, 2, 3]),
                SourceStream::audio(1, Some(AudioCodec::PcmS16Le), 48_000, 2, "pcm_s16le")
                    .with_name("Compatibility Mix"),
                SourceStream::audio(2, Some(AudioCodec::PcmS16Le), 48_000, 1, "pcm_s16le")
                    .with_name("Microphone"),
            ],
            frames(),
        )
    }

    fn profiles() -> Vec<(RecordingId, SourceProfile)> {
        vec![(RecordingId::new("rec-1"), profile())]
    }

    /// Two seconds of the recording, cut on keyframes: the copy case.
    fn copyable() -> EditDocument {
        EditDocument::from_recording("Ace", RecordingId::new("rec-1"), span(SECOND, 3 * SECOND))
    }

    fn plan(document: &EditDocument) -> ExportPlan {
        ExportPlan::of(document, &profiles()).expect("the plan can be made")
    }

    #[test]
    fn a_cut_on_a_keyframe_with_nothing_else_changed_is_a_copy() {
        let plan = plan(&copyable());

        assert_eq!(
            plan.method(),
            ExportMethod::StreamCopy,
            "{:?}",
            plan.blockers()
        );
        assert!(plan.blockers().is_empty());
        assert_eq!(plan.output_nanos(), 2 * SECOND);
        assert_eq!(plan.frames(), 20, "two seconds at 10 fps");
        assert_eq!(
            plan.segments()[0]
                .opening_frame
                .map(|frame| frame.presentation),
            Some(SourceTime::from_nanos(SECOND))
        );
    }

    #[test]
    fn a_cut_between_keyframes_is_a_re_encode_and_says_how_far_off_it_was() {
        // The rule docs/editing.md fixes: the cut is not moved back to the
        // keyframe, because that would show material the user deleted.
        let document = EditDocument::from_recording(
            "Ace",
            RecordingId::new("rec-1"),
            // 1.5 s is a frame, and it is not a keyframe: the keyframes are at
            // whole seconds.
            span(SECOND + SECOND / 2, 3 * SECOND),
        );

        let plan = plan(&document);

        assert_eq!(plan.method(), ExportMethod::Reencode);
        assert_eq!(
            plan.blockers(),
            [CopyBlocker::SegmentDoesNotStartOnAKeyframe {
                segment: 0,
                frame_nanos: SECOND + SECOND / 2,
                previous_keyframe_nanos: Some(SECOND),
            }]
        );
        let message = plan.blockers()[0].to_string();
        assert!(
            message.contains("0.500s earlier"),
            "the message has to say how much material a copy would have shown: {message}"
        );
    }

    #[test]
    fn a_cut_that_lands_between_frames_starts_on_the_next_one() {
        // Half a frame past a keyframe: the first frame at or after the cut is
        // the *next* one, which is not a keyframe, so this is a re-encode.
        // Snapping backwards to the keyframe at 1 s would be showing 50 ms the
        // user cut off.
        let document = EditDocument::from_recording(
            "Ace",
            RecordingId::new("rec-1"),
            span(SECOND + 50_000_000, 3 * SECOND),
        );

        let plan = plan(&document);

        assert_eq!(
            plan.segments()[0]
                .opening_frame
                .map(|frame| frame.presentation),
            Some(SourceTime::from_nanos(SECOND + 100_000_000)),
            "the first frame at or after the cut, never the one before it"
        );
        assert_eq!(plan.method(), ExportMethod::Reencode);
    }

    #[test]
    fn a_split_into_two_keyframe_aligned_pieces_is_still_a_copy() {
        // What #84's split and delete produce: two segments of one recording,
        // laid end to end, with the material between them gone.
        let source = SourceId::new(0);
        let document = EditDocument::new("Ace")
            .with_source(Source::new(source, RecordingId::new("rec-1")))
            .with_segment(Segment::new(source, span(0, 2 * SECOND)))
            .with_segment(Segment::new(source, span(5 * SECOND, 7 * SECOND)));

        let plan = plan(&document);

        assert_eq!(
            plan.method(),
            ExportMethod::StreamCopy,
            "{:?}",
            plan.blockers()
        );
        assert_eq!(plan.segments().len(), 2);
        assert_eq!(plan.segments()[0].output_start, OutputTime::ZERO);
        assert_eq!(
            plan.segments()[1].output_start,
            OutputTime::from_nanos(2 * SECOND),
            "the second segment starts where the first one ended, not where it does in the \
             recording"
        );
        assert_eq!(plan.output_nanos(), 4 * SECOND);
    }

    #[test]
    fn a_segment_maps_source_time_onto_the_output_timeline() {
        // The arithmetic the whole export is: material at 5.5 s of the
        // recording is at 2.5 s of the clip, because two seconds played before
        // it and the cut removed three.
        let source = SourceId::new(0);
        let document = EditDocument::new("Ace")
            .with_source(Source::new(source, RecordingId::new("rec-1")))
            .with_segment(Segment::new(source, span(0, 2 * SECOND)))
            .with_segment(Segment::new(source, span(5 * SECOND, 7 * SECOND)));

        let plan = plan(&document);
        let second = plan.segments()[1];

        assert_eq!(
            second.output_of(SourceTime::from_nanos(5 * SECOND + SECOND / 2)),
            Some(OutputTime::from_nanos(2 * SECOND + SECOND / 2))
        );
        assert_eq!(
            second.output_of(SourceTime::from_nanos(SECOND)),
            None,
            "material before the segment is material it does not play"
        );
    }

    #[test]
    fn a_speed_a_crop_or_a_rotation_is_a_re_encode() {
        let source = SourceId::new(0);
        for segment in [
            Segment::new(source, span(0, 2 * SECOND))
                .at_speed(Speed::new(2, 1).expect("a valid speed")),
            Segment::new(source, span(0, 2 * SECOND)).rotated(Rotation::Clockwise90),
            Segment::new(source, span(0, 2 * SECOND))
                .cropped_to(clipped_edit::CropRect::new(0.0, 0.0, 0.5, 1.0).expect("a valid crop")),
        ] {
            let document = EditDocument::new("Ace")
                .with_source(Source::new(source, RecordingId::new("rec-1")))
                .with_segment(segment);

            let plan = plan(&document);
            assert_eq!(plan.method(), ExportMethod::Reencode);
            assert_eq!(
                plan.blockers(),
                [CopyBlocker::SegmentTransformed { segment: 0 }]
            );
        }
    }

    #[test]
    fn text_over_the_picture_is_a_re_encode() {
        let document = copyable().with_overlay(TextOverlay::new(
            "Ace",
            clipped_edit::OutputSpan::new(OutputTime::ZERO, OutputTime::from_nanos(SECOND))
                .expect("a valid range"),
        ));

        let plan = plan(&document);
        assert_eq!(plan.blockers(), [CopyBlocker::Overlays { overlays: 1 }]);
    }

    #[test]
    fn a_clip_exported_at_a_different_shape_is_a_re_encode_and_the_same_shape_is_not() {
        // 1920x1080 is 16:9, so asking for 16:9 changes nothing and asking for
        // 9:16 changes every frame. Cross-multiplied, so 2560x1440 would answer
        // the same way.
        let widescreen = copyable().with_aspect_ratio(AspectRatio::WIDESCREEN);
        assert_eq!(plan(&widescreen).method(), ExportMethod::StreamCopy);

        let vertical = copyable().with_aspect_ratio(AspectRatio::VERTICAL);
        assert_eq!(
            plan(&vertical).blockers(),
            [CopyBlocker::AspectRatioDiffers {
                wanted: AspectRatio::VERTICAL,
                source_width: 1920,
                source_height: 1080,
            }]
        );
    }

    #[test]
    fn a_reordered_recording_is_never_copied() {
        // A copy cuts the tail of a segment in presentation order, which would
        // drop a picture a kept picture references.
        let reordered = SourceProfile::new(
            profile().streams().to_vec(),
            VideoFrameIndex::new(vec![
                IndexedFrame {
                    presentation: SourceTime::ZERO,
                    decode: SourceTime::ZERO,
                    keyframe: true,
                },
                IndexedFrame {
                    presentation: SourceTime::from_nanos(2 * SECOND),
                    decode: SourceTime::from_nanos(SECOND),
                    keyframe: false,
                },
            ]),
        );

        let plan = ExportPlan::of(
            &EditDocument::from_recording("Ace", RecordingId::new("rec-1"), span(0, 2 * SECOND)),
            &[(RecordingId::new("rec-1"), reordered)],
        )
        .expect("the plan can be made");

        assert!(plan.blockers().contains(&CopyBlocker::ReorderedStream));
    }

    #[test]
    fn an_edit_with_no_mix_carries_the_recordings_audio_as_it_was_recorded() {
        // An instant clip declares no audio tracks. Exporting it silent would
        // be a clip of a match with the game turned off.
        let plan = plan(&copyable());

        assert_eq!(
            plan.audio_tracks(),
            [
                PlannedAudioTrack {
                    name: Some("Compatibility Mix".to_owned()),
                    stream: 0
                },
                PlannedAudioTrack {
                    name: Some("Microphone".to_owned()),
                    stream: 1
                },
            ]
        );
        assert_eq!(plan.method(), ExportMethod::StreamCopy);
    }

    #[test]
    fn a_track_at_the_level_it_was_recorded_is_copied_and_anything_else_is_mixed() {
        let source = SourceId::new(0);
        let plain =
            copyable().with_audio_track(AudioTrack::new("Game", vec![TrackInput::new(source, 0)]));
        assert_eq!(plan(&plain).method(), ExportMethod::StreamCopy);
        assert_eq!(
            plan(&plain).audio_tracks(),
            [PlannedAudioTrack {
                name: Some("Game".to_owned()),
                stream: 0
            }]
        );

        let cases: [(AudioTrack, MixReason); 4] = [
            (
                AudioTrack::new("Game", vec![TrackInput::new(source, 0)]).at_gain_db(-6.0),
                MixReason::Level,
            ),
            (
                AudioTrack::new("Game", vec![TrackInput::new(source, 0)]).muted(),
                MixReason::Silenced,
            ),
            (
                AudioTrack::new("Game", vec![TrackInput::new(source, 0)])
                    .with_fades(Duration::from_millis(500), Duration::ZERO),
                MixReason::Fades,
            ),
            (
                AudioTrack::new(
                    "Game",
                    vec![TrackInput::new(source, 0), TrackInput::new(source, 1)],
                ),
                MixReason::SeveralInputs,
            ),
        ];

        for (track, reason) in cases {
            let document = copyable().with_audio_track(track);
            let plan = plan(&document);
            assert_eq!(
                plan.blockers(),
                [CopyBlocker::TrackNeedsMixing {
                    name: "Game".to_owned(),
                    reason,
                }],
                "{reason:?}"
            );
            assert!(
                plan.audio_tracks().is_empty(),
                "a track that has to be mixed is not a track that can be copied"
            );
        }
    }

    #[test]
    fn a_silenced_track_is_a_mix_rather_than_a_missing_track() {
        // A muted track is silent, and silence has to be produced. Dropping the
        // track instead would write a file with fewer tracks than the clip has,
        // which is not the same thing — so the track beside it is still planned
        // and only the silent one blocks the copy.
        //
        // Muting is the only way a track is silent in an export: soloing moved
        // out of the document in [issue
        // #85](https://github.com/wildware-uk/clipped/issues/85), so an export
        // is never handed one.
        let source = SourceId::new(0);
        let document = copyable()
            .with_audio_track(AudioTrack::new("Game", vec![TrackInput::new(source, 0)]))
            .with_audio_track(
                AudioTrack::new("Microphone", vec![TrackInput::new(source, 1)]).muted(),
            );

        let plan = plan(&document);

        assert_eq!(
            plan.blockers(),
            [CopyBlocker::TrackNeedsMixing {
                name: "Microphone".to_owned(),
                reason: MixReason::Silenced,
            }]
        );
        assert_eq!(
            plan.audio_tracks(),
            [PlannedAudioTrack {
                name: Some("Game".to_owned()),
                stream: 0
            }]
        );
    }

    #[test]
    fn joining_two_recordings_is_a_re_encode() {
        let document = EditDocument::new("Ace")
            .with_source(Source::new(SourceId::new(0), RecordingId::new("rec-1")))
            .with_source(Source::new(SourceId::new(1), RecordingId::new("rec-2")))
            .with_segment(Segment::new(SourceId::new(0), span(0, SECOND)))
            .with_segment(Segment::new(SourceId::new(1), span(0, SECOND)));

        let plan = ExportPlan::of(
            &document,
            &[
                (RecordingId::new("rec-1"), profile()),
                (RecordingId::new("rec-2"), profile()),
            ],
        )
        .expect("the plan can be made");

        assert_eq!(
            plan.blockers(),
            [CopyBlocker::SeveralRecordings { recordings: 2 }]
        );
    }

    #[test]
    fn a_recording_nobody_said_where_to_find_is_a_failure_rather_than_a_slow_export() {
        let error = ExportPlan::of(&copyable(), &[]).expect_err("the recording is not described");

        assert_eq!(
            error,
            PlanError::RecordingNotDescribed {
                recording: RecordingId::new("rec-1")
            }
        );
    }

    #[test]
    fn an_audio_track_naming_a_stream_the_recording_does_not_have_is_a_failure() {
        let document = copyable().with_audio_track(AudioTrack::new(
            "Discord",
            vec![TrackInput::new(SourceId::new(0), 7)],
        ));

        let error = ExportPlan::of(&document, &profiles()).expect_err("stream 7 is not there");

        assert_eq!(
            error,
            PlanError::AudioStreamMissing {
                track: "Discord".to_owned(),
                stream: 7,
                available: 2,
            }
        );
    }

    #[test]
    fn a_broken_document_is_refused_before_anything_is_planned() {
        let mut document = copyable();
        document.segments[0].speed = serde_json::from_str(r#"{"numerator":0,"denominator":1}"#)
            .expect("the shape is right even though the value is not");

        assert!(matches!(
            ExportPlan::of(&document, &profiles()),
            Err(PlanError::Document(_))
        ));
    }

    #[test]
    fn a_codec_the_container_writer_cannot_describe_is_a_re_encode() {
        let unknown = SourceProfile::new(
            vec![
                SourceStream::video(0, None, 1920, 1080, "vp9"),
                SourceStream::audio(1, None, 48_000, 2, "vorbis").with_name("Game"),
            ],
            frames(),
        );

        let plan = ExportPlan::of(&copyable(), &[(RecordingId::new("rec-1"), unknown)])
            .expect("the plan can be made");

        assert!(plan
            .blockers()
            .contains(&CopyBlocker::VideoCodecNotDescribable {
                codec: "vp9".to_owned()
            }));
    }

    #[test]
    fn a_segment_covering_no_pictures_is_named_rather_than_written_empty() {
        // Past the end of a ten-second recording.
        let document = EditDocument::from_recording(
            "Ace",
            RecordingId::new("rec-1"),
            span(20 * SECOND, 22 * SECOND),
        );

        let plan = plan(&document);

        assert_eq!(
            plan.blockers(),
            [CopyBlocker::SegmentHasNoFrames { segment: 0 }]
        );
        assert_eq!(plan.frames(), 0);
    }
}
