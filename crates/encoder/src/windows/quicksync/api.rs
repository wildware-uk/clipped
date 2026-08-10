//! Reaching Quick Sync: finding the Intel media runtime, loading it, and
//! turning status codes back into sentences.
//!
//! # Which library, and why there is a list of them
//!
//! oneVPL is normally reached through a *dispatcher* — `libvpl.dll` — which
//! enumerates the implementations on the machine and creates a session on the
//! one the application asked for. Clipped does not ship a dispatcher: it is a
//! separate binary that would have to be built, signed and redistributed, and
//! the only implementation this recorder can use is the one the Intel graphics
//! driver already installed. So this module does the small part of a
//! dispatcher's job that is actually needed — find the runtime, load it, and
//! call its entry points — which is exactly what the NVENC backend does with
//! `nvEncodeAPI64.dll` (`api.rs` there), and for the same reason: no SDK to
//! install, nothing to link, and a machine without the hardware simply fails to
//! load a library.
//!
//! [`LIBRARIES`] is a list rather than one name because Intel's media stack has
//! been renamed twice and the driver's file names have followed. Each candidate
//! is loaded in turn and the first one that exports [`ENTRY_INITIALIZE`] wins.
//!
//! **None of this has run against an Intel GPU.** The machine this was written
//! on has no Intel adapter (issue #17), so every path below has only ever been
//! executed in its failure direction: the libraries are not present, the loader
//! says so, and `QuickSyncEncoder::open` reports it. Which of the candidate
//! names a given driver installs, and which of them exports `MFXInitialize`,
//! is the single biggest thing about this backend that wants checking on real
//! hardware ([#160](https://github.com/wildware-uk/clipped/issues/160)).
//!
//! # Ownership
//!
//! [`QuickSyncRuntime`] owns the loaded module and releases it in `Drop`. A
//! session holds one for its whole life: the function pointers belong to the
//! module, and freeing it while an encoder is open would leave the session
//! calling into unmapped memory.

use core::ffi::c_void;
use core::fmt;
use core::mem;

use windows::core::{Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, DXGI_ADAPTER_DESC};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use crate::codec::Vendor;

use super::sys;

/// The libraries the Intel media runtime is looked for in, in the order they
/// are tried.
///
/// All three are installed into System32 by an Intel graphics driver rather
/// than by this application, and each is a *runtime* — the thing that actually
/// encodes — except the first, which is the oneVPL dispatcher and is tried
/// first because a machine that has one has it because something installed a
/// full oneVPL stack.
///
/// Two of these are the ones `crate::windows::runtime` probes when it reports
/// whether Quick Sync exists at all. `libmfx64.dll` is the third and is not in
/// that list, so a machine that has only that one would be reported as having
/// no Quick Sync runtime and would still encode. Making the two lists agree is
/// on [#160](https://github.com/wildware-uk/clipped/issues/160), together with
/// finding out which of the names a current driver actually installs — which is
/// what the answer should be based on, rather than on adding names to both
/// lists blind.
pub(super) const LIBRARIES: &[&str] = &["libvpl.dll", "libmfx64.dll", "libmfxhw64.dll"];

/// [`LIBRARIES`] as one sentence, for the error that names what was looked for.
///
/// [`EncodeErrorKind::RuntimeUnavailable`](crate::EncodeErrorKind::RuntimeUnavailable)
/// carries one library name because the other backends have one; Quick Sync has
/// three, so it carries all three. A test keeps the two in step.
pub(super) const LIBRARY_LIST: &str = "libvpl.dll, libmfx64.dll or libmfxhw64.dll";

/// The entry point every candidate library is required to export.
///
/// `MFXInitialize` is the oneVPL 2.x session entry point. A library that loads
/// and does not export it is a Media SDK 1.x runtime, which this backend does
/// not drive — see [`LoadFailure::NoOneVplRuntime`].
pub(super) const ENTRY_INITIALIZE: &str = "MFXInitialize";

