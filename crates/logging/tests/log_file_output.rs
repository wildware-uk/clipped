//! End-to-end checks on the subscriber this crate installs.
//!
//! Everything here needs a real installed subscriber writing real files, and
//! `tracing` allows exactly one subscriber per process, so this file is one
//! test containing several assertions rather than several tests. Splitting it
//! would mean either a second process per assertion or the second assertion
//! failing with `AlreadyInitialised`.
//!
//! Between them the assertions cover two acceptance criteria of issue #5: that
//! the level is configurable without a rebuild, and that the space logs occupy
//! is bounded.

use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use clipped_logging::{
    AudioSource, CaptureBackend, GameId, LogSettings, RedactedPath, SessionContext, SessionId,
    VideoEncoder,
};

/// How many rotated files this run is allowed to keep.
const RETAINED_FILES: usize = 4;

/// How many stale log files exist before the run starts.
const STALE_FILES: usize = 12;

/// A directory that deletes itself, so a failing test does not leave log files
/// in the temporary directory of whoever ran it.
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("clipped-logging-{label}-{}", std::process::id()));
        // A previous crashed run may have left the directory behind.
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory should be creatable");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The log files in `directory`, sorted by name.
fn log_files(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("the log directory should be readable")
        .map(|entry| {
            entry
                .expect("the directory entry should be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Everything the run wrote, concatenated.
fn log_contents(directory: &Path) -> String {
    log_files(directory)
        .iter()
        .map(|name| {
            fs::read_to_string(directory.join(name)).expect("a log file should be readable")
        })
        .collect()
}

#[test]
fn the_installed_subscriber_writes_bounded_configurable_files() {
    let directory = TempDirectory::new("file-output");

    // Files an earlier, long-running instance would have left behind. Rotation
    // is hourly, so these are what two hours of a multi-day run looks like.
    for hour in 0..STALE_FILES {
        fs::write(
            directory
                .path()
                .join(format!("clipped.2020-01-01-{hour:02}.log")),
            "stale\n",
        )
        .expect("the stale log file should be writable");
    }

    // The level is taken from the environment, which is the whole point: this
    // process was not rebuilt to log at `debug`. Nothing else in this binary
    // reads the environment, so mutating it here is safe.
    std::env::set_var("CLIPPED_LOG", "debug");
    std::env::remove_var("RUST_LOG");

    let settings = LogSettings::default()
        .with_directory(directory.path())
        .without_level_file()
        .with_console(false)
        .with_retained_files(NonZeroUsize::new(RETAINED_FILES).unwrap());
    let guard = clipped_logging::init(&settings).expect("the subscriber should install");
    assert_eq!(guard.directory(), directory.path());

    let context = SessionContext::new(SessionId::new("01H8XGJT4A").expect("a valid session id"))
        .with_game_id(GameId::new("counter-strike-2").expect("a valid game id"))
        .with_capture_backend(CaptureBackend::WindowsGraphicsCapture)
        .with_encoder(VideoEncoder::Nvenc)
        .with_audio_source(AudioSource::Microphone);
    {
        let span = context.span();
        let _entered = span.enter();

        tracing::info!("session started");
        tracing::debug!("only recorded because CLIPPED_LOG raised the level");
        tracing::trace!("must not be recorded at debug");
        tracing::info!(
            path = %RedactedPath::new(r"C:\Users\alice\Videos\Clipped\match.mkv"),
            "recording path chosen"
        );
    }

    // Dropping the guard stops the background writer, which flushes what it
    // still holds and closes the file.
    drop(guard);

    let files = log_files(directory.path());
    assert_eq!(
        files.len(),
        RETAINED_FILES,
        "the appender must prune down to its retention bound at start-up, \
         leaving {RETAINED_FILES} files, but found {files:?}"
    );
    assert!(
        files
            .iter()
            .any(|name| name.starts_with("clipped.") && name.ends_with(".log")),
        "the current log file should follow the documented naming scheme: {files:?}"
    );

    let contents = log_contents(directory.path());

    // Level configurable without a rebuild.
    assert!(
        contents.contains("only recorded because CLIPPED_LOG raised the level"),
        "CLIPPED_LOG=debug should have admitted the debug event:\n{contents}"
    );
    assert!(
        !contents.contains("must not be recorded at debug"),
        "the trace event should have been filtered out:\n{contents}"
    );
    assert!(
        contents.contains("filter_source=CLIPPED_LOG"),
        "the run should record where its filter came from:\n{contents}"
    );

    // The standard context fields reach the file, under their documented names.
    for field in [
        "session_id=01H8XGJT4A",
        "game_id=counter-strike-2",
        "capture_backend=windows_graphics_capture",
        "encoder=nvenc",
        "audio_source=microphone",
    ] {
        assert!(contents.contains(field), "missing {field} in:\n{contents}");
    }

    // Privacy: a recording path reached the file without the account name.
    assert!(
        contents.contains("path=match.mkv#"),
        "the redacted path should identify the file:\n{contents}"
    );
    for leaked in ["alice", "Videos", r"C:\Users"] {
        assert!(
            !contents.contains(leaked),
            "the log leaked {leaked}:\n{contents}"
        );
    }
}
