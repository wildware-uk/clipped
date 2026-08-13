//! What the recorder a user installs asks Windows for, read out of the binary
//! itself.
//!
//! [Issue #407](https://github.com/wildware-uk/clipped/issues/407) is a failure
//! that cannot be reproduced on a machine that has ever had Visual Studio, a
//! game, or almost any other native application installed: `clipped-recorder.exe`
//! imported `VCRUNTIME140.dll`, which belongs to the Microsoft Visual C++
//! 2015-2022 redistributable rather than to Windows, and on a machine without it
//! the recorder was killed by the loader with `STATUS_DLL_NOT_FOUND` before
//! `main` ran. Every developer machine has the redistributable, so "it works
//! here" says nothing at all.
//!
//! What *can* be checked anywhere is the question the loader will ask: which
//! libraries the executable imports. These tests read the PE import directory of
//! the binary cargo just built and assert both halves of
//! [ADR 0007](../../../docs/adr/0007-visual-c-runtime-linkage.md):
//!
//! 1. nothing from the redistributable is imported, so there is nothing to
//!    install;
//! 2. the universal CRT still *is* imported, so the recorder and the FFmpeg
//!    libraries continue to allocate from one heap.
//!
//! The second is the one that stops the decision being quietly undone. `-C
//! target-feature=+crt-static` also satisfies the first, is one flag rather than
//! eleven, and is what a contributor reaching for the obvious fix would write —
//! and it gives the recorder a private heap in the one process whose work is
//! passing buffers to and from FFmpeg (ADR 0007, "Alternatives"). A test that
//! only checked for the absence of `VCRUNTIME140.dll` would pass on it.
//!
//! Both tests read `clipped-recorder.exe`, the artefact the installer carries
//! ([docs/packaging.md](../../../docs/packaging.md)), rather than this test
//! binary. `apps/recorder/build.rs` links every target of the package the same
//! way, so the two agree; asserting against the shipped one is what makes the
//! claim about the shipped one.

#![cfg(windows)]

use std::path::Path;

/// The libraries the Visual C++ redistributable installs, lower-cased.
///
/// The version number is part of the file name and moves with the toolset —
/// `VCRUNTIME140.dll` today, `VCRUNTIME150.dll` on a future one — so these are
/// prefixes rather than whole names. `msvcr*` catches the pre-2015 runtimes,
/// which nothing here should reach but which would be the same class of failure
/// if a dependency ever dragged one in.
const REDISTRIBUTABLE_PREFIXES: &[&str] = &[
    "vcruntime",
    "msvcp",
    "msvcr",
    "concrt",
    "vccorlib",
    "vcamp",
    "vcomp",
];

/// The universal CRT's heap, which is where `malloc` and `free` come from.
///
/// Named specifically rather than as "any `api-ms-win-crt-` import" because the
/// heap is the one that matters: `dumpbin /IMPORTS` on the pinned FFmpeg build
/// shows `avutil-60.dll` importing `malloc`, `free`, `realloc` and `calloc` from
/// this same library, and that shared `ucrtbase.dll` is what makes a buffer
/// allocated on one side of the FFI safe to release on the other.
const UNIVERSAL_CRT_HEAP: &str = "api-ms-win-crt-heap-l1-1-0.dll";

#[test]
fn the_installed_recorder_imports_nothing_from_the_visual_c_redistributable() {
    let recorder = Path::new(env!("CARGO_BIN_EXE_clipped-recorder"));
    let imports = imported_libraries(recorder);

    let redistributable: Vec<&String> = imports
        .iter()
        .filter(|library| {
            REDISTRIBUTABLE_PREFIXES
                .iter()
                .any(|prefix| library.to_ascii_lowercase().starts_with(prefix))
        })
        .collect();

    assert!(
        redistributable.is_empty(),
        "the recorder imports {redistributable:?}, which Windows does not carry: on a machine \
         without the Microsoft Visual C++ redistributable it will be killed by the loader before \
         `main`, with nothing logged. See docs/adr/0007-visual-c-runtime-linkage.md and \
         apps/recorder/build.rs.\n\nEverything it imports:\n{imports:#?}"
    );
}

#[test]
fn the_recorder_shares_the_operating_system_heap_with_ffmpeg() {
    let recorder = Path::new(env!("CARGO_BIN_EXE_clipped-recorder"));
    let imports = imported_libraries(recorder);

    assert!(
        imports
            .iter()
            .any(|library| library.eq_ignore_ascii_case(UNIVERSAL_CRT_HEAP)),
        "the recorder no longer imports {UNIVERSAL_CRT_HEAP}, so it has a C runtime heap of its \
         own while the FFmpeg libraries beside it still allocate from ucrtbase.dll. Memory \
         allocated by one and released by the other is corruption that appears as a crash \
         somewhere else entirely. `-C target-feature=+crt-static` is the usual way to arrive \
         here; docs/adr/0007-visual-c-runtime-linkage.md says why it was not \
         taken.\n\nEverything it imports:\n{imports:#?}"
    );
}

