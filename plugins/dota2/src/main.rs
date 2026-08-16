//! The Dota 2 highlight plugin, as the host runs it.
//!
//! Everything this file does is the plugin lifecycle `docs/plugin-api.md`
//! describes, in the order it describes it:
//!
//! 1. Read the `attach` command from standard input.
//! 2. Say `hello`.
//! 3. Set Dota up to report its state, and say so if it could not.
//! 4. Print an event whenever the state says something happened, saying **how
//!    long ago** rather than when.
//! 5. Say `alive` while nothing is happening.
//! 6. Exit when standard input closes.
//!
//! The parts worth understanding are not here — they are in
//! [`clipped_dota2_plugin::gsi`] (the transport, the configuration file and the
//! timing) and [`clipped_dota2_plugin::dota`] (what a change in the state
//! means), both of which are tested without a process, a socket or a game. This
//! file is the wiring, and it is deliberately thin enough to read in one sitting.
//!
//! # What it does to the machine
//!
//! Two things, both declared in `plugin.json` and both described in
//! `docs/privacy.md`:
//!
//! - It **listens on `127.0.0.1:3213`** for the game's Game State Integration
//!   payloads, and accepts only payloads carrying a token it generated.
//! - It **writes one file** into Dota's own configuration directory, named for
//!   Clipped, and touches nothing else there.
//!
//! It never reads the game's memory, never opens a handle to the game process
//! and never sends anything off this machine (AGENTS.md section 34).

use core::time::Duration;
use std::io::{self, BufRead, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use clipped_plugins::{
    hello, read_command, write_report, HostCommand, PluginReport, MAX_PROBLEM_BYTES,
};

use clipped_dota2_plugin::dota::{self, installation, Watcher};
use clipped_dota2_plugin::gsi::{
    remembered_token, Cadence, GameStateListener, Installation, Installed, Integration,
};
use clipped_dota2_plugin::{LISTEN_ADDRESS, PLUGIN_ID};

/// How often this plugin says it is still there when nothing is happening.
///
/// Comfortably under the host's ten-second silence timeout. Dota goes minutes
/// without an event during a laning phase, and a host that read silence as
/// health could not tell a quiet plugin from a deadlocked one
/// (`docs/plugin-api.md`).
const HEARTBEAT: Duration = Duration::from_secs(2);

/// What this program does, when it is not being a plugin.
#[derive(Debug, clap::Parser)]
#[command(name = "clipped-dota2-plugin", about, version)]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
}

/// The three things a person asks this program to do.
///
/// Separate from being a plugin on purpose, and the same separation
/// `clipped-cs2-plugin` makes. Installing writes a file into the user's game
/// directory, and `docs/plugin-api.md` does not allow that to be a side effect
/// of a session starting: it is something a person asks for, having been told
/// what it writes and where. A plugin attached without it says so and stops
/// ([issue #382](https://github.com/wildware-uk/clipped/issues/382)).
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Write the Game State Integration configuration into Dota 2.
    ///
    /// Prints the full path of the file it wrote and what it asks the game to
    /// do. Nothing else in your game directory is touched, and `uninstall`
    /// removes it.
    Install {
        /// Dota 2's `gamestate_integration` folder. Found from Steam's own
        /// records when it is not given, which is what it is for.
        game_directory: Option<PathBuf>,
    },
    /// Remove the configuration file this plugin wrote, and nothing else.
    Uninstall {
        /// As above.
        game_directory: Option<PathBuf>,
    },
    /// Say whether the integration is set up, and where.
    Status,
}

fn main() -> ExitCode {
    match <Arguments as clap::Parser>::parse().command {
        None => run_as_plugin(),
        Some(Command::Install { game_directory }) => report(install(game_directory.as_deref())),
        Some(Command::Uninstall { game_directory }) => report(uninstall(game_directory.as_deref())),
        Some(Command::Status) => report(status()),
    }
}

