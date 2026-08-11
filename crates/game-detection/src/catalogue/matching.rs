//! Deciding which catalogue entry a running process is.
//!
//! # The precedence order
//!
//! Every entry that matches a process at all matches it at one of three
//! strengths, and the strongest wins outright:
//!
//! 1. **[`MatchStrength::LauncherIdentity`]** — the launcher itself said which
//!    game this is, and the entry records that identifier. The executable is
//!    not consulted, because it does not matter: this is the rung that
//!    identifies a game whose process is called `launcher.exe`.
//! 2. **[`MatchStrength::QualifiedPath`]** — the file name matches *and* the
//!    image path contains the fragment the entry demands, as whole directory
//!    names. This is what tells Half-Life 2 from Team Fortress 2, which are two
//!    games with one executable name.
//! 3. **[`MatchStrength::ExecutableName`]** — the file name matches and the
//!    entry asked for nothing else.
//!
//! Only if two entries tie on strength does the file they came from break it:
//! **a user's overlay entry beats a shipped one**. That order — strength
//! first, source second — is deliberate and is the one case worth explaining.
//! If a user's bare `game.exe` entry outranked a shipped entry that had
//! path-qualified the very same `game.exe`, then adding one broad personal
//! entry would silently take over every game that happens to share a common
//! executable name. Specificity is evidence about *this* process; a file name
//! is a guess whoever wrote it. A user who wants to override a specific
//! install writes a path qualifier of their own, and wins on rung 2 by having
//! said something equally specific.
//!
//! If entries still tie after both — two shipped games claiming a bare
//! `launcher.exe` — the answer is [`Match::Ambiguous`] and it names all of
//! them. Nothing picks one at random and nothing picks the first: a recorder
//! that guessed would file somebody's session under the wrong game, and the
//! caller is in a far better position to ask, to log, or to wait for the
//! window title (issue #41).
//!
//! # A path qualifier names directories, not characters
//!
//! `path_contains` is compared segment by segment rather than as a substring,
//! and that is a correctness rule rather than a nicety. `steamapps/common/
//! Half-Life 2` is a substring of `steamapps/common/Half-Life 2 Deathmatch`,
//! which is a different Steam application running the same `hl2.exe` — so a
//! substring test answers "Half-Life 2" about a game that is not Half-Life 2,
//! confidently, at the second-strongest rung, and without reporting anything as
//! ambiguous. Whole-segment matching is what makes rung 2 mean what the rest of
//! this file claims it means. The same applies at the front of a fragment:
//! `common/Portal` must not be found inside `.../uncommon/Portal/...`.
//!
//! # What is deliberately not a match key
//!
//! `child_processes`. A game's helper is not the game, and treating the list
//! as a way in would make every anti-cheat service and crash handler a reason
//! to start recording.

use super::entry::{Entry, LauncherKind};

/// How specifically an entry matched.
///
/// Ordered: `ExecutableName < QualifiedPath < LauncherIdentity`. The
/// derived ordering *is* the precedence rule, so a rung added later must be
/// declared in the right place in this list rather than appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchStrength {
    /// The executable's file name matched an unqualified rule.
    ExecutableName,
    /// The file name matched and the image path contained the entry's
    /// fragment.
    QualifiedPath,
    /// The launcher's own identifier for the game matched.
    LauncherIdentity,
}

/// A process to look up.
///
/// Built from what a process watcher can see. Only the file name is required:
/// the image path and the launcher's identifier are both things a caller may
/// not have, and an absent one simply means the rungs that need it cannot be
/// reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCandidate<'a> {
    executable_name: &'a str,
    executable_path: Option<&'a str>,
    launcher: Option<(LauncherKind, &'a str)>,
}

impl<'a> ProcessCandidate<'a> {
    /// A candidate known only by its executable's file name.
    #[must_use]
    pub const fn new(executable_name: &'a str) -> Self {
        Self {
            executable_name,
            executable_path: None,
            launcher: None,
        }
    }

    /// The same candidate, with the full path of the running image.
    ///
    /// Either separator; the comparison normalises both sides.
    #[must_use]
    pub const fn with_path(mut self, executable_path: &'a str) -> Self {
        self.executable_path = Some(executable_path);
        self
    }

