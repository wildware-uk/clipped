/**
 * `@clipped/ui` — the application shell's components and the design tokens
 * they are drawn with.
 *
 * The exports here are the shell. The design system's component layer -
 * buttons, tags, fields, radios, the segmented control, cards and the table -
 * is classes rather than components, in `components.css`, because that is what
 * the system itself is: a screen writes
 * `className="clipped-btn clipped-btn--primary"`. `docs/desktop-ui.md` has the
 * whole set and the rules for consuming it.
 *
 * The dialog is the one exception, and `Dialog` says why: its classes are in
 * `components.css` with the rest, but a modal dialog also has behaviour no
 * class can carry - a name, Escape, and where focus is - and every dialog in
 * the application has to have it the same way.
 *
 * Consumers import the stylesheet once, at the application's entry point:
 *
 * ```ts
 * import '@clipped/ui/styles.css';
 * ```
 */

export { AppShell, type AppShellProps } from './AppShell';
export { Dialog, type DialogProps } from './Dialog';
export { RecorderStatus, type RecorderStatusProps } from './RecorderStatus';
export { ScreenNav, type ScreenNavProps } from './ScreenNav';
export { railPanelId, railTabId } from './railIds';
export { SectionRail, type RailSection, type SectionRailProps } from './SectionRail';
export { ScreenNotBuilt, type ScreenNotBuiltProps } from './ScreenNotBuilt';
