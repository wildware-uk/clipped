//! The Clipped desktop application.
//!
//! This binary is a window and nothing else. It opens a WebView2 host, serves
//! the built React interface into it, and gets out of the way. It owns no
//! capture, no encoding and no session state: the recorder
//! (`apps/recorder`) is a separate process for the reason
//! `docs/adr/0002-separate-recorder-process.md` gives — closing or crashing
//! this window must not interrupt a recording.
//!
//! There are no `#[tauri::command]` handlers here yet. The desktop application
//! reaches the recorder over the IPC protocol defined by issue #49, and
//! guessing at that surface before it exists would produce commands the
//! recorder never answers.

// A release build must not open a console window behind the application. The
// debug build keeps one, because that is where `tracing` output and a panic
// backtrace are read from during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        // There is no interface to report this in: the failure is that the
        // window could not be created, which on Windows almost always means
        // the WebView2 runtime is missing. Panicking with that sentence is more
        // use than a silent exit code.
        .expect("failed to open the Clipped window; check that the WebView2 runtime is installed");
}
