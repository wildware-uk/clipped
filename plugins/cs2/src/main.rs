//! The Counter-Strike 2 plugin, as a program.
//!
//! With no arguments it is a plugin: it reads `attach` from its standard input,
//! says `hello`, and prints one JSON object per line for as long as the session
//! lasts. That is how Clipped starts it, and it is the whole of the contract
//! (`docs/plugin-api.md`, "Writing a plugin").
//!
//! With a subcommand it is the tool that sets Game State Integration up:
//!
//! ```text
//! clipped-cs2-plugin install "C:\Program Files (x86)\Steam\steamapps\common\Counter-Strike Global Offensive"
//! clipped-cs2-plugin status
//! clipped-cs2-plugin uninstall
//! ```
//!
//! Those are separate on purpose. Installing writes a file into the user's game
//! directory, and `docs/privacy.md` does not allow that to be a side effect of
//! launching a game: it is something a person asks for, having been told what
//! it writes and where. A plugin attached without it says so and keeps running.
//!
//! # The loop
//!
//! ```text
//!  standard input          main thread                 listener thread
//!  ──────────────          ───────────                 ───────────────
//!  attach ──────────────▶  hello
//!                          read the .cfg, bind ──────▶ 127.0.0.1:port
//!                          recv_timeout(HEARTBEAT) ◀── a payload
//!                          derive, print events
//!                          nothing for a while
//!                          print `alive`
//!  detach ──────────────▶  finish
//! ```
//!
//! Nothing blocks on the game. A Counter-Strike that stops posting is a channel
//! that goes quiet, and the plugin keeps saying `alive` — because a host that
//! read silence as health could not tell a quiet game from a deadlocked plugin,
//! and would kill this one for the game's inactivity.

use core::time::Duration;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use clap::{Parser, Subcommand};
use clipped_plugins::{
    hello, read_command, write_report, HostCommand, PluginReport, ReportedEvent, MAX_PROBLEM_BYTES,
};

use clipped_cs2_plugin::derive::{DerivedEvent, MatchTracker, StepNote, CONFIDENCE};
use clipped_cs2_plugin::integration::{self, Installed, SetupError, DEFAULT_PORT};
use clipped_cs2_plugin::listener::{GsiListener, ReceivedPayload, Refusal};
use clipped_cs2_plugin::location::{plugin_directory, InstallRecord};
use clipped_cs2_plugin::payload::GsiPayload;
use clipped_cs2_plugin::token;

/// How often the plugin says it is still there when nothing is happening.
///
/// Well under `SupervisionPolicy::silence_timeout`, which is ten seconds by
/// default. Counter-Strike's own heartbeat is longer than this, so most of
/// these are sent while the game is perfectly happy and simply quiet.
const HEARTBEAT: Duration = Duration::from_secs(2);

/// Clipped's Counter-Strike 2 highlight plugin.
///
/// Run with no arguments, it speaks the Clipped plugin protocol on standard
/// input and output. The subcommands set up the game's Game State Integration
/// configuration, which is the one file this program writes into your game.
#[derive(Debug, Parser)]
#[command(name = "clipped-cs2-plugin", version, about, long_about = None)]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Write the Game State Integration configuration into Counter-Strike 2.
    ///
    /// Prints the full path of the file it wrote and what is in it. Nothing
    /// else in your game directory is touched, and `uninstall` removes it.
    Install {
        /// Counter-Strike 2's folder: Steam → Counter-Strike 2 → Manage →
        /// Browse local files.
        game_directory: PathBuf,
        /// The loopback port to ask the game to post to.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Replace a configuration file Clipped wrote earlier, with a new
        /// token. Never replaces a file another tool wrote.
        #[arg(long)]
        replace: bool,
    },
    /// Remove the configuration file this plugin wrote, and nothing else.
    Uninstall {
        /// Counter-Strike 2's folder. Defaults to where `install` left it.
        game_directory: Option<PathBuf>,
    },
    /// Say whether the integration is set up, and where.
    Status,
}

