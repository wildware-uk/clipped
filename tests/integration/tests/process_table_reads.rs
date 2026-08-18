//! Windows' process table is read from the places that are written down here,
//! and from nowhere else.
//!
//! # Why this test exists
//!
//! `CreateToolhelp32Snapshot` is a copy of the whole process table, and every
//! read of it carries the same three obligations: a handle that has to be
//! closed exactly once, an enumeration that has to terminate on
//! `ERROR_NO_MORE_FILES` rather than on an error, and parent identifiers that
//! mean nothing until they are held against creation times, because Windows
//! reuses process identifiers. A second copy of that is three ways to be subtly
//! wrong, and the wrongness is invisible: a leaked snapshot handle looks like
//! a slow memory climb over days, and a believed-stale parent looks like one
//! application quietly refusing to record.
//!
//! [Issue #289](https://github.com/wildware-uk/clipped/issues/289) folded the
//! second copy — `clipped-game-detection`'s — into `clipped-windows`, and left
//! behind comments in three source files and a manifest saying the table now
//! had "exactly one read site in the workspace". There were two. The other,
//! `apps/desktop/src-tauri/src/this_application.rs`, is in a different Cargo
//! workspace, so no `cargo metadata` check could see it and every one of those
//! comments read as a reason not to go looking. That is the failure this file
//! is written against: not the duplicate, which is deliberate and argued
//! below, but the confident sentence saying there was not one.
//!
//! # Why it reads source rather than the dependency graph
//!
//! `tests/integration/tests/workspace_layering.rs` asks `cargo metadata` who
//! depends on whom, and that is the right question for layering. It cannot ask
//! this one. The desktop application is its own Cargo workspace
//! (`apps/desktop/src-tauri/Cargo.toml`), so a root-level `cargo metadata`
//! never sees it — and a Windows API call is not a dependency edge in any
//! case: `windows` is a third-party crate every one of these manifests already
//! names, so making a fourth call changes no graph at all. Searching the text
//! of the repository is what covers both. It is the same choice
//! `tests/integration/tests/foreground_rules.rs` made, and for the same
//! reason.
//!
//! # What it does not claim
//!
//! That two reads is wrong. It is a decision, taken on #289 and recorded in
//! `READ_SITES` with the argument attached, and this test's job is to make the
//! *next* one a decision too rather than an accident — by failing, with the
//! reasons in the message, in front of whoever writes it.

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

/// Every file that may call `CreateToolhelp32Snapshot`, and why that one may.
///
/// Adding an entry is the point: it is how a third read is taken deliberately.
/// What is not permitted is a call in a file this list does not name, because
/// that is how the second one arrived and went unnoticed for as long as it did.
const READ_SITES: &[(&str, &str)] = &[
    (
        "crates/windows/src/process_table.rs",
        "the recorder's one read. `clipped-windows` is the layer platform \
         queries belong in (AGENTS.md section 5), and both callers in this \
         workspace go through it: `ProcessTree` finds a game's children it has \
         never met, and `clipped-game-detection`'s watcher takes a baseline \
         and polls the same table when it cannot reach WMI (issue #289).",
    ),
    (
        "apps/desktop/src-tauri/src/this_application.rs",
        "the window's own read, and it cannot be the one above. The desktop \
         application is a separate Cargo workspace that may link exactly one \
         member of this one, `clipped-ipc` — ADR 0002, asserted by \
         `the_desktop_application_links_nothing_of_this_workspace_but_the_protocol` \
         in tests/integration/tests/workspace_layering.rs — so naming \
         `clipped-windows` to borrow forty lines of enumeration would put the \
         capture layer inside the window's process. It cannot be answered over \
         IPC either: the question is which processes *this* process started \
         (issue #390), which no other process can answer about it, and it is \
         asked on the message loop on every foreground change, when the \
         recorder may not be running at all. The module documents both, and \
         the two places the two reads deliberately differ.",
    ),
];

/// Every `.rs` and `.toml` file of this repository.
///
/// The skips are the same ones `tests/integration/tests/cited_tests_exist.rs`
/// makes, for the same reasons — `target` is build output, `.git` is history,
/// `node_modules` and `third-party` are somebody else's code, and `.claude` is
/// a working directory rather than part of the repository. `.claude` matters
/// more here than it does there: it is where agent worktrees live, so without
/// it this test would read twenty other checkouts of this repository and
/// report their copies of these same two files.
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
                if matches!(
                    name.as_ref(),
                    "target" | ".git" | "node_modules" | ".claude" | "third-party"
                ) {
                    continue;
                }
                pending.push(path);
            } else if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("rs" | "toml")
            ) {
                found.push(path);
            }
        }
    }

    found
}

