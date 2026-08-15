//! What automatic cleanup deletes, and everything it refuses to.
//!
//! The acceptance criteria of
//! [issue #111](https://github.com/wildware-uk/clipped/issues/111) are here:
//! every protection rule including combinations, a simulated full disk that
//! deletes only what the rules permit, and a deletion that goes to the trash
//! with a reason.
//!
//! [`plan`] takes its candidates rather than reading them, so almost all of this
//! is exact arithmetic over a list. The one test that needs a database is the
//! one that reads real protections out of one.

use super::*;

/// A gigabyte, so that the arithmetic below reads as sizes rather than digits.
const GIB: u64 = 1024 * 1024 * 1024;

/// A fixed present, so that ages are exact rather than nearly right.
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// A recording that started `days` before [`now`].
fn recording(id: i64, days: u64, size_bytes: u64) -> Candidate {
    let started = now() - Duration::from_secs(days * 24 * 60 * 60);
    let stamp = time::OffsetDateTime::from_unix_timestamp(
        i64::try_from(
            started
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("after the epoch")
                .as_secs(),
        )
        .expect("a real time"),
    )
    .expect("a real time")
    .format(&time::format_description::well_known::Rfc3339)
    .expect("it formats");

    Candidate {
        item: TrashItem::recording(id),
        path: PathBuf::from(format!(r"D:\Clipped\{id}.mkv")),
        size_bytes,
        started_at: stamp,
        protection: None,
    }
}

/// The same, protected.
fn protected(id: i64, days: u64, size_bytes: u64, protection: Protection) -> Candidate {
    Candidate {
        protection: Some(protection),
        ..recording(id, days, size_bytes)
    }
}

/// A quota of `gib` and nothing else.
fn quota(gib: u64) -> StorageLimits {
    StorageLimits::unlimited()
        .with_maximum_usage(gib * GIB)
        .expect("a real quota")
}

fn ids(candidates: &[Candidate]) -> Vec<i64> {
    candidates
        .iter()
        .map(|candidate| candidate.item.id)
        .collect()
}

#[test]
fn nothing_is_deleted_when_no_limit_is_breached() {
    let plan = plan(
        &quota(100),
        vec![recording(1, 30, 10 * GIB), recording(2, 1, 10 * GIB)],
        20 * GIB,
        500 * GIB,
        now(),
    );

    assert!(plan.is_empty(), "a library inside its quota loses nothing");
    assert_eq!(plan.reclaimed_bytes, 0);
    assert_eq!(plan.still_over_limit, 0);
    assert_eq!(ids(&plan.protected).len(), 2, "and both are accounted for");
}

#[test]
fn the_oldest_go_first_and_only_as_many_as_the_limit_needs() {
    // The newest recording is the one somebody is most likely to want, so a
    // sweep that took it while an older one survived would be the wrong way
    // round — and one that took everything would be worse.
    let plan = plan(
        &quota(30),
        vec![
            recording(3, 1, 10 * GIB),
            recording(1, 30, 10 * GIB),
            recording(2, 15, 10 * GIB),
        ],
        50 * GIB,
        500 * GIB,
        now(),
    );

    assert_eq!(
        ids(&plan.deletions),
        vec![1, 2],
        "oldest first, and stopping once the quota is met"
    );
    assert_eq!(ids(&plan.protected), vec![3], "the newest survives");
    assert_eq!(plan.reclaimed_bytes, 20 * GIB);
    assert_eq!(plan.still_over_limit, 0);
}

#[test]
fn a_favourite_is_never_taken_however_old_it_is() {
    let plan = plan(
        &quota(10),
        vec![
            protected(1, 365, 10 * GIB, Protection::Favourite),
            recording(2, 1, 10 * GIB),
        ],
        20 * GIB,
        500 * GIB,
        now(),
    );

    assert_eq!(
        ids(&plan.deletions),
        vec![2],
        "the favourite is not a candidate"
    );
    assert_eq!(ids(&plan.protected), vec![1]);
}

#[test]
fn a_recording_clips_were_cut_from_is_never_taken() {
    let plan = plan(
        &quota(10),
        vec![
            protected(1, 365, 10 * GIB, Protection::SourceOfClips { clips: 3 }),
            recording(2, 1, 10 * GIB),
        ],
        20 * GIB,
        500 * GIB,
        now(),
    );

    assert_eq!(ids(&plan.deletions), vec![2]);
    assert_eq!(
        plan.protected[0].protection,
        Some(Protection::SourceOfClips { clips: 3 }),
        "and the count is kept, so the message can say what would be orphaned"
    );
}

