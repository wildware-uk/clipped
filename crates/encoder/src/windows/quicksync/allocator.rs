//! The frame allocator oneVPL is given, and the Direct3D 11 textures it makes.
//!
//! # Why an encoder that copies nothing still has to allocate
//!
//! Two different pools of surfaces exist in an encoding session, and only one
//! of them is the caller's.
//!
//! The *input* surfaces are the captured textures. They belong to the capture
//! backend, and this backend never allocates one: a submitted
//! [`mfxFrameSurface1`](sys::mfxFrameSurface1) carries an opaque `MemId` that
//! **is** the address of an [`mfxHDLPair`](sys::mfxHDLPair) holding the
//! caller's texture, and [`get_hdl`] hands it straight back. That is the whole
//! of the zero-copy path.
//!
//! The *reconstructed* surfaces are the encoder's own: every inter-coded
//! picture is predicted from a decoded copy of an earlier one, and those copies
//! have to live somewhere. `mfxvideo.h` documents whose job that is, in the
//! comment on `mfxFrameAllocator::Alloc`:
//!
//! > For encoders, MFXVideoENCODE_Init calls Alloc twice: once for the input
//! > surfaces and again for the internal reconstructed surfaces.
//!
//! So once an external allocator is registered with
//! `MFXVideoCORE_SetFrameAllocator` — which the video-memory input path
//! requires, because nothing else can interpret the application's `MemId` — the
//! runtime allocates *through it*, and an allocator that refuses would fail
//! `MFXVideoENCODE_Init` on every Intel GPU rather than on none. This module
//! therefore allocates what it is asked for, from the caller's Direct3D 11
//! device, which is the same device the session is pinned to with
//! `MFXVideoCORE_SetHandle` and so the only device whose textures the encoder
//! can read.
//!
//! # What is measured and what is read from a document
//!
//! The paragraph above is read from Intel's header, not measured: there is no
//! Intel GPU on the machine this was written on (issue #17), so no oneVPL
//! runtime has ever called any of these five callbacks. What *is* measured, by
//! the tests at the bottom of this file, is the part that is ordinary Direct3D:
//! a request for *n* NV12 surfaces produces *n* textures, every surface's
//! `MemId` resolves back to a handle pair holding one of them, the response is
//! recognised again at `Free`, and nothing is leaked. That runs on any GPU that
//! can make an NV12 decoder target — which is any GPU with a hardware video
//! encoder, and so any machine this backend could run on — and says on standard
//! error when the machine cannot. Which of these callbacks a real oneVPL
//! runtime calls, in what order and with which memory types, is on
//! [#160](https://github.com/wildware-uk/clipped/issues/160).
//!
//! # Ownership and threading
//!
//! [`FrameAllocator`] owns a reference to the caller's device, every texture it
//! has allocated and the pool bookkeeping, and releases all of it in `Drop` —
//! which runs after `MFXClose`, so a runtime that never called `Free` still
//! leaks nothing (AGENTS.md section 58).
//!
//! oneVPL may call an allocator from its own threads, so the pools are behind a
//! mutex. A Direct3D 11 device is free-threaded for resource creation, which is
//! all that happens through it here.

use core::ffi::c_void;
use core::mem;
use core::ptr;
use std::sync::Mutex;

use windows::core::Interface as _;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_BIND_DECODER, D3D11_BIND_RENDER_TARGET,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC,
};

use super::sys;

/// The allocator oneVPL is given, and everything it has allocated.
pub(super) struct FrameAllocator {
    /// Boxed because `callbacks.pthis` points at `state` beside it: the address
    /// has to be stable for as long as the session can call back into it.
    inner: Box<Inner>,
}

/// The callbacks and the state they reach through `pthis`.
struct Inner {
    callbacks: sys::mfxFrameAllocator,
    state: State,
}

/// What the callbacks work on.
struct State {
    /// The caller's device, held as a counted reference so that a texture
    /// cannot outlive the device it was created on even if the caller drops
    /// theirs early.
    device: ID3D11Device,
    /// One entry per outstanding [`alloc`] response, keyed by the address of
    /// its `mids` array.
    pools: Mutex<Vec<Pool>>,
}

