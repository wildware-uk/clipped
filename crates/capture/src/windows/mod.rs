//! Capture backends built on Windows APIs.
//!
//! Everything in this module and its children is compiled only on Windows, and
//! it is the only place in `clipped-capture` that names a Windows type. The
//! platform-neutral half of the crate — the trait, the vocabulary and the
//! selection policy — still builds and still runs its unit tests on a machine
//! that is not Windows, which is what keeps AGENTS.md section 5's boundary
//! checkable rather than aspirational.
//!
//! # What is here
//!
//! - [`WindowsGraphicsCapture`], the `Windows.Graphics.Capture` backend
//!   ([issue #12](https://github.com/wildware-uk/clipped/issues/12)).
//!
//! Desktop Duplication
//! ([issue #13](https://github.com/wildware-uk/clipped/issues/13)) is not built.
//!
//! # Apartments and devices
//!
//! COM apartment initialisation ([`apartment`]) and Direct3D device creation
//! ([`device`]) live here rather than in `clipped-windows` because
//! `clipped-windows` is still a documentation-only crate and this is the first
//! code in the workspace that needs either. Both are small and self-contained
//! precisely so that moving them down a layer, when a second subsystem needs
//! them, is a move rather than a rewrite — and the apartment in particular is
//! already process-wide rather than per-capture, which is the shape it would
//! have in `clipped-windows` anyway.

mod apartment;
mod device;
mod graphics_capture;

pub use graphics_capture::WindowsGraphicsCapture;
