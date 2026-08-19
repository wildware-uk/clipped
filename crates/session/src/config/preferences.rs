//! One layer of settings, and what a fold of the layers answers.
//!
//! Every field here is an `Option`, and that is the point. `None` means *this
//! layer says nothing*, which is not the same as a layer setting the value the
//! layer below already had: a game whose frame rate is unset follows the global
//! frame rate when the user later changes it, and a game explicitly set to 60
//! stays at 60. Storing the effective value in the per-game layer would collapse
//! those two into one and quietly break the second half of AGENTS.md section
//! 30's worked example.
//!
//! The same type is used for the global layer and for each game's, so
//! inheritance is a fold over a list of layers rather than a special case per
//! setting. Hotkeys are the one thing that is not here; `super::hotkeys` says
//! why.
//!
//! # Which settings exist
//!
//! Exactly the ones this build can be told about today: the capture target, the
//! resolution, the frame rate, the codec, the encoder and the two audio
//! selections that `clipped-recorder record` accepts (`docs/recorder-cli.md`),
//! plus the replay window `clipped_replay::ReplayConfig` is built from
//! ([issue #35](https://github.com/wildware-uk/clipped/issues/35)). SPEC.md
//! section 31 lists more — capture mode, bitrate, event types, auto-clipping,
//! storage behaviour, HDR — and none of them are here, because a setting for a
//! subsystem that does not exist is a control that silently does nothing
//! (AGENTS.md section 27). Each arrives with the subsystem that reads it.

use core::fmt;
use core::time::Duration;

use clipped_replay::{MAXIMUM_WINDOW, MINIMUM_WINDOW};

use crate::config::error::SettingError;
use crate::config::value::{Resolved, Scope, SettingKey, SettingSource};
use crate::settings::{
    AudioSourceSetting, CodecPreference, EncoderPreference, RecordingSettings, ResolutionSetting,
    UnavailableChoice, DEFAULT_FRAMERATE,
};

/// The lowest frame rate a recording may be configured for.
///
/// The same bound `clipped-recorder record --framerate` enforces. A settings
/// file and a command line that disagreed about what is acceptable would be two
/// answers to one question.
pub const MINIMUM_FRAMERATE: u32 = 1;

/// The highest frame rate a recording may be configured for.
pub const MAXIMUM_FRAMERATE: u32 = 480;

/// The smallest side a fixed resolution may name.
pub const MINIMUM_DIMENSION: u32 = 128;

/// The largest side a fixed resolution may name.
pub const MAXIMUM_DIMENSION: u32 = 7680;

/// The longest an audio device name may be.
///
/// Windows device names are far shorter than this; the limit exists so that a
/// corrupted or hostile settings file cannot put an unbounded string into a log
/// line or a UI label.
pub const MAXIMUM_DEVICE_NAME: usize = 256;

/// How much video the replay buffer keeps when nobody has said.
///
/// Five minutes: one of the windows SPEC.md section 16 names, in the middle of
/// the list, and long enough that the thing a player wants to save is still in
/// the buffer by the time they reach for the hotkey. What it costs is
/// arithmetic rather than a guess —
/// [`clipped_replay::ReplayConfig::expected_bytes`] is the figure, and it is
/// about 700 MB at the 18.66 Mbit/s a 1440p60 recording uses.
///
/// That figure is what the window would occupy if it were all held at once;
/// a buffer spills, so what it really costs a machine is a continuous write of
/// about the same size (`docs/replay-buffer.md`). Whoever does not want to pay
/// it sets [`REPLAY_WINDOW_OFF`].
pub const DEFAULT_REPLAY_WINDOW: Duration = Duration::from_secs(5 * 60);

