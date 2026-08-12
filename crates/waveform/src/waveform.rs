//! The shape a timeline and a clip editor draw from.

use core::fmt;
use core::num::NonZeroUsize;
use core::ops::Range;
use core::time::Duration;

use crate::peaks::{build_levels, read_levels, Level, Peak};
use crate::source::SourceIdentity;
use crate::WaveformError;

/// Which audio track of a recording a waveform belongs to.
///
/// A Clipped recording has several: game, microphone and other system audio are
/// separate tracks by design (SPEC.md section 11, issue #28), and the editor
/// shows one waveform per track rather than one for the recording. Nothing here
/// assumes a particular number of them, including zero — recordings written
/// today have no audio track at all until issue #180 lands, and a recording with
/// no audio produces a [`Waveform`] with no tracks rather than an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDescriptor {
    stream_index: u32,
    sample_rate: u32,
    channels: u16,
    name: String,
}

impl TrackDescriptor {
    /// Describes a track.
    #[must_use]
    pub fn new(
        stream_index: u32,
        sample_rate: u32,
        channels: u16,
        name: impl Into<String>,
    ) -> Self {
        Self {
            stream_index,
            sample_rate,
            channels,
            name: name.into(),
        }
    }

    /// Which stream of the container this track is.
    ///
    /// The stable identifier: a track's position in the list can change if a
    /// container gains a stream, and this cannot.
    #[must_use]
    pub fn stream_index(&self) -> u32 {
        self.stream_index
    }

    /// The track's sample rate, in hertz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// How many channels the track has.
    ///
    /// The waveform merges them: one envelope per track, so a sound panned hard
    /// to one side is as visible as one in the middle.
    #[must_use]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// What the container calls the track — its `title` tag, or its language —
    /// or [`None`] when it says nothing.
    ///
    /// This is what an editor labels the row with. The muxer writes these
    /// (docs/muxing.md), and a file from somewhere else may not.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        Some(self.name.as_str()).filter(|name| !name.is_empty())
    }
}

/// One track's peaks, at every resolution they were stored at.
#[derive(Clone)]
pub struct TrackWaveform {
    descriptor: TrackDescriptor,
    duration: Duration,
    levels: Vec<Level>,
}

impl TrackWaveform {
    /// Builds a track's pyramid from its base-resolution peaks.
    pub(crate) fn from_base(
        descriptor: TrackDescriptor,
        duration: Duration,
        base_bucket: Duration,
        base: Vec<Peak>,
    ) -> Self {
        Self {
            descriptor,
            duration,
            levels: build_levels(base, base_bucket),
        }
    }

    /// Assembles a track from levels that were read back from a cache file.
    pub(crate) fn from_levels(
        descriptor: TrackDescriptor,
        duration: Duration,
        levels: Vec<Level>,
    ) -> Self {
        Self {
            descriptor,
            duration,
            levels,
        }
    }

    /// Which track this is.
    #[must_use]
    pub fn descriptor(&self) -> &TrackDescriptor {
        &self.descriptor
    }

    /// How much audio the track holds.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// The finest resolution this track was stored at.
    #[must_use]
    pub fn base_bucket(&self) -> Duration {
        self.levels.first().map_or(Duration::ZERO, |level| {
            Duration::from_nanos(level.bucket_nanos())
        })
    }

    /// The peaks for a range of the track, reduced to `buckets` of them.
    ///
    /// `buckets` is what the caller is drawing — in practice the pixel width of
    /// the row. The result is always exactly that many: time outside the
    /// recording answers [`Peak::SILENT`], so a fixed-width timeline does not
    /// have to special-case the end of a track.
    #[must_use]
    pub fn peaks(&self, range: Range<Duration>, buckets: NonZeroUsize) -> Vec<Peak> {
        read_levels(&self.levels, range, buckets)
    }

    /// The peaks for the whole track, reduced to `buckets` of them.
    #[must_use]
    pub fn overview(&self, buckets: NonZeroUsize) -> Vec<Peak> {
        self.peaks(Duration::ZERO..self.duration, buckets)
    }

    /// The stored levels, for the cache format.
    pub(crate) fn levels(&self) -> &[Level] {
        &self.levels
    }
}

impl fmt::Debug for TrackWaveform {
    /// Reports the shape rather than a million peaks.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrackWaveform")
            .field("descriptor", &self.descriptor)
            .field("duration", &self.duration)
            .field("levels", &self.levels)
            .finish()
    }
}

/// Every audio track of one recording, summarised.
#[derive(Debug, Clone)]
pub struct Waveform {
    source: SourceIdentity,
    tracks: Vec<TrackWaveform>,
}

impl Waveform {
    pub(crate) fn new(source: SourceIdentity, tracks: Vec<TrackWaveform>) -> Self {
        Self { source, tracks }
    }

    /// Which recording this describes, and what it looked like when it was
    /// analysed.
    #[must_use]
    pub fn source(&self) -> &SourceIdentity {
        &self.source
    }

    /// The tracks, in container stream order.
    #[must_use]
    pub fn tracks(&self) -> &[TrackWaveform] {
        &self.tracks
    }

