//! Where thumbnails are kept, and the rules for throwing them away.
//!
//! # A cache, not state
//!
//! Every byte here can be made again from the recording it came from, so nothing
//! in this module is careful with it. An entry that cannot be read is deleted and
//! regenerated; an entry whose recording has changed is overwritten; the whole
//! directory can be deleted while Clipped is running and the only consequence is
//! that some tiles are drawn without a picture for a few seconds. That is the
//! deliberate opposite of everything under AGENTS.md section 56 — recordings,
//! bookmarks and the database — and it is what
//! [`StorageCategory::is_regenerable`](crate::accounting::StorageCategory::is_regenerable)
//! already says about this category.
//!
//! # Why a directory of files and not the database
//!
//! AGENTS.md section 31 forbids media blobs in SQLite and #55's schema
//! deliberately has no BLOB column, so the picture is a file whatever else
//! happens. That leaves the choice of where the *bookkeeping* goes, and it is
//! here, beside the picture, rather than in a `thumbnail_path` column:
//!
//! - A column would need a migration in `clipped-storage`, whose migrations are
//!   append-only and released, for data that is derived and disposable.
//! - The cache is keyed on the recording's own path, which the index already
//!   holds. Nothing has to be joined, and a library rebuilt from nothing —
//!   which `crate::index` can do — still finds every thumbnail it had.
//! - A user who deletes the database keeps their thumbnails; a user who deletes
//!   the thumbnails keeps their library.
//!
//! `docs/thumbnails.md` writes the sidecar format down, which is what AGENTS.md
//! section 32 asks of a format that is not SQLite.
//!
//! # What an entry is
//!
//! Two files per recording, named after a digest of its path:
//!
//! ```text
//! <key>.jpg     the picture
//! <key>.json    what it is, and which recording it came from
//! ```
//!
//! The picture is an ordinary JPEG that any image viewer opens, which is the
//! point: a cache of a proprietary blob would be a second file format for no
//! gain. The sidecar is what makes it self-describing — without it, a directory
//! of digests could not tell which recording a picture belonged to, and cleaning
//! up after a deleted recording would be impossible.
//!
//! An entry may instead record that a recording produced no thumbnail, and why.
//! That matters as much as the picture: without it a file that cannot be decoded
//! misses on every lookup, and every miss asks for another attempt at the same
//! broken file, for the rest of the library's life.
//!
//! # Invalidation
//!
//! An entry names the recording it was made from: path, length and modification
//! time ([`SourceIdentity`]). A lookup compares that against the file on disk
//! now, so a recording that was trimmed, re-encoded or replaced does not show
//! the previous picture. There is no separate invalidation step to forget to
//! run.
//!
//! # Cleanup
//!
//! [`ThumbnailCache::prune`] deletes, in this order: entries whose recording is
//! gone, temporaries an interrupted store left behind, pictures whose sidecar is
//! missing, and then the least recently written entries until the directory is
//! inside its byte budget. It is a call the host makes when it has time — the
//! same place it decides to index the library — because deleting files is not
//! something a lookup should do behind a caller's back.

use core::time::Duration;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use clipped_background::SourceIdentity;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::render::RenderedThumbnail;
use super::ThumbnailError;

/// The directory under Clipped's per-user data directory that entries live in.
const DIRECTORY_NAME: &str = "thumbnails";

/// The extension every picture has.
pub const IMAGE_EXTENSION: &str = "jpg";

/// The extension every sidecar has.
pub const SIDECAR_EXTENSION: &str = "json";

/// The extension a half-written file carries until the rename that finishes it.
///
/// Deliberately neither of the two above, so that no lookup can ever read one.
const TEMPORARY_SUFFIX: &str = "writing";

/// The version this build writes into a sidecar, and the only one it reads.
///
/// A sidecar from a future build is treated as a miss rather than as an error:
/// the thumbnail is made again, which costs milliseconds, and nothing has to
/// guess at a format it does not know (AGENTS.md section 43).
const SIDECAR_VERSION: u32 = 1;

