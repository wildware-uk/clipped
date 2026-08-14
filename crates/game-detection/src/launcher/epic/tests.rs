//! Reading Epic's manifests, against real files on a real disk.
//!
//! Every case writes the manifests it is about into a directory of its own and
//! points [`Epic::read_at`] at it. Nothing here needs the Epic launcher
//! installed, which is the third acceptance criterion of
//! [issue #44](https://github.com/wildware-uk/clipped/issues/44): a provider
//! whose behaviour could only be checked on a machine that has the launcher is
//! one nobody can change safely (AGENTS.md section 25).

use std::fs;
use std::path::PathBuf;

use super::*;
use crate::catalogue::MatchStrength;

/// An empty directory of this test's own, removed first if it survived.
fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "clipped-epic-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("a scratch directory can be made");
    directory
}

/// Writes a manifest as the launcher writes them: PascalCase, and carrying
/// more than this crate reads.
///
/// Both paths are escaped, because both hold Windows separators and a lone
/// backslash is not valid JSON — which is what the launcher's own files do and
/// what the first version of this helper got wrong for `LaunchExecutable`.
fn manifest(directory: &Path, file: &str, app: &str, name: &str, install: &str, exe: &str) {
    let text = format!(
        r#"{{
  "FormatVersion": 0,
  "bIsIncompleteInstall": false,
  "AppName": "{app}",
  "DisplayName": "{name}",
  "InstallLocation": "{install}",
  "LaunchExecutable": "{exe}",
  "CatalogNamespace": "fn",
  "CatalogItemId": "4fe75bbc5a674f4f9b356b5c90567da5"
}}"#,
        install = install.replace('\\', r"\\"),
        exe = exe.replace('\\', r"\\")
    );
    fs::write(directory.join(file), text).expect("a manifest can be written");
}

