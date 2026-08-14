//! What the library occupies on disk, and the limits configured against it.
//!
//! Storage is a product feature rather than an implementation detail (SPEC.md
//! section 27): a user configures a maximum size, a minimum amount of free disk
//! space and a maximum recording age, and expects the figures they are shown to
//! be the truth about their own disk. This module measures that truth, reports
//! it, and — in [`cleanup`] alone — acts on it. Everything else here is
//! measurement and **deletes nothing**. The screen that shows all of it is
//! [issue #95](https://github.com/wildware-uk/clipped/issues/95).
//!
//! # Responsibilities
//!
//! - Naming what counts, so that a quota cannot silently omit half of what is on
//!   disk: [`StorageCategory`], [`StorageRoots`].
//! - Measuring it, bounded and interruptible: [`scan`], [`ScanOptions`],
//!   [`ScanReport`], [`StorageInventory`].
//! - Attributing it to a game, a session and an item, by reconciling the
//!   measurement against what the database believes: [`Reconciliation`],
//!   [`IndexedItem`].
//! - Holding the configured limits and refusing ones that cannot be satisfied:
//!   [`StorageLimits`], [`VolumeCapacity`].
//! - Saying whether a limit is breached *now*, which is what a recording about
//!   to start asks: [`StorageStatus`], [`Breach`].
//!
//! # Not responsible for
//!
//! Deleting, trashing or moving a file; deciding which file a cleanup would take
//! first; the retention rules that protect favourites and locked recordings; the
//! on-disk layout itself (that is `clipped-storage`); and persisting the
//! settings (see "Where the settings will live" below).
//!
//! # Why this lives in `clipped-library`
//!
//! Two crates could plausibly own it. `clipped-storage` owns the persistence
//! mechanism — the SQLite schema and the on-disk layout
//! ([issue #55](https://github.com/wildware-uk/clipped/issues/55)) — and
//! `clipped-library` owns the *view over what is actually on disk*, which its
//! crate documentation has claimed from the start. Accounting is that view: it
//! walks the filesystem, reconciles the result against the index, and answers
//! questions about games and sessions, which are library concepts and not
//! storage primitives. Putting it here also keeps `clipped-storage` free of
//! policy, which is the split `docs/architecture.md` now records against the
//! storage manager: the mechanism there, the measurement and the limits here.
//!
//! It is deliberately *not* a new crate. It depends on nothing but the standard
//! library and one Windows call, it is used by the same consumers as the rest of
//! `clipped-library`, and a crate whose whole content is one module is a layer
//! in the dependency graph that buys nothing (AGENTS.md section 4).
//!
//! Nothing here reads the database or `clipped-storage`. The index is supplied
//! as data — [`IndexedItem`] values — so accounting stays a pure function of
//! "what is on disk" and "what the index says", and the crate that owns the
//! schema is free to change underneath it.
//!
//! # The filesystem is the authority for bytes; the index is the authority for
//! meaning
//!
//! The database and the disk will disagree. Users move recordings, delete them
//! from Explorer, restore them from a backup, and copy files into the recording
//! folder that Clipped never wrote. [`Reconciliation`] resolves that with one
//! rule, applied consistently:
//!
//! | Situation | What accounting reports |
//! | --- | --- |
//! | Indexed and on disk, sizes agree | Counted, attributed to its game and session |
//! | Indexed and on disk, sizes differ | Counted at the size **on disk**; the disagreement is listed |
//! | Indexed, not on disk | Not counted at all; listed as missing, for the index to heal |
//! | On disk, not indexed | Counted, attributed to nothing |
//!
//! A quota that trusted the database would keep deleting recordings to get under
//! a limit that a stale row had invented, and one that ignored untracked files
//! would report 40 GB while the disk filled — both are worse than no quota at
//! all. Healing the index is the indexer's job, not this module's; accounting
//! only reports what it found.
//!
//! # Threading and where it runs
//!
//! Nothing here spawns a thread, blocks on a lock, or holds one. [`scan`] is an
//! ordinary synchronous walk, and the caller decides which thread pays for it.
//! Two rules follow from AGENTS.md section 20 and are the reason the API looks
//! the way it does:
//!
//! - **Never on a capture path.** A recording thread must not wait on the
//!   filesystem. The recorder checks limits *before* a recording starts, from
//!   the last completed inventory.
//! - **Never on the thread drawing the interface.** The desktop application runs
//!   a scan on a background thread and shows the previous figures until it
//!   finishes.
//!
//! Two mechanisms keep a scan from running away: [`ScanOptions::with_time_budget`]
//! stops it when it has spent long enough, and [`scan_until`] stops it when the
//! caller says so — the desktop application's "cancel" and its own shutdown both
//! go through that. A scan that stops early is *not* silently truncated: the
//! inventory it produces reports [`Completeness::Partial`], and a limit that
//! cannot be judged from a partial measurement is reported as unknown rather
//! than as satisfied ([`StorageStatus`]).
//!
//! # Cost, and what is incremental
//!
//! A full walk costs one directory enumeration per directory and no read of any
//! file's contents. `docs/storage-management.md` has measured figures for a
//! synthetic library — `cargo run -p clipped-library --example scan_cost` is the
//! harness that produced them. As a shape: the cost is linear in the number of
//! files and dominated by the filesystem, not by this code.
//!
//! A full walk is not the only way to stay current, and is not meant to be the
//! usual one. [`StorageInventory::record_added`] and
//! [`StorageInventory::record_removed`] maintain the figures between walks, so
//! finishing a recording costs an insertion rather than a rescan, and the walk
//! becomes the periodic reconciliation that catches what happened behind the
//! application's back.
//!
//! # What a cleanup will need, which is not built here
//!
//! Issue #111 has to answer "which files would be removed first", and cannot if
//! accounting throws away everything but a total. So the inventory keeps one
//! [`FileEntry`] per file — path, category, size and modification time — and
//! [`StorageInventory::cleanup_candidates_oldest_first`] orders them the way
//! SPEC.md section 27 describes deletion happening.
//!
//! Two exclusions are built into that, because they are about the *file* rather
//! than about policy: the logs, the database and the replay buffer's disk
//! backing are counted towards the total and are never offered as candidates
//! ([`StorageCategory::is_cleanup_candidate`]), and a file whose modification
//! time is unknown sorts last rather than first. That is all: the *protection*
//! rules (favourites, locked recordings, recordings being edited, sources
//! referenced by clips) are facts about the index rather than the disk, and they
//! belong to the ticket that deletes. Nothing here decides that a file may go.
//!
//! # Where the settings will live
//!
//! [`StorageLimits`] is a validated in-memory model, not a settings file. The
//! configuration API with global-to-per-game inheritance is
//! [issue #108](https://github.com/wildware-uk/clipped/issues/108), and this type
//! is shaped to move into it: three independent optional limits, no defaults
//! invented here beyond "unlimited", validation in the constructors, and a second
//! validation pass against the volume the settings are being applied to
//! ([`StorageLimits::validate_for`]). When #108 exists it deserialises into these
//! constructors, which is what keeps a hand-edited settings file from installing
//! a limit that this module would have refused (AGENTS.md section 30).

