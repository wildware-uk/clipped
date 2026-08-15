//! Runs the Riot provider against this machine's real installation.
//!
//! The fixtures in `launcher/riot/tests.rs` encode what the metadata is
//! believed to look like. This asks the metadata itself, and then asks the
//! provider to claim every executable actually sitting in each product's
//! directory — which is how the equivalent Epic probe found
//! [issue #459](https://github.com/wildware-uk/clipped/issues/459).
//!
//! Run it with `cargo run -p clipped-game-detection --example riot_probe`. It
//! is a probe rather than a test because what it reads is this machine's, and
//! nothing in CI has Riot on it.

fn main() {
    let found = match clipped_game_detection::launcher::riot::Riot::discover() {
        Ok(Some(riot)) => riot,
        Ok(None) => {
            println!("the Riot client is not installed here");
            return;
        }
        Err(error) => {
            println!("the metadata refused: {error}");
            return;
        }
    };

    println!(
        "products: {}, problems: {}",
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
            app.id(),
            app.patchline(),
            directory.display()
        );

        let mut checked = 0_u32;
        let mut wrong = 0_u32;
        for executable in executables(directory) {
            let claimed = found.app_for(&executable.to_string_lossy()).map_or(
                "<refused>",
                clipped_game_detection::launcher::riot::RiotApp::id,
            );
            checked += 1;
            if claimed != app.id() {
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
        println!("    {checked} executables checked, {wrong} claimed by the wrong product");
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
