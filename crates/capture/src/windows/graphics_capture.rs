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
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use crate::windows::apartment::ensure_multi_threaded_apartment;
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
        loop {
            match running.take_next_frame()? {
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
            if !running.wait_for_frame(deadline) {
                // Nothing arrived. Before reporting that as ordinary, ask
                // whether there is still anything to wait for: see
                // `Running::target_has_gone` for why the `Closed` event is not
                // enough on its own.
                if running.target_has_gone() {
                    return Err(CaptureError::TargetLost { method: METHOD });
                }
                return Ok(Acquisition::Timeout);
            }
        }
    }

    fn resize(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError> {
        let running = self
            .running
            .as_mut()
            .ok_or(CaptureError::NotInitialised { method: METHOD })?;
        let format = running.recreate_frame_pool(size)?;

        tracing::info!(
            width = size.width(),
            height = size.height(),
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
    /// Frames the pool is known to have discarded before this one.
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
    pool_size: SizeInt32,
    /// The format `initialise` or `resize` last reported.
    format: FrameFormat,
    /// The frame lent to the caller, still owned here.
    held: Option<HeldFrame>,
    /// A size change that has been reported and not yet acted on. While this is
    /// set the backend is idle and every acquisition repeats the report, which
    /// is what the trait documents.
    awaiting_resize: Option<FrameSize>,
    /// Frames handed to the caller, for the dropped-frame accounting below.
    delivered: u64,
    /// Frames taken from the pool and thrown away by this backend, which
    /// happens for the frame that reveals a size change and for a frame that
    /// arrives with a zero dimension because the window was minimised.
    discarded: u64,
    /// The running lower bound on frames the pool dropped; see
    /// [`Running::frames_missed_since_last_delivery`].
    lost: u64,
    /// The value of `lost` when the previous frame was delivered.
    lost_at_last_delivery: u64,
}

impl Running {
    /// Opens a capture of `target`.
    fn start(target: &CaptureTarget, config: &CaptureConfig) -> Result<Self, CaptureError> {
        ensure_multi_threaded_apartment()
            .map_err(|error| backend_error("preparing the COM apartment", error))?;

        let (item, window) = capture_item_for(target)?;
        let pool_size = item
            .Size()
            .map_err(|error| backend_error("reading the capture item's size", error))?;
        let size = frame_size(pool_size).ok_or(CaptureError::UnsupportedTarget {
            method: METHOD,
            target: target.properties().kind(),
            reason: "it currently has no visible content — a minimised window has a \
                     zero-sized client area, so there is nothing to capture until it is \
                     restored",
        })?;

        let device = CaptureDevice::create()
            .map_err(|error| backend_error("creating the Direct3D 11 device", error))?;

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
            .map_err(|error| backend_error("creating the capture session", error))?;

        configure_session(&session, config);

        let signal = Arc::new(CaptureSignal::default());

        let frame_arrived = {
            let signal = Arc::clone(&signal);
            pool.FrameArrived(&TypedEventHandler::new(move |_pool, _arguments| {
                signal.record_arrival();
                Ok(())
            }))
            .map_err(|error| backend_error("subscribing to captured frames", error))?
        };

        let item_closed = {
            let signal = Arc::clone(&signal);
            item.Closed(&TypedEventHandler::new(move |_item, _arguments| {
                signal.record_closed();
                Ok(())
            }))
            .map_err(|error| backend_error("subscribing to the capture target closing", error))?
        };

        session
            .StartCapture()
            .map_err(|error| backend_error("starting the capture session", error))?;

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
            held: None,
            awaiting_resize: None,
            delivered: 0,
            discarded: 0,
            lost: 0,
            lost_at_last_delivery: 0,
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
    fn take_next_frame(&mut self) -> Result<Taken, CaptureError> {
        let Some(frame) = self.try_get_next_frame()? else {
            return Ok(Taken::Nothing);
        };

        let content_size = frame
            .ContentSize()
            .map_err(|error| backend_error("reading a frame's content size", error))?;

        if content_size != self.pool_size {
            self.discarded += 1;
            drop(frame);
            return match frame_size(content_size) {
                Some(size) => Ok(Taken::SizeChanged(size)),
                // A window being minimised reports a zero-sized client area.
                // That is not a size to reconfigure an encoder for, and the
                // window is about to stop producing frames anyway, so it reads
                // as nothing having arrived; capture resumes when it is
                // restored, at which point the size is real and a genuine
                // `SizeChanged` follows.
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

        self.delivered += 1;
        let frames_missed = self.frames_missed_since_last_delivery();

        self.held = Some(HeldFrame {
            frame,
            texture,
            timestamp: CaptureTimestamp::from_performance_counter(
                // `TimeSpan::Duration` is signed and counts from an arbitrary
                // system-relative origin. It is never negative for a captured
                // frame; clamping rather than casting keeps a driver that ever
                // reported one from wrapping to an enormous timestamp.
                timestamp.Duration.max(0).unsigned_abs(),
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

    /// How many frames the pool is known to have dropped since the previous
    /// delivery.
    ///
    /// Windows Graphics Capture has no "frames missed" field, so this is
    /// derived, and it is derived as a *lower bound* rather than a guess.
    /// `FrameArrived` fires once per frame the compositor produced, so with `A`
    /// arrivals, `D` frames delivered to the caller, `X` frames this backend
    /// discarded and `L` frames lost, the pool currently holds `A - D - X - L`
    /// frames, and it can hold no more than [`FRAME_POOL_BUFFERS`]. So at least
    /// `A - D - X - FRAME_POOL_BUFFERS` frames have been lost. Losses only
    /// accumulate, so the bound is kept monotonic and the reported figure is its
    /// increment since the last frame.
    ///
    /// It under-reports rather than over-reports: a short burst that the pool
    /// absorbed and gave back is not counted. That is the right direction for a
    /// number a user reads as "your machine could not keep up".
    fn frames_missed_since_last_delivery(&mut self) -> u32 {
        let arrivals = self.signal.arrivals();
        let accounted = self
            .delivered
            .saturating_add(self.discarded)
            .saturating_add(u64::try_from(FRAME_POOL_BUFFERS).unwrap_or(0));
        self.lost = self.lost.max(arrivals.saturating_sub(accounted));

        let missed = self.lost.saturating_sub(self.lost_at_last_delivery);
        self.lost_at_last_delivery = self.lost;
        u32::try_from(missed).unwrap_or(u32::MAX)
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
    fn target_has_gone(&self) -> bool {
        let Some(window) = self.window else {
            return false;
        };
        // SAFETY: `IsWindow` only reads the window table, and reporting that a
        // handle is no longer a window is exactly what it is for; passing a
        // stale handle is sound and is the case being asked about.
        !unsafe { IsWindow(Some(window)) }.as_bool()
    }

    /// Blocks until a frame arrives, the target closes, or `deadline` passes.
    ///
    /// Returns whether it is worth looking in the pool again.
    fn wait_for_frame(&self, deadline: Instant) -> bool {
        self.signal.wait_until(deadline)
    }

    /// Rebuilds the frame pool for a new target size.
    fn recreate_frame_pool(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError> {
        self.release_held_frame();

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
        self.format = FrameFormat::new(size, PixelFormat::Bgra8Unorm);
        self.awaiting_resize = None;

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
/// thread-pool thread once per composed frame — up to 360 times a second on a
/// high-refresh display — so the handler does no allocation, no logging and no
/// COM call: it takes a lock held for two field updates and wakes the waiter.
#[derive(Debug, Default)]
struct CaptureSignal {
    state: Mutex<SignalState>,
    changed: Condvar,
}

/// The two facts the handlers report.
#[derive(Debug, Default, Clone, Copy)]
struct SignalState {
    /// Frames the compositor has composed since capture started. Monotonic, and
    /// the basis of the dropped-frame accounting.
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
    /// what [`Acquisition::Timeout`] reports. The comparison is against the
    /// arrival count read before waiting, so a frame that arrives between the
    /// caller's last look in the pool and this wait is not missed.
    fn wait_until(&self, deadline: Instant) -> bool {
        let mut state = self.lock();
        let arrivals_before = state.arrivals;

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
            let item = unsafe { interop.CreateForWindow(window) }
                .map_err(|error| backend_error("creating a capture item for the window", error))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CaptureMethodSetting, TargetHandle};

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

        if !WindowsGraphicsCapture::is_supported_here() {
            eprintln!("skipped: Windows Graphics Capture is unsupported on this machine");
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
        if !WindowsGraphicsCapture::is_supported_here() {
            eprintln!("skipped: Windows Graphics Capture is unsupported on this machine");
            return;
        }
        let Some(window) = a_real_window() else {
            eprintln!("skipped: this machine would not create a window");
            return;
        };

        let size = FrameSize::new(320, 240).expect("320x240 is a valid size");
        let target = CaptureTarget::new(
            TargetHandle::from_raw(window.0 as u64),
            TargetProperties::new(TargetKind::Window, size),
        );

        let mut backend = WindowsGraphicsCapture.create().expect("creation succeeds");
        let initialised = backend.initialise(&target, &CaptureConfig::default());
        if let Err(error) = initialised {
            // SAFETY: `window` is the window created above and not yet
            // destroyed.
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(window) };
            eprintln!("skipped: this machine would not capture a plain window: {error}");
            return;
        }

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
}