/// Prints what a subcommand had to say, and turns a failure into an exit code.
fn report(outcome: Result<String, String>) -> ExitCode {
    match outcome {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// The plugin proper.
fn run_as_plugin() -> ExitCode {
    let mut output = io::stdout().lock();

    // 1. The host writes one `attach` line as soon as the process exists.
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return ExitCode::SUCCESS;
    }
    let session = match read_command(line.trim_end()) {
        Ok(HostCommand::Attach { session, .. }) => session,
        Ok(HostCommand::Detach) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dota 2 plugin: could not read the attach command: {error}");
            return ExitCode::FAILURE;
        }
    };

    // 2. Introduce ourselves. Until this arrives the host counts the plugin as
    //    still starting.
    say(&mut output, &hello());
    eprintln!(
        "dota 2 plugin: attached to session {} for {}",
        session.session, session.process
    );

    // 3. A token, a socket, and a configuration file — in that order, because
    //    each one is worth reporting on its own and the socket is the one that
    //    decides whether there is any point in the rest.
    // Before the socket, and before anything is written anywhere: a plugin
    // attached to a Dota that was never set up has nothing to listen for, and
    // saying so is more use than a listener nobody posts to (issue #382).
    if let Err(reason) = installed_configuration() {
        problem(&mut output, &reason);
        return ExitCode::SUCCESS;
    }

    let Some(token) = token(&mut output) else {
        return ExitCode::FAILURE;
    };
    let address: SocketAddr = LISTEN_ADDRESS
        .parse()
        .expect("this plugin's own listen address is a socket address");
    let listener = match GameStateListener::bind(address, token.clone()) {
        Ok(listener) => listener,
        Err(error) => {
            problem(
                &mut output,
                &format!(
                    "Clipped could not listen for Dota 2's game state on {LISTEN_ADDRESS}: \
                     {error}"
                ),
            );
            // Exiting rather than idling. The host's supervisor treats an exit
            // as trouble worth a bounded number of retries and then reports it
            // to the user (`docs/plugin-api.md`); a process that stayed alive
            // reporting nothing would look like an integration with nothing to
            // say.
            return ExitCode::FAILURE;
        }
    };
    let opened = Instant::now();

    let payloads = listener.serve("dota 2 plugin");
    let mut cadence = Cadence::opened_at(opened);
    let mut watcher = Watcher::new();

    // 6. The host asks for a stop by writing `detach` and then closing this
    //    plugin's standard input, so reading it to the end is all that is
    //    needed to know when to leave — including when the host has died
    //    without saying anything.
    let finished = Arc::new(AtomicBool::new(false));
    let watching = Arc::clone(&finished);
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            match line.as_deref().map(str::trim_end).map(read_command) {
                Ok(Ok(HostCommand::Detach)) | Err(_) => break,
                Ok(_) => {}
            }
        }
        watching.store(true, Ordering::Relaxed);
    });

    while !finished.load(Ordering::Relaxed) {
        match payloads.recv_timeout(HEARTBEAT) {
            Ok(payload) => {
                // 4. The window this payload closed decides where its events
                //    go and how precisely they are known; the watcher decides
                //    what they are. Neither knows about the other.
                let window = cadence.observe(payload.received());
                let observed = watcher.observe(payload.state());
                if let Some(notice) = observed.notice {
                    // On the `problem` channel because contract 1 has no other
                    // one, and because it is a problem in the sense that
                    // variant means: a recording that will have no marks on it,
                    // with something the user can do about it. An
                    // informational channel — for a plugin that has something
                    // to say and nothing to complain about — would be a
                    // contract change, and is
                    // [#344](https://github.com/wildware-uk/clipped/issues/344).
                    problem(&mut output, notice.message());
                }
                for report in observed.reports {
                    say(
                        &mut output,
                        &PluginReport::Event(window.report(report.kind, report.data)),
                    );
                }
            }
            // 5. Nothing has happened. Say so.
            Err(RecvTimeoutError::Timeout) => say(&mut output, &PluginReport::Alive),
            Err(RecvTimeoutError::Disconnected) => {
                problem(
                    &mut output,
                    "Clipped stopped listening for Dota 2's game state unexpectedly.",
                );
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}

/// The token this machine's Dota is configured with, or nothing and a reason.
fn token(output: &mut impl Write) -> Option<clipped_dota2_plugin::gsi::AuthToken> {
    let Some(directory) = clipped_logging::application_directory() else {
        problem(
            output,
            "Clipped could not find a place to keep the token Dota 2 identifies itself with, so \
             it cannot listen for Dota 2's game state.",
        );
        return None;
    };
    let directory = directory.join("plugins").join(PLUGIN_ID);
    match remembered_token(&directory) {
        Ok((token, _)) => Some(token),
        Err(error) => {
            problem(output, &format!("{error}"));
            None
        }
    }
}

/// Where Dota's configuration file is, or why it cannot be found.
fn configuration_path(given: Option<&Path>) -> Result<PathBuf, String> {
    let directory = match given {
        Some(directory) => directory.to_path_buf(),
        None => installation::configuration_directory().map_err(|error| error.to_string())?,
    };
    Ok(directory.join(installation::CONFIG_FILE))
}

/// Whether Dota has been set up, and what to say when it has not.
///
/// The check the plugin makes on attach. It never writes: putting the file in
/// place is `install`'s, because writing into somebody's game directory is a
/// thing they ask for rather than something that happens to them when a game
/// starts (`docs/plugin-api.md`, issue #382).
fn installed_configuration() -> Result<PathBuf, String> {
    let path = configuration_path(None).map_err(|reason| {
        format!(
            "Clipped could not work out where Dota 2 keeps its Game State Integration configuration, so it cannot tell whether it is set up: {reason}"
        )
    })?;

    if path.is_file() {
        return Ok(path);
    }

    Err(format!(
        "Dota 2 is not set up to report its state, so this recording will have no events. Run `clipped-dota2-plugin install` once, then restart Dota 2. It writes {} and nothing else.",
        path.display()
    ))
}

/// The token this plugin listens with, kept beside its own settings.
fn plugin_token() -> Result<clipped_dota2_plugin::gsi::AuthToken, String> {
    let directory = clipped_logging::application_directory().ok_or_else(|| {
        "Clipped could not find a place to keep the token Dota 2 identifies itself with".to_owned()
    })?;
    let directory = directory.join("plugins").join(PLUGIN_ID);
    remembered_token(&directory)
        .map(|(token, _)| token)
        .map_err(|error| error.to_string())
}

/// What this plugin asks Dota to send, rendered with `token`.
fn rendered(token: &clipped_dota2_plugin::gsi::AuthToken) -> Result<String, String> {
    Integration::new(
        "Clipped",
        &format!("http://{LISTEN_ADDRESS}/"),
        dota::COMPONENTS,
    )
    .map(|integration| integration.render(token))
    .map_err(|error| error.to_string())
}

/// `install`: the only thing this program ever writes into a game directory.
fn install(game_directory: Option<&Path>) -> Result<String, String> {
    let path = configuration_path(game_directory)?;
    let directory = path
        .parent()
        .ok_or_else(|| "that configuration path has no directory".to_owned())?
        .to_path_buf();
    let token = plugin_token()?;
    let contents = rendered(&token)?;

    let installation =
        Installation::new(directory, installation::CONFIG_FILE).map_err(|e| e.to_string())?;
    let outcome = installation.apply(&contents).map_err(|e| e.to_string())?;

    let written = match outcome {
        Installed::Written { path } => path,
        Installed::AlreadyCurrent { path } => {
            return Ok(format!(
                "{} is already what Clipped would write. Nothing was changed.",
                path.display()
            ))
        }
    };

    Ok(format!(
        "Wrote {}
         
         It asks Dota 2 to post its state to http://{LISTEN_ADDRESS}/ while you play, with a          token so that nothing else on this machine can pretend to be the game.
         Nothing leaves this computer. Delete that file, or run `uninstall`, to stop it.
         
         Restart Dota 2 for it to take effect.",
        written.display()
    ))
}

/// `uninstall`: takes back exactly what `install` wrote, and nothing else.
fn uninstall(game_directory: Option<&Path>) -> Result<String, String> {
    let path = configuration_path(game_directory)?;
    if !path.is_file() {
        return Ok("There was no configuration file to remove.".to_owned());
    }

    // Only a file this plugin would have written. A file somebody else put
    // there under the same name is theirs, and deleting it because it is in the
    // way is not this program's decision to make.
    let token = plugin_token()?;
    let ours = rendered(&token)?;
    let found = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "{} could not be read, so it was left alone: {error}",
            path.display()
        )
    })?;
    if found.trim() != ours.trim() {
        return Err(format!(
            "{} was written by something other than Clipped, or edited by hand, and has been left alone. Delete it yourself if you meant to.",
            path.display()
        ));
    }

    std::fs::remove_file(&path)
        .map_err(|error| format!("{} could not be removed: {error}", path.display()))?;

    Ok(format!(
        "Removed {}
Dota 2 will stop posting its state next time it starts.",
        path.display()
    ))
}

