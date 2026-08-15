//! The typed values that `record`'s options parse into.
//!
//! Every one of these is parsed and range-checked before the command runs, so
//! that a mistyped resolution or an impossible framerate is reported as a usage
//! error with the offending value in it, rather than reaching a capture session
//! and failing somewhere less explicable (AGENTS.md section 15).
//!
//! The types are deliberately closed sets or validated newtypes rather than
//! strings and bare integers, so that [`crate::config::RecordingConfig`] cannot
//! hold a value the encoder would reject.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use clap::ValueEnum;

/// The smallest capture width or height accepted.
///
/// Below this there is nothing recognisable to watch, and every hardware
/// encoder has a minimum of its own in the same region.
pub const MIN_DIMENSION: u32 = 128;

/// The largest capture width or height accepted.
///
/// 7680 is 8K wide, which is beyond what SPEC.md section 10 asks for (4K) and
/// beyond what any current encoder will accept in one session. The bound is
/// here so that a typo is rejected with an explanation instead of being handed
/// to a driver.
pub const MAX_DIMENSION: u32 = 7680;

/// The slowest framerate accepted.
pub const MIN_FRAMERATE: u32 = 1;

/// The fastest framerate accepted.
///
/// SPEC.md section 10 asks for up to 144 where the hardware allows. The bound
/// is set well above that rather than at it, because the list in the
/// specification is of presets rather than of limits, and refusing a framerate
/// the machine can genuinely reach would be wrong. What it does catch is the
/// missing decimal point: `--framerate 6000` is a mistake, not a request.
pub const MAX_FRAMERATE: u32 = 480;

/// The default framerate when `--framerate` is not given.
pub const DEFAULT_FRAMERATE: u32 = 60;

/// How large the encoded video should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resolution {
    /// Encode at whatever size the capture target is, and follow it if it
    /// changes. This is the default because scaling costs quality and time,
    /// and a game is normally already running at the size the user chose.
    #[default]
    Source,
    /// Scale to a fixed size, whatever the target's own size is.
    Fixed {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
}

impl fmt::Display for Resolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source => formatter.write_str("source"),
            Self::Fixed { width, height } => write!(formatter, "{width}x{height}"),
        }
    }
}

/// Why a `--resolution` value was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionParseError {
    /// The value was empty or whitespace.
    Empty,
    /// The value had no `x` separating two numbers.
    NotWidthByHeight {
        /// What was supplied.
        value: String,
    },
    /// One side of the `x` was not a positive number.
    NotANumber {
        /// `width` or `height`, so the message can say which side is wrong.
        side: &'static str,
        /// What was supplied for that side.
        value: String,
    },
    /// One side was outside [`MIN_DIMENSION`]..=[`MAX_DIMENSION`].
    OutOfRange {
        /// `width` or `height`.
        side: &'static str,
        /// The value that was out of range.
        value: u32,
    },
    /// One side was odd.
    NotEven {
        /// `width` or `height`.
        side: &'static str,
        /// The value that was odd.
        value: u32,
    },
}

impl fmt::Display for ResolutionParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str(
                "a resolution is required, as WIDTHxHEIGHT such as 1920x1080, or `source` \
                 to keep the capture target's own size",
            ),
            Self::NotWidthByHeight { value } => write!(
                formatter,
                "`{value}` is not a resolution: expected WIDTHxHEIGHT such as 1920x1080, \
                 or `source` to keep the capture target's own size"
            ),
            Self::NotANumber { side, value } => write!(
                formatter,
                "`{value}` is not a valid {side}: expected a whole number of pixels, \
                 as in 1920x1080"
            ),
            Self::OutOfRange { side, value } => write!(
                formatter,
                "a {side} of {value} is outside the supported range \
                 {MIN_DIMENSION}-{MAX_DIMENSION}"
            ),
            Self::NotEven { side, value } => write!(
                formatter,
                "a {side} of {value} is odd; H.264, HEVC and AV1 all encode 4:2:0 chroma \
                 at half resolution, so both dimensions must be even. Try {} instead",
                value - 1
            ),
        }
    }
}

