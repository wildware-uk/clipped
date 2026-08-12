//! The Desktop Duplication backend.
//!
//! DXGI's `IDXGIOutputDuplication` hands over a duplicate of one *display
//! output* — everything on that screen, whatever produced it — as a Direct3D 11
//! texture on the adapter the output is attached to. It is the fallback path
//! SPEC.md section 8 names below Windows Graphics Capture, and it exists because
//! it works where the newer API does not: it predates it, it needs no
//! compositor cooperation, and it is what remains when
//! `GraphicsCaptureSession::IsSupported` says no.
//!
//! It also differs from Windows Graphics Capture in three ways that shape this
//! whole file.
//!
//! **It captures a screen, not a thing.** Anything drawn over the target — a
//! notification, an overlay, another window — is in the recording, which is why
//! [`BackendCapabilities::is_occlusion_independent`] is false here and true
//! there, and why SPEC.md section 8 prefers the other one. A *window* target is
//! reached by duplicating the output the window is on and copying its client
//! area out of each frame, so a window that moves is followed, and a window
//! that straddles two outputs is captured from the one showing most of it — see
//! [`place_window_in_output`], which is where that rule lives.
//!
//! **It says how many frames were missed.** `DXGI_OUTDUPL_FRAME_INFO` carries
//! `AccumulatedFrames`, the number of updates the operating system coalesced
//! into the frame being handed over, so
//! [`CapturedFrame::frames_missed`](crate::CapturedFrame::frames_missed) is a
//! measurement rather than the estimate the Windows Graphics Capture backend has
//! to derive from timestamps.
//!
//! **It can have its access taken away.** A mode change, a full-screen
//! transition, a driver reset or a session switch invalidates the duplication
//! with `DXGI_ERROR_ACCESS_LOST`, and the only correct response is to release it
//! and build a new one. That happens here, inside [`CaptureBackend::acquire`],
//! without the recording ending: see [`Running::reinitialise`].
//!
//! # Ownership
//!
//! [`Running`] owns everything for the life of one capture, and [`Session`]
//! owns the part that access loss throws away: the Direct3D device, the
//! duplication, the destination texture a cropped capture is copied into, and
//! the outstanding DXGI frame. Releasing a `Session` calls `ReleaseFrame` if one
//! is outstanding and then drops the duplication, which is what lets *other*
//! applications duplicate the output again — leaking one would be a fault a user
//! could only clear by ending the process (AGENTS.md section 58).
//!
//! `DesktopDuplicationBackend` holds an `Option<Running>`; `shut_down` is
//! `self.running = None`, and `Drop` calls `shut_down`, so an unwind on the
//! capture thread releases exactly what a clean stop would.
//!
//! # Threading
//!
//! One backend, one capture thread, and no callbacks: `AcquireNextFrame` blocks
//! with its own timeout, so unlike the Windows Graphics Capture backend there is
//! no event handler, no condition variable and no second thread involved. The
//! Direct3D 11 immediate context this file uses to copy a cropped frame is not
//! free-threaded, which costs nothing here because only the capture thread ever
//! touches it.

use core::fmt;
use core::num::NonZeroU64;
use core::time::Duration;
use std::thread;
use std::time::Instant;

use windows::core::Interface;
use windows::Win32::Foundation::{E_INVALIDARG, HMODULE, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_IDENTITY,
    DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_NOT_FOUND,
    DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTPUT_DESC,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::Performance::QueryPerformanceFrequency;
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindow};

use super::client_size;
use crate::{
    Acquisition, Availability, BackendCapabilities, BackendDeclaration, CaptureBackend,
    CaptureBackendFactory, CaptureConfig, CaptureError, CaptureMethod, CaptureTarget,
    CaptureTimestamp, CapturedFrame, FrameFormat, FrameSize, FrameTexture, PixelFormat, TargetKind,
    TargetProperties, TextureKind, Unavailable,
};

/// The method every error and log line in this file names.
const METHOD: CaptureMethod = CaptureMethod::DesktopDuplication;

/// The longest a single `AcquireNextFrame` is allowed to block.
///
/// The caller's timeout can be a second or more, and for a window target the
/// answers to "has it closed?", "has it been minimised?" and "has it been
/// dragged to the other display?" are only re-read between acquisitions. Slicing
/// the wait bounds how stale those answers get at about a tenth of a second,
/// while still being long enough that an idle desktop costs ten wakeups a
/// second rather than a spin.
const ACQUIRE_SLICE: Duration = Duration::from_millis(100);

/// How long to wait before trying again after a failed attempt to rebuild the
/// duplication.
///
/// Rebuilding fails while a mode change is still in progress, and retrying
/// immediately would spin on `DuplicateOutput` for as long as the transition
/// takes. A tenth of a second is short enough that a recording resumes within a
/// frame or two of the display settling.
const REBUILD_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// How often to repeat the warning while the duplication cannot be rebuilt.
///
/// The first failure is always logged. After that the message repeats at this
/// interval rather than once per attempt, so a display that stays unavailable
/// for a minute produces a dozen lines rather than six hundred (AGENTS.md
/// section 35).
const RECOVERY_WARNING_INTERVAL: Duration = Duration::from_secs(5);

/// How long a display may be missing from the DXGI enumeration before the
/// recording is told the target has gone.
///
/// A display is genuinely absent for a moment in the middle of a topology
/// change — the change that caused the access loss in the first place — and
/// ending a recording because a monitor blinked would be exactly the wrong
/// answer. It is only after it stays away that "unplugged" is the better
/// explanation than "still changing".
const OUTPUT_ABSENCE_GRACE: Duration = Duration::from_secs(5);

/// Desktop Duplication, as a thing that can be selected and created.
///
/// Zero-sized, like every other backend declaration: everything it says is a
/// constant, and everything that costs anything happens in the backend
/// [`create`](CaptureBackendFactory::create) returns. That is what lets
/// [`select`](crate::select) hold a `'static` reference to it and ask it
/// questions without touching a GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DesktopDuplication;

impl BackendDeclaration for DesktopDuplication {
    fn method(&self) -> CaptureMethod {
        METHOD
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Windows and monitors both, but by different routes: a monitor is what
        // DXGI duplicates, and a window is that duplicate cropped to the
        // window's client area every frame.
        //
        // Not occlusion independent, because this is a duplicate of the screen:
        // a notification, an overlay or another window on top of the target is
        // in the recording. That is the single reason SPEC.md section 8 ranks
        // this method below Windows Graphics Capture.
        //
        // Not cursor optional, because Desktop Duplication never draws the
        // cursor into the desktop image at all — it reports the pointer
        // separately, as a position and a shape, for an application to composite
        // itself. So `CaptureConfig::capture_cursor` cannot be honoured in
        // either direction here: the recording has no cursor whatever the
        // setting says, and declaring the capability false is what tells a
        // settings screen to say so rather than offer a switch that does nothing
        // (issue #100).
        BackendCapabilities::new(true, true)
    }

    fn availability(&self, target: &TargetProperties) -> Availability {
        if target.is_content_protected() {
            // `SetWindowDisplayAffinity` is enforced by the compositor, and
            // Desktop Duplication is downstream of it: `WDA_MONITOR` renders the
            // window as a black rectangle and `WDA_EXCLUDEFROMCAPTURE` leaves
            // whatever is behind it in the frame. Neither is the recording the
            // user asked for, and both succeed silently, which is the failure
            // issue #97 exists to prevent.
            return Availability::Unavailable(Unavailable::UnsupportedTarget {
                reason: "the target has excluded itself from capture with \
                         SetWindowDisplayAffinity, so it would be recorded as a black \
                         rectangle or omitted from the frame entirely",
            });
        }

        if target.is_minimised() {
            // This backend reaches a window by cropping the display it is on,
            // and Windows parks a minimised window at around (-32000, -32000):
            // it is on no display at all, so there is nothing to crop until it
            // is restored. Declining agrees with the other backend, which means
            // `select` refuses the recording rather than falling through to this
            // one and producing the same empty file (issue #383).
            return Availability::Unavailable(Unavailable::UnsupportedTarget {
                reason: "the window is minimised, so it is not on any display and there \
                         would be nothing to crop out of one",
            });
        }

        // Everything else this backend needs — a display output attached to a
        // desktop, on an adapter whose driver supports duplication — costs a
        // DXGI enumeration to find out, and `availability` is asked of every
        // candidate ahead of the winner while a user waits for a recording to
        // start. It is answered in `initialise` instead, where the target's
        // handle is available and a `CaptureError` can name the display.
        Availability::Available
    }
}

impl CaptureBackendFactory for DesktopDuplication {
    fn create(&self) -> Result<Box<dyn CaptureBackend>, CaptureError> {
        // Deliberately touches nothing native. The backend is created wherever
        // the session happens to be running and moved to the capture thread; the
        // device, the duplication and the immediate context all belong to that
        // thread, and building them here would build them on the wrong one.
        Ok(Box::new(DesktopDuplicationBackend { running: None }))
    }
}

/// A live Desktop Duplication capture, or an uninitialised shell of one.
///
/// The `Option` is the state machine the trait documents: `None` before
/// `initialise` and after `shut_down`, `Some` in between.
struct DesktopDuplicationBackend {
    running: Option<Running>,
}

// SAFETY: `CaptureBackend` is `Send` so that a session can create a backend and
// move it to the capture thread, and this type has to satisfy that.
//
// The value that actually crosses a thread boundary owns nothing: `create`
// returns `running: None`, and every native resource is built by `initialise`,
// which the trait documents as running on the capture thread.
//
// If a caller nevertheless moved an initialised backend, the interfaces it holds
// would tolerate it. Direct3D 11 devices and DXGI objects are free-threaded, and
// the one interface here with a threading rule — the immediate context, which is
// not safe for concurrent use — is only ever used from whichever thread is
// inside a `&mut self` method, and `CaptureBackend` is not `Sync`, so there can
// only be one of those at a time. What is *not* claimed is that two threads may
// use one backend at once: every method takes `&mut self` and a `CapturedFrame`
// is neither `Send` nor `Sync`.
unsafe impl Send for DesktopDuplicationBackend {}

impl fmt::Debug for DesktopDuplicationBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopDuplicationBackend")
            .field("method", &METHOD)
            .field(
                "format",
                &self.running.as_ref().map(|running| running.format),
            )
            .finish()
    }
}

impl CaptureBackend for DesktopDuplicationBackend {
    fn method(&self) -> CaptureMethod {
        METHOD
    }

    fn initialise(
        &mut self,
        target: &CaptureTarget,
        config: &CaptureConfig,
    ) -> Result<FrameFormat, CaptureError> {
        if self.running.is_some() {
            return Err(CaptureError::AlreadyInitialised { method: METHOD });
        }

        let running = Running::start(target, config)?;
        let format = running.format;
        self.running = Some(running);

        Ok(format)
    }

    fn acquire(&mut self, timeout: Duration) -> Result<Acquisition<'_>, CaptureError> {
        let running = self
            .running
            .as_mut()
            .ok_or(CaptureError::NotInitialised { method: METHOD })?;

        // Ownership rule 3 (docs/capture-pipeline.md): the previous frame goes
        // back to DXGI here, before anything else, and the borrow checker has
        // already proved that nobody is still holding it. Desktop Duplication is
        // stricter about this than most APIs — `AcquireNextFrame` fails with
        // `DXGI_ERROR_INVALID_CALL` until `ReleaseFrame` has been called — so
        // this is not tidiness, it is the next acquisition working at all.
        running.release_held_frame();

        if let Some(size) = running.awaiting_resize {
            return Ok(Acquisition::SizeChanged(size));
        }

        running.pump(Instant::now() + timeout)
    }

    fn resize(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError> {
        let running = self
            .running
            .as_mut()
            .ok_or(CaptureError::NotInitialised { method: METHOD })?;
        let format = running.adopt_size(size)?;

        tracing::info!(
            width = size.width(),
            height = size.height(),
            "Desktop Duplication reconfigured for a new frame size"
        );

        Ok(format)
    }

    fn shut_down(&mut self) {
        // Idempotent because taking from an `Option` twice is: the second call
        // drops a `None`. Everything is released by `Session::drop`, which is
        // also what runs if the capture thread unwinds instead of stopping.
        if let Some(running) = self.running.take() {
            tracing::info!(
                access_losses = running.access_losses,
                "Desktop Duplication stopped"
            );
        }
    }
}

impl Drop for DesktopDuplicationBackend {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// What part of the duplicated output the caller actually asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    /// The whole display. Frames are handed on exactly as DXGI produced them,
    /// with no copy of any kind.
    WholeOutput,
    /// One window's client area, copied out of each frame.
    ///
    /// The client area rather than the whole window, which is the same choice
    /// `clipped_windows::WindowGeometry` makes and for the same reason: the
    /// frame, the title bar and the drop shadow are not what a game renders and
    /// are not what should be recorded.
    Window(HWND),
}

/// Everything one `initialise` produced, including the parts that survive access
/// loss.
struct Running {
    /// What is being captured out of the output.
    region: Region,
    /// The display device name — `\\.\DISPLAY1` — the output was found under.
    ///
    /// Kept because an `HMONITOR` does not survive a display being attached,
    /// detached or rearranged, and rearrangement is one of the things that
    /// causes access loss in the first place. After a loss the output is looked
    /// for by handle first and by this name second, so a monitor whose handle
    /// was reissued is still found.
    output_name: String,
    /// The monitor the output was last found under.
    monitor: HMONITOR,
    /// `QueryPerformanceFrequency`, read once: fixed for the life of the system,
    /// and what turns `LastPresentTime` into a [`CaptureTimestamp`].
    counter_frequency: NonZeroU64,
    /// The live duplication, or [`None`] between access being lost and a
    /// successful rebuild.
    session: Option<Session>,
    /// The format `initialise` or `resize` last reported.
    format: FrameFormat,
    /// The frame lent to the caller, still owned here.
    held: Option<HeldFrame>,
    /// A size change that has been reported and not yet acted on. While this is
    /// set the backend is idle and every acquisition repeats the report, which
    /// is what the trait documents.
    awaiting_resize: Option<FrameSize>,
    /// When the duplication was lost and not yet rebuilt, for the log.
    recovering_since: Option<Instant>,
    /// When the "still cannot rebuild" warning was last emitted.
    recovery_warned_at: Option<Instant>,
    /// Whether the window has been reported as minimised since it last produced
    /// a frame, so the observation is logged once rather than every acquisition.
    reported_minimised: bool,
    /// How many times access to the display has been lost and rebuilt during
    /// this capture.
    ///
    /// Logged at teardown, because "the recording survived nine mode changes" is
    /// the sort of thing a bug report needs and nothing else records. It is also
    /// what `access_lost_is_recovered_from_without_ending_the_recording` asserts
    /// on, so that the test cannot pass by never provoking a loss at all.
    access_losses: u64,
}

