//! The promise automatic highlight generation exists to keep: it produces clips
//! a user can watch and it writes nothing at all.
//!
//! Three tests, because each one covers what the others cannot.
//!
//! The first is the measurement [issue
//! #76](https://github.com/wildware-uk/clipped/issues/76) asks for. Its
//! acceptance criterion is "no measurable frame-rate impact during gameplay
//! (measured), **or** generation is deferred", and this change takes the second
//! branch — generation runs over recordings a session has finished, on whatever
//! thread the caller likes, and never on the one that is capturing. The number
//! here is what makes that branch worth taking rather than a claim about a
//! machine nobody can check: a busy three-hour session's worth of events, and
//! the same run again over the clips it produced, which is the path a re-run
//! takes.
//!
//! The second is the filesystem's own answer. A timing test would still pass if
//! generation wrote a small file per clip, so a directory holding a stand-in
//! recording is compared byte for byte before and after a session's worth of
//! clips is generated in it.
//!
//! The third is why neither can start failing quietly. `generate.rs` opens no
//! file at all, so writing media is not merely something it does not do; it is
//! something it has no means to do. The check is on that module's own source,
//! so it fails on the *appearance* of file access rather than waiting for a code
//! path a test happens to call.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clipped_edit::RecordingId;
use clipped_events::{
    Confidence, EventKind, EventSource, EventTime, EventTiming, GameEvent, RecordedSpan,
};
use clipped_library::events::{RecordedSegment, SessionRecordings};
use clipped_library::virtual_clip::VirtualClip;
use clipped_session::config::Scope;
use clipped_session::highlights::{HighlightGeneration, HighlightRules, ResolvedHighlightRules};

/// Three hours of Counter-Strike.
const RECORDING_SECONDS: i64 = 3 * 60 * 60;
const RECORDING: Duration = Duration::from_secs(RECORDING_SECONDS.unsigned_abs());

/// How often something worth clipping happens, in seconds.
///
/// A minute apart, in bursts of four within six seconds — which is what the
/// merge is for, and what makes this a hundred and eighty clips rather than
/// seven hundred and twenty. A busier session than most, on purpose: if the
/// per-clip cost is honest here it is not hiding behind a small number.
const BURST_SPACING: i64 = 60;
const BURST_SIZE: i64 = 4;

/// The ceiling the measurement asserts, per clip.
///
/// Deliberately loose — this runs in a debug build on a machine that may be
/// busy, and it is not a benchmark. It is orders of magnitude above what
/// generation costs, which is what makes it a check that only fails when
/// something has started doing real work per clip: opening a file, decoding a
/// frame, copying footage.
const CEILING_PER_CLIP: Duration = Duration::from_millis(1);

fn at(seconds: i64) -> EventTime {
    EventTime::from_media_nanos(seconds * 1_000_000_000)
}

fn kill_at(seconds: i64) -> GameEvent {
    GameEvent::new(
        EventKind::Kill,
        EventTiming::new(at(seconds), Duration::ZERO),
        EventSource::plugin("acme-cs2").expect("a well-formed plugin identifier"),
        Confidence::new(0.95).expect("a valid confidence"),
    )
}

/// A busy session's events: bursts of kills, a minute apart.
fn events() -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut moment = 30;
    while moment + BURST_SIZE * 2 < RECORDING_SECONDS {
        for index in 0..BURST_SIZE {
            events.push(kill_at(moment + index * 2));
        }
        moment += BURST_SPACING;
    }
    events
}

fn session(recording: &str) -> SessionRecordings {
    SessionRecordings::of([RecordedSegment::new(
        RecordingId::new(recording),
        RecordedSpan::from_epoch(RECORDING),
    )])
}

fn shipped_rules() -> ResolvedHighlightRules {
    HighlightRules::resolve(Scope::Global, &HighlightRules::none(), None)
}

#[test]
fn generating_a_busy_sessions_highlights_costs_microseconds_and_no_bytes() {
    let events = events();
    let recordings = session("2026-08-12T20-14-03-cs2");
    let rules = shipped_rules();
    let generation = HighlightGeneration::new(&rules, &recordings);

    let started = Instant::now();
    let generated = generation.generate(&events);
    let elapsed = started.elapsed();

    let clips = generated.clips().len();
    assert!(
        clips > 100,
        "the fixture should produce a busy session's worth of clips, not {clips}"
    );
    assert!(
        generated.withheld().is_empty(),
        "every moment of this session is inside the recording"
    );
    let per_clip = elapsed / u32::try_from(clips).expect("the clip count fits in a u32");
    println!(
        "{} events became {clips} clips in {elapsed:?} ({per_clip:?} each)",
        events.len()
    );

    // And the same run again, over what it produced: the path a re-run takes,
    // where every moment is checked against every clip the library already
    // holds. It produces nothing, and it is the more expensive of the two.
    let again = Instant::now();
    let second = generation
        .with_existing_clips(generated.clips())
        .generate(&events);
    let again = again.elapsed();
    let per_clip_again = again / u32::try_from(clips).expect("the clip count fits in a u32");
    println!(
        "the same run against {clips} existing clips took {again:?} ({per_clip_again:?} each)"
    );

    assert!(
        second.clips().is_empty(),
        "a second run generated clips again"
    );
    assert!(
        per_clip < CEILING_PER_CLIP && per_clip_again < CEILING_PER_CLIP,
        "generation took {per_clip:?} per clip and {per_clip_again:?} on the re-run, over the \
         {CEILING_PER_CLIP:?} that something doing real work per clip would approach"
    );
}

