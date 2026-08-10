//! Container writing and remuxing built on the FFmpeg libraries.
//!
//! Recordings are written incrementally into a recoverable container so that an
//! abrupt termination costs at most the last few seconds rather than the whole
//! session (AGENTS.md section 17).
//!
//! # Responsibilities
//!
//! - Writing video and multiple audio tracks into a container.
//! - Remuxing without re-encoding where the codecs already match.
//! - Preserving track identity and metadata.
//!
//! # Not responsible for
//!
//! Encoding (see `clipped-encoder`) or edit decisions (see the export engine).
//!
//! # Position in the architecture
//!
//! Sits above `clipped-encoder` and below `clipped-session`.