/// The oneVPL API version these bindings were generated from.
///
/// The 2.x API is additive: structures are fixed-size with reserved fields and
/// never change layout, so a newer header against an older runtime is safe as
/// long as no field the older runtime does not know about is written. Nothing
/// here writes one — see `settings.rs`, which fills in fields present since
/// 2.0.
pub(super) const API_MAJOR: u16 = 2;

/// The minor version of the headers, for diagnostics.
pub(super) const API_MINOR: u16 = 15;

/// The oldest runtime version this backend will drive.
///
/// 2.0 is the version that introduced [`ENTRY_INITIALIZE`], so anything that
/// exports it is at least this. The constant exists so the message a user gets
/// names a version rather than only a symbol.
pub(super) const MINIMUM_API: (u16, u16) = (2, 0);

/// Why the runtime could not be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LoadFailure {
    /// None of [`LIBRARIES`] is installed, or the loader refused all of them.
    NotFound {
        /// What the loader said about the last candidate tried.
        detail: String,
    },
    /// A library loaded but exports no oneVPL 2.x entry point, which is what a
    /// Media SDK 1.x runtime looks like.
    NoOneVplRuntime {
        /// The library that loaded.
        library: &'static str,
    },
    /// The library exports the session entry point and not one of the encoding
    /// entry points, which means it is not the library it claims to be.
    MissingEntryPoint {
        /// The export that was looked for.
        name: &'static str,
    },
}

impl fmt::Display for LoadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { detail } => write!(
                formatter,
                "none of {} could be loaded ({detail})",
                LIBRARIES.join(", ")
            ),
            Self::NoOneVplRuntime { library } => write!(
                formatter,
                "{library} loaded but does not export {ENTRY_INITIALIZE}, so the installed Intel \
                 media runtime is older than oneVPL {}.{}; update the Intel graphics driver",
                MINIMUM_API.0, MINIMUM_API.1
            ),
            Self::MissingEntryPoint { name } => {
                write!(formatter, "the runtime does not export {name}")
            }
        }
    }
}

// The signatures of the entry points this backend calls, transcribed from
// `mfxsession.h` and `mfxvideo.h`. They are the one part of this binding a
// compiler cannot check: bindgen was asked not to emit the function
// declarations, because there is no import library to link them against (see
// `sys.rs`). Each alias carries the C declaration it came from, so a reviewer
// can compare them line by line.
//
// `MFX_CDECL` is `__cdecl`, and on x86-64 Windows there is only one calling
// convention, so `extern "C"` is it.

/// `mfxStatus MFXInitialize(mfxInitializationParam par, mfxSession *session)`
type Initialize =
    unsafe extern "C" fn(sys::mfxInitializationParam, *mut sys::mfxSession) -> sys::mfxStatus;
/// `mfxStatus MFXClose(mfxSession session)`
type Close = unsafe extern "C" fn(sys::mfxSession) -> sys::mfxStatus;
/// `mfxStatus MFXQueryVersion(mfxSession session, mfxVersion *version)`
type QueryVersion = unsafe extern "C" fn(sys::mfxSession, *mut sys::mfxVersion) -> sys::mfxStatus;
/// `mfxStatus MFXVideoCORE_SetHandle(mfxSession session, mfxHandleType type, mfxHDL hdl)`
type SetHandle =
    unsafe extern "C" fn(sys::mfxSession, sys::mfxHandleType, sys::mfxHDL) -> sys::mfxStatus;
/// `mfxStatus MFXVideoCORE_SetFrameAllocator(mfxSession session, mfxFrameAllocator *allocator)`
type SetFrameAllocator =
    unsafe extern "C" fn(sys::mfxSession, *mut sys::mfxFrameAllocator) -> sys::mfxStatus;
/// `mfxStatus MFXVideoCORE_SyncOperation(mfxSession session, mfxSyncPoint syncp, mfxU32 wait)`
type SyncOperation =
    unsafe extern "C" fn(sys::mfxSession, sys::mfxSyncPoint, sys::mfxU32) -> sys::mfxStatus;
