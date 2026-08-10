//! Which FFmpeg this process is linked against, and what that build contains.
//!
//! Clipped does not build FFmpeg. It links dynamically against a prebuilt,
//! LGPL-only build pinned by `scripts/fetch-ffmpeg.ps1`, for the reasons in
//! `docs/adr/0004-ffmpeg-dependency-strategy.md`. Dynamic linking is what makes
//! the LGPL relinking permission real, and it is also what makes "which FFmpeg
//! is actually loaded?" a question with a run-time answer rather than a
//! compile-time one: the DLL beside the executable can be replaced, which is
//! the point.
//!
//! So the answer is worth being able to ask for. These functions report the
//! loaded libraries and probe the build for the components later milestones
//! depend on, which makes three otherwise silent failures loud: linking against
//! a different FFmpeg to the pinned one, linking against a build that omits
//! something the pipeline needs, and linking against a GPL build, which would
//! make the binaries Clipped distributes undistributable under MPL-2.0.
//!
//! # This module is also the first of the FFmpeg wrappers
//!
//! `rusty_ffmpeg` is a `-sys` crate: it links the libraries and generates the
//! FFI, and stops there. Every safe abstraction over FFmpeg in Clipped is
//! written here in `clipped-muxer` (ADR 0004), starting with this module. What
//! it wraps is deliberately only what it needs — six entry points that read
//! constants out of the loaded libraries and three that look a component up by
//! name. The container writer that needs `AVFormatContext`, `AVStream` and
//! `AVPacket` wrapped is [issue #21](https://github.com/wildware-uk/clipped/issues/21),
//! and those wrappers are written when there is a muxer to hold them honest.
//!
//! Everything below is free of ownership questions, which is why it is
//! straightforward: FFmpeg's version, configuration and licence strings are
//! constants compiled into the libraries, and the descriptors reached by
//! `av_muxer_iterate` and `avcodec_find_*_by_name` are statics inside them.
//! Nothing here allocates on FFmpeg's side, so nothing here has to be freed.

use std::borrow::Cow;
use std::ffi::{c_char, c_void, CStr, CString};
use std::fmt;
use std::ptr;

use rusty_ffmpeg::ffi;

/// The version of one libav\* library, as FFmpeg's three components.
///
/// FFmpeg packs these into a single integer, one byte each. The major number is
/// the one that matters for compatibility: it changes when the ABI does, which
/// is why the DLL file names carry it (`avformat-62.dll`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LibraryVersion {
    /// Incremented on an ABI break. Part of the DLL file name.
    pub major: u32,
    /// Incremented when an API is added.
    pub minor: u32,
    /// Incremented for changes with no API effect. FFmpeg's own builds number
    /// this from 100 upwards to distinguish themselves from Libav's.
    pub micro: u32,
}

impl LibraryVersion {
    /// Unpacks the integer form returned by the `*_version()` entry points.
    fn from_packed(packed: u32) -> Self {
        Self {
            major: packed >> 16,
            minor: (packed >> 8) & 0xff,
            micro: packed & 0xff,
        }
    }
}

impl fmt::Display for LibraryVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.micro)
    }
}

/// A description of the FFmpeg libraries currently loaded.
///
/// Worth logging once at start-up: when a recording goes wrong on someone
/// else's machine, "which FFmpeg was this?" is among the first things to
/// establish, and the DLLs are replaceable by design.
///
/// The string fields are borrowed from the loaded libraries in the ordinary
/// case and owned only when a build reports something that is not valid UTF-8,
/// which no FFmpeg build has any reason to do — the fields are ASCII in every
/// one seen so far. Reporting is not worth a panic or a lost message either
/// way, so the invalid bytes are replaced rather than rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedBuild {
    /// FFmpeg's own name for the build, such as `n8.1.2-34-g9b6c8969e0-20260809`.
    ///
    /// This is the release tag, the commit it was built from and whatever the
    /// packager appended, so it identifies the artefact far more precisely than
    /// the library versions do.
    pub identifier: Cow<'static, str>,
    /// Version of the loaded libavutil.
    pub avutil: LibraryVersion,
    /// Version of the loaded libavcodec.
    pub avcodec: LibraryVersion,
    /// Version of the loaded libavformat.
    pub avformat: LibraryVersion,
    /// The `configure` arguments the build was produced with.
    ///
    /// This is where the licence position is visible: a build carrying GPL-only
    /// components such as libx264 says so here.
    pub configuration: Cow<'static, str>,
    /// The licence FFmpeg reports for itself, such as `LGPL version 3 or later`.
    pub licence: Cow<'static, str>,
}

impl fmt::Display for LinkedBuild {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "FFmpeg {} ({}); libavformat {}, libavcodec {}, libavutil {}",
            self.identifier, self.licence, self.avformat, self.avcodec, self.avutil
        )
    }
}

