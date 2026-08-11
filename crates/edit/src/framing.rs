//! Crop, rotation and aspect ratio: what the picture is, without touching it.
//!
//! All three are the shape of [issue
//! #86](https://github.com/wildware-uk/clipped/issues/86)'s framing tools.
//! Crop and rotation belong to a *segment* rather than to the document,
//! because an edit may join a landscape recording to a portrait one ([issue
//! #88](https://github.com/wildware-uk/clipped/issues/88)) and one crop
//! rectangle cannot mean the right thing in both. The aspect ratio belongs to
//! the document, because it describes the file being written and there is only
//! one of those.

use serde::{Deserialize, Serialize};

/// The part of the picture to keep, as fractions of the source frame.
///
/// Fractions rather than pixels for the reason the ratio in
/// [`Speed`](crate::Speed) is two integers: a crop expressed in pixels is
/// wrong the moment the same edit refers to a 1440p recording and a 1080p one,
/// and per-source pixel rectangles would make [issue
/// #88](https://github.com/wildware-uk/clipped/issues/88)'s normalisation rules
/// depend on data this document deliberately does not hold.
///
/// The rectangle is `[0, 1]` in both axes with the origin at the top left, and
/// is applied **before** [`Rotation`], so a crop drawn on the picture the user
/// is looking at survives them rotating it afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CropRect {
    /// Distance from the left edge, as a fraction of the frame's width.
    pub x: f64,
    /// Distance from the top edge, as a fraction of the frame's height.
    pub y: f64,
    /// Width kept, as a fraction of the frame's width.
    pub width: f64,
    /// Height kept, as a fraction of the frame's height.
    pub height: f64,
}

impl CropRect {
    /// The whole frame.
    pub const FULL: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    /// A rectangle, or `None` if it is not a real one inside the frame.
    ///
    /// Refuses anything that is not finite, a width or height that is not
    /// positive, and a rectangle that leaves the frame — all of which produce
    /// an export that either fails deep inside a filter graph or silently
    /// differs from the preview.
    #[must_use]
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Option<Self> {
        let rectangle = Self {
            x,
            y,
            width,
            height,
        };
        rectangle.is_valid().then_some(rectangle)
    }

    /// Whether the rectangle is finite, positive and inside the frame.
    #[must_use]
    pub fn is_valid(self) -> bool {
        let finite = self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite();
        finite
            && self.x >= 0.0
            && self.y >= 0.0
            && self.width > 0.0
            && self.height > 0.0
            && self.x + self.width <= 1.0
            && self.y + self.height <= 1.0
    }
}

/// How far the picture is turned, clockwise.
///
/// A quarter-turn enum rather than an angle: arbitrary rotation needs
/// interpolation and a background to fill the corners, which is a different and
/// much larger feature than "this was recorded sideways".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    /// Left as recorded.
    #[default]
    None,
    /// A quarter turn clockwise.
    Clockwise90,
    /// A half turn.
    Clockwise180,
    /// A quarter turn anticlockwise.
    Clockwise270,
}

impl Rotation {
    /// The rotation in degrees clockwise.
    #[must_use]
    pub const fn degrees(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Clockwise90 => 90,
            Self::Clockwise180 => 180,
            Self::Clockwise270 => 270,
        }
    }

    /// Whether the turn exchanges width and height.
    #[must_use]
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::Clockwise90 | Self::Clockwise270)
    }
}

/// The shape of the file an export writes.
///
/// A ratio, not a resolution: how many pixels to render is an export setting
/// that belongs to the export dialog ([issue
/// #90](https://github.com/wildware-uk/clipped/issues/90)) and may differ
/// between two exports of the same clip, while "this clip is vertical" is an
/// edit decision the user made and expects to still be there tomorrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AspectRatio {
    /// The ratio's width.
    pub width: u32,
    /// The ratio's height.
    pub height: u32,
}

