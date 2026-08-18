//! Launcher providers: asking a shop which of its games a process is.
//!
//! # Why this exists at all
//!
//! [`crate::catalogue`] has a rung above every other in its precedence order —
//! [`MatchStrength::LauncherIdentity`](crate::catalogue::MatchStrength::LauncherIdentity),
//! the one that identifies a game whose process is called `launcher.exe` — and
//! reaching it needs somebody to say "this process is application 730". Nothing
//! did. This module is where that comes from.
//!
//! The shape is one submodule per launcher, which is SPEC.md section 6's
//! "provider-based so that support for a new launcher is an addition rather than
//! a change to shared logic". [`steam`] is the first
//! ([#43](https://github.com/wildware-uk/clipped/issues/43)), [`epic`] the
//! second, [`ubisoft`] the third, [`xbox`] the fourth, [`battlenet`] the fifth
//! and [`riot`] the sixth
//! ([#44](https://github.com/wildware-uk/clipped/issues/44), which asks for one
//! pull request per launcher and is still open for EA).
//!
//! EA and GOG are deliberately **not** stubbed
//! here — an empty provider that always answers "no" is a control that silently
//! does nothing (AGENTS.md section 27), and
//! [`LauncherKind`](crate::catalogue::LauncherKind) already carries the
//! vocabulary they will need.
//!
//! # What every provider is expected to do, and not do
//!
//! - **Read local files only.** No network. A launcher's own metadata is on the
//!   machine, and a detector that needed the internet would stop working on the
//!   train.
//! - **Report, never decide.** A provider answers "which application is this
//!   path?" and hands back a
//!   [`ProcessCandidate`](crate::catalogue::ProcessCandidate). Whether that is a
//!   game worth recording is the catalogue's answer, and what to do about it is
//!   `clipped_session`'s.
//! - **Name the file when something is wrong.** These are files somebody else's
//!   installer wrote, so they will be missing, half-written and occasionally
//!   nonsense; every failure says which file (AGENTS.md section 15).
//!
//! There is still no `trait LauncherProvider`, and the fourth implementation is
//! the one that was expected to settle it. It settled it the other way: Xbox
//! reads a **two-level** registry key whose entries are not all installations,
//! and its identifier has to be *derived* from a package full name rather than
//! read out of a field. The four agree on the same three methods — `discover`,
//! `candidate_for`, `problems` — and agree on nothing else: Steam follows a
//! registry key to a library index to a manifest per application across several
//! drives, Epic reads one directory of JSON, Ubisoft enumerates a registry key
//! and reads a name out of somebody else's, Xbox enumerates two, and Battle.net
//! reads its identifier out of a **command line**.
//!
//! What the later ones *did* change is what is demonstrably shared, which is now
//! shared rather than repeated (AGENTS.md section 55):
//!
//! - [`registry`] — reading a value and enumerating subkeys, for Steam, Ubisoft
//!   and Xbox. Extracted from Steam when Ubisoft needed the same two-call
//!   sizing; Xbox needed the subkey enumeration twice over.
//! - [`claim`] — which installation directory owns a running executable.
//!   Extracted from Epic when Ubisoft needed the same rule, with the same two
//!   details that are easy to get subtly wrong. Xbox uses it unchanged, which is
//!   the first evidence that the extraction was the right shape rather than a
//!   convenient one.
//!
//! That is the case for waiting, not against it: each extraction happened when a
//! second caller appeared and named exactly what the two had in common, which a
//! trait written in advance would have had to guess at.
//!
//! What remains of #44 is EA and GOG. Riot was written in the end
//! ([#513](https://github.com/wildware-uk/clipped/pull/513)), and the thing this
//! paragraph used to warn about turned out to be the finding rather than the
//! obstacle: only one of eight `Metadata` directories on a real installation
//! carries an install path, because the other seven are products the client
//! *offers*. Reading that correctly is what [`riot`] does, and it is why a
//! directory listing is not an installation.
//!
//! # Who asks all of them
//!
//! [`Launchers`] — and until it existed, nothing did. Every provider here was
//! built, tested and verified against a real installation, and no code outside
//! their own tests ever called one, so the strongest rung in the catalogue's
//! precedence order never fired in a shipped build
//! ([#522](https://github.com/wildware-uk/clipped/issues/522)).

pub mod battlenet;
mod claim;
pub mod epic;
mod installed;
mod keyvalues;
mod registry;
pub mod riot;
pub mod steam;
pub mod ubisoft;
pub mod xbox;

pub use installed::Launchers;

#[cfg(test)]
mod tests {
    use crate::catalogue::{Catalogue, LauncherKind, Match, MatchStrength, ProcessCandidate};

    /// One row per provider module declared above, and what the shipped
    /// catalogue has to have for it.
    ///
    /// The third column is a reason, and it is [`None`] for every launcher
    /// today because every one of them has an entry. A launcher that
    /// deliberately has none says so there instead, and the guard then requires
    /// the catalogue to agree — so a reason left behind after somebody adds the
    /// entry fails just as loudly as a missing entry does.
    const PROVIDERS: &[(&str, LauncherKind, Option<&str>)] = &[
        ("battlenet", LauncherKind::BattleNet, None),
        ("epic", LauncherKind::Epic, None),
        ("riot", LauncherKind::Riot, None),
        ("steam", LauncherKind::Steam, None),
        ("ubisoft", LauncherKind::Ubisoft, None),
        ("xbox", LauncherKind::Xbox, None),
    ];