/// Every library named in `binary`'s import directory.
///
/// A deliberately small PE reader rather than a dependency: this walks one table
/// in a file format that has not changed since 1993, it runs in a test, and the
/// alternatives on crates.io are either whole object-file abstractions or would
/// have to clear `deny.toml`'s licence allow-list for the sake of forty lines
/// (AGENTS.md section 10).
///
/// It panics on anything it does not understand, because every input it is given
/// is a binary this build just produced: a file it cannot parse is a broken
/// build, not a case to handle.
fn imported_libraries(binary: &Path) -> Vec<String> {
    let image = std::fs::read(binary)
        .unwrap_or_else(|error| panic!("{} could not be read: {error}", binary.display()));

    assert_eq!(&image[..2], b"MZ", "{} is not a PE image", binary.display());

    // The DOS stub ends with the offset of the real header, at a fixed place.
    let pe = u32_at(&image, 0x3C) as usize;
    assert_eq!(
        &image[pe..pe + 4],
        b"PE\0\0",
        "{} has no PE signature where its DOS header points",
        binary.display()
    );

    // COFF header: 20 bytes, of which two fields are needed — how many sections
    // follow the optional header, and how long the optional header is.
    let section_count = u16_at(&image, pe + 6) as usize;
    let optional_header_size = u16_at(&image, pe + 20) as usize;
    let optional_header = pe + 24;

    // Only PE32+ is expected: Clipped is x64-only (SPEC.md section 3), and the
    // data directories sit at a different offset in a 32-bit image, so guessing
    // would read the wrong table rather than fail.
    assert_eq!(
        u16_at(&image, optional_header),
        0x20B,
        "{} is not a 64-bit image",
        binary.display()
    );

    // Data directory 1 is the import table. The directories begin at 112 bytes
    // into a PE32+ optional header and are eight bytes each: an address, then a
    // size.
    let import_directory_rva = u32_at(&image, optional_header + 112 + 8) as usize;
    if import_directory_rva == 0 {
        return Vec::new();
    }

    let sections: Vec<Section> = (0..section_count)
        .map(|index| {
            let header = optional_header + optional_header_size + index * 40;
            Section {
                virtual_address: u32_at(&image, header + 12) as usize,
                size: u32_at(&image, header + 8).max(u32_at(&image, header + 16)) as usize,
                raw_offset: u32_at(&image, header + 20) as usize,
            }
        })
        .collect();

    let mut libraries = Vec::new();
    let mut descriptor = file_offset(&sections, import_directory_rva, binary);
    loop {
        // The table ends with a descriptor of twenty zero bytes rather than with
        // a count, so the terminator is what stops this.
        let name_rva = u32_at(&image, descriptor + 12) as usize;
        if name_rva == 0 {
            break;
        }

        let start = file_offset(&sections, name_rva, binary);
        let end = start
            + image[start..]
                .iter()
                .position(|byte| *byte == 0)
                .expect("a library name is NUL-terminated");
        libraries.push(String::from_utf8_lossy(&image[start..end]).into_owned());

        descriptor += 20;
    }

    libraries
}

/// One section of the image, as much of it as translating an address needs.
#[derive(Debug)]
struct Section {
    /// Where the section is once Windows has mapped it.
    virtual_address: usize,
    /// How far it extends, in memory or on disk, whichever is longer.
    size: usize,
    /// Where the same bytes are in the file.
    raw_offset: usize,
}

/// Where a mapped address is in the file on disk.
///
/// The import directory records addresses as the loader will see them, and
/// nothing here maps the image, so every one of them has to be walked back
/// through the section that will contain it.
fn file_offset(sections: &[Section], rva: usize, binary: &Path) -> usize {
    sections
        .iter()
        .find(|section| {
            rva >= section.virtual_address && rva < section.virtual_address + section.size
        })
        .map(|section| rva - section.virtual_address + section.raw_offset)
        .unwrap_or_else(|| {
            panic!(
                "{} has no section containing address {rva:#x}",
                binary.display()
            )
        })
}

/// A little-endian `u16` at `offset`.
fn u16_at(image: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        image[offset..offset + 2]
            .try_into()
            .expect("a two-byte slice"),
    )
}

/// A little-endian `u32` at `offset`.
fn u32_at(image: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        image[offset..offset + 4]
            .try_into()
            .expect("a four-byte slice"),
    )
}
