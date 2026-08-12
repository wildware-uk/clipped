//! The tracks a recording is made of.
//!
//! A Clipped recording is one video track and *several* audio tracks — game,
//! other system audio, microphone, per-application sources, plus the
//! compatibility mix (SPEC.md section 11 and section 13). How many there are is
//! decided at run time from the user's routing rather than fixed at three
//! (SPEC.md section 44), so the layout here is a video track and a list, and
//! nothing in it assumes the list has one entry.
//!
//! The container is where a track's identity has to survive: Matroska carries a
//! name and a language tag per track, so an editor opening the file sees
//! "Microphone" rather than "Audio 3" without a sidecar (ADR 0001). Which
//! sources those tracks carry, what each is called and what order they come in
//! is [`AudioSource`](crate::AudioSource) in [`crate::audio`], and
//! [`AudioTrack::for_source`] is how a track is described from one.
//!
//! # Why the codecs are named here rather than taken from `clipped-encoder`
//!
//! `clipped-encoder` has a [`Codec`](https://docs.rs/) enumeration of its own,
//! and this crate sits above it, so it could have been reused. It is not,
//! for the reason `clipped-encoder`'s own `Resolution` gives for not sharing
//! `clipped-capture`'s `FrameSize`: the two answer different questions. That
//! one is *what this machine can be asked to produce*; this one is *what this
//! container can be asked to carry*, which includes audio codecs no encoder in
//! Clipped will ever produce and has to keep meaning something when the muxer
//! is remuxing a file it did not record
//! ([issue #92](https://github.com/wildware-uk/clipped/issues/92)).
//! `clipped-session`, which owns both sides, converts.

use core::fmt;
use core::time::Duration;

use rusty_ffmpeg::ffi;

use crate::audio::{AudioSource, RECORDING_AUDIO_CODEC};

/// Which track of a recording a packet belongs to.
///
/// Video is singular and audio is not, which is the shape of the product
/// rather than a limitation of the container: Clipped records one picture and
/// as many independent audio tracks as the routing asks for (SPEC.md section
/// 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrackId {
    /// The video track.
    Video,
    /// An audio track, numbered from zero in the order the layout declares
    /// them.
    Audio(u16),
}

impl fmt::Display for TrackId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Video => formatter.write_str("video track"),
            Self::Audio(index) => write!(formatter, "audio track {index}"),
        }
    }
}

/// A video codec a recording can be written in (SPEC.md section 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum VideoCodec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
    /// AV1.
    Av1,
}

impl VideoCodec {
    /// FFmpeg's identifier for this codec.
    pub(crate) const fn av_codec_id(self) -> ffi::AVCodecID {
        match self {
            Self::H264 => ffi::AV_CODEC_ID_H264,
            Self::Hevc => ffi::AV_CODEC_ID_HEVC,
            Self::Av1 => ffi::AV_CODEC_ID_AV1,
        }
    }

    /// The name FFmpeg knows this codec by, as `ffprobe` prints it in
    /// `codec_name`.
    ///
    /// Worth having in the public API because it is what a test asserting on
    /// generated media compares against (AGENTS.md section 22), and a
    /// hand-copied string in the test would be a second place to get it wrong.
    #[must_use]
    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }
}

impl fmt::Display for VideoCodec {
    /// The label shown to a user, as in `Codec: H.264`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Av1 => "AV1",
        })
    }
}

/// An audio codec a recording can be written in.
///
/// Clipped records in
/// [`RECORDING_AUDIO_CODEC`](crate::RECORDING_AUDIO_CODEC), which is
/// uncompressed PCM; `docs/muxing.md` records that decision and what it costs.
/// The others are carried because a remux copies whatever the file it was handed
/// contains ([issue #92](https://github.com/wildware-uk/clipped/issues/92)), and
/// because an audio encoder is work this workspace has not done yet. Matroska
/// imposes no constraint of its own here, which is part of why it was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AudioCodec {
    /// Uncompressed 16-bit little-endian PCM.
    ///
    /// The audio a Windows loopback capture produces before anything is done
    /// to it, and the only codec in this list whose packets need no encoder,
    /// which is what makes it the one the muxer's own tests are written
    /// against.
    PcmS16Le,
    /// AAC-LC.
    Aac,
    /// Opus.
    Opus,
}

