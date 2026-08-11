# packages/shared

TypeScript types and helpers shared between `apps/desktop` and `packages/ui`,
including the types mirroring the recorder IPC protocol.

A type belongs here when both of those packages need it. That rule is what keeps
the package from becoming a bag of everything: `packages/ui` renders what it is
given and knows nothing about application state, `apps/desktop` owns that state,
and the vocabulary they exchange it in lives here.

Today that is the screen model — what the navigation lists, in what order, and
which of them are still unbuilt. The types mirroring the recorder IPC protocol
join it with [issue #49](https://github.com/wildware-uk/clipped/issues/49).

There is no runtime behaviour here beyond the lookups over that data, and there
should not be: this package is imported by a component library that must stay
free of side effects.

```text
npm run test --workspace @clipped/shared
```
