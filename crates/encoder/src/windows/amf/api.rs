//! Reaching AMF: loading the runtime, negotiating a version, and the small
//! amount of plumbing every call through a vtable needs.
//!
//! AMF is reached through `amfrt64.dll`, which ships with the AMD display
//! driver. As with NVENC there is no import library and nothing to install: the
//! documented entry points are `AMFQueryVersion` and `AMFInit`, both found with
//! `GetProcAddress`, and the second hands back the factory every other object
//! comes from. A build of Clipped therefore runs on a machine with no AMD
//! hardware at all — the library is simply not there, which is an ordinary
//! [`LoadFailure::NotFound`] rather than a link error.
//!
//! # How a call reaches AMD's code
//!
//! Every AMF interface is a C++ abstract class. AMD's headers also declare each
//! one as a plain C structure holding a pointer to a table of function
//! pointers, which is the same object seen through its vtable, and that is the
//! shape [`sys`](super::sys) is generated from. A call is therefore
//! `(*(*object).pVtbl).Method.unwrap()(object, ...)`: read the table, take the
//! entry, pass the object as the first argument.
//!
//! Interfaces inherit singly — `AMFComponent` is an `AMFPropertyStorageEx` is
//! an `AMFPropertyStorage` is an `AMFInterface` — so every one of those tables
//! begins with the same entries in the same order. That is what lets
//! [`set_property`] take one pointer type for a component, a context and a
//! surface alike; AMD's own C interface relies on it too, in the
//! `AMFPropertyStorage*` parameters of `AddTo` and `CopyTo`.
//!
//! # Version negotiation
//!
//! `AMFInit` takes the interface version the caller was built against. This
//! build asks for [`VERSION`] and, before anything else, asks the installed
//! runtime what it offers; an older one gets a message naming the driver
//! release to install rather than a status code from a later call. AMD's own
//! release notes are what [`MINIMUM_DRIVER`] comes from.
//!
//! # Ownership
//!
//! [`AmfRuntime`] owns the loaded module and releases it in `Drop`. A session
//! holds one for its whole life: every function pointer it uses belongs to that
//! module, and freeing it while a component is alive would leave the session
//! calling into unmapped memory. The factory is not owned — it is a singleton
//! belonging to the library, and its interface has no `Release`.

use core::ffi::c_void;
use core::fmt;
use core::mem;
use core::ptr;

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use super::sys;

/// The library AMF lives in, on 64-bit Windows.
///
/// The same name `crate::windows::runtime` probes for when it reports whether
/// AMF is available at all, and deliberately so: detection and encoding must
/// agree about what "AMF is present" means.
pub(super) const LIBRARY: &str = "amfrt64.dll";

/// The AMF version this build compiles against, as major, minor and release.
///
/// Deliberately older than the newest AMF available, for the same reason the
/// NVENC backend pins interface 12.0: the parts used here — the core
/// interfaces, and the H.264 and HEVC encoder properties — have been stable for
/// years, and asking for an old version is what makes the backend work on a
/// driver somebody installed two years ago.
pub(super) const VERSION: (u32, u32, u32) = (1, 4, 30);

/// The oldest AMD driver release that carries [`VERSION`], for the message a
/// machine with an older one gets.
///
/// From the AMF SDK's own README: "Version 1.4.30: AMD Radeon Software
/// Adrenalin Edition 23.5.2 (23.10.01.45) or newer."
pub(super) const MINIMUM_DRIVER: &str = "Adrenalin 23.5.2";

/// [`VERSION`] packed the way `AMF_MAKE_FULL_VERSION` packs it, which is what
/// `AMFInit` and `AMFQueryVersion` speak.
pub(super) const REQUIRED_VERSION: u64 = full_version(VERSION.0, VERSION.1, VERSION.2, 0);

/// A version number packed the way `AMF_MAKE_FULL_VERSION` packs one.
///
/// Transcribed from `core/Version.h`; bindgen cannot emit it, because it is a
/// macro rather than a constant.
const fn full_version(major: u32, minor: u32, release: u32, build: u32) -> u64 {
    ((major as u64) << 48) | ((minor as u64) << 32) | ((release as u64) << 16) | (build as u64)
}

