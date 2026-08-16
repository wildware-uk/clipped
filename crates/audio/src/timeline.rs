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
//! Left alone, a device whose sample clock genuinely runs at a slightly
//! different rate from the performance counter would be corrected in one
//! [`DEADBAND`]-sized step rather than continuously: a few parts per million of
//! drift reaches 20 ms about once an hour, and produces one 20 ms silence or
//! one 20 ms trim when it does. That was the whole story before
//! [issue #30](https://github.com/wildware-uk/clipped/issues/30); what it added
//! is below.
//!
//! # Continuous correction
//!
//! The deadband bounds an *ignored* offset; it says nothing about a *steady*
//! one. A device running a genuine 5 ppm fast is inside the deadband on every
//! single packet — the offset per packet is a fraction of a microsecond — and
//! still 20 ms out an hour later, because the same small error is added every
//! time. [`Timeline::correction_ratio`] turns that observation around: instead
//! of waiting for the ignored offset to cross the deadband and then erasing it
//! in one step, it measures the *rate* the offset has been growing at since the
//! last time the timeline was known to be right, and hands back a resampling
//! ratio a fraction of a percent from `1.0` that the caller applies to the next
//! real packet's samples (`crate::resample`) so the offset stops growing in the
//! first place. The deadband and the step correction are still here — a real
//! gap or a device change is not a rate to track, it is silence to fill or
//! samples to trim, exactly as before — but a steady clock no longer needs
//! either, and the correction that used to be one 20 ms event an hour is now a
//! resampling ratio too small to hear, applied on every packet.
//!
//! The rate is only trustworthy once it has been measured for a while:
//! [`MIN_SERVO_WINDOW`] holds the ratio at `1.0` until enough time has passed
//! since the last reset for the offset to say more about the clock than about
//! jitter, and [`MAX_DRIFT_RATIO`] bounds how far the ratio can move from `1.0`
//! however wrong a measurement is, because real hardware drifts by single-digit
//! parts per million and a bug here must not be free to retune a track by more
//! than that.
//!
//! "The last time the timeline was known to be right" is deliberately not "the
//! anchor". Anything that already produces [`Continuity::SilenceFirst`],
//! [`Continuity::Trim`] or [`Continuity::Drop`] — a real gap, an endpoint
//! reopened on a different device — throws away everything the rate estimate
//! knew, because it may describe a different piece of hardware with a different
//! clock. Averaging across that event would blend two devices' drift into one
//! wrong number; resetting at it means the estimate that comes out the other
//! side is always about the device that is playing right now.

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

/// How long the timeline waits, after a reset, before trusting a drift-rate
/// estimate enough to act on it.
///
/// The deadband (20 ms) bounds how far a single packet's position may be from
/// where it is expected without triggering a correction, so immediately after
/// a reset the only offset available to estimate a rate from is smaller than
/// that — a few tens of microseconds of jitter, divided by a few milliseconds
/// of elapsed time, is a wildly noisy ppm figure that says nothing about the
/// device's real clock. Two seconds is about 200 packets of averaging, long
/// enough for the jitter to wash out against a genuine parts-per-million
/// drift.
const MIN_SERVO_WINDOW: u64 = 2_000_000_000;

