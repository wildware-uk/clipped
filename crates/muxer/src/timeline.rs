//! Turning source-clock nanoseconds into container timestamps.
//!
//! This is where muxers go wrong, so it is deliberately the one part of the
//! writer that touches no FFmpeg state and can be tested on its own.
//!
//! # The two time bases
//!
//! Callers work in nanoseconds on the recording's clock
//! ([`crate::PacketTimestamp`]). Matroska works in whatever unit the file
//! declares, which for FFmpeg's muxer is a millisecond: it fixes each stream's
//! time base at 1/1000 while writing the header, and the writer reads the value
//! back rather than assuming it. Everything below therefore converts from
//! nanoseconds into a time base handed to it, and rounds to nearest — the same
//! rounding `av_rescale_q` does — because truncating would drag every timestamp
//! towards the start of the file and, at high frame rates, drag some of them
//! onto each other.
//!
//! One millisecond of resolution is coarse enough to be worth stating plainly:
//! at 60 fps the frame interval is 16.67 ms and the rounding error is at most
//! half a millisecond, well below anything a viewer can perceive as drift, and
//! it does not accumulate because every timestamp is converted from the source
//! clock rather than from its predecessor. Above about 1000 fps two frames can
//! land on the same millisecond, which [`TrackTimeline::stamp`] resolves by
//! forcing the second one forward; no capture path in Clipped produces frames
//! that fast.
//!
//! # What "monotonic" means here
//!
//! A container demands that the *decode* timestamps of a track increase. It
//! does not demand that presentation timestamps do — a codec with B-frames
//! produces packets whose presentation order is not their decode order, and
//! that is normal and correct. So monotonicity is enforced on the decode
//! timestamp, and the presentation timestamp is left alone except where it
//! would precede the decode timestamp of the same packet, which no decoder can
//! honour.
//!
//! A packet whose decode timestamp does not increase is a fault somewhere
//! upstream — a clock that stepped, a queue that reordered, an encoder that
//! mislabelled. The muxer does not get to reject it: a recording is
//! irreplaceable and dropping the packet loses picture, while refusing the
//! whole write loses the session (AGENTS.md section 17). It is therefore nudged
//! to one tick past its predecessor, which keeps the file valid, and counted,
//! so that the summary and the log say plainly how much of the recording was
//! corrected rather than leaving it to be discovered by watching.

/// The unit a container counts timestamps in, as a fraction of a second.
///
/// FFmpeg's `AVRational`, kept as its own type here so the pure arithmetic
/// below can be tested without a linked libavutil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimeBase {
    numerator: i64,
    denominator: i64,
}

impl TimeBase {
    /// The time base `numerator`/`denominator` seconds.
    ///
    /// Returns [`None`] unless both parts are positive, which every time base a
    /// muxer declares is; a zero or negative one would make every conversion
    /// below meaningless, and it is better to notice that when the header is
    /// written than one timestamp at a time.
    pub(crate) const fn new(numerator: i64, denominator: i64) -> Option<Self> {
        if numerator <= 0 || denominator <= 0 {
            return None;
        }
        Some(Self {
            numerator,
            denominator,
        })
    }

    /// Converts nanoseconds into this time base, rounding to nearest and away
    /// from zero on a tie — `av_rescale_q`'s rounding.
    ///
    /// The arithmetic is done in 128 bits because the intermediate product is
    /// nanoseconds times the denominator: an hour of recording is 3.6e12
    /// nanoseconds, which multiplied by a 1000 Hz denominator is already 3.6e15,
    /// and a caller that passes an unrebased performance-counter reading —
    /// nanoseconds since boot — is another six orders of magnitude up. That
    /// overflows a `u64` product, and the symptom would be a timestamp in the
    /// wrong century rather than an error.
    pub(crate) const fn rescale_nanos(self, nanos: i64) -> i64 {
        let numerator = nanos as i128 * self.denominator as i128;
        let denominator = 1_000_000_000_i128 * self.numerator as i128;

        // Round half away from zero. Integer division in Rust truncates towards
        // zero, so adding half the divisor before dividing rounds a positive
        // value up at the halfway point, and subtracting it rounds a negative
        // value down; doubling both sides keeps that exact for an odd divisor.
        let doubled = numerator * 2;
        let rounded = if numerator >= 0 {
            (doubled + denominator) / (denominator * 2)
        } else {
            (doubled - denominator) / (denominator * 2)
        };

        // Saturating rather than wrapping: the input would have to be beyond
        // any clock's range to get here, and a timestamp pinned at the end of
        // time is at least visibly wrong.
        if rounded > i64::MAX as i128 {
            i64::MAX
        } else if rounded < i64::MIN as i128 {
            i64::MIN
        } else {
            rounded as i64
        }
    }
}