/// A packed version taken apart again, for a message a reader can act on.
pub(super) const fn version_parts(version: u64) -> (u32, u32, u32) {
    (
        ((version >> 48) & 0xFFFF) as u32,
        ((version >> 32) & 0xFFFF) as u32,
        ((version >> 16) & 0xFFFF) as u32,
    )
}

/// An ASCII string as the nul-terminated UTF-16 AMF names properties and
/// components with.
///
/// Built at compile time, so a property name costs no allocation and no
/// conversion at the point of use — including the ones set per frame. Every
/// name in AMF's headers is ASCII, which the assertion below enforces: a
/// non-ASCII literal fails to compile rather than being silently truncated.
macro_rules! wide {
    ($text:literal) => {{
        const BYTES: &[u8] = $text.as_bytes();
        const WIDE: [u16; BYTES.len() + 1] = {
            let mut wide = [0u16; BYTES.len() + 1];
            let mut index = 0;
            while index < BYTES.len() {
                assert!(
                    BYTES[index] < 0x80,
                    "an AMF property or component name must be ASCII"
                );
                wide[index] = BYTES[index] as u16;
                index += 1;
            }
            wide
        };
        &WIDE
    }};
}

pub(super) use wide;

/// A name back as a `String`, for a log line or a test assertion.
///
/// Only ever given a name built by [`wide!`], so the lossy conversion cannot
/// lose anything.
pub(super) fn name_to_string(name: &[u16]) -> String {
    let text: Vec<u16> = name.iter().copied().take_while(|unit| *unit != 0).collect();
    String::from_utf16_lossy(&text)
}

/// Why the runtime could not be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LoadFailure {
    /// The library is not installed, or the loader refused it.
    NotFound {
        /// What the loader said.
        detail: String,
    },
    /// The library loaded but does not export what AMF requires, which means it
    /// is not the library this build expects.
    MissingEntryPoint {
        /// The export that was looked for.
        name: &'static str,
    },
    /// The installed runtime is older than the version this build compiles
    /// against.
    TooOld {
        /// The version the runtime offers, as major, minor and release.
        installed: (u32, u32, u32),
    },
    /// The runtime would not say which version it is.
    VersionQuery {
        /// The status it returned.
        status: sys::AMF_RESULT,
    },
    /// `AMFInit` refused, or produced no factory.
    Init {
        /// The status it returned.
        status: sys::AMF_RESULT,
    },
}

impl fmt::Display for LoadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { detail } => formatter.write_str(detail),
            Self::MissingEntryPoint { name } => {
                write!(formatter, "the library does not export {name}")
            }
            Self::TooOld { installed } => write!(
                formatter,
                "the runtime is version {}.{}.{}",
                installed.0, installed.1, installed.2
            ),
            Self::VersionQuery { status } => write!(
                formatter,
                "AMFQueryVersion failed with {} ({status})",
                status_name(*status)
            ),
            Self::Init { status } => write!(
                formatter,
                "AMFInit failed with {} ({status})",
                status_name(*status)
            ),
        }
    }
}

/// The loaded AMF runtime and the factory every other object comes from.
///
/// # Ownership
///
/// Owns the module handle and nothing else. `Drop` frees it, and nothing else
/// in this crate does, so the module outlives every object built from it. The
/// factory belongs to the library — `AMFFactory` has no `Release` — so there is
/// nothing to release there.
pub(super) struct AmfRuntime {
    module: HMODULE,
    factory: *mut sys::AMFFactory,
    installed: u64,
}

