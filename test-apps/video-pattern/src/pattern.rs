//! The deterministic pattern: how it is drawn, and how it is read back.
//!
//! This module is the whole reason the test application exists. A capture test
//! needs to answer three questions about a frame it just captured — *which*
//! frame is this, *is it intact*, and *did the one before it go missing* — and
//! none of them can be answered by looking at a recording of a game, or by a
//! person looking at anything. So the subject draws the answers into its own
//! pixels, and this module is both halves of that: [`render`] writes them and
//! [`decode`] reads them, which is what makes the pair testable without a GPU
//! (see the tests at the bottom of this file, which run everywhere).
//!
//! # What a frame contains
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │ ▟▙▟▙▟▙▟▙  header row: magic, 32-bit counter, checksum   │  16 px
//! │                                                        │
//! │      ███  marker square, at x = f(counter)              │  64 px
//! │                                                        │
//! │  background: palette[counter % 8]                       │
//! └────────────────────────────────────────────────────────┘
//! ```
//!
//! - **A magic sequence**, four cells of fixed colours. It says "this is a
//!   Clipped test pattern" and, because [`locate`] can search for it, it is how
//!   a decoder finds the pattern inside a captured frame that is bigger than the
//!   pattern — a window capture includes the window's border and title bar, so
//!   the client area does not start at the top-left corner of the frame.
//! - **A 32-bit frame counter**, one cell per bit, least significant first,
//!   white for one and black for zero. This is the frame's identity: a test
//!   compares consecutive counters and knows exactly how many frames the source
//!   produced that it never saw (a drop) and whether it saw the same one twice
//!   (a duplicate).
//! - **An eight-bit checksum** of the counter. Without it a single misread cell
//!   turns frame 4 into frame 262148, and the test reports a fictional gap of a
//!   quarter of a million frames instead of a decode failure.
//! - **A background colour** taken from an eight-entry palette by the counter,
//!   and **a marker square** whose position is a function of the counter. Both
//!   are redundant with the counter, deliberately: they are what makes a *torn*
//!   frame — the top half from frame 900 and the bottom half from frame 901 —
//!   fail to decode instead of being read as a good frame 900. A capture
//!   pipeline that hands over a half-composed frame is exactly the sort of
//!   defect this application exists to catch, and a pattern that only encoded a
//!   counter in one corner could not see it.
//!
//! The last two cover each other, and it is worth knowing exactly where each
//! stops. The background changes every frame but repeats every eight, so on its
//! own it would miss a tear between frames exactly eight apart. The marker is 64
//! pixels wide and moves 8 pixels a frame, so the pixel a decoder samples is
//! still inside the marker when the frame is up to four frames out — but it is
//! well outside it by eight, and it only returns to the same x after the
//! marker's whole period, which is `(width - 64) / 8` frames rounded up: 152
//! frames in a 1280-pixel pattern.
//!
//! **Between them, every displacement from one frame up to the marker's period
//! is caught**, which is the guarantee the property test at the bottom of this
//! file sweeps in full at every width the application is run at. It is not
//! "every displacement": at the period itself the marker is back where it
//! started, and where that period is also a multiple of eight — as it is at
//! 1280 and 2560 pixels across — the background is back to the same colour too,
//! so a frame torn between two source frames exactly 152 apart reads as a good
//! one. That is five seconds of tear at 30 fps, which is not a compositor
//! handing over a half-composed frame; a test pins the limit
//! (`the_first_tear_the_pattern_cannot_see_is_a_whole_marker_period_away`) so
//! that it stays a stated bound rather than a discovery.
//!
//! # Pixel format
//!
//! BGRA, eight bits a channel — `clipped_capture::PixelFormat::Bgra8Unorm` —
//! which is what both Windows capture APIs produce for ordinary SDR content and
//! what this application's swap chain presents. Bytes in memory are blue,
//! green, red, alpha, in that order.
//!
//! # Tolerance
//!
//! Colours are compared with a small tolerance rather than for equality.
//! Nothing in the path from the swap chain through the compositor to a captured
//! texture is *supposed* to alter a pixel, and on this project's machines
//! nothing does — but a decoder that fails on a display doing a colour
//! conversion would be reporting the display, and the pattern's colours are
//! deliberately far enough apart that a tolerance of [`TOLERANCE`] cannot turn
//! one into another.

use core::fmt;

/// The side of one header cell, in pixels.
///
/// Sixteen rather than one: a decoder samples the middle of a cell, so a cell
/// has to survive a display that is not quite pixel-exact, and a person
/// watching the window has to be able to see the counter ticking over.
pub const CELL: u32 = 16;

/// Cells of fixed colour that mark the start of the header.
const MAGIC: [Colour; 4] = [
    Colour::new(0, 0, 255),
    Colour::new(0, 255, 0),
    Colour::new(255, 0, 0),
    Colour::new(255, 255, 255),
];

/// Bits of frame counter.
const COUNTER_BITS: u32 = 32;

/// Bits of checksum over the counter.
const CHECKSUM_BITS: u32 = 8;

/// Cells in the header row: the magic, the counter and the checksum.
const HEADER_CELLS: u32 = MAGIC.len() as u32 + COUNTER_BITS + CHECKSUM_BITS;