impl AudioCodec {
    /// FFmpeg's identifier for this codec.
    pub(crate) const fn av_codec_id(self) -> ffi::AVCodecID {
        match self {
            Self::PcmS16Le => ffi::AV_CODEC_ID_PCM_S16LE,
            Self::Aac => ffi::AV_CODEC_ID_AAC,
            Self::Opus => ffi::AV_CODEC_ID_OPUS,
        }
    }

    /// The name FFmpeg knows this codec by, as `ffprobe` prints it in
    /// `codec_name`.
    #[must_use]
    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::PcmS16Le => "pcm_s16le",
            Self::Aac => "aac",
            Self::Opus => "opus",
        }
    }

    /// Whether Matroska needs this codec's out-of-band header in the track
    /// entry.
    ///
    /// Opus stores its identification header and AAC its `AudioSpecificConfig`
    /// in `CodecPrivate`, and a track without one is undecodable. Uncompressed
    /// PCM has nothing to store: the sampling rate, channel count and bit depth
    /// in the track entry are the whole description.
    pub(crate) const fn requires_codec_private(self) -> bool {
        match self {
            Self::PcmS16Le => false,
            Self::Aac | Self::Opus => true,
        }
    }

    /// Bits in one sample of one channel, where that is fixed by the codec.
    ///
    /// Only uncompressed formats have one. Matroska records it as `BitDepth`
    /// for a PCM track, and a player that does not find it has to guess how to
    /// interpret the bytes.
    #[must_use]
    pub const fn bits_per_sample(self) -> Option<u16> {
        match self {
            Self::PcmS16Le => Some(16),
            Self::Aac | Self::Opus => None,
        }
    }
}

impl fmt::Display for AudioCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PcmS16Le => "PCM (16-bit)",
            Self::Aac => "AAC",
            Self::Opus => "Opus",
        })
    }
}

/// A language tag, as Matroska stores one: three lowercase ASCII letters.
///
/// Matroska's `Language` element is ISO 639-2, and FFmpeg writes whatever
/// string it is handed straight into it. A tag that is not three letters is
/// therefore not a validation nicety — it is a malformed element in a file
/// nobody can rewrite afterwards — so the type refuses to hold one.
///
/// [`UNDETERMINED`](Self::UNDETERMINED) is the default, and it is what
/// Matroska specifies for a track whose language is genuinely not known. Game
/// audio has no language; a microphone track has one only if somebody says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Language([u8; 3]);

impl Language {
    /// `und`, Matroska's tag for a track with no language.
    pub const UNDETERMINED: Self = Self(*b"und");
    /// `eng`.
    pub const ENGLISH: Self = Self(*b"eng");

    /// Takes an ISO 639-2 tag.
    ///
    /// # Errors
    ///
    /// [`InvalidLanguage`] when `tag` is not exactly three lowercase ASCII
    /// letters. The rejection describes the shape of the problem rather than
    /// repeating the value, which is the habit `clipped-logging` sets for
    /// anything that might carry user text (docs/logging.md).
    pub fn new(tag: &str) -> Result<Self, InvalidLanguage> {
        let bytes = tag.as_bytes();
        let Ok(code) = <[u8; 3]>::try_from(bytes) else {
            return Err(InvalidLanguage::Length { found: tag.len() });
        };

        match code.iter().position(|byte| !byte.is_ascii_lowercase()) {
            Some(index) => Err(InvalidLanguage::Character { index }),
            None => Ok(Self(code)),
        }
    }

    /// The tag, as it is written into the container.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // The constructors accept nothing but ASCII letters, so this cannot
        // fail; `from_utf8` rather than the unchecked form because the check is
        // three bytes and this is not a hot path.
        core::str::from_utf8(&self.0).unwrap_or("und")
    }
}

