//! The `replay` subcommand, and the save every other caller reaches it through.
//!
//! `record` is a recording. `replay` is a recording **plus a rolling buffer of
//! the last few minutes**, and a key that turns that buffer into a clip of what
//! just happened — which is the thing this category of application exists for
//! (SPEC.md sections 15 and 16, issue #38).
//!
//! ```text
//!  clipped-recorder replay --window "Counter-Strike 2" --duration 60
//!
//!  capture ─▶ encode ─┬─▶ recording.mkv          the ordinary recording
//!                     └─▶ ReplayBuffer           the last 60 s, in memory
//!                              │
//!            Ctrl+F10 ─────────┴──▶ save_last(30 s) ─▶ …-replay-1.mkv
//!                                                          │
//!                                            the session's sidecar ─▶ library
//! ```
//!
//! # Three things it is deliberately not
//!
//! **It is not a second recording path.** The recording is
//! `clipped_session::record_with_replay`, which is `record` with a buffer to
//! fill: one encoder, two consumers of its packets, and the same settings
//! resolution, the same window rules and the same file naming
//! (`crate::record`). A `replay` invocation therefore leaves the same recording
//! `record` would have left, plus whatever was saved out of it.
//!
//! **It is not a second description of a session.** A save is entered in the
//! session's own sidecar — the file `clipped_session::automatic` writes and
//! `clipped-library` indexes — so a clip reaches the library exactly as the
//! recording beside it does, through
//! [`ManualSession::clip_saved`](clipped_session::automatic::ManualSession::clip_saved)
//! (issue #402, `docs/sessions.md`).
//!
//! **It is not a buffer-only capture.** SPEC.md section 4's Manual/Replay mode
//! keeps the buffer and writes no continuous file; this writes both, because
//! the buffer is filled from the packets a recording produces and
//! `clipped-session` has no recording without a file. That is
//! [issue #423](https://github.com/wildware-uk/clipped/issues/423), and
//! `--help` says so rather than leaving somebody to discover the disk use.
//!
//! # Threads
//!
//! ```text
//!  main thread                    hotkey thread        the SaveReplay handler's
//!  ───────────                    ─────────────        ────────────────────────
//!  record_with_replay  ◀ blocks   GetMessageW          lease, write the clip,
//!    capture, encode, push        WM_HOTKEY ─────────▶ rewrite the sidecar
//!  Ctrl+C ─▶ stop, finalise
//! ```
//!
//! **A save never runs on the capture thread**, which is AGENTS.md section 20
//! and is what `clipped-hotkeys` is shaped for: a press is a map lookup and a
//! non-blocking send on the hotkey thread, and the handler runs on a thread of
//! that action's own for as long as writing the clip takes. What the two share
//! is the buffer's lock, held for one `memcpy` per packet by the recording and
//! for the length of a lease — 0.77 ms for a five-minute window — by a save
//! (`docs/replay-buffer.md`).

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use clipped_hotkeys::{Handlers, HotkeyAction, HotkeyError, HotkeyService, Registration};
use clipped_logging::RedactedPath;
use clipped_replay::SavedClip;
use clipped_session::automatic::ManualSession;
use clipped_session::config::SettingError;
use clipped_session::{record_with_replay, ReplayRecording, ReplaySaveError};

use crate::cli::ReplayArgs;
use crate::config::{ConfigError, ReplayConfig};
use crate::record::{enable_dpi_awareness, resolve_window, settings_for, RecordError};
use crate::shutdown::{install_ctrl_c_handler, run_until_shutdown, ShutdownSignal};

