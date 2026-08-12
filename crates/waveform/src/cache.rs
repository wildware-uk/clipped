//! Where computed waveforms are kept, and the rules for throwing them away.
//!
//! # A cache, not state
//!
//! Every byte here can be recomputed from the recording it came from, so
//! nothing in this module is careful with it. An entry that cannot be read is
//! deleted and regenerated; an entry whose recording has changed is overwritten;
//! the whole directory can be deleted while Clipped is running and the only
//! consequence is that some waveforms are drawn a few seconds later. That is a
//! deliberate contrast with everything under AGENTS.md section 56 — recordings,
//! bookmarks and the database — and it is why this is a directory of files
//! rather than rows in the database (see [`crate::format`] for that argument in
//! full).
//!
//! # Invalidation
//!
//! An entry names the recording it was computed from: path, length and
//! modification time ([`SourceIdentity`]). A lookup compares that against the
//! file on disk now, so a recording that was trimmed, re-encoded or replaced
//! does not show the previous waveform. There is no separate invalidation step
//! to forget to run.
//!
//! # What an entry holds
//!
//! Either the peaks, or the reason there are none. The second kind matters as
//! much as the first: without it a recording that cannot be decoded misses on
//! every lookup, and every miss asks for the whole file to be read again — so a
//! recording truncated by a crash would be re-demuxed on every timeline redraw
//! for the rest of the library's life. Both kinds carry the same
//! [`SourceIdentity`] header, so both are invalidated, pruned and counted the
//! same way.
//!
//! # Cleanup
//!
//! [`WaveformCache::prune`] does three things, in this order:
//!
//! 1. Deletes entries whose recording no longer exists. A library where the user
//!    deletes clips would otherwise accumulate peaks for files that are gone.
//! 2. Deletes temporaries an interrupted store left behind. Nothing else can:
//!    they are named so that no lookup will ever open one.
//! 3. Deletes the least recently written entries until the directory is inside
//!    its byte budget.
//!
//! "Least recently written" rather than least recently *used*: recording a use
//! means writing to the entry every time a waveform is drawn, and a timeline
//! that scrolls would do that many times a second. The cost of getting the order
//! slightly wrong is regenerating a waveform, which is the cheapest mistake in
//! this crate.
//!
//! Pruning is not automatic. It is a call the host process makes when it has
//! time — the same place it decides to index the library — because deleting
//! files is not something a lookup should do behind a caller's back.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::format;
use crate::source::SourceIdentity;
use crate::waveform::{Waveform, WaveformState};
use crate::WaveformError;

/// The extension every cache entry has.
pub const ENTRY_EXTENSION: &str = "cwf";

/// The extension a half-written entry carries until the rename that finishes it.
///
/// Deliberately not [`ENTRY_EXTENSION`], so that a lookup can never read one.
const TEMPORARY_EXTENSION: &str = "cwf.writing";

/// What [`Path::extension`] reports for a [`TEMPORARY_EXTENSION`] file.
///
/// `Path::extension` returns only the part after the last dot, so the two have
/// to be spelled separately; a test holds them in agreement.
const TEMPORARY_SUFFIX: &str = "writing";

/// The directory under Clipped's per-user data directory that entries live in.
const DIRECTORY_NAME: &str = "waveforms";

/// How much disk the cache may use before pruning starts deleting.
///
/// 512 MB, which at the storage cost in [`crate::peaks`] — under 24 kB per
/// minute per track — is roughly 120 hours of three-track recording. A library
/// larger than that keeps the waveforms for the recordings whose peaks were
/// computed most recently, which are the ones being looked at.
pub const DEFAULT_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// How much of an entry is read when only its identity is wanted.
///
/// The identity sits in the first 34 bytes plus the recording's path, so this is
/// far more than enough for any real path and far less than the megabytes of
/// peaks behind it.
const HEADER_READ_BYTES: usize = 8 * 1024;

/// A directory of computed waveforms.
#[derive(Debug, Clone)]
pub struct WaveformCache {
    root: PathBuf,
    budget: u64,
}

