//! The Windows Graphics Capture backend.
//!
//! `Windows.Graphics.Capture` asks the desktop compositor for a window's or a
//! display's composed content. The compositor already holds that content on the
//! GPU, so the frames arrive as Direct3D 11 textures on the device this backend
//! created, and nothing here reads a pixel: a `Direct3D11CaptureFrame`'s surface
//! is unwrapped to an `ID3D11Texture2D` and that pointer is what
//! [`CapturedFrame`] carries. There is no `Map`, no staging texture and no
//! system-memory copy anywhere in this file, which is the single performance
//! property the whole pipeline is built around (AGENTS.md section 18).
//!
//! # Ownership
//!
//! Every native resource has one owner and one release point. [`Running`] holds
//! the Direct3D device, the capture item, the frame pool, the capture session,
//! the two event registrations and the frame currently lent to the caller. The
//! COM apartment is the one exception, and `apartment.rs` says why: it belongs
//! to the process rather than to a capture. [`GraphicsCaptureBackend`] holds an `Option<Running>`,
//! and `shut_down` is `self.running = None`: dropping the option runs
//! [`Running::drop`], which closes the session and the pool, unregisters both
//! handlers and releases the device, in that order. `Drop` for the backend does
//! the same thing, so an unwind on the capture thread releases everything a
//! clean stop would (AGENTS.md section 58).
//!
//! # Threading
//!
//! One backend, one capture thread. The frame pool is created *free-threaded*,
//! so `FrameArrived` is raised on a thread-pool thread and this backend never
//! needs a message loop; the handler's only job is to bump a counter and wake
//! the capture thread, which is why it can be as short as it is. Everything
//! else — creating, acquiring, resizing, shutting down — happens on the capture
//! thread.

use core::fmt;
use core::num::NonZeroU64;
use core::time::Duration;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;

use windows::core::{Interface, HSTRING};
use windows::Foundation::Metadata::ApiInformation;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindow};

use crate::windows::apartment::ensure_multi_threaded_apartment;
use crate::windows::crop::{recordable_size, EvenCrop};
use crate::windows::device::CaptureDevice;
use crate::{
    Acquisition, Availability, BackendCapabilities, BackendDeclaration, CaptureBackend,
    CaptureBackendFactory, CaptureConfig, CaptureError, CaptureMethod, CaptureTarget,
    CaptureTimestamp, CapturedFrame, FrameFormat, FrameSize, FrameTexture, PixelFormat, TargetKind,
    TargetProperties, TextureKind, Unavailable,
};

/// The method every error and log line in this file names.
const METHOD: CaptureMethod = CaptureMethod::WindowsGraphicsCapture;

/// Buffers in the capture frame pool.
///
/// Three, not one: the caller holds one frame while it submits the texture to
/// the encoder, so a pool of one would leave the compositor nothing to compose
/// into and every frame produced during an encode would be lost. Two leaves one
/// spare and drops a frame whenever an encode overruns a single frame interval;
/// three leaves two, which absorbs the ordinary jitter of a busy machine. More
/// than that would buy latency and video memory rather than frames — a deeper
/// queue does not make the encoder faster, it just delays the moment the
/// backend admits it is behind.
const FRAME_POOL_BUFFERS: i32 = 3;

/// The tick rate of a WinRT `TimeSpan`, which is what
/// `Direct3D11CaptureFrame::SystemRelativeTime` is expressed in.
///
/// A `TimeSpan` counts 100-nanosecond units, always, so this is a constant
/// rather than a `QueryPerformanceFrequency` reading. The value it counts is
/// still a performance-counter reading — that is what "system relative" means —
/// which is why the timestamp is declared as
/// [`SourceClock::PerformanceCounter`](crate::SourceClock::PerformanceCounter)
/// and can be compared against audio positions from the same clock.
const TIMESPAN_TICKS_PER_SECOND: u64 = 10_000_000;

/// The runtime class whose properties are probed to find out how much of the
/// API this Windows build has.
const SESSION_CLASS: &str = "Windows.Graphics.Capture.GraphicsCaptureSession";

/// The Windows build in which `GraphicsCaptureSession.IsBorderRequired` — the
/// property that removes the yellow capture border — first appeared.
///
/// Windows 11 21H2. `docs/prerequisites.md` supports Windows 10 21H2 (build
/// 19044) upwards, so a supported machine can legitimately be without it, and
/// the backend degrades to capturing with the border rather than refusing.
const BORDER_PROPERTY_WINDOWS_BUILD: &str = "Windows 11 build 22000";

/// Windows Graphics Capture, as a thing that can be selected and created.
///
/// Zero-sized: everything it declares is a constant, and everything that costs
/// anything happens in the backend [`create`](CaptureBackendFactory::create)
/// returns. That is what lets [`select`](crate::select) hold a `'static`
/// reference to it in a registry and ask it questions without a GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct WindowsGraphicsCapture;

impl WindowsGraphicsCapture {
    /// Whether this Windows build has the capture API at all.
    ///
    /// `GraphicsCaptureSession::IsSupported` is the API's own answer, and it is
    /// false on Windows 10 before build 1903 and in some server SKUs where the
    /// compositor is absent. A failure to ask — the type not being registered —
    /// is treated as "no", because that is what it means.
    ///
    /// This is a WinRT activation, and
    /// [`BackendDeclaration::availability`] promises to work on any thread, so
    /// the obvious worry is a thread with no COM apartment. It is not one:
    /// windows-rs's factory cache retries a `CO_E_NOTINITIALIZED` activation
    /// after `CoIncrementMTAUsage`, which is exactly the apartment-agnostic
    /// behaviour a declaration needs. [`Apartment`] still exists for the capture
    /// thread, which does far more than one activation and should say so
    /// explicitly rather than lean on a library's fallback.
    fn is_supported_here() -> bool {
        GraphicsCaptureSession::IsSupported().unwrap_or(false)
    }

    /// Whether the runtime class has the named property on this build.
    ///
    /// Used for the two session properties that arrived after the API did.
    /// Probing rather than comparing build numbers is what Microsoft's own
    /// guidance asks for, and it keeps the answer correct on a build where a
    /// feature was serviced backwards.
    fn session_has_property(name: &str) -> bool {
        ApiInformation::IsPropertyPresent(&HSTRING::from(SESSION_CLASS), &HSTRING::from(name))
            .unwrap_or(false)
    }
}

impl BackendDeclaration for WindowsGraphicsCapture {
    fn method(&self) -> CaptureMethod {
        METHOD
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Occlusion independent because the compositor is asked for the
        // *item's* content, not for what is on screen where the item is: a
        // notification or another window drawn over the target does not appear
        // in the capture. This is the reason SPEC.md section 8 prefers this
        // method to Desktop Duplication.
        //
        // Cursor optional because `IsCursorCaptureEnabled` exists from Windows
        // 10 build 19041, and `docs/prerequisites.md` requires build 19044 or
        // later, so every supported build has it. `initialise` still probes
        // before setting it rather than trusting that arithmetic.
        BackendCapabilities::new(true, true)
            .with_occlusion_independent(true)
            .with_cursor_optional(true)
    }

    fn availability(&self, target: &TargetProperties) -> Availability {
        if !Self::is_supported_here() {
            return Availability::Unavailable(Unavailable::UnsupportedSystem {
                requirement: "Windows 10 build 1903 or later with the desktop compositor \
                              running (GraphicsCaptureSession::IsSupported reports false)",
            });
        }

        if target.is_content_protected() {
            // Windows enforces this in the compositor: capture of a window with
            // `WDA_EXCLUDEFROMCAPTURE` succeeds and delivers black frames
            // forever. Declining is the difference between an explanation and a
            // black recording (issue #97).
            return Availability::Unavailable(Unavailable::UnsupportedTarget {
                reason: "the target has excluded itself from capture with \
                         SetWindowDisplayAffinity, so Windows would deliver black frames",
            });
        }

        if target.is_minimised() {
            // The compositor is not composing the window, so it has nothing to
            // hand over: capture starts, the frame pool is created, and no frame
            // ever arrives. Declining is what turns that into a refusal before a
            // file exists instead of an empty recording somebody finds
            // afterwards (issue #383).
            //
            // Deliberately *after* the protection check. A window that is both
            // protected and minimised should be told about the protection, which
            // restoring it will not fix.
            return Availability::Unavailable(Unavailable::UnsupportedTarget {
                reason: "the window is minimised, so the compositor is not drawing it and \
                         no frame would ever arrive",
            });
        }

        Availability::Available
    }
}

impl CaptureBackendFactory for WindowsGraphicsCapture {
    fn create(&self) -> Result<Box<dyn CaptureBackend>, CaptureError> {
        // Nothing native is touched here on purpose. The backend is created
        // wherever the session happens to be running and moved to the capture
        // thread, and the apartment, device and frame pool all belong to that
        // thread; building them here would build them on the wrong one.
        Ok(Box::new(GraphicsCaptureBackend { running: None }))
    }
}

/// A live Windows Graphics Capture, or an uninitialised shell of one.
///
/// The `Option` is the state machine the trait documents: `None` before
/// `initialise` and after `shut_down`, `Some` in between. Making it an option
/// rather than a flag beside a pile of `Option` fields means "not initialised"
/// and "released" are the same state and there is no way to be half of each.
struct GraphicsCaptureBackend {
    running: Option<Running>,
}

// SAFETY: `CaptureBackend` is `Send` so that a session can create a backend and
// move it to the capture thread, and this type has to satisfy that. It is sound
// here for two reasons that hold together.
//
// First, a backend is moved *before* it holds anything: `create` returns
// `running: None`, and every native resource is created by `initialise`, which
// the trait documents as running on the capture thread. So the only value that
// actually crosses a thread boundary owns no COM interface.
//
// Second, if a caller nevertheless moves an initialised backend, nothing it
// holds is thread-bound: the WinRT types are agile, the Direct3D 11 interfaces
// are free-threaded, and the multi-threaded apartment they were activated in
// belongs to the process rather than to any thread — see `apartment.rs`, which
// is also the reason this backend has no per-thread COM state to get wrong.
//
// What is *not* claimed is that two threads may use one backend at once. They
// may not, and nothing here makes that possible: `CaptureBackend` is not `Sync`,
// every method takes `&mut self`, and a `CapturedFrame` is neither `Send` nor
// `Sync`.
unsafe impl Send for GraphicsCaptureBackend {}

