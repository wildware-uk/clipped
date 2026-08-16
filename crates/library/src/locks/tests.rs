//! Locking, unlocking, and what a locked sitting means for what is inside it.
//!
//! The half of [issue #472](https://github.com/wildware-uk/clipped/issues/472)
//! that could be built: a lock exists, it survives the trash, it cascades from a
//! sitting to its recordings, and automatic cleanup will not take a recording it
//! protects.
//!
//! The other half — a recording that is being edited — is not here, because
//! nothing opens an edit document for a user to protect. `crate::accounting::cleanup`
//! says so rather than implying it is handled.

use super::*;

use crate::accounting::cleanup;

/// A library with one sitting and two recordings in it.
fn library(name: &str) -> (std::path::PathBuf, Database) {
    let directory = std::env::temp_dir().join(format!(
        "clipped-locks-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory");

    let database = Database::open(directory.join("library.db")).expect("a library can be opened");
    database
        .connection()
        .execute_batch(
            r"
            INSERT INTO sessions (session_id, started_at)
                VALUES ('sitting', '2026-01-01T00:00:00Z');
            INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,
                                    size_bytes)
                VALUES (1, 'sitting', 1, 'one.mkv', '2026-01-01T00:00:00Z', 100);
            INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,
                                    size_bytes)
                VALUES (2, 'sitting', 2, 'two.mkv', '2026-01-01T01:00:00Z', 100);
            ",
        )
        .expect("the fixtures can be written");
    (directory, database)
}

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + core::time::Duration::from_secs(seconds)
}

/// When the recording was locked, as the row holds it.
fn stamp(database: &Database, recording: i64) -> Option<String> {
    database
        .connection()
        .query_row(
            "SELECT locked_at FROM recordings WHERE recording_id = ?1",
            params![recording],
            |row| row.get(0),
        )
        .expect("the row can be read")
}

