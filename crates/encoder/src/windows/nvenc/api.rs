//! Reaching NVENC: loading the runtime, negotiating a version, and turning
//! status codes back into sentences.
//!
//! NVENC is reached through `nvEncodeAPI64.dll`, which ships with the display
//! driver. There is no import library to link against and nothing to install:
//! the documented entry point is to load the library, ask it for a table of
//! function pointers, and call through the table. That is what this module
//! does, and it is why a build of Clipped compiles and runs on a machine with
//! no NVIDIA hardware at all — the library is simply not there, which is an
//! ordinary [`LoadFailure::NotFound`] rather than a link error.
//!
//! # Version negotiation
//!
//! The interface is versioned twice over: the table carries a version, every
//! structure carries a version, and the driver refuses anything newer than it
//! understands. This build compiles against interface
//! [`API_MAJOR`].[`API_MINOR`] and asks the driver, before anything else, what
//! it supports. A driver older than that gets a message naming the release to
//! install, rather than `NV_ENC_ERR_INVALID_VERSION` from a call several steps
//! later.
//!
//! Newer drivers are fine: NVENC is backwards compatible, so a 2026 driver
//! still accepts the 12.0 structures below. Pinning to an older interface than
//! the newest available is therefore a deliberate trade — a few features this
//! project does not use, in exchange for working on every driver back to
//! [`MINIMUM_DRIVER`].
//!
//! # Ownership
//!
//! [`NvencRuntime`] owns the loaded module and releases it in `Drop`. A session
//! holds one for its whole life: the function pointers belong to the module,
//! and freeing it while an encoder is open would leave the session calling into
//! unmapped memory.

use core::ffi::{c_void, CStr};
use core::fmt;
use core::mem;

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use super::sys;

/// The library the encoder lives in, on 64-bit Windows.
///
/// The same name `crate::windows::runtime` probes for when it reports whether
/// NVENC is available at all, and deliberately so: detection and encoding must
/// agree about what "NVENC is present" means.
pub(super) const LIBRARY: &str = "nvEncodeAPI64.dll";

/// The major version of the NVENC interface this build compiles against.
pub(super) const API_MAJOR: u32 = 12;

/// The minor version of the NVENC interface this build compiles against.
pub(super) const API_MINOR: u32 = 0;

/// The oldest Windows driver release that carries interface 12.0, for the
/// message a machine with an older driver gets.
///
/// From NVIDIA's Video Codec SDK 12.0 release notes.
pub(super) const MINIMUM_DRIVER: &str = "522.25";

/// The interface version, packed the way `NVENCAPI_VERSION` packs it.
const API_VERSION: u32 = API_MAJOR | (API_MINOR << 24);

/// The version stamp every NVENC structure carries, built the way
/// `NVENCAPI_STRUCT_VERSION` builds it.
///
/// The driver reads this field first and rejects the call if it does not
/// recognise it, which is how a structure that grew a field in a later SDK
/// stays compatible: the version says which shape the caller is using.
const fn struct_version(revision: u32) -> u32 {
    API_VERSION | (revision << 16) | (0x7 << 28)
}

/// The high bit some structures add to their version, marking the ones whose
/// layout is only valid for the exact interface version above.
const EXACT: u32 = 1 << 31;

// The structure versions, transcribed from the `*_VER` macros in
// `nvEncodeAPI.h` 12.0. bindgen cannot generate these: they are macro
// invocations, not constants (see `sys.rs`).
pub(super) const FUNCTION_LIST_VER: u32 = struct_version(2);
pub(super) const OPEN_SESSION_EX_PARAMS_VER: u32 = struct_version(1);
pub(super) const CAPS_PARAM_VER: u32 = struct_version(1);
pub(super) const CONFIG_VER: u32 = struct_version(8) | EXACT;
pub(super) const INITIALIZE_PARAMS_VER: u32 = struct_version(5) | EXACT;
pub(super) const PRESET_CONFIG_VER: u32 = struct_version(4) | EXACT;
pub(super) const PIC_PARAMS_VER: u32 = struct_version(6) | EXACT;
pub(super) const LOCK_BITSTREAM_VER: u32 = struct_version(2);
pub(super) const MAP_INPUT_RESOURCE_VER: u32 = struct_version(4);
pub(super) const REGISTER_RESOURCE_VER: u32 = struct_version(4);
pub(super) const CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
pub(super) const SEQUENCE_PARAM_PAYLOAD_VER: u32 = struct_version(1);

