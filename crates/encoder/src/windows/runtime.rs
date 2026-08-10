//! Whether each vendor's encoder runtime is installed and will load.
//!
//! This is the measurement that most closely matches what encoding will
//! actually do: issues #15 to #17 encode through these libraries, so a library
//! that will not load is an encoder that will not work, whatever else the
//! system says about it.
//!
//! Loading is the measurement, not the file existing. A driver that was half
//! removed, or one whose files are present for the wrong architecture, leaves
//! the library on disk and unusable, and reporting an encoder as available on
//! the strength of a directory listing is exactly the sort of answer that turns
//! into a failed recording later.
//!
//! # Security
//!
//! Every library here is a system component, and each is loaded with
//! `LOAD_LIBRARY_SEARCH_SYSTEM32` so that the search order cannot be diverted
//! by a file of the same name in the working directory or anywhere else on the
//! path (AGENTS.md section 13). The handle is released immediately: the probe
//! wants to know whether the loader succeeds, and nothing is called through it.

use windows::core::{Error, PCWSTR};
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

use crate::codec::EncoderKind;
use crate::probe::{RuntimeObservation, RuntimeOutcome};

/// The libraries each encoder family is reached through.
///
/// Quick Sync has several names because Intel's media stack was renamed twice
/// and the driver's file names followed, and the list is not written out here:
/// it is [`quicksync::LIBRARIES`](crate::windows::quicksync::LIBRARIES), the
/// same constant the backend loads from. Detection reporting a family
/// unavailable while the backend would have opened it — or the reverse — is
/// what two hand-maintained copies of a file-name list eventually produce, and
/// "Automatic" silently skipping an encoder that works is the shape that bug
/// would take.
///
/// **Unverified on real hardware.** There is no Intel GPU on the machine this
/// was written on, so the Quick Sync rows have never returned
/// [`RuntimeOutcome::Loaded`] here — only `NotFound`, which is the correct
/// answer for this machine and tells us nothing about a machine where the
/// library exists (issue #14).
pub(super) const LIBRARIES: &[(EncoderKind, &[&str])] = &[
    (EncoderKind::Nvenc, &["nvEncodeAPI64.dll"]),
    (EncoderKind::Amf, &["amfrt64.dll"]),
    (EncoderKind::QuickSync, super::quicksync::LIBRARIES),
];

/// Windows error for a file that is not there.
const ERROR_FILE_NOT_FOUND: u32 = 2;

/// Windows error for a module that is not there, which is what the loader
/// reports for a missing DLL.
const ERROR_MOD_NOT_FOUND: u32 = 126;

/// Tries to load one library, and says what happened.
pub(super) fn observe(kind: EncoderKind, library: &str) -> RuntimeObservation {
    RuntimeObservation::new(kind, library, load(library))
}

/// Loads and immediately releases `library`.
fn load(library: &str) -> RuntimeOutcome {
    let wide: Vec<u16> = library.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: `wide` is a nul-terminated UTF-16 string that outlives the call,
    // the reserved handle parameter is `None` as the documentation requires,
    // and the search flag is one of the documented `LOAD_LIBRARY_SEARCH_*`
    // values. The returned module is released below and never used for
    // anything else, so no pointer into it escapes.
    let module =
        unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32) };

    match module {
        Ok(handle) => {
            // SAFETY: `handle` came from a successful `LoadLibraryExW` in this
            // function and has not been freed or copied anywhere else, so this
            // is the one and only release of that reference.
            if let Err(error) = unsafe { FreeLibrary(handle) } {
                // A library that loads and will not unload is not a reason to
                // report the encoder as missing — it is there — but it is odd
                // enough to be worth a line (AGENTS.md section 15).
                tracing::debug!(
                    library,
                    %error,
                    "an encoder runtime was loaded and could not be released"
                );
            }
            RuntimeOutcome::Loaded
        }
        Err(error) => match win32_code(&error) {
            Some(ERROR_FILE_NOT_FOUND | ERROR_MOD_NOT_FOUND) | None => RuntimeOutcome::NotFound,
            Some(code) => RuntimeOutcome::FailedToLoad { code },
        },
    }
}

/// The Win32 error code inside an `HRESULT` that was built from one.
///
/// `windows` reports loader failures as `HRESULT`s, and the Win32 facility
/// encoding — `0x8007xxxx` — is the documented way back to the original code.
/// Anything else is some other kind of failure and is not decoded.
fn win32_code(error: &Error) -> Option<u32> {
    let hresult = error.code().0 as u32;
    (hresult & 0xFFFF_0000 == 0x8007_0000).then_some(hresult & 0xFFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_that_does_not_exist_is_reported_as_missing() {
        // Not an assertion about any GPU: no machine has a
        // `clipped-no-such-encoder.dll`, so this exercises the failure path
        // without depending on what is installed.
        assert_eq!(
            load("clipped-no-such-encoder-runtime.dll"),
            RuntimeOutcome::NotFound
        );
    }

    #[test]
    fn a_system_library_that_exists_everywhere_loads() {
        // The other half of the same path: `kernel32.dll` is present on every
        // Windows installation, so a probe that cannot load it is broken in a
        // way that has nothing to do with encoders.
        assert_eq!(load("kernel32.dll"), RuntimeOutcome::Loaded);
    }

    #[test]
    fn every_encoder_family_that_needs_a_runtime_has_one_listed() {
        for kind in EncoderKind::ALL {
            let listed = LIBRARIES
                .iter()
                .any(|(listed, libraries)| *listed == kind && !libraries.is_empty());
            assert_eq!(
                listed,
                kind.is_hardware(),
                "{kind} should have a runtime listed if and only if it is hardware"
            );
        }
    }

    #[test]
    fn detection_probes_exactly_the_libraries_the_quick_sync_backend_loads() {
        // The two lists were once written out separately, and the day they
        // disagree is the day a machine is told Quick Sync is unavailable while
        // the backend would have opened it — or worse, the reverse. They are
        // one constant now, and this is what says so out loud.
        let probed: Vec<&str> = LIBRARIES
            .iter()
            .filter(|(kind, _)| *kind == EncoderKind::QuickSync)
            .flat_map(|(_, libraries)| libraries.iter().copied())
            .collect();

        assert_eq!(probed, crate::windows::quicksync::LIBRARIES);
    }
}
