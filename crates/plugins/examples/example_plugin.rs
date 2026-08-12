//! A plugin, whole, in one file: the worked example `docs/plugin-api.md` walks
//! through.
//!
//! It is a complete plugin — a real one differs only in where the events come
//! from. This one invents a single kill so that there is something to look at;
//! a real integration would be reading Counter-Strike 2's Game State
//! Integration payloads, League of Legends' Live Client Data API, or a log file
//! the game writes (AGENTS.md section 34: official interfaces only, never a
//! game's memory).
//!
//! The whole of the contract is visible here:
//!
//! 1. Read the `attach` command from standard input.
//! 2. Say `hello`, with the contract version this plugin was written against.
//! 3. Print an event whenever something happens, saying **how long ago** it
//!    happened rather than when — the host owns the recording's timeline.
//! 4. Say `alive` while nothing is happening, more often than the host's
//!    silence timeout, or be treated as hung.
//! 5. Exit when standard input closes, which happens when the session ends and
//!    also when the host does.
//!
//! Nothing here mentions a recording, a file or a clock the host owns, and
//! there is no way to say who reported an event: the host stamps that from the
//! manifest.
//!
//! Run it by hand to see what it says:
//!
//! ```text
//! cargo run -p clipped-plugins --example example_plugin
//! {"command":"attach","contract":1,"session":{"session":"by-hand","process":{"executable":"cs2.exe","process_id":1}}}
//! ```

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clipped_events::EventKind;
use clipped_plugins::{
    hello, read_command, write_report, HostCommand, PluginReport, ReportedEvent,
};
use serde_json::json;

/// How often this plugin says it is still there.
///
/// Comfortably under `SupervisionPolicy::silence_timeout`, which is ten
/// seconds by default. A plugin whose own work can take longer than that
/// between reports has to send this from a thread of its own, as this one does.
const HEARTBEAT: Duration = Duration::from_millis(250);

fn main() {
    let mut output = io::stdout().lock();

    // 1. The host writes one `attach` line as soon as the process exists.
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        // Standard input closed before anything arrived: the host has gone.
        return;
    }
    let session = match read_command(line.trim_end()) {
        Ok(HostCommand::Attach { session, .. }) => session,
        Ok(HostCommand::Detach) => return,
        Err(error) => {
            // A plugin that cannot understand its host says so on standard
            // error — which is the host's standard error — and stops.
            eprintln!("example plugin: could not read the attach command: {error}");
            return;
        }
    };

    // 2. Introduce ourselves. Until this arrives the host counts the plugin as
    //    still starting, and gives up on it after a few seconds.
    let _ = output.write_all(write_report(&hello()).as_bytes());
    let _ = output.flush();

    // A real plugin would now open the game's own interface, using what it was
    // told about the process to find it.
    eprintln!(
        "example plugin: attached to session {} for {}",
        session.session, session.process
    );

    // 5. The host asks for a stop by writing `detach` and then closing this
    //    plugin's standard input, so reading it to the end is all that is
    //    needed to know when to leave.
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

    // 3. One event, where a real integration would report what the game said.
    //    `ago_ns` is how long before this line was written the thing happened:
    //    a payload that took 480 ms to arrive and be parsed describes something
    //    480 ms old, and saying so is what puts the mark in the right place.
    let kill = ReportedEvent {
        kind: EventKind::Kill,
        ago_ns: 480_000_000,
        precision_ns: 100_000_000,
        confidence: 1.0,
        data: json!({"weapon": "ak47", "headshot": true})
            .as_object()
            .expect("an object literal")
            .clone(),
    };
    let _ = output.write_all(write_report(&PluginReport::Event(kill)).as_bytes());
    let _ = output.flush();

    // 4. Nothing else is going to happen, so say so, until the host says stop.
    while !finished.load(Ordering::Relaxed) {
        if output
            .write_all(write_report(&PluginReport::Alive).as_bytes())
            .is_err()
        {
            // The host has gone. So should we: a plugin outliving the recorder
            // is a process nobody owns.
            break;
        }
        let _ = output.flush();
        thread::sleep(HEARTBEAT);
    }
}