/// The window that means *keep no replay buffer at all*.
///
/// Zero, and zero is the reading of the number rather than a sentinel to be
/// learned: `replay_window_seconds` is how many seconds of history a recording
/// keeps, and none is a number of seconds. It is spelled as a number because
/// the key holds a number — admitting `"none"` here, as `microphone` and
/// `system_audio` do, would make one key's *type* depend on its value, and
/// those two are text keys whose entire value space is text (a device may
/// genuinely be called "none", which is why they need a word for it and this
/// does not).
///
/// **It is not "unset".** A layer that says nothing about the replay window is
/// a layer with no `replay_window_seconds` key at all — [`Preferences`] holds
/// an `Option<Duration>` and the writer omits what is [`None`] — so there is no
/// in-band spelling of "unset" for zero to be confused with. Setting zero and
/// clearing the setting stay as different here as they are for every other key:
/// the first is an answer and the second is an inherit.
///
/// What declining is worth is the other side of the same arithmetic. A buffer
/// spills to disk (`docs/replay-buffer.md`), so it writes continuously at the
/// recording's own bitrate for as long as the recording runs — 208 MB for a
/// thirty-minute window — and a recording that keeps none writes none of it.
///
/// It is deliberately *outside* [`MINIMUM_WINDOW`] rather than below it: no
/// buffer is not a very short buffer. A recording that declines one holds no
/// `ReplayRecording` at all, which is the absence every caller already spells
/// `Option` (`crate::replay`), and is what lets a window tell "this recording
/// keeps no buffer" from "this buffer has nothing in it yet".
pub const REPLAY_WINDOW_OFF: Duration = Duration::ZERO;

/// Whether the game's own window is captured, or the display it is on.
///
/// Both are reachable rather than aspirational: `clipped-capture`'s two
/// backends each declare `captures_monitors()`, and
/// `clipped_windows::monitor_for_window` is how the display a game's window is
/// on is found. The default is the window, which is what `watch` records today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureTargetSetting {
    /// The game's own window.
    #[default]
    GameWindow,
    /// The whole display the game's window is on.
    Display,
}

impl CaptureTargetSetting {
    /// Every value, for a settings screen to list.
    pub const ALL: [Self; 2] = [Self::GameWindow, Self::Display];

    /// The token this is written as in the settings file.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::GameWindow => "game-window",
            Self::Display => "display",
        }
    }

    /// The value a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// Which audio endpoint to record.
///
/// The three values `--microphone` and `--system-audio` accept
/// (`docs/recorder-cli.md`). A name is matched against the device list at the
/// moment a recording starts, so a device that is unplugged is a failure the
/// session reports rather than a settings file that has become invalid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AudioDeviceSetting {
    /// Whatever Windows currently considers the default endpoint.
    #[default]
    Default,
    /// Record nothing from this source.
    Disabled,
    /// The endpoint whose name contains this text.
    Named(String),
}

impl AudioDeviceSetting {
    /// A named device, having checked the name.
    ///
    /// # Errors
    ///
    /// [`SettingError::OutOfRange`] when the name is blank, longer than
    /// [`MAXIMUM_DEVICE_NAME`], or contains a control character. `key` names
    /// which of the two audio settings is being set, so the message says
    /// `microphone` rather than "a device".
    pub fn named(key: SettingKey, name: impl Into<String>) -> Result<Self, SettingError> {
        let name = name.into();
        Self::check_name(key, &name)?;
        Ok(Self::Named(name))
    }

    /// Whether this value is one the settings file can hold and read back.
    ///
    /// [`Self::Named`] is a public variant, so a caller can build one without
    /// going through [`Self::named`]; this is what the setters check so that
    /// the two routes cannot disagree.
    ///
    /// # Errors
    ///
    /// As [`Self::named`].
    fn check(&self, key: SettingKey) -> Result<(), SettingError> {
        match self {
            Self::Default | Self::Disabled => Ok(()),
            Self::Named(name) => Self::check_name(key, name),
        }
    }

    fn check_name(key: SettingKey, name: &str) -> Result<(), SettingError> {
        if name.trim().is_empty() {
            return Err(SettingError::OutOfRange {
                key,
                value: format!("{name:?}"),
                accepted: "a device name, \"default\" or \"none\"".to_owned(),
            });
        }
        if name.chars().count() > MAXIMUM_DEVICE_NAME {
            return Err(SettingError::OutOfRange {
                key,
                value: format!("a name of {} characters", name.chars().count()),
                accepted: format!("at most {MAXIMUM_DEVICE_NAME} characters"),
            });
        }
        if name.chars().any(char::is_control) {
            return Err(SettingError::OutOfRange {
                key,
                value: format!("{name:?}"),
                accepted: "a device name without control characters".to_owned(),
            });
        }
        Ok(())
    }
}

