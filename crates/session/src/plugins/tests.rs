//! What a plugin costs a recording, measured against real plugin processes.
//!
//! Nothing here is a fake. Each test installs one of the programs in
//! `crates/plugins/examples` exactly as a user installs a plugin — a directory,
//! a `plugin.json`, and the executable the manifest names — and lets
//! [`SessionPlugins`] start it, read it over a real pipe and kill it. A fake
//! that pretended to hang would only prove that the pretence was ignored
//! (AGENTS.md section 54).
//!
//! The two example sources are compiled as examples of *this* package, so that
//! `cargo test -p clipped-session` builds them and there is still one copy of
//! each in the repository; `Cargo.toml` says why at more length.
//!
//! # What each test is here to catch
//!
//! | Test | The defect it fails on |
//! | --- | --- |
//! | `a_plugins_events_are_placed_on_the_recordings_own_timeline` | attaching against any clock reading other than the one taken beside the capture epoch |
//! | `a_plugin_starts_only_once_the_recording_has_a_timeline_to_place_it_on` | starting a plugin before there is anywhere to put its events |
//! | `nothing_a_recording_does_can_be_delayed_by_a_plugin_that_floods` | draining or polling from the thread that is recording |
//! | `a_plugin_that_hangs_does_not_delay_the_end_of_a_recording` | waiting for a plugin to finish |
//! | `a_plugin_that_crashes_is_reported_rather_than_swallowed` | logging a `PluginTrouble` and forgetting it |
//! | `a_plugin_that_does_not_support_the_game_being_recorded_is_not_started` | starting every enabled plugin on every launch |
//! | `every_plugin_a_user_installed_and_never_enabled_is_named` | ignoring an installed plugin silently |

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::ThreadId;

use clipped_events::EventKind;
use clipped_media_validation::TemporaryDirectory;
use clipped_plugins::{discover, PluginTrouble};

use super::*;

/// How long a test waits for something a child process has to do.
///
/// Generous on purpose, for the reason `crates/plugins`' own fixture gives:
/// starting a process on a machine running several test binaries at once takes
/// as long as it takes, and a tight bound here would fail for reasons that have
/// nothing to do with the code (AGENTS.md section 25).
const PATIENCE: Duration = Duration::from_secs(30);

/// The longest a single publication of the recording's position may take.
///
/// This is the number that makes "nothing in the capture path waits on a plugin"
/// a measurement rather than a claim about the code's shape. It is deliberately
/// far larger than a relaxed atomic store, because the assertion is not "this is
/// fast" — it is "this never waited for a plugin", and **every** way a plugin
/// can make something wait is longer than this by an order of magnitude: the
/// policy below waits [`PATIENCE`] for a plugin that has gone quiet, allows
/// 200 ms of grace before one is killed, and starting a replacement process
/// takes as long as the operating system takes.
const A_RECORDING_IS_NEVER_HELD_UP_LONGER_THAN: Duration = Duration::from_millis(100);

/// A policy tuned to fail fast where a test needs it to, and nowhere else.
///
/// Both timeouts are held open at [`PATIENCE`]. They were 400 ms — the numbers
/// `crates/plugins`' own supervisor tests used — and 400 ms is a budget for
/// `CreateProcess`, a Windows loader run and a first write on a pipe, which a
/// shared CI runner exceeded twice in a row. That crate now holds both open and
/// shortens one only in the test that is about it (#405); this is the same
/// change here (#415).
///
/// The failure it prevents is not a slow test but a **wrong** one. Every test
/// sharing this policy is about something a plugin does *after* it has
/// introduced itself, and a start-up that overran the old budget turned each of
/// them into a test about how busy the runner was:
/// `nothing_a_recording_does_can_be_delayed_by_a_plugin_that_floods` would see a
/// plugin disabled as `NeverStarted` and fail asserting it had dropped events;
/// `a_plugin_that_crashes_is_reported_rather_than_swallowed` would spend both
/// restart attempts on slow starts and disable the plugin for the wrong reason.
///
/// The one test that is about a timeout —
/// `a_plugin_that_hangs_does_not_delay_the_end_of_a_recording` — sets the one it
/// is about, and says why that value cannot give it the wrong answer.
///
/// The product half of #405 is what makes this safe: the silence timeout is
/// asked only of a plugin that has already introduced itself, so a slow start is
/// charged to the start-up budget alone rather than to whichever of the two
/// numbers is smaller.
fn impatient() -> SupervisionPolicy {
    SupervisionPolicy {
        silence_timeout: PATIENCE,
        hello_timeout: PATIENCE,
        dropped_event_budget: 4,
        protocol_fault_budget: 5,
        attempts: 2,
        first_delay: Duration::from_millis(10),
        maximum_delay: Duration::from_millis(20),
        settled_after: Duration::from_secs(60),
        stop_grace: Duration::from_millis(200),
    }
}

