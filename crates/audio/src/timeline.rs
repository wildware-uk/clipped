//! The rule that keeps a captured track the same length as the recording.
//!
//! # The problem this exists for
//!
//! A WASAPI endpoint delivers nothing at all for periods it has nothing to say
//! about. Not silent buffers — nothing: `GetNextPacketSize` returns zero, and
//! then packets resume as though no time had passed. A capture that simply
//! concatenates what it is given therefore produces a track that is shorter
//! than the recording by exactly the amount of that quiet, and every sound
//! after the first gap appears earlier in the file than it happened. Because
//! the loss is cumulative, the error grows all session: a two-hour recording
//! with twenty minutes of quiet in it ends up twenty minutes out of step with
//! its video by the end, having looked perfectly synchronised at the start.
//!
//! Both captures meet it, for different reasons, and the reasons are worth
//! knowing because they decide how often it happens. A loopback stream is
//! silent whenever no application is rendering, which on an idle machine is
//! most of the time; it is the single most common way loopback capture is got
//! wrong. A microphone normally produces packets continuously — a quiet room is
//! quiet samples, not no samples — so the gaps are the device being unplugged,
//! the stream being reopened, or the audio engine dropping data a stalled
//! consumer did not collect. That makes them rarer and longer rather than
//! absent, and a microphone track that skipped an unplugged headset would slide
//! out of step with the video exactly the same way.
//!
//! The fix is not subtle — fill the gap with silence — but the length of the
//! fill has to come from the device's own clock or it merely replaces one drift
//! with another.
//!
//! # How
//!
//! A [`Timeline`] holds one anchor and one frame count. The anchor is a
//! performance-counter reading; the frame count is every frame handed to the
//! caller, real or synthesised. Together they say what the timeline's *next*
//! frame should be stamped with:
//!
//! ```text
//! expected = anchor + frames_emitted / sample_rate
//! ```
//!
//! Every packet arrives from WASAPI with its own performance-counter position.
//! Comparing that position against `expected` is the whole mechanism:
//!
//! - position later than expected — time passed that produced no samples, so
//!   [`Continuity::SilenceFirst`] says how many frames of silence go in front of
//!   the packet;
//! - position earlier than expected — this crate already emitted silence over
//!   part of the period the packet covers, so [`Continuity::Trim`] drops the
//!   overlapping frames from the front of it, or [`Continuity::Drop`] discards
//!   it if it is entirely inside the covered period;
//! - within a deadband either way — ordinary jitter, emitted unchanged.
//!
//! Because the comparison is always against the anchor rather than against the
//! previous packet, an error that is ignored once is still there next time, so
//! nothing accumulates: the deadband bounds the offset rather than allowing it
//! to grow.
//!
//! # Why there is a deadband at all
//!
//! Measured on Windows 11 build 26200 against the default 48 kHz render
//! endpoint, consecutive loopback packets arrive 10 ms apart with their reported
//! positions varying by a few tens of microseconds. Correcting that would
//! insert a one-frame silence or trim a frame from most packets in a recording
//! — audible as a faint tick, and pointless. [`DEADBAND`] is far wider than the
//! jitter and far narrower than a perceptible synchronisation error.
//!
//! The cost is that a device whose sample clock genuinely runs at a slightly
//! different rate from the performance counter is corrected in one
//! [`DEADBAND`]-sized step rather than continuously: a few parts per million of
//! drift reaches 20 ms about once an hour, and produces one 20 ms silence or
//! one 20 ms trim when it does. Removing that step needs resampling against a
//! reference clock, which is
//! [issue #30](https://github.com/wildware-uk/clipped/issues/30). Until then a
//! rare, bounded correction is the honest trade against unbounded drift.

use crate::format::AudioFormat;
use crate::time::AudioTimestamp;

/// How far a packet's reported position may differ from where the timeline
/// expects it before the difference is treated as real.
///
/// 20 ms: about 2000 times the observed packet-to-packet jitter, and under the
/// threshold at which a listener notices audio leading or lagging video.
const DEADBAND: u64 = 20_000_000;

/// How far behind the present the timeline is filled to when the endpoint has
/// gone quiet.
///
/// Silence is only ever synthesised up to `now - LAG`, never up to `now`,
/// because audio for the last few milliseconds may still be inside the
/// endpoint's buffer. Filling over it would only mean trimming it away again
/// when it arrived — replacing real samples with synthesised silence, which is
/// precisely the mistake this module exists to avoid. 60 ms is six device
/// periods on the machine this was measured on.
const LAG: u64 = 60_000_000;

