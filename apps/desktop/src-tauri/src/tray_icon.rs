//! The four marks the tray icon can wear, drawn rather than shipped.
//!
//! # Why they are shapes and not four colours of the same dot
//!
//! AGENTS.md section 46 asks that state is never conveyed by colour alone, and a
//! notification-area icon is the hardest place in the application to honour
//! that: it is sixteen pixels wide and it has no label. So each state gets a
//! **different shape**, legible in a screenshot printed in black and white:
//!
//! ```text
//!   ┌───┐        ┌ ┐         ●         ┌─╲─┐
//!   │   │                              │  ╲│
//!   └───┘        └ ┘                   └───╲
//!  attached    connecting   recording  unavailable
//!   (idle)
//! ```
//!
//! Colour is carried as well — the recording mark is the design system's accent
//! — but it is the reinforcement, not the signal. The tooltip says the same
//! thing in words, and the first line of the menu says it again, so the state
//! reaches a screen reader as well as an eye.
//!
//! # Why they are drawn in code
//!
//! Four PNGs would be four binary files in a repository whose every other visual
//! decision is written down and checkable. These are twenty lines of geometry
//! with the reason for each shape beside it, they cost a few microseconds once
//! at startup, and a change to one is a diff somebody can read.
//!
//! # What is deliberately not attempted
//!
//! Matching the taskbar's theme. Windows draws the notification area dark by
//! default in both light and dark modes, but not always and not on every
//! version, so each mark is a light fill inside a dark outline — the "sticker"
//! treatment — which reads on either ground without asking which one it is.

use tauri::image::Image;

/// The side of the drawn image, in pixels.
///
/// 32 rather than 16: Windows scales a tray icon for the display's DPI, and
/// giving it more to scale down from is what stops a 150% display drawing a
/// blurred 16-pixel mark.
const SIZE: u32 = 32;

/// How many samples across each pixel is taken, for edges that are not stepped.
///
/// Four by four. The difference between this and no antialiasing at all is
/// visible at 16 pixels, and the cost is 16 predicate calls for each of 1,024
/// pixels, once.
const SUBSAMPLES: u32 = 4;

/// The centre of the image, in pixel coordinates.
const CENTRE: f64 = SIZE as f64 / 2.0;

/// The fill: near-white, so that the mark reads on a dark notification area.
const FILL: [u8; 3] = [0xf3, 0xf2, 0xf2];

/// The outline: near-black, so that the same mark reads on a light one.
const OUTLINE: [u8; 3] = [0x11, 0x11, 0x11];

/// The recording fill: the design system's accent, `--color-accent`.
const ACCENT: [u8; 3] = [0xec, 0x30, 0x13];

/// Which mark the tray is wearing.
///
/// One variant for each thing the link can be, and no more: a mark for a state
/// the application cannot be in would be a mark nobody ever sees, and a state
/// with no mark of its own would be drawn as something it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayMark {
    /// A recorder is attached and nothing is being recorded: an open square.
    Idle,
    /// A recording is running: a filled disc, the shape every recorder has used
    /// for the purpose since tape.
    Recording,
    /// Looking for a recorder, or waiting to try again: four corner brackets,
    /// an outline that has not closed.
    Connecting,
    /// No recorder, and nothing further will be tried: a struck-through square.
    Unavailable,
}

impl TrayMark {
    /// The mark, as an image the tray can wear.
    ///
    /// Drawn on each call. It happens when the state changes — a few times a
    /// session — so caching it would be a cache to keep correct for no
    /// measurable gain.
    pub(crate) fn image(self) -> Image<'static> {
        let mut canvas = vec![0_u8; (SIZE * SIZE * 4) as usize];