/// How much disk the cache may use before pruning starts deleting.
///
/// 256 MB, which at the measured 20 kB a picture (`docs/thumbnails.md`) is about
/// thirteen thousand recordings — far more than a library reaches before the
/// recordings themselves fill a disk. A library larger than that keeps the
/// thumbnails made most recently, which are the ones being looked at.
pub const DEFAULT_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// A thumbnail that exists on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    source: SourceIdentity,
    image: PathBuf,
    width: u32,
    height: u32,
    at: Duration,
    blank: bool,
}

impl Thumbnail {
    /// The recording it was made from, as it was when the frame was taken.
    #[must_use]
    pub fn source(&self) -> &SourceIdentity {
        &self.source
    }

    /// The JPEG. This is what a screen draws.
    #[must_use]
    pub fn image_path(&self) -> &Path {
        &self.image
    }

    /// How wide the picture is, in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// How tall the picture is, in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// How far into the recording the frame was taken from.
    #[must_use]
    pub fn at(&self) -> Duration {
        self.at
    }

    /// Whether every frame considered was a flat colour, so the picture is a
    /// black or white rectangle.
    ///
    /// A screen may show it anyway — it is what the recording looks like — or
    /// fall back to whatever it draws for a recording with no thumbnail. Both
    /// are honest; inventing a picture is not (AGENTS.md section 27).
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.blank
    }
}

/// What is known about a recording's thumbnail.
///
/// The three answers a screen has to be able to draw, and none of them is an
/// error it has to handle: a tile is drawn with a picture, without one, or
/// without one and with a reason (issue #57's third acceptance criterion).
#[derive(Debug)]
#[non_exhaustive]
pub enum ThumbnailState {
    /// There is not one yet. Something is being, or is about to be, made.
    Pending,
    /// There is one, here.
    Ready(Thumbnail),
    /// There will not be one, and this is why.
    Unavailable(ThumbnailError),
}

impl ThumbnailState {
    /// Whether there is a picture to draw.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    /// The thumbnail, when there is one.
    #[must_use]
    pub fn thumbnail(&self) -> Option<&Thumbnail> {
        match self {
            Self::Ready(thumbnail) => Some(thumbnail),
            _ => None,
        }
    }

    /// The picture to draw, when there is one.
    #[must_use]
    pub fn image_path(&self) -> Option<&Path> {
        self.thumbnail().map(Thumbnail::image_path)
    }

    /// Why there is no picture, when that has been established.
    ///
    /// [`None`] while it is [`Pending`](Self::Pending): "not yet" is not a
    /// reason.
    #[must_use]
    pub fn reason(&self) -> Option<&ThumbnailError> {
        match self {
            Self::Unavailable(error) => Some(error),
            _ => None,
        }
    }
}

/// A directory of thumbnails.
#[derive(Debug, Clone)]
pub struct ThumbnailCache {
    root: PathBuf,
    budget: u64,
}

impl ThumbnailCache {
    /// The cache in Clipped's per-user data directory.
    ///
    /// [`None`] when the environment describes no per-user directory at all,
    /// which on Windows means `%LOCALAPPDATA%` is unset. That is not an error: a
    /// caller without a cache makes thumbnails and keeps them in memory, exactly
    /// as it would on the first run.
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

