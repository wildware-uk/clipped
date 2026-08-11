# packages/shared

TypeScript types and helpers shared between `apps/desktop` and `packages/ui`,
including the types mirroring the recorder IPC protocol.

## What exists today

`SCREENS`: the application's screens as one list, with each screen's label,
route and the issue that builds it. The sidebar and the router are both derived
from it, so a navigation item cannot point at a route that does not exist, and a
screen cannot quietly appear without a destination.

`src/ipc`: the recorder control protocol. Every message in `docs/ipc.md` — the
handshake, the request and response envelopes, the commands' parameters, the
replies, the events, the error codes and their details — plus the framing and a
parser that turns a frame into one of those types without ever throwing.

## The IPC types are mirrored, and checked

They are written by hand rather than generated. `docs/ipc.md`, under
[The TypeScript types](../../docs/ipc.md#the-typescript-types), records why, and
issue #209 is where it was decided.

A hand-written mirror is only worth having if something fails when it stops
matching, so two tests hold the two sides together:

| Where                         | What it insists on                                                                              |
| ----------------------------- | ----------------------------------------------------------------------------------------------- |
| `crates/ipc/src/schema.rs`    | `src/ipc/protocol-schema.json` is still what the Rust types produce                             |
| `src/ipc/conformance.test.ts` | the types here still agree with that schema, field by field, value by value, and frame by frame |

The schema is derived from the Rust rather than written: field names and
optionality come from `serde`, the wire strings from serialising real values,
and the verdict on every sample frame from running it through the real
deserialiser. Regenerate it with:

```powershell
cargo run -p clipped-ipc --bin protocol-schema
```

Do not edit `src/ipc/protocol-schema.json` by hand. It is generated, and the
Rust test will notice.

## What is not here

**The connection.** Opening the named pipe, performing the handshake and
matching replies to requests is issue #217. These are the messages and the
framing; nothing here does any I/O.

## Rules

Nothing in this package may import React, Tauri, or anything else that ties it
to one side of the UI boundary. It is consumed as TypeScript source, so there is
no build step.
