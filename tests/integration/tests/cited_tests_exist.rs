//! Every test this repository's documentation names must actually exist.
//!
//! # Why this is worth a test of its own
//!
//! A comment saying "`crates/library/tests/reconciliation.rs` checks that
//! re-indexing keeps what the user did" is doing real work: it is how a reader
//! decides the property is covered, and how a reviewer decides a change is
//! safe. When the file is not there, that sentence is worse than silence —
//! it stops somebody looking for the test they would otherwise have written.
//!
//! It has happened twice, and neither was noticed by anything:
//!
//! - `SUPPORTED_SCHEMA_VERSION` cited
//!   `crates/library/tests/sidecars.rs::the_documented_sidecar_is_the_one_this_build_reads`
//!   as what kept the sidecar reader and writer at the same version. No test of
//!   that name had ever existed, and the drift it named would have shipped a
//!   recorder whose sidecars its own library refused — every session, silently.
//! - `ingest` and `presence` both cited
//!   `crates/library/tests/reconciliation.rs` as what checked that re-indexing
//!   never clears a favourite. No file of that name had ever existed either,
//!   and that property fails without anything failing: a favourite set last
//!   week is simply gone after the next scan.
//!
//! Both were found by going to *run* the named test. Nothing else would have
//! found them, which is what this file is for.
//!
//! # What it checks, and what it deliberately does not
//!
//! Two exact forms, both mechanical:
//!
//! 1. A path to a test file — `crates/<crate>/tests/<file>.rs` — names a file
//!    that is there.
//! 2. A path with a test named on it — `…rs::<name>` — additionally names a
//!    `fn <name>` inside that file.
//!
//! It does **not** try to resolve a bare backticked identifier in prose. That
//! would need to tell a test name from a function, a type or a constant, and a
//! guard with false positives is one people learn to route around. The two
//! forms above are the ones used when a comment means to point at a test, and
//! they are the ones that have gone stale.

use std::collections::BTreeSet;
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

/// Every `.rs` and `.md` file worth reading, skipping build output and
/// anything a tool put there.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            if path.is_dir() {
                // `target` is build output, `.git` is history, `node_modules`
                // is somebody else's code, and `.claude` is a working
                // directory rather than part of the repository.
                if matches!(
                    name.as_ref(),
                    "target" | ".git" | "node_modules" | ".claude"
                ) {
                    continue;
                }
                pending.push(path);
            } else if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("rs" | "md")
            ) {
                found.push(path);
            }
        }
    }

    found
}

/// Whether `source` is this file.
///
/// Skipped, and only this file is: it has to spell the pattern out to explain
/// it, and every citation it contains is an example rather than a claim. A
/// guard that failed on its own documentation would be switched off within a
/// week.
fn is_this_file(source: &Path) -> bool {
    source.file_name().and_then(|name| name.to_str()) == Some("cited_tests_exist.rs")
}

/// Every citation of the form `crates/<crate>/tests/<file>.rs[::<name>]`.
///
/// Returned as `(path, test name)` so that the two checks below can share one
/// pass over the repository.
fn citations(text: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("crates/") {
        rest = &rest[start..];
        let Some(end) = rest.find(".rs") else {
            break;
        };
        let path = &rest[..end + 3];
        rest = &rest[end + 3..];

        // `crates/<crate>/tests/<file>.rs` and nothing else: a citation of a
        // source file is not a claim that a test exists.
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() != 4 || segments[2] != "tests" {
            continue;
        }
        if path.contains(char::is_whitespace) {
            continue;
        }

        let named = rest.strip_prefix("::").map(|after| {
            after
                .chars()
                .take_while(|character| character.is_ascii_lowercase() || *character == '_')
                .collect::<String>()
        });

        found.push((path.to_owned(), named.filter(|name| !name.is_empty())));
    }

    found
}

#[test]
fn every_test_file_this_repository_names_is_there() {
    let root = repository();
    let mut missing = BTreeSet::new();

    for source in sources(&root) {
        if is_this_file(&source) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&source) else {
            continue;
        };
        for (path, _) in citations(&text) {
            if !root.join(&path).is_file() {
                let cited_in = source
                    .strip_prefix(&root)
                    .unwrap_or(&source)
                    .display()
                    .to_string()
                    .replace('\\', "/");
                missing.insert(format!("{path} (named in {cited_in})"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "these test files are named as covering something and do not exist, so whatever they \
         were said to check is unchecked:\n  {}",
        missing.into_iter().collect::<Vec<_>>().join("\n  ")
    );
}

#[test]
fn every_test_this_repository_names_is_in_the_file_it_is_named_in() {
    let root = repository();
    let mut missing = BTreeSet::new();

    for source in sources(&root) {
        if is_this_file(&source) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&source) else {
            continue;
        };
        for (path, name) in citations(&text) {
            let Some(name) = name else {
                continue;
            };
            let Ok(file) = fs::read_to_string(root.join(&path)) else {
                // The file itself is missing, which the test above reports. One
                // failure per fault is worth more than two.
                continue;
            };
            if !file.contains(&format!("fn {name}(")) {
                let cited_in = source
                    .strip_prefix(&root)
                    .unwrap_or(&source)
                    .display()
                    .to_string()
                    .replace('\\', "/");
                missing.insert(format!("{path}::{name} (named in {cited_in})"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "these tests are named as covering something and are not in the file they are named in:\
         \n  {}",
        missing.into_iter().collect::<Vec<_>>().join("\n  ")
    );
}

#[cfg(test)]
mod tests {
    use super::citations;

    #[test]
    fn a_citation_is_read_with_and_without_a_test_name() {
        let found = citations(
            "see `crates/library/tests/sidecars.rs` and \
             crates/muxer/tests/mkv_writing.rs::a_track_is_written for the rest",
        );

        assert_eq!(
            found,
            vec![
                ("crates/library/tests/sidecars.rs".to_owned(), None),
                (
                    "crates/muxer/tests/mkv_writing.rs".to_owned(),
                    Some("a_track_is_written".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn a_source_file_is_not_a_claim_that_a_test_exists() {
        // Only `crates/<crate>/tests/<file>.rs` is a citation. Naming a module
        // is how most of this repository's comments point at code, and reading
        // one as a promise of a test would make this guard cry wolf until
        // somebody switched it off.
        assert!(
            citations("`crates/audio/src/windows/endpoint_capture.rs` does the work").is_empty()
        );
        assert!(citations("`crates/library/src/index/ingest.rs`").is_empty());
    }
}