/// One layer of settings: the global layer, or one game's.
///
/// Every field is optional and every setter validates, so a `Preferences` that
/// exists is a `Preferences` whose values are in range. That invariant is what
/// lets [`super::ConfigurationStore`] promise that a rejected file leaves the
/// previous configuration standing — there is no half-applied state to unwind.
///
/// "In range" is defined by what the settings file can carry, not only by what
/// the type can hold: every value a setter accepts is one
/// `crate::config::document` can write and read back unchanged. That is why a
/// blank device name and a fractional replay window are refused here — each
/// would produce a file this same build could not read, or could read only as
/// something other than what was set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Preferences {
    capture_target: Option<CaptureTargetSetting>,
    resolution: Option<ResolutionSetting>,
    framerate: Option<u32>,
    codec: Option<CodecPreference>,
    encoder: Option<EncoderPreference>,
    microphone: Option<AudioDeviceSetting>,
    system_audio: Option<AudioDeviceSetting>,
    replay_window: Option<Duration>,
    /// Keys this build does not recognise, kept exactly as they were read.
    ///
    /// A newer Clipped writing a setting this one has never heard of must not
    /// cost the user that setting when this one saves the file (AGENTS.md
    /// section 56). They are carried through untouched and written back out.
    unknown: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Preferences {
    /// A layer that says nothing about any setting.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether this layer says nothing at all, unknown keys included.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.unknown.is_empty() && SettingKey::ALL.iter().all(|key| !self.is_set(*key))
    }

    /// Which of the game's window or its display to capture, if this layer
    /// says.
    #[must_use]
    pub const fn capture_target(&self) -> Option<CaptureTargetSetting> {
        self.capture_target
    }

    /// Sets, or with `None` clears, the capture target.
    pub fn set_capture_target(&mut self, value: Option<CaptureTargetSetting>) {
        self.capture_target = value;
    }

    /// The size to encode at, if this layer says.
    #[must_use]
    pub const fn resolution(&self) -> Option<ResolutionSetting> {
        self.resolution
    }

    /// Sets, or with `None` clears, the resolution.
    ///
    /// # Errors
    ///
    /// [`SettingError::OutOfRange`] when a fixed size names a side outside
    /// [`MINIMUM_DIMENSION`]..=[`MAXIMUM_DIMENSION`] or an odd one. Odd sides
    /// are refused because every codec here encodes 4:2:0 chroma, which has no
    /// representation for half a chroma sample.
    pub fn set_resolution(&mut self, value: Option<ResolutionSetting>) -> Result<(), SettingError> {
        if let Some(ResolutionSetting::Fixed { width, height }) = value {
            check_dimension("width", width)?;
            check_dimension("height", height)?;
        }
        self.resolution = value;
        Ok(())
    }

    /// The frame rate ceiling, if this layer says.
    #[must_use]
    pub const fn framerate(&self) -> Option<u32> {
        self.framerate
    }

    /// Sets, or with `None` clears, the frame rate ceiling.
    ///
    /// # Errors
    ///
    /// [`SettingError::OutOfRange`] outside
    /// [`MINIMUM_FRAMERATE`]..=[`MAXIMUM_FRAMERATE`].
    pub fn set_framerate(&mut self, value: Option<u32>) -> Result<(), SettingError> {
        if let Some(framerate) = value {
            if !(MINIMUM_FRAMERATE..=MAXIMUM_FRAMERATE).contains(&framerate) {
                return Err(SettingError::OutOfRange {
                    key: SettingKey::Framerate,
                    value: framerate.to_string(),
                    accepted: format!("{MINIMUM_FRAMERATE}-{MAXIMUM_FRAMERATE} frames per second"),
                });
            }
        }
        self.framerate = value;
        Ok(())
    }

    /// The codec to produce, if this layer says.
    #[must_use]
    pub const fn codec(&self) -> Option<CodecPreference> {
        self.codec
    }

    /// Sets, or with `None` clears, the codec.
    pub fn set_codec(&mut self, value: Option<CodecPreference>) {
        self.codec = value;
    }

    /// The encoder family to encode with, if this layer says.
    #[must_use]
    pub const fn encoder(&self) -> Option<EncoderPreference> {
        self.encoder
    }

    /// Sets, or with `None` clears, the encoder family.
    pub fn set_encoder(&mut self, value: Option<EncoderPreference>) {
        self.encoder = value;
    }

    /// Which microphone to record, if this layer says.
    #[must_use]
    pub const fn microphone(&self) -> Option<&AudioDeviceSetting> {
        self.microphone.as_ref()
    }

    /// Sets, or with `None` clears, the microphone selection.
    ///
    /// # Errors
    ///
    /// [`SettingError::OutOfRange`] for a device name
    /// [`AudioDeviceSetting::named`] would refuse. The variant is public, so
    /// this is the check that keeps the invariant true whichever way the value
    /// was built.
    pub fn set_microphone(
        &mut self,
        value: Option<AudioDeviceSetting>,
    ) -> Result<(), SettingError> {
        if let Some(device) = &value {
            device.check(SettingKey::Microphone)?;
        }
        self.microphone = value;
        Ok(())
    }

    /// Which system-audio endpoint to record, if this layer says.
    #[must_use]
    pub const fn system_audio(&self) -> Option<&AudioDeviceSetting> {
        self.system_audio.as_ref()
    }

    /// Sets, or with `None` clears, the system-audio selection.
    ///
    /// # Errors
    ///
    /// As [`Self::set_microphone`], with the message naming `system_audio`.
    pub fn set_system_audio(
        &mut self,
        value: Option<AudioDeviceSetting>,
    ) -> Result<(), SettingError> {
        if let Some(device) = &value {
            device.check(SettingKey::SystemAudio)?;
        }
        self.system_audio = value;
        Ok(())
    }

    /// How much video the replay buffer keeps, if this layer says.
    #[must_use]
    pub const fn replay_window(&self) -> Option<Duration> {
        self.replay_window
    }

    /// Sets, or with `None` clears, the replay window.
    ///
    /// [`REPLAY_WINDOW_OFF`] is accepted alongside the buffer's own range, and
    /// is the whole of how somebody declines a replay buffer. Without it the
    /// nearest thing to "no" is [`MINIMUM_WINDOW`], which is a buffer that
    /// still spills — so every recording the desktop window starts would keep
    /// one and nobody could say otherwise
    /// ([issue #539](https://github.com/wildware-uk/clipped/issues/539)).
    ///
    /// # Errors
    ///
    /// [`SettingError::OutOfRange`] outside
    /// [`clipped_replay::MINIMUM_WINDOW`]..=[`clipped_replay::MAXIMUM_WINDOW`],
    /// which is the range the buffer itself accepts. Validating here as well
    /// means the refusal reaches the user at the moment they set it rather than
    /// at the moment a game launches.
    ///
    /// A window that is not a whole number of seconds is refused for a
    /// different reason: `replay_window_seconds` is whole seconds, so half a
    /// second would be dropped by the writer and the setting would come back
    /// from the file as something other than what was set.
    pub fn set_replay_window(&mut self, value: Option<Duration>) -> Result<(), SettingError> {
        if let Some(window) = value {
            if window != REPLAY_WINDOW_OFF && !(MINIMUM_WINDOW..=MAXIMUM_WINDOW).contains(&window) {
                return Err(SettingError::OutOfRange {
                    key: SettingKey::ReplayWindow,
                    value: format!("{} seconds", window.as_secs()),
                    // The same sentence the settings screen puts beside the
                    // field, rather than a second copy of the range that would
                    // stop naming the off value the moment one was added
                    // (AGENTS.md section 55).
                    accepted: crate::config::document::accepted_for(SettingKey::ReplayWindow),
                });
            }
            if window.subsec_nanos() != 0 {
                return Err(SettingError::OutOfRange {
                    key: SettingKey::ReplayWindow,
                    value: format!("{} seconds", window.as_secs_f64()),
                    accepted: "a whole number of seconds, which is what the settings file holds"
                        .to_owned(),
                });
            }
        }
        self.replay_window = value;
        Ok(())
    }

    /// Sets `key` from the text the settings file spells it with, or clears it.
    ///
    /// The one setter a caller that does not know a setting's type can use —
    /// a settings screen sending `("framerate", "120")` over the control
    /// protocol, which is the shape a form has. `None` clears the setting, so
    /// that Reset and "set to the default" stay different things
    /// ([`Resolved::is_overridden`](crate::config::Resolved::is_overridden)).
    ///
    /// It parses with the file reader's own parsers, so a value refused here is
    /// exactly a value the same text in [`FILE_NAME`](crate::config::FILE_NAME)
    /// would be refused for, with the same message.
    ///
    /// # Errors
    ///
    /// [`SettingError`] naming the setting, the value and what would have been
    /// accepted.
    pub fn set_written(
        &mut self,
        key: SettingKey,
        value: Option<&str>,
    ) -> Result<(), SettingError> {
        match value {
            None => {
                self.clear(key);
                Ok(())
            }
            Some(token) => crate::config::document::set_written_setting(self, key, token),
        }
    }

    /// Whether this layer sets `key` at all.
    ///
    /// The question a Reset control asks, without needing to know the type
    /// behind each setting.
    #[must_use]
    pub fn is_set(&self, key: SettingKey) -> bool {
        match key {
            SettingKey::CaptureTarget => self.capture_target.is_some(),
            SettingKey::Resolution => self.resolution.is_some(),
            SettingKey::Framerate => self.framerate.is_some(),
            SettingKey::Codec => self.codec.is_some(),
            SettingKey::Encoder => self.encoder.is_some(),
            SettingKey::Microphone => self.microphone.is_some(),
            SettingKey::SystemAudio => self.system_audio.is_some(),
            SettingKey::ReplayWindow => self.replay_window.is_some(),
        }
    }

    /// Clears `key`, so that this layer inherits it again.
    pub fn clear(&mut self, key: SettingKey) {
        match key {
            SettingKey::CaptureTarget => self.capture_target = None,
            SettingKey::Resolution => self.resolution = None,
            SettingKey::Framerate => self.framerate = None,
            SettingKey::Codec => self.codec = None,
            SettingKey::Encoder => self.encoder = None,
            SettingKey::Microphone => self.microphone = None,
            SettingKey::SystemAudio => self.system_audio = None,
            SettingKey::ReplayWindow => self.replay_window = None,
        }
    }

    /// The keys from a newer build that were read and are being kept.
    pub fn unrecognised_keys(&self) -> impl Iterator<Item = &str> {
        self.unknown.keys().map(String::as_str)
    }

    /// Records a key this build does not understand.
    pub(crate) fn keep_unrecognised(&mut self, key: String, value: serde_json::Value) {
        self.unknown.insert(key, value);
    }

    /// The kept keys, for writing back out.
    pub(crate) const fn unrecognised(
        &self,
    ) -> &std::collections::BTreeMap<String, serde_json::Value> {
        &self.unknown
    }

    /// Takes on the unrecognised keys of the layer this one is replacing.
    ///
    /// A caller cannot carry forward a key it has never heard of, so it is not
    /// asked to: a settings screen that builds a fresh [`Preferences`] and
    /// hands it to [`Configuration::set_global`](crate::config::Configuration::set_global)
    /// keeps the newer build's settings anyway. Keys the caller *does* hold
    /// win, so a configuration that was read and edited is not overwritten by
    /// its own older copy.
    pub(crate) fn adopt_unrecognised_from(&mut self, previous: &Self) {
        for (key, value) in &previous.unknown {
            self.unknown
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
}

fn check_dimension(side: &'static str, value: u32) -> Result<(), SettingError> {
    if !(MINIMUM_DIMENSION..=MAXIMUM_DIMENSION).contains(&value) {
        return Err(SettingError::OutOfRange {
            key: SettingKey::Resolution,
            value: format!("a {side} of {value}"),
            accepted: format!("{MINIMUM_DIMENSION}-{MAXIMUM_DIMENSION} pixels"),
        });
    }
    if value % 2 != 0 {
        return Err(SettingError::OutOfRange {
            key: SettingKey::Resolution,
            value: format!("a {side} of {value}"),
            accepted: "an even number of pixels, which 4:2:0 chroma requires".to_owned(),
        });
    }
    Ok(())
}

/// What every setting resolves to for one scope, and where each answer came
/// from.
///
/// This is the shape the settings screen renders
/// ([issue #51](https://github.com/wildware-uk/clipped/issues/51)) and the
/// shape applying settings to a recording reads
/// ([issue #61](https://github.com/wildware-uk/clipped/issues/61)). Both get
/// the same answer from the same fold, which is what "a single resolution
/// point" means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSettings {
    scope: Scope,
    capture_target: Resolved<CaptureTargetSetting>,
    resolution: Resolved<ResolutionSetting>,
    framerate: Resolved<u32>,
    codec: Resolved<CodecPreference>,
    encoder: Resolved<EncoderPreference>,
    microphone: Resolved<AudioDeviceSetting>,
    system_audio: Resolved<AudioDeviceSetting>,
    replay_window: Resolved<Duration>,
}

