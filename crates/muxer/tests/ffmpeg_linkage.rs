//! Proves that the FFmpeg link works, and that it links what we intended.
//!
//! These tests load the FFmpeg libraries and call into them, so a failure here
//! is a real failure of the link rather than of a compile-time check. Three
//! separate things are being defended, and they fail independently:
//!
//! 1. **The libraries are the pinned build.** Dynamic linking means the DLLs
//!    beside the executable can be swapped, which is the LGPL permission this
//!    project relies on and therefore also a way to end up running something
//!    nobody chose. `scripts/fetch-ffmpeg.ps1` pins the artefact by checksum;
//!    these expectations pin what that artefact reports about itself.
//! 2. **The build is LGPL, not GPL.** Clipped is MPL-2.0 and distributes
//!    binaries. A GPL FFmpeg would make that impossible, and the difference
//!    between the two builds is invisible at the API surface — same versions,
//!    same functions, more codecs. It is visible in the `configure` line, so
//!    that is what is asserted.
//! 3. **The build contains what later milestones need.** An LGPL build is a
//!    smaller build, and the components it drops are load-bearing elsewhere:
//!    Matroska for the recording container (ADR 0001), a permissively licensed
//!    software encoder for the fallback path
//!    ([issue #18](https://github.com/wildware-uk/clipped/issues/18)).
//!
//! When the pin moves, these constants move with it. `docs/ffmpeg.md` describes
//! the whole procedure; the fetch script's mismatch message points here too.

use clipped_muxer::linkage::{
    decoder_available, encoder_available, linked_build, muxer_available, LibraryVersion,
};

/// `av_version_info()` of the pinned build: the FFmpeg release tag, the commit
/// it was built from, and the packager's date suffix.
const PINNED_IDENTIFIER: &str = "n8.1.2-34-g9b6c8969e0-20260809";

/// What the pinned build reports as its own licence. Not `GPL`, and not
/// `LGPL version 2.1 or later` either: the build enables components that are
/// LGPL v3 only, so v3 is the licence the distribution has to satisfy.
const PINNED_LICENCE: &str = "LGPL version 3 or later";

const PINNED_AVUTIL: LibraryVersion = LibraryVersion {
    major: 60,
    minor: 26,
    micro: 102,
};
const PINNED_AVCODEC: LibraryVersion = LibraryVersion {
    major: 62,
    minor: 28,
    micro: 102,
};
const PINNED_AVFORMAT: LibraryVersion = LibraryVersion {
    major: 62,
    minor: 12,
    micro: 102,
};

#[test]
fn loaded_libraries_are_the_pinned_ffmpeg_build() {
    let build = linked_build();

    assert_eq!(
        build.identifier, PINNED_IDENTIFIER,
        "linked against a different FFmpeg to the pinned one. Loaded: {build}"
    );
    assert_eq!(build.avutil, PINNED_AVUTIL, "libavutil version");
    assert_eq!(build.avcodec, PINNED_AVCODEC, "libavcodec version");
    assert_eq!(build.avformat, PINNED_AVFORMAT, "libavformat version");
}

#[test]
fn pinned_build_carries_no_gpl_only_components() {
    let build = linked_build();

    assert_eq!(
        build.licence, PINNED_LICENCE,
        "FFmpeg reports a licence Clipped cannot distribute against. Loaded: {build}"
    );

    let configuration = &build.configuration;
    for gpl_flag in ["--enable-gpl", "--enable-nonfree"] {
        assert!(
            !configuration.contains(gpl_flag),
            "the linked FFmpeg was configured with {gpl_flag}, which cannot be \
             distributed inside an MPL-2.0 application. Configuration: {configuration}"
        );
    }

    // x264 and x265 are the two GPL components anyone reaching for a software
    // H.264 or HEVC encoder would try first, so their explicit absence is the
    // thing most worth pinning down. `configure` records them as disabled here
    // because the builder passes the flags rather than merely omitting them.
    for gpl_library in ["--disable-libx264", "--disable-libx265"] {
        assert!(
            configuration.contains(gpl_library),
            "expected the linked FFmpeg to be built with {gpl_library}. \
             Configuration: {configuration}"
        );
    }
}

#[test]
fn pinned_build_provides_the_components_later_milestones_need() {
    assert!(
        muxer_available("matroska"),
        "MKV is the archival recording container (ADR 0001)"
    );
    assert!(
        muxer_available("mp4"),
        "sharing a recording means remuxing to MP4 (issue #92)"
    );

    // Thumbnails and the waveform have to decode what was recorded, and what
    // was recorded is whichever of these the machine's hardware encoder
    // produced (SPEC.md section 9).
    for codec in ["h264", "hevc", "av1"] {
        assert!(
            decoder_available(codec),
            "no {codec} decoder, so recordings in that codec could not be \
             thumbnailed or scrubbed"
        );
    }

    // The software encoder fallback (issue #18) cannot be libx264 in an
    // LGPL-only build. These two are the permissively licensed replacements the
    // ADR names, and this is the assertion that keeps them available if the pin
    // moves.
    for encoder in ["libopenh264", "libsvtav1"] {
        assert!(
            encoder_available(encoder),
            "no {encoder}, which issue #18's software fallback depends on"
        );
    }
}

#[test]
fn component_probes_report_absence_as_well_as_presence() {
    // Without this, a probe that had degenerated into `true` would sail through
    // every assertion above.
    assert!(!muxer_available("clipped-not-a-real-muxer"));
    assert!(!decoder_available("clipped-not-a-real-decoder"));
    assert!(!encoder_available("clipped-not-a-real-encoder"));

    // A handful of FFmpeg components register under several comma-separated
    // names, and `stream_segment,ssegment` is the one such muxer in this build.
    // Looking it up by its second name is what proves the probe splits aliases
    // rather than comparing the whole field.
    assert!(muxer_available("ssegment"));

    // The wrappers take a Rust `&str`, which may contain a NUL that a C lookup
    // could not carry. Answering "no such component" is the only honest result;
    // truncating at the NUL would silently answer a different question, and
    // `matroska` is the name that would be found if it did.
    assert!(!muxer_available("matroska\0extra"));
    assert!(!decoder_available("h264\0extra"));
    assert!(!encoder_available("libopenh264\0extra"));
}
