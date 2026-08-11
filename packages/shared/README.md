# packages/shared

TypeScript types and helpers shared between `apps/desktop` and `packages/ui`,
including the types mirroring the recorder IPC protocol.

## What exists today

`SCREENS`: the application's screens as one list, with each screen's label,
route and the issue that builds it. The sidebar and the router are both derived
from it, so a navigation item cannot point at a route that does not exist, and a
screen cannot quietly appear without a destination.

The IPC types are **not** here yet. They are defined by issue #49, against the
recorder's real protocol; a guessed protocol would be worse than none, because
the interface would type-check against a recorder that answers differently.

## Rules

Nothing in this package may import React, Tauri, or anything else that ties it
to one side of the UI boundary. It is consumed as TypeScript source, so there is
no build step.