/// How long a capture waits for its first packet before deciding the endpoint
/// was already silent when the recording started.
///
/// Anchoring on the first real packet is better than anchoring on a reading
/// this process takes, because the packet's position is the device's own and a
/// reading here is one buffer period away from it. So the timeline stays
/// unanchored for a moment at the start to give a packet the chance to define
/// it. If none arrives, the recording genuinely began during silence, the
/// open-time reading is the best evidence available of when the track should
/// start, and the timeline anchors on that instead.
const ANCHOR_GRACE: u64 = 250_000_000;

/// The longest run of synthesised silence handed over in one buffer.
///
/// A capture whose consumer stalled for a minute owes a minute of silence, and
/// allocating a minute of zeroed samples to say so would be 23 MB for a stereo
/// 48 kHz endpoint and would grow without limit as the stall did. The debt is
/// paid in 100 ms instalments instead, so the memory this crate uses is fixed
/// whatever happens to the consumer (AGENTS.md section 18).
const SILENCE_CHUNK: u64 = 100_000_000;

/// What to do with a packet that has just arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Continuity {
    /// Emit the packet unchanged.
    Continue,
    /// Emit this many frames of silence, then the packet.
    SilenceFirst(u64),
    /// Emit the packet without its first this-many frames.
    Trim(u64),
    /// Discard the packet: the timeline has already covered the period it
    /// describes.
    Drop,
}

/// One capture's running position, and the arithmetic that keeps it honest.
#[derive(Debug)]
pub(crate) struct Timeline {
    format: AudioFormat,
    /// The performance-counter reading taken when the capture was opened, used
    /// as the anchor only if no packet arrives within [`ANCHOR_GRACE`].
    opened_nanos: u64,
    /// The performance-counter reading the first emitted frame is stamped with.
    anchor: Option<u64>,
    /// Frames handed to the caller since the anchor, real and synthesised.
    frames: u64,
    /// Frames of silence owed to the caller and not yet handed over.
    silence_owed: u64,
}

impl Timeline {
    /// Starts a timeline for `format`, opened at `opened`.
    pub(crate) fn new(format: AudioFormat, opened: AudioTimestamp) -> Self {
        Self {
            format,
            opened_nanos: opened.as_nanos(),
            anchor: None,
            frames: 0,
            silence_owed: 0,
        }
    }

    /// Frames handed to the caller so far, real and synthesised.
    pub(crate) fn frames_emitted(&self) -> u64 {
        self.frames
    }

    /// Decides what to do with a packet reported at `arrived`.
    ///
    /// Anchors the timeline if this is the first frame it has seen.
    pub(crate) fn plan(&mut self, arrived: AudioTimestamp, packet_frames: u64) -> Continuity {
        let arrived = arrived.as_nanos();

        let Some(expected) = self.expected_nanos() else {
            // Nothing emitted yet and no silence owed: this packet defines
            // where the track starts, with the device's own reading.
            self.anchor = Some(arrived);
            return Continuity::Continue;
        };

        if let Some(gap) = arrived.checked_sub(expected) {
            if gap < DEADBAND {
                return Continuity::Continue;
            }
            return Continuity::SilenceFirst(self.format.nanos_to_frames(gap));
        }

        let overlap = expected - arrived;
        if overlap < DEADBAND {
            return Continuity::Continue;
        }
        let overlap_frames = self.format.nanos_to_frames(overlap);
        if overlap_frames >= packet_frames {
            Continuity::Drop
        } else {
            Continuity::Trim(overlap_frames)
        }
    }

    /// Records that `frames` frames have been handed to the caller, and returns
    /// the timestamp the first of them carries.
    ///
    /// # Panics
    ///
    /// If the timeline has not been anchored. Every path that emits a frame
    /// anchors first — [`plan`](Self::plan) on a real packet, and
    /// [`owe_silence_until`](Self::owe_silence_until) on a synthesised one — so
    /// reaching this is a bug in this module rather than a state a caller can
    /// produce.
    pub(crate) fn emit(&mut self, frames: u64) -> AudioTimestamp {
        let anchor = self
            .anchor
            .expect("the timeline is anchored before anything is emitted");
        let timestamp =
            AudioTimestamp::from_nanos(anchor + self.format.frames_to_nanos(self.frames));
        self.frames += frames;
        timestamp
    }

