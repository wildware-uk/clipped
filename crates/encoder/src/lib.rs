//! Hardware and software video encoder backends.
//!
//! Hardware encoding is the default because the recorder runs alongside a game
//! and CPU time is the scarcest resource on the machine (AGENTS.md section 18).
//! A software encoder exists only as a fallback for hardware that cannot encode.
//!
//! # Responsibilities
//!
//! - Detecting available encoders and their supported codecs.
//! - Owning encoder sessions and their GPU resources.
//! - Turning captured frames into encoded packets.
//!
//! # Not responsible for
//!
//! Frame acquisition (see `clipped-capture`) or container writing (see
//! `clipped-muxer`).
//!
//! # Position in the architecture
//!
//! Sits above `clipped-windows` and below `clipped-muxer` and
//! `clipped-session`.
