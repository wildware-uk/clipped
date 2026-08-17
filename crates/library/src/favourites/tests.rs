//! Marking things, unmarking them, and what a favourited session means.
//!
//! The acceptance criteria of
//! [issue #58](https://github.com/wildware-uk/clipped/issues/58): that a
//! favourite persists and is visible to the things that read a library, that
//! automatic cleanup skips one, and that favouriting a session covers its
//! recordings in the way the module documents.

use super::*;

use crate::accounting::cleanup;
use crate::test_support::Scratch;

/// A library with one session, two recordings in it and a clip.
///
/// The directory comes back first so that it is dropped last: the database has
/// the file inside it open, and Windows will not remove a file that is.
fn library(name: &str) -> (Scratch, Database) {
    let directory = Scratch::new(&format!("favourites-{name}"));

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
            INSERT INTO clips (clip_id, path, created_at)
                VALUES (1, 'clip.mkv', '2026-01-02T00:00:00Z');
            ",
        )
        .expect("the fixtures can be written");
    (directory, database)
}

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + core::time::Duration::from_secs(seconds)
}

#[test]
fn a_mark_persists_and_can_be_read_back() {
    let (_directory, database) = library("persists");

    for what in [
        Favourite::Session("sitting".to_owned()),
        Favourite::Recording(1),
        Favourite::Clip(1),
    ] {
        assert!(
            !is_marked(&database, &what).expect("it can be read"),
            "{what} starts unmarked"
        );
        assert!(mark(&database, &what, at(1_800_000_000)).expect("it can be marked"));
        assert!(
            is_marked(&database, &what).expect("it can be read"),
            "{what} is marked afterwards"
        );
    }
}

#[test]
fn marking_something_twice_keeps_the_instant_it_was_first_marked() {
    // "When did you favourite this" answers when it was first favourited. A
    // second click on a full star must not quietly change it.
    let (_directory, database) = library("twice");
    let what = Favourite::Recording(1);

    assert!(mark(&database, &what, at(1_000_000_000)).expect("it can be marked"));
    assert!(
        !mark(&database, &what, at(1_900_000_000)).expect("a second mark is not an error"),
        "nothing changed, and that is what the answer says"
    );

    let stamp: String = database
        .connection()
        .query_row(
            "SELECT favourited_at FROM recordings WHERE recording_id = 1",
            [],
            |row| row.get(0),
        )
        .expect("the mark is there");
    assert!(
        stamp.starts_with("2001-"),
        "the first instant survives the second mark: {stamp}"
    );
}

#[test]
fn unmarking_clears_it_and_unmarking_nothing_is_not_a_failure() {
    let (_directory, database) = library("unmark");
    let what = Favourite::Clip(1);

    assert!(mark(&database, &what, at(1_800_000_000)).expect("it can be marked"));
    assert!(unmark(&database, &what).expect("it can be unmarked"));
    assert!(!is_marked(&database, &what).expect("it can be read"));

    let missing = Favourite::Clip(9_999);
    assert!(
        !unmark(&database, &missing).expect("a row that is not there is not an error"),
        "nothing was changed, and the answer says so"
    );
    assert!(!is_marked(&database, &missing).expect("nor is reading one"));
}

#[test]
fn automatic_cleanup_skips_a_favourite() {
    // The issue's second acceptance criterion, from this side: enforced in
    // `accounting::cleanup` and asserted here against a real mark rather than a
    // hand-built candidate.
    let (_directory, database) = library("cleanup");
    mark(&database, &Favourite::Recording(1), at(1_800_000_000)).expect("it can be marked");

    let candidates = cleanup::candidates(&database).expect("the candidates can be read");
    let marked = candidates
        .iter()
        .find(|candidate| candidate.item.id == 1)
        .expect("the favourite is still a row");
    let other = candidates
        .iter()
        .find(|candidate| candidate.item.id == 2)
        .expect("the other recording is too");

    assert_eq!(marked.protection, Some(cleanup::Protection::Favourite));
    assert!(
        !marked.is_deletable(),
        "a favourite is not automatic to delete"
    );
    assert!(other.is_deletable(), "and the one beside it still is");
}

#[test]
fn favouriting_a_session_protects_its_recordings_without_marking_them() {
    // The third acceptance criterion, and the rule the module documents. Both
    // halves matter: the recordings are protected, and they are *not*
    // individually marked — so unfavouriting the sitting leaves no trail.
    let (_directory, database) = library("session");
    mark(
        &database,
        &Favourite::Session("sitting".to_owned()),
        at(1_800_000_000),
    )
    .expect("a sitting can be marked");

    let candidates = cleanup::candidates(&database).expect("the candidates can be read");
    assert_eq!(candidates.len(), 2);
    for candidate in &candidates {
        assert_eq!(
            candidate.protection,
            Some(cleanup::Protection::FavouriteSession),
            "every recording in a favourited sitting is protected: {}",
            candidate.item
        );
    }

    assert!(
        !is_marked(&database, &Favourite::Recording(1)).expect("it can be read"),
        "the mark is the sitting's, not written down through its children"
    );

    // And undoing it gives the recordings back.
    assert!(unmark(&database, &Favourite::Session("sitting".to_owned()))
        .expect("a sitting can be unmarked"));
    let after = cleanup::candidates(&database).expect("the candidates can be read");
    assert!(
        after.iter().all(cleanup::Candidate::is_deletable),
        "unfavouriting the sitting releases its recordings"
    );
}

#[test]
fn a_recording_marked_in_its_own_right_survives_the_session_being_unfavourited() {
    // The case the cascade was rejected for. Somebody favourites a sitting,
    // then favourites one recording in it deliberately, then unfavourites the
    // sitting — and expects that one recording to still be theirs.
    let (_directory, database) = library("own-right");
    mark(
        &database,
        &Favourite::Session("sitting".to_owned()),
        at(1_800_000_000),
    )
    .expect("a sitting can be marked");
    mark(&database, &Favourite::Recording(2), at(1_800_000_100)).expect("and one recording in it");

    unmark(&database, &Favourite::Session("sitting".to_owned()))
        .expect("a sitting can be unmarked");

    let candidates = cleanup::candidates(&database).expect("the candidates can be read");
    let by_id = |id: i64| {
        candidates
            .iter()
            .find(|candidate| candidate.item.id == id)
            .unwrap_or_else(|| panic!("recording {id} is missing"))
    };

    assert!(by_id(1).is_deletable(), "the one nobody marked is released");
    assert_eq!(
        by_id(2).protection,
        Some(cleanup::Protection::Favourite),
        "and the one they marked themselves is still theirs"
    );
}

#[test]
fn clearing_a_mark_that_was_never_set_changes_nothing_and_says_so() {
    // The same defect `locks::unlock` had, and it reaches a window: `changed`
    // on a `favourited` reply is what tells "you did that" from "that was
    // already so", and unmarking something unmarked claimed the first.
    let (_directory, database) = library("idempotent-unmark");

    for what in [
        Favourite::Session("sitting".to_owned()),
        Favourite::Recording(1),
        Favourite::Clip(1),
    ] {
        assert!(
            !unmark(&database, &what).expect("it is not an error"),
            "{what} was not marked, so nothing changed"
        );
    }

    mark(&database, &Favourite::Recording(1), at(1_000)).expect("it marks");
    assert!(
        unmark(&database, &Favourite::Recording(1)).expect("it unmarks"),
        "a real clearing still reports one, or the guard above would be satisfied by a function \
         that always answered `false`"
    );
}
