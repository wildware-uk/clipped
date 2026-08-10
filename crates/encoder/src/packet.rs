//! Packets coming out: encoded bytes, their timestamps, and what they contain.
//!
//! An [`EncodedPacket`] is one coded picture, ready for a container. It borrows
//! the encoder's own output buffer rather than owning a copy, because the
//! alternative is an allocation and a memcpy per frame on the thread that is
//! also capturing (AGENTS.md section 18). A consumer that needs the bytes for
//! longer copies them; a muxer writing them straight to a file does not.

use core::fmt;
use core::time::Duration;

/// What kind of coded picture a packet holds.
///
/// The distinction that matters to everything downstream is
/// [`is_keyframe`](EncodedPacket::is_keyframe): a clip, a seek and a replay
/// buffer's start point can only land on one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PictureKind {
    /// A picture that depends on nothing before it and resets the reference
    /// list — an IDR in H.264 and HEVC, a key frame in AV1. A stream can be cut
    /// here.
    Keyframe,
    /// A picture coded without reference to any other, but which does not reset
    /// the reference list. Cheaper than a keyframe and not a safe cut point.
    Intra,
    /// A picture predicted from earlier pictures.
    Predicted,
    /// A picture predicted from both earlier and later pictures.
    Bidirectional,
    /// The encoder produced a picture whose type it did not report.
    Unknown,
}

impl PictureKind {
    /// Whether a recording can be cut immediately before this picture.
    #[must_use]
    pub const fn is_keyframe(self) -> bool {
        matches!(self, Self::Keyframe)
    }
}

impl fmt::Display for PictureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Keyframe => "keyframe",
            Self::Intra => "intra",
            Self::Predicted => "predicted",
            Self::Bidirectional => "bidirectional",
            Self::Unknown => "unknown",
        })
    }
}

/// One coded picture, borrowed from the encoder that produced it.
///
/// # Ownership
///
/// The bytes belong to the encoder's output buffer and are valid until the next
/// call to [`VideoEncoder::next_packet`](crate::VideoEncoder::next_packet),
/// which is when the encoder releases them back to the hardware. The borrow of
/// `&mut self` in that method is what makes the compiler enforce it — the same
/// contract `clipped-capture` uses for a captured frame, for the same reason.
///
/// # Timestamps
///
/// [`presentation_time`](Self::presentation_time) is the time the frame was
/// submitted with, carried through the encoder untouched.
/// [`decode_time`](Self::decode_time) is when a decoder must decode it, which
/// differs only when the encoder reorders pictures. Both are measured from the
/// same zero as the frames going in, so a muxer never has to guess a time base.
#[derive(Debug, Clone, Copy)]
pub struct EncodedPacket<'encoder> {
    data: &'encoder [u8],
    presentation_time: Duration,
    decode_time: Duration,
    picture: PictureKind,
}

impl<'encoder> EncodedPacket<'encoder> {
    /// Describes a packet over the encoder's own buffer.
    #[must_use]
    pub const fn new(
        data: &'encoder [u8],
        presentation_time: Duration,
        decode_time: Duration,
        picture: PictureKind,
    ) -> Self {
        Self {
            data,
            presentation_time,
            decode_time,
            picture,
        }
    }

    /// The coded bytes.
    #[must_use]
    pub const fn data(&self) -> &'encoder [u8] {
        self.data
    }

    /// When this picture should be shown.
    #[must_use]
    pub const fn presentation_time(&self) -> Duration {
        self.presentation_time
    }

    /// When this picture must be decoded.
    #[must_use]
    pub const fn decode_time(&self) -> Duration {
        self.decode_time
    }

    /// What kind of picture this is.
    #[must_use]
    pub const fn picture(&self) -> PictureKind {
        self.picture
    }

    /// Whether a recording can be cut immediately before this packet.
    #[must_use]
    pub const fn is_keyframe(&self) -> bool {
        self.picture.is_keyframe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_keyframe_is_a_cut_point() {
        // What the replay buffer's clip boundaries depend on. An intra picture
        // that is not an IDR looks like a keyframe and is not one: cutting
        // there produces a clip whose first second is broken.
        assert!(PictureKind::Keyframe.is_keyframe());
        assert!(!PictureKind::Intra.is_keyframe());
        assert!(!PictureKind::Predicted.is_keyframe());
        assert!(!PictureKind::Bidirectional.is_keyframe());
        assert!(!PictureKind::Unknown.is_keyframe());
    }

    #[test]
    fn a_packet_reports_what_it_was_built_from() {
        let bytes = [0u8, 0, 0, 1, 0x65];
        let packet = EncodedPacket::new(
            &bytes,
            Duration::from_millis(33),
            Duration::from_millis(33),
            PictureKind::Keyframe,
        );

        assert_eq!(packet.data(), &bytes);
        assert_eq!(packet.presentation_time(), Duration::from_millis(33));
        assert_eq!(packet.decode_time(), Duration::from_millis(33));
        assert!(packet.is_keyframe());
    }
}
