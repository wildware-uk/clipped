//! What reconciling a large library costs, measured rather than asserted.
//!
//! Issue #56 asks for a documented library size and a measured time. This is
//! how that figure is produced, and it is an example rather than a test because
//! it builds tens of thousands of files: a `cargo test` that took two minutes
//! and filled a disk would be a test nobody runs (AGENTS.md section 25).
//!
//! ```text
//! cargo run --release -p clipped-library --example index_cost
//! cargo run --release -p clipped-library --example index_cost -- 5000 D:\scratch
//! ```
//!
//! The arguments are the number of sessions and where to build them. The
//! default is **2,000 sessions and 3,000 recordings**, which is a library of
//! somebody who has recorded every evening for five years.
//!
//! Four runs are timed, because they are four different costs:
//!
//! | Run | What it measures |
//! | --- | --- |
//! | First | Reading every sidecar and writing every row |
//! | Second | The steady state: a start-up with nothing to do |
//! | After deleting a tenth of the files | Noticing what has gone |
//! | After putting them back | Noticing what has returned |
//!
//! The files are empty. That is deliberate: this measures the index, and the
//! size of a recording changes nothing about it — reconciliation never opens a
//! media file (`docs/storage.md`). What it does not measure is a cold
//! filesystem cache, which on a mechanical disk would dominate the walk.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use clipped_library::index::{
    game_summaries, reconcile, IndexControl, IndexPace, IndexReport, IndexSettings,
};
use clipped_storage::Database;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let sessions: usize = arguments
        .next()
        .map_or(2_000, |count| count.parse().expect("a number of sessions"));
    let directory = arguments.next().map_or_else(
        || std::env::temp_dir().join("clipped-index-cost"),
        PathBuf::from,
    );

    let root = directory.join("clips");
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&root).expect("the library can be created");

    println!("building {sessions} sessions in {}", root.display());
    let built = Instant::now();
    let recordings = build(&root, sessions);
    println!(
        "built {sessions} sessions and {recordings} recordings in {:.2?}\n",
        built.elapsed()
    );

    let mut database = Database::open(directory.join("library.db")).expect("a database");
    let settings = IndexSettings {
        // The measurement is of the work, not of the resting: the background
        // pace deliberately sleeps between batches and would measure the sleep.
        pace: IndexPace::foreground(),
        ..IndexSettings::new([root.clone()])
    };
    let control = IndexControl::new();

    let first = run(&mut database, &settings, &control, "first run");
    assert_eq!(first.sessions_indexed, sessions);
    run(
        &mut database,
        &settings,
        &control,
        "second run, nothing changed",
    );

    let removed = remove_every_tenth(&root, sessions);
    let missing = run(&mut database, &settings, &control, "after deleting a tenth");
    assert_eq!(missing.recordings_newly_missing, removed);

    restore_every_tenth(&root, sessions);
    let returned = run(
        &mut database,
        &settings,
        &control,
        "after putting them back",
    );
    assert_eq!(returned.recordings_returned, removed);

    // And the pace that actually ships. It is slower on purpose: the difference
    // between the two lines is the time this run spends deliberately out of a
    // recording's way (`IndexPace::background`).
    let background = IndexSettings::new([root.clone()]);
    run(
        &mut database,
        &background,
        &control,
        "again, at the background pace",
    );

    let summaries = game_summaries(&database).expect("the games view can be built");
    println!(
        "\n{} games, {} sessions, {} recordings, {} bytes",
        summaries.len(),
        summaries.iter().map(|game| game.sessions).sum::<u64>(),
        summaries.iter().map(|game| game.recordings).sum::<u64>(),
        summaries.iter().map(|game| game.bytes).sum::<u64>(),
    );
    println!(
        "the library is left in {} — delete it when you are done",
        directory.display()
    );
}

fn run(
    database: &mut Database,
    settings: &IndexSettings,
    control: &IndexControl,
    label: &str,
) -> IndexReport {
    let report = reconcile(database, settings, control, SystemTime::now())
        .unwrap_or_else(|error| panic!("{label} failed: {error}"));
    println!(
        "{label:34} {:>9.2?}  {:>4} transactions, longest {:>8.2?}  \
         {:>5} missing, {:>5} returned",
        report.duration,
        report.transactions,
        report.longest_transaction,
        report.recordings_newly_missing,
        report.recordings_returned,
    );
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    report
}

/// Builds `sessions` sidecars, half of them with a second recording, spread
/// over twenty games.
fn build(root: &Path, sessions: usize) -> usize {
    let mut recordings = 0;
    for index in 0..sessions {
        let game = index % 20;
        let session_id = format!("game-{game:02}-2026{:04}-{index:06}", index % 1_000);
        let mut outputs = vec![format!("clipped-{session_id}.mkv")];
        if index % 2 == 0 {
            outputs.push(format!("clipped-{session_id}-2.mkv"));
        }
        recordings += outputs.len();

        let written: Vec<String> = outputs
            .iter()
            .enumerate()
            .map(|(ordinal, name)| {
                fs::write(root.join(name), []).expect("a recording can be written");
                format!(
                    r#"{{"index": {}, "output": {}, "started_at": "2026-08-11T14:32:05+01:00",
                        "ended_at": "2026-08-11T14:50:13+01:00", "outcome": "recorded",
                        "frames_encoded": 65040, "duration_seconds": 1084.0,
                        "width": 2560, "height": 1440, "end_reason": "target-lost"}}"#,
                    ordinal + 1,
                    serde_json::to_string(&root.join(name).display().to_string())
                        .expect("a path encodes"),
                )
            })
            .collect();

        let sidecar = format!(
            r#"{{
              "schema_version": 1,
              "session_id": "{session_id}",
              "game": {{"kind": "known", "game_id": "game-{game:02}", "name": "Game {game}"}},
              "started_at": "2026-08-11T14:32:05+01:00",
              "ended_at": "2026-08-11T15:31:21+01:00",
              "recordings": [{}],
              "clips": [], "bookmarks": [],
              "events": [
                {{"at": "2026-08-11T14:32:05+01:00", "event": "session-started", "pid": 4242,
                  "image_name": "game.exe"}},
                {{"at": "2026-08-11T14:32:09+01:00", "event": "recording-started", "index": 1}},
                {{"at": "2026-08-11T14:50:13+01:00", "event": "recording-ended", "index": 1,
                  "outcome": "recorded"}},
                {{"at": "2026-08-11T15:31:21+01:00", "event": "session-ended",
                  "reason": "game-exited"}}
              ]
            }}"#,
            written.join(", ")
        );
        fs::write(
            root.join(format!("clipped-{session_id}.session.json")),
            sidecar,
        )
        .expect("a sidecar can be written");
    }
    recordings
}

/// Deletes the first recording of every tenth session, as a user clearing space
/// in Explorer would.
fn remove_every_tenth(root: &Path, sessions: usize) -> usize {
    every_tenth(root, sessions, |path| {
        fs::remove_file(path).expect("a recording can be deleted");
    })
}

fn restore_every_tenth(root: &Path, sessions: usize) -> usize {
    every_tenth(root, sessions, |path| {
        fs::write(path, []).expect("a recording can be restored");
    })
}

fn every_tenth(root: &Path, sessions: usize, act: impl Fn(&Path)) -> usize {
    let mut count = 0;
    for index in (0..sessions).step_by(10) {
        let game = index % 20;
        let session_id = format!("game-{game:02}-2026{:04}-{index:06}", index % 1_000);
        act(&root.join(format!("clipped-{session_id}.mkv")));
        count += 1;
    }
    count
}
