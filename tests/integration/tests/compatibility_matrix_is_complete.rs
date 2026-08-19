//! `docs/compatibility.md` is a matrix, and a matrix with a missing row is
//! worse than one that admits it is short.
//!
//! [Issue #96](https://github.com/wildware-uk/clipped/issues/96) asks for a
//! published compatibility matrix. The value of one is entirely in whether a
//! reader can trust that the things it does not mention do not exist — so a
//! capture backend, an encoder family or a system test that landed after the
//! page was written, and has no row on it, is the failure this file is for.
//!
//! # Why the list is read out of the source
//!
//! Because a list written twice is a list that drifts, and this repository has
//! four tests of exactly this shape for exactly that reason:
//! `tests/integration/tests/process_table_reads.rs`,
//! `tests/integration/tests/disk_space_reads.rs`,
//! `tests/integration/tests/foreground_rules.rs` and
//! `tests/integration/tests/settings_reach_the_running_recorder.rs`. Each
//! derives what it checks from the code as it is now, rather than from a copy
//! of it kept beside the assertion, so that adding the thing is what makes the
//! test notice.
//!
//! The labels are read out of the two `Display` implementations rather than the
//! enums, deliberately: those are the strings a user sees in the window, in the
//! log and in `clipped-recorder capabilities`, so they are the words a reader
//! of the matrix will search it for. A variant renamed without its label
//! changing does not move the matrix, and should not fail this.
//!
//! # What this deliberately does not check
//!
//! That a row is **true**, or that it is still true. Nothing can: a reading
//! taken on one machine on one afternoon can stop being right without any file
//! in this repository changing, which is why the page says which machine and
//! which day for every measured cell and says **unknown** for the rest.
//!
//! Nor that a test named on the page exists —
//! `tests/integration/tests/cited_tests_exist.rs` already does that, for every
//! `.md` in the repository, and doing it twice would be two failures for one
//! fault. The two run in opposite directions and meet in the middle: that one
//! says every test the page names is real, this one says every real test is on
//! the page.

use std::fs;
use std::path::{Path, PathBuf};

/// The repository root, found from this crate's manifest.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("the repository root resolves")
}

/// The matrix itself.
fn matrix(root: &Path) -> String {
    let path = root.join("docs/compatibility.md");
    fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{} is the compatibility matrix issue #96 asks to be published, and it is not there: \
             {error}",
            path.display(),
        )
    })
}

/// Every string literal a `impl fmt::Display for {type_name}` writes out, in
/// source order.
///
/// The block is found by name and then walked brace by brace, so a match arm
/// added below the ones already there is picked up wherever in the block it
/// sits. It fails loudly rather than answering an empty list: a `Display`
/// implementation that has been moved or reshaped is exactly the case where a
/// silent pass would be worst, because the matrix would then be free to fall
/// behind the code with this test still green (AGENTS.md section 54).
fn displayed_labels(file: &Path, type_name: &str) -> Vec<String> {
    let source = fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("{} is part of this repository: {error}", file.display()));

    let declaration = format!("impl fmt::Display for {type_name} {{");
    let start = source.find(&declaration).unwrap_or_else(|| {
        panic!(
            "{} no longer writes `{declaration}`. That block is where the labels the compatibility \
             matrix is read against come from, so moving or reshaping it means deciding what keeps \
             docs/compatibility.md in step instead (issue #96)",
            file.display(),
        )
    }) + declaration.len()
        - 1;

    let block = &source[start..start + block_length(&source[start..], file, type_name)];

    let labels: Vec<String> = block
        .match_indices("=> \"")
        .filter_map(|(at, matched)| {
            let rest = &block[at + matched.len()..];
            rest.find('"').map(|end| rest[..end].to_owned())
        })
        .collect();

    assert!(
        !labels.is_empty(),
        "`impl fmt::Display for {type_name}` in {} was read as writing no labels at all, which is \
         not what it says",
        file.display(),
    );
    labels
}

/// How many bytes of `source` the brace-delimited block starting at its first
/// character occupies, closing brace included.
fn block_length(source: &str, file: &Path, type_name: &str) -> usize {
    let mut depth = 0_usize;
    for (at, character) in source.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return at + 1;
                }
            }
            _ => {}
        }
    }
    panic!(
        "`impl fmt::Display for {type_name}` in {} is never closed",
        file.display(),
    );
}

/// The system test files under a directory: those that hold a `#[test]`.
///
/// Read from the filesystem rather than from a list, so that a new one is
/// noticed by existing. The `#[test]` condition is what tells a test apart from
/// a helper sharing the directory — `tests/capture/readback.rs` is the readback
/// helper the others use and is not a test, and `docs/testing.md` says so.
fn system_tests(root: &Path, directory: &str) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(root.join(directory))
        .unwrap_or_else(|error| panic!("{directory} is part of this repository: {error}"))
        .map(|entry| entry.expect("the directory can be read").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .filter(|path| {
            fs::read_to_string(path)
                .expect("a Rust file in this repository reads as text")
                .contains("#[test]")
        })
        .map(|path| {
            let name = path
                .file_name()
                .expect("a file read from a directory has a name")
                .to_string_lossy();
            format!("{directory}/{name}")
        })
        .collect();

    assert!(
        !found.is_empty(),
        "{directory} holds no test at all, which is not what docs/testing.md describes",
    );
    found.sort();
    found
}

#[test]
fn every_capture_backend_and_encoder_the_code_has_is_in_the_matrix() {
    let root = repository();
    let page = matrix(&root);

    let mut missing = Vec::new();
    for (file, type_name) in [
        ("crates/capture/src/method.rs", "CaptureMethod"),
        ("crates/encoder/src/codec.rs", "EncoderKind"),
    ] {
        for label in displayed_labels(&root.join(file), type_name) {
            if !page.contains(&label) {
                missing.push(format!("{label} (from {type_name} in {file})"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "docs/compatibility.md has no row for {missing:?}. Issue #96's matrix is only worth \
         reading if a reader can take the absence of a backend from it as meaning there is no \
         such backend, so a new capture method or encoder family needs a row — even if the only \
         honest row is that nobody has run it, which is what the Quick Sync and Game Capture rows \
         already are",
    );
}

#[test]
fn every_capture_and_audio_system_test_is_in_the_matrix() {
    let root = repository();
    let page = matrix(&root);

    let missing: Vec<String> = ["tests/capture", "tests/audio"]
        .into_iter()
        .flat_map(|directory| system_tests(&root, directory))
        .filter(|path| !page.contains(path))
        .collect();

    assert!(
        missing.is_empty(),
        "docs/compatibility.md names none of {missing:?}. These are the tests that point a real \
         capture backend or a real recording at a controlled subject on somebody's own machine, \
         and continuous integration has never run one of them and never will — so the matrix is \
         the only place their result is written down. Name the file, and say what a run of it \
         decided",
    );
}
