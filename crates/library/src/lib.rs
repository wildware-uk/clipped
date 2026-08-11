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

pub mod accounting;
