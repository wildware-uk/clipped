# packages/ui

Shared React components implementing the Clipped design system, consumed by
`apps/desktop`.

## What exists today

The application shell, and the design system's tokens it is drawn with:

| Export           | What it is                                                   |
| ---------------- | ------------------------------------------------------------ |
| `AppShell`       | Title strip, sidebar and screen; owns focus on navigation    |
| `ScreenNav`      | One accessible list of links to screens                      |
| `RecorderStatus` | The recorder's state at the foot of the sidebar              |
| `ScreenNotBuilt` | What a navigation item leads to before its screen is written |
| `styles.css`     | Typeface, tokens, element defaults and the shell's classes   |
| `tokens.css`     | The design tokens alone                                      |

The system's full token set and its component layer — buttons, fields, tables,
dialogs — arrive with issue #79. Everything here is written against
`var(--token)` rather than against a colour or a distance, so that lands as an
addition rather than a rewrite.

## Consuming it

The package is consumed as TypeScript source; there is no build step and no
`dist`. Import the stylesheet once, at the application's entry point:

```ts
import '@clipped/ui/styles.css';
```

## Conventions

- Class names are `clipped-block__element--modifier`, and every one of them is
  defined in `src/styles.css`. There is no CSS-in-JS and no utility framework:
  the design system is a stylesheet, and this package stays one.
- No component reaches for `window`, Tauri or the network. They render what they
  are given, which is what makes them testable in jsdom.
- Colour is never the only carrier of state (AGENTS.md section 46).
