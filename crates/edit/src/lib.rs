//! What an edit *is*: metadata over recordings that are never touched.
//!
//! A clip in Clipped is not a copy of a recording with the boring parts cut
//! out. It is a document that says which recordings to play, which parts of
//! them, in which order, how loud each audio track should be and what text to
//! draw over the picture. The recordings themselves are never modified, moved
//! or re-encoded because somebody made a clip — an export writes a *new* file,
//! and deleting every clip a user ever made leaves the originals byte for byte
//! as the recorder wrote them (AGENTS.md sections 56 and 57, SPEC.md section
//! 2).
//!
//! [`docs/editing.md`](../../../docs/editing.md) is the specification: the time
//! model, the persistence decision and the compatibility rules are argued
//! there. This documentation is the map of the implementation.
//!
//! # Responsibilities
//!
//! - The document: [`EditDocument`] and everything reachable from it.
//! - **Reading it deterministically.** [`EditDocument::locate`] turns a
//!   position on the edited timeline into a position in one source recording,
//!   which is the question an exporter ([issue
//!   #89](https://github.com/wildware-uk/clipped/issues/89)) and a preview
//!   ([issue #83](https://github.com/wildware-uk/clipped/issues/83)) both ask
//!   several thousand times per clip and must never answer differently.
//! - **Editing it.** Trimming the ends, splitting at the playhead, deleting a
//!   section, changing what each audio track sounds like, and undoing any of
//!   it: [`operations`] and [`history`]. Every one of them is arithmetic over
//!   the same two kinds of time, and none of them goes anywhere near a
//!   recording.
//! - **The mix.** What each audio track of the export is fed by, how loud it
//!   is, whether it is silenced and how it fades: [`audio`]. Soloing a track is
//!   deliberately *not* part of the document — it is [`Solo`], which the editor
//!   holds while it listens.
//! - Validation: what makes a document readable at all, checked on the way in
//!   and on the way out.
//! - The encoding, its version, and the migration of documents written by an
//!   older build ([`schema`]).
//!
//! # Not responsible for
//!
//! **Storing anything.** This crate performs no file or database access at
//! all: [`EditDocument::write`] hands back a `String` and
//! [`EditDocument::read`] takes one, and where that text lives is the
//! database's business ([issue
//! #55](https://github.com/wildware-uk/clipped/issues/55)). That is not
//! squeamishness about I/O — it is the cheapest possible guarantee that
//! editing cannot damage a recording, because a crate that never opens a file
//! cannot rewrite one. `tests/sources_are_never_touched.rs` asserts it against
//! the source of this crate rather than trusting this paragraph.
//!
//! **The rest of the edits.** [`operations`] covers what issues
//! [#84](https://github.com/wildware-uk/clipped/issues/84) and
//! [#85](https://github.com/wildware-uk/clipped/issues/85) ask for and stops
//! there: framing and speed are
//! [#86](https://github.com/wildware-uk/clipped/issues/86), overlays
//! [#87](https://github.com/wildware-uk/clipped/issues/87) and combining
//! recordings [#88](https://github.com/wildware-uk/clipped/issues/88). The
//! model those tickets change is already here, and so is the timeline
//! arithmetic they have to agree with, so that six tickets do not each invent
//! their own.
//!
//! **Rendering.** Nothing here decodes, draws or encodes a frame.
//!
//! # Position in the architecture
//!
//! A leaf crate, at layer 0 of the dependency table in README.md, and for the
//! reason `clipped-ipc` is there: an edit document is read by *both* ends of
//! the application. The editor in the desktop process shows it, the recorder
//! process exports it, and the database stores it as text without
//! understanding it. A document model that reached into the recording engine
//! could not be linked by the half of the system that only wants to draw a
//! timeline, so this crate depends on no other crate in this workspace and
//! holds no application logic.
//!
//! # Two kinds of time, and never one
//!
//! The moment an edit contains a cut or a speed change, "three seconds in"
//! means two different things, and a model that blurs them produces an export
//! whose audio is a few frames adrift of its picture. So there are two types:
//!
//! - [`SourceTime`] — a position in a source recording's own timeline, counted
//!   in nanoseconds from that recording's first frame. Exactly what the
//!   recorder wrote: `clipped-muxer` rescales nanoseconds into the container's
//!   time base, so a `SourceTime` needs no conversion to be looked up in the
//!   file.
//! - [`OutputTime`] — a position on the *edited* timeline, counted in
//!   nanoseconds from the start of the clip. It is what the playhead in the
//!   editor sits at, what an overlay's timing range is measured in, and what
//!   the exported file's own timeline will be.
//!
//! They are separate types rather than a convention, so mixing them is a
//! compile error rather than a bug report about lip sync.
//!
//! ```text
//!   source A  ├────────────────────────────────────────────────┤
//!                  ╰── 8s ──╯            ╰── 12s ──╯
//!   source B  ├──────────────────────────────┤
//!                    ╰─ 4s ─╯
//!
//!   output    ├── segment 0 ──┤── segment 1 ──┤─ segment 2 ─┤
//!             0s              8s             20s           24s
//! ```
//!
//! # Example
//!
//! ```
//! use clipped_edit::{
//!     AudioTrack, EditDocument, EditOperation, OutputTime, RecordingId, SourceId, SourceSpan,
//!     SourceTime, TrackInput, TrackOutput,
//! };
//!
//! // Twelve seconds of a recording, starting thirty seconds in.
//! let span = SourceSpan::new(
//!     SourceTime::from_nanos(30_000_000_000),
//!     SourceTime::from_nanos(42_000_000_000),
//! )
//! .expect("the span ends after it starts");
//! let document = EditDocument::from_recording("Ace", RecordingId::new("rec-1"), span);
//!
//! let text = document.write()?;
//! let reloaded = EditDocument::read(&text)?;
//! assert_eq!(reloaded.document, document);
//!
//! // Two seconds into the clip is thirty-two seconds into the recording.
//! let placement = document
//!     .locate(OutputTime::from_nanos(2_000_000_000))
//!     .expect("two seconds in is inside the clip");
//! assert_eq!(placement.source_time, SourceTime::from_nanos(32_000_000_000));
//!
//! // Editing it moves material in output time and never in source time.
//! let trimmed = document.apply(EditOperation::TrimStart {
//!     at: OutputTime::from_nanos(2_000_000_000),
//! })?;
//! let placement = trimmed
//!     .locate(OutputTime::ZERO)
//!     .expect("the clip now starts where the trim did");
//! assert_eq!(placement.source_time, SourceTime::from_nanos(32_000_000_000));
//!
//! // And Discord was too loud, which is a slider rather than a re-record.
//! let mixed = document
//!     .with_audio_track(AudioTrack::new(
//!         "Discord",
//!         vec![TrackInput::new(SourceId::new(0), 2)],
//!     ))
//!     .apply(EditOperation::SetTrackGain {
//!         track: 0,
//!         gain_db: -8.0,
//!     })?;
//! assert_eq!(
//!     mixed.track_output(0),
//!     Some(TrackOutput::Audible { gain_db: -8.0 })
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod audio;
pub mod document;
pub mod error;
pub mod framing;
pub mod history;
pub mod operations;
pub mod overlay;
pub mod schema;
pub mod segment;
pub mod source;
pub mod time;
pub mod timeline;

pub use audio::{AudioTrack, Solo, TrackInput, TrackOutput};
pub use document::EditDocument;
pub use error::{DocumentProblem, EditDocumentError, OperationRefused};
pub use framing::{AspectRatio, CropRect, Rotation};
pub use history::{EditHistory, MAX_UNDO_STEPS};
pub use operations::EditOperation;
pub use overlay::{OverlayPosition, TextOverlay};
pub use schema::{Loaded, Migrated, SCHEMA_VERSION};
pub use segment::Segment;
pub use source::{RecordingId, Source, SourceId};
pub use time::{OutputSpan, OutputTime, SourceSpan, SourceTime, Speed};
pub use timeline::Placement;
