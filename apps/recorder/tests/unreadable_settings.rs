//! Proves that a settings file this build cannot read is *reported*.
//!
//! `watch::tests` proves what such a file does to a recording: the shipped
//! defaults stand, the command line survives, and the file is left byte for
//! byte as it was found (the data-loss defect found in #108). What those tests
//! cannot prove is the other half of the promise `docs/recorder-cli.md` makes —
//! that the user is told — because a diagnostic nobody observes is a line
//! somebody can delete without a single test noticing.
//!
//! # Why this is a binary of its own
//!
//! The report is observed by running the real `load_configuration` against a
//! real file, inside a subscriber. That only works in a process where nothing
//! else is installing subscribers: `tracing` caches a decision per callsite,
//! and two threads holding different scoped subscribers at once leaves this
//! module's callsite cached as one nothing reaches. Sharing a process with
//! `clipped-recorder`'s other tests made this test fail about half the time,
//! which is worse than not having it. `crates/logging/tests/frame_tracing.rs`
//! is split from its neighbour for exactly this reason.
//!
//! There is deliberately one `#[test]` here. A second would put two subscribers
//! back in one process.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clipped_recorder::watch::load_configuration;
use clipped_session::config::{Configuration, ConfigurationStore, FILE_NAME};
use tracing_subscriber::fmt::MakeWriter;

/// A settings directory of this test's own, removed when it is dropped.
///
/// The workspace has no `tempfile` dependency and this is not enough reason to
/// add one; `clipped_recorder::config` and `clipped_session::automatic` build
/// the same thing from `std::env::temp_dir` (AGENTS.md sections 10 and 55).
struct SettingsDirectory(PathBuf);

impl SettingsDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-unreadable-settings-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the directory can be created");
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join(FILE_NAME)
    }
}

impl Drop for SettingsDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Collects everything a subscriber writes, so this test can assert on it.
///
/// The same shape `clipped_library`'s `accounting_privacy` tests use.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

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

/// Loads the settings at `path` with everything the recorder logged while it
/// did so.
fn loading(path: &Path) -> (Configuration, String) {
    let log = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();

    let configuration =
        tracing::subscriber::with_default(subscriber, || load_configuration(Some(path)));
    let written = String::from_utf8(
        log.0
            .lock()
            .expect("the capture buffer is not poisoned")
            .clone(),
    )
    .expect("the subscriber writes UTF-8");
    (configuration, written)
}

#[test]
fn an_unreadable_settings_file_is_reported_once_where_it_is_read() {
    let unreadable = SettingsDirectory::new("unreadable");
    fs::write(
        unreadable.file(),
        "{ \"schema_version\": 99, \"this build\": cannot read this",
    )
    .expect("the file can be written");

    let (configuration, report) = loading(&unreadable.file());

    // The state whose report is being asserted was reached, so that this
    // cannot pass by the file having turned out to be readable after all.
    assert_eq!(
        configuration,
        Configuration::defaults(),
        "a settings file this build cannot read leaves the shipped defaults standing"
    );

    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "an unreadable settings file is reported exactly once: {lines:?}"
    );
    let line = lines[0];
    assert!(
        line.contains("WARN"),
        "the report is a warning, not a debug line nobody will see: {line}"
    );
    assert!(
        line.contains("Settings not applied"),
        "the report carries the sentence the user is shown: {line}"
    );
    assert!(
        line.contains("left as it is"),
        "the report says the file was not written over, which is the part somebody who fears \
         they have lost their settings needs: {line}"
    );

    // And nothing is said when there is nothing wrong. A report that happened
    // either way would prove nothing about the case above.
    let readable = SettingsDirectory::new("readable");
    ConfigurationStore::at(readable.file())
        .store(Configuration::defaults())
        .expect("the settings file can be written");
    let (_, quiet) = loading(&readable.file());
    assert!(
        quiet.is_empty(),
        "a settings file that reads cleanly is not worth a word: {quiet}"
    );

    let absent = SettingsDirectory::new("absent");
    let (_, silence) = loading(&absent.file());
    assert!(
        silence.is_empty(),
        "a machine with no settings file has nothing wrong with it: {silence}"
    );
}
