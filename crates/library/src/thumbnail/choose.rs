//! Which frame of a recording becomes its thumbnail.
//!
//! # Why not the first frame
//!
//! The first frame of a Clipped recording is the moment capture attached to the
//! game's window, which is a loading screen, a black fade, an anti-cheat splash
//! or a publisher logo far more often than it is the game. A library whose tiles
//! are mostly black rectangles is a library nobody can scan visually, which is
//! the entire job a thumbnail has.
//!
//! # The rule
//!
//! Sample a few frames spread through the recording and keep the one with the
//! most **variety** in its luma.
//!
//! - Where to look: [`candidate_offsets`] — a fixed set of fractions of the
//!   duration, never the first or last moment.
//! - What to prefer: [`score`] — one minus the share of sampled pixels that fall
//!   in the largest of [`BINS`] brightness bins.
//!
//! A black loading screen puts every sample in one bin and scores 0. A fade to
//! white does the same. A frame of a game — any game, bright or dark — spreads
//! its pixels across several bins and scores well above both. That is the whole
//! of the rule, and it is deliberately not cleverer than that: face detection,
//! motion analysis and scene-change detection all cost more decoding than the
//! thing they are choosing between is worth.
//!
//! # What it costs
//!
//! Sampling is over a fixed grid of about [`SAMPLE_TARGET`] pixels, not over the
//! whole plane, so scoring a 4K frame costs the same as scoring a 720p one.
//! Candidates are keyframes: each one is a seek and, in the ordinary case, a
//! single decoded frame ([`FRAMES_PER_CANDIDATE`] is the bound when the first
//! one is blank). The measured total is in `docs/thumbnails.md`.

use core::time::Duration;

/// How many brightness bins a frame's luma is spread across.
///
/// Sixteen. Coarse enough that noise and compression do not turn a flat colour
/// into variety, fine enough that a dark scene with detail in it is separated
/// from a black one.
pub(super) const BINS: usize = 16;

/// Roughly how many pixels are sampled from a frame.
///
/// The grid is chosen to land near this number whatever the resolution, so the
/// cost of scoring does not grow with the picture. Four thousand samples put
/// about 256 in each of [`BINS`] bins when a frame is evenly spread, which is
/// far more than enough to tell "one colour" from "many".
pub(super) const SAMPLE_TARGET: usize = 4_096;

/// The score below which a frame is treated as blank.
///
/// A frame with 98% of its samples in one bin. Black, white, or a single flat
/// colour: nothing a person could recognise a recording by.
pub(super) const BLANK: f32 = 0.02;

/// The score at or above which a candidate is taken without looking further.
///
/// Reached by any ordinary frame of a game. Its only purpose is to stop decoding
/// extra frames at a candidate whose first frame is already a good picture.
pub(super) const GOOD_ENOUGH: f32 = 0.35;

/// How many frames are decoded at one candidate before moving to the next.
///
/// One in the ordinary case. The bound exists for the candidate that lands on a
/// black keyframe — a cut, a loading screen — where a few more frames may be
/// worth decoding, and it is small because the next candidate is a better bet
/// than the next frame.
pub(super) const FRAMES_PER_CANDIDATE: u32 = 4;

/// Where in a recording of `duration` frames are looked at, earliest first.
///
/// Fractions rather than fixed offsets, because a 20-second replay clip and a
/// four-hour session both have to be sampled somewhere representative. Never the
/// first moment, which is the loading screen; never past the last tenth of the
/// file, which is the end card, the scoreboard, or a truncated tail.
///
/// A recording whose duration the container does not declare, or which is too
/// short to sample, produces a single offset of zero: decode from the start and
/// take what comes.
pub(super) fn candidate_offsets(duration: Option<Duration>) -> Vec<Duration> {
    /// The fractions of the recording sampled, earliest first.
    const FRACTIONS: [f64; 3] = [0.10, 0.35, 0.60];
    /// Nothing is sampled beyond this fraction of the file.
    const LAST_FRACTION: f64 = 0.90;
    /// Below this, a recording is sampled from the start alone: seeking about
    /// inside a two-second clip finds the same keyframe three times.
    const TOO_SHORT: Duration = Duration::from_secs(2);

    let Some(duration) = duration.filter(|duration| *duration >= TOO_SHORT) else {
        return vec![Duration::ZERO];
    };

    let seconds = duration.as_secs_f64();
    let last = seconds * LAST_FRACTION;
    FRACTIONS
        .iter()
        .map(|fraction| Duration::from_secs_f64((seconds * fraction).min(last)))
        .collect()
}

/// How much variety there is in a frame's luma, between 0.0 and 1.0.
///
/// `plane` is the frame's luma plane, `stride` its row pitch in bytes — which is
/// larger than `width` on almost every decoder — and `width` and `height` the
/// picture's real size.
///
/// Zero means every sampled pixel had the same brightness: black, white, or a
/// flat colour. Higher means more of the [`BINS`] brightness bins were occupied
/// evenly, which is what a picture of something looks like.
pub(super) fn score(plane: &[u8], stride: usize, width: usize, height: usize) -> f32 {
    let mut histogram = [0u32; BINS];
    let mut samples = 0u32;

    // A grid, not a scan: one row in `step` and one column in `step`, chosen so
    // that the total lands near `SAMPLE_TARGET` whatever the resolution.
    let step = grid_step(width, height);
    let mut y = 0;
    while y < height {
        let row = y * stride;
        let mut x = 0;
        while x < width {
            let Some(luma) = plane.get(row + x) else {
                // A plane shorter than the geometry says. Nothing is assumed
                // about the rest of it.
                break;
            };
            histogram[usize::from(*luma) * BINS / 256] += 1;
            samples += 1;
            x += step;
        }
        y += step;
    }

    if samples == 0 {
        return 0.0;
    }
    let largest = histogram.iter().copied().max().unwrap_or(samples);
    1.0 - (largest as f32 / samples as f32)
}

