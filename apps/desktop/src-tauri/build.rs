//! Generates the Tauri build context.
//!
//! `tauri_build::build()` reads tauri.conf.json, resolves the capability files
//! under `capabilities/`, and emits the Windows resource that carries the
//! application icon and version metadata. It also fails the build if the
//! frontend bundle named by `frontendDist` has not been produced yet, which is
//! why this crate is not part of the root Cargo workspace: `cargo build
//! --workspace` on a clean clone must not require `npm run build` to have run
//! first. `docs/desktop-ui.md` records that decision.

fn main() {
    tauri_build::build();
}
