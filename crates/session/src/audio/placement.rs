//! Where a captured buffer of audio goes on the recording's timeline.
//!
//! All of the arithmetic that decides an audio packet's timestamp is here, away
//! from the WASAPI captures and the muxing queue, because it is the part that
//! decides whether a recording sounds right and the part no hardware is needed
//! to check. `docs/av-sync.md` is the model; this module implements two of its
//! rules and nothing else:
//!
//! - **Every packet is placed through [`CaptureClock::media_time_on`]**, naming
//!   the clock the reading came from, so the claim "these nanoseconds are on the
//!   recording's reference clock" is one line a reviewer can check rather than a
//!   subtraction somewhere in a capture loop.
//! - **Audio that precedes the epoch is trimmed at the epoch, to the sample.**
//!   The audio endpoints are opened before the first video frame arrives, so the
//!   first buffer of a recording routinely describes a moment before the
//!   recording starts — 293 ms of it on the machine `docs/av-sync.md` was
//!   measured on. Left alone, the muxer clamps every one of those frames onto
//!   the first instant of the file, which puts a quarter of a second of audio in
//!   one place and makes the track that much longer than its video.
//!
//! Nothing here shifts a timestamp to make two sources agree. The frames that
//! survive the trim keep the media time their own hardware gave them.

use clipped_capture::{CaptureClock, ClockMismatch, MediaTime, SourceClock};

/// The clock a Windows audio endpoint reports positions on.
///
/// WASAPI attaches a performance-counter reading to every packet, which is the
/// same counter both capture backends stamp frames with — so this is a fact
/// about the platform rather than an assumption this crate is making, and
/// naming it is what [`CaptureClock::media_time_on`] asks the caller for.
/// `clipped_audio::AudioTimestamp`'s own documentation is the source.
pub(crate) const AUDIO_CLOCK: SourceClock = SourceClock::PerformanceCounter;

/// One buffer, placed on the recording's timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Placed {
    /// Where the first surviving frame belongs.
    pub(crate) at: MediaTime,
    /// How many interleaved samples were dropped off the front because they
    /// described a moment before the recording started.
    pub(crate) samples_trimmed: usize,
}

/// Places `timestamp` — nanoseconds on the audio endpoint's clock — on the
/// recording's timeline, trimming anything before the epoch.
///
/// `samples` is the interleaved buffer the capture handed over and `channels`
/// is how many of them make one frame; both are needed because the trim is
/// measured in frames and applied in samples, and a trim that used the sample
/// count would drop the wrong amount and swap the channels of everything after
/// it.
///
/// Returns [`None`] when the whole buffer precedes the epoch, which is the
/// ordinary case for the first buffer or two of a recording: there is nothing
/// left to write, and writing it anyway would stack it on the first instant of
/// the file.
///
/// # Errors
///
/// [`ClockMismatch`] when the recording is not timed against the clock the
/// endpoint reports on. Nothing can place such a packet, and guessing would put
/// the sound somewhere the picture is not (`docs/av-sync.md`).
pub(crate) fn place(
    clock: CaptureClock,
    timestamp: u64,
    samples: usize,
    channels: u16,
    sample_rate: u32,
) -> Result<Option<Placed>, ClockMismatch> {
    let media = clock.media_time_on(AUDIO_CLOCK, timestamp)?;
    let nanos = media.as_nanos();
    if nanos >= 0 {
        return Ok(Some(Placed {
            at: media,
            samples_trimmed: 0,
        }));
    }

    // Rounded *up*, so the first frame kept is at or after the epoch rather
    // than a fraction of a sample before it. A frame is 20.8 microseconds at
    // 48 kHz, so this costs at most one frame of audio and is what keeps the
    // trimmed buffer from needing the muxer's clamp after all.
    let trimmed_frames = frames_covering(nanos.unsigned_abs(), sample_rate);
    let trimmed_samples = trimmed_frames.saturating_mul(usize::from(channels.max(1)));
    if trimmed_samples >= samples {
        return Ok(None);
    }

    Ok(Some(Placed {
        // The frames that survive keep their own position: the epoch plus
        // however far into the buffer the first kept frame is. Nothing is
        // shifted to zero.
        at: MediaTime::from_nanos(nanos.saturating_add(nanos_for(trimmed_frames, sample_rate))),
        samples_trimmed: trimmed_samples,
    }))
}