/// An example binary, built beside this test binary.
///
/// Cargo publishes no environment variable naming an example's path — only
/// binaries get `CARGO_BIN_EXE_*` — but it does put examples in
/// `<target>/<profile>/examples`, and a test executable is in
/// `<target>/<profile>/deps`.
fn example_binary(name: &str) -> PathBuf {
    let test_executable = std::env::current_exe().expect("a test knows its own path");
    let profile = test_executable
        .parent()
        .and_then(Path::parent)
        .expect("a test executable lives in <target>/<profile>/deps");
    let example = profile
        .join("examples")
        .join(name)
        .with_extension(std::env::consts::EXE_EXTENSION);

    assert!(
        example.is_file(),
        "{} has not been built. `cargo test -p clipped-session` builds it; if this test was run \
         some other way, build the examples first with `cargo build -p clipped-session \
         --examples`.",
        example.display()
    );
    example
}

/// Installs `example` as a plugin called `id`, under the file name
/// `installed_as` — which is what tells `misbehaving_plugin` how to misbehave —
/// supporting `supports`.
///
/// Returns it *installed*, which cannot be started, so that each test decides
/// whether it is also enabled.
fn install(
    root: &TemporaryDirectory,
    id: &str,
    example: &str,
    installed_as: &str,
    supports: &str,
) -> InstalledPlugin {
    let executable = format!("{installed_as}.{}", std::env::consts::EXE_EXTENSION);
    let executable = executable.trim_end_matches('.').to_owned();
    let manifest = format!(
        r#"{{
            "contract": 1,
            "id": "{id}",
            "name": "Test plugin {id}",
            "version": "0.0.0",
            "description": "Installed by a test of the session wiring.",
            "executable": "{executable}",
            "supports": {{ "executables": ["{supports}"] }}
        }}"#
    );

    let directory = root.path().join(id);
    fs::create_dir_all(&directory).expect("a plugin directory can be created");
    fs::write(directory.join("plugin.json"), manifest).expect("a manifest can be written");
    fs::copy(example_binary(example), directory.join(&executable))
        .expect("an example can be installed as a plugin");

    discover(root.path())
        .installed
        .into_iter()
        .find(|plugin| plugin.id().as_str() == id)
        .expect("the plugin that was just installed is discovered")
}

/// The same plugin, enabled with consent to exactly what it declares — which is
/// the only way to get something a session will start.
fn enabled(installed: InstalledPlugin) -> EnabledPlugin {
    let consent = installed.consent_token();
    installed
        .enable(&consent)
        .expect("consent to what it declares now")
}

/// The session a test attaches plugins to.
fn session() -> SessionDetails {
    SessionDetails {
        session: "20260812-140000-test".to_owned(),
        process: ObservedProcess::new("cs2.exe", std::process::id()),
    }
}

/// Runs `step` until it answers `true`, or fails the test after [`PATIENCE`].
fn until(what: &str, mut step: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if step() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("waited {PATIENCE:?} for {what}, and it did not happen");
}

/// Which thread the supervisor has been polled on, and how many times.
fn polling(plugins: &SessionPlugins) -> (Option<ThreadId>, u64) {
    let collected = plugins.shared.collected();
    (collected.polled_on, collected.polls)
}

fn disabled_with(reports: &[SupervisionEvent]) -> Option<PluginTrouble> {
    reports.iter().find_map(|report| match report {
        SupervisionEvent::Disabled { trouble, .. } => Some(trouble.clone()),
        _ => None,
    })
}

