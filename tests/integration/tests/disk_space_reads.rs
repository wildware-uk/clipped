//! Free disk space is asked for in the place written down here, and nowhere
//! else.
//!
//! # Why this test exists
//!
//! `GetDiskFreeSpaceExW` was called from two crates. Neither copy was a
//! mistake at the time — `clipped-library` needs the size of a volume to judge
//! the storage limits SPEC.md section 27 configures, `clipped-session` needs
//! the free space to stop a recording while there is still room to write its
//! trailer, and neither may depend on the other to ask — but the *mechanism*
//! underneath both questions was written twice: a UTF-16 buffer that has to be
//! null-terminated by hand, two out-pointers, and the rule that a path which
//! does not exist yet is answered by the nearest ancestor that does.
//!
//! Each of those has a quiet failure. An unterminated buffer reads past its
//! end. A copy that forgets the walk refuses to answer on exactly the first
//! run where the recordings directory has not been created, which is when a
//! full disk is most surprising. A copy that clamps free space to the total
//! and one that does not disagree about a volume with a per-user quota, where
//! Windows reports more available to the account than the volume holds.
//!
//! [Issue #277](https://github.com/wildware-uk/clipped/issues/277) folded both
//! into `clipped_windows::volume_free_space`. This file is what keeps the
//! third copy from being written: it fails, in front of whoever writes it,
//! with the reason the call is where it is.
//!
//! # Why it reads source rather than the dependency graph
//!
//! `tests/integration/tests/workspace_layering.rs` asks `cargo metadata` who
//! depends on whom, and that is the right question for layering. It cannot ask
//! this one. A Windows API call is not a dependency edge: `windows` is a
//! third-party crate that eight of these manifests already name, so a second
//! call site changes no graph at all. And `apps/desktop/src-tauri` is its own
//! Cargo workspace, which a root-level `cargo metadata` never sees. Searching
//! the text of the repository is what covers both. It is the same choice
//! `tests/integration/tests/process_table_reads.rs` made, and for the same
//! reason.
//!
//! # Why there is no matching check on the manifests
//!
//! `process_table_reads.rs` also asserts which manifests may turn
//! `Win32_System_Diagnostics_ToolHelp` on, because that namespace exists for
//! process snapshots and enabling it is the step before taking one. The
//! equivalent here would be `Win32_Storage_FileSystem`, and it would not mean
//! the same thing: that namespace is the whole of Win32's file API, and
//! `clipped-ipc` already enables it for the flags `CreateNamedPipeW` takes.
//! A list that fires on any crate wanting a file API would be a list people
//! learn to route around (AGENTS.md section 54's reasoning, applied to a
//! guard), so the call itself is what is checked.

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

/// Every file that may call `GetDiskFreeSpaceExW`, and why that one may.
///
/// One entry, which is the outcome of issue #277 and not an accident of there
/// being only one caller: there are two callers, and they want the same two
/// numbers for opposite purposes. Adding an entry is how a second reading
/// would be taken deliberately — but the argument for one has to answer why
/// `clipped_windows::volume_free_space` cannot be called instead, given that
/// `clipped-windows` is layer 0 and everything in this workspace may name it.
const READ_SITES: &[(&str, &str)] = &[(
    "crates/windows/src/volume.rs",
    "the workspace's one read. `clipped-windows` is the layer platform \
     queries belong in (AGENTS.md section 5), and both callers go through it: \
     `clipped_library::accounting::capacity_of` judges the storage limits in \
     SPEC.md section 27 (issue #93), and `clipped_session::disk::free_space` \
     decides whether a recording can still be finished properly (issue #103). \
     Neither may ask the other — a recorder that had to link the media library \
     and its SQLite index to find out how full a disk is would invert ADR \
     0002 — and both may ask layer 0.",
)];

/// Every `.rs` and `.toml` file of this repository.
///
/// The skips are the ones `tests/integration/tests/process_table_reads.rs`
/// makes, for the reasons given there: `target` is build output, `.git` is
/// history, `node_modules` and `third-party` are somebody else's code, and
/// `.claude` holds agent worktrees — other checkouts of this repository, whose
/// copies of these files would be reported as extra call sites.
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
/// failure message reads the same on every machine and the constant above can
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
    source.file_name().and_then(|name| name.to_str()) == Some("disk_space_reads.rs")
}

#[test]
fn free_disk_space_is_asked_for_only_where_it_is_written_down() {
    let root = repository();

    // The open bracket is what makes this a call rather than a mention. The
    // name appears in prose in `docs/`, in the manifest comment that explains
    // why `clipped-windows` enables the namespace, and in the module
    // documentation of both callers explaining where the call went — none of
    // which read anything.
    let mut found = BTreeSet::new();
    for source in sources(&root) {
        if is_this_file(&source) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&source) else {
            continue;
        };
        if text.contains("GetDiskFreeSpaceExW(") {
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
        "these files call GetDiskFreeSpaceExW and are not among the reads this \
         repository has decided to take: {unexpected:#?}\n\nThe existing reads, and \
         why each one is separate (AGENTS.md section 55, issue #277):{reasons}\n\nA \
         second copy of this call is a second UTF-16 buffer to terminate, a second \
         chance to drop the walk up to the nearest existing ancestor, and a second \
         answer about a volume with a per-user quota. Call \
         `clipped_windows::volume_free_space` instead — every crate in this \
         workspace may name layer 0. If a second read really is right, add it to \
         READ_SITES with the argument for it.",
    );

    let missing: Vec<&String> = permitted.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "READ_SITES names files that no longer read free disk space: {missing:#?}. A \
         stale entry is worse than no list at all — it is a licence for the next copy \
         to be written where this one used to be. Remove it, and the comments that \
         point at it.",
    );
}