impl Default for Language {
    fn default() -> Self {
        Self::UNDETERMINED
    }
}

impl fmt::Display for Language {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a string was not accepted as a [`Language`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidLanguage {
    /// The tag was not three bytes long.
    Length {
        /// How long it was, in bytes.
        found: usize,
    },
    /// A byte was not a lowercase ASCII letter.
    Character {
        /// Which byte, counting from zero.
        index: usize,
    },
}

impl fmt::Display for InvalidLanguage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { found } => write!(
                formatter,
                "a language tag is three lowercase ASCII letters, such as `eng`; this one \
                 is {found} bytes"
            ),
            Self::Character { index } => write!(
                formatter,
                "a language tag is three lowercase ASCII letters, such as `eng`; byte \
                 {index} of this one is not one"
            ),
        }
    }
}

impl std::error::Error for InvalidLanguage {}

/// A nominal frame rate, as a fraction.
///
/// Game capture is variable-rate — a frame exists when the compositor produced
/// one — so this is what the encoder was *configured* for rather than a promise
/// about the timestamps. It is written to the container because players and
/// editors read it to label a clip and to choose a timeline rate, and a file
/// without it makes them infer one from the first few frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRate {
    numerator: u32,
    denominator: u32,
}

impl FrameRate {
    /// Builds a frame rate from a fraction, as `FrameRate::new(60000, 1001)`
    /// for 59.94 fps.
    ///
    /// Returns [`None`] when either part is zero, which no frame rate is.
    #[must_use]
    pub const fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if numerator == 0 || denominator == 0 {
            return None;
        }
        Some(Self {
            numerator,
            denominator,
        })
    }

    /// A whole number of frames per second.
    ///
    /// Returns [`None`] for zero.
    #[must_use]
    pub const fn per_second(frames: u32) -> Option<Self> {
        Self::new(frames, 1)
    }

    /// How long one frame occupies at this rate.
    ///
    /// The number a caller needs for
    /// [`EncodedPacket::with_duration`](crate::EncodedPacket::with_duration),
    /// which decides the duration of the *last* block of a track and so whether
    /// a finished file reads as ending one frame early. Exposed rather than left
    /// to each caller to divide out, because a rate is a fraction — 60000/1001
    /// is not 59 frames a second — and a second arithmetic for it is a second
    /// place for that to go wrong (AGENTS.md section 55).
    ///
    /// Rounded down to the nanosecond, which is the resolution every timestamp
    /// in this crate is carried at.
    #[must_use]
    pub const fn frame_interval(self) -> Duration {
        Duration::from_nanos(1_000_000_000 * self.denominator as u64 / self.numerator as u64)
    }

    /// The fraction, as FFmpeg's rational.
    pub(crate) const fn as_rational(self) -> ffi::AVRational {
        ffi::AVRational {
            num: self.numerator as i32,
            den: self.denominator as i32,
        }
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.numerator, self.denominator)
    }
}

/// The video track of a recording.
#[derive(Debug, Clone)]
pub struct VideoTrack {
    codec: VideoCodec,
    width: u32,
    height: u32,
    frame_rate: Option<FrameRate>,
    codec_private: Vec<u8>,
    name: Option<String>,
    language: Language,
}

impl VideoTrack {
    /// Describes a video track of `width` by `height` pixels in `codec`.
    #[must_use]
    pub const fn new(codec: VideoCodec, width: u32, height: u32) -> Self {
        Self {
            codec,
            width,
            height,
            frame_rate: None,
            codec_private: Vec::new(),
            name: None,
            language: Language::UNDETERMINED,
        }
    }

    /// Attaches the codec's out-of-band header — H.264's SPS and PPS, HEVC's
    /// VPS/SPS/PPS, AV1's sequence header.
    ///
    /// Every hardware encoder can be asked for these separately from the
    /// bitstream, and Matroska stores them in `CodecPrivate` so that a
    /// recording can be decoded from any keyframe rather than only from the
    /// first one. A track without them is playable only if the encoder repeats
    /// its headers in-band, which not all of them do.
    #[must_use]
    pub fn with_codec_private(mut self, header: impl Into<Vec<u8>>) -> Self {
        self.codec_private = header.into();
        self
    }

