//! Windows platform primitives shared by the capture, audio and encoder crates.
//!
//! Every direct call into a Windows API lives either here or in a `windows/`
//! submodule of the crate that owns the behaviour, so that the rest of the
//! workspace stays free of platform conditionals and a future Linux port has a
//! single well-marked surface to reimplement (AGENTS.md section 5).
//!
//! # Responsibilities
//!
//! - COM/WinRT apartment initialisation and lifetime.
//! - Safe wrappers over raw Windows handles and interfaces.
//! - Process, window and monitor queries used by higher layers.
//!
//! # Not responsible for
//!
//! Capture, encoding or audio policy. This crate exposes capability, it does
//! not decide how that capability is used.
//!
//! # Position in the architecture
//!
//! The lowest layer of the workspace. It depends on no other `clipped-*` crate.
