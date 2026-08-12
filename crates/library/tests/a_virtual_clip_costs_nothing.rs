//! The promise the virtual clip model exists to keep: making a clip is instant
//! and adds no video data.
//!
//! Three tests, asserting it three different ways, because each one covers what
//! the others cannot.
//!
//! The first is the measurement [issue
//! #74](https://github.com/wildware-uk/clipped/issues/74) asks for: generate
//! ten thousand highlights over a three-hour recording and report what it cost.
//! It is the number a user experiences.
//!
//! The second is the filesystem's own answer. A timing test would still pass if
//! creating a clip wrote a small file, so a directory holding a stand-in
//! recording is compared byte for byte before and after — nothing appears, and
//! the recording is untouched.
//!
//! The third is why neither can start failing quietly. This module opens no
//! file at all, so writing media is not merely something it does not do; it is
//! something it has no means to do. The check is on the module's own source, so
//! it fails on the *appearance* of file access rather than waiting for a code
//! path a test happens to call. It is scoped to `virtual_clip.rs` rather than
//! to the crate, deliberately: indexing reconciles the database against what is
//! on disk ([issue #56](https://github.com/wildware-uk/clipped/issues/56)) and
//! will read files quite legitimately, in modules that are not this one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clipped_edit::RecordingId;
use clipped_events::{
    Confidence, EventKind, EventSource, EventTime, EventTiming, GameEvent, RecordedSpan,
};
use clipped_library::virtual_clip::{window_around, ClipOrigin, HighlightCause, VirtualClip};

/// Three hours of Counter-Strike, which is the length that makes the question
/// worth asking: a model that copied anything proportional to the material
/// would show it here.
const RECORDING: Duration = Duration::from_secs(3 * 60 * 60);

/// Ten thousand is more highlights than a session produces, on purpose. If the
/// per-clip cost is honest at this count it is not hiding a one-off.
const CLIPS: usize = 10_000;

/// The ceiling the first test asserts, per clip.
///
/// Deliberately loose — this runs in a debug build on a machine that may be
/// busy, and the assertion is not a benchmark of allocator performance. It is
/// two orders of magnitude above what the model actually costs, which is what
/// makes it a check that only fails when something has started doing real work
/// per clip: opening a file, decoding a frame, copying footage.
const CEILING_PER_CLIP: Duration = Duration::from_micros(100);

/// A directory of this test's own, removed when it is dropped.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-library-{label}-{}-{:?}",
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

fn kill_at(seconds: i64) -> GameEvent {
    GameEvent::new(
        EventKind::Kill,
        EventTiming::new(
            EventTime::from_media_nanos(seconds * 1_000_000_000),
            Duration::ZERO,
        ),
        EventSource::plugin("acme-cs2").expect("a well-formed plugin identifier"),
        Confidence::new(0.95).expect("a valid confidence"),
    )
}

/// What [#76](https://github.com/wildware-uk/clipped/issues/76) will do for one
/// event, with the window a rule handed it: fifteen seconds before a kill and
/// ten after.
fn highlight_of(
    event: &GameEvent,
    recording: &RecordingId,
    recorded: &RecordedSpan,
) -> VirtualClip {
    let window = window_around(
        event.timing().at(),
        Duration::from_secs(15),
        Duration::from_secs(10),
        recorded,
    )
    .expect("the recording covers the kill");

    VirtualClip::of_range(
        format!("{} at {}", event.kind(), event.timing().at()),
        recording.clone(),
        window,
        ClipOrigin::Highlight(HighlightCause::of(event)),
    )
    .with_tag(event.kind().as_str())
}

