//! Proves the privacy guarantee this crate actually makes.
//!
//! AGENTS.md section 13 forbids window contents, microphone content, private
//! message contents and file contents from reaching a log. A rule of that shape
//! is only worth as much as its enforcement, so the standard context fields are
//! types that cannot hold free text, and paths are reduced before they are
//! recorded. These tests render real log output through a real subscriber and
//! assert on the bytes that would have been written to disk.
//!
//! What is *not* claimed: this crate cannot stop a contributor writing
//! `tracing::info!("{window_title}")` by hand. The guarantee is about the
//! standard fields and the path type, which is where the pressure to log
//! something identifying actually comes from. See `docs/logging.md`.

use std::io;
use std::sync::{Arc, Mutex};

use clipped_logging::{
    AudioSource, CaptureBackend, GameId, RedactedPath, SessionContext, SessionId, VideoEncoder,
};
use tracing_subscriber::fmt::MakeWriter;

/// Content that must never appear in a log file: a window title, a chat
/// message, a full user path and a line of a captured file.
const FORBIDDEN: &[&str] = &[
    "Counter-Strike 2 - Direct3D 11",
    "meet me at 14 Example Street",
    r"C:\Users\alice\Videos\Clipped\match.mkv",
    "-----BEGIN PRIVATE KEY-----",
];

/// Collects everything a subscriber writes, so a test can assert on it.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl CapturedLog {
    fn contents(&self) -> String {
        String::from_utf8(
            self.0
                .lock()
                .expect("the capture buffer is not poisoned")
                .clone(),
        )
        .expect("the subscriber writes UTF-8")
    }
}

impl io::Write for CapturedLog {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("the capture buffer is not poisoned")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for CapturedLog {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

/// Runs `body` against a subscriber local to this thread and returns what it
/// wrote. Thread-local rather than global so these tests stay independent of
/// each other and of test ordering.
fn captured(body: impl FnOnce()) -> String {
    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::with_default(subscriber, body);
    captured.contents()
}

#[test]
fn the_standard_fields_cannot_be_given_user_content() {
    // `capture_backend`, `encoder` and `audio_source` are enumerations, so the
    // compiler refuses free text for them and there is nothing to test at
    // runtime. `session_id` and `game_id` take a string, so they validate.
    for value in FORBIDDEN {
        assert!(
            SessionId::new(*value).is_err(),
            "session_id accepted {value:?}"
        );
        assert!(GameId::new(*value).is_err(), "game_id accepted {value:?}");
    }
}

#[test]
fn a_full_session_context_renders_only_identifiers() {
    let context = SessionContext::new(SessionId::new("01H8XGJT4A").expect("a valid session id"))
        .with_game_id(GameId::new("counter-strike-2").expect("a valid game id"))
        .with_capture_backend(CaptureBackend::DesktopDuplication)
        .with_encoder(VideoEncoder::AmdAmf)
        .with_audio_source(AudioSource::Game);

    let output = captured(|| {
        let span = context.span();
        let _entered = span.enter();
        tracing::info!(dropped_frames = 3, "capture recovered");
    });

    for field in [
        "session_id=01H8XGJT4A",
        "game_id=counter-strike-2",
        "capture_backend=desktop_duplication",
        "encoder=amd_amf",
        "audio_source=game",
    ] {
        assert!(output.contains(field), "missing {field} in:\n{output}");
    }
}

#[test]
fn a_recording_path_is_logged_without_the_directories_it_lived_in() {
    let output = captured(|| {
        tracing::info!(
            path = %RedactedPath::new(r"C:\Users\alice\Videos\Clipped\match.mkv"),
            "recording finished"
        );
    });

    assert!(output.contains("path=match.mkv#"), "{output}");
    for leaked in ["alice", "Users", "Videos", r"C:\"] {
        assert!(
            !output.contains(leaked),
            "the log leaked {leaked}:\n{output}"
        );
    }
}

#[test]
fn rejecting_user_content_does_not_print_it() {
    // The rejection path is the one place a forbidden value is held by this
    // crate, so the error it produces must describe the shape of the problem
    // and not the value — otherwise refusing to log a chat message would print
    // it to the console instead.
    for value in FORBIDDEN {
        let message = SessionId::new(*value)
            .expect_err("the value should be rejected")
            .to_string();
        let output = captured(|| tracing::warn!(error = %message, "rejected an identifier"));

        for fragment in ["Counter-Strike", "Example Street", "alice", "PRIVATE KEY"] {
            assert!(
                !output.contains(fragment),
                "the rejection leaked {fragment}:\n{output}"
            );
        }
    }
}
