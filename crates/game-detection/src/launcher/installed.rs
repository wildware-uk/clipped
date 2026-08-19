//! Every launcher on this machine, asked once, so that a process can be handed
//! to the catalogue carrying the identity of whichever shop installed it.
//!
//! # Why this exists
//!
//! The providers beside it each answer "is this path one of *my* games?", and
//! for a long time nothing asked them: `identify_process` built a candidate from
//! a name and a path and never called
//! [`ProcessCandidate::from_launcher`](crate::catalogue::ProcessCandidate::from_launcher),
//! so [`MatchStrength::LauncherIdentity`](crate::catalogue::MatchStrength) —
//! the catalogue's strongest rung, and the reason the providers were written —
//! never fired in a shipped build
//! ([issue #522](https://github.com/wildware-uk/clipped/issues/522)). This is
//! the one place that asks all of them.
//!
//! # Read once, and what that costs
//!
//! Discovery reads the disk: a registry key each for Steam, Ubisoft and Xbox, a
//! directory of JSON for Epic, an uninstall key for Battle.net and a directory
//! of YAML for Riot. A process watcher reports every process that starts on the
//! machine, so doing that per process would be a registry walk per `svchost`.
//!
//! So it is done **once**, and the consequence is stated rather than hidden: a
//! game installed while the recorder is running is not in the snapshot, and is
//! detected by the catalogue's name and path rungs — exactly as well as it was
//! before this existed — until the recorder is restarted. That is the same
//! shape of limitation each provider already documents about a game *moved*
//! after installation, and the alternative is re-reading six sources on a timer
//! to catch something that happens a few times a year.
//!
//! [`refresh`](Launchers::refresh) is there for whoever decides that trade
//! differently later; nothing calls it yet.
//!
//! # A launcher that cannot be read costs nothing else
//!
//! Discovery never fails. A launcher that is not installed is absent, and one
//! whose metadata could not be read is a [`problem`](Launchers::problems) —
//! because a corrupt Epic manifest directory must not cost the user the Steam
//! games on the same machine (AGENTS.md section 16). The problems are kept and
//! named so that a diagnostics screen can say detection is working with less
//! than everything, rather than the user finding out one game at a time.

use crate::catalogue::{LauncherKind, ProcessCandidate};

use super::battlenet::BattleNet;
use super::epic::Epic;
use super::riot::Riot;
use super::steam::Steam;
use super::ubisoft::Ubisoft;
use super::xbox::Xbox;

/// The launchers found on this machine, and what could not be read.
#[derive(Debug, Default)]
pub struct Launchers {
    steam: Option<Steam>,
    epic: Option<Epic>,
    ubisoft: Option<Ubisoft>,
    xbox: Option<Xbox>,
    battlenet: Option<BattleNet>,
    riot: Option<Riot>,
    problems: Vec<String>,
}

impl Launchers {
    /// Asks every provider what it can see, once.
    ///
    /// Never fails: see the module documentation. A machine with no launchers
    /// at all answers the same as one where every provider failed, which is
    /// [`is_empty`](Self::is_empty) — the difference between the two is in
    /// [`problems`](Self::problems), where somebody can act on it.
    #[must_use]
    pub fn discover() -> Self {
        let mut found = Self::default();
        found.refresh();
        found
    }

    /// Nothing installed: the value a caller uses when it does not want the
    /// launchers consulted at all.
    ///
    /// Tests use it to assert that a process reaches the catalogue's other
    /// rungs unchanged, which is the property that keeps this from making
    /// detection worse.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The same, with a Riot installation a caller built itself.
    ///
    /// One builder rather than six, because one caller needs one: a test of
    /// something *downstream* of this — `clipped_session`'s session manager —
    /// has to be able to say "a launcher claims this directory" without reading
    /// the machine it runs on (AGENTS.md section 25), and [`Riot::from_products`]
    /// is the cheapest way to say it. The other five are two lines each the day
    /// something needs them, which is the same argument the module beside this
    /// one makes for not writing a provider trait in advance.
    #[must_use]
    pub fn with_riot(mut self, riot: Riot) -> Self {
        self.riot = Some(riot);
        self
    }

    /// Reads every launcher again, replacing what was found before.
    ///
    /// The escape hatch for the "read once" trade the module documentation
    /// describes. Nothing calls it: adding a caller means deciding how often,
    /// which is a decision with a cost and no evidence behind it yet.
    pub fn refresh(&mut self) {
        self.problems.clear();

        self.steam = self.keep("Steam", Steam::discover());
        self.epic = self.keep("Epic", Epic::discover());
        self.ubisoft = self.keep("Ubisoft Connect", Ubisoft::discover());
        self.xbox = self.keep("Xbox", Xbox::discover());
        self.battlenet = self.keep("Battle.net", BattleNet::discover());
        self.riot = self.keep("Riot", Riot::discover());
    }

