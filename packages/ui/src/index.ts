/**
 * `@clipped/ui` — the application shell's components and the design tokens
 * they are drawn with.
 *
 * The exports here are the shell. The design system's component layer -
 * buttons, tags, fields, radios, the segmented control, cards, the table and
 * the dialog - is classes rather than components, in `components.css`, because
 * that is what the system itself is: a screen writes
 * `className="clipped-btn clipped-btn--primary"`. `docs/desktop-ui.md` has the
 * whole set and the rules for consuming it.
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