/// Why the runtime could not be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LoadFailure {
    /// The library is not installed, or the loader refused it.
    NotFound {
        /// What the loader said.
        detail: String,
    },
    /// The library loaded but does not export what the interface requires,
    /// which means it is not the library this build expects.
    MissingEntryPoint {
        /// The export that was looked for.
        name: &'static str,
    },
    /// The driver is older than the interface this build compiles against.
    TooOld {
        /// The newest interface the driver offers, as `major.minor`.
        installed: (u32, u32),
    },
    /// The driver would not say which interface version it supports.
    VersionQuery {
        /// The status the runtime returned.
        status: sys::NVENCSTATUS,
    },
    /// Creating the function table failed.
    CreateInstance {
        /// The status the runtime returned.
        status: sys::NVENCSTATUS,
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
                "the driver offers interface {}.{}",
                installed.0, installed.1
            ),
            Self::VersionQuery { status } => write!(
                formatter,
                "NvEncodeAPIGetMaxSupportedVersion failed with {} ({status})",
                status_name(*status)
            ),
            Self::CreateInstance { status } => write!(
                formatter,
                "NvEncodeAPICreateInstance failed with {} ({status})",
                status_name(*status)
            ),
        }
    }
}

/// The loaded NVENC runtime and its function table.
///
/// # Ownership
///
/// Owns the module handle. `Drop` frees it, and nothing else in this crate
/// does, so the module outlives every function pointer taken from it.
pub(super) struct NvencRuntime {
    module: HMODULE,
    functions: sys::NV_ENCODE_API_FUNCTION_LIST,
}

impl NvencRuntime {
    /// Loads the runtime and builds the function table.
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
        // The module returned is owned by the `NvencRuntime` built below, whose
        // `Drop` releases it exactly once.
        let module =
            unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(|error| LoadFailure::NotFound {
                    detail: error.message(),
                })?;

        // From here on the module is owned: every early return has to free it,
        // which is what this guard does without a `let _ =` at each one.
        let runtime = ModuleGuard(module);

        // SAFETY: the module is the one just loaded, and it has not been
        // freed.
        let max_version = unsafe { max_supported_version(module) }?;
        if max_version < (API_MAJOR, API_MINOR) {
            return Err(LoadFailure::TooOld {
                installed: max_version,
            });
        }

        // SAFETY: as above — the module is live for the whole of this
        // function.
        let functions = unsafe { create_instance(module) }?;

        Ok(Self {
            module: runtime.take(),
            functions,
        })
    }

    /// The function table. Every call into NVENC goes through it.
    pub(super) const fn functions(&self) -> &sys::NV_ENCODE_API_FUNCTION_LIST {
        &self.functions
    }
}

impl Drop for NvencRuntime {
    fn drop(&mut self) {
        // SAFETY: `self.module` came from the single successful
        // `LoadLibraryExW` in `load`, is not copied anywhere else, and this
        // type is not `Clone`, so this is the one and only release of that
        // reference. The session that held the function pointers is destroyed
        // before this runs — `NvencEncoder` drops its session first, by field
        // order — so nothing calls into the module afterwards.
        if let Err(error) = unsafe { FreeLibrary(self.module) } {
            // Not fatal and not silent: the process keeps a library mapped,
            // which costs address space and nothing else (AGENTS.md section 15).
            tracing::debug!(library = LIBRARY, %error, "the encoder runtime could not be released");
        }
    }
}

impl fmt::Debug for NvencRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NvencRuntime")
            .field("library", &LIBRARY)
            .field("interface", &format_args!("{API_MAJOR}.{API_MINOR}"))
            .finish()
    }
}

/// Frees a module unless it is taken, so that the failure paths in
/// [`NvencRuntime::load`] cannot leak one.
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

/// Asks the driver for the newest interface version it supports.
///
/// # Safety
///
/// `module` must be a live `nvEncodeAPI64.dll`.
unsafe fn max_supported_version(module: HMODULE) -> Result<(u32, u32), LoadFailure> {
    const NAME: &str = "NvEncodeAPIGetMaxSupportedVersion";

    // SAFETY: the module is live, per this function's contract, and the name
    // is nul-terminated.
    let symbol = unsafe {
        GetProcAddress(
            module,
            PCSTR(c"NvEncodeAPIGetMaxSupportedVersion".as_ptr().cast()),
        )
    }
    .ok_or(LoadFailure::MissingEntryPoint { name: NAME })?;

    // SAFETY: `NvEncodeAPIGetMaxSupportedVersion` has this signature in
    // `nvEncodeAPI.h`, and on x86-64 Windows there is only one calling
    // convention, so `extern "C"` is the `NVENCAPI` (`__stdcall`) one.
    let function: unsafe extern "C" fn(*mut u32) -> sys::NVENCSTATUS =
        unsafe { mem::transmute(symbol) };

    let mut version: u32 = 0;
    // SAFETY: `version` is a live local for the duration of the call, which is
    // all the entry point requires.
    let status = unsafe { function(&raw mut version) };
    if status != sys::NV_ENC_SUCCESS {
        return Err(LoadFailure::VersionQuery { status });
    }

    // The driver packs the version as `(major << 4) | minor`.
    Ok((version >> 4, version & 0xF))
}

