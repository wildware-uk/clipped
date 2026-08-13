//! A session somebody asked for, rather than one a game launch produced.
//!
//! [`SessionManager`](super::SessionManager) is the *policy* between "a game
//! started" and "record it". A recording started over the protocol has no such
//! policy to apply: the person pressed a button, pointed at a window, and that
//! is the whole of the decision. What it does need is everything the policy
//! produces afterwards — a [`Session`] with its identity, its recording, its
//! settings and its history, written into the sidecar the library indexes.
//!
//! So this is a *second driver of the same model*, and deliberately not a
//! second model. Every line below calls something
//! [`SessionManager`](super::SessionManager) calls, in the order it calls it:
//!
//! ```text
//!  SessionManager                      ManualSession
//!  ──────────────                      ─────────────
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
use std::time::SystemTime;

use crate::config::{Configuration, ResolvedSettings};

use super::session::{GameIdentity, Session, SessionEndReason, SessionEventKind, SessionId};
use super::{persist, resolve_for, RecordingOutcome};

/// Which recording of a manual session. There is only ever the one.
///
/// Named rather than written as `1` at four call sites, because the reason it
/// is one is worth stating: a session's recordings are numbered from one and
/// this session holds a single recording, so its ordinal is fixed. The file it
/// produces is therefore named by [`Session::recording_path`] without a suffix,
/// exactly as the first recording of an automatic session is.
const ONLY_RECORDING: u32 = 1;

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
    /// `pid` and `image_name` are the window's process, which is what a session
    /// records as having started it — the same two fields an automatic
    /// session's `session-started` event carries, from the same field of the
    /// same event, so that "what was this a recording of" is one question with
    /// one answer.
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
        pid: u32,
        image_name: &str,
        now: SystemTime,
    ) -> Self {
        // Nothing asked the catalogue about this window, and saying so is not
        // the same as saying the catalogue was unsure. See
        // [`GameIdentity::Unidentified`].
        let game = GameIdentity::Unidentified;
        let settings = resolve_for(configuration, &game);

        let mut session = Session::new(game, now);
        session.record(
            now,
            SessionEventKind::Started {
                pid,
                image_name: image_name.to_owned(),
            },
        );
        session.begin_recording(ONLY_RECORDING, output, settings.clone(), now);

        let directory = directory.to_path_buf();
        persist(&directory, &session);

        tracing::info!(
            session = session.id().as_str(),
            pid,
            image = image_name,
            scope = %settings.scope(),
            settings = %settings,
            "a session was opened for a recording that was asked for, with the settings that \
             apply to a game nothing identified"
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

    /// Records what the recording turned out to be, closes the session and
    /// writes the sidecar for the last time.
    ///
    /// Consuming, because a session that has ended cannot gain anything: the
    /// [`Session`] is handed back for whatever wants to report it, and this
    /// type is gone.
    #[must_use]
    pub fn finish(mut self, outcome: &RecordingOutcome, now: SystemTime) -> Session {
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

        self.session
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::automatic::{RecordingOutcomeSummary, UNATTRIBUTED};

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
            4_242,
            "cs2.exe",
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
            4_242,
            "cs2.exe",
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
    fn a_session_nothing_identified_is_filed_under_the_same_name_a_tie_is() {
        // Not because the two mean the same thing — they do not — but because
        // the *filing* answer is the same one: a session with no single game
        // has to be named after something that is not a game.
        let directory = scratch("slug");
        let session = ManualSession::start(
            &directory,
            directory.join("clipped-20260813-120000.mkv"),
            &Configuration::defaults(),
            1,
            "notepad.exe",
            moment(1_786_458_725),
        );

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
            1,
            "cs2.exe",
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
}
