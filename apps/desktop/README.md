# apps/desktop

The Clipped desktop application: system tray, settings, games, library and the
clip editor.

This directory is a placeholder. The Tauri and React application is scaffolded
in the M5 milestone; nothing here is buildable yet.

The desktop application is a *client* of the recorder. It talks to the recorder
process over the IPC boundary rather than linking the recording crates
directly, so that closing or crashing the UI cannot interrupt a recording. No
crate under `crates/` may depend on anything in this directory.