/// One frame, borrowed by the caller.
///
/// For a whole-output capture the texture *is* DXGI's desktop image and the
/// duplication still has a frame outstanding; for a window capture it is this
/// backend's own destination texture and the DXGI frame was released as soon as
/// the copy was queued. Either way the texture stays valid until the next
/// acquisition, which is the promise [`FrameTexture`] carries.
struct HeldFrame {
    /// The texture handed to the caller.
    texture: ID3D11Texture2D,
    /// The timestamp the frame arrived with, converted once.
    timestamp: CaptureTimestamp,
    /// Updates the operating system coalesced into this frame that the caller
    /// therefore never saw, from `DXGI_OUTDUPL_FRAME_INFO::AccumulatedFrames`.
    frames_missed: u32,
}

impl Running {
    /// Opens a capture of `target`.
    fn start(target: &CaptureTarget, config: &CaptureConfig) -> Result<Self, CaptureError> {
        let raw = target.handle().as_raw() as *mut core::ffi::c_void;

        let (region, monitor) = match target.properties().kind() {
            TargetKind::Window => {
                let window = HWND(raw);
                if !is_window(window) {
                    return Err(CaptureError::TargetLost { method: METHOD });
                }
                let Some(monitor) = monitor_for_window(window) else {
                    return Err(CaptureError::TargetLost { method: METHOD });
                };
                (Region::Window(window), monitor)
            }
            TargetKind::Monitor => (Region::WholeOutput, HMONITOR(raw)),
        };

        let output = match find_output(Some(monitor), None)? {
            Some(output) => output,
            None => return Err(CaptureError::TargetLost { method: METHOD }),
        };

        let session = Session::open(&output, region)?;
        let format = FrameFormat::new(
            match region {
                Region::WholeOutput => session.size,
                // A window's frames are its client area, which the caller has
                // already measured; reading it again here keeps the format
                // honest if the window was resized between enumeration and now.
                Region::Window(window) => {
                    client_size(window).ok_or(CaptureError::TargetLost { method: METHOD })?
                }
            },
            PixelFormat::Bgra8Unorm,
        );

        if config.capture_cursor() {
            tracing::warn!(
                "Desktop Duplication never draws the mouse cursor into the desktop image, \
                 so the recording will have no cursor even though one was asked for"
            );
        }

        tracing::info!(
            target_kind = %target.properties().kind(),
            display = output.name.as_str(),
            output_width = session.size.width(),
            output_height = session.size.height(),
            width = format.size().width(),
            height = format.size().height(),
            pixel_format = %format.pixel_format(),
            cropped = matches!(region, Region::Window(_)),
            adapter = output.adapter_name.as_str(),
            "Desktop Duplication started"
        );

        Ok(Self {
            region,
            output_name: output.name,
            monitor: output.monitor,
            counter_frequency: performance_counter_frequency()?,
            session: Some(session),
            format,
            held: None,
            awaiting_resize: None,
            recovering_since: None,
            recovery_warned_at: None,
            reported_minimised: false,
            access_losses: 0,
        })
    }

    /// Returns the frame lent to the caller to DXGI.
    fn release_held_frame(&mut self) {
        self.held = None;
        if let Some(session) = self.session.as_mut() {
            session.release_frame();
        }
    }

    /// Borrows the held frame as the caller's [`CapturedFrame`].
    ///
    /// # Panics
    ///
    /// If no frame is held. Only [`Running::pump`] calls this, immediately after
    /// putting one there.
    fn lend_held_frame(&self) -> CapturedFrame<'_> {
        let held = self.held.as_ref().expect("a frame was just acquired");

        // SAFETY: `held.texture` is a live `ID3D11Texture2D`. It is either the
        // duplication's desktop image, which DXGI keeps valid until
        // `ReleaseFrame`, or this backend's own destination texture; in both
        // cases the `HeldFrame` holds an owning reference, so the refcount
        // cannot reach zero while it exists. The `HeldFrame` is owned by this
        // `Running`, which is owned by the backend; `CapturedFrame` borrows the
        // backend, and the only thing that clears `held` — and the only thing
        // that calls `ReleaseFrame` or overwrites the destination — is
        // `release_held_frame`, which `acquire` calls behind `&mut self` and
        // therefore only when no frame is outstanding. So the texture outlives
        // the returned frame and its contents do not change underneath it.
        let texture =
            unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, held.texture.as_raw()) };

        CapturedFrame::new(texture, self.format, held.timestamp)
            .with_frames_missed(held.frames_missed)
    }

    /// Waits for a frame, recovering from access loss, until `deadline`.
    ///
    /// This is the whole acquisition loop, and it is a loop rather than a single
    /// attempt because three ordinary things produce no frame for the caller:
    /// DXGI reports a pointer-only update, the duplication has to be rebuilt, or
    /// the target window is not currently somewhere this output can show it.
    fn pump(&mut self, deadline: Instant) -> Result<Acquisition<'_>, CaptureError> {
        loop {
            // The one place the caller's timeout is honoured unconditionally.
            // Rebuilding a duplication and following a window between displays
            // both take real time and neither is bounded by anything else here,
            // and a capture thread that stays inside `acquire` past its timeout
            // is a capture thread that has stopped answering a stop request
            // (AGENTS.md section 20).
            if Instant::now() >= deadline {
                return Ok(Acquisition::Timeout);
            }

            if self.session.is_none() {
                match self.reinitialise() {
                    Ok(Some(size)) => return Ok(Acquisition::SizeChanged(size)),
                    Ok(None) => {}
                    Err(Recovery::TargetGone) => {
                        return Err(CaptureError::TargetLost { method: METHOD })
                    }
                    Err(Recovery::NotYet) => {
                        if !sleep_until(REBUILD_RETRY_INTERVAL, deadline) {
                            return Ok(Acquisition::Timeout);
                        }
                        continue;
                    }
                }
            }

            match self.check_target()? {
                TargetState::Ready => {}
                TargetState::Idle => {
                    // Reported as a minimised target rather than as an ordinary
                    // timeout once the caller's whole wait has been spent, so
                    // that a session can say why its recording is accumulating
                    // nothing (issue #383). The pacing is unchanged: the sleeps
                    // and the deadline are the ones that were already here.
                    if !sleep_until(REBUILD_RETRY_INTERVAL, deadline) {
                        return Ok(Acquisition::TargetMinimised);
                    }
                    continue;
                }
                TargetState::MovedOutput => {
                    self.discard_session();
                    continue;
                }
                TargetState::Resized(size) => {
                    self.awaiting_resize = Some(size);
                    tracing::info!(
                        width = size.width(),
                        height = size.height(),
                        "the captured window changed size; waiting for the caller to resize"
                    );
                    return Ok(Acquisition::SizeChanged(size));
                }
            }

            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(Acquisition::Timeout);
            };

            match self.take_next_frame(remaining.min(ACQUIRE_SLICE))? {
                Taken::Frame => return Ok(Acquisition::Frame(self.lend_held_frame())),
                Taken::Nothing => {
                    if Instant::now() >= deadline {
                        return Ok(Acquisition::Timeout);
                    }
                }
                Taken::AccessLost(reason) => {
                    self.access_losses += 1;
                    tracing::warn!(
                        display = self.output_name.as_str(),
                        %reason,
                        losses = self.access_losses,
                        "Desktop Duplication lost access to the display; rebuilding it \
                         without ending the recording"
                    );
                    self.discard_session();
                }
            }
        }
    }

    /// Takes one frame from the duplication, waiting at most `timeout`.
    fn take_next_frame(&mut self, timeout: Duration) -> Result<Taken, CaptureError> {
        // Read before the session is borrowed: everything below runs against
        // `session`, and reaching back through `self` while it is borrowed
        // mutably would not compile.
        let region = self.region;
        let counter_frequency = self.counter_frequency;
        let session = self
            .session
            .as_mut()
            .expect("pump rebuilds the session before asking for a frame");

        // A duplication whose last frame could not be given back is finished,
        // whatever the reason was; asking it for another frame only produces
        // `DXGI_ERROR_INVALID_CALL` for the rest of the recording.
        if let Some(error) = session.release_failure.take() {
            return Ok(Taken::AccessLost(error));
        }

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);

        // SAFETY: both out parameters are live locals of the types the signature
        // names, and the duplication is a live interface owned by this session.
        // On success `resource` holds one reference, released when it drops, and
        // the frame it names is outstanding until `ReleaseFrame` — which
        // `Session::release_frame` makes, driven by the `frame_held` flag set
        // immediately below.
        let acquired = unsafe {
            session
                .duplication
                .AcquireNextFrame(milliseconds, &raw mut info, &raw mut resource)
        };

        match acquired {
            Ok(()) => session.frame_held = true,
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Taken::Nothing),
            Err(error) if is_access_lost(error.code()) => {
                return Ok(Taken::AccessLost(error));
            }
            Err(error) => return Err(backend_error("acquiring the next duplicated frame", error)),
        }

        // Nothing new to record. DXGI wakes an acquisition for a pointer move as
        // well as for a desktop update, and reports the difference in these two
        // fields: `AccumulatedFrames` is zero when only the pointer changed, and
        // `LastPresentTime` is zero when nothing has been presented since the
        // duplication was created. Delivering either would be delivering the
        // previous frame again, with no timestamp of its own to carry.
        if info.AccumulatedFrames == 0 || info.LastPresentTime <= 0 {
            session.release_frame();
            return Ok(Taken::Nothing);
        }

        let Some(resource) = resource else {
            session.release_frame();
            return Err(backend_error(
                "acquiring the next duplicated frame",
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "AcquireNextFrame reported success without returning a surface",
                ),
            ));
        };

        let desktop: ID3D11Texture2D = resource
            .cast()
            .map_err(|error| backend_error("reading the duplicated desktop texture", error))?;

        let timestamp = CaptureTimestamp::from_performance_counter(
            info.LastPresentTime.unsigned_abs(),
            counter_frequency,
        );
        // `AccumulatedFrames` counts the updates coalesced into this one,
        // including it, so one means the caller kept up. This is a real
        // dropped-frame count from the source, not an estimate: the Windows
        // Graphics Capture backend has to derive its figure from gaps between
        // timestamps because that API reports nothing of the kind.
        let frames_missed = info.AccumulatedFrames.saturating_sub(1);

        let texture = match region {
            Region::WholeOutput => {
                // Zero copy: the caller is handed DXGI's own desktop image, and
                // the frame stays outstanding until the next acquisition.
                desktop
            }
            Region::Window(window) => {
                let Some(client) = client_rect_in_desktop(window) else {
                    session.release_frame();
                    return Ok(Taken::Nothing);
                };

                let copied = session.copy_out(&desktop, client);
                // The desktop image is DXGI's to reuse the moment the copy is
                // queued: Direct3D 11 keeps the source alive for as long as the
                // command needs it, which is why the frame can be released here
                // rather than being held for the length of the caller's encode.
                session.release_frame();
                match copied? {
                    Some(texture) => texture,
                    // The window is not on this output at all, which is what a
                    // display change in progress looks like from here.
                    None => return Ok(Taken::Nothing),
                }
            }
        };

        self.held = Some(HeldFrame {
            texture,
            timestamp,
            frames_missed,
        });
        Ok(Taken::Frame)
    }

    /// Answers whether the target is somewhere a frame can be taken from.
    fn check_target(&mut self) -> Result<TargetState, CaptureError> {
        let Region::Window(window) = self.region else {
            return Ok(TargetState::Ready);
        };

        if !is_window(window) {
            return Err(CaptureError::TargetLost { method: METHOD });
        }

        // A minimised window has a client area, and Windows will happily report
        // its position — off the bottom of the virtual desktop, at around
        // (-32000, -32000). Cropping to it would record a rectangle of nothing.
        // SAFETY: `IsIconic` reads the window's state and is defined for any
        // handle; this one named a window a moment ago.
        if unsafe { IsIconic(window) }.as_bool() {
            if !self.reported_minimised {
                self.reported_minimised = true;
                tracing::info!(
                    "the captured window is minimised, so there is nothing on the display \
                     to crop; capture resumes when it is restored"
                );
            }
            return Ok(TargetState::Idle);
        }
        self.reported_minimised = false;

        let Some(monitor) = monitor_for_window(window) else {
            return Err(CaptureError::TargetLost { method: METHOD });
        };
        if monitor != self.monitor {
            // The window has been dragged to another display. Windows' own
            // answer to "which display is it on?" is the one showing most of it,
            // so this fires as the majority crosses the boundary, and the
            // duplication is rebuilt against the new output.
            tracing::info!(
                from = self.output_name.as_str(),
                "the captured window moved to another display; duplicating that one instead"
            );
            self.monitor = monitor;
            return Ok(TargetState::MovedOutput);
        }

        let Some(size) = client_size(window) else {
            return Err(CaptureError::TargetLost { method: METHOD });
        };
        if size != self.format.size() {
            return Ok(TargetState::Resized(size));
        }

        Ok(TargetState::Ready)
    }

    /// Releases the duplication so that the next acquisition rebuilds it.
    fn discard_session(&mut self) {
        self.held = None;
        self.session = None;
        if self.recovering_since.is_none() {
            self.recovering_since = Some(Instant::now());
            self.recovery_warned_at = None;
        }
    }

    /// Builds a new duplication for the target, after access was lost or the
    /// window moved to another display.
    ///
    /// Returns the new frame size if it differs from the one the caller was last
    /// told, which is what a mode change looks like from here.
    fn reinitialise(&mut self) -> Result<Option<FrameSize>, Recovery> {
        // Where is the window *now*? A window target's display is asked for
        // again rather than remembered, because the events this function exists
        // to recover from are the events that move windows: removing a display
        // — a DisplayPort monitor being switched off is the everyday version —
        // invalidates every `HMONITOR` and makes Windows relocate the windows
        // that were on it to a surviving display. Looking for the remembered
        // display would spend the absence grace failing to find a monitor that
        // has gone and then end a recording whose window is still on screen and
        // still capturable (AGENTS.md section 16).
        let mut name = Some(self.output_name.clone());
        if let Region::Window(window) = self.region {
            if !is_window(window) {
                return Err(Recovery::TargetGone);
            }
            // `None` for a minimised window, which is on no display at all; the
            // remembered display is the right guess until it is restored.
            if let Some(monitor) = monitor_for_window(window) {
                if monitor != self.monitor {
                    tracing::info!(
                        was = self.output_name.as_str(),
                        "the captured window is no longer on the display being duplicated; \
                         duplicating the one it is on now"
                    );
                    self.monitor = monitor;
                }
                // Deliberately no fallback to the remembered name either. That
                // fallback is there for a handle invalidated by a display
                // change, and a handle read a line ago cannot be one; what the
                // name would match is the display the window may have just been
                // moved off, which would rebuild the duplication there and
                // record the wrong display.
                name = None;
            }
        }

        let output = match find_output(Some(self.monitor), name.as_deref()) {
            Ok(Some(output)) => output,
            // Every other display is there, but not this one. That is what an
            // unplugged monitor looks like — and also what one looks like for a
            // moment in the middle of the topology change that took access away,
            // so it only ends the recording once it has stayed away.
            Ok(None) => {
                if self.absent_for() >= OUTPUT_ABSENCE_GRACE {
                    return Err(Recovery::TargetGone);
                }
                self.warn_recovery_failed("the display is not attached to the desktop");
                return Err(Recovery::NotYet);
            }
            Err(error) => {
                self.warn_recovery_failed(&error.to_string());
                return Err(Recovery::NotYet);
            }
        };

        // Every failure here is retried for as long as the display is attached,
        // however long that turns out to be, and that is deliberate. The
        // tempting alternative — give up after a while — ends a recording
        // because a UAC prompt was on screen for six seconds, which is exactly
        // the case `DXGI_ERROR_ACCESS_DENIED` describes and exactly the case a
        // game recorder must survive. The one failure that genuinely cannot be
        // retried away is a display that has been rotated mid-recording, which
        // is refused every time until it is rotated back; that is
        // [issue #138](https://github.com/wildware-uk/clipped/issues/138), and
        // until then the warning below is what says so.
        let session = match Session::open(&output, self.region) {
            Ok(session) => session,
            Err(error) => {
                self.warn_recovery_failed(&error.to_string());
                return Err(Recovery::NotYet);
            }
        };

        let size = match self.region {
            Region::WholeOutput => session.size,
            Region::Window(window) => match client_size(window) {
                Some(client) => client,
                None => return Err(Recovery::TargetGone),
            },
        };

        let outage = self.absent_for();
        tracing::info!(
            display = output.name.as_str(),
            adapter = output.adapter_name.as_str(),
            width = size.width(),
            height = size.height(),
            outage_ms = outage.as_millis(),
            "Desktop Duplication reinitialised; the recording continues"
        );

        self.output_name = output.name;
        self.monitor = output.monitor;
        self.session = Some(session);
        self.recovering_since = None;
        self.recovery_warned_at = None;

        if size == self.format.size() {
            return Ok(None);
        }
        self.awaiting_resize = Some(size);
        Ok(Some(size))
    }

    /// How long capture has been without a duplication.
    fn absent_for(&self) -> Duration {
        self.recovering_since
            .map(|since| since.elapsed())
            .unwrap_or_default()
    }

    /// Logs that recovery has not succeeded yet, at most every
    /// [`RECOVERY_WARNING_INTERVAL`].
    fn warn_recovery_failed(&mut self, reason: &str) {
        let now = Instant::now();
        let due = self
            .recovery_warned_at
            .is_none_or(|last| now.duration_since(last) >= RECOVERY_WARNING_INTERVAL);
        if !due {
            return;
        }
        self.recovery_warned_at = Some(now);
        tracing::warn!(
            display = self.output_name.as_str(),
            outage_ms = self.absent_for().as_millis(),
            reason,
            "Desktop Duplication cannot reach the display yet; still trying"
        );
    }

    /// Accepts the size an acquisition reported, reconfiguring the crop.
    fn adopt_size(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError> {
        self.release_held_frame();
        self.format = FrameFormat::new(size, PixelFormat::Bgra8Unorm);
        self.awaiting_resize = None;

        if let (Region::Window(_), Some(session)) = (self.region, self.session.as_mut()) {
            session.destination = Some(Destination::create(&session.device, size)?);
        }

        Ok(self.format)
    }
}