/// Why `replay` did not run, or did not finish.
#[derive(Debug)]
pub enum ReplayCommandError {
    /// The arguments did not describe a recording that could be made, or a
    /// buffer that could be kept.
    Configuration(ConfigError),
    /// The replay buffer's own configuration refused the window.
    ///
    /// Separate from [`Self::Configuration`] because the bound belongs to
    /// `clipped-replay` rather than to this command line: the message is that
    /// crate's, so the two cannot drift.
    Buffer(clipped_replay::ConfigError),
    /// Everything `record` can fail with, resolving the window and recording
    /// it.
    Recording(RecordError),
    /// The hotkey service could not be started.
    ///
    /// Not a conflict: a combination another application owns is a *successful*
    /// start with a conflict in the registration, which is reported and does not
    /// stop the recording (`clipped_hotkeys::HotkeyService::start`).
    Hotkeys(HotkeyError),
    /// The `hotkeys` section of the settings file does not describe a usable set
    /// of bindings — two actions on one combination is the only way to reach it.
    ///
    /// Refused **before** the recording starts, and deliberately unlike `serve`,
    /// which registers nothing and carries on serving. A `serve` with no hotkey
    /// still answers the protocol and still records; a `replay` with no hotkey
    /// is a buffer nothing can save from, which is the whole of what the
    /// subcommand is for. Finding that out after a capture session has opened
    /// would be finding it out late (AGENTS.md section 45).
    Hotkeybindings(SettingError),
}

impl fmt::Display for ReplayCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => write!(formatter, "{error}"),
            Self::Buffer(error) => write!(formatter, "{error}"),
            Self::Recording(error) => write!(formatter, "{error}"),
            Self::Hotkeys(error) => write!(formatter, "{error}"),
            Self::Hotkeybindings(error) => write!(
                formatter,
                "No hotkey can be registered: {error} Fix the `hotkeys` section of the \
                 settings file, because a replay buffer nothing can save from is not \
                 worth recording."
            ),
        }
    }
}

impl Error for ReplayCommandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(error) => Some(error),
            Self::Buffer(error) => Some(error),
            Self::Recording(error) => Some(error),
            Self::Hotkeys(error) => Some(error),
            Self::Hotkeybindings(error) => Some(error),
        }
    }
}

impl From<ConfigError> for ReplayCommandError {
    fn from(error: ConfigError) -> Self {
        Self::Configuration(error)
    }
}

impl From<clipped_replay::ConfigError> for ReplayCommandError {
    fn from(error: clipped_replay::ConfigError) -> Self {
        Self::Buffer(error)
    }
}

impl From<RecordError> for ReplayCommandError {
    fn from(error: RecordError) -> Self {
        Self::Recording(error)
    }
}

impl From<HotkeyError> for ReplayCommandError {
    fn from(error: HotkeyError) -> Self {
        Self::Hotkeys(error)
    }
}

/// The bindings `replay` registers, from the settings file's `hotkeys` section.
///
/// **The only place this subcommand decides what Save replay is bound to.** It
/// registered [`Bindings::defaults`](clipped_hotkeys::Bindings::defaults) until
/// [issue #444](https://github.com/wildware-uk/clipped/issues/444), which threw
/// every override in `settings.json` away in silence — and the symptom was
/// worse than a hotkey that did nothing, because the conflict report tells a
/// user whose combination is taken to "choose a different combination", and
/// choosing one changed nothing.
///
/// Extracted so that the resolution can be asserted without a recording. What
/// that assertion cannot cover is `run` calling
/// `Bindings::defaults()` again instead of this, for the reason
/// `crate::hotkeys`'s own test states about its half: what is unguarded is this
/// process handing the answer on. Keeping this the one source in the module is
/// what makes that a visible edit rather than a silent one.
fn bindings_for(
    configuration: &clipped_session::config::Configuration,
) -> Result<clipped_hotkeys::Bindings, ReplayCommandError> {
    configuration
        .resolve_hotkeys()
        .map(|resolved| resolved.bindings().clone())
        .map_err(ReplayCommandError::Hotkeybindings)
}