impl AspectRatio {
    /// 16:9, which is what nearly every game is recorded at.
    pub const WIDESCREEN: Self = Self {
        width: 16,
        height: 9,
    };
    /// 9:16, for the vertical short-form platforms.
    pub const VERTICAL: Self = Self {
        width: 9,
        height: 16,
    };
    /// 1:1.
    pub const SQUARE: Self = Self {
        width: 1,
        height: 1,
    };
    /// 4:5, the tallest ratio a feed will show without cropping it.
    pub const PORTRAIT: Self = Self {
        width: 4,
        height: 5,
    };
    /// 4:3.
    pub const CLASSIC: Self = Self {
        width: 4,
        height: 3,
    };
    /// 21:9, for ultrawide monitors.
    pub const ULTRAWIDE: Self = Self {
        width: 21,
        height: 9,
    };

    /// The presets the editor offers, widest first.
    ///
    /// A starting set, not a final one: [issue
    /// #86](https://github.com/wildware-uk/clipped/issues/86) settles the list
    /// against the design deck, and any ratio can be constructed without being
    /// on it.
    pub const PRESETS: &'static [Self] = &[
        Self::ULTRAWIDE,
        Self::WIDESCREEN,
        Self::CLASSIC,
        Self::SQUARE,
        Self::PORTRAIT,
        Self::VERTICAL,
    ];

    /// A ratio, or `None` if either side is zero.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self { width, height })
    }

    /// Whether both sides are non-zero, which deserialisation cannot promise.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.width != 0 && self.height != 0
    }
}

impl core::fmt::Display for AspectRatio {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}:{}", self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crop_may_not_leave_the_frame() {
        assert!(CropRect::new(0.0, 0.0, 1.0, 1.0).is_some());
        assert!(CropRect::new(0.25, 0.25, 0.75, 0.75).is_some());
        assert!(
            CropRect::new(0.5, 0.0, 0.6, 1.0).is_none(),
            "half an inch past the right edge is not a crop"
        );
        assert!(CropRect::new(-0.1, 0.0, 0.5, 0.5).is_none());
    }

    #[test]
    fn a_crop_may_not_be_empty_or_impossible() {
        assert!(CropRect::new(0.0, 0.0, 0.0, 1.0).is_none());
        assert!(CropRect::new(0.0, 0.0, 1.0, -1.0).is_none());
        assert!(CropRect::new(f64::NAN, 0.0, 1.0, 1.0).is_none());
        assert!(CropRect::new(0.0, 0.0, f64::INFINITY, 1.0).is_none());
    }

    #[test]
    fn the_full_frame_is_a_valid_crop() {
        assert!(CropRect::FULL.is_valid());
    }

    #[test]
    fn rotation_defaults_to_leaving_the_picture_alone() {
        assert_eq!(Rotation::default(), Rotation::None);
        assert_eq!(Rotation::default().degrees(), 0);
        assert!(!Rotation::None.swaps_axes());
        assert!(Rotation::Clockwise90.swaps_axes());
        assert!(!Rotation::Clockwise180.swaps_axes());
        assert!(Rotation::Clockwise270.swaps_axes());
    }

    #[test]
    fn rotation_is_written_as_a_name_rather_than_a_number() {
        let json = serde_json::to_string(&Rotation::Clockwise90).expect("it serialises");
        assert_eq!(json, r#""clockwise90""#);
        assert_eq!(
            serde_json::from_str::<Rotation>(&json).expect("it reads back"),
            Rotation::Clockwise90
        );
    }

    #[test]
    fn an_aspect_ratio_needs_both_sides() {
        assert!(AspectRatio::new(0, 9).is_none());
        assert!(AspectRatio::new(16, 0).is_none());
        assert_eq!(AspectRatio::new(16, 9), Some(AspectRatio::WIDESCREEN));
        assert_eq!(AspectRatio::WIDESCREEN.to_string(), "16:9");
    }

    #[test]
    fn every_preset_is_a_ratio_that_validates() {
        for preset in AspectRatio::PRESETS {
            assert!(preset.is_valid(), "{preset} is not a usable ratio");
        }
    }
}