    /// The track for a container stream index, if the recording has one.
    #[must_use]
    pub fn track(&self, stream_index: u32) -> Option<&TrackWaveform> {
        self.tracks
            .iter()
            .find(|track| track.descriptor().stream_index() == stream_index)
    }

    /// Whether the recording had no audio at all.
    ///
    /// A supported answer rather than a failure: recordings written before issue
    /// #180 have no audio track, and a timeline for one is a timeline with no
    /// audio rows.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.tracks.is_empty()
    }

    /// The longest track, which is as much of the recording as has audio.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.tracks
            .iter()
            .map(TrackWaveform::duration)
            .max()
            .unwrap_or(Duration::ZERO)
    }
}

/// What the cache can say about a recording's waveform.
///
/// Deliberately not a `Result`. "It has not been generated yet" is the ordinary
/// state of a recording that has just been written, and a timeline that treated
/// it as an error would show an error banner over every new recording. Every
/// variant is something a timeline can draw: [`tracks`](Self::tracks) is empty
/// for all but [`Ready`](Self::Ready), so the caller that draws rows over
/// whatever it is given needs no branch at all.
#[derive(Debug)]
pub enum WaveformState {
    /// The peaks are here.
    Ready(Waveform),
    /// Nothing is cached yet. Generation has been requested, or can be.
    Pending,
    /// There will be no waveform, and why.
    ///
    /// A diagnostic, not something to put in front of a user (AGENTS.md section
    /// 45): the timeline draws without audio rows either way.
    Unavailable(WaveformError),
}

impl WaveformState {
    /// The waveform, when there is one.
    #[must_use]
    pub fn waveform(&self) -> Option<&Waveform> {
        match self {
            Self::Ready(waveform) => Some(waveform),
            Self::Pending | Self::Unavailable(_) => None,
        }
    }

    /// The tracks to draw, which is none of them unless the peaks are ready.
    #[must_use]
    pub fn tracks(&self) -> &[TrackWaveform] {
        self.waveform().map_or(&[], Waveform::tracks)
    }

    /// Whether the peaks are here.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    /// Why there will be no waveform, when that is known.
    #[must_use]
    pub fn reason(&self) -> Option<&WaveformError> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            Self::Ready(_) | Self::Pending => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peaks::BASE_BUCKET;
    use std::path::PathBuf;

    fn source() -> SourceIdentity {
        SourceIdentity::from_parts(PathBuf::from("recording.mkv"), 1, 1)
    }

    fn track(stream_index: u32, level: i8, seconds: u64) -> TrackWaveform {
        let buckets = usize::try_from(seconds).expect("a small number") * 100;
        TrackWaveform::from_base(
            TrackDescriptor::new(stream_index, 48_000, 2, ""),
            Duration::from_secs(seconds),
            BASE_BUCKET,
            vec![Peak::new(-level, level); buckets],
        )
    }

    #[test]
    fn a_recording_with_no_audio_is_a_waveform_with_no_tracks_rather_than_an_error() {
        let waveform = Waveform::new(source(), Vec::new());
        assert!(waveform.is_silent());
        assert_eq!(waveform.duration(), Duration::ZERO);
        assert!(waveform.tracks().is_empty());
    }

    #[test]
    fn tracks_are_addressed_by_stream_index_rather_than_by_position() {
        let waveform = Waveform::new(source(), vec![track(1, 10, 1), track(4, 20, 2)]);
        assert_eq!(
            waveform.track(4).expect("stream 4").duration(),
            Duration::from_secs(2)
        );
        assert!(waveform.track(2).is_none());
        // And the recording is as long as its longest track.
        assert_eq!(waveform.duration(), Duration::from_secs(2));
    }

    #[test]
    fn every_track_keeps_its_own_peaks() {
        let waveform = Waveform::new(source(), vec![track(1, 10, 1), track(2, 100, 1)]);
        let width = NonZeroUsize::new(8).expect("eight");
        let quiet = waveform.track(1).expect("stream 1").overview(width);
        let loud = waveform.track(2).expect("stream 2").overview(width);
        assert!(quiet.iter().all(|peak| peak.maximum() == 10));
        assert!(loud.iter().all(|peak| peak.maximum() == 100));
    }

    #[test]
    fn a_name_the_container_did_not_give_is_none_rather_than_an_empty_label() {
        assert_eq!(TrackDescriptor::new(0, 48_000, 2, "").name(), None);
        assert_eq!(
            TrackDescriptor::new(0, 48_000, 2, "Microphone").name(),
            Some("Microphone")
        );
    }

    #[test]
    fn a_state_that_is_not_ready_still_answers_the_drawing_questions() {
        let pending = WaveformState::Pending;
        assert!(!pending.is_ready());
        assert!(pending.tracks().is_empty());
        assert!(pending.waveform().is_none());
        assert!(pending.reason().is_none());

        let unavailable = WaveformState::Unavailable(WaveformError::Cancelled);
        assert!(!unavailable.is_ready());
        assert!(unavailable.tracks().is_empty());
        assert!(unavailable.reason().is_some());

        let ready = WaveformState::Ready(Waveform::new(source(), vec![track(1, 10, 1)]));
        assert!(ready.is_ready());
        assert_eq!(ready.tracks().len(), 1);
    }
}