#[test]
fn every_protection_survives_a_disk_that_is_completely_full() {
    // The combination the issue asks for, in the worst case: the limit cannot
    // be met without deleting something protected, and nothing protected is
    // deleted anyway.
    let plan = plan(
        &quota(10),
        vec![
            protected(1, 365, 10 * GIB, Protection::Favourite),
            protected(2, 300, 10 * GIB, Protection::SourceOfClips { clips: 1 }),
            protected(3, 200, 10 * GIB, Protection::AlreadyDeleted),
            protected(4, 100, 10 * GIB, Protection::Missing),
        ],
        50 * GIB,
        0,
        now(),
    );

    assert!(
        plan.is_empty(),
        "a full disk is not a reason to delete something protected"
    );
    assert_eq!(plan.protected.len(), 4);
    assert!(
        plan.still_over_limit > 0,
        "and the caller has to be told the limit is still over rather than that it worked"
    );
}

#[test]
fn a_recording_past_the_maximum_age_goes_even_when_there_is_room() {
    // A maximum age is not a size limit: it means what it says whether or not
    // the disk is full.
    let limits = StorageLimits::unlimited()
        .with_maximum_age(Duration::from_secs(30 * 24 * 60 * 60))
        .expect("a real age");

    let plan = plan(
        &limits,
        vec![recording(1, 60, GIB), recording(2, 5, GIB)],
        2 * GIB,
        500 * GIB,
        now(),
    );

    assert_eq!(
        ids(&plan.deletions),
        vec![1],
        "the old one, and not the recent one"
    );
    assert_eq!(ids(&plan.protected), vec![2]);
}

#[test]
fn an_old_favourite_is_still_a_favourite() {
    // The combination that would be easiest to get wrong: the age rule runs
    // before the size rule, so a protection that only guarded the size path
    // would let this one through.
    let limits = StorageLimits::unlimited()
        .with_maximum_age(Duration::from_secs(30 * 24 * 60 * 60))
        .expect("a real age");

    let plan = plan(
        &limits,
        vec![protected(1, 3650, GIB, Protection::Favourite)],
        GIB,
        500 * GIB,
        now(),
    );

    assert!(
        plan.is_empty(),
        "ten years old and still not automatic to delete"
    );
}

#[test]
fn a_timestamp_that_cannot_be_read_is_never_treated_as_old() {
    // The worst possible reading of an unreadable field would be "delete it".
    // A day is the shortest maximum age the configuration allows.
    let limits = StorageLimits::unlimited()
        .with_maximum_age(Duration::from_secs(24 * 60 * 60))
        .expect("a real age");
    let mut broken = recording(1, 3650, GIB);
    broken.started_at = "not a timestamp".to_owned();

    let plan = plan(&limits, vec![broken], GIB, 500 * GIB, now());

    assert!(
        plan.is_empty(),
        "a recording whose age cannot be read is not a recording to delete"
    );
}

#[test]
fn the_free_space_limit_frees_as_much_as_the_quota_would() {
    // The two size limits are independent, and whichever asks for more wins.
    let limits = StorageLimits::unlimited().with_minimum_free_space(50 * GIB);

    assert_eq!(excess(&limits, 100 * GIB, 10 * GIB), 40 * GIB);
    assert_eq!(
        excess(&limits, 100 * GIB, 500 * GIB),
        0,
        "a volume with room asks for nothing"
    );

    let both = quota(60).with_minimum_free_space(50 * GIB);
    assert_eq!(
        excess(&both, 100 * GIB, 10 * GIB),
        40 * GIB,
        "the larger of the two is what has to be freed"
    );
}

#[test]
fn an_unlimited_library_never_deletes_anything() {
    let plan = plan(
        &StorageLimits::unlimited(),
        vec![recording(1, 3650, 500 * GIB)],
        500 * GIB,
        0,
        now(),
    );

    assert!(
        plan.is_empty(),
        "no limit means no limit, however old or large or full"
    );
}

