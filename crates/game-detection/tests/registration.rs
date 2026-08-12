//! What a user does when detection is wrong, through the public API alone.
//!
//! The unit tests beside each module cover the rules. This covers the thing
//! issue #45 is actually about: that everything a settings screen ([#63],
//! [#107]) needs is reachable from outside this crate, in the order somebody
//! would do it — find that a game is not recognised, register it, see it
//! recognised, decide not to record it after all, and be told why.
//!
//! [#63]: https://github.com/wildware-uk/clipped/issues/63
//! [#107]: https://github.com/wildware-uk/clipped/issues/107

use std::fs;
use std::path::PathBuf;

use clipped_game_detection::catalogue::{
    Catalogue, Match, MatchStrength, Overlay, ProcessCandidate, Registration, Verdict,
    OVERLAY_FILE_NAME,
};

/// A directory of one test's own, removed when it is dropped.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-registration-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory can be created");
        Self(path)
    }

    fn overlay(&self) -> Overlay {
        Overlay::at(self.0.join(OVERLAY_FILE_NAME))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The catalogue as Clipped would load it with this overlay in place.
fn loaded(overlay: &Overlay) -> Catalogue {
    Catalogue::load_with_overlay_at(overlay.path())
        .expect("the catalogue loads")
        .into_catalogue()
}

#[test]
fn a_user_registers_a_game_clipped_does_not_know_and_then_changes_their_mind() {
    let directory = TestDirectory::new("story");
    let overlay = directory.overlay();
    let process = ProcessCandidate::new("obscure-game.exe")
        .with_path(r"D:\Games\Obscure Game\obscure-game.exe");

    // Nothing claims it, and nothing pretends to: no entry is even considered.
    let before = loaded(&overlay);
    assert_eq!(before.match_process(&process), Match::None);
    assert!(before.explain_process(&process).considered().is_empty());

    // The user registers it from a file they picked.
    let registration = Registration::for_executable(std::path::Path::new(
        r"D:\Games\Obscure Game\obscure-game.exe",
    ))
    .expect("the path names a file")
    .named("Obscure Game");
    let game_id = overlay
        .register(&registration)
        .expect("the registration is written");

    // The next time Clipped reads the catalogue — which is every start-up — the
    // process is that game, and the session layer has something to record.
    let after = loaded(&overlay);
    let outcome = after.match_process(&process);
    assert!(
        matches!(
            outcome,
            Match::One {
                strength: MatchStrength::ExecutableName,
                ..
            }
        ),
        "expected the registered game, got {outcome:?}"
    );
    let entry = outcome.entry().expect("exactly one entry");
    assert_eq!(entry.game_id(), &game_id);
    assert_eq!(entry.name(), "Obscure Game");
    assert!(entry.source().is_overlay());

    // They change their mind: this one is not worth recording.
    overlay
        .exclude(game_id.as_str())
        .expect("the exclusion is written");
    let excluded = loaded(&overlay);
    assert_eq!(excluded.match_process(&process), Match::None);

    // And Clipped can say why, which is what makes it a decision rather than a
    // game that mysteriously stopped working.
    let report = excluded.explain_process(&process);
    let considered = report.considered();
    assert_eq!(considered.len(), 1);
    assert_eq!(considered[0].entry().game_id(), &game_id);
    assert_eq!(
        considered[0].verdict(),
        &Verdict::Excluded(MatchStrength::ExecutableName)
    );

    // The entry is still there to be shown, excluded rather than gone.
    let entry = excluded
        .find_by_id(game_id.as_str())
        .expect("an exclusion is not a deletion");
    assert!(entry.is_excluded());
}

#[test]
fn a_shipped_game_can_be_renamed_and_the_rename_read_back() {
    // The same flow against a game Clipped ships, which is the case that must
    // not be stored as a copy of the shipped entry: `renamed_from` is what a
    // screen offers as "reset", and it comes from the shipped data every time
    // the catalogue is read, not from what was shipped when the user typed it.
    let directory = TestDirectory::new("rename");
    let overlay = directory.overlay();
    let shipped = Catalogue::seed().expect("the shipped data is valid");
    let first = shipped.entries().first().expect("the seed data has games");
    let (game_id, name) = (first.game_id().as_str().to_owned(), first.name().to_owned());

    overlay
        .rename(&game_id, "What I Call It")
        .expect("the rename is written");

    let renamed = loaded(&overlay);
    let entry = renamed.find_by_id(&game_id).expect("still catalogued");
    assert_eq!(entry.name(), "What I Call It");
    assert_eq!(entry.renamed_from(), Some(name.as_str()));
    assert!(
        !entry.source().is_overlay(),
        "the entry is still Clipped's; only the decision about it is the user's"
    );

    overlay
        .clear_rename(&game_id)
        .expect("the rename is cleared");
    assert_eq!(
        loaded(&overlay)
            .find_by_id(&game_id)
            .expect("still catalogued")
            .name(),
        name
    );
}