    /// Sets the nominal frame rate. See [`FrameRate`].
    #[must_use]
    pub const fn with_frame_rate(mut self, frame_rate: FrameRate) -> Self {
        self.frame_rate = Some(frame_rate);
        self
    }

    /// Names the track, as an editor will show it.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Tags the track with a language.
    #[must_use]
    pub const fn with_language(mut self, language: Language) -> Self {
        self.language = language;
        self
    }

    /// The codec this track carries.
    #[must_use]
    pub const fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// Picture width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Picture height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The nominal frame rate, if one was given.
    #[must_use]
    pub const fn frame_rate(&self) -> Option<FrameRate> {
        self.frame_rate
    }

    /// The codec's out-of-band header, empty if none was given.
    #[must_use]
    pub fn codec_private(&self) -> &[u8] {
        &self.codec_private
    }

    /// The track's name, if it was given one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The track's language tag.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }
}

/// One audio track of a recording.
///
/// The name is the point. A recording with four audio tracks and no names is a
/// recording whose tracks have to be identified by listening to them, and
/// SPEC.md section 11 is explicit that the tracks are meant to arrive in an
/// editor already labelled.
#[derive(Debug, Clone)]
pub struct AudioTrack {
    codec: AudioCodec,
    sample_rate: u32,
    channels: u16,
    codec_private: Vec<u8>,
    name: Option<String>,
    language: Language,
    default: bool,
    source: Option<AudioSource>,
}

impl AudioTrack {
    /// Describes an audio track in `codec` at `sample_rate` hertz with
    /// `channels` channels.
    ///
    /// The track has no source, so it is named by the caller and keeps the
    /// position it was declared in. That is what a remux wants — it copies a
    /// file it did not record — and what a recording of Clipped's own sources
    /// does not: use [`for_source`](Self::for_source) for those.
    #[must_use]
    pub const fn new(codec: AudioCodec, sample_rate: u32, channels: u16) -> Self {
        Self {
            codec,
            sample_rate,
            channels,
            codec_private: Vec::new(),
            name: None,
            language: Language::UNDETERMINED,
            default: false,
            source: None,
        }
    }

    /// Describes the track one of a recording's audio sources goes to.
    ///
    /// Everything about the track that is not the format comes from the source:
    /// the name a player and an editor show, where the track sits among the
    /// others, and whether it is the one a player selects on its own. That is
    /// the point — a recording made on any machine with any routing labels and
    /// orders its tracks the same way (SPEC.md sections 11 and 13), and no
    /// caller has to remember that the compatibility mix goes first and carries
    /// the flag.
    ///
    /// The format is the caller's, because it is the capture's: a microphone is
    /// often mono where system audio is stereo, and resampling to make them
    /// match is a decision this crate is not entitled to take (AGENTS.md section
    /// 21).
    ///
    /// The language is left as [`Language::UNDETERMINED`], which is what
    /// Matroska specifies for a track whose language is not known. Game audio
    /// has none; a microphone's is a fact about the person speaking, and
    /// guessing it from the operating system's locale would put a wrong tag in a
    /// file nobody can rewrite. A caller that *knows* says so with
    /// [`with_language`](Self::with_language).
    #[must_use]
    pub fn for_source(source: AudioSource, sample_rate: u32, channels: u16) -> Self {
        let default = source.is_default_track();
        Self {
            name: Some(source.track_name().to_owned()),
            default,
            source: Some(source),
            ..Self::new(RECORDING_AUDIO_CODEC, sample_rate, channels)
        }
    }

    /// Says which of a recording's sources this track carries.
    ///
    /// For a track described by hand that is nonetheless one of the model's:
    /// [`for_source`](Self::for_source) is the usual way in, and does this as
    /// well as naming and ordering the track.
    #[must_use]
    pub fn with_source(mut self, source: AudioSource) -> Self {
        self.source = Some(source);
        self
    }

