//! The Direct3D 11 window the pattern is drawn into.
//!
//! # What this is, and what it is not
//!
//! It is the smallest thing that can present a chosen image at a chosen rate:
//! a window, a flip-model swap chain, and one staging texture that the pattern
//! is written into and copied from. There is no shader, no vertex buffer and no
//! render target view, because the pattern is defined in pixels
//! ([`crate::pattern`]) and a copy from a mapped texture reproduces it exactly.
//! Anything drawn by a rasteriser would be subject to the driver's sampling and
//! rounding, and "the capture is wrong" and "the triangle landed half a pixel
//! left" would become indistinguishable.
//!
//! It is not a measurement harness. `crates/capture/examples/wgc_probe.rs`
//! measures pacing, dropped frames and leaks in the *capture backend*, and
//! brings its own window to do it; this is the *subject* a test points a
//! capture at. The two overlap in about a hundred lines of window and swap
//! chain creation, and `docs/testing.md` records why they are still separate
//! and what would have to happen for the probe to use this instead.
//!
//! # Ownership
//!
//! [`PatternWindow`] owns every native resource here — the window, the Direct3D
//! device and its immediate context, the swap chain and the staging texture —
//! for its whole life, and they are released in the order Windows requires:
//! exclusive fullscreen is left first (DXGI leaves the display in the mode the
//! application chose if the swap chain is released while fullscreen), then the
//! COM interfaces, then the window — DXGI's own guidance is that a swap chain
//! goes before the window it presents to.
//!
//! That order is the struct's field order rather than a sequence of statements,
//! because Rust drops a struct's fields in declaration order and `Drop::drop`
//! runs before any of them. So [`PatternWindow::drop`] does only the one thing
//! that has to happen before everything else — leaving fullscreen — and the
//! window is last in the struct, behind the COM interfaces, held by
//! [`OwnedWindow`], whose only job is to destroy it. Nothing here is shared with
//! another thread (AGENTS.md sections 20 and 58).
//!
//! # Threading
//!
//! One window, one thread. The window is created on the thread that calls
//! [`PatternWindow::open`], its messages are pumped on that thread, and every
//! present happens there. The only things that cross a thread boundary are the
//! two stop flags in [`crate::app`], which are atomics.

use windows::core::{w, Interface, BOOL, PCWSTR};
use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_WRITE,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_DESC, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH, DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, PeekMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, MSG, PM_REMOVE, SW_SHOWNOACTIVATE, WM_CLOSE,
    WM_DESTROY, WM_QUIT, WNDCLASSW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
};

use crate::app::{AppError, Presentation};
use crate::pattern::{self, SurfaceMut};

/// The window class name, registered on the first window and reused after that.
const CLASS_NAME: PCWSTR = w!("ClippedVideoPatternWindow");

/// Frames presented before an exclusive fullscreen transition is asked for.
///
/// DXGI will not go fullscreen on a swap chain that has never presented,
/// because it has no output to go fullscreen on until it has.
const WARM_UP_FRAMES: u32 = 10;

/// Where and how the window is put on screen.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Placement {
    /// Left edge in virtual-desktop coordinates.
    pub(crate) x: i32,
    /// Top edge in virtual-desktop coordinates.
    pub(crate) y: i32,
    /// Client width in physical pixels.
    pub(crate) width: u32,
    /// Client height in physical pixels.
    pub(crate) height: u32,
    /// Which of the four ways of presenting is wanted.
    pub(crate) presentation: Presentation,
}

/// A window handle that is destroyed when it is dropped.
///
/// It exists so that the window outlives the swap chain that presents to it:
/// the fields of a struct are dropped in declaration order, so putting this
/// last in [`PatternWindow`] is what puts `DestroyWindow` after the COM
/// interfaces are released, without a `Drop` implementation having to move
/// anything out of a `&mut self`.
#[derive(Debug)]
struct OwnedWindow(HWND);

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        // SAFETY: `self.0` was created on this thread and, unless the user
        // closed the window, has not been destroyed. Destroying an
        // already-destroyed window fails rather than misbehaving, which is why
        // the result is discarded rather than checked.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// A window presenting the test pattern.
