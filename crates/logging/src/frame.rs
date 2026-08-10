//! Guard rails for logging on capture threads.
//!
//! A capture thread runs once per frame — 240 times a second on a high refresh
//! rate monitor — and AGENTS.md sections 18 and 35 rule out both per-frame log
//! spam and diagnostics that cost something when they are switched off. The two
//! mechanisms here cover the two things a contributor actually wants:
//!
//! - [`trace_frame!`] for detail that is only ever useful while debugging the
//!   capture loop itself. It is behind the `frame-tracing` feature, so in a
//!   normal build the call is a branch on a `const false` that the optimiser
//!   removes entirely.
//! - [`FrameSampler`] for detail that is worth having occasionally in a normal
//!   build. It reduces a per-frame event to one every *n* frames using a plain
//!   counter: no allocation, no lock, no clock read.
//!
//! `crates/logging/tests/hot_loop_cost.rs` measures what [`trace_frame!`]
//! costs when it is switched off, and `tests/frame_tracing.rs` proves that its
//! arguments are not evaluated. `docs/logging.md` records the numbers and
//! explains which of the two mechanisms to reach for.

use std::num::NonZeroU32;

/// Whether per-frame tracing was compiled in.
///
/// `false` unless the crate's `frame-tracing` feature is enabled. [`trace_frame!`]
/// branches on this constant, which means the disabled form is still type
/// checked — a `trace_frame!` call cannot rot — while costing nothing at
/// runtime.
pub const FRAME_TRACING: bool = cfg!(feature = "frame-tracing");

/// Emits a `TRACE` event only in builds with the `frame-tracing` feature.
///
/// Takes exactly the same arguments as [`tracing::trace!`]. Use it for anything
/// that would run once per frame or once per audio packet:
///
/// ```
/// # let frame_index = 0_u64;
/// # let presentation_time = std::time::Duration::ZERO;
/// clipped_logging::trace_frame!(
///     frame_index,
///     presentation_time_us = presentation_time.as_micros() as u64,
///     "frame acquired"
/// );
/// ```
///
/// Enable it for a debugging session. The feature belongs to this package, so
/// the switch is spelled against a package that depends on it — today only this
/// one, because no binary has taken the dependency yet:
///
/// ```text
/// cargo test -p clipped-logging --features frame-tracing
/// ```
///
/// Once a binary depends on `clipped-logging`, the form for running it is
/// `cargo run -p clipped-recorder --features clipped-logging/frame-tracing`.
/// See the "Logging from a capture thread" section of `docs/logging.md`.
///
/// Note that the arguments are not evaluated in a normal build, so they must
/// not be relied on for side effects.
#[macro_export]
macro_rules! trace_frame {
    ($($argument:tt)*) => {
        if $crate::FRAME_TRACING {
            $crate::__macro_support::tracing::trace!($($argument)*);
        }
    };
}

/// Reduces a per-frame event to one event every *n* frames.
///
/// For diagnostics worth keeping in a normal build — a resolution change is
/// noticed per frame but only worth reporting occasionally, a dropped frame
/// counter is worth summarising rather than reporting individually.
///
/// Deliberately `&mut self` and not shareable: it belongs to the thread that
/// owns the loop, so sampling never introduces synchronisation onto a capture
/// thread (AGENTS.md section 20).
///
/// ```
/// use std::num::NonZeroU32;
/// use clipped_logging::FrameSampler;
///
/// let mut sampler = FrameSampler::every(NonZeroU32::new(600).unwrap());
/// # let mut logged = 0;
/// for _frame in 0..1_800 {
///     if sampler.should_log() {
///         # logged += 1;
///         tracing::debug!("capture loop still running");
///     }
/// }
/// # assert_eq!(logged, 3);
/// ```
#[derive(Debug, Clone)]
pub struct FrameSampler {
    interval: NonZeroU32,
    counter: u32,
}

impl FrameSampler {
    /// Samples one frame in `interval`. The first call to
    /// [`should_log`](Self::should_log) returns `true`, so a loop reports
    /// itself immediately rather than after the first full interval.
    #[must_use]
    pub fn every(interval: NonZeroU32) -> Self {
        Self {
            interval,
            counter: 0,
        }
    }

    /// Advances the sampler and reports whether this frame is the one to log.
    pub fn should_log(&mut self) -> bool {
        let sample = self.counter == 0;
        self.counter += 1;
        if self.counter >= self.interval.get() {
            self.counter = 0;
        }
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_the_first_frame_and_then_every_interval() {
        let mut sampler = FrameSampler::every(NonZeroU32::new(4).unwrap());
        let sampled: Vec<bool> = (0..9).map(|_| sampler.should_log()).collect();

        assert_eq!(
            sampled,
            [true, false, false, false, true, false, false, false, true]
        );
    }

    #[test]
    fn an_interval_of_one_samples_every_frame() {
        let mut sampler = FrameSampler::every(NonZeroU32::new(1).unwrap());
        assert!((0..5).all(|_| sampler.should_log()));
    }

    #[test]
    fn frame_tracing_is_off_unless_the_feature_is_enabled() {
        assert_eq!(FRAME_TRACING, cfg!(feature = "frame-tracing"));
    }
}
