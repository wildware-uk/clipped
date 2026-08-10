//! The recorder's behaviour as a program: what it prints, and what it exits
//! with.
//!
//! The parsing rules themselves are unit-tested next to the code that
//! implements them. What is only observable from outside is asserted here: that
//! `--help` really does document the defaults, that a bad argument produces a
//! usage error rather than a panic or a stack trace, and — most importantly —
//! that `record` does not produce a file, because it cannot record.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Exit code for arguments that were rejected. Mirrors
/// `clipped_recorder::EXIT_USAGE`, restated so that the test fails if the value
/// changes rather than following it.
const EXIT_USAGE: i32 = 2;

/// Exit code for "not implemented in this build". Mirrors
/// `clipped_recorder::EXIT_NOT_IMPLEMENTED`.
const EXIT_NOT_IMPLEMENTED: i32 = 3;

/// A directory of this test's own, removed when it is dropped.
#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-recorder-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory can be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs the recorder with the given arguments.
fn recorder(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clipped-recorder"))
        .args(arguments)
        .output()
        .expect("the recorder binary can be run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_lists_the_subcommands_that_exist() {
    let output = recorder(&["--help"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let help = stdout(&output);
    assert!(help.contains("record"), "{help}");
    // The subcommands that do not exist yet must not appear: a command listed
    // in the help is a promise (AGENTS.md section 27).
    assert!(
        !help.contains("list-windows") && !help.contains("capabilities"),
        "help advertises a subcommand that has not been written: {help}"
    );
}

#[test]
fn record_help_documents_a_default_for_every_option() {
    let output = recorder(&["record", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let help = stdout(&output);
    for expected in [
        "--window <TITLE>",
        "--process <NAME>",
        "--pid <PID>",
        "-o, --output <PATH>",
        "--overwrite",
        "-r, --resolution <WIDTHxHEIGHT>",
        "-f, --framerate <FPS>",
        "--codec <CODEC>",
        "--encoder <ENCODER>",
        "--microphone <DEVICE>",
        "--system-audio <DEVICE>",
    ] {
        assert!(help.contains(expected), "`{expected}` is missing:\n{help}");
    }
    for expected in [
        "[default: source]",
        "[default: 60]",
        "[default: auto]",
        "[default: default]",
        "[default: off",
        "[default: a timestamped file",
    ] {
        assert!(
            help.contains(expected),
            "`{expected}` is missing from the help:\n{help}"
        );
    }
}

#[test]
fn running_with_no_arguments_shows_the_help_rather_than_doing_something() {
    let output = recorder(&[]);
    assert_eq!(output.status.code(), Some(EXIT_USAGE));
    assert!(stderr(&output).contains("Usage:"), "{}", stderr(&output));
}

#[test]
fn a_missing_target_names_all_three_ways_of_giving_one() {
    let output = recorder(&["record"]);
    assert_eq!(output.status.code(), Some(EXIT_USAGE));

    let message = stderr(&output);
    for expected in ["--window <TITLE>", "--process <NAME>", "--pid <PID>"] {
        assert!(message.contains(expected), "{message}");
    }
}

#[test]
fn an_invalid_value_is_a_usage_error_and_not_a_panic() {
    let output = recorder(&["record", "--window", "cs2", "--framerate", "6000"]);
    assert_eq!(output.status.code(), Some(EXIT_USAGE));

    let message = stderr(&output);
    assert!(message.contains("1-480"), "{message}");
    assert!(
        !message.contains("panicked") && !message.contains("RUST_BACKTRACE"),
        "an invalid argument must not panic: {message}"
    );
}

#[test]
fn record_says_it_cannot_record_and_writes_nothing() {
    let directory = TestDirectory::new("no-output-written");
    let output_path = directory.path().join("session.mkv");

    let output = recorder(&[
        "record",
        "--window",
        "Counter-Strike 2",
        "--output",
        output_path.to_str().expect("the path is valid UTF-8"),
    ]);

    assert_eq!(
        output.status.code(),
        Some(EXIT_NOT_IMPLEMENTED),
        "stderr was: {}",
        stderr(&output)
    );
    let message = stderr(&output);
    assert!(
        message.contains("capture engine is not implemented yet"),
        "{message}"
    );
    assert!(message.contains("M1 - Recording Engine"), "{message}");

    assert!(
        !output_path.exists(),
        "a build that cannot record must not create an output file"
    );
    let left_behind: Vec<_> = fs::read_dir(directory.path())
        .expect("the directory can be listed")
        .map(|entry| entry.expect("the entry can be read").file_name())
        .collect();
    assert!(
        left_behind.is_empty(),
        "the run left files behind: {left_behind:?}"
    );
}

#[test]
fn an_existing_recording_is_not_overwritten_without_being_asked() {
    let directory = TestDirectory::new("existing-recording");
    let output_path = directory.path().join("session.mkv");
    fs::write(&output_path, b"an earlier recording").expect("the file can be created");

    let output = recorder(&[
        "record",
        "--window",
        "cs2",
        "--output",
        output_path.to_str().expect("the path is valid UTF-8"),
    ]);

    assert_eq!(output.status.code(), Some(EXIT_USAGE));
    let message = stderr(&output);
    assert!(message.contains("already exists"), "{message}");
    assert!(message.contains("--overwrite"), "{message}");
    assert!(
        message.contains("try 'clipped-recorder record --help'"),
        "a usage error should point at the help: {message}"
    );

    assert_eq!(
        fs::read(&output_path).expect("the file is still there"),
        b"an earlier recording",
        "the existing recording must be untouched"
    );
}

#[test]
fn a_missing_output_directory_is_reported_before_anything_else_happens() {
    let directory = TestDirectory::new("missing-directory");
    let output_path = directory.path().join("nope").join("session.mkv");

    let output = recorder(&[
        "record",
        "--window",
        "cs2",
        "--output",
        output_path.to_str().expect("the path is valid UTF-8"),
    ]);

    assert_eq!(output.status.code(), Some(EXIT_USAGE));
    assert!(
        stderr(&output).contains("does not exist"),
        "{}",
        stderr(&output)
    );
    assert!(
        !output_path
            .parent()
            .expect("the path has a parent")
            .exists(),
        "validation must not create the directory it is complaining about"
    );
}

#[test]
fn the_version_is_reported_on_standard_output() {
    let output = recorder(&["--version"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains(env!("CARGO_PKG_VERSION")),
        "{}",
        stdout(&output)
    );
}