/// The sampling interval, in pixels, that puts about [`SAMPLE_TARGET`] samples
/// in a `width` × `height` picture.
///
/// Never zero, and never so large that a small picture is sampled fewer than a
/// handful of times.
fn grid_step(width: usize, height: usize) -> usize {
    let pixels = width.saturating_mul(height);
    if pixels <= SAMPLE_TARGET {
        return 1;
    }
    // `step` samples one pixel in `step` per axis, so it divides the count by
    // `step²`; the interval that lands on the target is therefore the square
    // root of the ratio. `usize::isqrt` is exact, where `(x as f64).sqrt()`
    // would put a floating-point round trip in the middle of a size
    // calculation.
    (pixels / SAMPLE_TARGET).isqrt().max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture of `width` × `height` whose pixels come from `pixel`.
    fn picture(
        width: usize,
        height: usize,
        pixel: impl Fn(usize, usize) -> u8,
    ) -> (Vec<u8>, usize) {
        // A stride wider than the picture, as every real decoder produces, so
        // that a scorer reading `stride` as `width` fails these tests.
        let stride = width + 37;
        let mut plane = vec![0u8; stride * height];
        for y in 0..height {
            for x in 0..width {
                plane[y * stride + x] = pixel(x, y);
            }
        }
        (plane, stride)
    }

    #[test]
    fn a_black_loading_screen_scores_nothing_and_a_frame_of_a_game_scores_well() {
        // The failure this rule exists to prevent: a library of black tiles.
        let (black, stride) = picture(1_920, 1_080, |_, _| 0);
        assert_eq!(score(&black, stride, 1_920, 1_080), 0.0);

        // A fade to white is just as useless, and the rule has to reject it for
        // the same reason rather than by looking at brightness.
        let (white, stride) = picture(1_920, 1_080, |_, _| 255);
        assert_eq!(score(&white, stride, 1_920, 1_080), 0.0);

        // Something with structure in it: a gradient across the frame occupies
        // every bin evenly, which is the best a frame can do.
        let (gradient, stride) = picture(1_920, 1_080, |x, _| (x * 255 / 1_919) as u8);
        let scored = score(&gradient, stride, 1_920, 1_080);
        assert!(scored > GOOD_ENOUGH, "a detailed frame scored {scored}");
        assert!(scored > BLANK);
    }

    #[test]
    fn a_dark_frame_with_detail_beats_a_flat_bright_one() {
        // The reason the rule is variety and not brightness: a night scene in a
        // game is a good thumbnail and a flat grey screen is not, and a rule
        // that preferred bright frames would choose the wrong one.
        let (night, stride) = picture(640, 360, |x, y| ((x + y) % 64) as u8);
        let night_score = score(&night, stride, 640, 360);

        let (grey, stride) = picture(640, 360, |_, _| 128);
        let grey_score = score(&grey, stride, 640, 360);

        assert!(
            night_score > grey_score,
            "a dark detailed frame scored {night_score} and a flat grey one {grey_score}"
        );
        assert!(grey_score <= BLANK);
    }

    #[test]
    fn a_logo_on_black_is_recognised_as_nearly_blank() {
        // The other half of the loading screen problem: a publisher splash is
        // not literally black, and a rule that only rejected pure black would
        // take it. One percent of the frame lit is still a blank tile.
        let (splash, stride) = picture(1_000, 1_000, |x, y| {
            if x > 495 && x < 505 && y > 400 && y < 500 {
                200
            } else {
                0
            }
        });
        let scored = score(&splash, stride, 1_000, 1_000);
        assert!(
            scored < GOOD_ENOUGH,
            "a logo on black scored {scored}, which would be taken as a good frame"
        );
    }

    #[test]
    fn scoring_costs_the_same_at_any_resolution() {
        // The property the grid exists for: a 4K frame must not cost nine times
        // a 720p one to look at. Asserted on the sample count rather than on a
        // clock, which would be a flaky test on a shared machine.
        for (width, height) in [(1_280, 720), (1_920, 1_080), (3_840, 2_160)] {
            let step = grid_step(width, height);
            let samples = width.div_ceil(step) * height.div_ceil(step);
            assert!(
                samples <= SAMPLE_TARGET * 4,
                "{width}x{height} would sample {samples} pixels"
            );
            assert!(
                samples >= SAMPLE_TARGET / 4,
                "{width}x{height} would sample only {samples} pixels"
            );
        }
        // A thumbnail-sized picture is sampled in full rather than skipped over.
        assert_eq!(grid_step(64, 36), 1);
    }

    #[test]
    fn candidates_are_spread_through_the_recording_and_avoid_both_ends() {
        let offsets = candidate_offsets(Some(Duration::from_secs(600)));
        assert_eq!(
            offsets,
            vec![
                Duration::from_secs(60),
                Duration::from_secs(210),
                Duration::from_secs(360)
            ]
        );
        // Never the first moment, which is the loading screen, and never the
        // last, which may be a truncated tail.
        assert!(offsets[0] > Duration::ZERO);
        assert!(*offsets.last().expect("three offsets") < Duration::from_secs(540));
    }

    #[test]
    fn a_recording_with_no_declared_duration_is_read_from_the_start() {
        // A file still being written, or a container with no duration in its
        // header. Seeking into it is guesswork; decoding from the start is not.
        assert_eq!(candidate_offsets(None), vec![Duration::ZERO]);
        assert_eq!(
            candidate_offsets(Some(Duration::from_millis(900))),
            vec![Duration::ZERO]
        );
    }
}
