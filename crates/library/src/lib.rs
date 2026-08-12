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
//! - Thumbnail and waveform bookkeeping.
//! - Storage accounting and the limits configured against it:
//!   [`accounting`].
//!
//! # Not responsible for
//!
//! Storage primitives (see `clipped-storage`) or generating media. Nothing here
//! deletes a file — measuring what is on disk and removing something from it are
//! deliberately different modules in different tickets (AGENTS.md section 56).
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
//! Nothing else in this crate is built yet.

pub mod accounting;
pub mod search;
pub mod virtual_clip;
