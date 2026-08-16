//! Which recording something derived — a waveform, a thumbnail — was made
//! from, and whether it is still the same recording.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The modification time of a file whose filesystem does not report one.
pub const UNKNOWN_MODIFIED: i64 = i64::MIN;

/// What is recorded about a source file so that something computed from it —
/// a waveform, a thumbnail — can be shown to have come from it.
///
/// Path, length and modification time. Not a content hash: hashing a
/// two-gigabyte recording to decide whether to redraw a waveform, or a
/// thumbnail forty kilobytes in size, would cost more than making the thing
/// did, and the failure this has to catch is a file that was trimmed,
/// re-encoded or replaced in place, which changes at least one of the three.
///
/// The modification time is [`UNKNOWN_MODIFIED`] on a filesystem that does not
/// report one. That is a valid identity: it simply means the length is all
/// there is to compare, and a same-length replacement is not detected. It is
/// recorded rather than being an error, because losing a cached result is
/// cheap and refusing to make one is not.
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

    /// Assembles an identity from values that were read back from a cache
    /// entry.
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

    /// Its length in bytes when it was read.
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
    /// Paths are compared without regard to case on Windows, where two
    /// spellings of the same path name the same file.
    #[must_use]
    pub fn still_describes(&self, current: &Self) -> bool {
        self.size == current.size
            && self.modified_nanos == current.modified_nanos
            && same_path(&self.path, &current.path)
    }

    /// The cache entry name for this source.
    ///
    /// A digest of the path, not of the contents, so that looking a cached
    /// result up costs a `stat` and one small file open. Not cryptographic,
    /// and it does not have to be, because the entry carries the whole
    /// identity and [`still_describes`](Self::still_describes) checks it — a
    /// collision therefore costs a recomputation, not a wrong result.
    #[must_use]
    pub fn cache_key(&self) -> String {
        format!("{:016x}", fnv1a_64(normalise(&self.path).as_bytes()))
    }

    /// The path as a cache writes it down.
    ///
    /// Lossy, because a cache entry that is text — JSON, in the thumbnail
    /// sidecar — can only be text. A path Windows cannot spell in Unicode
    /// would come back as a different path, fail
    /// [`still_describes`](Self::still_describes) and be regenerated — the
    /// same outcome as a cache miss, which is the right one for derived
    /// data.
    #[must_use]
    pub fn path_text(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// A path in the form the cache key and [`SourceIdentity::still_describes`]
/// are taken over.
fn normalise(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.into_owned()
    }
}

/// Whether two paths name the same file as far as this platform is concerned.
fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        normalise(left) == normalise(right)
    } else {
        left == right
    }
}

/// A modification time as nanoseconds since the Unix epoch.
///
/// Signed, and clamped rather than wrapping, so a file dated before 1970 —
/// which a copy tool can produce — is an ordinary early time rather than a
/// time in the far future.
fn nanos_since_epoch(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => {
            i64::try_from(before.duration().as_nanos()).map_or(UNKNOWN_MODIFIED + 1, |nanos| -nanos)
        }
    }
}

/// FNV-1a, 64-bit.
///
/// The same algorithm `clipped_logging::RedactedPath` uses for the same
/// reason: the digest only has to be stable and cheap, not irreversible, and
/// implementing it here is eleven lines against a dependency this crate is
/// not allowed to take — `clipped-logging` is layer 0 too, and a layer-0
/// crate depends on nothing else in this workspace (`src/lib.rs`). The two
/// copies computing the same digest for the same bytes is not a coincidence
/// worth hiding: a cache key here and a log digest there being able to
/// correlate is a small, free benefit of having picked the same algorithm
/// independently, not a contract either crate relies on.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(path: &str, size: u64, modified: i64) -> SourceIdentity {
        SourceIdentity::from_parts(PathBuf::from(path), size, modified)
    }

    #[test]
    fn a_file_that_grew_or_was_rewritten_is_no_longer_the_same_recording() {
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
            // Elsewhere those are two files, and nothing here pretends
            // otherwise.
            assert_ne!(lower.cache_key(), upper.cache_key());
        }
    }

    #[test]
    fn a_key_is_a_fixed_width_hexadecimal_digest() {
        let key = identity(r"C:\videos\match.mkv", 1, 1).cache_key();
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|character| character.is_ascii_hexdigit()));
        // Different paths get different entries.
        assert_ne!(key, identity(r"C:\videos\other.mkv", 1, 1).cache_key());
    }

    #[test]
    fn a_time_before_the_epoch_is_negative_rather_than_enormous() {
        let before = UNIX_EPOCH - core::time::Duration::from_secs(1);
        assert_eq!(nanos_since_epoch(before), -1_000_000_000);
        assert_eq!(nanos_since_epoch(UNIX_EPOCH), 0);
    }

    #[test]
    fn the_identity_of_a_real_file_is_its_length_and_its_time() {
        let directory = clipped_media_validation::TemporaryDirectory::new("background-identity");
        let path = directory.file("recording.bin");
        std::fs::write(&path, b"0123456789").expect("the file can be written");

        let identity = SourceIdentity::of(&path).expect("the file can be stat-ed");
        assert_eq!(identity.size(), 10);
        assert_eq!(identity.path(), path);
        assert!(identity.still_describes(&SourceIdentity::of(&path).expect("still there")));

        std::fs::write(&path, b"0123456789abcdef").expect("the file can be rewritten");
        assert!(!identity.still_describes(&SourceIdentity::of(&path).expect("still there")));
    }

    #[test]
    fn a_missing_file_has_no_identity() {
        let directory = clipped_media_validation::TemporaryDirectory::new("background-identity");
        assert!(SourceIdentity::of(directory.file("absent.mkv")).is_err());
    }

    #[test]
    fn path_text_round_trips_an_ordinary_path() {
        // What the thumbnail sidecar stores and reads back (issue #293's
        // consumer): lossy in general, exact for any path made of valid
        // Unicode, which is every path either crate's own tests write.
        let identity = identity(r"C:\Videos\Clipped\match.mkv", 1, 1);
        assert_eq!(identity.path_text(), r"C:\Videos\Clipped\match.mkv");
    }
}