/// Builds the function table.
///
/// # Safety
///
/// `module` must be a live `nvEncodeAPI64.dll`.
unsafe fn create_instance(
    module: HMODULE,
) -> Result<sys::NV_ENCODE_API_FUNCTION_LIST, LoadFailure> {
    // SAFETY: the module is live, per this function's contract, and the name
    // is nul-terminated.
    let symbol =
        unsafe { GetProcAddress(module, PCSTR(c"NvEncodeAPICreateInstance".as_ptr().cast())) }
            .ok_or(LoadFailure::MissingEntryPoint {
                name: "NvEncodeAPICreateInstance",
            })?;

    // SAFETY: this is the signature `nvEncodeAPI.h` declares for the export.
    let function: unsafe extern "C" fn(*mut sys::NV_ENCODE_API_FUNCTION_LIST) -> sys::NVENCSTATUS =
        unsafe { mem::transmute(symbol) };

    let mut functions: sys::NV_ENCODE_API_FUNCTION_LIST =
        // SAFETY: the structure is plain data with no invalid bit patterns —
        // integers and function pointers, the latter represented as `Option`,
        // for which all-zeroes is `None`. NVENC requires the whole structure to
        // be zeroed apart from `version`.
        unsafe { mem::zeroed() };
    functions.version = FUNCTION_LIST_VER;

    // SAFETY: `functions` is a live local of exactly the type the entry point
    // expects, with its version field set as the interface requires.
    let status = unsafe { function(&raw mut functions) };
    if status != sys::NV_ENC_SUCCESS {
        return Err(LoadFailure::CreateInstance { status });
    }

    Ok(functions)
}

/// The name NVIDIA gives a status code, for diagnostics.
///
/// Every code in interface 12.0 is listed. An unrecognised one still reports
/// its number, because a status this build has never seen is exactly the case
/// where the number matters.
pub(super) const fn status_name(status: sys::NVENCSTATUS) -> &'static str {
    match status {
        sys::NV_ENC_SUCCESS => "NV_ENC_SUCCESS",
        sys::NV_ENC_ERR_NO_ENCODE_DEVICE => "NV_ENC_ERR_NO_ENCODE_DEVICE",
        sys::NV_ENC_ERR_UNSUPPORTED_DEVICE => "NV_ENC_ERR_UNSUPPORTED_DEVICE",
        sys::NV_ENC_ERR_INVALID_ENCODERDEVICE => "NV_ENC_ERR_INVALID_ENCODERDEVICE",
        sys::NV_ENC_ERR_INVALID_DEVICE => "NV_ENC_ERR_INVALID_DEVICE",
        sys::NV_ENC_ERR_DEVICE_NOT_EXIST => "NV_ENC_ERR_DEVICE_NOT_EXIST",
        sys::NV_ENC_ERR_INVALID_PTR => "NV_ENC_ERR_INVALID_PTR",
        sys::NV_ENC_ERR_INVALID_EVENT => "NV_ENC_ERR_INVALID_EVENT",
        sys::NV_ENC_ERR_INVALID_PARAM => "NV_ENC_ERR_INVALID_PARAM",
        sys::NV_ENC_ERR_INVALID_CALL => "NV_ENC_ERR_INVALID_CALL",
        sys::NV_ENC_ERR_OUT_OF_MEMORY => "NV_ENC_ERR_OUT_OF_MEMORY",
        sys::NV_ENC_ERR_ENCODER_NOT_INITIALIZED => "NV_ENC_ERR_ENCODER_NOT_INITIALIZED",
        sys::NV_ENC_ERR_UNSUPPORTED_PARAM => "NV_ENC_ERR_UNSUPPORTED_PARAM",
        sys::NV_ENC_ERR_LOCK_BUSY => "NV_ENC_ERR_LOCK_BUSY",
        sys::NV_ENC_ERR_NOT_ENOUGH_BUFFER => "NV_ENC_ERR_NOT_ENOUGH_BUFFER",
        sys::NV_ENC_ERR_INVALID_VERSION => "NV_ENC_ERR_INVALID_VERSION",
        sys::NV_ENC_ERR_MAP_FAILED => "NV_ENC_ERR_MAP_FAILED",
        sys::NV_ENC_ERR_NEED_MORE_INPUT => "NV_ENC_ERR_NEED_MORE_INPUT",
        sys::NV_ENC_ERR_ENCODER_BUSY => "NV_ENC_ERR_ENCODER_BUSY",
        sys::NV_ENC_ERR_EVENT_NOT_REGISTERD => "NV_ENC_ERR_EVENT_NOT_REGISTERD",
        sys::NV_ENC_ERR_GENERIC => "NV_ENC_ERR_GENERIC",
        sys::NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY => "NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY",
        sys::NV_ENC_ERR_UNIMPLEMENTED => "NV_ENC_ERR_UNIMPLEMENTED",
        sys::NV_ENC_ERR_RESOURCE_REGISTER_FAILED => "NV_ENC_ERR_RESOURCE_REGISTER_FAILED",
        sys::NV_ENC_ERR_RESOURCE_NOT_REGISTERED => "NV_ENC_ERR_RESOURCE_NOT_REGISTERED",
        sys::NV_ENC_ERR_RESOURCE_NOT_MAPPED => "NV_ENC_ERR_RESOURCE_NOT_MAPPED",
        _ => "an unrecognised NVENC status",
    }
}