impl fmt::Debug for GraphicsCaptureBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphicsCaptureBackend")
            .field("method", &METHOD)
            .field(
                "format",
                &self.running.as_ref().map(|running| running.format),
            )
            .finish()
    }
}

impl CaptureBackend for GraphicsCaptureBackend {
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

        tracing::info!(
            target_kind = %target.properties().kind(),
            width = format.size().width(),
            height = format.size().height(),
            pixel_format = %format.pixel_format(),
            frame_pool_buffers = FRAME_POOL_BUFFERS,
            adapter = self
                .running
                .as_ref()
                .and_then(|running| running.device.adapter_description())
                .unwrap_or_else(|| "unknown".to_owned()),
            "Windows Graphics Capture started"
        );

        Ok(format)
    }

    fn acquire(&mut self, timeout: Duration) -> Result<Acquisition<'_>, CaptureError> {
        let running = self
            .running
            .as_mut()
            .ok_or(CaptureError::NotInitialised { method: METHOD })?;

        // Ownership rule 3 (docs/capture-pipeline.md): the previous frame goes
        // back to the pool here, before anything else, and the borrow checker
        // has already proved that nobody is still holding it.
        running.release_held_frame();

        if let Some(size) = running.awaiting_resize {
            return Ok(Acquisition::SizeChanged(size));
        }

        let deadline = Instant::now() + timeout;
        let mut look = Look::First;
        loop {
            // Read before looking in the pool, not after. `wait_for_frame`
            // compares against this number, so anything that arrives while this
            // thread is between the pool and the lock is already newer than the
            // baseline and wakes the wait immediately instead of being slept
            // through.
            let arrivals_before = running.arrivals();

            match running.take_next_frame(look)? {
                Taken::Frame => return Ok(Acquisition::Frame(running.lend_held_frame())),
                Taken::SizeChanged(size) => {
                    running.awaiting_resize = Some(size);
                    tracing::info!(
                        width = size.width(),
                        height = size.height(),
                        "the capture target changed size; waiting for the caller to resize"
                    );
                    return Ok(Acquisition::SizeChanged(size));
                }
                Taken::Nothing => {}
            }

            if running.target_is_closed() {
                return Err(CaptureError::TargetLost { method: METHOD });
            }
            if !running.wait_for_frame(arrivals_before, deadline) {
                // Nothing arrived for the whole timeout, so the source is idle
                // rather than this backend being behind. Break the timestamp
                // chain: the gap the next frame ends is the source's silence,
                // and counting it as frames this backend missed would report a
                // paused game as a dropped-frame storm.
                running.gaps.forget();

                // Before reporting the timeout as ordinary, ask whether there
                // is still anything to wait for: see `Running::target_has_gone`
                // for why the `Closed` event is not enough on its own.
                if running.target_has_gone() {
                    return Err(CaptureError::TargetLost { method: METHOD });
                }
                // And, on the same path and for the same reason, whether there
                // is a reason for the silence that the caller can put to a user.
                // The whole timeout has already been spent waiting, so this
                // costs the caller's loop nothing that the timeout did not.
                if running.target_is_minimised() {
                    return Ok(Acquisition::TargetMinimised);
                }
                return Ok(Acquisition::Timeout);
            }
            look = Look::AfterWaiting;
        }
    }

    fn resize(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError> {
        let running = self
            .running
            .as_mut()
            .ok_or(CaptureError::NotInitialised { method: METHOD })?;
        let format = running.recreate_frame_pool(size)?;

        tracing::info!(
            target_width = size.width(),
            target_height = size.height(),
            width = format.size().width(),
            height = format.size().height(),
            "Windows Graphics Capture frame pool recreated"
        );

        Ok(format)
    }

    fn shut_down(&mut self) {
        // Idempotent because taking from an `Option` twice is: the second call
        // drops a `None`. Everything is released by `Running::drop`, which is
        // also what runs if the capture thread unwinds instead of stopping.
        if self.running.take().is_some() {
            tracing::info!("Windows Graphics Capture stopped");
        }
    }
}

impl Drop for GraphicsCaptureBackend {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// Whether an attempt to take a frame is the first of an acquisition or a
/// retry after waiting for one to arrive.
///
/// This is what separates "the source produced nothing" from "this backend was
/// not there to take it", and it is the whole basis of the dropped-frame count:
/// a frame already sitting in the pool the first time an acquisition looks was
/// composed while the caller was elsewhere, whereas a frame that only appeared
/// after a wait was one the source had not produced yet. Measured on Windows 11
/// build 26200 against a 60 fps source: a caller that returned immediately took
/// 601 of 603 frames after waiting, and a caller that stalled 200 ms per frame
/// took all 50 of its frames on the first look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    /// The first look of this acquisition.
    First,
    /// A look after [`CaptureSignal::wait_until`] reported something new.
    AfterWaiting,
}

/// The dropped-frame count, derived from the source's own clock.
///
/// Windows Graphics Capture has no "frames missed" field, and it cannot be
/// counted from `FrameArrived` either. The compositor does not compose into a
/// pool with no free buffer, and it raises no event for the frames it therefore
/// skips, so arrivals track deliveries however far behind the caller falls.
/// Measured on Windows 11 build 26200 against a 60 fps source with a caller
/// stalling 200 ms per frame: 52 arrivals, 50 deliveries and nothing else, over
/// ten seconds in which the source presented about 600 frames.
///
/// What does survive is the timestamps. Consecutive delivered frames are
/// differenced and the gap is divided by [`shortest`](Self::shortest), the
/// smallest interval this capture has ever seen between two frames, which is
/// the best evidence available of how often the source produces one. A gap of
/// `n` whole intervals means `n - 1` source frames went by without one reaching
/// the caller. The estimate is the shortest interval *seen*, so it is never
/// shorter than the source's real interval and the division never over-counts:
/// the figure is a lower bound, which is the right direction for a number a
/// user reads as "your machine could not keep up".
///
/// # What is deliberately not counted
///
/// A gap counts only when the frame was already waiting in the pool the first
/// time the acquisition looked ([`Look::First`]). That is the difference
/// between a caller too slow to collect what the source produced and a source
/// that produced nothing — a paused game, a static menu, an idle desktop —
/// which the caller sat and waited through. [`forget`](Self::forget) breaks the
/// chain for the same reason after a timeout, a discarded frame and a resize:
/// the silence around those is nobody's dropped frame.
///
/// # What it cannot separate
///
/// A source that slows down at the same moment the caller does. Nothing in the
/// API distinguishes them, and a capture whose caller never once kept up has no
/// short interval to compare against and reports nothing at all. Both are
/// stated in `docs/capture-pipeline.md` rather than papered over.
#[derive(Debug, Default, Clone, Copy)]
struct FrameGaps {
    /// The source timestamp of the previously delivered frame, in `TimeSpan`
    /// ticks. [`None`] before the first frame and after [`forget`](Self::forget).
    previous: Option<u64>,
    /// The shortest interval between two consecutive delivered frames seen so
    /// far, in `TimeSpan` ticks.
    shortest: Option<NonZeroU64>,
}

impl FrameGaps {
    /// Records a frame timestamped `ticks` and returns how many source frames
    /// went by since the previous one without reaching the caller.
    fn missed_before(&mut self, ticks: u64, look: Look) -> u32 {
        let Some(previous) = self.previous.replace(ticks) else {
            return 0;
        };
        let Some(gap) = ticks.checked_sub(previous).and_then(NonZeroU64::new) else {
            // Equal, or going backwards. Neither is a frame interval, and a
            // capture backend is not the place to argue with the compositor's
            // clock.
            return 0;
        };

        let interval = match self.shortest {
            Some(shortest) if shortest <= gap => shortest,
            _ => {
                self.shortest = Some(gap);
                gap
            }
        };

        if look != Look::First {
            return 0;
        }
        u32::try_from((gap.get() / interval.get()).saturating_sub(1)).unwrap_or(u32::MAX)
    }

    /// Forgets the previous frame, so the next gap is not measured across
    /// silence this backend did not cause.
    fn forget(&mut self) {
        self.previous = None;
    }
}

/// What one attempt to take a frame out of the pool produced.
enum Taken {
    /// A frame, now held in [`Running::held`].
    Frame,
    /// The pool was empty.
    Nothing,
    /// A frame arrived in a shape the pool is not configured for. It has been
    /// released; the caller must call `resize` before capture continues.
    SizeChanged(FrameSize),
}

/// One frame, and the texture unwrapped from it.
///
/// Both are kept because both are needed for the whole of the frame's life: the
/// `Direct3D11CaptureFrame` is what returns the buffer to the pool when it is
/// dropped, and the `ID3D11Texture2D` is the pointer handed to the encoder. The
/// texture holds its own reference, so the ordering of the two drops does not
/// matter, but keeping them in one struct means neither can be forgotten.
struct HeldFrame {
    /// Dropping this returns the buffer to the frame pool.
    ///
    /// Never read, and that is the point: the field exists for its `Drop`, and
    /// the `expect` says so in a way the compiler will complain about if it
    /// stops being true.
    #[expect(
        dead_code,
        reason = "held so that the frame pool cannot recycle this buffer while the \
                  caller is using the texture beside it; released by Drop"
    )]
    frame: Direct3D11CaptureFrame,
    /// The frame's surface as Direct3D 11 sees it.
    texture: ID3D11Texture2D,
    /// The timestamp the frame arrived with, converted once.
    timestamp: CaptureTimestamp,
    /// Source frames that went by between the previous delivered frame and this
    /// one without reaching the caller; see [`FrameGaps`].
    frames_missed: u32,
}

