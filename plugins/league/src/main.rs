//! The plugin itself: the contract's five steps, and a poll in the middle.
//!
//! ```text
//! host  → {"command":"attach","contract":1,"session":{…}}
//! plugin→ {"report":"hello","contract":1}
//! plugin→ {"report":"event","kind":"kill","ago_ns":86600000000,"precision_ns":104000000,…}
//! plugin→ {"report":"alive"}
//! host  → {"command":"detach"}
//! ```
//!
//! Everything interesting is in the library beside it: `snapshot` reads a
//! payload, `watch` decides what it means, and `live_api` is the request. This
//! file is the loop that joins them, and it is deliberately the only place that
//! reads a clock or sleeps.
//!
//! Run it by hand — which is how somebody with League installed verifies it,
//! and is what `docs/plugin-api.md` describes:
//!
//! ```text
//! cargo run -p clipped-league-plugin
//! {"command":"attach","contract":1,"session":{"session":"by-hand","process":{"executable":"League of Legends.exe","process_id":1}}}
//! ```

#[cfg(windows)]
fn main() {
    windows_main::run();
}

/// League of Legends and Clipped are both Windows applications (SPEC.md section
/// 3), and this plugin's only interface to the game is WinHTTP. There is no
/// second implementation for another platform, and saying so is better than a
/// binary that starts, reports nothing and looks like an integration that does
/// not work.
#[cfg(not(windows))]
fn main() {
    eprintln!(
        "clipped-league-plugin runs on Windows: it reads League of Legends' Live Client Data API \
         through WinHTTP, and both the game and Clipped are Windows applications."
    );
    std::process::exit(1);
}

#[cfg(windows)]
mod windows_main {
    use std::io::{self, BufRead, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use clipped_league_plugin::live_api::{Answer, LiveClientApi};
    use clipped_league_plugin::{LeagueWatch, PollResult, POLL_INTERVAL};
    use clipped_plugins::{hello, read_command, write_report, HostCommand, PluginReport};

    /// How long the loop sleeps at a time while waiting for the next poll.
    ///
    /// The poll interval, in slices, so that a `detach` arriving a moment after
    /// a poll is acted on then rather than a second later. A quarter of a
    /// second is far below the host's silence timeout and far above anything
    /// that costs a machine running a game.
    const SLICE: Duration = Duration::from_millis(250);

    /// The plugin, from `attach` to end of file.
    pub(crate) fn run() {
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
                eprintln!("league plugin: could not read the attach command: {error}");
                return;
            }
        };

        // 2. Introduce ourselves, before anything that can fail. A plugin that
        //    opened its socket first would be counted as never having started
        //    if the socket took longer than the host's patience.
        let _ = output.write_all(write_report(&hello()).as_bytes());
        let _ = output.flush();
        eprintln!(
            "league plugin: attached to session {} for {}",
            session.session, session.process
        );

        // 5. Standard input closing is how a session ends, and also how the
        //    host disappearing is noticed. Read on a thread of its own so that
        //    the loop below is never waiting on it.
        let finished = finished_when_input_closes();

        let api = match LiveClientApi::open() {
            Ok(api) => api,
            Err(error) => {
                // Nothing this plugin can do without a session handle, and the
                // user should hear why rather than watch a match produce no
                // marks (AGENTS.md section 45).
                eprintln!("league plugin: {error}");
                let _ = output.write_all(
                    write_report(&PluginReport::Problem {
                        message: "Clipped could not open an HTTPS client for League's Live Client \
                                  Data API, so this match will not be marked."
                            .to_owned(),
                    })
                    .as_bytes(),
                );
                let _ = output.flush();
                return;
            }
        };

        let mut watch = LeagueWatch::new();
        let attached = Instant::now();
        let mut said_why_it_is_quiet = false;

        while !finished.load(Ordering::Relaxed) {
            let answer = api.snapshot();
            let poll = match &answer {
                Answer::Body { body, round_trip } => {
                    said_why_it_is_quiet = false;
                    PollResult::Answered {
                        body,
                        round_trip: *round_trip,
                    }
                }
                Answer::NoMatch => {
                    said_why_it_is_quiet = false;
                    PollResult::NoMatch
                }
                Answer::Unreachable { because } => {
                    // Once per outage, on standard error: before a match has
                    // loaded this is the normal state of the world, and a line
                    // a second would bury the log it is in (AGENTS.md section
                    // 35).
                    if !said_why_it_is_quiet {
                        said_why_it_is_quiet = true;
                        eprintln!("league plugin: {because}");
                    }
                    PollResult::Unreachable
                }
            };

            // 3. What the game said, if anything, and 4. that we are still here
            //    whether or not it did.
            let mut reports = watch.observe(poll, attached.elapsed());
            reports.push(PluginReport::Alive);
            for report in reports {
                if output
                    .write_all(write_report(&report).as_bytes())
                    .and_then(|()| output.flush())
                    .is_err()
                {
                    // The host has gone. So should we: a plugin outliving the
                    // recorder is a process nobody owns.
                    return;
                }
            }

            sleep_until_the_next_poll(&finished);
        }
    }

    /// A flag that becomes true when the host says stop, or goes away.
    fn finished_when_input_closes() -> Arc<AtomicBool> {
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

    /// Waits out the poll interval, in slices, giving up early on a `detach`.
    fn sleep_until_the_next_poll(finished: &AtomicBool) {
        let until = Instant::now() + POLL_INTERVAL;
        while Instant::now() < until && !finished.load(Ordering::Relaxed) {
            thread::sleep(SLICE.min(until.saturating_duration_since(Instant::now())));
        }
    }
}