    /// What is known about the thumbnail of the recording at `path`.
    ///
    /// Never an error. A recording with no entry, or whose entry belongs to an
    /// older version of the file, is [`ThumbnailState::Pending`] — something to
    /// make, not something to report.
    #[must_use]
    pub fn lookup(&self, path: impl AsRef<Path>) -> ThumbnailState {
        let path = path.as_ref();
        let current = match SourceIdentity::of(path) {
            Ok(identity) => identity,
            Err(cause) => {
                return ThumbnailState::Unavailable(ThumbnailError::Unreadable {
                    path: clipped_logging::RedactedPath::new(path),
                    cause,
                })
            }
        };

        let sidecar = self.sidecar_path(&current.cache_key());
        let entry = match self.read_sidecar(&sidecar) {
            Some(entry) => entry,
            None => return ThumbnailState::Pending,
        };

        if !entry.identity().still_describes(&current) {
            // The recording changed, or two paths' digests collided. Either way
            // the entry describes something else and is about to be replaced.
            debug!(
                recording = %clipped_logging::RedactedPath::new(current.path()),
                "the cached thumbnail belongs to an older version of this recording"
            );
            return ThumbnailState::Pending;
        }

        if let Some(reason) = entry.failure {
            // An earlier attempt already decoded this file and got nothing.
            // Reporting that is the difference between a library screen costing
            // one `stat` a tile and costing a seek and a decode a tile, every
            // time it is opened, for ever.
            return ThumbnailState::Unavailable(ThumbnailError::Remembered {
                path: clipped_logging::RedactedPath::new(current.path()),
                reason,
            });
        }

        let Some(image) = entry.image else {
            warn!(
                recording = %clipped_logging::RedactedPath::new(current.path()),
                "a thumbnail sidecar records neither a picture nor a failure"
            );
            return ThumbnailState::Pending;
        };

        let file = self.image_path(&current.cache_key());
        if !file.is_file() {
            // Somebody deleted the picture and left the sidecar, or a store was
            // interrupted between the two writes.
            debug!(
                recording = %clipped_logging::RedactedPath::new(current.path()),
                "a thumbnail sidecar has no picture beside it; it will be made again"
            );
            return ThumbnailState::Pending;
        }

        ThumbnailState::Ready(Thumbnail {
            source: current,
            image: file,
            width: image.width,
            height: image.height,
            at: Duration::from_secs_f64(image.at_seconds.max(0.0)),
            blank: image.blank,
        })
    }

    /// Writes a thumbnail, and reports where it went.
    ///
    /// Each file is written to a temporary in the same directory and renamed
    /// over its destination, and the picture is finished before the sidecar that
    /// describes it. A process that dies mid-store therefore leaves either the
    /// previous entry, or a picture no lookup will read and pruning will collect
    /// — never a sidecar pointing at a picture that is not there.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created or a file cannot be written. A
    /// caller may log and carry on: the picture in hand is still correct, and
    /// the only cost is making it again next time.
    pub fn store(&self, rendered: &RenderedThumbnail) -> Result<Thumbnail, ThumbnailError> {
        let source = rendered.source().clone();
        let key = source.cache_key();
        self.create_directory()?;

        let image = self.image_path(&key);
        self.write_atomically(&image, rendered.jpeg())?;

        let entry = Entry {
            version: SIDECAR_VERSION,
            recording: source.path_text(),
            size_bytes: source.size(),
            modified_nanos: source.modified_nanos(),
            image: Some(ImageRecord {
                file: format!("{key}.{IMAGE_EXTENSION}"),
                width: rendered.width(),
                height: rendered.height(),
                at_seconds: rendered.at().as_secs_f64(),
                blank: rendered.is_blank(),
            }),
            failure: None,
        };
        self.write_sidecar(&key, &entry)?;

        Ok(Thumbnail {
            source,
            image,
            width: rendered.width(),
            height: rendered.height(),
            at: rendered.at(),
            blank: rendered.is_blank(),
        })
    }

    /// Writes down that a recording produced no thumbnail, and why.
    ///
    /// Without this a recording that cannot be decoded is
    /// [`Pending`](ThumbnailState::Pending) for ever: every lookup misses, every
    /// miss asks for another attempt, and every attempt seeks into and decodes
    /// the same broken file. The failures AGENTS.md section 16 says to expect —
    /// a recording truncated by a crash, a codec this build cannot decode, a
    /// file with no video in it — are exactly the ones that repeat.
    ///
    /// What is remembered belongs to the version of the file that failed, so a
    /// recording repaired or replaced is attempted again with no separate
    /// invalidation step.
    ///
    /// # Errors
    ///
    /// As [`store`](Self::store), and a caller may ignore it for the same
    /// reason: the cost of not writing this down is a repeated attempt.
    pub fn remember_failure(
        &self,
        source: &SourceIdentity,
        reason: &ThumbnailError,
    ) -> Result<(), ThumbnailError> {
        self.create_directory()?;
        let key = source.cache_key();
        // The picture, if there was an older one, no longer describes this
        // recording. Leaving it would leave the budget counting bytes nothing
        // will ever read.
        remove_if_present(&self.image_path(&key));
        self.write_sidecar(
            &key,
            &Entry {
                version: SIDECAR_VERSION,
                recording: source.path_text(),
                size_bytes: source.size(),
                modified_nanos: source.modified_nanos(),
                image: None,
                failure: Some(reason.to_string()),
            },
        )
    }