/// `mfxStatus MFXVideoENCODE_Query(mfxSession session, mfxVideoParam *in, mfxVideoParam *out)`
type EncodeQuery = unsafe extern "C" fn(
    sys::mfxSession,
    *mut sys::mfxVideoParam,
    *mut sys::mfxVideoParam,
) -> sys::mfxStatus;
/// `mfxStatus MFXVideoENCODE_Init(mfxSession session, mfxVideoParam *par)`, and
/// `MFXVideoENCODE_GetVideoParam`, which takes the same arguments.
type EncodeParams =
    unsafe extern "C" fn(sys::mfxSession, *mut sys::mfxVideoParam) -> sys::mfxStatus;
/// `mfxStatus MFXVideoENCODE_Close(mfxSession session)`
type EncodeClose = unsafe extern "C" fn(sys::mfxSession) -> sys::mfxStatus;
/// `mfxStatus MFXVideoENCODE_EncodeFrameAsync(mfxSession session, mfxEncodeCtrl *ctrl,
/// mfxFrameSurface1 *surface, mfxBitstream *bs, mfxSyncPoint *syncp)`
type EncodeFrameAsync = unsafe extern "C" fn(
    sys::mfxSession,
    *mut sys::mfxEncodeCtrl,
    *mut sys::mfxFrameSurface1,
    *mut sys::mfxBitstream,
    *mut sys::mfxSyncPoint,
) -> sys::mfxStatus;

/// The entry points this backend calls, resolved once when the runtime loads.
///
/// Every one is required: a library that has `MFXInitialize` and not
/// `MFXVideoENCODE_Init` is not a oneVPL runtime, and finding that out at load
/// time is better than finding it out three calls into a recording.
#[allow(non_snake_case)]
pub(super) struct Functions {
    /// Creates the session.
    pub(super) MFXInitialize: Initialize,
    /// Destroys it.
    pub(super) MFXClose: Close,
    /// Which API version the runtime implements.
    pub(super) MFXQueryVersion: QueryVersion,
    /// Pins the session to a graphics device.
    pub(super) MFXVideoCORE_SetHandle: SetHandle,
    /// Registers the allocator that turns a surface identifier into a texture.
    pub(super) MFXVideoCORE_SetFrameAllocator: SetFrameAllocator,
    /// Waits for one asynchronous operation.
    pub(super) MFXVideoCORE_SyncOperation: SyncOperation,
    /// Asks whether a configuration can be encoded, without opening an encoder.
    pub(super) MFXVideoENCODE_Query: EncodeQuery,
    /// Opens the encoder.
    pub(super) MFXVideoENCODE_Init: EncodeParams,
    /// Closes it.
    pub(super) MFXVideoENCODE_Close: EncodeClose,
    /// Reads back what the encoder was configured with, including the parameter
    /// sets.
    pub(super) MFXVideoENCODE_GetVideoParam: EncodeParams,
    /// Submits one frame.
    pub(super) MFXVideoENCODE_EncodeFrameAsync: EncodeFrameAsync,
}

/// The loaded Intel media runtime and its entry points.
///
/// # Ownership
///
/// Owns the module handle. `Drop` frees it, and nothing else in this crate
/// does, so the module outlives every function pointer taken from it.
pub(super) struct QuickSyncRuntime {
    module: HMODULE,
    library: &'static str,
    functions: Functions,
}