#[test]
fn the_protections_are_read_out_of_a_real_database() {
    // The half the arithmetic above cannot check: that the query produces the
    // protections the rules act on. Everything else here hands `plan` a list.
    let directory = std::env::temp_dir().join(format!(
        "clipped-cleanup-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory");
    let database = Database::open(directory.join("library.db")).expect("a library can be opened");

    let connection = database.connection();
    connection
        .execute_batch(
            r"
            INSERT INTO sessions (session_id, started_at) VALUES ('s', '2020-01-01T00:00:00Z');
            INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,
                                    size_bytes)
                VALUES (1, 's', 1, 'D:\plain.mkv', '2020-01-01T00:00:00Z', 100);
            INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,
                                    size_bytes, favourited_at)
                VALUES (2, 's', 2, 'D:\loved.mkv', '2020-01-02T00:00:00Z', 100,
                        '2020-02-01T00:00:00Z');
            INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,
                                    size_bytes)
                VALUES (3, 's', 3, 'D:\cut-from.mkv', '2020-01-03T00:00:00Z', 100);
            INSERT INTO clips (clip_id, source_recording_id, path, created_at)
                VALUES (1, 3, 'D:\clip.mkv', '2020-03-01T00:00:00Z');
            INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,
                                    size_bytes, missing_since)
                VALUES (4, 's', 4, 'D:\gone.mkv', '2020-01-04T00:00:00Z', 100,
                        '2020-04-01T00:00:00Z');
            ",
        )
        .expect("the fixtures can be written");

    let found = candidates(&database).expect("the candidates can be read");
    let by_id = |id: i64| {
        found
            .iter()
            .find(|candidate| candidate.item.id == id)
            .unwrap_or_else(|| panic!("recording {id} is missing"))
    };

    assert_eq!(
        by_id(1).protection,
        None,
        "an ordinary recording is a candidate"
    );
    assert_eq!(by_id(2).protection, Some(Protection::Favourite));
    assert_eq!(
        by_id(3).protection,
        Some(Protection::SourceOfClips { clips: 1 }),
        "the clip cut from it is what protects it"
    );
    assert_eq!(by_id(4).protection, Some(Protection::Missing));

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn a_deletion_goes_to_the_trash_and_can_be_restored_from_it() {
    // The third acceptance criterion, and the reason this feature is safe to
    // have at all: nothing is unlinked. Every automatic deletion is a move that
    // the person it happened to can undo.
    let directory = std::env::temp_dir().join(format!(
        "clipped-cleanup-apply-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory");

    let media = directory.join("old.mkv");
    std::fs::write(&media, b"footage").expect("a recording to delete");

    let mut database =
        Database::open(directory.join("library.db")).expect("a library can be opened");
    let connection = database.connection();
    connection
        .execute(
            "INSERT INTO sessions (session_id, started_at) VALUES ('s', '2020-01-01T00:00:00Z')",
            [],
        )
        .expect("a session can be written");
    // The path is bound rather than interpolated: a Windows path is full of
    // backslashes, and a test that escaped them by hand would be testing its own
    // escaping.
    connection
        .execute(
            "INSERT INTO recordings (recording_id, session_id, session_index, path, started_at,                                      size_bytes)              VALUES (1, 's', 1, ?1, '2020-01-01T00:00:00Z', 7)",
            [media.display().to_string()],
        )
        .expect("the fixture can be written");

    let trash = crate::trash::Trash::new(directory.join("trash"));
    let found = candidates(&database).expect("the candidates can be read");
    assert_eq!(found.len(), 1);
    assert!(found[0].is_deletable(), "nothing protects this one");

    let plan = plan(&quota(1), found, 10 * GIB, 500 * GIB, now());
    assert_eq!(plan.deletions.len(), 1, "the quota is over, so it goes");

    let outcome = apply(&plan, &trash, &mut database, now()).expect("the sweep runs");

    assert_eq!(outcome.deleted, vec![TrashItem::recording(1)]);
    assert!(outcome.refused.is_empty(), "{:?}", outcome.refused);
    assert!(
        !media.exists(),
        "the file is moved out of the library, which is what reclaims the space"
    );

    let entries = trash.list(&database).expect("the trash can be listed");
    assert_eq!(entries.len(), 1, "and it is in the trash rather than gone");

    trash
        .restore(&mut database, TrashItem::recording(1))
        .expect("an automatic deletion can be undone");
    assert!(
        media.exists(),
        "restoring is what makes automatic deletion safe to have at all"
    );

    let _ = std::fs::remove_dir_all(&directory);
}