/// What one attempt to take a frame produced.
enum Taken {
    /// A frame, now held in [`Running::held`].
    Frame,
    /// Nothing this acquisition can use: the wait expired, or DXGI woke it for a
    /// pointer move rather than a desktop update.
    Nothing,
    /// The duplication is no longer valid and has to be rebuilt.
    AccessLost(windows::core::Error),
}

/// Whether the target is somewhere a frame can be cropped from.
enum TargetState {
    /// Go ahead.
    Ready,
    /// The target exists but is not on screen — minimised — so there is nothing
    /// to crop until it comes back.
    Idle,
    /// The window is now on a different display; the duplication has to follow
    /// it.
    MovedOutput,
    /// The window's client area is a different size, which the caller has to be
    /// told about before any more frames are produced.
    Resized(FrameSize),
}

/// Why a rebuild did not produce a duplication.
enum Recovery {
    /// The display or window has gone for good; the recording is over.
    TargetGone,
    /// Not yet — a transition is probably still in progress. Try again.
    NotYet,
}

/// Everything access loss throws away.
///
/// Kept apart from [`Running`] precisely so that "release everything and start
/// again" is `self.session = None` followed by `Session::open`, rather than a
/// list of fields somebody has to remember to reset.
struct Session {
    /// The Direct3D 11 device, created on the adapter that owns this output —
    /// which is not necessarily the default adapter on a machine with more than
    /// one GPU, and `DuplicateOutput` refuses a device from the wrong one.
    device: DuplicationDevice,
    /// The duplication itself. Dropping it is what lets other applications
    /// duplicate this output again.
    duplication: IDXGIOutputDuplication,
    /// Whether a frame is outstanding and still needs `ReleaseFrame`.
    frame_held: bool,
    /// The size of the duplicated image.
    size: FrameSize,
    /// The output's top-left corner in virtual-desktop coordinates, which is
    /// what a window's screen position has to be measured against.
    origin: (i32, i32),
    /// Where a cropped frame is copied to. [`None`] for a whole-output capture,
    /// which needs no copy at all.
    destination: Option<Destination>,
    /// The failure `ReleaseFrame` reported, if it reported one.
    ///
    /// This is not bookkeeping, it is the difference between a recording that
    /// survives a mode change and one that stops. A `ReleaseFrame` that fails
    /// leaves DXGI believing the frame is still outstanding, and every later
    /// `AcquireNextFrame` on the same duplication answers
    /// `DXGI_ERROR_INVALID_CALL` — for ever, because the frame it is waiting for
    /// can never be given back. Measured on Windows 11 build 26200: changing
    /// `\\.\DISPLAY1` from 2560x1440 to 1280x720 mid-capture made `ReleaseFrame`
    /// fail, and the very next acquisition reported `0x887A0001` rather than the
    /// `DXGI_ERROR_ACCESS_LOST` the recovery path is watching for, so the
    /// recording ended at the display change instead of surviving it.
    ///
    /// So the failure is remembered, and the next acquisition treats it as what
    /// it is: this duplication is finished, and a new one has to be built.
    release_failure: Option<windows::core::Error>,
}

impl Session {
    /// Duplicates `output` and prepares whatever the region needs.
    fn open(output: &Output, region: Region) -> Result<Self, CaptureError> {
        // A rotated output hands over its desktop image in the *unrotated*
        // orientation, so a portrait display duplicates as a landscape image
        // that an application is expected to rotate itself. Recording that
        // sideways would be a silently wrong recording, and cropping a window
        // out of it would be worse: the window's coordinates are in the rotated
        // desktop's space and would name the wrong pixels. Refusing is the
        // honest answer until issue #138 adds the rotation
        // (AGENTS.md section 54).
        if !is_upright(output.rotation) {
            return Err(CaptureError::UnsupportedTarget {
                method: METHOD,
                target: match region {
                    Region::WholeOutput => TargetKind::Monitor,
                    Region::Window(_) => TargetKind::Window,
                },
                reason: "the display is rotated, and Desktop Duplication hands over a \
                         rotated display's image unrotated, so the recording would be \
                         sideways",
            });
        }

        let device = DuplicationDevice::create(&output.adapter)?;

        // SAFETY: `output.output` is a live `IDXGIOutput1` from the enumeration
        // in `find_output`, and `device` is a Direct3D 11 device created on that
        // output's own adapter immediately above, which is what
        // `DuplicateOutput` requires. The returned duplication is an owned
        // reference released when this `Session` drops.
        let duplication =
            unsafe { output.output.DuplicateOutput(device.device()) }.map_err(duplication_error)?;

        // SAFETY: `GetDesc` writes a `DXGI_OUTDUPL_DESC` through a pointer
        // windows-rs supplies from its own stack slot; it takes nothing from
        // this side and cannot fail.
        let description = unsafe { duplication.GetDesc() };
        let size = FrameSize::new(description.ModeDesc.Width, description.ModeDesc.Height).ok_or(
            CaptureError::UnsupportedTarget {
                method: METHOD,
                target: TargetKind::Monitor,
                reason: "the display reports a zero-sized mode, so there is nothing to \
                         duplicate",
            },
        )?;

        if description.DesktopImageInSystemMemory.as_bool() {
            // Not fatal — the texture is still a texture — but it means the
            // desktop is being composed without a graphics driver's help, which
            // is worth knowing before anybody asks why capture is slow.
            tracing::info!(
                display = output.name.as_str(),
                "this display's desktop image lives in system memory, which usually means a \
                 basic display driver rather than a real one"
            );
        }

        if matches!(region, Region::Window(_)) && output.desktop_bounds.width != size.width() {
            // The duplicated image is in physical pixels and the window
            // positions this backend crops with are in whatever unit Windows
            // reports to this process. They are the same unit only in a process
            // that has declared itself DPI aware, which is what
            // `clipped_windows::enable_per_monitor_dpi_awareness` is for and
            // what a recorder calls once at start-up. Without it the crop is
            // silently in the wrong place on a scaled display, so say so rather
            // than let somebody find it in a recording.
            tracing::warn!(
                display = output.name.as_str(),
                desktop_width = output.desktop_bounds.width,
                image_width = size.width(),
                "this display's desktop rectangle and its duplicated image are different \
                 widths, which means this process is not DPI aware; a cropped capture will \
                 be taken from the wrong part of the screen"
            );
        }

        let destination = match region {
            Region::WholeOutput => None,
            Region::Window(window) => Some(Destination::create(
                &device,
                client_size(window).ok_or(CaptureError::TargetLost { method: METHOD })?,
            )?),
        };

        Ok(Self {
            device,
            duplication,
            frame_held: false,
            size,
            origin: (output.desktop_bounds.left, output.desktop_bounds.top),
            destination,
            release_failure: None,
        })
    }

    /// The output's rectangle in virtual-desktop coordinates.
    fn desktop_bounds(&self) -> DesktopRect {
        DesktopRect {
            left: self.origin.0,
            top: self.origin.1,
            width: self.size.width(),
            height: self.size.height(),
        }
    }

    /// Copies the part of `desktop` the window at `client` covers into this
    /// session's destination texture, and returns the texture the caller will be
    /// handed.
    ///
    /// [`None`] when the window is not on this output at all, which is a frame
    /// there is nothing to make.
    fn copy_out(
        &mut self,
        desktop: &ID3D11Texture2D,
        client: DesktopRect,
    ) -> Result<Option<ID3D11Texture2D>, CaptureError> {
        let destination = self
            .destination
            .as_ref()
            .expect("a window capture always has a destination texture");

        // Worked out here, against the destination texture's own size, rather
        // than passed in: the invariant the copy below depends on is a fact
        // about *this* texture, and it stops being one the moment the size it is
        // checked against comes from somewhere else. The window's size and the
        // destination's disagree for as long as it takes a caller to act on a
        // reported size change, and a window being drag-resized changes size
        // inside a single acquisition.
        let Some(placement) =
            place_window_in_output(self.desktop_bounds(), client, destination.size)
        else {
            return Ok(None);
        };

        if placement.partial {
            // Only part of the frame is covered — because the window is partly
            // on another display, or because it has shrunk since the frame was
            // sized. Whatever was copied there for a previous frame is still in
            // the texture, and leaving it would record a stale strip of the
            // window that scrolls as the window is dragged, so the uncovered
            // part is painted black first. This costs a clear only while the
            // window does not fill its frame.
            //
            // SAFETY: `view` is a render target view of `destination.texture`,
            // both created by this session's device, and the context is that
            // device's immediate context, used only from the capture thread.
            unsafe {
                self.device
                    .context()
                    .ClearRenderTargetView(&destination.view, &[0.0, 0.0, 0.0, 1.0]);
            }
        }

        // SAFETY: both textures belong to this session's device — `desktop` is
        // the duplication's image, which is created on the device the
        // duplication was made with — and the box is inside the source, which
        // `place_window_in_output` clamps to the output's own bounds; the
        // duplicated image is the output's size. The destination offsets plus
        // the box's extent are inside the destination, because the placement was
        // computed immediately above from `destination.size`, which is the size
        // that texture was created at, and the clamp against it is what that
        // function guarantees. The immediate context is used only from the
        // capture thread.
        unsafe {
            self.device.context().CopySubresourceRegion(
                &destination.texture,
                0,
                placement.destination.0,
                placement.destination.1,
                0,
                desktop,
                0,
                Some(&raw const placement.source),
            );
        }

        Ok(Some(destination.texture.clone()))
    }

    /// Gives an outstanding frame back to DXGI.
    ///
    /// A failure is recorded rather than returned, because this is called from
    /// paths that cannot fail — `Drop`, and the start of an acquisition — but it
    /// is emphatically not ignored: see [`Session::release_failure`] for what
    /// happens to a duplication whose frame could not be given back.
    fn release_frame(&mut self) {
        if !self.frame_held {
            return;
        }
        self.frame_held = false;
        // SAFETY: the duplication is live and has exactly one frame outstanding,
        // which is what `frame_held` records and what `ReleaseFrame` requires.
        if let Err(error) = unsafe { self.duplication.ReleaseFrame() } {
            self.release_failure.get_or_insert(error);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The outstanding frame first: releasing the duplication with a frame
        // still held is exactly the state that leaves an output un-duplicable
        // for other applications.
        self.release_frame();
        // Everything else — the duplication, the destination texture and the
        // device — is released by its own `Drop` after this body returns.
    }
}

/// The texture a cropped capture is copied into, and the view used to clear it.
struct Destination {
    /// The texture handed to the caller as the frame.
    texture: ID3D11Texture2D,
    /// A render target view of it, so that the parts of the frame no window
    /// covers can be painted black.
    view: ID3D11RenderTargetView,
    /// The size the texture was created at.
    ///
    /// Kept here rather than read back from the texture or taken from the frame
    /// format, because it is what every copy into this texture has to be clamped
    /// to and the window it is a crop of can be a different size at any moment.
    size: FrameSize,
}

impl Destination {
    /// Creates a `size` destination on `device`.
    fn create(device: &DuplicationDevice, size: FrameSize) -> Result<Self, CaptureError> {
        let description = D3D11_TEXTURE2D_DESC {
            Width: size.width(),
            Height: size.height(),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            // Render target so the uncovered part of a straddling window can be
            // cleared; shader resource because that is what an encoder binds.
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `description` is a live local describing a texture with no
        // initial data, and the out parameter is the representation windows-rs
        // uses for one of that type. On success it holds one reference, released
        // when this `Destination` drops.
        unsafe {
            device
                .device()
                .CreateTexture2D(&raw const description, None, Some(&raw mut texture))
        }
        .map_err(|error| backend_error("creating the cropped frame texture", error))?;

        let texture = texture.ok_or_else(|| {
            backend_error(
                "creating the cropped frame texture",
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "CreateTexture2D reported success without returning a texture",
                ),
            )
        })?;

        let mut view: Option<ID3D11RenderTargetView> = None;
        // SAFETY: `texture` was created immediately above with
        // `D3D11_BIND_RENDER_TARGET`, which is what a render target view of it
        // requires; a null description means "the whole resource, in its own
        // format".
        unsafe {
            device
                .device()
                .CreateRenderTargetView(&texture, None, Some(&raw mut view))
        }
        .map_err(|error| backend_error("creating a view of the cropped frame texture", error))?;

        let view = view.ok_or_else(|| {
            backend_error(
                "creating a view of the cropped frame texture",
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "CreateRenderTargetView reported success without returning a view",
                ),
            )
        })?;

