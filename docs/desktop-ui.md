# The desktop application

Clipped's window: a [Tauri](https://tauri.app) shell hosting a React interface,
in `apps/desktop`, with the components and design tokens it is drawn from in
`packages/ui` and the types both sides share in `packages/shared`.

This document covers the shape of the application and the decisions behind it.
[apps/desktop/README.md](../apps/desktop/README.md) has the commands.

## Where it sits

The desktop application is a **client** of the recorder, not a host for it. The
recorder is a separate process that owns capture, encoding and session state
([ADR 0002](adr/0002-separate-recorder-process.md)), so closing or crashing this
window cannot interrupt a recording.

That is a rule about linking, and
`tests/integration/tests/workspace_layering.rs` asserts it in both directions
rather than hoping. It reads every dependency each manifest declares — including
the ones that are not members of the workspace being read, which is the only way
either question can be answered at all:

- no crate in the Cargo workspace names `clipped-desktop`, whatever the
  dependency's source;
- `apps/desktop/src-tauri` names no crate of the Cargo workspace, so the window
  reaches the recorder over IPC rather than by linking capture or encoding into
  its own process;
- `apps/desktop`, `packages/ui` and `packages/shared` are not Cargo packages at
  all, so turning one into a crate has to be a deliberate decision.

The layering table itself covers only workspace members, and `clipped-desktop`
is not one — which is why these are separate assertions and not something the
layer table could have caught.

The two processes will speak over the IPC protocol defined by issue #49. **That
protocol does not exist yet**, and neither does anything in this application
that pretends it does: the recorder status block in the sidebar says the window
cannot reach the recorder, and there are no transport controls, because a Start
Recording button with nothing behind it is exactly what AGENTS.md section 27
forbids.

## What the shell is

```text
┌─────────────────────────────────────────────────────────┐
│ ■ CLIPPED  Game Recorder                                │  <header>
├───────────────┬─────────────────────────────────────────┤
│ Home          │                                         │
│ Library       │                                         │
│ Games         │   <main>                                │
│ Editor        │   the screen                            │
│ Settings      │                                         │
│ ───────────── │                                         │
│ Trash         │                                         │
│ Diagnostics   │                                         │
│               │                                         │
├───────────────┤                                         │
│ RECORDER      │                                         │
│ ■ Not connect │                                         │
└───────────────┴─────────────────────────────────────────┘
```

