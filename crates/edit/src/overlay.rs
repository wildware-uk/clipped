//! Text drawn over the picture for part of the clip.
//!
//! Deliberately minimal, because [issue
//! #87](https://github.com/wildware-uk/clipped/issues/87) is deliberately
//! minimal: a line of text, where it sits, how big it is and when it is on
//! screen. Not a titling tool. Colour, font choice, outlines and animation are
//! all absent, and adding any of them later is a new field and a new schema
//! version rather than a change to anything here.
//!
//! An overlay is timed in [`OutputTime`](crate::OutputTime), not source time.
//! "Three seconds into the clip" is what the user means when they drag it, and
//! it stays true when the material behind it is trimmed, sped up or replaced.

use serde::{Deserialize, Serialize};

use crate::time::OutputSpan;

/// Where on the frame text sits, as fractions of the output frame.
///
/// The point is the *centre* of the text, so `0.5, 0.5` centres it whatever it
/// says and however long it is. Fractions rather than pixels so that the same
/// clip exported at 1080p and at 720p looks the same, which is half of what
/// [issue #87](https://github.com/wildware-uk/clipped/issues/87) means by
/// "preview and export match".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayPosition {
    /// Distance from the left edge, as a fraction of the frame's width.
    pub x: f64,
    /// Distance from the top edge, as a fraction of the frame's height.
    pub y: f64,
}

impl OverlayPosition {
    /// The middle of the frame.
    pub const CENTRE: Self = Self { x: 0.5, y: 0.5 };

    /// A position, or `None` if it is not a real point on the frame.
    #[must_use]
    pub fn new(x: f64, y: f64) -> Option<Self> {
        let position = Self { x, y };
        position.is_valid().then_some(position)
    }

    /// Whether the position is finite and inside the frame.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && (0.0..=1.0).contains(&self.x)
            && (0.0..=1.0).contains(&self.y)
    }
}

/// The smallest text size the model accepts, as a percentage of frame height.
pub const MINIMUM_TEXT_HEIGHT_PERCENT: u8 = 1;

/// The largest text size the model accepts, as a percentage of frame height.
pub const MAXIMUM_TEXT_HEIGHT_PERCENT: u8 = 50;

/// A line of text on screen for part of the clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextOverlay {
    /// What it says.
    pub text: String,
    /// When it is on screen, on the edited timeline.
    pub when: OutputSpan,
    /// Where the centre of the text sits.
    pub position: OverlayPosition,
    /// The text's height, as a percentage of the output frame's height.
    ///
    /// A percentage rather than a point size for the reason the position is a
    /// fraction: a point size is a different fraction of the picture at every
    /// export resolution, so the preview and the file would disagree by
    /// construction.
    pub height_percent: u8,
}

impl TextOverlay {
    /// The height used when nothing else is asked for: legible, not a banner.
    pub const DEFAULT_HEIGHT_PERCENT: u8 = 6;

    /// `text`, centred, on screen for `when`.
    #[must_use]
    pub fn new(text: impl Into<String>, when: OutputSpan) -> Self {
        Self {
            text: text.into(),
            when,
            position: OverlayPosition::CENTRE,
            height_percent: Self::DEFAULT_HEIGHT_PERCENT,
        }
    }

    /// The same overlay at `position`.
    #[must_use]
    pub fn at(mut self, position: OverlayPosition) -> Self {
        self.position = position;
        self
    }

    /// The same overlay at `height_percent` of the frame's height.
    #[must_use]
    pub fn sized(mut self, height_percent: u8) -> Self {
        self.height_percent = height_percent;
        self
    }

    /// Whether the height is one the model accepts.
    #[must_use]
    pub const fn has_usable_height(&self) -> bool {
        self.height_percent >= MINIMUM_TEXT_HEIGHT_PERCENT
            && self.height_percent <= MAXIMUM_TEXT_HEIGHT_PERCENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::OutputTime;

    fn when(start_nanos: u64, end_nanos: u64) -> OutputSpan {
        OutputSpan::new(
            OutputTime::from_nanos(start_nanos),
            OutputTime::from_nanos(end_nanos),
        )
        .expect("the test span ends after it starts")
    }

    #[test]
    fn a_new_overlay_is_centred_and_legible() {
        let overlay = TextOverlay::new("Ace", when(0, 3_000_000_000));

        assert_eq!(overlay.position, OverlayPosition::CENTRE);
        assert_eq!(overlay.height_percent, TextOverlay::DEFAULT_HEIGHT_PERCENT);
        assert!(overlay.has_usable_height());
    }

    #[test]
    fn a_position_has_to_be_a_point_on_the_frame() {
        assert!(OverlayPosition::new(0.0, 0.0).is_some());
        assert!(OverlayPosition::new(1.0, 1.0).is_some());
        assert!(OverlayPosition::new(1.01, 0.5).is_none());
        assert!(OverlayPosition::new(0.5, -0.01).is_none());
        assert!(OverlayPosition::new(f64::NAN, 0.5).is_none());
    }

    #[test]
    fn text_may_be_neither_invisible_nor_the_whole_frame() {
        let overlay = TextOverlay::new("Ace", when(0, 1_000));
        assert!(!overlay.clone().sized(0).has_usable_height());
        assert!(overlay.clone().sized(1).has_usable_height());
        assert!(overlay.clone().sized(50).has_usable_height());
        assert!(!overlay.sized(51).has_usable_height());
    }

    #[test]
    fn an_overlay_is_timed_on_the_edited_timeline() {
        let overlay = TextOverlay::new("Ace", when(2_000, 5_000));
        let json = serde_json::to_value(&overlay).expect("it serialises");

        assert_eq!(json["when"]["start"], 2_000);
        assert_eq!(json["when"]["end"], 5_000);
        assert_eq!(
            serde_json::from_value::<TextOverlay>(json).expect("it reads back"),
            overlay
        );
    }
}