impl AmfRuntime {
    /// Loads the runtime and creates the factory.
    ///
    /// The library is loaded with `LOAD_LIBRARY_SEARCH_SYSTEM32` so that the
    /// search order cannot be diverted by a file of the same name somewhere on
    /// the path — the same rule `crate::windows::runtime` follows, for the same
    /// reason (AGENTS.md section 13).
    pub(super) fn load() -> Result<Self, LoadFailure> {
        let wide: Vec<u16> = LIBRARY.encode_utf16().chain(core::iter::once(0)).collect();

        // SAFETY: `wide` is a nul-terminated UTF-16 string that outlives the
        // call, the reserved handle parameter is `None` as the documentation
        // requires, and the flag is a documented `LOAD_LIBRARY_SEARCH_*` value.
        // The module returned is owned by the `AmfRuntime` built below, whose
        // `Drop` releases it exactly once.
        let module =
            unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(|error| LoadFailure::NotFound {
                    detail: error.message(),
                })?;

        // From here on the module is owned: every early return has to free it,
        // which is what this guard does without a `let _ =` at each one.
        let guard = ModuleGuard(module);

        // SAFETY: the module is the one just loaded and has not been freed.
        let installed = unsafe { query_version(module) }?;
        if installed < REQUIRED_VERSION {
            return Err(LoadFailure::TooOld {
                installed: version_parts(installed),
            });
        }

        // SAFETY: as above — the module is live for the whole of this function.
        let factory = unsafe { init(module) }?;

        Ok(Self {
            module: guard.take(),
            factory,
            installed,
        })
    }

    /// The factory. Contexts and components are created through it.
    pub(super) const fn factory(&self) -> *mut sys::AMFFactory {
        self.factory
    }

    /// The version of the runtime that was actually loaded, for the log line a
    /// session writes when it opens.
    pub(super) const fn installed_version(&self) -> (u32, u32, u32) {
        version_parts(self.installed)
    }
}

impl Drop for AmfRuntime {
    fn drop(&mut self) {
        // SAFETY: `self.module` came from the single successful
        // `LoadLibraryExW` in `load`, is not copied anywhere else, and this type
        // is not `Clone`, so this is the one and only release of that reference.
        // The session that held the objects is torn down before this runs —
        // `AmfEncoder` drops its session first, by field order — so nothing
        // calls into the module afterwards.
        if let Err(error) = unsafe { FreeLibrary(self.module) } {
            // Not fatal and not silent: the process keeps a library mapped,
            // which costs address space and nothing else (AGENTS.md section 15).
            tracing::debug!(library = LIBRARY, %error, "the encoder runtime could not be released");
        }
    }
}

impl fmt::Debug for AmfRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (major, minor, release) = self.installed_version();
        formatter
            .debug_struct("AmfRuntime")
            .field("library", &LIBRARY)
            .field("installed", &format_args!("{major}.{minor}.{release}"))
            .finish()
    }
}

/// Frees a module unless it is taken, so that the failure paths in
/// [`AmfRuntime::load`] cannot leak one.
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
        // `take` is the only way for that ownership to leave, at which point the
        // guard is forgotten rather than dropped.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

/// Asks the runtime which version it is.
///
/// # Safety
///
/// `module` must be a live `amfrt64.dll`.
unsafe fn query_version(module: HMODULE) -> Result<u64, LoadFailure> {
    const NAME: &str = "AMFQueryVersion";

    // SAFETY: the module is live, per this function's contract, and the name is
    // nul-terminated.
    let symbol = unsafe { GetProcAddress(module, PCSTR(c"AMFQueryVersion".as_ptr().cast())) }
        .ok_or(LoadFailure::MissingEntryPoint { name: NAME })?;

    // SAFETY: this is the signature `core/Factory.h` declares for the export
    // (`AMFQueryVersion_Fn`). `AMF_CDECL_CALL` is `__cdecl`, and on x86-64
    // Windows there is only one calling convention, so `extern "C"` is it.
    let function: unsafe extern "C" fn(*mut u64) -> sys::AMF_RESULT =
        unsafe { mem::transmute(symbol) };

    let mut version: u64 = 0;
    // SAFETY: `version` is a live local for the duration of the call, which is
    // all the entry point requires.
    let status = unsafe { function(&raw mut version) };
    if status != sys::AMF_OK {
        return Err(LoadFailure::VersionQuery { status });
    }
    Ok(version)
}

