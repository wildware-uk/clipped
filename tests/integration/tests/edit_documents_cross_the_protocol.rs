//! The two places an edit document is described outside `crates/edit`, held
//! against the crate that owns it.
//!
//! An edit document crosses the control protocol as the same JSON `crates/edit`
//! writes (`docs/editing.md`), which means two other places in this repository
//! carry a copy of what a document looks like:
//!
//! - `crates/ipc/src/schema.rs`'s exemplar, which is what
//!   `packages/shared/src/ipc/protocol-schema.json` publishes and what the
//!   desktop application's fixtures are built from
//!   (`apps/desktop/src/test/clipDocumentFixture.ts`); and
//! - `apps/desktop/src/editor/document.ts`'s `EDIT_SCHEMA_VERSION`, which is the
//!   version the window will open and is mirrored by hand
//!   ([issue #601](https://github.com/wildware-uk/clipped/issues/601)).
//!
//! Neither can be checked where it lives. `clipped-ipc` and `clipped-edit` are
//! both at layer 0 of README.md's dependency table, so the protocol crate may
//! not link the model to produce its exemplar — a dependency has to point at a
//! *strictly* lower layer, which `workspace_layering.rs` asserts. And the
//! TypeScript is not Rust. This crate is where both ends are visible at once,
//! which is what it is for.
//!
//! The failure mode being closed is the quiet one. A field added to
//! `EditDocument`, or a `SCHEMA_VERSION` bumped, leaves both copies still
//! *plausible* — valid JSON, sensible field names, nothing that fails to
//! compile — and wrong. The desktop tests would go on passing against a
//! protocol that had moved, which is the thing PR #647 and #652 set out to make
//! impossible for the rest of the wire surface.

use std::path::{Path, PathBuf};

/// The repository root, from this crate's manifest.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/integration is two directories below the root")
        .to_path_buf()
}

/// The exemplar document the protocol schema publishes.
///
/// Read out of `crates/ipc/src/schema.rs` rather than out of the generated
/// JSON, so that a stale `protocol-schema.json` cannot make this pass: the
/// source is what the next regeneration will use.
fn exemplar_in_the_schema() -> String {
    let source = std::fs::read_to_string(repository_root().join("crates/ipc/src/schema.rs"))
        .expect("crates/ipc/src/schema.rs is readable");
    let opening = "const EXEMPLAR_EDIT_DOCUMENT: &str = r#\"";
    let start = source
        .find(opening)
        .expect("crates/ipc/src/schema.rs still defines EXEMPLAR_EDIT_DOCUMENT")
        + opening.len();
    let rest = &source[start..];
    let end = rest
        .find("\"#;")
        .expect("EXEMPLAR_EDIT_DOCUMENT is a raw string that ends");
    rest[..end].to_owned()
}

#[test]
fn the_protocols_exemplar_document_is_what_the_model_actually_writes() {
    // `crates/ipc` cannot call this writer, so the exemplar is a literal. This
    // is what stops it being a lie: the same document, built through the same
    // constructor the recorder synthesises a starting document with, and
    // written by the same writer.
    let document = clipped_edit::EditDocument::from_recording(
        "Ace on Mirage",
        clipped_edit::RecordingId::new("1"),
        clipped_edit::SourceSpan::new(
            clipped_edit::SourceTime::from_nanos(4_000_000_000),
            clipped_edit::SourceTime::from_nanos(34_000_000_000),
        )
        .expect("four seconds to thirty-four is a span"),
    );

    assert_eq!(
        exemplar_in_the_schema(),
        document.write().expect("the exemplar writes"),
        "crates/ipc/src/schema.rs's EXEMPLAR_EDIT_DOCUMENT is no longer what \
         EditDocument::write produces. Every desktop fixture built from \
         protocol-schema.json is derived from it, so they are now testing a document \
         shape that nothing writes. Replace the constant with the text above and run \
         `cargo run -p clipped-ipc --bin protocol-schema`."
    );
}

#[test]
fn the_exemplar_document_is_one_this_build_reads_back() {
    // The other direction, and not implied by the first: a writer and a reader
    // that agreed with each other and disagreed with the validator would still
    // pass above. This is the round trip the window performs on every open.
    let loaded = clipped_edit::EditDocument::read(&exemplar_in_the_schema())
        .expect("the exemplar on the wire is a document this build reads");

    assert_eq!(
        loaded.migrated, None,
        "the exemplar must be at the current version; a sample the recorder would \
         have to convert is not a sample of what crosses the wire"
    );
    assert_eq!(
        loaded.document.schema_version(),
        clipped_edit::SCHEMA_VERSION
    );
}

#[test]
fn the_window_reads_the_version_the_model_writes() {
    // Issue #601. `EDIT_SCHEMA_VERSION` decides which documents the editor will
    // open at all: a window mirroring version 2 against a model that has moved
    // to 3 refuses every document the recorder sends, with a message telling
    // the user to update a Clipped that is already up to date.
    //
    // Nothing checked the two against each other before this, because nothing
    // could see both. This can.
    let source =
        std::fs::read_to_string(repository_root().join("apps/desktop/src/editor/document.ts"))
            .expect("apps/desktop/src/editor/document.ts is readable");

    let opening = "export const EDIT_SCHEMA_VERSION = ";
    let start = source
        .find(opening)
        .expect("apps/desktop/src/editor/document.ts still defines EDIT_SCHEMA_VERSION")
        + opening.len();
    let rest = &source[start..];
    let end = rest
        .find(';')
        .expect("EDIT_SCHEMA_VERSION is a statement that ends");
    let mirrored: u32 = rest[..end]
        .trim()
        .parse()
        .expect("EDIT_SCHEMA_VERSION is a whole number");

    assert_eq!(
        mirrored,
        clipped_edit::SCHEMA_VERSION,
        "apps/desktop/src/editor/document.ts mirrors EDIT_SCHEMA_VERSION = {mirrored}, and \
         clipped_edit::SCHEMA_VERSION is {}. The editor would refuse every document the \
         recorder sends. Update the constant, and the reader below it, for the shape the \
         new version defines.",
        clipped_edit::SCHEMA_VERSION
    );
}