    /// Deletes the entry for a recording, if there is one.
    ///
    /// For a recording the user deleted. Pruning finds these too; this is the
    /// immediate answer for a caller that already knows.
    ///
    /// # Errors
    ///
    /// When an entry exists and cannot be deleted. A missing entry is success.
    pub fn forget(&self, path: impl AsRef<Path>) -> Result<(), ThumbnailError> {
        let path = path.as_ref();
        // The key is a digest of the path alone, so an entry is found for a
        // recording that no longer exists — which is the whole point here.
        let key = SourceIdentity::from_parts(path.to_path_buf(), 0, 0).cache_key();
        for file in [self.image_path(&key), self.sidecar_path(&key)] {
            match fs::remove_file(&file) {
                Ok(()) => {}
                Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
                Err(cause) => {
                    return Err(ThumbnailError::Cache {
                        detail: "remove an entry",
                        entry: clipped_logging::RedactedPath::new(&file),
                        cause,
                    })
                }
            }
        }
        Ok(())
    }

    /// Removes entries for recordings that are gone, then the oldest entries
    /// until the directory is inside its budget.
    ///
    /// Reports what it did rather than returning an error: a cache directory
    /// that cannot be read is a cache that holds nothing, and there is no caller
    /// for whom that is a failure. Anything unexpected is logged.
    pub fn prune(&self) -> PruneReport {
        let mut report = PruneReport::default();
        let Ok(listing) = fs::read_dir(&self.root) else {
            return report;
        };

        let mut sidecars = Vec::new();
        let mut images = Vec::new();
        for found in listing.flatten() {
            let path = found.path();
            let Ok(metadata) = found.metadata() else {
                continue;
            };
            match path.extension().and_then(|extension| extension.to_str()) {
                Some(SIDECAR_EXTENSION) => sidecars.push((path, metadata)),
                Some(IMAGE_EXTENSION) => images.push((path, metadata)),
                // A store killed between its write and its rename. Nothing else
                // ever deletes one: a lookup only opens `<key>.json` and
                // `<key>.jpg`, so an abandoned temporary is invisible to every
                // other path in this module and would sit here for ever without
                // counting towards the budget meant to bound the directory.
                Some(TEMPORARY_SUFFIX)
                    if remove(&path, "a store was interrupted before it finished") =>
                {
                    report.temporaries_removed += 1;
                    report.bytes_removed += metadata.len();
                }
                _ => {}
            }
        }

        // Sidecars whose recording has gone, and the pictures beside them.
        let mut live_keys = HashSet::new();
        let mut surviving = Vec::new();
        for (path, metadata) in sidecars {
            let key = stem_of(&path);
            let recording = self
                .read_sidecar(&path)
                .map(|entry| PathBuf::from(entry.recording));
            match recording {
                Some(recording) if recording.exists() => {
                    live_keys.insert(key.clone());
                    surviving.push(SurvivingEntry {
                        key,
                        bytes: metadata.len(),
                        written: metadata.modified().ok(),
                    });
                }
                // Either the recording is gone or the sidecar could not be read
                // at all. Both are dead weight, and both take the picture with
                // them.
                _ => {
                    if remove(&path, "the recording is gone") {
                        report.orphans_removed += 1;
                        report.entries_removed += 1;
                        report.bytes_removed += metadata.len();
                    }
                }
            }
        }

        // Pictures with no sidecar beside them: a store interrupted between its
        // two renames, or a sidecar removed above.
        for (path, metadata) in images {
            if live_keys.contains(&stem_of(&path)) {
                report.remaining_bytes += metadata.len();
            } else if remove(&path, "the picture has no sidecar") {
                report.strays_removed += 1;
                report.bytes_removed += metadata.len();
            }
        }

        report.remaining_bytes += surviving.iter().map(|entry| entry.bytes).sum::<u64>();
        if report.remaining_bytes > self.budget {
            // Oldest first, so the newest pictures — the recordings somebody is
            // looking at — are the ones that survive. Least recently *written*
            // rather than least recently used: recording a use would mean
            // writing to the directory every time a library screen was drawn.
            surviving.sort_by_key(|entry| entry.written);
            for entry in surviving {
                if report.remaining_bytes <= self.budget {
                    break;
                }
                let image = self.image_path(&entry.key);
                let image_bytes = fs::metadata(&image).map(|found| found.len()).unwrap_or(0);
                if remove(
                    &self.sidecar_path(&entry.key),
                    "the cache is over its budget",
                ) {
                    remove_if_present(&image);
                    let freed = entry.bytes + image_bytes;
                    report.remaining_bytes = report.remaining_bytes.saturating_sub(freed);
                    report.entries_removed += 1;
                    report.bytes_removed += freed;
                }
            }
        }

        report
    }