/// Creates the factory.
///
/// # Safety
///
/// `module` must be a live `amfrt64.dll`.
unsafe fn init(module: HMODULE) -> Result<*mut sys::AMFFactory, LoadFailure> {
    const NAME: &str = "AMFInit";

    // SAFETY: the module is live, per this function's contract, and the name is
    // nul-terminated.
    let symbol = unsafe { GetProcAddress(module, PCSTR(c"AMFInit".as_ptr().cast())) }
        .ok_or(LoadFailure::MissingEntryPoint { name: NAME })?;

    // SAFETY: this is the signature `core/Factory.h` declares for the export
    // (`AMFInit_Fn`), with the same argument about the calling convention as
    // `query_version` above.
    let function: unsafe extern "C" fn(u64, *mut *mut sys::AMFFactory) -> sys::AMF_RESULT =
        unsafe { mem::transmute(symbol) };

    let mut factory: *mut sys::AMFFactory = ptr::null_mut();
    // SAFETY: `factory` is a live local out-parameter and the version is the
    // packed constant the entry point expects.
    let status = unsafe { function(REQUIRED_VERSION, &raw mut factory) };
    if status != sys::AMF_OK {
        return Err(LoadFailure::Init { status });
    }
    if factory.is_null() {
        // A success that produced nothing is not a success, and treating it as
        // one would dereference null on the next call.
        return Err(LoadFailure::Init {
            status: sys::AMF_FAIL,
        });
    }
    Ok(factory)
}

/// The name AMD gives a status code, for diagnostics.
///
/// Every code in `core/Result.h` at version 1.4.30 is listed. An unrecognised
/// one still reports its number, because a status this build has never seen is
/// exactly the case where the number matters.
pub(super) const fn status_name(status: sys::AMF_RESULT) -> &'static str {
    match status {
        sys::AMF_OK => "AMF_OK",
        sys::AMF_FAIL => "AMF_FAIL",
        sys::AMF_UNEXPECTED => "AMF_UNEXPECTED",
        sys::AMF_ACCESS_DENIED => "AMF_ACCESS_DENIED",
        sys::AMF_INVALID_ARG => "AMF_INVALID_ARG",
        sys::AMF_OUT_OF_RANGE => "AMF_OUT_OF_RANGE",
        sys::AMF_OUT_OF_MEMORY => "AMF_OUT_OF_MEMORY",
        sys::AMF_INVALID_POINTER => "AMF_INVALID_POINTER",
        sys::AMF_NO_INTERFACE => "AMF_NO_INTERFACE",
        sys::AMF_NOT_IMPLEMENTED => "AMF_NOT_IMPLEMENTED",
        sys::AMF_NOT_SUPPORTED => "AMF_NOT_SUPPORTED",
        sys::AMF_NOT_FOUND => "AMF_NOT_FOUND",
        sys::AMF_ALREADY_INITIALIZED => "AMF_ALREADY_INITIALIZED",
        sys::AMF_NOT_INITIALIZED => "AMF_NOT_INITIALIZED",
        sys::AMF_INVALID_FORMAT => "AMF_INVALID_FORMAT",
        sys::AMF_WRONG_STATE => "AMF_WRONG_STATE",
        sys::AMF_FILE_NOT_OPEN => "AMF_FILE_NOT_OPEN",
        sys::AMF_NO_DEVICE => "AMF_NO_DEVICE",
        sys::AMF_DIRECTX_FAILED => "AMF_DIRECTX_FAILED",
        sys::AMF_OPENCL_FAILED => "AMF_OPENCL_FAILED",
        sys::AMF_GLX_FAILED => "AMF_GLX_FAILED",
        sys::AMF_XV_FAILED => "AMF_XV_FAILED",
        sys::AMF_ALSA_FAILED => "AMF_ALSA_FAILED",
        sys::AMF_EOF => "AMF_EOF",
        sys::AMF_REPEAT => "AMF_REPEAT",
        sys::AMF_INPUT_FULL => "AMF_INPUT_FULL",
        sys::AMF_RESOLUTION_CHANGED => "AMF_RESOLUTION_CHANGED",
        sys::AMF_RESOLUTION_UPDATED => "AMF_RESOLUTION_UPDATED",
        sys::AMF_INVALID_DATA_TYPE => "AMF_INVALID_DATA_TYPE",
        sys::AMF_INVALID_RESOLUTION => "AMF_INVALID_RESOLUTION",
        sys::AMF_CODEC_NOT_SUPPORTED => "AMF_CODEC_NOT_SUPPORTED",
        sys::AMF_SURFACE_FORMAT_NOT_SUPPORTED => "AMF_SURFACE_FORMAT_NOT_SUPPORTED",
        sys::AMF_SURFACE_MUST_BE_SHARED => "AMF_SURFACE_MUST_BE_SHARED",
        sys::AMF_DECODER_NOT_PRESENT => "AMF_DECODER_NOT_PRESENT",
        sys::AMF_DECODER_SURFACE_ALLOCATION_FAILED => "AMF_DECODER_SURFACE_ALLOCATION_FAILED",
        sys::AMF_DECODER_NO_FREE_SURFACES => "AMF_DECODER_NO_FREE_SURFACES",
        sys::AMF_ENCODER_NOT_PRESENT => "AMF_ENCODER_NOT_PRESENT",
        sys::AMF_DEM_ERROR => "AMF_DEM_ERROR",
        sys::AMF_DEM_PROPERTY_READONLY => "AMF_DEM_PROPERTY_READONLY",
        sys::AMF_DEM_REMOTE_DISPLAY_CREATE_FAILED => "AMF_DEM_REMOTE_DISPLAY_CREATE_FAILED",
        sys::AMF_DEM_START_ENCODING_FAILED => "AMF_DEM_START_ENCODING_FAILED",
        sys::AMF_DEM_QUERY_OUTPUT_FAILED => "AMF_DEM_QUERY_OUTPUT_FAILED",
        sys::AMF_TAN_CLIPPING_WAS_REQUIRED => "AMF_TAN_CLIPPING_WAS_REQUIRED",
        sys::AMF_TAN_UNSUPPORTED_VERSION => "AMF_TAN_UNSUPPORTED_VERSION",
        sys::AMF_NEED_MORE_INPUT => "AMF_NEED_MORE_INPUT",
        _ => "an unrecognised AMF status",
    }
}