        match self {
            Self::Idle => {
                paint(&mut canvas, OUTLINE, |x, y| square_ring(x, y, 5.0, 13.0));
                paint(&mut canvas, FILL, |x, y| square_ring(x, y, 7.0, 11.0));
            }
            Self::Recording => {
                // The one mark whose fill carries the contrast by itself: the
                // accent measures 3.88:1 on a dark notification area and 3.79:1
                // on a light one, so it clears WCAG 1.4.11 on either. The
                // outline is still drawn, because the mark should be the same
                // object as the other three rather than the odd one out.
                // `every_mark_reads_on_a_light_ground_and_on_a_dark_one`
                // measures it rather than taking this comment's word.
                paint(&mut canvas, OUTLINE, |x, y| disc(x, y, 12.5));
                paint(&mut canvas, ACCENT, |x, y| disc(x, y, 10.0));
            }
            Self::Connecting => {
                paint(&mut canvas, OUTLINE, |x, y| {
                    square_ring(x, y, 5.0, 13.0) && corner(x, y, 4.0)
                });
                paint(&mut canvas, FILL, |x, y| {
                    square_ring(x, y, 7.0, 11.0) && corner(x, y, 6.0)
                });
            }
            Self::Unavailable => {
                paint(&mut canvas, OUTLINE, |x, y| {
                    square_ring(x, y, 5.0, 13.0) || (inside_square(x, y, 13.0) && slash(x, y, 3.4))
                });
                paint(&mut canvas, FILL, |x, y| {
                    square_ring(x, y, 7.0, 11.0) || (inside_square(x, y, 11.0) && slash(x, y, 1.8))
                });
            }
        }

        Image::new_owned(canvas, SIZE, SIZE)
    }
}

/// Composites `colour` over the canvas wherever `inside` says it belongs.
///
/// Coverage is the fraction of a pixel's subsamples inside the shape, which is
/// what turns a hard predicate into an edge that does not look like a staircase.
/// The blend is ordinary source-over with straight alpha, because that is what
/// [`Image::new_owned`] takes.
fn paint(canvas: &mut [u8], colour: [u8; 3], inside: impl Fn(f64, f64) -> bool) {
    for y in 0..SIZE {
        for x in 0..SIZE {
            let coverage = coverage_at(x, y, &inside);
            if coverage <= 0.0 {
                continue;
            }

            let pixel = ((y * SIZE + x) * 4) as usize;
            let below = f64::from(canvas[pixel + 3]) / 255.0;
            let alpha = coverage + below * (1.0 - coverage);
            if alpha <= 0.0 {
                continue;
            }

            for channel in 0..3 {
                let under = f64::from(canvas[pixel + channel]);
                let over = f64::from(colour[channel]);
                canvas[pixel + channel] =
                    ((over * coverage + under * below * (1.0 - coverage)) / alpha).round() as u8;
            }
            canvas[pixel + 3] = (alpha * 255.0).round() as u8;
        }
    }
}

/// What fraction of one pixel is inside the shape.
fn coverage_at(x: u32, y: u32, inside: &impl Fn(f64, f64) -> bool) -> f64 {
    let step = 1.0 / f64::from(SUBSAMPLES);
    let mut hits = 0_u32;

    for row in 0..SUBSAMPLES {
        for column in 0..SUBSAMPLES {
            let sample_x = f64::from(x) + (f64::from(column) + 0.5) * step;
            let sample_y = f64::from(y) + (f64::from(row) + 0.5) * step;
            if inside(sample_x - CENTRE, sample_y - CENTRE) {
                hits += 1;
            }
        }
    }

    f64::from(hits) / f64::from(SUBSAMPLES * SUBSAMPLES)
}

/// Inside a square of half-width `half`, centred.
fn inside_square(x: f64, y: f64, half: f64) -> bool {
    x.abs() <= half && y.abs() <= half
}

/// Between two centred squares: the outline of one.
fn square_ring(x: f64, y: f64, inner: f64, outer: f64) -> bool {
    inside_square(x, y, outer) && !inside_square(x, y, inner)
}

/// Inside a centred disc.
fn disc(x: f64, y: f64, radius: f64) -> bool {
    x.mul_add(x, y * y) <= radius * radius
}

/// In one of the four corners rather than along the middle of an edge.
///
/// What turns a closed square into four brackets.
fn corner(x: f64, y: f64, from: f64) -> bool {
    x.abs() >= from && y.abs() >= from
}