/// What [`TrackTimeline::stamp`] had to change to keep the file valid.
///
/// Counted rather than logged one by one: a clock that steps backwards once
/// produces one of these, and a queue that has genuinely lost its ordering
/// produces thousands, and the difference between those two is the whole
/// diagnosis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TimestampFixes {
    /// The packet was timed before the first packet of the file.
    pub(crate) before_start: bool,
    /// The packet's decode timestamp did not exceed its predecessor's.
    pub(crate) not_monotonic: bool,
    /// The packet was to be presented before it was decoded.
    pub(crate) presented_before_decoded: bool,
}

impl TimestampFixes {
    /// Whether anything had to be changed.
    pub(crate) const fn any(self) -> bool {
        self.before_start || self.not_monotonic || self.presented_before_decoded
    }
}

/// A packet's timestamps in the container's own units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContainerTimestamps {
    /// When the packet is presented.
    pub(crate) presentation: i64,
    /// When the packet is decoded. Strictly greater than the previous packet's.
    pub(crate) decode: i64,
    /// How long it lasts, or zero when the caller did not say.
    pub(crate) duration: i64,
    /// What had to be corrected to get here.
    pub(crate) fixes: TimestampFixes,
}

/// One track's decode position in the file being written, counted in the
/// container's own units.
///
/// Each track keeps its own, because monotonicity is a per-track requirement:
/// audio and video interleave freely and their timestamps cross constantly.
///
/// This is the correction policy on its own, with no conversion attached to it,
/// because two callers need it from different starting points. A recording
/// arrives in nanoseconds on the capture clock and is converted by
/// [`TrackTimeline`]; a remux arrives already counted in the *source* file's
/// units and is rescaled into the destination's by FFmpeg
/// ([`crate::remux`]). Both then have the same question to answer — does this
/// packet's decode timestamp advance? — and it is answered once, here.
#[derive(Debug, Default)]
pub(crate) struct DecodeOrder {
    last_decode: Option<i64>,
}

impl DecodeOrder {
    /// Places a packet whose timestamps are already in the container's units.
    ///
    /// Nothing is clamped to zero here: a negative timestamp is meaningful in a
    /// remux — Opus carries its pre-skip as one, and an edit list is how MP4
    /// represents it — and it is [`TrackTimeline`], where zero is the start of a
    /// recording being written, that clamps.
    pub(crate) fn place(
        &mut self,
        mut presentation: i64,
        mut decode: i64,
        duration: i64,
    ) -> ContainerTimestamps {
        let mut fixes = TimestampFixes::default();

        if let Some(previous) = self.last_decode {
            if decode <= previous {
                fixes.not_monotonic = true;
                decode = previous.saturating_add(1);
            }
        }

        if presentation < decode {
            // Only reachable once the decode timestamp has been moved, since a
            // caller is free to present later than it decodes but never
            // earlier. Raising the presentation timestamp to match keeps the
            // packet where the correction put it rather than inventing a
            // reordering the encoder never asked for.
            fixes.presented_before_decoded = true;
            presentation = decode;
        }

        self.last_decode = Some(decode);

        ContainerTimestamps {
            presentation,
            decode,
            duration,
            fixes,
        }
    }
}

/// One track's position in the file being written, converting from the
/// recording's clock as it goes.
#[derive(Debug)]
pub(crate) struct TrackTimeline {
    time_base: TimeBase,
    order: DecodeOrder,
}

impl TrackTimeline {
    /// A timeline counting in `time_base`, with nothing written yet.
    pub(crate) const fn new(time_base: TimeBase) -> Self {
        Self {
            time_base,
            order: DecodeOrder { last_decode: None },
        }
    }

