# apps/desktop

The Clipped desktop application: a Tauri window hosting a React interface.

The desktop application is a _client_ of the recorder. It talks to the recorder
process over the IPC boundary rather than linking the recording crates
directly, so that closing or crashing the UI cannot interrupt a recording. No
crate under `crates/` may depend on anything in this directory, which
`tests/integration/tests/workspace_layering.rs` asserts.

## What exists today

The shell, and only the shell: the window, its layout, its navigation, the
design system's tokens, and the accessibility baseline. Every screen behind a
navigation item says it has not been built and names the issue that builds it,
and the recorder status block says the window cannot reach the recorder — which
is true, because the IPC protocol (issue #49) does not exist yet. Nothing here
invents a recording, a library or a level meter to look finished.

[docs/desktop-ui.md](../../docs/desktop-ui.md) describes how the shell is put
together and what the next tickets extend.

## Running it

From the repository root, after `npm install`:

```powershell
npm run dev       # the application: Vite plus the Tauri window
npm run dev:web   # the interface alone, in a browser, for fast iteration
npm run build     # type-checked production bundle into apps/desktop/dist
npm run build:app # the installable Windows application
npm run lint      # eslint, prettier and tsc
npm test          # the shell's behaviour, in jsdom
```

`npm run dev` compiles the Rust side, so the first run takes several minutes and
needs the same toolchain as the recorder plus the WebView2 runtime, which
Windows 11 ships. `npm run dev:web` needs neither and is the faster loop while
working on the interface itself.

## Layout

| Path             | What it is                                                 |
| ---------------- | ---------------------------------------------------------- |
| `index.html`     | The document Vite builds and Tauri serves                  |
| `src/`           | The React application: the shell, its routes and its tests |
| `src-tauri/`     | The Rust binary that owns the window                       |
| `vite.config.ts` | The bundler, and the Vitest configuration beside it        |

`src-tauri` is its own Cargo workspace on purpose; `src-tauri/Cargo.toml`
explains why, and what CI does instead.
