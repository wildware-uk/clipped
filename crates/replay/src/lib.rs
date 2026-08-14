//! A rolling buffer of encoded media segments, for saving what has just
//! happened.
//!
//! A replay buffer keeps the last few minutes of a game so that a hotkey
//! pressed *after* something interesting still produces a clip of it. The
//! constraint that shapes everything here is in SPEC.md section 16 and is worth
//! stating as a number: a 1080p60 BGRA frame is 8.3 MB, so thirty minutes of
//! raw frames is about 834 GiB, while the same thirty minutes encoded at the
//! bitrate a recording actually uses is about 3.9 GiB. **This crate buffers what
//! comes out of the encoder, never what goes into it**, which is what makes the
//! footprint depend on the bitrate and the duration rather than on the
//! resolution and the frame rate.
//!
//! # Responsibilities
//!
//! - Grouping encoded packets into segments that begin on a keyframe, so that
//!   each one can be decoded on its own: [`ReplayBuffer::push`], [`Segment`].
//! - Keeping the configured window and evicting what falls out of it, within a
//!   ceiling that is arithmetic rather than a guess: [`ReplayConfig`].
//! - Selecting the segments that cover a requested range and holding them
//!   against eviction while a save reads them: [`ReplayBuffer::lease`],
//!   [`SegmentLease`].
//! - Writing those segments out as a playable clip: [`save_clip`],
//!   [`SavedClip`].
//! - Saying what it holds and what it has thrown away: [`ReplayStats`].
//!
//! # Not responsible for
//!
//! Encoding (see `clipped-encoder`), the container itself (see `clipped-muxer`,
//! whose `MkvWriter` a save drives — there is no second muxer here), spilling
//! segments to disk for long durations
//! ([issue #36](https://github.com/wildware-uk/clipped/issues/36)), deciding
//! *when* to save ([issues #38 and
//! #39](https://github.com/wildware-uk/clipped/issues/38)), or what a clip
//! should be called, which belongs to the layer that knows what it is of.
//!
//! # Position in the architecture
//!
//! Sits above `clipped-encoder`, whose packets it consumes, and
//! `clipped-muxer`, which writes the clips it saves; below `clipped-session`,
//! which owns the thread that drives capture into the encoder and pushes each
//! packet here as well as to the muxer. The buffer is a *second consumer of the
//! same packets*, not a second encoder: a recording and a replay buffer running
//! at once encode once, and a clip saved from the buffer is written by the same
//! writer the recording is.
//!
//! # What a save costs, and why
//!
//! A segment begins on a keyframe because a stream can only be cut there, so
//! the keyframe interval is the granularity of a save. A saved clip is
//! therefore **never shorter than was asked for and never more than one segment
//! longer, and the extra is at the front**: it begins at the keyframe at or
//! before the requested start — up to two seconds early at the default segment
//! length — and is trimmed to the requested end.
//! [`SavedClip::leading_slack`] reports how much extra a particular clip
//! carries. `crate::save` sets out why the two ends differ, and
//! `docs/replay-buffer.md` covers the trade-off, the measured memory figures and
//! the behaviour under pressure.
//!
//! # Example
//!
//! ```
//! use core::time::Duration;
//!
//! use clipped_encoder::{BitRate, EncodedPacket, PictureKind};
//! use clipped_replay::{ReplayBuffer, ReplayConfig};
//!
//! // Thirty seconds of 1080p60 at the bitrate `clipped-session` gives it.
//! let config = ReplayConfig::new(
//!     Duration::from_secs(30),
//!     BitRate::bits_per_second(18_662_400).expect("a real rate"),
//! )?;
//! let buffer = ReplayBuffer::new(config);
//!
//! // What the encoding thread does with every packet it drains.
//! let bytes = [0u8; 64];
//! for frame in 0..600u64 {
//!     let at = Duration::from_millis(frame * 1000 / 60);
//!     buffer.push(&EncodedPacket::new(
//!         &bytes,
//!         at,
//!         at,
//!         if frame % 120 == 0 {
//!             PictureKind::Keyframe
//!         } else {
//!             PictureKind::Predicted
//!         },
//!     ));
//! }
//!
//! // What a replay hotkey does. The lease keeps its segments readable even if
//! // the buffer evicts every one of them while the clip is being written.
//! let lease = buffer.lease_last(Duration::from_secs(5))?;
//! assert!(lease.is_complete());
//! assert!(lease
//!     .packets()
//!     .next()
//!     .expect("a lease holds packets")
//!     .is_keyframe());
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```

mod buffer;
mod config;
mod error;
mod lease;
mod range;
mod save;
mod segment;

pub use buffer::{AudioPushOutcome, PushOutcome, ReplayBuffer, ReplayStats};
pub use config::{ReplayConfig, DEFAULT_SEGMENT, MAXIMUM_WINDOW, MINIMUM_WINDOW};
pub use error::{ConfigError, LeaseError};
pub use lease::SegmentLease;
pub use range::TimeRange;
pub use save::{save_clip, SaveError, SavedClip};
pub use segment::{Segment, SegmentAudio, SegmentId, SegmentPacket};
