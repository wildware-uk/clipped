//! A picture for every recording in the library.
//!
//! Every screen that lists a recording needs one: the library grid, the game
//! page, the clip list and search results (SPEC.md sections 22 and 30). This
//! module makes them, keeps them, invalidates them and cleans them up, and does
//! all of it where a game will not notice.
//!
//! ```text
//! recording.mkv ──► seek to three candidates ──► score each frame
//!                                                     │
//!                          keep the one with the most variety
//!                                                     │
//!                              scale to 640 px ──► JPEG ──► cache
//! ```
//!
//! # Responsibilities
//!
//! - Choosing **which** frame is worth showing (`src/thumbnail/choose.rs`, and
//!   the argument for not taking the first one).
//! - Decoding that frame and encoding it as a JPEG ([`render`], [`render_paced`]).
//! - Keeping the result, invalidating it when the recording changes, and
//!   cleaning up after recordings that are gone ([`ThumbnailCache`]).
//! - Running the whole thing where it cannot compete with a recording
//!   ([`ThumbnailService`]).
//!
//! # Not responsible for
//!
//! Drawing anything. The library screen
//! ([issue #52](https://github.com/wildware-uk/clipped/issues/52)) is the
//! consumer and it is not built; this module produces a JPEG on disk and a path
//! to it, and what a tile does with that is the interface's business.
//!
//! It does not capture, encode or mux either. Nothing here opens a capture
//! device, a GPU adapter or an encoder session — it reads files that have
//! already been written, which is precisely why it is safe to run while a game
//! is being recorded, subject to priority and suspension
//! ([`ThumbnailService`]).
//!
//! # Position in the architecture
//!
//! `clipped-library` is layer 1, so this module may name `rusty_ffmpeg`
//! directly: `clipped-muxer` owns the workspace's safe wrappers over the
//! container API and sits at layer 2, above this crate, so depending on it would
//! invert the direction `tests/integration/tests/workspace_layering.rs` asserts.
//! `docs/adr/0004-ffmpeg-dependency-strategy.md` permits exactly that case, and
//! `crates/encoder` and `crates/waveform` are the other two crates that use the
//! allowance.
//!
//! It lives in `clipped-library` because a thumbnail is a property of a library
//! item, and because this crate already owns what a thumbnail costs on disk
//! ([`StorageCategory::Thumbnails`](crate::accounting::StorageCategory)).
//! `docs/thumbnails.md` records the alternative that was considered — a crate of
//! its own, beside `clipped-waveform` — and why it was not taken in this ticket.
//!
//! # Where the pictures live
//!
//! A directory of JPEGs and JSON sidecars under Clipped's per-user directory,
//! not in the database: AGENTS.md section 31 forbids media blobs in SQLite,
//! #55's schema deliberately has no BLOB column, and a picture that can be made
//! again in tens of milliseconds does not deserve a migration.
//! `src/thumbnail/cache.rs` makes that argument in full and
//! `docs/thumbnails.md` documents the sidecar format, which is what AGENTS.md
//! section 32 asks of a format that is not SQLite.
//!
//! # When there is no picture
//!
//! Every read path degrades to a tile with no picture rather than to an error
//! (issue #57's third acceptance criterion). [`ThumbnailState`] has exactly
//! three answers — not yet, here it is, and here is why there will not be one —
//! and none of them is a failure a screen has to handle. A recording whose every
//! frame is black still gets a picture, marked
//! [`is_blank`](Thumbnail::is_blank), because that is what the recording looks
//! like and inventing something else would be fake data (AGENTS.md section 27).
//!
//! # Getting a thumbnail
//!
//! ```no_run
//! use clipped_library::thumbnail::{ServiceOptions, ThumbnailCache, ThumbnailService};
//!
//! # fn example() -> Option<()> {
//! let service = ThumbnailService::start(
//!     ThumbnailCache::in_default_directory()?,
//!     ServiceOptions::new(),
//! );
//!
//! // A library screen asks per tile and draws whatever it is given. This is
//! // `Pending` the first time and `Ready` once the worker has caught up.
//! let state = service.thumbnail(r"C:\Videos\Clipped\match.mkv");
//! match state.image_path() {
//!     Some(picture) => println!("draw {}", picture.display()),
//!     None => println!("draw a tile with no picture"),
//! }
//!
//! // A recording has started, so nothing else should be reading the disk.
//! service.suspend_for_recording();
//! # Some(())
//! # }
//! ```

mod cache;
mod choose;
mod error;
mod render;
mod service;
mod source;

#[cfg(windows)]
mod windows;

pub use cache::{
    PruneReport, Thumbnail, ThumbnailCache, ThumbnailState, DEFAULT_BUDGET_BYTES, IMAGE_EXTENSION,
    SIDECAR_EXTENSION,
};
pub use error::ThumbnailError;
pub use render::{
    render, render_paced, Continue, Pace, RenderedThumbnail, ThumbnailOptions, Unpaced,
    DEFAULT_QUALITY, DEFAULT_WIDTH,
};
pub use service::{
    Completion, RequestOutcome, ServiceOptions, ThumbnailService, WorkerPriority,
    DEFAULT_QUEUE_CAPACITY,
};
pub use source::{SourceIdentity, UNKNOWN_MODIFIED};
