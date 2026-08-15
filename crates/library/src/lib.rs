//! Recording library indexing, search and metadata.
//!
//! The library is a view over what is actually on disk. Indexing reconciles the
//! database against the filesystem rather than assuming the two agree, because
//! users move and delete files behind the application's back.
//!
//! # Responsibilities
//!
//! - Indexing games, sessions, recordings and clips.
//! - Search, favourites and tags.
//! - Thumbnails, and waveform bookkeeping.
//! - Storage accounting and the limits configured against it:
//!   [`accounting`].
//! - The trash a deletion goes to, and the restore that undoes it: [`trash`].
//!
//! # Not responsible for
//!
//! Storage primitives (see `clipped-storage`), capture, encoding or muxing.
//! [`thumbnail`] is the one module that writes files of its own, and everything
//! it writes is a disposable copy of something the recording still holds.
//!
//! **[`trash`] is the only module here that can remove a recording**, it can
//! only remove one that is already in the trash, and it can only unlink a file
//! that is inside the trash directory. Measuring what is on disk
//! ([`accounting`]) and reconciling the index against it ([`index`]) still
//! delete nothing at all: a recording that has gone is marked, never removed
//! (AGENTS.md section 56).
//!
//! # Position in the architecture
//!
//! Sits above `clipped-storage` and below `clipped-session`.
//!
//! # What exists today
//!
//! [`search`]: the query language a user types into the search box, its parser
//! and a matcher for it (SPEC.md section 30, `docs/search.md`). It is a
//! language and its meaning, and it deliberately touches no database — the
//! index that will run it over a real library is
//! [issue #56](https://github.com/wildware-uk/clipped/issues/56), and the
//! database under that is
//! [issue #55](https://github.com/wildware-uk/clipped/issues/55).
//!
//! [`accounting`]: what Clipped has put on disk and what a quota permits
//! (SPEC.md section 27, `docs/storage-management.md`). It measures and reports;
//! removing anything is
//! [issue #111](https://github.com/wildware-uk/clipped/issues/111).
//!
//! [`index`]: the reconciliation between the session sidecars the recorder
//! writes, the media files beside them, and the SQLite index. It is the only
//! thing that writes library rows, and `docs/library.md` is its prose.
//!
//! [`events`]: where a session's game events sit in the files that session
//! produced — the conversion from a moment on the recording's timeline to a
//! position in one file, for a session that wrote several of them, one that
//! started after the game did, or none at all
//! ([issue #71](https://github.com/wildware-uk/clipped/issues/71)).
//! The `game_events` table exists now (migration `0003`) and
//! [`index::ingest`] fills it from the sidecar, so a game event a session heard
//! survives the process that heard it. **What is still missing is the
//! placement**: every row's `recording_id` is null, because deciding which file
//! covers a moment needs each recording's span on the session's *media*
//! timeline and the sidecar records only a wall-clock start and a duration.
//! So this module still places events it is *handed* rather than events it
//! reads back. `docs/highlights.md` argues both the table and the model.
//!
//! [`thumbnail`]: the picture every screen that lists a recording shows for it
//! (SPEC.md section 22, `docs/thumbnails.md`). It decodes a frame through
//! FFmpeg, keeps the result in a documented sidecar cache rather than in the
//! database, and runs on a background thread that a recording suspends. It is
//! the one part of this crate that opens a media file.
//!
//! [`trash`]: deleting a recording so that it can be undeleted, with the
//! retention SPEC.md section 28 configures and a restore that returns the file
//! byte for byte (`docs/storage-management.md`,
//! [issue #94](https://github.com/wildware-uk/clipped/issues/94)). It is what
//! makes [issue #111](https://github.com/wildware-uk/clipped/issues/111)'s
//! automatic cleanup defensible.
//!
//! Nothing else in this crate is built yet.

pub mod accounting;
pub mod events;
pub mod favourites;
pub mod index;
pub mod search;
pub mod thumbnail;
pub mod trash;
pub mod virtual_clip;