    /// The provider modules, taken from the declarations at the top of this
    /// file rather than from a second list somebody has to remember to update.
    ///
    /// `pub mod` is the distinction that matters: a provider is public API a
    /// caller names, and the shared helpers beside them — `claim`, `registry`,
    /// `keyvalues`, `installed` — are private and are not launchers.
    fn provider_modules() -> Vec<&'static str> {
        include_str!("mod.rs")
            .lines()
            .filter_map(|line| line.strip_prefix("pub mod "))
            .filter_map(|rest| rest.strip_suffix(';'))
            .collect()
    }

    /// Every provider here is one [`PROVIDERS`] knows the launcher kind of.
    ///
    /// This is the half that makes the guard below unavoidable. Adding
    /// `pub mod ea;` above fails here until somebody says which
    /// [`LauncherKind`] it produces, and saying that is what puts them in front
    /// of the catalogue requirement.
    #[test]
    fn every_provider_module_is_accounted_for() {
        let declared = provider_modules();
        assert!(
            !declared.is_empty(),
            "no `pub mod` declaration was found in this file, so this guard is reading the wrong \
             text and is checking nothing"
        );

        for module in &declared {
            assert!(
                PROVIDERS.iter().any(|(name, ..)| name == module),
                "`pub mod {module};` is a launcher provider with no row in `PROVIDERS`. Add one \
                 naming the `LauncherKind` it produces, so that the catalogue is checked for an \
                 entry that identity can match."
            );
        }

        for (module, ..) in PROVIDERS {
            assert!(
                declared.contains(module),
                "`PROVIDERS` names `{module}`, which is no longer a `pub mod` in this file"
            );
        }
    }

    /// Every launcher with a provider has a catalogue entry it can actually
    /// match.
    ///
    /// # Why this is not another test of the providers
    ///
    /// Every other test under this module asserts that a launcher identity is
    /// *produced*, and none of them asserted that anything could *consume* one.
    /// That is how five providers came to be written, tested, verified against
    /// real installations and merged while no non-Steam entry in `games.toml`
    /// carried an `app_id`: the whole provider tree was green, and the strongest
    /// rung in the catalogue's precedence order fired for Steam and for nothing
    /// else ([#44](https://github.com/wildware-uk/clipped/issues/44)).
    ///
    /// So this asks the question from the other end, and it asks it of the real
    /// matcher rather than of the data. It builds the candidate a provider would
    /// hand over and requires
    /// [`Catalogue::match_process`](crate::catalogue::Catalogue::match_process)
    /// to answer with that entry at
    /// [`MatchStrength::LauncherIdentity`](crate::catalogue::MatchStrength::LauncherIdentity).
    /// Reading `app_id().is_some()` off the entry instead would pass on data the
    /// matcher rejects, which is the same mistake one rung further down.
    #[test]
    fn every_provided_launcher_has_a_catalogue_entry_it_can_match() {
        let catalogue = Catalogue::seed().expect("the shipped catalogue should load");

        for (module, kind, deliberately_none) in PROVIDERS {
            let matchable: Vec<_> = catalogue
                .entries()
                .iter()
                .filter_map(|entry| {
                    let launcher = entry.launcher()?;
                    if launcher.kind() != *kind {
                        return None;
                    }
                    // Deliberately an executable name no entry lists. This rung
                    // does not consult the executable at all, so using one the
                    // entry also names would let a name-rung match masquerade
                    // as the launcher-rung match this is checking for.
                    let candidate =
                        ProcessCandidate::new("nothing-in-the-catalogue-names-this.exe")
                            .from_launcher(*kind, launcher.app_id()?);
                    match catalogue.match_process(&candidate) {
                        Match::One {
                            entry: matched,
                            strength: MatchStrength::LauncherIdentity,
                        } if matched.game_id() == entry.game_id() => {
                            Some(entry.game_id().as_str().to_owned())
                        }
                        _ => None,
                    }
                })
                .collect();

            match deliberately_none {
                None => assert!(
                    !matchable.is_empty(),
                    "the `{module}` provider produces `LauncherKind::{kind}` identities and no \
                     shipped catalogue entry can be matched by one, so everything it reads off \
                     this machine reaches nothing. A game it identifies is still placed by the \
                     weaker executable-name and path rungs, and the rung the provider exists to \
                     feed never fires. Add an `app_id` to an entry in \
                     crates/game-detection/data/games.toml — read off a real installation, not \
                     recalled — or record in `PROVIDERS` why this launcher deliberately has none."
                ),
                Some(reason) => assert!(
                    matchable.is_empty(),
                    "`PROVIDERS` says `{module}` deliberately has no catalogue entry, because \
                     {reason}. But {matchable:?} can now be matched by a \
                     `LauncherKind::{kind}` identity, so the reason is out of date: remove it."
                ),
            }
        }
    }
}