impl WaveformCache {
    /// The cache in Clipped's per-user data directory.
    ///
    /// [`None`] when the environment describes no per-user directory at all,
    /// which on Windows means `%LOCALAPPDATA%` is unset. That is not an error:
    /// a caller without a cache computes waveforms and keeps them in memory,
    /// exactly as it would on the first run.
    #[must_use]
    pub fn in_default_directory() -> Option<Self> {
        clipped_logging::application_directory()
            .map(|directory| Self::at(directory.join(DIRECTORY_NAME)))
    }

    /// A cache in a directory of the caller's choosing.
    ///
    /// Nothing is created until something is stored.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            budget: DEFAULT_BUDGET_BYTES,
        }
    }

    /// The same cache with a different byte budget for [`prune`](Self::prune).
    #[must_use]
    pub fn with_budget(mut self, bytes: u64) -> Self {
        self.budget = bytes;
        self
    }

    /// Where entries are written.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The budget [`prune`](Self::prune) works to.
    #[must_use]
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// What is known about the waveform of the recording at `path`.
    ///
    /// Never an error. A recording that has no entry, or whose entry belongs to
    /// an older version of the file, is [`WaveformState::Pending`] — something
    /// to generate, not something to report.
    ///
    /// [`WaveformState::Unavailable`] has two sources: a recording that cannot
    /// be stat-ed at all, and a recording an earlier analysis already read from
    /// end to end and got nothing from
    /// ([`remember_failure`](Self::remember_failure)).
    #[must_use]
    pub fn lookup(&self, path: impl AsRef<Path>) -> WaveformState {
        let path = path.as_ref();
        let current = match SourceIdentity::of(path) {
            Ok(identity) => identity,
            Err(cause) => {
                return WaveformState::Unavailable(WaveformError::Unreadable {
                    path: clipped_logging::RedactedPath::new(path),
                    cause,
                })
            }
        };

        let entry = self.entry_path(&current);
        let bytes = match fs::read(&entry) {
            Ok(bytes) => bytes,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
                return WaveformState::Pending
            }
            Err(cause) => {
                warn!(
                    recording = %current.redacted(),
                    error = %cause,
                    "a waveform cache entry could not be read; it will be generated again"
                );
                return WaveformState::Pending;
            }
        };

        match format::decode(&bytes) {
            Ok(format::Entry::Ready(waveform)) if waveform.source().still_describes(&current) => {
                WaveformState::Ready(waveform)
            }
            Ok(format::Entry::Failed { source, reason }) if source.still_describes(&current) => {
                // An earlier analysis read this whole file and got nothing.
                // Reporting that is the difference between a timeline redraw
                // costing a lookup and costing a re-demux of a multi-gigabyte
                // recording, every time, for ever.
                WaveformState::Unavailable(WaveformError::Remembered {
                    path: current.redacted(),
                    reason,
                })
            }
            Ok(_) => {
                // The recording changed, or two paths' digests collided. Either
                // way the entry describes something else and is about to be
                // overwritten by the regenerated one.
                debug!(
                    recording = %current.redacted(),
                    "the cached waveform belongs to an older version of this recording"
                );
                WaveformState::Pending
            }
            Err(corrupt) => {
                warn!(
                    recording = %current.redacted(),
                    reason = %corrupt,
                    "a waveform cache entry was unreadable and has been discarded"
                );
                // Removing it now rather than leaving it to `prune`: it will
                // never be readable, and every lookup would log this again.
                if let Err(cause) = fs::remove_file(&entry) {
                    debug!(error = %cause, "the unreadable entry could not be removed");
                }
                WaveformState::Pending
            }
        }
    }

    /// Writes a computed waveform.
    ///
    /// Written to a temporary file in the same directory and renamed over the
    /// destination, so a process that dies mid-write leaves either the previous
    /// entry or none — never a half-written one that the next lookup has to
    /// detect.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created or the entry cannot be written. A
    /// caller may log and carry on: the waveform in hand is still usable, and
    /// the only cost is computing it again next time.
    pub fn store(&self, waveform: &Waveform) -> Result<(), WaveformError> {
        self.write_entry(waveform.source(), format::encode(waveform))
    }

    /// Writes down that a recording could not be summarised, and why.
    ///
    /// Without this a recording that cannot be decoded is [`Pending`] for ever:
    /// every lookup misses, every miss asks for another analysis, and every
    /// analysis re-reads and re-demuxes the whole file. The failures AGENTS.md
    /// section 16 says to expect — a recording truncated by a crash, an audio
    /// codec this build has no decoder for, audio past the analyser's length
    /// bound — are exactly the ones that repeat, so remembering the answer is
    /// what keeps a timeline redraw from being a multi-gigabyte read.
    ///
    /// The entry names the identity the failure belongs to, so it stops
    /// applying the moment the recording changes: a file repaired, replaced or
    /// re-encoded is analysed again with no separate invalidation step.
    ///
    /// [`Pending`]: WaveformState::Pending
    ///
    /// # Errors
    ///
    /// As [`store`](Self::store), and for the same reason a caller may ignore
    /// it: the cost of not writing this down is a repeated analysis.
    pub fn remember_failure(
        &self,
        source: &SourceIdentity,
        reason: &WaveformError,
    ) -> Result<(), WaveformError> {
        self.write_entry(source, format::encode_failure(source, &reason.to_string()))
    }

    /// Writes `bytes` as the entry for `source`.
    ///
    /// Written to a temporary file in the same directory and renamed over the
    /// destination, so a process that dies mid-write leaves either the previous
    /// entry or none.
    fn write_entry(&self, source: &SourceIdentity, bytes: Vec<u8>) -> Result<(), WaveformError> {
        fs::create_dir_all(&self.root).map_err(|cause| WaveformError::Cache {
            detail: "create its directory",
            entry: clipped_logging::RedactedPath::new(&self.root),
            cause,
        })?;

        let entry = self.entry_path(source);
        let temporary = entry.with_extension(TEMPORARY_EXTENSION);
        fs::write(&temporary, bytes).map_err(|cause| WaveformError::Cache {
            detail: "write an entry",
            entry: clipped_logging::RedactedPath::new(&temporary),
            cause,
        })?;
        fs::rename(&temporary, &entry).map_err(|cause| {
            // The rename is what makes the write atomic, so a failure here
            // leaves the temporary file behind. Remove it rather than leaving
            // pruning to find it, since it matches no recording.
            let _ = fs::remove_file(&temporary);
            WaveformError::Cache {
                detail: "replace an entry",
                entry: clipped_logging::RedactedPath::new(&entry),
                cause,
            }
        })
    }

    /// Deletes the entry for a recording, if there is one.
    ///
    /// For a recording the user deleted. Pruning finds these too; this is the
    /// immediate answer for a caller that already knows.
    ///
    /// # Errors
    ///
    /// When an entry exists and cannot be deleted. A missing entry is success.
    pub fn forget(&self, path: impl AsRef<Path>) -> Result<(), WaveformError> {
        let path = path.as_ref();
        // The key is a digest of the path alone, so an entry can be found for a
        // recording that no longer exists — which is the whole point here.
        let key = SourceIdentity::from_parts(path.to_path_buf(), 0, 0);
        let entry = self.entry_path(&key);
        match fs::remove_file(&entry) {
            Ok(()) => Ok(()),
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(cause) => Err(WaveformError::Cache {
                detail: "remove an entry",
                entry: clipped_logging::RedactedPath::new(&entry),
                cause,
            }),
        }
    }

    /// Removes entries for recordings that are gone, then the oldest entries
    /// until the directory is inside its budget.
    ///
    /// Reports what it did rather than returning an error: a cache directory
    /// that cannot be read is a cache that holds nothing, and there is no caller
    /// for whom that is a failure. Anything unexpected is logged.
    pub fn prune(&self) -> PruneReport {
        let mut report = PruneReport::default();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return report;
        };

        let mut surviving = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };

            let suffix = path.extension().and_then(|extension| extension.to_str());
            if suffix == Some(ENTRY_EXTENSION) {
                if self
                    .recording_of(&path)
                    .is_some_and(|source| source.exists())
                {
                    surviving.push((path, metadata.len(), metadata.modified().ok()));
                } else if remove(&path, "the recording is gone") {
                    // Either the recording is gone or the entry could not be
                    // read at all. Both are dead weight.
                    report.entries_removed += 1;
                    report.orphans_removed += 1;
                    report.bytes_removed += metadata.len();
                }
            } else if suffix == Some(TEMPORARY_SUFFIX) {
                // A store killed between its write and its rename. Nothing else
                // ever deletes one of these: a lookup only opens `<key>.cwf`, so
                // an abandoned temporary is invisible to every other path in
                // this module and would sit in the directory for ever without
                // ever counting towards the budget that is supposed to bound it.
                //
                // A temporary another process is writing *right now* is not at
                // risk on Windows, where a file open without
                // `FILE_SHARE_DELETE` cannot be removed at all. Where it can be,
                // the worst outcome is that the other store's rename fails and
                // its waveform is computed again, which is the cheapest failure
                // in this crate.
                if remove(&path, "a store was interrupted before it finished") {
                    report.entries_removed += 1;
                    report.temporaries_removed += 1;
                    report.bytes_removed += metadata.len();
                }
            }
        }

        let mut total: u64 = surviving.iter().map(|(_, size, _)| size).sum();
        if total > self.budget {
            // Oldest first, so the newest peaks — the recordings somebody is
            // looking at — are the ones that survive.
            surviving.sort_by_key(|entry| entry.2);
            for (path, size, _) in surviving {
                if total <= self.budget {
                    break;
                }
                if remove(&path, "the cache is over its budget") {
                    total = total.saturating_sub(size);
                    report.entries_removed += 1;
                    report.bytes_removed += size;
                }
            }
        }

        report.remaining_bytes = total;
        report
    }

    /// The file an identity's entry is written to.
    fn entry_path(&self, identity: &SourceIdentity) -> PathBuf {
        self.root
            .join(identity.cache_key())
            .with_extension(ENTRY_EXTENSION)
    }

    /// Which recording an entry belongs to, without reading its peaks.
    fn recording_of(&self, entry: &Path) -> Option<PathBuf> {
        let mut file = fs::File::open(entry).ok()?;
        let mut header = vec![0u8; HEADER_READ_BYTES];
        let read = read_up_to(&mut file, &mut header);
        header.truncate(read);
        format::decode_identity(&header)
            .ok()
            .map(|identity| identity.path().to_path_buf())
    }
}