fn main() -> ExitCode {
    match Arguments::parse().command {
        None => run_as_plugin(),
        Some(Command::Install {
            game_directory,
            port,
            replace,
        }) => report(install(&game_directory, port, replace)),
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

/// `install`: the only thing this program ever writes into a game directory.
fn install(game_directory: &Path, port: u16, replace: bool) -> Result<String, String> {
    let directory = plugin_directory().map_err(|error| error.to_string())?;
    let token = token::generate().map_err(|status| {
        format!("Windows would not produce a random token for the game to authenticate with ({status:?}), so nothing was written")
    })?;

    let installed = integration::install(game_directory, port, &token, replace)
        .map_err(|error: SetupError| error.to_string())?;
    let record = InstallRecord {
        configuration: installed.path.clone(),
    };
    let record_path = record
        .write(&directory)
        .map_err(|error| error.to_string())?;

    Ok(format!(
        "Wrote {}\n\
         \n\
         It asks Counter-Strike 2 to post its state to http://127.0.0.1:{}/ while you play, \
         with a token so that nothing else on this machine can pretend to be the game.\n\
         Nothing leaves this computer. Delete that file, or run `uninstall`, to stop it.\n\
         \n\
         Remembered in {}\n\
         Restart Counter-Strike 2 for it to take effect.",
        installed.path.display(),
        installed.port,
        record_path.display()
    ))
}

/// `uninstall`: takes back exactly what `install` wrote.
fn uninstall(game_directory: Option<&Path>) -> Result<String, String> {
    let directory = plugin_directory().map_err(|error| error.to_string())?;
    let remembered = InstallRecord::read(&directory).map_err(|error| error.to_string())?;

    let game_directory =
        match (game_directory, remembered.as_ref()) {
            (Some(given), _) => given.to_path_buf(),
            // The recorded path is the configuration file itself; its directory is
            // the one `integration` works in.
            (None, Some(record)) => record
                .configuration
                .parent()
                .ok_or_else(|| "the recorded configuration path has no directory".to_owned())?
                .to_path_buf(),
            (None, None) => return Err(
                "Clipped has not installed Counter-Strike 2's Game State Integration, so there \
                 is nothing to remove. Give the game's folder if you installed it by hand."
                    .to_owned(),
            ),
        };

    let removed = integration::uninstall(&game_directory).map_err(|error| error.to_string())?;
    InstallRecord::remove(&directory).map_err(|error| error.to_string())?;

    Ok(match removed {
        Some(path) => format!(
            "Removed {}\nCounter-Strike 2 will stop posting its state next time it starts.",
            path.display()
        ),
        None => "There was no configuration file to remove.".to_owned(),
    })
}

/// `status`: what a person needs before they can ask a useful question.
fn status() -> Result<String, String> {
    let directory = plugin_directory().map_err(|error| error.to_string())?;
    let Some(record) = InstallRecord::read(&directory).map_err(|error| error.to_string())? else {
        return Ok(format!(
            "Not installed.\n\
             Run `clipped-cs2-plugin install <Counter-Strike 2 folder>` to set it up. \
             It will listen on 127.0.0.1:{DEFAULT_PORT}."
        ));
    };

    if !record.configuration.exists() {
        return Ok(format!(
            "Installed, but {} is no longer there — the game may have been moved or verified.\n\
             Run `install` again.",
            record.configuration.display()
        ));
    }

    let installed = integration::read(&record.configuration)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "{} was replaced by something other than Clipped and has been left alone.",
                record.configuration.display()
            )
        })?;

    Ok(format!(
        "Installed: {}\nCounter-Strike 2 posts to http://127.0.0.1:{}/ with a token this plugin \
         checks on every payload.",
        installed.path.display(),
        installed.port
    ))
}

