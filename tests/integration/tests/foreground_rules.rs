//! The window and the hotkey must agree about what "what is in front" is.
//!
//! Two processes answer the same question. The desktop application answers it
//! for its Record button and its tray item, from a foreground hook of its own
//! in `apps/desktop/src-tauri/src/foreground.rs`; the recorder answers it for a
//! press of the "Start or stop recording" hotkey, in
//! `crates/windows/src/foreground.rs`
//! ([issue #416](https://github.com/wildware-uk/clipped/issues/416)).
//!
//! They cannot share the code. The desktop application may link no member of
//! this workspace but `clipped-ipc` — ADR 0002, asserted by
//! `tests/integration/tests/workspace_layering.rs` — and `clipped-windows` is a
//! member. So what they share is the *list*, and this is what stops the two
//! copies of it drifting.
//!
//! # What drift looks like
//!
//! It is invisible in every other test, and it is not hypothetical: the list is
//! of shell surfaces, and Windows adds them. Whichever side learns about a new
//! one first, the other goes on offering it — so the button offers to record
//! Search and the hotkey refuses, or the other way round, and both are correct
//! according to their own unit tests. Issue #416's third acceptance criterion
//! is exactly this: "The window's Record button and the hotkey agree about what
//! the target is."
//!
//! # What this deliberately does not check
//!
//! The other exclusion the two share — Clipped's own windows — is *not*
//! compared, because the two implementations are genuinely different and should
//! be. The window asks `this_application::includes`, which walks descent from
//! itself and defends against process-identifier reuse with creation times; the
//! recorder is a different process and recognises Clipped by the executables it
//! ships as. Both are documented where they live. A test that demanded they be
//! written the same way would be demanding the wrong thing.
//!
//! Nor does it check the mechanism: the window follows the foreground with a
//! hook because opening its tray menu takes the foreground away, and the
//! recorder polls at the moment of the press because a press takes nothing.
//! That difference is deliberate and is argued in both modules.

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

/// The `&[&str]` a named constant is defined as, in source order.
///
/// Reading the source rather than linking both sides is forced by the layering
/// this file exists to work within: no test can link the desktop application
/// and this workspace at once — they are separate Cargo workspaces
/// (`apps/desktop/src-tauri/Cargo.toml`), and linking them would be the
/// violation.
///
/// It fails loudly rather than answering an empty list, because a constant that
/// has been renamed is the case where a silent pass would be worst: the two
/// lists would then be free to drift with this test still green, which is the
/// failure mode `tests/integration/tests/cited_tests_exist.rs` was written
/// about (AGENTS.md section 54).
fn string_list(file: &Path, constant: &str) -> Vec<String> {
    let source = fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("{} is part of this repository: {error}", file.display()));

    let declaration = format!("const {constant}: &[&str] = &[");
    let start = source.find(&declaration).unwrap_or_else(|| {
        panic!(
            "{} no longer declares `{constant}` as `&[&str]`. It is one of the two halves of the \
             rule this test keeps in step, so renaming or reshaping it means deciding what keeps \
             them in step instead (issue #416)",
            file.display(),
        )
    }) + declaration.len();
    let length = source[start..].find(']').unwrap_or_else(|| {
        panic!(
            "`{constant}` in {} is not closed on the lines that follow it",
            file.display(),
        )
    });

    let entries: Vec<String> = source[start..start + length]
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            entry
                .strip_prefix('"')
                .and_then(|entry| entry.strip_suffix('"'))
                .map(ToOwned::to_owned)
        })
        .collect();

    assert!(
        !entries.is_empty(),
        "`{constant}` in {} was read as an empty list, which is not what it says",
        file.display(),
    );
    entries
}

#[test]
fn the_window_and_the_recorder_refuse_the_same_shell_surfaces() {
    let root = repository();
    let recorder = string_list(
        &root.join("crates/windows/src/foreground.rs"),
        "SHELL_SURFACE_CLASSES",
    );
    let window = string_list(
        &root.join("apps/desktop/src-tauri/src/foreground.rs"),
        "SHELL_WINDOW_CLASSES",
    );

    assert_eq!(
        recorder, window,
        "the recorder and the desktop application disagree about which windows are the shell's \
         own, so the Record button and the Start-or-stop-recording hotkey would offer to record \
         different things (issue #416). Whichever list gained an entry, the other needs it too: \
         crates/windows/src/foreground.rs and apps/desktop/src-tauri/src/foreground.rs",
    );
}

#[test]
fn neither_of_them_refuses_a_file_explorer_window() {
    // The lists are of shell *surfaces*, not of Explorer. `CabinetWClass` is a
    // File Explorer window, and somebody may legitimately want to record one —
    // which is the distinction issue #416 asks to be decided explicitly, and
    // the one an author adding "the Explorer classes" in a hurry would undo on
    // both sides at once, leaving the test above green.
    let root = repository();

    for (file, constant) in [
        ("crates/windows/src/foreground.rs", "SHELL_SURFACE_CLASSES"),
        (
            "apps/desktop/src-tauri/src/foreground.rs",
            "SHELL_WINDOW_CLASSES",
        ),
    ] {
        assert!(
            !string_list(&root.join(file), constant).contains(&"CabinetWClass".to_owned()),
            "{file} refuses File Explorer windows, which are ordinary windows a user may want \
             recorded",
        );
    }
}