/// Fills as much of `buffer` as the file has, returning how many bytes that was.
///
/// `Read::read` is allowed to return fewer bytes than asked for without being at
/// the end, and a short read here would look like a truncated header.
fn read_up_to(file: &mut fs::File, buffer: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(cause) if cause.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    filled
}

/// What one call to [`WaveformCache::prune`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    entries_removed: usize,
    orphans_removed: usize,
    temporaries_removed: usize,
    bytes_removed: u64,
    remaining_bytes: u64,
}

/// Deletes one entry, saying why in the log, and reports whether it went.
///
/// A file that will not delete — held open by a virus scanner, on a read-only
/// volume — is logged and left. Pruning is housekeeping; there is nothing here
/// worth failing a caller over.
fn remove(path: &Path, why: &str) -> bool {
    // Redacted, never `path.display()`: an entry lives under `%LOCALAPPDATA%`,
    // which begins with the account name, and a debug log is a file somebody
    // attaches to a bug report (AGENTS.md section 13, docs/logging.md
    // "Privacy"). The entry's own name is a digest of the recording's path, so
    // the reduced form loses nothing that identifies which entry this was.
    match fs::remove_file(path) {
        Ok(()) => {
            debug!(
                entry = %clipped_logging::RedactedPath::new(path),
                reason = why,
                "removed a cached waveform"
            );
            true
        }
        Err(cause) => {
            warn!(
                entry = %clipped_logging::RedactedPath::new(path),
                error = %cause,
                "a waveform cache entry could not be removed"
            );
            false
        }
    }
}

