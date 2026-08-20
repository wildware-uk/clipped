//! The exclusion holds between two processes, which is the whole claim.
//!
//! The unit tests beside the crate show it holding between two *threads*, and
//! that is not the same thing and not what
//! [issue #194](https://github.com/wildware-uk/clipped/issues/194) is about: an
//! in-process `Mutex` already excluded threads and did nothing for the two
//! `cargo test` binaries that were really competing. So this spawns a second
//! process, has it take the resource, and asks for it here.
//!
//! It also covers the half a lock file cannot do. The child is **killed**
//! rather than asked to stop, and the resource is available immediately
//! afterwards — because Windows abandons a mutex whose owner died. A test
//! binary killed part way through must not leave the next run waiting for a
//! lock nobody holds.

#![cfg(windows)]

use core::time::Duration;
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};

use clipped_test_exclusion::{Exclusive, Resource};

/// How long to wait when the answer is supposed to be "no".
///
/// Short, because the resource is held for the whole of it and the test is
/// spending that time on purpose.
const BRIEF: Duration = Duration::from_millis(750);

/// Starts the helper and waits until it says it has the resource.
///
/// Waiting for the line rather than for a duration: the parent asserts it
/// cannot have the resource, and a parent that raced the child's wait would
/// sometimes win and report the opposite of what it means.
fn holder(resource: &str) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hold_lock"))
        .arg(resource)
        .stdout(Stdio::piped())
        .spawn()
        .expect("the helper can be started");

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the helper says when it has the resource");
    assert!(
        line.starts_with("held "),
        "the helper should announce what it took, and said {line:?}"
    );
    child
}

#[test]
fn a_resource_another_process_holds_cannot_be_had_here() {
    let mut child = holder("foreground");

    let refused = Exclusive::acquire_within(Resource::Foreground, BRIEF)
        .expect_err("another process is holding it");

    let said = refused.to_string();
    assert!(
        said.contains("the foreground window"),
        "the refusal should name the resource: {said}"
    );
    assert!(
        said.contains("contention rather than a limitation"),
        "and should say the machine is busy rather than incapable: {said}"
    );

    child.kill().expect("the helper can be stopped");
    child.wait().expect("the helper can be waited for");
}

/*
 * The other direction, and what makes the case above mean something: with the
 * holder gone, the same call succeeds. Without this, a crate that refused
 * *everything* would pass the case above perfectly well.
 */
#[test]
fn a_resource_is_available_once_the_other_process_has_gone() {
    let mut child = holder("audio");
    assert!(
        Exclusive::acquire_within(Resource::DefaultAudioEndpoint, BRIEF).is_err(),
        "the helper is holding it"
    );

    // Killed rather than asked to stop. Windows abandons a mutex whose owner
    // died, and a test binary killed part way through must not poison the lock
    // for every later run — which is the failure a lock file has and this does
    // not.
    child.kill().expect("the helper can be stopped");
    child.wait().expect("the helper can be waited for");

    let after = Exclusive::acquire_within(Resource::DefaultAudioEndpoint, Duration::from_secs(5));
    assert!(
        after.is_ok(),
        "a resource whose holder was killed should be available at once, and was not: {:?}",
        after.err().map(|contended| contended.to_string())
    );
}

/*
 * Two resources are two exclusions across processes as well as within one. A
 * single lock would serialise a suite that needs the foreground behind one that
 * needs an audio endpoint, which is slower for no gain in correctness.
 */
#[test]
fn one_resource_being_held_elsewhere_does_not_hold_another() {
    let mut child = holder("fullscreen");

    let other = Exclusive::acquire_within(Resource::CaptureMeasurement, Duration::from_secs(5));
    assert!(
        other.is_ok(),
        "holding exclusive fullscreen in another process must not block a capture measurement: \
         {:?}",
        other.err().map(|contended| contended.to_string())
    );

    child.kill().expect("the helper can be stopped");
    child.wait().expect("the helper can be waited for");
}