impl Error for ResolutionParseError {}

impl FromStr for Resolution {
    type Err = ResolutionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.is_empty() {
            return Err(ResolutionParseError::Empty);
        }
        if value.eq_ignore_ascii_case("source") {
            return Ok(Self::Source);
        }

        let (width, height) =
            value
                .split_once(['x', 'X'])
                .ok_or_else(|| ResolutionParseError::NotWidthByHeight {
                    value: value.to_owned(),
                })?;

        Ok(Self::Fixed {
            width: parse_dimension("width", width)?,
            height: parse_dimension("height", height)?,
        })
    }
}

/// Parses and range-checks one side of a `WIDTHxHEIGHT` value.
fn parse_dimension(side: &'static str, value: &str) -> Result<u32, ResolutionParseError> {
    let value = value.trim();
    let dimension: u32 = value
        .parse()
        .map_err(|_| ResolutionParseError::NotANumber {
            side,
            value: value.to_owned(),
        })?;

    if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&dimension) {
        return Err(ResolutionParseError::OutOfRange {
            side,
            value: dimension,
        });
    }
    if dimension % 2 != 0 {
        return Err(ResolutionParseError::NotEven {
            side,
            value: dimension,
        });
    }
    Ok(dimension)
}

/// How much video a replay buffer keeps, in whole seconds.
///
/// A newtype for the reason [`Framerate`] is one: the bounds are checked where
/// the value enters the program, so that `replay --duration 4h` is a usage
/// error printed before a game is launched rather than a refusal discovered
/// once capture has started (issue #38's third acceptance criterion).
///
/// **There is deliberately no default here.** How much a replay buffer keeps is
/// a *setting* — `replay_window_seconds`, resolved through the same fold as
/// every other one, defaulting to five minutes
/// (`clipped_session::config::DEFAULT_REPLAY_WINDOW`) — and a second default in
/// this file would be a second answer to a question the configuration API
/// already answers (AGENTS.md sections 30 and 55). `--duration` overrides it
/// for one run.
///
/// The bounds are `clipped-replay`'s own — 30 seconds to 30 minutes
/// ([`MINIMUM_WINDOW`](clipped_replay::MINIMUM_WINDOW),
/// [`MAXIMUM_WINDOW`](clipped_replay::MAXIMUM_WINDOW), SPEC.md section 16) —
/// read from that crate rather than restated here, so that a buffer which
/// learns to hold an hour does not have to be told twice (AGENTS.md
/// section 55).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReplayWindow(u32);

impl ReplayWindow {
    /// The shortest window a buffer may be configured with, in seconds.
    #[must_use]
    pub fn minimum_seconds() -> u32 {
        seconds_of(clipped_replay::MINIMUM_WINDOW)
    }

    /// The longest window a buffer may be configured with, in seconds.
    #[must_use]
    pub fn maximum_seconds() -> u32 {
        seconds_of(clipped_replay::MAXIMUM_WINDOW)
    }

    /// How many seconds it keeps.
    #[must_use]
    pub const fn seconds(self) -> u32 {
        self.0
    }

    /// The same, as a duration.
    #[must_use]
    pub const fn duration(self) -> std::time::Duration {
        std::time::Duration::from_secs(self.0 as u64)
    }
}

impl fmt::Display for ReplayWindow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A whole number of seconds, saturating rather than wrapping.
///
/// The bounds it converts are constants of a few hundred seconds, so the
/// saturation is unreachable; it is here because a silent wrap would turn a
/// thirty-minute maximum into a very small one.
fn seconds_of(duration: std::time::Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}

/// Why a `--duration` value was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayWindowParseError {
    /// The value was not a whole number of seconds.
    NotANumber {
        /// What was supplied.
        value: String,
    },
    /// The value was outside what a replay buffer supports.
    OutOfRange {
        /// The value that was out of range.
        value: u32,
    },
}