/// Everything a running capture owns.
struct Running {
    /// The Direct3D 11 device every texture belongs to.
    device: CaptureDevice,
    /// The window or display being captured.
    item: GraphicsCaptureItem,
    /// The window being captured, when the target is one. [`None`] for a
    /// display; see [`Running::target_has_gone`] for what it is for.
    window: Option<HWND>,
    /// The pool the compositor composes into.
    pool: Direct3D11CaptureFramePool,
    /// The session, which is what is actually started and stopped.
    session: GraphicsCaptureSession,
    /// Shared with both event handlers; see [`CaptureSignal`].
    signal: Arc<CaptureSignal>,
    /// Registration for `FrameArrived`, removed on release. WinRT calls this an
    /// `EventRegistrationToken`; windows-rs projects it as the `i64` inside.
    frame_arrived: i64,
    /// Registration for `GraphicsCaptureItem::Closed`, removed on release.
    item_closed: i64,
    /// The size the pool is currently configured for. A frame whose
    /// `ContentSize` differs from this is what a resize looks like.
    ///
    /// **The content's own size, never the rounded one.** The pool is what the
    /// compositor composes into, and the comparison above is the whole of how a
    /// resize is recognised; a pool deliberately a row shorter than the content
    /// would report a size change for ever (issue #561).
    pool_size: SizeInt32,
    /// The format `initialise` or `resize` last reported, which is
    /// [`pool_size`](Self::pool_size) rounded down to even.
    format: FrameFormat,
    /// Where a frame is cropped to an even size, for a target whose content has
    /// an odd dimension. [`None`] — and no copy at all — for every even one.
    crop: Option<EvenCrop>,
    /// The frame lent to the caller, still owned here.
    held: Option<HeldFrame>,
    /// A size change that has been reported and not yet acted on. While this is
    /// set the backend is idle and every acquisition repeats the report, which
    /// is what the trait documents.
    awaiting_resize: Option<FrameSize>,
    /// The dropped-frame accounting; see [`FrameGaps`].
    gaps: FrameGaps,
}

