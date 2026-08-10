//! Video frame capture backends and target selection.
//!
//! Capture is expressed as a backend trait with one implementation per capture
//! API, so that the correct backend can be chosen at runtime from the target's
//! characteristics and the user never has to know which API is involved
//! (SPEC.md section 8).
//!
//! What exists today is the *interface* and the *selection policy*: the trait a
//! backend implements, the vocabulary it reports in, and the pure function that
//! picks one. No backend implements it yet — Windows Graphics Capture is
//! [issue #12](https://github.com/wildware-uk/clipped/issues/12) and Desktop
//! Duplication is [issue #13](https://github.com/wildware-uk/clipped/issues/13)
//! — so nothing in this crate can currently produce a frame, and it is
//! documented as an interface rather than as behaviour.
//!
//! # Responsibilities
//!
//! - The backend interface: [`CaptureBackend`], [`CaptureBackendFactory`] and
//!   [`BackendDeclaration`].
//! - Choosing a backend and reporting the choice: [`select`], [`Selection`].
//! - The vocabulary frames arrive in: [`CapturedFrame`], [`FrameFormat`],
//!   [`CaptureTimestamp`].
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
//! The interface itself is platform-neutral: no type here names a Windows API,
//! so a backend for another platform is a matter of implementing
//! [`CaptureBackend`] and declaring a [`CaptureMethod`]. Platform code lives in
//! `clipped-windows` or in a `windows/` submodule of this crate, and there is
//! none of either here yet (AGENTS.md section 5).
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
//! Choosing a backend and reporting it the way SPEC.md section 8 asks:
//!
//! ```
//! use clipped_capture::{CaptureMethodSetting, FrameSize, TargetKind, TargetProperties, select};
//! # use clipped_capture::{Availability, BackendCapabilities, BackendDeclaration, CaptureMethod};
//! # #[derive(Debug)]
//! # struct Wgc;
//! # impl BackendDeclaration for Wgc {
//! #     fn method(&self) -> CaptureMethod { CaptureMethod::WindowsGraphicsCapture }
//! #     fn capabilities(&self) -> BackendCapabilities {
//! #         BackendCapabilities::new(true, true).with_occlusion_independent(true)
//! #     }
//! #     fn availability(&self, _: &TargetProperties) -> Availability { Availability::Available }
//! # }
//! # let registry: [&dyn BackendDeclaration; 1] = [&Wgc];
//! let size = FrameSize::new(2560, 1440).expect("a window has a size");
//! let target = TargetProperties::new(TargetKind::Window, size);
//!
//! let selection = select(&registry, &target, CaptureMethodSetting::Automatic)?;
//!
//! println!("Capture method: {}", selection.setting());
//! println!("Current method: {}", selection.method());
//! # Ok::<(), clipped_capture::SelectionError>(())
//! ```

mod backend;
mod error;
mod frame;
mod method;
mod selection;
mod target;
mod time;

pub use backend::{
    Acquisition, Availability, BackendCapabilities, BackendDeclaration, CaptureBackend,
    CaptureBackendFactory, CaptureConfig, Unavailable,
};
pub use error::CaptureError;
pub use frame::{CapturedFrame, FrameFormat, FrameSize, FrameTexture, PixelFormat, TextureKind};
pub use method::{CaptureMethod, CaptureMethodSetting};
pub use selection::{select, Considered, Outcome, Rejection, Selection, SelectionError};
pub use target::{CaptureTarget, TargetHandle, TargetKind, TargetProperties};
pub use time::{CaptureTimestamp, SourceClock};