// ---------------------------------------------------------------------------
// Variants
// ---------------------------------------------------------------------------

/// An empty variant, for a `GetProperty` out-parameter.
///
/// Every constructor here starts from a zeroed structure rather than writing
/// one union member and leaving the rest of the bytes as they were: the whole
/// value is passed to AMD's code by value, and handing another language
/// uninitialised bytes is not something to do for the sake of a few instructions.
pub(super) fn empty_variant() -> sys::AMFVariantStruct {
    // SAFETY: `AMFVariantStruct` is a tag and a union of integers, floats,
    // pointers and small plain structures. All-zeroes is `AMF_VARIANT_EMPTY`
    // with a zeroed payload, which is a valid value of every member.
    unsafe { mem::zeroed() }
}

/// A whole-number variant, which is what most AMF properties take.
pub(super) fn int64(value: i64) -> sys::AMFVariantStruct {
    let mut variant = empty_variant();
    variant.type_ = sys::AMF_VARIANT_INT64;
    variant.__bindgen_anon_1.int64Value = value;
    variant
}

/// A boolean variant.
pub(super) fn boolean(value: bool) -> sys::AMFVariantStruct {
    let mut variant = empty_variant();
    variant.type_ = sys::AMF_VARIANT_BOOL;
    variant.__bindgen_anon_1.boolValue = u8::from(value);
    variant
}

/// A picture-size variant.
pub(super) fn size(width: u32, height: u32) -> sys::AMFVariantStruct {
    let mut variant = empty_variant();
    variant.type_ = sys::AMF_VARIANT_SIZE;
    variant.__bindgen_anon_1.sizeValue = sys::AMFSize {
        // Saturating rather than wrapping: a width beyond `i32::MAX` is not a
        // picture, and no encoder would accept the negative number a cast would
        // produce.
        width: i32::try_from(width).unwrap_or(i32::MAX),
        height: i32::try_from(height).unwrap_or(i32::MAX),
    };
    variant
}