/// Within `half` of the leading diagonal.
fn slash(x: f64, y: f64, half: f64) -> bool {
    (x - y).abs() <= half * std::f64::consts::SQRT_2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every mark, for the tests that have to walk all of them.
    const EVERY_MARK: &[TrayMark] = &[
        TrayMark::Idle,
        TrayMark::Recording,
        TrayMark::Connecting,
        TrayMark::Unavailable,
    ];

    /// The opaque pixels of a mark, as a bitmap of which pixels are drawn at
    /// all — the only thing a person can tell apart at sixteen pixels without
    /// looking at colour.
    fn silhouette(mark: TrayMark) -> Vec<bool> {
        let image = mark.image();
        image
            .rgba()
            .chunks_exact(4)
            .map(|pixel| pixel[3] > 128)
            .collect()
    }

    #[test]
    fn every_mark_is_a_different_shape_and_not_only_a_different_colour() {
        // AGENTS.md section 46, at the one place in the application where a
        // state has nowhere to be written down. Comparing silhouettes rather
        // than images is the whole point: two marks that differed only in hue
        // would compare equal here, which is exactly the failure to catch.
        for (index, first) in EVERY_MARK.iter().enumerate() {
            for second in &EVERY_MARK[index + 1..] {
                assert_ne!(
                    silhouette(*first),
                    silhouette(*second),
                    "{first:?} and {second:?} are the same shape, so a colour-blind user, a \
                     greyscale screenshot and a high-contrast theme cannot tell them apart"
                );
            }
        }
    }

    #[test]
    fn every_mark_is_the_size_the_tray_was_promised_and_draws_something() {
        for mark in EVERY_MARK {
            let image = mark.image();
            assert_eq!((image.width(), image.height()), (SIZE, SIZE), "{mark:?}");
            assert_eq!(image.rgba().len(), (SIZE * SIZE * 4) as usize, "{mark:?}");

            let drawn = silhouette(*mark).into_iter().filter(|on| *on).count();
            assert!(
                drawn > 40,
                "{mark:?} draws {drawn} pixels, which is not a mark anybody can see"
            );
        }
    }

    /// Windows' dark notification area.
    const DARK_TASKBAR: [u8; 3] = [0x20, 0x20, 0x20];

    /// Windows' light one.
    const LIGHT_TASKBAR: [u8; 3] = [0xf3, 0xf3, 0xf3];

    /// WCAG 2.1 relative luminance.
    fn luminance(colour: [u8; 3]) -> f64 {
        let channel = |value: u8| {
            let fraction = f64::from(value) / 255.0;
            if fraction <= 0.039_28 {
                fraction / 12.92
            } else {
                ((fraction + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(colour[0]) + 0.7152 * channel(colour[1]) + 0.0722 * channel(colour[2])
    }

    /// WCAG 2.1 contrast ratio between two colours.
    fn contrast(first: [u8; 3], second: [u8; 3]) -> f64 {
        let (a, b) = (luminance(first), luminance(second));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn every_mark_reads_on_a_light_ground_and_on_a_dark_one() {
        // A tray icon is not given a choice of ground: Windows draws the
        // notification area dark by default and light on some machines and
        // versions. So every mark has to carry *something* that clears WCAG
        // 1.4.11's 3:1 against each — which is the whole purpose of drawing a
        // light fill inside a dark outline, and it is measured here rather than
        // asserted, the way `packages/ui/src/contrast.test.ts` does for the
        // window.
        for mark in EVERY_MARK {
            let image = mark.image();
            let opaque: Vec<[u8; 3]> = image
                .rgba()
                .chunks_exact(4)
                .filter(|pixel| pixel[3] > 200)
                .map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect();

            for (ground, name) in [(DARK_TASKBAR, "dark"), (LIGHT_TASKBAR, "light")] {
                let best = opaque
                    .iter()
                    .map(|pixel| contrast(*pixel, ground))
                    .fold(0.0_f64, f64::max);
                assert!(
                    best >= 3.0,
                    "{mark:?} measures {best:.2}:1 at best against a {name} taskbar, so it is not \
                     reliably visible there"
                );
            }
        }
    }
}
