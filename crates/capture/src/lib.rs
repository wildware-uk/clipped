//! Video frame capture backends and target selection.
//!
//! Capture is expressed as a backend trait with one implementation per capture
//! API, so that the correct backend can be chosen at runtime from the target's
//! characteristics and the user never has to know which API is involved
//! (SPEC.md section 8).
//!
//! One backend exists: Windows Graphics Capture, in the `windows` submodule,
//! compiled only on Windows and reachable through [`registered_backends`].
//! Desktop Duplication is
//! [issue #13](https://github.com/wildware-uk/clipped/issues/13) and is not
//! built, so a Windows build has exactly one capture backend and a build for
//! any other platform has none.
//!
//! # Responsibilities
//!
//! - The backend interface: [`CaptureBackend`], [`CaptureBackendFactory`] and
//!   [`BackendDeclaration`].
//! - Choosing a backend and reporting the choice: [`select`], [`Selection`],
//!   over the backends this build has: [`registered_backends`].
//! - The vocabulary frames arrive in: [`CapturedFrame`], [`FrameFormat`],
//!   [`CaptureTimestamp`].
//! - Capturing, on Windows: [`windows::WindowsGraphicsCapture`].
//!
//! # Not responsible for
//!
//! Enumerating windows and monitors — that is platform work
//! ([issue #10](https://github.com/wildware-uk/clipped/issues/10)), and it
//! produces the [`CaptureTarget`] this crate consumes — encoding, muxing, or
//! deciding when a recording starts.
//!
//! # Position in the architecture
//!
//! Sits above `clipped-windows` and below `clipped-session`. It must never
//! depend on application or UI concerns (AGENTS.md section 4).
//!
//! # How platform-neutral this actually is
//!
//! No trait method, signature or data structure here is Windows-specific, and
//! every line of Windows code in this crate is inside the [`windows`] submodule,
//! behind `#[cfg(windows)]` (AGENTS.md section 5). Everything else — the
//! interface, the vocabulary, the selection policy — compiles and runs its unit
//! tests on a machine that is not Windows, which is what makes that boundary
//! checkable rather than a claim.
//!
//! The *vocabulary* is a different matter, and three enumerations name a
//! platform outright: [`CaptureMethod::WindowsGraphicsCapture`] and
//! [`CaptureMethod::DesktopDuplication`] name Windows capture APIs,
//! [`TextureKind::D3d11Texture2D`] names a Direct3D 11 interface, and
//! [`SourceClock::PerformanceCounter`] names a Windows clock. That is a
//! decision rather than an oversight. A capture interface cannot be honest and
//! wordless about what a texture is or which clock stamped a frame — the
//! encoder has to know what it has been handed — so the unavoidable platform
//! words are concentrated in three small closed enumerations, where a reader
//! can see all of them at once, instead of being spread through the traits as
//! casts and conditional compilation.
//!
//! The cost is that a backend for another platform is not purely a matter of
//! implementing [`CaptureBackend`]: it also needs variants in those
//! enumerations, and they are closed to other crates, so it has to be built
//! here or the enumerations have to be opened. [`SourceClock::Monotonic`]
//! already exists for that day; [`TextureKind`] has one variant and would need
//! a second. Whether that shape is still right when a second platform actually
//! exists is a question for then; what a reader is owed now is the boundary as
//! it is, not a claim that there is not one.
//!
//! # Ownership and threading, in one paragraph each
//!
//! **Ownership.** A backend owns every native resource it uses — device, frame
//! pool or duplication, and every texture — for its whole life, and hands out
//! only borrows. A [`CapturedFrame`] borrows the backend mutably, so the
//! compiler refuses to let a caller hold two at once or keep one across the
//! next acquisition, which is both what the underlying APIs require and the
//! rule contributors are most likely to break. Anything that needs pixels for
//! longer must copy them into a resource it owns.
//!
//! **Threading.** One backend belongs to one capture thread. [`CaptureBackend`]
//! is [`Send`] so a session can build it elsewhere and move it there, and is
//! not `Sync` because nothing shares it; [`CapturedFrame`] is neither, so frames
//! cannot leave that thread. The declarations selection reads are `Send + Sync`
//! and can be consulted from anywhere, because reading them touches nothing.
//!
//! The long form of both, and what a capture thread is forbidden from doing, is
//! in `docs/capture-pipeline.md`.
//!
//! # Timestamps
//!
//! [`CaptureTimestamp`] has no `now()`. A frame's timestamp is the one its
//! source produced, never the moment this process noticed it, and the type is
//! shaped so that the wrong version is not the convenient one. The reasoning is
//! on [`CaptureTimestamp`] itself.
//!
//! # Example
//!
//! Choosing a backend from the ones this build has, and reporting it the way
//! SPEC.md section 8 asks:
//!
//! ```
//! use clipped_capture::{
//!     CaptureMethodSetting, FrameSize, TargetKind, TargetProperties,
//!     registered_backend, registered_backends, registered_declarations, select,
//! };
//!
//! let size = FrameSize::new(2560, 1440).expect("a window has a size");
//! let target = TargetProperties::new(TargetKind::Window, size);
//!
//! match select(&registered_declarations(), &target, CaptureMethodSetting::Automatic) {
//!     Ok(selection) => {
//!         println!("Capture method: {}", selection.setting());
//!         println!("Current method: {}", selection.method());
//!
//!         // The backend the report named is the one that gets created.
//!         let backend = registered_backend(selection.method())
//!             .expect("selection only ever chooses a registered backend");
//!         let _uninitialised = backend.create()?;
//!     }
//!     // A build for a platform with no capture backend, or a machine where
//!     // Windows declines this target, ends here — and the error names every
//!     // candidate that was asked and what each said, which is the point.
//!     Err(error) => println!("{error}"),
//! }
//! # let _ = registered_backends();
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod backend;
mod error;
mod frame;
mod method;
mod registry;
mod selection;
mod target;
mod time;

#[cfg(windows)]
pub mod windows;

pub use backend::{
    Acquisition, Availability, BackendCapabilities, BackendDeclaration, CaptureBackend,
    CaptureBackendFactory, CaptureConfig, Unavailable,
};
pub use error::CaptureError;
pub use frame::{CapturedFrame, FrameFormat, FrameSize, FrameTexture, PixelFormat, TextureKind};
pub use method::{CaptureMethod, CaptureMethodSetting};
pub use registry::{registered_backend, registered_backends, registered_declarations};
pub use selection::{select, Considered, Outcome, Rejection, Selection, SelectionError};
pub use target::{CaptureTarget, TargetHandle, TargetKind, TargetProperties};
pub use time::{CaptureTimestamp, SourceClock};
