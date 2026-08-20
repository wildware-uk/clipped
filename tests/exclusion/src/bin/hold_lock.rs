//! Holds one resource until it is killed, so a test can be the loser.
//!
//! The whole premise of `clipped-test-exclusion` is that it excludes across
//! *processes*, and nothing inside one process can show that. This is the other
//! process: it takes the resource named on its command line, says so on
//! standard output, and then waits.
//!
//! It waits for ever rather than for a duration on purpose. A helper that let
//! go on its own would make the test that spawns it pass for the wrong reason
//! the day the timing drifted — the parent is supposed to be refused *while
//! this is alive*, and the parent is what ends it.
//!
//! ```text
//! cargo run -p clipped-test-exclusion --bin hold_lock -- foreground
//! ```

#![cfg(windows)]

use clipped_test_exclusion::{Exclusive, Resource};

fn main() {
    let named = std::env::args().nth(1).unwrap_or_else(|| {
        panic!("name the resource to hold: foreground, audio, fullscreen or capture")
    });

    let resource = match named.as_str() {
        "foreground" => Resource::Foreground,
        "audio" => Resource::DefaultAudioEndpoint,
        "fullscreen" => Resource::ExclusiveFullscreen,
        "capture" => Resource::CaptureMeasurement,
        other => panic!("no resource is called {other}"),
    };

    let held = Exclusive::acquire(resource).unwrap_or_else(|contended| panic!("{contended}"));

    // The parent waits for this line before asserting it cannot have the
    // resource. Without it the parent races the child's `WaitForSingleObject`
    // and would sometimes win, which is a flaky test asserting the opposite of
    // what it means.
    println!("held {}", held.resource());
    // Flushed explicitly: this process is killed rather than allowed to exit,
    // so nothing else will flush it.
    use std::io::Write as _;
    std::io::stdout()
        .flush()
        .expect("standard output can flush");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