/// The plugin proper.
fn run_as_plugin() -> ExitCode {
    let mut output = io::stdout().lock();

    // 1. The host writes one `attach` line as soon as the process exists.
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        // Standard input closed before anything arrived: the host has gone.
        return ExitCode::SUCCESS;
    }
    let session = match read_command(line.trim_end()) {
        Ok(HostCommand::Attach { session, .. }) => session,
        Ok(HostCommand::Detach) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clipped-cs2-plugin: could not read the attach command: {error}");
            return ExitCode::FAILURE;
        }
    };

    // 2. Introduce ourselves before doing anything that can fail, so that a
    //    plugin which cannot start is reported as one that *said* it could not,
    //    rather than as one that never introduced itself.
    let _ = output.write_all(write_report(&hello()).as_bytes());
    let _ = output.flush();
    eprintln!(
        "clipped-cs2-plugin: attached to session {} for {}",
        session.session, session.process
    );

    // 3. Find the configuration this plugin was asked to install, and open the
    //    socket it told the game about.
    let installed = match configured() {
        Ok(installed) => installed,
        Err(problem) => return give_up(&mut output, &problem),
    };
    let listener = match GsiListener::bind(installed.port) {
        Ok(listener) => listener,
        Err(error) => {
            return give_up(
                &mut output,
                &format!(
                    "Clipped cannot listen on 127.0.0.1:{} for Counter-Strike 2 game state: \
                     {error}",
                    installed.port
                ),
            )
        }
    };

    let (payloads, arriving) = mpsc::channel();
    let listening = thread::spawn(move || serve(&listener, &installed.token, &payloads));
    let finished = watch_for_detach();

    // 4. Everything from here is one payload at a time, on this thread.
    let result = report_events(&mut output, &arriving, &finished);

    drop(arriving);
    // The listener thread is blocked on `accept`, which nothing here can
    // interrupt. It is not detached and forgotten: this process is about to
    // exit, which closes the socket, and a thread waiting on a socket the
    // kernel has reclaimed is a thread that ends with the process.
    drop(listening);
    result
}

/// Reads what `install` left, or says what the user has to do.
fn configured() -> Result<Installed, String> {
    let directory = plugin_directory().map_err(|error| error.to_string())?;
    let record = InstallRecord::read(&directory)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "Counter-Strike 2 has no Game State Integration file for Clipped. Run \
             `clipped-cs2-plugin install` with the game's folder to add one."
                .to_owned()
        })?;
    integration::read(&record.configuration)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "{} was replaced by something other than Clipped, so this plugin has left it \
                 alone and has no port to listen on.",
                record.configuration.display()
            )
        })
}

/// Says what is wrong in the words the user is shown, and stops.
///
/// The message is a `problem` report rather than a log line, because an
/// integration that silently never works is worse than one that says why
/// (AGENTS.md section 45). Exiting is deliberate: the host restarts a plugin
/// that exits a bounded number of times and then leaves it stopped **and says
/// why**, which is the right end state for something only the user can fix.
fn give_up(output: &mut impl Write, problem: &str) -> ExitCode {
    let mut message = problem.to_owned();
    // The host bounds what it will show. Truncating here rather than there
    // means the user reads a shortened sentence instead of nothing at all.
    while message.len() > MAX_PROBLEM_BYTES {
        let _ = message.pop();
    }
    let _ = output.write_all(write_report(&PluginReport::Problem { message }).as_bytes());
    let _ = output.flush();
    ExitCode::FAILURE
}

/// The listener thread: payloads in, refusals counted and logged.
fn serve(listener: &GsiListener, token: &str, payloads: &Sender<ReceivedPayload>) {
    let mut unauthenticated = 0_u64;
    let mut malformed = 0_u64;
    listener.serve(token, payloads, |refusal| {
        // Counted as well as logged. A plugin reporting nothing because every
        // payload failed its token check looks exactly like a quiet game, and
        // the count is the difference (AGENTS.md section 15).
        let count = match refusal {
            Refusal::Unauthenticated => {
                unauthenticated += 1;
                unauthenticated
            }
            Refusal::Malformed { .. } | Refusal::Unreadable => {
                malformed += 1;
                malformed
            }
        };
        // The first of each kind, then every hundredth: a browser left open on
        // the port would otherwise fill a log file.
        if count == 1 || count % 100 == 0 {
            eprintln!("clipped-cs2-plugin: {refusal} ({count} so far)");
        }
    });
}

/// Watches standard input for `detach`, or for the host going away.
fn watch_for_detach() -> Arc<AtomicBool> {
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
    finished
}

