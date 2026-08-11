# The desktop application

Clipped's window: a [Tauri](https://tauri.app) shell around a React interface,
in `apps/desktop`, built from the two TypeScript packages under `packages/`.

It is a *client* of the recorder, not a host for it. The recorder is a separate
process that owns capture, encoding and session state, and closing or crashing
this window must never interrupt a recording
([ADR 0002](adr/0002-separate-recorder-process.md)). Nothing here links a crate
from `crates/`, and nothing under `crates/` may refer to anything here.

Today the shell has no way to reach the recorder at all: the protocol between
them is [issue #49](https://github.com/wildware-uk/clipped/issues/49) and
supervising the recorder process is
[issue #106](https://github.com/wildware-uk/clipped/issues/106). The window says
so, in the sidebar, rather than showing a Start Recording button that would do
nothing.

## What exists

The shell and nothing else: the title bar, the navigation, the content region,
and the theming all three are drawn from. Every screen the navigation lists is
still unbuilt and says so, naming the issue that builds it. There is no sample
data anywhere in the window (AGENTS.md sections 27 and 54).

```text
apps/desktop/
    index.html            The document the window loads
    src/                  The application: routing, window control, screens
    src-tauri/            The Rust shell (see "The Rust crate" below)
packages/ui/              React components and the design-system stylesheet
packages/shared/          Types and data both of the above need
```

`packages/ui` never imports a Tauri API. That is what lets the whole shell
render in a test runner, and it is why the title bar takes its window actions as
props instead of calling `getCurrentWindow()` itself; `apps/desktop`'s
`useWindowControls` hook is the single place that does.

## How to run it

From the repository root, on a clean clone:

```text
npm install
npm run dev
```

`npm run dev` starts Vite and opens the Tauri window against it, with hot
reloading for the interface and a Rust rebuild when `src-tauri` changes. Node is
pinned by `.nvmrc` and the toolchain requirements are in
[prerequisites.md](prerequisites.md).

The other commands, all from the root:

```text
npm run build              The production frontend bundle
npm run build:app          That bundle compiled and linked into the executable
npm run bundle             The executable wrapped in a Windows installer
npm run lint               ESLint over every package
npm run typecheck          tsc over every package
npm run test               Vitest over every package
npm run format             Prettier over the TypeScript workspace
```

`build:app` is `tauri build --no-bundle`, and it is what CI runs: it proves the
production build works without paying for the installer bundler. The flag lives
in the script rather than being passed on the command line because npm's
PowerShell wrapper eats a forwarded `--`, which drops the flag silently and
builds an installer anyway.

## The Rust crate

`apps/desktop/src-tauri` is a Cargo package, and it is deliberately **not** a
member of the repository's Cargo workspace.

`tauri-build` embeds the bundled web interface into the binary at compile time
and fails if `apps/desktop/dist` has not been produced yet. As a workspace
member it would therefore make `cargo build --workspace` fail on a clean clone
until `npm install && npm run build` had been run first, and the four commands
in CONTRIBUTING.md would stop being the truth about how to build this
repository.

The cost is that the root `cargo fmt`, `cargo clippy` and `cargo deny` do not
reach it, and that it carries its own `Cargo.lock`. The `ui` job in
`.github/workflows/ci.yml` runs each of those against it directly, so it is held
to the same standard from a second place, and `[workspace.lints]` is copied into
its manifest with a note to keep the two in step.

The crate itself is 20 lines: it opens a window and reports it if that fails.
Everything the interface may ask the Rust side for is listed in
`src-tauri/capabilities/default.json` — window controls, and nothing else. There
is no filesystem, shell, network or process access, and there are no
`#[tauri::command]` functions yet.

### The window

The title bar is the application's own, not the system's, because the design
system draws it that way: `decorations` is `false` in `tauri.conf.json` and
`packages/ui`'s `TitleBar` renders the brand mark and the three window controls.

One thing is lost by that: Windows 11's Snap Layouts flyout, which appears when
the pointer rests on a *system* maximise button. Dragging a window to a screen
edge still snaps it, `Win`+arrow still works, and every control in the bar is a
real button, so nothing about the keyboard path changes.

## The design system

The interface is built from the "Modernist" design system in the Clipped design
project, which also carries a deck of every screen:

```text
https://claude.ai/design/p/00676e7a-fd8a-44ce-9410-082644e1418e
```

Flat, set in Archivo, a single red accent on a light ground, zero corner radius,
strong 2px rules, labels flush left. Build UI from it rather than inventing
parallel styling.

`packages/ui/src/styles/tokens.css` is a verbatim copy of that system's token
sheet — the colour roles and their 100–900 ramps, the type scale, spacing,
radius and shadow variables. It is copied rather than adapted so that a change
made in the design project can be diffed against this file. **Take every colour,
font, space and radius from `var(--…)`; do not hard-code a hex, a font name or a
pixel value a token already carries.** `base.css` is the element-level styling
that comes with it, and `shell.css` is Clipped's own chrome, built from the same
tokens.

The class-level component layer the design system defines — `.btn`, `.input`,
`.card`, `.table`, `.dialog` and the rest — is not here yet. It arrives with the
React components that use it, in
[issue #79](https://github.com/wildware-uk/clipped/issues/79).

Archivo is vendored into `packages/ui/src/fonts` rather than fetched from Google
Fonts, which is what the design system's own stylesheet does. A desktop
application that requests a font from a third party every time it starts is
network communication the user did not ask for (AGENTS.md section 14,
[privacy.md](privacy.md)), and it would fall back to a system font whenever the
machine is offline. It is licensed under the SIL Open Font License 1.1, recorded
in THIRD-PARTY-NOTICES.md.

## Accessibility

AGENTS.md section 46 is the standard, and these are the parts of it the shell is
responsible for:

- **Keyboard navigation.** Every control is a real `button` or `a` in document
  order. There is no roving `tabindex` and no key handler of our own, so Tab and
  Enter behave the way they do everywhere else. A skip link is the first thing
  Tab reaches and moves focus into the content region.
- **Visible focus.** `:focus-visible` draws a 2px accent outline, from the
  design system. The browser default is never left in place and focus is never
  simply removed.
- **Names.** The window controls are icons, so each carries an `aria-label`. The
  navigation is a labelled `nav`, the content region is a `main` named after the
  screen showing in it.
- **State that is not only colour.** The current screen is marked three ways:
  the accent bar, the heavier weight, and `aria-current="page"`.
- **Scalable text.** Windows display scaling applies to the webview as it does
  to any window, and nothing here fixes a height that would clip text when it
  grows. `zoomHotkeysEnabled` is set in `tauri.conf.json`, which is what lets
  WebView2 handle Ctrl+`+`, Ctrl+`-` and Ctrl+`0` — but that is configured
  rather than confirmed: it has not been exercised against a running window, so
  do not treat it as verified until someone has.

`eslint-plugin-jsx-a11y` catches the mechanical half of this at the point the
code is written. The rest is asserted by the component tests, which drive the
shell with the keyboard rather than by clicking.

## How to test it

```text
npm run lint
npm run typecheck
npm run test
npm run format:check
```

The tests are Vitest and Testing Library, beside the components in
`packages/*/src`. They render what a user sees and drive it the way a user
drives it — `user.tab()`, `user.keyboard('{Enter}')` — so a regression in the
keyboard path fails a test rather than waiting for a manual audit.

`vitest` and `jsdom` are declared in the *root* `package.json`, beside ESLint,
Prettier and TypeScript, rather than in the packages whose tests use them. That
is not tidiness: npm hoists a dependency to the root when only one workspace
asks for it and nests it when it feels like it, and Vitest resolves `jsdom`
relative to its own install location. When the two land at different levels the
run fails with `Cannot find package 'jsdom'` — which is what CI reported the
first time this landed. Declaring both at the root puts them at the same level
by construction.

There is deliberately no test that mounts the Tauri window: what that would
exercise is Tauri, not Clipped. The one module that talks to the window manager
is small and is verified by running the application.
