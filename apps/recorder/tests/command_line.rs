//! The recorder's behaviour as a program: what it prints, and what it exits
//! with.
//!
//! The parsing rules themselves are unit-tested next to the code that
//! implements them. What is only observable from outside is asserted here: that
//! `--help` really does document the defaults, that a bad argument produces a
//! usage error rather than a panic or a stack trace, and that a `record`
//! invocation which cannot get as far as capturing anything leaves no file and
//! no directory behind.
//!
//! What a *successful* recording produces is `tests/record_end_to_end.rs`,
//! which needs a GPU and a desktop session; nothing in this file starts a
//! capture.
//!
//! One test here starts `watch`, which does not exit on its own, and stops it
//! once it has said it is watching. It is in this file rather than beside the
//! rest of `watch`'s tests because what it is about is only true of the built
//! program: a sentence on the console of whoever started it.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use clap::CommandFactory;
use clipped_recorder::cli::{Cli, TARGET_ARGUMENTS};
use clipped_session::config::FILE_NAME;

/// Exit code for arguments that were rejected. Mirrors
/// `clipped_recorder::EXIT_USAGE`, restated so that the test fails if the value
/// changes rather than following it.
const EXIT_USAGE: i32 = 2;

/// A window title no window on any machine can have, so that a `record`
/// invocation using it fails at target resolution, deterministically, without
/// capturing anything (AGENTS.md section 25).
const NO_SUCH_WINDOW: &str = "no window is called this: clipped-recorder test 5f3a9c";

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

/// The `record` subcommand's arguments, as clap holds them.
///
/// Cloned out because `get_arguments` borrows the `Command` it came from, and
/// the expectations below are built from the same definition the binary was
/// built from rather than from a copy kept in this file.
fn record_arguments() -> Vec<clap::Arg> {
    Cli::command()
        .find_subcommand("record")
        .expect("record is a subcommand")
        .get_arguments()
        .cloned()
        .collect()
}

