//! A session somebody asked for, rather than one a game launch produced.
//!
//! [`SessionManager`](super::SessionManager) is the *policy* between "a game
//! started" and "record it". A recording started over the protocol has no such
//! policy to apply: the person pressed a button, pointed at a window, and that
//! is the whole of the decision. What it does need is everything the policy
//! produces afterwards — a [`Session`] with its identity, its recording, its
//! settings and its history, written into the sidecar the library indexes.
//!
//! The catalogue is still asked, and that is not a policy decision creeping
//! back in. Whether to record was settled by the person; *what they recorded* is
//! a question only the catalogue can answer, and answering it is what puts a
//! sitting somebody started from the window beside the sittings `watch` made of
//! the same game ([issue #403](https://github.com/wildware-uk/clipped/issues/403)).
//!
//! So this is a *second driver of the same model*, and deliberately not a
//! second model. Every line below calls something
//! [`SessionManager`](super::SessionManager) calls, in the order it calls it:
//!
//! ```text
//!  SessionManager                      ManualSession
//!  ──────────────                      ─────────────
//!  identify_process(catalogue, …)      identify_process(catalogue, …)
//!  Session::new                        Session::new
//!  record(Started)                     record(Started)
//!  resolve_for(configuration, game)    resolve_for(configuration, game)
//!  begin_recording                     begin_recording
//!  persist ─────────── sidecar::write ─────────── persist
//!  end_recording                       end_recording
//!  end(reason)                         end(RecordingEnded)
//! ```
//!
//! [`Session`], [`super::sidecar`] and [`super::persist`] are the one
//! description of what a session is and the one writer of the file it lives in
//! (AGENTS.md section 55). A sidecar written by the window and one written by
//! `watch` are the same bytes for the same facts, and
//! `a_session_somebody_asked_for_is_written_exactly_as_a_session_a_game_produced`
//! is what says so — it writes one of each and compares the two real files,
//! rather than asserting a shape either of them could drift from.
//!
//! # What is *not* here
//!
//! Everything that needs a clock to have moved. There is no restart grace, no
//! suspend rule, no deferral and no cap, because none of them can happen: the
//! session holds exactly one recording and ends when it ends. A manual session
//! that wanted a second recording would be a session manager, and it would be
//! that one rather than another.
//!
//! # Threading
//!
//! One owner, like the manager. In `apps/recorder`'s `serve` that owner is the
//! recording state's mutex, and the two calls this makes to the filesystem
//! happen on the connection thread that started the recording and on the
//! recording thread as it ends — never on a capture or encoder thread
//! (AGENTS.md section 20). Each is one small `write` and one `rename`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipped_game_detection::catalogue::Catalogue;
use clipped_game_detection::launcher::Launchers;

use crate::config::{Configuration, ResolvedSettings};

use super::session::{Session, SessionClip, SessionEndReason, SessionEventKind, SessionId};
use super::{identify_process, persist, resolve_for, RecordingOutcome};

/// Which recording of a manual session. There is only ever the one.
///
/// Named rather than written as `1` at four call sites, because the reason it
/// is one is worth stating: a session's recordings are numbered from one and
/// this session holds a single recording, so its ordinal is fixed. The file it
/// produces is therefore named by [`Session::recording_path`] without a suffix,
/// exactly as the first recording of an automatic session is.
const ONLY_RECORDING: u32 = 1;

/// The process behind the window a recording was asked for.
///
/// Two things at once, and they are the same two an automatic session takes from
/// the process watcher: what the session records as having started it, and what
/// the catalogue is asked about ([`super::identify_process`]).
///
/// The image path is optional for the reason
/// [`ProcessCandidate`](clipped_game_detection::catalogue::ProcessCandidate)
/// makes it optional. Windows will not open a process running at a higher
/// integrity level than the recorder, so an unelevated Clipped cannot always say
/// where a window's executable lives; a catalogue entry qualified by a path then
/// cannot be checked and does not match, and the sitting is filed under no game
/// rather than under a name nothing established.
#[derive(Debug, Clone, Copy)]
pub struct RecordedProcess<'a> {
    process_id: u32,
    image_name: &'a str,
    image_path: Option<&'a str>,
}