/// The side of the moving marker square, in pixels.
const MARKER: u32 = CELL * 4;

/// The top edge of the marker's band, in pixels from the top of the pattern.
const MARKER_TOP: u32 = CELL * 2;

/// How far the marker moves per frame, in pixels.
///
/// Eight pixels a frame at 30 frames a second is a marker that crosses a
/// 1280-pixel window in five seconds: fast enough to see, slow enough that a
/// person watching a recording can tell smooth playback from stuttering
/// playback by eye.
const MARKER_STEP: u32 = 8;

/// The smallest width the pattern fits in.
pub const MIN_WIDTH: u32 = HEADER_CELLS * CELL;

/// The smallest height the pattern fits in.
pub const MIN_HEIGHT: u32 = MARKER_TOP + MARKER + CELL;

/// How far a channel may drift before two colours are considered different.
///
/// The pattern's colours are at least 64 apart in some channel, so this cannot
/// confuse two of them.
const TOLERANCE: u8 = 24;

/// Above this, a channel reads as a one bit; below [`DARK`], as a zero. A cell
/// between the two is a decode failure rather than a guess.
const BRIGHT: u8 = 200;

/// Below this, a channel reads as a zero bit.
const DARK: u8 = 55;

/// Bytes per pixel in BGRA8.
const BYTES_PER_PIXEL: usize = 4;

/// The background colours, cycled by the frame counter.
///
/// Eight, so that the colour changes every frame and repeats on a period that
/// is coprime with nothing in particular — the point is only that consecutive
/// frames differ, so that the compositor always has new content to compose and
/// a frame's background is a weak check on its counter.
const PALETTE: [Colour; 8] = [
    Colour::new(192, 64, 64),
    Colour::new(64, 192, 64),
    Colour::new(64, 64, 192),
    Colour::new(192, 192, 64),
    Colour::new(192, 64, 192),
    Colour::new(64, 192, 192),
    Colour::new(128, 128, 128),
    Colour::new(32, 96, 160),
];

/// The marker square's colour: white, which is in neither the palette nor
/// anything else the pattern draws outside the header.
const MARKER_COLOUR: Colour = Colour::new(255, 255, 255);

/// A colour, in the order a person reads one rather than the order it is
/// stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colour {
    red: u8,
    green: u8,
    blue: u8,
}

impl Colour {
    /// A colour from its red, green and blue channels.
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// The four bytes this colour occupies in a BGRA8 surface, opaque.
    const fn bgra(self) -> [u8; BYTES_PER_PIXEL] {
        [self.blue, self.green, self.red, 255]
    }

    /// Whether every channel is within [`TOLERANCE`] of `other`'s.
    fn matches(self, other: Self) -> bool {
        self.red.abs_diff(other.red) <= TOLERANCE
            && self.green.abs_diff(other.green) <= TOLERANCE
            && self.blue.abs_diff(other.blue) <= TOLERANCE
    }
}

impl fmt::Display for Colour {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "#{:02x}{:02x}{:02x}",
            self.red, self.green, self.blue
        )
    }
}

/// Where in a captured frame the pattern is, and how big it is.
///
/// A pattern rendered into a borderless window at 1280x720 arrives as a
/// 1280x720 frame with the pattern at (0, 0). The same pattern in an ordinary
/// window arrives inside a larger frame, offset by the window's border and
/// title bar, and in a letterboxed fullscreen frame it can be offset in either
/// direction. [`locate`] finds it; every other function here is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Distance from the left edge of the frame to the left edge of the
    /// pattern, in pixels.
    pub x: u32,
    /// Distance from the top edge of the frame to the top edge of the pattern,
    /// in pixels.
    pub y: u32,
    /// The pattern's width in pixels.
    pub width: u32,
    /// The pattern's height in pixels.
    pub height: u32,
}

impl Region {
    /// A pattern of `width` by `height` at the frame's top-left corner, which
    /// is where a borderless window's content is.
    #[must_use]
    pub const fn origin(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

impl fmt::Display for Region {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}x{} at ({}, {})",
            self.width, self.height, self.x, self.y
        )
    }
}

/// A BGRA8 surface somebody else owns, with its row stride.
///
/// The stride is separate from the width because every surface this is used
/// with has one: a mapped Direct3D texture's rows are padded to the driver's
/// alignment, and treating `width * 4` as the stride produces an image that
/// shears diagonally — the classic readback bug, and one worth making
/// unrepresentable here.
#[derive(Debug, Clone, Copy)]
pub struct Surface<'pixels> {
    pixels: &'pixels [u8],
    stride: usize,
    width: u32,
    height: u32,
}

impl<'pixels> Surface<'pixels> {
    /// Describes `pixels` as a BGRA8 image of `width` by `height` whose rows
    /// are `stride` bytes apart.
    ///
    /// # Errors
    ///
    /// [`PatternError::Stride`] if a row of `width` pixels does not fit in
    /// `stride`, or [`PatternError::TooSmall`] if `pixels` is shorter than the
    /// rows it is supposed to hold. Both mean the caller has miscalculated a
    /// mapped texture's shape, which is worth refusing rather than reading past
    /// the end of a row.
    pub fn new(
        pixels: &'pixels [u8],
        stride: usize,
        width: u32,
        height: u32,
    ) -> Result<Self, PatternError> {
        check_shape(pixels.len(), stride, width, height)?;
        Ok(Self {
            pixels,
            stride,
            width,
            height,
        })
    }

