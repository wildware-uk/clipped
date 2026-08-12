//! What this crate's own log lines say about where files live.
//!
//! `crates/waveform/Cargo.toml` justifies the `clipped-logging` dependency with
//! the claim that this crate "logs the reduced form rather than the path", and
//! `docs/logging.md` states that no directory component survives redaction —
//! "not the account name". Both were true of the recording paths and false of
//! the cache's own paths, which were logged and formatted into error messages
//! raw: a debug line reading `C:\Users\<account>\AppData\Local\Clipped\...` in a
//! file somebody attaches to a bug report (AGENTS.md section 13).
//!
//! `crates/logging/tests/privacy.rs` explicitly disclaims covering hand-written
//! call sites, so this is where that claim is held to. It renders real log
//! output from real cache operations through a real subscriber and asserts on
//! the bytes that would have reached the log file.
//!
//! It does not open a device, a window or a recording: it writes small files and
//! reads them back.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clipped_media_validation::TemporaryDirectory;
use clipped_waveform::{SourceIdentity, WaveformCache, WaveformError};
use tracing_subscriber::fmt::MakeWriter;

/// Directory names that stand in for the ones a real path carries: the Windows
/// account name and two folders somebody chose. None of them may reach the log.
///
/// Deliberately unlike anything else that appears in a log line — a crate name,
/// a field name, a level — so that a match is a leak and never a coincidence.
const DIRECTORIES: &[&str] = &["wf-account-name", "wf-videos-folder", "wf-library-folder"];

/// Collects everything a subscriber writes, so a test can assert on it.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl CapturedLog {
    fn contents(&self) -> String {
        String::from_utf8_lossy(
            &self
                .0
                .lock()
                .expect("the capture buffer is not poisoned")
                .clone(),
        )
        .into_owned()
    }
}

impl io::Write for CapturedLog {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("the capture buffer is not poisoned")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for CapturedLog {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

/// Runs `body` against a subscriber local to this thread and returns everything
/// it logged, at every level.
fn captured(body: impl FnOnce()) -> String {
    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::with_default(subscriber, body);
    captured.contents()
}

/// A cache and a recording, both buried under [`DIRECTORIES`].
///
/// The shape of the real thing: the cache lives under the per-user directory,
/// whose first component is the account name, and the recording lives under
/// folders the user named.
fn buried(directory: &TemporaryDirectory) -> (WaveformCache, PathBuf) {
    let mut root = directory.file("library");
    for name in DIRECTORIES {
        root = root.join(name);
    }
    std::fs::create_dir_all(&root).expect("the directories can be created");

    let recording = root.join("match.mkv");
    std::fs::write(&recording, vec![0u8; 64]).expect("the recording can be written");
    (WaveformCache::at(root.join("waveforms")), recording)
}

/// Fails with the offending line when a directory component reached the log.
fn assert_nothing_leaked(output: &str, what: &str) {
    for leaked in DIRECTORIES {
        assert!(
            !output.contains(leaked),
            "{what} put the directory {leaked} into the log:\n{output}"
        );
    }
}

/// Fails when nothing was logged at all, which would make the assertions above
/// pass for the wrong reason.
fn assert_something_was_logged(output: &str, what: &str) {
    assert!(
        !output.trim().is_empty(),
        "{what} logged nothing, so this test proves nothing"
    );
}

#[test]
fn pruning_logs_which_entries_it_removed_without_saying_where_they_lived() {
    let directory = TemporaryDirectory::new("waveform-privacy");
    let (cache, recording) = buried(&directory);
    let identity = SourceIdentity::of(&recording).expect("the recording exists");

    cache
        .remember_failure(&identity, &WaveformError::Cancelled)
        .expect("an entry can be written");
    // A temporary an interrupted store left behind, and an entry whose
    // recording is gone: the two things `prune` deletes and logs.
    std::fs::write(cache.root().join("0123456789abcdef.cwf.writing"), b"half")
        .expect("the temporary can be written");
    std::fs::remove_file(&recording).expect("the recording can be deleted");

    let output = captured(|| {
        let report = cache.prune();
        assert_eq!(report.entries_removed(), 2, "{report:?}");
        assert_eq!(report.orphans_removed(), 1, "{report:?}");
        assert_eq!(report.temporaries_removed(), 1, "{report:?}");
    });

    assert_something_was_logged(&output, "pruning two entries");
    assert_nothing_leaked(&output, "pruning");
    // And it still says which entry went, in the reduced form: a digest
    // identifies the file without describing where it was.
    assert!(
        output.contains("entry=") && output.contains(".cwf#"),
        "pruning did not say which entry it removed:\n{output}"
    );
}

#[test]
fn a_cache_that_cannot_be_written_says_so_without_naming_the_directories() {
    let directory = TemporaryDirectory::new("waveform-privacy");
    let (_, recording) = buried(&directory);
    let identity = SourceIdentity::of(&recording).expect("the recording exists");

    // A file where the cache directory should be, so nothing can be created
    // under it. This is the error `WaveformService` logs at warn level next to
    // the recording it was working on.
    let obstruction = recording.with_file_name("blocked");
    std::fs::write(&obstruction, b"not a directory").expect("the file can be written");
    let cache = WaveformCache::at(obstruction.join("waveforms"));

    let error = cache
        .remember_failure(&identity, &WaveformError::Cancelled)
        .expect_err("no directory can be created underneath a file");

    let output = captured(|| {
        tracing::warn!(
            recording = %clipped_logging::RedactedPath::new(&recording),
            error = %error,
            "a waveform could not be cached"
        );
    });

    assert_something_was_logged(&output, "a cache failure");
    assert_nothing_leaked(&output, "a cache error message");
    assert!(
        output.contains("match.mkv#") && output.contains("waveforms#"),
        "the line names neither the recording nor the directory:\n{output}"
    );
}

#[test]
fn an_unreadable_entry_is_reported_without_naming_the_directories() {
    let directory = TemporaryDirectory::new("waveform-privacy");
    let (cache, recording) = buried(&directory);
    let identity = SourceIdentity::of(&recording).expect("the recording exists");
    cache
        .remember_failure(&identity, &WaveformError::Cancelled)
        .expect("an entry can be written");

    // Half an entry, of the kind a power cut leaves. The lookup that finds it
    // logs at warn level and deletes it, which logs again.
    let entry = first_entry(cache.root());
    std::fs::write(&entry, b"CLIPWAVE").expect("the entry can be truncated");

    let output = captured(|| {
        let state = cache.lookup(&recording);
        assert!(!state.is_ready());
    });

    assert_something_was_logged(&output, "an unreadable entry");
    assert_nothing_leaked(&output, "an unreadable entry");
}

/// The one `.cwf` in a directory that has just had one written to it.
fn first_entry(root: &Path) -> PathBuf {
    std::fs::read_dir(root)
        .expect("the cache directory exists")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|suffix| suffix.to_str()) == Some("cwf"))
        .expect("an entry was written")
}