impl ResolvedSettings {
    /// Folds the default, the global layer and — for a game scope — that game's
    /// layer, in that order.
    ///
    /// This function is the single resolution point AGENTS.md section 30 asks
    /// for. Every consumer of a setting reaches it through here, so that "does
    /// this game override the frame rate" has one answer rather than one per
    /// call site.
    pub(crate) fn fold(scope: Scope, global: &Preferences, game: Option<&Preferences>) -> Self {
        // A game's layer only applies to a game's scope. Resolving the global
        // page must never show a value a game set, or the user would edit the
        // global settings and see a game's number change under their hands.
        let game = game.filter(|_| scope.game().is_some());
        let layers = Layers {
            layer: scope.layer(),
            global,
            game,
        };

        Self {
            capture_target: layers
                .pick(CaptureTargetSetting::default(), Preferences::capture_target),
            resolution: layers.pick(ResolutionSetting::default(), Preferences::resolution),
            framerate: layers.pick(DEFAULT_FRAMERATE, Preferences::framerate),
            codec: layers.pick(CodecPreference::default(), Preferences::codec),
            encoder: layers.pick(EncoderPreference::default(), Preferences::encoder),
            microphone: layers.pick(AudioDeviceSetting::default(), |preferences| {
                preferences.microphone().cloned()
            }),
            system_audio: layers.pick(AudioDeviceSetting::default(), |preferences| {
                preferences.system_audio().cloned()
            }),
            replay_window: layers.pick(DEFAULT_REPLAY_WINDOW, Preferences::replay_window),
            scope,
        }
    }

