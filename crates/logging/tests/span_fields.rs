//! Proves that a session span renders each of its fields exactly once.
//!
//! `docs/logging.md` publishes a field vocabulary, and that vocabulary is what
//! somebody greps a user's log with. A field that appears twice on every line
//! makes those lines twice as long and reads as a fault in whatever produced
//! them, so "how many times" is part of the contract and not a detail of the
//! formatter.
//!
//! The counting here is deliberately done on the rendered bytes rather than on
//! the span's field set. What went wrong in #185 was invisible at the API
//! level: the span was built with the right fields and the right values, and
//! only the formatted output was wrong.
//!
//! # Why these tests use two layers
//!
//! The doubling in #185 needed *two* `fmt` layers to appear, which is why it
//! showed up in the recorder and not in this crate's own tests.
//!
//! `tracing-subscriber` keeps a span's formatted fields in one
//! `FormattedFields<DefaultFields>` slot in the span's extensions, keyed by
//! type. Two `fmt` layers using the same field formatter — which is what
//! [`clipped_logging::init`] builds, one writing the log file and one writing
//! the console — therefore share a single slot, and each of them appends to it
//! when `Span::record` fills in a field. Creation-time fields are written once,
//! because only the first layer to see the span initialises the slot; recorded
//! fields are written once per layer.
//!
//! That is why the fix is to build the span with its values rather than to
//! declare them `Empty` and record them afterwards. Configuring the formatter
//! would have fixed the recorder's own subscriber and left the bug waiting for
//! anyone who adds a second layer, or points a different subscriber at this
//! crate's spans.

use std::io;
use std::sync::{Arc, Mutex};

use clipped_logging::{
    AudioSource, CaptureBackend, GameId, SessionContext, SessionId, VideoEncoder,
};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

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

/// Runs `body` against a subscriber shaped like the one the recorder installs,
/// and returns what its first layer wrote.
///
/// Two `fmt` layers, because that is what [`clipped_logging::init`] builds — a
/// file and a console — and because one layer cannot reproduce #185. Only the
/// first writes into the returned buffer; the second goes to a sink, so a
/// doubled *field* is visible without every *line* also being doubled.
///
/// Thread-local rather than global so these tests stay independent of each
/// other and of test ordering.
fn captured(body: impl FnOnce()) -> String {
    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(captured.clone())
                .with_ansi(false),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(io::sink)
                .with_ansi(false),
        );

    tracing::subscriber::with_default(subscriber, body);
    captured.contents()
}

/// How many times `name=` introduces a field in `output`.
///
/// Matching the name with its `=` attached is what makes this count fields
/// rather than substrings: a value that happened to contain a field's name
/// would not be followed by `=`.
fn field_occurrences(output: &str, name: &str) -> usize {
    output.matches(&format!("{name}=")).count()
}

fn full_context() -> SessionContext {
    SessionContext::new(SessionId::new("01H8XGJT4A").expect("a valid session id"))
        .with_game_id(GameId::new("counter-strike-2").expect("a valid game id"))
        .with_capture_backend(CaptureBackend::WindowsGraphicsCapture)
        .with_encoder(VideoEncoder::Nvenc)
        .with_audio_source(AudioSource::Game)
}

#[test]
fn every_field_of_a_full_context_is_rendered_exactly_once() {
    let output = captured(|| {
        let span = full_context().span();
        let _entered = span.enter();
        tracing::info!("recording finished");
    });

    for field in [
        "session_id",
        "game_id",
        "capture_backend",
        "encoder",
        "audio_source",
    ] {
        assert_eq!(
            field_occurrences(&output, field),
            1,
            "{field} should appear once, in:\n{output}"
        );
    }
}

#[test]
fn a_field_that_was_never_given_a_value_is_absent_rather_than_empty() {
    // The property `SessionContext` documents: a session whose game is not yet
    // known carries no `game_id`, rather than a `game_id=` with nothing after
    // it. An empty field is worse than a missing one — it reads as a value that
    // was lost, and it matches a grep for the field.
    let context = SessionContext::new(SessionId::new("01H8XGJT4A").expect("a valid session id"))
        .with_capture_backend(CaptureBackend::WindowsGraphicsCapture);

    let output = captured(|| {
        let span = context.span();
        let _entered = span.enter();
        tracing::info!("capture started");
    });

    assert_eq!(field_occurrences(&output, "session_id"), 1, "in:\n{output}");
    assert_eq!(
        field_occurrences(&output, "capture_backend"),
        1,
        "in:\n{output}"
    );

    for absent in ["game_id", "encoder", "audio_source"] {
        assert_eq!(
            field_occurrences(&output, absent),
            0,
            "{absent} was never given a value, so it should not be rendered at \
             all, in:\n{output}"
        );
    }
}

#[test]
fn a_context_narrowed_after_the_span_exists_still_renders_once() {
    // The shape that produced #185: a session learns its encoder only once the
    // encoder has been opened, so the context is built up in stages. Each stage
    // makes a new span from a new context; none of them may double a field.
    let mut context =
        SessionContext::new(SessionId::new("01H8XGJT4A").expect("a valid session id"));

    for stage in 0..3 {
        context = match stage {
            0 => context.with_capture_backend(CaptureBackend::DesktopDuplication),
            1 => context.with_encoder(VideoEncoder::SoftwareH264),
            _ => context.with_audio_source(AudioSource::Microphone),
        };

        let output = captured(|| {
            let span = context.span();
            let _entered = span.enter();
            tracing::info!("stage reached");
        });

        for field in ["session_id", "capture_backend", "encoder", "audio_source"] {
            assert!(
                field_occurrences(&output, field) <= 1,
                "{field} is doubled at stage {stage}, in:\n{output}"
            );
        }
    }
}
