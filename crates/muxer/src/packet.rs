//! Encoded packets on their way into a container.
//!
//! The muxer takes what an encoder produced and does not look inside it. What
//! it needs from the caller is which track the bytes belong to, when they are
//! to be presented, and whether a decoder can start here — everything a
//! Matroska block carries besides the payload itself.

use core::fmt;
use core::time::Duration;

use crate::track::TrackId;

/// A moment on the clock a recording is timed against, in nanoseconds.
///
/// # Which clock
///
/// One recording's packets must all be timed against the *same* monotonic
/// clock, whatever it is, because the only thing the muxer can do with two
/// timestamps is subtract them. On Windows that clock is the high-resolution
/// performance counter: `clipped-capture` stamps frames with
/// `Direct3D11CaptureFrame::SystemRelativeTime` and Windows audio capture
/// reports positions against the same counter, so video and audio timestamps
/// are comparable without a conversion nobody can check
/// (`crates/capture/src/time.rs` sets out why the clock is read from the frame
/// source rather than when a frame arrives).
///
/// The zero point does not matter and is not assumed: the writer rebases every
/// timestamp onto the first packet of the file, so a counter that started at
/// system boot is as good as one that started at zero. What matters is that
/// nothing in a recording mixes two clocks.
///
/// # Why nanoseconds, and why not a `Duration`
///
/// Nanoseconds because that is the resolution both Windows clocks report and
/// the unit `clipped-capture` already converts to. Signed because the writer
/// works in differences from the file's origin, and a packet whose timestamp
/// precedes the first one written produces a negative difference that has to
/// be representable to be handled (see [`crate::writer`]). `Duration` cannot
/// hold one, and unsigned arithmetic would turn it into an enormous positive
/// number, which is exactly the kind of silent wrap that ends up as a
/// recording with one frame at the far end of the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PacketTimestamp(i64);

impl PacketTimestamp {
    /// The reading, in nanoseconds on the recording's clock.
    #[must_use]
    pub const fn from_nanos(nanos: i64) -> Self {
        Self(nanos)
    }

    /// The reading, in nanoseconds on the recording's clock.
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }
}

impl fmt::Display for PacketTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}ns", self.0)
    }
}

/// One encoded packet, borrowed for as long as it takes to write it.
///
/// The payload is borrowed rather than owned because a recorder produces one
/// of these per frame for hours: taking a `Vec` here would mean an allocation
/// and a copy per frame on the path between the encoder and the disk
/// (AGENTS.md section 18). The bytes only have to stay alive for the call that
/// writes them.
///
/// The payload is the codec's own bitstream, exactly as the encoder produced
/// it. For H.264 and HEVC that is Annex B — the form every Windows hardware
/// encoder emits — and FFmpeg's Matroska muxer converts it to the
/// length-prefixed form the container stores; see `docs/muxing.md`.
#[derive(Debug, Clone, Copy)]
pub struct EncodedPacket<'data> {
    track: TrackId,
    presentation: PacketTimestamp,
    decode: Option<PacketTimestamp>,
    duration: Option<Duration>,
    keyframe: bool,
    data: &'data [u8],
}

impl<'data> EncodedPacket<'data> {
    /// An encoded packet for `track`, to be presented at `presentation`.
    ///
    /// The packet is assumed not to be a keyframe and to have no separate
    /// decode timestamp, which is the shape of every audio packet and of every
    /// video packet from an encoder that does not reorder.
    #[must_use]
    pub const fn new(track: TrackId, presentation: PacketTimestamp, data: &'data [u8]) -> Self {
        Self {
            track,
            presentation,
            decode: None,
            duration: None,
            keyframe: false,
            data,
        }
    }

    /// Gives the packet a decode timestamp distinct from its presentation one.
    ///
    /// Only an encoder that reorders needs this. When a codec uses B-frames,
    /// packets leave the encoder in decode order and their presentation
    /// timestamps go up and down; the container needs both, and it is the
    /// decode order that has to increase. Without one, the decode timestamp is
    /// the presentation timestamp, which is correct for everything that does
    /// not reorder and wrong in a way the muxer cannot detect for anything
    /// that does — hence a value the encoder supplies rather than one guessed
    /// here.
    #[must_use]
    pub const fn with_decode_timestamp(mut self, decode: PacketTimestamp) -> Self {
        self.decode = Some(decode);
        self
    }

    /// How long the packet occupies, when the encoder knows.
    ///
    /// Matroska stores no duration for a normal block, so this is a hint the
    /// muxer passes on: it decides the duration of the *last* block of a track,
    /// which is otherwise assumed to be zero and makes a file read as ending
    /// one frame early.
    #[must_use]
    pub const fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// Marks the packet as one a decoder can start at.
    ///
    /// Matroska stores this per block and a player uses it to seek. Getting it
    /// wrong does not corrupt the file, but it makes seeking land on a frame
    /// that cannot be decoded, so it is taken from the encoder rather than
    /// inferred from the bitstream.
    #[must_use]
    pub const fn with_keyframe(mut self, keyframe: bool) -> Self {
        self.keyframe = keyframe;
        self
    }

    /// Which track this belongs to.
    #[must_use]
    pub const fn track(&self) -> TrackId {
        self.track
    }

    /// When the packet is to be presented.
    #[must_use]
    pub const fn presentation(&self) -> PacketTimestamp {
        self.presentation
    }

    /// When the packet is to be decoded, which is when it is to be presented
    /// unless the encoder said otherwise.
    #[must_use]
    pub const fn decode(&self) -> PacketTimestamp {
        match self.decode {
            Some(decode) => decode,
            None => self.presentation,
        }
    }

    /// The duration the encoder gave, if it gave one.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Whether a decoder can start here.
    #[must_use]
    pub const fn is_keyframe(&self) -> bool {
        self.keyframe
    }

    /// The encoded bytes.
    #[must_use]
    pub const fn data(&self) -> &'data [u8] {
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_without_a_decode_timestamp_decodes_when_it_is_presented() {
        let packet = EncodedPacket::new(
            TrackId::Audio(0),
            PacketTimestamp::from_nanos(1_234_000_000),
            &[1, 2, 3],
        );

        assert_eq!(packet.decode(), packet.presentation());
        assert!(!packet.is_keyframe());
        assert_eq!(packet.duration(), None);
    }

    #[test]
    fn a_reordered_packet_keeps_both_timestamps() {
        // What a B-frame looks like arriving from an encoder: it is decoded
        // before the frame it is displayed after.
        let packet = EncodedPacket::new(
            TrackId::Video,
            PacketTimestamp::from_nanos(100_000_000),
            &[0; 4],
        )
        .with_decode_timestamp(PacketTimestamp::from_nanos(66_000_000))
        .with_duration(Duration::from_nanos(16_666_667))
        .with_keyframe(true);

        assert_eq!(packet.presentation().as_nanos(), 100_000_000);
        assert_eq!(packet.decode().as_nanos(), 66_000_000);
        assert_eq!(packet.duration(), Some(Duration::from_nanos(16_666_667)));
        assert!(packet.is_keyframe());
    }
}
