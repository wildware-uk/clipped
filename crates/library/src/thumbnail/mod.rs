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
//! [`Continue`], [`Pace`], [`Unpaced`] and [`ThumbnailService`]'s queue, thread
//! and suspension mechanism come from `clipped-background`
//! ([issue #293](https://github.com/wildware-uk/clipped/issues/293)):
//! `clipped-waveform` needed exactly the same background worker, and
//! `crates/background/src/lib.rs` is where that is explained in full. What
//! stays specific to a thumbnail is `src/thumbnail/render.rs` (the seek,
//! decode, score and encode) and the JPEG-plus-JSON cache format below.
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

pub use cache::{
    PruneReport, Thumbnail, ThumbnailCache, ThumbnailState, DEFAULT_BUDGET_BYTES, IMAGE_EXTENSION,
    SIDECAR_EXTENSION,
};
pub use clipped_background::{
    Continue, Pace, RequestOutcome, SourceIdentity, Unpaced, WorkerPriority, UNKNOWN_MODIFIED,
};
pub use error::ThumbnailError;
pub use render::{
    render, render_paced, RenderedThumbnail, ThumbnailOptions, DEFAULT_QUALITY, DEFAULT_WIDTH,
};
pub use service::{Completion, ServiceOptions, ThumbnailService, DEFAULT_QUEUE_CAPACITY};

/// Every recording in the index worth reading a file for.
///
/// What a library scan hands to the background services that make a picture and
/// a waveform for each one — `ThumbnailService::request` and
/// `WaveformService::request`, both documented for exactly this and both of
/// which had nothing calling them. The thumbnail service, its cache and its
/// renderer were all finished and **unreachable**, and `clipped-waveform` was
/// not a dependency of anything at all, so a shipped build produced neither
/// ([issue #57](https://github.com/wildware-uk/clipped/issues/57),
/// [issue #66](https://github.com/wildware-uk/clipped/issues/66)).
///
/// Recordings whose file has gone or which are in the trash are left out. There
/// is nothing to decode for either, and asking would put a failure in the log
/// once per scan for a file the user already knows about.
///
/// It lives in this module rather than in [`crate::index`] because this is where
/// the first caller was; both consumers now read files off the same list.
///
/// # Errors
///
/// Whatever SQLite reported.
pub fn recordings_worth_reading(
    database: &clipped_storage::Database,
) -> Result<Vec<std::path::PathBuf>, clipped_storage::rusqlite::Error> {
    let mut statement = database.connection().prepare(
        "SELECT path FROM recordings \
         WHERE missing_since IS NULL AND deleted_at IS NULL \
         ORDER BY started_at DESC",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|path| path.map(std::path::PathBuf::from))
        .collect()
}

#[cfg(test)]
mod scan_tests {
    use super::recordings_worth_reading;

    #[test]
    fn a_recording_whose_file_has_gone_is_not_asked_about() {
        // A scan runs after every reconciliation. Asking for a picture of a
        // file that is not there puts a decode failure in the log once per
        // scan, for something the user already knows about.
        let directory = std::env::temp_dir().join(format!(
            "clipped-thumbnail-scan-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a scratch directory");
        let database = clipped_storage::Database::open(directory.join("library.db"))
            .expect("a library can be opened");

        database
            .connection()
            .execute_batch(
                "INSERT INTO sessions (session_id, started_at)
                     VALUES ('s', '2026-01-01T00:00:00Z');
                 INSERT INTO recordings (recording_id, session_id, session_index, path, started_at)
                     VALUES (1, 's', 1, 'here.mkv', '2026-01-03T00:00:00Z');
                 INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,
                                         missing_since)
                     VALUES (2, 's', 2, 'gone.mkv', '2026-01-02T00:00:00Z',
                             '2026-02-01T00:00:00Z');
                 INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,
                                         deleted_at)
                     VALUES (3, 's', 3, 'trashed.mkv', '2026-01-01T00:00:00Z',
                             '2026-02-01T00:00:00Z');",
            )
            .expect("the fixtures can be written");

        let wanted = recordings_worth_reading(&database).expect("the scan can read the index");

        assert_eq!(
            wanted,
            vec![std::path::PathBuf::from("here.mkv")],
            "only the recording that is actually on disk and not in the trash"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }
}
