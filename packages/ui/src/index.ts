/**
 * `@clipped/ui` — the application shell's components and the design tokens
 * they are drawn with.
 *
 * The shell is all that is here. The Clipped design system's full token set and
 * its component layer - buttons, fields, tables, dialogs - arrive with issue
 * #79; everything in this package is written against `var(--...)` tokens so
 * that lands as an addition rather than a rewrite.
 *
 * Consumers import the stylesheet once, at the application's entry point:
 *
 * ```ts
 * import '@clipped/ui/styles.css';
 * ```
 */

export { AppShell, type AppShellProps } from './AppShell';
export { RecorderStatus, type RecorderStatusProps } from './RecorderStatus';
export { ScreenNav, type ScreenNavProps } from './ScreenNav';
export { ScreenNotBuilt, type ScreenNotBuiltProps } from './ScreenNotBuilt';
