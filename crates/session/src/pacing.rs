//! Holding a recording to the frame rate it was asked for.
//!
//! A capture backend produces a frame whenever the source's content changes,
//! which has nothing to do with the rate the recording was asked for: a game at
//! 144 frames a second hands over 144 of them, and a recording made at
//! `--framerate 60` has to choose 60. Encoding all 144 would produce a file
//! nearly two and a half times the size the user asked for, at a rate the
//! settings said nothing about — and it would cost the game the GPU time to do
//! it.
//!
//! So a frame is either admitted or skipped, here, before it is submitted. That
//! is deliberate: skipping a frame *before* encoding costs the recording one
//! frame, while dropping an encoded packet would corrupt every later frame that
//! referenced it. What is skipped is counted and reported
//! ([`crate::RecordingReport`]).
//!
//! Nothing is ever *added*. A source slower than the requested rate produces a
//! recording at the source's rate, with the real intervals between frames in
//! the timestamps, rather than duplicated frames padding it out to a nominal
//! one — the container stores a timestamp per frame and a player honours it.

use crate::error::SessionError;

/// Nanoseconds in a second.
const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// How much early a frame may be and still count as being on time, as a
/// fraction of the interval.
///
/// A source running at exactly the requested rate does not deliver frames at
/// exactly the requested interval: a compositor's timestamps jitter by a
/// fraction of a millisecond either way, and without a margin every second
/// frame of a 60 fps source recorded at 60 fps would arrive a hair early and be
/// thrown away — halving the frame rate of the most ordinary case there is.
///
/// An eighth of an interval is 2.1 ms at 60 fps, which is far above the jitter
/// measured on this project's machines and far below half an interval, so it
/// cannot admit two frames where one was asked for.
const EARLY_TOLERANCE: i64 = 8;

/// Decides which captured frames are encoded, so that a recording keeps to the
/// rate it was asked for.
///
/// Stateful, and deliberately not `Copy`: it carries the deadline for the next
/// frame. One gate belongs to one recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameGate {
    interval: i64,
    tolerance: i64,
    next_deadline: Option<i64>,
}

impl FrameGate {
    /// A gate admitting `framerate` frames a second.
    ///
    /// # Errors
    ///
    /// [`SessionError::ZeroFramerate`], because a recording of no frames a
    /// second is not a recording and the alternative is a division by zero.
    pub fn new(framerate: u32) -> Result<Self, SessionError> {
        if framerate == 0 {
            return Err(SessionError::ZeroFramerate);
        }
        let interval = NANOS_PER_SECOND / i64::from(framerate);
        Ok(Self {
            interval,
            tolerance: interval / EARLY_TOLERANCE,
            next_deadline: None,
        })
    }

    /// Whether the frame at `media_nanos` should be encoded.
    ///
    /// `media_nanos` is the frame's position on the recording's timeline, from
    /// [`clipped_capture::CaptureClock`]. The first frame is always admitted:
    /// a recording starts at its first frame whatever the rate.
    ///
    /// The deadline advances by one interval per admitted frame rather than
    /// being measured from the frame that was admitted, so a source that
    /// delivers slightly early does not drag the whole recording early. When
    /// the source falls behind — a game that stopped drawing for a second —
    /// the deadline is resynchronised to the frame that arrived, rather than a
    /// burst of frames being admitted to catch up on a schedule nobody kept.
    pub fn admit(&mut self, media_nanos: i64) -> bool {
        let Some(deadline) = self.next_deadline else {
            self.next_deadline = Some(media_nanos.saturating_add(self.interval));
            return true;
        };

        if media_nanos.saturating_add(self.tolerance) < deadline {
            return false;
        }

        let advanced = deadline.saturating_add(self.interval);
        self.next_deadline = Some(if advanced <= media_nanos {
            media_nanos.saturating_add(self.interval)
        } else {
            advanced
        });
        true
    }

    /// The interval between frames at the requested rate, in nanoseconds.
    #[must_use]
    pub const fn interval_nanos(&self) -> i64 {
        self.interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `source_fps` frames a second for `seconds` through a gate set to
    /// `target_fps`, and returns how many were admitted.
    fn admitted(source_fps: u32, target_fps: u32, seconds: u32) -> u32 {
        let mut gate = FrameGate::new(target_fps).expect("a real frame rate");
        let step = NANOS_PER_SECOND / i64::from(source_fps);
        (0..source_fps * seconds)
            .filter(|frame| gate.admit(i64::from(*frame) * step))
            .count() as u32
    }

    #[test]
    fn zero_frames_a_second_is_refused_rather_than_dividing_by_it() {
        assert!(matches!(
            FrameGate::new(0),
            Err(SessionError::ZeroFramerate)
        ));
    }

    #[test]
    fn a_source_slower_than_the_requested_rate_keeps_every_frame() {
        // The video pattern application at 30 fps, recorded at the default 60.
        // Throwing any of these away would be inventing a shortfall.
        assert_eq!(admitted(30, 60, 10), 300);
    }

    #[test]
    fn a_source_at_the_requested_rate_keeps_every_frame() {
        // The case a naive "is it a whole interval since the last one?" check
        // halves: the frames arrive at exactly the interval, so half of them
        // are a rounding error early.
        assert_eq!(admitted(60, 60, 10), 600);
    }

    #[test]
    fn jitter_around_the_requested_rate_does_not_halve_the_recording() {
        // Real timestamps wander. A 60 fps source with a millisecond of jitter
        // either way must still record 60 frames a second.
        let mut gate = FrameGate::new(60).expect("a real frame rate");
        let jitter = [900_000_i64, -700_000, 250_000, -1_000_000, 0, 640_000];
        let step = NANOS_PER_SECOND / 60;
        let kept = (0..600)
            .filter(|frame| {
                let jitter = jitter[*frame as usize % jitter.len()];
                gate.admit(i64::from(*frame) * step + jitter)
            })
            .count();
        assert_eq!(kept, 600);
    }

    #[test]
    fn a_faster_source_is_held_to_the_requested_rate() {
        // A 144 fps game recorded at 60. The count is not exactly 600, because
        // the source's frames do not land on the requested rate's deadlines;
        // what matters is that it is near 60 a second rather than near 144.
        let kept = admitted(144, 60, 10);
        assert!(
            (580..=620).contains(&kept),
            "144 fps recorded at 60 should keep about 600 frames in ten seconds, kept {kept}"
        );
    }

    #[test]
    fn a_gap_in_the_source_does_not_produce_a_burst_afterwards() {
        // A game that stops drawing for five seconds — a loading screen, an
        // alt-tab — and then resumes. Admitting three hundred frames in a row
        // to catch up on a schedule nobody kept would be a spike in encoder
        // load exactly when the game came back.
        let mut gate = FrameGate::new(60).expect("a real frame rate");
        assert!(gate.admit(0));
        assert!(gate.admit(5 * NANOS_PER_SECOND));

        let step = NANOS_PER_SECOND / 60;
        let after = (1..=10)
            .filter(|frame| gate.admit(5 * NANOS_PER_SECOND + i64::from(*frame) * step))
            .count();
        assert_eq!(
            after, 10,
            "after the gap the source is back at the requested rate and every frame counts"
        );
    }

    #[test]
    fn the_first_frame_is_always_admitted() {
        // Whatever the rate, a recording starts at its first frame. A gate
        // that waited for a deadline would lose the moment the user pressed
        // the key.
        for framerate in [1, 30, 60, 480] {
            let mut gate = FrameGate::new(framerate).expect("a real frame rate");
            assert!(gate.admit(4_242_424_242), "at {framerate} fps");
        }
    }
}