    /// The picture for a key.
    fn image_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.{IMAGE_EXTENSION}"))
    }

    /// The sidecar for a key.
    fn sidecar_path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.{SIDECAR_EXTENSION}"))
    }

    fn create_directory(&self) -> Result<(), ThumbnailError> {
        fs::create_dir_all(&self.root).map_err(|cause| ThumbnailError::Cache {
            detail: "create its directory",
            entry: clipped_logging::RedactedPath::new(&self.root),
            cause,
        })
    }

    /// Reads a sidecar, or [`None`] if there is not one this build understands.
    ///
    /// An unreadable or unparseable sidecar is removed on the spot rather than
    /// left to pruning: it will never become readable, and every lookup would
    /// otherwise log the same complaint again.
    fn read_sidecar(&self, path: &Path) -> Option<Entry> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return None,
            Err(cause) => {
                warn!(
                    entry = %clipped_logging::RedactedPath::new(path),
                    error = %cause,
                    "a thumbnail cache entry could not be read; it will be made again"
                );
                return None;
            }
        };

        match serde_json::from_slice::<Entry>(&bytes) {
            Ok(entry) if entry.version == SIDECAR_VERSION => Some(entry),
            Ok(entry) => {
                debug!(
                    entry = %clipped_logging::RedactedPath::new(path),
                    version = entry.version,
                    expected = SIDECAR_VERSION,
                    "a thumbnail cache entry was written by another build of Clipped"
                );
                None
            }
            Err(corrupt) => {
                warn!(
                    entry = %clipped_logging::RedactedPath::new(path),
                    reason = %corrupt,
                    "a thumbnail cache entry was unreadable and has been discarded"
                );
                remove_if_present(path);
                None
            }
        }
    }

    fn write_sidecar(&self, key: &str, entry: &Entry) -> Result<(), ThumbnailError> {
        let bytes = serde_json::to_vec_pretty(entry).map_err(|cause| ThumbnailError::Cache {
            detail: "describe an entry",
            entry: clipped_logging::RedactedPath::new(self.sidecar_path(key)),
            cause: std::io::Error::other(cause),
        })?;
        self.write_atomically(&self.sidecar_path(key), &bytes)
    }

    /// Writes `bytes` to `destination` through a temporary and a rename.
    fn write_atomically(&self, destination: &Path, bytes: &[u8]) -> Result<(), ThumbnailError> {
        // `<key>.jpg.writing`, appended rather than substituted: replacing the
        // extension would give the picture and its sidecar the same temporary
        // name, and a store would then race with itself.
        clipped_logging::write_atomically(destination, |temporary| {
            std::io::Write::write_all(temporary, bytes)
        })
        .map_err(|cause| ThumbnailError::Cache {
            detail: "replace an entry",
            entry: clipped_logging::RedactedPath::new(destination),
            cause,
        })
    }
}

