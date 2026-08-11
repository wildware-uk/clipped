# apps/desktop

The Clipped desktop application: system tray, settings, games, library and the
clip editor.

Today it is the shell those screens will sit in — the window, the navigation and
the theming — and nothing behind it. Every screen the navigation lists says it
is not built yet and names the issue that builds it. There is no sample data
anywhere in the window.

```text
index.html      The document the window loads
src/            The application: routing, window control, screens
src-tauri/      The Rust shell, and the only Cargo package here
```

[docs/desktop-ui.md](../../docs/desktop-ui.md) is the full account: how to run
it, how it is put together, the design system it is built from, the
accessibility baseline it holds to, and why `src-tauri` is not a member of the
Cargo workspace.

From the repository root:

```text
npm install
npm run dev
```

The desktop application is a *client* of the recorder. It talks to the recorder
process over the IPC boundary rather than linking the recording crates
directly, so that closing or crashing the UI cannot interrupt a recording. No
crate under `crates/` may depend on anything in this directory, and
`src-tauri/Cargo.toml` depends on no crate from the workspace — both are
asserted by `tests/integration/tests/workspace_layering.rs`.

That boundary does not exist yet: the protocol is
[issue #49](https://github.com/wildware-uk/clipped/issues/49) and supervising
the recorder process is
[issue #106](https://github.com/wildware-uk/clipped/issues/106). Until one of
them lands, the window has no way to reach the recorder and says so.
