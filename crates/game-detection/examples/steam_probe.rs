//! What Steam detection makes of *this* machine.
//!
//! The tests in `tests/steam.rs` run against fixtures, deliberately: a test
//! that needed somebody's Steam library would pass or fail for reasons that have
//! nothing to do with the code (AGENTS.md section 25). This is the other half —
//! the check that the fixtures still describe reality, run by hand against a
//! real installation with real libraries on real drives.
//!
//! ```powershell
//! cargo run -p clipped-game-detection --example steam_probe
//! cargo run -p clipped-game-detection --example steam_probe -- cs2.exe "B:\SteamLibrary\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\cs2.exe"
//! ```
//!
//! With no arguments it prints the libraries, the applications and anything that
//! could not be read. With an executable name and path it prints what the
//! catalogue makes of that process once Steam has had its say, which is the
//! whole pipeline issue #43 builds.
//!
//! It prints real paths, because the point of running it is to look at them.

#[cfg(not(windows))]
fn main() {
    eprintln!("Steam detection reads the Windows registry; this probe needs Windows.");
}

#[cfg(windows)]
fn main() {
    use clipped_game_detection::catalogue::Catalogue;
    use clipped_game_detection::launcher::steam::Steam;

    let steam = match Steam::discover() {
        Ok(Some(steam)) => steam,
        Ok(None) => {
            println!("Steam is not installed on this machine.");
            return;
        }
        Err(error) => {
            eprintln!("Steam could not be read: {error}");
            std::process::exit(1);
        }
    };

    println!("Steam:      {}", steam.root().display());
    println!("Libraries:  {}", steam.libraries().len());
    for library in steam.libraries() {
        println!("  {}", library.display());
    }
    println!("Apps:       {}", steam.apps().len());
    for app in steam.apps() {
        let icon = app.icon().map_or_else(
            || "no cached artwork".to_owned(),
            |icon| icon.display().to_string(),
        );
        println!(
            "  {:>8}  {:<48}  {}\n            {}",
            app.app_id(),
            app.name(),
            icon,
            app.installation_directory().display()
        );
    }
    println!("Problems:   {}", steam.problems().len());
    for problem in steam.problems() {
        println!("  {problem}");
    }

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [name, path] = arguments.as_slice() else {
        return;
    };

    let catalogue = match Catalogue::load() {
        Ok(loaded) => loaded.into_catalogue(),
        Err(error) => {
            eprintln!("the catalogue could not be loaded: {error}");
            std::process::exit(1);
        }
    };
    let candidate = steam.candidate_for(name, path);
    println!("\nProcess:    {name}\n            {path}");
    println!(
        "Steam says: {}",
        candidate.launcher().map_or_else(
            || "not a Steam application".to_owned(),
            |(kind, app_id)| format!("{kind} application {app_id}"),
        )
    );
    println!("Catalogue:  {:?}", catalogue.match_process(&candidate));
}
