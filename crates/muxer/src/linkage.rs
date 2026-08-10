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

use std::ffi::CStr;
use std::fmt;

use ffmpeg_the_third as ffmpeg;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedBuild {
    /// FFmpeg's own name for the build, such as `n8.1.2-34-g9b6c8969e0-20260809`.
    ///
    /// This is the release tag, the commit it was built from and whatever the
    /// packager appended, so it identifies the artefact far more precisely than
    /// the library versions do.
    pub identifier: &'static str,
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
    pub configuration: &'static str,
    /// The licence FFmpeg reports for itself, such as `LGPL version 3 or later`.
    pub license: &'static str,
}

impl fmt::Display for LinkedBuild {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "FFmpeg {} ({}); libavformat {}, libavcodec {}, libavutil {}",
            self.identifier, self.license, self.avformat, self.avcodec, self.avutil
        )
    }
}

/// Reports the FFmpeg libraries this process has loaded.
pub fn linked_build() -> LinkedBuild {
    LinkedBuild {
        identifier: build_identifier(),
        avutil: LibraryVersion::from_packed(ffmpeg::util::version()),
        avcodec: LibraryVersion::from_packed(ffmpeg::codec::version()),
        avformat: LibraryVersion::from_packed(ffmpeg::format::version()),
        configuration: ffmpeg::util::configuration(),
        license: ffmpeg::util::license(),
    }
}

/// Reads `av_version_info()`, which the binding does not wrap.
fn build_identifier() -> &'static str {
    // SAFETY: `av_version_info` takes no arguments, touches no state and
    // returns a pointer to a string constant compiled into libavutil, so it is
    // never null and outlives the process. The bytes are ASCII in every FFmpeg
    // build, but a lossy conversion is used rather than assuming it, because
    // the string includes a packager-supplied suffix.
    let version = unsafe { CStr::from_ptr(ffmpeg::ffi::av_version_info()) };
    version.to_str().unwrap_or("unknown")
}

/// Reports whether the loaded build can write the named container.
///
/// The name is FFmpeg's muxer name, as `ffmpeg -muxers` lists it: `matroska`,
/// `mp4`. Some muxers register under several comma-separated names, so each is
/// compared in turn.
pub fn muxer_available(name: &str) -> bool {
    ffmpeg::format::format::list_muxers()
        .any(|muxer| muxer.name().split(',').any(|alias| alias == name))
}

/// Reports whether the loaded build can decode the named codec.
///
/// The name is FFmpeg's decoder name, as `ffmpeg -decoders` lists it: `h264`,
/// `hevc`, `av1`.
pub fn decoder_available(name: &str) -> bool {
    ffmpeg::decoder::find_by_name(name).is_some()
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
    ffmpeg::encoder::find_by_name(name).is_some()
}
