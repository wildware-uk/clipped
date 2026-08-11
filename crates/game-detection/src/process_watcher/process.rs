//! What the watcher says about a process.
//!
//! Everything here is plain data with no platform types in it, so the matching
//! rules that come next ([issue #42](https://github.com/wildware-uk/clipped/issues/42))
//! can be written and tested against constructed process trees rather than
//! against whatever happens to be running.

use std::path::{Path, PathBuf};

/// A process, as it was when the watcher first saw it.
///
/// The three facts the ticket asks for — executable, identifier and parent —
/// plus the file name, which is separate from the path for a reason worth
/// stating: the path is not always available. Windows refuses
/// `PROCESS_QUERY_LIMITED_INFORMATION` on protected processes, and WMI reports
/// `ExecutablePath` as null for the same ones, but the name is in the process
/// table either way. A watcher that dropped those processes would be blind to
/// exactly the anti-cheat wrappers a game launch puts in the middle of its
/// chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSnapshot {
    /// The process identifier.
    ///
    /// Unique only among *live* processes: Windows reuses identifiers, often
    /// within seconds on a busy machine. Nothing may treat this as a durable
    /// name for a process — [`LaunchId`] is what stays put.
    pub pid: u32,

    /// The identifier of the process that created this one.
    ///
    /// This is what makes debouncing possible: a launcher, the game it starts
    /// and any wrapper between them form a chain, and the chain is the only
    /// thing that distinguishes one launch from two.
    ///
    /// It is not itself durable either. The parent usually outlives the child
    /// but need not, and once it has exited its identifier may name something
    /// entirely unrelated, so a parent identifier is only meaningful against
    /// processes seen at the same time.
    pub parent_pid: u32,

    /// The full path of the executable, when Windows would give it up.
    ///
    /// # Privacy
    ///
    /// A user path, which can carry an account name and a games library
    /// layout. Match on it freely; it must not reach a log line except through
    /// [`clipped_logging::RedactedPath`] (docs/logging.md).
    pub image_path: Option<PathBuf>,

    /// The executable's file name, such as `cs2.exe`.
    ///
    /// Always present, and always the file name alone.
    pub image_name: String,
}

impl ProcessSnapshot {
    /// Builds a snapshot, deriving the file name from `image_path`.
    ///
    /// `fallback_name` is used when the path is absent or has no final
    /// component, which is what happens for a process this application cannot
    /// open. Sources that only ever have a name — the process table gives one
    /// unconditionally — pass [`None`] for the path.
    #[must_use]
    pub fn new(
        pid: u32,
        parent_pid: u32,
        image_path: Option<PathBuf>,
        fallback_name: &str,
    ) -> Self {
        let image_name = image_path
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| fallback_name.to_owned());

        Self {
            pid,
            parent_pid,
            image_path,
            image_name,
        }
    }
}

/// Identifies one launch for as long as the watcher runs.
///
/// A launch is what the debounce produces: the launcher, the game and anything
/// between them, collapsed into one thing that started. Every process the
/// watcher reports carries the identifier of the launch it belongs to, so a
/// consumer can tell "the game exited" from "one of the launcher's helpers
/// exited" without reconstructing the tree itself.
///
/// Identifiers are allocated in order and never reused, unlike the process
/// identifiers Windows hands out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LaunchId(u64);

impl LaunchId {
    /// The launch every process that was already running is attributed to.
    ///
    /// The watcher takes one snapshot of the process table when it starts, so
    /// that a game already running when Clipped opens still reports its exit.
    /// Those processes were not observed starting and are not a launch, so they
    /// share this reserved identifier rather than being given a fictional one
    /// each.
    pub const ALREADY_RUNNING: Self = Self(0);

    /// Whether this is [`Self::ALREADY_RUNNING`].
    #[must_use]
    pub const fn is_already_running(self) -> bool {
        self.0 == Self::ALREADY_RUNNING.0
    }