    /// Places a packet on the timeline.
    ///
    /// Both timestamps are nanoseconds *relative to the start of the file*, so
    /// a negative one means the packet is older than the first packet written
    /// — which happens when tracks start at slightly different moments and the
    /// later-starting one is written first. It is clamped to the start of the
    /// file rather than dropped, and counted.
    pub(crate) fn stamp(
        &mut self,
        presentation_nanos: i64,
        decode_nanos: i64,
        duration_nanos: Option<u64>,
    ) -> ContainerTimestamps {
        let mut presentation = self.time_base.rescale_nanos(presentation_nanos);
        let mut decode = self.time_base.rescale_nanos(decode_nanos);

        let before_start = decode < 0 || presentation < 0;
        if before_start {
            decode = decode.max(0);
            presentation = presentation.max(0);
        }

        let duration = duration_nanos
            .and_then(|nanos| i64::try_from(nanos).ok())
            .map_or(0, |nanos| self.time_base.rescale_nanos(nanos));

        let mut stamps = self.order.place(presentation, decode, duration);
        stamps.fixes.before_start = before_start;
        stamps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The time base FFmpeg's Matroska muxer fixes every stream at.
    fn milliseconds() -> TimeBase {
        TimeBase::new(1, 1000).expect("1/1000 is a valid time base")
    }

    #[test]
    fn a_time_base_has_to_be_positive_in_both_halves() {
        assert!(TimeBase::new(1, 1000).is_some());
        assert!(TimeBase::new(0, 1000).is_none());
        assert!(TimeBase::new(1, 0).is_none());
        assert!(TimeBase::new(-1, 1000).is_none());
    }

    #[test]
    fn nanoseconds_convert_to_the_containers_units_by_rounding_to_nearest() {
        let base = milliseconds();

        assert_eq!(base.rescale_nanos(0), 0);
        assert_eq!(base.rescale_nanos(1_000_000), 1);
        // 16.666 ms, one frame at 60 fps: nearest is 17, not the 16 that
        // truncation would give.
        assert_eq!(base.rescale_nanos(16_666_667), 17);
        assert_eq!(base.rescale_nanos(16_400_000), 16);
        // Exactly half a millisecond goes away from zero, as av_rescale_q does.
        assert_eq!(base.rescale_nanos(500_000), 1);
        assert_eq!(base.rescale_nanos(-500_000), -1);
        assert_eq!(base.rescale_nanos(-16_666_667), -17);
    }

    #[test]
    fn rounding_error_does_not_accumulate_over_an_hour_of_frames() {
        // Every timestamp is converted from the source clock rather than from
        // its predecessor, so the error stays bounded by half a tick no matter
        // how long the recording runs. An hour at 60 fps is 216,000 frames; if
        // the conversion drifted by even a tenth of a millisecond per frame,
        // the last one would be 21 seconds out.
        let base = milliseconds();
        let frame_interval = 16_666_667_i64;

        for frame in [0_i64, 1, 60, 216_000 - 1] {
            let nanos = frame * frame_interval;
            let expected_millis = (nanos + 500_000) / 1_000_000;
            assert_eq!(
                base.rescale_nanos(nanos),
                expected_millis,
                "frame {frame} of an hour-long recording"
            );
        }
    }

    #[test]
    fn a_timestamp_from_an_unrebased_clock_does_not_overflow() {
        // Ten days of uptime in nanoseconds, which is what a performance
        // counter reads and what a caller that forgot to rebase would pass.
        // In 64-bit arithmetic the product with the denominator wraps.
        let base = milliseconds();
        let ten_days = 10 * 24 * 60 * 60 * 1_000_000_000_i64;
        assert_eq!(base.rescale_nanos(ten_days), 864_000_000);
    }

    #[test]
    fn a_run_of_frames_keeps_its_spacing_and_needs_no_correction() {
        let mut timeline = TrackTimeline::new(milliseconds());
        let interval = 16_666_667_i64;

        let mut stamps = Vec::new();
        for frame in 0..6 {
            let stamped = timeline.stamp(frame * interval, frame * interval, Some(16_666_667));
            assert!(!stamped.fixes.any(), "frame {frame} was corrected");
            assert_eq!(stamped.duration, 17);
            stamps.push(stamped.decode);
        }

        assert_eq!(stamps, [0, 17, 33, 50, 67, 83]);
    }

    #[test]
    fn a_packet_that_goes_backwards_is_nudged_forward_rather_than_dropped() {
        let mut timeline = TrackTimeline::new(milliseconds());

        timeline.stamp(0, 0, None);
        let second = timeline.stamp(100_000_000, 100_000_000, None);
        assert_eq!(second.decode, 100);

        // A packet timed before the one already written. Losing it would lose
        // picture; writing it as-is would make the file invalid.
        let backwards = timeline.stamp(50_000_000, 50_000_000, None);
        assert!(backwards.fixes.not_monotonic);
        assert_eq!(backwards.decode, 101);
        assert_eq!(
            backwards.presentation, 101,
            "the presentation timestamp follows the correction rather than staying behind it"
        );

        // And the track carries on from where the correction put it.
        let next = timeline.stamp(120_000_000, 120_000_000, None);
        assert!(!next.fixes.any());
        assert_eq!(next.decode, 120);
    }

    #[test]
    fn two_packets_on_the_same_tick_are_separated() {
        // Above about 1000 fps two frames round to the same millisecond. The
        // second one has to be moved, or the muxer rejects it.
        let mut timeline = TrackTimeline::new(milliseconds());

        let first = timeline.stamp(0, 0, None);
        let second = timeline.stamp(100_000, 100_000, None);

        assert_eq!(first.decode, 0);
        assert!(second.fixes.not_monotonic);
        assert_eq!(second.decode, 1);
    }

    #[test]
    fn reordered_presentation_timestamps_are_left_alone() {
        // A B-frame sequence: decode order is IPB, presentation order is IBP.
        // Nothing here is a fault and nothing may be corrected.
        let mut timeline = TrackTimeline::new(milliseconds());

        let key = timeline.stamp(0, 0, None);
        let predicted = timeline.stamp(66_666_667, 16_666_667, None);
        let bidirectional = timeline.stamp(33_333_333, 33_333_333, None);

        assert!(!key.fixes.any());
        assert!(!predicted.fixes.any());
        assert!(!bidirectional.fixes.any());

        assert_eq!(
            [key.decode, predicted.decode, bidirectional.decode],
            [0, 17, 33]
        );
        assert_eq!(
            [
                key.presentation,
                predicted.presentation,
                bidirectional.presentation
            ],
            [0, 67, 33]
        );
    }

    #[test]
    fn a_packet_older_than_the_file_is_clamped_to_its_start() {
        // The audio thread's first packet reaching the writer after the video
        // thread's, having been captured before it.
        let mut timeline = TrackTimeline::new(milliseconds());

        let early = timeline.stamp(-40_000_000, -40_000_000, None);
        assert!(early.fixes.before_start);
        assert_eq!(early.decode, 0);
        assert_eq!(early.presentation, 0);

        let next = timeline.stamp(20_000_000, 20_000_000, None);
        assert!(!next.fixes.any());
        assert_eq!(next.decode, 20);
    }

    #[test]
    fn each_track_keeps_its_own_position() {
        // Video at 0, 33, 66 and audio at 0, 21, 42 interleave constantly. If
        // they shared a position, every second packet would be "corrected".
        let mut video = TrackTimeline::new(milliseconds());
        let mut audio = TrackTimeline::new(milliseconds());

        for (video_nanos, audio_nanos) in
            [(0, 0), (33_000_000, 21_000_000), (66_000_000, 42_000_000)]
        {
            assert!(!video.stamp(video_nanos, video_nanos, None).fixes.any());
            assert!(!audio.stamp(audio_nanos, audio_nanos, None).fixes.any());
        }
    }

    #[test]
    fn a_remuxed_packet_older_than_the_file_keeps_its_negative_timestamp() {
        // The difference between the two entry points, and it matters. A
        // recording being written starts at its first packet, so a timestamp
        // before that is a fault to clamp. A file being copied has a start of
        // its own: Opus carries its pre-skip as a negative timestamp, and MP4
        // stores that in an edit list. Clamping it would move the sound forward
        // by the pre-skip and put the copy out of sync with the source.
        let mut order = DecodeOrder::default();

        let first = order.place(-336, -336, 960);
        assert!(!first.fixes.any());
        assert_eq!(first.decode, -336);
        assert_eq!(first.presentation, -336);

        let next = order.place(624, 624, 960);
        assert!(!next.fixes.any());
        assert_eq!(next.decode, 624);
    }

    #[test]
    fn a_remuxed_stream_that_reorders_is_copied_as_it_is() {
        // A B-frame sequence arriving from a source file, in the source's own
        // units. Nothing here is a fault and nothing may be corrected: MP4
        // stores the gap between the two timestamps as a composition offset.
        let mut order = DecodeOrder::default();

        let key = order.place(0, -3_000, 3_000);
        let predicted = order.place(9_000, 0, 3_000);
        let bidirectional = order.place(3_000, 3_000, 3_000);

        assert!(!key.fixes.any());
        assert!(!predicted.fixes.any());
        assert!(!bidirectional.fixes.any());
        assert_eq!(
            [key.decode, predicted.decode, bidirectional.decode],
            [-3_000, 0, 3_000]
        );
        assert_eq!(
            [
                key.presentation,
                predicted.presentation,
                bidirectional.presentation
            ],
            [0, 9_000, 3_000]
        );
    }

    #[test]
    fn a_remuxed_packet_whose_decode_order_broke_is_nudged_rather_than_dropped() {
        // A source whose decode timestamps do not increase is a file FFmpeg's
        // muxer refuses outright, so a remux that passed it through would fail
        // part-way and leave a truncated MP4. One tick forward keeps the copy
        // going, and the count is what says how much of it was corrected.
        let mut order = DecodeOrder::default();

        order.place(0, 0, 3_000);
        let backwards = order.place(1_500, 1_500, 3_000);
        assert!(!backwards.fixes.any());

        let repeated = order.place(1_500, 1_500, 3_000);
        assert!(repeated.fixes.not_monotonic);
        assert!(repeated.fixes.presented_before_decoded);
        assert_eq!(repeated.decode, 1_501);
        assert_eq!(repeated.presentation, 1_501);
    }

    #[test]
    fn a_duration_the_caller_did_not_give_is_zero_rather_than_a_guess() {
        let mut timeline = TrackTimeline::new(milliseconds());
        assert_eq!(timeline.stamp(0, 0, None).duration, 0);
        assert_eq!(
            timeline
                .stamp(1_000_000, 1_000_000, Some(21_333_333))
                .duration,
            21
        );
    }
}