#[test]
fn the_poll_interval_is_what_the_supervisor_asks_for() {
    // `PluginSupervisor::poll` states its own requirement in prose: about once
    // a second. A loop slower than that leaves a hung plugin holding a game's
    // port for longer than the policy says it may.
    assert!(POLL_INTERVAL <= Duration::from_secs(1));
    assert!(
        EPOCH_POLL_INTERVAL < POLL_INTERVAL,
        "a plugin should start with the recording rather than up to a second after it"
    );
    assert!(
        STOPPING_POLL_INTERVAL < POLL_INTERVAL,
        "a plugin that ignores `detach` is killed by the poll that notices, so the end of a \
         recording must not wait out a whole ordinary interval first"
    );
}

#[test]
fn a_plugins_events_are_placed_on_the_recordings_own_timeline() {
    // The end-to-end path, and the one number in it that cannot be checked any
    // other way: a plugin says how long ago something happened, and what comes
    // out is a position in *this recording's* file. The recording below has been
    // running for two seconds when the plugin reports a kill it saw 480 ms
    // earlier, so the kill belongs at about 1.5 s into the file — and a session
    // that built its timeline from any reading other than the one the recording
    // published would put it within milliseconds of zero.
    let root = TemporaryDirectory::new("session-plugins-timeline");
    let plugin = enabled(install(
        &root,
        "acme-cs2",
        "example_plugin",
        "example-plugin",
        "cs2.exe",
    ));

    let progress = RecordingProgress::new();
    let plugins = SessionPlugins::start(vec![plugin], session(), &progress, None, impatient());

    // The recording's first frame, two seconds ago.
    let epoch = Instant::now() - Duration::from_secs(2);
    progress.timeline_began(epoch);

    let mut events = Vec::new();
    let mut reports = Vec::new();
    until("the example plugin to report a kill", || {
        events.extend(plugins.take_events());
        reports.extend(plugins.take_reports());
        !events.is_empty()
    });

    assert!(
        reports
            .iter()
            .any(|report| matches!(report, SupervisionEvent::Ready { .. })),
        "it introduced itself before it said anything else: {reports:?}"
    );

    let kill = &events[0];
    assert_eq!(kill.kind(), &EventKind::Kill);
    assert_eq!(
        kill.source().as_str(),
        "acme-cs2",
        "the source is the manifest's identifier, which the plugin never sent"
    );
    assert_eq!(kill.timing().latency(), Duration::from_millis(480));

    let at = kill.timing().at().as_media_nanos();
    let now = SessionTimeline::starting_at(epoch).now().as_media_nanos();
    assert!(
        at > 1_000_000_000,
        "the kill belongs a second and a half into this recording, and was placed at {at}ns — \
         which is where it would sit if the timeline had been built from a clock reading taken \
         when the plugins started rather than beside the capture epoch"
    );
    assert!(
        at <= now - 400_000_000,
        "the kill happened 480 ms before it was reported, and was placed at {at}ns with the \
         recording at {now}ns"
    );

    // And it stops when the recording does.
    let outcome = plugins.finish();
    assert_eq!(outcome.health.len(), 1);
    assert_eq!(
        outcome.health[0].state,
        PluginState::Stopped,
        "a plugin must not outlive the recording that started it"
    );
    assert_eq!(outcome.events_lost, 0);
    assert!(!outcome.lost_anything());
}

#[test]
fn a_plugin_starts_only_once_the_recording_has_a_timeline_to_place_it_on() {
    // A recording that never produced a frame has no epoch, so there is nowhere
    // to put an event and nothing is started. This is also what stops a plugin
    // being attached against a provisional clock reading and then corrected,
    // which would be the second conversion `crates/plugins` deliberately does
    // not have.
    let root = TemporaryDirectory::new("session-plugins-no-frame");
    let plugin = enabled(install(
        &root,
        "acme-cs2",
        "example_plugin",
        "example-plugin",
        "cs2.exe",
    ));

    let progress = RecordingProgress::new();
    let plugins = SessionPlugins::start(vec![plugin], session(), &progress, None, impatient());

    // Long enough that a plugin which was going to be started would have said
    // hello. It is not measured against the policy's start-up budget, which is
    // now [`PATIENCE`] and would make this sleep thirty seconds: what is being
    // waited for is a plugin *process*, and the reason nothing is reported is
    // that nothing was started at all. So this only has to outlast starting one
    // and hearing from it, which is what a slow runner makes longer.
    thread::sleep(Duration::from_millis(750));
    assert!(
        plugins.take_events().is_empty(),
        "no frame has reached the file, so there is no timeline for an event to sit on"
    );
    assert!(
        plugins.take_reports().is_empty(),
        "nothing was started, so the supervisor has nothing to say"
    );

    let outcome = plugins.finish();
    assert!(
        outcome.health.is_empty(),
        "a recording that captured nothing starts no plugin: {:?}",
        outcome.health
    );
}

