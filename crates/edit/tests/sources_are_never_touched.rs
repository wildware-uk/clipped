//! The promise the whole crate exists to keep: a recording is not changed
//! because somebody edited it.
//!
//! Two tests, asserting it two different ways.
//!
//! The first is the one [issue
//! #82](https://github.com/wildware-uk/clipped/issues/82) asks for: check a
//! file, put a document referring to it through everything this crate can do,
//! and check it again. It is the property a user cares about.
//!
//! The second is the reason the first cannot start failing quietly. This crate
//! reads and writes `String`s and never opens anything, so damaging a recording
//! is not merely something it does not do — it is something it has no means to
//! do. That is worth asserting directly, because a checksum test only covers
//! the paths it happens to call, and the first `std::fs::write` added to this
//! crate would be the moment that stopped being enough.

use std::fs;
use std::path::{Path, PathBuf};

use clipped_edit::{
    AudioTrack, CropRect, EditDocument, EditHistory, EditOperation, OutputSpan, OutputTime,
    OverlayPosition, RecordingId, Rotation, Segment, Source, SourceId, SourceSpan, SourceTime,
    Speed, TextOverlay, TrackInput,
};

/// A directory of this test's own, removed when it is dropped.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-edit-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory can be created");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// FNV-1a, 64-bit: a checksum in six lines rather than a dependency.
///
/// Cryptographic strength is irrelevant here — nothing is defending against a
/// forged recording, the question is only whether these exact bytes changed —
/// and the test also compares the file's whole contents, so the digest is the
/// evidence and the comparison is the belt.
fn checksum(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Bytes standing in for a recording.
///
/// Deliberately not a real MKV. This crate never opens the file, so its
/// contents cannot matter to the result — and a test that needed a real
/// recording would need a GPU, a game and several seconds, which is exactly the
/// kind of test that stops being run.
fn write_stand_in_recording(directory: &Path) -> PathBuf {
    let path = directory.join("2026-08-11 Counter-Strike 2.mkv");
    let bytes: Vec<u8> = (0..4096_u32)
        .map(|index| u8::try_from(index % 251).expect("a value under 251 fits in a byte"))
        .collect();
    fs::write(&path, &bytes).expect("the stand-in recording can be written");
    path
}

fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
    SourceSpan::new(
        SourceTime::from_nanos(start_nanos),
        SourceTime::from_nanos(end_nanos),
    )
    .expect("the span ends after it starts")
}

/// A document that uses every part of the model.
fn everything(recording: &Path) -> EditDocument {
    let first = SourceId::new(0);
    let second = SourceId::new(1);

    EditDocument::new("Ace")
        // A real recording identifier is the library's, not a path
        // (`clipped_edit::source`). The full path is used here deliberately
        // anyway: it is the harshest version of the question, because the
        // document then holds everything needed to open that file and still
        // does not open it.
        .with_source(Source::new(
            first,
            RecordingId::new(recording.display().to_string()),
        ))
        .with_source(Source::new(second, RecordingId::new("rec-2")))
        .with_segment(Segment::new(first, span(30_000_000_000, 38_000_000_000)))
        .with_segment(
            Segment::new(first, span(92_000_000_000, 104_000_000_000))
                .at_speed(Speed::new(2, 1).expect("a valid speed"))
                .cropped_to(CropRect::new(0.1, 0.1, 0.8, 0.8).expect("a valid crop"))
                .rotated(Rotation::Clockwise180),
        )
        .with_segment(Segment::new(second, span(5_000_000_000, 9_000_000_000)))
        .with_audio_track(
            AudioTrack::new(
                "Game",
                vec![TrackInput::new(first, 0), TrackInput::new(second, 0)],
            )
            .at_gain_db(-3.5),
        )
        .with_audio_track(AudioTrack::new("Microphone", vec![TrackInput::new(first, 1)]).muted())
        .with_overlay(
            TextOverlay::new(
                "Ace",
                OutputSpan::new(OutputTime::ZERO, OutputTime::from_nanos(3_000_000_000))
                    .expect("the span ends after it starts"),
            )
            .at(OverlayPosition::new(0.5, 0.8).expect("a valid position"))
            .sized(8),
        )
}