        Ok(Self {
            texture,
            view,
            size,
        })
    }
}

/// The Direct3D 11 device a duplication is made with.
///
/// Not `CaptureDevice` from `device.rs`, and the difference is not cosmetic.
/// That one is created on the *default* adapter and carries a WinRT view for
/// `Direct3D11CaptureFramePool`; this one has to be
/// created on the adapter that owns the output being duplicated, because
/// `DuplicateOutput` rejects a device from any other adapter — on a machine with
/// a discrete and an integrated GPU, which is most laptops and this project's
/// own development machine, the default adapter is frequently the wrong one. It
/// also needs the immediate context, which cropping copies through and which
/// Windows Graphics Capture never touches.
struct DuplicationDevice {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

impl DuplicationDevice {
    /// Creates a hardware device on `adapter`.
    fn create(adapter: &IDXGIAdapter1) -> Result<Self, CaptureError> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;

        // SAFETY: the adapter is a live interface from the enumeration in
        // `find_output`. `D3D_DRIVER_TYPE_UNKNOWN` is what the API requires when
        // an adapter is named — passing a driver type as well is the documented
        // way to get `E_INVALIDARG`. The module handle is null, which is
        // required unless the driver type is the software rasteriser; no feature
        // level list is requested and no feature level is returned. Both out
        // parameters are live locals of the types windows-rs projects for them,
        // and each holds one reference on success.
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                Some(&raw mut context),
            )
        }
        .map_err(|error| backend_error("creating the Direct3D 11 device", error))?;

        match (device, context) {
            (Some(device), Some(context)) => Ok(Self { device, context }),
            _ => Err(backend_error(
                "creating the Direct3D 11 device",
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "D3D11CreateDevice reported success without returning a device",
                ),
            )),
        }
    }

    /// The device, for duplication and for creating textures.
    const fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// The immediate context, used only from the capture thread.
    const fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }
}

/// One display output, and the adapter it hangs off.
struct Output {
    /// The output, for `DuplicateOutput`.
    output: IDXGIOutput1,
    /// The adapter the output is attached to, which the Direct3D device has to
    /// be created on.
    adapter: IDXGIAdapter1,
    /// The display device name, as in `\\.\DISPLAY1`.
    name: String,
    /// The adapter's description, for the log.
    adapter_name: String,
    /// The monitor handle this output currently answers to.
    monitor: HMONITOR,
    /// The output's rectangle in virtual-desktop coordinates.
    desktop_bounds: DesktopRect,
    /// How the display is rotated.
    rotation: DXGI_MODE_ROTATION,
}

/// A rectangle in virtual-desktop coordinates.
///
/// Plain integers rather than a `RECT`, because the cropping arithmetic that
/// consumes it is the one part of this backend that can be tested without a
/// display, and it should not need a Windows type to do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DesktopRect {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

/// Where a window's client area sits inside one output's duplicated image.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placement {
    /// The region of the duplicated image to copy out.
    source: D3D11_BOX,
    /// Where it lands in the frame: `(x, y)` in the destination texture.
    destination: (u32, u32),
    /// Whether the copy covers less than the whole frame, which happens when the
    /// window straddles two displays or is smaller than the frame it is being
    /// copied into.
    partial: bool,
}

/// Works out what to copy where, for a window at `client` on an output at
/// `output`, into a destination texture of size `frame`.
///
/// The frame is the size the caller was last told, whatever the window is doing,
/// so that a window being dragged does not resize the encoder every few pixels.
/// What changes is how much of it this output can supply:
///
/// - Entirely on this output and the size of the frame: the whole frame is
///   copied, `partial` is false.
/// - Straddling two outputs: only the part on this one is copied, into the
///   matching corner of the frame, and `partial` is true so the caller knows to
///   clear the rest. Which output is "this" one is Windows' own answer — the
///   display showing most of the window — so the majority of the window is
///   always the part that is recorded, and the strip hanging over the edge is
///   black rather than stale or stretched.
/// - Entirely off this output: [`None`], and the caller waits for the display
///   change that is presumably in progress.
///
/// Returning [`None`] rather than an empty copy matters: an empty
/// `D3D11_BOX` is undefined behaviour as far as Direct3D is concerned, and a
/// frame of black would be a frame the recording claims to have captured.
///
/// # The frame and the window can disagree, and this is what makes that safe
///
/// `client` is read fresh for every acquisition; `frame` is the size the
/// destination texture was created at, which only changes when the caller acts
/// on an [`Acquisition::SizeChanged`]. Between those two things is a window
/// being drag-resized: `check_target` reads its size, `AcquireNextFrame` then
/// blocks for up to [`ACQUIRE_SLICE`], and the client rectangle is read again
/// afterwards. So the copy is clamped to the frame as well as to the output, and
/// the result is guaranteed to satisfy
/// `destination.0 + width <= frame.width()` and
/// `destination.1 + height <= frame.height()`.
///
/// That guarantee is not tidiness: Direct3D documents a
/// `CopySubresourceRegion` that writes outside the destination resource as
/// *undefined behaviour*, and a window that has grown since the frame was sized
/// would ask for exactly that. A window that has *shrunk* copies less than the
/// whole frame, which is `partial`, so the uncovered part is cleared rather than
/// left holding the old edges of the window.
fn place_window_in_output(
    output: DesktopRect,
    client: DesktopRect,
    frame: FrameSize,
) -> Option<Placement> {
    // In the output's own coordinates, which is what the duplicated image is
    // in, and in 64 bits because a window remembered from a display that has
    // since been unplugged can be an enormous distance from this one.
    let left = i64::from(client.left) - i64::from(output.left);
    let top = i64::from(client.top) - i64::from(output.top);

    let visible_left = left.max(0);
    let visible_top = top.max(0);
    // Three limits, and every one of them is real: the window's own extent, what
    // the output has pixels for, and what the destination texture has room for.
    let visible_right = (left + i64::from(client.width))
        .min(i64::from(output.width))
        .min(left + i64::from(frame.width()));
    let visible_bottom = (top + i64::from(client.height))
        .min(i64::from(output.height))
        .min(top + i64::from(frame.height()));

    if visible_right <= visible_left || visible_bottom <= visible_top {
        return None;
    }

    // Every value below is between zero and an output dimension, so each cast
    // is of a number that has just been clamped into `u32`'s range.
    let source = D3D11_BOX {
        left: visible_left as u32,
        top: visible_top as u32,
        front: 0,
        right: visible_right as u32,
        bottom: visible_bottom as u32,
        back: 1,
    };
    // Both are below the matching frame dimension: `visible_left` is less than
    // `visible_right`, which is at most `left + frame.width()`, so their
    // difference from `left` is less than `frame.width()`.
    let destination = ((visible_left - left) as u32, (visible_top - top) as u32);
    // Measured against the frame rather than the window: what has to be cleared
    // first is the part of the *destination* this copy will not reach.
    let partial =
        source.right - source.left != frame.width() || source.bottom - source.top != frame.height();

    Some(Placement {
        source,
        destination,
        partial,
    })
}

/// Finds the DXGI output for a monitor, by handle and then by name.
///
/// Both are needed. The handle is exact, and it is what a caller has; the name
/// is what survives the display change that took the duplication away in the
/// first place, because Windows invalidates every `HMONITOR` when displays are
/// attached, detached or rearranged.
///
/// Returns `Ok(None)` when the system has outputs but none of them is this one —
/// a display that has been unplugged — and an error when there is nothing to
/// enumerate at all.
///
/// # Errors
///
/// [`CaptureError::UnsupportedTarget`] when no adapter reports any output, which
/// is what a remote session, a headless server and a virtual machine with no
/// display look like, and [`CaptureError::Backend`] if DXGI itself fails.
fn find_output(
    monitor: Option<HMONITOR>,
    name: Option<&str>,
) -> Result<Option<Output>, CaptureError> {
    // SAFETY: `CreateDXGIFactory1` takes no arguments beyond the interface it is
    // asked for, which the type parameter supplies, and returns an owned
    // reference windows-rs releases on drop.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
        .map_err(|error| backend_error("creating the DXGI factory", error))?;

    let mut by_name = None;
    let mut any_output = false;
    let mut undescribed = 0_u32;

    for adapter_index in 0.. {
        // SAFETY: the factory is live, and the index is checked by the call
        // itself: `DXGI_ERROR_NOT_FOUND` is how it says the enumeration is over.
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(backend_error("enumerating display adapters", error)),
        };

        // SAFETY: `adapter` is live, and `GetDesc` writes into a slot windows-rs
        // owns and returns by value.
        let adapter_name = unsafe { adapter.GetDesc() }
            .map(|description| {
                String::from_utf16_lossy(&description.Description)
                    .trim_end_matches('\0')
                    .to_owned()
            })
            .unwrap_or_else(|_| "unknown".to_owned());

        for output_index in 0.. {
            // SAFETY: as above; `DXGI_ERROR_NOT_FOUND` ends the enumeration.
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(backend_error("enumerating display outputs", error)),
            };

            // SAFETY: `output` is live and `GetDesc` returns a description by
            // value, borrowing nothing.
            let description: DXGI_OUTPUT_DESC = match unsafe { output.GetDesc() } {
                Ok(description) => description,
                // An output that will not say where it is or what it is called
                // cannot be matched against a monitor, cropped to, or
                // duplicated. Skipping it is the only thing left to do, but it
                // is not the same thing as there being no display attached, and
                // saying nothing would leave the machine where this happens with
                // an error that names the wrong cause (AGENTS.md section 15).
                Err(error) => {
                    undescribed += 1;
                    tracing::warn!(
                        adapter = adapter_name.as_str(),
                        output_index,
                        %error,
                        "a display output would not report its description, so it cannot be \
                         duplicated; ignoring it"
                    );
                    continue;
                }
            };
            if !description.AttachedToDesktop.as_bool() {
                continue;
            }
            any_output = true;

            let output: IDXGIOutput1 = match output.cast() {
                Ok(output) => output,
                // IDXGIOutput1 is Windows 8; a system without it cannot
                // duplicate anything, and there is nothing to fall back to.
                Err(error) => {
                    return Err(backend_error(
                        "asking a display output for its duplication interface",
                        error,
                    ))
                }
            };

            let found = Output {
                output,
                adapter: adapter.clone(),
                name: String::from_utf16_lossy(&description.DeviceName)
                    .trim_end_matches('\0')
                    .to_owned(),
                adapter_name: adapter_name.clone(),
                monitor: description.Monitor,
                desktop_bounds: rect_to_desktop(description.DesktopCoordinates),
                rotation: description.Rotation,
            };

            if monitor.is_some_and(|monitor| monitor == found.monitor) {
                return Ok(Some(found));
            }
            if by_name.is_none() && name.is_some_and(|name| name == found.name) {
                by_name = Some(found);
            }
        }
    }

    if let Some(found) = by_name {
        return Ok(Some(found));
    }
    if any_output {
        return Ok(None);
    }
    if undescribed > 0 {
        return Err(CaptureError::UnsupportedTarget {
            method: METHOD,
            target: TargetKind::Monitor,
            reason: "this machine's display outputs would not report their descriptions, so \
                     none of them can be duplicated; the warning logged for each one names \
                     what DXGI said",
        });
    }

    Err(CaptureError::UnsupportedTarget {
        method: METHOD,
        target: TargetKind::Monitor,
        reason: "no display output is attached to this desktop, so there is nothing to \
                 duplicate — a remote session, a headless machine or a virtual one with no \
                 display looks like this",
    })
}

/// Whether an `HRESULT` from a duplication means the duplication has to be
/// rebuilt.
///
/// `DXGI_ERROR_ACCESS_LOST` is the documented one, and it covers a mode change,
/// a full-screen transition, a driver reset and a desktop switch. The other two
/// are the same event seen from further away: a device removed or reset takes
/// the duplication with it, and a session disconnect — a user switch, a remote
/// desktop connection — ends the desktop this one was duplicating. All three are
/// answered the same way, by releasing everything and building it again, so they
/// are one question here.
fn is_access_lost(code: windows::core::HRESULT) -> bool {
    code == DXGI_ERROR_ACCESS_LOST
        || code == DXGI_ERROR_DEVICE_REMOVED
        || code == DXGI_ERROR_DEVICE_RESET
        || code == DXGI_ERROR_SESSION_DISCONNECTED
}

/// Whether a display is the right way up.
fn is_upright(rotation: DXGI_MODE_ROTATION) -> bool {
    rotation == DXGI_MODE_ROTATION_IDENTITY || rotation == DXGI_MODE_ROTATION_UNSPECIFIED
}

/// Turns a `DuplicateOutput` failure into an error that says what it means.
///
/// The codes it has for "no" mean quite different things to a user, and
/// `DXGI_ERROR_UNSUPPORTED` in particular is the one worth naming: it is what a
/// basic display adapter says, and "your display driver cannot do this" is a
/// fact somebody can act on.
fn duplication_error(error: windows::core::Error) -> CaptureError {
    let reason = match error.code() {
        DXGI_ERROR_UNSUPPORTED => Some(
            "this display adapter's driver does not support Desktop Duplication, which is \
             usually a basic or virtual display driver rather than a real one",
        ),
        DXGI_ERROR_NOT_CURRENTLY_AVAILABLE => Some(
            "Windows has already handed out as many duplications of this display as it \
             allows, so another application is using them",
        ),
        DXGI_ERROR_ACCESS_DENIED => Some(
            "Windows refused access to the display, which is what a secure desktop — a \
             sign-in screen or an elevation prompt — looks like from here",
        ),
        // Measured on Windows 11 build 26200: DXGI gives a *process* one
        // duplication per output, and a second `DuplicateOutput` for a display
        // this process is already duplicating answers `E_INVALIDARG`
        // (0x80070057) rather than anything DXGI-shaped. It is the one hard
        // limit this backend has found, so it is not left to reach a caller as
        // an unclassified backend failure that reads like "this machine cannot
        // duplicate a display".
        E_INVALIDARG => Some(
            "this process is already duplicating this display, and Windows allows a process \
             only one duplication of each display at a time",
        ),
        _ => None,
    };

    match reason {
        Some(reason) => CaptureError::UnsupportedTarget {
            method: METHOD,
            target: TargetKind::Monitor,
            reason,
        },
        None => backend_error("duplicating the display output", error),
    }
}

