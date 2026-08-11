# apps/desktop

The Clipped desktop application: a Tauri window hosting a React interface.

The desktop application is a _client_ of the recorder. It talks to the recorder
process over the IPC boundary rather than linking the recording crates
directly, so that closing or crashing the UI cannot interrupt a recording.
`tests/integration/tests/workspace_layering.rs` asserts both halves of that: no
crate in the Cargo workspace names `clipped-desktop`, and `src-tauri` names no
crate of the Cargo workspace but `clipped-ipc` — the protocol itself, which a
webview cannot speak on its own, and which depends on nothing else in the
workspace (ADR 0006).

## What exists today

The shell, and the supervision behind it. Every screen behind a navigation item
says it has not been built and names the issue that builds it. The recorder
status block shows what the window's link with the recorder reports — it starts
a recorder if none is running, attaches to one that is, and says so
(ADR 0006) — and nothing here invents a recording, a library or a level meter to
look finished.

`npm run dev` starts the window against the recorder built beside it; set
`CLIPPED_RECORDER_EXE` to point it at one built somewhere else, which during
development is normally `target\debug\clipped-recorder.exe`.

[docs/desktop-ui.md](../../docs/desktop-ui.md) describes how the shell is put
together and what the next tickets extend.

## Running it

From the repository root, after `npm install`:

```powershell
npm run dev       # the application: Vite plus the Tauri window
npm run dev:web   # the interface alone, in a browser, for fast iteration
npm run build     # the production bundle into apps/desktop/dist
npm run build:app # the installable Windows application
npm run lint      # eslint, prettier and tsc
npm test          # the shell's behaviour, in jsdom
```

`npm run dev` compiles the Rust side, so the first run takes several minutes and
needs the same toolchain as the recorder plus the WebView2 runtime, which
Windows 11 ships and `scripts/check-prerequisites.ps1` reports on.
`npm run dev:web` needs neither and is the faster loop while working on the
interface itself.

`npm run build` is `vite build` and nothing else. esbuild strips the types
rather than checking them, so a build succeeding says nothing about whether the
types hold — `npm run lint` is what runs `tsc`, and it is the one CI gates on.

## Layout

| Path             | What it is                                                 |
| ---------------- | ---------------------------------------------------------- |
| `index.html`     | The document Vite builds and Tauri serves                  |
| `src/`           | The React application: the shell, its routes and its tests |
| `src-tauri/`     | The Rust binary that owns the window                       |
| `vite.config.ts` | The bundler, and the Vitest configuration beside it        |

`src-tauri` is its own Cargo workspace on purpose; `src-tauri/Cargo.toml`
explains why, and what CI does instead.