/// A directory of this test's own, removed when it is dropped.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-session-{label}-{}-{:?}",
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
fn generating_highlights_adds_no_file_and_leaves_the_recording_alone() {
    let directory = TestDirectory::new("no-media");
    let path = directory.0.join("2026-08-12 Counter-Strike 2.mkv");
    // Not a real MKV: nothing here opens it, so its contents cannot matter to
    // the result, and a test needing a real recording would need a GPU and a
    // game — which is the kind of test that stops being run.
    let bytes: Vec<u8> = (0..8192_u32)
        .map(|index| u8::try_from(index % 251).expect("a value under 251 fits in a byte"))
        .collect();
    fs::write(&path, &bytes).expect("the stand-in recording can be written");
    let before = contents(&directory.0);

    // A path is the harshest form of the question: generation holds everything
    // needed to open that file, and to write a clip beside it, and does neither.
    let recordings = session(&path.display().to_string());
    let rules = shipped_rules();
    let generated = HighlightGeneration::new(&rules, &recordings).generate(&events());

    // And everything a caller does with the result afterwards: check each clip
    // is playable, and write the document out as the text somebody else stores.
    let mut document_bytes = 0;
    for clip in generated.clips() {
        clip.edit().validate().expect("a generated clip is valid");
        document_bytes += clip.edit().write().expect("the document saves").len();
    }

    let after = contents(&directory.0);
    assert_eq!(
        after,
        before,
        "generating {} clips changed what is on disk",
        generated.clips().len()
    );
    assert_eq!(
        after.get(&path).map(Vec::len),
        Some(bytes.len()),
        "the source recording's size changed"
    );

    // The whole cost of a session's highlights, as text. The recording they
    // describe is three hours of video; they are a few kilobytes of metadata.
    println!(
        "{} generated clips are {document_bytes} bytes of text in total",
        generated.clips().len()
    );
    assert!(
        document_bytes < generated.clips().len() * 1_024,
        "a generated clip should be under a kilobyte of metadata, not media: {document_bytes} \
         bytes for {}",
        generated.clips().len()
    );
}

/// Every way of touching a file, or of spending time, that generation could
/// reach for.
///
/// Names rather than behaviour, which is crude — and is the point: the check has
/// to fail on the *appearance* of file access or of a wait, before anybody has
/// to reason about whether that particular one is safe on a thread a recording
/// might be sharing (AGENTS.md section 20).
const FORBIDDEN: &[&str] = &[
    "std::fs",
    "fs::",
    "File::",
    "OpenOptions",
    "std::process",
    "Command::",
    "std::thread",
    "sleep",
    "Mutex",
    "RwLock",
];

#[test]
fn generation_has_no_way_to_open_a_file_or_to_wait_for_anything() {
    let module = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("highlights")
        .join("generate.rs");
    let text = fs::read_to_string(&module).expect("the module's source can be read");

    let mut findings = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        for forbidden in FORBIDDEN {
            if code.contains(forbidden) {
                findings.push(format!("{}:{}: {forbidden}", module.display(), number + 1));
            }
        }
    }

    assert!(
        findings.is_empty(),
        "generating a highlight is arithmetic over data the caller already holds: it writes \
         nothing and waits for nothing, so that it can run beside a recording without being able \
         to cost it a frame (issue #76, AGENTS.md section 20). Found: {findings:#?}"
    );
}

/// The other half of "it writes nothing": what it *produces* is a clip with no
/// file, whatever the events did.
#[test]
fn every_generated_clip_is_metadata_over_a_recording_that_already_exists() {
    let recordings = session("rec-1");
    let rules = shipped_rules();
    let generated = HighlightGeneration::new(&rules, &recordings).generate(&events());

    for clip in generated.clips() {
        assert!(
            clip.origin().is_generated(),
            "a generated clip has to be filterable as one"
        );
        assert_eq!(
            clip.source_recordings().collect::<Vec<&RecordingId>>(),
            vec![&RecordingId::new("rec-1")],
            "a generated clip plays the session's own recording and nothing else"
        );
        let duration = clip.duration().expect("a generated clip has a length");
        assert!(
            duration >= Duration::from_secs(25) && duration <= Duration::from_secs(2 * 60),
            "a clip of {duration:?} is outside what the shipped rules can produce"
        );
    }

    // Nothing was written, so nothing has to be cleaned up: the clips are
    // values, and dropping them costs the memory back.
    let clips: Vec<VirtualClip> = generated.into_clips();
    assert!(!clips.is_empty());
}
