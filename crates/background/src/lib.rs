//! The one background worker `clipped-waveform` and the thumbnail module of
//! `clipped-library` both need, and until
//! [issue #293](https://github.com/wildware-uk/clipped/issues/293) each
//! implemented for itself.
//!
//! Both crates read a *finished* recording off disk in the background,
//! summarise it into something small, and cache the result — one into audio
//! peaks, the other into a JPEG. That work is what AGENTS.md section 18 calls
//! the archetypal background job: it competes with a game for the processor
//! and, because a recording in progress is writing to the same disk, for I/O
//! bandwidth too. Getting out of the way turned out to be the same four
//! things regardless of what is being generated:
//!
//! - [`Worker`], a single named thread reading from a bounded queue that
//!   drops the **oldest** waiting request when it is full, because the newest
//!   is the recording somebody just looked at.
//! - [`Worker::suspend_for_recording`] and [`Worker::resume`], a real blocking
//!   stop rather than a hint, checked between packets by whatever is doing the
//!   reading.
//! - [`Pace`], the checkpoint contract that makes that stop possible without
//!   the worker knowing anything about waveforms or thumbnails.
//! - The Windows calls that lower a thread's scheduling and I/O priority
//!   (`src/windows/priority.rs`), read back rather than assumed
//!   (AGENTS.md section 27).
//!
//! What is *not* here is anything about what the worker is generating: no
//! FFmpeg, no JPEG, no `.cwf` or `.json` sidecar. [`Worker::start`] takes a
//! `process` closure that does all of that, exactly as it did before this
//! crate existed — reading a [`SourceIdentity`], calling the analyser or the
//! renderer, writing the cache, deciding what to log. Two caches that stay
//! genuinely different (waveforms are a binary sidecar, thumbnails are a JPEG
//! plus JSON, `docs/thumbnails.md`) is the reason the two services remain two
//! types; the queue, the thread and the suspension are the reason this crate
//! exists (AGENTS.md section 55: not two implementations of one thing).
//!
//! # Why this is layer 0, and not `clipped-logging`'s neighbour
//!
//! [`SourceIdentity::cache_key`] needs a stable digest of a path, which is
//! also what `clipped_logging::RedactedPath` computes for a log line. Taking
//! that dependency would have been the tidier choice, but `clipped-logging`
//! is itself layer 0 (README.md, "Dependency direction"), and a layer-0 crate
//! may depend on nothing else in this workspace — otherwise it would not be
//! the lowest layer either sits at. So `fnv1a_64` in `src/source.rs` is a
//! second, deliberate copy of eleven lines of arithmetic, the same reason
//! `clipped-logging` itself gives for not reaching for a hashing crate. What
//! this crate does *not* duplicate is the privacy-sensitive half:
//! `SourceIdentity` carries no `RedactedPath` and formats nothing for a log
//! itself, so `clipped-waveform` and the thumbnail module still call
//! `clipped_logging::RedactedPath::new` themselves, exactly as they did
//! before — this crate's digest and their redaction happen to use the same
//! algorithm without either depending on the other.
//!
//! # Position in the architecture
//!
//! Layer 0: it depends on nothing else in this workspace, which is what lets
//! `clipped-waveform` and `clipped-library` — both layer 1 — depend on it
//! without inverting the direction `tests/integration/tests/workspace_layering.rs`
//! asserts.

mod pace;
mod source;
mod worker;

#[cfg(windows)]
mod windows;

pub use pace::{Continue, Pace, Unpaced};
pub use source::{SourceIdentity, UNKNOWN_MODIFIED};
pub use worker::{Completion, Outcome, RequestOutcome, Worker, WorkerPriority};