    /// Which layer this was resolved for.
    #[must_use]
    pub const fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Whether the game's window or its display is captured.
    #[must_use]
    pub const fn capture_target(&self) -> &Resolved<CaptureTargetSetting> {
        &self.capture_target
    }

    /// The size to encode at.
    #[must_use]
    pub const fn resolution(&self) -> &Resolved<ResolutionSetting> {
        &self.resolution
    }

    /// The frame rate ceiling.
    #[must_use]
    pub const fn framerate(&self) -> &Resolved<u32> {
        &self.framerate
    }

    /// The codec to produce.
    #[must_use]
    pub const fn codec(&self) -> &Resolved<CodecPreference> {
        &self.codec
    }

    /// The encoder family to encode with.
    #[must_use]
    pub const fn encoder(&self) -> &Resolved<EncoderPreference> {
        &self.encoder
    }

    /// Which microphone to record.
    #[must_use]
    pub const fn microphone(&self) -> &Resolved<AudioDeviceSetting> {
        &self.microphone
    }

    /// Which system-audio endpoint to record.
    #[must_use]
    pub const fn system_audio(&self) -> &Resolved<AudioDeviceSetting> {
        &self.system_audio
    }

    /// How much video the replay buffer keeps.
    #[must_use]
    pub const fn replay_window(&self) -> &Resolved<Duration> {
        &self.replay_window
    }

