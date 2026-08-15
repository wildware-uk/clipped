//! Runs the Epic provider against this machine's real installation.

fn main() {
    let Ok(Some(epic)) = clipped_game_detection::launcher::epic::Epic::discover() else {
        println!("Epic is not installed here");
        return;
    };

    println!(
        "applications: {}, problems: {}",
        epic.apps().len(),
        epic.problems().len()
    );

    for app in epic.apps() {
        let executable = app.executable();
        let name = executable.rsplit(['\\', '/']).next().unwrap_or("");
        let full = format!("{}\\{executable}", app.installation_directory().display());
        let claimed = epic
            .app_for(name, &full)
            .map_or("<refused>", |found| found.app_name());
        let mark = if claimed == app.app_name() {
            "ok "
        } else {
            "!! "
        };
        println!("  {mark} {:<34} -> {claimed}", app.app_name());
    }
}
