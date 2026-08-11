//! The standard context fields, expressed as types rather than strings.
//!
//! AGENTS.md section 35 lists `session_id`, `game_id`, `capture_backend`,
//! `encoder` and `audio_source` as the context every diagnostic should carry.
//! Attaching them by hand at each call site has two failure modes that only
//! show up when someone is trying to debug a user's recording months later: a
//! misspelled field name makes a log line unsearchable, and a field
//! accidentally filled with free text (a window title, a chat line) puts user
//! content in a log, which AGENTS.md section 13 forbids.
//!
//! Both are prevented structurally here. The field names exist once, in
//! [`SessionContext::span`]. The values are either closed enumerations or
//! validated identifiers, so free text cannot be recorded through them at all.

use std::fmt;

use tracing::Span;

use crate::error::{IdentifierRejection, LoggingError};

/// The longest identifier accepted by [`SessionId`] and [`GameId`].
const IDENTIFIER_LIMIT: usize = 64;

/// Rejects anything that is not a plain identifier.
///
/// Free text fails this: spaces, punctuation, path separators and newlines are
/// all outside the permitted set, so a window title, a file path or a message
/// body cannot be smuggled into a log field.
fn validate_identifier(field: &'static str, value: &str) -> Result<(), LoggingError> {
    let reason = if value.is_empty() {
        Some(IdentifierRejection::Empty)
    } else if value.chars().count() > IDENTIFIER_LIMIT {
        Some(IdentifierRejection::TooLong {
            limit: IDENTIFIER_LIMIT,
            length: value.chars().count(),
        })
    } else {
        value
            .chars()
            .position(|character| {
                !(character.is_ascii_alphanumeric()
                    || character == '.'
                    || character == '-'
                    || character == '_')
            })
            .map(|index| IdentifierRejection::ForbiddenCharacter { index })
    };

    match reason {
        Some(reason) => Err(LoggingError::InvalidIdentifier { field, reason }),
        None => Ok(()),
    }
}

/// Identifies one recording session across every log line it produced.
///
/// The value is opaque to this crate; the session subsystem decides what it
/// means. It must be a plain identifier so that it stays greppable and cannot
/// carry user content.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// Validates and wraps a session identifier.
    ///
    /// # Errors
    ///
    /// Returns [`LoggingError::InvalidIdentifier`] if the value is empty,
    /// longer than 64 characters, or contains anything outside
    /// `[A-Za-z0-9._-]`.
    pub fn new(value: impl Into<String>) -> Result<Self, LoggingError> {
        let value = value.into();
        validate_identifier("session_id", &value)?;
        Ok(Self(value))
    }

    /// The identifier as it will appear in logs.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies the game a session belongs to, for example `counter-strike-2`.
///
/// Constrained exactly like [`SessionId`]: an identifier, never a window title
/// and never a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GameId(String);

impl GameId {
    /// Validates and wraps a game identifier.
    ///
    /// # Errors
    ///
    /// Returns [`LoggingError::InvalidIdentifier`] if the value is empty,
    /// longer than 64 characters, or contains anything outside
    /// `[A-Za-z0-9._-]`.
    pub fn new(value: impl Into<String>) -> Result<Self, LoggingError> {
        let value = value.into();
        validate_identifier("game_id", &value)?;
        Ok(Self(value))
    }

    /// The identifier as it will appear in logs.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How frames are being acquired (SPEC.md section 7).
///
/// Closed on purpose: a new backend is a deliberate change here and in the
/// documented field vocabulary, not an arbitrary string appearing in logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureBackend {
    /// Windows Graphics Capture.
    WindowsGraphicsCapture,
    /// DXGI Desktop Duplication.
    DesktopDuplication,
}

impl fmt::Display for CaptureBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WindowsGraphicsCapture => "windows_graphics_capture",
            Self::DesktopDuplication => "desktop_duplication",
        })
    }
}

/// Which video encoder a session is using (SPEC.md section 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VideoEncoder {
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD Advanced Media Framework.
    AmdAmf,
    /// Intel Quick Sync Video.
    IntelQuickSync,
    /// Software H.264.
    SoftwareH264,
    /// Software H.265.
    SoftwareH265,
}

impl fmt::Display for VideoEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Nvenc => "nvenc",
            Self::AmdAmf => "amd_amf",
            Self::IntelQuickSync => "intel_quicksync",
            Self::SoftwareH264 => "software_h264",
            Self::SoftwareH265 => "software_h265",
        })
    }
}

/// Which audio track a diagnostic concerns (SPEC.md section 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioSource {
    /// The downmixed compatibility track.
    CompatibilityMix,
    /// Audio from the game's process tree.
    Game,
    /// System audio other than the game.
    OtherSystem,
    /// Everything the machine is playing, captured from the output device in
    /// loopback mode.
    ///
    /// Distinct from the three above, which are the tracks a session routes
    /// audio into: this is one capture of one endpoint, before anything has
    /// been attributed to a process or mixed. It is what `clipped-audio`
    /// records today, and in M2 it becomes the source the others are separated
    /// out of ([ADR 0003](../../../docs/adr/0003-process-specific-audio-capture.md)).
    SystemAudio,
    /// The capture microphone.
    Microphone,
    /// A separately tracked application, such as a voice chat client.
    Application,
}

impl fmt::Display for AudioSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CompatibilityMix => "compatibility_mix",
            Self::Game => "game",
            Self::OtherSystem => "other_system",
            Self::SystemAudio => "system_audio",
            Self::Microphone => "microphone",
            Self::Application => "application",
        })
    }
}