    /// What this track carries, when it was described from a source.
    #[must_use]
    pub const fn source(&self) -> Option<&AudioSource> {
        self.source.as_ref()
    }

    /// Where this track sits among a recording's audio tracks.
    ///
    /// A track with no source ranks last, so a layout built by hand keeps the
    /// order it was declared in.
    pub(crate) fn ordering_rank(&self) -> u8 {
        match &self.source {
            Some(source) => source.ordering_rank(),
            None => u8::MAX,
        }
    }

    /// Attaches the codec's out-of-band header, such as Opus' identification
    /// header or AAC's `AudioSpecificConfig`.
    ///
    /// PCM has none.
    #[must_use]
    pub fn with_codec_private(mut self, header: impl Into<Vec<u8>>) -> Self {
        self.codec_private = header.into();
        self
    }

    /// Names the track, as an editor will show it: `Game`, `Microphone`.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Tags the track with a language.
    #[must_use]
    pub const fn with_language(mut self, language: Language) -> Self {
        self.language = language;
        self
    }

    /// Marks the track as the one a player should select on its own.
    ///
    /// This is Matroska's `FlagDefault`. It matters because a recording with
    /// several audio tracks opened in a player that picks the first one is a
    /// recording that sounds wrong: the compatibility mix (SPEC.md section 13)
    /// is the track that should play, and it is not necessarily the first one
    /// declared.
    #[must_use]
    pub const fn as_default(self) -> Self {
        self.with_default_flag(true)
    }

    /// Sets whether a player should select this track on its own.
    ///
    /// [`for_source`](Self::for_source) already answers this from the track
    /// model, and a recording has no reason to disagree with it. The setter
    /// exists because the flag is only *testable* on a file whose default track
    /// is not the first one — FFmpeg's MP4 muxer enables the first track of each
    /// kind whether or not it was told to, so a recording that marks the first
    /// track produces an identical copy with the flag honoured and with it
    /// dropped (`crates/muxer/tests/mp4_remux.rs`) — and something has to be
    /// able to write that file.
    #[must_use]
    pub const fn with_default_flag(mut self, default: bool) -> Self {
        self.default = default;
        self
    }

    /// The codec this track carries.
    #[must_use]
    pub const fn codec(&self) -> AudioCodec {
        self.codec
    }

    /// Sampling rate in hertz.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// How many channels each frame has.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// The codec's out-of-band header, empty if none was given.
    #[must_use]
    pub fn codec_private(&self) -> &[u8] {
        &self.codec_private
    }

    /// The track's name, if it was given one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The track's language tag.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// Whether the track carries Matroska's `FlagDefault`.
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.default
    }
}

/// Every track a recording will contain, fixed before the file is created.
///
/// It has to be fixed: Matroska writes its track entries into the header, so a
/// track that appears halfway through a session cannot be added to the file
/// that is already open. A routing change mid-recording is therefore a new
/// file, not a new track, and that is a session-level decision rather than a
/// muxer one.
#[derive(Debug, Clone)]
pub struct RecordingLayout {
    video: VideoTrack,
    audio: Vec<AudioTrack>,
}

impl RecordingLayout {
    /// Starts a layout with its video track.
    ///
    /// Video is not optional, because a Clipped recording is a recording of a
    /// game being played.
    #[must_use]
    pub const fn new(video: VideoTrack) -> Self {
        Self {
            video,
            audio: Vec::new(),
        }
    }

