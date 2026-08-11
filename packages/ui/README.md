# packages/ui

Shared React components implementing the Clipped design system, consumed by
`apps/desktop`.

```text
src/components/   The components
src/styles/       The design system's tokens, the base layer, the shell's chrome
src/fonts/        Archivo, vendored rather than fetched from a font CDN
```

What is here so far is the application shell — `AppShell`, `TitleBar`,
`Sidebar`, and the notice an unbuilt screen shows. The design system's component
layer proper (`.btn`, `.input`, `.card`, `.table`, `.dialog` and the React
components that use them) arrives with
[issue #79](https://github.com/wildware-uk/clipped/issues/79).

**Nothing in this package imports a Tauri API.** That is what lets the whole
shell render in a test runner, and it is why `TitleBar` takes its window actions
as props instead of reaching for `getCurrentWindow()` itself. Components take
what they display and what they do from the outside; `apps/desktop` supplies
both.

`src/styles/tokens.css` is a verbatim copy of the design system's token sheet.
Take every colour, font, space and radius from `var(--…)` — do not hard-code a
hex, a font name or a pixel value a token already carries.

[docs/desktop-ui.md](../../docs/desktop-ui.md) covers the design system, where
it lives and how to re-sync this copy.

```text
npm run test --workspace @clipped/ui
```

The tests render each component and drive it the way a user drives it, with the
keyboard rather than by clicking, so the accessibility baseline in AGENTS.md
section 46 fails a test when it regresses rather than waiting for an audit.
