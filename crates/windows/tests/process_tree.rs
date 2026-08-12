//! A process tree against real processes, built by the test.
//!
//! The rules that decide membership are unit tested in
//! `src/process_tree.rs` against written-down process trees, where a recycled
//! identifier and a launcher that hands over can be arranged exactly. What
//! cannot be arranged there is Windows itself: that the process table really
//! reports the parentage this crate believes it does, that a handle really
//! keeps an identifier reserved after the process behind it has gone, and that
//! an orphan really keeps naming a parent that no longer exists. That is what
//! this file is for.
//!
//! The subject is a chain of three processes the test starts itself, so it
//! assumes no installed game and nothing about what else is running
//! (AGENTS.md sections 25 and 26):
//!
//! ```text
//! cmd.exe  ── waits for a line on its standard input, then starts ──▶
//!     cmd.exe ──▶ more.com  ── waits for that input to close ──▶ exits
//! ```
//!
//! Two properties make it the right subject. The descendants appear only when
//! the test says so, which is how "a game spawns a helper an hour in" is
//! reproduced in a second; and killing the first one leaves the other two
//! running with a parent identifier that names nothing, which is what a
//! launcher exiting under its game does.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use clipped_windows::{ProcessTree, WindowsError};

/// How long a test will wait for Windows to start or end the chain.
///
/// Generous on purpose: it bounds a hang rather than asserting a latency, and
/// every test below reaches its condition in well under a second on the machine
/// this was written on.
const PATIENCE: Duration = Duration::from_secs(20);

/// How many processes a started chain consists of: the root, the shell it
/// starts, and the `more` that shell starts.
const CHAIN_LENGTH: usize = 3;

/// The chain of processes a test scopes a tree to.
///
/// Owns every process it starts and ends all of them on the way out, whatever
/// the test did or did not do first.
struct Chain {
    root: Child,
    /// The write end of the chain's standard input. Writing a line makes the
    /// root start its descendants; closing it makes the deepest of them see
    /// end-of-file, which ends the chain from the bottom up.
    ///
    /// Held here rather than left inside `root` because `Child::wait` closes
    /// the child's pipes, which would end the descendants at the very moment a
    /// test is asserting that they outlive their parent.
    input: Option<ChildStdin>,
    descendants_started: bool,
}