#[test]
fn editing_a_recording_leaves_the_recording_byte_for_byte_identical() {
    let directory = TestDirectory::new("checksum");
    let recording = write_stand_in_recording(&directory.0);

    let before = fs::read(&recording).expect("the recording can be read");
    let before_checksum = checksum(&before);

    // Everything this crate can be asked to do, with a document that names
    // that file: build it, validate it, save it, load it back, read the whole
    // timeline the way an export would, and save it again.
    let document = everything(&recording);
    document.validate().expect("the document is valid");
    let text = document.write().expect("the document saves");
    let loaded = EditDocument::read(&text).expect("the document loads");
    let duration = loaded
        .document
        .output_duration_nanos()
        .expect("a validated document has a duration");
    for step in 0..=100 {
        let at = OutputTime::from_nanos(duration * step / 100);
        let _ = loaded.document.locate(at);
        let _ = loaded.document.overlays_at(at).count();
    }
    let _ = loaded.document.write().expect("it saves again");

    // Including editing it. Trimming, splitting and deleting are what a user
    // would call "cutting the recording up", and they are the operations most
    // likely to be assumed to touch it: cut eighteen of this clip's twenty-two
    // seconds away, undo all of it, redo all of it, and save each result.
    let mut history = EditHistory::new(loaded.document);
    for operation in [
        EditOperation::Split {
            at: OutputTime::from_nanos(4_000_000_000),
        },
        EditOperation::DeleteSection {
            range: OutputSpan::new(
                OutputTime::from_nanos(6_000_000_000),
                OutputTime::from_nanos(14_000_000_000),
            )
            .expect("the range ends after it starts"),
        },
        EditOperation::TrimStart {
            at: OutputTime::from_nanos(2_000_000_000),
        },
        EditOperation::TrimEnd {
            at: OutputTime::from_nanos(4_000_000_000),
        },
    ] {
        assert!(
            history.apply(operation).expect("the operation applies"),
            "{operation:?} should have changed the document"
        );
        let _ = history.document().write().expect("an edited clip saves");
    }
    while history.undo() {
        let _ = history.document().write().expect("it saves");
    }
    while history.redo() {
        let _ = history.document().write().expect("it saves");
    }

    let after = fs::read(&recording).expect("the recording is still there");
    assert_eq!(
        checksum(&after),
        before_checksum,
        "the source recording's checksum changed while a clip was being made"
    );
    assert_eq!(after, before, "the source recording's bytes changed");
    assert_eq!(
        fs::metadata(&recording)
            .expect("the recording is still there")
            .len(),
        u64::try_from(before.len()).expect("the stand-in recording is four kilobytes")
    );
}

/// Every way of touching a file that this crate could reach for.
///
/// Names rather than behaviour, which is crude — and is the point: the check
/// has to fail on the *appearance* of file access, before anybody has to reason
/// about whether that particular access is safe.
const FILE_ACCESS: &[&str] = &[
    "std::fs",
    "fs::",
    "File::",
    "OpenOptions",
    "std::process",
    "Command::",
];

fn rust_sources(directory: &Path, into: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("the source directory can be read") {
        let path = entry.expect("the directory entry can be read").path();
        if path.is_dir() {
            rust_sources(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
}

#[test]
fn this_crate_has_no_way_to_open_a_file_at_all() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&source, &mut files);
    assert!(!files.is_empty(), "the crate's own source should be here");

    let mut findings = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).expect("a source file can be read");
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for access in FILE_ACCESS {
                if code.contains(access) {
                    findings.push(format!("{}:{}: {access}", file.display(), number + 1));
                }
            }
        }
    }

    assert!(
        findings.is_empty(),
        "this crate must not read, write or execute anything: an edit document is text \
         somebody else stores, and a model that cannot open a file cannot damage a \
         recording (AGENTS.md sections 56 and 57). Found: {findings:#?}"
    );
}