/// A frame-rate variant, as the exact ratio.
pub(super) fn rate(numerator: u32, denominator: u32) -> sys::AMFVariantStruct {
    let mut variant = empty_variant();
    variant.type_ = sys::AMF_VARIANT_RATE;
    variant.__bindgen_anon_1.rateValue = sys::AMFRate {
        num: numerator,
        den: denominator,
    };
    variant
}

/// The whole number inside a variant, or [`None`] if it holds something else.
pub(super) fn as_int64(variant: &sys::AMFVariantStruct) -> Option<i64> {
    if variant.type_ == sys::AMF_VARIANT_INT64 {
        // SAFETY: the tag says the `int64Value` member is the one that was
        // written, which is exactly the condition for reading it.
        Some(unsafe { variant.__bindgen_anon_1.int64Value })
    } else {
        None
    }
}

/// A variant's value as a whole number, whichever numeric type it holds.
///
/// The capability properties are the reason this is not just [`as_int64`]: AMF
/// declares them as `amf_bool` and `amf_int64` in its headers, and a component
/// stores each in whichever the header says — so a reader that insisted on
/// `AMF_VARIANT_INT64` would silently fail to measure whichever of them the
/// driver happened to store as a boolean. A `false` is `0` and a `true` is `1`,
/// which is what both callers of this want.
pub(super) fn as_whole_number(variant: &sys::AMFVariantStruct) -> Option<i64> {
    match variant.type_ {
        // SAFETY: the tag says which union member was written, which is exactly
        // the condition for reading it.
        sys::AMF_VARIANT_INT64 => Some(unsafe { variant.__bindgen_anon_1.int64Value }),
        // SAFETY: as above.
        sys::AMF_VARIANT_BOOL => Some(i64::from(
            unsafe { variant.__bindgen_anon_1.boolValue } != 0,
        )),
        _ => None,
    }
}