impl<'a> RecordedProcess<'a> {
    /// A process known by its identifier and the file name of its executable.
    #[must_use]
    pub const fn new(process_id: u32, image_name: &'a str) -> Self {
        Self {
            process_id,
            image_name,
            image_path: None,
        }
    }

    /// The same process, with the full path of the running image where the
    /// caller could find one out.
    #[must_use]
    pub const fn with_image_path(mut self, image_path: Option<&'a str>) -> Self {
        self.image_path = image_path;
        self
    }
}

/// One recording somebody asked for, and the session record it produces.
///
/// Opened when the recording starts and consumed when it ends. Its sidecar is
/// on disk from the moment it is opened — before the encoder has produced
/// anything — so a recorder killed mid-recording leaves a session record saying
/// what was being recorded, which is what the library reconciles the orphaned
/// file against (AGENTS.md section 17, `crates/library/src/index`).
#[derive(Debug)]
pub struct ManualSession {
    /// Where the sidecar goes: beside the recording, as an automatic session's
    /// does.
    directory: PathBuf,
    session: Session,
    settings: ResolvedSettings,
}

impl ManualSession {
    /// Opens a session for a recording of `output`, and writes its sidecar.
    ///
    /// `process` is the window's, which is what a session records as having
    /// started it — the same fields an automatic session's `session-started`
    /// event carries, from the same field of the same event, so that "what was
    /// this a recording of" is one question with one answer.
    ///
    /// **The catalogue decides the game, exactly as it does for a launch.**
    /// [`super::identify_process`] is the one lookup, so a sitting somebody
    /// recorded by pointing at Counter-Strike is filed under Counter-Strike and
    /// the library groups it with the sittings `watch` recorded of it
    /// ([issue #403](https://github.com/wildware-uk/clipped/issues/403)). A
    /// window the catalogue does not claim — a browser, an editor, a game
    /// nobody has catalogued, a game the user excluded — is still recorded, and
    /// its session stays [`GameIdentity::Unidentified`](super::GameIdentity)
    /// rather than being filed under a guess (AGENTS.md section 27).
    ///
    /// A caller with no catalogue to offer passes an empty one and gets that
    /// second answer for everything, which is what makes a catalogue that could
    /// not be read cost attribution rather than the recording (AGENTS.md
    /// section 16).
    ///
    /// The settings are resolved **here and once**, through
    /// [`Configuration`]'s single fold, exactly as
    /// [`SessionManager::begin_recording`](super::SessionManager) resolves
    /// them. [`Self::settings`] is what the caller applies to the recording it
    /// is about to start; a configuration that changes while it runs changes
    /// nothing about it.
    ///
    /// A sidecar that cannot be written is a warning and not a failure: the
    /// video is what cannot be made again (AGENTS.md section 17).
    #[must_use]
    pub fn start(
        directory: &Path,
        output: PathBuf,
        configuration: &Configuration,
        catalogue: &Catalogue,
        launchers: &Launchers,
        process: RecordedProcess<'_>,
        now: SystemTime,
    ) -> Self {
        // The child processes the answer carries are the manager's business and
        // not this session's: they are how a *game* is watched for having
        // exited, and this session ends when its one recording does.
        let (game, _children) =
            identify_process(catalogue, launchers, process.image_name, process.image_path);
        // A game the catalogue named resolves that game's settings layer, so a
        // frame rate somebody set for Counter-Strike applies to a recording of
        // it however the recording was started. The same fold, the same lookup
        // (`super::resolve_for`, docs/sessions.md).
        let settings = resolve_for(configuration, &game);

        let mut session = Session::new(game, now);
        session.record(
            now,
            SessionEventKind::Started {
                pid: process.process_id,
                image_name: process.image_name.to_owned(),
            },
        );
        session.begin_recording(ONLY_RECORDING, output, settings.clone(), now);

        let directory = directory.to_path_buf();
        persist(&directory, &session);

        tracing::info!(
            session = session.id().as_str(),
            pid = process.process_id,
            image = process.image_name,
            game = session.game().slug(),
            // The path is deliberately absent: it is a user path that can carry
            // an account name and a library layout, and it is here to be
            // matched on rather than logged (`clipped_windows::
            // process_image_path`, docs/logging.md). Whether one was available
            // is the part worth knowing, because an entry qualified by a path
            // cannot match without it.
            image_path_known = process.image_path.is_some(),
            scope = %settings.scope(),
            settings = %settings,
            "a session was opened for a recording that was asked for, with the settings that \
             apply to the game the catalogue made of its window"
        );

        Self {
            directory,
            session,
            settings,
        }
    }