    /// Adds the silence needed to bring the timeline up to `now`, less
    /// [`LAG`], to what the caller is owed.
    ///
    /// Called when a read found no packet: either the endpoint is silent, or
    /// there is no endpoint at all. Both are periods the device will never
    /// describe, so the performance counter read here is the only evidence of
    /// their length — the one place in this crate that reads a clock rather
    /// than being told a position. When the endpoint speaks again,
    /// [`plan`](Self::plan) reconciles whatever this over- or under-estimated
    /// against the device's own position, so an error here is corrected rather
    /// than accumulated.
    pub(crate) fn owe_silence_until(&mut self, now: AudioTimestamp) {
        let now = now.as_nanos();
        let target = now.saturating_sub(LAG);

        let expected = match self.expected_nanos() {
            Some(expected) => expected,
            None => {
                // Still inside the grace period: give a real packet a little
                // longer to anchor the timeline on the device's clock.
                if now.saturating_sub(self.opened_nanos) < ANCHOR_GRACE {
                    return;
                }
                self.anchor = Some(self.opened_nanos);
                self.opened_nanos
            }
        };

        if let Some(behind) = target.checked_sub(expected) {
            self.silence_owed += self.format.nanos_to_frames(behind);
        }
    }

    /// Adds `frames` of silence to what the caller is owed.
    pub(crate) fn owe_silence(&mut self, frames: u64) {
        self.silence_owed += frames;
    }

    /// Frames of silence still owed to the caller.
    pub(crate) fn silence_owed(&self) -> u64 {
        self.silence_owed
    }

    /// Takes the next instalment of owed silence, in frames.
    ///
    /// Returns zero when nothing is owed. Never returns more than
    /// [`SILENCE_CHUNK`] is worth, so the buffer backing it has a fixed size
    /// however long the silence lasts.
    pub(crate) fn take_silence_instalment(&mut self) -> u64 {
        let instalment = self
            .silence_owed
            .min(self.format.nanos_to_frames(SILENCE_CHUNK));
        self.silence_owed -= instalment;
        instalment
    }