impl Running {
    /// Opens a capture of `target`.
    fn start(target: &CaptureTarget, config: &CaptureConfig) -> Result<Self, CaptureError> {
        ensure_multi_threaded_apartment()
            .map_err(|error| backend_error("preparing the COM apartment", error))?;

        let (item, window) = capture_item_for(target)?;
        let pool_size = item
            .Size()
            .map_err(|error| starting_error(window, "reading the capture item's size", error))?;
        let content = frame_size(pool_size).ok_or(CaptureError::UnsupportedTarget {
            method: METHOD,
            target: target.properties().kind(),
            reason: "it currently has no visible content — a minimised window has a \
                     zero-sized client area, so there is nothing to capture until it is \
                     restored",
        })?;
        let size = recordable_size(content, METHOD, target.properties().kind())?;

        let device = CaptureDevice::create()
            .map_err(|error| backend_error("creating the Direct3D 11 device", error))?;

        // Only for a target whose content has an odd dimension, which is the
        // only case that needs a copy at all; see `super::crop`.
        let crop = even_crop(&device, content, size)?;

        // Free-threaded so that frames are delivered on a thread-pool thread
        // rather than through a message loop the capture thread would have to
        // pump. See the module documentation.
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            device.winrt(),
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            FRAME_POOL_BUFFERS,
            pool_size,
        )
        .map_err(|error| backend_error("creating the capture frame pool", error))?;

        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|error| starting_error(window, "creating the capture session", error))?;

        configure_session(&session, config);

        let signal = Arc::new(CaptureSignal::default());

        let frame_arrived = {
            let signal = Arc::clone(&signal);
            pool.FrameArrived(&TypedEventHandler::new(move |_pool, _arguments| {
                signal.record_arrival();
                Ok(())
            }))
            .map_err(|error| starting_error(window, "subscribing to captured frames", error))?
        };

        let item_closed = {
            let signal = Arc::clone(&signal);
            item.Closed(&TypedEventHandler::new(move |_item, _arguments| {
                signal.record_closed();
                Ok(())
            }))
            .map_err(|error| {
                starting_error(window, "subscribing to the capture target closing", error)
            })?
        };

        session
            .StartCapture()
            .map_err(|error| starting_error(window, "starting the capture session", error))?;

        Ok(Self {
            device,
            item,
            window,
            pool,
            session,
            signal,
            frame_arrived,
            item_closed,
            pool_size,
            format: FrameFormat::new(size, PixelFormat::Bgra8Unorm),
            crop,
            held: None,
            awaiting_resize: None,
            gaps: FrameGaps::default(),
        })
    }

    /// Returns the frame lent to the caller to the pool.
    fn release_held_frame(&mut self) {
        self.held = None;
    }

    /// Borrows the held frame as the caller's [`CapturedFrame`].
    ///
    /// # Panics
    ///
    /// If no frame is held. Only [`CaptureBackend::acquire`] calls this, and
    /// only immediately after [`Taken::Frame`], which is exactly the state that
    /// puts one there.
    fn lend_held_frame(&self) -> CapturedFrame<'_> {
        let held = self
            .held
            .as_ref()
            .expect("a frame was just taken from the pool");

        // SAFETY: `held.texture` is a live `ID3D11Texture2D` — it came from the
        // `IDirect3DDxgiInterfaceAccess` of a frame surface a moment ago, and
        // the `HeldFrame` holds an owning reference to it, so its refcount
        // cannot reach zero while it exists. The `HeldFrame` is owned by this
        // `Running`, which is owned by the backend; `CapturedFrame` borrows the
        // backend, and the only thing that clears `held` is
        // `release_held_frame`, which `acquire` calls behind `&mut self`. So the
        // texture outlives the returned frame, and the buffer is not recycled
        // in the meantime either, because the `Direct3D11CaptureFrame` beside it
        // is what returns the buffer to the pool and it is dropped at the same
        // moment.
        let texture =
            unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, held.texture.as_raw()) };

        CapturedFrame::new(texture, self.format, held.timestamp)
            .with_frames_missed(held.frames_missed)
    }

    /// Takes one frame out of the pool, if there is one.
    ///
    /// `look` says whether this acquisition has waited yet, which is what the
    /// dropped-frame count is derived from; see [`Look`].
    fn take_next_frame(&mut self, look: Look) -> Result<Taken, CaptureError> {
        let Some(frame) = self.try_get_next_frame()? else {
            return Ok(Taken::Nothing);
        };

        let content_size = frame
            .ContentSize()
            .map_err(|error| backend_error("reading a frame's content size", error))?;

        if content_size != self.pool_size {
            // A frame this backend threw away is not a frame the caller saw, so
            // the chain of timestamps the dropped-frame count differences ends
            // here rather than spanning the discard.
            self.gaps.forget();
            drop(frame);

            // **The shape a window passes through while it is minimised is not a
            // size to record at.** Reporting one as a `SizeChanged` ends the
            // recording — `clipped_session` cannot follow a size change inside
            // one file — for a window that is sitting there at the size it
            // always was.
            //
            // Measured on Windows 11 build 26200, recording a 1280x720 window
            // that was minimised for six seconds and restored: the compositor
            // composed one frame whose `ContentSize` was **160x28**, the legacy
            // shape Windows reduces a minimised window to, and it arrived
            // *after* `IsIconic` had gone false again. The recording was
            // finished at 160x28 the instant the window came back
            // ([issue #383](https://github.com/wildware-uk/clipped/issues/383)).
            //
            // What told it apart in that measurement was the window's own client
            // area: `GetClientRect` answered 0x0, because a window that is
            // minimised or part way through being restored has no client area at
            // all. A window that has genuinely been resized has one, so this
            // discards the transition and reports the resize exactly as before.
            // One syscall, on a path that has already thrown a frame away.
            if !self.target_has_a_client_area() {
                return Ok(Taken::Nothing);
            }

            return match frame_size(content_size) {
                Some(size) => Ok(Taken::SizeChanged(size)),
                // A window can also report a zero-sized client area, which no
                // encoder can be configured for. It reads as nothing having
                // arrived, and a genuine `SizeChanged` follows once there is a
                // real size to report.
                None => Ok(Taken::Nothing),
            };
        }

        let timestamp = frame
            .SystemRelativeTime()
            .map_err(|error| backend_error("reading a frame's timestamp", error))?;
        let surface = frame
            .Surface()
            .map_err(|error| backend_error("reading a frame's surface", error))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|error| backend_error("unwrapping a frame's Direct3D surface", error))?;

        // SAFETY: `access` is the `IDirect3DDxgiInterfaceAccess` of a live
        // capture-frame surface, and `ID3D11Texture2D` is the interface a WinRT
        // Direct3D surface is documented to expose through it. windows-rs
        // checks the returned pointer against the requested interface's GUID,
        // so a surface that was somehow not a texture returns `E_NOINTERFACE`
        // rather than a mistyped pointer. The reference returned is owned and
        // released when `HeldFrame` drops.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|error| backend_error("reading a frame's Direct3D 11 texture", error))?;

        // The one place a frame is not the compositor's own texture. A target
        // whose content has an odd dimension cannot be encoded at that shape, so
        // the even crop of it is copied into a texture this backend owns and
        // *that* is what the caller is handed — because the size a frame
        // declares has to be the size of the picture in it, not a smaller number
        // beside a larger texture (`super::crop`, AGENTS.md section 22). The
        // frame pool buffer is still held below: releasing it early would let
        // the compositor recycle the source of a copy that has been issued and
        // may not have retired.
        let texture = match self.crop.as_ref() {
            Some(crop) => {
                crop.fill_from(&texture);
                crop.texture().clone()
            }
            None => texture,
        };

        // `TimeSpan::Duration` is signed and counts from an arbitrary
        // system-relative origin. It is never negative for a captured frame;
        // clamping rather than casting keeps a driver that ever reported one
        // from wrapping to an enormous timestamp.
        let ticks = timestamp.Duration.max(0).unsigned_abs();
        let frames_missed = self.gaps.missed_before(ticks, look);

        self.held = Some(HeldFrame {
            frame,
            texture,
            timestamp: CaptureTimestamp::from_performance_counter(
                ticks,
                NonZeroU64::new(TIMESPAN_TICKS_PER_SECOND)
                    .expect("a TimeSpan ticks 10 million times a second"),
            ),
            frames_missed,
        });

        Ok(Taken::Frame)
    }

    /// Calls `TryGetNextFrame`, mapping "the pool is empty" to [`None`].
    ///
    /// The WinRT method returns a null frame when there is nothing queued, and
    /// windows-rs has no `Option` to put that in: it reports the successful call
    /// that produced no object as `Err(Error::empty())`, whose `HRESULT` is
    /// `S_OK`. So a success code arriving as an error means "the pool is empty",
    /// and anything else is a real failure. Translating that here is the whole
    /// reason this function exists — the rest of the file gets an honest
    /// `Option`, and a genuine `HRESULT` still reaches the caller as an error.
    fn try_get_next_frame(&self) -> Result<Option<Direct3D11CaptureFrame>, CaptureError> {
        match self.pool.TryGetNextFrame() {
            Ok(frame) => Ok(Some(frame)),
            Err(error) if error.code().is_ok() => Ok(None),
            Err(error) => Err(backend_error("taking the next frame from the pool", error)),
        }
    }

    /// Frames the compositor has handed to the pool since capture started.
    fn arrivals(&self) -> u64 {
        self.signal.arrivals()
    }

    /// Whether the capture item told us it had closed.
    fn target_is_closed(&self) -> bool {
        self.signal.is_closed()
    }

    /// Whether the window being captured has been destroyed.
    ///
    /// `GraphicsCaptureItem::Closed` is the intended way to learn this, and it
    /// is subscribed to, but it is not sufficient on its own: the event is
    /// delivered through the creating thread's dispatcher queue, and a capture
    /// thread deliberately has neither a dispatcher queue nor a message loop
    /// (see the module documentation on why). Measured on Windows 11 build
    /// 26200, closing the captured window produces no `Closed` callback here at
    /// all — capture simply goes quiet, and a caller would sit in
    /// [`Acquisition::Timeout`] forever waiting for a window that no longer
    /// exists, never finalising the recording.
    ///
    /// So a window target is also *checked*, on the one path where the answer
    /// matters: an acquisition that is about to report a timeout. That is at
    /// most a handful of `IsWindow` calls a second, never one per frame, and
    /// `IsWindow` is a read of the window table.
    ///
    /// A monitor target has no equivalent check — there is no `IsMonitor` — so
    /// a display being disconnected is left to the `Closed` event and to
    /// [issue #98](https://github.com/wildware-uk/clipped/issues/98), which owns
    /// display changes.
    ///
    /// # What this does not answer
    ///
    /// Microsoft's documentation for `IsWindow` advises against calling it on a
    /// window the calling thread did not create, and every window this backend
    /// captures belongs to another process. The reason is handle recycling: the
    /// captured window can be destroyed and its `HWND` reissued to a different
    /// window, at which point `IsWindow` reports true and closure is never
    /// detected — the never-finalising recording this check exists to prevent,
    /// reinstated. That is accepted rather than solved, because the alternative
    /// is worse: `GraphicsCaptureItem::Closed` does not arrive here at all (see
    /// above), so the choice is between a check that is wrong in a rare race and
    /// no check at all. It is also bounded — the handle has to be recycled
    /// during this one recording — and the failure is the pre-existing
    /// behaviour rather than a new one.
    fn target_has_gone(&self) -> bool {
        let Some(window) = self.window else {
            return false;
        };
        // SAFETY: `IsWindow` only reads the window table, and reporting that a
        // handle is no longer a window is exactly what it is for; passing a
        // stale handle is sound and is the case being asked about.
        !unsafe { IsWindow(Some(window)) }.as_bool()
    }

    /// Whether the window being captured is minimised, and therefore not being
    /// composed.
    ///
    /// Asked on exactly the path [`target_has_gone`](Self::target_has_gone) is
    /// asked on, and only after it: a window that has been destroyed is not a
    /// minimised one, and the recording is over rather than paused. So this is a
    /// handful of `IsIconic` calls a second at most, never one per frame, and
    /// `IsIconic` reads the window's own state.
    ///
    /// [`None`] window — a display target — is never minimised.
    fn target_is_minimised(&self) -> bool {
        let Some(window) = self.window else {
            return false;
        };
        // SAFETY: `IsIconic` reads the window's style bits and is defined for
        // any handle value; `target_has_gone` has just reported this one still
        // names a window.
        unsafe { IsIconic(window) }.as_bool()
    }

    /// Whether the window has a client area at all at this moment.
    ///
    /// False while it is minimised and while it is being restored, which is
    /// exactly the window in which the compositor produces frames of the shape a
    /// minimised window is reduced to. Asked only of a frame whose shape does not
    /// match the pool's, to tell a window that has been *resized* from one that
    /// is on its way back from the taskbar.
    ///
    /// Always true for a display target: there is no window to ask, and a
    /// display's mode change is a real change that must be reported.
    fn target_has_a_client_area(&self) -> bool {
        let Some(window) = self.window else {
            return true;
        };
        super::client_size(window).is_some()
    }

    /// Blocks until a frame arrives, the target closes, or `deadline` passes.
    ///
    /// `arrivals_before` must be the arrival count read *before* the pool was
    /// last looked in, or a frame that arrived in between is slept through; see
    /// [`CaptureSignal::wait_until`].
    ///
    /// Returns whether it is worth looking in the pool again.
    fn wait_for_frame(&self, arrivals_before: u64, deadline: Instant) -> bool {
        self.signal.wait_until(arrivals_before, deadline)
    }

    /// Rebuilds the frame pool for a new target size.
    ///
    /// `size` is the *target's* new size, as the acquisition that reported it
    /// read it from the compositor, so it may have an odd dimension. The pool
    /// takes it unchanged — it has to match what the compositor composes — and
    /// the format returned is what will actually be recorded.
    fn recreate_frame_pool(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError> {
        self.release_held_frame();

        let kind = if self.window.is_some() {
            TargetKind::Window
        } else {
            TargetKind::Monitor
        };
        let recorded = recordable_size(size, METHOD, kind)?;
        // Before the pool is touched, so that a target that cannot be recorded
        // at all leaves the capture as it was rather than half reconfigured.
        let crop = even_crop(&self.device, size, recorded)?;

        let pool_size = SizeInt32 {
            Width: i32::try_from(size.width()).unwrap_or(i32::MAX),
            Height: i32::try_from(size.height()).unwrap_or(i32::MAX),
        };

        // `Recreate` replaces the pool's buffers in place, keeping the session,
        // the item and both event registrations. Tearing the session down and
        // building a new one would drop every frame composed while it was gone,
        // which on a window being dragged between monitors is most of them.
        self.pool
            .Recreate(
                self.device.winrt(),
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                FRAME_POOL_BUFFERS,
                pool_size,
            )
            .map_err(|error| backend_error("recreating the capture frame pool", error))?;

        self.pool_size = pool_size;
        self.format = FrameFormat::new(recorded, PixelFormat::Bgra8Unorm);
        self.crop = crop;
        self.awaiting_resize = None;
        // Whatever the source did while the caller was reconfiguring its
        // encoder is not something this backend dropped.
        self.gaps.forget();

        Ok(self.format)
    }
}

impl Drop for Running {
    /// Releases everything, in the order the APIs require.
    ///
    /// Failures are logged rather than propagated: this runs from `shut_down`,
    /// which cannot fail by contract, and from an unwind, where returning an
    /// error is not an option either. Each step is independent, so one failing
    /// does not stop the rest (docs/capture-pipeline.md, ownership rule 6).
    fn drop(&mut self) {
        // The lent frame first: a frame outstanding when the pool is closed is
        // a buffer the compositor cannot reclaim.
        self.held = None;

        if let Err(error) = self.session.Close() {
            tracing::warn!(%error, "closing the Windows Graphics Capture session failed");
        }
        if let Err(error) = self.pool.RemoveFrameArrived(self.frame_arrived) {
            tracing::warn!(%error, "unsubscribing from captured frames failed");
        }
        if let Err(error) = self.pool.Close() {
            tracing::warn!(%error, "closing the capture frame pool failed");
        }
        if let Err(error) = self.item.RemoveClosed(self.item_closed) {
            tracing::warn!(%error, "unsubscribing from the capture target closing failed");
        }

        // Everything still holding a reference — the device, the item, the pool,
        // the session — is released by its own `Drop` after this body returns.
        // The COM apartment is not among them: it belongs to the process, not to
        // this capture, and `apartment.rs` explains at length why taking it away
        // when a recording stops is a crash rather than tidiness.
    }
}

/// What the two event handlers tell the capture thread, and how it waits.
///
/// Deliberately the smallest thing that works. `FrameArrived` is raised on a
/// thread-pool thread once per frame the compositor puts into a *free* pool
/// buffer — which, while the caller is keeping up, is once per frame the source
/// presents, so up to the display's refresh rate — so the handler does no
/// allocation, no logging and no COM call: it takes a lock held for two field
/// updates and wakes the waiter.
///
/// It is not once per composed frame, and nothing here may be used as though it
/// were. When the pool has no free buffer the compositor does not compose and
/// raises no event, so the arrival count says how many frames were collected,
/// never how many were produced. `FrameGaps::missed_before` is where the
/// frames nobody collected are accounted for, and it uses the source's
/// timestamps for exactly this reason.
#[derive(Debug, Default)]
struct CaptureSignal {
    state: Mutex<SignalState>,
    changed: Condvar,
}