/// The main loop: derive, print, and say `alive` when there is nothing to say.
fn report_events(
    output: &mut impl Write,
    arriving: &Receiver<ReceivedPayload>,
    finished: &AtomicBool,
) -> ExitCode {
    let mut tracker = MatchTracker::new();

    while !finished.load(Ordering::Relaxed) {
        let written = match arriving.recv_timeout(HEARTBEAT) {
            Ok(payload) => match GsiPayload::parse(&payload.body) {
                Ok(parsed) => {
                    let step = tracker.observe(&parsed, payload.received);
                    for note in &step.notes {
                        log(note);
                    }
                    write_events(output, &step.events)
                }
                Err(error) => {
                    // It authenticated and it is not readable. Worth one line
                    // each time, because it means the game's payload has
                    // changed shape and this plugin has stopped working.
                    eprintln!("clipped-cs2-plugin: a payload could not be read: {error}");
                    true
                }
            },
            // Nothing has happened. Saying so is required: a host that read
            // silence as health could not tell a quiet game from a deadlocked
            // plugin, and would stop this one for Counter-Strike being idle.
            Err(RecvTimeoutError::Timeout) => write(output, &PluginReport::Alive),
            // The listener thread has gone, which it only does when the socket
            // is finished with. Nothing more will arrive.
            Err(RecvTimeoutError::Disconnected) => return ExitCode::SUCCESS,
        };

        if !written {
            // The host has gone. So should we: a plugin outliving the recorder
            // is a process nobody owns.
            return ExitCode::SUCCESS;
        }
    }
    ExitCode::SUCCESS
}

/// Writes the events derived from one payload.
///
/// Answers whether the host is still listening.
fn write_events(output: &mut impl Write, events: &[DerivedEvent]) -> bool {
    // One clock reading for the whole batch, taken here rather than per event,
    // so that events the plugin could not separate stay inseparable.
    let now = Instant::now();
    events
        .iter()
        .all(|event| write(output, &PluginReport::Event(reported(event, now))))
}

/// Writes one report, and answers whether it went.
fn write(output: &mut impl Write, report: &PluginReport) -> bool {
    output.write_all(write_report(report).as_bytes()).is_ok() && output.flush().is_ok()
}

/// A derived event as the wire carries it.
///
/// The whole of the conversion is `ago_ns`: how long before this line was
/// written the thing happened. The plugin never says *when* on the recording's
/// timeline, because it does not have that timeline — the host subtracts this
/// from its own clock (`docs/plugin-api.md`, "How long ago, not when").
fn reported(event: &DerivedEvent, now: Instant) -> ReportedEvent {
    ReportedEvent {
        kind: event.kind.clone(),
        ago_ns: u64::try_from(now.saturating_duration_since(event.at).as_nanos())
            .unwrap_or(u64::MAX),
        precision_ns: u64::try_from(event.precision.as_nanos()).unwrap_or(u64::MAX),
        confidence: CONFIDENCE,
        data: event.data.clone(),
    }
}

/// What a payload said that was not an event.
fn log(note: &StepNote) {
    match note {
        // Ordinary and expected once per session.
        StepNote::Baselined => eprintln!(
            "clipped-cs2-plugin: taking the first payload as a baseline; events are reported \
             from here on"
        ),
        StepNote::Stale {
            stamped,
            last_accepted,
        } => eprintln!(
            "clipped-cs2-plugin: a payload stamped {stamped} arrived after one stamped \
             {last_accepted} and was discarded"
        ),
        StepNote::NotOrderable => eprintln!(
            "clipped-cs2-plugin: a payload carried no provider timestamp, so payloads that \
             arrive out of order cannot be detected"
        ),
        // Ordinary, and continuous for the rest of a round every time the
        // player dies. Logging it would fill a file with the fact that
        // somebody was watching a teammate (AGENTS.md section 35).
        StepNote::AboutAnotherPlayer => {}
        StepNote::CountersReset => eprintln!(
            "clipped-cs2-plugin: the match counters went backwards, so this plugin has \
             re-read them rather than reporting the difference"
        ),
    }
}
