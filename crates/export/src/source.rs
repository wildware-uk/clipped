//! What an export needs to know about a recording before it decides anything.
//!
//! Deliberately plain data with public constructors, and no FFmpeg type in
//! sight. [`crate::media`] is what fills one of these in from a file; every
//! decision made from it is in [`crate::plan`], and is therefore testable
//! against a profile written out by hand rather than only against a recording
//! somebody has to produce first.
//!
//! # The two things a plan cannot answer without the file
//!
//! - **What the streams are.** The codec, the picture size, the sampling rate,
//!   and the out-of-band header a copy has to carry across.
//! - **Where the frames are.** A cut lands on a time; whether that time is a
//!   frame at all, and whether that frame is a keyframe, is a property of the
//!   bitstream. [`VideoFrameIndex`] is that answer, built by demuxing the file
//!   once without decoding anything.

use clipped_edit::SourceTime;
use clipped_muxer::{AudioCodec, FrameRate, VideoCodec};

/// One stream of a source recording, as far as an export cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStream {
    index: usize,
    format: StreamFormat,
    codec_name: String,
    extradata: Vec<u8>,
    name: Option<String>,
    language: Option<String>,
    default: bool,
}

/// What kind of stream it is, and the parts of its description a copy needs.
///
/// The codec is an [`Option`] because a recording may hold a codec this
/// workspace has no name for — a remuxed file, a recording made by something
/// else — and "I cannot describe this track to the container" is an answer an
/// export has to be able to give rather than a case it may assume away.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamFormat {
    /// A picture track.
    Video {
        /// The codec, where `clipped-muxer` can describe it.
        codec: Option<VideoCodec>,
        /// Picture width in pixels.
        width: u32,
        /// Picture height in pixels.
        height: u32,
        /// The nominal frame rate the container declares, where it declares one.
        frame_rate: Option<FrameRate>,
    },
    /// A sound track.
    Audio {
        /// The codec, where `clipped-muxer` can describe it.
        codec: Option<AudioCodec>,
        /// Sampling rate in hertz.
        sample_rate: u32,
        /// How many channels each frame has.
        channels: u16,
    },
    /// A subtitle, an attachment, a data stream: not part of the recording.
    Other,
}

impl SourceStream {
    /// Describes a picture track.
    #[must_use]
    pub fn video(
        index: usize,
        codec: Option<VideoCodec>,
        width: u32,
        height: u32,
        codec_name: impl Into<String>,
    ) -> Self {
        Self::new(
            index,
            StreamFormat::Video {
                codec,
                width,
                height,
                frame_rate: None,
            },
            codec_name,
        )
    }

    /// Describes a sound track.
    #[must_use]
    pub fn audio(
        index: usize,
        codec: Option<AudioCodec>,
        sample_rate: u32,
        channels: u16,
        codec_name: impl Into<String>,
    ) -> Self {
        Self::new(
            index,
            StreamFormat::Audio {
                codec,
                sample_rate,
                channels,
            },
            codec_name,
        )
    }

    /// Describes a stream of any kind.
    #[must_use]
    pub fn new(index: usize, format: StreamFormat, codec_name: impl Into<String>) -> Self {
        Self {
            index,
            format,
            codec_name: codec_name.into(),
            extradata: Vec::new(),
            name: None,
            language: None,
            default: false,
        }
    }

    /// The same stream carrying the codec's out-of-band header.
    #[must_use]
    pub fn with_extradata(mut self, extradata: impl Into<Vec<u8>>) -> Self {
        self.extradata = extradata.into();
        self
    }

    /// The same stream with the name the container gave it.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The same stream with the language tag the container gave it.
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// The same stream marked as the one a player selects on its own.
    #[must_use]
    pub const fn with_default_flag(mut self, default: bool) -> Self {
        self.default = default;
        self
    }

    /// Which stream of the container this is, counting from zero.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// What kind of stream it is.
    #[must_use]
    pub const fn format(&self) -> &StreamFormat {
        &self.format
    }

    /// The codec, as FFmpeg names it: `h264`, `opus`, `pcm_s16le`.
    ///
    /// Kept as text as well as in [`StreamFormat`] because a stream this
    /// workspace has no enumeration for still has to be nameable in the
    /// sentence that explains why it could not be copied (AGENTS.md section 15).
    #[must_use]
    pub fn codec_name(&self) -> &str {
        &self.codec_name
    }

    /// The codec's out-of-band header, empty when there is none.
    #[must_use]
    pub fn extradata(&self) -> &[u8] {
        &self.extradata
    }

