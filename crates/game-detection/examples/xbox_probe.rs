//! Runs the Xbox provider against this machine's real gaming services registry.
//!
//! The fixtures in `launcher/xbox/tests.rs` encode what the repository is
//! believed to look like. This asks the registry itself, which is how the
//! equivalent Epic probe found
//! [issue #459](https://github.com/wildware-uk/clipped/issues/459).
//!
//! `WindowsApps` is not readable by an ordinary process, so this cannot walk the
//! executables inside each package the way `ubisoft_probe` does. What it can do
//! is check the two things that would silently produce nothing: that a family
//! name came out of every package full name, and that a path *inside* each
//! recorded directory is claimed by the package that recorded it.

fn main() {
    let found = match clipped_game_detection::launcher::xbox::Xbox::discover() {
        Ok(Some(xbox)) => xbox,
        Ok(None) => {
            println!("no Xbox games are registered on this machine");
            return;
        }
        Err(error) => {
            println!("the registry refused: {error}");
            return;
        }
    };

    println!(
        "packages: {}, problems: {}",
        found.apps().len(),
        found.problems().len()
    );
    for problem in found.problems() {
        println!("  problem: {problem}");
    }

    let mut wrong = 0;
    for app in found.apps() {
        let directory = app.installation_directory();
        // A path a running process would report, inside this package.
        let inside = directory.join("game.exe");
        let claimed = found
            .app_for(&inside.to_string_lossy())
            .map_or("<refused>", |found| found.family_name());
        let mark = if claimed == app.family_name() {
            "ok "
        } else {
            wrong += 1;
            "!! "
        };
        println!(
            "  {mark} {:<46} {:<14} {}",
            app.family_name(),
            app.name(),
            directory.display()
        );
        if claimed != app.family_name() {
            println!("        claimed by {claimed}");
        }
    }
    println!("\n{wrong} claimed by the wrong package");
}