impl fmt::Display for ReplayWindowParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotANumber { value } => write!(
                formatter,
                "`{value}` is not a replay duration: expected a whole number of seconds, such \
                 as 60"
            ),
            Self::OutOfRange { value } => write!(
                formatter,
                "a replay buffer of {value} seconds is outside the supported range {}-{} \
                 seconds",
                ReplayWindow::minimum_seconds(),
                ReplayWindow::maximum_seconds()
            ),
        }
    }
}

impl Error for ReplayWindowParseError {}

impl FromStr for ReplayWindow {
    type Err = ReplayWindowParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let seconds: u32 = value
            .parse()
            .map_err(|_| ReplayWindowParseError::NotANumber {
                value: value.to_owned(),
            })?;

        if !(ReplayWindow::minimum_seconds()..=ReplayWindow::maximum_seconds()).contains(&seconds) {
            return Err(ReplayWindowParseError::OutOfRange { value: seconds });
        }
        Ok(Self(seconds))
    }
}

/// How much of the buffer one save keeps, in whole seconds.
///
/// Separate from [`ReplayWindow`] because the two are bounded differently and a
/// person gets them wrong in different ways. A *window* has a floor: a buffer
/// shorter than thirty seconds is not worth the machinery. A *save* has none —
/// SPEC.md section 15's shortest offered period is fifteen seconds, and "the
/// last five seconds" is a perfectly sensible thing to ask a sixty-second
/// buffer for.
///
/// What it may not be is longer than the window it is taken from, and that is
/// checked where both values are known ([`crate::config::ReplayConfig`]) rather
/// than here, because a value's own parser cannot see the other argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReplayLength(u32);

impl ReplayLength {
    /// The shortest save accepted.
    ///
    /// One second. Below it there is no clip: a replay is bought at keyframe
    /// granularity, which is two seconds by default, so anything shorter is
    /// rounded up to one segment anyway (`docs/replay-buffer.md`).
    pub const MINIMUM_SECONDS: u32 = 1;

    /// How many seconds it keeps.
    #[must_use]
    pub const fn seconds(self) -> u32 {
        self.0
    }

    /// The same, as a duration.
    #[must_use]
    pub const fn duration(self) -> std::time::Duration {
        std::time::Duration::from_secs(self.0 as u64)
    }
}

impl From<ReplayWindow> for ReplayLength {
    /// The whole window, which is what a save asks for when nothing said
    /// otherwise.
    fn from(window: ReplayWindow) -> Self {
        Self(window.seconds())
    }
}

impl fmt::Display for ReplayLength {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Why a `--save-duration` value was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayLengthParseError {
    /// The value was not a whole number of seconds.
    NotANumber {
        /// What was supplied.
        value: String,
    },
    /// The value was zero, or longer than any buffer may be.
    OutOfRange {
        /// The value that was out of range.
        value: u32,
    },
}

impl fmt::Display for ReplayLengthParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotANumber { value } => write!(
                formatter,
                "`{value}` is not a save duration: expected a whole number of seconds, such as 30"
            ),
            Self::OutOfRange { value } => write!(
                formatter,
                "a save of {value} seconds is outside the supported range {}-{} seconds",
                ReplayLength::MINIMUM_SECONDS,
                ReplayWindow::maximum_seconds()
            ),
        }
    }
}

impl Error for ReplayLengthParseError {}

impl FromStr for ReplayLength {
    type Err = ReplayLengthParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let seconds: u32 = value
            .parse()
            .map_err(|_| ReplayLengthParseError::NotANumber {
                value: value.to_owned(),
            })?;

        if !(Self::MINIMUM_SECONDS..=ReplayWindow::maximum_seconds()).contains(&seconds) {
            return Err(ReplayLengthParseError::OutOfRange { value: seconds });
        }
        Ok(Self(seconds))
    }
}

/// How many frames a second to encode.
///
/// A newtype rather than a `u32` so that the bounds are checked once, where the
/// value enters the program, and every later use can assume them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Framerate(u32);

impl Framerate {
    /// The framerate used when `--framerate` is not given.
    pub const DEFAULT: Self = Self(DEFAULT_FRAMERATE);