mod category;
pub mod cleanup;
mod error;
mod inventory;
mod limits;
mod reconcile;
/// Where each kind of file lives, as the caller declares it.
///
/// Visible to the rest of the crate rather than to accounting alone, for
/// [`roots::contains`]: "is this path inside that directory?" is also the guard
/// that stops the trash unlinking anything it did not put there
/// (`crate::trash`), and one comparison that handles Windows' case rules is
/// better than two (AGENTS.md section 55).
pub(crate) mod roots;
mod scan;
mod status;
mod volume;

#[cfg(windows)]
mod windows;

pub use category::StorageCategory;
pub use error::{LimitError, RootsError, VolumeError};
pub use inventory::{Completeness, FileEntry, PartialReason, StorageInventory, UnavailableRoot};
pub use limits::{StorageLimits, MAXIMUM_AGE_FLOOR, MINIMUM_QUOTA};
pub use reconcile::{Attribution, IndexedItem, MatchedFile, Reconciliation};
pub use roots::{StorageRoot, StorageRoots};
pub use scan::{scan, scan_until, ScanOptions, ScanReport, ENTRIES_PER_BUDGET_CHECK};
pub use status::{Breach, LimitKind, StorageStatus, Unknown, UnknownReason};
pub use volume::{capacity_of, VolumeCapacity};
