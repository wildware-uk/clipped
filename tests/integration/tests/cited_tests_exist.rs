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
//! 1. A path to a test file — `crates/<crate>/tests/<file>.rs` for a crate's
//!    own, or `tests/<suite>/…/<file>.rs` for the suites at the repository root
//!    — names a file that is there.
//! 2. A path with a test named on it — `…rs::<name>` — additionally names a
//!    `fn <name>` inside that file.
//!
//! The second kind of path was unchecked until there were forty-odd citations
//! of it, most of them in `docs/`, which is the same way the first kind came to
//! be worth checking.
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

/// Every citation of a test file, in either of the two shapes this repository
/// writes them in.
///
/// - `crates/<crate>/tests/<file>.rs` — a crate's own integration test.
/// - `tests/<anything>/<file>.rs` — the suites that live at the root, which is
///   where the capture, media and workspace tests are. They are cited as often
///   as the first kind, in `docs/` most of all, and were not checked at all
///   until they had grown to forty-odd citations.
///
/// Both may carry `::<name>`. Returned as `(path, test name)` so that the two
/// checks below share one pass over the repository.
fn citations(text: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    for prefix in ["crates/", "tests/"] {
        collect(text, prefix, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

/// Whether a citation starting at `at` really starts there.
///
/// `tests/` occurs inside `crates/library/tests/sidecars.rs`, and reading that
/// as a citation of `tests/sidecars.rs` would fail on a file nobody named. A
/// citation begins at the start of the text or after something that cannot be
/// part of a path.
fn starts_a_path(text: &str, at: usize) -> bool {
    let Some(before) = text[..at].chars().next_back() else {
        return true;
    };
    !(before.is_alphanumeric() || matches!(before, '/' | '\\' | '-' | '_' | '.'))
}

/// Every citation beginning with `prefix`, appended to `found`.
fn collect(text: &str, prefix: &str, found: &mut Vec<(String, Option<String>)>) {
    let mut at = 0;

    while let Some(offset) = text[at..].find(prefix) {
        let start = at + offset;
        at = start + prefix.len();
        if !starts_a_path(text, start) {
            continue;
        }

        let rest = &text[start..];
        let Some(end) = rest.find(".rs") else {
            break;
        };
        let path = &rest[..end + 3];
        let after = &rest[end + 3..];
        at = start + end + 3;

        if !is_a_test_file(path) || path.contains(char::is_whitespace) {
            continue;
        }

        let named = after.strip_prefix("::").map(|after| {
            after
                .chars()
                .take_while(|character| character.is_ascii_lowercase() || *character == '_')
                .collect::<String>()
        });

        found.push((path.to_owned(), named.filter(|name| !name.is_empty())));
    }
}

/// Whether this path names a test file rather than a source file.
///
/// A citation of a module is how most of this repository's comments point at
/// code, and reading one as a promise that a test exists would make this guard
/// cry wolf until somebody switched it off.
fn is_a_test_file(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    match segments.as_slice() {
        // `crates/<crate>/tests/<file>.rs`.
        ["crates", _, "tests", _] => true,
        // `tests/<suite>/…/<file>.rs`, but not `tests/<suite>/src/<file>.rs`,
        // which is a harness's own code rather than a test.
        ["tests", rest @ ..] => rest.len() >= 2 && !rest.contains(&"src"),
        _ => false,
    }
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
    fn a_suite_at_the_repository_root_is_a_citation_too() {
        // Forty-odd of them, in `docs/` most of all, and none was checked until
        // this form was added.
        let found = citations(
            "`tests/capture/screenshot_fullscreen.rs` and              tests/integration/tests/workspace_layering.rs::the_desktop_names_one_crate",
        );

        assert_eq!(
            found,
            vec![
                ("tests/capture/screenshot_fullscreen.rs".to_owned(), None),
                (
                    "tests/integration/tests/workspace_layering.rs".to_owned(),
                    Some("the_desktop_names_one_crate".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn the_tests_directory_inside_a_crate_path_is_not_a_second_citation() {
        // `crates/library/tests/sidecars.rs` contains `tests/sidecars.rs`.
        // Reading that as a citation of its own would fail on a file nobody
        // named, which is the false positive that makes a guard get switched
        // off.
        assert_eq!(
            citations("`crates/library/tests/sidecars.rs`"),
            vec![("crates/library/tests/sidecars.rs".to_owned(), None)]
        );
    }

    #[test]
    fn a_harnesss_own_source_under_tests_is_not_a_test() {
        // `tests/media/src/fixture.rs` is the media harness's code. It is a
        // real file and citing it is fine; it is not a claim that a test of
        // that name exists, and the second check below would look for a
        // function in it.
        assert!(citations("`tests/media/src/fixture.rs` builds the fixtures").is_empty());
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