/// A cache entry that survived the orphan sweep, and what it costs.
struct SurvivingEntry {
    key: String,
    bytes: u64,
    written: Option<std::time::SystemTime>,
}

/// What one [`ThumbnailCache::prune`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Entries deleted, for any reason.
    pub entries_removed: u64,
    /// Of those, entries whose recording no longer exists.
    pub orphans_removed: u64,
    /// Half-written files an interrupted store left behind.
    pub temporaries_removed: u64,
    /// Pictures with no sidecar to say what they were.
    pub strays_removed: u64,
    /// How many bytes were freed.
    pub bytes_removed: u64,
    /// How many bytes the directory holds now.
    pub remaining_bytes: u64,
}

/// The `<key>` part of `<key>.jpg` or `<key>.json`.
fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Deletes a file, reporting whether it went, and logging when it would not.
fn remove(path: &Path, why: &str) -> bool {
    match fs::remove_file(path) {
        Ok(()) => {
            debug!(
                entry = %clipped_logging::RedactedPath::new(path),
                reason = why,
                "removed a thumbnail cache file"
            );
            true
        }
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => false,
        Err(cause) => {
            warn!(
                entry = %clipped_logging::RedactedPath::new(path),
                error = %cause,
                "a thumbnail cache file could not be removed"
            );
            false
        }
    }
}

/// Deletes a file if it is there, ignoring a failure.
///
/// For the paths where the alternative to deleting is leaving a file that
/// nothing reads: pruning collects it later, so a failure here costs disk and
/// not correctness.
fn remove_if_present(path: &Path) {
    let _ = fs::remove_file(path);
}

/// A sidecar, as it is written down.
///
/// The format is documented in `docs/thumbnails.md`. Fields are added at the end
/// and read back with [`Option`] or [`Default`] where a missing one is
/// meaningful, so a sidecar from an older build of the same version still reads
/// (AGENTS.md section 43).
#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    /// The sidecar format version. [`SIDECAR_VERSION`] is the one this reads.
    version: u32,
    /// The recording this describes, as its path was spelled.
    recording: String,
    /// Its length in bytes when the thumbnail was made.
    size_bytes: u64,
    /// Its modification time, in nanoseconds since the Unix epoch.
    modified_nanos: i64,
    /// The picture, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<ImageRecord>,
    /// Why there is no picture, when there is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

impl Entry {
    fn identity(&self) -> SourceIdentity {
        SourceIdentity::from_parts(
            PathBuf::from(&self.recording),
            self.size_bytes,
            self.modified_nanos,
        )
    }
}

