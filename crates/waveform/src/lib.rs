//! Audio waveform peaks for the timeline and the clip editor.
//!
//! This crate answers one question about a finished recording: for each of its
//! audio tracks, how loud was it at each moment? That is what a timeline draws
//! under a video (SPEC.md section 18) and what the clip editor shows as separate
//! audio rows (SPEC.md section 19, issue #66).
//!
//! ```text
//! VIDEO        █████████████████
//!
//! GAME         ▃▅▆▃▇▆▄▅▄▄▅▇▅▄
//!
//! MIC          ▁▁▃▇▅▁▁▃▆▁▁▁▅▁
//!
//! DISCORD      ▂▂▁▅▁▁▇▂▁▅▃▁▁▁
//! ```
//!
//! # Responsibilities
//!
//! - Decoding the audio of a **finished** file and reducing it to peaks
//!   ([`analyse`]).
//! - Holding those peaks at several resolutions so that zooming is neither wrong
//!   nor slow ([`Peak`], [`TrackWaveform`]).
//! - Keeping them in a cache with invalidation and cleanup rules
//!   ([`WaveformCache`]).
//! - Running the whole thing where it cannot compete with a recording
//!   ([`WaveformService`]).
//!
//! # Not responsible for
//!
//! Capture, playback, or anything that touches an audio device. Nothing here
//! opens one. It reads files that have already been written, which is precisely
//! why it is safe to run while a game is being recorded — subject to priority,
//! which is [`WaveformService`]'s subject.
//!
//! It does not draw anything either. The timeline (issue #65) and the clip
//! editor (issue #83) are the consumers; this crate produces the numbers they
//! draw, and neither consumer exists yet.
//!
//! # Position in the architecture
//!
//! Layer 1: it depends on `clipped-logging`, on `clipped-background` and on
//! the FFmpeg binding, and on nothing else in this workspace. It names
//! `rusty_ffmpeg` directly because there is no lower-layer route to a
//! demuxer — `clipped-muxer` owns the safe wrappers and sits at layer 2 —
//! which is the case `docs/adr/0004-ffmpeg-dependency-strategy.md` permits.
//!
//! [`Pace`], [`Continue`], [`Unpaced`] and [`WaveformService`]'s queue, thread
//! and suspension mechanism come from `clipped-background`
//! ([issue #293](https://github.com/wildware-uk/clipped/issues/293)): the
//! thumbnail module of `clipped-library` needed exactly the same background
//! worker, and `crates/background/src/lib.rs` is where that is explained in
//! full. This crate's own half is what remains specific to a waveform:
//! demuxing and decoding a recording's audio (`src/analyse.rs`), and the
//! `.cwf` cache format (`src/cache.rs`, `src/format.rs`).
//!
//! # Where things are written
//!
//! Peaks are derived data: they can always be recomputed from the recording, so
//! losing them costs time and nothing else. They therefore live in a **cache**
//! of documented sidecar files (`docs/waveforms.md`, AGENTS.md section 32) under
//! Clipped's per-user directory, and not in the database. `src/format.rs` makes
//! that argument in full, and says how the index would move into SQLite if it
//! ever should.
//!
//! # A recording with no audio
//!
//! Recordings written today have no audio track at all: multi-track audio is
//! issue #180, and the muxer writes video alone until it lands. That is a
//! supported answer here rather than a failure — [`Waveform::is_silent`] — and
//! so is a file this build cannot decode. Every read path degrades to a
//! timeline with no audio rows ([`WaveformState`]) rather than to an error.
//!
//! # Getting a waveform
//!
//! ```no_run
//! use core::num::NonZeroUsize;
//! use clipped_waveform::{ServiceOptions, WaveformCache, WaveformService};
//!
//! # fn example() -> Option<()> {
//! let service = WaveformService::start(
//!     WaveformCache::in_default_directory()?,
//!     ServiceOptions::new(),
//! );
//!
//! // A timeline asks for what it needs and draws whatever it is given. This is
//! // `Pending` the first time and `Ready` once the worker has caught up.
//! let state = service.waveform(r"C:\Videos\Clipped\match.mkv");
//! for track in state.tracks() {
//!     let row = track.overview(NonZeroUsize::new(1_920)?);
//!     println!("{:?}: {} peaks", track.descriptor().name(), row.len());
//! }
//!
//! // A recording has started, so nothing else should be reading the disk.
//! service.suspend_for_recording();
//! # Some(())
//! # }
//! ```

mod analyse;
mod cache;
mod error;
mod format;
mod peaks;
mod samples;
mod service;
mod waveform;

pub use analyse::{analyse, analyse_paced};
pub use cache::{PruneReport, WaveformCache, DEFAULT_BUDGET_BYTES, ENTRY_EXTENSION};
pub use clipped_background::{
    Continue, Pace, RequestOutcome, SourceIdentity, Unpaced, WorkerPriority, UNKNOWN_MODIFIED,
};
pub use error::WaveformError;
pub use peaks::{Peak, BASE_BUCKET, MAX_BASE_BUCKETS, OVERVIEW_BUCKETS};
pub use service::{Completion, ServiceOptions, WaveformService, DEFAULT_QUEUE_CAPACITY};
pub use waveform::{TrackDescriptor, TrackWaveform, Waveform, WaveformState};
