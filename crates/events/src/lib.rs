//! Vocabulary of game and recording events shared across the application.
//!
//! Game integrations report what happened in a game through the abstract event
//! model defined here rather than exposing each game's native protocol to the
//! rest of the application (AGENTS.md section 33).
//!
//! # The model in one paragraph
//!
//! A [`GameEvent`] is a [`kind`](EventKind) — `kill`, `match_started`, or a
//! namespaced name a plugin invented — placed at a [`moment`](EventTiming) on
//! the recording's timeline, [`attributed`](EventSource) to whoever reported
//! it, carrying [`how sure`](Confidence) they are and an optional
//! [`payload`](EventPayload) of detail only they understand. Counter-Strike's
//! Game State Integration, League's Live Client Data API and Dota's GSI are
//! three unrelated protocols; a plugin translates each into this, and nothing
//! above it can tell which game it is looking at.
//!
//! The prose version, including the reasoning behind each decision and what a
//! plugin author has to do, is `docs/plugin-api.md`.
//!
//! # Responsibilities
//!
//! - The universal event types and their payloads.
//! - Event timing relative to a recording.
//! - The stored form of an event, and the rules that keep it readable after the
//!   build that wrote it has been replaced ([`schema`]).
//!
//! # Not responsible for
//!
//! Producing events (see `clipped-plugins`), persisting them (see
//! `clipped-storage`) or acting on them (see `clipped-session`).
//!
//! # Four decisions worth knowing before reading the code
//!
//! **Where an event goes on the timeline.** [`EventTime`] is a moment on the
//! recording's own timeline — the same quantity as `clipped_capture::MediaTime`
//! — and a plugin does not produce one directly, because a plugin knows a game
//! clock or a wall clock and neither is that. Whoever attaches a plugin to a
//! session converts once, through the recording's `CaptureClock`, and hands the
//! result to [`EventTime::from_media_nanos`]. An event carries the moment it
//! *describes*, not the moment it was heard; [`EventTiming`] keeps how late the
//! report was and how precisely the moment is known as separate, explicit
//! fields, because both are things a consumer needs and neither has an honest
//! default.
//!
//! **What constrains `Custom`.** A custom event name is namespaced —
//! `acme-cs2.flag_captured` — and a standard one never is. That single
//! syntactic rule is what stops the open variant swallowing the closed
//! vocabulary: a plugin cannot emit `kill`, cannot claim a name the project has
//! not yet defined, and cannot produce a mark on a timeline that nobody can
//! trace back to it. See [`CustomName`].
//!
//! **What happens when the model grows.** Adding a kind or a field does not
//! break events already stored: unknown fields are ignored, an unknown kind is
//! kept as [`EventKind::Unrecognised`], and the envelope around it is frozen so
//! that an event this build cannot name is still one it can place, attribute
//! and hand back unchanged. [`schema`] owns that policy, and it is the same one
//! `docs/ipc.md` sets out for the wire protocol, with the one difference that
//! reading stored data never refuses.
//!
//! **Where a user's own label lives.** Neither of the above is the right home
//! for something a *person* named — an input binding they called "my
//! ultimate", a fingerprint match they typed a name for. It is not a game
//! concept the closed vocabulary should learn, and it is not a plugin's word,
//! so it is not namespaced the way [`CustomName`] is: [`EventKind::UserLabelled`]
//! carries a [`UserLabel`], marked on the wire with [`USER_LABEL_PREFIX`]
//! rather than a namespace, so that the exact text a person typed survives
//! being stored and read back. [`EventSource::application_component`] is the
//! matching decision for *who* reported it: `clipped.input` and
//! `clipped.fingerprint` are two different, plugin-proof sources, so a mark
//! from one host subsystem is distinguishable on a timeline from a mark from
//! another (issue #345). **This crate does not yet produce either** — nothing
//! here calls these constructors — because deciding a producer boundary for a
//! plugin's report (`crates/plugins/src/report.rs::ReportedEvent::into_event`
//! must refuse a `UserLabelled` kind exactly as it refuses `Unrecognised`) and
//! wiring an actual input-binding or fingerprint subsystem are both work for
//! the crates that own those layers.
//!
//! # Position in the architecture
//!
//! A leaf crate of shared types. It depends on no other `clipped-*` crate so
//! that both the plugin surface and the persistence layer can use it without
//! creating a cycle. Keeping it that way is a constraint rather than an
//! accident: a vocabulary that needs the session or the library to be
//! understood is not shared.

mod event;
mod kind;
pub mod schema;
mod time;

pub use event::{
    Confidence, EventPayload, EventSource, GameEvent, InvalidConfidence, InvalidSource,
    PayloadTooLarge, MAX_PAYLOAD_BYTES,
};
pub use kind::{
    CustomName, EventKind, InvalidCustomName, InvalidUserLabel, UserLabel, MAX_IDENTIFIER_BYTES,
    MAX_USER_LABEL_BYTES, RESERVED_NAMESPACE, USER_LABEL_PREFIX,
};
pub use schema::{SchemaVersion, StoredEvent};
pub use time::{EventTime, EventTiming, RecordedSpan};
