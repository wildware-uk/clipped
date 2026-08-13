//! The mark the tray icon wears: Clipped's brand mark, with the state of the
//! link to the recorder badged onto it. Drawn rather than shipped.
//!
//! # Why the state is a badge and not four unrelated shapes
//!
//! The notification area is where this application sits all day, so it is where
//! it should be recognisable — the same mark as the installer, the window and
//! the taskbar, rather than a private vocabulary of squares and brackets that
//! means nothing to somebody who has just installed it.
//!
//! State still has to be visible, and AGENTS.md section 46 asks that it is never
//! carried by colour alone. The notification area is the hardest place in the
//! application to honour that: sixteen pixels wide, and no label. So the state
//! rides on top of the mark instead of replacing it, as a badge whose *shape* is
//! the signal:
//!
//! ```text
//!      idle          recording        connecting        unavailable
//!    no badge      a filled disc     a ring, open        a slash,
//!                                    in the middle     struck through
//! ```
//!
//! Every one of those survives being printed in black and white. The filled disc
//! and the ring are the close pair, and they differ where it counts whatever the
//! hue: the ring has a light hole and the disc does not.
//! `every_mark_is_a_different_shape_and_not_only_a_different_colour` measures
//! that rather than trusting this paragraph. Colour is carried as well — the
//! recording badge is the design system's accent — but it is the reinforcement,
//! not the signal. The tooltip says the same thing in words, and the first line
//! of the menu says it again, so the state reaches a screen reader as well as an
//! eye.
//!
//! Idle has no badge on purpose. A badge means something is going on, and a mark
//! that is badged even at rest has nothing left to say when something is.
//!
//! # Why three bars and not the application icon's five
//!
//! `icons/source.png` draws five waveform bars. At sixteen pixels five bars are
//! five one-pixel columns separated by gaps narrower than a pixel, and they
//! resolve into a smear; three keep bar–gap–bar apart at that size. Both were
//! rendered at 16 px before the count was chosen. The mark stays recognisable
//! because what identifies it is the accent disc and the tall centre bar, not
//! how many bars flank it.
//!
//! # Why it is drawn in code
//!
//! A PNG per state would be four binary files in a repository whose every other
//! visual decision is written down and checkable. This is geometry with the
//! reason for each number beside it, it costs a few microseconds once at
//! startup, and a change to it is a diff somebody can read.
//!
//! # What is deliberately not attempted
//!
//! Matching the taskbar's theme. Windows draws the notification area dark by
//! default in both light and dark modes, but not always and not on every
//! version, so the mark brings its own ground and the badge is a light face
//! inside a dark ring — which reads on either without asking which one it is.

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
///
/// Also the half-width of the mark's ground, which fills the canvas.
const CENTRE: f64 = SIZE as f64 / 2.0;

/// The fill: near-white, so that the mark reads on a dark notification area.
///
/// The brand mark's bars and the badge's face, which are the same colour
/// because they are the same ink.
const FILL: [u8; 3] = [0xf3, 0xf2, 0xf2];

/// The outline: near-black, so that the same mark reads on a light one.
const OUTLINE: [u8; 3] = [0x11, 0x11, 0x11];

/// The recording fill: the design system's accent, `--color-accent`.
const ACCENT: [u8; 3] = [0xec, 0x30, 0x13];

/// The ground the brand mark is drawn on: `--color-neutral-900`.
const GROUND: [u8; 3] = [0x2d, 0x2b, 0x2b];

/// The corner radius of the ground, which fills the canvas.
const GROUND_CORNER: f64 = 7.0;

/// The radius of the accent disc the bars stand in.
const DISC_RADIUS: f64 = 10.2;

/// The half-heights of the waveform bars, left to right.
///
/// Three of them, where `icons/source.png` has five; the module header explains
/// why the count differs from the application icon's.
const BAR_HALF_HEIGHTS: [f64; 3] = [4.2, 8.4, 5.4];

