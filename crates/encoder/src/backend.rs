//! The encoder backend interface: what every encoder does, and in what order.
//!
//! One trait, [`VideoEncoder`], implemented once per encoder family: NVENC
//! here, AMF in [#16](https://github.com/wildware-uk/clipped/issues/16), Quick
//! Sync in [#17](https://github.com/wildware-uk/clipped/issues/17) and the
//! software fallback in
//! [#18](https://github.com/wildware-uk/clipped/issues/18). Nothing in it names
//! a vendor, a graphics API or an operating system; the platform-specific parts
//! are the handles in [`SourceFrame`](crate::SourceFrame), which are opaque to
//! everything above.
//!
//! # Why this shape
//!
//! It is deliberately the same shape as `clipped-capture`'s backend trait, and
//! for the same reasons (`docs/capture-pipeline.md`):
//!
//! - **Frames and packets are borrowed, never owned.** A `&mut self` method
//!   returns a borrow of the encoder, so the compiler will not let a caller
//!   hold two packets at once or keep one across the call that releases it.
//!   Ownership is checked rather than documented and hoped for.
//! - **Nothing is hidden behind a channel.** An encoder is a state machine
//!   driven by one thread. Whoever wants a queue between capture and disk builds
//!   one over this, and can see what it costs.
//! - **Teardown is infallible and idempotent.** There is nothing a caller could
//!   do about a failure to release a session, and a `Result` there would collect
//!   `let _ =` at every call site (AGENTS.md section 15).
//!
//! # There is no factory
//!
//! `clipped-capture` has a factory trait because a pure function picks a
//! backend from declarations without touching hardware. Encoding does not need
//! one: [`recommend`](crate::recommend) already ranks encoder families from the
//! capability report, and opening a session needs a platform device handle that
//! no abstract factory could supply. Each backend is therefore constructed by
//! its own function — `NvencEncoder::open` — and a dispatcher over
//! [`EncoderKind`](crate::EncoderKind) belongs in the crate that has more than
//! one backend to dispatch to (AGENTS.md section 1, "do not over-engineer").

use core::fmt;

use crate::codec::{Codec, EncoderKind};
use crate::config::EncoderConfig;
use crate::error::EncodeError;
use crate::frame::SourceFrame;
use crate::packet::EncodedPacket;

/// A live encoding session.
///
/// # Lifecycle
///
/// ```text
/// <backend>::open(device, config)
///          ↓
///   submit(frame) ──→ next_packet() ⇄ next_packet() → None
///          ↑                                            │
///          └────────────────────────────────────────────┘
///          ↓
///      finish()  ──→ next_packet() ⇄ ... → None
///          ↓
///     shut_down()
/// ```
///
/// Submit a frame, drain every packet it produced, repeat; at the end of the
/// recording call [`finish`](Self::finish), drain again, and shut down. In
/// Rust:
///
/// ```
/// use clipped_encoder::{EncodeError, SourceFrame, VideoEncoder};
///
/// fn encode(
///     encoder: &mut dyn VideoEncoder,
///     frame: &SourceFrame<'_>,
///     out: &mut Vec<u8>,
/// ) -> Result<(), EncodeError> {
///     encoder.submit(frame)?;
///     while let Some(packet) = encoder.next_packet()? {
///         out.extend_from_slice(packet.data());
///     }
///     Ok(())
/// }
/// ```
///
/// Draining before the next submission is not optional. An encoder holds a
/// bounded pool of output buffers, and a caller that submits without draining
/// runs it dry: the encoder reports
/// [`OutputBuffersExhausted`](crate::EncodeErrorKind::OutputBuffersExhausted)
/// rather than blocking, because a capture thread that blocks is a capture
/// thread that is dropping frames (AGENTS.md section 20).
///
/// A submission does not always produce a packet. An encoder configured to
/// reorder pictures holds frames back until it has enough of them, so
/// `next_packet` returning `None` is ordinary; the packets appear after a later
/// submission, or after `finish`.
///
/// # Ownership
///
/// The encoder owns its session, its output buffers and every registration it
/// makes with the driver, from the moment it is opened until `shut_down` or
/// `Drop` — whichever comes first, and an implementation must do the same work
/// in both, so that a panic on the encoding thread cannot leak a hardware
/// session. Leaking one is not a slow leak: consumer GPUs cap how many
/// encoding sessions may exist at once, and a leaked one is taken from every
/// application on the machine until the process exits (AGENTS.md section 58).
///
/// It owns nothing it is given. The graphics device belongs to the caller and
/// the textures belong to the capture backend; see [`crate::frame`].
///
/// # Threading
///
/// One encoder belongs to one thread. It is [`Send`], so a session can build it
/// on one thread and move it to the encoding thread, and deliberately not
/// `Sync`: every method takes `&mut self` and nothing shares it.
///
/// That thread is usually the capture thread, because a frame texture may not
/// outlive the capture that produced it, so the frame must be submitted before
/// the capture loop moves on. Packets are what cross to another thread — they
/// are bytes, and a muxer, a disk or a replay buffer can take as long as it
/// likes with a copy of them.
pub trait VideoEncoder: fmt::Debug + Send {
    /// Which encoder family this is, for reporting and for logs.
    fn encoder(&self) -> EncoderKind;

