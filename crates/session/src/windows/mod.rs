//! The one place this crate touches a Windows API.
//!
//! Coordinating capture, encoding and muxing is platform-neutral work, and all
//! of it above this module is written that way. What is not neutral is the
//! handoff between two crates that each speak about a graphics device without
//! sharing a type for one: `clipped-capture` owns a Direct3D 11 device
//! privately and hands out textures, and `clipped-encoder` has to be opened
//! against the device those textures belong to. Asking a texture which device
//! it came from is the join, and it is a Direct3D call
//! ([`device`]).

pub(crate) mod device;