    /// The surface width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The surface height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The colour at (`x`, `y`), or [`None`] outside the surface.
    fn pixel(&self, x: u32, y: u32) -> Option<Colour> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let start = y as usize * self.stride + x as usize * BYTES_PER_PIXEL;
        let bytes = self.pixels.get(start..start + BYTES_PER_PIXEL)?;
        Some(Colour::new(bytes[2], bytes[1], bytes[0]))
    }
}

/// A BGRA8 surface somebody else owns, which [`render`] draws into.
#[derive(Debug)]
pub struct SurfaceMut<'pixels> {
    pixels: &'pixels mut [u8],
    stride: usize,
    width: u32,
    height: u32,
}

impl<'pixels> SurfaceMut<'pixels> {
    /// Describes `pixels` as a BGRA8 image of `width` by `height` whose rows
    /// are `stride` bytes apart.
    ///
    /// # Errors
    ///
    /// As [`Surface::new`].
    pub fn new(
        pixels: &'pixels mut [u8],
        stride: usize,
        width: u32,
        height: u32,
    ) -> Result<Self, PatternError> {
        check_shape(pixels.len(), stride, width, height)?;
        Ok(Self {
            pixels,
            stride,
            width,
            height,
        })
    }

    /// Fills the rectangle at (`x`, `y`) with `colour`, clipped to the surface.
    ///
    /// One row is built and then copied into each row of the rectangle, rather
    /// than each pixel being written in turn. That is not premature: this runs
    /// once per frame over the whole surface, and a debug build writing a
    /// 2560x1440 frame pixel by pixel could only present about twelve frames a
    /// second — which a capture test reads as a source that cannot keep up,
    /// with no hint that the fault is in the subject rather than in capture.
    /// Tests are built in debug, so the debug speed is the speed that matters.
    fn fill(&mut self, x: u32, y: u32, width: u32, height: u32, colour: Colour) {
        let right = (x + width).min(self.width);
        let bottom = (y + height).min(self.height);
        if right <= x || bottom <= y {
            return;
        }

        let row: Vec<u8> = colour
            .bgra()
            .iter()
            .copied()
            .cycle()
            .take((right - x) as usize * BYTES_PER_PIXEL)
            .collect();

        for line in y..bottom {
            let start = line as usize * self.stride + x as usize * BYTES_PER_PIXEL;
            let Some(span) = self.pixels.get_mut(start..start + row.len()) else {
                continue;
            };
            span.copy_from_slice(&row);
        }
    }
}

/// Rejects a surface whose stride, size and buffer do not agree.
fn check_shape(length: usize, stride: usize, width: u32, height: u32) -> Result<(), PatternError> {
    let row = width as usize * BYTES_PER_PIXEL;
    if stride < row {
        return Err(PatternError::Stride { stride, row });
    }
    let needed = stride
        .checked_mul(height.saturating_sub(1) as usize)
        .and_then(|full| full.checked_add(row))
        .ok_or(PatternError::Stride { stride, row })?;
    if length < needed {
        return Err(PatternError::TooSmall {
            length,
            needed,
            width,
            height,
        });
    }
    Ok(())
}

/// What the pattern read out of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternFrame {
    index: u32,
}

impl PatternFrame {
    /// The frame counter the source drew into this frame.
    ///
    /// It counts frames the *application presented*, from zero, and it is the
    /// only identity a captured frame has: two captures with the same index are
    /// the same source frame delivered twice, and a jump of more than one is
    /// that many source frames that never arrived.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }
}

/// The checksum drawn beside a counter.
///
/// A fold of the counter's bytes, offset so that frame zero does not have a
/// zero checksum — eight black cells beside thirty-two black cells is the one
/// case a decoder could plausibly read out of an untouched black surface, and
/// this makes that case fail.
const fn checksum(index: u32) -> u8 {
    let bytes = index.to_le_bytes();
    bytes[0] ^ bytes[1] ^ bytes[2] ^ bytes[3] ^ 0x5A
}

/// How many frames the marker takes to return to where it started, in a pattern
/// `width` pixels across.
///
/// This is the bound on what a torn frame can be caught at: two frames a whole
/// period apart put the marker in the same place, so the marker check cannot
/// separate them and only the background is left (see the module documentation).
///
/// `width` is never smaller than [`MIN_WIDTH`] here: every caller checks that
/// first, because a pattern that does not fit is refused rather than drawn
/// truncated.
const fn marker_period(width: u32) -> u32 {
    // `positions - 1` steps is the last start that still leaves room for the
    // marker, so the marker never runs off the right-hand edge.
    (width - MARKER).div_ceil(MARKER_STEP)
}

/// The marker's left edge for `index` in a pattern `width` pixels across.
const fn marker_x(index: u32, width: u32) -> u32 {
    (index % marker_period(width)) * MARKER_STEP
}

