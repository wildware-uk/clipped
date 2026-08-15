//! Asks every launcher on this machine about every process running on it.
//!
//! The aggregate of the six per-launcher probes beside this one, and the one
//! that would have caught [issue #522](https://github.com/wildware-uk/clipped/issues/522):
//! the providers were each verified against a real installation, and nothing
//! asked them about a real *process*, so the rung they produce reached nothing.
//!
//! It answers three questions a per-launcher probe cannot:
//!
//! 1. **Does discovery work as a set?** One line per launcher, with how many
//!    applications it found and what it could not read.
//! 2. **Does anything running get claimed?** Every process this account can see
//!    a path for, against every launcher, printed when one claims it.
//! 3. **Would the catalogue place it?** The claim is only worth having if an
//!    entry carries the same identifier, so the shipped catalogue is asked as
//!    well — which is what makes the difference between "Steam says this is
//!    application 730" and "Clipped knows that is Counter-Strike 2" visible.
//!
//! It is a probe rather than a test because what it reads is this machine's:
//! which launchers are installed, and what is running right now.
//!
//! A path can be named instead, which is how to get an answer without starting
//! a game: it asks the same question of that path and prints what the launchers
//! and the catalogue say.
//!
//! ```text
//! cargo run -p clipped-game-detection --example launchers_probe
//! cargo run -p clipped-game-detection --example launchers_probe -- "C:\Riot Games\League of Legends\LeagueClient.exe"
//! ```

fn main() {
    let launchers = clipped_game_detection::launcher::Launchers::discover();

    println!("=== launchers ===");
    if launchers.is_empty() {
        println!("  none installed");
    }
    for problem in launchers.problems() {
        println!("  problem: {problem}");
    }
    // Each provider's own probe prints its applications; this one is about
    // whether the *set* answers, so it says only whether anything was found.
    println!(
        "  something installed: {}, problems: {}",
        !launchers.is_empty(),
        launchers.problems().len()
    );

    let catalogue = match clipped_game_detection::catalogue::Catalogue::seed() {
        Ok(catalogue) => catalogue,
        Err(error) => {
            println!("the shipped catalogue could not be read: {error}");
            return;
        }
    };

    if let Some(path) = std::env::args().nth(1) {
        let name = std::path::Path::new(&path)
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        report(&launchers, &catalogue, &name, &path);
        return;
    }

    let watcher = match clipped_game_detection::ProcessWatcher::start(
        clipped_game_detection::WatchConfig::default(),
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            println!("the process table could not be read: {error}");
            return;
        }
    };

    println!("\n=== processes ===");
    let mut seen = 0_u32;
    let mut with_a_path = 0_u32;
    let mut claimed = 0_u32;
    let mut placed = 0_u32;

    for process in watcher.already_running() {
        seen += 1;
        let Some(path) = process.image_path.as_ref() else {
            continue;
        };
        with_a_path += 1;

        let path = path.to_string_lossy();
        let (was_claimed, was_placed) = report(&launchers, &catalogue, &process.image_name, &path);
        claimed += u32::from(was_claimed);
        placed += u32::from(was_placed);
    }

    println!(
        "\n{seen} processes, {with_a_path} with a readable path, {claimed} claimed by a launcher, \
         {placed} placed by the catalogue"
    );
    if claimed > 0 && placed == 0 {
        println!(
            "\nA launcher claimed something and the catalogue placed none of it. That is what an \
             identity with no `app_id` to match looks like — see issue #514."
        );
    }
}

/// What the launchers and the catalogue make of one process, printed when
/// anything claims it.
///
/// Returns whether it was claimed and whether the catalogue placed it, which is
/// the difference this probe exists to show: a claim nothing can match is an
/// identity with no `app_id` behind it.
fn report(
    launchers: &clipped_game_detection::launcher::Launchers,
    catalogue: &clipped_game_detection::catalogue::Catalogue,
    name: &str,
    path: &str,
) -> (bool, bool) {
    let candidate = launchers.candidate_for(name, path);
    let Some((kind, app_id)) = candidate.launcher() else {
        return (false, false);
    };

    let outcome = catalogue.match_process(&candidate);
    let entry = outcome
        .entry()
        .map(|entry| entry.game_id().as_str().to_owned());

    println!(
        "  {name} — {kind:?} {app_id} → {}",
        entry
            .clone()
            .unwrap_or_else(|| "no catalogue entry names it".to_owned())
    );
    (true, entry.is_some())
}
