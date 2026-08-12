//! What a bookmark has to get right, and what a file has to survive.

use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;

use super::*;

/// 2026-08-11T14:32:05Z.
fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

fn moment() -> SystemTime {
    at(1_786_458_725)
}

/// A directory of this test's own.
///
/// Named after the process, the thread and a counter: several of these run at
/// once under `cargo test`, and agents share this machine (AGENTS.md
/// section 25).
fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);

    let directory = std::env::temp_dir().join(format!(
        "clipped-bookmarks-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("a scratch directory can be made");
    directory
}

/// A request with every field filled in, each with a value nothing else uses.
///
/// Distinct values on purpose: a round-trip test whose fields could be confused
/// with each other is a round-trip test that passes while the code swaps two of
/// them.
fn filled() -> BookmarkRequest {
    BookmarkRequest::new()
        .with_label(Some("triple kill on mid".to_owned()))
        .expect("a plain label")
        .with_colour(Some("#ffcc00".to_owned()))
        .expect("a plain colour")
        .with_duration(Some(Duration::from_millis(12_500)))
        .expect("a duration inside the range")
        .with_lead(Duration::from_millis(3_250))
        .expect("a lead inside the range")
}

#[test]
fn a_bookmark_is_stamped_before_the_press_by_the_lead_it_was_taken_with() {
    // The whole reaction-time decision, as arithmetic. A build that stamped at
    // the press would put this at 120s, which is the failure the lead exists to
    // prevent.
    let request = BookmarkRequest::new()
        .with_lead(Duration::from_secs(5))
        .expect("five seconds is inside the range");
    let bookmark = Bookmark::placed(&request, Duration::from_secs(120), moment());

    assert_eq!(bookmark.at(), Duration::from_secs(115));
    assert_eq!(bookmark.lead(), Duration::from_secs(5));
    assert_eq!(
        bookmark.pressed_at(),
        Duration::from_secs(120),
        "the press has to be recoverable from the bookmark, or a timeline cannot show both"
    );
}

#[test]
fn a_bookmark_taken_before_the_lead_has_elapsed_lands_at_the_start_of_the_recording() {
    // Two seconds into a recording, with a five-second lead. The moment being
    // marked is the beginning of the file, and refusing the bookmark — or
    // wrapping round to a huge offset — would both be worse than clamping.
    let bookmark = Bookmark::placed(&BookmarkRequest::new(), Duration::from_secs(2), moment());

    assert_eq!(bookmark.at(), Duration::ZERO);
    assert_eq!(
        bookmark.lead(),
        DEFAULT_LEAD,
        "the lead is still recorded, so the press is still recoverable"
    );
}

#[test]
fn the_default_request_is_the_bare_hotkey_press() {
    let request = BookmarkRequest::new();
    assert_eq!(request.label(), None);
    assert_eq!(request.colour(), None);
    assert_eq!(request.duration(), None);
    assert_eq!(request.lead(), DEFAULT_LEAD);
}

#[test]
fn a_label_or_colour_the_file_could_not_carry_back_is_refused_by_name() {
    let long = "x".repeat(MAXIMUM_LABEL + 1);
    let error = BookmarkRequest::new()
        .with_label(Some(long))
        .expect_err("a label past the limit");
    assert!(
        matches!(error, BookmarkError::TooLong { field: "label", .. }),
        "{error}"
    );

    let error = BookmarkRequest::new()
        .with_colour(Some("#ff\u{0}00".to_owned()))
        .expect_err("a colour with a control character");
    assert!(
        matches!(error, BookmarkError::ControlCharacter { field: "colour" }),
        "{error}"
    );

    let error = BookmarkRequest::new()
        .with_lead(MAXIMUM_LEAD + Duration::from_secs(1))
        .expect_err("a lead past the limit");
    assert!(
        matches!(error, BookmarkError::OutOfRange { field: "lead", .. }),
        "{error}"
    );

    let error = BookmarkRequest::new()
        .with_duration(Some(MAXIMUM_DURATION + Duration::from_secs(1)))
        .expect_err("a duration past the limit");
    assert!(
        matches!(
            error,
            BookmarkError::OutOfRange {
                field: "duration",
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn every_field_of_a_bookmark_survives_being_written_and_read_back() {
    // The one the "hollow round trip" failures were: each field is asserted
    // separately, against a value no other field carries, so a build that
    // dropped one or wrote the lead into the offset fails here rather than
    // shipping.
    let directory = scratch("round-trip");
    let output = directory.join("clipped-cs2-20260811-143205.mkv");
    let log = BookmarkLog::for_recording(&output);

    let written = log
        .add(&filled(), Duration::from_millis(65_500), moment())
        .expect("the bookmark can be written");

    let read = BookmarkFile::for_recording(&output).expect("the file can be read back");
    assert_eq!(read.schema_version, SCHEMA_VERSION);
    assert_eq!(read.recording, "clipped-cs2-20260811-143205.mkv");
    assert_eq!(read.bookmarks.len(), 1);

    let back = &read.bookmarks[0];
    assert_eq!(
        back.at(),
        Duration::from_millis(62_250),
        "the offset is the position less the lead"
    );
    assert_eq!(back.lead(), Duration::from_millis(3_250));
    assert_eq!(back.label(), Some("triple kill on mid"));
    assert_eq!(back.colour(), Some("#ffcc00"));
    assert_eq!(back.duration(), Some(Duration::from_millis(12_500)));
    assert_eq!(back.created_at(), written.created_at());
    assert_eq!(
        back, &written,
        "the whole bookmark, not only the fields above"
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn the_file_is_written_in_the_field_names_the_library_index_uses() {
    // The bookmarks table holds at_seconds, label, colour, duration_seconds and
    // created_at (crates/storage/migrations/0001_initial.sql). A file that
    // called them something else would mean a translation step nobody wrote.
    let directory = scratch("field-names");
    let output = directory.join("clipped-a.mkv");
    let log = BookmarkLog::for_recording(&output);
    log.add(&filled(), Duration::from_secs(100), moment())
        .expect("the bookmark can be written");

    let text = fs::read_to_string(log.path()).expect("the file can be read");
    let file: Value = serde_json::from_str(&text).expect("the file is JSON");

    assert_eq!(file["schema_version"], Value::from(SCHEMA_VERSION));
    assert_eq!(file["recording"], Value::from("clipped-a.mkv"));

    let bookmark = &file["bookmarks"][0];
    assert_eq!(bookmark["at_seconds"], Value::from(96.75));
    assert_eq!(bookmark["lead_seconds"], Value::from(3.25));
    assert_eq!(bookmark["label"], Value::from("triple kill on mid"));
    assert_eq!(bookmark["colour"], Value::from("#ffcc00"));
    assert_eq!(bookmark["duration_seconds"], Value::from(12.5));
    assert!(
        bookmark["created_at"].is_string(),
        "a bookmark has to say when it was taken: {bookmark}"
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_bookmark_with_nothing_but_a_moment_writes_no_empty_fields() {
    let directory = scratch("bare");
    let output = directory.join("clipped-b.mkv");
    let log = BookmarkLog::for_recording(&output);
    log.add(&BookmarkRequest::new(), Duration::from_secs(30), moment())
        .expect("the bookmark can be written");

    let text = fs::read_to_string(log.path()).expect("the file can be read");
    let file: Value = serde_json::from_str(&text).expect("the file is JSON");
    let bookmark = &file["bookmarks"][0];

    assert!(bookmark.get("label").is_none(), "{bookmark}");
    assert!(bookmark.get("colour").is_none(), "{bookmark}");
    assert!(bookmark.get("duration_seconds").is_none(), "{bookmark}");

    let read = BookmarkFile::for_recording(&output).expect("it reads back");
    assert_eq!(read.bookmarks[0].label(), None);
    assert_eq!(read.bookmarks[0].colour(), None);
    assert_eq!(read.bookmarks[0].duration(), None);

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn every_bookmark_taken_so_far_is_on_disk_before_the_next_one_is_asked_for() {
    // This is the "survives the recorder being killed" property, tested the
    // only way it can be without killing a process: after each call, what is on
    // disk is everything taken so far. A build that batched writes, or wrote on
    // shutdown, fails on the first assertion.
    let directory = scratch("incremental");
    let output = directory.join("clipped-c.mkv");
    let log = BookmarkLog::for_recording(&output);

    for (index, position) in [30_u64, 90, 150].into_iter().enumerate() {
        log.add(
            &BookmarkRequest::new()
                .with_label(Some(format!("mark {index}")))
                .expect("a plain label"),
            Duration::from_secs(position),
            moment(),
        )
        .expect("the bookmark can be written");

        let read = BookmarkFile::for_recording(&output).expect("the file exists already");
        assert_eq!(
            read.bookmarks.len(),
            index + 1,
            "everything taken so far has to be on disk, not only at the end"
        );
        assert_eq!(
            read.bookmarks[index].label(),
            Some(format!("mark {index}").as_str())
        );
    }

    assert_eq!(log.count(), 3);
    let left: Vec<_> = fs::read_dir(&directory)
        .expect("the directory can be listed")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(
        left.len(),
        1,
        "the temporary file should have been renamed, not left behind: {left:?}"
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn the_bookmarks_of_a_recording_are_named_after_it_and_sit_beside_it() {
    let log = BookmarkLog::for_recording(Path::new(r"D:\clips\clipped-cs2-20260811-143205.mkv"));
    assert_eq!(
        log.path(),
        Path::new(r"D:\clips\clipped-cs2-20260811-143205.bookmarks.json"),
        "the pair has to travel together when somebody moves their clips"
    );
}

#[test]
fn a_file_from_a_later_build_is_read_rather_than_refused() {
    // Forward compatibility, asserted on the fields rather than on the file
    // parsing: a reader that refused an unknown key would make every recording
    // taken by a newer Clipped unreadable by this one, and one that lost the
    // known fields while tolerating the unknown one would be worse.
    let directory = scratch("forward");
    let path = directory.join("clipped-d.bookmarks.json");
    fs::write(
        &path,
        r##"{
          "schema_version": 2,
          "recording": "clipped-d.mkv",
          "invented_later": {"anything": true},
          "bookmarks": [
            {
              "at_seconds": 42.5,
              "lead_seconds": 5.0,
              "label": "kept",
              "colour": "#00ff00",
              "duration_seconds": 3.0,
              "created_at": "2026-08-11T14:32:05+01:00",
              "chapter": "second half"
            }
          ]
        }"##,
    )
    .expect("the file can be written");

    let read = BookmarkFile::read(&path).expect("a later build's file still reads");
    assert_eq!(read.schema_version, 2);
    assert_eq!(read.recording, "clipped-d.mkv");
    assert_eq!(read.bookmarks.len(), 1);
    assert_eq!(read.bookmarks[0].at(), Duration::from_millis(42_500));
    assert_eq!(read.bookmarks[0].lead(), Duration::from_secs(5));
    assert_eq!(read.bookmarks[0].label(), Some("kept"));
    assert_eq!(read.bookmarks[0].colour(), Some("#00ff00"));
    assert_eq!(read.bookmarks[0].duration(), Some(Duration::from_secs(3)));
    assert_eq!(read.bookmarks[0].created_at(), "2026-08-11T14:32:05+01:00");

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_hand_edited_file_with_impossible_figures_is_read_rather_than_panicking() {
    // `Duration::from_secs_f64` panics on a negative value and on one too large
    // to represent, and a recorder that a text editor can crash is not one to
    // ship (AGENTS.md section 16).
    let directory = scratch("nonsense");
    let path = directory.join("clipped-e.bookmarks.json");
    fs::write(
        &path,
        r#"{"schema_version":1,"recording":"clipped-e.mkv","bookmarks":[
          {"at_seconds":-4.0,"lead_seconds":1e308,"created_at":"whenever"}
        ]}"#,
    )
    .expect("the file can be written");

    let read = BookmarkFile::read(&path).expect("it still reads");
    assert_eq!(read.bookmarks[0].at(), Duration::ZERO);
    assert_eq!(read.bookmarks[0].lead(), Duration::ZERO);

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_file_that_is_not_this_format_is_refused_by_name_rather_than_read_as_empty() {
    let directory = scratch("malformed");
    let path = directory.join("clipped-f.bookmarks.json");
    fs::write(&path, "this is not JSON").expect("the file can be written");

    let error = BookmarkFile::read(&path).expect_err("it is not a bookmark file");
    assert!(matches!(error, BookmarkError::Malformed { .. }), "{error}");
    assert!(
        error.to_string().contains("clipped-f.bookmarks.json"),
        "the refusal should name the file: {error}"
    );

    let missing = BookmarkFile::read(&directory.join("nothing.bookmarks.json"))
        .expect_err("there is no such file");
    assert!(
        matches!(missing, BookmarkError::NotReadable { .. }),
        "{missing}"
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_bookmark_that_could_not_be_saved_is_reported_and_still_kept() {
    // A directory where the file should be is the cheapest way to make a write
    // fail without special permissions. What matters is both halves: the user
    // is told, and the bookmark is not thrown away.
    let directory = scratch("unwritable");
    let output = directory.join("clipped-g.mkv");
    let log = BookmarkLog::for_recording(&output);
    fs::create_dir_all(log.path()).expect("a directory can be made where the file would go");

    let error = log
        .add(&BookmarkRequest::new(), Duration::from_secs(60), moment())
        .expect_err("the file cannot be written over a directory");
    assert!(matches!(error, BookmarkError::NotWritten { .. }), "{error}");
    assert!(
        error.to_string().contains("clipped-g.bookmarks.json"),
        "the failure should name the file the user has to fix: {error}"
    );
    assert_eq!(
        log.count(),
        1,
        "a bookmark that could not be written is still taken, and the next write carries it"
    );

    let _ = fs::remove_dir_all(&directory);
}
