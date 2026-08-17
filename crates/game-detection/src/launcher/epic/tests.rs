//! Reading Epic's manifests, against real files on a real disk.
//!
//! Every case writes the manifests it is about into a directory of its own and
//! points [`Epic::read_at`] at it. Nothing here needs the Epic launcher
//! installed, which is the third acceptance criterion of
//! [issue #44](https://github.com/wildware-uk/clipped/issues/44): a provider
//! whose behaviour could only be checked on a machine that has the launcher is
//! one nobody can change safely (AGENTS.md section 25).

use std::fs;

use super::*;
use crate::catalogue::MatchStrength;
use crate::test_support::Scratch;

/// An empty directory of this test's own, removed again when the test passes.
///
/// This used to return a bare path, cleaned up by a line at the end of each
/// test that a panic skipped and that nothing checked the result of. See
/// [`Scratch`] for what the returned value does and how to hold it
/// ([issue #598](https://github.com/wildware-uk/clipped/issues/598)).
fn scratch(name: &str) -> Scratch {
    Scratch::new(&format!("epic-{name}"))
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
        .app_for("inner.exe", r"D:\Epic\Inner\Binaries\inner.exe")
        .expect("something claims it");

    assert_eq!(app.app_name(), "Inner");
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

    assert!(epic.app_for("x.exe", r"D:\Epic\Fortnite").is_none());
}

#[test]
fn the_executable_decides_when_several_applications_share_one_directory() {
    // Not hypothetical, and not something the fixtures showed until the
    // provider was run against a real installation: Epic installs plugins
    // *into* the thing they extend and gives each its own manifest, so three of
    // ten directories on that machine were shared —
    //
    //     B:\Epic Games\UE_5.8  <-  QuixelBridge_5.8, FabPlugin_5.8, UE_5.8
    //
    // Depth cannot break that tie. Before #459 the first manifest by `AppName`
    // won, so anything run from the engine's directory was the Fab plugin.
    let directory = scratch("shared-directory");
    manifest(
        &directory,
        "UE.item",
        "UE_5.8",
        "Unreal Engine",
        r"B:\Epic Games\UE_5.8",
        r"Engine\Binaries\Win64\UnrealEditor.exe",
    );
    manifest(
        &directory,
        "Fab.item",
        "FabPlugin_5.8",
        "Fab UE Plugin",
        r"B:\Epic Games\UE_5.8",
        r"Engine\Plugins\Fab\FabPlugin.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");

    // `FabPlugin_5.8` sorts before `UE_5.8`, so the old rule answered with the
    // plugin for both of these.
    let editor = epic
        .app_for(
            "UnrealEditor.exe",
            r"B:\Epic Games\UE_5.8\Engine\Binaries\Win64\UnrealEditor.exe",
        )
        .expect("the editor's own manifest claims it");
    assert_eq!(editor.app_name(), "UE_5.8");

    let plugin = epic
        .app_for(
            "FabPlugin.exe",
            r"B:\Epic Games\UE_5.8\Engine\Plugins\Fab\FabPlugin.exe",
        )
        .expect("the plugin's own manifest claims it");
    assert_eq!(plugin.app_name(), "FabPlugin_5.8");
}

#[test]
fn a_shared_directory_that_no_executable_settles_is_refused_rather_than_guessed() {
    // The other half of #459, and the reason the answer is `None` rather than
    // whichever sorted first: a launcher identity for the wrong application is
    // worse than none, because the catalogue's path and name rungs are a better
    // answer than a confident wrong one.
    let directory = scratch("shared-unsettled");
    manifest(
        &directory,
        "UE.item",
        "UE_5.8",
        "Unreal Engine",
        r"B:\Epic Games\UE_5.8",
        r"Engine\Binaries\Win64\UnrealEditor.exe",
    );
    manifest(
        &directory,
        "Fab.item",
        "FabPlugin_5.8",
        "Fab UE Plugin",
        r"B:\Epic Games\UE_5.8",
        r"Engine\Plugins\Fab\FabPlugin.exe",
    );

    let epic = Epic::read_at(&directory).expect("the directory is there");

    assert!(
        epic.app_for(
            "SomethingElse.exe",
            r"B:\Epic Games\UE_5.8\Engine\Binaries\Win64\SomethingElse.exe"
        )
        .is_none(),
        "no manifest names this executable, so neither of them owns it"
    );

    // And through the join the catalogue actually uses.
    let candidate = epic.candidate_for(
        "SomethingElse.exe",
        r"B:\Epic Games\UE_5.8\Engine\Binaries\Win64\SomethingElse.exe",
    );
    assert_eq!(
        candidate.launcher(),
        None,
        "a candidate with no identity falls back to the path and name rungs"
    );
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
}

#[test]
fn a_directory_that_is_not_there_is_refused_by_name() {
    // Named inside a directory of this test's own rather than straight under
    // `%TEMP%`: a path that has to *not* exist used to be removed on the way
    // in, and removing something the test did not create is how a suite deletes
    // another run's files (issue #598).
    let directory = scratch("no-such-directory");
    let missing = directory.join("absent");

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