///
/// The field order is the teardown order and is not arbitrary; see the module
/// documentation.
#[derive(Debug)]
pub(crate) struct PatternWindow {
    swap_chain: IDXGISwapChain1,
    /// The CPU-writable texture each frame is drawn into before being copied
    /// into the back buffer. Recreated when the swap chain changes size.
    staging: ID3D11Texture2D,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    /// Destroyed after everything above it, which is what DXGI asks for.
    window: OwnedWindow,
    width: u32,
    height: u32,
    /// Frames presented so far, which is also the counter drawn into the next
    /// one. Every present goes through [`PatternWindow::present_next`], so the
    /// warm-up frames an exclusive transition needs are counted like any other
    /// and no counter is ever drawn twice in a run.
    presented: u32,
    /// Whether `SetFullscreenState(true)` was granted, so that [`Drop`] knows
    /// whether it has a display mode to give back.
    ///
    /// Followed rather than assumed after it is set: Windows takes the display
    /// back on its own, and this is compared against
    /// `IDXGISwapChain::GetFullscreenState` before every present
    /// ([issue #178](https://github.com/wildware-uk/clipped/issues/178)).
    exclusive: bool,
    /// How many times Windows changed the presentation this application did not
    /// ask it to.
    ///
    /// Reported rather than swallowed. A capture test reads `exclusive=yes` from
    /// the `ready` line and would otherwise have no way to learn the display was
    /// handed back a frame later — so it could not tell "captured a window that
    /// owns its display" from "captured a borderless window" (AGENTS.md section
    /// 16).
    involuntary_transitions: u32,
    /// Set when the window has been destroyed or asked to close, so the run
    /// loop stops presenting into a swap chain whose window has gone.
    closed: bool,
}

