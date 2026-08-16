//! `clipped-recorder recover`, run as a program against a real directory.
//!
//! The rules — which entries count as interrupted, what a rewrite preserves,
//! what discarding records — are unit-tested inside
//! `clipped_session::automatic::recovery`, next to the code that implements
//! them. What is only observable from outside is here: that the subcommand
//! exists, that it prints what somebody can act on, that listing changes
//! nothing, and that the destructive path refuses to run without being told
//! which recording it is destroying.
//!
//! Nothing in this file records anything. The interrupted state is constructed
//! on disk — a session record whose one recording began and never ended, and a
//! file beside it — which is exactly what
//! `crates/muxer/tests/abrupt_termination.rs` measures a killed recorder
//! leaving behind, minus the several seconds of real capture it needs a GPU
//! for.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// Exit code for arguments that were rejected. Mirrors
/// `clipped_recorder::EXIT_USAGE`, restated so that the test fails if the value
/// changes rather than following it.
const EXIT_USAGE: i32 = 2;

/// The session the fixtures below are of.
const SESSION: &str = "counter-strike-2-20260811-143205";

/// A directory of this test's own, removed when it is dropped.
#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-recover-{label}-{}-{:?}",
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
        // The trash is a *sibling* of the recordings directory, not a child of
        // it, so removing the directory does not take it. Without this, every
        // run of the discard test leaves a copy of its fixture in the
        // machine's temporary directory for good.
        let _ = fs::remove_dir_all(trash_beside(&self.0));
    }
}

/// Where the trash goes when the settings file names no directory: beside the
/// recordings, same name with `.trash` appended.
///
/// Restated here rather than imported from `clipped_session::config`, for the
/// reason [`EXIT_USAGE`] is: this test should fail if that rule changes, not
/// quietly follow it somewhere else.
fn trash_beside(recordings: &Path) -> PathBuf {
    let mut name = recordings.as_os_str().to_os_string();
    name.push(".trash");
    PathBuf::from(name)
}

/// The one file anywhere under `root`, or [`None`] if `root` holds none.
///
/// # Panics
///
/// If there is more than one, which would mean a discard left something behind
/// as well as what it moved.
fn only_file_under(root: &Path) -> Option<PathBuf> {
    fn collect(directory: &Path, into: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, into);
            } else {
                into.push(path);
            }
        }
    }

    let mut found = Vec::new();
    collect(root, &mut found);
    assert!(
        found.len() <= 1,
        "expected at most one file under {}, found {found:?}",
        root.display()
    );
    found.pop()
}

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

/// Writes what a killed recorder leaves: a file, and a session record whose
/// entry for it has no end.
fn interrupted(directory: &Path) -> PathBuf {
    let recording = directory.join(format!("clipped-{SESSION}.mkv"));
    fs::write(&recording, vec![0u8; 8192]).expect("the recording can be written");

    let file = serde_json::json!({
        "schema_version": 1,
        "session_id": SESSION,
        "game": { "kind": "known", "game_id": "counter-strike-2", "name": "Counter-Strike 2" },
        "started_at": "2026-08-11T14:32:05+01:00",
        "ended_at": null,
        "recordings": [{
            "index": 1,
            "output": recording.display().to_string(),
            "started_at": "2026-08-11T14:32:05+01:00",
            "ended_at": null,
            "outcome": null
        }],
        "clips": [],
        "bookmarks": [],
        "events": []
    });
    fs::write(
        directory.join(format!("clipped-{SESSION}.session.json")),
        serde_json::to_vec_pretty(&file).expect("the shape encodes"),
    )
    .expect("the session record can be written");

    recording
}

/// The session record, as it stands now.
fn record(directory: &Path) -> Value {
    let text = fs::read_to_string(directory.join(format!("clipped-{SESSION}.session.json")))
        .expect("the session record can be read");
    serde_json::from_str(&text).expect("the session record is JSON")
}