    /// The same candidate, with what a launcher says it is.
    ///
    /// Producing this is issues #43 and #44 — Steam knows the application
    /// identifier of the process it started, and so do the others. The
    /// catalogue only consumes it.
    #[must_use]
    pub const fn from_launcher(mut self, kind: LauncherKind, app_id: &'a str) -> Self {
        self.launcher = Some((kind, app_id));
        self
    }

    /// The executable's file name.
    #[must_use]
    pub const fn executable_name(&self) -> &'a str {
        self.executable_name
    }

    /// The image path, where the caller had one.
    #[must_use]
    pub const fn executable_path(&self) -> Option<&'a str> {
        self.executable_path
    }

    /// What the launcher said, where a caller had that.
    #[must_use]
    pub const fn launcher(&self) -> Option<(LauncherKind, &'a str)> {
        self.launcher
    }
}

/// What the catalogue made of a process.
#[derive(Debug, Clone, PartialEq)]
pub enum Match<'a> {
    /// Nothing in the catalogue claims this process.
    None,
    /// Exactly one entry claims it, at this strength.
    One {
        /// The entry.
        entry: &'a Entry,
        /// How specifically it matched.
        strength: MatchStrength,
    },
    /// Several entries claim it equally well. The catalogue does not choose.
    Ambiguous {
        /// Every entry that tied, in catalogue order.
        entries: Vec<&'a Entry>,
        /// The strength they tied at.
        strength: MatchStrength,
    },
}

impl Match<'_> {
    /// The single entry, when there was exactly one.
    #[must_use]
    pub const fn entry(&self) -> Option<&Entry> {
        match self {
            Self::One { entry, .. } => Some(*entry),
            Self::None | Self::Ambiguous { .. } => None,
        }
    }
}

/// Applies the precedence order to a whole catalogue.
pub(crate) fn best_match<'a>(entries: &'a [Entry], candidate: &ProcessCandidate<'_>) -> Match<'a> {
    let scored: Vec<(&Entry, MatchStrength)> = entries
        .iter()
        .filter_map(|entry| strength(entry, candidate).map(|strength| (entry, strength)))
        .collect();

    let Some(best) = scored.iter().map(|(_, strength)| *strength).max() else {
        return Match::None;
    };

    let mut winners: Vec<&Entry> = scored
        .iter()
        .filter(|(_, strength)| *strength == best)
        .map(|(entry, _)| *entry)
        .collect();

    // Rung first, file second: only entries that already tied on specificity
    // reach this, and among those the user's own file wins.
    if winners.len() > 1 && winners.iter().any(|entry| entry.source().is_overlay()) {
        winners.retain(|entry| entry.source().is_overlay());
    }

    match winners.len() {
        1 => Match::One {
            entry: winners[0],
            strength: best,
        },
        _ => Match::Ambiguous {
            entries: winners,
            strength: best,
        },
    }
}

/// The strongest rung one entry reaches for one candidate, if any.
fn strength(entry: &Entry, candidate: &ProcessCandidate<'_>) -> Option<MatchStrength> {
    if let (Some(launcher), Some((kind, app_id))) = (entry.launcher(), candidate.launcher()) {
        // An entry with no recorded identifier says nothing about *which* game
        // the launcher started, so matching on the launcher alone would make
        // every Steam game the same game.
        if launcher.kind() == kind
            && launcher
                .app_id()
                .is_some_and(|recorded| recorded.eq_ignore_ascii_case(app_id))
        {
            return Some(MatchStrength::LauncherIdentity);
        }
    }

    let path = candidate.executable_path().map(normalise_path);
    entry
        .executables()
        .iter()
        .filter(|rule| equal_names(rule.name(), candidate.executable_name()))
        .filter_map(|rule| match rule.path_contains() {
            None => Some(MatchStrength::ExecutableName),
            // A qualified rule that cannot be checked does not fall back to
            // matching on the name: the qualifier is there precisely because
            // the name alone is not enough to be sure.
            Some(fragment) => path
                .as_deref()
                .filter(|path| covers_segments(path, &normalise_path(fragment)))
                .map(|_| MatchStrength::QualifiedPath),
        })
        .max()
}