/// The interface inside a variant, or [`None`] if it holds something else.
///
/// The reference belongs to the caller: `GetProperty` copies the property's
/// variant, which acquires the interface, so whoever reads it here has to
/// release it.
pub(super) fn as_interface(variant: &sys::AMFVariantStruct) -> Option<*mut sys::AMFInterface> {
    if variant.type_ == sys::AMF_VARIANT_INTERFACE {
        // SAFETY: the tag says the `pInterface` member is the one that was
        // written.
        let interface = unsafe { variant.__bindgen_anon_1.pInterface };
        (!interface.is_null()).then_some(interface)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Calling through a vtable
// ---------------------------------------------------------------------------

/// Sets one property on any AMF object that stores properties.
///
/// # Safety
///
/// `storage` must be a live AMF object whose interface derives from
/// `AMFPropertyStorage` — a context, a component, a surface or a buffer — and
/// `name` must be nul-terminated, which every name built by [`wide!`] is.
pub(super) unsafe fn set_property(
    storage: *mut sys::AMFPropertyStorage,
    name: &[u16],
    value: sys::AMFVariantStruct,
) -> sys::AMF_RESULT {
    // SAFETY: the caller guarantees a live object, so its vtable pointer is
    // valid and points at a table beginning with `AMFPropertyStorage`'s entries
    // (see this module's header).
    let Some(function) = (unsafe { (*(*storage).pVtbl).SetProperty }) else {
        return sys::AMF_NOT_IMPLEMENTED;
    };
    // SAFETY: the object is live, the name outlives the call and is
    // nul-terminated, and the variant is passed by value.
    unsafe { function(storage, name.as_ptr(), value) }
}

/// Reads one property from any AMF object that stores properties.
///
/// # Safety
///
/// As [`set_property`].
pub(super) unsafe fn get_property(
    storage: *mut sys::AMFPropertyStorage,
    name: &[u16],
) -> Result<sys::AMFVariantStruct, sys::AMF_RESULT> {
    // SAFETY: as `set_property`.
    let Some(function) = (unsafe { (*(*storage).pVtbl).GetProperty }) else {
        return Err(sys::AMF_NOT_IMPLEMENTED);
    };

    let mut value = empty_variant();
    // SAFETY: the object is live, the name is nul-terminated, and `value` is a
    // live local out-parameter.
    let status = unsafe { function(storage, name.as_ptr(), &raw mut value) };
    if status == sys::AMF_OK {
        Ok(value)
    } else {
        Err(status)
    }
}

/// Releases one reference to an AMF interface.
///
/// Returns the reference count that remains, which is how this backend knows
/// whether AMF has finished with an object it was handed.
///
/// # Safety
///
/// `interface` must be a live AMF object the caller holds a reference to, and
/// that reference is consumed.
pub(super) unsafe fn release(interface: *mut sys::AMFInterface) -> i32 {
    // SAFETY: the caller guarantees a live object, whose vtable begins with
    // `AMFInterface`'s three entries.
    let Some(function) = (unsafe { (*(*interface).pVtbl).Release }) else {
        return 0;
    };
    // SAFETY: the object is live and the caller's reference is consumed here.
    unsafe { function(interface) }
}

/// Takes a reference to an AMF interface, and returns the new count.
///
/// # Safety
///
/// `interface` must be a live AMF object the caller holds a reference to.
pub(super) unsafe fn acquire(interface: *mut sys::AMFInterface) -> i32 {
    // SAFETY: as `release`.
    let Some(function) = (unsafe { (*(*interface).pVtbl).Acquire }) else {
        return 0;
    };
    // SAFETY: the object is live for the duration of the call.
    unsafe { function(interface) }
}

/// Asks an interface for another one, by identifier.
///
/// # Safety
///
/// `interface` must be a live AMF object, and `identifier` must be the
/// identifier of the interface `T` describes — otherwise the returned pointer
/// would be used with the wrong vtable.
pub(super) unsafe fn query_interface<T>(
    interface: *mut sys::AMFInterface,
    identifier: &sys::AMFGuid,
) -> Result<*mut T, sys::AMF_RESULT> {
    // SAFETY: the caller guarantees a live object, whose vtable begins with
    // `AMFInterface`'s three entries.
    let Some(function) = (unsafe { (*(*interface).pVtbl).QueryInterface }) else {
        return Err(sys::AMF_NOT_IMPLEMENTED);
    };

    let mut found: *mut c_void = ptr::null_mut();
    // SAFETY: the object is live, `identifier` outlives the call, and `found` is
    // a live local out-parameter.
    let status = unsafe { function(interface, identifier, &raw mut found) };
    if status != sys::AMF_OK {
        return Err(status);
    }
    if found.is_null() {
        return Err(sys::AMF_NO_INTERFACE);
    }
    Ok(found.cast::<T>())
}

/// The identifier of `AMFBuffer`.
///
/// Transcribed from the `AMF_DECLARE_IID` line in `core/Buffer.h`; bindgen
/// cannot emit it, because in AMD's headers it is a static inline function
/// rather than a constant.
pub(super) const BUFFER_IID: sys::AMFGuid = sys::AMFGuid {
    data1: 0xb04b_7248,
    data2: 0xb6f0,
    data3: 0x4321,
    data41: 0xb6,
    data42: 0x91,
    data43: 0xba,
    data44: 0xa4,
    data45: 0x74,
    data46: 0x0f,
    data47: 0x9f,
    data48: 0xcb,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_packed_the_way_amf_packs_one() {
        // Transcribed arithmetic is the one part of an FFI binding a compiler
        // cannot check, and a wrong version here is refused by `AMFInit` with a
        // status that says nothing about which digit was wrong. This is
        // `AMF_MAKE_FULL_VERSION(1, 4, 30, 0)` done by hand.
        assert_eq!(REQUIRED_VERSION, 0x0001_0004_001E_0000);
        assert_eq!(version_parts(REQUIRED_VERSION), (1, 4, 30));
        assert_eq!(version_parts(full_version(1, 4, 37, 0)), (1, 4, 37));
        assert!(full_version(1, 4, 29, 0) < REQUIRED_VERSION);
        assert!(full_version(1, 5, 0, 0) > REQUIRED_VERSION);
    }

    #[test]
    fn a_name_is_the_utf16_amf_expects() {
        // A property AMF does not recognise is not an error: `SetProperty`
        // answers AMF_NOT_FOUND and the encoder runs with the default. So a
        // mangled name would show up as a recording with the wrong bit rate
        // rather than as a failure, which is why the conversion is checked.
        let name: &[u16] = wide!("TargetBitrate");
        assert_eq!(name.last(), Some(&0), "AMF names are nul-terminated");
        assert_eq!(name.len(), "TargetBitrate".len() + 1);
        assert_eq!(name_to_string(name), "TargetBitrate");
    }

    #[test]
    fn a_load_failure_names_the_entry_point_that_failed() {
        // The reader of a bug report goes looking for the function named in it.
        assert!(
            LoadFailure::VersionQuery {
                status: sys::AMF_FAIL,
            }
            .to_string()
            .starts_with("AMFQueryVersion failed"),
            "{}",
            LoadFailure::VersionQuery {
                status: sys::AMF_FAIL
            }
        );
        assert!(
            LoadFailure::Init {
                status: sys::AMF_NOT_SUPPORTED,
            }
            .to_string()
            .starts_with("AMFInit failed"),
            "{}",
            LoadFailure::Init {
                status: sys::AMF_NOT_SUPPORTED
            }
        );
        assert_eq!(
            LoadFailure::TooOld {
                installed: (1, 4, 29)
            }
            .to_string(),
            "the runtime is version 1.4.29"
        );
    }

    #[test]
    fn every_documented_status_has_a_name() {
        // A bug report that says "AMF failed with 31" is worth much less than
        // one that says AMF_SURFACE_FORMAT_NOT_SUPPORTED, and the mapping is the
        // sort of table that rots silently.
        for status in 0..=sys::AMF_NEED_MORE_INPUT {
            assert_ne!(
                status_name(status),
                "an unrecognised AMF status",
                "status {status} is documented in AMF 1.4.30 and has no name here"
            );
        }
        assert_eq!(status_name(9999), "an unrecognised AMF status");
    }

    #[test]
    fn a_variant_carries_its_tag_and_its_value() {
        let bitrate = int64(40_000_000);
        assert_eq!(bitrate.type_, sys::AMF_VARIANT_INT64);
        assert_eq!(as_int64(&bitrate), Some(40_000_000));

        // A variant of another type must not be read as a number: AMF would be
        // told a bit rate made of the bytes of a pointer.
        assert_eq!(as_int64(&boolean(true)), None);
        assert_eq!(as_int64(&size(2560, 1440)), None);
        assert_eq!(as_int64(&empty_variant()), None);
        assert_eq!(as_interface(&bitrate), None);

        let frame_size = size(2560, 1440);
        assert_eq!(frame_size.type_, sys::AMF_VARIANT_SIZE);
        // SAFETY: the tag says `sizeValue` is the member that was written.
        let value = unsafe { frame_size.__bindgen_anon_1.sizeValue };
        assert_eq!((value.width, value.height), (2560, 1440));

        let frame_rate = rate(60_000, 1001);
        assert_eq!(frame_rate.type_, sys::AMF_VARIANT_RATE);
        // SAFETY: the tag says `rateValue` is the member that was written.
        let value = unsafe { frame_rate.__bindgen_anon_1.rateValue };
        assert_eq!((value.num, value.den), (60_000, 1001));
    }

    #[test]
    fn a_capability_reads_as_a_number_whether_amf_stored_it_as_one_or_as_a_flag() {
        // The capability properties are declared as both types across AMD's
        // headers, and a reader that took only `AMF_VARIANT_INT64` would report
        // "not measured" for whichever of them the driver stored as a boolean —
        // a measurement lost with no failure anywhere.
        assert_eq!(as_whole_number(&int64(2_073_600)), Some(2_073_600));
        assert_eq!(as_whole_number(&boolean(true)), Some(1));
        assert_eq!(as_whole_number(&boolean(false)), Some(0));

        // And anything that is not a number must stay unmeasured rather than
        // being read out of the wrong union member.
        assert_eq!(as_whole_number(&size(2560, 1440)), None);
        assert_eq!(as_whole_number(&empty_variant()), None);
    }
}
