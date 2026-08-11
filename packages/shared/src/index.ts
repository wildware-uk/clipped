/**
 * `@clipped/shared` — types and data with no framework attached, shared by the
 * desktop application (`apps/desktop`) and its component library
 * (`packages/ui`).
 *
 * Nothing here may import React, Tauri or anything else that ties it to one
 * side of that boundary: this package is the vocabulary both sides speak.
 *
 * The types mirroring the recorder's IPC protocol will live here too. They are
 * defined by issue #49 and are deliberately absent until that lands, because a
 * guessed protocol is worse than no protocol.
 */

export type { Screen, ScreenGroup, ScreenId } from './screens';
export { SCREENS, screensInGroup } from './screens';