/// One answered allocation request.
///
/// The three vectors are parallel, and the two of them the runtime sees are
/// self-referential: `mids[i]` is the address of `handles[i]`, and the response
/// points at `mids`. All three are built before any pointer into them is taken
/// and are never resized afterwards, so moving a `Pool` — which moves only the
/// vector headers — leaves every pointer valid.
struct Pool {
    /// What the response handed over: one memory identifier per surface.
    mids: Vec<sys::mfxMemId>,
    /// The handle pairs those identifiers address.
    handles: Vec<sys::mfxHDLPair>,
    /// The textures the handle pairs hold, which own the Direct3D resources.
    textures: Vec<ID3D11Texture2D>,
}

impl Pool {
    /// How many surfaces are in it, which is what the response told the
    /// runtime.
    fn frames(&self) -> usize {
        debug_assert_eq!(
            self.mids.len(),
            self.handles.len(),
            "one identifier per handle pair"
        );
        debug_assert_eq!(
            self.handles.len(),
            self.textures.len(),
            "one handle pair per texture"
        );
        self.mids.len()
    }
}

impl FrameAllocator {
    /// Builds an allocator that allocates from `device`.
    ///
    /// # Errors
    ///
    /// A sentence naming what is wrong if the handle is not an `ID3D11Device`.
    ///
    /// # Safety
    ///
    /// `device` must be a live `ID3D11Device` the caller owns.
    pub(super) unsafe fn new(device: *mut c_void) -> Result<Self, String> {
        if device.is_null() {
            return Err("the graphics device is null".to_owned());
        }

        // SAFETY: the caller guarantees a live COM object it owns.
        // `from_raw_borrowed` takes no reference of its own, and `cast` takes
        // one this allocator then owns and releases when it drops.
        let device: ID3D11Device = unsafe { windows::core::IUnknown::from_raw_borrowed(&device) }
            .ok_or_else(|| "the graphics device is null".to_owned())?
            .cast()
            .map_err(|error| {
                format!(
                    "the graphics device does not answer as an ID3D11Device ({})",
                    error.message()
                )
            })?;

        // SAFETY: plain data — reserved integers, a context pointer and five
        // function pointers, which bindgen represents as `Option` and for which
        // all-zeroes is `None`.
        let callbacks: sys::mfxFrameAllocator = unsafe { mem::zeroed() };

        let mut inner = Box::new(Inner {
            callbacks,
            state: State {
                device,
                pools: Mutex::new(Vec::new()),
            },
        });

        // Taken after the box exists, so it addresses the state at its final
        // location.
        inner.callbacks.pthis = (&raw mut inner.state).cast::<c_void>();
        inner.callbacks.Alloc = Some(alloc);
        inner.callbacks.Lock = Some(lock);
        inner.callbacks.Unlock = Some(unlock);
        inner.callbacks.GetHDL = Some(get_hdl);
        inner.callbacks.Free = Some(free);

        Ok(Self { inner })
    }

    /// The structure `MFXVideoCORE_SetFrameAllocator` is given.
    pub(super) fn interface(&mut self) -> *mut sys::mfxFrameAllocator {
        &raw mut self.inner.callbacks
    }