/// The background colour for `index`.
const fn background(index: u32) -> Colour {
    PALETTE[(index % PALETTE.len() as u32) as usize]
}

/// Draws frame `index` over the whole of `target`.
///
/// Every pixel is written, so nothing of the previous frame survives — which
/// matters because the swap chain this is presented through discards its back
/// buffer's contents, and a partially drawn frame would decode as a torn one.
///
/// # Errors
///
/// [`PatternError::TooSmall`] if the surface is smaller than
/// [`MIN_WIDTH`] by [`MIN_HEIGHT`], which is the size the header and the marker
/// need. Drawing a pattern that cannot be decoded would be worse than refusing.
pub fn render(target: &mut SurfaceMut<'_>, index: u32) -> Result<(), PatternError> {
    if target.width < MIN_WIDTH || target.height < MIN_HEIGHT {
        return Err(PatternError::DoesNotFit {
            width: target.width,
            height: target.height,
        });
    }

    target.fill(0, 0, target.width, target.height, background(index));

    for (cell, colour) in MAGIC.iter().enumerate() {
        target.fill(cell as u32 * CELL, 0, CELL, CELL, *colour);
    }

    let bits = u64::from(index) | u64::from(checksum(index)) << COUNTER_BITS;
    for bit in 0..COUNTER_BITS + CHECKSUM_BITS {
        let set = bits >> bit & 1 == 1;
        let colour = if set {
            Colour::new(255, 255, 255)
        } else {
            Colour::new(0, 0, 0)
        };
        target.fill((MAGIC.len() as u32 + bit) * CELL, 0, CELL, CELL, colour);
    }

    target.fill(
        marker_x(index, target.width),
        MARKER_TOP,
        MARKER,
        MARKER,
        MARKER_COLOUR,
    );

    Ok(())
}

/// Reads the frame the pattern at `region` describes.
///
/// Every part of the pattern is checked, not only the counter: the magic, the
/// checksum, the background colour and the marker's position all have to agree
/// with each other. That is what turns "this looks like frame 900" into "this
/// is frame 900, whole".
///
/// # Errors
///
/// [`DecodeError`] naming the first thing that did not agree. A caller counting
/// dropped frames should treat any of them as "this frame was not readable"
/// rather than as a missing frame, and say which happened.
pub fn decode(surface: &Surface<'_>, region: Region) -> Result<PatternFrame, DecodeError> {
    if region.width < MIN_WIDTH || region.height < MIN_HEIGHT {
        return Err(DecodeError::RegionTooSmall { region });
    }
    if region.x + region.width > surface.width || region.y + region.height > surface.height {
        return Err(DecodeError::OutsideSurface {
            region,
            width: surface.width,
            height: surface.height,
        });
    }

    let cell = |index: u32| -> Result<Colour, DecodeError> {
        let x = region.x + index * CELL + CELL / 2;
        let y = region.y + CELL / 2;
        surface.pixel(x, y).ok_or(DecodeError::OutsideSurface {
            region,
            width: surface.width,
            height: surface.height,
        })
    };

    for (position, expected) in MAGIC.iter().enumerate() {
        let found = cell(position as u32)?;
        if !found.matches(*expected) {
            return Err(DecodeError::Magic {
                position: position as u32,
                expected: *expected,
                found,
            });
        }
    }

    let mut bits = 0u64;
    for bit in 0..COUNTER_BITS + CHECKSUM_BITS {
        let found = cell(MAGIC.len() as u32 + bit)?;
        let set = match bit_value(found) {
            Some(set) => set,
            None => return Err(DecodeError::Bit { bit, found }),
        };
        bits |= u64::from(set) << bit;
    }

    let index = (bits & u64::from(u32::MAX)) as u32;
    let found = (bits >> COUNTER_BITS) as u8;
    let expected = checksum(index);
    if found != expected {
        return Err(DecodeError::Checksum {
            index,
            expected,
            found,
        });
    }

    let expected = background(index);
    let corner = surface
        .pixel(
            region.x + region.width - CELL / 2,
            region.y + region.height - CELL / 2,
        )
        .ok_or(DecodeError::OutsideSurface {
            region,
            width: surface.width,
            height: surface.height,
        })?;
    if !corner.matches(expected) {
        return Err(DecodeError::Background {
            index,
            expected,
            found: corner,
        });
    }

    let x = region.x + marker_x(index, region.width) + MARKER / 2;
    let y = region.y + MARKER_TOP + MARKER / 2;
    let found = surface.pixel(x, y).ok_or(DecodeError::OutsideSurface {
        region,
        width: surface.width,
        height: surface.height,
    })?;
    if !found.matches(MARKER_COLOUR) {
        return Err(DecodeError::Marker { index, x, y, found });
    }

    Ok(PatternFrame { index })
}

/// Whether a cell reads as a one, a zero, or neither.
fn bit_value(colour: Colour) -> Option<bool> {
    let channels = [colour.red, colour.green, colour.blue];
    if channels.iter().all(|channel| *channel >= BRIGHT) {
        Some(true)
    } else if channels.iter().all(|channel| *channel <= DARK) {
        Some(false)
    } else {
        None
    }
}

