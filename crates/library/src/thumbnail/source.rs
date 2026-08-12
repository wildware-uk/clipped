//! Which recording a thumbnail was taken from, and whether it is still the same
//! recording.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clipped_logging::RedactedPath;

/// The modification time of a file whose filesystem does not report one.
pub const UNKNOWN_MODIFIED: i64 = i64::MIN;

/// What is recorded about a recording so that a cached thumbnail can be shown to
/// have come from it.
///
/// Path, length and modification time. Not a content hash: hashing a
/// two-gigabyte recording to decide whether to redraw a 40 kB picture would cost
/// far more than making the picture did, and the failure this has to catch is a
/// file that was trimmed, re-encoded or replaced in place, which changes at
/// least one of the three.
///
/// The modification time is [`UNKNOWN_MODIFIED`] on a filesystem that does not
/// report one. That is a valid identity — it means the length is all there is to
/// compare — and it is recorded rather than being an error, because losing a
/// thumbnail is cheap and refusing to make one is not.
///
/// # This is the second copy in the workspace
///
/// `clipped-waveform` has the same three fields for the same reason, and the two
/// cannot share one: both crates are layer 1, so neither may depend on the
/// other, and the only shared home would be a new crate below both. That is an
/// architecture change rather than a thumbnail ticket, so it is
/// [issue #293](https://github.com/wildware-uk/clipped/issues/293) and this
/// file says plainly that it is waiting on it (AGENTS.md section 55).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    path: PathBuf,
    size: u64,
    modified_nanos: i64,
}

impl SourceIdentity {
    /// Reads the identity of the file at `path`.
    ///
    /// # Errors
    ///
    /// When the file cannot be stat-ed, which normally means it is gone.
    pub fn of(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let metadata = fs::metadata(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            size: metadata.len(),
            modified_nanos: metadata
                .modified()
                .map_or(UNKNOWN_MODIFIED, nanos_since_epoch),
        })
    }

    /// Assembles an identity from values read back from a cache entry.
    #[must_use]
    pub fn from_parts(path: impl Into<PathBuf>, size: u64, modified_nanos: i64) -> Self {
        Self {
            path: path.into(),
            size,
            modified_nanos,
        }
    }

    /// The recording this describes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Its length in bytes when the thumbnail was taken.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Its modification time in nanoseconds since the Unix epoch, or
    /// [`UNKNOWN_MODIFIED`].
    #[must_use]
    pub fn modified_nanos(&self) -> i64 {
        self.modified_nanos
    }

    /// Whether an entry recorded against `self` still describes the file
    /// `current` was just read from.
    ///
    /// Paths are compared without regard to case on Windows, where two spellings
    /// of the same path name the same file.
    #[must_use]
    pub fn still_describes(&self, current: &Self) -> bool {
        self.size == current.size
            && self.modified_nanos == current.modified_nanos
            && normalise(&self.path) == normalise(&current.path)
    }

    /// The cache entry name for this source.
    ///
    /// A digest of the path, not of the contents, so that looking a thumbnail up
    /// costs a `stat` and one small file open. It reuses `clipped-logging`'s
    /// digest rather than introducing a second hash into the workspace; it is
    /// not cryptographic and does not have to be, because the entry carries the
    /// whole identity and [`still_describes`](Self::still_describes) checks it.
    /// A collision therefore costs a regeneration, not a wrong picture.
    #[must_use]
    pub fn cache_key(&self) -> String {
        format!("{:016x}", RedactedPath::new(normalise(&self.path)).digest())
    }

    /// The path in the form logs record it.
    #[must_use]
    pub fn redacted(&self) -> RedactedPath {
        RedactedPath::new(&self.path)
    }

    /// The path as the cache writes it down.
    ///
    /// Lossy, because a cache entry is JSON and JSON is text. A path Windows
    /// cannot spell in Unicode would come back as a different path, fail
    /// [`still_describes`](Self::still_describes) and be regenerated — the same
    /// outcome as a cache miss, which is the right one for derived data.
    #[must_use]
    pub fn path_text(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// A path in the form keys and comparisons are taken over.
fn normalise(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.into_owned()
    }
}

/// A modification time as nanoseconds since the Unix epoch.
///
/// Signed, and clamped rather than wrapping, so a file dated before 1970 — which
/// a copy tool can produce — is an ordinary early time rather than a time in the
/// far future.
fn nanos_since_epoch(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => {
            i64::try_from(before.duration().as_nanos()).map_or(UNKNOWN_MODIFIED + 1, |nanos| -nanos)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(path: &str, size: u64, modified: i64) -> SourceIdentity {
        SourceIdentity::from_parts(PathBuf::from(path), size, modified)
    }

    #[test]
    fn a_recording_that_was_trimmed_or_rewritten_is_no_longer_the_same_recording() {
        let original = identity("a.mkv", 100, 5);
        assert!(original.still_describes(&identity("a.mkv", 100, 5)));
        assert!(!original.still_describes(&identity("a.mkv", 101, 5)));
        assert!(!original.still_describes(&identity("a.mkv", 100, 6)));
    }

    #[test]
    fn a_different_file_is_never_the_same_recording_even_at_the_same_size() {
        let original = identity("a.mkv", 100, 5);
        assert!(!original.still_describes(&identity("b.mkv", 100, 5)));
    }

    #[test]
    fn two_spellings_of_one_windows_path_share_a_key_and_an_identity() {
        let lower = identity(r"c:\videos\match.mkv", 100, 5);
        let upper = identity(r"C:\Videos\Match.mkv", 100, 5);
        if cfg!(windows) {
            assert_eq!(lower.cache_key(), upper.cache_key());
            assert!(lower.still_describes(&upper));
        } else {
            assert_ne!(lower.cache_key(), upper.cache_key());
        }
    }

    #[test]
    fn a_key_is_a_fixed_width_hexadecimal_digest() {
        let key = identity(r"C:\videos\match.mkv", 1, 1).cache_key();
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|character| character.is_ascii_hexdigit()));
        assert_ne!(key, identity(r"C:\videos\other.mkv", 1, 1).cache_key());
    }

    #[test]
    fn a_time_before_the_epoch_is_negative_rather_than_enormous() {
        let before = UNIX_EPOCH - core::time::Duration::from_secs(1);
        assert_eq!(nanos_since_epoch(before), -1_000_000_000);
        assert_eq!(nanos_since_epoch(UNIX_EPOCH), 0);
    }
}