    /// Frames per second.
    #[must_use]
    pub const fn frames_per_second(self) -> u32 {
        self.0
    }
}

impl Default for Framerate {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for Framerate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Why a `--framerate` value was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramerateParseError {
    /// The value was not a whole number.
    NotANumber {
        /// What was supplied.
        value: String,
    },
    /// The value was outside [`MIN_FRAMERATE`]..=[`MAX_FRAMERATE`].
    OutOfRange {
        /// The value that was out of range.
        value: u32,
    },
}

impl fmt::Display for FramerateParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotANumber { value } => write!(
                formatter,
                "`{value}` is not a framerate: expected a whole number of frames per second, \
                 such as 60"
            ),
            Self::OutOfRange { value } => write!(
                formatter,
                "a framerate of {value} is outside the supported range \
                 {MIN_FRAMERATE}-{MAX_FRAMERATE} frames per second"
            ),
        }
    }
}

impl Error for FramerateParseError {}

impl FromStr for Framerate {
    type Err = FramerateParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let framerate: u32 = value.parse().map_err(|_| FramerateParseError::NotANumber {
            value: value.to_owned(),
        })?;

        if !(MIN_FRAMERATE..=MAX_FRAMERATE).contains(&framerate) {
            return Err(FramerateParseError::OutOfRange { value: framerate });
        }
        Ok(Self(framerate))
    }
}

/// Which video codec to encode with.
///
/// Which of these a machine can actually produce is a property of its GPU and
/// driver, detected by issue #14. This enumeration is the set the recorder
/// knows how to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum VideoCodec {
    /// Let the recorder choose, preferring the most efficient codec the
    /// selected encoder supports (SPEC.md section 9).
    #[default]
    Auto,
    /// H.264 / AVC. The most compatible, and the largest files.
    H264,
    /// HEVC / H.265.
    Hevc,
    /// AV1.
    Av1,
}

impl fmt::Display for VideoCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Auto => "auto",
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        };
        formatter.write_str(name)
    }
}

/// Which encoder implementation to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum EncoderSelection {
    /// Prefer hardware encoding, choosing the best available (SPEC.md
    /// section 9).
    #[default]
    Auto,
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD Advanced Media Framework.
    Amf,
    /// Intel Quick Sync Video.
    #[value(name = "quicksync")]
    QuickSync,
    /// CPU encoding. A fallback, not an equal option: it costs frames in the
    /// game that is being recorded.
    Software,
}

impl fmt::Display for EncoderSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Auto => "auto",
            Self::Nvenc => "nvenc",
            Self::Amf => "amf",
            Self::QuickSync => "quicksync",
            Self::Software => "software",
        };
        formatter.write_str(name)
    }
}

/// The longest audio device name accepted.
///
/// Windows endpoint names are short human-readable strings; anything longer
/// than this is not a device name and would only ever be a mistake or an
/// attempt to put something else into a log line.
pub const MAX_DEVICE_NAME_LENGTH: usize = 256;

/// Which audio device a track should be captured from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AudioDeviceSelection {
    /// Whatever Windows currently considers the default endpoint, following it
    /// if the user changes it.
    #[default]
    Default,
    /// Do not capture this source at all.
    Disabled,
    /// A device whose name contains this text.
    Named(String),
}

impl AudioDeviceSelection {
    /// The literal-name escape: `name:Default` selects a device actually
    /// called "Default" rather than the system default endpoint.
    const LITERAL_PREFIX: &'static str = "name:";
}

impl fmt::Display for AudioDeviceSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => formatter.write_str("default"),
            Self::Disabled => formatter.write_str("none"),
            Self::Named(name) => write!(formatter, "{}{name}", Self::LITERAL_PREFIX),
        }
    }
}