/// Finds a pattern of `width` by `height` somewhere in `surface`.
///
/// A window capture is of the whole window, so the client area the pattern was
/// drawn into starts below the title bar and inside the border, and neither
/// offset is a number this crate should be predicting from a Windows version.
/// Searching for the magic sequence and then decoding at the offset it implies
/// answers the question from the frame itself.
///
/// The search is deliberately narrow. A candidate offset has to be the exact
/// top-left *corner* of the first magic cell — a pixel of its colour whose
/// neighbours above and to the left are not — and each candidate is then
/// confirmed by a full [`decode`], so a false positive would have to reproduce
/// the counter, the checksum, the background and the marker as well.
///
/// Insisting on the corner rather than on any pixel of the cell is what makes
/// the [`Region`] returned exact. A cell is sixteen pixels across, so a search
/// that accepted any pixel of it would happily return an offset eight pixels
/// out — and decode successfully, because every sample point would still land
/// inside the right cell. That is fine until the day a captured frame is
/// cropped by the amount it is out, and then it is a decoder failing for a
/// reason nobody can see.
///
/// Callers are expected to do this once and reuse the [`Region`] — see
/// `docs/testing.md`.
#[must_use]
pub fn locate(surface: &Surface<'_>, width: u32, height: u32) -> Option<Region> {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return None;
    }
    let last_x = surface.width.checked_sub(width)?;
    let last_y = surface.height.checked_sub(height)?;

    let is_corner_of_first_cell = |x: u32, y: u32| {
        let here = surface
            .pixel(x, y)
            .is_some_and(|here| here.matches(MAGIC[0]));
        let left = x > 0
            && surface
                .pixel(x - 1, y)
                .is_some_and(|left| left.matches(MAGIC[0]));
        let above = y > 0
            && surface
                .pixel(x, y - 1)
                .is_some_and(|above| above.matches(MAGIC[0]));
        here && !left && !above
    };

    for y in 0..=last_y {
        for x in 0..=last_x {
            if !is_corner_of_first_cell(x, y) {
                continue;
            }
            let region = Region {
                x,
                y,
                width,
                height,
            };
            if decode(surface, region).is_ok() {
                return Some(region);
            }
        }
    }
    None
}

/// A surface that cannot hold a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatternError {
    /// The rows do not fit in the stride, or the buffer does not hold the rows.
    Stride {
        /// The stride the caller gave, in bytes.
        stride: usize,
        /// The bytes one row of pixels needs.
        row: usize,
    },
    /// The buffer is shorter than the image it is supposed to hold.
    TooSmall {
        /// The buffer's length in bytes.
        length: usize,
        /// The bytes the described image needs.
        needed: usize,
        /// The surface width in pixels.
        width: u32,
        /// The surface height in pixels.
        height: u32,
    },
    /// The surface has fewer pixels than the pattern needs.
    DoesNotFit {
        /// The surface width in pixels.
        width: u32,
        /// The surface height in pixels.
        height: u32,
    },
}

impl fmt::Display for PatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stride { stride, row } => write!(
                formatter,
                "a row of pixels needs {row} bytes but the stride is only {stride}"
            ),
            Self::TooSmall {
                length,
                needed,
                width,
                height,
            } => write!(
                formatter,
                "a {width}x{height} image needs {needed} bytes and the buffer holds {length}"
            ),
            Self::DoesNotFit { width, height } => write!(
                formatter,
                "the test pattern needs at least {MIN_WIDTH}x{MIN_HEIGHT} pixels and this \
                 surface is {width}x{height}"
            ),
        }
    }
}

impl std::error::Error for PatternError {}