/// How wide each bar is.
const BAR_WIDTH: f64 = 2.6;

/// How much clear ground separates one bar from the next.
const BAR_GAP: f64 = 1.8;

/// How far right and down of the centre the badge sits.
///
/// Bottom right, the corner Windows badges in itself, and far enough out that
/// the badge overhangs the ground's rounded corner instead of sitting inside the
/// artwork — which is what leaves the disc and the bars readable underneath it.
const BADGE_OFFSET: f64 = 7.9;

/// The outer radius of the badge's dark ring.
///
/// The ring is what separates the badge from the mark it sits on, and from a
/// light taskbar behind the corner it overhangs.
const BADGE_RING_RADIUS: f64 = 8.8;

/// The radius of the badge's light face, inside that ring.
const BADGE_FACE_RADIUS: f64 = 7.5;

/// Which mark the tray is wearing.
///
/// One variant for each thing the link can be, and no more: a mark for a state
/// the application cannot be in would be a mark nobody ever sees, and a state
/// with no mark of its own would be drawn as something it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayMark {
    /// A recorder is attached and nothing is being recorded: the brand mark on
    /// its own, unbadged.
    Idle,
    /// A recording is running: a filled disc, the shape every recorder has used
    /// for the purpose since tape.
    Recording,
    /// Looking for a recorder, or waiting to try again: a ring, which is the
    /// recording disc with its middle not yet filled in.
    Connecting,
    /// No recorder, and nothing further will be tried: a slash, struck through
    /// the badge.
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

        paint_brand_mark(&mut canvas);

        // The badge's ring and face are the same on every state that has one, so
        // that the eye only has to read the shape in the middle to tell them
        // apart. Idle has no badge at all.
        if self != Self::Idle {
            paint(&mut canvas, OUTLINE, |x, y| {
                badge_disc(x, y, BADGE_RING_RADIUS)
            });
            paint(&mut canvas, FILL, |x, y| {
                badge_disc(x, y, BADGE_FACE_RADIUS)
            });
        }

        match self {
            Self::Idle => {}
            Self::Recording => {
                // The one shape whose fill carries the contrast by itself: the
                // accent measures 3.88:1 on a dark notification area and 3.79:1
                // on a light one, so it clears WCAG 1.4.11 on either — and here
                // it also sits on the badge's near-white face, which is a much
                // wider margin than that.
                // `every_mark_reads_on_a_light_ground_and_on_a_dark_one`
                // measures it rather than taking this comment's word.
                paint(&mut canvas, ACCENT, |x, y| badge_disc(x, y, 5.1));
            }
            Self::Connecting => {
                // A ring rather than a filled disc, so that this and Recording
                // differ in the middle and not only in colour.
                paint(&mut canvas, OUTLINE, |x, y| {
                    badge_disc(x, y, 5.4) && !badge_disc(x, y, 2.4)
                });
            }
            Self::Unavailable => {
                paint(&mut canvas, OUTLINE, |x, y| badge_slash(x, y, 5.1, 1.5));
            }
        }

        Image::new_owned(canvas, SIZE, SIZE)
    }
}