/// How many whole frames it takes to cover `nanos` at `sample_rate`, rounding
/// up.
///
/// A rate of zero cannot happen — `clipped-audio` reports a `NonZeroU32` and
/// `AudioTrackWriter` refuses a track without one — but dividing by it here
/// would be a panic in a recording rather than a wrong number, so it is guarded.
fn frames_covering(nanos: u64, sample_rate: u32) -> usize {
    if sample_rate == 0 {
        return 0;
    }
    let rate = u128::from(sample_rate);
    let frames = (u128::from(nanos) * rate).div_ceil(1_000_000_000);
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// How long `frames` last at `sample_rate`, in nanoseconds.
///
/// In 128 bits for the reason `clipped_muxer::audio` gives for the same
/// arithmetic: a frame count multiplied by a billion leaves a 64-bit integer
/// after a few days of recording, and a wrapped timestamp is a packet at the far
/// end of the timeline.
fn nanos_for(frames: usize, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    let nanos = frames as i128 * 1_000_000_000 / i128::from(sample_rate);
    i64::try_from(nanos).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use clipped_capture::CaptureTimestamp;

    use super::*;

    /// The counter reading a real recording's epoch is: a machine that has been
    /// up for a year. Nothing here may assume the epoch is small.
    const EPOCH: u64 = 31_107_000 * 1_000_000_000;

    fn clock() -> CaptureClock {
        CaptureClock::start_at(CaptureTimestamp::from_source(
            SourceClock::PerformanceCounter,
            EPOCH,
        ))
    }

    #[test]
    fn a_buffer_after_the_epoch_keeps_the_position_its_endpoint_gave_it() {
        // The ordinary case, and the one the whole model rests on: nothing is
        // shifted, so a sound is where its own hardware said it happened.
        let placed = place(clock(), EPOCH + 250_000_000, 960 * 2, 2, 48_000)
            .expect("the endpoint reports on the recording's clock")
            .expect("a buffer after the epoch survives whole");

        assert_eq!(placed.at.as_nanos(), 250_000_000);
        assert_eq!(placed.samples_trimmed, 0);
    }

    #[test]
    fn a_buffer_straddling_the_epoch_is_trimmed_to_the_sample_and_the_rest_kept() {
        // The audio thread opens its endpoint while the capture backend is
        // still initialising, so the first buffer of a recording describes a
        // moment before the recording starts. 10 ms of stereo at 48 kHz,
        // beginning 5 ms early: the first 240 frames are before the epoch and
        // the remaining 240 belong at zero.
        let placed = place(clock(), EPOCH - 5_000_000, 480 * 2, 2, 48_000)
            .expect("the endpoint reports on the recording's clock")
            .expect("half of this buffer is inside the recording");

        assert_eq!(
            placed.samples_trimmed,
            240 * 2,
            "the trim is measured in frames and applied in samples; a trim of 240 *samples* \
             would keep 4 ms too much and swap the channels of everything after it"
        );
        assert_eq!(
            placed.at.as_nanos(),
            0,
            "the first surviving frame lands exactly on the epoch"
        );
    }

    #[test]
    fn a_trimmed_buffer_never_starts_before_the_epoch() {
        // The reason the frame count is rounded up rather than down. A buffer
        // beginning at a moment that is not a whole number of frames before the
        // epoch would, with a rounded-down trim, still start negative — and the
        // muxer would clamp it, which is the behaviour this trim exists to
        // avoid. 44.1 kHz because its frame period is not a whole number of
        // nanoseconds.
        for early_nanos in [1, 7, 22_675, 1_000_000, 293_000_000] {
            let placed = place(clock(), EPOCH - early_nanos, 44_100, 1, 44_100)
                .expect("the endpoint reports on the recording's clock")
                .expect("a second of audio outlasts every offset above");
            assert!(
                placed.at.as_nanos() >= 0,
                "a buffer {early_nanos} ns early was placed at {} ns",
                placed.at.as_nanos()
            );
            assert!(
                placed.at.as_nanos() < nanos_for(1, 44_100),
                "and no more than one frame late: {} ns",
                placed.at.as_nanos()
            );
        }
    }

    #[test]
    fn a_buffer_entirely_before_the_epoch_is_dropped_rather_than_stacked_at_zero() {
        // What the muxer would otherwise do with it: clamp every frame onto the
        // first instant of the file. A quarter of a second of audio in one
        // place is both audible and the reason the finished track ends later
        // than the picture.
        let dropped = place(clock(), EPOCH - 100_000_000, 480 * 2, 2, 48_000)
            .expect("the endpoint reports on the recording's clock");
        assert_eq!(dropped, None);

        // Exactly one buffer's length early is still entirely before the epoch:
        // its last frame ends at the epoch and starts before it.
        let boundary = place(clock(), EPOCH - 10_000_000, 480 * 2, 2, 48_000)
            .expect("the endpoint reports on the recording's clock");
        assert_eq!(boundary, None);
    }

    #[test]
    fn a_reading_on_another_clock_is_refused_rather_than_subtracted() {
        // A recording timed against something other than the performance
        // counter cannot place a WASAPI position at all, and a difference of
        // two unrelated counters is a number that looks fine until somebody
        // watches the recording (docs/av-sync.md).
        let monotonic =
            CaptureClock::start_at(CaptureTimestamp::from_source(SourceClock::Monotonic, EPOCH));
        assert!(place(monotonic, EPOCH, 960, 2, 48_000).is_err());
    }

    #[test]
    fn frame_arithmetic_rounds_up_and_survives_a_recording_that_runs_for_days() {
        assert_eq!(frames_covering(0, 48_000), 0);
        assert_eq!(
            frames_covering(1, 48_000),
            1,
            "a fraction of a frame is a frame"
        );
        assert_eq!(frames_covering(20_833, 48_000), 1);
        assert_eq!(frames_covering(20_834, 48_000), 2);
        assert_eq!(frames_covering(1_000_000_000, 48_000), 48_000);

        assert_eq!(nanos_for(48_000, 48_000), 1_000_000_000);
        assert_eq!(nanos_for(882, 44_100), 20_000_000);
        // A week of audio, where a 64-bit product of frames and a billion would
        // have wrapped long ago.
        assert_eq!(
            nanos_for(48_000 * 60 * 60 * 24 * 7, 48_000),
            604_800_000_000_000
        );
    }
}
