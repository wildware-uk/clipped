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
//! # What is built
//!
//! [`index`] — the reconciliation between the session sidecars the recorder
//! writes, the media files beside them, and the SQLite index. It is the only
//! thing that writes library rows, and [docs/library.md] is its prose.
//!
//! [docs/library.md]: https://github.com/wildware-uk/clipped/blob/main/docs/library.md

pub mod index;