/// Paints the brand mark, full bleed: the ground, the accent disc, the bars.
///
/// The base of every state, so that the tray always shows the same object and
/// only what is badged onto it changes.
fn paint_brand_mark(canvas: &mut [u8]) {
    paint(canvas, GROUND, |x, y| {
        rounded_rectangle(x, y, CENTRE, CENTRE, GROUND_CORNER)
    });
    paint(canvas, ACCENT, |x, y| disc(x, y, DISC_RADIUS));

    // The bars are centred as a group rather than on the tallest one, because
    // the tallest is not in the middle and centring on it would push the whole
    // waveform off the disc.
    let bars = BAR_HALF_HEIGHTS.len() as f64;
    let span = bars * BAR_WIDTH + (bars - 1.0) * BAR_GAP;
    let mut bar_centre = -span / 2.0 + BAR_WIDTH / 2.0;

    for half_height in BAR_HALF_HEIGHTS {
        let centre = bar_centre;
        paint(canvas, FILL, move |x, y| {
            // Corner radius half the bar's width, which rounds each end into a
            // semicircle rather than a stubby corner.
            rounded_rectangle(x - centre, y, BAR_WIDTH / 2.0, half_height, BAR_WIDTH / 2.0)
        });
        bar_centre += BAR_WIDTH + BAR_GAP;
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

/// Inside a centred rectangle whose corners are rounded to `radius`.
///
/// Outside the corner quadrants this is the plain rectangle; inside one, it is
/// the disc of that radius sitting in it. A `radius` equal to `half_width` gives
/// a capsule, which is how the bars get their rounded ends.
fn rounded_rectangle(x: f64, y: f64, half_width: f64, half_height: f64, radius: f64) -> bool {
    let past_x = x.abs() - (half_width - radius);
    let past_y = y.abs() - (half_height - radius);

    if past_x <= 0.0 || past_y <= 0.0 {
        return x.abs() <= half_width && y.abs() <= half_height;
    }

    past_x.mul_add(past_x, past_y * past_y) <= radius * radius
}

/// Inside a centred disc.
fn disc(x: f64, y: f64, radius: f64) -> bool {
    x.mul_add(x, y * y) <= radius * radius
}

/// Inside a disc centred on the badge rather than on the image.
fn badge_disc(x: f64, y: f64, radius: f64) -> bool {
    disc(x - BADGE_OFFSET, y - BADGE_OFFSET, radius)
}

/// A stroke across the badge, from its lower left to its upper right.
///
/// `half_width` is measured perpendicular to the stroke; `reach` is how far it
/// runs from the badge's centre in each of x and y, so the stroke is the
/// diagonal of a square `reach` on each side.
fn badge_slash(x: f64, y: f64, reach: f64, half_width: f64) -> bool {
    let (x, y) = (x - BADGE_OFFSET, y - BADGE_OFFSET);

    // `x + y` is the perpendicular distance from the anti-diagonal, times the
    // diagonal of the unit square.
    (x + y).abs() <= half_width * std::f64::consts::SQRT_2 && inside_square(x, y, reach)
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

    /// Windows' dark notification area.
    const DARK_TASKBAR: [u8; 3] = [0x20, 0x20, 0x20];

    /// Windows' light one.
    const LIGHT_TASKBAR: [u8; 3] = [0xf3, 0xf3, 0xf3];

    /// Both of them, for the tests that have to hold on either.
    const EVERY_TASKBAR: &[([u8; 3], &str)] = &[(DARK_TASKBAR, "dark"), (LIGHT_TASKBAR, "light")];

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

    /// The opaque pixels of a mark, as a bitmap of which pixels are drawn at all.
    fn silhouette(mark: TrayMark) -> Vec<bool> {
        let image = mark.image();
        image
            .rgba()
            .chunks_exact(4)
            .map(|pixel| pixel[3] > 128)
            .collect()
    }

    /// Where ink stops and paper starts, as WCAG relative luminance.
    ///
    /// Not a knife edge for anything drawn: measured on this module's own
    /// colours, the near-white fill is 0.890, the accent 0.200, the neutral-900
    /// ground 0.025 and the near-black outline 0.006, so every colour in every
    /// mark sits at least 0.25 clear of this line. A threshold placed near the
    /// accent would make [`printed`] depend on rounding rather than on shape.
    const INK: f64 = 0.45;

    /// A mark as it survives being printed in black and white on `ground`: one
    /// bit per pixel, ink or paper.
    ///
    /// This, and not the alpha mask, is what the marks have to differ in. Every
    /// mark is now the same full-bleed brand mark with something badged onto it,
    /// so their alpha masks are identical or nearly so by construction, and a
    /// silhouette can no longer tell them apart. What a colour-blind user, a
    /// greyscale screenshot and a high-contrast theme are left with is
    /// luminance, which is what this measures.
    ///
    /// The mark is composited onto `ground` first, because it has transparent
    /// corners and a badge that overhangs one of them, and what shows through
    /// there is the taskbar rather than black.
    fn printed(mark: TrayMark, ground: [u8; 3]) -> Vec<bool> {
        let image = mark.image();
        image
            .rgba()
            .chunks_exact(4)
            .map(|pixel| {
                let alpha = f64::from(pixel[3]) / 255.0;
                let over = |channel: usize| {
                    f64::from(pixel[channel])
                        .mul_add(alpha, f64::from(ground[channel]) * (1.0 - alpha))
                        .round() as u8
                };
                luminance([over(0), over(1), over(2)]) < INK
            })
            .collect()
    }

    /// How many pixels of a mark have to print differently before the two marks
    /// count as tellable apart.
    ///
    /// Eight, of 1,024. A one- or two-pixel difference is an antialiasing
    /// accident rather than a shape anybody can see, and demanding only that the
    /// two bitmaps are not identical would accept one: the alpha silhouettes
    /// this test used to compare separated Idle from the other three by exactly
    /// two pixels, which is the margin this number exists to reject. The closest
    /// real pair is Recording against Connecting, which differ by the light
    /// middle of the ring — 27 pixels, on either taskbar.
    const TELLABLE_APART: usize = 8;

    #[test]
    fn every_mark_is_a_different_shape_and_not_only_a_different_colour() {
        // AGENTS.md section 46, at the one place in the application where a
        // state has nowhere to be written down. Comparing the marks printed in
        // black and white rather than in colour is the whole point: two marks
        // that differed only in hue would compare equal here, which is exactly
        // the failure to catch. Both taskbars are checked because the marks have
        // transparent corners, and what shows through them is not the same
        // colour on both.
        for (ground, name) in EVERY_TASKBAR {
            for (index, first) in EVERY_MARK.iter().enumerate() {
                for second in &EVERY_MARK[index + 1..] {
                    let differing = printed(*first, *ground)
                        .into_iter()
                        .zip(printed(*second, *ground))
                        .filter(|(ink_in_first, ink_in_second)| ink_in_first != ink_in_second)
                        .count();

                    assert!(
                        differing >= TELLABLE_APART,
                        "{first:?} and {second:?} print alike on a {name} taskbar — {differing} \
                         pixels of {} differ, under the {TELLABLE_APART} it takes to be a \
                         different shape — so a colour-blind user, a greyscale screenshot and a \
                         high-contrast theme cannot tell them apart",
                        SIZE * SIZE
                    );
                }
            }
        }
    }

    #[test]
    fn idle_wears_the_brand_mark_and_nothing_else() {
        // The resting state is the mark on its own. Nothing above asserts this:
        // a badged Idle would still be a different shape from the other three,
        // so `every_mark_is_a_different_shape_and_not_only_a_different_colour`
        // would go on passing while the tray flagged a state that is not
        // happening.
        let mut unbadged = vec![0_u8; (SIZE * SIZE * 4) as usize];
        paint_brand_mark(&mut unbadged);

        assert_eq!(
            TrayMark::Idle.image().rgba(),
            unbadged,
            "Idle has something drawn over the brand mark; at rest there is \
             nothing to flag, and a mark that is badged even when idle has \
             nothing left to say when a recording starts"
        );
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

    #[test]
    fn every_mark_reads_on_a_light_ground_and_on_a_dark_one() {
        // A tray icon is not given a choice of ground: Windows draws the
        // notification area dark by default and light on some machines and
        // versions. So every mark has to carry *something* that clears WCAG
        // 1.4.11's 3:1 against each — which is the whole purpose of a near-white
        // waveform on a near-black ground, and it is measured here rather than
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

            for (ground, name) in EVERY_TASKBAR {
                let best = opaque
                    .iter()
                    .map(|pixel| contrast(*pixel, *ground))
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
