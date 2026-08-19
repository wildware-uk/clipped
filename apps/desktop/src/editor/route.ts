/**
 * How the window addresses a clip in the editor.
 *
 * Its own module rather than part of `EditorRoute.tsx` because that file
 * exports components, and a file that exports both loses React's fast refresh
 * — which the lint rule `react-refresh/only-export-components` is there to
 * catch.
 */

/** The search parameter naming the clip to open. */
export const CLIP_PARAMETER = 'clip';

/**
 * The address of the editor with `clip` open.
 *
 * A search parameter rather than a path segment or a store: `#/editor` is the
 * empty editor a sidebar link leads to, `#/editor?clip=3` is clip 3, and the
 * back button does what a back button should. It is the same bargain the
 * playback route makes for a recording (`../clipPlayback.ts`).
 */
export function editorPath(clip: string | number): string {
  return `/editor?${CLIP_PARAMETER}=${encodeURIComponent(String(clip))}`;
}
