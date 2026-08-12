/**
 * The element ids that tie a rail's sections to the panes they open.
 *
 * A module of its own rather than two more exports from `SectionRail.tsx`,
 * because a file that exports both a component and a function loses fast
 * refresh: React cannot tell what changed, so it reloads the module and the
 * screen's open section goes back to the first one on every edit.
 *
 * Both ends need them — the rail writes `aria-controls` and the screen writes
 * the pane's `id` — so they are built here rather than by each caller
 * separately, where the two would drift into a pane nothing points at.
 */

/** The id of the rail entry for a section. */
export function railTabId(name: string, id: string): string {
  return `${name}-rail-${id}`;
}

/** The id of the pane a section shows. */
export function railPanelId(name: string, id: string): string {
  return `${name}-pane-${id}`;
}