    /// Adds an audio track, in the position the track model gives it.
    ///
    /// Tracks with a source are ordered by
    /// [`AudioSource::ordering_rank`](crate::AudioSource::ordering_rank) — the
    /// compatibility mix first — whatever order they are added in, and tracks
    /// with the same rank keep the order they were added in. A track with no
    /// source ranks last, so a layout described entirely by hand is in
    /// declaration order and nothing about a remuxed file moves.
    ///
    /// The ordering is here rather than in the caller because it is the file
    /// that has to be predictable: a container fixes its track list in the
    /// header, an editor project refers to tracks by index, and a recording
    /// whose microphone is track 3 on one machine and track 2 on another is a
    /// recording nobody can write an instruction about.
    ///
    /// The track's number — the `n` in `TrackId::Audio(n)` — is its position
    /// after this insertion, so ask for it with
    /// [`audio_track_for`](Self::audio_track_for) rather than counting calls.
    #[must_use]
    pub fn with_audio_track(mut self, track: AudioTrack) -> Self {
        let rank = track.ordering_rank();
        let position = self
            .audio
            .iter()
            .rposition(|declared| declared.ordering_rank() <= rank)
            .map_or(0, |index| index + 1);
        self.audio.insert(position, track);
        self
    }

    /// Which track carries `source`, once every track has been added.
    ///
    /// The way a caller addresses its packets: the alternative is counting the
    /// order tracks were declared in, which is the arithmetic that puts the
    /// microphone's audio on the game's track. Returns [`None`] for a source
    /// this recording has no track for — a recording made with the microphone
    /// turned off has no microphone track, and asking for one is a question with
    /// an answer rather than a failure.
    ///
    /// A layout carrying two tracks for one source — SPEC.md section 14's
    /// processed and raw microphone,
    /// [issue #32](https://github.com/wildware-uk/clipped/issues/32) — gets the
    /// first of them here, and needs [`audio_tracks`](Self::audio_tracks) to
    /// tell them apart.
    #[must_use]
    pub fn audio_track_for(&self, source: &AudioSource) -> Option<TrackId> {
        let index = self
            .audio
            .iter()
            .position(|track| track.source() == Some(source))?;
        u16::try_from(index).ok().map(TrackId::Audio)
    }

    /// The video track.
    #[must_use]
    pub const fn video(&self) -> &VideoTrack {
        &self.video
    }

