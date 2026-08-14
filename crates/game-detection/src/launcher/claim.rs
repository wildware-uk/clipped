//! Which installation directory claims a running executable.
//!
//! Every provider asks the same question of a different set of directories, and
//! answers it with the same rule: **the deepest directory the path lies inside
//! wins**, because one installation can sit inside another and the more
//! specific answer is the right one.
//!
//! It lives here rather than three times over because the rule has two details
//! that are easy to get subtly wrong, and getting them wrong in one provider
//! and not another would be worse than either (AGENTS.md section 55):
//!
//! - **Segments, not strings.** The registry writes `C:/Program Files (x86)/…`
//!   and a running process reports `C:\Program Files (x86)\…`. A string compare
//!   claims nothing at all on a real machine.
//! - **Strictly deeper, not equal.** The last segment of an executable path is
//!   its file name, so a path *equal* to an installation directory is that
//!   directory and not a program in it.
//!
//! What it deliberately does not decide is what to do about a tie. Returning
//! every claimant at the deepest level leaves that to the provider, because the
//! providers genuinely differ: Epic can break a tie on the executable its
//! manifest names, and Ubisoft, whose registry records no executable, cannot.

use crate::catalogue::{normalise_path, path_segments};

/// Every entry whose installation directory claims `executable_path`, at the
/// deepest level any of them reach.
///
/// Empty when nothing claims it. More than one when several directories are
/// equally deep and equally correct, which is a real state — Epic installs
/// plugins into the thing they extend — and which the caller has to resolve or
/// refuse ([issue #459](https://github.com/wildware-uk/clipped/issues/459)).
pub(super) fn deepest_claimants<'a, T>(
    executable_path: &str,
    entries: &'a [T],
    directory_of: impl Fn(&T) -> String,
) -> Vec<&'a T> {
    let normalised = normalise_path(executable_path);
    let path: Vec<&str> = path_segments(&normalised).collect();

    let mut deepest = 0;
    let mut claimants: Vec<&T> = Vec::new();
    for entry in entries {
        let normalised = normalise_path(&directory_of(entry));
        let directory: Vec<&str> = path_segments(&normalised).collect();
        if directory.is_empty() || path.len() <= directory.len() {
            continue;
        }
        if !path.starts_with(&directory) {
            continue;
        }
        if directory.len() > deepest {
            deepest = directory.len();
            claimants.clear();
        }
        if directory.len() == deepest {
            claimants.push(entry);
        }
    }
    claimants
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory, standing in for whatever a provider's entry type is.
    fn dirs(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn the_deeper_of_two_nested_directories_wins_whichever_order_they_are_in() {
        let shallow_first = dirs(&["C:/Games", "C:/Games/Trackmania"]);
        let deep_first = dirs(&["C:/Games/Trackmania", "C:/Games"]);
        let path = r"C:\Games\Trackmania\Trackmania.exe";

        for entries in [&shallow_first, &deep_first] {
            let claimants = deepest_claimants(path, entries, Clone::clone);
            assert_eq!(claimants.len(), 1, "one directory is deeper than the other");
            assert_eq!(claimants[0], "C:/Games/Trackmania");
        }
    }

    #[test]
    fn a_path_equal_to_the_directory_is_not_a_program_in_it() {
        let entries = dirs(&["C:/Games/Trackmania"]);
        let claimants = deepest_claimants(r"C:\Games\Trackmania", &entries, Clone::clone);
        assert!(claimants.is_empty());
    }

    #[test]
    fn the_two_spellings_of_a_windows_path_name_the_same_directory() {
        let entries = dirs(&["C:/Program Files (x86)/Ubisoft/games/Trackmania/"]);
        let claimants = deepest_claimants(
            r"C:\Program Files (x86)\Ubisoft\games\Trackmania\Trackmania.exe",
            &entries,
            Clone::clone,
        );
        assert_eq!(claimants.len(), 1, "a string compare would find nothing");
    }

    #[test]
    fn several_directories_at_the_same_depth_all_come_back() {
        // The tie the caller has to resolve or refuse. Returning one of them
        // arbitrarily here is what #459 was.
        let entries = dirs(&["B:/Epic/UE_5.8", "B:/Epic/UE_5.8", "B:/Epic/Other"]);
        let claimants = deepest_claimants(
            r"B:\Epic\UE_5.8\Engine\Binaries\x.exe",
            &entries,
            Clone::clone,
        );
        assert_eq!(claimants.len(), 2, "both equally deep claims are reported");
    }

    #[test]
    fn a_path_inside_nothing_is_claimed_by_nothing() {
        let entries = dirs(&["C:/Games/Trackmania"]);
        let claimants = deepest_claimants(r"D:\Steam\cs2\cs2.exe", &entries, Clone::clone);
        assert!(claimants.is_empty());
    }

    #[test]
    fn a_directory_that_is_only_a_prefix_of_the_name_does_not_claim_it() {
        // `C:/Games/Track` is not a parent of `C:/Games/Trackmania`, though it
        // is a prefix of it as a string. Segments are what stops this.
        let entries = dirs(&["C:/Games/Track"]);
        let claimants = deepest_claimants(
            r"C:\Games\Trackmania\Trackmania.exe",
            &entries,
            Clone::clone,
        );
        assert!(claimants.is_empty());
    }
}