impl Chain {
    /// Starts the root, which does nothing until [`Self::start_descendants`].
    fn start() -> Self {
        let mut root = Command::new("cmd.exe")
            // `set /p` reads one line from standard input; everything after the
            // `&` runs once it has one. `more` then holds the same input open
            // until it is closed.
            .args(["/c", "set /p go= & cmd.exe /c more"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("cmd.exe is on every Windows installation");
        let input = root.stdin.take().expect("standard input was piped");

        Self {
            root,
            input: Some(input),
            descendants_started: false,
        }
    }

    /// The identifier of the process at the top of the chain.
    fn root_pid(&self) -> u32 {
        self.root.id()
    }

    /// Makes the root start the two processes below it.
    fn start_descendants(&mut self) {
        let input = self.input.as_mut().expect("the chain is still open");
        input
            .write_all(b"go\r\n")
            .expect("the root is still reading");
        input.flush().expect("the root is still reading");
        self.descendants_started = true;
    }

    /// Ends the root and leaves its descendants running, orphaned.
    fn kill_root(&mut self) {
        self.root.kill().expect("the root is this test's own child");
        self.root.wait().expect("the root is this test's own child");
    }
}

impl Drop for Chain {
    fn drop(&mut self) {
        // Both halves are needed, and in this order. Killing the root ends only
        // the root — Windows does not end a process's children with it, which
        // is the whole reason this fixture is shaped the way it is — and
        // closing the input is what ends the two below it: `more` sees
        // end-of-file, exits, and the shell that started it follows. A test
        // that failed early must not leave processes behind on a machine that
        // is shared.
        let _ = self.root.kill();
        let _ = self.root.wait();
        self.input = None;
    }
}

/// Waits until the whole chain under `root_pid` is running.
///
/// On a *separate* tree, so that the tree a test is asserting about is left
/// exactly as the test left it: every assertion below is then made about a
/// known number of `refresh` calls rather than about however many a wait
/// happened to make.
fn wait_for_the_chain(root_pid: u32) {
    let deadline = Instant::now() + PATIENCE;
    let mut watcher = ProcessTree::rooted_at(root_pid)
        .expect("a process this test started can be opened")
        .with_rescan_interval(Duration::ZERO);

    while Instant::now() < deadline {
        watcher
            .refresh()
            .expect("the process table can always be read");
        if watcher.members().len() == CHAIN_LENGTH {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    panic!(
        "timed out waiting for the chain under {root_pid}; it holds {:?}",
        watcher.members()
    );
}

/// Refreshes until `condition` holds, or fails saying what it last saw.
fn refresh_until(tree: &mut ProcessTree, what: &str, condition: impl Fn(&ProcessTree) -> bool) {
    let deadline = Instant::now() + PATIENCE;

    while Instant::now() < deadline {
        tree.refresh()
            .expect("the process table can always be read");
        if condition(tree) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    panic!(
        "timed out waiting for {what}; the tree holds {:?}",
        tree.members()
    );
}

#[test]
fn a_child_started_after_the_tree_was_built_joins_it_and_so_does_its_own_child() {
    let mut chain = Chain::start();
    let mut tree = ProcessTree::rooted_at(chain.root_pid())
        .expect("a process this test started can be opened")
        .with_rescan_interval(Duration::ZERO);

    assert_eq!(
        tree.members(),
        [chain.root_pid()],
        "nothing has been started under the root yet"
    );

    chain.start_descendants();
    wait_for_the_chain(chain.root_pid());

    // Exactly one refresh, after the whole chain is up: both generations have
    // to arrive in the same call, or a game that started three processes deep
    // between scans would be adopted one generation per interval.
    let change = tree
        .refresh()
        .expect("the process table can always be read");

    assert_eq!(
        change.joined().len(),
        2,
        "both descendants have to be reported as joining, not only counted"
    );
    assert_eq!(tree.members().len(), CHAIN_LENGTH);
    assert!(
        tree.contains(chain.root_pid()),
        "the root stays a member while it runs"
    );
    // The deeper of the two is a grandchild: it was started by the shell the
    // root started, so a tree that only followed direct children would have
    // reported one process joining rather than two.
    for pid in change.joined() {
        assert!(tree.contains(*pid));
        assert_ne!(*pid, chain.root_pid());
    }
}

#[test]
fn a_parent_exiting_leaves_its_children_in_the_tree() {
    let mut chain = Chain::start();
    let mut tree = ProcessTree::rooted_at(chain.root_pid())
        .expect("a process this test started can be opened")
        .with_rescan_interval(Duration::ZERO);

    chain.start_descendants();
    wait_for_the_chain(chain.root_pid());
    let descendants: Vec<u32> = tree
        .refresh()
        .expect("the process table can always be read")
        .joined()
        .to_vec();

    // The launcher case: the process the tree is rooted at goes, and what it
    // started carries on with a parent identifier that names nothing.
    chain.kill_root();
    let change = tree
        .refresh()
        .expect("the process table can always be read");

    assert_eq!(change.exited(), [chain.root_pid()]);
    assert!(
        change.joined().is_empty(),
        "nothing new started, so nothing should have joined"
    );
    assert_eq!(
        tree.members(),
        descendants,
        "the orphans stay in the tree after their parent has gone"
    );
}

#[test]
fn a_child_that_appeared_only_after_its_parent_died_is_still_adopted() {
    let mut chain = Chain::start();
    // This tree is built while the root is the only process there is, and is
    // then not refreshed until after the root has gone: the two processes below
    // it are ones it has never seen, whose parent identifier names a process
    // that no longer exists.
    let mut tree = ProcessTree::rooted_at(chain.root_pid())
        .expect("a process this test started can be opened")
        .with_rescan_interval(Duration::ZERO);

    chain.start_descendants();
    wait_for_the_chain(chain.root_pid());
    chain.kill_root();

    let change = tree
        .refresh()
        .expect("the process table can always be read");

    assert_eq!(
        change.joined().len(),
        2,
        "a dead member's identifier is still pinned, so its orphans are still \
         reachable: {:?}",
        tree.members()
    );
    assert_eq!(change.exited(), [chain.root_pid()]);
    assert_eq!(tree.members(), change.joined());
}

#[test]
fn a_process_this_tree_did_not_start_is_not_a_member() {
    let mut chain = Chain::start();
    let mut stranger = Chain::start();
    let mut tree = ProcessTree::rooted_at(chain.root_pid())
        .expect("a process this test started can be opened")
        .with_rescan_interval(Duration::ZERO);

    chain.start_descendants();
    stranger.start_descendants();
    wait_for_the_chain(chain.root_pid());
    wait_for_the_chain(stranger.root_pid());
    tree.refresh()
        .expect("the process table can always be read");

    assert!(
        !tree.contains(stranger.root_pid()),
        "an unrelated process must not be scoped into a game's audio"
    );
    assert_eq!(
        tree.members().len(),
        CHAIN_LENGTH,
        "the tree holds its own chain and nothing else: {:?}",
        tree.members()
    );
}

#[test]
fn membership_is_not_re_examined_more_often_than_the_rescan_interval() {
    let mut chain = Chain::start();
    chain.start_descendants();
    wait_for_the_chain(chain.root_pid());

    // Built with an interval nothing will outlast, so its first scan — the one
    // in the constructor — is the only scan it will ever do.
    let tree = ProcessTree::rooted_at(chain.root_pid())
        .expect("a process this test started can be opened")
        .with_rescan_interval(Duration::from_secs(3_600));
    assert_eq!(
        tree.members().len(),
        CHAIN_LENGTH,
        "the chain never came up"
    );
    let mut tree = tree;

    chain.kill_root();
    let change = tree
        .refresh()
        .expect("the process table can always be read");

    assert!(
        change.is_empty() && tree.contains(chain.root_pid()),
        "a refresh inside the rescan interval must read nothing and report nothing"
    );

    // The same tree, told it may look again: the only thing that changed is the
    // interval, so the exit above is what it now finds.
    let mut tree = tree.with_rescan_interval(Duration::ZERO);
    let change = tree
        .refresh()
        .expect("the process table can always be read");

    assert_eq!(change.exited(), [chain.root_pid()]);
    assert!(!tree.contains(chain.root_pid()));
}

#[test]
fn the_tree_empties_when_everything_in_it_has_gone() {
    let mut chain = Chain::start();
    let mut tree = ProcessTree::rooted_at(chain.root_pid())
        .expect("a process this test started can be opened")
        .with_rescan_interval(Duration::ZERO);

    chain.start_descendants();
    wait_for_the_chain(chain.root_pid());
    tree.refresh()
        .expect("the process table can always be read");
    assert_eq!(tree.members().len(), CHAIN_LENGTH);

    // Closing the chain's input ends the deepest process, then the one that
    // started it, then the root: three exits, from the leaf upwards. They do
    // not all happen at once, so this is the one place a test waits.
    drop(chain);
    refresh_until(&mut tree, "the whole chain to exit", |tree| {
        tree.members().is_empty()
    });

    assert!(
        tree.members().is_empty(),
        "an empty tree is how a caller knows the game has nothing left to record"
    );
}

#[test]
fn a_tree_cannot_be_rooted_at_a_process_that_has_already_exited() {
    let mut finished = Command::new("cmd.exe")
        .args(["/c", "exit", "0"])
        .spawn()
        .expect("cmd.exe is on every Windows installation");
    let pid = finished.id();
    finished.wait().expect("this test started it");

    // Nothing holds a handle to it, so the identifier is free and names either
    // nothing or something this tree has no business scoping audio to. Either
    // way there is no game to follow.
    match ProcessTree::rooted_at(pid) {
        Err(WindowsError::ProcessUnavailable { process_id }) => assert_eq!(process_id, pid),
        // Windows may already have given the identifier to something else,
        // which is the whole hazard this crate is written around; that is not a
        // failure of this test, and there is nothing left to assert.
        Ok(_) => {}
        Err(error) => panic!("unexpected error: {error}"),
    }
}