/// Whatever the runtime has to say about the last failure on `encoder`.
///
/// # Safety
///
/// `encoder` must be a live session opened through `functions`, or null.
pub(super) unsafe fn last_error(
    functions: &sys::NV_ENCODE_API_FUNCTION_LIST,
    encoder: *mut c_void,
) -> String {
    let Some(get) = functions.nvEncGetLastErrorString else {
        return String::new();
    };

    // SAFETY: the caller guarantees `encoder`, and the runtime documents this
    // call as returning a nul-terminated string owned by the session and valid
    // until the next call into it — which is why it is copied here rather than
    // borrowed.
    let message = unsafe { get(encoder) };
    if message.is_null() {
        return String::new();
    }

    // SAFETY: non-null and nul-terminated, per the call above.
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_versions_match_the_header_macros() {
        // Transcribed constants are the one part of an FFI binding a compiler
        // cannot check, and a wrong one produces NV_ENC_ERR_INVALID_VERSION
        // from a call several steps away from the mistake. These are the
        // arithmetic in `NVENCAPI_STRUCT_VERSION` done by hand: interface 12.0
        // is 0x0000000C, the revision goes in bits 16-23 and 0x7 in bits 28-31.
        assert_eq!(API_VERSION, 0x0000_000C);
        assert_eq!(FUNCTION_LIST_VER, 0x7002_000C);
        assert_eq!(CONFIG_VER, 0xF008_000C);
        assert_eq!(INITIALIZE_PARAMS_VER, 0xF005_000C);
        assert_eq!(PIC_PARAMS_VER, 0xF006_000C);
        assert_eq!(LOCK_BITSTREAM_VER, 0x7002_000C);
        assert_eq!(REGISTER_RESOURCE_VER, 0x7004_000C);
    }

    #[test]
    fn a_load_failure_names_the_entry_point_that_failed() {
        // The reader of a bug report goes looking for the function named in it.
        // Naming `NvEncodeAPICreateInstance` for a version query that never got
        // as far as creating an instance sends them to the wrong place
        // (AGENTS.md section 15).
        assert!(
            LoadFailure::VersionQuery {
                status: sys::NV_ENC_ERR_INVALID_VERSION,
            }
            .to_string()
            .starts_with("NvEncodeAPIGetMaxSupportedVersion failed"),
            "{}",
            LoadFailure::VersionQuery {
                status: sys::NV_ENC_ERR_INVALID_VERSION
            }
        );
        assert!(
            LoadFailure::CreateInstance {
                status: sys::NV_ENC_ERR_INVALID_VERSION,
            }
            .to_string()
            .starts_with("NvEncodeAPICreateInstance failed"),
            "{}",
            LoadFailure::CreateInstance {
                status: sys::NV_ENC_ERR_INVALID_VERSION
            }
        );
        assert_eq!(
            LoadFailure::MissingEntryPoint {
                name: "NvEncodeAPICreateInstance",
            }
            .to_string(),
            "the library does not export NvEncodeAPICreateInstance"
        );
    }

    #[test]
    fn every_documented_status_has_a_name() {
        // A bug report that says "NVENC failed with 23" is worth much less than
        // one that says NV_ENC_ERR_RESOURCE_REGISTER_FAILED, and the mapping is
        // the sort of table that rots silently.
        for status in 0..=25 {
            assert_ne!(
                status_name(status),
                "an unrecognised NVENC status",
                "status {status} is documented in interface 12.0 and has no name here"
            );
        }
        assert_eq!(status_name(9999), "an unrecognised NVENC status");
    }
}