/// Records with a replay buffer until Ctrl+C, saving a clip on every press of
/// the replay hotkey.
///
/// # Errors
///
/// [`ReplayCommandError::Configuration`] if the arguments cannot describe a
/// recording, [`ReplayCommandError::Buffer`] if the duration is outside what a
/// replay buffer supports, [`ReplayCommandError::Hotkeys`] if the hotkey thread
/// could not be started, and [`ReplayCommandError::Recording`] for everything
/// `record` can fail with. A failure after recording started still leaves a
/// finalised, playable file — and every clip saved before it.
#[cfg(windows)]
pub fn run(args: &ReplayArgs) -> Result<(), ReplayCommandError> {
    // The same file `watch` and `serve` read, through the same function, so
    // that "what does this record at" has one answer whichever subcommand is
    // asking (AGENTS.md sections 30 and 55). Read once, here: nothing re-reads
    // a settings file underneath a running encoder (issue #61).
    let configuration = crate::watch::load_configuration(
        clipped_session::config::ConfigurationStore::default_path().as_deref(),
    );
    // The global settings, which are what a session nothing identified resolves
    // to (`clipped_session::automatic::ManualSession`): the same answer the
    // session below will record as this recording's, arrived at through the
    // same fold rather than a second one.
    let settings = configuration.resolve_global();

    let config = ReplayConfig::resolve(args, *settings.replay_window().value())?;
    // Before the capture session, the encoder and the file, so that an
    // unsupported duration is a usage error and not a discovery.
    let replay = Arc::new(ReplayRecording::new(config.window)?);

    // What Clipped knows about games, read once and asked once, so that a
    // sitting `replay` made is filed under the game a sitting `serve` or `watch`
    // made of it would be (issue #403). A games file that cannot be read costs
    // attribution and nothing else: the person asked for a recording, and their
    // footage is what cannot be made again (`crate::serve`, AGENTS.md section
    // 17).
    let catalogue = crate::serve::catalogue_for_recordings();
    // The launchers as well, so a replay saved from the command line is filed
    // under the same game a recording of the same process would be (issue #522).
    let launchers = clipped_game_detection::launcher::Launchers::discover();

    enable_dpi_awareness();
    let window = resolve_window(&config.recording.target)?;
    let asked_for = settings_for(&config.recording, &window);

    // The session opens before the encoder, so that a recorder killed during
    // this recording still leaves something saying what the files beside it are
    // (AGENTS.md section 17) — and so that a clip saved thirty seconds in has a
    // session record to be entered in.
    let session = Arc::new(Mutex::new(ManualSession::start(
        config
            .recording
            .output
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
        config.recording.output.clone(),
        &configuration,
        &catalogue,
        &launchers,
        // The image path as well as the name, because most catalogue entries
        // are qualified by one: Counter-Strike 2 is `cs2.exe` *in the directory
        // Steam installs it into* (`clipped_game_detection::catalogue`).
        clipped_session::automatic::RecordedProcess::new(
            window.process_id(),
            window.process_name().unwrap_or_default(),
        )
        .with_image_path(
            clipped_windows::process_image_path(window.process_id())
                .map(|path| path.to_string_lossy().into_owned())
                .as_deref(),
        ),
        SystemTime::now(),
    )));
    let settings = settings.apply_configured_to(asked_for);

    let signal = ShutdownSignal::new();
    // The handler first, then the flag, for the reason `crate::record`
    // documents: a recorder started in a process group of its own inherits
    // Ctrl+C disabled, and turning it on before there is a handler would open a
    // window in which it terminates the process with a recording open.
    install_ctrl_c_handler(&signal).map_err(RecordError::from)?;
    crate::shutdown::allow_ctrl_c();

    // The settings file's `hotkeys` section, resolved through the same call
    // `serve` uses, because "what is Save replay bound to" may not have two
    // answers depending on which subcommand asked (AGENTS.md section 55).
    //
    // This registered `Bindings::defaults()` until issue #444, which threw every
    // override away in silence: on a machine where another application owns
    // Ctrl+F10, the conflict report below told the user to choose a different
    // combination and choosing one changed nothing. The comment on
    // `crate::hotkeys::start`'s own test describes that failure exactly — it
    // guards `serve` against it, and there was no equivalent here.
    let bindings = bindings_for(&configuration)?;
    let hotkey = bindings.binding(HotkeyAction::SaveReplay).map_or_else(
        || "the replay hotkey".to_owned(),
        |hotkey| hotkey.to_string(),
    );
    let (hotkeys, _presses) =
        HotkeyService::start(&bindings, handlers_for(&replay, &session, config.save))?;

    // Every conflict, before the recording starts rather than when a press does
    // nothing. A combination another application owns costs the user that
    // hotkey and not the recording (AGENTS.md section 45, issue #39).
    for line in conflict_report(hotkeys.registration()) {
        eprintln!("{line}");
    }

    eprintln!(
        "Keeping the last {}. Press {hotkey} to save {}; Ctrl+C to stop.",
        described(config.window),
        described(config.save),
    );

    let outcome = run_until_shutdown(
        &signal,
        |signal| record_with_replay(&settings, signal, &replay),
        |reason| {
            tracing::info!(
                reason = %reason,
                "the recording was finalised and the finalisation hook ran"
            );
        },
    );

    // Before the session is closed: a press that arrived as the recording ended
    // is still a clip somebody asked for, and `stop` waits for a save that is
    // already running rather than cutting it off (AGENTS.md section 17).
    hotkeys.stop();

    let session = Arc::try_unwrap(session)
        .unwrap_or_else(|_| unreachable!("the hotkey handler holding the session has stopped"))
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    match outcome {
        Ok(report) => {
            let clips = session.session().clips().len();
            // The session's record is written for the last time here, with the
            // clips already in it: they were entered as each was saved, so a
            // recorder killed mid-session still leaves a record of every clip
            // that reached the disk (AGENTS.md section 17).
            let ended = session.finish(
                &clipped_session::automatic::RecordingOutcome::Recorded(Box::new(report.clone())),
                SystemTime::now(),
            );
            tracing::info!(
                session = ended.id().as_str(),
                clips,
                "the replay session ended"
            );
            eprintln!("{report}");
            for track in report.audio_tracks() {
                eprintln!("  {track}");
            }
            eprintln!(
                "{clips} replay {} saved.",
                if clips == 1 { "clip" } else { "clips" }
            );
            Ok(())
        }
        Err(error) => {
            let ended = session.finish(
                &clipped_session::automatic::RecordingOutcome::Failed {
                    detail: error.to_string(),
                },
                SystemTime::now(),
            );
            tracing::info!(
                session = ended.id().as_str(),
                "the replay session ended because its recording failed"
            );
            Err(ReplayCommandError::Recording(RecordError::Session(error)))
        }
    }
}

