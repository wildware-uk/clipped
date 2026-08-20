//! What a run of decoded pattern counters says about the frames behind it.
//!
//! Every frame `video-pattern` draws carries a counter, so a sequence of
//! decoded counters is a sequence of source frames. Reading it back answers
//! three questions no frame count can: were any delivered **twice**, did any
//! arrive **out of order**, and how many of the source's frames are **missing**
//! from the span that did arrive.
//!
//! # Why this is in the library rather than in a test
//!
//! Two tests need the same arithmetic on two different things.
//! `tests/capture/wgc_video_pattern.rs` asks it of frames a capture backend
//! handed over; `tests/capture/recorded_frames.rs` asks it of frames decoded
//! back out of a finished recording
//! ([issue #183](https://github.com/wildware-uk/clipped/issues/183)). The
//! questions are identical and the answers must not be able to disagree, which
//! is what a second copy would eventually do (AGENTS.md section 55).
//!
//! What is deliberately *not* here is what to conclude. A capture that dropped
//! frames and a recording that dropped frames are different faults with
//! different thresholds — and a recording asked for a lower frame rate than the
//! source draws at is *expected* to be missing most of them. So this counts,
//! and each caller judges.

/// A run of counters, in the order they were decoded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CounterRun {
    first: Option<u32>,
    last: Option<u32>,
    decoded: u64,
    missing: u64,
    duplicated: u64,
    out_of_order: u64,
}

