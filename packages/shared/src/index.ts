/**
 * `@clipped/shared` — types and data with no framework attached, shared by the
 * desktop application (`apps/desktop`) and its component library
 * (`packages/ui`).
 *
 * Nothing here may import React, Tauri or anything else that ties it to one
 * side of that boundary: this package is the vocabulary both sides speak.
 *
 * `./ipc` is the recorder control protocol — every message in `docs/ipc.md`,
 * mirrored from `crates/ipc` and checked against it by
 * `src/ipc/conformance.test.ts` on every run.
 */

export type { Screen, ScreenGroup, ScreenId } from './screens';
export { SCREENS, screensInGroup } from './screens';

export * from './ipc';