#[test]
fn nothing_a_recording_does_can_be_delayed_by_a_plugin_that_floods() {
    // The measurement the whole design exists for. A real process fills a pipe
    // as fast as the operating system will take it, while a thread standing in
    // for the capture thread does the one thing a recording does that a plugin
    // can see — publish where it has reached — six hundred times. If any part of
    // draining or supervising a plugin had been put on that path, every one of
    // those publications would be behind a queue a flooding process is refilling.
    let root = TemporaryDirectory::new("session-plugins-flood");
    let plugin = enabled(install(
        &root,
        "flooder",
        "misbehaving_plugin",
        "flood-plugin",
        "cs2.exe",
    ));

    let progress = RecordingProgress::new();
    let plugins = SessionPlugins::start(vec![plugin], session(), &progress, None, impatient());

    let recording = progress.clone();
    let capture = thread::Builder::new()
        .name("test-capture-thread".to_owned())
        .spawn(move || {
            // The first kept frame, which is where a recording publishes its
            // epoch (`crate::recording`).
            recording.timeline_began(Instant::now());

            let mut worst = Duration::ZERO;
            for frame in 0..600_u64 {
                let before = Instant::now();
                recording.reached(Duration::from_millis(frame * 16));
                worst = worst.max(before.elapsed());
                thread::sleep(Duration::from_millis(1));
            }
            (thread::current().id(), worst)
        })
        .expect("a stand-in for the capture thread can be started");

    let mut events = Vec::new();
    let mut reports = Vec::new();
    until("a flooding plugin to be stopped for good", || {
        let before = Instant::now();
        events.extend(plugins.take_events());
        reports.extend(plugins.take_reports());
        let taking = before.elapsed();
        assert!(
            taking < A_RECORDING_IS_NEVER_HELD_UP_LONGER_THAN,
            "taking a flooding plugin's events cost the caller {taking:?}"
        );
        disabled_with(&reports).is_some()
    });

    let (capture_thread, worst) = capture
        .join()
        .expect("the stand-in capture thread finishes");
    assert!(
        worst < A_RECORDING_IS_NEVER_HELD_UP_LONGER_THAN,
        "a recording spent {worst:?} publishing its position while a plugin was flooding, which \
         is long enough to have waited for one"
    );

    let (polled_on, polls) = polling(&plugins);
    assert!(polls > 0, "the supervisor was never polled at all");
    assert_ne!(
        polled_on,
        Some(capture_thread),
        "the supervisor was polled on the thread that was recording (AGENTS.md section 20)"
    );
    assert_ne!(
        polled_on,
        Some(thread::current().id()),
        "the supervisor was polled on the thread that owns the recording rather than on one of \
         its own, so a plugin that would not answer would hold up the loop driving the session"
    );

    assert!(
        events.iter().all(|event| event.kind() == &EventKind::Kill),
        "what did get through is still well-formed"
    );

    // Which *kind* of trouble a flood is charged as is `crates/plugins`'
    // decision and is asserted there, against a supervisor polled in a tight
    // loop. This one is polled once a second, as a session polls it, and at that
    // cadence a flood that has finished flooding is reported as the exit it also
    // is. What this test holds is the part a session owns: the loss is counted,
    // it is attributed to the plugin that caused it, and the recording is told
    // rather than left looking complete.
    let outcome = plugins.finish();
    assert!(
        outcome.inbox.dropped > 0,
        "a plugin outran the recording and nothing recorded the loss: {outcome:?}"
    );
    assert!(
        outcome.health[0].dropped > 0 && outcome.health[0].dropped <= outcome.inbox.dropped,
        "the loss is attributed to the plugin that caused it: it was charged {} of the {} the \
         queue lost",
        outcome.health[0].dropped,
        outcome.inbox.dropped
    );
    assert!(
        outcome.lost_anything(),
        "a timeline missing marks has to say so: {outcome:?}"
    );
    assert!(
        is_stopped(&outcome.health[0].state),
        "the plugin is stopped for good, and is {:?}",
        outcome.health[0].state
    );
}