#[test]
fn an_installed_application_is_read_out_of_its_manifest() {
    let directory = scratch("installed");
    manifest(
        &directory,
        "Fortnite.item",
        "Fortnite",
        "Fortnite",
        r"D:\Epic\Fortnite",
        r"FortniteGame\Binaries\Win64\FortniteClient-Win64-Shipping.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");

    assert!(epic.problems().is_empty(), "{:?}", epic.problems());
    assert_eq!(epic.apps().len(), 1);
    let app = &epic.apps()[0];
    assert_eq!(app.app_name(), "Fortnite");
    assert_eq!(app.name(), "Fortnite");
    assert_eq!(app.installation_directory(), Path::new(r"D:\Epic\Fortnite"));
    assert_eq!(
        app.executable(),
        r"FortniteGame\Binaries\Win64\FortniteClient-Win64-Shipping.exe"
    );
    assert_eq!(app.manifest(), directory.join("Fortnite.item"));

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_running_executable_under_an_installation_is_claimed_as_that_application() {
    // The join this module exists to produce: a path becomes a launcher
    // identity, which is the catalogue's strongest rung.
    let directory = scratch("claim");
    manifest(
        &directory,
        "Fortnite.item",
        "Fortnite",
        "Fortnite",
        r"D:\Epic\Fortnite",
        r"FortniteGame\Binaries\Win64\FortniteClient-Win64-Shipping.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");
    let candidate = epic.candidate_for(
        "FortniteClient-Win64-Shipping.exe",
        r"D:\Epic\Fortnite\FortniteGame\Binaries\Win64\FortniteClient-Win64-Shipping.exe",
    );

    assert_eq!(
        candidate.launcher(),
        Some((LauncherKind::Epic, "Fortnite")),
        "a process inside an Epic installation has to reach the catalogue as one"
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_process_epic_does_not_know_about_carries_no_launcher_identity() {
    // The half that keeps this from making detection worse: an absent identity
    // is what makes the catalogue fall back to its path and name rungs, so a
    // game installed without Epic matches exactly as well as it did before.
    let directory = scratch("stranger");
    manifest(
        &directory,
        "Fortnite.item",
        "Fortnite",
        "Fortnite",
        r"D:\Epic\Fortnite",
        "x.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");
    let candidate = epic.candidate_for("cs2.exe", r"D:\Steam\steamapps\common\cs2\game\cs2.exe");

    assert_eq!(candidate.launcher(), None);

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn the_deepest_installation_claims_a_path_inside_two_of_them() {
    // One installation directory can sit inside another, and the more specific
    // answer is the right one. Without the depth comparison this would answer
    // whichever manifest `read_dir` happened to produce first.
    let directory = scratch("nested");
    manifest(
        &directory,
        "Outer.item",
        "Outer",
        "Outer",
        r"D:\Epic",
        "outer.exe",
    );
    manifest(
        &directory,
        "Inner.item",
        "Inner",
        "Inner",
        r"D:\Epic\Inner",
        "inner.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");
    let app = epic
        .app_for_path(r"D:\Epic\Inner\Binaries\inner.exe")
        .expect("something claims it");

    assert_eq!(app.app_name(), "Inner");

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn an_installation_directory_itself_is_not_a_program_in_it() {
    let directory = scratch("directory-only");
    manifest(
        &directory,
        "Fortnite.item",
        "Fortnite",
        "Fortnite",
        r"D:\Epic\Fortnite",
        "x.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");

    assert!(epic.app_for_path(r"D:\Epic\Fortnite").is_none());

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_game_that_is_owned_and_not_installed_is_skipped_without_being_a_problem() {
    // Epic writes a manifest for an entitlement. Reporting each one would put
    // every free-giveaway game a user has ever claimed on the diagnostics
    // screen as though something were wrong.
    let directory = scratch("entitlement");
    manifest(
        &directory,
        "Owned.item",
        "Owned",
        "Owned but never installed",
        "",
        "",
    );
    manifest(
        &directory,
        "Fortnite.item",
        "Fortnite",
        "Fortnite",
        r"D:\Epic\Fortnite",
        "x.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");

    assert_eq!(epic.apps().len(), 1, "{:?}", epic.apps());
    assert_eq!(epic.apps()[0].app_name(), "Fortnite");
    assert!(
        epic.problems().is_empty(),
        "an entitlement is not a fault: {:?}",
        epic.problems()
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn one_unreadable_manifest_does_not_cost_the_user_every_other_game() {
    // The property that makes this safe to run on a real machine: an update
    // interrupted half way through leaves a file that is not JSON, and the
    // other twenty games must still be detected.
    let directory = scratch("half-written");
    fs::write(directory.join("Broken.item"), "{ \"AppName\": ").expect("it can be written");
    manifest(
        &directory,
        "Fortnite.item",
        "Fortnite",
        "Fortnite",
        r"D:\Epic\Fortnite",
        "x.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");

    assert_eq!(epic.apps().len(), 1);
    assert_eq!(epic.apps()[0].app_name(), "Fortnite");
    assert_eq!(epic.problems().len(), 1);
    assert_eq!(epic.problems()[0].path(), directory.join("Broken.item"));
    assert!(
        epic.problems()[0].to_string().contains("Broken.item"),
        "a problem has to name the file: {}",
        epic.problems()[0]
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_file_that_is_not_a_manifest_is_ignored_rather_than_reported() {
    // The directory holds more than `.item` files. Reading them and reporting
    // that they are not manifests would be a problem this invented.
    let directory = scratch("other-files");
    fs::write(directory.join("notes.txt"), "not a manifest").expect("it can be written");
    manifest(
        &directory,
        "Fortnite.item",
        "Fortnite",
        "Fortnite",
        r"D:\Epic\Fortnite",
        "x.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");

    assert_eq!(epic.apps().len(), 1);
    assert!(epic.problems().is_empty(), "{:?}", epic.problems());

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_manifest_with_no_identifier_says_which_field_is_missing() {
    let directory = scratch("no-identifier");
    manifest(&directory, "Odd.item", "", "Odd", r"D:\Epic\Odd", "x.exe");

    let epic = Epic::read_at(&directory).expect("the directory is there");

    assert!(epic.apps().is_empty());
    assert_eq!(epic.problems().len(), 1);
    assert!(
        epic.problems()[0].to_string().contains("AppName"),
        "the message has to name the field: {}",
        epic.problems()[0]
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_manifest_with_no_display_name_falls_back_to_its_identifier() {
    // So that nothing downstream has to draw a nameless row.
    let directory = scratch("no-name");
    manifest(
        &directory,
        "Odd.item",
        "OddApp",
        "",
        r"D:\Epic\Odd",
        "x.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");

    assert_eq!(epic.apps().len(), 1);
    assert_eq!(epic.apps()[0].name(), "OddApp");

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_directory_that_is_not_there_is_refused_by_name() {
    let missing = std::env::temp_dir().join("clipped-epic-no-such-directory");
    let _ = fs::remove_dir_all(&missing);

    let error = Epic::read_at(&missing).expect_err("there is nothing to read");

    assert!(matches!(error, EpicError::MissingRoot { .. }));
    assert_eq!(error.path(), missing);
}

#[test]
fn the_launcher_rung_is_the_strongest_the_catalogue_has() {
    // Not a test of this module so much as of why it exists: an identity from
    // here outranks a match on a path or a file name, which is what lets a
    // catalogue entry find a game whose process is called something generic.
    assert!(MatchStrength::LauncherIdentity > MatchStrength::QualifiedPath);
    assert!(MatchStrength::LauncherIdentity > MatchStrength::ExecutableName);
}
