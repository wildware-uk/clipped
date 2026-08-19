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
//!
//! It is the only one. How much room is left where a recording is being
//! written used to be the second — a disk filling up is the most likely way a
//! long recording ends badly, so the recording asks rather than waiting to be
//! told — but that call was `clipped-library`'s as well, and both copies moved
//! down to `clipped_windows::volume_free_space`
//! ([issue #277](https://github.com/wildware-uk/clipped/issues/277)). What is
//! *decided* from the answer never moved: it is platform-neutral and lives in
//! [`crate::disk`], which is where the recording's own threshold is judged.

pub(crate) mod device;
