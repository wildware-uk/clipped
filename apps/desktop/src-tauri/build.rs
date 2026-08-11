//! Generates what the Tauri runtime reads at start-up: the application
//! manifest, the Windows resources, and the permission schemas derived from
//! `tauri.conf.json`.

fn main() {
    tauri_build::build();
}