#[test]
fn a_plugin_that_hangs_does_not_delay_the_end_of_a_recording() {
    // The failure a plugin is a separate process for. It says hello, stops
    // answering, and ignores `detach` — so ending the recording means killing
    // it, which is not possible for a thread that has stopped answering.
    //
    // The policy below is `impatient()` with the one number this test is about
    // moved, and the reason is the whole point of the test. A silence timeout
    // this test could reach would have the ordinary supervision loop kill the
    // plugin for going quiet *before* the recording ever ended, so `finish`
    // would be handed a plugin that was already dead and the shutdown path —
    // detach, poll, kill what ignored it — would never run. Holding the silence
    // open past the end of the test is what makes the plugin still be hanging at
    // the moment the recording ends, which is the case this test is named for.
    //
    // Longer than `PATIENCE` and not merely longer than the old 400 ms: every
    // `until` in this file gives up after `PATIENCE`, so a silence timeout of
    // exactly that would be a race between this test finishing and the
    // supervisor killing its subject.
    let root = TemporaryDirectory::new("session-plugins-hang");
    let plugin = enabled(install(
        &root,
        "hanger",
        "misbehaving_plugin",
        "hang-plugin",
        "cs2.exe",
    ));

    let policy = SupervisionPolicy {
        silence_timeout: Duration::from_secs(120),
        ..impatient()
    };
    let progress = RecordingProgress::new();
    let plugins = SessionPlugins::start(vec![plugin], session(), &progress, None, policy);
    progress.timeline_began(Instant::now());

    let mut reports = Vec::new();
    until("the hanging plugin to introduce itself", || {
        reports.extend(plugins.take_reports());
        reports
            .iter()
            .any(|report| matches!(report, SupervisionEvent::Ready { .. }))
    });
    assert!(
        !reports
            .iter()
            .any(|report| matches!(report, SupervisionEvent::Disabled { .. })),
        "this plugin has to be alive and hanging when the recording ends, or the shutdown path \
         this test is named for is never reached: {reports:?}"
    );

    let before = Instant::now();
    let outcome = plugins.finish();
    let ending = before.elapsed();

    // The bound the policy states: the stop grace, plus the poll that notices
    // it has passed, plus room for a machine running several test binaries at
    // once. A recording that *waited* for this plugin would never finish at all.
    let bound = policy.stop_grace + POLL_INTERVAL + Duration::from_secs(2);
    assert!(
        ending < bound,
        "finishing a recording whose plugin had hung took {ending:?}, which is longer than the \
         {bound:?} the supervision policy allows"
    );
    assert_eq!(
        outcome.health.len(),
        1,
        "the plugin that was attached is accounted for"
    );
    assert!(
        is_stopped(&outcome.health[0].state),
        "a plugin that will not stop is killed rather than waited for, and this one is {:?}",
        outcome.health[0].state
    );
}