#[test]
fn recover_names_the_footage_and_where_it_is() {
    let directory = TestDirectory::new("list");
    let recording = interrupted(directory.path());

    let output = recorder(&[
        "recover",
        "--directory",
        &directory.path().display().to_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let said = stderr(&output);
    assert!(said.contains(SESSION), "{said}");
    assert!(said.contains("Counter-Strike 2"), "{said}");
    assert!(
        said.contains(&recording.display().to_string()),
        "the file has to be named, or there is nothing to act on: {said}"
    );
    assert!(said.contains("8.0 KiB"), "{said}");
    assert!(
        stdout(&output).is_empty(),
        "standard output is a command's result and this one produces files"
    );
}

#[test]
fn listing_changes_nothing_at_all() {
    // The default has to be the safe one. Somebody typing `recover` to find out
    // where their recording went must not have anything happen to it, and the
    // record must still offer it afterwards.
    let directory = TestDirectory::new("list-is-safe");
    let recording = interrupted(directory.path());
    let before = record(directory.path());

    let output = recorder(&[
        "recover",
        "--directory",
        &directory.path().display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    assert!(recording.exists(), "listing must not touch the footage");
    assert_eq!(
        record(directory.path()),
        before,
        "listing must not touch the session record"
    );
}

#[test]
fn adopting_keeps_the_footage_and_stops_it_being_offered_again() {
    let directory = TestDirectory::new("adopt");
    let recording = interrupted(directory.path());

    let adopted = recorder(&[
        "recover",
        "--directory",
        &directory.path().display().to_string(),
        "--adopt",
    ]);
    assert_eq!(adopted.status.code(), Some(0), "{}", stderr(&adopted));
    assert!(stderr(&adopted).contains("Kept"), "{}", stderr(&adopted));
    assert!(recording.exists(), "adopting must never touch the footage");
    assert_eq!(
        record(directory.path())["recordings"][0]["outcome"],
        Value::from("interrupted")
    );

    let again = recorder(&[
        "recover",
        "--directory",
        &directory.path().display().to_string(),
    ]);
    assert_eq!(again.status.code(), Some(0), "{}", stderr(&again));
    assert!(
        stderr(&again).contains("Nothing to recover"),
        "an adopted recording must not be offered on the next launch: {}",
        stderr(&again)
    );
}

#[test]
fn discarding_without_naming_a_session_is_refused_before_anything_is_deleted() {
    // The failure this rules out is somebody typing `recover --discard`,
    // meaning "throw away that one", and losing every interrupted recording on
    // the machine (AGENTS.md section 56).
    let directory = TestDirectory::new("bulk-discard");
    let recording = interrupted(directory.path());

    let output = recorder(&[
        "recover",
        "--directory",
        &directory.path().display().to_string(),
        "--discard",
    ]);

    assert_eq!(
        output.status.code(),
        Some(EXIT_USAGE),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("--session"),
        "the refusal should say what to type: {}",
        stderr(&output)
    );
    assert!(recording.exists(), "nothing should have been deleted");
}

/// `--discard` used to unlink the file, and this test used to assert that the
/// space came back ("8.0 KiB freed"). Issue #451 changed it to a move into the
/// trash, which means the bytes are still on the disk and that promise would
/// now be a lie — so what is asserted here is the thing the change was made
/// for: the footage is recoverable, byte for byte, by somebody who typed
/// `--discard` and then wished they had not (AGENTS.md section 56).
#[test]
fn discarding_a_named_recording_moves_it_to_the_trash_and_records_that_it_did() {
    let directory = TestDirectory::new("discard");
    let recording = interrupted(directory.path());
    let original = fs::read(&recording).expect("the fixture was just written");

    let output = recorder(&[
        "recover",
        "--directory",
        &directory.path().display().to_string(),
        "--discard",
        "--session",
        SESSION,
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        !recording.exists(),
        "the file should be gone from the recording directory"
    );

    // Gone from where it was, and still there to be had back.
    let stowed = only_file_under(&trash_beside(directory.path())).unwrap_or_else(|| {
        panic!(
            "--discard should have left the recording in the trash: {}",
            stderr(&output)
        )
    });
    assert_eq!(
        fs::read(&stowed).expect("the trashed file can be read"),
        original,
        "the trashed file is not the recording that was discarded",
    );

    // And the message has to name where it went, because nothing else will:
    // this file has no library row, so no trash screen lists it.
    let message = stderr(&output);
    assert!(
        message.contains(&stowed.display().to_string()),
        "the message should say where the file is now: {message}",
    );
    assert!(
        !message.contains("freed"),
        "nothing was freed - the bytes are still on the disk: {message}",
    );

    // The entry stays, saying what happened. A gap is not a record.
    let after = record(directory.path());
    assert_eq!(
        after["recordings"][0]["outcome"],
        Value::from("discarded"),
        "{after}"
    );
    assert_eq!(
        after["recordings"][0]["output"],
        Value::from(recording.display().to_string()),
        "{after}"
    );
}

#[test]
fn a_directory_with_nothing_to_recover_says_so_and_succeeds() {
    let directory = TestDirectory::new("empty");

    let output = recorder(&[
        "recover",
        "--directory",
        &directory.path().display().to_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("Nothing to recover"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_session_that_is_not_waiting_to_be_recovered_is_a_command_line_to_fix() {
    let directory = TestDirectory::new("no-such-session");
    interrupted(directory.path());

    let output = recorder(&[
        "recover",
        "--directory",
        &directory.path().display().to_string(),
        "--session",
        "half-life-3-20260811-143205",
    ]);

    assert_eq!(
        output.status.code(),
        Some(EXIT_USAGE),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("half-life-3-20260811-143205"),
        "the refusal should name what was asked for: {}",
        stderr(&output)
    );
}

#[test]
fn recover_is_offered_in_the_help() {
    let output = recorder(&["--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("recover"), "{}", stdout(&output));
}