/// Why an audio device selection was rejected.
///
/// None of these repeat the whole offending value: a device name is user
/// content and the same care applies as in `clipped-logging`'s identifier
/// rejections (docs/logging.md, "Privacy").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioDeviceParseError {
    /// The name was empty or only whitespace.
    Empty,
    /// The name was longer than [`MAX_DEVICE_NAME_LENGTH`].
    TooLong {
        /// How many characters were supplied.
        length: usize,
    },
    /// The name contained a control character, which no audio endpoint name
    /// does and which would corrupt a log line if it were carried through.
    ControlCharacter {
        /// Where the offending character was, counted in characters.
        index: usize,
    },
}

impl fmt::Display for AudioDeviceParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str(
                "an audio device is required: `default` for the system default endpoint, \
                 `none` to capture nothing, or part of a device's name",
            ),
            Self::TooLong { length } => write!(
                formatter,
                "an audio device name may be at most {MAX_DEVICE_NAME_LENGTH} characters; \
                 {length} were supplied"
            ),
            Self::ControlCharacter { index } => write!(
                formatter,
                "an audio device name may not contain control characters; \
                 there is one at character {index}"
            ),
        }
    }
}

impl Error for AudioDeviceParseError {}

impl FromStr for AudioDeviceSelection {
    type Err = AudioDeviceParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(literal) = value.strip_prefix(Self::LITERAL_PREFIX) {
            return Ok(Self::Named(validate_device_name(literal)?));
        }

        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("default") {
            return Ok(Self::Default);
        }
        if trimmed.eq_ignore_ascii_case("none") {
            return Ok(Self::Disabled);
        }
        Ok(Self::Named(validate_device_name(value)?))
    }
}