impl QuickSyncRuntime {
    /// Loads the first of [`LIBRARIES`] that answers, and resolves its entry
    /// points.
    ///
    /// Each library is loaded with `LOAD_LIBRARY_SEARCH_SYSTEM32` so that the
    /// search order cannot be diverted by a file of the same name somewhere on
    /// the path — the same rule `crate::windows::runtime` and the NVENC backend
    /// follow, for the same reason (AGENTS.md section 13).
    pub(super) fn load() -> Result<Self, LoadFailure> {
        let mut last_loader_error = String::from("no candidate library was tried");
        let mut loaded_without_onevpl = None;

        for library in LIBRARIES {
            let module = match load_library(library) {
                Ok(module) => module,
                Err(error) => {
                    last_loader_error = error;
                    continue;
                }
            };

            // From here the module is owned: every path out has to free it,
            // which is what the guard does without a `let _ =` at each one.
            let guard = ModuleGuard(module);

            // SAFETY: the module is the one just loaded and has not been freed.
            if unsafe { symbol(module, ENTRY_INITIALIZE) }.is_none() {
                // Keep looking: a machine can have a Media SDK 1.x runtime and
                // a oneVPL one side by side, and only the second is drivable
                // here. Remember the first so the message can say what was
                // found rather than only what was missing.
                loaded_without_onevpl.get_or_insert(*library);
                continue;
            }

            // SAFETY: as above — the module is live for the rest of this
            // iteration, and `resolve` only reads exports from it.
            let functions = unsafe { resolve(module) }?;

            return Ok(Self {
                module: guard.take(),
                library,
                functions,
            });
        }

        Err(loaded_without_onevpl.map_or(
            LoadFailure::NotFound {
                detail: last_loader_error,
            },
            |library| LoadFailure::NoOneVplRuntime { library },
        ))
    }

    /// The entry points. Every call into oneVPL goes through them.
    pub(super) const fn functions(&self) -> &Functions {
        &self.functions
    }

    /// Which of [`LIBRARIES`] answered, for logs and for errors.
    pub(super) const fn library(&self) -> &'static str {
        self.library
    }
}

impl Drop for QuickSyncRuntime {
    fn drop(&mut self) {
        // SAFETY: `self.module` came from the single successful
        // `LoadLibraryExW` in `load`, is not copied anywhere else, and this
        // type is not `Clone`, so this is the one and only release of that
        // reference. The session that held the function pointers is closed
        // before this runs — `QuickSyncEncoder` drops its session first, by
        // field order — so nothing calls into the module afterwards.
        if let Err(error) = unsafe { FreeLibrary(self.module) } {
            // Not fatal and not silent: the process keeps a library mapped,
            // which costs address space and nothing else (AGENTS.md section
            // 15).
            tracing::debug!(
                library = self.library,
                %error,
                "the encoder runtime could not be released"
            );
        }
    }
}

impl fmt::Debug for QuickSyncRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuickSyncRuntime")
            .field("library", &self.library)
            .field("headers", &format_args!("{API_MAJOR}.{API_MINOR}"))
            .finish()
    }
}

/// Frees a module unless it is taken, so that the failure paths in
/// [`QuickSyncRuntime::load`] cannot leak one.
struct ModuleGuard(HMODULE);

impl ModuleGuard {
    /// Hands ownership on; the guard no longer frees the module.
    fn take(self) -> HMODULE {
        let module = self.0;
        mem::forget(self);
        module
    }
}

