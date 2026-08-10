//! Shared machinery for the muxer's media tests: temporary files, and taking a
//! finished recording apart with `ffprobe`.
//!
//! AGENTS.md section 22 asks that generated media be validated rather than
//! assumed valid, and `ffprobe` is the tool named for it. Everything here is a
//! thin wrapper over running it: what is being tested is the file on disk, so
//! the assertions are about what an outside tool says the file contains, not
//! about what the code that wrote it believed.
//!
//! # Which `ffprobe`
//!
//! The one from the pinned FFmpeg build, in `FFMPEG_DIR/bin`, rather than
//! whichever is on `PATH`. The pinned build is fetched by
//! `scripts/fetch-ffmpeg.ps1` on every machine that builds this workspace,
//! including CI, so the tests run everywhere the crate compiles; a `PATH`
//! lookup would make them pass, fail or silently skip depending on what
//! somebody happened to install. It is still only a test tool — nothing in the
//! recorder shells out to it (`docs/ffmpeg.md`).

// Each test binary compiles this module separately and uses the part of it that
// it needs, so anything used by only one of them is "unused" in the others.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// The directory the pinned FFmpeg build was installed into.
///
/// Read at compile time from the variable the workspace's `.cargo/config.toml`
/// sets, which is the same one `crates/muxer/build.rs` uses to find the DLLs.
/// A build that cannot see it cannot have linked either, so a missing variable
/// is a build error rather than a skipped test.
const FFMPEG_DIR: &str = env!("FFMPEG_DIR");

/// The `ffprobe` from the pinned build.
pub(crate) fn ffprobe() -> PathBuf {
    Path::new(FFMPEG_DIR).join("bin").join("ffprobe.exe")
}

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

/// A directory under the system temporary directory, removed when dropped.
///
/// Recordings are large and tests are run repeatedly, so nothing is left
/// behind — including when a test fails, since `Drop` runs while the panic
/// unwinds.
#[derive(Debug)]
pub(crate) struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    /// Creates a directory named after this process, so that two test binaries
    /// running at once cannot collide.
    pub(crate) fn new(purpose: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "clipped-muxer-{purpose}-{}-{}-{unique}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("a temporary directory can be created");
        Self(path)
    }

    /// A path inside the directory. Nothing is created.
    pub(crate) fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What `ffprobe` says a file contains.
#[derive(Debug)]
pub(crate) struct Probe {
    /// The `format` object.
    pub(crate) format: Value,
    /// The `streams` array.
    pub(crate) streams: Vec<Value>,
    /// Whatever `ffprobe` wrote to standard error, which is where it reports a
    /// file it could not read.
    pub(crate) diagnostics: String,
}

impl Probe {
    /// Runs `ffprobe` over `path`, decoding every frame.
    ///
    /// `-count_frames` makes this a decode rather than a parse: `nb_read_frames`
    /// is how many frames actually came out of the decoder, which is the
    /// difference between "the container lists a video stream" and "the video
    /// plays".
    pub(crate) fn of(path: &Path) -> Self {
        Self::run(path, &["-count_frames", "-count_packets"])
    }

    fn run(path: &Path, extra: &[&str]) -> Self {
        let output = Command::new(ffprobe())
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-show_format",
                "-show_streams",
            ])
            .args(extra)
            .args(["-of", "json"])
            .arg(path)
            .output()
            .expect("the pinned ffprobe can be run");

        let diagnostics = String::from_utf8_lossy(&output.stderr).into_owned();
        let parsed: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);

        Self {
            format: parsed
                .get("format")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new())),
            streams: parsed
                .get("streams")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            diagnostics,
        }
    }

    /// The streams of one kind, in the order the file declares them.
    pub(crate) fn streams_of(&self, codec_type: &str) -> Vec<&Value> {
        self.streams
            .iter()
            .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some(codec_type))
            .collect()
    }

    /// The container's duration in seconds, when it records one.
    ///
    /// Matroska stores the duration in the segment header, which is written
    /// when the file is finished, so an interrupted recording has none and this
    /// returns [`None`].
    pub(crate) fn duration_seconds(&self) -> Option<f64> {
        self.format
            .get("duration")
            .and_then(Value::as_str)
            .and_then(|duration| duration.parse().ok())
    }
}

/// A stream's string field, which is how `ffprobe` reports most of them.
pub(crate) fn field<'a>(stream: &'a Value, name: &str) -> Option<&'a str> {
    stream.get(name).and_then(Value::as_str)
}

/// A stream's numeric field, whether `ffprobe` quoted it or not.
pub(crate) fn number(stream: &Value, name: &str) -> Option<f64> {
    match stream.get(name) {
        Some(Value::Number(value)) => value.as_f64(),
        Some(Value::String(value)) => value.parse().ok(),
        _ => None,
    }
}

/// A track's `title` tag, which is Matroska's `Name` element.
pub(crate) fn tag<'a>(stream: &'a Value, name: &str) -> Option<&'a str> {
    stream.get("tags")?.get(name)?.as_str()
}

/// Whether the track carries Matroska's `FlagDefault`.
pub(crate) fn is_default(stream: &Value) -> bool {
    stream
        .get("disposition")
        .and_then(|disposition| disposition.get("default"))
        .and_then(Value::as_i64)
        == Some(1)
}

/// One packet, as `ffprobe` reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ProbedPacket {
    /// Which stream it belongs to.
    pub(crate) stream_index: i64,
    /// Its decode timestamp, in seconds.
    pub(crate) decode_seconds: f64,
    /// Its presentation timestamp, in seconds.
    pub(crate) presentation_seconds: f64,
}

/// Reads every packet in the file, in the order they are stored.
pub(crate) fn packets(path: &Path) -> Vec<ProbedPacket> {
    let output = Command::new(ffprobe())
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-show_packets",
            "-show_entries",
            "packet=stream_index,pts_time,dts_time",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .expect("the pinned ffprobe can be run");

    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    parsed
        .get("packets")
        .and_then(Value::as_array)
        .map(|packets| {
            packets
                .iter()
                .filter_map(|packet| {
                    Some(ProbedPacket {
                        stream_index: packet.get("stream_index")?.as_i64()?,
                        decode_seconds: number(packet, "dts_time")?,
                        presentation_seconds: number(packet, "pts_time")?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Asserts that every stream's decode timestamps strictly increase.
///
/// The container's own requirement, and the acceptance criterion of
/// [issue #21](https://github.com/wildware-uk/clipped/issues/21). Checked per
/// stream, because packets from different tracks interleave and their
/// timestamps cross constantly — a whole-file check would be asserting
/// something that is not true of any correct recording.
pub(crate) fn assert_decode_timestamps_increase(packets: &[ProbedPacket]) {
    let mut last: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();

    for packet in packets {
        if let Some(previous) = last.insert(packet.stream_index, packet.decode_seconds) {
            assert!(
                packet.decode_seconds > previous,
                "stream {} has a packet at {}s after one at {}s",
                packet.stream_index,
                packet.decode_seconds,
                previous
            );
        }
    }

    assert!(!last.is_empty(), "the file contains no packets at all");
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
