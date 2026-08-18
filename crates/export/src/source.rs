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
//!
//! # And the one it cannot answer without the file either: how big
//!
//! The same pass reads how many bytes the container stores for each packet, in
//! [`IndexedFrame::bytes`] and in [`AudioPacketIndex`]. A stream copy writes the
//! recording's own packets, so the size of the export is the size of the packets
//! it takes plus what the container costs to write round them — which makes an
//! estimate an arithmetic sum over an index already being built rather than
//! anything anybody has to guess at ([`crate::plan::EstimatedSize`]).
//!
//! It is not free in memory: a sound track is tens of packets a second, so an
//! hour of one is a megabyte or so of index beside the pictures'. That is the
//! same order as the picture index the plan already needs and it lives only for
//! as long as the plan is being made.

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
    /// How many bytes of coded picture the container stores for it.
    ///
    /// A copy writes exactly these bytes, so summing them over the pictures a
    /// segment takes is the size of that segment's video — measured rather than
    /// worked out from a bitrate.
    pub bytes: u32,
}

/// One coded packet of a recording's sound track, as the container stores it.
///
/// The sound half of the same answer: [`IndexedFrame`] carries the picture
/// sizes, and this carries a sound track's. There is no keyframe here because
/// every audio packet is one — sound is cut on packet boundaries and a decoder
/// can start at any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacket {
    /// When it is presented, on the recording's own timeline.
    pub presentation: SourceTime,
    /// How many bytes of coded sound the container stores for it.
    pub bytes: u32,
}

/// Every coded packet of one of a recording's sound tracks, in the order they
/// are stored.
///
/// Built by the same pass that builds [`VideoFrameIndex`], and for the same
/// reason: how many bytes a copy of a range would write is a property of the
/// bitstream, and a variable-bitrate track's answer cannot be worked out from
/// its duration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioPacketIndex {
    packets: Vec<AudioPacket>,
}

impl AudioPacketIndex {
    /// An index over `packets`, which must be in the order they are stored.
    #[must_use]
    pub fn new(packets: Vec<AudioPacket>) -> Self {
        Self { packets }
    }

    /// The packets, in the order they are stored.
    #[must_use]
    pub fn packets(&self) -> &[AudioPacket] {
        &self.packets
    }

    /// How many packets the track holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    /// Whether the track holds no packets at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    /// The packets a half-open source range covers.
    ///
    /// The same rule as [`VideoFrameIndex::frames_in`] and the same rule the
    /// writing loop applies to sound: presented at or after `start` and strictly
    /// before `end`. That is what makes the sum below the bytes a copy of that
    /// range writes rather than an approximation of them.
    pub fn packets_in(
        &self,
        start: SourceTime,
        end: SourceTime,
    ) -> impl Iterator<Item = AudioPacket> + '_ {
        self.packets
            .iter()
            .filter(move |packet| packet.presentation >= start && packet.presentation < end)
            .copied()
    }
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
    audio_packets: Option<Vec<AudioPacketIndex>>,
}

impl SourceProfile {
    /// A profile over `streams`, whose video track is described by `frames`.
    ///
    /// The sound tracks' packets are **not** described by this: a profile made
    /// this way says nothing about how large they are, and a plan over it
    /// answers that it does not know rather than answering zero. Add them with
    /// [`with_audio_packets`](Self::with_audio_packets).
    #[must_use]
    pub fn new(streams: Vec<SourceStream>, frames: VideoFrameIndex) -> Self {
        Self {
            streams,
            frames,
            audio_packets: None,
        }
    }

    /// The same profile, with one packet index per sound track, in the order
    /// [`audio`](Self::audio) returns them.
    #[must_use]
    pub fn with_audio_packets(mut self, audio_packets: Vec<AudioPacketIndex>) -> Self {
        self.audio_packets = Some(audio_packets);
        self
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

    /// Where each sound track's packets are, when they were measured.
    ///
    /// [`None`] for a profile written out by hand rather than read from a file.
    /// Deliberately distinguished from an empty list, which is a recording with
    /// no sound at all: "nothing measured this" and "there is none" are
    /// different answers and an estimate must not confuse them (AGENTS.md
    /// section 54).
    #[must_use]
    pub fn audio_packets(&self) -> Option<&[AudioPacketIndex]> {
        self.audio_packets.as_deref()
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
            bytes: if keyframe { 4_000 } else { 500 },
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
                bytes: 500,
            },
        ]);
        assert!(reordered.is_reordered());
    }

    #[test]
    fn a_sound_tracks_packets_are_taken_by_the_same_half_open_rule_as_the_pictures() {
        // The sizes are what an estimate sums, so which packets a range covers
        // decides the answer. The packet at 200 ms belongs to the second range
        // and to nothing else — the same rule the writing loop applies, which is
        // what makes the sum the bytes a copy would write.
        let index = AudioPacketIndex::new(
            (0..10)
                .map(|number| AudioPacket {
                    presentation: SourceTime::from_nanos(number * 100_000_000),
                    bytes: 960,
                })
                .collect(),
        );

        let first: u64 = index
            .packets_in(SourceTime::ZERO, SourceTime::from_nanos(200_000_000))
            .map(|packet| u64::from(packet.bytes))
            .sum();
        let second: u64 = index
            .packets_in(
                SourceTime::from_nanos(200_000_000),
                SourceTime::from_nanos(400_000_000),
            )
            .map(|packet| u64::from(packet.bytes))
            .sum();

        assert_eq!(first, 2 * 960);
        assert_eq!(second, 2 * 960);
        assert_eq!(index.len(), 10);
        assert!(!index.is_empty());
    }

    #[test]
    fn a_profile_written_by_hand_says_nothing_about_its_sound_rather_than_saying_zero() {
        // "Nothing measured this" and "there is none" are different answers, and
        // a size estimate over the second would be a figure with no basis.
        let profile = SourceProfile::new(Vec::new(), VideoFrameIndex::default());
        assert_eq!(profile.audio_packets(), None);

        let measured = profile.with_audio_packets(vec![AudioPacketIndex::default()]);
        assert_eq!(
            measured.audio_packets().map(<[_]>::len),
            Some(1),
            "a recording whose one sound track holds no packets is not a recording nobody looked \
             at"
        );
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