#[test]
fn ten_thousand_highlights_over_a_three_hour_recording_cost_no_measurable_time() {
    let recording = RecordingId::new("2026-08-11T20-14-03-cs2");
    let recorded = RecordedSpan::from_epoch(RECORDING);

    // The events, and the window arithmetic, are set up outside the
    // measurement only in the sense that the events themselves are: the timed
    // region includes turning each event into a range and building the clip,
    // because that is what generation does per event.
    let events: Vec<GameEvent> = (0..CLIPS)
        .map(|index| {
            let seconds = 20 + (index as i64 * 1_000) % (3 * 60 * 60 - 40);
            kill_at(seconds)
        })
        .collect();

    let started = Instant::now();
    let clips: Vec<VirtualClip> = events
        .iter()
        .map(|event| highlight_of(event, &recording, &recorded))
        .collect();
    let elapsed = started.elapsed();

    let per_clip = elapsed / u32::try_from(CLIPS).expect("ten thousand fits in a u32");
    println!("{CLIPS} virtual clips in {elapsed:?} ({per_clip:?} each)");

    assert_eq!(clips.len(), CLIPS);
    assert!(
        clips
            .iter()
            .all(|clip| clip.duration() == Some(Duration::from_secs(25))),
        "each clip should be the twenty-five second window the rule asked for"
    );
    assert!(
        per_clip < CEILING_PER_CLIP,
        "creating a virtual clip took {per_clip:?} each ({elapsed:?} for {CLIPS}), which is over \
         the {CEILING_PER_CLIP:?} a model that copies nothing should never approach"
    );
}

/// Every file in a directory, with its bytes.
fn contents(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(directory).expect("the directory can be read") {
        let path = entry.expect("the directory entry can be read").path();
        assert!(path.is_file(), "this test writes no directories");
        let bytes = fs::read(&path).expect("the file can be read");
        found.insert(path, bytes);
    }
    found
}

#[test]
fn creating_clips_adds_no_video_data_and_leaves_the_recording_alone() {
    let directory = TestDirectory::new("no-media");
    let path = directory.0.join("2026-08-11 Counter-Strike 2.mkv");
    // Not a real MKV: nothing here opens it, so its contents cannot matter to
    // the result, and a test needing a real recording would need a GPU and a
    // game — which is the kind of test that stops being run.
    let bytes: Vec<u8> = (0..8192_u32)
        .map(|index| u8::try_from(index % 251).expect("a value under 251 fits in a byte"))
        .collect();
    fs::write(&path, &bytes).expect("the stand-in recording can be written");
    let before = contents(&directory.0);

    // A path is the harshest form of the question: the clips hold everything
    // needed to open that file, and still do not open it.
    let recording = RecordingId::new(path.display().to_string());
    let recorded = RecordedSpan::from_epoch(RECORDING);
    let clips: Vec<VirtualClip> = (0..1_000)
        .map(|index| highlight_of(&kill_at(20 + index), &recording, &recorded))
        .collect();

    // And everything a caller does with them afterwards: read the timeline,
    // and write the document out as the text somebody else stores.
    let mut total_document_bytes = 0;
    for clip in &clips {
        clip.edit().validate().expect("a generated clip is valid");
        total_document_bytes += clip.edit().write().expect("the document saves").len();
    }

    let after = contents(&directory.0);
    assert_eq!(
        after, before,
        "creating a thousand clips changed what is on disk"
    );
    assert_eq!(
        after.get(&path).map(Vec::len),
        Some(bytes.len()),
        "the source recording's size changed"
    );

    // The whole cost of a thousand clips of a three-hour recording, as text.
    println!("1000 clip documents are {total_document_bytes} bytes of text in total");
    assert!(
        total_document_bytes < 1_000 * 1_024,
        "a virtual clip should be under a kilobyte of metadata, not media: \
         {total_document_bytes} bytes for a thousand"
    );
}

/// Every way of touching a file that this module could reach for.
///
/// Names rather than behaviour, which is crude — and is the point: the check
/// has to fail on the appearance of file access, before anybody has to reason
/// about whether that particular access is safe.
const FILE_ACCESS: &[&str] = &[
    "std::fs",
    "fs::",
    "File::",
    "OpenOptions",
    "std::process",
    "Command::",
];

#[test]
fn the_virtual_clip_model_has_no_way_to_open_a_file_at_all() {
    let module = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("virtual_clip.rs");
    let text = fs::read_to_string(&module).expect("the module's source can be read");

    let mut findings = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        for access in FILE_ACCESS {
            if code.contains(access) {
                findings.push(format!("{}:{}: {access}", module.display(), number + 1));
            }
        }
    }

    assert!(
        findings.is_empty(),
        "a virtual clip is metadata over a recording nobody opens: it costs nothing to create \
         because there is nothing to create (SPEC.md section 20). Found: {findings:#?}"
    );
}
