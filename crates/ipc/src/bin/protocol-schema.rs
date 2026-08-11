//! Writes the protocol schema the TypeScript mirror is checked against.
//!
//! ```text
//! cargo run -p clipped-ipc --bin protocol-schema
//! ```
//!
//! The output goes to `packages/shared/src/ipc/protocol-schema.json`, which is
//! committed: `packages/shared/src/ipc/conformance.test.ts` runs in a job with
//! no Rust toolchain, and a check that could only run where `cargo` does would
//! not run where the TypeScript is built. The committed copy being current is
//! itself a test — `the_committed_schema_is_the_one_this_build_produces` in
//! `crates/ipc/src/schema.rs` — so the file cannot go stale in silence.
//!
//! It writes nothing when the file is already correct, so running it is safe
//! and leaves a clean working tree.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clipped_ipc::schema::{protocol_schema, SCHEMA_PATH};

fn main() -> ExitCode {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(SCHEMA_PATH);

    let mut document =
        serde_json::to_string_pretty(&protocol_schema()).expect("the schema serialises");
    document.push('\n');

    if fs::read_to_string(&path).is_ok_and(|existing| existing == document) {
        println!("unchanged {}", path.display());
        return ExitCode::SUCCESS;
    }

    match fs::write(&path, &document) {
        Ok(()) => {
            println!("wrote {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("could not write {}: {error}", path.display());
            ExitCode::FAILURE
        }
    }
}