/// Checks that a device name is something an audio endpoint could be called.
fn validate_device_name(name: &str) -> Result<String, AudioDeviceParseError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AudioDeviceParseError::Empty);
    }
    let length = name.chars().count();
    if length > MAX_DEVICE_NAME_LENGTH {
        return Err(AudioDeviceParseError::TooLong { length });
    }
    if let Some((index, _)) = name.char_indices().find(|(_, c)| c.is_control()) {
        return Err(AudioDeviceParseError::ControlCharacter { index });
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_parses_width_by_height() {
        assert_eq!(
            "1920x1080".parse::<Resolution>(),
            Ok(Resolution::Fixed {
                width: 1920,
                height: 1080
            })
        );
    }

    #[test]
    fn resolution_accepts_a_capital_separator_and_surrounding_space() {
        assert_eq!(
            " 2560X1440 ".parse::<Resolution>(),
            Ok(Resolution::Fixed {
                width: 2560,
                height: 1440
            })
        );
    }

    #[test]
    fn resolution_accepts_source_in_any_case() {
        assert_eq!("source".parse::<Resolution>(), Ok(Resolution::Source));
        assert_eq!("Source".parse::<Resolution>(), Ok(Resolution::Source));
    }

    #[test]
    fn resolution_round_trips_through_display() {
        for value in ["source", "1920x1080", "3840x2160"] {
            let parsed: Resolution = value.parse().expect("valid resolution");
            assert_eq!(parsed.to_string(), value);
        }
    }

    #[test]
    fn resolution_without_a_separator_names_the_expected_form() {
        let error = "1080".parse::<Resolution>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "`1080` is not a resolution: expected WIDTHxHEIGHT such as 1920x1080, \
             or `source` to keep the capture target's own size"
        );
    }

    #[test]
    fn resolution_with_a_non_numeric_side_says_which_side() {
        let error = "1920xtall".parse::<Resolution>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "`tall` is not a valid height: expected a whole number of pixels, as in 1920x1080"
        );
    }

    #[test]
    fn resolution_rejects_dimensions_outside_the_supported_range() {
        let error = "64x64".parse::<Resolution>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "a width of 64 is outside the supported range 128-7680"
        );

        let error = "16000x9000".parse::<Resolution>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "a width of 16000 is outside the supported range 128-7680"
        );
    }

    #[test]
    fn resolution_rejects_odd_dimensions_and_suggests_an_even_one() {
        let error = "1920x1081".parse::<Resolution>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "a height of 1081 is odd; H.264, HEVC and AV1 all encode 4:2:0 chroma at half \
             resolution, so both dimensions must be even. Try 1080 instead"
        );
    }

    #[test]
    fn resolution_rejects_an_empty_value() {
        assert_eq!(
            "   ".parse::<Resolution>(),
            Err(ResolutionParseError::Empty)
        );
    }

    #[test]
    fn resolution_rejects_a_negative_dimension_without_panicking() {
        let error = "-1920x1080".parse::<Resolution>().unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("`-1920` is not a valid width"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn framerate_parses_and_bounds_check() {
        assert_eq!(
            "144".parse::<Framerate>().map(Framerate::frames_per_second),
            Ok(144)
        );
        assert_eq!(Framerate::default().frames_per_second(), DEFAULT_FRAMERATE);
    }

    #[test]
    fn framerate_outside_the_range_says_what_the_range_is() {
        let error = "6000".parse::<Framerate>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "a framerate of 6000 is outside the supported range 1-480 frames per second"
        );

        let error = "0".parse::<Framerate>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "a framerate of 0 is outside the supported range 1-480 frames per second"
        );
    }

    #[test]
    fn framerate_rejects_a_fraction_rather_than_truncating_it() {
        let error = "59.94".parse::<Framerate>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "`59.94` is not a framerate: expected a whole number of frames per second, such as 60"
        );
    }

    #[test]
    fn codec_and_encoder_display_as_they_are_typed() {
        assert_eq!(VideoCodec::default().to_string(), "auto");
        assert_eq!(VideoCodec::H264.to_string(), "h264");
        assert_eq!(EncoderSelection::default().to_string(), "auto");
        assert_eq!(EncoderSelection::QuickSync.to_string(), "quicksync");
    }

    #[test]
    fn codec_and_encoder_accept_every_name_shown_in_help() {
        for codec in VideoCodec::value_variants() {
            let name = codec.to_string();
            assert_eq!(
                VideoCodec::from_str(&name, false),
                Ok(*codec),
                "`{name}` is displayed but not accepted"
            );
        }
        for encoder in EncoderSelection::value_variants() {
            let name = encoder.to_string();
            assert_eq!(
                EncoderSelection::from_str(&name, false),
                Ok(*encoder),
                "`{name}` is displayed but not accepted"
            );
        }
    }

    #[test]
    fn audio_device_recognises_the_two_reserved_words() {
        assert_eq!(
            "default".parse::<AudioDeviceSelection>(),
            Ok(AudioDeviceSelection::Default)
        );
        assert_eq!(
            "None".parse::<AudioDeviceSelection>(),
            Ok(AudioDeviceSelection::Disabled)
        );
    }

    #[test]
    fn audio_device_takes_anything_else_as_a_name() {
        assert_eq!(
            "Headset Microphone".parse::<AudioDeviceSelection>(),
            Ok(AudioDeviceSelection::Named("Headset Microphone".to_owned()))
        );
    }

    #[test]
    fn audio_device_escape_selects_a_device_called_default() {
        assert_eq!(
            "name:default".parse::<AudioDeviceSelection>(),
            Ok(AudioDeviceSelection::Named("default".to_owned()))
        );
    }

    #[test]
    fn audio_device_rejects_an_empty_name() {
        let error = "  ".parse::<AudioDeviceSelection>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "an audio device is required: `default` for the system default endpoint, \
             `none` to capture nothing, or part of a device's name"
        );
    }

    #[test]
    fn audio_device_rejects_a_name_that_is_too_long() {
        let name = "a".repeat(MAX_DEVICE_NAME_LENGTH + 1);
        let error = name.parse::<AudioDeviceSelection>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "an audio device name may be at most 256 characters; 257 were supplied"
        );
    }

    #[test]
    fn audio_device_rejects_control_characters_without_repeating_the_value() {
        let error = "Head\nset".parse::<AudioDeviceSelection>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "an audio device name may not contain control characters; \
             there is one at character 4"
        );
        assert!(
            !error.to_string().contains("Head"),
            "the rejection must not repeat the value it rejected"
        );
    }
}