#[test]
fn a_plugin_that_crashes_is_reported_rather_than_swallowed() {
    // AGENTS.md section 45: an integration that silently never works is worse
    // than one that says why. The trouble has to reach whoever is driving the
    // session, not only the log.
    let root = TemporaryDirectory::new("session-plugins-crash");
    let plugin = enabled(install(
        &root,
        "crasher",
        "misbehaving_plugin",
        "crash-plugin",
        "cs2.exe",
    ));

    let progress = RecordingProgress::new();
    let plugins = SessionPlugins::start(vec![plugin], session(), &progress, None, impatient());
    progress.timeline_began(Instant::now());

    let mut reports = Vec::new();
    until(
        "a plugin that panics to be replaced and then given up on",
        || {
            reports.extend(plugins.take_reports());
            disabled_with(&reports).is_some()
        },
    );

    let attempts: Vec<u32> = reports
        .iter()
        .filter_map(|report| match report {
            SupervisionEvent::Restarting { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .collect();
    assert_eq!(
        attempts,
        vec![1, 2],
        "a crashing plugin is given the attempts the policy allows, and no more: {reports:?}"
    );
    assert!(
        matches!(
            disabled_with(&reports),
            Some(PluginTrouble::Exited { code: Some(_) })
        ),
        "the panic reaches the session as an exit, in words a user can be shown: {reports:?}"
    );

    let outcome = plugins.finish();
    assert!(
        is_stopped(&outcome.health[0].state),
        "it is not coming back, and is {:?}",
        outcome.health[0].state
    );
}

#[test]
fn a_plugin_that_does_not_support_the_game_being_recorded_is_not_started() {
    // `supports` is answered from the manifest before anything runs, so that a
    // launch of something else on the machine does not start every plugin
    // installed. A session filters what it was given rather than making its
    // caller do it.
    let root = TemporaryDirectory::new("session-plugins-unsupported");
    let dota = enabled(install(
        &root,
        "acme-dota",
        "example_plugin",
        "example-plugin",
        "dota2.exe",
    ));

    let progress = RecordingProgress::new();
    // `session()` is a recording of `cs2.exe`.
    let plugins = SessionPlugins::start(vec![dota], session(), &progress, None, impatient());
    progress.timeline_began(Instant::now());

    thread::sleep(Duration::from_millis(750));
    let outcome = plugins.finish();
    assert!(
        outcome.health.is_empty(),
        "a plugin for another game was started for this one: {:?}",
        outcome.health
    );
    assert!(outcome.events.is_empty());
}

#[test]
fn every_plugin_a_user_installed_and_never_enabled_is_named() {
    // The decision this module records, and the reason it is a function rather
    // than a comment: a user who installed a plugin and cannot see it working
    // has to be told which one is not enabled, rather than left to guess
    // (AGENTS.md section 27, issue #282).
    let root = TemporaryDirectory::new("session-plugins-unenabled");
    let cs2 = install(
        &root,
        "acme-cs2",
        "example_plugin",
        "example-plugin",
        "cs2.exe",
    );
    let dota = install(
        &root,
        "acme-dota",
        "example_plugin",
        "example-plugin",
        "dota2.exe",
    );

    let installed = vec![cs2, dota];
    let named: Vec<&str> = installed_but_not_enabled(&installed, &session().process)
        .iter()
        .map(|plugin| plugin.id().as_str())
        .collect();
    assert_eq!(
        named,
        vec!["acme-cs2"],
        "only the plugins that claim the game being recorded are worth naming"
    );
}

#[test]
fn a_sessions_second_recording_stamps_its_events_from_the_sessions_zero() {
    // Issue #488. A session that writes two files -- a window destroyed and
    // recreated, or a game relaunched inside its restart grace -- must stamp
    // both files' events against one origin, because
    // `clipped_library::events` places a moment by sorting a session's
    // recordings on a single axis and asking which contains it. Two files each
    // measured from their own zero cannot be sorted or searched, and every
    // event of the second would land in the first.
    //
    // The failure this guards is silent: without the session's epoch, the
    // number below is a small positive one that looks entirely reasonable.
    let root = TemporaryDirectory::new("second-recording-shares-the-zero");
    let plugin = enabled(install(
        &root,
        "acme-cs2",
        "example_plugin",
        "example-plugin",
        "cs2.exe",
    ));

    // The session started ten minutes ago; this, its second recording, began
    // two seconds ago.
    let session_epoch = Instant::now() - Duration::from_secs(600);
    let recording_epoch = Instant::now() - Duration::from_secs(2);

    let progress = RecordingProgress::new();
    let plugins = SessionPlugins::start(
        vec![plugin],
        session(),
        &progress,
        Some(session_epoch),
        impatient(),
    );
    progress.timeline_began(recording_epoch);

    let mut events = Vec::new();
    until("the example plugin to report a kill", || {
        events.extend(plugins.take_events());
        !events.is_empty()
    });

    let at = events[0].timing().at().as_media_nanos();
    assert!(
        at > 500_000_000_000,
        "the kill happened ten minutes into the session and was placed at {at}ns — which is \
         where it would sit if this recording had been given a timeline of its own, putting \
         every event of a session's second file inside its first"
    );

    let _ = plugins.finish();
}