/// The picture half of a sidecar.
#[derive(Debug, Serialize, Deserialize)]
struct ImageRecord {
    /// The picture's file name, beside the sidecar.
    file: String,
    /// Its width in pixels.
    width: u32,
    /// Its height in pixels.
    height: u32,
    /// How far into the recording the frame was taken from.
    at_seconds: f64,
    /// Whether every frame considered was a flat colour.
    #[serde(default)]
    blank: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that deletes itself, without reaching for the media harness:
    /// these tests write a handful of small files and never touch a container.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos());
            let path = std::env::temp_dir().join(format!(
                "clipped-thumbnail-cache-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("a temporary directory can be created");
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A file standing in for a recording, and its identity.
    fn recording(scratch: &Scratch, name: &str) -> (PathBuf, SourceIdentity) {
        let path = scratch.file(name);
        fs::write(&path, b"not really a recording").expect("the file can be written");
        let identity = SourceIdentity::of(&path).expect("the file can be stat-ed");
        (path, identity)
    }

    /// Stores a picture for `identity` by hand, which is what a store does
    /// without needing a decoder to have run.
    fn store_picture(cache: &ThumbnailCache, identity: &SourceIdentity, bytes: &[u8]) {
        let key = identity.cache_key();
        cache.create_directory().expect("the directory is created");
        fs::write(cache.image_path(&key), bytes).expect("the picture can be written");
        cache
            .write_sidecar(
                &key,
                &Entry {
                    version: SIDECAR_VERSION,
                    recording: identity.path_text(),
                    size_bytes: identity.size(),
                    modified_nanos: identity.modified_nanos(),
                    image: Some(ImageRecord {
                        file: format!("{key}.{IMAGE_EXTENSION}"),
                        width: 640,
                        height: 360,
                        at_seconds: 12.5,
                        blank: false,
                    }),
                    failure: None,
                },
            )
            .expect("the sidecar can be written");
    }

    #[test]
    fn a_stored_thumbnail_is_found_again_with_everything_it_was_stored_with() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        let (path, identity) = recording(&scratch, "match.mkv");
        store_picture(&cache, &identity, b"jpeg bytes");

        let state = cache.lookup(&path);
        let thumbnail = state.thumbnail().expect("the thumbnail is ready");
        assert_eq!(thumbnail.width(), 640);
        assert_eq!(thumbnail.height(), 360);
        assert_eq!(thumbnail.at(), Duration::from_millis(12_500));
        assert!(!thumbnail.is_blank());
        assert_eq!(
            fs::read(thumbnail.image_path()).expect("the picture is there"),
            b"jpeg bytes"
        );
    }

    #[test]
    fn a_recording_that_was_replaced_does_not_show_the_previous_picture() {
        // The invalidation this cache exists to get right. A user who trims a
        // recording and sees the old frame has been shown something that is not
        // in the file any more.
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        let (path, identity) = recording(&scratch, "match.mkv");
        store_picture(&cache, &identity, b"the old frame");
        assert!(cache.lookup(&path).is_ready());

        fs::write(&path, b"a different recording entirely").expect("the file can be rewritten");
        assert!(
            matches!(cache.lookup(&path), ThumbnailState::Pending),
            "a rewritten recording still showed its previous thumbnail"
        );
    }

    #[test]
    fn a_missing_picture_is_something_to_make_again_rather_than_a_broken_screen() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        let (path, identity) = recording(&scratch, "match.mkv");
        store_picture(&cache, &identity, b"jpeg bytes");

        // Somebody emptied the cache directory of pictures but left the
        // sidecars, or a store was interrupted between its two renames.
        fs::remove_file(cache.image_path(&identity.cache_key())).expect("the picture is removed");
        assert!(matches!(cache.lookup(&path), ThumbnailState::Pending));
    }

    #[test]
    fn a_remembered_failure_is_reported_rather_than_retried() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        let (path, identity) = recording(&scratch, "truncated.mkv");

        cache
            .remember_failure(
                &identity,
                &ThumbnailError::NoVideo {
                    path: clipped_logging::RedactedPath::new(identity.path()),
                },
            )
            .expect("the failure can be written");

        let state = cache.lookup(&path);
        assert!(!state.is_ready());
        let reason = state.reason().expect("the screen is told why");
        assert!(
            reason.to_string().contains("no video stream"),
            "the remembered reason was {reason}"
        );