#[test]
fn a_lock_persists_and_can_be_read_back() {
    let (directory, database) = library("persists");

    for what in [
        Lockable::Session("sitting".to_owned()),
        Lockable::Recording(1),
    ] {
        assert!(
            !is_locked(&database, &what).expect("it can be read"),
            "{what} starts unlocked"
        );
        assert!(
            lock(&database, &what, at(1_000)).expect("it can be locked"),
            "{what} was not locked before"
        );
        assert!(
            is_locked(&database, &what).expect("it can be read"),
            "{what}"
        );

        assert!(
            unlock(&database, &what).expect("it can be unlocked"),
            "{what}"
        );
        assert!(
            !is_locked(&database, &what).expect("it can be read"),
            "{what} is unlocked again"
        );
    }

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn locking_something_already_locked_does_not_move_the_moment_it_was_locked() {
    // "When did you lock this" has one answer, and a second click on a closed
    // padlock is not a new decision.
    let (directory, database) = library("idempotent");
    let what = Lockable::Recording(1);

    assert!(lock(&database, &what, at(1_000)).expect("it locks"));
    let first = stamp(&database, 1).expect("it is locked");

    assert!(
        !lock(&database, &what, at(9_999)).expect("locking again is not an error"),
        "nothing changed, and the caller is told so"
    );
    assert_eq!(stamp(&database, 1).as_deref(), Some(first.as_str()));

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_target_that_is_not_there_locks_nothing_and_is_not_an_error() {
    // The row may have gone between a screen drawing it and somebody clicking
    // it, which is not worth a failure.
    let (directory, database) = library("absent");

    for what in [
        Lockable::Session("gone".to_owned()),
        Lockable::Recording(404),
    ] {
        assert!(
            !lock(&database, &what, at(1_000)).expect("it is not an error"),
            "{what} is not there, so nothing was locked"
        );
        assert!(
            !is_locked(&database, &what).expect("it can be asked"),
            "{what}"
        );
        assert!(
            !unlock(&database, &what).expect("it is not an error"),
            "{what} was not unlocked either"
        );
    }

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn locking_a_sitting_protects_the_recordings_in_it_without_marking_them() {
    // The decision recorded on issue #472, as behaviour: the cascade is real,
    // and it is a cascade of *protection* rather than of marks.
    let (directory, database) = library("cascade");

    lock(
        &database,
        &Lockable::Session("sitting".to_owned()),
        at(1_000),
    )
    .expect("it locks");

    for recording in [1, 2] {
        assert!(
            protects(&database, recording).expect("it can be asked"),
            "recording {recording} is inside a locked sitting"
        );
        assert!(
            !is_locked(&database, &Lockable::Recording(recording)).expect("it can be asked"),
            "recording {recording} has no lock of its own, and unlocking the sitting must not \
             have to find and clear one"
        );
        assert_eq!(
            stamp(&database, recording),
            None,
            "the mark was written down through the children"
        );
    }

    // And releasing the sitting releases them, which is the whole reason the
    // mark is not copied.
    unlock(&database, &Lockable::Session("sitting".to_owned())).expect("it unlocks");
    assert!(!protects(&database, 1).expect("it can be asked"));

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn automatic_cleanup_will_not_take_a_locked_recording_or_one_in_a_locked_sitting() {
    // The point of the column. Each is asserted with the *reason* the sweep
    // gives, because "it was not deleted" is also true of a recording the sweep
    // simply did not reach.
    let (directory, database) = library("cleanup");

    lock(&database, &Lockable::Recording(1), at(1_000)).expect("it locks");
    lock(
        &database,
        &Lockable::Session("sitting".to_owned()),
        at(1_000),
    )
    .expect("it locks");

    let candidates = cleanup::candidates(&database).expect("they can be read");
    let reason = |id: i64| {
        candidates
            .iter()
            .find(|candidate| candidate.item.id == id)
            .unwrap_or_else(|| panic!("recording {id} is a candidate"))
            .protection
    };

    assert_eq!(
        reason(1),
        Some(cleanup::Protection::Locked),
        "a recording with a lock of its own is told so, not told about its sitting"
    );
    assert_eq!(
        reason(2),
        Some(cleanup::Protection::LockedSession),
        "a recording protected by its sitting has no lock of its own to release, so the reason \
         has to say which lock is doing it"
    );
    assert!(
        candidates.iter().all(|candidate| !candidate.is_deletable()),
        "nothing in a locked sitting may be swept"
    );

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_lock_outlives_a_trip_through_the_trash() {
    // A recording that came back unprotected would be a trap: you would have to
    // know to lock it again, and the only sign that you had not would be its
    // absence later. Nothing in this module makes this true — `locked_at` and
    // `deleted_at` are different columns — which is exactly why it is asserted
    // rather than assumed.
    let (directory, database) = library("trash");
    let what = Lockable::Recording(1);

    lock(&database, &what, at(1_000)).expect("it locks");
    let before = stamp(&database, 1).expect("it is locked");

    database
        .connection()
        .execute(
            "UPDATE recordings SET deleted_at = ?1, deleted_from = path WHERE recording_id = 1",
            params!["2026-02-01T00:00:00Z"],
        )
        .expect("it goes to the trash");
    assert!(
        is_locked(&database, &what).expect("it can be asked"),
        "a locked recording in the trash is still locked"
    );

    database
        .connection()
        .execute(
            "UPDATE recordings SET deleted_at = NULL, deleted_from = NULL WHERE recording_id = 1",
            [],
        )
        .expect("it comes back");

    assert!(is_locked(&database, &what).expect("it can be asked"));
    assert_eq!(
        stamp(&database, 1).as_deref(),
        Some(before.as_str()),
        "and it is the same lock, from the same moment, rather than a new one"
    );

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn clearing_a_lock_that_was_never_set_changes_nothing_and_says_so() {
    // `lock` has always guarded on the column as well as the key. `unlock` did
    // not, and an `UPDATE` that writes NULL where NULL already is matches its
    // row and reports one change — so "you unlocked that" was said about
    // something that had never been locked.
    let (directory, database) = library("idempotent-unlock");

    for what in [
        Lockable::Session("sitting".to_owned()),
        Lockable::Recording(1),
    ] {
        assert!(
            !unlock(&database, &what).expect("it is not an error"),
            "{what} was not locked, so nothing changed"
        );
    }

    // And a real unlock still reports one, or the guard above would be
    // satisfied by a function that always answered `false`.
    lock(&database, &Lockable::Recording(1), at(1_000)).expect("it locks");
    assert!(unlock(&database, &Lockable::Recording(1)).expect("it unlocks"));

    let _ = std::fs::remove_dir_all(directory);
}
