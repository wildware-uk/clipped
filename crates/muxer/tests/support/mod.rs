//! Shared machinery for the muxer's media tests: starting, following and
//! killing the recorder that writes them.
//!
//! Taking the finished recording apart again is not here. AGENTS.md section 22
//! asks that generated media be validated rather than assumed valid, and the
//! workspace has one harness for that — `clipped-media-validation`, in
//! `tests/media` — which every crate that produces media uses and which is
//! itself tested against truncated files, files with a track missing and files
//! whose timestamps go backwards. This crate having its own `ffprobe` wrapper
//! is what that harness replaced (`docs/testing.md`).

// Each test binary compiles this module separately and uses the part of it that
// it needs, so anything used by only one of them is "unused" in the others.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The `synthetic_recording` example, built beside this test binary.
///
/// Cargo publishes no environment variable naming an example's path — only
/// binaries get `CARGO_BIN_EXE_*` — but it does put examples in
/// `<target>/<profile>/examples`, and this test executable is in
/// `<target>/<profile>/deps`.
pub(crate) fn synthetic_recording_example() -> PathBuf {
    let test_executable = std::env::current_exe().expect("a test knows its own path");
    let profile_dir = test_executable
        .parent()
        .and_then(Path::parent)
        .expect("a test executable lives in <target>/<profile>/deps");
    let example = profile_dir
        .join("examples")
        .join("synthetic_recording")
        .with_extension(std::env::consts::EXE_EXTENSION);

    assert!(
        example.is_file(),
        "{} has not been built. `cargo test` builds the examples; if this test was run \
         some other way, build them first with `cargo build -p clipped-muxer --examples`.",
        example.display()
    );
    example
}

/// Runs the synthetic recording example, waiting for it to finish.
pub(crate) fn run_synthetic_recording(arguments: &[&str]) -> std::process::Output {
    Command::new(synthetic_recording_example())
        .args(arguments)
        .output()
        .expect("the synthetic recording example can be run")
}

/// The synthetic recording example, started and left running.
///
/// Its standard output is a pipe so that a test can follow how much media it
/// has written, and kill it at a known point.
#[derive(Debug)]
pub(crate) struct RunningRecorder {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
    /// The last progress line read, from either of the methods below.
    last_reported: f64,
}

impl RunningRecorder {
    /// Starts the example.
    pub(crate) fn start(arguments: &[&str]) -> Self {
        let mut child = Command::new(synthetic_recording_example())
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the synthetic recording example can be started");

        let output = BufReader::new(child.stdout.take().expect("standard output was a pipe"));
        Self {
            child,
            output,
            last_reported: 0.0,
        }
    }

    /// Reads progress until the recording has written at least `seconds` of
    /// media.
    ///
    /// Panics if the process stops first, because a test that silently went on
    /// to kill a process that had already died would prove nothing.
    pub(crate) fn wait_for_media_seconds(&mut self, seconds: f64) {
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .output
                .read_line(&mut line)
                .expect("the recording's output can be read");
            assert!(
                read > 0,
                "the recorder stopped before it had written {seconds} seconds of media"
            );

            if let Some(written) = parse_media_seconds(&line) {
                self.last_reported = written;
                if written >= seconds {
                    return;
                }
            }
        }
    }

    /// Ends the process the way a crash would — immediately, with no chance to
    /// clean up — and reports the last moment of media it said it had written.
    ///
    /// On Windows this is `TerminateProcess`, which runs no destructor, flushes
    /// no buffer and gives the process no notification. That is the point: it
    /// is as close to a power cut as a test can get, and it is what
    /// AGENTS.md section 17 asks the container to survive.
    ///
    /// The figure comes from what is left in the pipe after the kill rather
    /// than from the line the test was waiting for. Between deciding to kill
    /// and the kill landing, the recorder keeps working — for however long the
    /// machine takes to schedule it, which on a loaded build agent is not a
    /// bounded quantity — so the line that triggered the kill is a lower bound
    /// on what was written, not a measure of it. Reading the pipe to the end
    /// gives the real one, and what the child had already written to the pipe
    /// survives its death.
    pub(crate) fn kill(mut self) -> f64 {
        self.child.kill().expect("the recorder can be killed");
        self.child.wait().expect("the recorder can be reaped");

        let mut line = String::new();
        while self
            .output
            .read_line(&mut line)
            .expect("the recording's remaining output can be read")
            > 0
        {
            if let Some(written) = parse_media_seconds(&line) {
                self.last_reported = written;
            }
            line.clear();
        }
        self.last_reported
    }
}

/// Reads one of the recorder's progress lines.
fn parse_media_seconds(line: &str) -> Option<f64> {
    line.trim().strip_prefix("media_seconds=")?.parse().ok()
}

/// Builds a fixture with the pinned build's own `ffmpeg`.
///
/// For sources `examples/synthetic_recording.rs` cannot produce. It writes H.264
/// with `libopenh264`, which does not reorder and has no B-frames, so a test
/// about composition offsets needs a stream from somewhere else — and a test
/// about a codec MP4 refuses needs a codec nothing here encodes.
///
/// This is the same program `clipped-media-validation` inspects with, and it is
/// used the same way: as a test tool. Nothing in the recorder shells out to
/// FFmpeg (`docs/ffmpeg.md`).
///
/// # Panics
///
/// When `ffmpeg` fails, with its own diagnostics, because a fixture that was not
/// built means the test after it is asserting on nothing.
pub(crate) fn build_fixture_with_ffmpeg(ffmpeg: &Path, arguments: &[&str]) {
    let output = Command::new(ffmpeg)
        // `-nostdin` because `ffmpeg` otherwise reads the console, and a test
        // harness's console is not something it should be reading.
        .arg("-nostdin")
        .args(arguments)
        .output()
        .expect("the pinned ffmpeg can be run");

    assert!(
        output.status.success(),
        "the fixture could not be built with `ffmpeg {}`: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