/// Recording is a Windows feature; this build has no capture backend.
///
/// # Errors
///
/// Always [`ReplayCommandError::Recording`] carrying
/// [`SessionError::UnsupportedPlatform`](clipped_session::SessionError::UnsupportedPlatform).
#[cfg(not(windows))]
pub fn run(args: &ReplayArgs) -> Result<(), ReplayCommandError> {
    let _ = args;
    Err(ReplayCommandError::Recording(RecordError::Session(
        clipped_session::SessionError::UnsupportedPlatform,
    )))
}

/// What a `replay` invocation gives the hotkey service: a save, on the replay
/// key, and nothing else.
///
/// The absence is deliberate. `replay` records one window until Ctrl+C, so it
/// has nothing to toggle and nowhere to put a screenshot; those actions belong
/// to `serve`, which owns a protocol and the recordings behind it (issue #232).
/// An action with no handler here is reported as
/// [`Unhandled`](clipped_hotkeys::Unhandled) when its key is pressed rather than
/// silently doing nothing (AGENTS.md section 54).
///
/// `keep` is how much of the buffer one press saves — the `--save` window, which
/// may be shorter than the window being kept.
///
/// Separate from [`run`] because [`run`] needs a window, an encoder and a
/// capture session before it reaches this, and the question this answers —
/// *which key does what, and what does it do* — needs none of them
/// (`apps/recorder/tests/replay_clip.rs`).
#[must_use]
pub fn handlers_for(
    replay: &Arc<ReplayRecording>,
    session: &Arc<Mutex<ManualSession>>,
    keep: Duration,
) -> Handlers {
    let replay = Arc::clone(replay);
    let session = Arc::clone(session);
    Handlers::new().on(HotkeyAction::SaveReplay, move |_press| {
        report_save(&save(&replay, &session, keep, None, SystemTime::now()));
    })
}

/// The lines a `replay` invocation prints for the combinations Windows would not
/// give it.
///
/// One per refused combination and none at all in the ordinary case, each the
/// conflict's own sentence: it names the combination, names the action that
/// wanted it, says who is likely to have it and says what to do next, and only
/// the process that asked Windows knows any of that (AGENTS.md section 45).
/// Repeating any of it here would be a second copy to go stale.
///
/// Separate from [`run`] because a conflict depends on what else is installed on
/// the machine, so the case that matters cannot be arranged by a test that
/// registers anything real — only by building the report
/// ([`Registration::of`](clipped_hotkeys::Registration::of)).
fn conflict_report(registration: &Registration) -> Vec<String> {
    registration.conflicts().map(ToString::to_string).collect()
}

