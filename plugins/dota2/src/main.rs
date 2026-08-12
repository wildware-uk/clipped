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

fn main() {
    let mut output = io::stdout().lock();

    // 1. The host writes one `attach` line as soon as the process exists.
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let session = match read_command(line.trim_end()) {
        Ok(HostCommand::Attach { session, .. }) => session,
        Ok(HostCommand::Detach) => return,
        Err(error) => {
            eprintln!("dota 2 plugin: could not read the attach command: {error}");
            return;
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
    let Some(token) = token(&mut output) else {
        return;
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
            return;
        }
    };
    let opened = Instant::now();
    configure(&mut output, &token);

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
                return;
            }
        }
    }
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

/// Puts the Game State Integration configuration in place, and says what that
/// means for the user.
///
/// Never fatal. A user who has installed the configuration file by hand, or who
/// is running a Dota that Steam has no manifest for, still gets a working
/// listener — so a failure here is something to report rather than a reason to
/// stop.
fn configure(output: &mut impl Write, token: &clipped_dota2_plugin::gsi::AuthToken) {
    let directory = match installation::configuration_directory() {
        Ok(directory) => directory,
        Err(error) => {
            problem(output, &format!("{error}"));
            return;
        }
    };

    let integration = match Integration::new(
        "Clipped",
        &format!("http://{LISTEN_ADDRESS}/"),
        dota::COMPONENTS,
    ) {
        Ok(integration) => integration,
        Err(error) => {
            problem(output, &format!("{error}"));
            return;
        }
    };
    let installation = match Installation::new(&directory, installation::CONFIG_FILE) {
        Ok(installation) => installation,
        Err(error) => {
            problem(output, &format!("{error}"));
            return;
        }
    };

    match installation.apply(&integration.render(token)) {
        // Valve's client reads this directory when *it* starts, so a file
        // written now is a file this session of Dota will never read
        // (`gsi::config`). Phrased as what is wrong rather than as what
        // succeeded, because the user's copy of it is a line on a `problem`
        // channel and "Clipped has set Dota 2 up" is not a thing to act on —
        // restarting Dota is (AGENTS.md section 28).
        Ok(Installed::Written { .. }) => problem(
            output,
            "Dota 2 has to be restarted before Clipped can report its events, so this recording \
             will not have any.",
        ),
        Ok(Installed::AlreadyCurrent { .. }) => {}
        Err(error) => problem(output, &format!("{error}")),
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
