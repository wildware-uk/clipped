//! The Clipped desktop application's process entry point.
//!
//! This binary is a shell: it opens one window, hands it the bundled web
//! interface and gets out of the way. It deliberately holds no recording
//! logic and links no crate from `crates/` — the desktop application is a
//! client of the recorder over IPC (issue #49), not a host for it, so that
//! closing or crashing this process cannot interrupt a recording
//! (docs/adr/0002-separate-recorder-process.md).
//!
//! Everything the window is allowed to ask the Rust side for is listed in
//! `capabilities/default.json`. There are no `#[tauri::command]` functions
//! yet; the interface only drives its own window.

// A release build is a windowed application. Without this, Windows attaches a
// console to it and every launch flashes an empty terminal behind the window.
// Debug builds keep the console, because that is where `tauri dev` prints.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = tauri::Builder::default().run(tauri::generate_context!()) {
        // There is no window to report this in — failing to open one is what
        // went wrong — so the message goes to stderr and the exit code says it
        // failed. `tauri::Error` names the cause: a missing WebView2 runtime,
        // a webview that could not be created, a malformed configuration.
        eprintln!("Clipped could not start its window: {error}");
        std::process::exit(1);
    }
}
