//! Every architecture decision record has a number of its own.
//!
//! # Why this test exists
//!
//! An ADR's number is a registry, and two branches can claim the same entry at
//! once. Git reports nothing when they do, because the *filenames* differ —
//! `0015-capture-holds-the-display-awake.md` and
//! `0015-derived-pictures-cross-the-control-protocol.md` merge cleanly and
//! leave the index carrying two rows numbered 0015.
//!
//! That happened four times in two days: 0010, 0011 and 0015 were each claimed
//! twice, and one branch was cut from a checkout whose index stopped at 0009
//! and picked a number six behind the tree. Each was caught by a person reading
//! a merge, which is not a mechanism.
//!
//! It is the same shape as a duplicate migration version, and
//! `crates/storage/src/migrations.rs` already has a test for that one. This is
//! its counterpart for the documents.
//!
//! # What it does not check
//!
//! That the numbers have no holes. A number can be skipped — a rejected ADR, or
//! one abandoned before it landed — and a gap costs a reader nothing. Two
//! documents answering to one number costs them the ability to cite either.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The directory the records live in.
fn records() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/tests/integration.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the integration tests are two directories below the root")
        .join("docs/adr")
}

/// Every record, by the number its filename begins with.
fn by_number() -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for entry in std::fs::read_dir(records()).expect("docs/adr can be read") {
        let name = entry.expect("a directory entry can be read").file_name();
        let name = name.to_string_lossy().into_owned();

        // `README.md` is the index rather than a record, and anything that does
        // not start with four digits is not one either.
        let Some(number) = name.split('-').next().filter(|n| n.len() == 4) else {
            continue;
        };
        if !number.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        found.entry(number.to_owned()).or_default().push(name);
    }

    found
}

#[test]
fn no_two_records_answer_to_the_same_number() {
    let duplicates: Vec<_> = by_number()
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .collect();

    assert!(
        duplicates.is_empty(),
        "two decision records share a number, so a citation of it names both: {duplicates:?}. \
         Renumber the later one to the next free number and update every reference to it — \
         `git grep` for the old filename and for `ADR <number>`.",
    );
}

#[test]
fn the_index_lists_every_record_and_invents_none() {
    // The index is what a reader reaches for, and a record missing from it is a
    // record nobody finds. The reverse — a row for a file that is not there —
    // is a link that answers 404 on the repository's own web view.
    let index = std::fs::read_to_string(records().join("README.md")).expect("the index is there");

    let mut missing = Vec::new();
    for files in by_number().values() {
        for file in files {
            if !index.contains(file.as_str()) {
                missing.push(file.clone());
            }
        }
    }
    assert!(
        missing.is_empty(),
        "these records are not in docs/adr/README.md, so nothing points at them: {missing:?}",
    );

    let mut invented = Vec::new();
    for line in index.lines().filter(|line| line.starts_with("| [")) {
        let Some(target) = line
            .split('(')
            .nth(1)
            .and_then(|rest| rest.split(')').next())
        else {
            continue;
        };
        if !records().join(target).exists() {
            invented.push(target.to_owned());
        }
    }
    assert!(
        invented.is_empty(),
        "docs/adr/README.md links to records that are not there: {invented:?}",
    );
}
