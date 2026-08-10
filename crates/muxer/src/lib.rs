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
//!
//! # FFmpeg
//!
//! This crate owns the workspace's link against FFmpeg. It is a dynamic link
//! against a prebuilt, LGPL-only build that `scripts/fetch-ffmpeg.ps1`
//! downloads and checksum-verifies; `docs/adr/0004-ffmpeg-dependency-strategy.md`
//! records why, and `docs/ffmpeg.md` covers building against it. The container
//! writing itself is not implemented yet
//! ([issue #21](https://github.com/wildware-uk/clipped/issues/21)); what exists
//! today is [`linkage`], which reports and probes the FFmpeg actually loaded.

pub mod linkage;
