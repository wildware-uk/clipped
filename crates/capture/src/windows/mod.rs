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
//! - [`DesktopDuplication`], the DXGI Desktop Duplication backend
//!   ([issue #13](https://github.com/wildware-uk/clipped/issues/13)), which is
//!   the fallback SPEC.md section 8 names below it.
//! - [`D3d11FrameSampler`], which reads a few pixels back off a captured frame
//!   so that a capture that has gone black can be noticed
//!   ([issue #97](https://github.com/wildware-uk/clipped/issues/97)). It is not
//!   a backend; it is the platform half of
//!   [`BlackFrameWatch`](crate::BlackFrameWatch).
//!
//! # Apartments and devices
//!
//! COM apartment initialisation (`apartment.rs`) and Direct3D device creation
//! (`device.rs`) live here rather than in `clipped-windows` because
//! `clipped-windows` is still a documentation-only crate and this is the first
//! code in the workspace that needs either. Both are small and self-contained
//! precisely so that moving them down a layer, when a second subsystem needs
//! them, is a move rather than a rewrite — and the apartment in particular is
//! already process-wide rather than per-capture, which is the shape it would
//! have in `clipped-windows` anyway.

mod apartment;
mod desktop_duplication;
mod device;
mod graphics_capture;
mod pixel_sample;

pub use desktop_duplication::DesktopDuplication;
pub use graphics_capture::WindowsGraphicsCapture;
pub use pixel_sample::D3d11FrameSampler;