A title strip, a fixed sidebar carrying two navigation lists and the recorder's
state, and the screen. It follows the application deck in the maintainer's
design project; the deck's custom window chrome does not, and
[issue #202](https://github.com/wildware-uk/clipped/issues/202) covers that
separately.

Both navigation lists and every route are derived from one array, `SCREENS` in
`@clipped/shared`, so a navigation item cannot point at a route that does not
exist. **None of the seven screens has been written.** Each one leads to a panel
saying so and naming the issue that builds it — #60 for Home and Library, #107
for Games, #83 for Editor, #51 for Settings, #94 for Trash, #101 for
Diagnostics. Building one replaces its placeholder route with the real screen.

## Decisions

### Tauri 2, React 19, Vite 7

SPEC.md section 4 recommends Tauri with React and TypeScript. Tauri 2 is the
current major; Tauri 1 is in security-fix-only maintenance, and starting on it
would mean paying for the migration later. Vite is Tauri's own recommendation
and is what `tauri dev` drives.

Routing is `react-router`'s `HashRouter`. The production window loads the
interface from Tauri's asset protocol as a set of files, with no server to
rewrite an unknown path back to `index.html`, so a browser-history router would
404 on a reload of `/settings`. The fragment never reaches the protocol handler,
which also makes the behaviour identical in a browser (`npm run dev:web`) and in
the window.

### The Tauri crate is not a Cargo workspace member

`apps/desktop/src-tauri` is its own single-crate Cargo workspace.

`tauri-build` fails when `frontendDist` — `apps/desktop/dist` — does not exist.
Making the crate a workspace member would therefore make `cargo build
--workspace` depend on a prior `npm install` and `npm run build`, breaking the
promise CONTRIBUTING.md makes about a clean clone, and would put several hundred
crates and a WebView2 dependency in front of every `cargo test --workspace`.

The cost is that `cargo fmt --all`, `cargo clippy --workspace`, `cargo build
--workspace` and `cargo deny` do not reach it, and every one of those is paid
back explicitly rather than dropped:

- the Desktop UI job runs `cargo fmt --check` and `cargo clippy -- -D warnings`
  against the crate, after the frontend build has produced `dist`;
- the Dependencies job runs `cargo deny` against it with `--manifest-path` and
  `--config deny.toml`, so both lockfiles are held to one policy. It needs
  neither Node nor `dist`, because `cargo metadata` runs no build scripts, which
  is why it sits in that job rather than beside the two above.

That last one matters more than it looks: the detached lockfile has 429 packages
against the root's 113, and almost none of them are shared. It reports five
unmaintained-crate advisories, all of them the `unic-*` family reached through
`urlpattern` → `tauri-utils` → `tauri`. They are accepted in `deny.toml`'s
`[advisories] ignore` against
[issue #200](https://github.com/wildware-uk/clipped/issues/200), which is a
decision on the record rather than a check that cannot see them.

### One npm workspace, one lockfile

`apps/desktop` and `packages/*` are npm workspaces of the repository root, with
a single `package-lock.json` there. Two lockfiles would mean two resolutions and,
sooner or later, two copies of React in one window. Every script is run from the
root:

```powershell
npm install     # once
npm run dev     # the application
npm run lint    # eslint, prettier and tsc
npm test        # the shell's behaviour, in jsdom
npm run build   # the production bundle
```

`packages/*` are consumed as TypeScript source rather than built to `dist`.
Vite compiles them along with the application, so there is no build order to get
wrong and no stale artefact to debug.

## Design system

The interface is drawn in the Modernist system: flat, Archivo, a single red
accent on a light ground, zero corner radius, strong 2px rules, flush-left
labels. `packages/ui/src/tokens.css` holds the tokens, and no colour, type size,
spacing step or typeface appears anywhere else as a value: `styles.css` names a
token for each, and no component contains any of them. What is left as a literal
in `styles.css` is one-off geometry that is not a point on any scale — the
hairline that hides the skip link until it is focused, a link's underline offset,
and the half-step of vertical padding the utility navigation is drawn at. Issue
#79 brings the system's full token set and its component layer into the same
file, which is an addition rather than a rewrite because of that.

Two deliberate differences from the published system, both marked in
`tokens.css`:

- **Secondary text** is drawn at 70% of the ink rather than 55%. At 55% it
  measures 3.65:1 on the window ground and 3.54:1 on the sidebar, short of the
  4.5:1 AGENTS.md section 46 asks of body text; at 70% it measures 5.81:1 and
  5.55:1.
- **Type sizes are `rem`**, not pixels, so that the Windows text-size setting
  and the application's zoom both work. The values are the system's own sizes at
  the default root size.

Archivo is bundled from `@fontsource/archivo` (SIL OFL 1.1) rather than fetched
from Google Fonts as the system's own stylesheet does. A locally installed
recorder must not make a network request to draw its own window
([docs/privacy.md](privacy.md)) and must work with no connection at all.

## Accessibility

AGENTS.md section 46 is the baseline, and the shell is built to it rather than
retrofitted:

- **Keyboard.** Navigation items are real anchors in a real list, so Tab reaches
  them and Enter activates them. The first stop in the tab order is a "Skip to
  content" button. Nothing in the chrome is reachable by mouse alone.
- **Focus.** `:focus-visible` draws a `--rule-weight` accent outline. After a
  navigation, focus moves into `<main>` — without that, a screen reader
  announces nothing, because as far as the platform is concerned the window
  never changed. On the _first_ screen it deliberately does not move, which is a
  guard that has to survive React's StrictMode double-invoking the effect: it
  holds the screen key it last acted on rather than a "have I run?" flag, and
  `Shell.test.tsx` mounts the same `<StrictMode>` tree `main.tsx` does so the
  guard is covered as it actually runs.
- **Contrast.** Every pairing of words and ground in the shell clears WCAG's
  4.5:1 for body text, and `packages/ui/src/contrast.test.ts` measures it rather
  than asserting it — it implements the relative-luminance formula, resolves the
  values out of `tokens.css`, and reads the skip link's own two declarations out
  of `styles.css`:

  | | Ratio |
  | --- | --- |
  | Body text on the window ground | 14.86:1 |
  | Secondary text on the window ground | 5.81:1 |
  | Secondary text in the sidebar | 5.55:1 |
  | Accent text on the window ground | 6.41:1 |
  | The open navigation item | 5.83:1 |
  | The title strip | 11.45:1 |
  | The title strip's tagline | 4.87:1 |
  | The skip link | 6.41:1 |

  The skip link is why the test reads the stylesheet rather than a table: it
  first shipped on `--color-accent` at 3.76:1, and at 14px weight 800 it is not
  WCAG large text, so 4.5:1 is the bar it has to clear.
- **Labels.** Each of the two navigation lists is a named `<nav>`; the recorder
  status is a named region and a polite live region, so a change in state is
  announced rather than only drawn.
- **State is never colour alone.** The open screen is marked by an accent rule
  down its left edge, a heavier weight, *and* `aria-current="page"`.
- **The window title** names the open screen, so the taskbar, Alt+Tab and the
  screen reader's window announcement all say where you are.

`eslint-plugin-jsx-a11y` runs in its `strict` configuration as part of
`npm run lint`, and `apps/desktop/src/Shell.test.tsx` drives the shell with Tab
and Enter rather than asserting that the markup looks right.

## Testing

`npm test` runs Vitest, from `apps/desktop` but over `packages/*/src` as well,
because those packages are consumed as source and one test command for the npm
workspace is worth more than a second configuration to remember. Most of it runs
against jsdom; `packages/ui/src/contrast.test.ts` asks for the node environment,
because it reads the stylesheets as text and Vitest replaces a CSS import with an
empty module.

The tests assert the things about this shell that would rot quietly: that no
part of it shows data it does not have — including that the only control in the
whole window is the skip link — that the chrome is operable from the keyboard
alone, and that every pairing of words and ground clears 4.5:1.

`Shell.test.tsx` renders the `<StrictMode>` tree `main.tsx` builds rather than
`<App />` on its own. That is not ceremony: StrictMode double-invokes effects on
mount while preserving refs, and a focus guard that passed under a bare `<App />`
failed under the real tree.

`useWindowTitle.test.ts` stands up a `__TAURI_INTERNALS__` so the branch that
only runs inside the window is reached — jsdom is a browser, so without it the
native call, which is the only reason the hook exists, has no coverage. The real
`@tauri-apps/api` runs against the stub, so the test sees the command it
actually sends, and asserts that `src-tauri/capabilities/default.json` grants
that command. Removing `core:window:allow-set-title` therefore fails a test
rather than a window nobody opened.

The Rust side of that call is out of reach here — Tauri decides whether to
answer it in the process that owns the window — and so is WebView2. Keyboard
behaviour in the real window is checked by hand, by driving it with Tab,
Shift+Tab and Enter and watching the window title follow the screen.