/// `QueryPerformanceFrequency`, which is fixed for the life of the system.
fn performance_counter_frequency() -> Result<NonZeroU64, CaptureError> {
    let mut frequency = 0_i64;
    // SAFETY: the out parameter is a live local, which is all the call requires;
    // it reports failure through its return value.
    unsafe { QueryPerformanceFrequency(&raw mut frequency) }
        .map_err(|error| backend_error("reading the performance counter frequency", error))?;

    NonZeroU64::new(frequency.unsigned_abs()).ok_or_else(|| {
        backend_error(
            "reading the performance counter frequency",
            windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "QueryPerformanceFrequency reported a frequency of zero",
            ),
        )
    })
}

/// Whether `window` still names a window.
fn is_window(window: HWND) -> bool {
    // SAFETY: `IsWindow` only reads the window table, and reporting that a
    // handle is no longer a window is exactly what it exists for; passing a
    // stale handle is sound and is the case being asked about.
    unsafe { IsWindow(Some(window)) }.as_bool()
}

/// The display showing most of `window`, or [`None`] if it is not on one.
///
/// `MONITOR_DEFAULTTONULL` rather than the nearest display: a window that is on
/// no display at all — which is where Windows parks a minimised one — should
/// answer "nowhere" rather than name a display it is not on and have this
/// backend rebuild the duplication for it.
fn monitor_for_window(window: HWND) -> Option<HMONITOR> {
    // SAFETY: `MonitorFromWindow` takes a handle by value and is defined for an
    // arbitrary one, so there is no precondition to establish; the result is
    // checked rather than assumed.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONULL) };
    (!monitor.is_invalid()).then_some(monitor)
}

/// A window's client area in virtual-desktop coordinates.
fn client_rect_in_desktop(window: HWND) -> Option<DesktopRect> {
    let size = client_size(window)?;

    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: `origin` is a live local and the handle named a window when
    // `client_size` measured it; `ClientToScreen` writes the point in place and
    // reports failure through its return value.
    if !unsafe { ClientToScreen(window, &raw mut origin) }.as_bool() {
        return None;
    }

    Some(DesktopRect {
        left: origin.x,
        top: origin.y,
        width: size.width(),
        height: size.height(),
    })
}

/// A Windows `RECT` as a [`DesktopRect`].
fn rect_to_desktop(rect: RECT) -> DesktopRect {
    DesktopRect {
        left: rect.left,
        top: rect.top,
        width: rect.right.saturating_sub(rect.left).unsigned_abs(),
        height: rect.bottom.saturating_sub(rect.top).unsigned_abs(),
    }
}

/// Sleeps for `interval`, or until `deadline`, and says whether there is any
/// time left afterwards.
///
/// Sleeping on a capture thread is not something to do lightly (AGENTS.md
/// section 20). It happens on exactly two paths, both of which have no frame to
/// wait for: a duplication that could not be rebuilt yet, and a target that is
/// not on screen. The alternative is spinning on `DuplicateOutput` for the
/// length of a display transition.
fn sleep_until(interval: Duration, deadline: Instant) -> bool {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return false;
    };
    if remaining.is_zero() {
        return false;
    }
    thread::sleep(interval.min(remaining));
    Instant::now() < deadline
}