/// The standard context attached to a session's diagnostics.
///
/// Build one when a session starts, enter its [`span`](Self::span), and every
/// event emitted underneath carries the same fields without repeating them:
///
/// ```
/// use clipped_logging::{CaptureBackend, SessionContext, SessionId};
///
/// let context = SessionContext::new(SessionId::new("01H8XGJ")?)
///     .with_capture_backend(CaptureBackend::WindowsGraphicsCapture);
/// let span = context.span();
/// let _entered = span.enter();
/// tracing::info!(width = 2560, height = 1440, "capture started");
/// # Ok::<(), clipped_logging::LoggingError>(())
/// ```
///
/// Fields that are not yet known are simply absent from the log line rather
/// than recorded as a placeholder, so `game_id` appearing at all means game
/// detection had actually resolved one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionContext {
    session_id: SessionId,
    game_id: Option<GameId>,
    capture_backend: Option<CaptureBackend>,
    encoder: Option<VideoEncoder>,
    audio_source: Option<AudioSource>,
}

impl SessionContext {
    /// Starts a context for a session whose other properties are not yet known.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            game_id: None,
            capture_backend: None,
            encoder: None,
            audio_source: None,
        }
    }

    /// Records which game the session belongs to.
    #[must_use]
    pub fn with_game_id(mut self, game_id: GameId) -> Self {
        self.game_id = Some(game_id);
        self
    }

    /// Records which capture backend the session selected.
    #[must_use]
    pub fn with_capture_backend(mut self, capture_backend: CaptureBackend) -> Self {
        self.capture_backend = Some(capture_backend);
        self
    }

    /// Records which video encoder the session selected.
    #[must_use]
    pub fn with_encoder(mut self, encoder: VideoEncoder) -> Self {
        self.encoder = Some(encoder);
        self
    }

    /// Narrows the context to one audio track.
    ///
    /// Audio work touches several tracks, so this is usually applied to a
    /// derived context rather than the session-wide one.
    #[must_use]
    pub fn with_audio_source(mut self, audio_source: AudioSource) -> Self {
        self.audio_source = Some(audio_source);
        self
    }

    /// The session this context describes.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Builds the span carrying these fields.
    ///
    /// The span is created at `INFO`, so it exists whenever the resolved filter
    /// admits `INFO` for this crate — which the default filter does — and the
    /// context is therefore present on `DEBUG` and `TRACE` events too.
    ///
    /// This is the single place the standard field names are written.
    ///
    /// # Why the values are passed in rather than recorded afterwards
    ///
    /// Every field is given its value when the span is created. The obvious
    /// alternative — declare the optional fields [`Empty`] and fill them in
    /// with [`Span::record`] — is what this used to do, and it rendered each
    /// recorded field twice under the subscriber the recorder actually installs
    /// ([issue #185](https://github.com/wildware-uk/clipped/issues/185)).
    ///
    /// `tracing-subscriber` stores a span's formatted fields in a single
    /// `FormattedFields<DefaultFields>` slot in the span's extensions, keyed by
    /// type. [`crate::init`] builds two `fmt` layers over one registry, a log
    /// file and a console, and they share that slot: the first to see the span
    /// writes the creation-time fields, and then *both* append to it every time
    /// a field is recorded. Passing the values up front means nothing is ever
    /// recorded, so there is nothing to append twice — under any subscriber,
    /// including one this crate did not build.
    ///
    /// An absent field is `None`, which `tracing` records as nothing at all, so
    /// it stays absent rather than appearing empty.
    #[must_use]
    pub fn span(&self) -> Span {
        tracing::info_span!(
            "clipped_session",
            session_id = %self.session_id,
            game_id = self.game_id.as_ref().map(tracing::field::display),
            capture_backend = self.capture_backend.map(tracing::field::display),
            encoder = self.encoder.map(tracing::field::display),
            audio_source = self.audio_source.map(tracing::field::display),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_identifiers_are_accepted() {
        assert_eq!(
            SessionId::new("01H8XGJT.4a-b_9").unwrap().as_str(),
            "01H8XGJT.4a-b_9"
        );
        assert_eq!(
            GameId::new("counter-strike-2").unwrap().as_str(),
            "counter-strike-2"
        );
    }

    #[test]
    fn free_text_is_rejected() {
        // Each of these is a realistic slip: a window title, a chat line, a
        // full path, a fragment of a captured file.
        for value in [
            "Counter-Strike 2 - Direct3D 11",
            "hey, my address is 12 Example Street",
            r"C:\Users\alice\Videos\Clipped\match.mkv",
            "line one\nline two",
            "",
        ] {
            assert!(
                SessionId::new(value).is_err(),
                "session_id should have rejected {value:?}"
            );
        }
    }

    #[test]
    fn overlong_values_are_rejected() {
        let value = "a".repeat(IDENTIFIER_LIMIT + 1);
        let error = SessionId::new(value).unwrap_err();
        assert!(matches!(
            error,
            LoggingError::InvalidIdentifier {
                field: "session_id",
                reason: IdentifierRejection::TooLong {
                    limit: 64,
                    length: 65
                },
            }
        ));
    }

    #[test]
    fn rejection_does_not_repeat_the_rejected_value() {
        // The rejected value may be exactly the content that must not reach a
        // log or a console, so the error must not echo it back.
        let secret = "the.password.is.hunter2 said alice";
        let message = SessionId::new(secret).unwrap_err().to_string();
        assert!(!message.contains("hunter2"), "{message}");
        assert!(!message.contains("alice"), "{message}");
    }

    #[test]
    fn display_strings_are_stable_and_greppable() {
        assert_eq!(
            CaptureBackend::WindowsGraphicsCapture.to_string(),
            "windows_graphics_capture"
        );
        assert_eq!(VideoEncoder::Nvenc.to_string(), "nvenc");
        assert_eq!(
            AudioSource::CompatibilityMix.to_string(),
            "compatibility_mix"
        );
    }
}