/// `path` relative to the repository root, with forward slashes, so that a
/// failure message reads the same on every machine and the constants above can
/// be written the way the repository writes paths everywhere else.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("every file was found beneath the root")
        .to_string_lossy()
        .replace('\\', "/")
}

/// Whether `source` is this file.
///
/// Skipped, and only this file is: it has to name the call to explain itself,
/// and every mention it contains is a description rather than a read.
fn is_this_file(source: &Path) -> bool {
    source.file_name().and_then(|name| name.to_str()) == Some("process_table_reads.rs")
}

#[test]
fn the_process_table_is_read_only_where_it_is_written_down() {
    let root = repository();

    // The open bracket is what makes this a call rather than a mention. The
    // name appears in prose in `docs/`, in a `use` list, and inside the quoted
    // string a `SourceError` is built from — none of which read anything, and
    // a check that failed on those would be one people learned to route
    // around.
    let mut found = BTreeSet::new();
    for source in sources(&root) {
        if is_this_file(&source) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&source) else {
            continue;
        };
        if text.contains("CreateToolhelp32Snapshot(") {
            found.insert(relative(&root, &source));
        }
    }

    let permitted: BTreeSet<String> = READ_SITES
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();

    let unexpected: Vec<&String> = found.difference(&permitted).collect();
    let reasons: String = READ_SITES
        .iter()
        .map(|(path, why)| format!("\n\n  {path}\n    {why}"))
        .collect();

    assert!(
        unexpected.is_empty(),
        "these files call CreateToolhelp32Snapshot and are not among the reads this \
         repository has decided to take: {unexpected:#?}\n\nThe existing reads, and \
         why each one is separate (AGENTS.md section 55, issue #289):{reasons}\n\nIf a \
         third read is right, add it to READ_SITES with the argument for it. If it is \
         not, call one of the above instead.",
    );

    let missing: Vec<&String> = permitted.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "READ_SITES names files that no longer read the process table: {missing:#?}. A \
         stale entry is worse than no list at all — it is a licence for the next copy \
         to be written where this one used to be. Remove it, and the comments that \
         point at it.",
    );
}

/// The manifests that turn the `windows` crate's ToolHelp bindings on.
///
/// The same list as `READ_SITES`, one directory up: a crate cannot call
/// `CreateToolhelp32Snapshot` without this feature, so the feature is the gate
/// and the call is what comes through it.
const TOOLHELP_MANIFESTS: &[&str] = &[
    "crates/windows/Cargo.toml",
    "apps/desktop/src-tauri/Cargo.toml",
];

#[test]
fn only_the_crates_that_read_the_table_can_reach_the_bindings() {
    let root = repository();

    // Checked as well as the calls, and not instead of them, because the two
    // fail at different moments. A crate that enables the feature has not read
    // anything yet — this is the half of the guard that fires while somebody is
    // still deciding, rather than after they have written the walk.
    let mut found = BTreeSet::new();
    for source in sources(&root) {
        if source.file_name().and_then(|name| name.to_str()) != Some("Cargo.toml") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&source) else {
            continue;
        };
        // The feature entry itself, which is a quoted string in a list. The
        // same name appears unquoted in the comment above every one of these
        // feature lists, explaining what the namespace is for.
        if text.lines().any(|line| {
            line.trim_start()
                .starts_with("\"Win32_System_Diagnostics_ToolHelp\"")
        }) {
            found.insert(relative(&root, &source));
        }
    }

    let permitted: BTreeSet<String> = TOOLHELP_MANIFESTS
        .iter()
        .map(|path| (*path).to_owned())
        .collect();

    assert_eq!(
        found, permitted,
        "the crates that can reach Windows' ToolHelp bindings must be exactly the ones \
         that read the process table (issue #289). A manifest here that is not in \
         TOOLHELP_MANIFESTS is a third read being made possible; one in \
         TOOLHELP_MANIFESTS that is not here is a feature that has been removed and a \
         list that has gone stale.",
    );
}