impl PruneReport {
    /// How many files were deleted, for any of the three reasons.
    #[must_use]
    pub fn entries_removed(&self) -> usize {
        self.entries_removed
    }

    /// How many of those were entries for recordings that no longer exist.
    #[must_use]
    pub fn orphans_removed(&self) -> usize {
        self.orphans_removed
    }

    /// How many of those were temporaries an interrupted store left behind.
    ///
    /// Normally zero. A number that keeps growing means stores are being killed
    /// part way through, which is worth knowing about.
    #[must_use]
    pub fn temporaries_removed(&self) -> usize {
        self.temporaries_removed
    }

    /// How many bytes the deletions freed.
    #[must_use]
    pub fn bytes_removed(&self) -> u64 {
        self.bytes_removed
    }

    /// How many bytes of entries are left.
    #[must_use]
    pub fn remaining_bytes(&self) -> u64 {
        self.remaining_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peaks::{Peak, BASE_BUCKET};
    use crate::waveform::{TrackDescriptor, TrackWaveform};
    use clipped_media_validation::TemporaryDirectory;
    use core::time::Duration;

    /// A recording file with `size` bytes in it, and a waveform for it.
    fn recording(directory: &TemporaryDirectory, name: &str, size: usize) -> PathBuf {
        let path = directory.file(name);
        fs::write(&path, vec![0u8; size]).expect("the recording can be written");
        path
    }

    fn waveform_for(path: &Path, buckets: usize) -> Waveform {
        let identity = SourceIdentity::of(path).expect("the recording exists");
        let track = TrackWaveform::from_base(
            TrackDescriptor::new(1, 48_000, 2, "Game"),
            Duration::from_millis(10 * buckets as u64),
            BASE_BUCKET,
            vec![Peak::new(-100, 100); buckets],
        );
        Waveform::new(identity, vec![track])
    }

    #[test]
    fn a_recording_with_nothing_cached_is_pending_rather_than_an_error() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "match.mkv", 16);
        assert!(matches!(cache.lookup(&path), WaveformState::Pending));
    }

    #[test]
    fn a_recording_that_is_not_there_is_unavailable_with_a_reason() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let state = cache.lookup(directory.file("gone.mkv"));
        assert!(!state.is_ready());
        assert!(state.tracks().is_empty());
        assert!(matches!(
            state.reason(),
            Some(WaveformError::Unreadable { .. })
        ));
    }

    #[test]
    fn what_was_stored_is_what_comes_back() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "match.mkv", 16);

        cache
            .store(&waveform_for(&path, 200))
            .expect("the entry can be written");

        let state = cache.lookup(&path);
        let waveform = state.waveform().expect("the entry is ready");
        assert_eq!(waveform.tracks().len(), 1);
        assert_eq!(
            waveform.tracks()[0].descriptor().name(),
            Some("Game"),
            "the track's label survived the round trip"
        );
        assert_eq!(waveform.source().path(), path);
    }

    #[test]
    fn a_recording_that_changed_invalidates_its_own_entry() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "match.mkv", 16);
        cache
            .store(&waveform_for(&path, 10))
            .expect("the entry can be written");
        assert!(cache.lookup(&path).is_ready());

        // Rewritten at a different length: the same path, a different
        // recording.
        fs::write(&path, vec![1u8; 999]).expect("the recording can be rewritten");
        assert!(matches!(cache.lookup(&path), WaveformState::Pending));

        // And storing again makes it ready, at the new identity.
        cache
            .store(&waveform_for(&path, 10))
            .expect("the entry can be rewritten");
        assert_eq!(
            cache
                .lookup(&path)
                .waveform()
                .expect("ready again")
                .source()
                .size(),
            999
        );
    }

    #[test]
    fn an_entry_that_is_not_readable_is_discarded_rather_than_reported() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "match.mkv", 16);
        cache
            .store(&waveform_for(&path, 10))
            .expect("the entry can be written");

        // A half-written entry, of the kind a power cut leaves.
        let entry = cache.entry_path(&SourceIdentity::of(&path).expect("it exists"));
        let bytes = fs::read(&entry).expect("the entry can be read");
        fs::write(&entry, &bytes[..bytes.len() / 2]).expect("the entry can be truncated");

        assert!(matches!(cache.lookup(&path), WaveformState::Pending));
        assert!(!entry.exists(), "the unreadable entry was removed");
    }

    #[test]
    fn a_finished_store_leaves_only_the_entry_it_wrote() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "match.mkv", 16);
        cache
            .store(&waveform_for(&path, 10))
            .expect("the entry can be written");

        let remaining: Vec<_> = fs::read_dir(cache.root())
            .expect("the cache directory exists")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining.len(), 1, "{remaining:?}");
        assert!(remaining[0].ends_with(".cwf"), "{remaining:?}");
    }

    #[test]
    fn the_temporary_a_killed_store_leaves_behind_is_swept_by_pruning() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "match.mkv", 16);
        cache
            .store(&waveform_for(&path, 10))
            .expect("the entry can be written");

        // Exactly what a process killed between `store`'s `fs::write` and its
        // `fs::rename` leaves behind: the temporary, written, never renamed.
        // Written by hand because the alternative is killing a process.
        let identity = SourceIdentity::of(&path).expect("the recording exists");
        let abandoned = cache
            .entry_path(&identity)
            .with_extension(TEMPORARY_EXTENSION);
        fs::write(&abandoned, vec![0u8; 4_096]).expect("the temporary can be written");

        // Nothing else in this module can ever remove it: a lookup only opens
        // `<key>.cwf`, so it is invisible to every other path here and would sit
        // in the directory for ever, uncounted against the budget that is
        // supposed to bound the directory.
        assert!(cache.lookup(&path).is_ready(), "the finished entry reads");

        let report = cache.prune();
        assert_eq!(
            report.temporaries_removed(),
            1,
            "the abandoned temporary was not swept"
        );
        assert!(report.bytes_removed() >= 4_096);
        assert!(!abandoned.exists(), "{}", abandoned.display());
        assert!(
            cache.lookup(&path).is_ready(),
            "pruning took the finished entry as well"
        );
    }

    #[test]
    fn a_temporary_is_named_so_that_no_lookup_can_read_it() {
        // `prune` matches on `Path::extension`, which is only the part after
        // the last dot, so the two spellings have to agree or the sweep above
        // silently stops finding anything.
        let temporary = Path::new("entry").with_extension(TEMPORARY_EXTENSION);
        assert_eq!(
            temporary.extension().and_then(|suffix| suffix.to_str()),
            Some(TEMPORARY_SUFFIX)
        );
        assert_ne!(TEMPORARY_SUFFIX, ENTRY_EXTENSION);
    }

    #[test]
    fn a_remembered_failure_is_reported_rather_than_left_to_be_analysed_again() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "truncated.mkv", 16);
        let identity = SourceIdentity::of(&path).expect("the recording exists");

        cache
            .remember_failure(
                &identity,
                &WaveformError::TooLong {
                    limit: Duration::from_secs(8 * 3_600),
                },
            )
            .expect("the entry can be written");

        let state = cache.lookup(&path);
        // Not `Pending`. `Pending` is what makes the service read the whole file
        // again, and again, and again.
        assert!(
            !matches!(state, WaveformState::Pending),
            "a remembered failure came back as something to generate"
        );
        assert!(!state.is_ready());
        assert!(state.tracks().is_empty());
        match state.reason() {
            Some(WaveformError::Remembered { reason, .. }) => {
                assert!(reason.contains("8 hours"), "{reason}");
            }
            other => panic!("expected a remembered failure, found {other:?}"),
        }
    }

    #[test]
    fn a_remembered_failure_stops_applying_once_the_recording_changes() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "truncated.mkv", 16);
        let identity = SourceIdentity::of(&path).expect("the recording exists");
        cache
            .remember_failure(&identity, &WaveformError::Cancelled)
            .expect("the entry can be written");
        assert!(cache.lookup(&path).reason().is_some());

        // A recording repaired, replaced with a good copy, or re-encoded. The
        // remembered failure was about the old file and must not outlive it.
        fs::write(&path, vec![1u8; 999]).expect("the recording can be replaced");
        assert!(
            matches!(cache.lookup(&path), WaveformState::Pending),
            "a replaced recording is still remembered as a failure"
        );

        // And peaks may then be stored over the failure entry, at the same key.
        cache
            .store(&waveform_for(&path, 10))
            .expect("the entry can be overwritten");
        assert!(cache.lookup(&path).is_ready());
    }

    #[test]
    fn a_cache_error_names_what_failed_without_naming_the_directories_above_it() {
        let directory = TemporaryDirectory::new("waveform-cache");
        // A file where the cache's parent directory would be, so that the
        // directory cannot be created. The name stands in for the account name
        // in `C:\Users\<account>\AppData\Local\Clipped\waveforms`, which is
        // what a raw path in this message puts into a support log (AGENTS.md
        // section 13).
        let obstruction = directory.file("pretend-account-name");
        fs::write(&obstruction, b"not a directory").expect("the file can be written");

        let cache = WaveformCache::at(obstruction.join("waveforms"));
        let path = recording(&directory, "match.mkv", 16);
        let error = cache
            .store(&waveform_for(&path, 10))
            .expect_err("no directory can be created underneath a file");

        assert!(matches!(error, WaveformError::Cache { .. }), "{error:?}");
        let message = error.to_string();
        assert!(
            message.contains("waveforms#"),
            "the message does not say which directory failed: {message}"
        );
        assert!(
            !message.contains("pretend-account-name"),
            "the message leaked a directory component: {message}"
        );
    }

    #[test]
    fn forgetting_a_recording_removes_its_entry_and_forgives_a_second_call() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "match.mkv", 16);
        cache
            .store(&waveform_for(&path, 10))
            .expect("the entry can be written");

        cache.forget(&path).expect("the entry can be removed");
        assert!(matches!(cache.lookup(&path), WaveformState::Pending));
        cache
            .forget(&path)
            .expect("removing nothing is not an error");
    }

    #[test]
    fn pruning_removes_entries_for_recordings_that_are_gone() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let kept = recording(&directory, "kept.mkv", 16);
        let deleted = recording(&directory, "deleted.mkv", 16);
        cache.store(&waveform_for(&kept, 10)).expect("stored");
        cache.store(&waveform_for(&deleted, 10)).expect("stored");

        fs::remove_file(&deleted).expect("the recording can be deleted");
        let report = cache.prune();

        assert_eq!(report.orphans_removed(), 1);
        assert_eq!(report.entries_removed(), 1);
        assert!(report.bytes_removed() > 0);
        assert!(cache.lookup(&kept).is_ready(), "the survivor is untouched");
    }

    #[test]
    fn pruning_deletes_the_oldest_entries_until_the_budget_is_met() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let first = recording(&directory, "first.mkv", 16);
        let second = recording(&directory, "second.mkv", 16);

        // Two entries of about 2 kB each. A budget below one of them leaves
        // exactly one, and it has to be the newer.
        let cache = WaveformCache::at(directory.file("cache"));
        cache.store(&waveform_for(&first, 1_000)).expect("stored");
        // The modification times have to differ for "oldest" to mean anything.
        // A file system's timestamp resolution can be coarse, so this is set
        // rather than waited for.
        age(&cache.entry_path(&SourceIdentity::of(&first).expect("exists")));
        cache.store(&waveform_for(&second, 1_000)).expect("stored");

        let budget = fs::metadata(cache.entry_path(&SourceIdentity::of(&second).expect("exists")))
            .expect("the entry exists")
            .len();
        let bounded = cache.clone().with_budget(budget);
        let report = bounded.prune();

        assert_eq!(report.entries_removed(), 1);
        assert_eq!(report.orphans_removed(), 0);
        assert!(report.remaining_bytes() <= budget);
        assert!(
            bounded.lookup(&second).is_ready(),
            "the newest entry survived"
        );
        assert!(matches!(bounded.lookup(&first), WaveformState::Pending));
    }

    #[test]
    fn pruning_an_empty_or_missing_cache_does_nothing_rather_than_failing() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("never-created"));
        assert_eq!(cache.prune(), PruneReport::default());
    }

    #[test]
    fn a_file_that_is_not_an_entry_is_left_alone() {
        let directory = TemporaryDirectory::new("waveform-cache");
        let cache = WaveformCache::at(directory.file("cache"));
        let path = recording(&directory, "match.mkv", 16);
        cache.store(&waveform_for(&path, 10)).expect("stored");

        let stranger = cache.root().join("notes.txt");
        fs::write(&stranger, b"not ours").expect("the file can be written");
        cache.prune();
        assert!(stranger.exists(), "pruning deleted somebody else's file");
    }

    /// Backdates a file so that "oldest" is decidable without waiting.
    fn age(path: &Path) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("the entry can be opened");
        file.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .expect("the entry's time can be set");
    }
}