/// Why a frame did not read as a pattern.
///
/// Each variant carries what was expected and what was there, because the
/// question a failing capture test has to answer is not "did it fail" but "what
/// did the pipeline do to this frame" (AGENTS.md section 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The region named is smaller than a pattern.
    RegionTooSmall {
        /// The region asked for.
        region: Region,
    },
    /// The region named is not entirely inside the surface.
    OutsideSurface {
        /// The region asked for.
        region: Region,
        /// The surface width in pixels.
        width: u32,
        /// The surface height in pixels.
        height: u32,
    },
    /// A magic cell was the wrong colour: this is not a pattern, or not at this
    /// offset.
    Magic {
        /// Which magic cell, counting from zero.
        position: u32,
        /// The colour the pattern draws there.
        expected: Colour,
        /// The colour found.
        found: Colour,
    },
    /// A counter or checksum cell was neither black nor white.
    Bit {
        /// Which bit, counting from the least significant bit of the counter.
        bit: u32,
        /// The colour found.
        found: Colour,
    },
    /// The counter and its checksum disagree, so at least one cell was misread.
    Checksum {
        /// The counter as read.
        index: u32,
        /// The checksum that counter should have had.
        expected: u8,
        /// The checksum drawn beside it.
        found: u8,
    },
    /// The background is not the colour this counter's frame is drawn on, which
    /// is what a frame assembled from two source frames looks like.
    Background {
        /// The counter as read.
        index: u32,
        /// The colour this frame's background should be.
        expected: Colour,
        /// The colour found.
        found: Colour,
    },
    /// The marker is not where this counter puts it — the other half of the
    /// torn-frame check.
    Marker {
        /// The counter as read.
        index: u32,
        /// Where the marker's centre should have been, in surface pixels.
        x: u32,
        /// Where the marker's centre should have been, in surface pixels.
        y: u32,
        /// The colour found there.
        found: Colour,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RegionTooSmall { region } => write!(
                formatter,
                "a pattern needs at least {MIN_WIDTH}x{MIN_HEIGHT} pixels and {region} is smaller"
            ),
            Self::OutsideSurface {
                region,
                width,
                height,
            } => write!(
                formatter,
                "the pattern region {region} does not fit inside a {width}x{height} frame"
            ),
            Self::Magic {
                position,
                expected,
                found,
            } => write!(
                formatter,
                "magic cell {position} should be {expected} and is {found}: this frame does \
                 not hold a Clipped test pattern at this offset"
            ),
            Self::Bit { bit, found } => write!(
                formatter,
                "counter bit {bit} is {found}, which is neither black nor white"
            ),
            Self::Checksum {
                index,
                expected,
                found,
            } => write!(
                formatter,
                "the counter reads {index}, whose checksum is {expected:#04x}, but the frame \
                 carries {found:#04x}: at least one cell was misread"
            ),
            Self::Background {
                index,
                expected,
                found,
            } => write!(
                formatter,
                "frame {index} is drawn on {expected} and this frame's background is {found}: \
                 the frame is not all from one source frame"
            ),
            Self::Marker { index, x, y, found } => write!(
                formatter,
                "frame {index} puts its marker at ({x}, {y}) and that pixel is {found}: the \
                 frame is not all from one source frame"
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame buffer and the stride it was allocated with.
    ///
    /// The stride is deliberately not `width * 4` in most tests: a mapped
    /// Direct3D texture's rows are padded, and code that only ever sees a tight
    /// stride is code that has never been tested against the thing it will
    /// actually be handed.
    struct Frame {
        pixels: Vec<u8>,
        stride: usize,
        width: u32,
        height: u32,
    }

    impl Frame {
        fn new(width: u32, height: u32, padding: usize) -> Self {
            let stride = width as usize * BYTES_PER_PIXEL + padding;
            Self {
                pixels: vec![0; stride * height as usize],
                stride,
                width,
                height,
            }
        }

        fn draw(&mut self, index: u32) -> Result<(), PatternError> {
            let mut target =
                SurfaceMut::new(&mut self.pixels, self.stride, self.width, self.height)?;
            render(&mut target, index)
        }

        /// Draws the pattern into a sub-rectangle, the way a window capture
        /// sees a client area inside a window.
        fn draw_at(&mut self, index: u32, x: u32, y: u32, width: u32, height: u32) {
            let offset = y as usize * self.stride + x as usize * BYTES_PER_PIXEL;
            let mut target =
                SurfaceMut::new(&mut self.pixels[offset..], self.stride, width, height)
                    .expect("the sub-rectangle fits inside the frame");
            render(&mut target, index).expect("the sub-rectangle is big enough for a pattern");
        }

        fn surface(&self) -> Surface<'_> {
            Surface::new(&self.pixels, self.stride, self.width, self.height)
                .expect("the frame describes itself")
        }

        fn set(&mut self, x: u32, y: u32, width: u32, height: u32, colour: Colour) {
            let mut target =
                SurfaceMut::new(&mut self.pixels, self.stride, self.width, self.height)
                    .expect("the frame describes itself");
            target.fill(x, y, width, height, colour);
        }

        /// Builds the frame a compositor hands over when it tears: the header
        /// row from one source frame, everything below it from another.
        fn torn(top: u32, rest: u32, width: u32, height: u32) -> Self {
            let mut frame = Self::new(width, height, 0);
            frame.draw(rest).expect("the frame holds a pattern");

            let mut header = Self::new(width, height, 0);
            header.draw(top).expect("the frame holds a pattern");

            let row = width as usize * BYTES_PER_PIXEL;
            for line in 0..CELL as usize {
                let start = line * frame.stride;
                frame.pixels[start..start + row]
                    .copy_from_slice(&header.pixels[start..start + row]);
            }
            frame
        }
    }

    #[test]
    fn a_rendered_frame_decodes_to_the_counter_it_was_drawn_for() {
        // The whole contract, over counters chosen to exercise every byte of
        // the encoding: zero, one, the palette wrap, a value with bits in all
        // four bytes, and the last value a `u32` holds.
        let mut frame = Frame::new(1280, 720, 128);
        for index in [0, 1, 7, 8, 12_345, 0x0F0F_0F0F, u32::MAX] {
            frame.draw(index).expect("1280x720 holds a pattern");
            let decoded = decode(&frame.surface(), Region::origin(1280, 720))
                .unwrap_or_else(|error| panic!("frame {index} should decode: {error}"));
            assert_eq!(decoded.index(), index);
        }
    }

    #[test]
    fn a_tight_stride_is_read_the_same_as_a_padded_one() {
        let mut tight = Frame::new(800, 600, 0);
        let mut padded = Frame::new(800, 600, 256);
        tight.draw(4321).expect("800x600 holds a pattern");
        padded.draw(4321).expect("800x600 holds a pattern");

        assert_eq!(
            decode(&tight.surface(), Region::origin(800, 600)).expect("decodes"),
            decode(&padded.surface(), Region::origin(800, 600)).expect("decodes"),
        );
    }

    #[test]
    fn a_pattern_offset_inside_a_larger_frame_is_found() {
        // What a window capture looks like: the client area sits below the
        // title bar and inside the border, and the offset is not a number the
        // test knows in advance.
        let mut frame = Frame::new(1400, 900, 64);
        frame.draw_at(909, 8, 31, 1280, 720);

        let region = locate(&frame.surface(), 1280, 720).expect("the pattern is in there");
        assert_eq!(
            region,
            Region {
                x: 8,
                y: 31,
                width: 1280,
                height: 720
            }
        );
        assert_eq!(
            decode(&frame.surface(), region).expect("decodes").index(),
            909
        );
    }

    #[test]
    fn a_frame_with_no_pattern_in_it_is_not_located() {
        let mut frame = Frame::new(800, 600, 0);
        frame.set(0, 0, 800, 600, Colour::new(0, 0, 255));
        assert_eq!(locate(&frame.surface(), 704, 112), None);
    }

    #[test]
    fn a_single_flipped_counter_cell_is_rejected_rather_than_misread() {
        // The reason the checksum is there. Without it this frame reads as
        // 12,345 + 65,536 and the test that consumes it reports a gap of
        // sixty-five thousand frames that never happened.
        let mut frame = Frame::new(1280, 720, 0);
        frame.draw(12_345).expect("1280x720 holds a pattern");
        let bit_16 = (MAGIC.len() as u32 + 16) * CELL;
        frame.set(bit_16, 0, CELL, CELL, Colour::new(255, 255, 255));

        match decode(&frame.surface(), Region::origin(1280, 720)) {
            Err(DecodeError::Checksum { index, .. }) => {
                assert_eq!(index, 12_345 + 65_536, "the misread counter is reported");
            }
            other => panic!("a flipped counter cell must be rejected, got {other:?}"),
        }
    }

    #[test]
    fn a_grey_counter_cell_is_rejected_rather_than_guessed() {
        let mut frame = Frame::new(1280, 720, 0);
        frame.draw(77).expect("1280x720 holds a pattern");
        frame.set(
            (MAGIC.len() as u32 + 3) * CELL,
            0,
            CELL,
            CELL,
            Colour::new(128, 128, 128),
        );

        assert!(matches!(
            decode(&frame.surface(), Region::origin(1280, 720)),
            Err(DecodeError::Bit { bit: 3, .. })
        ));
    }

    #[test]
    fn a_frame_whose_background_belongs_to_another_frame_is_rejected() {
        // A torn frame: the header says 100 and the rest of the image came
        // from a different source frame. A pattern that only encoded a counter
        // would report this as a good frame 100.
        let mut frame = Frame::new(1280, 720, 0);
        frame.draw(100).expect("1280x720 holds a pattern");
        frame.set(0, 200, 1280, 520, background(101));

        assert!(matches!(
            decode(&frame.surface(), Region::origin(1280, 720)),
            Err(DecodeError::Background { index: 100, .. })
        ));
    }

    #[test]
    fn a_frame_whose_marker_is_in_another_frames_place_is_rejected() {
        let mut frame = Frame::new(1280, 720, 0);
        frame.draw(100).expect("1280x720 holds a pattern");
        // Paint over the marker with the frame's own background, which is what
        // the top half of frame 100 over the bottom half of frame 130 would
        // leave behind at frame 100's marker position.
        frame.set(
            marker_x(100, 1280),
            MARKER_TOP,
            MARKER,
            MARKER,
            background(100),
        );

        assert!(matches!(
            decode(&frame.surface(), Region::origin(1280, 720)),
            Err(DecodeError::Marker { index: 100, .. })
        ));
    }

    #[test]
    fn a_frame_torn_between_two_source_frames_less_than_a_marker_period_apart_is_rejected() {
        // The property the background and the marker exist for, swept in full
        // rather than sampled: the background repeats every eight frames, and
        // the marker's sample point is still inside the marker while the frame
        // is up to four frames out, so neither is enough alone. Between them
        // every displacement below the marker's period is caught — 151 of them
        // at 1280 pixels across, 311 at 2560 — which is the guarantee the
        // module documentation states.
        for width in [MIN_WIDTH, 800, 1280, 1920, 2560] {
            for displacement in 1..marker_period(width) {
                let frame = Frame::torn(500, 500 + displacement, width, 720);
                let decoded = decode(&frame.surface(), Region::origin(width, 720));
                assert!(
                    decoded.is_err(),
                    "in a {width}-pixel pattern, a frame whose header says 500 and whose \
                     body is frame {} decoded as {decoded:?}; a torn frame read as a good \
                     one is a corrupted recording nobody notices",
                    500 + displacement
                );
            }
        }
    }

    /// Whether a frame torn between two source frames `displacement` apart
    /// still decodes as a good one, in a pattern `width` pixels across.
    fn a_tear_gets_through(width: u32, displacement: u32) -> bool {
        let frame = Frame::torn(500, 500 + displacement, width, 720);
        decode(&frame.surface(), Region::origin(width, 720)).is_ok()
    }

    #[test]
    fn the_first_tear_the_pattern_cannot_see_is_a_whole_marker_period_away() {
        // The other side of that guarantee, pinned so that the documented bound
        // is a measurement rather than a claim. A tear gets through only when
        // the marker is back within half its own width of where it was *and*
        // the background is back to the same colour, and the first displacement
        // that does both is the marker's whole period — five seconds of tear at
        // 30 fps, which is two different applications rather than one frame the
        // compositor assembled from two.
        for width in [MIN_WIDTH, 800, 1280, 1920, 2560] {
            let period = marker_period(width);
            if period % PALETTE.len() as u32 == 0 {
                assert!(
                    a_tear_gets_through(width, period),
                    "a {width}-pixel pattern's marker repeats every {period} frames and \
                     {period} is a multiple of the palette's eight, so both checks come back \
                     at once and this displacement is the documented blind spot; if it is \
                     now caught, the module documentation understates what the pattern can do"
                );
            } else {
                assert!(
                    !a_tear_gets_through(width, period),
                    "a {width}-pixel pattern's marker repeats every {period} frames, which is \
                     not a multiple of the palette's eight, so the background should still \
                     catch a tear the marker cannot see"
                );
            }
        }

        // The two the module documentation quotes, found rather than asserted
        // in the abstract: 152 at the width the capture tests run at, and four
        // frames past the period at 800, where the period is not a multiple of
        // the palette's.
        assert_eq!(first_blind_displacement(1280), Some(152));
        assert_eq!(first_blind_displacement(800), Some(96));
        assert_eq!(marker_period(1280), 152);
    }

    /// The smallest tear that reads as a good frame, searched up to twice the
    /// marker's period.
    fn first_blind_displacement(width: u32) -> Option<u32> {
        (1..marker_period(width) * 2).find(|displacement| a_tear_gets_through(width, *displacement))
    }

    #[test]
    fn the_marker_moves_every_frame_and_never_leaves_the_pattern() {
        // The marker is what a person watching playback judges smoothness by,
        // so it has to move on every single frame, and it has to stay inside
        // the frame or the decoder would be checking a pixel that is not there.
        for width in [MIN_WIDTH, 800, 1280, 2560] {
            let mut previous = marker_x(0, width);
            for index in 1..600u32 {
                let x = marker_x(index, width);
                assert_ne!(x, previous, "the marker did not move at frame {index}");
                assert!(
                    x + MARKER <= width,
                    "the marker leaves a {width}-pixel pattern at frame {index}"
                );
                previous = x;
            }
        }
    }

    #[test]
    fn a_surface_too_small_for_the_pattern_is_refused_rather_than_half_drawn() {
        let mut frame = Frame::new(320, 240, 0);
        assert!(matches!(
            frame.draw(0),
            Err(PatternError::DoesNotFit { .. })
        ));
    }

    #[test]
    fn a_stride_that_cannot_hold_a_row_is_refused() {
        let mut pixels = vec![0u8; 4096];
        assert!(matches!(
            SurfaceMut::new(&mut pixels, 100, 800, 600),
            Err(PatternError::Stride { .. })
        ));
        assert!(matches!(
            Surface::new(&pixels, 3200, 800, 600),
            Err(PatternError::TooSmall { .. })
        ));
    }

    /// Asserts that no colour in `colours` is within the decoder's tolerance of
    /// another, naming the pair when one is.
    fn assert_all_distinguishable(colours: &[Colour], what: &str) {
        for (first, one) in colours.iter().enumerate() {
            for other in &colours[first + 1..] {
                assert!(
                    !one.matches(*other),
                    "{what}: {one} and {other} are within the decoder's tolerance of \
                     each other, so one can be read as the other"
                );
            }
        }
    }

    #[test]
    fn every_colour_the_decoder_tells_apart_is_far_enough_from_the_others() {
        // The tolerance the decoder compares with only means anything if the
        // colours it has to distinguish are further apart than it. Three groups
        // have to hold, and they are separate groups because white appears in
        // two of them for two different reasons — the last magic cell, and the
        // marker — and being the same colour there is not a collision.
        assert_all_distinguishable(&MAGIC, "the magic cells");
        assert_all_distinguishable(&PALETTE, "the background palette");

        // The background is checked against the marker's white and against the
        // black and white of the counter cells, so no background colour may be
        // mistakable for either.
        for background in PALETTE {
            assert_all_distinguishable(
                &[background, MARKER_COLOUR, Colour::new(0, 0, 0)],
                "a background colour against the marker and the counter cells",
            );
            assert!(
                bit_value(background).is_none(),
                "{background} would read as a counter bit rather than as a background"
            );
        }
    }

    #[test]
    fn the_checksum_of_frame_zero_is_not_zero() {
        // Eight black cells beside thirty-two black cells is what an untouched
        // black surface would read as, so frame zero must not look like that.
        assert_ne!(checksum(0), 0);
    }
}