#[test]
fn help_lists_the_subcommands_that_exist() {
    let output = recorder(&["--help"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let help = stdout(&output);
    for subcommand in ["record", "list-windows", "capabilities"] {
        assert!(help.contains(subcommand), "{subcommand} is missing: {help}");
    }
}

#[test]
fn list_windows_prints_a_table_of_what_can_be_captured() {
    // Nothing here asserts *which* windows are open — that depends on the
    // machine, and `crates/windows/tests/desktop.rs` covers the enumeration
    // itself against windows it creates. What only the built binary can show is
    // that the subcommand runs, exits 0 and prints a table to standard output.
    let output = recorder(&["list-windows"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        stderr(&output)
    );

    let listing = stdout(&output);
    let capturable = capturable_count(&listing);

    // Which branch is right depends on the desktop the runner happens to have,
    // so the test reads the count the command printed and holds it to what it
    // said, rather than assuming a session with a window in it (AGENTS.md
    // section 25). Both branches assert; neither is a way out.
    if capturable == 0 {
        assert!(
            listing.contains("Nothing on this desktop can be captured. Pass --all to see why."),
            "an empty desktop should say so and point at --all: {listing}"
        );
    } else {
        for column in [
            "HANDLE", "PID", "PROCESS", "CLIENT", "DPI", "MONITOR", "TITLE",
        ] {
            assert!(
                listing.contains(column),
                "the {column} column is missing: {listing}"
            );
        }
    }
}

/// The `N` from `N of M top-level windows can be captured.`
///
/// Read from the output rather than assumed, because it is the number that
/// decides whether a table follows.
fn capturable_count(listing: &str) -> usize {
    let headline = listing
        .lines()
        .find(|line| line.contains("top-level windows can be captured."))
        .unwrap_or_else(|| {
            panic!("the listing should say how much of the desktop it is showing: {listing}")
        });

    headline
        .split_whitespace()
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("the headline should start with a count: {headline}"))
}

#[test]
fn list_windows_reports_a_selector_that_matches_nothing_as_a_usage_error() {
    // A title no window can have, so this is deterministic on any machine
    // (AGENTS.md section 25).
    let output = recorder(&[
        "list-windows",
        "--window",
        "no window is called this: clipped-recorder test 5f3a9c",
    ]);

    assert_eq!(output.status.code(), Some(EXIT_USAGE));
    let message = stderr(&output);
    assert!(message.contains("no window matches"), "{message}");
    assert!(
        !message.contains("panicked"),
        "an unmatched selector must not panic: {message}"
    );
}

#[test]
fn list_windows_rejects_a_handle_that_is_not_one_before_enumerating_anything() {
    let output = recorder(&["list-windows", "--handle", "chrome"]);

    assert_eq!(output.status.code(), Some(EXIT_USAGE));
    let message = stderr(&output);
    assert!(
        message.contains("invalid value 'chrome' for '--handle <HANDLE>'"),
        "the rejection should name the argument and the value: {message}"
    );
    assert!(
        message.contains("--help"),
        "a value clap rejected should point at the help: {message}"
    );
}

#[test]
fn capabilities_reports_encoders_and_codecs() {
    // The acceptance criterion for issue #14, checked through the binary
    // because that is where it is claimed: `capabilities` prints what was
    // detected and exits successfully. What it *finds* depends on the machine,
    // so what is asserted is the shape — every encoder family, the codecs, the
    // legend that separates a measurement from an inference, and the standing
    // note that nothing records yet.
    let output = recorder(&["capabilities"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let report = stdout(&output);
    for expected in [
        "Adapters",
        "Encoders",
        "NVIDIA NVENC",
        "AMD AMF",
        "Intel Quick Sync",
        "Software (CPU)",
        "H.264",
        "HEVC",
        "AV1",
        "Automatic would choose",
        "Encoding in this build:",
        // What a recording carries, which is the difference SPEC.md section 46
        // says the product exists for. The binary said the opposite of this for
        // months after it stopped being true, and a test asserting the old
        // string is what held it there (issue #735).
        "a track for each audio source it opened",
    ] {
        assert!(
            report.contains(expected),
            "the report should mention {expected}:\n{report}"
        );
    }
    assert!(
        !report.contains("no audio track"),
        "a recording has carried audio since #180, and this is the command somebody runs to \
         find out what their machine can do:\n{report}"
    );

    // Asserted through the binary because the binary is what a user runs: the
    // software fallback (#18) and NVENC (#15) are implemented, and the shipped
    // report went on saying the opposite of both until #167. A unit test on
    // the string would not have caught that, because the string was the thing
    // that was wrong.
    assert!(
        !report.contains("No encoder is implemented"),
        "this build has encoder backends and must not deny it:\n{report}"
    );
    // The same failure a milestone later: the footer went on saying nothing
    // recorded, which stopped being true when the session landed (#126).
    assert!(
        !report.contains("Nothing records yet"),
        "this build records and must not deny it:\n{report}"
    );
    assert!(
        report.contains("inferred from published limits"),
        "the report must explain which answers were not measured:\n{report}"
    );
}

#[test]
fn capabilities_refreshes_and_the_cache_gives_the_same_answer_back() {
    // The probed and the cached answer have to be the same answer, or the cache
    // is not a cache. Run in this order deliberately: `--refresh` ignores what
    // is stored, probes — opening an encoder session, which is the only run
    // that does (issue #133) — and stores what it found; the plain call then
    // has to read that back unchanged, measurements included.
    //
    // The order used to be the other way round, and cannot be any more: a plain
    // run opens no session, so on a machine whose cache holds an unmeasured
    // report it would print inferred limits where the refresh prints measured
    // ones. Comparing those two would be asserting that measuring changes
    // nothing.
    //
    // The other tests in this file run against the same real cache file at the
    // same time, and that is not a race any more: a run that opens no session
    // never overwrites a stored measurement of the same machine, so no
    // interleaving of theirs can put published limits between these two calls
    // (`clipped_encoder::detect_cached`, and the tests that hold that rule).
    let refreshed = recorder(&["capabilities", "--refresh"]);
    let cached = recorder(&["capabilities"]);
    assert!(refreshed.status.success() && cached.status.success());

    // Everything up to the footer, which differs by design: it says where the
    // answer came from and how long it took.
    let body = |output: &Output| {
        stdout(output)
            .split_once("(i) inferred")
            .expect("the report has a legend")
            .0
            .to_owned()
    };
    assert_eq!(
        body(&refreshed),
        body(&cached),
        "a probed report and the cached copy of it describe the same machine"
    );
    assert!(
        stdout(&refreshed).contains("probed just now"),
        "--refresh must actually probe:\n{}",
        stdout(&refreshed)
    );
    assert!(
        stdout(&cached).contains("read from"),
        "the run after a refresh must be answered by the cache it wrote:\n{}",
        stdout(&cached)
    );
}

#[test]
fn record_help_documents_a_default_for_every_option() {
    // How many options must show a default is read from the command
    // definition, not from a list written here: a twelfth option that
    // documents no default changes the count and fails this test. Which
    // argument is missing one is `cli.rs`'s unit test, which can walk them
    // individually; what only the built binary can show is that both help
    // forms really print them, and that `-h` — what most people type — is not
    // the poor relation.
    let optional_arguments = record_arguments()
        .iter()
        .filter(|argument| {
            let name = argument.get_id().as_str();
            !TARGET_ARGUMENTS.contains(&name) && !matches!(name, "help" | "version")
        })
        .count();
    assert!(optional_arguments > 0, "no optional arguments were found");

    for form in ["-h", "--help"] {
        let output = recorder(&["record", form]);
        assert!(output.status.success(), "{}", stderr(&output));
        let help = stdout(&output);

        for argument in record_arguments() {
            let Some(long) = argument.get_long() else {
                continue;
            };
            assert!(
                help.contains(&format!("--{long}")),
                "`record {form}` does not mention `--{long}`:\n{help}"
            );
        }

        assert_eq!(
            help.matches("[default:").count(),
            optional_arguments,
            "`record {form}` states a default for some but not all of the \
             {optional_arguments} optional arguments:\n{help}"
        );
    }
}

#[test]
fn record_without_an_output_creates_no_recordings_directory() {
    // The default output path is under the user's videos folder, so this run
    // is given a home directory of its own. A run that never gets as far as a
    // frame must not leave a `Videos\Clipped` behind for a recording that never
    // happened: the directory is created by the recording that goes in it.
    let home = TestDirectory::new("default-output-home");

    let output = Command::new(env!("CARGO_BIN_EXE_clipped-recorder"))
        .args(["record", "--window", NO_SUCH_WINDOW])
        .env("USERPROFILE", home.path())
        .env("HOME", home.path())
        .output()
        .expect("the recorder binary can be run");

    assert_eq!(
        output.status.code(),
        Some(EXIT_USAGE),
        "stderr was: {}",
        stderr(&output)
    );

    // The resolved path is logged with its directory redacted away, so the
    // generated file name is the evidence that the default was the one used.
    let message = stderr(&output);
    assert!(
        message.contains("output=clipped-") && message.contains(".mkv#"),
        "the run should have resolved a generated default file name: {message}"
    );

    let left_behind: Vec<_> = fs::read_dir(home.path())
        .expect("the home directory can be listed")
        .map(|entry| entry.expect("the entry can be read").file_name())
        .collect();
    assert!(
        left_behind.is_empty(),
        "validating a run that cannot record created {left_behind:?} under the home directory"
    );
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
fn record_with_a_target_that_matches_no_window_writes_no_recording() {
    // The file is created by the muxer, which is reached only after a window
    // has been resolved, a backend has been chosen and a frame has arrived. A
    // run that fails before any of that must leave the output path untouched:
    // an empty file left behind would make that name permanently unusable,
    // because the next attempt would refuse to overwrite a recording.
    let directory = TestDirectory::new("no-output-written");
    let output_path = directory.path().join("session.mkv");

    let output = recorder(&[
        "record",
        "--window",
        NO_SUCH_WINDOW,
        "--output",
        output_path.to_str().expect("the path is valid UTF-8"),
    ]);

    assert_eq!(
        output.status.code(),
        Some(EXIT_USAGE),
        "stderr was: {}",
        stderr(&output)
    );
    let message = stderr(&output);
    assert!(message.contains("no window matches"), "{message}");
    assert!(
        !message.contains("panicked"),
        "an unmatched selector must not panic: {message}"
    );

    assert!(
        !output_path.exists(),
        "a run that never captured a frame must not create an output file"
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
    // The setting, not the flag: this message is also the `invalid_parameters`
    // refusal a window is shown over IPC, where `--overwrite` is not something
    // anybody can pass (AGENTS.md section 45). The command-line spelling is in
    // `--help`, which the next assertion is about.
    assert!(message.contains("overwrite"), "{message}");
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

/// How long `watch` is given to reach the point where it says it is watching.
///
/// Start-up subscribes to WMI, which is a fraction of a second on an idle
/// machine and longer on a loaded one. Nothing waits for the whole of this in
/// the ordinary case — the wait ends at the announcement — so it is set
/// generously: a tighter timeout buys nothing and costs a flaky test (AGENTS.md
/// section 25).
const WATCH_START_TIMEOUT: Duration = Duration::from_secs(60);

/// The environment variable naming the per-user root Clipped keeps its files
/// under, and the directory it makes inside it — `%LOCALAPPDATA%\Clipped` on
/// Windows.
///
/// `clipped_logging::application_directory` is what the recorder itself calls,
/// and the rule is restated here rather than asked of it because that function
/// reads *this* process's environment. What is under test is a child's, and
/// setting the variable process-wide would be visible to every test running
/// beside this one.
#[cfg(windows)]
const PER_USER_ROOT_VARIABLE: &str = "LOCALAPPDATA";
#[cfg(windows)]
const PER_USER_DIRECTORY: &str = "Clipped";
#[cfg(not(windows))]
const PER_USER_ROOT_VARIABLE: &str = "XDG_STATE_HOME";
#[cfg(not(windows))]
const PER_USER_DIRECTORY: &str = "clipped";

/// Runs `watch` with `home` as Clipped's per-user root until it says it is
/// watching, stops it, and returns everything it wrote to standard error.
///
/// `watch` runs until Ctrl+C, so waiting for it to exit would wait for ever,
/// and killing it after a fixed sleep would be a race. Stopping it at the
/// announcement is neither: the settings file is read during start-up, before
/// `Watching for games.` is printed, so a run that has printed that has already
/// done the part this file is about.
///
/// Standard error is read on a thread of its own. A pipe holds a few kilobytes;
/// a child left to fill one and block would be a deadlock rather than a
/// failure.
fn watch_until_it_is_watching(home: &Path, recordings: &Path) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_clipped-recorder"))
        .args([
            "watch",
            "--output-directory",
            recordings.to_str().expect("the path is valid UTF-8"),
        ])
        .env(PER_USER_ROOT_VARIABLE, home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the recorder binary can be run");

    let pipe = child.stderr.take().expect("standard error was piped");
    let (lines, printed) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { return };
            if lines.send(line).is_err() {
                return;
            }
        }
    });

    let deadline = Instant::now() + WATCH_START_TIMEOUT;
    let mut collected: Vec<String> = Vec::new();
    let mut watching = false;
    while !watching {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match printed.recv_timeout(remaining) {
            Ok(line) => {
                watching = line.starts_with("Watching for games.");
                collected.push(line);
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    reader.join().expect("the reader thread does not panic");
    while let Ok(line) = printed.try_recv() {
        collected.push(line);
    }

    let console = collected.join("\n");
    assert!(
        watching,
        "`watch` never got as far as watching, so nothing about its start-up was \
         exercised:\n{console}"
    );
    console
}

#[test]
fn watch_says_on_the_console_that_a_settings_file_it_cannot_read_was_left_alone() {
    // The other half of this report is the log, and `tests/unreadable_settings.rs`
    // holds that half. This is the half a user who started `watch` in a terminal
    // sees, and only the built program can show it: the sentence is written by
    // `eprintln!`, so nothing that calls `load_configuration` in-process can
    // tell whether it was written at all.
    let home = TestDirectory::new("unreadable-settings-home");
    let settings = home.path().join(PER_USER_DIRECTORY);
    fs::create_dir_all(&settings).expect("the settings directory can be created");
    let unreadable = "{ \"schema_version\": 99, \"this build\": cannot read this";
    fs::write(settings.join(FILE_NAME), unreadable).expect("the settings file can be written");

    let recordings = TestDirectory::new("unreadable-settings-recordings");
    let console = watch_until_it_is_watching(home.path(), recordings.path());

    // The sentence has to *start* a line, which is what separates it from the
    // log record of the same report. That record also reaches standard error on
    // a default configuration, but it begins with a timestamp and carries the
    // sentence as a quoted field — so a `contains` here would pass with the
    // console half deleted, and this does not.
    let sentence = console
        .lines()
        .find(|line| line.starts_with("Settings not applied:"))
        .unwrap_or_else(|| {
            panic!(
                "nothing on the console said the settings file could not be read, so somebody \
                 watching a terminal is told nothing:\n{console}"
            )
        });
    assert!(
        sentence.ends_with("left as it is."),
        "the sentence must reach the console whole, including the part that says the file was \
         not written over: {sentence}"
    );

    // What the sentence promises, checked rather than taken on trust.
    assert_eq!(
        fs::read_to_string(settings.join(FILE_NAME)).expect("the settings file is still there"),
        unreadable,
        "a file the recorder said it had left alone must be exactly as it was found"
    );

    // And a run with nothing wrong says none of it. Without this, the assertion
    // above would also pass if the sentence were printed unconditionally.
    let quiet_home = TestDirectory::new("no-settings-home");
    let quiet_recordings = TestDirectory::new("no-settings-recordings");
    let quiet = watch_until_it_is_watching(quiet_home.path(), quiet_recordings.path());
    assert!(
        !quiet.contains("Settings not applied"),
        "a machine with no settings file has nothing wrong with it:\n{quiet}"
    );
}