/// `status`: what a person needs before they can ask a useful question.
fn status() -> Result<String, String> {
    let path = match configuration_path(None) {
        Ok(path) => path,
        Err(reason) => {
            return Ok(format!(
                "Not installed, and where it would go is unknown: {reason}"
            ))
        }
    };

    if !path.is_file() {
        return Ok(format!(
            "Not installed.
             Run `clipped-dota2-plugin install` to set it up. It will write {} and listen on {LISTEN_ADDRESS}.",
            path.display()
        ));
    }

    let token = plugin_token()?;
    let ours = rendered(&token)?;
    let found = std::fs::read_to_string(&path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()))?;

    if found.trim() == ours.trim() {
        Ok(format!(
            "Installed: {}
Dota 2 posts to http://{LISTEN_ADDRESS}/ with a token this plugin checks on every payload.",
            path.display()
        ))
    } else {
        Ok(format!(
            "{} is there and is not what Clipped would write — another tool's, or edited by hand. It has been left alone; run `install` to replace it with Clipped's.",
            path.display()
        ))
    }
}

/// Writes one report to the host.
fn say(output: &mut impl Write, report: &PluginReport) {
    if output.write_all(write_report(report).as_bytes()).is_err() || output.flush().is_err() {
        // The host has gone. So should we: a plugin outliving the recorder is a
        // process nobody owns.
        std::process::exit(0);
    }
}

/// Reports something the user can act on, within the length the host accepts.
///
/// Truncated here rather than by the host, which would drop the whole report:
/// a message that is one sentence too long is still worth most of a sentence,
/// and every message this plugin sends is a sentence the user is meant to read
/// (AGENTS.md section 45).
fn problem(output: &mut impl Write, message: &str) {
    const ELLIPSIS: char = '…';

    let mut message = message.to_owned();
    if message.len() > MAX_PROBLEM_BYTES {
        let mut cut = MAX_PROBLEM_BYTES - ELLIPSIS.len_utf8();
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        message.truncate(cut);
        message.push(ELLIPSIS);
    }
    say(output, &PluginReport::Problem { message });
}