/// Wraps a Windows failure as a [`CaptureError::Backend`] naming what was being
/// attempted.
fn backend_error(operation: &'static str, error: windows::core::Error) -> CaptureError {
    CaptureError::Backend {
        method: METHOD,
        operation,
        source: Box::new(error),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::{Mutex, PoisonError};

    use clipped_windows::{enumerate_monitors, MonitorInfo};
    use windows::core::{w, Interface as _, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, CreateSolidBrush, EnumDisplaySettingsW, FillRect, GetDC,
        ReleaseDC, CDS_FULLSCREEN, CDS_TYPE, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
        ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, HBRUSH,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
        PeekMessageW, RegisterClassW, SetWindowPos, ShowWindow, TranslateMessage, MSG, PM_REMOVE,
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_NOACTIVATE,
        WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };

    use super::*;
    use crate::{CaptureMethodSetting, TargetHandle};

    /// The environment variable that turns "this machine could not run the
    /// test" from a pass into a failure, exactly as it does for the Windows
    /// Graphics Capture tests. CI sets it (`.github/workflows/ci.yml`).
    const REQUIRE_CAPTURE: &str = "CLIPPED_REQUIRE_CAPTURE";

    /// The environment variable that opts a run in to the test that changes a
    /// display's mode in order to provoke a real `DXGI_ERROR_ACCESS_LOST`.
    ///
    /// Off by default: changing the resolution of a display underneath whoever
    /// is at the keyboard is not something a `cargo test` should do by surprise.
    /// It is run deliberately, and the run is what the pull request reports.
    const ALLOW_DISPLAY_CHANGES: &str = "CLIPPED_ALLOW_DISPLAY_CHANGES";

    /// Serialises every test that duplicates a display.
    ///
    /// **DXGI gives a process one duplication per output.** A second
    /// `DuplicateOutput` for a display this process is already duplicating fails
    /// with `E_INVALIDARG` (0x80070057), and libtest runs tests in parallel by
    /// default, so without this the tests fight over the machine's two displays.
    /// Worse, the loser's failure arrives as "this machine would not duplicate
    /// a display", which reads as a skip: measured on Windows 11 build 26200,
    /// `cargo test` looked green while two of the tests below were quietly not
    /// running, and only setting `CLIPPED_REQUIRE_CAPTURE` — which turns a skip
    /// into a failure, and which CI sets — made it visible.
    ///
    /// The lock is taken for the whole of a test rather than around each
    /// `initialise`, because a duplication is held for the length of a capture.
    static ONE_DUPLICATION_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// Takes [`ONE_DUPLICATION_AT_A_TIME`], ignoring poisoning.
    ///
    /// A test that panicked while holding it has already been reported; refusing
    /// to run the rest would turn one failure into five and hide which was the
    /// real one.
    fn one_duplication_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        ONE_DUPLICATION_AT_A_TIME
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Reports that a test could not run here, and returns whether the caller
    /// should return early.
    ///
    /// Written through `std::io::stderr()` rather than `eprintln!` because
    /// libtest captures the macros: a skip printed with `eprintln!` is invisible
    /// in a passing run, which is the failure mode — a regression that turns
    /// these into no-ops looks exactly like a run in which they passed.
    fn skipped(reason: &str) -> bool {
        if env_is_set(REQUIRE_CAPTURE) {
            panic!("{REQUIRE_CAPTURE} is set, so this must not be skipped: {reason}");
        }
        let _ = writeln!(std::io::stderr(), "SKIPPED (duplication): {reason}");
        true
    }

    /// Says something a reader of the test output needs, past libtest's capture.
    fn note(message: &str) {
        let _ = writeln!(std::io::stderr(), "[duplication] {message}");
    }

    fn env_is_set(name: &str) -> bool {
        std::env::var_os(name).is_some_and(|value| !value.is_empty())
    }

    fn rect(left: i32, top: i32, width: u32, height: u32) -> DesktopRect {
        DesktopRect {
            left,
            top,
            width,
            height,
        }
    }

    fn frame(width: u32, height: u32) -> FrameSize {
        FrameSize::new(width, height).expect("a test frame size is not zero")
    }

    #[test]
    fn a_window_inside_the_output_is_copied_whole() {
        // The ordinary case: a 1280x720 window at (2660, 100) on a display whose
        // own origin is (2560, 0). All of it is on this output, so the copy is
        // the whole frame and nothing needs clearing first.
        let placement = place_window_in_output(
            rect(2560, 0, 2560, 1440),
            rect(2660, 100, 1280, 720),
            frame(1280, 720),
        )
        .expect("a window on the output has something to copy");

        assert_eq!(placement.source.left, 100);
        assert_eq!(placement.source.top, 100);
        assert_eq!(placement.source.right, 1380);
        assert_eq!(placement.source.bottom, 820);
        assert_eq!(placement.destination, (0, 0));
        assert!(
            !placement.partial,
            "a window entirely on this display fills its frame, so clearing it first would \
             be a wasted clear on every frame"
        );
    }

    #[test]
    fn a_window_hanging_off_the_right_edge_keeps_the_part_that_is_there() {
        // 400 wide at 2360 into a 2560-wide output: 200 columns are on it and
        // 200 are past its right edge.
        let placement = place_window_in_output(
            rect(2560, 0, 2560, 1440),
            rect(4920, 100, 400, 300),
            frame(400, 300),
        )
        .expect("most of the window is on this output");

        assert_eq!(placement.source.left, 2360);
        assert_eq!(
            placement.source.right, 2560,
            "clamped to the output's width"
        );
        assert_eq!(
            placement.destination,
            (0, 0),
            "the part that is on this output is the left of the window, so it lands at the \
             left of the frame"
        );
        assert!(
            placement.partial,
            "the right 200 columns of the frame have no pixels behind them and must be \
             cleared rather than left holding the previous frame"
        );
    }

    #[test]
    fn a_window_straddling_the_left_edge_lands_at_an_offset_in_the_frame() {
        // The case the issue asks about, in miniature: the window starts 100
        // pixels before this output begins, so its first 100 columns are on the
        // display next door and the frame's first 100 columns stay black.
        let placement = place_window_in_output(
            rect(2560, 0, 2560, 1440),
            rect(2460, 200, 400, 300),
            frame(400, 300),
        )
        .expect("most of the window is on this output");

        assert_eq!(placement.source.left, 0);
        assert_eq!(placement.source.right, 300);
        assert_eq!(placement.source.top, 200);
        assert_eq!(
            placement.destination,
            (100, 0),
            "the 300 columns this output has are the window's right-hand 300, so they belong \
             100 pixels into the frame"
        );
        assert!(placement.partial);
    }

    #[test]
    fn a_window_on_another_display_entirely_has_nothing_to_copy() {
        assert_eq!(
            place_window_in_output(
                rect(2560, 0, 2560, 1440),
                rect(100, 100, 800, 600),
                frame(800, 600)
            ),
            None,
            "a window wholly on the other display must not produce an empty copy, which \
             Direct3D does not define, nor a black frame the recording would claim it \
             captured"
        );
        // Adjacent but not overlapping: the window ends exactly where the output
        // begins.
        assert_eq!(
            place_window_in_output(
                rect(2560, 0, 2560, 1440),
                rect(2160, 0, 400, 300),
                frame(400, 300)
            ),
            None
        );
        // A window parked where Windows puts a minimised one.
        assert_eq!(
            place_window_in_output(
                rect(0, 0, 2560, 1440),
                rect(-32000, -32000, 400, 300),
                frame(400, 300)
            ),
            None
        );
    }

    #[test]
    fn a_window_that_has_grown_since_the_frame_was_sized_is_clamped_to_the_frame() {
        // The race `Session::copy_out`'s safety argument stands on.
        // `check_target` reads the window's size and reports a change to the
        // caller; `AcquireNextFrame` then blocks for up to ACQUIRE_SLICE, and
        // the client rectangle is read again afterwards. A window being
        // drag-resized in that gap is read *larger* than the destination texture
        // it is about to be copied into, and Direct3D documents a
        // `CopySubresourceRegion` that writes outside the destination resource
        // as undefined behaviour.
        let placement = place_window_in_output(
            rect(0, 0, 2560, 1440),
            rect(100, 100, 1600, 900),
            frame(1280, 720),
        )
        .expect("the window is on the output");

        assert_eq!(
            (
                placement.source.right - placement.source.left,
                placement.source.bottom - placement.source.top
            ),
            (1280, 720),
            "the copy has to be the size of the destination texture, not of the window: \
             1600x900 of source into a 1280x720 destination is a write past the end of it"
        );
        assert_eq!(placement.destination, (0, 0));
        assert!(
            !placement.partial,
            "the clamped copy still covers every pixel of the frame, so there is nothing to \
             clear first"
        );
    }

    #[test]
    fn a_window_that_has_shrunk_since_the_frame_was_sized_leaves_nothing_stale_behind() {
        // The same race in the other direction. The copy fits, but it no longer
        // covers the whole frame, and the part it does not cover is still
        // holding the edges of the larger window it used to be.
        let placement = place_window_in_output(
            rect(0, 0, 2560, 1440),
            rect(100, 100, 640, 360),
            frame(1280, 720),
        )
        .expect("the window is on the output");

        assert_eq!(
            (
                placement.source.right - placement.source.left,
                placement.source.bottom - placement.source.top
            ),
            (640, 360)
        );
        assert!(
            placement.partial,
            "the copy reaches a quarter of the frame, so the rest must be cleared rather \
             than left showing the window at the size it used to be"
        );
    }

    #[test]
    fn no_placement_ever_reaches_past_the_frame_or_the_output() {
        // The invariant `Session::copy_out`'s `// SAFETY:` comment asserts,
        // checked over every combination of window position and size that can
        // reach it — off each edge, across each corner, smaller than the frame
        // and much larger — rather than at the three positions the cases above
        // happen to name.
        let output = rect(0, 0, 1920, 1080);
        let destination = frame(640, 480);
        let mut placed = 0_u32;

        for left in (-900..2500).step_by(37) {
            for top in (-700..1500).step_by(41) {
                for (width, height) in [(640, 480), (320, 200), (1280, 960), (4000, 3000)] {
                    let Some(placement) =
                        place_window_in_output(output, rect(left, top, width, height), destination)
                    else {
                        continue;
                    };
                    placed += 1;

                    let copied = (
                        placement.source.right - placement.source.left,
                        placement.source.bottom - placement.source.top,
                    );
                    assert!(
                        placement.source.right <= output.width
                            && placement.source.bottom <= output.height,
                        "the source box left the duplicated image: {placement:?}"
                    );
                    assert!(
                        placement.destination.0 + copied.0 <= destination.width()
                            && placement.destination.1 + copied.1 <= destination.height(),
                        "the copy would write outside the destination texture, which \
                         Direct3D does not define: {placement:?} into {destination}"
                    );
                    assert_eq!(
                        placement.partial,
                        copied != (destination.width(), destination.height()),
                        "a copy that does not cover the whole frame has to be preceded by a \
                         clear: {placement:?}"
                    );
                }
            }
        }

        assert!(
            placed > 1000,
            "the sweep has to actually produce placements to be checking anything; it \
             produced {placed}"
        );
    }

    #[test]
    fn only_the_codes_that_mean_the_duplication_is_gone_ask_for_a_rebuild() {
        assert!(is_access_lost(DXGI_ERROR_ACCESS_LOST));
        assert!(is_access_lost(DXGI_ERROR_DEVICE_REMOVED));
        assert!(is_access_lost(DXGI_ERROR_DEVICE_RESET));
        assert!(is_access_lost(DXGI_ERROR_SESSION_DISCONNECTED));

        assert!(
            !is_access_lost(DXGI_ERROR_WAIT_TIMEOUT),
            "an idle desktop is not a lost duplication; rebuilding on every timeout would \
             tear the capture down ten times a second"
        );
        assert!(
            !is_access_lost(DXGI_ERROR_UNSUPPORTED),
            "a driver that cannot duplicate at all will not start doing so if we ask again"
        );
        assert!(!is_access_lost(windows::Win32::Foundation::E_FAIL));
    }

    #[test]
    fn the_one_duplication_per_output_limit_is_named_rather_than_left_unexplained() {
        // The hard limit this backend measured — a process gets one duplication
        // of an output, and a second `DuplicateOutput` answers `E_INVALIDARG` —
        // is the one whose HRESULT used to reach a caller unclassified. In the
        // tests that read as "this machine would not duplicate a display", which
        // is precisely the misdiagnosis the test mutex exists to prevent, and it
        // would read the same way to a user with a second recorder running.
        let error = duplication_error(windows::core::Error::new(
            E_INVALIDARG,
            "DuplicateOutput refused",
        ));

        let CaptureError::UnsupportedTarget { reason, .. } = &error else {
            panic!("expected the limit to be named as an unsupported target, got: {error}");
        };
        assert!(
            reason.contains("already duplicating this display"),
            "the reason has to say what is actually wrong: {reason}"
        );
    }

    #[test]
    fn a_display_that_is_not_the_right_way_up_is_refused_rather_than_recorded_sideways() {
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE90,
        };

        assert!(is_upright(DXGI_MODE_ROTATION_IDENTITY));
        assert!(is_upright(DXGI_MODE_ROTATION_UNSPECIFIED));
        assert!(!is_upright(DXGI_MODE_ROTATION_ROTATE90));
        assert!(!is_upright(DXGI_MODE_ROTATION_ROTATE180));
    }

    #[test]
    fn the_backend_declares_the_fallback_it_is() {
        let capabilities = DesktopDuplication.capabilities();
        assert!(capabilities.captures_monitors());
        assert!(
            capabilities.captures_windows(),
            "a window is reached by cropping the output it is on, which is what this \
             backend's crop machinery exists for"
        );
        assert!(
            !capabilities.is_occlusion_independent(),
            "this duplicates the screen, so anything drawn over the target is in the \
             recording; declaring otherwise would make selection prefer it for the very \
             case it is worst at"
        );
        assert!(
            !capabilities.is_cursor_optional(),
            "Desktop Duplication never draws the cursor into the desktop image, so the \
             setting cannot be honoured either way"
        );
        assert_eq!(
            DesktopDuplication.method(),
            CaptureMethod::DesktopDuplication
        );
    }

    #[test]
    fn a_protected_target_is_declined_rather_than_recorded_black() {
        let size = FrameSize::new(1920, 1080).expect("1920x1080 is a valid size");
        let protected =
            TargetProperties::new(TargetKind::Window, size).with_content_protected(true);

        match DesktopDuplication.availability(&protected) {
            Availability::Unavailable(Unavailable::UnsupportedTarget { reason }) => {
                assert!(
                    reason.contains("SetWindowDisplayAffinity"),
                    "the reason should name the thing the application did: {reason}"
                );
            }
            other => panic!("a protected window must be declined, not accepted: {other:?}"),
        }
    }

    #[test]
    fn a_minimised_window_is_declined_before_a_recording_is_started_for_it() {
        // Both backends have to decline it, or `select` under `Automatic` falls
        // past the one that said no and lands on this one — which would then
        // crop the corner of the desktop Windows parks a minimised window in and
        // produce exactly the empty recording issue #383 is about, with a
        // different backend's name on it.
        let size = FrameSize::new(1320, 900).expect("1320x900 is a valid size");
        let minimised = TargetProperties::new(TargetKind::Window, size).with_minimised(true);

        match DesktopDuplication.availability(&minimised) {
            Availability::Unavailable(Unavailable::UnsupportedTarget { reason }) => assert!(
                reason.contains("minimised"),
                "the reason has to name the thing the user can put right: {reason}"
            ),
            other => panic!("a minimised window must be declined, not accepted: {other:?}"),
        }

        // Not a backend that declines everything: the same window restored, and
        // a whole display, are both this backend's ordinary business.
        assert!(matches!(
            DesktopDuplication.availability(&TargetProperties::new(TargetKind::Window, size)),
            Availability::Available
        ));
        assert!(matches!(
            DesktopDuplication.availability(&TargetProperties::new(TargetKind::Monitor, size)),
            Availability::Available
        ));
    }

    #[test]
    fn nothing_can_capture_a_minimised_window_so_selection_refuses_it_outright() {
        // The end the two declarations exist for, reached through the real
        // selection policy and the real registry: a session asks `select` before
        // it opens an encoder or creates a file, so this refusal is the one that
        // means no empty recording is left behind (issue #383).
        let size = FrameSize::new(1320, 900).expect("1320x900 is a valid size");
        let minimised = TargetProperties::new(TargetKind::Window, size).with_minimised(true);

        let error = crate::select(
            &crate::registered_declarations(),
            &minimised,
            CaptureMethodSetting::Automatic,
        )
        .expect_err("no backend can produce a frame for a window nothing is drawing");

        let message = error.to_string();
        assert!(
            message.contains("minimised"),
            "the refusal has to say why, or a recording that was refused is a mystery: \
             {message}"
        );
    }

    #[test]
    fn selection_reports_this_backend_as_the_current_method() {
        // Reached through the real selection policy and the real registry rather
        // than a fake, which is what makes this an answer to the issue's third
        // acceptance criterion rather than a test of a `match`. It has to be
        // forced: Windows Graphics Capture outranks this backend under
        // `Automatic`, which is the whole point of the preference order.
        let size = FrameSize::new(2560, 1440).expect("2560x1440 is a valid size");
        let target = TargetProperties::new(TargetKind::Monitor, size);

        let selection = crate::select(
            &crate::registered_declarations(),
            &target,
            CaptureMethodSetting::Forced(CaptureMethod::DesktopDuplication),
        )
        .expect("Desktop Duplication is registered and available for a display");

        assert_eq!(
            format!(
                "Capture method: {}\nCurrent method: {}",
                selection.setting(),
                selection.method()
            ),
            "Capture method: Desktop Duplication\nCurrent method: Desktop Duplication"
        );

        // And the factory selection names is the one that gets built.
        let backend = crate::registered_backend(selection.method())
            .expect("selection only ever chooses a registered backend");
        assert_eq!(
            backend
                .create()
                .expect("creating an uninitialised backend touches nothing that can fail")
                .method(),
            CaptureMethod::DesktopDuplication
        );
    }

    #[test]
    fn acquiring_before_initialising_is_reported_rather_than_attempted() {
        let mut backend = DesktopDuplication
            .create()
            .expect("creating an uninitialised backend touches nothing that can fail");

        let error = backend
            .acquire(Duration::from_millis(1))
            .expect_err("there is nothing to acquire from");
        assert!(matches!(error, CaptureError::NotInitialised { .. }));
        assert_eq!(error.method(), CaptureMethod::DesktopDuplication);

        // `shut_down` is documented as idempotent, including before there was
        // anything to shut down.
        backend.shut_down();
        backend.shut_down();
    }

    #[test]
    fn a_target_handle_that_is_not_a_window_is_reported_as_lost() {
        let size = FrameSize::new(1280, 720).expect("1280x720 is a valid size");
        let target = CaptureTarget::new(
            TargetHandle::from_raw(0xDEAD_BEEF),
            TargetProperties::new(TargetKind::Window, size),
        );

        let mut backend = DesktopDuplication.create().expect("creation succeeds");
        let error = backend
            .initialise(&target, &CaptureConfig::default())
            .expect_err("a handle that is not a window cannot be captured");

        assert!(
            matches!(error, CaptureError::TargetLost { .. }),
            "expected the target to be reported as lost, got: {error}"
        );
    }

    /// The colour the test window is painted, chosen to be nothing a desktop
    /// produces by accident.
    const MARKER_RED: u32 = 0x2B;
    const MARKER_GREEN: u32 = 0x7F;
    const MARKER_BLUE: u32 = 0x5C;

    /// The same colour as a `BGRA8` pixel reads when its four bytes are loaded
    /// as a little-endian `u32` with the alpha byte masked off.
    const MARKER_PIXEL: u32 = MARKER_RED << 16 | MARKER_GREEN << 8 | MARKER_BLUE;

    /// Where the test window goes on its display, and how big it is.
    const WINDOW_INSET: i32 = 120;
    const WINDOW_WIDTH: i32 = 400;
    const WINDOW_HEIGHT: i32 = 300;

    /// The brush the marker window class is painted with.
    ///
    /// One for the process, not one per window, and never deleted. The window
    /// class below is registered once and is never unregistered — Windows
    /// unregisters it when the process ends — and a class holds its background
    /// brush for as long as it exists, so a brush deleted with the window that
    /// created it leaves every later window's class pointing at a freed GDI
    /// handle that `DefWindowProcW` then hands to `WM_ERASEBKGND`. The ownership
    /// that matches the class's own lifetime is one brush belonging to the
    /// class, which is this (AGENTS.md section 58).
    ///
    /// Stored as a `usize` because [`HBRUSH`] is a raw pointer and therefore not
    /// `Sync`; it is only ever a GDI handle, and it is only ever handed straight
    /// back to GDI.
    static MARKER_BRUSH: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

    /// The process's one marker brush, created on first use.
    fn marker_brush() -> HBRUSH {
        let handle = *MARKER_BRUSH.get_or_init(|| {
            // SAFETY: the colour is a plain value, and the returned brush is
            // owned by the process from here on: it belongs to the window class
            // registered with it, which lives as long as the process does.
            let brush = unsafe {
                CreateSolidBrush(COLORREF(MARKER_RED | MARKER_GREEN << 8 | MARKER_BLUE << 16))
            };
            brush.0 as usize
        });
        HBRUSH(handle as *mut core::ffi::c_void)
    }

    /// A visible, topmost, unfocusable window painted [`MARKER_PIXEL`].
    ///
    /// Topmost because this is a duplicate of the screen: a window drawn over
    /// the marker would be recorded instead of it, and the test would fail for a
    /// reason that has nothing to do with the backend. Deliberately not
    /// activated, because a test must not take the keyboard away from whoever is
    /// using the machine.
    struct MarkerWindow {
        window: HWND,
    }

    impl MarkerWindow {
        /// Puts a window [`WINDOW_INSET`] pixels into `monitor`.
        fn on(monitor: &MonitorInfo) -> Option<Self> {
            let bounds = monitor.bounds();
            Self::at(bounds.left() + WINDOW_INSET, bounds.top() + WINDOW_INSET)
        }

        fn at(x: i32, y: i32) -> Option<Self> {
            let class = w!("clipped_duplication_marker");

            // SAFETY: `GetModuleHandleW(None)` returns this executable's own
            // instance handle and takes nothing from this side.
            let instance = unsafe { GetModuleHandleW(None) }.ok()?;

            let class_definition = WNDCLASSW {
                lpfnWndProc: Some(marker_window_procedure),
                hInstance: instance.into(),
                lpszClassName: class,
                hbrBackground: marker_brush(),
                ..Default::default()
            };
            // SAFETY: every pointer in the class definition is either null or a
            // static wide literal, and the window procedure is a real
            // `extern "system"` function that does not unwind. Registering a
            // class that is already registered fails, which is expected: the
            // tests in this module share one class and only the first
            // registration does anything.
            let _ = unsafe { RegisterClassW(&raw const class_definition) };

            // SAFETY: the class is registered above, both strings are static
            // wide literals, and no parent, menu or creation parameter is
            // passed. The window is destroyed by `Drop`.
            let window = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                    class,
                    w!("clipped desktop duplication test"),
                    WS_POPUP | WS_VISIBLE,
                    x,
                    y,
                    WINDOW_WIDTH,
                    WINDOW_HEIGHT,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
            }
            .ok()?;

            // SAFETY: `window` is the window just created; `SW_SHOWNOACTIVATE`
            // is what keeps the keyboard where it was.
            let _ = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };

            let marker = Self { window };
            marker.paint();
            Some(marker)
        }

        /// Repaints the window, which is also what makes the display change and
        /// therefore what makes Desktop Duplication produce a frame at all: an
        /// idle desktop presents nothing and every acquisition would time out.
        fn paint(&self) {
            let mut client = RECT::default();
            // SAFETY: `client` is a live local; the window is live.
            if unsafe { GetClientRect(self.window, &raw mut client) }.is_err() {
                return;
            }
            // SAFETY: the window is live, so `GetDC` returns a device context
            // for it; the rectangle and the brush are both live for the call,
            // and the context is released immediately afterwards.
            unsafe {
                let context = GetDC(Some(self.window));
                FillRect(context, &raw const client, marker_brush());
                ReleaseDC(Some(self.window), context);
            }
            pump_messages();
        }

        /// Moves the window, keeping its size.
        fn move_to(&self, x: i32, y: i32) {
            // SAFETY: the window is live; no other window is named, and the
            // flags say the size and Z order are not being changed.
            let _ = unsafe {
                SetWindowPos(
                    self.window,
                    None,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            pump_messages();
        }

        /// The window's client area size.
        fn client_size(&self) -> FrameSize {
            super::client_size(self.window).expect("the test window has a client area")
        }

        fn handle(&self) -> HWND {
            self.window
        }
    }

    impl Drop for MarkerWindow {
        fn drop(&mut self) {
            // The brush is deliberately not deleted here: it belongs to the
            // window class, which outlives every window made from it. See
            // [`MARKER_BRUSH`].
            //
            // SAFETY: the handle was created by this struct and has not been
            // released; the window was created on this thread, which is what
            // `DestroyWindow` requires.
            unsafe {
                let _ = DestroyWindow(self.window);
            }
            pump_messages();
        }
    }

    /// The window procedure: nothing but the default, because the window exists
    /// to be a rectangle of a known colour and nothing else.
    unsafe extern "system" fn marker_window_procedure(
        window: HWND,
        message: u32,
        w_param: WPARAM,
        l_param: LPARAM,
    ) -> LRESULT {
        // SAFETY: forwarding the arguments exactly as they arrived to the
        // default handler is what a window procedure that handles nothing does.
        unsafe { DefWindowProcW(window, message, w_param, l_param) }
    }

    /// Drains this thread's message queue.
    ///
    /// The window is created on the test thread, so nothing paints, moves or
    /// closes until its messages are dispatched.
    fn pump_messages() {
        let mut message = MSG::default();
        loop {
            // SAFETY: `message` is a live local; asking for every message of
            // every window on this thread is what the `None` and the zeroes
            // mean, and `PM_REMOVE` takes each one out of the queue.
            let available =
                unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool();
            if !available {
                return;
            }
            // SAFETY: `message` was just filled in by `PeekMessageW`.
            unsafe {
                let _ = TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
    }

    /// Reads one pixel out of a captured frame.
    ///
    /// This is what makes the capture tests evidence rather than assertion: a
    /// frame of the right size proves nothing about *which* display it came
    /// from, and the only way to know is to look at a pixel whose colour the
    /// test put there. It copies a single pixel into a staging texture on the
    /// frame's own device, so nothing about the capture path is disturbed.
    fn read_pixel(frame: &CapturedFrame<'_>, x: u32, y: u32) -> Option<u32> {
        let raw = frame.texture().as_raw();
        // SAFETY: the pointer came from a live `CapturedFrame`, whose texture
        // the backend guarantees is a valid `ID3D11Texture2D` for as long as the
        // frame exists. `from_raw_borrowed` takes no reference of its own, so
        // nothing here can release it.
        let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }?;

        // SAFETY: `texture` is live; both calls return owned references
        // windows-rs releases on drop.
        let device = unsafe { texture.GetDevice() }.ok()?;
        // SAFETY: as above.
        let context = unsafe { device.GetImmediateContext() }.ok()?;

        let mut source = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `source` is a live local of the type the signature names.
        unsafe { texture.GetDesc(&raw mut source) };
        if x >= source.Width || y >= source.Height {
            return None;
        }

        let staging_description = D3D11_TEXTURE2D_DESC {
            Width: 1,
            Height: 1,
            MipLevels: 1,
            ArraySize: 1,
            Format: source.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: the description is a live local describing a one-pixel staging
        // texture with no initial data; the out parameter is the shape
        // windows-rs uses for one of that type.
        unsafe {
            device
                .CreateTexture2D(&raw const staging_description, None, Some(&raw mut staging))
                .ok()?;
        }
        let staging = staging?;

        let region = D3D11_BOX {
            left: x,
            top: y,
            front: 0,
            right: x + 1,
            bottom: y + 1,
            back: 1,
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures belong to `device`, the region is inside the
        // source — checked against its description above — and the destination
        // is exactly the one pixel the region asks for. The map is matched by
        // the unmap, and `pData` is read only while the mapping is held and only
        // for the four bytes the pixel occupies.
        let pixel = unsafe {
            context.CopySubresourceRegion(
                &staging,
                0,
                0,
                0,
                0,
                texture,
                0,
                Some(&raw const region),
            );
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))
                .ok()?;
            let pixel = mapped.pData.cast::<u32>().read_unaligned();
            context.Unmap(&staging, 0);
            pixel
        };

        Some(pixel & 0x00FF_FFFF)
    }

    /// The display the test window goes on: a non-primary one where there is
    /// one, so that nothing lands on top of whoever is using the machine.
    fn test_display(monitors: &[MonitorInfo]) -> Option<&MonitorInfo> {
        monitors
            .iter()
            .find(|monitor| !monitor.is_primary())
            .or_else(|| monitors.first())
    }

    /// Starts a capture of one target.
    ///
    /// The backend is constructed rather than created through the factory so
    /// that the tests can look at the state the recovery path changes — the
    /// access-loss count, and whether there is a live session — which is not
    /// something a `Box<dyn CaptureBackend>` can be asked.
    fn capture_of(
        handle: u64,
        kind: TargetKind,
        size: FrameSize,
    ) -> Result<DesktopDuplicationBackend, CaptureError> {
        let target = CaptureTarget::new(
            TargetHandle::from_raw(handle),
            TargetProperties::new(kind, size),
        );
        let mut backend = DesktopDuplicationBackend { running: None };
        backend.initialise(&target, &CaptureConfig::default())?;
        Ok(backend)
    }

    /// The frame size a monitor's own bounds ask for.
    fn size_of(monitor: &MonitorInfo) -> Option<FrameSize> {
        let bounds = monitor.bounds().size();
        FrameSize::new(bounds.width(), bounds.height())
    }

    #[test]
    fn each_display_duplicates_its_own_output_and_nothing_else() {
        let _one_at_a_time = one_duplication_at_a_time();
        // The first acceptance criterion, on this machine's real displays. A
        // frame of the right size is not evidence that it came from the right
        // display, so the test paints a known colour on one display and asserts
        // that it appears in that display's capture at the position it was
        // painted — and that the same position in every other display's capture
        // does not show it.
        let _ = clipped_windows::enable_per_monitor_dpi_awareness();

        let Ok(monitors) = enumerate_monitors() else {
            skipped("this machine would not enumerate its displays");
            return;
        };
        let Some(marked) = test_display(&monitors) else {
            skipped("this machine reports no displays");
            return;
        };
        if monitors.len() < 2 {
            note(
                "only one display is attached, so the half of this test that proves a \
                 capture does not contain another display's content cannot run",
            );
        }
        let Some(window) = MarkerWindow::on(marked) else {
            skipped("this machine would not create a window");
            return;
        };
        note(&format!(
            "marker window on {} ({})",
            marked.device_name(),
            marked.bounds()
        ));

        // The window sits at (WINDOW_INSET, WINDOW_INSET) in its own display's
        // coordinates, so this position is inside it there and is whatever the
        // desktop happens to show at the same spot on every other display.
        let sample = (
            (WINDOW_INSET + WINDOW_WIDTH / 2).unsigned_abs(),
            (WINDOW_INSET + WINDOW_HEIGHT / 2).unsigned_abs(),
        );

        for monitor in &monitors {
            let Some(size) = size_of(monitor) else {
                continue;
            };

            let mut backend = match capture_of(monitor.handle().as_u64(), TargetKind::Monitor, size)
            {
                Ok(backend) => backend,
                Err(error) => {
                    skipped(&format!(
                        "this machine would not duplicate {}: {error}",
                        monitor.device_name()
                    ));
                    return;
                }
            };

            assert_eq!(
                backend
                    .running
                    .as_ref()
                    .expect("initialise succeeded")
                    .format
                    .size(),
                size,
                "{} must duplicate at its own resolution",
                monitor.device_name()
            );

            let mut frames = 0_u32;
            let mut marker_seen = false;
            let mut previous: Option<CaptureTimestamp> = None;
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline && !(marker_seen && frames >= 5) {
                window.paint();
                match backend.acquire(Duration::from_millis(250)) {
                    Ok(Acquisition::Frame(frame)) => {
                        frames += 1;
                        assert_eq!(frame.format().size(), size);
                        if let Some(earlier) = previous {
                            assert!(
                                frame.timestamp().duration_since(earlier).is_some(),
                                "frame timestamps must not go backwards: {earlier} then {}",
                                frame.timestamp()
                            );
                        }
                        previous = Some(frame.timestamp());
                        if read_pixel(&frame, sample.0, sample.1) == Some(MARKER_PIXEL) {
                            marker_seen = true;
                        }
                    }
                    Ok(Acquisition::Timeout) => {}
                    Ok(Acquisition::TargetMinimised) => panic!(
                        "the test window is not minimised, so reporting it as one is a backend bug \
                         rather than something this test provoked"
                    ),
                    Ok(Acquisition::SizeChanged(new_size)) => {
                        backend
                            .resize(new_size)
                            .expect("the capture can be resized");
                    }
                    Err(error) => panic!("{} stopped capturing: {error}", monitor.device_name()),
                }
            }

            note(&format!(
                "{}: {frames} frames, marker {}",
                monitor.device_name(),
                if marker_seen { "found" } else { "absent" }
            ));

            if monitor.handle() == marked.handle() {
                assert!(
                    frames > 0,
                    "{} produced no frames at all while a window on it was being repainted",
                    monitor.device_name()
                );
                assert!(
                    marker_seen,
                    "{} is the display the marker window is on, so its capture must contain \
                     the marker colour at ({}, {})",
                    monitor.device_name(),
                    sample.0,
                    sample.1
                );
            } else {
                // Asserted whatever this display presented, because the marker
                // window is repainted at the top of every iteration of the loop
                // above — for *this* display's capture as much as for the marked
                // one's — and the marked display's own pass asserts that those
                // repaints produce frames. So a duplication that was secretly
                // reading the marked display could not reach this line with the
                // marker unseen: it would have had frames, and the marker would
                // have been in them. Zero frames here is the same evidence
                // arriving the other way round — a duplication that saw none of
                // the presents that were demonstrably happening on the marked
                // display is not a duplication of the marked display.
                assert!(
                    !marker_seen,
                    "{} showed the marker colour, which is only painted on {}: a capture is \
                     picking up the wrong display",
                    monitor.device_name(),
                    marked.device_name()
                );
            }
        }
    }

    #[test]
    fn a_window_is_cropped_to_its_client_area_and_the_crop_follows_it() {
        let _one_at_a_time = one_duplication_at_a_time();
        // A window target is a duplicate of a whole display with the window cut
        // out of it, so two things can be wrong: the size of the cut, and where
        // it is taken from. The marker colour answers the second — if the crop
        // is offset by so much as a pixel, the frame's corners are the desktop
        // behind the window rather than the window.
        let _ = clipped_windows::enable_per_monitor_dpi_awareness();

        let Ok(monitors) = enumerate_monitors() else {
            skipped("this machine would not enumerate its displays");
            return;
        };
        let Some(display) = test_display(&monitors) else {
            skipped("this machine reports no displays");
            return;
        };
        let Some(window) = MarkerWindow::on(display) else {
            skipped("this machine would not create a window");
            return;
        };

        let size = window.client_size();
        let mut backend = match capture_of(window.handle().0 as u64, TargetKind::Window, size) {
            Ok(backend) => backend,
            Err(error) => {
                skipped(&format!(
                    "this machine would not duplicate a window: {error}"
                ));
                return;
            }
        };

        let fully_on_the_window = |backend: &mut DesktopDuplicationBackend, label: &str| {
            let mut frames = 0_u32;
            let mut clean = 0_u32;
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline && clean < 3 {
                window.paint();
                match backend.acquire(Duration::from_millis(250)) {
                    Ok(Acquisition::Frame(frame)) => {
                        frames += 1;
                        assert_eq!(
                            frame.format().size(),
                            size,
                            "the frame is the window's client area, not the display"
                        );
                        let width = size.width();
                        let height = size.height();
                        let corners = [
                            read_pixel(&frame, 2, 2),
                            read_pixel(&frame, width - 3, 2),
                            read_pixel(&frame, 2, height - 3),
                            read_pixel(&frame, width - 3, height - 3),
                        ];
                        if corners.iter().all(|pixel| *pixel == Some(MARKER_PIXEL)) {
                            clean += 1;
                        }
                    }
                    Ok(Acquisition::Timeout) => {}
                    Ok(Acquisition::TargetMinimised) => panic!(
                        "the test window is not minimised, so reporting it as one is a backend bug \
                         rather than something this test provoked"
                    ),
                    Ok(Acquisition::SizeChanged(new_size)) => {
                        panic!("the window was not resized, but {new_size} was reported")
                    }
                    Err(error) => panic!("capture stopped {label}: {error}"),
                }
            }
            note(&format!(
                "{label}: {frames} frames, {clean} with every corner on the window"
            ));
            clean
        };

        assert!(
            fully_on_the_window(&mut backend, "before the window moved") >= 3,
            "the crop should have been taken from the window: every corner of the frame has \
             to be the colour the window is painted"
        );

        // Move it, and require the same again. This is the part of the issue
        // that says the crop has to follow the window.
        let bounds = display.bounds();
        window.move_to(
            bounds.left() + WINDOW_INSET + 260,
            bounds.top() + WINDOW_INSET + 180,
        );
        assert!(
            fully_on_the_window(&mut backend, "after the window moved") >= 3,
            "the crop must follow the window; a fixed crop would now be showing the desktop \
             the window used to be over"
        );
    }

    #[test]
    fn a_window_partly_off_the_output_is_padded_rather_than_shifted() {
        let _one_at_a_time = one_duplication_at_a_time();
        // The straddle case, provoked against the outer edge of the test display
        // so that no part of the test window has to appear on the display
        // somebody is using. The clamp and the clear are the same code either
        // way: `place_window_in_output` cannot tell whether what lies beyond the
        // output's edge is another display or nothing at all.
        //
        // The window starts entirely on the display and is moved out afterwards
        // on purpose. That fills the frame with the marker colour first, so the
        // strip that hangs off the edge is holding the *window's own colour*
        // when it stops being covered — which is what makes the assertion below
        // able to fail. Starting at the edge would leave the untouched strip
        // reading zero whether or not anything ever cleared it.
        let _ = clipped_windows::enable_per_monitor_dpi_awareness();

        let Ok(monitors) = enumerate_monitors() else {
            skipped("this machine would not enumerate its displays");
            return;
        };
        let Some(display) = test_display(&monitors) else {
            skipped("this machine reports no displays");
            return;
        };
        let Some(window) = MarkerWindow::on(display) else {
            skipped("this machine would not create a window");
            return;
        };

        let size = window.client_size();
        let mut backend = match capture_of(window.handle().0 as u64, TargetKind::Window, size) {
            Ok(backend) => backend,
            Err(error) => {
                skipped(&format!(
                    "this machine would not duplicate a window: {error}"
                ));
                return;
            }
        };

        let right_edge = size.width() - 3;
        let sample = |backend: &mut DesktopDuplicationBackend,
                      wanted: (Option<u32>, Option<u32>),
                      label: &str| {
            let mut matched = 0_u32;
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && matched < 3 {
                window.paint();
                match backend.acquire(Duration::from_millis(250)) {
                    Ok(Acquisition::Frame(frame)) => {
                        assert_eq!(frame.format().size(), size);
                        let seen = (
                            read_pixel(&frame, 2, 20),
                            read_pixel(&frame, right_edge, 20),
                        );
                        if seen == wanted {
                            matched += 1;
                        } else if matched > 0 {
                            panic!("{label}: expected {wanted:?} but saw {seen:?}");
                        }
                    }
                    Ok(Acquisition::Timeout) => {}
                    Ok(Acquisition::TargetMinimised) => panic!(
                        "the test window is not minimised, so reporting it as one is a backend bug \
                         rather than something this test provoked"
                    ),
                    Ok(Acquisition::SizeChanged(new_size)) => {
                        panic!("the window was not resized, but {new_size} was reported")
                    }
                    Err(error) => panic!("capture stopped {label}: {error}"),
                }
            }
            matched
        };

        assert!(
            sample(
                &mut backend,
                (Some(MARKER_PIXEL), Some(MARKER_PIXEL)),
                "with the window fully on the display",
            ) >= 3,
            "the frame should be the window from edge to edge before it is moved"
        );

        let bounds = display.bounds();
        let width = i32::try_from(bounds.size().width()).unwrap_or(i32::MAX);
        window.move_to(
            bounds.left() + width - WINDOW_WIDTH / 2,
            bounds.top() + WINDOW_INSET,
        );

        assert!(
            sample(
                &mut backend,
                (Some(MARKER_PIXEL), Some(0)),
                "with half the window past the edge of the display",
            ) >= 3,
            "the half of the frame hanging past the edge of the display has no pixels behind \
             it and must be cleared, not left holding the half of the window that used to be \
             there"
        );
    }

    #[test]
    fn a_caller_that_falls_behind_is_told_how_many_updates_it_missed() {
        let _one_at_a_time = one_duplication_at_a_time();
        // `AccumulatedFrames` is the reason this backend reports a real
        // dropped-frame count where the Windows Graphics Capture backend has to
        // estimate one from timestamps, so it is worth proving that it is being
        // read rather than assumed. The test makes the display change several
        // times without acquiring anything, which is exactly what a caller too
        // slow to keep up does, and then asks for a frame.
        let _ = clipped_windows::enable_per_monitor_dpi_awareness();

        let Ok(monitors) = enumerate_monitors() else {
            skipped("this machine would not enumerate its displays");
            return;
        };
        let Some(display) = test_display(&monitors) else {
            skipped("this machine reports no displays");
            return;
        };
        let Some(window) = MarkerWindow::on(display) else {
            skipped("this machine would not create a window");
            return;
        };
        let Some(size) = size_of(display) else {
            skipped("this display has no size");
            return;
        };
        let mut backend = match capture_of(display.handle().as_u64(), TargetKind::Monitor, size) {
            Ok(backend) => backend,
            Err(error) => {
                skipped(&format!(
                    "this machine would not duplicate a display: {error}"
                ));
                return;
            }
        };

        let mut reported = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline && reported.len() < 3 {
            // Well over a frame interval each, so the display genuinely presents
            // several times while nothing is collecting them.
            for _ in 0..6 {
                window.paint();
                thread::sleep(Duration::from_millis(30));
            }
            match backend.acquire(Duration::from_millis(250)) {
                Ok(Acquisition::Frame(frame)) => reported.push(
                    frame
                        .frames_missed()
                        .expect("Desktop Duplication always knows how many it missed"),
                ),
                Ok(Acquisition::Timeout) => {}
                Ok(Acquisition::TargetMinimised) => panic!(
                    "the test window is not minimised, so reporting it as one is a backend bug \
                     rather than something this test provoked"
                ),
                Ok(Acquisition::SizeChanged(new_size)) => {
                    backend
                        .resize(new_size)
                        .expect("the capture can be resized");
                }
                Err(error) => panic!("capture stopped: {error}"),
            }
        }

        note(&format!("frames missed while stalled: {reported:?}"));
        assert_eq!(reported.len(), 3, "not enough frames arrived to judge");
        assert!(
            reported.iter().any(|missed| *missed > 0),
            "the display was repainted six times between acquisitions, so at least one \
             frame has to report that updates went by without reaching the caller: {reported:?}"
        );
    }

    #[test]
    fn a_lost_duplication_is_rebuilt_and_the_capture_carries_on() {
        let _one_at_a_time = one_duplication_at_a_time();
        // The rebuild half of the second acceptance criterion, run on every
        // machine that can duplicate anything: the session is thrown away
        // exactly as the `DXGI_ERROR_ACCESS_LOST` branch throws it away, and the
        // next acquisition has to build a new one and go on delivering frames
        // without the caller ever seeing an error. The other half — that DXGI
        // really does report access lost, and that this is really what happens
        // when it does — is
        // `access_lost_is_recovered_from_without_ending_the_recording`.
        let _ = clipped_windows::enable_per_monitor_dpi_awareness();

        let Ok(monitors) = enumerate_monitors() else {
            skipped("this machine would not enumerate its displays");
            return;
        };
        let Some(display) = test_display(&monitors) else {
            skipped("this machine reports no displays");
            return;
        };
        let Some(window) = MarkerWindow::on(display) else {
            skipped("this machine would not create a window");
            return;
        };
        let Some(size) = size_of(display) else {
            skipped("this display has no size");
            return;
        };
        let mut backend = match capture_of(display.handle().as_u64(), TargetKind::Monitor, size) {
            Ok(backend) => backend,
            Err(error) => {
                skipped(&format!(
                    "this machine would not duplicate a display: {error}"
                ));
                return;
            }
        };

        let frames = |backend: &mut DesktopDuplicationBackend| {
            let mut count = 0_u32;
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline && count < 3 {
                window.paint();
                match backend.acquire(Duration::from_millis(250)) {
                    Ok(Acquisition::Frame(_)) => count += 1,
                    Ok(Acquisition::Timeout) => {}
                    Ok(Acquisition::TargetMinimised) => panic!(
                        "the test window is not minimised, so reporting it as one is a backend bug \
                         rather than something this test provoked"
                    ),
                    Ok(Acquisition::SizeChanged(new_size)) => {
                        backend
                            .resize(new_size)
                            .expect("the capture can be resized");
                    }
                    Err(error) => panic!("capture stopped: {error}"),
                }
            }
            count
        };

        assert!(
            frames(&mut backend) >= 3,
            "capture produced nothing to begin with"
        );

        // Exactly what `pump` does when DXGI reports that access has been lost.
        backend
            .running
            .as_mut()
            .expect("the capture is running")
            .discard_session();

        assert!(
            frames(&mut backend) >= 3,
            "the capture did not recover: a lost duplication has to be rebuilt inside \
             `acquire`, without the recording ending"
        );
        assert!(
            backend
                .running
                .as_ref()
                .expect("the capture is running")
                .session
                .is_some(),
            "the rebuilt duplication should be live"
        );
    }

    #[test]
    fn a_window_target_is_found_again_when_the_display_it_was_on_has_gone() {
        let _one_at_a_time = one_duplication_at_a_time();
        // A display removed mid-recording — powering off a DisplayPort monitor
        // is the everyday version, and issue #13's scope names monitor removal —
        // invalidates every `HMONITOR`, takes that display's name off the DXGI
        // enumeration, and makes Windows move the windows that were on it to a
        // surviving display. The window is still on screen and still capturable,
        // so a window recording has to carry on.
        //
        // A test cannot unplug a monitor, but it can leave the backend in
        // exactly the state one leaves behind: a remembered monitor handle that
        // matches no output, and a remembered display name that is not
        // enumerated. Before this, `reinitialise` looked for the remembered
        // display and nothing else, so it spent the five-second absence grace
        // failing and then ended the recording with `TargetLost`.
        let _ = clipped_windows::enable_per_monitor_dpi_awareness();

        let Ok(monitors) = enumerate_monitors() else {
            skipped("this machine would not enumerate its displays");
            return;
        };
        let Some(display) = test_display(&monitors) else {
            skipped("this machine reports no displays");
            return;
        };
        let Some(window) = MarkerWindow::on(display) else {
            skipped("this machine would not create a window");
            return;
        };

        let size = window.client_size();
        let mut backend = match capture_of(window.handle().0 as u64, TargetKind::Window, size) {
            Ok(backend) => backend,
            Err(error) => {
                skipped(&format!(
                    "this machine would not duplicate a window: {error}"
                ));
                return;
            }
        };

        let running = backend.running.as_mut().expect("the capture is running");
        running.discard_session();
        // Nothing the enumeration can match, by either route.
        running.monitor = HMONITOR(core::ptr::null_mut());
        running.output_name = r"\\.\DISPLAY_THAT_HAS_BEEN_UNPLUGGED".to_owned();

        match running.reinitialise() {
            Ok(_) => {}
            Err(Recovery::NotYet) => panic!(
                "the window is on {}, which is attached and duplicable; a rebuild that only \
                 ever asks for the display the window used to be on cannot find it",
                display.device_name()
            ),
            Err(Recovery::TargetGone) => panic!(
                "the recording was ended even though the window is still on screen on {}",
                display.device_name()
            ),
        }

        assert!(
            running.session.is_some(),
            "the duplication should have been rebuilt against the display the window is on"
        );
        assert_eq!(
            running.output_name,
            display.device_name(),
            "the rebuilt duplication has to be of the display the window is on now, not of \
             the one it was remembered on"
        );

        // And it is a working capture, not just a live handle.
        let mut frames = 0_u32;
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline && frames < 3 {
            window.paint();
            match backend.acquire(Duration::from_millis(250)) {
                Ok(Acquisition::Frame(frame)) => {
                    assert_eq!(frame.format().size(), size);
                    frames += 1;
                }
                Ok(Acquisition::Timeout) => {}
                Ok(Acquisition::TargetMinimised) => panic!(
                    "the test window is not minimised, so reporting it as one is a backend bug \
                     rather than something this test provoked"
                ),
                Ok(Acquisition::SizeChanged(new_size)) => {
                    backend
                        .resize(new_size)
                        .expect("the capture can be resized");
                }
                Err(error) => panic!("the recording ended after the display was lost: {error}"),
            }
        }
        note(&format!(
            "{frames} frames after the remembered display was forgotten"
        ));
        assert!(
            frames >= 3,
            "the recording has to carry on delivering frames of the window"
        );
    }

    /// A display mode change, put back when this value is dropped.
    struct TemporaryDisplayMode {
        name: Vec<u16>,
    }

    impl TemporaryDisplayMode {
        /// Switches `device_name` to a different resolution, temporarily.
        ///
        /// `CDS_FULLSCREEN` is what makes it temporary: Windows treats the mode
        /// as belonging to this process and puts the registry's mode back when
        /// the process exits, so even a panic that somehow skipped `Drop` could
        /// not leave somebody's display in the wrong mode for longer than the
        /// test run.
        fn change(device_name: &str) -> Option<Self> {
            let name: Vec<u16> = device_name.encode_utf16().chain(Some(0)).collect();
            let mut current = DEVMODEW {
                dmSize: u16::try_from(core::mem::size_of::<DEVMODEW>()).ok()?,
                ..Default::default()
            };
            // SAFETY: the name is a live null-terminated wide string and
            // `current` is a live local whose `dmSize` says how big it is.
            if !unsafe {
                EnumDisplaySettingsW(
                    PCWSTR(name.as_ptr()),
                    ENUM_CURRENT_SETTINGS,
                    &raw mut current,
                )
            }
            .as_bool()
            {
                return None;
            }

            for index in 0..u32::MAX {
                let mut candidate = DEVMODEW {
                    dmSize: current.dmSize,
                    ..Default::default()
                };
                // SAFETY: as above; the enumeration ends by returning false.
                if !unsafe {
                    EnumDisplaySettingsW(
                        PCWSTR(name.as_ptr()),
                        ENUM_DISPLAY_SETTINGS_MODE(index),
                        &raw mut candidate,
                    )
                }
                .as_bool()
                {
                    break;
                }
                let different = candidate.dmPelsWidth != current.dmPelsWidth
                    || candidate.dmPelsHeight != current.dmPelsHeight;
                if !different
                    || candidate.dmBitsPerPel != 32
                    || candidate.dmPelsWidth < 1280
                    || candidate.dmPelsHeight < 720
                {
                    continue;
                }

                // SAFETY: the name and the mode are live for the call, no window
                // is named, and no callback data is passed.
                let changed = unsafe {
                    ChangeDisplaySettingsExW(
                        PCWSTR(name.as_ptr()),
                        Some(&raw const candidate),
                        None,
                        CDS_FULLSCREEN,
                        None,
                    )
                };
                if changed == DISP_CHANGE_SUCCESSFUL {
                    note(&format!(
                        "{device_name} switched from {}x{} to {}x{}",
                        current.dmPelsWidth,
                        current.dmPelsHeight,
                        candidate.dmPelsWidth,
                        candidate.dmPelsHeight
                    ));
                    return Some(Self { name });
                }
            }
            None
        }
    }

    impl Drop for TemporaryDisplayMode {
        fn drop(&mut self) {
            // SAFETY: the name is still live; a null mode with no flags is the
            // documented way to ask for the registry's own settings back.
            let restored = unsafe {
                ChangeDisplaySettingsExW(PCWSTR(self.name.as_ptr()), None, None, CDS_TYPE(0), None)
            };
            note(&format!("display mode restored: {restored:?}"));
        }
    }

    #[test]
    fn access_lost_is_recovered_from_without_ending_the_recording() {
        let _one_at_a_time = one_duplication_at_a_time();
        // The second acceptance criterion against a real `DXGI_ERROR_ACCESS_LOST`
        // rather than a simulated one. A display mode change is the cause the
        // issue names first and the only one of the four — mode change,
        // full-screen transition, driver reset, session switch — that a test can
        // cause on purpose without either crashing the machine or signing the
        // user out.
        //
        // It is opt-in because it changes a display's resolution for a few
        // seconds, which is not something an unattended `cargo test` should do
        // to whoever is using the machine.
        if !env_is_set(ALLOW_DISPLAY_CHANGES) {
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED (duplication): set {ALLOW_DISPLAY_CHANGES} to let this test change \
                 a display's mode for a few seconds"
            );
            return;
        }

        let _ = clipped_windows::enable_per_monitor_dpi_awareness();
        let monitors = enumerate_monitors().expect("displays can be enumerated");
        let display = test_display(&monitors)
            .expect("a machine running this test has a display")
            .clone();
        let window = MarkerWindow::on(&display).expect("a window can be created");
        let size = size_of(&display).expect("a display has a size");
        let mut backend = capture_of(display.handle().as_u64(), TargetKind::Monitor, size)
            .expect("the display can be duplicated");

        let pump = |backend: &mut DesktopDuplicationBackend, seconds: u64| {
            let mut frames = 0_u32;
            let deadline = Instant::now() + Duration::from_secs(seconds);
            while Instant::now() < deadline {
                window.paint();
                match backend.acquire(Duration::from_millis(200)) {
                    Ok(Acquisition::Frame(_)) => frames += 1,
                    Ok(Acquisition::Timeout) => {}
                    Ok(Acquisition::TargetMinimised) => panic!(
                        "the test window is not minimised, so reporting it as one is a backend bug \
                         rather than something this test provoked"
                    ),
                    Ok(Acquisition::SizeChanged(new_size)) => {
                        note(&format!("the display changed mode to {new_size}"));
                        backend
                            .resize(new_size)
                            .expect("the capture can be resized");
                    }
                    // The whole point: a mode change must not reach the caller
                    // as an error, because an error here is a recording that
                    // stops when somebody alt-tabs out of a game.
                    Err(error) => panic!("the recording ended at a display change: {error}"),
                }
            }
            frames
        };

        assert!(
            pump(&mut backend, 2) > 0,
            "capture produced nothing to begin with"
        );

        let changed = TemporaryDisplayMode::change(display.device_name());
        assert!(
            changed.is_some(),
            "no alternative display mode could be set, so no access loss could be provoked"
        );
        let after_change = pump(&mut backend, 4);
        drop(changed);
        let after_restore = pump(&mut backend, 4);

        let losses = backend
            .running
            .as_ref()
            .expect("the capture is running")
            .access_losses;
        note(&format!(
            "{losses} access losses; {after_change} frames after the change, \
             {after_restore} after the restore"
        ));

        assert!(
            losses > 0,
            "the display's mode was changed and changed back, so DXGI must have reported \
             access lost at least once — if it did not, this test is not testing recovery"
        );
        assert!(
            after_change > 0 && after_restore > 0,
            "frames have to keep arriving on both sides of a mode change: {after_change} \
             then {after_restore}"
        );
    }
}