    /// How many allocation responses are outstanding, for diagnostics and for
    /// the tests.
    pub(super) fn pool_count(&self) -> usize {
        self.inner
            .state
            .pools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl Drop for FrameAllocator {
    fn drop(&mut self) {
        let outstanding = self.pool_count();
        if outstanding != 0 {
            // Released anyway by dropping the pools below — but a runtime that
            // closed without freeing what it allocated is worth a line, because
            // it means the release order this backend assumes is not the one
            // that happened (AGENTS.md section 15).
            tracing::debug!(
                pools = outstanding,
                "the Intel runtime closed without freeing every surface pool it allocated"
            );
        }
    }
}

/// The Direct3D format a oneVPL FourCC names, or [`None`] if this allocator
/// cannot make one.
fn dxgi_format(fourcc: sys::mfxU32) -> Option<DXGI_FORMAT> {
    #[allow(clippy::cast_sign_loss)]
    const NV12: sys::mfxU32 = sys::MFX_FOURCC_NV12 as sys::mfxU32;
    #[allow(clippy::cast_sign_loss)]
    const RGB4: sys::mfxU32 = sys::MFX_FOURCC_RGB4 as sys::mfxU32;
    #[allow(clippy::cast_sign_loss)]
    const P010: sys::mfxU32 = sys::MFX_FOURCC_P010 as sys::mfxU32;

    match fourcc {
        // What an encoder's reconstructed pictures are: 8-bit 4:2:0.
        NV12 => Some(DXGI_FORMAT_NV12),
        // What a captured frame is, in case a runtime asks to allocate input
        // surfaces of its own rather than take the caller's.
        RGB4 => Some(DXGI_FORMAT_B8G8R8A8_UNORM),
        // 10-bit 4:2:0. Nothing configures it today — HDR is
        // https://github.com/wildware-uk/clipped/issues/99 — but a runtime that
        // asks for it is asking for something Direct3D can make.
        P010 => Some(DXGI_FORMAT_P010),
        _ => None,
    }
}

/// The Direct3D binding a surface of this format needs.
///
/// Transcribed from Intel's own Direct3D 11 sample allocator
/// (`sample_common/src/d3d11_allocator.cpp`): the video formats are bound as
/// decoder targets, which is the binding the media engine reads and writes, and
/// a packed RGB surface is bound as a render target because no decoder target
/// can be that format.
const fn bind_flags(format: DXGI_FORMAT) -> u32 {
    if format.0 == DXGI_FORMAT_B8G8R8A8_UNORM.0 {
        D3D11_BIND_RENDER_TARGET.0 as u32
    } else {
        D3D11_BIND_DECODER.0 as u32
    }
}

/// Allocates one pool of surfaces.
///
/// # Safety
///
/// Called by oneVPL with the pointers its interface documents: `pthis` is the
/// context this allocator registered, and `request` and `response` are live.
unsafe extern "C" fn alloc(
    pthis: sys::mfxHDL,
    request: *mut sys::mfxFrameAllocRequest,
    response: *mut sys::mfxFrameAllocResponse,
) -> sys::mfxStatus {
    if pthis.is_null() || request.is_null() || response.is_null() {
        return sys::MFX_ERR_NULL_PTR;
    }

    // SAFETY: `pthis` is the pointer `FrameAllocator::new` put in the
    // callbacks, which addresses a `State` inside a box that outlives the
    // session; the request is live for the length of the call, per the
    // interface.
    let (state, request) = unsafe { (&*pthis.cast::<State>(), &*request) };

    let frames = request.NumFrameSuggested.max(request.NumFrameMin);
    if frames == 0 {
        tracing::error!("the Intel runtime asked for a pool of no surfaces");
        return sys::MFX_ERR_MEMORY_ALLOC;
    }

    #[allow(clippy::cast_sign_loss)]
    if u32::from(request.Type) & (sys::MFX_MEMTYPE_SYSTEM_MEMORY as u32) != 0 {
        // The pipeline is configured for video memory throughout
        // (`MFX_IOPATTERN_IN_VIDEO_MEMORY`), and a system-memory pool would be
        // a copy per picture. Refusing names the request rather than allocating
        // something the caller did not ask to pay for (AGENTS.md section 18).
        tracing::error!(
            request_type = format!("{:#06x}", request.Type),
            frames,
            "the Intel runtime asked for encoder surfaces in system memory, which this pipeline \
             does not use (https://github.com/wildware-uk/clipped/issues/160)"
        );
        return sys::MFX_ERR_UNSUPPORTED;
    }

    let fourcc = request.Info.FourCC;
    let Some(format) = dxgi_format(fourcc) else {
        tracing::error!(
            fourcc = format!("{fourcc:#010x}"),
            "the Intel runtime asked for encoder surfaces in a layout this backend cannot \
             allocate (https://github.com/wildware-uk/clipped/issues/160)"
        );
        return sys::MFX_ERR_UNSUPPORTED;
    };

    // SAFETY: the size member is the one oneVPL fills in for an allocation
    // request, and the other member of the union is the coordinate pair used
    // only for cropping.
    let size = unsafe { request.Info.__bindgen_anon_1.__bindgen_anon_1 };

    let description = D3D11_TEXTURE2D_DESC {
        Width: u32::from(size.Width),
        Height: u32::from(size.Height),
        MipLevels: 1,
        // One texture per surface rather than one array with a subresource per
        // surface: `mfxHDLPair` can name either, and separate textures mean a
        // pool can be released one at a time and a handle needs no index.
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind_flags(format),
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let mut textures = Vec::with_capacity(usize::from(frames));
    for index in 0..frames {
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: the device is live for as long as this allocator is, the
        // description is a live local, and `texture` is a live out-parameter
        // that receives a reference this function then owns.
        let created = unsafe {
            state
                .device
                .CreateTexture2D(&description, None, Some(&mut texture))
        };

        match created.map(|()| texture.take()) {
            Ok(Some(texture)) => textures.push(texture),
            outcome => {
                let detail = outcome.err().map_or_else(
                    || "the device reported success and produced no texture".to_owned(),
                    |error| error.message(),
                );
                tracing::error!(
                    width = description.Width,
                    height = description.Height,
                    format = format.0,
                    allocated = index,
                    of = frames,
                    detail,
                    "a Quick Sync encoder surface could not be allocated"
                );
                return sys::MFX_ERR_MEMORY_ALLOC;
            }
        }
    }

    let mut handles: Vec<sys::mfxHDLPair> = textures
        .iter()
        .map(|texture| sys::mfxHDLPair {
            first: texture.as_raw(),
            // The subresource index, which is zero because every texture above
            // is allocated with an array size of one.
            second: ptr::null_mut(),
        })
        .collect();
    let mids: Vec<sys::mfxMemId> = handles
        .iter_mut()
        .map(|handle| (&raw mut *handle).cast::<c_void>())
        .collect();

    let mut pool = Pool {
        mids,
        handles,
        textures,
    };

    // SAFETY: `response` is live for the length of the call, per the interface,
    // and the pointer written into it addresses a heap buffer the pool owns and
    // never resizes, which lives until `free` or until the allocator drops.
    unsafe {
        (*response).mids = pool.mids.as_mut_ptr();
        (*response).NumFrameActual = frames;
        (*response).AllocId = request.__bindgen_anon_1.AllocId;
    }

    tracing::debug!(
        frames,
        width = description.Width,
        height = description.Height,
        fourcc = format!("{fourcc:#010x}"),
        request_type = format!("{:#06x}", request.Type),
        "allocated a pool of Quick Sync encoder surfaces"
    );

    state
        .pools
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(pool);
    sys::MFX_ERR_NONE
}

/// Releases one pool.
///
/// # Safety
///
/// Called by oneVPL with a response [`alloc`] filled in.
unsafe extern "C" fn free(
    pthis: sys::mfxHDL,
    response: *mut sys::mfxFrameAllocResponse,
) -> sys::mfxStatus {
    if pthis.is_null() || response.is_null() {
        return sys::MFX_ERR_NULL_PTR;
    }

    // SAFETY: as in `alloc` — the context is this allocator's state and the
    // response is live for the length of the call.
    let (state, mids) = unsafe { (&*pthis.cast::<State>(), (*response).mids) };

    let mut pools = state
        .pools
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(index) = pools
        .iter()
        .position(|pool| ptr::eq(pool.mids.as_ptr(), mids))
    else {
        // Not a leak — every pool is released when the allocator drops — but a
        // response this allocator did not produce means the runtime is freeing
        // something twice or something else's.
        tracing::error!("the Intel runtime freed a surface pool this backend did not allocate");
        return sys::MFX_ERR_INVALID_HANDLE;
    };

    let pool = pools.remove(index);
    drop(pools);
    tracing::debug!(
        frames = pool.frames(),
        "released a pool of Quick Sync encoder surfaces"
    );
    drop(pool);

    // SAFETY: the response is live and its `mids` array has just been released,
    // so leaving the pointer in place would leave a dangling one behind.
    unsafe {
        (*response).mids = ptr::null_mut();
        (*response).NumFrameActual = 0;
    }
    sys::MFX_ERR_NONE
}

/// Refuses to map a surface into system memory.
///
/// # Safety
///
/// Called by oneVPL. Nothing is dereferenced.
unsafe extern "C" fn lock(
    _pthis: sys::mfxHDL,
    _mid: sys::mfxMemId,
    _ptr: *mut sys::mfxFrameData,
) -> sys::mfxStatus {
    // Every surface this allocator hands over lives in video memory and none is
    // read by the processor: the caller's textures are captured on the GPU and
    // the reconstructed pictures never leave it. A runtime that wants to read
    // one is asking for a copy this pipeline exists to avoid, and it would need
    // a staging texture and a device context to get it.
    sys::MFX_ERR_UNSUPPORTED
}

/// The counterpart of [`lock`], which never succeeded.
///
/// # Safety
///
/// Called by oneVPL. Nothing is dereferenced.
unsafe extern "C" fn unlock(
    _pthis: sys::mfxHDL,
    _mid: sys::mfxMemId,
    _ptr: *mut sys::mfxFrameData,
) -> sys::mfxStatus {
    sys::MFX_ERR_UNSUPPORTED
}

/// Turns a surface's memory identifier back into the graphics handle it is.
///
/// # Safety
///
/// `mid` must be an identifier this backend put on a surface — the address of
/// an [`mfxHDLPair`](sys::mfxHDLPair) that is still alive, whether one of
/// [`alloc`]'s or the one `Session::submit` builds for the caller's texture —
/// and `handle` must be a live out-parameter.
unsafe extern "C" fn get_hdl(
    _pthis: sys::mfxHDL,
    mid: sys::mfxMemId,
    handle: *mut sys::mfxHDL,
) -> sys::mfxStatus {
    if mid.is_null() || handle.is_null() {
        return sys::MFX_ERR_INVALID_HANDLE;
    }

    // SAFETY: `handle` is a live out-parameter per this function's contract.
    // The identifier is handed back unchanged: oneVPL's Direct3D 11 convention
    // is that the handle for a video-memory surface is a pointer to an
    // `mfxHDLPair` of texture and subresource index, and that is exactly what
    // both `alloc` and `Session::submit` put in the `MemId`.
    unsafe {
        *handle = mid;
    }
    sys::MFX_ERR_NONE
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use windows::Win32::Graphics::Direct3D11::{
        D3D11_FORMAT_SUPPORT_DECODER_OUTPUT, D3D11_FORMAT_SUPPORT_TEXTURE2D,
    };

    use super::*;

    /// A request for `frames` surfaces of `fourcc`, as a runtime would make it.
    fn request(fourcc: sys::mfxU32, frames: sys::mfxU16) -> sys::mfxFrameAllocRequest {
        // SAFETY: plain data, and oneVPL requires everything a caller is not
        // told about to be zero.
        let mut request: sys::mfxFrameAllocRequest = unsafe { core::mem::zeroed() };
        request.Info.FourCC = fourcc;
        request.Info.__bindgen_anon_1.__bindgen_anon_1.Width = 1920;
        request.Info.__bindgen_anon_1.__bindgen_anon_1.Height = 1088;
        request.NumFrameMin = frames;
        request.NumFrameSuggested = frames;
        #[allow(clippy::cast_sign_loss)]
        {
            request.Type = (sys::MFX_MEMTYPE_FROM_ENCODE
                | sys::MFX_MEMTYPE_DXVA2_DECODER_TARGET
                | sys::MFX_MEMTYPE_INTERNAL_FRAME) as sys::mfxU16;
        }
        request
    }

    /// An allocator on this machine's first hardware Direct3D 11 device, or
    /// [`None`] on a machine that has none — with the reason on standard error,
    /// because a skipped test that says nothing is a test that passed for
    /// reasons nobody knows.
    ///
    /// The device is the one the backend's own tests use, which excludes the
    /// Basic Render Driver: it is a software rasteriser, no capture backend
    /// hands the encoder a texture from one, and it cannot make the surfaces an
    /// encoder needs.
    fn allocator_on_this_machine() -> Option<(FrameAllocator, ID3D11Device)> {
        let Some(device) = super::super::tests::any_hardware_device() else {
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED (encoder, hardware): this machine has no Direct3D 11 hardware adapter"
            );
            return None;
        };

        // SAFETY: the device is live and this call only asks it a question.
        let support = unsafe { device.CheckFormatSupport(DXGI_FORMAT_NV12) }.unwrap_or(0);
        let wanted =
            D3D11_FORMAT_SUPPORT_TEXTURE2D.0 as u32 | D3D11_FORMAT_SUPPORT_DECODER_OUTPUT.0 as u32;
        if support & wanted != wanted {
            // A GPU that cannot make an NV12 decoder target cannot hold an
            // encoder's reconstructed pictures either, so there is nothing here
            // for this test to check. Every GPU with a hardware video encoder
            // can — which is the only kind of machine this backend runs on.
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED (encoder, hardware): this machine's Direct3D 11 device does not support \
                 NV12 decoder targets (format support {support:#010x})"
            );
            return None;
        }

        // SAFETY: the device is alive for as long as the returned pair is, and
        // the allocator takes a counted reference of its own.
        let allocator = unsafe { FrameAllocator::new(device.as_raw()) }.ok()?;
        Some((allocator, device))
    }

    #[test]
    fn a_pool_of_surfaces_is_allocated_and_released() {
        // The half of the allocator that is ordinary Direct3D rather than
        // Intel's: whatever GPU this machine has, a request for four NV12
        // surfaces has to produce four textures whose identifiers resolve back
        // to them, and freeing the response has to release exactly that pool.
        // What a *real* oneVPL runtime asks for is issue #160; that it is
        // answered with real textures is checked here.
        let Some((mut allocator, _device)) = allocator_on_this_machine() else {
            return;
        };

        #[allow(clippy::cast_sign_loss)]
        let mut asked = request(sys::MFX_FOURCC_NV12 as sys::mfxU32, 4);
        // SAFETY: plain data; the allocator fills it in.
        let mut answer: sys::mfxFrameAllocResponse = unsafe { core::mem::zeroed() };

        let callbacks = allocator.interface();
        // SAFETY: the callbacks were just built by this test's allocator, and
        // both structures are live locals.
        let status = unsafe {
            let alloc = (*callbacks).Alloc.expect("the allocator registers Alloc");
            alloc((*callbacks).pthis, &raw mut asked, &raw mut answer)
        };

        assert_eq!(
            status,
            sys::MFX_ERR_NONE,
            "four NV12 surfaces could not be allocated on this machine's device"
        );
        assert_eq!(answer.NumFrameActual, 4);
        assert!(!answer.mids.is_null());
        assert_eq!(allocator.pool_count(), 1);

        for index in 0..4 {
            // SAFETY: the response's array has four entries, as just asserted.
            let mid = unsafe { *answer.mids.add(index) };
            assert!(!mid.is_null(), "surface {index} has no identifier");

            let mut handle: sys::mfxHDL = ptr::null_mut();
            // SAFETY: `mid` is an identifier this allocator produced and
            // `handle` is a live local out-parameter.
            let status = unsafe {
                let get_hdl = (*callbacks).GetHDL.expect("the allocator registers GetHDL");
                get_hdl((*callbacks).pthis, mid, &raw mut handle)
            };
            assert_eq!(status, sys::MFX_ERR_NONE);
            assert_eq!(handle, mid, "the handle is the identifier, unchanged");

            // SAFETY: the identifier is the address of a handle pair the pool
            // owns and which is alive until the response is freed.
            let pair = unsafe { *mid.cast::<sys::mfxHDLPair>() };
            assert!(
                !pair.first.is_null(),
                "surface {index} has no texture behind it"
            );
            assert!(pair.second.is_null(), "the subresource index is zero");
        }

        // SAFETY: the response is the one just filled in.
        let status = unsafe {
            let free = (*callbacks).Free.expect("the allocator registers Free");
            free((*callbacks).pthis, &raw mut answer)
        };
        assert_eq!(status, sys::MFX_ERR_NONE);
        assert_eq!(
            allocator.pool_count(),
            0,
            "freeing the response did not release the pool"
        );
        assert!(
            answer.mids.is_null(),
            "a freed response keeps a dangling array"
        );
    }

    #[test]
    fn a_response_that_was_never_allocated_is_refused() {
        // A double free, or a response from somewhere else, must not release a
        // pool that is still in use — the textures behind it are what the
        // encoder is predicting from.
        let Some((mut allocator, _device)) = allocator_on_this_machine() else {
            return;
        };

        let mut mid: sys::mfxMemId = ptr::null_mut();
        let mut answer = sys::mfxFrameAllocResponse {
            AllocId: 0,
            reserved: [0; 3],
            mids: &raw mut mid,
            NumFrameActual: 1,
            reserved2: 0,
        };

        let callbacks = allocator.interface();
        // SAFETY: the callbacks are this allocator's and the response is a live
        // local.
        let status = unsafe {
            let free = (*callbacks).Free.expect("the allocator registers Free");
            free((*callbacks).pthis, &raw mut answer)
        };
        assert_eq!(status, sys::MFX_ERR_INVALID_HANDLE);
    }

    #[test]
    fn a_layout_this_backend_cannot_allocate_is_refused_rather_than_guessed_at() {
        let Some((mut allocator, _device)) = allocator_on_this_machine() else {
            return;
        };

        // `MFX_FOURCC_YV12`, which nothing in this pipeline produces or
        // consumes.
        let mut asked = request(u32::from_le_bytes(*b"YV12"), 2);
        // SAFETY: plain data.
        let mut answer: sys::mfxFrameAllocResponse = unsafe { core::mem::zeroed() };

        let callbacks = allocator.interface();
        // SAFETY: as above.
        let status = unsafe {
            let alloc = (*callbacks).Alloc.expect("the allocator registers Alloc");
            alloc((*callbacks).pthis, &raw mut asked, &raw mut answer)
        };
        assert_eq!(status, sys::MFX_ERR_UNSUPPORTED);
        assert_eq!(allocator.pool_count(), 0);
    }

    #[test]
    fn a_surface_is_never_mapped_into_system_memory() {
        // The refusal is deliberate rather than unimplemented: a runtime that
        // reads a surface with the processor is a copy per picture, which is
        // what this whole path exists to avoid.
        // SAFETY: both callbacks ignore every argument, as their contracts say.
        unsafe {
            assert_eq!(
                lock(ptr::null_mut(), ptr::null_mut(), ptr::null_mut()),
                sys::MFX_ERR_UNSUPPORTED
            );
            assert_eq!(
                unlock(ptr::null_mut(), ptr::null_mut(), ptr::null_mut()),
                sys::MFX_ERR_UNSUPPORTED
            );
        }
    }

    #[test]
    fn a_null_identifier_has_no_handle() {
        let mut handle: sys::mfxHDL = ptr::null_mut();
        // SAFETY: null is one of the values the contract permits, and `handle`
        // is a live local.
        let status = unsafe { get_hdl(ptr::null_mut(), ptr::null_mut(), &raw mut handle) };
        assert_eq!(status, sys::MFX_ERR_INVALID_HANDLE);
    }
}
