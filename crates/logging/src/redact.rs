//! Reduces filesystem paths to something safe to record.

use std::fmt;
use std::path::Path;

/// The longest file name kept in a [`RedactedPath`].
const FILE_NAME_LIMIT: usize = 64;

/// A path reduced to a form that identifies a file in logs without describing
/// where it lives.
///
/// Recording paths are one of the things SPEC.md section 36 asks to be logged,
/// and they are also the most likely way for personal information to reach a
/// log file: a Windows user path starts with the account name, and a library
/// path names the folders someone chose. [`RedactedPath`] keeps only the final
/// component and a digest of the whole path.
///
/// The digest is what makes the redacted form useful: two log lines about the
/// same file share a digest, so a sequence of operations can still be followed,
/// while two different files that happen to share a name stay distinguishable.
///
/// ```
/// use clipped_logging::RedactedPath;
///
/// // Written with forward slashes, which Windows accepts as separators too, so
/// // that this example also holds on a contributor's Linux or macOS machine —
/// // `Path::file_name` only splits on the separators of the platform it was
/// // compiled for, and a backslash means nothing to a Unix path.
/// let redacted = RedactedPath::new("C:/Users/alice/Videos/Clipped/match.mkv");
/// assert!(!redacted.to_string().contains("alice"));
/// assert!(redacted.to_string().starts_with("match.mkv#"));
/// ```
///
/// # What this does and does not guarantee
///
/// It guarantees that no directory component survives, so the account name,
/// the drive layout and the folder names do not reach the log.
///
/// It does not anonymise the file name itself. Clipped names its own
/// recordings, so that is normally a generated name — but a path the *user*
/// chose can still carry meaning in its final component. Redaction is a
/// backstop, not a licence to log arbitrary user paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RedactedPath {
    file_name: String,
    digest: u64,
}

impl RedactedPath {
    /// Redacts a path.
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let digest = fnv1a_64(path.as_os_str().to_string_lossy().as_bytes());
        let file_name = path
            .file_name()
            .map(|name| sanitise(&name.to_string_lossy()))
            .unwrap_or_default();

        Self { file_name, digest }
    }

    /// The final path component, with control characters replaced and the
    /// length capped.
    ///
    /// Empty when the path had no final component, such as a bare drive root.
    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// A stable, non-cryptographic digest of the full original path.
    ///
    /// Equal digests mean the same path; this is for correlating log lines, not
    /// for hiding a value from someone determined to recover it.
    #[must_use]
    pub fn digest(&self) -> u64 {
        self.digest
    }
}

impl fmt::Display for RedactedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{:016x}", self.file_name, self.digest)
    }
}

/// Replaces anything that could break a log line, and caps the length.
fn sanitise(file_name: &str) -> String {
    let mut sanitised: String = file_name
        .chars()
        .take(FILE_NAME_LIMIT)
        .map(|character| {
            if character.is_control() || character == '#' {
                '_'
            } else {
                character
            }
        })
        .collect();

    if file_name.chars().count() > FILE_NAME_LIMIT {
        sanitised.push('~');
    }
    sanitised
}

/// FNV-1a, 64-bit.
///
/// Chosen over a cryptographic hash because the digest only has to be stable
/// and cheap: it is a correlation key, and the documentation for
/// [`RedactedPath`] does not claim it is irreversible. Implementing it here
/// avoids a dependency for eleven lines of arithmetic.
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

    /// A path in the form the platform these tests are compiled for uses.
    ///
    /// `Path::file_name` splits on that platform's separators only, so a
    /// backslash-separated literal is one long file name on Linux or macOS and
    /// an assertion written against it would fail there for a reason that has
    /// nothing to do with redaction. Windows is the platform Clipped ships on
    /// and keeps the backslash form; everywhere else the same directories are
    /// spelled the local way.
    #[cfg(windows)]
    const RECORDING_PATH: &str = r"C:\Users\alice\Videos\Clipped\match.mkv";
    #[cfg(not(windows))]
    const RECORDING_PATH: &str = "/home/alice/Videos/Clipped/match.mkv";

    #[test]
    fn directory_components_do_not_survive() {
        let redacted = RedactedPath::new(RECORDING_PATH).to_string();

        assert!(redacted.starts_with("match.mkv#"), "{redacted}");
        for leaked in ["alice", "Videos", "Clipped"] {
            assert!(!redacted.contains(leaked), "{redacted} leaked {leaked}");
        }
    }

    #[test]
    #[cfg(windows)]
    fn a_windows_drive_and_account_name_do_not_survive() {
        let redacted = RedactedPath::new(r"C:\Users\alice\Videos\Clipped\match.mkv").to_string();

        // Pinned rather than merely inspected, because `docs/logging.md` prints
        // this exact line as an example of what a redacted path looks like.
        assert_eq!(redacted, "match.mkv#eb9715073a66288e");
        for leaked in ["alice", "Users", "C:"] {
            assert!(!redacted.contains(leaked), "{redacted} leaked {leaked}");
        }
    }

    #[test]
    fn a_windows_path_written_with_forward_slashes_is_redacted_anywhere() {
        // Windows accepts both separators, so this form is a real Windows path
        // and is also one every platform can parse.
        let redacted = RedactedPath::new("C:/Users/alice/Videos/Clipped/match.mkv").to_string();

        assert!(redacted.starts_with("match.mkv#"), "{redacted}");
        for leaked in ["alice", "Users", "C:"] {
            assert!(!redacted.contains(leaked), "{redacted} leaked {leaked}");
        }
    }

    #[test]
    fn the_same_path_redacts_to_the_same_value() {
        let path = "/home/alice/clips/match.mkv";
        assert_eq!(RedactedPath::new(path), RedactedPath::new(path));
    }

    #[test]
    fn files_with_the_same_name_stay_distinguishable() {
        let first = RedactedPath::new("D:/clips/2026/match.mkv");
        let second = RedactedPath::new("D:/clips/2025/match.mkv");

        assert_eq!(first.file_name(), second.file_name());
        assert_ne!(first.digest(), second.digest());
    }

    #[test]
    fn a_path_without_a_file_name_still_redacts() {
        // A bare root: `C:\` on Windows, `/` elsewhere. Both have no final
        // component, which is the case being covered.
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let redacted = RedactedPath::new(root).to_string();

        assert_eq!(redacted.chars().next(), Some('#'));
        assert!(!redacted.contains(root), "{redacted}");
    }

    #[test]
    fn control_characters_cannot_break_a_log_line() {
        let redacted = RedactedPath::new("/tmp/two\nlines.mkv").to_string();
        assert!(!redacted.contains('\n'), "{redacted}");
    }

    #[test]
    fn long_file_names_are_capped() {
        let name = "a".repeat(200);
        let redacted = RedactedPath::new(format!("/tmp/{name}"));
        assert_eq!(redacted.file_name().chars().count(), FILE_NAME_LIMIT + 1);
    }
}