/// Compares two file names the way Windows compares them.
///
/// Case-insensitively, and by `to_lowercase` rather than by the ASCII-only
/// form, because a game's executable is not guaranteed to be spelled in ASCII
/// and getting a Japanese title wrong is not a nicer failure for being rarer.
/// This runs once per process start, so the allocation is irrelevant.
///
/// There is deliberately no length check to short-circuit on, because case
/// folding changes length: `GROẞE.exe` is three bytes of `U+1E9E` and folds to
/// the two bytes of `ß`, so comparing lengths first would refuse the exact pair
/// this function exists to accept.
fn equal_names(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// Whether `path` contains `fragment` as a run of whole path segments.
///
/// Both arguments must already have been through [`normalise_path`]. A fragment
/// of no segments — `"/"`, which validation cannot tell from a real one because
/// it is not empty — matches nothing rather than everything: an entry that
/// claimed every process on the machine is the failure this rung prevents, not
/// a licence to widen the entry.
fn covers_segments(path: &str, fragment: &str) -> bool {
    let path: Vec<&str> = segments(path).collect();
    let wanted: Vec<&str> = segments(fragment).collect();
    if wanted.is_empty() {
        return false;
    }
    // `windows` panics on zero, which the guard above has already excluded, and
    // yields nothing when the fragment is longer than the path.
    path.windows(wanted.len())
        .any(|window| window == wanted.as_slice())
}

/// The non-empty segments of a normalised path.
///
/// Empty ones are dropped so that a leading, trailing or doubled separator in
/// either the fragment or the path changes nothing about what matches.
pub(crate) fn segments(normalised: &str) -> impl Iterator<Item = &str> {
    normalised.split('/').filter(|segment| !segment.is_empty())
}

/// Puts a path in the one form both sides of a comparison use.
///
/// Lower-cased, and with backslashes turned into forward slashes so that a
/// catalogue entry may be written with either. Separators are not otherwise
/// touched; [`covers_segments`] is what decides where one segment ends, and it
/// is indifferent to how many separators there were.
pub(crate) fn normalise_path(value: &str) -> String {
    value.replace('\\', "/").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::entry::EntrySource;
    use crate::catalogue::Catalogue;
    use std::path::PathBuf;

    /// Parses catalogue text as though it were the shipped seed data.
    fn seed(body: &str) -> Catalogue {
        Catalogue::parse(&format!("schema_version = 1\n{body}"), EntrySource::Seed)
            .expect("the fixture is a valid catalogue")
    }

    /// Parses catalogue text as though it were a user's own file.
    fn overlay(body: &str) -> Catalogue {
        Catalogue::parse(
            &format!("schema_version = 1\n{body}"),
            EntrySource::Overlay {
                path: PathBuf::from("games.toml"),
            },
        )
        .expect("the fixture is a valid catalogue")
    }

    /// Two games, one executable name, told apart only by where they live.
    const SHARED_NAME: &str = r#"
[[game]]
game_id = "half-life-2"
name = "Half-Life 2"
[[game.executables]]
name = "hl2.exe"
path_contains = "steamapps/common/Half-Life 2"

[[game]]
game_id = "team-fortress-2"
name = "Team Fortress 2"
[[game.executables]]
name = "hl2.exe"
path_contains = "steamapps/common/Team Fortress 2"
"#;

    /// Three publishers, one file name, nothing to tell them apart.
    const THREE_LAUNCHERS: &str = r#"
[[game]]
game_id = "first-game"
name = "First Game"
[[game.executables]]
name = "launcher.exe"

[[game]]
game_id = "second-game"
name = "Second Game"
[[game.executables]]
name = "launcher.exe"

[[game]]
game_id = "third-game"
name = "Third Game"
[[game.executables]]
name = "launcher.exe"
"#;

    fn matched_id<'a>(outcome: &'a Match<'a>) -> &'a str {
        match outcome {
            Match::One { entry, .. } => entry.game_id().as_str(),
            other => panic!("expected exactly one match, got {other:?}"),
        }
    }

    #[test]
    fn a_process_nothing_claims_matches_nothing() {
        let catalogue = seed(SHARED_NAME);
        let outcome = catalogue.match_process(&ProcessCandidate::new("notepad.exe"));
        assert_eq!(outcome, Match::None);
    }

    #[test]
    fn an_executable_name_matches_case_insensitively_as_windows_does() {
        let catalogue = seed(THREE_LAUNCHERS);
        let outcome = catalogue.match_process(&ProcessCandidate::new("LAUNCHER.EXE"));
        assert!(
            matches!(outcome, Match::Ambiguous { ref entries, .. } if entries.len() == 3),
            "expected all three entries, got {outcome:?}"
        );
    }

    #[test]
    fn two_games_sharing_an_executable_name_are_told_apart_by_path() {
        let catalogue = seed(SHARED_NAME);

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("hl2.exe")
                .with_path(r"D:\Steam\steamapps\common\Team Fortress 2\hl2.exe"),
        );
        assert_eq!(matched_id(&outcome), "team-fortress-2");

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("hl2.exe")
                .with_path(r"D:\Steam\steamapps\common\Half-Life 2\hl2.exe"),
        );
        assert_eq!(matched_id(&outcome), "half-life-2");
    }

    #[test]
    fn a_qualified_entry_does_not_match_a_process_whose_path_is_unknown() {
        // The qualifier exists because the name alone is not enough. Falling
        // back to the name would answer "Half-Life 2 or Team Fortress 2, take
        // your pick" for a process that might be neither.
        let catalogue = seed(SHARED_NAME);
        let outcome = catalogue.match_process(&ProcessCandidate::new("hl2.exe"));
        assert_eq!(outcome, Match::None);
    }

    #[test]
    fn a_qualified_entry_does_not_match_a_path_it_does_not_cover() {
        let catalogue = seed(SHARED_NAME);
        let outcome = catalogue.match_process(
            &ProcessCandidate::new("hl2.exe")
                .with_path(r"D:\Steam\steamapps\common\Portal\hl2.exe"),
        );
        assert_eq!(outcome, Match::None);
    }

    #[test]
    fn a_path_qualifier_names_a_directory_rather_than_a_prefix_of_one() {
        // `steamapps/common/Half-Life 2` is a substring of
        // `steamapps/common/Half-Life 2 Deathmatch`, which is a different Steam
        // application running the same `hl2.exe`. A substring test answers
        // "Half-Life 2" here, wrongly, at the second-strongest rung and without
        // reporting anything as ambiguous.
        let catalogue = seed(SHARED_NAME);
        let outcome = catalogue.match_process(
            &ProcessCandidate::new("hl2.exe")
                .with_path(r"D:\Steam\steamapps\common\Half-Life 2 Deathmatch\hl2.exe"),
        );
        assert_eq!(outcome, Match::None, "got {outcome:?}");
    }

    #[test]
    fn a_path_qualifier_must_begin_at_a_directory_boundary_too() {
        let catalogue = seed(
            r#"
[[game]]
game_id = "portal"
name = "Portal"
[[game.executables]]
name = "portal.exe"
path_contains = "common/Portal"
"#,
        );

        // The fragment's first segment is a whole directory name as well, so a
        // library folder called `uncommon` is not a `common` one.
        let outcome = catalogue.match_process(
            &ProcessCandidate::new("portal.exe")
                .with_path(r"D:\Steam\steamapps\uncommon\Portal\portal.exe"),
        );
        assert_eq!(outcome, Match::None, "got {outcome:?}");

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("portal.exe")
                .with_path(r"D:\Steam\steamapps\common\Portal\portal.exe"),
        );
        assert_eq!(matched_id(&outcome), "portal");
    }

    #[test]
    fn a_path_qualifier_written_with_stray_separators_names_the_same_directories() {
        // Segments are what is compared, so a fragment somebody wrote with a
        // leading, trailing or doubled separator still means what they meant.
        let catalogue = seed(
            r#"
[[game]]
game_id = "portal"
name = "Portal"
[[game.executables]]
name = "portal.exe"
path_contains = "/steamapps//common/Portal/"
"#,
        );

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("portal.exe")
                .with_path(r"D:\Steam\steamapps\common\Portal\portal.exe"),
        );
        assert_eq!(matched_id(&outcome), "portal");
    }

    #[test]
    fn a_path_qualifier_of_separators_alone_claims_nothing_rather_than_everything() {
        // `"/"` is not empty, so validation cannot tell it from a real
        // qualifier, and it names no directory at all. The safe reading of "no
        // directories" is that the entry claims nothing: an entry claiming every
        // process on the machine is what this rung exists to prevent.
        let catalogue = seed(
            r#"
[[game]]
game_id = "over-broad"
name = "Over-broad"
[[game.executables]]
name = "game.exe"
path_contains = "/"
"#,
        );

        let outcome = catalogue
            .match_process(&ProcessCandidate::new("game.exe").with_path(r"C:\Anything\game.exe"));
        assert_eq!(outcome, Match::None, "got {outcome:?}");
    }

    #[test]
    fn an_executable_name_matches_when_folding_its_case_changes_its_length() {
        // `ẞ` (U+1E9E) is three bytes and folds to the two bytes of `ß`, so a
        // byte-length comparison refuses the pair. Names are compared by
        // `to_lowercase` rather than the ASCII-only form precisely so that a
        // game whose executable is not spelled in ASCII still matches, and a
        // length check would quietly undo that.
        let catalogue = seed(
            r#"
[[game]]
game_id = "grosse"
name = "Große"
[[game.executables]]
name = "GROẞE.exe"
"#,
        );

        let outcome = catalogue.match_process(&ProcessCandidate::new("große.exe"));
        assert_eq!(matched_id(&outcome), "grosse");
    }

    #[test]
    fn a_path_qualified_match_beats_a_bare_name_match() {
        let catalogue = seed(
            r#"
[[game]]
game_id = "generic"
name = "Something Generic"
[[game.executables]]
name = "game.exe"

[[game]]
game_id = "specific"
name = "Something Specific"
[[game.executables]]
name = "game.exe"
path_contains = "publisher/specific"
"#,
        );

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("game.exe").with_path(r"C:\Games\Publisher\Specific\game.exe"),
        );
        assert_eq!(matched_id(&outcome), "specific");
        assert!(matches!(
            outcome,
            Match::One {
                strength: MatchStrength::QualifiedPath,
                ..
            }
        ));
    }

    #[test]
    fn three_publishers_sharing_a_launcher_name_are_reported_as_ambiguous() {
        let catalogue = seed(THREE_LAUNCHERS);
        let outcome = catalogue.match_process(&ProcessCandidate::new("launcher.exe"));

        let Match::Ambiguous { entries, strength } = outcome else {
            panic!("expected an ambiguous match, got {outcome:?}");
        };
        assert_eq!(strength, MatchStrength::ExecutableName);
        let ids: Vec<&str> = entries
            .iter()
            .map(|entry| entry.game_id().as_str())
            .collect();
        assert_eq!(ids, ["first-game", "second-game", "third-game"]);
    }

    #[test]
    fn a_user_overlay_entry_shadows_a_shipped_one_at_the_same_strength() {
        let catalogue = seed(THREE_LAUNCHERS).overlaid_with(overlay(
            r#"
[[game]]
game_id = "my-own-game"
name = "My Own Game"
[[game.executables]]
name = "launcher.exe"
"#,
        ));

        let outcome = catalogue.match_process(&ProcessCandidate::new("launcher.exe"));
        assert_eq!(matched_id(&outcome), "my-own-game");
    }

    #[test]
    fn an_overlay_entry_replaces_the_shipped_entry_with_the_same_game_id() {
        let catalogue = seed(SHARED_NAME).overlaid_with(overlay(
            r#"
[[game]]
game_id = "team-fortress-2"
name = "TF2, my install"
[[game.executables]]
name = "hl2.exe"
path_contains = "games/tf2"
"#,
        ));

        // The shipped rule is gone rather than merged: the user's entry is the
        // whole of what the catalogue now says about that game.
        assert_eq!(catalogue.entries().len(), 2);
        let outcome = catalogue.match_process(
            &ProcessCandidate::new("hl2.exe")
                .with_path(r"D:\Steam\steamapps\common\Team Fortress 2\hl2.exe"),
        );
        assert_eq!(outcome, Match::None);

        let outcome = catalogue
            .match_process(&ProcessCandidate::new("hl2.exe").with_path(r"E:\Games\TF2\hl2.exe"));
        assert_eq!(matched_id(&outcome), "team-fortress-2");
        assert_eq!(
            catalogue
                .find_by_id("team-fortress-2")
                .expect("the overlay entry is in the catalogue")
                .name(),
            "TF2, my install"
        );
    }

    #[test]
    fn a_shipped_qualified_entry_beats_a_users_bare_one() {
        // Strength before source, which is the surprising half of the rule:
        // one broad personal entry must not quietly take over every game that
        // shares a common executable name.
        let catalogue = seed(SHARED_NAME).overlaid_with(overlay(
            r#"
[[game]]
game_id = "my-source-game"
name = "My Source Game"
[[game.executables]]
name = "hl2.exe"
"#,
        ));

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("hl2.exe")
                .with_path(r"D:\Steam\steamapps\common\Half-Life 2\hl2.exe"),
        );
        assert_eq!(matched_id(&outcome), "half-life-2");

        // And where the shipped qualifiers do not apply, the user's entry is
        // the only thing that matches, so it answers.
        let outcome = catalogue
            .match_process(&ProcessCandidate::new("hl2.exe").with_path(r"E:\Mods\hl2.exe"));
        assert_eq!(matched_id(&outcome), "my-source-game");
    }

    #[test]
    fn launcher_metadata_identifies_a_game_whose_executable_name_is_useless() {
        let catalogue = seed(
            r#"
[[game]]
game_id = "known-game"
name = "Known Game"
[[game.executables]]
name = "known.exe"
[game.launcher]
kind = "steam"
app_id = "730"

[[game]]
game_id = "some-other-game"
name = "Some Other Game"
[[game.executables]]
name = "launcher.exe"
"#,
        );

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("launcher.exe").from_launcher(LauncherKind::Steam, "730"),
        );
        assert_eq!(matched_id(&outcome), "known-game");
        assert!(matches!(
            outcome,
            Match::One {
                strength: MatchStrength::LauncherIdentity,
                ..
            }
        ));
    }

    #[test]
    fn a_launcher_identity_match_beats_a_path_qualified_one() {
        let catalogue = seed(
            r#"
[[game]]
game_id = "by-identity"
name = "By Identity"
[[game.executables]]
name = "other.exe"
[game.launcher]
kind = "steam"
app_id = "440"

[[game]]
game_id = "by-path"
name = "By Path"
[[game.executables]]
name = "game.exe"
path_contains = "steamapps/common/By Path"
"#,
        );

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("game.exe")
                .with_path(r"D:\Steam\steamapps\common\By Path\game.exe")
                .from_launcher(LauncherKind::Steam, "440"),
        );
        assert_eq!(matched_id(&outcome), "by-identity");
    }

    #[test]
    fn a_launcher_with_no_recorded_identifier_does_not_match_on_the_launcher_alone() {
        // Otherwise every entry marked `kind = "steam"` would match every
        // process Steam started.
        let catalogue = seed(
            r#"
[[game]]
game_id = "some-steam-game"
name = "Some Steam Game"
[[game.executables]]
name = "some.exe"
[game.launcher]
kind = "steam"
"#,
        );

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("unrelated.exe").from_launcher(LauncherKind::Steam, "730"),
        );
        assert_eq!(outcome, Match::None);
    }

    #[test]
    fn a_launcher_identifier_from_a_different_launcher_does_not_match() {
        let catalogue = seed(
            r#"
[[game]]
game_id = "steam-game"
name = "Steam Game"
[[game.executables]]
name = "steam-game.exe"
[game.launcher]
kind = "steam"
app_id = "730"
"#,
        );

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("something.exe").from_launcher(LauncherKind::Epic, "730"),
        );
        assert_eq!(outcome, Match::None);
    }

    #[test]
    fn a_child_process_is_not_a_way_into_the_catalogue() {
        let catalogue = seed(
            r#"
[[game]]
game_id = "a-game"
name = "A Game"
child_processes = ["anticheat-service.exe"]
[[game.executables]]
name = "a-game.exe"
"#,
        );

        let outcome = catalogue.match_process(&ProcessCandidate::new("anticheat-service.exe"));
        assert_eq!(outcome, Match::None);
    }

    #[test]
    fn a_path_qualifier_may_be_written_with_either_separator() {
        let catalogue = seed(
            r#"
[[game]]
game_id = "backslash-entry"
name = "Backslash Entry"
[[game.executables]]
name = "game.exe"
path_contains = "steamapps\\common\\Backslash"
"#,
        );

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("game.exe")
                .with_path("D:/Steam/steamapps/common/Backslash/game.exe"),
        );
        assert_eq!(matched_id(&outcome), "backslash-entry");
    }
}