/// The two facts the handlers report.
#[derive(Debug, Default, Clone, Copy)]
struct SignalState {
    /// Frames the compositor has put into a free pool buffer since capture
    /// started. Monotonic, and used only to decide whether it is worth looking
    /// in the pool again — never as a count of what the source produced.
    arrivals: u64,
    /// Whether the window closed or the display was disconnected.
    closed: bool,
}

impl CaptureSignal {
    /// Locks the state, recovering from a poisoned mutex.
    ///
    /// A panic on the capture thread while this lock is held would poison it,
    /// and the honest response is to carry on with the state as it was: the
    /// data behind the lock is two integers with no invariant between them, so
    /// there is nothing for poisoning to protect, and refusing to capture
    /// because of it would turn a panic somewhere else into a dead recorder.
    fn lock(&self) -> std::sync::MutexGuard<'_, SignalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn record_arrival(&self) {
        self.lock().arrivals += 1;
        self.changed.notify_all();
    }

    fn record_closed(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    fn arrivals(&self) -> u64 {
        self.lock().arrivals
    }

    fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Waits for something to happen, or for `deadline`.
    ///
    /// Returns false only when the deadline passed with nothing new, which is
    /// what [`Acquisition::Timeout`] reports.
    ///
    /// `arrivals_before` is the baseline to compare against, and the caller has
    /// to have read it *before* it last looked in the pool. Sampling it here
    /// instead would fold a frame that arrived between that look and this lock
    /// into the baseline, and the waiter would then sleep through the very
    /// frame it is waiting for: on a source that produces sporadically, the
    /// caller is told nothing arrived for a whole acquisition timeout while a
    /// frame sits in the pool.
    fn wait_until(&self, arrivals_before: u64, deadline: Instant) -> bool {
        let mut state = self.lock();

        loop {
            if state.arrivals != arrivals_before || state.closed {
                return true;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            if remaining.is_zero() {
                return false;
            }
            let (next, timed_out) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
            if timed_out.timed_out() && state.arrivals == arrivals_before && !state.closed {
                return false;
            }
        }
    }
}

/// Builds the `GraphicsCaptureItem` for a window or a display.
///
/// The interop factory is the only route from a Win32 handle into WinRT, and it
/// is the one place in this crate where a [`TargetHandle`](crate::TargetHandle)
/// is turned back into a handle — which is what `TargetHandle`'s documentation
/// reserves for the platform module.
fn capture_item_for(
    target: &CaptureTarget,
) -> Result<(GraphicsCaptureItem, Option<HWND>), CaptureError> {
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|error| backend_error("obtaining the capture item factory", error))?;

    let raw = target.handle().as_raw() as *mut core::ffi::c_void;

    match target.properties().kind() {
        TargetKind::Window => {
            let window = HWND(raw);

            // SAFETY: `IsWindow` only reads the window table; a stale or
            // invalid handle is exactly what it exists to report, and it is
            // sound to pass one.
            if !unsafe { IsWindow(Some(window)) }.as_bool() {
                return Err(CaptureError::TargetLost { method: METHOD });
            }

            // SAFETY: `window` is a live top-level window — checked
            // immediately above — which is what `CreateForWindow` requires.
            // The returned item is an owned reference released on drop.
            let item = unsafe { interop.CreateForWindow(window) }.map_err(|error| {
                // Through `starting_error` rather than `backend_error`, for the
                // same reason every later step in `Running::start` goes through
                // it: the `IsWindow` check is one statement away, and a window
                // can go in that gap exactly as it can in the wider ones. This
                // is the *earliest* point at which it can, so classifying it
                // here is what stops a game that exited as the recording
                // started being reported as a broken backend.
                starting_error(
                    Some(window),
                    "creating a capture item for the window",
                    error,
                )
            })?;
            Ok((item, Some(window)))
        }
        TargetKind::Monitor => {
            let monitor = HMONITOR(raw);

            // SAFETY: `monitor` is an `HMONITOR` produced by monitor
            // enumeration, which is what `CreateForMonitor` requires. A
            // monitor that has since been disconnected fails the call with an
            // `HRESULT` rather than misbehaving, and that becomes the error
            // below.
            let item = unsafe { interop.CreateForMonitor(monitor) }
                .map_err(|error| backend_error("creating a capture item for the display", error))?;
            Ok((item, None))
        }
    }
}

/// Applies the parts of [`CaptureConfig`] the session can express, and says in
/// the log what this Windows build would not let it do.
///
/// Nothing here is fatal. A build without the border property still captures,
/// with the border; a session that refuses to hide the cursor still captures,
/// with the cursor. Failing a recording over either would be a worse answer than
/// recording something slightly different from what was asked (AGENTS.md
/// section 16).
fn configure_session(session: &GraphicsCaptureSession, config: &CaptureConfig) {
    if WindowsGraphicsCapture::session_has_property("IsCursorCaptureEnabled") {
        if let Err(error) = session.SetIsCursorCaptureEnabled(config.capture_cursor()) {
            tracing::warn!(
                %error,
                capture_cursor = config.capture_cursor(),
                "the capture session refused the cursor setting; recording whatever it \
                 gives us instead"
            );
        }
    } else {
        tracing::warn!(
            "this Windows build has no GraphicsCaptureSession.IsCursorCaptureEnabled, so \
             the cursor setting cannot be honoured (it needs Windows 10 build 19041)"
        );
    }

    // The yellow capture border. Windows draws it around a captured window
    // unless an application opts out, and a recording of a game with a yellow
    // rectangle around it is not the recording anybody wanted.
    if WindowsGraphicsCapture::session_has_property("IsBorderRequired") {
        if let Err(error) = session.SetIsBorderRequired(false) {
            tracing::warn!(
                %error,
                "Windows refused to remove the capture border; recording with it. This is \
                 usually a policy or packaging restriction rather than a fault"
            );
        }
    } else {
        tracing::info!(
            required_build = BORDER_PROPERTY_WINDOWS_BUILD,
            "this Windows build cannot remove the capture border, so the recording will \
             have one"
        );
    }
}

/// The crop a capture of `content` needs in order to hand out `recorded`-sized
/// frames, or [`None`] when the content is already even and nothing has to be
/// copied.
///
/// The log line is once per capture start or resize, and it is there because a
/// recording one row shorter than the window is a surprise worth being able to
/// explain from a log rather than from a pixel ruler (AGENTS.md section 19).
fn even_crop(
    device: &CaptureDevice,
    content: FrameSize,
    recorded: FrameSize,
) -> Result<Option<EvenCrop>, CaptureError> {
    if recorded == content {
        return Ok(None);
    }

    let crop = EvenCrop::create(device.d3d11(), recorded)
        .map_err(|error| backend_error("creating the cropped frame texture", error))?;

    tracing::info!(
        content_width = content.width(),
        content_height = content.height(),
        width = recorded.width(),
        height = recorded.height(),
        "this target has an odd dimension, which 4:2:0 chroma cannot represent, so the \
         recording is one row or column short of it and every frame is copied into a \
         texture of the size the track declares"
    );

    Ok(Some(crop))
}