    /// The audio tracks, in the order the container will declare them.
    #[must_use]
    pub fn audio_tracks(&self) -> &[AudioTrack] {
        &self.audio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frames_duration_comes_from_the_whole_fraction_and_not_the_rounded_rate() {
        // 59.94 fps is 60000/1001, and a caller that rounded it to 60 would give
        // the last block of every clip a duration 0.1 % short. The fraction is
        // why this is one function rather than a division at each call site.
        assert_eq!(
            FrameRate::per_second(60)
                .expect("a real rate")
                .frame_interval(),
            Duration::from_nanos(16_666_666)
        );
        assert_eq!(
            FrameRate::new(60_000, 1001)
                .expect("a real rate")
                .frame_interval(),
            Duration::from_nanos(16_683_333)
        );
        assert_eq!(
            FrameRate::per_second(1)
                .expect("a real rate")
                .frame_interval(),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn a_track_is_named_in_a_message_the_way_a_person_would_name_it() {
        assert_eq!(TrackId::Video.to_string(), "video track");
        assert_eq!(TrackId::Audio(2).to_string(), "audio track 2");
    }

    #[test]
    fn language_tags_are_three_lowercase_letters_and_nothing_else() {
        assert_eq!(Language::new("eng"), Ok(Language::ENGLISH));
        assert_eq!(Language::new("und"), Ok(Language::UNDETERMINED));
        assert_eq!(Language::ENGLISH.as_str(), "eng");

        // Everything below would otherwise reach Matroska's Language element
        // verbatim, in a file that cannot be corrected afterwards.
        assert_eq!(
            Language::new("en"),
            Err(InvalidLanguage::Length { found: 2 })
        );
        assert_eq!(
            Language::new("english"),
            Err(InvalidLanguage::Length { found: 7 })
        );
        assert_eq!(Language::new(""), Err(InvalidLanguage::Length { found: 0 }));
        assert_eq!(
            Language::new("ENG"),
            Err(InvalidLanguage::Character { index: 0 })
        );
        assert_eq!(
            Language::new("en1"),
            Err(InvalidLanguage::Character { index: 2 })
        );
        // Three bytes, but not three characters: `é` is two bytes on its own.
        assert!(Language::new("é1").is_err());
    }

    #[test]
    fn a_rejected_language_tag_does_not_repeat_itself() {
        // The habit docs/logging.md sets: a rejection describes the shape of
        // the problem, because the value may be the thing that must not be
        // printed. A language tag is harmless, but the moment one of these
        // messages is built from a caller's string the habit has to already be
        // in place.
        let message = Language::new("Sécurité").unwrap_err().to_string();
        assert!(!message.contains("Sécurité"), "{message}");
    }

    #[test]
    fn a_frame_rate_cannot_be_zero_in_either_half() {
        assert!(FrameRate::new(60, 1).is_some());
        assert!(FrameRate::new(60_000, 1001).is_some());
        assert!(FrameRate::new(0, 1).is_none());
        assert!(FrameRate::new(60, 0).is_none());
        assert!(FrameRate::per_second(0).is_none());
    }

    #[test]
    fn a_layout_keeps_audio_tracks_in_the_order_they_were_declared() {
        // The order is the numbering: `TrackId::Audio(1)` has to be the second
        // track added, or every packet after the first goes to the wrong place.
        let layout = RecordingLayout::new(VideoTrack::new(VideoCodec::H264, 1920, 1080))
            .with_audio_track(AudioTrack::new(AudioCodec::PcmS16Le, 48_000, 2).with_name("Mix"))
            .with_audio_track(AudioTrack::new(AudioCodec::Opus, 48_000, 2).with_name("Game"))
            .with_audio_track(AudioTrack::new(AudioCodec::Opus, 48_000, 1).with_name("Microphone"));

        let names: Vec<_> = layout
            .audio_tracks()
            .iter()
            .map(|track| track.name().unwrap_or_default())
            .collect();
        assert_eq!(names, ["Mix", "Game", "Microphone"]);
        assert_eq!(layout.audio_tracks()[2].channels(), 1);
        assert_eq!(layout.video().width(), 1920);
    }

    #[test]
    fn sourced_tracks_are_ordered_by_the_model_and_not_by_the_caller() {
        // Declared in the order somebody's settings screen might produce, which
        // is not the order the file has to be in.
        let layout = RecordingLayout::new(VideoTrack::new(VideoCodec::H264, 1920, 1080))
            .with_audio_track(AudioTrack::for_source(AudioSource::Microphone, 48_000, 1))
            .with_audio_track(AudioTrack::for_source(
                AudioSource::application("Spotify"),
                48_000,
                2,
            ))
            .with_audio_track(AudioTrack::for_source(AudioSource::Game, 48_000, 2))
            .with_audio_track(AudioTrack::for_source(
                AudioSource::application("Discord"),
                48_000,
                2,
            ))
            .with_audio_track(AudioTrack::for_source(
                AudioSource::CompatibilityMix,
                48_000,
                2,
            ))
            .with_audio_track(AudioTrack::for_source(
                AudioSource::OtherSystemAudio,
                48_000,
                2,
            ));

        let names: Vec<_> = layout
            .audio_tracks()
            .iter()
            .map(|track| track.name().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            [
                "Compatibility Mix",
                "Game",
                "Other System Audio",
                "Microphone",
                // Two application tracks of equal rank, in the order they were
                // configured: nothing else distinguishes them, and reordering
                // them would be a file whose tracks moved because a name was
                // edited.
                "Spotify",
                "Discord",
            ]
        );

        // And a caller addresses its packets by source rather than by counting.
        assert_eq!(
            layout.audio_track_for(&AudioSource::CompatibilityMix),
            Some(TrackId::Audio(0))
        );
        assert_eq!(
            layout.audio_track_for(&AudioSource::Microphone),
            Some(TrackId::Audio(3))
        );
        assert_eq!(
            layout.audio_track_for(&AudioSource::application("Discord")),
            Some(TrackId::Audio(5))
        );
        // A source this recording does not have. A microphone that was turned
        // off is a question with an answer, not a failure.
        assert_eq!(layout.audio_track_for(&AudioSource::VoiceChat), None);
    }

    #[test]
    fn a_track_described_from_a_source_carries_what_the_model_says() {
        let mix = AudioTrack::for_source(AudioSource::CompatibilityMix, 48_000, 2);
        assert_eq!(mix.name(), Some("Compatibility Mix"));
        assert_eq!(mix.codec(), RECORDING_AUDIO_CODEC);
        assert!(
            mix.is_default(),
            "a player that picks one track has to pick this one (SPEC.md section 13)"
        );
        // Not guessed from the machine's locale: a wrong language tag is in the
        // file for good.
        assert_eq!(mix.language(), Language::UNDETERMINED);

        let microphone = AudioTrack::for_source(AudioSource::Microphone, 44_100, 1);
        assert!(!microphone.is_default());
        assert_eq!(microphone.channels(), 1, "a mono capture stays mono");
        assert_eq!(microphone.sample_rate(), 44_100);
        assert_eq!(
            microphone.with_language(Language::ENGLISH).language(),
            Language::ENGLISH,
            "a caller that knows the language can still say so"
        );
    }

    #[test]
    fn tracks_with_no_source_keep_the_order_they_were_declared_in() {
        // What a remux writes: tracks copied from a file this crate did not
        // record, whose order is the source file's and must not be rearranged.
        let layout = RecordingLayout::new(VideoTrack::new(VideoCodec::H264, 1920, 1080))
            .with_audio_track(AudioTrack::new(AudioCodec::Opus, 48_000, 2).with_name("Third"))
            .with_audio_track(AudioTrack::new(AudioCodec::Opus, 48_000, 2).with_name("Second"))
            .with_audio_track(AudioTrack::new(AudioCodec::Opus, 48_000, 2).with_name("First"));

        let names: Vec<_> = layout
            .audio_tracks()
            .iter()
            .map(|track| track.name().unwrap_or_default())
            .collect();
        assert_eq!(names, ["Third", "Second", "First"]);

        // And a sourced track added afterwards still leads, because the file's
        // first audio track is the one a naive player will take.
        let layout = layout.with_audio_track(AudioTrack::for_source(
            AudioSource::CompatibilityMix,
            48_000,
            2,
        ));
        assert_eq!(
            layout.audio_tracks()[0].name(),
            Some("Compatibility Mix"),
            "a sourced track sorts ahead of tracks that have no source"
        );
    }

    #[test]
    fn codec_names_match_the_ones_ffprobe_prints() {
        // Asserted against FFmpeg's own descriptors rather than against a
        // second list of strings: these names are what the media tests compare
        // `codec_name` with, so a typo here would be a test asserting the wrong
        // thing rather than a failure.
        for (codec, id) in [
            (VideoCodec::H264, ffi::AV_CODEC_ID_H264),
            (VideoCodec::Hevc, ffi::AV_CODEC_ID_HEVC),
            (VideoCodec::Av1, ffi::AV_CODEC_ID_AV1),
        ] {
            assert_eq!(codec.av_codec_id(), id);
            assert_eq!(codec.ffmpeg_name(), ffmpeg_codec_name(id));
        }

        for (codec, id) in [
            (AudioCodec::PcmS16Le, ffi::AV_CODEC_ID_PCM_S16LE),
            (AudioCodec::Aac, ffi::AV_CODEC_ID_AAC),
            (AudioCodec::Opus, ffi::AV_CODEC_ID_OPUS),
        ] {
            assert_eq!(codec.av_codec_id(), id);
            assert_eq!(codec.ffmpeg_name(), ffmpeg_codec_name(id));
        }
    }

    /// The name FFmpeg's own descriptor table has for a codec.
    fn ffmpeg_codec_name(id: ffi::AVCodecID) -> String {
        // SAFETY: `avcodec_get_name` accepts any value of AVCodecID and returns
        // a pointer to a string constant inside libavcodec — the descriptor's
        // name, or a literal for an unknown identifier — which is NUL-terminated
        // and lives for the process.
        let name = unsafe { std::ffi::CStr::from_ptr(ffi::avcodec_get_name(id)) };
        name.to_string_lossy().into_owned()
    }
}