    /// The buffer these settings ask for, or [`None`] when they ask for none.
    ///
    /// The one place [`REPLAY_WINDOW_OFF`] becomes the absence the rest of the
    /// workspace already spells `Option`, so that "has the user declined the
    /// buffer" is not a comparison each caller writes for itself and gets
    /// subtly different (AGENTS.md section 55). `clipped-recorder serve` asks
    /// it for every `start_recording` that sent `replay` without a length, and
    /// `clipped-recorder replay` asks it before it opens anything.
    ///
    /// [`Self::replay_window`] is still the accessor for the *setting* — what a
    /// settings screen shows, and what [`Self::source_of`] reports a layer for.
    /// This is the accessor for what to build.
    #[must_use]
    pub fn replay_buffer_window(&self) -> Option<Duration> {
        Some(self.replay_window.get()).filter(|window| *window != REPLAY_WINDOW_OFF)
    }

    /// Where `key`'s answer came from, without naming its type.
    ///
    /// The settings screen renders each setting with a widget of its own, but
    /// the "inherited from global" badge and the Reset control are the same for
    /// all of them, and this is what drives both.
    #[must_use]
    pub const fn source_of(&self, key: SettingKey) -> SettingSource {
        match key {
            SettingKey::CaptureTarget => self.capture_target.source(),
            SettingKey::Resolution => self.resolution.source(),
            SettingKey::Framerate => self.framerate.source(),
            SettingKey::Codec => self.codec.source(),
            SettingKey::Encoder => self.encoder.source(),
            SettingKey::Microphone => self.microphone.source(),
            SettingKey::SystemAudio => self.system_audio.source(),
            SettingKey::ReplayWindow => self.replay_window.source(),
        }
    }