impl CounterRun {
    /// A run with nothing in it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            first: None,
            last: None,
            decoded: 0,
            missing: 0,
            duplicated: 0,
            out_of_order: 0,
        }
    }

    /// Adds the counter decoded out of the next frame.
    ///
    /// A counter equal to the previous one is a duplicate; one below it is out
    /// of order; one above it by more than a step leaves the counters in
    /// between missing. A counter that is out of order is deliberately *not*
    /// also counted as missing: the gap it appears to open is an artefact of
    /// the frame arriving late, not of a frame that never arrived, and counting
    /// both would report one fault twice.
    pub fn record(&mut self, index: u32) {
        match self.last {
            None => self.first = Some(index),
            Some(previous) if index == previous => self.duplicated += 1,
            Some(previous) if index < previous => self.out_of_order += 1,
            Some(previous) => self.missing += u64::from(index - previous) - 1,
        }
        self.last = Some(index);
        self.decoded += 1;
    }

    /// The first counter decoded.
    #[must_use]
    pub const fn first(&self) -> Option<u32> {
        self.first
    }

    /// The last counter decoded.
    #[must_use]
    pub const fn last(&self) -> Option<u32> {
        self.last
    }

    /// How many frames decoded at all.
    #[must_use]
    pub const fn decoded(&self) -> u64 {
        self.decoded
    }

    /// Counters the source presented in this span that never arrived.
    ///
    /// Only meaningful against the step the caller asked for: a recording held
    /// to 30 fps from a 60 fps source is missing every other counter on
    /// purpose.
    #[must_use]
    pub const fn missing(&self) -> u64 {
        self.missing
    }

    /// Counters that arrived more than once.
    #[must_use]
    pub const fn duplicated(&self) -> u64 {
        self.duplicated
    }

    /// Counters that went backwards, which is frames delivered out of order.
    #[must_use]
    pub const fn out_of_order(&self) -> u64 {
        self.out_of_order
    }

    /// How many frames the source presented between the first and the last one
    /// seen, inclusive.
    ///
    /// Zero for a run that decoded nothing, and zero for one whose last counter
    /// is below its first — which cannot happen without an out-of-order frame
    /// and is reported as that rather than as a negative span.
    #[must_use]
    pub const fn presented(&self) -> u64 {
        match (self.first, self.last) {
            (Some(first), Some(last)) if last >= first => (last as u64) - (first as u64) + 1,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CounterRun;

    /// The counters a run of `frames` consecutive frames decodes.
    fn run_of(counters: impl IntoIterator<Item = u32>) -> CounterRun {
        let mut run = CounterRun::new();
        for counter in counters {
            run.record(counter);
        }
        run
    }

    #[test]
    fn a_consecutive_run_has_nothing_wrong_with_it() {
        let run = run_of(0..180);

        assert_eq!(run.decoded(), 180);
        assert_eq!(run.presented(), 180);
        assert_eq!(run.missing(), 0);
        assert_eq!(run.duplicated(), 0);
        assert_eq!(run.out_of_order(), 0);
        assert_eq!(run.first(), Some(0));
        assert_eq!(run.last(), Some(179));
    }

    #[test]
    fn a_counter_seen_twice_is_one_duplicate_and_not_a_gap() {
        let run = run_of([0, 1, 1, 2]);

        assert_eq!(run.duplicated(), 1);
        assert_eq!(run.missing(), 0);
        assert_eq!(run.out_of_order(), 0);
        assert_eq!(run.decoded(), 4, "a duplicate still decoded");
        assert_eq!(run.presented(), 3, "and the source still drew three");
    }

    #[test]
    fn a_counter_that_never_arrived_is_missing() {
        let run = run_of([0, 1, 5, 6]);

        assert_eq!(run.missing(), 3, "2, 3 and 4");
        assert_eq!(run.duplicated(), 0);
        assert_eq!(run.out_of_order(), 0);
    }

    /*
     * The two are told apart deliberately. A frame arriving late looks like a
     * gap followed by a backwards step, and counting the gap as well would
     * report one fault as two — which matters when the threshold for one is
     * "any at all" and for the other is "a few per cent".
     */
    #[test]
    fn a_counter_that_went_backwards_is_out_of_order_and_not_also_missing() {
        let run = run_of([0, 1, 2, 1]);

        assert_eq!(run.out_of_order(), 1);
        assert_eq!(run.missing(), 0);
        assert_eq!(run.duplicated(), 0);
    }

    /*
     * A recording held to half the source's rate is missing every other
     * counter, and that is the recorder doing what it was told. Nothing here
     * decides that; the caller compares `missing` against the step it asked
     * for, and this case exists so that the counting it relies on is the
     * counting it gets.
     */
    #[test]
    fn a_run_at_every_other_counter_is_missing_the_ones_in_between() {
        let run = run_of((0..180).step_by(2));

        assert_eq!(run.decoded(), 90);
        assert_eq!(run.presented(), 179, "counters 0 to 178 inclusive");
        assert_eq!(run.missing(), 89, "one between each pair");
        assert_eq!(run.duplicated(), 0);
        assert_eq!(run.out_of_order(), 0);
    }

    #[test]
    fn a_run_of_nothing_says_so_rather_than_dividing_by_it() {
        let run = CounterRun::new();

        assert_eq!(run.decoded(), 0);
        assert_eq!(run.presented(), 0);
        assert_eq!(run.first(), None);
        assert_eq!(run.last(), None);
    }

    /*
     * A single frame is one frame, not zero. `presented` is inclusive of both
     * ends, and an off-by-one here would make every ratio built on it wrong by
     * one frame in a run of one.
     */
    #[test]
    fn one_frame_is_a_span_of_one() {
        let run = run_of([7]);

        assert_eq!(run.decoded(), 1);
        assert_eq!(run.presented(), 1);
        assert_eq!(run.first(), Some(7));
        assert_eq!(run.last(), Some(7));
    }

    /*
     * The counter is a `u32` and the span is a `u64`. Widening after
     * subtracting would be a subtraction that can overflow in debug and wrap in
     * release; this is the case that would find it.
     */
    #[test]
    fn a_span_wider_than_a_u32_does_not_overflow() {
        let run = run_of([0, u32::MAX]);

        assert_eq!(run.presented(), u64::from(u32::MAX) + 1);
        assert_eq!(run.missing(), u64::from(u32::MAX) - 1);
    }
}