    /// One provider's answer, with a failure recorded rather than propagated.
    fn keep<T, E: core::fmt::Display>(
        &mut self,
        launcher: &str,
        outcome: Result<Option<T>, E>,
    ) -> Option<T> {
        match outcome {
            Ok(found) => found,
            Err(error) => {
                self.problems.push(format!("{launcher}: {error}"));
                None
            }
        }
    }

    /// Whether no launcher was found at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.steam.is_none()
            && self.epic.is_none()
            && self.ubisoft.is_none()
            && self.xbox.is_none()
            && self.battlenet.is_none()
            && self.riot.is_none()
    }

    /// The launchers that are installed and could not be read, each naming
    /// itself and what went wrong.
    #[must_use]
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// What the launcher of `kind` calls the application it knows as `app_id`.
    ///
    /// The pair is exactly what [`ProcessCandidate::launcher`] carries, so a
    /// caller handed a claim can turn it into something to show a person. That
    /// is the whole of what this is for: a game a launcher installed but the
    /// catalogue does not name has an identifier and no name, and
    /// `steam-1145360` is not a thing to put in a library
    /// ([issue #664](https://github.com/wildware-uk/clipped/issues/664)).
    ///
    /// [`None`] when that launcher is not installed, or when nothing it
    /// installed carries that identifier.
    #[must_use]
    pub fn name_of(&self, kind: LauncherKind, app_id: &str) -> Option<&str> {
        match kind {
            LauncherKind::Steam => self.steam.as_ref()?.name_of(app_id),
            LauncherKind::Epic => self.epic.as_ref()?.name_of(app_id),
            LauncherKind::Xbox => self.xbox.as_ref()?.name_of(app_id),
            LauncherKind::BattleNet => self.battlenet.as_ref()?.name_of(app_id),
            LauncherKind::Riot => self.riot.as_ref()?.name_of(app_id),
            LauncherKind::Ubisoft => self.ubisoft.as_ref()?.name_of(app_id),
            // `LauncherKind` is `#[non_exhaustive]` and names kinds no provider
            // is written for yet — `Ea`, and whatever is added next. A claim can
            // only come from a provider, so this arm is unreachable in practice;
            // answering `None` rather than panicking keeps a future kind from
            // turning a naming question into a crash.
            _ => None,
        }
    }

    /// A running process as the catalogue wants to be asked about it, carrying
    /// the identity of the launcher that claims its path.
    ///
    /// The identity is attached when **exactly one** launcher claims the path
    /// and left off otherwise, which covers both of the cases that are not a
    /// single answer:
    ///
    /// - Nothing claims it — a game installed without a launcher, or one whose
    ///   launcher is not among the six. The candidate is what it would have
    ///   been before this module existed, so the catalogue's name and path rungs
    ///   match it exactly as well as they did.
    /// - More than one claims it, which would mean two shops with overlapping
    ///   install directories. Refusing is the same answer
    ///   [`deepest_claimants`](super::claim::deepest_claimants) gives a tie
    ///   inside one launcher, and for the same reason: handing the catalogue an
    ///   identity for the wrong game is worse than handing it none
    ///   ([issue #459](https://github.com/wildware-uk/clipped/issues/459)).
    #[must_use]
    pub fn candidate_for<'a>(
        &'a self,
        executable_name: &'a str,
        executable_path: &'a str,
    ) -> ProcessCandidate<'a> {
        let plain = ProcessCandidate::new(executable_name).with_path(executable_path);

        // Each provider's own `candidate_for` decides what its identity is, so
        // this asks them rather than reaching into their applications and
        // spelling six identifier rules out a second time (AGENTS.md section
        // 55).
        let claimed: Vec<ProcessCandidate<'a>> = [
            self.steam
                .as_ref()
                .map(|steam| steam.candidate_for(executable_name, executable_path)),
            self.epic
                .as_ref()
                .map(|epic| epic.candidate_for(executable_name, executable_path)),
            self.ubisoft
                .as_ref()
                .map(|ubisoft| ubisoft.candidate_for(executable_name, executable_path)),
            self.xbox
                .as_ref()
                .map(|xbox| xbox.candidate_for(executable_name, executable_path)),
            self.battlenet
                .as_ref()
                .map(|battlenet| battlenet.candidate_for(executable_name, executable_path)),
            self.riot
                .as_ref()
                .map(|riot| riot.candidate_for(executable_name, executable_path)),
        ]
        .into_iter()
        .flatten()
        .filter(|candidate| candidate.launcher().is_some())
        .collect();

        let mut claimed = claimed;
        match claimed.len() {
            1 => claimed.remove(0),
            _ => plain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::LauncherKind;

    #[test]
    fn nothing_installed_leaves_a_process_exactly_as_it_was() {
        // The property that keeps this from making detection worse: without a
        // launcher to claim it, the candidate is the name and the path, which
        // is what the catalogue's other rungs match on.
        let launchers = Launchers::none();

        let candidate = launchers.candidate_for("cs2.exe", r"D:\Games\cs2\cs2.exe");

        assert_eq!(candidate.executable_name(), "cs2.exe");
        assert_eq!(candidate.executable_path(), Some(r"D:\Games\cs2\cs2.exe"));
        assert_eq!(candidate.launcher(), None);
        assert!(launchers.is_empty());
        assert!(launchers.problems().is_empty());
    }

    #[test]
    fn a_process_inside_one_launchers_game_carries_that_launchers_identity() {
        let launchers = Launchers {
            riot: Some(Riot::from_products([(
                "league_of_legends".to_owned(),
                "live".to_owned(),
                std::path::PathBuf::from("C:/Riot Games/League of Legends"),
            )])),
            ..Launchers::default()
        };

        let candidate = launchers.candidate_for(
            "LeagueClient.exe",
            r"C:\Riot Games\League of Legends\LeagueClient.exe",
        );

        assert_eq!(
            candidate.launcher(),
            Some((LauncherKind::Riot, "league_of_legends"))
        );
        assert_eq!(candidate.executable_name(), "LeagueClient.exe");
    }

    #[test]
    fn a_process_no_launcher_claims_keeps_its_name_and_path_and_nothing_else() {
        let launchers = Launchers {
            riot: Some(Riot::from_products([(
                "league_of_legends".to_owned(),
                "live".to_owned(),
                std::path::PathBuf::from("C:/Riot Games/League of Legends"),
            )])),
            ..Launchers::default()
        };

        let candidate =
            launchers.candidate_for("cs2.exe", r"D:\Steam\steamapps\common\cs2\cs2.exe");

        assert_eq!(candidate.launcher(), None);
        assert_eq!(
            candidate.executable_path(),
            Some(r"D:\Steam\steamapps\common\cs2\cs2.exe")
        );
    }

    #[test]
    fn two_launchers_claiming_one_path_are_refused_rather_than_chosen_between() {
        // Two shops with overlapping install directories. It should not happen,
        // and if it does the honest answer is that this cannot say which game it
        // is — the same answer a tie inside one launcher gets.
        let directory = "C:/Games/Shared";
        let launchers = Launchers {
            riot: Some(Riot::from_products([(
                "one".to_owned(),
                "live".to_owned(),
                std::path::PathBuf::from(directory),
            )])),
            ubisoft: Some(Ubisoft::from_installs([(
                "5595".to_owned(),
                directory.to_owned(),
                Some("Two".to_owned()),
            )])),
            ..Launchers::default()
        };

        let candidate = launchers.candidate_for("game.exe", r"C:\Games\Shared\game.exe");

        assert_eq!(candidate.launcher(), None);
    }

    #[test]
    fn discovery_answers_on_this_machine_whatever_is_installed_on_it() {
        // Not a mock: this reads the real registry and the real directories.
        // It cannot assert *what* is installed — that is the machine's business
        // — so what it asserts is that asking is not a failure, and that
        // everything that came back is usable.
        let launchers = Launchers::discover();

        for problem in launchers.problems() {
            println!("problem: {problem}");
        }
        println!(
            "installed: steam {}, epic {}, ubisoft {}, xbox {}, battle.net {}, riot {}",
            launchers.steam.is_some(),
            launchers.epic.is_some(),
            launchers.ubisoft.is_some(),
            launchers.xbox.is_some(),
            launchers.battlenet.is_some(),
            launchers.riot.is_some(),
        );

        // Whatever is here, a process nothing installed is never claimed.
        let candidate = launchers.candidate_for("notepad.exe", r"C:\Windows\System32\notepad.exe");
        assert_eq!(
            candidate.launcher(),
            None,
            "a Windows accessory is not one of anybody's games"
        );
    }
}