/// A [`SizeInt32`] as a [`FrameSize`], or [`None`] if either side is not
/// positive.
///
/// `SizeInt32` is signed and a minimised window reports zeroes, which is
/// exactly the case [`FrameSize`] refuses to represent.
fn frame_size(size: SizeInt32) -> Option<FrameSize> {
    let width = u32::try_from(size.Width).ok()?;
    let height = u32::try_from(size.Height).ok()?;
    FrameSize::new(width, height)
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

/// The error for a failure that happened while a capture of `window` was being
/// started.
///
/// `capture_item_for` checks that the window exists before it asks Windows for
/// a capture item, and everything from the very next statement onwards — the
/// `CreateForWindow` call itself included — is another few milliseconds in
/// which the window can go. A game that exits exactly as a recording starts is
/// the ordinary way to reach it (AGENTS.md section 16). Windows reports it from
/// `CreateCaptureSession` as `ERROR_INVALID_STATE (0x8007139F)`, "the group or
/// resource is not in the correct state to perform the requested operation",
/// which names neither the window nor the reason; a caller handed that has no
/// way to tell a vanished target from a broken backend, and would report a
/// fault to the user instead of stopping quietly.
///
/// So the window is asked about again, and only when it has actually gone does
/// this become [`CaptureError::TargetLost`]. When it is still there, the
/// original failure is passed on unchanged, because then it really is a fault
/// and hiding it behind "target lost" would be worse than the message Windows
/// gave. A display target has nothing to ask — see
/// [`Running::target_has_gone`], which has the same limitation and the same
/// reasons.
fn starting_error(
    window: Option<HWND>,
    operation: &'static str,
    error: windows::core::Error,
) -> CaptureError {
    if let Some(window) = window {
        // SAFETY: `IsWindow` only reads the window table, and reporting that a
        // handle is no longer a window is exactly what it is for; passing a
        // stale handle is sound and is the case being asked about. The caveat
        // about handle recycling is the one recorded on
        // `Running::target_has_gone`, and it applies here in the same way.
        if !unsafe { IsWindow(Some(window)) }.as_bool() {
            return CaptureError::TargetLost { method: METHOD };
        }
    }
    backend_error(operation, error)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::{CaptureMethodSetting, TargetHandle};

    /// The environment variable that turns "this machine could not run the
    /// test" from a pass into a failure.
    ///
    /// Set it on any machine that is supposed to be able to capture. CI sets it
    /// (`.github/workflows/ci.yml`), so a green Windows job means the capture
    /// tests *ran*, not merely that they did not fail.
    const REQUIRE_CAPTURE: &str = "CLIPPED_REQUIRE_CAPTURE";

    /// Reports that a test could not run here, and returns whether the caller
    /// should return early.
    ///
    /// Two things make this more than an `eprintln!`. It panics instead of
    /// skipping when [`REQUIRE_CAPTURE`] is set, so a machine that is meant to
    /// capture cannot quietly stop testing capture. And it writes through
    /// `std::io::stderr()` rather than the `eprintln!` macro, because libtest
    /// captures the macros: a skip printed with `eprintln!` is invisible in a
    /// passing CI run, which is the failure mode — a regression that turns this
    /// test into a no-op looks exactly like a test that passed.
    fn skipped(reason: &str) -> bool {
        if env_is_set(REQUIRE_CAPTURE) {
            panic!("{REQUIRE_CAPTURE} is set, so this must not be skipped: {reason}");
        }
        let _ = writeln!(std::io::stderr(), "SKIPPED (capture): {reason}");
        true
    }

    fn env_is_set(name: &str) -> bool {
        std::env::var_os(name).is_some_and(|value| !value.is_empty())
    }

    /// 60 fps in `TimeSpan` ticks: 166,666 of them, near enough.
    const SIXTY_FPS: u64 = TIMESPAN_TICKS_PER_SECOND / 60;

    #[test]
    fn a_caller_that_keeps_up_is_told_it_missed_nothing() {
        // Ten seconds of a 60 fps source arriving on time, every frame taken on
        // the first look — the case that has to report nothing even though the
        // arithmetic is being asked to run, which is why this is not
        // `Look::AfterWaiting`.
        let mut gaps = FrameGaps::default();
        let mut missed = 0;
        for frame in 0..600u64 {
            missed += gaps.missed_before(frame * SIXTY_FPS, Look::First);
        }
        assert_eq!(missed, 0);

        // The same, with the jitter a real compositor has: intervals a few
        // hundred microseconds either side of nominal must not round up into a
        // dropped frame.
        let mut gaps = FrameGaps::default();
        let mut ticks = 0;
        let mut missed = 0;
        for frame in 0..600u64 {
            ticks += SIXTY_FPS + (frame % 7) * 200;
            missed += gaps.missed_before(ticks, Look::First);
        }
        assert_eq!(missed, 0);
    }

    #[test]
    fn a_caller_that_falls_behind_is_told_how_many_frames_went_by() {
        // The measured shape of a slow encoder: the source runs at 60 fps, the
        // caller takes 200 ms per frame, and every frame it eventually collects
        // is already sitting in the pool when it looks. Eleven of the twelve
        // source frames in each 200 ms went nowhere.
        let mut gaps = FrameGaps::default();

        // One ordinary interval first, which is where the source's rate is
        // learnt from — and which must itself count as nothing missed.
        assert_eq!(gaps.missed_before(0, Look::AfterWaiting), 0);
        assert_eq!(gaps.missed_before(SIXTY_FPS, Look::AfterWaiting), 0);

        let stalled = TIMESPAN_TICKS_PER_SECOND / 5;
        let mut missed = 0;
        for frame in 1..=10u64 {
            missed += gaps.missed_before(SIXTY_FPS + frame * stalled, Look::First);
        }

        assert_eq!(
            missed, 110,
            "ten 200 ms gaps at a 16.67 ms source interval are eleven missed frames each"
        );
    }

    #[test]
    fn a_source_that_stops_producing_is_not_a_dropped_frame() {
        // A paused game. The caller waits out the silence and the frame that
        // ends it arrives while it is waiting, so nothing was dropped: the
        // source simply had nothing to give.
        let mut gaps = FrameGaps::default();
        gaps.missed_before(0, Look::AfterWaiting);
        gaps.missed_before(SIXTY_FPS, Look::AfterWaiting);

        let five_seconds = SIXTY_FPS + 5 * TIMESPAN_TICKS_PER_SECOND;
        assert_eq!(
            gaps.missed_before(five_seconds, Look::AfterWaiting),
            0,
            "a gap the caller waited through is the source being idle, not a drop"
        );
    }

    #[test]
    fn silence_the_backend_did_not_cause_is_not_measured_across() {
        // `forget` is what `acquire` calls on a timeout, `take_next_frame` on a
        // discarded frame and `resize` on a new pool. After it, the next frame
        // starts a new chain rather than being blamed for the interval since
        // the last one.
        let mut gaps = FrameGaps::default();
        gaps.missed_before(0, Look::AfterWaiting);
        gaps.missed_before(SIXTY_FPS, Look::AfterWaiting);

        gaps.forget();
        assert_eq!(
            gaps.missed_before(SIXTY_FPS + 5 * TIMESPAN_TICKS_PER_SECOND, Look::First),
            0
        );
    }

    #[test]
    fn a_shorter_interval_than_any_seen_before_becomes_the_reference() {
        // A capture that starts while the source is slow must not keep
        // reporting drops once it finds out the source is faster than that.
        let mut gaps = FrameGaps::default();
        gaps.missed_before(0, Look::First);
        // A first gap of 100 ms: nothing to compare it against, so nothing
        // missed, and it becomes the reference.
        assert_eq!(
            gaps.missed_before(TIMESPAN_TICKS_PER_SECOND / 10, Look::First),
            0
        );
        // Then the real rate shows up, and it too counts as nothing.
        let faster = TIMESPAN_TICKS_PER_SECOND / 10 + SIXTY_FPS;
        assert_eq!(gaps.missed_before(faster, Look::First), 0);
        // From here on, 100 ms is five missed frames rather than none.
        assert_eq!(
            gaps.missed_before(faster + TIMESPAN_TICKS_PER_SECOND / 10, Look::First),
            5
        );
    }

    #[test]
    fn a_timestamp_that_does_not_advance_is_not_an_interval() {
        let mut gaps = FrameGaps::default();
        gaps.missed_before(1_000_000, Look::First);
        assert_eq!(gaps.missed_before(1_000_000, Look::First), 0);
        assert_eq!(gaps.missed_before(999_999, Look::First), 0);
    }

    #[test]
    fn the_backend_declares_what_the_selection_policy_prefers_it_for() {
        let capabilities = WindowsGraphicsCapture.capabilities();
        assert!(capabilities.captures_windows());
        assert!(capabilities.captures_monitors());
        assert!(
            capabilities.is_occlusion_independent(),
            "capturing the item's own content rather than the screen is the reason \
             SPEC.md section 8 prefers this method to Desktop Duplication"
        );
        assert!(capabilities.is_cursor_optional());
        assert_eq!(
            WindowsGraphicsCapture.method(),
            CaptureMethod::WindowsGraphicsCapture
        );
    }

    #[test]
    fn a_protected_target_is_declined_rather_than_recorded_black() {
        let size = FrameSize::new(1920, 1080).expect("1920x1080 is a valid size");
        let protected =
            TargetProperties::new(TargetKind::Window, size).with_content_protected(true);

        match WindowsGraphicsCapture.availability(&protected) {
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
    fn selection_reports_this_backend_as_the_current_method() {
        // The end SPEC.md section 8 asks for, reached through the real
        // selection policy and the real declaration rather than a fake.
        let size = FrameSize::new(2560, 1440).expect("2560x1440 is a valid size");
        let target = TargetProperties::new(TargetKind::Window, size);

        if !WindowsGraphicsCapture::is_supported_here()
            && skipped("GraphicsCaptureSession::IsSupported reports false here")
        {
            return;
        }

        let selection = crate::select(
            &crate::registered_declarations(),
            &target,
            CaptureMethodSetting::Automatic,
        )
        .expect("Windows Graphics Capture is registered and available");

        assert_eq!(
            format!(
                "Capture method: {}\nCurrent method: {}",
                selection.setting(),
                selection.method()
            ),
            "Capture method: Automatic\nCurrent method: Windows Graphics Capture"
        );
    }

    #[test]
    fn acquiring_before_initialising_is_reported_rather_than_attempted() {
        let mut backend = WindowsGraphicsCapture
            .create()
            .expect("creating an uninitialised backend touches nothing that can fail");

        let error = backend
            .acquire(Duration::from_millis(1))
            .expect_err("there is nothing to acquire from");
        assert!(matches!(error, CaptureError::NotInitialised { .. }));
        assert_eq!(error.method(), CaptureMethod::WindowsGraphicsCapture);

        // `shut_down` is documented as idempotent, including before there was
        // anything to shut down.
        backend.shut_down();
        backend.shut_down();
    }

    #[test]
    fn a_target_handle_that_is_not_a_window_is_reported_as_lost() {
        // The handle of a window that has closed is the ordinary way to reach
        // this: enumeration returned it, the user took a moment to choose, and
        // the window went away in between (AGENTS.md section 16).
        let size = FrameSize::new(1280, 720).expect("1280x720 is a valid size");
        let target = CaptureTarget::new(
            TargetHandle::from_raw(0xDEAD_BEEF),
            TargetProperties::new(TargetKind::Window, size),
        );

        let mut backend = WindowsGraphicsCapture.create().expect("creation succeeds");
        let error = backend
            .initialise(&target, &CaptureConfig::default())
            .expect_err("a handle that is not a window cannot be captured");

        assert!(
            matches!(error, CaptureError::TargetLost { .. }),
            "expected the target to be reported as lost, got: {error}"
        );
    }

    #[test]
    fn a_window_that_goes_while_the_capture_is_starting_is_reported_as_lost() {
        // Found while verifying exclusive fullscreen for issue #12: the test
        // application went away a fraction of a second after announcing its
        // window, and `initialise` reported `Backend { operation: "creating the
        // capture session", source: HRESULT(0x8007139F) }` — "the group or
        // resource is not in the correct state to perform the requested
        // operation". Nothing in that says the window had gone, so a session
        // would surface a fault to the user instead of stopping quietly, and
        // the runtime fallback in issue #97 would try another backend against a
        // target that no longer exists.
        let Some(window) = a_real_window() else {
            skipped("this machine would not create a window");
            return;
        };

        // The HRESULT Windows actually returned, so this test fails if the
        // classification stops covering it.
        let refusal = windows::core::Error::new(
            windows::core::HRESULT(0x8007_139Fu32 as i32),
            "the group or resource is not in the correct state to perform the requested \
             operation.",
        );

        // While the window is still there the failure is a real failure, and
        // must not be dressed up as a target that went away.
        let while_alive = starting_error(
            Some(window),
            "creating the capture session",
            refusal.clone(),
        );
        assert!(
            matches!(while_alive, CaptureError::Backend { .. }),
            "a failure against a window that is still there is a backend failure, got: \
             {while_alive}"
        );

        // SAFETY: `window` is the window created above, on this thread, and has
        // not been destroyed yet — which is what `DestroyWindow` requires.
        let destroyed =
            unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(window) }.is_ok();
        assert!(destroyed, "the test window should have been destroyable");

        let once_gone = starting_error(
            Some(window),
            "creating the capture session",
            refusal.clone(),
        );
        assert!(
            matches!(once_gone, CaptureError::TargetLost { .. }),
            "a window that has gone should be reported as lost, got: {once_gone}"
        );

        // A display target has no window to ask about, so it keeps the failure
        // Windows gave rather than guessing.
        let display = starting_error(None, "creating the capture session", refusal);
        assert!(
            matches!(display, CaptureError::Backend { .. }),
            "a display has no window to have lost, got: {display}"
        );
    }

    /// Creates a real top-level window, or [`None`] on a machine that has no
    /// window station to put one on.
    ///
    /// `STATIC` is one of the classes Windows registers for every process, so
    /// this needs no window class, no module handle and no window procedure —
    /// the test is about capture, and a hand-written `WNDCLASS` here would be
    /// forty lines of the thing that is not being tested. What matters is that
    /// it is a genuine top-level window with a genuine `HWND`, because a fake
    /// one would not exercise `CreateForWindow` at all.
    fn a_real_window() -> Option<HWND> {
        // SAFETY: `STATIC` is a system window class; both strings are static
        // wide literals living for the whole program; no parent, menu, instance
        // or creation parameter is passed, which is what the zero arguments
        // mean. The caller destroys the returned window.
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                Default::default(),
                windows::core::w!("STATIC"),
                windows::core::w!("clipped capture test window"),
                windows::Win32::UI::WindowsAndMessaging::WS_OVERLAPPEDWINDOW
                    | windows::Win32::UI::WindowsAndMessaging::WS_VISIBLE,
                80,
                80,
                320,
                240,
                None,
                None,
                None,
                None,
            )
        }
        .ok()
    }

    /// Lets the test window process the messages Windows has sent it.
    ///
    /// The window is created on this thread and this thread spends its time
    /// inside `acquire`, so without this it never handles a `WM_SIZE` and a
    /// window told to restore stays half-restored for ever — reporting a client
    /// area of the shape Windows collapsed it to, which is a *correct* answer
    /// about a window nobody is running. A capture backend has to be judged
    /// against a window that behaves like a window (AGENTS.md section 25).
    fn pump_messages() {
        let mut message = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        loop {
            // SAFETY: `message` is a live local; asking for every message of
            // every window on this thread is what the `None` and the zeroes
            // mean, and `PM_REMOVE` takes each one out of the queue.
            let available = unsafe {
                windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
                    &raw mut message,
                    None,
                    0,
                    0,
                    windows::Win32::UI::WindowsAndMessaging::PM_REMOVE,
                )
            }
            .as_bool();
            if !available {
                return;
            }
            // SAFETY: `message` was just filled in by `PeekMessageW`.
            unsafe {
                let _ =
                    windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&raw const message);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&raw const message);
            }
        }
    }

    /// A live capture of a real top-level window, or [`None`] on a machine that
    /// cannot provide one — having said so through [`skipped`].
    ///
    /// The caller destroys the window. The backend is returned first so that
    /// dropping it releases the capture before the window goes.
    fn a_capture_of_a_real_window() -> Option<(Box<dyn CaptureBackend>, HWND)> {
        if !WindowsGraphicsCapture::is_supported_here()
            && skipped("GraphicsCaptureSession::IsSupported reports false here")
        {
            return None;
        }
        let Some(window) = a_real_window() else {
            skipped("this machine would not create a window");
            return None;
        };

        let size = FrameSize::new(320, 240).expect("320x240 is a valid size");
        let target = CaptureTarget::new(
            TargetHandle::from_raw(window.0 as u64),
            TargetProperties::new(TargetKind::Window, size),
        );

        let mut backend = WindowsGraphicsCapture.create().expect("creation succeeds");
        if let Err(error) = backend.initialise(&target, &CaptureConfig::default()) {
            // SAFETY: `window` is the window created above and not yet
            // destroyed.
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(window) };
            skipped(&format!(
                "this machine would not capture a plain window: {error}"
            ));
            return None;
        }
        Some((backend, window))
    }

    #[test]
    fn a_minimised_window_is_reported_as_minimised_rather_than_as_a_size_a_loss_or_silence() {
        // Minimise handling is part of issue #12's scope, so it is exercised
        // deliberately here rather than left to whatever a measurement run
        // happened to do. Windows reports a minimised window's client area as
        // zero by zero and stops composing for it, and the three ways to get
        // that wrong are all silent: passing the zero size on as a
        // `SizeChanged` — which no encoder can be configured for — reading the
        // silence as the window having gone and finalising the recording while
        // it is still on the taskbar, or reporting it as an ordinary
        // `Acquisition::Timeout`, which is indistinguishable from a paused game
        // and is how a recording of a minimised window came to be a 791-byte
        // file nobody was warned about (issue #383).
        let Some((mut backend, window)) = a_capture_of_a_real_window() else {
            return;
        };

        for _ in 0..4 {
            let _ = backend.acquire(Duration::from_millis(50));
        }

        // SAFETY: `window` is the live test window, created on this thread.
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                window,
                windows::Win32::UI::WindowsAndMessaging::SW_MINIMIZE,
            )
        };
        pump_messages();

        let mut said_minimised = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            pump_messages();
            match backend.acquire(Duration::from_millis(100)) {
                Ok(Acquisition::Frame(_) | Acquisition::Timeout) => {}
                Ok(Acquisition::TargetMinimised) => said_minimised = true,
                Ok(Acquisition::SizeChanged(size)) => panic!(
                    "nothing resized this window, so reporting {size} is the shape Windows \
                     reduces a minimised window to being reported as a capture size — which \
                     ends the recording, because a size change cannot be followed inside one \
                     file (issue #383)"
                ),
                Err(error) => panic!(
                    "a minimised window is still a window, and must not be reported as \
                     anything else: {error}"
                ),
            }
        }

        assert!(
            said_minimised,
            "three seconds of acquisitions against a minimised window said only that nothing \
             arrived; a session cannot tell that from a paused game, so it writes an empty \
             file and reports it as an idle source (issue #383)"
        );

        // SAFETY: `window` is still the live test window.
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                window,
                windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
            )
        };
        pump_messages();

        // Restoring has to leave a capture that still works and has to stop the
        // backend saying the window is minimised: a report that never cleared
        // would have a restored window recorded as a permanently minimised one.
        // A `STATIC` window with nothing drawing into it may legitimately
        // compose nothing, so what is asserted about frames is that acquisition
        // keeps answering rather than that one arrives.
        //
        // **What this window cannot be used to assert**, and where the answer is
        // instead. Restoring a real application's window makes the compositor
        // produce one frame of the *minimised* shape after `IsIconic` has gone
        // false — 160x28, measured on Windows 11 build 26200 — and reporting it
        // as a size change finishes the recording of a window that is back on
        // screen at the size it always was (issue #383). The backend now tells
        // that apart by asking `GetClientRect`, which answers 0x0 for a window
        // in that state. This window answers 146x28 instead, and keeps
        // answering it: it is a `STATIC` window whose thread spends its life
        // inside `acquire`, so its restore never completes and it really is
        // that size — a correct answer about a window nobody is running, and
        // not the state a recording meets. Asserting on it here would be
        // asserting on the test's own artefact. The evidence for the restore
        // behaviour is the end-to-end run recorded on the issue:
        // `test-apps/video-pattern` minimised for five seconds mid-recording
        // produced one 823-frame file that ended only when the window closed.
        let mut still_minimised = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            pump_messages();
            match backend.acquire(Duration::from_millis(100)) {
                Ok(Acquisition::SizeChanged(size)) => {
                    backend.resize(size).expect("the pool can be recreated");
                }
                Ok(Acquisition::TargetMinimised) => still_minimised = true,
                Ok(_) => still_minimised = false,
                Err(error) => panic!("capture did not survive the window being restored: {error}"),
            }
        }
        assert!(
            !still_minimised,
            "the window was restored and the backend is still reporting it as minimised"
        );

        drop(backend);
        // SAFETY: `window` is live and was created on this thread.
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(window) };
    }

    #[test]
    fn a_minimised_window_is_declined_before_a_recording_is_started_for_it() {
        // The other half of issue #383, and the half that costs nothing to
        // check: `select` asks every candidate before a file exists, so a
        // backend that says yes here is a backend that opens an encoder session
        // and a container header for a window it can never get a frame from.
        let size = FrameSize::new(1320, 900).expect("1320x900 is a valid size");
        let minimised = TargetProperties::new(TargetKind::Window, size).with_minimised(true);

        if !WindowsGraphicsCapture::is_supported_here()
            && skipped("GraphicsCaptureSession::IsSupported reports false here")
        {
            return;
        }

        match WindowsGraphicsCapture.availability(&minimised) {
            Availability::Unavailable(Unavailable::UnsupportedTarget { reason }) => assert!(
                reason.contains("minimised"),
                "the reason has to name the thing the user can put right: {reason}"
            ),
            other => panic!("a minimised window must be declined, not accepted: {other:?}"),
        }

        // The same window, restored, is the ordinary case — without this the
        // assertion above would pass just as well against a backend that
        // declined every window there is.
        assert!(
            matches!(
                WindowsGraphicsCapture
                    .availability(&TargetProperties::new(TargetKind::Window, size)),
                Availability::Available
            ),
            "a window that is not minimised is exactly what this backend is for"
        );
    }

    #[test]
    fn a_window_that_closes_mid_capture_is_reported_as_lost() {
        // A regression test for a real defect, found by the lifecycle run in
        // `examples/wgc_probe.rs` and not by any amount of reading: the backend
        // subscribed to `GraphicsCaptureItem::Closed` and trusted it, and on
        // Windows 11 build 26200 that event is never delivered to a capture
        // thread with no dispatcher queue. Destroying the window produced an
        // endless run of `Acquisition::Timeout` instead of
        // `CaptureError::TargetLost`, so a session would have waited for ever
        // on a window that no longer existed and never finalised its recording
        // (AGENTS.md sections 16 and 17).
        let Some((mut backend, window)) = a_capture_of_a_real_window() else {
            return;
        };

        // Let capture settle, then take the window away underneath it.
        for _ in 0..4 {
            let _ = backend.acquire(Duration::from_millis(50));
        }
        // SAFETY: `window` is live and was created on this thread, which is
        // what `DestroyWindow` requires.
        let destroyed =
            unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(window) }.is_ok();
        assert!(destroyed, "the test window should have been destroyable");

        // Generous, because this is a real compositor: what is being asserted
        // is that the answer arrives at all, not how quickly.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match backend.acquire(Duration::from_millis(100)) {
                Err(CaptureError::TargetLost { .. }) => return,
                Err(other) => panic!("expected the target to be reported lost, got: {other}"),
                Ok(_) => assert!(
                    Instant::now() < deadline,
                    "the backend kept reporting acquisitions for a window that had been \
                     destroyed; a session would never finalise its recording"
                ),
            }
        }
    }

    #[test]
    fn a_time_span_becomes_a_performance_counter_timestamp_in_nanoseconds() {
        // One 60 Hz frame interval is 166,666 TimeSpan ticks. The conversion
        // has to land on the same clock the audio side reads, or the two
        // streams cannot be compared at all.
        let ticks = NonZeroU64::new(TIMESPAN_TICKS_PER_SECOND).expect("10 MHz is not zero");
        let first = CaptureTimestamp::from_performance_counter(1_000_000, ticks);
        let second = CaptureTimestamp::from_performance_counter(1_166_666, ticks);

        assert_eq!(first.clock(), crate::SourceClock::PerformanceCounter);
        assert_eq!(
            second
                .duration_since(first)
                .expect("both readings are on the same clock"),
            Duration::from_nanos(16_666_600)
        );
    }

    #[test]
    fn a_minimised_window_has_no_frame_size() {
        assert_eq!(
            frame_size(SizeInt32 {
                Width: 0,
                Height: 0
            }),
            None,
            "a zero-sized client area is what a minimised window reports"
        );
        assert_eq!(
            frame_size(SizeInt32 {
                Width: -1,
                Height: 1080
            }),
            None
        );
        assert_eq!(
            frame_size(SizeInt32 {
                Width: 1920,
                Height: 1080
            }),
            FrameSize::new(1920, 1080)
        );
    }

    #[test]
    fn a_window_with_an_odd_dimension_is_captured_one_row_short_of_it_rather_than_not_at_all() {
        // Issue #561, end to end through this backend. Before it, a window like
        // this reached the encoder at its own odd size and *every* encoder
        // refused it, so the recording failed before its first frame — and under
        // ADR 0012 a window resized into this shape failed every recording after
        // it for the rest of the sitting.
        //
        // Three things have to hold together, and getting any one of them wrong
        // is silent:
        //
        // 1. the size reported is even, or nothing can encode it;
        // 2. the texture handed over is *that* size, or the track declares a
        //    shape the pictures in it do not have (AGENTS.md section 22);
        // 3. the frame pool keeps the content's own odd size, or every frame's
        //    `ContentSize` disagrees with it and the backend reports a resize for
        //    ever — which under ADR 0012 is a new file every few milliseconds.
        if !WindowsGraphicsCapture::is_supported_here()
            && skipped("GraphicsCaptureSession::IsSupported reports false here")
        {
            return;
        }
        let Some(window) = a_real_popup_window(987, 593) else {
            skipped("this machine would not create a window");
            return;
        };
        let _window = OwnedWindow(window);
        paint(window);

        let measured =
            crate::windows::client_size(window).expect("a visible window has a client area");
        let target = CaptureTarget::new(
            TargetHandle::from_raw(window.0 as u64),
            TargetProperties::new(TargetKind::Window, measured),
        );

        // `Running` rather than the trait, because the pool's size is the third
        // fact above and nothing outside this file can see it.
        let running = match Running::start(&target, &CaptureConfig::default()) {
            Ok(running) => running,
            Err(error) => {
                skipped(&format!(
                    "this machine would not capture a plain window: {error}"
                ));
                return;
            }
        };

        let content = frame_size(running.pool_size).expect("a visible window has content");
        assert!(
            content.width() % 2 == 1 || content.height() % 2 == 1,
            "the compositor reports {content} for a window created with a 987x593 client \
             area, so this test is no longer exercising an odd size at all"
        );
        assert_eq!(
            running.format.size(),
            content
                .rounded_down_to_even()
                .expect("987x593 has an even picture inside it"),
            "the size reported is what the encoder is configured for and what the Matroska \
             track declares, and an odd one is refused by all four encoders"
        );
        assert!(
            running.crop.is_some(),
            "an odd source with no crop means the texture is a row taller than the frame \
             says it is"
        );

        let recorded = running.format.size();
        let mut backend = GraphicsCaptureBackend {
            running: Some(running),
        };

        let mut seen = 0_u32;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && seen < 2 {
            paint(window);
            pump_messages();
            match backend.acquire(Duration::from_millis(200)) {
                Ok(Acquisition::Frame(frame)) => {
                    seen += 1;
                    assert_eq!(
                        frame.format().size(),
                        recorded,
                        "every frame declares the size the encoder was configured for"
                    );
                    assert_eq!(
                        texture_size(&frame),
                        Some((recorded.width(), recorded.height())),
                        "the frame declares {recorded} and hands over a texture of another \
                         size; the software encoder refuses that outright and the three \
                         hardware encoders are handed a surface their session was not opened \
                         for"
                    );
                }
                Ok(Acquisition::Timeout | Acquisition::TargetMinimised) => {}
                Ok(Acquisition::SizeChanged(size)) => panic!(
                    "nothing resized this window: a frame pool one row shorter than the \
                     content reports {size} for ever, and a session follows every one of \
                     them with a new file (ADR 0012)"
                ),
                Err(error) => panic!("a window that is on screen must be capturable: {error}"),
            }
        }

        assert!(
            seen > 0,
            "five seconds of acquisitions against a painted window produced no frame at all, \
             so nothing above about the texture was checked"
        );
        backend.shut_down();
    }

    /// The dimensions of a frame's own texture, which is the half of
    /// [`FrameFormat`] nothing else can check.
    fn texture_size(frame: &CapturedFrame<'_>) -> Option<(u32, u32)> {
        let raw = frame.texture().as_raw();
        // SAFETY: the pointer came from a live `CapturedFrame`, whose texture the
        // backend guarantees is a valid `ID3D11Texture2D` for as long as the
        // frame exists. `from_raw_borrowed` takes no reference of its own.
        let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }?;

        let mut description = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `texture` is live and `description` is a live local the call
        // writes into; `GetDesc` reads what the runtime already holds.
        unsafe { texture.GetDesc(&raw mut description) };
        Some((description.Width, description.Height))
    }

    /// Creates a borderless top-level window whose *client* area is exactly
    /// `width` by `height`.
    ///
    /// `WS_POPUP` rather than [`a_real_window`]'s `WS_OVERLAPPEDWINDOW`, because
    /// a popup has no border and no caption: the size asked for is the client
    /// area, which is the thing a test about an odd client area has to control.
    fn a_real_popup_window(width: i32, height: i32) -> Option<HWND> {
        // SAFETY: `STATIC` is a system window class; both strings are static
        // wide literals living for the whole program; no parent, menu, instance
        // or creation parameter is passed. The caller destroys the window.
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE,
                windows::core::w!("STATIC"),
                windows::core::w!("clipped odd size test window"),
                windows::Win32::UI::WindowsAndMessaging::WS_POPUP
                    | windows::Win32::UI::WindowsAndMessaging::WS_VISIBLE,
                60,
                60,
                width,
                height,
                None,
                None,
                None,
                None,
            )
        }
        .ok()
    }

    /// Fills the window with a colour, which is what makes the compositor
    /// produce a frame for it.
    fn paint(window: HWND) {
        let mut rect = windows::Win32::Foundation::RECT::default();
        // SAFETY: `rect` is a live local, which is all `GetClientRect` needs.
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetClientRect(window, &raw mut rect)
        };

        // SAFETY: the window is live; the device context is released below, and
        // the brush is deleted after the fill that uses it.
        unsafe {
            let context = windows::Win32::Graphics::Gdi::GetDC(Some(window));
            let brush = windows::Win32::Graphics::Gdi::CreateSolidBrush(
                windows::Win32::Foundation::COLORREF(0x0020_A0F0),
            );
            let _ = windows::Win32::Graphics::Gdi::FillRect(context, &raw const rect, brush);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(brush.into());
            let _ = windows::Win32::Graphics::Gdi::ReleaseDC(Some(window), context);
        }
    }

    /// Destroys a test window however its test ends, including a panic.
    struct OwnedWindow(HWND);

    impl Drop for OwnedWindow {
        fn drop(&mut self) {
            // SAFETY: the window was created on this thread by the test and is
            // destroyed exactly once, here.
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0) };
            pump_messages();
        }
    }
}
