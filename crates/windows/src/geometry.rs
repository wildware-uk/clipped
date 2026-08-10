//! Sizes and rectangles in physical pixels.
//!
//! Everything this crate reports is in *physical* pixels — the pixels a capture
//! backend and an encoder deal in — never in the scaled logical units a
//! DPI-unaware process is shown. [`crate::enable_per_monitor_dpi_awareness`]
//! explains what makes that true and what has to happen for it to stay true.

use core::fmt;

/// A size in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelSize {
    width: u32,
    height: u32,
}

impl PixelSize {
    /// A size of `width` by `height` physical pixels.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// The width in physical pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// The height in physical pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Whether either dimension is zero, so there is nothing to capture.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

impl fmt::Display for PixelSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}x{}", self.width, self.height)
    }
}

/// A rectangle in the virtual desktop's physical-pixel coordinate space.
///
/// The origin is the top-left corner of the primary monitor, so a monitor
/// arranged to the left of, or above, the primary one has negative coordinates.
/// That is a real arrangement rather than a curiosity, which is why the origin
/// is signed and the size is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelRect {
    left: i32,
    top: i32,
    size: PixelSize,
}

impl PixelRect {
    /// A rectangle whose top-left corner is at (`left`, `top`).
    #[must_use]
    pub const fn new(left: i32, top: i32, size: PixelSize) -> Self {
        Self { left, top, size }
    }

    /// The left edge, in virtual-desktop coordinates.
    #[must_use]
    pub const fn left(self) -> i32 {
        self.left
    }

    /// The top edge, in virtual-desktop coordinates.
    #[must_use]
    pub const fn top(self) -> i32 {
        self.top
    }

    /// The width and height.
    #[must_use]
    pub const fn size(self) -> PixelSize {
        self.size
    }
}

impl fmt::Display for PixelRect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at ({}, {})", self.size, self.left, self.top)
    }
}

/// The DPI Windows reports for a display that is not being scaled.
///
/// Scale factors are expressed against this: 144 DPI is 150%.
pub const DEFAULT_DPI: u32 = 96;

/// The scale factor `dpi` represents, as a percentage of [`DEFAULT_DPI`].
///
/// Rounded, because the percentages Windows offers in its display settings are
/// whole numbers and a user comparing the two should see the same figure.
#[must_use]
pub const fn scale_percentage(dpi: u32) -> u32 {
    (dpi * 100 + DEFAULT_DPI / 2) / DEFAULT_DPI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_with_a_zero_dimension_has_nothing_to_capture() {
        assert!(PixelSize::new(0, 1080).is_empty());
        assert!(PixelSize::new(1920, 0).is_empty());
        assert!(!PixelSize::new(1920, 1080).is_empty());
    }

    #[test]
    fn scale_percentages_match_the_ones_windows_display_settings_offer() {
        assert_eq!(scale_percentage(96), 100);
        assert_eq!(scale_percentage(120), 125);
        assert_eq!(scale_percentage(144), 150);
        assert_eq!(scale_percentage(168), 175);
        assert_eq!(scale_percentage(192), 200);
    }

    #[test]
    fn a_rectangle_reads_the_way_a_display_arrangement_is_described() {
        let rectangle = PixelRect::new(-1920, 0, PixelSize::new(1920, 1080));
        assert_eq!(rectangle.to_string(), "1920x1080 at (-1920, 0)");
    }
}
