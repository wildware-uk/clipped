//! Runs the Battle.net provider against this machine's real installation.
//!
//! The fixtures in `launcher/battlenet/tests.rs` encode what the uninstall
//! entries are believed to look like. This asks the registry itself and then
//! asks the provider to claim every executable actually sitting in each install
//! directory — which is how the equivalent Epic probe found
//! [issue #459](https://github.com/wildware-uk/clipped/issues/459).

fn main() {
    let found = match clipped_game_detection::launcher::battlenet::BattleNet::discover() {
        Ok(Some(battle_net)) => battle_net,
        Ok(None) => {
            println!("Battle.net has installed nothing on this machine");
            return;
        }
        Err(error) => {
            println!("the registry refused: {error}");
            return;
        }
    };

    println!(
        "games: {}, problems: {}",
        found.apps().len(),
        found.problems().len()
    );
    for problem in found.problems() {
        println!("  problem: {problem}");
    }

    for app in found.apps() {
        let directory = app.installation_directory();
        println!(
            "\n  {} ({}) -> {}",
            app.name(),
            app.uid(),
            directory.display()
        );

        let mut checked = 0_u32;
        let mut wrong = 0_u32;
        for executable in executables(directory) {
            let claimed = found.app_for(&executable.to_string_lossy()).map_or(
                "<refused>",
                clipped_game_detection::launcher::battlenet::BattleNetApp::uid,
            );
            checked += 1;
            if claimed != app.uid() {
                wrong += 1;
                println!(
                    "    !! {} -> {claimed}",
                    executable
                        .strip_prefix(directory)
                        .unwrap_or(&executable)
                        .display()
                );
            }
        }
        println!("    {checked} executables checked, {wrong} claimed by the wrong game");
    }
}

/// Every `.exe` below a directory, a few levels down.
fn executables(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(at: &std::path::Path, depth: u32, into: &mut Vec<std::path::PathBuf>) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, depth - 1, into);
            } else if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
            {
                into.push(path);
            }
        }
    }

    let mut found = Vec::new();
    walk(directory, 4, &mut found);
    found
}
