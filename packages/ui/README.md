# packages/ui

Shared React components implementing the Clipped design system, consumed by
`apps/desktop`.

## What exists today

The application shell, and the Modernist design system it is drawn with:

| Export           | What it is                                                                            |
| ---------------- | ------------------------------------------------------------------------------------- |
| `AppShell`       | Title strip, sidebar and screen; owns focus on navigation                             |
| `ScreenNav`      | One accessible list of links to screens                                               |
| `RecorderStatus` | The recorder's state at the foot of the sidebar                                       |
| `ScreenNotBuilt` | What a navigation item leads to before its screen is written                          |
| `styles.css`     | Typeface, element defaults and the shell's classes; imports both files below          |
| `tokens.css`     | The design tokens — and the only literals in the package                              |
| `components.css` | Buttons, tags, fields, radios, the segmented control, cards, the table and the dialog |

The component classes are the ones the Clipped application deck draws with, and
no more. Where they live, how to consume them, and where each one departs from
the design system's own reference page is in
[docs/desktop-ui.md](../../docs/desktop-ui.md).

## Consuming it

The package is consumed as TypeScript source; there is no build step and no
`dist`. Import the stylesheet once, at the application's entry point:

```ts
import '@clipped/ui/styles.css';
```

## Conventions

- Class names are `clipped-block__element--modifier`, and every one of them is
  defined in `src/styles.css` or `src/components.css`. There is no CSS-in-JS and
  no utility framework: the design system is a stylesheet, and this package
  stays one.
- A colour, a typeface, a type size, a distance or a leading is written as a
  value only in `src/tokens.css`. Everywhere else it is `var(--token)`, and
  `src/stylesheet.test.ts` fails the suite if that stops being true — for a
  number in any CSS length unit, in either case, rather than a sample of them.
- Every control that can be disabled draws itself as disabled, and
  `src/stylesheet.test.ts` lists the four, so a fifth cannot ship without one.
- No component reaches for `window`, Tauri or the network. They render what they
  are given, which is what makes them testable in jsdom.
- Colour is never the only carrier of state (AGENTS.md section 46).