    /// The identifier as a number, for logging and for tests.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next identifier after this one.
    pub(crate) const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// The first identifier a real launch may take.
    pub(crate) const fn first() -> Self {
        Self(1)
    }
}

/// One launch, after the debounce has finished collecting it.
///
/// The whole chain is reported rather than a single "the game" process, because
/// deciding which member is the game is a different job with a different set of
/// inputs — the known-game database, the launcher installs, the user's
/// overrides — and it belongs to
/// [issue #42](https://github.com/wildware-uk/clipped/issues/42). This ticket
/// reports what started; it does not guess which part of it mattered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchGroup {
    /// The identifier every member of this launch shares.
    pub id: LaunchId,

    /// The members, oldest first.
    ///
    /// Never empty: a launch with no surviving members is dropped rather than
    /// reported.
    pub processes: Vec<ProcessSnapshot>,
}

impl LaunchGroup {
    /// The first process of the chain — usually the launcher.
    ///
    /// # Panics
    ///
    /// Never: a group is only built with at least one member.
    #[must_use]
    pub fn root(&self) -> &ProcessSnapshot {
        self.processes
            .first()
            .expect("a launch group always has at least one member")
    }

    /// The most recently started member of the chain.
    ///
    /// This is the best single guess at "the thing that was actually launched",
    /// and it is a guess with a proof behind it rather than a heuristic: a
    /// process cannot start before its parent, so the last member to start has
    /// no descendants inside the group. Where a launcher starts a game which
    /// re-executes itself, this is the surviving copy.
    ///
    /// It is still only a guess about *which* process matters — see
    /// [`LaunchGroup`].
    ///
    /// # Panics
    ///
    /// Never: a group is only built with at least one member.
    #[must_use]
    pub fn newest(&self) -> &ProcessSnapshot {
        self.processes
            .last()
            .expect("a launch group always has at least one member")
    }
}

/// A process the watcher had reported has gone.
///
/// Only processes that were reported are reported as exiting. A launcher that
/// disappeared inside its own debounce window was never news, and neither is
/// its exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessExit {
    /// The process, exactly as it was reported when it started.
    ///
    /// It is remembered rather than looked up, because by the time anything
    /// learns a process has exited there is nothing left to ask: the identifier
    /// may already belong to something else.
    pub process: ProcessSnapshot,

    /// The launch it belonged to, or [`LaunchId::ALREADY_RUNNING`].
    pub launch: LaunchId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_comes_from_the_path_when_there_is_one() {
        let process = ProcessSnapshot::new(
            10,
            4,
            Some(PathBuf::from(r"C:\Games\Adventure\game.exe")),
            "ignored.exe",
        );

        assert_eq!(process.image_name, "game.exe");
    }

    #[test]
    fn the_name_falls_back_when_the_path_is_unavailable() {
        // A protected process: the process table knows its name and nothing
        // else will open it.
        let process = ProcessSnapshot::new(10, 4, None, "anticheat.exe");

        assert_eq!(process.image_name, "anticheat.exe");
        assert_eq!(process.image_path, None);
    }

    #[test]
    fn a_path_with_no_final_component_falls_back_too() {
        let process = ProcessSnapshot::new(10, 4, Some(PathBuf::from(r"C:\")), "game.exe");

        assert_eq!(process.image_name, "game.exe");
    }

    #[test]
    fn the_reserved_launch_identifier_is_distinguishable() {
        assert!(LaunchId::ALREADY_RUNNING.is_already_running());
        assert!(!LaunchId::first().is_already_running());
        assert!(LaunchId::first() > LaunchId::ALREADY_RUNNING);
    }

    #[test]
    fn a_group_reports_its_ends() {
        let launcher = ProcessSnapshot::new(10, 4, None, "launcher.exe");
        let game = ProcessSnapshot::new(11, 10, None, "game.exe");
        let group = LaunchGroup {
            id: LaunchId::first(),
            processes: vec![launcher.clone(), game.clone()],
        };

        assert_eq!(group.root(), &launcher);
        assert_eq!(group.newest(), &game);
    }
}
