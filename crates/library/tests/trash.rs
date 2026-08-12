//! Deleting and undeleting, against a real database and real files.
//!
//! The value of this file is not that a delete works on a tidy library. It is
//! that the footage comes back — byte for byte, with everything the user put on
//! it — and that nothing here can reach a file the user did not ask to lose
//! (AGENTS.md sections 23 and 56). Every library below is built by the real
//! indexer from a real session sidecar, so the rows these operations act on are
//! the rows a running Clipped would have.
//!
//! Nothing here opens a window, a capture device or an audio device. The one
//! test that needs real media writes it with the pinned FFmpeg build and skips
//! cleanly on a checkout that has none.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

use clipped_library::index::{reconcile, IndexControl, IndexPace, IndexSettings};
use clipped_library::trash::{EmptyTrash, FileOutcome, Retention, Trash, TrashError, TrashItem};
use clipped_storage::Database;
use serde_json::json;

/// A day, for moving `now` around without waiting for one.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// 2026-08-12T08:00:00Z — the moment every deletion below is recorded at.
///
/// Fixed rather than read from the clock, so that what these tests assert does
/// not depend on when they ran (AGENTS.md section 25).
fn deleted_at() -> SystemTime {
    unix(1_786_521_600)
}

fn unix(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

/// A library on disk: a folder of recordings, a trash beside it, and the index.
struct Library {
    root: PathBuf,
    trash: Trash,
    database: PathBuf,
}

impl Library {
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("clipped-trash-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let root = directory.join("Recordings");
        fs::create_dir_all(&root).expect("a scratch library can be created");
        Self {
            // A sibling of the recordings folder, never a child: a trash inside
            // it would be counted twice by storage accounting, which refuses the
            // overlap (`clipped_library::accounting::StorageRoots`).
            trash: Trash::new(directory.join("Trash")),
            root,
            database: directory.join("library.db"),
        }
    }

    /// Writes one session's sidecar and the recordings it names, then indexes
    /// it.
    ///
    /// The recordings are written with distinct, non-repeating content so that
    /// "the same bytes came back" is a real comparison rather than one that any
    /// two files of the same length would pass.
    fn with_recordings(self, session: &str, files: &[&str]) -> Self {
        let recordings: Vec<_> = files
            .iter()
            .enumerate()
            .map(|(offset, file)| {
                let path = self.root.join(file);
                fs::write(&path, footage(offset, 4_096)).expect("a recording can be written");
                json!({
                    "index": offset + 1,
                    "output": path.display().to_string(),
                    "started_at": "2026-08-11T14:32:09+01:00",
                    "ended_at": "2026-08-11T14:50:13+01:00",
                    "outcome": "recorded",
                    "duration_seconds": 1084.0,
                    "end_reason": "stopped",
                })
            })
            .collect();

        fs::write(
            self.root.join(format!("clipped-{session}.session.json")),
            json!({
                "schema_version": 1,
                "session_id": session,
                "game": { "kind": "known", "game_id": "cs2", "name": "Counter-Strike 2" },
                "started_at": "2026-08-11T14:32:05+01:00",
                "ended_at": "2026-08-11T15:31:21+01:00",
                "recordings": recordings,
                "events": [
                    { "at": "2026-08-11T15:31:21+01:00", "event": "session-ended",
                      "reason": "game-exited" }
                ],
            })
            .to_string(),
        )
        .expect("a sidecar can be written");

        self.index();
        self
    }

    fn open(&self) -> Database {
        Database::open(&self.database).expect("the database can be opened")
    }

    /// Runs the real indexer over the library, which is what writes the rows the
    /// trash then acts on.
    fn index(&self) -> Database {
        let mut database = self.open();
        let mut settings = IndexSettings::new([self.root.clone()]);
        settings.pace = IndexPace::foreground();
        reconcile(
            &mut database,
            &settings,
            &IndexControl::new(),
            unix(1_786_500_000),
        )
        .expect("reconciliation completes");
        database
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// The recording the index holds for `name`.
    fn recording(&self, database: &Database, name: &str) -> TrashItem {
        let id: i64 = database
            .connection()
            .query_row(
                "SELECT recording_id FROM recordings WHERE path = ?1",
                [self.file(name).display().to_string()],
                |row| row.get(0),
            )
            .expect("the recording is in the index");
        TrashItem::recording(id)
    }
}

/// Bytes that differ from offset to offset and from file to file.
///
/// A recording of 4,096 zeroes would compare equal to any other, which would
/// make "restored byte for byte" a test that could not fail.
fn footage(seed: usize, length: usize) -> Vec<u8> {
    let mut state = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..length)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

/// One row's columns, as text, for asserting on what a delete wrote.
fn recording_row(database: &Database, item: TrashItem) -> HashMap<&'static str, Option<String>> {
    let mut row = HashMap::new();
    let columns = database
        .connection()
        .query_row(
            "SELECT path, deleted_at, deleted_from, missing_since, favourited_at \
             FROM recordings WHERE recording_id = ?1",
            [item.id],
            |row| {
                Ok([
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ])
            },
        )
        .expect("the recording is in the index");
    for (name, value) in [
        "path",
        "deleted_at",
        "deleted_from",
        "missing_since",
        "favourited_at",
    ]
    .into_iter()
    .zip(columns)
    {
        row.insert(name, value);
    }
    row
}

fn count(database: &Database, query: &str) -> i64 {
    database
        .connection()
        .query_row(query, [], |row| row.get(0))
        .expect("the count can be read")
}

#[test]
fn deleting_a_recording_moves_the_file_rather_than_unlinking_it() {
    // The physical decision, asserted rather than described: the bytes are
    // still on the disk afterwards, in the trash, under the name the user
    // knows them by.
    let library = Library::new("moved").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let original = library.file("a.mkv");
    let bytes = fs::read(&original).expect("the recording can be read");

    let entry = library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");

    assert!(!original.exists(), "the file was left where it was");
    assert!(
        entry.path.starts_with(library.trash.directory()),
        "the file went somewhere other than the trash: {}",
        entry.path.display()
    );
    assert_eq!(
        fs::read(&entry.path).expect("the trashed file can be read"),
        bytes,
        "the footage changed on the way into the trash"
    );
    assert_eq!(entry.original_path, original);

    let row = recording_row(&database, item);
    assert_eq!(
        row["path"].as_deref(),
        Some(entry.path.display().to_string()).as_deref()
    );
    assert_eq!(
        row["deleted_from"].as_deref(),
        Some(original.display().to_string()).as_deref()
    );
    assert!(
        row["deleted_at"].is_some(),
        "the row was not marked deleted"
    );
}

#[test]
fn a_restore_returns_the_file_byte_for_byte() {
    // The acceptance criterion. A rename moves a directory entry and never
    // touches the data, and this is what holds that claim to the bytes.
    let library = Library::new("byte-for-byte").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let original = library.file("a.mkv");
    let before = fs::read(&original).expect("the recording can be read");
    let length = before.len();

    let entry = library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");
    let outcome = library
        .trash
        .restore(&mut database, item)
        .expect("it is restored");

    assert_eq!(outcome.path, original);
    assert!(!outcome.diverted());
    assert!(outcome.file_restored);
    assert!(
        !entry.path.parent().expect("an entry directory").exists(),
        "the trash kept an empty folder for something that was restored"
    );
    let after = fs::read(&original).expect("the restored recording can be read");
    assert_eq!(after.len(), length, "the restored file changed length");
    assert_eq!(after, before, "the restored file is not the same bytes");

    let row = recording_row(&database, item);
    assert_eq!(row["deleted_at"], None);
    assert_eq!(row["deleted_from"], None);
    assert_eq!(
        row["path"].as_deref(),
        Some(original.display().to_string()).as_deref()
    );
}

#[test]
fn a_restored_recording_keeps_its_metadata_its_clips_and_its_bookmarks() {
    // The other half of the acceptance criterion. None of this survives if
    // deleting removes a row, which is why deleting marks one.
    let library = Library::new("metadata").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    database
        .connection()
        .execute_batch(&format!(
            "UPDATE recordings SET favourited_at = '2026-08-11T16:00:00+01:00' \
                 WHERE recording_id = {id};\
             INSERT INTO tags (name) VALUES ('ace');\
             INSERT INTO recording_tags (recording_id, tag_id) \
                 VALUES ({id}, (SELECT tag_id FROM tags WHERE name = 'ace'));\
             INSERT INTO bookmarks (recording_id, at_seconds, label, created_at) \
                 VALUES ({id}, 42.5, 'the ace', '2026-08-11T16:01:00+01:00');\
             INSERT INTO clips (source_recording_id, path, created_at) \
                 VALUES ({id}, 'D:\\Clips\\ace.mkv', '2026-08-11T16:02:00+01:00');",
            id = item.id
        ))
        .expect("the user's metadata can be written");

    library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");
    library
        .trash
        .restore(&mut database, item)
        .expect("it is restored");

    let row = recording_row(&database, item);
    assert_eq!(
        row["favourited_at"].as_deref(),
        Some("2026-08-11T16:00:00+01:00"),
        "the favourite was lost"
    );
    assert_eq!(
        count(
            &database,
            &format!(
                "SELECT COUNT(*) FROM recording_tags WHERE recording_id = {}",
                item.id
            )
        ),
        1,
        "the tag was lost"
    );
    assert_eq!(
        count(
            &database,
            &format!(
                "SELECT COUNT(*) FROM bookmarks WHERE recording_id = {}",
                item.id
            )
        ),
        1,
        "the bookmark was lost"
    );
    assert_eq!(
        count(
            &database,
            &format!(
                "SELECT COUNT(*) FROM clips WHERE source_recording_id = {}",
                item.id
            )
        ),
        1,
        "the clip lost its source"
    );
}

#[test]
fn a_restored_recording_still_plays() {
    // "Restored recordings play" is the acceptance criterion, and a byte
    // comparison is evidence rather than proof of it. This one is a real
    // Matroska file, and `ffprobe` — not this workspace's own muxer — is what
    // says it opens afterwards (AGENTS.md section 22).
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return;
    };
    let library = Library::new("plays").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let recording = library.file("a.mkv");
    let output = Command::new(tools.ffmpeg())
        .args(["-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=30:duration=2",
        ])
        .args(["-c:v", "libopenh264", "-pix_fmt", "yuv420p"])
        .arg(&recording)
        .output()
        .expect("ffmpeg can be started");
    assert!(
        output.status.success(),
        "ffmpeg failed to write the subject recording: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The index measured a placeholder; re-index so the row matches the real
    // file that replaced it.
    let mut database = library.index();
    let item = library.recording(&database, "a.mkv");

    library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");
    let outcome = library
        .trash
        .restore(&mut database, item)
        .expect("it is restored");

    clipped_media_validation::Media::open(&outcome.path)
        .expect("the restored recording opens")
        .validate()
        .stream_count(1)
        .video(clipped_media_validation::VideoStream::codec("h264").resolution(320, 180))
        .duration_seconds(2.0, 0.3)
        .monotonic_timestamps()
        .assert_valid();
}

#[test]
fn restoring_into_an_occupied_location_never_overwrites_what_is_there() {
    // The user restored a backup while the recording sat in the trash. Both
    // files are theirs and neither is this code's to destroy.
    let library = Library::new("occupied").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let original = library.file("a.mkv");
    let restored_footage = fs::read(&original).expect("the recording can be read");

    library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");
    fs::write(&original, b"a file the user put back themselves").expect("the occupant is written");

    let outcome = library
        .trash
        .restore(&mut database, item)
        .expect("it is restored");

    assert!(
        outcome.diverted(),
        "it claimed to restore over the occupant"
    );
    assert_eq!(
        fs::read(&original).expect("the occupant can be read"),
        b"a file the user put back themselves",
        "restoring destroyed a file the user did not ask to lose"
    );
    assert_eq!(
        fs::read(&outcome.path).expect("the restored file can be read"),
        restored_footage
    );
    assert_eq!(
        recording_row(&database, item)["path"].as_deref(),
        Some(outcome.path.display().to_string()).as_deref(),
        "the index points at where the file used to be rather than where it is"
    );
}

#[test]
fn restoring_recreates_the_folder_the_recording_came_from() {
    let library = Library::new("folder-gone").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let original = library.file("a.mkv");

    library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");
    fs::remove_dir_all(&library.root).expect("the user deletes the whole folder");

    let outcome = library
        .trash
        .restore(&mut database, item)
        .expect("it is restored");

    assert_eq!(outcome.path, original);
    assert!(original.exists(), "the recording did not come back");
}

#[test]
fn retention_expiry_is_judged_from_the_moment_of_deletion_and_not_by_waiting() {
    let library = Library::new("expiry").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let entry = library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");

    let early = library
        .trash
        .expire(&mut database, Retention::SevenDays, deleted_at() + 6 * DAY)
        .expect("the sweep runs");
    assert!(early.removed.is_empty(), "footage expired a day early");
    assert!(entry.path.exists(), "the file was removed a day early");

    let late = library
        .trash
        .expire(&mut database, Retention::SevenDays, deleted_at() + 8 * DAY)
        .expect("the sweep runs");

    assert_eq!(late.removed.len(), 1);
    assert_eq!(late.removed[0].item, item);
    assert_eq!(late.removed[0].file, FileOutcome::Deleted);
    assert_eq!(late.bytes_reclaimed(), 4_096);
    assert!(!entry.path.exists(), "the file survived its retention");
    assert_eq!(
        count(&database, "SELECT COUNT(*) FROM recordings"),
        0,
        "the row survived its retention"
    );
}

#[test]
fn a_sweep_never_reaches_a_recording_that_is_not_in_the_trash() {
    // The rule the whole module exists for. A sweep with the shortest retention
    // there is, over a library where one recording was deleted and one was not.
    let library =
        Library::new("sweep-scope").with_recordings("cs2-20260811-143205", &["a.mkv", "b.mkv"]);
    let mut database = library.open();
    let deleted = library.recording(&database, "a.mkv");
    let kept = library.file("b.mkv");
    let kept_bytes = fs::read(&kept).expect("the recording can be read");
    library
        .trash
        .send(&mut database, deleted, deleted_at())
        .expect("it is deleted");

    let report = library
        .trash
        .expire(&mut database, Retention::Immediate, deleted_at() + DAY)
        .expect("the sweep runs");

    assert_eq!(report.removed.len(), 1);
    assert!(kept.exists(), "a recording nobody deleted was destroyed");
    assert_eq!(fs::read(&kept).expect("it can be read"), kept_bytes);
    assert_eq!(
        count(&database, "SELECT COUNT(*) FROM recordings"),
        1,
        "the row of a recording nobody deleted was removed"
    );
}

#[test]
fn something_that_is_not_in_the_trash_cannot_be_permanently_deleted() {
    // The interlock: destroying footage takes two steps, and the first one is
    // recoverable.
    let library = Library::new("interlock").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");

    let error = library
        .trash
        .permanently_delete(&mut database, item)
        .expect_err("a live recording cannot be destroyed in one call");

    assert!(matches!(error, TrashError::NotInTrash { .. }), "{error}");
    assert!(
        library.file("a.mkv").exists(),
        "a recording that was never deleted was destroyed"
    );
    assert_eq!(count(&database, "SELECT COUNT(*) FROM recordings"), 1);
}

#[test]
fn emptying_the_trash_is_refused_unless_it_is_what_the_user_confirmed() {
    let library =
        Library::new("empty-confirm").with_recordings("cs2-20260811-143205", &["a.mkv", "b.mkv"]);
    let mut database = library.open();
    let first = library.recording(&database, "a.mkv");
    let second = library.recording(&database, "b.mkv");
    library
        .trash
        .send(&mut database, first, deleted_at())
        .expect("it is deleted");
    let shown = library.trash.list(&database).expect("the trash lists");
    let confirmation = EmptyTrash::for_listing(&shown);

    // Something else arrives between the dialogue opening and the click.
    library
        .trash
        .send(
            &mut database,
            second,
            deleted_at() + Duration::from_secs(60),
        )
        .expect("a second item is deleted");

    let error = library
        .trash
        .empty(&mut database, confirmation)
        .expect_err("the trash is not what was confirmed");

    assert!(matches!(error, TrashError::Changed { .. }), "{error}");
    assert_eq!(
        library
            .trash
            .list(&database)
            .expect("the trash lists")
            .len(),
        2,
        "something was destroyed on a confirmation that no longer applied"
    );

    let now = library.trash.list(&database).expect("the trash lists");
    let report = library
        .trash
        .empty(&mut database, EmptyTrash::for_listing(&now))
        .expect("a confirmation of what is there empties it");

    assert_eq!(report.removed.len(), 2);
    assert_eq!(report.bytes_reclaimed(), 8_192);
    assert!(library.trash.list(&database).expect("it lists").is_empty());
}

#[test]
fn deleting_something_twice_is_refused_rather_than_moving_it_again() {
    let library = Library::new("twice").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let entry = library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");

    let error = library
        .trash
        .send(&mut database, item, deleted_at() + DAY)
        .expect_err("it is already in the trash");

    assert!(
        matches!(error, TrashError::AlreadyInTrash { .. }),
        "{error}"
    );
    assert!(
        entry.path.exists(),
        "a second delete moved the file out from under the first"
    );
    assert_eq!(
        recording_row(&database, item)["deleted_from"].as_deref(),
        Some(library.file("a.mkv").display().to_string()).as_deref(),
        "a second delete rewrote where the recording came from"
    );
}

#[test]
fn a_delete_the_index_refuses_puts_the_file_back() {
    // The compensating move, provoked rather than described: a read-only
    // connection takes the query and refuses the update. The file has to be
    // where it was afterwards, because an index can be rebuilt from the
    // sidecars and a recording cannot be rebuilt from anything.
    let library = Library::new("index-refuses").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let item = library.recording(&library.open(), "a.mkv");
    let original = library.file("a.mkv");
    let bytes = fs::read(&original).expect("the recording can be read");
    let mut read_only =
        Database::open_read_only(&library.database).expect("a reading connection opens");

    let error = library
        .trash
        .send(&mut read_only, item, deleted_at())
        .expect_err("a read-only index cannot record a deletion");

    assert!(matches!(error, TrashError::Database(_)), "{error}");
    assert!(original.exists(), "the recording was left in the trash");
    assert_eq!(
        fs::read(&original).expect("it can be read"),
        bytes,
        "the recording came back changed"
    );
    assert!(
        !library.trash.directory().join("20260812-080000").exists(),
        "a trash entry was left behind"
    );
}

#[test]
fn an_item_whose_file_has_already_gone_can_still_be_deleted_and_restored() {
    // A user who removed the file in Explorer still wants the row out of their
    // library, and must still be able to change their mind.
    let library = Library::new("no-file").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let original = library.file("a.mkv");
    fs::remove_file(&original).expect("the user deletes it themselves");

    let entry = library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("an item with no file can still be deleted");
    assert_eq!(entry.path, original, "a trash entry was invented for it");
    assert!(
        !library.trash.directory().exists(),
        "a trash directory was created for a file that does not exist"
    );

    let outcome = library
        .trash
        .restore(&mut database, item)
        .expect("it is restored");

    assert!(
        !outcome.file_restored,
        "a file that was never there was reported as restored"
    );
    assert_eq!(outcome.path, original);
    assert_eq!(recording_row(&database, item)["deleted_at"], None);
}

#[test]
fn a_trash_somebody_has_emptied_in_explorer_still_restores_the_row() {
    // Recordings are ordinary files in an ordinary folder (AGENTS.md
    // section 32), so a user can delete one from the trash themselves. The
    // metadata is still theirs to get back, and the row must say the file is
    // gone rather than claim it is in a folder nothing is in.
    let library =
        Library::new("emptied-by-hand").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let entry = library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");
    fs::remove_file(&entry.path).expect("the user empties the trash themselves");

    let outcome = library
        .trash
        .restore(&mut database, item)
        .expect("the row is still restored");

    assert!(
        !outcome.file_restored,
        "a file that is not there was reported as restored"
    );
    assert_eq!(outcome.path, library.file("a.mkv"));
    assert_eq!(recording_row(&database, item)["deleted_at"], None);
}

#[test]
fn a_clip_is_deleted_and_restored_the_same_way_a_recording_is() {
    let library = Library::new("clips").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let file = library.root.join("ace.mkv");
    fs::write(&file, footage(9, 1_024)).expect("a clip can be written");
    database
        .connection()
        .execute(
            "INSERT INTO clips (path, created_at, size_bytes) VALUES (?1, ?2, 1024)",
            clipped_storage::rusqlite::params![
                file.display().to_string(),
                "2026-08-11T16:02:00+01:00"
            ],
        )
        .expect("the clip can be indexed");
    let item = TrashItem::clip(database.connection().last_insert_rowid());
    let bytes = fs::read(&file).expect("the clip can be read");

    let entry = library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("a clip is deleted");
    assert!(!file.exists());
    assert!(entry.path.starts_with(library.trash.directory()));

    let outcome = library
        .trash
        .restore(&mut database, item)
        .expect("a clip is restored");

    assert_eq!(outcome.path, file);
    assert_eq!(fs::read(&file).expect("it can be read"), bytes);
}

#[test]
fn what_is_in_the_trash_is_listed_newest_first_with_what_is_left_of_its_retention() {
    let library =
        Library::new("listing").with_recordings("cs2-20260811-143205", &["a.mkv", "b.mkv"]);
    let mut database = library.open();
    let older = library.recording(&database, "a.mkv");
    let newer = library.recording(&database, "b.mkv");
    library
        .trash
        .send(&mut database, older, deleted_at())
        .expect("it is deleted");
    library
        .trash
        .send(&mut database, newer, deleted_at() + DAY)
        .expect("it is deleted");

    let listing = library.trash.list(&database).expect("the trash lists");

    assert_eq!(
        listing.iter().map(|entry| entry.item).collect::<Vec<_>>(),
        vec![newer, older],
        "the trash is not listed newest first"
    );
    assert_eq!(
        listing[1].remaining(Retention::ThreeDays, deleted_at() + DAY),
        Some(2 * DAY),
        "the days left are not counted from when the item was deleted"
    );
    assert_eq!(listing[0].original_path, library.file("b.mkv"));
}

#[test]
fn a_trashed_recording_survives_the_indexer_running_over_its_session_again() {
    // The interaction that makes restore possible at all. The session sidecar
    // still names the location the recording came from, and reconciliation
    // rewrites every column it is authoritative for — so `path`, which now
    // points into the trash, has to be one it leaves alone, exactly as it
    // leaves the favourite alone.
    let library = Library::new("reindex").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let entry = library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");
    drop(database);

    let mut database = library.index();

    let row = recording_row(&database, item);
    assert_eq!(
        row["path"].as_deref(),
        Some(entry.path.display().to_string()).as_deref(),
        "re-indexing lost the only record of where the deleted file is"
    );
    assert!(row["deleted_at"].is_some(), "re-indexing undeleted the row");
    assert_eq!(
        row["missing_since"], None,
        "re-indexing reported a deleted recording as missing"
    );

    let outcome = library
        .trash
        .restore(&mut database, item)
        .expect("it can still be restored");
    assert!(outcome.file_restored);
    assert_eq!(outcome.path, library.file("a.mkv"));
}

#[test]
fn a_recording_the_index_does_not_have_is_refused() {
    let library = Library::new("no-such-item").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();

    let error = library
        .trash
        .send(&mut database, TrashItem::recording(9_999), deleted_at())
        .expect_err("there is no such recording");

    assert!(matches!(error, TrashError::NoSuchItem { .. }), "{error}");
}

#[test]
fn a_file_that_is_not_in_the_trash_is_left_alone_when_its_entry_expires() {
    // The guard, reached the way it really would be: an entry whose media had
    // already gone names a path in the library rather than in the trash, and a
    // file that reappeared there — a backup put back — is not this to destroy.
    let library = Library::new("guard").with_recordings("cs2-20260811-143205", &["a.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");
    let original = library.file("a.mkv");
    fs::remove_file(&original).expect("the user deletes it themselves");
    library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");
    fs::write(&original, b"restored from a backup").expect("the user puts a file back");

    let report = library
        .trash
        .expire(&mut database, Retention::Immediate, deleted_at() + DAY)
        .expect("the sweep runs");

    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].file, FileOutcome::LeftInPlace);
    assert_eq!(report.bytes_reclaimed(), 0);
    assert!(
        original.exists(),
        "a file outside the trash was destroyed by a sweep"
    );
    assert_eq!(
        fs::read(&original).expect("it can be read"),
        b"restored from a backup"
    );
}

/// The index's own view of the library must not count what is in the trash.
///
/// Already the summary's rule (`clipped_library::index`), and asserted here
/// because it is the trash that now produces the state it describes.
#[test]
fn a_deleted_recording_stops_counting_towards_what_the_library_holds() {
    let library =
        Library::new("summaries").with_recordings("cs2-20260811-143205", &["a.mkv", "b.mkv"]);
    let mut database = library.open();
    let item = library.recording(&database, "a.mkv");

    library
        .trash
        .send(&mut database, item, deleted_at())
        .expect("it is deleted");

    let summaries =
        clipped_library::index::game_summaries(&database).expect("the summaries can be read");
    let game = summaries
        .iter()
        .find(|summary| summary.game_id.as_deref() == Some("cs2"))
        .expect("the game is in the library");
    assert_eq!(game.recordings, 1, "a deleted recording is still counted");
    assert_eq!(game.bytes, 4_096, "deleted footage is still counted");
}