    /// The session's identifier, which its sidecar is named from.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        self.session.id()
    }

    /// The session as it stands.
    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.session
    }

    /// The settings this recording was resolved for.
    ///
    /// Applied to the recording by the caller with
    /// [`ResolvedSettings::apply_configured_to`], which is what `watch` does
    /// with the answer for the same reason: only a setting a user configured
    /// replaces what the caller was asked for.
    #[must_use]
    pub const fn settings(&self) -> &ResolvedSettings {
        &self.settings
    }

    /// Where this session's sidecar is.
    #[must_use]
    pub fn sidecar_path(&self) -> PathBuf {
        self.session.sidecar_path(&self.directory)
    }

    /// Where the next clip saved out of this session goes.
    ///
    /// The session names it, for the reason it names its recording: everything
    /// one sitting produced sorts together in a directory listing, and a clip
    /// whose name says nothing about where it came from is a file nobody can
    /// place six months later. [`Session::clip_path`] is the naming and
    /// [`Self::clip_saved`] is what makes the next call answer differently.
    #[must_use]
    pub fn next_clip_path(&self) -> PathBuf {
        self.session
            .clip_path(&self.directory, self.session.clips_saved() + 1)
    }

    /// Records a clip that has been written, and rewrites the sidecar.
    ///
    /// Called **after** the file exists, so that a session record naming a clip
    /// is a session record whose clip is on disk: the library reconciles what
    /// the sidecar says against the filesystem, and a row for a file that was
    /// never written would be an entry the user cannot play (AGENTS.md
    /// section 54).
    ///
    /// `source_start` and `source_end` are offsets into this session's
    /// recording, on the recording's own timeline, which is the timeline the
    /// replay buffer's packets are stamped on.
    ///
    /// A sidecar that cannot be rewritten is a warning and not a failure, for
    /// the reason [`Self::start`] gives: the clip is what cannot be made again.
    pub fn clip_saved(
        &mut self,
        path: PathBuf,
        source_start: Duration,
        source_end: Duration,
        requested: Duration,
        complete: bool,
        now: SystemTime,
    ) -> &SessionClip {
        self.session.add_clip(SessionClip {
            path,
            created_at: now,
            source_index: ONLY_RECORDING,
            source_start,
            source_end,
            requested,
            complete,
        });
        persist(&self.directory, &self.session);

        let clip = self
            .session
            .clips()
            .last()
            .expect("a clip was just added to the session");
        tracing::info!(
            session = self.session.id().as_str(),
            clip = %clipped_logging::RedactedPath::new(clip.path()),
            seconds = clip.duration().as_secs_f64(),
            requested_seconds = clip.requested().as_secs_f64(),
            complete = clip.is_complete(),
            "a replay clip was saved out of this session's recording"
        );
        clip
    }

    /// Records what the recording turned out to be, closes the session and
    /// writes the sidecar for the last time.
    ///
    /// Consuming, because a session that has ended cannot gain anything: the
    /// [`Session`] is handed back for whatever wants to report it, and this
    /// type is gone.
    #[must_use]
    pub fn finish(mut self, outcome: &RecordingOutcome, now: SystemTime) -> Session {
        self.finish_in_place(outcome, now);
        self.session
    }

    /// The same, without consuming the session.
    ///
    /// For an owner that cannot give the session up: `apps/recorder`'s `serve`
    /// holds it behind a mutex so that a `save_replay` arriving on a connection
    /// thread can enter its clip in it, and taking it out of that mutex to end
    /// it would mean waiting for every reader first. Ending it in place needs
    /// only the lock everything else takes.
    ///
    /// Ending twice would write a second `session-ended` event, so callers end
    /// a session once — which is what dropping the recording that owns it
    /// enforces in practice.
    pub fn finish_in_place(&mut self, outcome: &RecordingOutcome, now: SystemTime) -> &Session {
        let summary = outcome.summarise();
        let outcome_token = summary.token();
        let produced_a_file = summary.produced_a_file();

        self.session.end_recording(ONLY_RECORDING, summary, now);
        self.session.end(SessionEndReason::RecordingEnded, now);
        persist(&self.directory, &self.session);

        tracing::info!(
            session = self.session.id().as_str(),
            outcome = outcome_token,
            produced_a_file,
            sidecar = %clipped_logging::RedactedPath::new(self.session.sidecar_path(&self.directory)),
            "the session that was opened for a recording ended, because its recording did"
        );

        &self.session
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clipped_game_detection::catalogue::EntrySource;

    use super::*;
    use crate::automatic::{GameIdentity, RecordingOutcomeSummary, UNATTRIBUTED};

    /// The three answers the catalogue can give, as four entries: one plain
    /// game, one that only counts where it is installed, and two that tie on a
    /// shared executable name.
    const GAMES: &str = r#"
schema_version = 1

[[game]]
game_id = "test-game"
name = "Test Game"
[[game.executables]]
name = "test-game.exe"

[[game]]
game_id = "installed-somewhere"
name = "Installed Somewhere"
[[game.executables]]
name = "somewhere.exe"
path_contains = "games/installed-somewhere"

[[game]]
game_id = "first-tie"
name = "First Tie"
[[game.executables]]
name = "tied.exe"

[[game]]
game_id = "second-tie"
name = "Second Tie"
[[game.executables]]
name = "tied.exe"
"#;

    fn catalogue() -> Catalogue {
        Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue")
    }

    /// The same catalogue with a decision of the user's laid over it, as their
    /// own file would.
    fn catalogue_decided(decision: &str) -> Catalogue {
        let overlay = Catalogue::parse(
            &format!("schema_version = 1\n{decision}"),
            EntrySource::Overlay {
                path: PathBuf::from(r"C:\Users\somebody\games.toml"),
            },
        )
        .expect("the fixture is a valid overlay");
        catalogue().overlaid_with(overlay)
    }

    fn moment(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "clipped-manual-session-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
        directory
    }

    /// A session opened for a recording of `process`, over the fixture
    /// catalogue and nothing configured.
    fn session_of(
        directory: &Path,
        catalogue: &Catalogue,
        process: RecordedProcess<'_>,
    ) -> ManualSession {
        ManualSession::start(
            directory,
            directory.join("clipped-20260813-120000.mkv"),
            &Configuration::defaults(),
            catalogue,
            &Launchers::none(),
            process,
            moment(1_786_458_725),
        )
    }

    #[test]
    fn a_window_belonging_to_a_game_the_catalogue_knows_is_filed_under_that_game() {
        // Issue #403's first acceptance criterion, at the point the answer is
        // decided: the sitting carries the same `game` an automatic session of
        // the same process would, so the library groups the two together
        // instead of showing one row of Test Game and one of nothing.
        let directory = scratch("known");
        let session = session_of(
            &directory,
            &catalogue(),
            RecordedProcess::new(4_242, "test-game.exe"),
        );

        assert_eq!(
            session.session().game(),
            &GameIdentity::Known {
                game_id: "test-game".to_owned(),
                name: "Test Game".to_owned(),
            }
        );
        assert!(
            session.id().as_str().starts_with("test-game-"),
            "a session of a game is named after it: {}",
            session.id()
        );

        let written =
            std::fs::read_to_string(session.sidecar_path()).expect("the sidecar is written");
        assert!(
            written.contains("\"test-game\"") && written.contains("\"Test Game\""),
            "the record the library reads has to name the game: {written}"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_game_that_only_counts_where_it_is_installed_needs_the_path_before_it_is_named() {
        // Both halves, because they are one decision. Most shipped entries are
        // qualified by an install directory — Counter-Strike 2 among them — so
        // a caller that found out where the window's executable lives gets the
        // game, and one that could not gets no game rather than the wrong one.
        let directory = scratch("qualified");
        let named = session_of(
            &directory,
            &catalogue(),
            RecordedProcess::new(4_242, "somewhere.exe")
                .with_image_path(Some(r"D:\games\installed-somewhere\bin\somewhere.exe")),
        );
        assert_eq!(named.session().game().slug(), "installed-somewhere");

        let unqualified = session_of(
            &directory,
            &catalogue(),
            RecordedProcess::new(4_242, "somewhere.exe"),
        );
        assert_eq!(
            unqualified.session().game(),
            &GameIdentity::Unidentified,
            "an entry that asked where the game is installed must not match on the file name \
             alone"
        );

        let elsewhere = session_of(
            &directory,
            &catalogue(),
            RecordedProcess::new(4_242, "somewhere.exe")
                .with_image_path(Some(r"D:\games\something-else\bin\somewhere.exe")),
        );
        assert_eq!(elsewhere.session().game(), &GameIdentity::Unidentified);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_game_the_user_excluded_is_not_the_game_a_window_recording_is_filed_under() {
        // An exclusion is a decision about a game and not about a subcommand
        // (issue #45). A user who told Clipped to leave Test Game alone and
        // then pointed at its window still gets their recording — they asked
        // for it — and it is not filed under the game they excluded.
        let directory = scratch("excluded");
        let session = session_of(
            &directory,
            &catalogue_decided("[[decision]]\ngame_id = \"test-game\"\nexcluded = true\n"),
            RecordedProcess::new(4_242, "test-game.exe"),
        );

        assert_eq!(session.session().game(), &GameIdentity::Unidentified);
        assert_eq!(session.session().game().slug(), UNATTRIBUTED);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_game_the_user_renamed_is_filed_under_the_name_they_chose() {
        // The other half of the user's overlay, and the reason the lookup goes
        // through the loaded catalogue rather than the shipped data: a rename
        // is a decision that has to reach every place a game is named.
        let directory = scratch("renamed");
        let session = session_of(
            &directory,
            &catalogue_decided("[[decision]]\ngame_id = \"test-game\"\nname = \"TG\"\n"),
            RecordedProcess::new(4_242, "test-game.exe"),
        );

        assert_eq!(
            session.session().game(),
            &GameIdentity::Known {
                game_id: "test-game".to_owned(),
                name: "TG".to_owned(),
            }
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_window_two_entries_claim_equally_well_is_recorded_as_the_tie_it_is() {
        // The catalogue does not choose, and neither does this: the sitting is
        // filed under `unattributed` with both candidates written down, which
        // is what somebody needs to decide it afterwards.
        let directory = scratch("tie");
        let session = session_of(
            &directory,
            &catalogue(),
            RecordedProcess::new(4_242, "tied.exe"),
        );

        assert_eq!(
            session.session().game(),
            &GameIdentity::Ambiguous {
                candidates: vec!["first-tie".to_owned(), "second-tie".to_owned()],
            }
        );
        assert_eq!(session.session().game().slug(), UNATTRIBUTED);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_sidecar_is_on_disk_before_the_recording_has_produced_anything() {
        // The whole reason the file is written when the session opens rather
        // than when it closes: a recorder killed during a recording leaves a
        // record of what was being recorded, and the library has something to
        // reconcile the file against (AGENTS.md section 17).
        let directory = scratch("opened");
        let output = directory.join("clipped-20260813-120000.mkv");
        let session = ManualSession::start(
            &directory,
            output.clone(),
            &Configuration::defaults(),
            &catalogue(),
            &Launchers::none(),
            RecordedProcess::new(4_242, "notepad.exe"),
            moment(1_786_458_725),
        );

        let written = std::fs::read_to_string(session.sidecar_path())
            .expect("the sidecar is written when the session opens");
        assert!(written.contains("\"unidentified\""), "{written}");
        assert!(
            written.contains(&output.display().to_string().replace('\\', "\\\\")),
            "the sidecar has to name the file that is being written: {written}"
        );
        assert!(
            written.contains("\"ended_at\": null"),
            "a session that is still recording has not ended: {written}"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn finishing_stores_the_outcome_against_the_recording_and_closes_the_session() {
        let directory = scratch("finished");
        let output = directory.join("clipped-20260813-120000.mkv");
        let session = ManualSession::start(
            &directory,
            output,
            &Configuration::defaults(),
            &catalogue(),
            &Launchers::none(),
            RecordedProcess::new(4_242, "test-game.exe"),
            moment(1_786_458_725),
        );
        let path = session.sidecar_path();

        let ended = session.finish(
            &RecordingOutcome::NoWindow {
                detail: "the window went".to_owned(),
            },
            moment(1_786_458_785),
        );

        assert_eq!(ended.ended_at(), Some(moment(1_786_458_785)));
        assert!(matches!(
            ended.recordings()[0].outcome(),
            Some(RecordingOutcomeSummary::NoWindow { .. })
        ));

        let written = std::fs::read_to_string(&path).expect("the sidecar is rewritten");
        assert!(
            written.contains("recording-ended"),
            "a manual session ends because its recording did: {written}"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_saved_clip_is_written_into_the_session_record_with_where_it_came_from() {
        // The whole of "a replay reaches the library the same way a recording
        // does" on this side of the file: the session records the clip, its
        // bounds in the recording it was cut from, and whether the buffer held
        // the whole request — and rewrites the sidecar immediately, so a
        // recorder killed a second later has already said the clip exists
        // (AGENTS.md section 17).
        let directory = scratch("clips");
        let mut session = ManualSession::start(
            &directory,
            directory.join("clipped-20260813-120000.mkv"),
            &Configuration::defaults(),
            &catalogue(),
            &Launchers::none(),
            RecordedProcess::new(4_242, "cs2.exe"),
            moment(1_786_458_725),
        );

        let first = session.next_clip_path();
        assert!(
            first.ends_with(format!("clipped-{}-replay-1.mkv", session.id())),
            "a clip is named after its session and numbered within it: {}",
            first.display()
        );

        let clip = session.clip_saved(
            first.clone(),
            Duration::from_millis(553_017),
            Duration::from_secs(583),
            Duration::from_secs(30),
            true,
            moment(1_786_458_785),
        );
        assert_eq!(clip.duration(), Duration::from_millis(29_983));

        assert_eq!(
            session.next_clip_path(),
            directory.join(format!("clipped-{}-replay-2.mkv", session.id())),
            "a second save must not be handed the first one's name"
        );

        let written =
            std::fs::read_to_string(session.sidecar_path()).expect("the sidecar is rewritten");
        let file: serde_json::Value = serde_json::from_str(&written).expect("the sidecar is JSON");
        let clip = &file["clips"][0];
        assert_eq!(clip["source_recording"], serde_json::Value::from(1));
        assert_eq!(
            clip["source_start_seconds"],
            serde_json::Value::from(553.017)
        );
        assert_eq!(clip["source_end_seconds"], serde_json::Value::from(583.0));
        assert_eq!(clip["requested_seconds"], serde_json::Value::from(30.0));
        assert_eq!(clip["complete"], serde_json::Value::from(true));
        assert!(
            clip["path"]
                .as_str()
                .expect("the clip names its file")
                .ends_with("-replay-1.mkv"),
            "{clip}"
        );
        assert!(
            written.contains("replay-saved"),
            "the save belongs in the session's history as well as in its clips: {written}"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_window_the_catalogue_does_not_claim_is_recorded_and_filed_under_no_game() {
        // Issue #403's second acceptance criterion. A text editor is not a game
        // and must not be filed as one, however much a user would like their
        // library tidy: `Unidentified` is the honest answer and it is filed
        // under the same name a tie is, because a session with no single game
        // has to be named after something that is not a game.
        let directory = scratch("slug");
        let session = session_of(
            &directory,
            &catalogue(),
            RecordedProcess::new(1, "notepad.exe"),
        );

        assert_eq!(session.session().game(), &GameIdentity::Unidentified);
        assert_eq!(session.session().game().slug(), UNATTRIBUTED);
        assert!(
            session.id().as_str().starts_with(UNATTRIBUTED),
            "{}",
            session.id()
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_settings_a_recording_is_made_with_are_the_ones_written_down() {
        // The same fold `watch` resolves through, so a user who configured a
        // frame rate gets it on a recording they started from the window and
        // the session's record says where the answer came from.
        let directory = scratch("settings");
        let mut configuration = Configuration::defaults();
        let mut global = crate::config::Preferences::none();
        global
            .set_framerate(Some(144))
            .expect("144 is an acceptable frame rate");
        configuration.set_global(global);

        let session = ManualSession::start(
            &directory,
            directory.join("clipped-20260813-120000.mkv"),
            &configuration,
            &catalogue(),
            &Launchers::none(),
            RecordedProcess::new(1, "notepad.exe"),
            moment(1_786_458_725),
        );

        assert_eq!(session.settings().framerate().get(), 144);
        let written =
            std::fs::read_to_string(session.sidecar_path()).expect("the sidecar is written");
        assert!(
            written.contains("\"144\""),
            "the session's record has to say what the recording was made with: {written}"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_recording_of_a_game_is_made_with_that_games_settings_rather_than_the_global_ones() {
        // The behaviour change identifying the game brings with it, stated
        // rather than discovered (docs/sessions.md): a session with a `known`
        // game resolves that game's layer, so somebody who set Test Game to 144
        // gets 144 whether the recording was started by `watch` or by pointing
        // at the window. Before #403 every window recording resolved the global
        // layer, because every window recording had no game.
        let directory = scratch("per-game");
        let mut configuration = Configuration::defaults();
        let mut global = crate::config::Preferences::none();
        global
            .set_framerate(Some(30))
            .expect("30 is an acceptable frame rate");
        configuration.set_global(global);
        let mut for_the_game = crate::config::Preferences::none();
        for_the_game
            .set_framerate(Some(144))
            .expect("144 is an acceptable frame rate");
        configuration.set_game(
            crate::config::GameKey::parse("test-game").expect("a catalogue identifier is a key"),
            for_the_game,
        );

        let identified = ManualSession::start(
            &directory,
            directory.join("clipped-20260813-120000.mkv"),
            &configuration,
            &catalogue(),
            &Launchers::none(),
            RecordedProcess::new(4_242, "test-game.exe"),
            moment(1_786_458_725),
        );
        assert_eq!(identified.settings().framerate().get(), 144);

        // And a window that is not that game is still the global layer, which
        // is what makes the line above about the game rather than about the
        // settings file having one entry in it.
        let anonymous = ManualSession::start(
            &directory,
            directory.join("clipped-20260813-130000.mkv"),
            &configuration,
            &catalogue(),
            &Launchers::none(),
            RecordedProcess::new(4_242, "notepad.exe"),
            moment(1_786_462_325),
        );
        assert_eq!(anonymous.settings().framerate().get(), 30);

        let _ = std::fs::remove_dir_all(&directory);
    }
}