    /// Whether this scope set `key` itself, rather than inheriting it.
    #[must_use]
    pub fn is_overridden(&self, key: SettingKey) -> bool {
        self.source_of(key) == self.scope.layer()
    }

    /// `key`'s effective value, spelled the way the settings file spells it.
    ///
    /// The same words `--codec hevc` and `"codec": "hevc"` use, so that a log
    /// line, a session's record and the file a user edited all say the same
    /// thing about the same setting (`crate::config::document`).
    #[must_use]
    pub fn written_value(&self, key: SettingKey) -> String {
        crate::config::document::written_setting(self, key)
    }

    /// These settings, applied to a recording.
    ///
    /// This is the whole of "the settings that apply are that game's": a
    /// caller resolves once, for one game, and hands the answer to the
    /// recording it is about to start. Nothing re-reads the configuration
    /// afterwards, so a settings change during a recording reaches the *next*
    /// one — a value changing under a running encoder is a different feature
    /// and not this one.
    ///
    /// What `recording` already carries and this does not touch: what to
    /// capture and where to write it, both of which the caller has resolved
    /// against the machine, and the disk guard.
    ///
    /// Two settings are deliberately not applied here because they are not
    /// fields of a [`RecordingSettings`]:
    ///
    /// - [`capture_target`](Self::capture_target) decides *which handle* the
    ///   caller resolves — the game's window, or the display it is on — so it
    ///   is read before a recording's target exists.
    /// - [`replay_window`](Self::replay_window) becomes a
    ///   `clipped_replay::ReplayConfig`, which is built where a recording opens
    ///   a replay buffer.
    ///
    /// The recording is given [`UnavailableChoice::Substitute`], which is the
    /// difference between a setting and a command-line flag: a configured
    /// encoder this machine no longer has substitutes and logs rather than
    /// failing a recording nobody was watching (see
    /// [`UnavailableChoice`]).
    #[must_use]
    pub fn apply_to(&self, recording: RecordingSettings) -> RecordingSettings {
        recording
            .with_resolution(self.resolution.get())
            .with_framerate(self.framerate.get())
            .with_codec(self.codec.get())
            .with_encoder(self.encoder.get())
            .with_microphone(audio_source(self.microphone.value()))
            .with_system_audio(audio_source(self.system_audio.value()))
            .with_unavailable_choice(UnavailableChoice::Substitute)
    }

