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
//!
//! # Not responsible for
//!
//! Storage primitives (see `clipped-storage`) or generating media.
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
//! [issue #55](https://github.com/wildware-uk/clipped/issues/55). Nothing else
//! in this crate is built yet.

pub mod search;