/// Saves the last `keep` of `replay` and enters it in `session`.
///
/// **The one place a replay is saved**, reached from the hotkey handler above
/// and from `save_replay` over the protocol (`crate::serve`). Two callers and
/// one routine, because everything after the buffer is the part that is easy to
/// get subtly different: what the clip is called, where it goes, and whether
/// the session's record — and therefore the library — ever hears about it
/// (AGENTS.md section 55).
///
/// The order is deliberate and is the whole of the crash safety: the clip is
/// written **first** and the session's record is rewritten **after** it exists,
/// so a recorder killed between the two leaves a clip nothing indexed rather
/// than an index entry for a file that was never written (AGENTS.md
/// section 54). The first is recoverable — the next `recover` or a re-index
/// finds a file — and the second is a library row the user cannot play.
///
/// `now` is the wall clock, passed in so that this is testable without one
/// (AGENTS.md section 25).
///
/// # Errors
///
/// [`ReplaySaveError`], which says which of the three things went wrong: the
/// recording is not buffering, the buffer holds nothing yet, or the file could
/// not be written.
pub fn save(
    replay: &ReplayRecording,
    session: &Mutex<ManualSession>,
    keep: Duration,
    destination: Option<PathBuf>,
    now: SystemTime,
) -> Result<SavedReplay, ReplaySaveError> {
    // The name comes out of the session, so a clip is called after the sitting
    // it belongs to and numbered within it. The lock is held for that and
    // released before the write: writing a clip takes as long as the disk takes,
    // and a second press must not queue behind the first holding a mutex.
    let path = match destination {
        Some(chosen) => chosen,
        None => session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_clip_path(),
    };

    let clip = replay.save_last(keep, &path)?;

    let complete = clip.is_complete();
    session
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clip_saved(
            clip.path().to_path_buf(),
            clip.covered().start(),
            clip.covered().end(),
            clip.requested_length(),
            complete,
            now,
        );

    Ok(SavedReplay { clip })
}

/// A clip that was written and entered in its session's record.
#[derive(Debug)]
pub struct SavedReplay {
    /// What was written, and how it compares with what was asked for.
    pub clip: SavedClip,
}

/// Says what a press produced, to whoever is running the command.
///
/// To standard error, as every other diagnostic is: standard output is a
/// command's result, and a `replay` invocation in a pipeline carries nothing
/// (docs/recorder-cli.md).
///
/// A clip the buffer could not fill is **not** reported as a failure, because it
/// is not one: a hotkey pressed ten seconds into a recording asking for thirty
/// produces the ten seconds there are, and saying so is more useful than either
/// silence or an error (AGENTS.md section 45).
fn report_save(outcome: &Result<SavedReplay, ReplaySaveError>) {
    match outcome {
        Ok(saved) => {
            let clip = &saved.clip;
            eprintln!(
                "Replay saved: {} ({})",
                clip.path().display(),
                described(clip.duration())
            );
            if !clip.is_complete() {
                eprintln!(
                    "  {} of what was asked for was not in the buffer yet.",
                    described(clip.shortfall())
                );
            }
            tracing::info!(
                path = %RedactedPath::new(clip.path()),
                seconds = clip.duration().as_secs_f64(),
                complete = clip.is_complete(),
                "a replay clip was saved because the hotkey was pressed"
            );
        }
        Err(error) => {
            eprintln!("The replay could not be saved: {error}");
            tracing::error!(%error, "a replay hotkey press produced no clip");
        }
    }
}

/// A duration as somebody would say it: `30 seconds`, `1 minute 30 seconds`.
///
/// Rounded to the second, because a clip's length is bought at keyframe
/// granularity and a figure with three decimal places in a sentence about a
/// hotkey press is precision nobody asked for. The log line above carries the
/// exact figure.
fn described(duration: Duration) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let seconds = duration.as_secs_f64().round() as u64;
    match (seconds / 60, seconds % 60) {
        (0, seconds) => plural(seconds, "second"),
        (minutes, 0) => plural(minutes, "minute"),
        (minutes, seconds) => format!(
            "{} {}",
            plural(minutes, "minute"),
            plural(seconds, "second")
        ),
    }
}

