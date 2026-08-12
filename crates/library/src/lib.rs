//! Recording library indexing, search and metadata.
//!
//! The library is a view over what is actually on disk. Indexing reconciles the
//! database against the filesystem rather than assuming the two agree, because
//! users move and delete files behind the application's back.
//!
//! # Responsibilities
//!
//! - Indexing games, sessions, recordings and clips.
//! - [`virtual_clip`]: what a clip *is* before anything has been exported — a
//!   range of a recording, and why it exists. `docs/highlights.md` argues it.
//! - Search, favourites and tags.
//! - Thumbnail and waveform bookkeeping.
//!
//! # Not responsible for
//!
//! Storage primitives (see `clipped-storage`) or generating media. Nor
//! *deciding* which moments deserve a clip: the highlight rules are
//! [issue #75](https://github.com/wildware-uk/clipped/issues/75) and generation
//! is [#76](https://github.com/wildware-uk/clipped/issues/76). This crate owns
//! the shape of what they produce.
//!
//! # Position in the architecture
//!
//! Sits above `clipped-storage` and below `clipped-session`. It also names the
//! two layer 0 vocabularies a clip is made of — `clipped-edit` for the range
//! and `clipped-events` for the reason — which is why the virtual clip model
//! is here and not in either of them: those two crates are both at layer 0 and
//! cannot name each other, and this is the lowest crate that can see both.

pub mod virtual_clip;

pub use virtual_clip::{
    window_around, ClipOrigin, ClipState, HighlightCause, SourceAvailability, SourceDeletion,
    VirtualClip,
};