    /// The track's name, where the container gave it one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The track's language tag, where the container gave it one.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Whether a player should select this track on its own.
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.default
    }

    /// Whether this is a picture track.
    #[must_use]
    pub const fn is_video(&self) -> bool {
        matches!(self.format, StreamFormat::Video { .. })
    }

    /// Whether this is a sound track.
    #[must_use]
    pub const fn is_audio(&self) -> bool {
        matches!(self.format, StreamFormat::Audio { .. })
    }
}

/// One coded picture, as the container stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexedFrame {
    /// When it is presented, on the recording's own timeline.
    pub presentation: SourceTime,
    /// When it is decoded, on the same timeline.
    ///
    /// Equal to [`presentation`](Self::presentation) for a stream that does not
    /// reorder, which is every stream Clipped's own encoders produce.
    pub decode: SourceTime,
    /// Whether a decoder can start here.
    pub keyframe: bool,
}

/// Every coded picture of a recording's video track, in decode order.
///
/// Built by demuxing the file once and decoding nothing, which is why it is
/// affordable: a two-hour recording is a few hundred thousand of these and one
/// pass over the container's index.
///
/// It exists because a cut is a *time* and a copy is a decision about *frames*.
/// "Which frame does this segment start on, and can a decoder start there?" has
/// no answer that does not come from the bitstream, and both halves of the
/// export — the plan and the write — have to get the same one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoFrameIndex {
    frames: Vec<IndexedFrame>,
}

impl VideoFrameIndex {
    /// An index over `frames`, which must be in decode order.
    #[must_use]
    pub fn new(frames: Vec<IndexedFrame>) -> Self {
        Self { frames }
    }

    /// The pictures, in decode order.
    #[must_use]
    pub fn frames(&self) -> &[IndexedFrame] {
        &self.frames
    }

    /// How many pictures the recording holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the recording holds no pictures at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The first picture presented at or after `at`.
    ///
    /// The rule `docs/editing.md` fixes: ranges are half-open, so a segment's
    /// first exported frame is the first at or after its start. Searched over
    /// presentation times rather than the vector's order, because a stream that
    /// reorders is stored in decode order and the first frame *shown* after a
    /// cut need not be the first one stored after it.
    #[must_use]
    pub fn first_at_or_after(&self, at: SourceTime) -> Option<IndexedFrame> {
        self.frames
            .iter()
            .filter(|frame| frame.presentation >= at)
            .min_by_key(|frame| frame.presentation)
            .copied()
    }

    /// The pictures a half-open source range covers, in decode order.
    ///
    /// The other half of the same rule: everything presented at or after the
    /// start and strictly before the end. A frame therefore belongs to exactly
    /// one side of a cut — none is duplicated at a join and none is dropped.
    pub fn frames_in(
        &self,
        start: SourceTime,
        end: SourceTime,
    ) -> impl Iterator<Item = IndexedFrame> + '_ {
        self.frames
            .iter()
            .filter(move |frame| frame.presentation >= start && frame.presentation < end)
            .copied()
    }

    /// Whether any picture is decoded in a different order from the one it is
    /// shown in.
    ///
    /// A copy cuts a segment's tail in presentation order, so a reordered
    /// stream could lose a picture another kept picture references. Clipped's
    /// own recordings do not reorder; one that does is refused rather than
    /// copied, and [`crate::plan`] says so in as many words.
    #[must_use]
    pub fn is_reordered(&self) -> bool {
        self.frames
            .iter()
            .any(|frame| frame.decode != frame.presentation)
    }
}

/// Everything one recording contributes to an export decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceProfile {
    streams: Vec<SourceStream>,
    frames: VideoFrameIndex,
}

impl SourceProfile {
    /// A profile over `streams`, whose video track is described by `frames`.
    #[must_use]
    pub fn new(streams: Vec<SourceStream>, frames: VideoFrameIndex) -> Self {
        Self { streams, frames }
    }

    /// Every stream the container declares, in its own order.
    #[must_use]
    pub fn streams(&self) -> &[SourceStream] {
        &self.streams
    }

    /// The picture track, or [`None`] for a file that has none.
    #[must_use]
    pub fn video(&self) -> Option<&SourceStream> {
        self.streams.iter().find(|stream| stream.is_video())
    }

    /// The sound tracks, in the order the container declares them.
    #[must_use]
    pub fn audio(&self) -> Vec<&SourceStream> {
        self.streams
            .iter()
            .filter(|stream| stream.is_audio())
            .collect()
    }