/// How far [`Timeline::correction_ratio`] may move from `1.0`, either way.
///
/// 1000 ppm (0.1%) is far beyond any real clock this crate has been run
/// against — a few parts per million is typical, tens of ppm would be a
/// notably bad crystal — so this is a backstop against a bad measurement
/// rather than a bound this crate expects to reach. A ratio this large would
/// still be inaudible as a steady pitch shift, but is clamped anyway: nothing
/// about a wrong estimate should be free to retune a track by more than a
/// bad device's own hardware could.
const MAX_DRIFT_RATIO: f64 = 0.001;

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
    /// The performance-counter reading of the packet that last reset the
    /// drift-rate estimate: the anchor, or the most recent packet that needed
    /// [`Continuity::SilenceFirst`], [`Continuity::Trim`] or
    /// [`Continuity::Drop`]. [`None`] before the timeline is anchored.
    servo_since: Option<u64>,
    /// The signed offset (`arrived - expected`, in nanoseconds) at
    /// `servo_since`, so [`correction_ratio`](Self::correction_ratio) can
    /// measure the offset accrued *since* the reset rather than the whole
    /// offset, which may predate it.
    servo_offset: i64,
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
            servo_since: None,
            servo_offset: 0,
        }
    }

    /// Frames handed to the caller so far, real and synthesised.
    pub(crate) fn frames_emitted(&self) -> u64 {
        self.frames
    }

    /// The resampling ratio [`crate::resample`] should apply to the *next*
    /// real packet reported at `arrived`, to correct for the drift measured
    /// since the last reset.
    ///
    /// `1.0` — no correction — until the timeline is anchored and
    /// [`MIN_SERVO_WINDOW`] has passed since the last reset; otherwise `1.0 +
    /// offset / elapsed`, clamped to [`MAX_DRIFT_RATIO`], where `offset` is how
    /// much `arrived` has moved against
    /// [`expected_nanos`](Self::expected_nanos) since the reset and `elapsed`
    /// is how long ago that reset was. A source running fast arrives earlier
    /// than expected over time, giving a ratio below `1.0` that asks for fewer
    /// output frames per input frame — a small time-compression that keeps the
    /// track from running ahead of the reference clock — and a slow source the
    /// mirror image.
    ///
    /// Read-only: this only reports what [`plan`](Self::plan) has already
    /// measured, so calling it any number of times, or not at all, changes
    /// nothing.
    pub(crate) fn correction_ratio(&self, arrived: AudioTimestamp) -> f64 {
        let (Some(expected), Some(servo_since)) = (self.expected_nanos(), self.servo_since) else {
            return 1.0;
        };
        let arrived = arrived.as_nanos();
        let Some(elapsed) = arrived.checked_sub(servo_since) else {
            // A position that is not even later than the reset says nothing
            // useful about a rate; `plan` will treat it as its own event.
            return 1.0;
        };
        if elapsed < MIN_SERVO_WINDOW {
            return 1.0;
        }

        let offset_now = i128::from(arrived) - i128::from(expected);
        let accrued = (offset_now - i128::from(self.servo_offset)) as f64;
        let ratio = 1.0 + accrued / elapsed as f64;
        ratio.clamp(1.0 - MAX_DRIFT_RATIO, 1.0 + MAX_DRIFT_RATIO)
    }

    /// Discards the drift-rate estimate without touching the anchor or the
    /// frame count.
    ///
    /// Called when the stream a capture is reading from is replaced — a
    /// device change, or a reopen after a failure — so that a rate measured
    /// against one piece of hardware is never used to correct another. Safe to
    /// call even when the replacement stream turns out to need no correction
    /// of its own: [`correction_ratio`](Self::correction_ratio) reports `1.0`
    /// until the next packet re-establishes a baseline, exactly as it does
    /// before the first packet ever arrives.
    pub(crate) fn reset_drift_correction(&mut self) {
        self.servo_since = None;
    }

    /// Records `arrived` and the signed offset from `expected` as the new
    /// baseline the drift-rate estimate measures from.
    fn reset_servo(&mut self, arrived: u64, offset: i64) {
        self.servo_since = Some(arrived);
        self.servo_offset = offset;
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
            self.reset_servo(arrived, 0);
            return Continuity::Continue;
        };

        if let Some(gap) = arrived.checked_sub(expected) {
            if gap < DEADBAND {
                return Continuity::Continue;
            }
            // A gap this large is a real event — silence, a reopen — not a
            // rate to track; see "Continuous correction" in the module docs.
            self.reset_servo(arrived, i64::try_from(gap).unwrap_or(i64::MAX));
            return Continuity::SilenceFirst(self.format.nanos_to_frames(gap));
        }

        let overlap = expected - arrived;
        if overlap < DEADBAND {
            return Continuity::Continue;
        }
        let overlap_frames = self.format.nanos_to_frames(overlap);
        self.reset_servo(
            arrived,
            i64::try_from(overlap).map_or(i64::MIN, |overlap| -overlap),
        );
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

    #[test]
    fn correction_ratio_is_unity_before_a_timeline_is_anchored() {
        // No packet has arrived yet, so there is nothing to measure a rate
        // against — the same "no evidence yet" position `plan` is in before
        // its first call.
        let timeline = timeline();
        assert_eq!(timeline.correction_ratio(at(31_107_500 * SECOND)), 1.0);
    }

    #[test]
    fn correction_ratio_is_unity_until_the_servo_window_has_elapsed() {
        // A single packet's offset against the anchor is jitter, not a rate:
        // dividing a small offset by a small elapsed time is noise, however
        // large the resulting "ppm" looks, so the ratio must not move on it.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        timeline.plan(at(start), PACKET_FRAMES);
        timeline.emit(PACKET_FRAMES);

        // One second since the reset: below MIN_SERVO_WINDOW (two seconds),
        // whatever the apparent offset.
        let arrived = at(start + SECOND + 5_000_000);
        assert_eq!(timeline.correction_ratio(arrived), 1.0);
    }

    #[test]
    fn correction_ratio_reports_the_measured_drift_once_the_window_has_passed() {
        // Two seconds of packets arriving exactly on time, so the track's
        // `expected` position is exactly two seconds after the anchor and the
        // servo's baseline is still the anchor itself (nothing has reset it).
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        run_steady(&mut timeline, start, 200);
        let expected_now = start + 2 * SECOND;

        // A hypothetical next packet arriving 400 microseconds later than
        // that: 200 ppm fast, comfortably inside the deadband (20 ms) so
        // `plan` alone would ignore it, and comfortably past the servo
        // window.
        let offset = 400_000i64;
        let arrived = at(expected_now + offset as u64);
        let ratio = timeline.correction_ratio(arrived);

        let elapsed = 2 * SECOND + offset as u64;
        let expected_ratio = 1.0 + offset as f64 / elapsed as f64;
        assert!(
            (ratio - expected_ratio).abs() < 1e-12,
            "got {ratio}, expected {expected_ratio}"
        );
        assert!(
            ratio > 1.0,
            "a packet arriving later than expected is a fast source, which needs a ratio \
             above 1.0 to emit more frames and slow the track down: got {ratio}"
        );
    }

    #[test]
    fn correction_ratio_is_negative_going_for_a_source_running_slow() {
        // The mirror image of the test above: a packet arriving *earlier*
        // than expected is a source running slow, and the fix is to emit
        // fewer frames — a ratio below 1.0. Three seconds of baseline rather
        // than two, so that subtracting the offset still leaves the elapsed
        // time comfortably past the servo window.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        run_steady(&mut timeline, start, 300);
        let expected_now = start + 3 * SECOND;

        let offset = 400_000u64;
        let arrived = at(expected_now - offset);
        let ratio = timeline.correction_ratio(arrived);
        assert!(
            ratio < 1.0,
            "a packet arriving earlier than expected is a slow source, which needs a ratio \
             below 1.0: got {ratio}"
        );
    }

    #[test]
    fn correction_ratio_is_clamped_rather_than_following_a_bad_measurement() {
        // A synthetic offset far larger than any real hardware clock would
        // produce. Whatever the arithmetic says, the ratio this crate hands
        // to a resampler must not move further than MAX_DRIFT_RATIO from
        // 1.0.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        run_steady(&mut timeline, start, 200);
        let expected_now = start + 2 * SECOND;

        let arrived = at(expected_now + 500_000_000); // 500 ms against 2.5 s: 20% "drift".
        assert_eq!(
            timeline.correction_ratio(arrived),
            1.0 + MAX_DRIFT_RATIO,
            "an outrageous offset must be clamped, not passed straight through"
        );
    }

    #[test]
    fn a_real_gap_resets_the_drift_estimate_rather_than_extending_it() {
        // Two seconds of on-time packets establish a baseline at the anchor,
        // then a five-second gap — a real event, not a rate — arrives. If the
        // servo were not reset by it, the huge offset the gap produces would
        // still be sitting in the estimate afterwards; because it is reset,
        // a packet shortly after the gap sees the same "not enough history
        // yet" answer a freshly anchored timeline would.
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        let after_sound = run_steady(&mut timeline, start, 200);

        let resumes = after_sound + 5 * SECOND;
        let plan = timeline.plan(at(resumes), PACKET_FRAMES);
        assert_eq!(plan, Continuity::SilenceFirst(5 * 48_000));
        timeline.owe_silence(5 * 48_000);
        while timeline.silence_owed() > 0 {
            let instalment = timeline.take_silence_instalment();
            timeline.emit(instalment);
        }
        timeline.emit(PACKET_FRAMES);

        // Only one second since the packet that closed the gap: below the
        // servo window, so the ratio has to be 1.0 — which it could only be
        // if the reset actually happened, since eight seconds have passed
        // since the *original* anchor.
        let ratio = timeline.correction_ratio(at(resumes + SECOND));
        assert_eq!(
            ratio, 1.0,
            "the estimate should have restarted at the gap rather than carrying pre-gap history"
        );
    }

    #[test]
    fn reset_drift_correction_clears_the_estimate_without_touching_the_track() {
        let mut timeline = timeline();
        let start = 31_107_500 * SECOND;
        run_steady(&mut timeline, start, 200);
        let frames_before = timeline.frames_emitted();

        timeline.reset_drift_correction();

        assert_eq!(
            timeline.correction_ratio(at(start + 10 * SECOND)),
            1.0,
            "a reset estimate reports no correction until a new baseline is measured"
        );
        assert_eq!(
            timeline.frames_emitted(),
            frames_before,
            "resetting the drift estimate must not touch the frame count"
        );
    }
}
