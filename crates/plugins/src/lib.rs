//! The highlight plugin contract: what a plugin is, how one is found, and what
//! keeps a bad one away from a recording.
//!
//! Game-specific integrations live behind this boundary and nowhere else
//! (AGENTS.md section 33). Counter-Strike 2 posts Game State Integration
//! payloads to a local endpoint, League of Legends answers an HTTPS request on
//! `127.0.0.1:2999`, Dota 2 posts a different shape again — and none of that is
//! visible above a plugin. What comes out is `clipped_events::GameEvent`, and
//! nothing that consumes one can tell which game produced it.
//!
//! The prose version of everything below, including the worked example a plugin
//! author starts from, is `docs/plugin-api.md`.
//!
//! # What a plugin is
//!
//! **A directory containing a manifest and an executable.** Clipped starts the
//! executable when a game it declares support for is launched, tells it about
//! the session on its standard input, and reads the events it prints on its
//! standard output, one JSON object per line.
//!
//! ```text
//! plugins/counter-strike-2/
//!     plugin.json            what it is, what it supports, what it will do
//!                            with the network
//!     clipped-cs2-plugin.exe a program that prints events
//! ```
//!
//! That is the decision, and it is worth stating what was rejected, because a
//! plugin API is a compatibility surface that cannot be changed casually
//! (AGENTS.md section 43). The alternative was a Rust trait implemented in the
//! recorder's own process, which is what SPEC.md section 22 sketches. Three
//! requirements ruled it out:
//!
//! - **A crashing plugin must not touch a recording** (AGENTS.md sections 16
//!   and 17). In-process, a panic can be caught but an abort, a stack overflow
//!   or a corrupted heap cannot; a plugin fault would end the recorder and the
//!   recording with it. Across a process boundary a plugin crash is an exit
//!   code. This is the same argument
//!   [ADR 0002](../../../docs/adr/0002-separate-recorder-process.md) already
//!   made for keeping the recorder out of the window's process.
//! - **A hanging plugin must be reclaimable.** The recorder runs for days
//!   (AGENTS.md section 59), a plugin's most likely failure is waiting for a
//!   game that has stopped answering, and **a hung thread cannot be killed**.
//!   A hung process can, and is (`crate::supervision`).
//! - **A network declaration must mean something.** `docs/privacy.md` requires
//!   a plugin's network access to be declared and consented to before it runs,
//!   and says plainly that an in-process native plugin can never be held to
//!   such a declaration. A child process can be — not yet, but the mechanism
//!   exists and [issue #280](https://github.com/wildware-uk/clipped/issues/280)
//!   is where it is applied. `NetworkAccess::ENFORCEMENT` is what the user is
//!   told in the meantime, and it does not overstate it.
//!
//! The cost is paid honestly: a process per plugin, a pipe, and JSON. It buys a
//! contract that a plugin written in any language can meet, and a failure mode
//! for every kind of misbehaviour that ends at a queue.
//!
//! There is therefore **no `HighlightProvider` trait**. A trait would be the
//! contract for plugins linked into the recorder, which is the thing that was
//! rejected; adding one now would be an abstraction with one implementation
//! (AGENTS.md section 1) whose only real use is the model this crate exists to
//! avoid. SPEC.md section 22's four operations are all here, as the lifecycle
//! rather than as a vtable:
//!
//! | SPEC.md section 22 | Here | Where it happens |
//! | --- | --- | --- |
//! | `supports(process)` | [`InstalledPlugin::supports`] | In the manifest, before anything runs |
//! | `attach(session)` | [`PluginSupervisor::attach`] | Starts the process, writes `attach` to it |
//! | `events()` | [`EventReceiver`] | A bounded queue the recording drains |
//! | `detach()` | [`PluginSupervisor::detach`] | Writes `detach`, closes its input, kills it if it stays |
//!
//! # What keeps a bad plugin away from a recording
//!
//! One sentence: **a recording never calls a plugin.** It drains a bounded
//! queue that never blocks and never grows ([`EventReceiver`]), and everything
//! else about a plugin — starting it, reading it, timing it, killing it —
//! happens on threads a recording does not wait for (AGENTS.md section 20).
//!
//! | It | Costs a recording | Because |
//! | --- | --- | --- |
//! | crashes | nothing | it is another process; it is replaced, with a widening delay, a bounded number of times |
//! | hangs | nothing | nothing waits on it; after [`SupervisionPolicy::silence_timeout`] it is killed and replaced |
//! | floods | a bounded queue and a counter | delivery never blocks, the queue drops the excess and counts it, and the plugin is stopped |
//! | lies about its timing | nothing it cannot be checked on | it reports *how long ago*, never a position on the recording's timeline (`crate::report`) |
//! | claims to be something else | nothing | there is no `source` field on the wire; the host stamps it from the manifest |
//!
//! # What a plugin may and may not do to a game
//!
//! AGENTS.md section 34 is absolute, and it is a rule about a user's game
//! account rather than about code quality: **no DLL injection, no process
//! memory reading or writing, no code injection, nothing that resembles an
//! anti-cheat bypass.** Permitted: official APIs, local telemetry, game logs,
//! Game State Integration, documented IPC, supported replay files, and the
//! game's own local endpoints. A plugin in this repository that reaches for
//! anything else is not merged; a plugin outside it that does is a plugin whose
//! users risk a ban for a highlight, which is never worth it.
//!
//! # Responsibilities
//!
//! - The manifest and what it declares ([`PluginManifest`], [`NetworkAccess`]).
//! - Finding plugins, and refusing them out loud ([`discover`]).
//! - The wire a plugin speaks ([`PluginReport`], [`HostCommand`]).
//! - Turning what a plugin says into `clipped_events` types
//!   ([`ReportedEvent::into_event`]).
//! - Running plugins, and what happens when one misbehaves
//!   ([`PluginSupervisor`]).
//!
//! # Not responsible for
//!
//! Deciding what a highlight is worth recording (the highlight rules, M10),
//! persisting events (`clipped-storage`, issue #71), storing which plugins are
//! enabled ([issue #282](https://github.com/wildware-uk/clipped/issues/282);
//! the configuration API owns that, and a second settings store here is the
//! defect AGENTS.md section 30 warns about), or drawing any of it
//! ([issue #281](https://github.com/wildware-uk/clipped/issues/281)).
//!
//! # Position in the architecture
//!
//! Layer 1: above `clipped-events`, whose vocabulary it produces, and below
//! `clipped-session`, which attaches plugins to a recording and drains their
//! events. It knows nothing about capture, encoding or files.

mod discovery;
#[cfg(test)]
mod fixture;
mod inbox;
mod manifest;
mod network;
mod process;
mod report;
mod supervision;
mod supervisor;

pub use discovery::{
    discover, ConsentLapsed, Discovery, EnabledPlugin, InstalledPlugin, RejectedPlugin, Rejection,
    MANIFEST_FILE,
};
pub use inbox::{inbox, Delivery, EventInbox, EventReceiver, InboxStats, DEFAULT_CAPACITY};
pub use manifest::{
    ContractVersion, ManifestError, ObservedProcess, PluginId, PluginManifest, Supports, CONTRACT,
};
pub use network::{
    ConsentToken, NetworkAccess, NetworkClass, NetworkDeclarationError, NetworkDirection,
    NetworkGrant,
};
pub use report::{
    hello, read_command, read_report, write_report, HostCommand, PluginReport, ReportRefused,
    ReportedEvent, SessionDetails, SessionTimeline, MAX_PROBLEM_BYTES,
};
pub use supervision::{PluginTrouble, SupervisionPolicy};
pub use supervisor::{PluginHealth, PluginState, PluginSupervisor, SupervisionEvent};