/// `1 second`, `2 seconds`. English, in a sentence somebody reads.
fn plural(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("{count} {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    // `Bindings` is a test-only name in this module now: production resolves
    // the settings file rather than reaching for the shipped defaults
    // (`bindings_for`, issue #444).
    use clipped_hotkeys::{Bindings, ConflictCause};

    use super::*;

    /// The wire the settings file reaches Windows down, for this subcommand.
    ///
    /// `replay` resolved nothing and registered the shipped defaults until
    /// issue #444. The failure was not a hotkey that did nothing — it was that
    /// the conflict report tells a user whose combination is already taken to
    /// choose a different one, and choosing one changed nothing at all, so the
    /// only documented remedy was inert.
    ///
    /// Asserted through [`bindings_for`], which is the one place this module
    /// decides the question. It cannot see a `run` that called
    /// `Bindings::defaults()` directly instead — the same limitation
    /// `crate::hotkeys`'s equivalent states about its own half — which is why
    /// that function's documentation says it is the only source.
    #[test]
    fn the_combination_replay_registers_is_the_one_the_settings_file_names() {
        use clipped_session::config::{Configuration, HotkeyOverride, HotkeyOverrides};

        let mut overrides = HotkeyOverrides::none();
        overrides
            .set(
                HotkeyAction::SaveReplay,
                Some(HotkeyOverride::Bound(
                    "Ctrl+Shift+F8".parse().expect("Ctrl+Shift+F8 is a hotkey"),
                )),
            )
            .expect("nothing else is bound to Ctrl+Shift+F8");
        let mut configuration = Configuration::defaults();
        configuration.set_hotkeys(overrides);

        let bindings = bindings_for(&configuration).expect("the overrides resolve");

        assert_eq!(
            bindings
                .binding(HotkeyAction::SaveReplay)
                .map(|hotkey| hotkey.to_string())
                .as_deref(),
            Some("Ctrl+Shift+F8"),
            "replay would register a combination the settings file does not name, so the \
             overrides in it were thrown away and the user's own hotkey does nothing",
        );
    }

    /// The report for a machine where something else already owns `Ctrl`+`F10`.
    ///
    /// Built rather than registered: whether Windows refuses the combination
    /// depends on what else is installed, so a test that tried to arrange a real
    /// conflict would pass or fail by accident.
    fn a_registration_with_a_conflict() -> Registration {
        Registration::of(
            &Bindings::defaults(),
            &BTreeSet::from([HotkeyAction::SaveReplay]),
            &BTreeMap::from([(HotkeyAction::SaveReplay, ConflictCause::AlreadyRegistered)]),
        )
    }

    /// The failure this reporting exists to prevent: the key does nothing, and
    /// nothing ever said why (AGENTS.md section 45).
    #[test]
    fn a_combination_windows_refused_is_reported_by_name_before_the_recording_starts() {
        let lines = conflict_report(&a_registration_with_a_conflict());

        assert_eq!(
            lines.len(),
            1,
            "one line per refused combination: {lines:?}"
        );
        let line = &lines[0];
        assert!(
            line.contains("Ctrl+F10"),
            "the line has to name the combination, or there is nothing to change: {line}"
        );
        assert!(
            line.contains("Save replay"),
            "and the action, so it is clear what was lost: {line}"
        );
        assert!(
            line.contains("Choose a different combination"),
            "and what to do next: {line}"
        );
    }

    /// The ordinary case, which must be silent: a recorder that printed a line
    /// about every hotkey it did get would bury the one that matters.
    #[test]
    fn a_registration_windows_accepted_reports_nothing() {
        let clean = Registration::of(
            &Bindings::defaults(),
            &BTreeSet::from([HotkeyAction::SaveReplay]),
            &BTreeMap::new(),
        );

        assert!(conflict_report(&clean).is_empty());
    }

    #[test]
    fn a_duration_is_described_in_the_units_somebody_would_say_it_in() {
        assert_eq!(described(Duration::from_secs(30)), "30 seconds");
        assert_eq!(described(Duration::from_secs(1)), "1 second");
        assert_eq!(described(Duration::from_secs(60)), "1 minute");
        assert_eq!(described(Duration::from_secs(90)), "1 minute 30 seconds");
        // The figure a clip actually carries: 31.983 s of a 30 s request.
        assert_eq!(described(Duration::from_millis(31_983)), "32 seconds");
    }
}