    /// These settings, applied to a recording that already carries answers of
    /// its own.
    ///
    /// [`Self::apply_to`] is for a caller whose recording carries nothing but a
    /// target and an output, so that every setting has to come from somewhere
    /// and the shipped default is where. This is for a caller that was already
    /// told what to record with — `clipped-recorder watch`, whose command line
    /// says a resolution, a frame rate, a codec, an encoder and two audio
    /// selections before any game has launched — and it applies **only what a
    /// user configured**: a setting no layer above the default speaks to leaves
    /// what the recording already asked for.
    ///
    /// The difference is not a nicety. `apply_to` would replace
    /// `watch --framerate 144` with the 60 Clipped ships with, on every machine
    /// with no settings file for it, and `--microphone none` with the default
    /// microphone — a flag that parses and then does nothing, which is the
    /// defect AGENTS.md section 27 is about, and in the microphone's case one
    /// that records a device the user asked not to record.
    ///
    /// A setting a user *did* configure wins over the same setting on the
    /// command line, which is the layering the settings screen assumes
    /// ([issue #61](https://github.com/wildware-uk/clipped/issues/61) records
    /// the question of whether a flag typed at that moment should beat it).
    ///
    /// [`UnavailableChoice::Substitute`] is given only when the resolution or
    /// the encoder is one of the configured settings, because those are the two
    /// the choice governs: a recording still encoding at what a command line
    /// named keeps that command line's refusal (`docs/configuration.md`, "What
    /// a stale setting does, and why it is not what a flag does").
    #[must_use]
    pub fn apply_configured_to(&self, recording: RecordingSettings) -> RecordingSettings {
        let mut recording = recording;
        if let Some(resolution) = configured(&self.resolution) {
            recording = recording.with_resolution(resolution);
        }
        if let Some(framerate) = configured(&self.framerate) {
            recording = recording.with_framerate(framerate);
        }
        if let Some(codec) = configured(&self.codec) {
            recording = recording.with_codec(codec);
        }
        if let Some(encoder) = configured(&self.encoder) {
            recording = recording.with_encoder(encoder);
        }
        if let Some(microphone) = configured(&self.microphone) {
            recording = recording.with_microphone(audio_source(&microphone));
        }
        if let Some(system_audio) = configured(&self.system_audio) {
            recording = recording.with_system_audio(audio_source(&system_audio));
        }
        if configured(&self.resolution).is_some() || configured(&self.encoder).is_some() {
            recording = recording.with_unavailable_choice(UnavailableChoice::Substitute);
        }
        recording
    }
}

/// The value, when a layer above the shipped default supplied it.
///
/// `None` is "nothing configured this", which is the state
/// [`ResolvedSettings::apply_configured_to`] leaves the recording's own answer
/// standing for.
fn configured<T: Clone>(resolved: &Resolved<T>) -> Option<T> {
    (resolved.source() != SettingSource::Default).then(|| resolved.value().clone())
}

/// The recording engine's name for a configured audio selection.
///
/// The two vocabularies are deliberately separate — `crate::settings` is what a
/// recording is told and this module is what a user configured — and this is
/// the one conversion between them.
fn audio_source(device: &AudioDeviceSetting) -> AudioSourceSetting {
    device.as_source()
}

impl AudioDeviceSetting {
    /// What a recording is told to open for this selection.
    ///
    /// Public because a caller can hold a configured value without ever
    /// resolving a whole [`ResolvedSettings`]: the settings screen's level
    /// check parses one value, and needs the recording engine's name for it in
    /// order to point a capture at the same endpoint a recording would
    /// ([issue #109](https://github.com/wildware-uk/clipped/issues/109)). It is
    /// [`audio_source`]'s implementation rather than a second copy of the same
    /// three lines, so the two cannot drift apart (AGENTS.md section 55).
    #[must_use]
    pub fn as_source(&self) -> AudioSourceSetting {
        match self {
            Self::Default => AudioSourceSetting::SystemDefault,
            Self::Disabled => AudioSourceSetting::Off,
            Self::Named(name) => AudioSourceSetting::Named(name.clone()),
        }
    }
}

impl fmt::Display for ResolvedSettings {
    /// Every setting and where it came from, on one line.
    ///
    /// This is what a recording writes into the log when it starts, and it is
    /// the answer to "why was this session recorded like that" months later
    /// (AGENTS.md section 35).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, key) in SettingKey::ALL.into_iter().enumerate() {
            if position > 0 {
                formatter.write_str(", ")?;
            }
            write!(
                formatter,
                "{key}={} ({})",
                self.written_value(key),
                self.source_of(key)
            )?;
        }
        Ok(())
    }
}

/// The layers one resolution reads, in the order they are folded.
struct Layers<'a> {
    layer: SettingSource,
    global: &'a Preferences,
    game: Option<&'a Preferences>,
}

impl Layers<'_> {
    /// The last layer that says anything about a setting, and which one that
    /// was.
    fn pick<T>(&self, default: T, read: impl Fn(&Preferences) -> Option<T>) -> Resolved<T> {
        let mut value = default;
        let mut source = SettingSource::Default;
        if let Some(set) = read(self.global) {
            value = set;
            source = SettingSource::Global;
        }
        if let Some(set) = self.game.and_then(read) {
            value = set;
            source = SettingSource::Game;
        }
        Resolved::new(value, source, self.layer)
    }
}