        // And a repaired recording is attempted again: what is remembered
        // belongs to the version of the file that failed, not to the path.
        fs::write(&path, b"a repaired recording").expect("the file can be rewritten");
        assert!(matches!(cache.lookup(&path), ThumbnailState::Pending));
    }

    #[test]
    fn a_recording_that_is_gone_takes_its_thumbnail_with_it() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        let (present, present_identity) = recording(&scratch, "kept.mkv");
        let (deleted, deleted_identity) = recording(&scratch, "deleted.mkv");
        store_picture(&cache, &present_identity, b"a picture");
        store_picture(&cache, &deleted_identity, b"a picture");

        fs::remove_file(&deleted).expect("the recording can be deleted");
        let report = cache.prune();

        assert_eq!(report.orphans_removed, 1, "{report:?}");
        assert!(!cache.image_path(&deleted_identity.cache_key()).exists());
        assert!(!cache.sidecar_path(&deleted_identity.cache_key()).exists());
        // And the recording that is still there keeps its thumbnail.
        assert!(cache.lookup(&present).is_ready());
    }

    #[test]
    fn pruning_collects_temporaries_and_pictures_nothing_describes() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        cache.create_directory().expect("the directory is created");

        // A store killed between its write and its rename, and a picture whose
        // sidecar never landed. Nothing but pruning ever removes either.
        let temporary = cache.root().join("abcdef0123456789.jpg.writing");
        fs::write(&temporary, b"half a picture").expect("the file can be written");
        let stray = cache.image_path("0123456789abcdef");
        fs::write(&stray, b"a picture nothing describes").expect("the file can be written");

        let report = cache.prune();
        assert_eq!(report.temporaries_removed, 1, "{report:?}");
        assert_eq!(report.strays_removed, 1, "{report:?}");
        assert!(!temporary.exists());
        assert!(!stray.exists());
    }

    #[test]
    fn the_directory_is_held_inside_its_budget_oldest_first() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache")).with_budget(1_024);
        let mut kept = Vec::new();
        for index in 0..8 {
            let (path, identity) = recording(&scratch, &format!("match-{index}.mkv"));
            store_picture(&cache, &identity, &vec![b'j'; 512]);
            // Writes have to be distinguishable in time, or "oldest" means
            // nothing. Ten milliseconds is comfortably above NTFS's resolution.
            std::thread::sleep(Duration::from_millis(10));
            kept.push(path);
        }

        let report = cache.prune();
        assert!(
            report.remaining_bytes <= cache.budget(),
            "the cache is still {} bytes over its budget: {report:?}",
            report.remaining_bytes - cache.budget()
        );
        assert!(report.entries_removed > 0, "{report:?}");
        // The newest survives, because it is the one somebody is looking at.
        assert!(
            cache
                .lookup(kept.last().expect("eight recordings"))
                .is_ready(),
            "the most recently written thumbnail was pruned first"
        );
        assert!(
            !cache.lookup(&kept[0]).is_ready(),
            "the oldest thumbnail survived a prune that had to delete something"
        );
    }

    #[test]
    fn a_sidecar_from_another_build_is_made_again_rather_than_guessed_at() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        let (path, identity) = recording(&scratch, "match.mkv");
        store_picture(&cache, &identity, b"jpeg bytes");

        let sidecar = cache.sidecar_path(&identity.cache_key());
        let text = fs::read_to_string(&sidecar).expect("the sidecar is there");
        fs::write(
            &sidecar,
            text.replace(r#""version": 1"#, r#""version": 999"#),
        )
        .expect("the sidecar can be rewritten");

        assert!(matches!(cache.lookup(&path), ThumbnailState::Pending));
    }

    #[test]
    fn an_unreadable_sidecar_is_discarded_rather_than_logged_about_for_ever() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        let (path, identity) = recording(&scratch, "match.mkv");
        store_picture(&cache, &identity, b"jpeg bytes");

        let sidecar = cache.sidecar_path(&identity.cache_key());
        fs::write(&sidecar, b"{ this was JSON once").expect("the sidecar can be rewritten");

        assert!(matches!(cache.lookup(&path), ThumbnailState::Pending));
        assert!(
            !sidecar.exists(),
            "an unreadable sidecar was left for the next lookup to complain about again"
        );
    }

    #[test]
    fn forgetting_a_recording_removes_both_of_its_files() {
        let scratch = Scratch::new();
        let cache = ThumbnailCache::at(scratch.file("cache"));
        let (path, identity) = recording(&scratch, "match.mkv");
        store_picture(&cache, &identity, b"jpeg bytes");

        cache.forget(&path).expect("the entry can be forgotten");
        assert!(!cache.image_path(&identity.cache_key()).exists());
        assert!(!cache.sidecar_path(&identity.cache_key()).exists());
        // Forgetting something that was never there is success, not an error.
        cache.forget(&path).expect("forgetting twice is harmless");
    }
}