    /// What this session was configured to produce.
    fn configuration(&self) -> &EncoderConfig;

    /// The codec being produced, which is `self.configuration().codec()`.
    fn codec(&self) -> Codec {
        self.configuration().codec()
    }

    /// The out-of-band header a container needs before the first packet.
    ///
    /// The sequence and picture parameter sets for H.264 and HEVC, the sequence
    /// header for AV1. A container that stores this separately — MP4 does —
    /// needs it before the first frame is written, and an elementary stream
    /// does not, because the encoder repeats it in the bitstream at every
    /// keyframe.
    ///
    /// Available from the moment the session opens, so a muxer can be
    /// configured before a single frame has been captured.
    fn parameter_sets(&self) -> &[u8];

    /// Hands one frame to the encoder.
    ///
    /// The texture is read during this call and never afterwards, so the caller
    /// may release the frame as soon as it returns.
    ///
    /// # Errors
    ///
    /// [`EncodeError`], with the encoder, codec and resolution attached. A
    /// frame whose format or size does not match the configuration is refused
    /// rather than encoded into a corrupt picture, and so is one whose
    /// presentation time is earlier than the previous frame's.
    fn submit(&mut self, frame: &SourceFrame<'_>) -> Result<(), EncodeError>;

    /// Takes the next encoded packet, if one is ready.
    ///
    /// Returns [`None`] when everything submitted so far has been drained. The
    /// packet borrows the encoder's output buffer and is released by the next
    /// call, which the borrow checker enforces:
    ///
    /// ```compile_fail
    /// # use clipped_encoder::{EncodeError, VideoEncoder};
    /// fn hold_two(encoder: &mut dyn VideoEncoder) -> Result<(), EncodeError> {
    ///     let first = encoder.next_packet()?;
    ///     // error[E0499]: cannot borrow `*encoder` as mutable more than once
    ///     let second = encoder.next_packet()?;
    ///     drop((first, second));
    ///     Ok(())
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// [`EncodeError`] if the hardware failed while producing the packet.
    fn next_packet(&mut self) -> Result<Option<EncodedPacket<'_>>, EncodeError>;

    /// Declares the end of the stream and flushes what the encoder is holding.
    ///
    /// Drain with [`next_packet`](Self::next_packet) afterwards: an encoder
    /// that reorders pictures has frames in hand that only come out now.
    /// Submitting after this is a programming error and is refused.
    ///
    /// # Errors
    ///
    /// [`EncodeError`] if the flush itself failed. The packets already drained
    /// remain valid — a recording that ends badly still keeps everything
    /// written before the failure (AGENTS.md section 17).
    fn finish(&mut self) -> Result<(), EncodeError>;

    /// Releases the session and every resource behind it.
    ///
    /// Infallible and idempotent by contract, and must do the same work as
    /// `Drop`.
    fn shut_down(&mut self);
}