/// Reports the FFmpeg libraries this process has loaded.
pub fn linked_build() -> LinkedBuild {
    // SAFETY: each of these takes no arguments, reads no state and returns
    // either a packed integer or a pointer to a string constant compiled into
    // the library. `av_version_info`, `avutil_configuration` and
    // `avutil_license` are documented to return a static string, so the
    // pointers are non-null, NUL-terminated and outlive the process, which is
    // what `borrow_static_c_str` requires.
    unsafe {
        LinkedBuild {
            identifier: borrow_static_c_str(ffi::av_version_info()),
            avutil: LibraryVersion::from_packed(ffi::avutil_version()),
            avcodec: LibraryVersion::from_packed(ffi::avcodec_version()),
            avformat: LibraryVersion::from_packed(ffi::avformat_version()),
            configuration: borrow_static_c_str(ffi::avutil_configuration()),
            licence: borrow_static_c_str(ffi::avutil_license()),
        }
    }
}

/// Reports whether the loaded build can write the named container.
///
/// The name is FFmpeg's muxer name, as `ffmpeg -muxers` lists it: `matroska`,
/// `mp4`. Some muxers register under several comma-separated names, so each is
/// compared in turn.
pub fn muxer_available(name: &str) -> bool {
    let mut cursor: *mut c_void = ptr::null_mut();

    loop {
        // SAFETY: `av_muxer_iterate` is the supported way to walk the registered
        // muxers. `cursor` starts null, as it requires, and is only ever passed
        // back to the same function; it is a plain iteration token, not a
        // resource, so there is nothing to release when the loop ends early. The
        // return value is either null, ending the walk, or a pointer to an
        // `AVOutputFormat` that is a static inside libavformat and so remains
        // valid for the process.
        let format = unsafe { ffi::av_muxer_iterate(&mut cursor) };
        if format.is_null() {
            return false;
        }

        // SAFETY: `format` is non-null and points at a live `AVOutputFormat`, as
        // above. Its `name` field is a static string constant belonging to the
        // same descriptor.
        let names = unsafe { borrow_static_c_str((*format).name) };
        if names.split(',').any(|alias| alias == name) {
            return true;
        }
    }
}

/// Reports whether the loaded build can decode the named codec.
///
/// The name is FFmpeg's decoder name, as `ffmpeg -decoders` lists it: `h264`,
/// `hevc`, `av1`.
pub fn decoder_available(name: &str) -> bool {
    let Some(name) = as_c_string(name) else {
        return false;
    };

    // SAFETY: `avcodec_find_decoder_by_name` reads the NUL-terminated string it
    // is given and returns either null or a pointer to a static `AVCodec`. The
    // pointer is only compared against null here, and `name` outlives the call.
    !unsafe { ffi::avcodec_find_decoder_by_name(name.as_ptr()) }.is_null()
}

/// Reports whether the loaded build can encode with the named encoder.
///
/// The name is FFmpeg's encoder name, as `ffmpeg -encoders` lists it, so it
/// identifies an implementation rather than a codec: `h264_nvenc`,
/// `libopenh264`, `libsvtav1`.
///
/// Encoding is `clipped-encoder`'s job, not this crate's. What lives here is
/// the question "does the FFmpeg we pinned contain it?", which is a property of
/// the linked build and matters to the licence position: the software fallback
/// has to be a permissively licensed encoder, because an LGPL-only build has no
/// libx264 in it (see `docs/adr/0004-ffmpeg-dependency-strategy.md`).
pub fn encoder_available(name: &str) -> bool {
    let Some(name) = as_c_string(name) else {
        return false;
    };

    // SAFETY: as for `decoder_available`.
    !unsafe { ffi::avcodec_find_encoder_by_name(name.as_ptr()) }.is_null()
}

/// Borrows a string constant belonging to one of the loaded FFmpeg libraries.
///
/// Invalid UTF-8 is replaced rather than rejected: every one of these strings
/// is ASCII in practice, and the callers are reporting and probing, where a
/// replacement character in a message is a far better outcome than no message.
/// The result borrows the library's own bytes unless a replacement was needed.
///
/// # Safety
///
/// `pointer` must be non-null and point at a NUL-terminated string that lives
/// for the remainder of the process — an FFmpeg string constant, or a field of
/// a descriptor that is itself a static inside a loaded library.
unsafe fn borrow_static_c_str(pointer: *const c_char) -> Cow<'static, str> {
    // SAFETY: guaranteed by this function's own contract.
    let text = unsafe { CStr::from_ptr(pointer) };
    text.to_string_lossy()
}

/// Converts a component name into the NUL-terminated form FFmpeg looks up by.
///
/// `None` when the name contains an interior NUL, which no FFmpeg component
/// name can: the lookup would be answering a different question to the one
/// asked, so the honest answer is that no such component exists.
fn as_c_string(name: &str) -> Option<CString> {
    CString::new(name).ok()
}
