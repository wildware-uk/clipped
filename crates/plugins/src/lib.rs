//! Highlight provider plugin contract and plugin loading.
//!
//! Plugins observe games through official APIs, local telemetry and logs. They
//! must never use techniques that resemble cheating, because a plugin is not
//! worth risking a user's game account over (AGENTS.md section 34).
//!
//! # Responsibilities
//!
//! - The `HighlightProvider` contract plugins implement.
//! - Discovering, starting and supervising plugins.
//! - Translating plugin output into `clipped-events` types.
//!
//! # Not responsible for
//!
//! Deciding what a highlight is worth recording (see the highlight rules) or
//! persisting events.
//!
//! # Position in the architecture
//!
//! Sits above `clipped-events` and below `clipped-session`.