impl PatternWindow {
    /// Creates the window, its device and its swap chain.
    ///
    /// The window is created without taking the foreground
    /// (`SW_SHOWNOACTIVATE`) and, in every mode except fullscreen, on top of
    /// other windows. Both are deliberate: a test application that steals focus
    /// from whoever is at the keyboard gets minimised, and a minimised window
    /// stops being composed, which stops it being captured — the failure that
    /// `crates/capture/examples/wgc_probe.rs` documents at length.
    ///
    /// # Errors
    ///
    /// [`AppError::Windows`] naming the call that failed, or
    /// [`AppError::PatternDoesNotFit`] if the client area Windows actually gave
    /// is too small for the pattern to be decodable.
    pub(crate) fn open(placement: Placement) -> Result<Self, AppError> {
        // SAFETY: `GetModuleHandleW(None)` returns this executable's module
        // handle and takes no pointer arguments.
        let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }
            .map_err(|source| AppError::windows("GetModuleHandleW", source))?;

        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_procedure),
            hInstance: instance.into(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: `class` is a live, fully initialised `WNDCLASSW` whose string
        // and module fields are static and outlive the call. Registering the
        // same class twice is refused rather than harmful, and refusing it is
        // the expected outcome of the second call: `open` is public API through
        // `clipped_video_pattern::run`, so nothing stops a caller opening a
        // second window in one process (AGENTS.md section 16).
        let atom = unsafe { RegisterClassW(&class) };
        if atom == 0 {
            let error = windows::core::Error::from_thread();
            if error.code() != ERROR_CLASS_ALREADY_EXISTS.to_hresult() {
                return Err(AppError::windows("RegisterClassW", error));
            }
        }

        let (style, extended_style) = match placement.presentation {
            Presentation::Windowed => (WS_OVERLAPPEDWINDOW | WS_VISIBLE, WS_EX_TOPMOST),
            Presentation::Borderless => (WS_POPUP | WS_VISIBLE, WS_EX_TOPMOST),
            // A fullscreen window already covers its display, and DXGI wants to
            // manage the z-order of one it takes exclusively, so topmost is not
            // asked for here.
            Presentation::FullscreenBorderless | Presentation::FullscreenExclusive => {
                (WS_POPUP | WS_VISIBLE, Default::default())
            }
        };

        // A windowed presentation is sized by its *client* area, so that the
        // pattern is a known number of pixels rather than a number that varies
        // with how thick this Windows version draws a border.
        let (width, height) = if matches!(placement.presentation, Presentation::Windowed) {
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: i32::try_from(placement.width).unwrap_or(i32::MAX),
                bottom: i32::try_from(placement.height).unwrap_or(i32::MAX),
            };
            // SAFETY: `rect` is a live local the call adjusts in place; the
            // style is the one the window is about to be created with.
            unsafe { AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false) }
                .map_err(|source| AppError::windows("AdjustWindowRect", source))?;
            (rect.right - rect.left, rect.bottom - rect.top)
        } else {
            (
                i32::try_from(placement.width).unwrap_or(i32::MAX),
                i32::try_from(placement.height).unwrap_or(i32::MAX),
            )
        };

        // SAFETY: the class was registered immediately above, both strings are
        // static wide literals, and no parent, menu or creation parameter is
        // passed. The returned window is owned by this struct and destroyed in
        // `Drop`.
        let window = unsafe {
            CreateWindowExW(
                extended_style,
                CLASS_NAME,
                w!("Clipped video pattern"),
                style,
                placement.x,
                placement.y,
                width,
                height,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }
        .map_err(|source| AppError::windows("CreateWindowExW", source))?;

        // The handle is owned from here on: if anything below fails, dropping
        // this is what destroys the window (AGENTS.md section 58).
        let window = OwnedWindow(window);

        // SAFETY: `window.0` is the window created immediately above.
        let _ = unsafe { ShowWindow(window.0, SW_SHOWNOACTIVATE) };

        Self::assemble(window)
    }

    /// Builds the device, swap chain and staging texture for an existing
    /// window.
    fn assemble(window: OwnedWindow) -> Result<Self, AppError> {
        let (width, height) = client_size(window.0)?;
        if width < pattern::MIN_WIDTH || height < pattern::MIN_HEIGHT {
            return Err(AppError::PatternDoesNotFit { width, height });
        }

        let mut device = None;
        let mut context = None;
        let levels = [D3D_FEATURE_LEVEL_11_0];

        // SAFETY: `levels` is a live local read during the call, and the two
        // out parameters are the addresses of live `Option`s, which is how
        // windows-rs represents them. No adapter and no software module is
        // requested, which selects the default hardware adapter. Both returned
        // interfaces are owned and released by their `Drop`.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|source| AppError::windows("D3D11CreateDevice", source))?;

        let device = device.ok_or_else(|| {
            AppError::windows(
                "D3D11CreateDevice",
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "reported success without returning a device",
                ),
            )
        })?;
        let context = context.ok_or_else(|| {
            AppError::windows(
                "D3D11CreateDevice",
                windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "reported success without returning a device context",
                ),
            )
        })?;

        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|source| AppError::windows("ID3D11Device as IDXGIDevice", source))?;
        // SAFETY: `dxgi` is a live DXGI device; `GetAdapter` is an ordinary
        // query returning an owned interface.
        let adapter = unsafe { dxgi.GetAdapter() }
            .map_err(|source| AppError::windows("IDXGIDevice::GetAdapter", source))?;
        // SAFETY: `adapter` is live; `GetParent` returns the factory that
        // created it, checked against the requested interface by windows-rs.
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }
            .map_err(|source| AppError::windows("IDXGIAdapter::GetParent", source))?;

        // The flip model, because it is what a modern game presents through and
        // the only model `SetFullscreenState` still works with on current
        // Windows. `ALLOW_MODE_SWITCH` is what makes the exclusive transition
        // legal at all.
        let description = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            // A plain `as` cast rather than `unsigned_abs`: this is a bit
            // pattern in a signed newtype, not a number, and the absolute value
            // of a flag whose sign bit is set is a different flag altogether.
            Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0 as u32,
            ..Default::default()
        };
        let fullscreen = DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
            RefreshRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            Windowed: true.into(),
            ..Default::default()
        };

        // SAFETY: `device` is a live Direct3D 11 device, `window` is a live
        // window owned by this thread, and both descriptions are live locals
        // read during the call.
        let swap_chain = unsafe {
            factory.CreateSwapChainForHwnd(&device, window.0, &description, Some(&fullscreen), None)
        }
        .map_err(|source| AppError::windows("CreateSwapChainForHwnd", source))?;

        let staging = create_staging(&device, width, height)?;

        Ok(Self {
            swap_chain,
            staging,
            context,
            device,
            window,
            width,
            height,
            presented: 0,
            exclusive: false,
            involuntary_transitions: 0,
            closed: false,
        })
    }

    /// The window's handle, which is what a capture test points at.
    pub(crate) const fn handle(&self) -> HWND {
        self.window.0
    }

    /// The client size the pattern is being drawn at.
    pub(crate) const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Whether the display was taken exclusively.
    pub(crate) const fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    /// How many frames have been presented, which is one more than the last
    /// counter drawn.
    ///
    /// This is the number the `stopped` line reports, and a capture test checks
    /// the counters it decoded against it — so it has to count *every* present,
    /// including the warm-up frames an exclusive transition needs
    /// (`docs/testing.md`).
    pub(crate) const fn presented(&self) -> u32 {
        self.presented
    }

    /// Asks DXGI for the display, and reports whether it was given.
    ///
    /// The sequence is the documented one, in order, because DXGI refuses the
    /// transition when any part of it is missing: the window has to be in the
    /// foreground, the swap chain has to have presented at least once so that
    /// its output is known, and the target has to be resized to a real mode
    /// before the transition is asked for.
    ///
    /// The frames presented to satisfy the second of those carry the counters
    /// they would have carried anyway, and are counted: they are frames the
    /// application really presented, a capture running early really can see
    /// them, and drawing counter zero here and again in the run loop would make
    /// the same counter arrive twice — which a capture test is entitled to read
    /// as the capture backend duplicating a frame.
    ///
    /// A refusal is a legitimate outcome and is reported as `false` rather than
    /// as an error. Windows only grants the foreground to a process the user
    /// has interacted with, so an application launched by a test frequently
    /// cannot go exclusive, and a test that treated that as a failure would be
    /// reporting Windows' focus policy (AGENTS.md section 16).
    ///
    /// # Errors
    ///
    /// [`AppError::Windows`] only when the swap chain could not be put back
    /// into a usable state afterwards — a resize the caller cannot recover
    /// from.
    pub(crate) fn take_display_exclusively(&mut self) -> Result<bool, AppError> {
        // SAFETY: `self.window.0` is live and owned by this thread.
        let foreground = unsafe { SetForegroundWindow(self.window.0) }.as_bool();

        // Present a few frames first: DXGI needs the swap chain to have an
        // output before it can go exclusive on it.
        for _ in 0..WARM_UP_FRAMES {
            self.present_next()?;
            self.pump_messages();
        }

        let mode = DXGI_MODE_DESC {
            Width: self.width,
            Height: self.height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            ..Default::default()
        };
        // SAFETY: `self.swap_chain` is live and `mode` is a live local read
        // during the call.
        if let Err(error) = unsafe { self.swap_chain.ResizeTarget(&mode) } {
            eprintln!("[warn] ResizeTarget before the fullscreen transition failed: {error}");
        }

        // SAFETY: `self.swap_chain` is live; `GetContainingOutput` returns the
        // output the window is on, which is the one to go exclusive on.
        let output = unsafe { self.swap_chain.GetContainingOutput() }.ok();
        // SAFETY: `self.swap_chain` is live, and `output` is either absent — in
        // which case DXGI chooses — or the output DXGI itself just named.
        match unsafe { self.swap_chain.SetFullscreenState(true, output.as_ref()) } {
            Ok(()) => self.exclusive = true,
            Err(error) => {
                eprintln!(
                    "[warn] exclusive fullscreen was refused ({error}); presenting as a \
                     borderless window covering the display instead{}",
                    if foreground {
                        ""
                    } else {
                        ", and SetForegroundWindow was refused too, which is the usual cause"
                    }
                );
                return Ok(false);
            }
        }

        // The buffers follow whatever mode the swap chain ended up in, and the
        // staging texture has to follow the buffers.
        // SAFETY: no view of the back buffers is outstanding — `present` takes
        // one and drops it before returning — which is `ResizeBuffers`' one
        // precondition.
        //
        // The flags are the ones the swap chain was created with rather than
        // `Default::default()`. They should match, and they did not (issue
        // #178).
        unsafe {
            self.swap_chain.ResizeBuffers(
                0,
                0,
                0,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH,
            )
        }
        .map_err(|source| AppError::windows("IDXGISwapChain::ResizeBuffers", source))?;
        self.follow_swap_chain_size()?;

        Ok(true)
    }

    /// Reads the swap chain's current size and recreates the staging texture if
    /// it changed.
    fn follow_swap_chain_size(&mut self) -> Result<(), AppError> {
        // SAFETY: `self.swap_chain` is live; `GetDesc1` writes into a live
        // local and retains nothing.
        let description = unsafe { self.swap_chain.GetDesc1() }
            .map_err(|source| AppError::windows("IDXGISwapChain1::GetDesc1", source))?;

        if (description.Width, description.Height) == (self.width, self.height) {
            return Ok(());
        }
        if description.Width < pattern::MIN_WIDTH || description.Height < pattern::MIN_HEIGHT {
            return Err(AppError::PatternDoesNotFit {
                width: description.Width,
                height: description.Height,
            });
        }

        self.staging = create_staging(&self.device, description.Width, description.Height)?;
        self.width = description.Width;
        self.height = description.Height;
        Ok(())
    }

    /// Draws the next frame — the counter is [`PatternWindow::presented`] — and
    /// presents it.
    ///
    /// The counter belongs to the window rather than to the caller so that
    /// there is exactly one of it. Every frame this window ever presents is
    /// numbered from the same sequence, which is what makes "the counter drawn
    /// into the last frame plus one" and "the number of frames presented" the
    /// same number, and what a capture test's cross-check rests on
    /// (`docs/testing.md`).
    ///
    /// # Errors
    ///
    /// [`AppError::Windows`] if the swap chain, the map or the copy failed, and
    /// [`AppError::Pattern`] if the surface Windows handed back cannot hold the
    /// pattern. Either means the frames a test is about to capture would not be
    /// the frames it asked for, so presenting on regardless would be worse than
    /// stopping.
    pub(crate) fn present_next(&mut self) -> Result<(), AppError> {
        let index = self.presented;
        self.present(index)?;
        self.presented = self.presented.wrapping_add(1);
        Ok(())
    }

    /// Follows a fullscreen or windowed transition this application did not ask
    /// for.
    ///
    /// Flip-model swap chains require `ResizeBuffers` after **every** such
    /// transition, including the ones Windows makes on its own. Without this the
    /// first one kills the application — `Present` returns `0x887A0001` and the
    /// DXGI info queue says exactly why:
    ///
    /// ```text
    /// IDXGISwapChain::Present: The application has not called ResizeBuffers or
    /// re-created the SwapChain after a fullscreen or windowed transition.
    /// ```
    ///
    /// The trigger found in the wild was a display Windows had powered off: the
    /// compositor drops to about 4 Hz, `GetFullscreenState` goes `TRUE` for
    /// exactly one presented frame and then `FALSE`, and a capture test pointed
    /// at this application is left with no subject
    /// ([issue #178](https://github.com/wildware-uk/clipped/issues/178)).
    ///
    /// Asked before the back buffer is taken, because releasing every view of
    /// the buffers is `ResizeBuffers`' one precondition.
    ///
    /// A swap chain that cannot be asked is left alone: that is a device already
    /// in a state this cannot mend, and `Present` will report it in its own
    /// words a moment later rather than being pre-empted here.
    fn follow_involuntary_transition(&mut self) -> Result<(), AppError> {
        let mut fullscreen = BOOL(0);
        // SAFETY: `self.swap_chain` is live; `GetFullscreenState` writes into a
        // live local and no output is asked for.
        if unsafe {
            self.swap_chain
                .GetFullscreenState(Some(&mut fullscreen), None)
        }
        .is_err()
        {
            return Ok(());
        }

        let now = fullscreen.as_bool();
        if now == self.exclusive {
            return Ok(());
        }

        self.involuntary_transitions = self.involuntary_transitions.saturating_add(1);
        eprintln!(
            "[warn] Windows {} the display without being asked, so this run is presenting {} \
             from frame {} onwards",
            if now { "took" } else { "gave back" },
            if now {
                "exclusively"
            } else {
                "as a borderless window covering the display"
            },
            self.presented
        );
        self.exclusive = now;

        // SAFETY: no view of the back buffers is outstanding — this runs before
        // `GetBuffer` — which is `ResizeBuffers`' one precondition. The flags
        // are the ones the swap chain was created with.
        unsafe {
            self.swap_chain.ResizeBuffers(
                0,
                0,
                0,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH,
            )
        }
        .map_err(|source| {
            AppError::windows("IDXGISwapChain::ResizeBuffers after a transition", source)
        })?;
        self.follow_swap_chain_size()
    }

    /// How many times Windows changed the presentation on its own.
    pub(crate) const fn involuntary_transitions(&self) -> u32 {
        self.involuntary_transitions
    }

    /// Draws frame `index` and presents it.
    fn present(&mut self, index: u32) -> Result<(), AppError> {
        self.follow_involuntary_transition()?;

        // SAFETY: `self.swap_chain` is live and buffer zero is its back buffer.
        // The returned texture is owned here and released at the end of this
        // function, which is what leaves `ResizeBuffers` a clean slate.
        let back_buffer: ID3D11Texture2D = unsafe { self.swap_chain.GetBuffer(0) }
            .map_err(|source| AppError::windows("IDXGISwapChain::GetBuffer", source))?;

        self.draw(index)?;

        // SAFETY: both textures belong to `self.device` and were created with
        // the same size, format, mip count and sample count, which is what
        // `CopyResource` requires. Neither is mapped: `draw` unmaps before
        // returning.
        unsafe { self.context.CopyResource(&back_buffer, &self.staging) };

        // SAFETY: `self.swap_chain` is live. A sync interval of zero presents
        // without waiting for a vertical blank, because this application paces
        // itself.
        unsafe { self.swap_chain.Present(0, Default::default()) }
            .ok()
            .map_err(|source| AppError::windows("IDXGISwapChain::Present", source))
    }

    /// Writes frame `index` into the staging texture.
    fn draw(&mut self, index: u32) -> Result<(), AppError> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `self.staging` is a live staging texture created with
        // `CPU_ACCESS_WRITE`, which is what `D3D11_MAP_WRITE` on subresource
        // zero requires, and `mapped` is a live local the call writes into.
        unsafe {
            self.context
                .Map(&self.staging, 0, D3D11_MAP_WRITE, 0, Some(&raw mut mapped))
        }
        .map_err(|source| AppError::windows("ID3D11DeviceContext::Map", source))?;

        let result = self.write_pattern(&mapped, index);

        // Unmapped before the result is inspected: a staging texture left
        // mapped cannot be copied from, and the next frame would fail for a
        // reason that has nothing to do with why this one did.
        // SAFETY: `self.staging` is the texture mapped immediately above, and
        // the slice built from the mapping is dropped by now.
        unsafe { self.context.Unmap(&self.staging, 0) };

        result
    }

    /// Renders the pattern into a mapped staging texture.
    fn write_pattern(&self, mapped: &D3D11_MAPPED_SUBRESOURCE, index: u32) -> Result<(), AppError> {
        let stride = mapped.RowPitch as usize;
        let length =
            stride
                .checked_mul(self.height as usize)
                .ok_or(AppError::PatternDoesNotFit {
                    width: self.width,
                    height: self.height,
                })?;

        // SAFETY: `Map` returned a pointer to the whole of subresource zero,
        // whose rows are `RowPitch` bytes apart and of which there are `height`
        // — so the region is `RowPitch * height` bytes and is writable until
        // `Unmap`. Nothing else refers to it: the slice is built here, used
        // here, and dropped before `draw` unmaps.
        let pixels = unsafe { core::slice::from_raw_parts_mut(mapped.pData.cast::<u8>(), length) };

        let mut surface = SurfaceMut::new(pixels, stride, self.width, self.height)?;
        pattern::render(&mut surface, index)?;
        Ok(())
    }

    /// Drains the window's message queue, and reports whether the window has
    /// gone or been asked to close.
    pub(crate) fn pump_messages(&mut self) -> bool {
        let mut message = MSG::default();
        // SAFETY: `message` is a live local. `PM_REMOVE` with no window filter
        // takes messages for any window on this thread, which is this one.
        // `TranslateMessage` and `DispatchMessageW` read the message
        // `PeekMessageW` just wrote.
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == WM_QUIT {
                    self.closed = true;
                    continue;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        self.closed
    }
}