    /// Where the next frame belongs, or [`None`] if nothing anchors the
    /// timeline yet.
    fn expected_nanos(&self) -> Option<u64> {
        self.anchor
            .map(|anchor| anchor + self.format.frames_to_nanos(self.frames))
    }
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroU32};

    use super::*;
    use crate::format::{ChannelMask, SampleFormat};

    const SECOND: u64 = 1_000_000_000;
    /// A 10 ms WASAPI packet at 48 kHz, as measured on Windows 11 build 26200.
    const PACKET_FRAMES: u64 = 480;
    const PACKET_NANOS: u64 = 10_000_000;

    fn format() -> AudioFormat {
        AudioFormat::new(
            NonZeroU32::new(48_000).expect("48 kHz is not zero"),
            NonZeroU16::new(2).expect("stereo is not zero channels"),
            ChannelMask::from_bits(0x3),
            SampleFormat::Float32,
        )
    }

    /// A timeline opened at an arbitrary counter reading, as one is in life:
    /// the counter counts from boot, not from the recording.
    fn timeline() -> Timeline {
        Timeline::new(format(), AudioTimestamp::from_nanos(31_107_000 * SECOND))
    }

    fn at(nanos: u64) -> AudioTimestamp {
        AudioTimestamp::from_nanos(nanos)
    }

    /// Runs `packets` consecutive on-time packets from `start`, emitting each,
    /// and returns the timeline and the counter position after the last one.
    fn run_steady(timeline: &mut Timeline, start: u64, packets: u64) -> u64 {
        for packet in 0..packets {
            let arrived = at(start + packet * PACKET_NANOS);
            assert_eq!(
                timeline.plan(arrived, PACKET_FRAMES),
                Continuity::Continue,
                "packet {packet} of an evenly paced run should need no correction"
            );
            timeline.emit(PACKET_FRAMES);
        }
        start + packets * PACKET_NANOS
    }

    #[test]
    fn an_evenly_paced_stream_is_passed_through_untouched() {
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        run_steady(&mut timeline, start, 600);

        assert_eq!(timeline.frames_emitted(), 600 * PACKET_FRAMES);
        assert_eq!(timeline.silence_owed(), 0);
        // Six seconds in, the next frame is stamped six seconds after the
        // first: no silence has been invented and none has been lost.
        assert_eq!(timeline.emit(0), at(start + 6 * SECOND));
    }

    #[test]
    fn the_first_packet_anchors_the_timeline_on_the_devices_own_position() {
        // Not on the open-time reading: the device's position is one buffer
        // period away from anything this process could read for itself, and
        // that difference would become a constant offset against the video.
        let mut timeline = timeline();
        let arrived = 31_107_400 * SECOND;
        assert_eq!(
            timeline.plan(at(arrived), PACKET_FRAMES),
            Continuity::Continue
        );
        assert_eq!(timeline.emit(PACKET_FRAMES), at(arrived));
    }

    #[test]
    fn a_silent_period_becomes_exactly_as_much_silence_as_it_lasted() {
        // The bug this module exists for. Two seconds of packets, five seconds
        // in which WASAPI delivers nothing at all, then packets again — and
        // the track has to come out seven seconds long, not two.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        let after_sound = run_steady(&mut timeline, start, 200);

        let resumes = after_sound + 5 * SECOND;
        let plan = timeline.plan(at(resumes), PACKET_FRAMES);
        assert_eq!(
            plan,
            Continuity::SilenceFirst(5 * 48_000),
            "five seconds of nothing is five seconds of silence"
        );

        let Continuity::SilenceFirst(frames) = plan else {
            unreachable!("checked immediately above");
        };
        timeline.owe_silence(frames);
        while timeline.silence_owed() > 0 {
            let instalment = timeline.take_silence_instalment();
            timeline.emit(instalment);
        }
        // The sound that ends the silence is stamped when it happened.
        assert_eq!(timeline.emit(PACKET_FRAMES), at(resumes));
        assert_eq!(timeline.frames_emitted(), 7 * 48_000 + PACKET_FRAMES);
    }

    #[test]
    fn packet_to_packet_jitter_does_not_produce_a_correction() {
        // Real positions wander by tens of microseconds. Correcting that would
        // trim or pad almost every packet in a recording.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        timeline.plan(at(start), PACKET_FRAMES);
        timeline.emit(PACKET_FRAMES);

        let jitter = [37_000i64, -52_000, 11_000, -80_000, 64_000, -9_000];
        for (packet, offset) in jitter.iter().enumerate() {
            let nominal = start + (packet as u64 + 1) * PACKET_NANOS;
            let arrived = nominal.wrapping_add(*offset as u64);
            assert_eq!(
                timeline.plan(at(arrived), PACKET_FRAMES),
                Continuity::Continue,
                "an offset of {offset} ns is jitter, not a gap"
            );
            timeline.emit(PACKET_FRAMES);
        }
        assert_eq!(timeline.frames_emitted(), 7 * PACKET_FRAMES);
    }

    #[test]
    fn jitter_that_is_ignored_does_not_accumulate_into_drift() {
        // The reason the comparison is against the anchor and not against the
        // previous packet. A source running consistently 100 microseconds
        // early per packet is inside the deadband every time, so nothing is
        // corrected packet by packet — but the offset against the anchor keeps
        // growing, and the timeline has to notice when it crosses the
        // deadband rather than drifting away for ever.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        timeline.plan(at(start), PACKET_FRAMES);
        timeline.emit(PACKET_FRAMES);

        let mut corrections = 0;
        for packet in 1..600u64 {
            let arrived = start + packet * (PACKET_NANOS + 100_000);
            match timeline.plan(at(arrived), PACKET_FRAMES) {
                Continuity::Continue => {}
                Continuity::SilenceFirst(frames) => {
                    corrections += 1;
                    timeline.emit(frames);
                }
                other => panic!("a source running slow cannot need {other:?}"),
            }
            timeline.emit(PACKET_FRAMES);
        }

        // 100 microseconds per 10 ms packet is 1% slow, so six seconds of
        // packets cover about six seconds and 60 ms of counter time; the
        // timeline follows the counter, correcting in 20 ms steps.
        assert!(
            (2..=4).contains(&corrections),
            "expected the drift to be corrected in a few deadband-sized steps, got {corrections}"
        );
        let emitted = timeline.frames_emitted();
        let expected = format().nanos_to_frames(599 * (PACKET_NANOS + 100_000)) + PACKET_FRAMES;
        assert!(
            emitted.abs_diff(expected) < format().nanos_to_frames(DEADBAND),
            "the track ({emitted} frames) should track the counter ({expected} frames) \
             to within one deadband"
        );
    }

    #[test]
    fn silence_synthesised_ahead_of_the_device_is_trimmed_back_out() {
        // A read that timed out synthesises silence up to `now - LAG`. If the
        // endpoint then delivers audio covering part of that period, the
        // overlap has to come off the front of the packet: emitting both would
        // make the track longer than the recording, which is the same bug in
        // the other direction.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        let after_sound = run_steady(&mut timeline, start, 100);

        // Nothing for a second, so silence is owed up to a second ago less the
        // lag, and paid.
        timeline.owe_silence_until(at(after_sound + SECOND));
        let owed = timeline.silence_owed();
        assert_eq!(owed, format().nanos_to_frames(SECOND - LAG));
        while timeline.silence_owed() > 0 {
            let instalment = timeline.take_silence_instalment();
            timeline.emit(instalment);
        }

        // Then a packet turns up describing a moment already covered by
        // 200 ms.
        let covered_to = after_sound + SECOND - LAG;
        let plan = timeline.plan(at(covered_to - 200_000_000), 480);
        assert_eq!(
            plan,
            Continuity::Drop,
            "a packet entirely inside covered time is discarded, not appended"
        );

        // And one that starts just before the covered point keeps its tail.
        let overlap_nanos = 25_000_000;
        let long_packet = format().nanos_to_frames(100_000_000);
        assert_eq!(
            timeline.plan(at(covered_to - overlap_nanos), long_packet),
            Continuity::Trim(format().nanos_to_frames(overlap_nanos))
        );
    }

    #[test]
    fn a_capture_that_starts_during_silence_still_produces_a_track() {
        // No endpoint activity at all: the grace period passes, the timeline
        // anchors on the open-time reading, and silence starts flowing so the
        // consumer sees a track rather than a stall.
        let opened = 31_107_000 * SECOND;
        let mut timeline = Timeline::new(format(), at(opened));

        timeline.owe_silence_until(at(opened + 100_000_000));
        assert_eq!(
            timeline.silence_owed(),
            0,
            "inside the grace period a real packet may still anchor the timeline"
        );

        timeline.owe_silence_until(at(opened + SECOND));
        assert_eq!(
            timeline.silence_owed(),
            format().nanos_to_frames(SECOND - LAG),
            "after the grace period the silence is synthesised from the open-time reading"
        );
        assert_eq!(timeline.emit(0), at(opened));
    }

    #[test]
    fn owed_silence_is_handed_over_in_fixed_size_instalments() {
        // A consumer that stalled for a minute must not cause a minute of
        // zeroed samples to be allocated at once.
        let mut timeline = timeline();
        timeline.plan(at(31_107_500 * SECOND), PACKET_FRAMES);
        timeline.emit(PACKET_FRAMES);

        timeline.owe_silence(60 * 48_000);
        let chunk = format().nanos_to_frames(SILENCE_CHUNK);
        let mut instalments = 0;
        let mut total = 0;
        while timeline.silence_owed() > 0 {
            let instalment = timeline.take_silence_instalment();
            assert!(
                instalment <= chunk,
                "instalment {instalment} exceeds {chunk}"
            );
            total += instalment;
            timeline.emit(instalment);
            instalments += 1;
        }
        assert_eq!(total, 60 * 48_000);
        assert_eq!(instalments, 600);
    }

    #[test]
    fn every_buffer_is_stamped_exactly_where_the_previous_one_ended() {
        // What the muxer will rely on: no gaps and no overlaps between
        // consecutive buffers, across silence, corrections and all.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        let mut expected = None;
        let mut emitted = 0u64;

        let mut hand_over = |timeline: &mut Timeline, frames: u64| {
            let timestamp = timeline.emit(frames);
            if let Some(expected) = expected {
                assert_eq!(timestamp, expected, "buffers must be contiguous");
            }
            emitted += frames;
            expected = Some(at(start + format().frames_to_nanos(emitted)));
        };

        for packet in 0..50u64 {
            // A gap of two seconds every ten packets, and ordinary pacing
            // otherwise.
            let arrived = start + packet * PACKET_NANOS + (packet / 10) * 2 * SECOND;
            match timeline.plan(at(arrived), PACKET_FRAMES) {
                Continuity::Continue => {}
                Continuity::SilenceFirst(frames) => {
                    timeline.owe_silence(frames);
                    while timeline.silence_owed() > 0 {
                        let instalment = timeline.take_silence_instalment();
                        hand_over(&mut timeline, instalment);
                    }
                }
                other => panic!("a stream that only ever pauses cannot need {other:?}"),
            }
            hand_over(&mut timeline, PACKET_FRAMES);
        }

        // Fifty packets and four two-second gaps.
        assert_eq!(
            timeline.frames_emitted(),
            50 * PACKET_FRAMES + 4 * 2 * 48_000
        );
    }
}
