//! Rendering an edit document to a new file.
//!
//! An export is the function that turns an edit into something a person can
//! send somebody. The document says *what* the clip is — which recordings,
//! which parts of them, in which order, with which audio (`clipped-edit`) — and
//! this crate turns that into a file, without touching any of the recordings it
//! drew on (AGENTS.md sections 56 and 57).
//!
//! [`docs/exporting.md`](../../../docs/exporting.md) is the subsystem document:
//! the copy-or-re-encode decision, the frame rules an export is measured
//! against, and what is not built yet are argued there. This is the map of the
//! implementation.
//!
//! # Responsibilities
//!
//! - **Deciding how.** [`ExportPlan`] answers whether an edit can be rendered
//!   by copying the coded packets the recording already holds, or whether it
//!   needs every frame decoded and encoded again — and names every reason.
//!   Nothing is written to answer it.
//! - **Doing it, for a copy.** [`export`] reads the recording, cuts it where
//!   the document says, moves the timestamps onto the clip's own timeline and
//!   writes the result through `clipped_muxer::MkvWriter`.
//! - **Progress and cancellation** ([`ExportOptions`], [`Cancellation`]),
//!   because an export is the longest thing a user waits for.
//! - **Leaving nothing behind.** A failure or a cancellation removes the
//!   part-written file.
//!
//! # Not responsible for
//!
//! - **Re-encoding.** Not built. An edit that needs it is refused with
//!   [`ExportError::ReencodeRequired`] naming what forced it, rather than
//!   exported as something that is not the clip (AGENTS.md section 54). See
//!   `docs/exporting.md`, "What is not built yet".
//! - **Containers.** `clipped-muxer` writes the file; there is no second muxer
//!   here (AGENTS.md section 55).
//! - **Naming the file, or deciding where it goes.** The caller chooses the
//!   path, and a path that is already taken is refused rather than overwritten.
//! - **Which thread it runs on.** [`export`] blocks; the caller runs it
//!   somewhere that is not capturing.
//!
//! # Position in the architecture
//!
//! Layer 3 of the dependency table in README.md, beside `clipped-replay` and
//! above `clipped-muxer`, because it drives the container writer. It links
//! FFmpeg to *read* recordings, which nothing at a lower layer exposes — the
//! same reason, and the same ADR 0004 amendment, that `clipped-waveform` cites.
//!
//! # The two kinds of time, and the one conversion
//!
//! `clipped-edit` distinguishes **source time** — a position in a recording —
//! from **output time**, a position on the edited timeline, so that mixing them
//! is a compile error rather than a bug report about lip sync. An export is the
//! function between them, and it is one line of arithmetic:
//!
//! ```text
//!   output = segment.output_start + (source − segment.span.start)
//! ```
//!
//! Every packet the copy writes goes through it, and
//! [`PlannedSegment::output_of`] is where it lives so that it can be tested on
//! its own.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//!
//! use clipped_edit::{EditDocument, RecordingId, SourceSpan, SourceTime};
//! use clipped_export::{export, plan_export, Cancellation, ExportOptions, SourceFiles};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let span = SourceSpan::new(SourceTime::ZERO, SourceTime::from_nanos(8_000_000_000))
//!     .expect("the span ends after it starts");
//! let document = EditDocument::from_recording("Ace", RecordingId::new("rec-1"), span);
//! let sources = SourceFiles::new().with(RecordingId::new("rec-1"), "match.mkv");
//!
//! // What will this cost? Answered without writing anything.
//! let plan = plan_export(&document, &sources)?;
//! for blocker in plan.blockers() {
//!     eprintln!("this cannot be a fast copy: {blocker}");
//! }
//!
//! let cancellation = Cancellation::new();
//! let report = |progress: clipped_export::ExportProgress| {
//!     eprintln!("{:.0}%", progress.fraction() * 100.0);
//! };
//! let options = ExportOptions::new()
//!     .reporting_to(&report)
//!     .cancelled_by(cancellation.clone());
//!
//! let export = export(&document, &sources, Path::new("ace.mkv"), &options)?;
//! println!("{export}");
//! # Ok(())
//! # }
//! ```

mod error;
mod media;
pub mod plan;
pub mod progress;
mod render;
pub mod source;

pub use crate::error::ExportError;
pub use crate::plan::{
    CopyBlocker, ExportMethod, ExportPlan, MixReason, PlanError, PlannedAudioTrack, PlannedSegment,
};
pub use crate::progress::{Cancellation, ExportOptions, ExportProgress, DEFAULT_PROGRESS_INTERVAL};
pub use crate::render::{export, plan_export, Export, SourceFiles};
pub use crate::source::{IndexedFrame, SourceProfile, SourceStream, StreamFormat, VideoFrameIndex};