impl Drop for PatternWindow {
    /// Leaves exclusive fullscreen, and nothing else.
    ///
    /// The rest of the teardown is the fields' own: this runs first, then the
    /// swap chain, the staging texture, the context and the device are released
    /// in that order, and [`OwnedWindow`] destroys the window last.
    fn drop(&mut self) {
        if self.exclusive {
            // Required before the swap chain is released: DXGI leaves the
            // display in the mode this process chose otherwise, and the person
            // at the keyboard gets a rearranged desktop from a test run.
            // SAFETY: `self.swap_chain` is still live at this point.
            if let Err(error) = unsafe { self.swap_chain.SetFullscreenState(false, None) } {
                eprintln!("[warn] could not leave exclusive fullscreen: {error}");
            }
        }
    }
}

/// The window's client size in physical pixels.
fn client_size(window: HWND) -> Result<(u32, u32), AppError> {
    let mut client = RECT::default();
    // SAFETY: `window` is live and `client` is a live local the call writes
    // into.
    unsafe { GetClientRect(window, &mut client) }
        .map_err(|source| AppError::windows("GetClientRect", source))?;
    Ok((
        (client.right - client.left).unsigned_abs(),
        (client.bottom - client.top).unsigned_abs(),
    ))
}

/// A CPU-writable texture matching the back buffer, which the pattern is drawn
/// into and copied from.
fn create_staging(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, AppError> {
    let description = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        // A bit pattern, so the sign bit is a flag rather than a sign; see the
        // swap chain description above.
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
    };

    let mut texture = None;
    // SAFETY: `description` is a live local read during the call, no initial
    // data is supplied, and `texture` is a live local the call writes an owned
    // interface into.
    unsafe { device.CreateTexture2D(&description, None, Some(&mut texture)) }
        .map_err(|source| AppError::windows("ID3D11Device::CreateTexture2D", source))?;

    texture.ok_or_else(|| {
        AppError::windows(
            "ID3D11Device::CreateTexture2D",
            windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "reported success without returning a texture",
            ),
        )
    })
}

/// The window procedure: everything is default except closing.
extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE || message == WM_DESTROY {
        // SAFETY: posting a quit message takes no pointers. The run loop sees
        // it through `pump_messages` and stops, which is how closing the window
        // by hand ends the application cleanly rather than leaving it
        // presenting into nothing.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // SAFETY: forwarding the message Windows just delivered, unchanged, which
    // is what `DefWindowProcW` exists for.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// The raw handle value, for printing and for handing to a capture backend.
pub(crate) fn handle_value(window: HWND) -> usize {
    window.0 as usize
}