impl Drop for ModuleGuard {
    fn drop(&mut self) {
        // SAFETY: the guard owns the reference `LoadLibraryExW` returned, and
        // `take` is the only way for that ownership to leave, at which point
        // the guard is forgotten rather than dropped.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

/// Loads one library from System32, or says what the loader said.
fn load_library(library: &str) -> Result<HMODULE, String> {
    let wide: Vec<u16> = library.encode_utf16().chain(core::iter::once(0)).collect();

    // SAFETY: `wide` is a nul-terminated UTF-16 string that outlives the call,
    // the reserved handle parameter is `None` as the documentation requires,
    // and the flag is a documented `LOAD_LIBRARY_SEARCH_*` value. The module
    // returned is owned by the caller, which frees it exactly once.
    unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map_err(|error| format!("{library}: {}", error.message()))
}

/// Looks one export up.
///
/// # Safety
///
/// `module` must be a live module.
unsafe fn symbol(module: HMODULE, name: &str) -> Option<unsafe extern "system" fn() -> isize> {
    let bytes: Vec<u8> = name.bytes().chain(core::iter::once(0)).collect();

    // SAFETY: the module is live, per this function's contract, and `bytes` is
    // a nul-terminated byte string that outlives the call.
    unsafe { GetProcAddress(module, PCSTR(bytes.as_ptr())) }
}

/// Resolves every entry point this backend calls.
///
/// # Safety
///
/// `module` must be a live oneVPL runtime, and the returned function pointers
/// are only valid while it stays loaded.
#[allow(non_snake_case)]
unsafe fn resolve(module: HMODULE) -> Result<Functions, LoadFailure> {
    /// Resolves one export and gives it the signature its header declares.
    ///
    /// The transmute is the unavoidable part of reaching a C API through the
    /// loader: `GetProcAddress` returns an untyped pointer. Both types are
    /// named at every use so that a reviewer sees what is being claimed.
    macro_rules! entry {
        ($name:literal, $signature:ty) => {{
            // SAFETY: the module is live, per this function's contract.
            let symbol = unsafe { symbol(module, $name) }
                .ok_or(LoadFailure::MissingEntryPoint { name: $name })?;
            // SAFETY: the signature is the one `mfxsession.h` or `mfxvideo.h`
            // declares for this export, transcribed onto the alias named here,
            // and a function pointer is the same size as the pointer
            // `GetProcAddress` returned.
            unsafe { mem::transmute::<unsafe extern "system" fn() -> isize, $signature>(symbol) }
        }};
    }

    Ok(Functions {
        MFXInitialize: entry!("MFXInitialize", Initialize),
        MFXClose: entry!("MFXClose", Close),
        MFXQueryVersion: entry!("MFXQueryVersion", QueryVersion),
        MFXVideoCORE_SetHandle: entry!("MFXVideoCORE_SetHandle", SetHandle),
        MFXVideoCORE_SetFrameAllocator: entry!("MFXVideoCORE_SetFrameAllocator", SetFrameAllocator),
        MFXVideoCORE_SyncOperation: entry!("MFXVideoCORE_SyncOperation", SyncOperation),
        MFXVideoENCODE_Query: entry!("MFXVideoENCODE_Query", EncodeQuery),
        MFXVideoENCODE_Init: entry!("MFXVideoENCODE_Init", EncodeParams),
        MFXVideoENCODE_Close: entry!("MFXVideoENCODE_Close", EncodeClose),
        MFXVideoENCODE_GetVideoParam: entry!("MFXVideoENCODE_GetVideoParam", EncodeParams),
        MFXVideoENCODE_EncodeFrameAsync: entry!(
            "MFXVideoENCODE_EncodeFrameAsync",
            EncodeFrameAsync
        ),
    })
}

/// The PCI vendor of the adapter a Direct3D 11 device was created on, or
/// [`None`] if the handle will not answer.
///
/// This is how the backend refuses to open on the wrong GPU. A Quick Sync
/// session encodes on the Intel adapter and nowhere else, and the adapter it
/// uses is decided by the Direct3D 11 device the caller hands over — so a
/// device created on a discrete GPU cannot be encoded from, and saying that at
/// `open` is the difference between a clear message and a status code from
/// somewhere inside the runtime (`docs/encoder-pipeline.md`, "Hybrid graphics").
///
/// # Safety
///
/// `device` must be null or a live `ID3D11Device` owned by the caller.
pub(super) unsafe fn device_vendor(device: *mut c_void) -> Option<Vendor> {
    if device.is_null() {
        return None;
    }

    // SAFETY: the caller guarantees `device` is a live COM object it owns.
    // `from_raw_borrowed` takes no reference of its own, so the borrow ends
    // with this function and nothing here releases the caller's device.
    let unknown = unsafe { windows::core::IUnknown::from_raw_borrowed(&device) }?;
    let dxgi: IDXGIDevice = unknown.cast().ok()?;

    // SAFETY: `dxgi` is a live DXGI device obtained by querying the caller's
    // device, and both calls are ordinary queries that return by value or as a
    // new reference this function drops.
    let description: DXGI_ADAPTER_DESC = unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok()? };

    Some(Vendor::from_pci_id(description.VendorId))
}

/// The name oneVPL gives a status code, for diagnostics.
///
/// Every code in the 2.15 headers is listed. An unrecognised one still reports
/// its number, because a status this build has never seen is exactly the case
/// where the number matters.
pub(super) const fn status_name(status: sys::mfxStatus) -> &'static str {
    match status {
        sys::MFX_ERR_NONE => "MFX_ERR_NONE",
        sys::MFX_ERR_UNKNOWN => "MFX_ERR_UNKNOWN",
        sys::MFX_ERR_NULL_PTR => "MFX_ERR_NULL_PTR",
        sys::MFX_ERR_UNSUPPORTED => "MFX_ERR_UNSUPPORTED",
        sys::MFX_ERR_MEMORY_ALLOC => "MFX_ERR_MEMORY_ALLOC",
        sys::MFX_ERR_NOT_ENOUGH_BUFFER => "MFX_ERR_NOT_ENOUGH_BUFFER",
        sys::MFX_ERR_INVALID_HANDLE => "MFX_ERR_INVALID_HANDLE",
        sys::MFX_ERR_LOCK_MEMORY => "MFX_ERR_LOCK_MEMORY",
        sys::MFX_ERR_NOT_INITIALIZED => "MFX_ERR_NOT_INITIALIZED",
        sys::MFX_ERR_NOT_FOUND => "MFX_ERR_NOT_FOUND",
        sys::MFX_ERR_MORE_DATA => "MFX_ERR_MORE_DATA",
        sys::MFX_ERR_MORE_SURFACE => "MFX_ERR_MORE_SURFACE",
        sys::MFX_ERR_ABORTED => "MFX_ERR_ABORTED",
        sys::MFX_ERR_DEVICE_LOST => "MFX_ERR_DEVICE_LOST",
        sys::MFX_ERR_INCOMPATIBLE_VIDEO_PARAM => "MFX_ERR_INCOMPATIBLE_VIDEO_PARAM",
        sys::MFX_ERR_INVALID_VIDEO_PARAM => "MFX_ERR_INVALID_VIDEO_PARAM",
        sys::MFX_ERR_UNDEFINED_BEHAVIOR => "MFX_ERR_UNDEFINED_BEHAVIOR",
        sys::MFX_ERR_DEVICE_FAILED => "MFX_ERR_DEVICE_FAILED",
        sys::MFX_ERR_MORE_BITSTREAM => "MFX_ERR_MORE_BITSTREAM",
        sys::MFX_ERR_GPU_HANG => "MFX_ERR_GPU_HANG",
        sys::MFX_ERR_REALLOC_SURFACE => "MFX_ERR_REALLOC_SURFACE",
        sys::MFX_ERR_RESOURCE_MAPPED => "MFX_ERR_RESOURCE_MAPPED",
        sys::MFX_ERR_NOT_IMPLEMENTED => "MFX_ERR_NOT_IMPLEMENTED",
        sys::MFX_ERR_MORE_EXTBUFFER => "MFX_ERR_MORE_EXTBUFFER",
        sys::MFX_ERR_MORE_DATA_SUBMIT_TASK => "MFX_ERR_MORE_DATA_SUBMIT_TASK",
        sys::MFX_WRN_IN_EXECUTION => "MFX_WRN_IN_EXECUTION",
        sys::MFX_WRN_DEVICE_BUSY => "MFX_WRN_DEVICE_BUSY",
        sys::MFX_WRN_VIDEO_PARAM_CHANGED => "MFX_WRN_VIDEO_PARAM_CHANGED",
        sys::MFX_WRN_PARTIAL_ACCELERATION => "MFX_WRN_PARTIAL_ACCELERATION",
        sys::MFX_WRN_INCOMPATIBLE_VIDEO_PARAM => "MFX_WRN_INCOMPATIBLE_VIDEO_PARAM",
        sys::MFX_WRN_VALUE_NOT_CHANGED => "MFX_WRN_VALUE_NOT_CHANGED",
        sys::MFX_WRN_OUT_OF_RANGE => "MFX_WRN_OUT_OF_RANGE",
        sys::MFX_WRN_FILTER_SKIPPED => "MFX_WRN_FILTER_SKIPPED",
        sys::MFX_WRN_ALLOC_TIMEOUT_EXPIRED => "MFX_WRN_ALLOC_TIMEOUT_EXPIRED",
        sys::MFX_ERR_NONE_PARTIAL_OUTPUT => "MFX_ERR_NONE_PARTIAL_OUTPUT",
        sys::MFX_TASK_WORKING => "MFX_TASK_WORKING",
        sys::MFX_TASK_BUSY => "MFX_TASK_BUSY",
        _ => "an unrecognised oneVPL status",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_status_has_a_name() {
        // A bug report that says "Quick Sync failed with -15" is worth much
        // less than one that says MFX_ERR_INVALID_VIDEO_PARAM, and the mapping
        // is the sort of table that rots silently. The two ranges are the ones
        // `mfxdefs.h` defines: errors are negative and warnings positive.
        for status in -25..=13 {
            if matches!(status, -19 | -20 | 11) {
                // The three numbers `mfxdefs.h` leaves unused.
                continue;
            }
            assert_ne!(
                status_name(status),
                "an unrecognised oneVPL status",
                "status {status} is documented in the 2.15 headers and has no name here"
            );
        }
        assert_eq!(status_name(9999), "an unrecognised oneVPL status");
    }

    #[test]
    fn a_load_failure_says_what_to_do_about_it() {
        // The reader of a bug report has to be able to tell "no Intel driver"
        // from "an Intel driver too old to drive" — the second is fixable by
        // the user and the first is not (AGENTS.md section 15).
        let missing = LoadFailure::NotFound {
            detail: "libmfxhw64.dll: the specified module could not be found".to_owned(),
        }
        .to_string();
        assert!(missing.contains("libvpl.dll"), "{missing}");
        assert!(missing.contains("libmfxhw64.dll"), "{missing}");

        let old = LoadFailure::NoOneVplRuntime {
            library: "libmfxhw64.dll",
        }
        .to_string();
        assert!(old.contains("MFXInitialize"), "{old}");
        assert!(
            old.contains("update the Intel graphics driver"),
            "a version comparison the reader cannot act on: {old}"
        );
    }

    #[test]
    fn the_runtime_is_absent_on_a_machine_with_no_intel_driver() {
        // Not an assertion about Intel hardware: on a machine that has the
        // driver this loads, and on one that does not it reports why. Both are
        // correct answers, and the point of the test is that neither panics
        // and that a failure names every candidate.
        match QuickSyncRuntime::load() {
            Ok(runtime) => {
                assert!(
                    LIBRARIES.contains(&runtime.library()),
                    "the runtime came from a library that is not a candidate: {}",
                    runtime.library()
                );
            }
            Err(failure) => {
                let message = failure.to_string();
                assert!(
                    !message.is_empty(),
                    "a load failure with nothing to say is a bug report with nothing in it"
                );
            }
        }
    }

    #[test]
    fn the_libraries_looked_for_are_the_libraries_named_in_the_error() {
        // Two hand-maintained lists of the same file names drift apart, and the
        // one a user reads is the error message.
        for library in LIBRARIES {
            assert!(
                LIBRARY_LIST.contains(library),
                "{library} is searched for and not named in the message a user gets"
            );
        }
        assert_eq!(
            LIBRARY_LIST.matches(".dll").count(),
            LIBRARIES.len(),
            "the message names a library that is not searched for"
        );
    }

    #[test]
    fn a_null_device_has_no_vendor() {
        // SAFETY: null is one of the two values the contract permits.
        assert_eq!(unsafe { device_vendor(core::ptr::null_mut()) }, None);
    }
}