    /// The recording's *n*th sound track, numbered as an edit document numbers
    /// them: 0 for the first audio stream, not for the video one.
    #[must_use]
    pub fn audio_stream(&self, stream: u16) -> Option<&SourceStream> {
        self.audio().into_iter().nth(usize::from(stream))
    }

    /// Where the pictures are.
    #[must_use]
    pub const fn frames(&self) -> &VideoFrameIndex {
        &self.frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(presentation_nanos: u64, keyframe: bool) -> IndexedFrame {
        IndexedFrame {
            presentation: SourceTime::from_nanos(presentation_nanos),
            decode: SourceTime::from_nanos(presentation_nanos),
            keyframe,
        }
    }

    /// Ten frames 100 ms apart, a keyframe every fifth.
    fn index() -> VideoFrameIndex {
        VideoFrameIndex::new(
            (0..10)
                .map(|number| frame(number * 100_000_000, number % 5 == 0))
                .collect(),
        )
    }

    #[test]
    fn the_frame_a_cut_starts_on_is_the_first_one_at_or_after_it() {
        let index = index();

        // Exactly on a frame: that frame, not the next one.
        assert_eq!(
            index.first_at_or_after(SourceTime::from_nanos(500_000_000)),
            Some(frame(500_000_000, true))
        );
        // A nanosecond later, and the frame has already been shown.
        assert_eq!(
            index.first_at_or_after(SourceTime::from_nanos(500_000_001)),
            Some(frame(600_000_000, false))
        );
        assert_eq!(
            index.first_at_or_after(SourceTime::from_nanos(900_000_001)),
            None,
            "past the last frame there is nothing to start on"
        );
    }

    #[test]
    fn a_range_takes_its_start_and_leaves_its_end_to_whatever_comes_next() {
        // The half-open rule, which is the whole of "no frame is duplicated at
        // a join and none is dropped". The frame at 500 ms belongs to the
        // second range and to nothing else.
        let index = index();

        let first: Vec<u64> = index
            .frames_in(SourceTime::ZERO, SourceTime::from_nanos(500_000_000))
            .map(|frame| frame.presentation.as_nanos())
            .collect();
        let second: Vec<u64> = index
            .frames_in(
                SourceTime::from_nanos(500_000_000),
                SourceTime::from_nanos(1_000_000_000),
            )
            .map(|frame| frame.presentation.as_nanos())
            .collect();

        assert_eq!(
            first,
            vec![0, 100_000_000, 200_000_000, 300_000_000, 400_000_000]
        );
        assert_eq!(
            second,
            vec![
                500_000_000,
                600_000_000,
                700_000_000,
                800_000_000,
                900_000_000
            ]
        );
        assert_eq!(
            first.len() + second.len(),
            index.len(),
            "every frame is on exactly one side of the cut"
        );
    }

    #[test]
    fn a_stream_is_reordered_only_when_a_picture_is_decoded_out_of_order() {
        assert!(!index().is_reordered());

        let reordered = VideoFrameIndex::new(vec![
            frame(0, true),
            IndexedFrame {
                presentation: SourceTime::from_nanos(200_000_000),
                decode: SourceTime::from_nanos(100_000_000),
                keyframe: false,
            },
        ]);
        assert!(reordered.is_reordered());
    }

    #[test]
    fn audio_streams_are_numbered_as_a_document_numbers_them() {
        // An edit document's `TrackInput::stream` is "0 for the first audio
        // track, not for the video one", and the container's own indices are
        // not that. Getting this wrong exports the game audio under the
        // microphone's slider.
        let profile = SourceProfile::new(
            vec![
                SourceStream::video(0, Some(VideoCodec::H264), 1920, 1080, "h264"),
                SourceStream::audio(1, Some(AudioCodec::PcmS16Le), 48_000, 2, "pcm_s16le")
                    .with_name("Compatibility Mix"),
                SourceStream::audio(2, Some(AudioCodec::PcmS16Le), 48_000, 1, "pcm_s16le")
                    .with_name("Microphone"),
            ],
            VideoFrameIndex::default(),
        );

        assert_eq!(
            profile.audio_stream(0).and_then(SourceStream::name),
            Some("Compatibility Mix")
        );
        assert_eq!(
            profile.audio_stream(1).and_then(SourceStream::name),
            Some("Microphone")
        );
        assert_eq!(profile.audio_stream(2), None);
        assert_eq!(profile.video().map(SourceStream::index), Some(0));
    }
}
