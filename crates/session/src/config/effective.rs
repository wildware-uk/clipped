//! What a recording is actually running with.
//!
//! [`ResolvedSettings`] is what the *configuration* answered for a game. It is
//! not on its own what the recording is doing, and the difference is the whole
//! reason this module exists: both callers that start a recording build it from
//! something of their own first — `watch` from its command line, `serve` from
//! the `start_recording` it was sent — and then lay the configured settings over
//! it with
//! [`apply_configured_to`](ResolvedSettings::apply_configured_to), which only
//! replaces what a user configured. A setting no layer above the shipped default
//! mentions is therefore left as the caller asked for it, and reporting the
//! resolved answer for it would name a value the encoder is not using.
//!
//! So a reading of "the effective settings" is taken from the
//! [`RecordingSettings`] the recording was handed, and the layer is read off the
//! [`ResolvedSettings`] it was laid over. It is issue #61's third acceptance
//! criterion — *"effective settings are visible in diagnostics and session
//! metadata"* — for the diagnostics half; the session metadata half is
//! `crate::automatic::sidecar`, which writes the resolved answer because a
//! sitting's record is about what was configured for that game.
//!
//! # What is in a reading, and what is not
//!
//! Every setting that is a field of a [`RecordingSettings`], and no others:
//!
//! | Not reported            | Why                                                                  |
//! | ----------------------- | -------------------------------------------------------------------- |
//! | `capture_target`        | nothing in this build reads it, so no recording has an answer for it |
//! | `replay_window_seconds` | not a property of the recording; it sizes a buffer when one is opened |
//!
//! Naming them here rather than reporting them as something is the same rule
//! the Diagnostics screen follows for the eight figures nothing counts: a value
//! nobody measured and a value that is zero are different facts (AGENTS.md
//! section 27).

use crate::config::value::{SettingKey, SettingSource};
use crate::config::ResolvedSettings;
use crate::settings::RecordingSettings;

/// One setting a recording is running with, and where the answer came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSetting {
    key: SettingKey,
    value: String,
    source: EffectiveSource,
}

impl EffectiveSetting {
    /// Which setting this is.
    #[must_use]
    pub const fn key(&self) -> SettingKey {
        self.key
    }

    /// The value, spelled the way `settings.json` and the command line spell it.
    ///
    /// One vocabulary for the file, the command line, the log line a recording
    /// starts with and this, so that a user comparing what was recorded against
    /// what they set never has to translate between two spellings of one
    /// encoder (`crate::config::document`).
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Where the recording got it.
    #[must_use]
    pub const fn source(&self) -> EffectiveSource {
        self.source
    }
}

/// Where a recording's answer for one setting came from.
///
/// Deliberately not [`SettingSource`] with a fourth variant added. That type is
/// the settings *file*'s three layers and is what a settings screen draws its
/// "inherited from global" badge and its Reset control from; a fourth value it
/// could never produce would be a state every one of those callers has to
/// handle and none can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveSource {
    /// A layer of the settings file, or the value Clipped ships with.
    Configured(SettingSource),
    /// The recording asked for this itself.
    ///
    /// A `clipped-recorder watch` command line, or a `start_recording` that
    /// named the setting — and no layer above the shipped default overrode it.
    Request,
}

impl EffectiveSource {
    /// The word this is on the wire and in a report.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Configured(source) => source.token(),
            Self::Request => "request",
        }
    }
}

impl std::fmt::Display for EffectiveSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.token())
    }
}

/// What `recording` is running with, given the settings it was laid over.
///
/// The rule, stated once here because everything downstream of it is a
/// rendering: a setting whose value matches what the configuration resolved is
/// reported as coming from the layer that resolved it, and every other setting
/// is reported as the recording's own request. That is exactly the inverse of
/// what [`ResolvedSettings::apply_configured_to`] did — it replaced a request's
/// answer only where a layer above the default had one — so the two cannot
/// drift apart without this reading changing.
///
/// A caller that passes a `resolved` other than the one the recording was built
/// from gets a reading of `request` for everything, which is wrong but not
/// misleading: it claims nothing about a settings file it was not shown.
#[must_use]
pub fn effective_settings(
    recording: &RecordingSettings,
    resolved: &ResolvedSettings,
) -> Vec<EffectiveSetting> {
    REPORTED
        .into_iter()
        .map(|key| {
            let value = crate::config::document::written_recording_setting(recording, key);
            let source = if value == resolved.written_value(key) {
                EffectiveSource::Configured(resolved.source_of(key))
            } else {
                EffectiveSource::Request
            };
            EffectiveSetting { key, value, source }
        })
        .collect()
}

/// The settings a [`RecordingSettings`] has an answer for, in the order the
/// settings file and the settings screen list them.
const REPORTED: [SettingKey; 6] = [
    SettingKey::Resolution,
    SettingKey::Framerate,
    SettingKey::Codec,
    SettingKey::Encoder,
    SettingKey::Microphone,
    SettingKey::SystemAudio,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Configuration, GameKey, Preferences};
    use crate::settings::{
        AudioSourceSetting, CaptureTargetSettings, CodecPreference, ResolutionSetting,
    };
    use std::path::PathBuf;

    fn a_recording() -> RecordingSettings {
        RecordingSettings::new(
            CaptureTargetSettings::window(0x1234, 1920, 1080),
            PathBuf::from("recording.mkv"),
        )
    }

    fn resolved_for(preferences: Preferences) -> ResolvedSettings {
        let mut configuration = Configuration::defaults();
        let game = GameKey::parse("a-game").expect("a valid key");
        configuration.set_game(game.clone(), preferences);
        configuration.resolve_for(&game)
    }

    #[test]
    fn a_setting_the_command_line_asked_for_is_reported_as_the_recordings_own() {
        // The case the whole module is for. Nothing configures the codec, so
        // `apply_configured_to` leaves the command line's answer standing — and
        // a reading that reported the resolved `auto` here would name a codec
        // the encoder is not using, on the screen somebody opens *because* the
        // recording came out wrong.
        let resolved = resolved_for(Preferences::none());
        let recording = resolved.apply_configured_to(
            a_recording().with_codec(CodecPreference::Fixed(clipped_encoder::Codec::H264)),
        );

        let effective = effective_settings(&recording, &resolved);
        let codec = effective
            .iter()
            .find(|setting| setting.key() == SettingKey::Codec)
            .expect("the codec is reported");

        assert_eq!(codec.value(), "h264");
        assert_eq!(codec.source(), EffectiveSource::Request);
        assert_eq!(codec.source().token(), "request");
    }

    #[test]
    fn a_setting_the_game_configured_is_reported_against_that_game() {
        // And the other half: what the game's own layer said reached the
        // recording, so the reading has to say so rather than crediting the
        // command line it replaced.
        let mut preferences = Preferences::none();
        preferences.set_framerate(Some(120)).expect("in range");
        let resolved = resolved_for(preferences);
        let recording = resolved.apply_configured_to(a_recording().with_framerate(30));

        let effective = effective_settings(&recording, &resolved);
        let framerate = effective
            .iter()
            .find(|setting| setting.key() == SettingKey::Framerate)
            .expect("the frame rate is reported");

        assert_eq!(framerate.value(), "120");
        assert_eq!(
            framerate.source(),
            EffectiveSource::Configured(SettingSource::Game)
        );
        assert_eq!(framerate.source().token(), "game");
    }

    #[test]
    fn a_setting_the_global_layer_supplied_names_the_global_layer() {
        let mut global = Preferences::none();
        global
            .set_microphone(Some(crate::config::AudioDeviceSetting::Disabled))
            .expect("a valid device setting");
        let mut configuration = Configuration::defaults();
        configuration.set_global(global);
        let game = GameKey::parse("a-game").expect("a valid key");
        let resolved = configuration.resolve_for(&game);

        let recording = resolved.apply_configured_to(
            a_recording().with_microphone(AudioSourceSetting::Named("Yeti".to_owned())),
        );

        let effective = effective_settings(&recording, &resolved);
        let microphone = effective
            .iter()
            .find(|setting| setting.key() == SettingKey::Microphone)
            .expect("the microphone is reported");

        assert_eq!(microphone.value(), "none");
        assert_eq!(
            microphone.source(),
            EffectiveSource::Configured(SettingSource::Global)
        );
    }

    #[test]
    fn every_reported_setting_is_one_a_recording_actually_carries() {
        // The list is written out rather than derived from `SettingKey::ALL`,
        // because two of that list are not fields of a `RecordingSettings` and
        // reporting a value for them would be the invented reading AGENTS.md
        // section 27 rules out. This is what stops the list quietly growing one.
        assert!(!REPORTED.contains(&SettingKey::CaptureTarget));
        assert!(!REPORTED.contains(&SettingKey::ReplayWindow));
        assert_eq!(
            REPORTED.len(),
            SettingKey::ALL.len() - 2,
            "every other setting a game may override is a field of a recording and should be \
             reported"
        );
    }

    #[test]
    fn a_recording_with_a_fixed_size_reports_it_the_way_the_file_spells_it() {
        let mut preferences = Preferences::none();
        preferences
            .set_resolution(Some(ResolutionSetting::Fixed {
                width: 2560,
                height: 1440,
            }))
            .expect("in range");
        let resolved = resolved_for(preferences);
        let recording = resolved.apply_configured_to(a_recording());

        let effective = effective_settings(&recording, &resolved);
        let resolution = effective
            .iter()
            .find(|setting| setting.key() == SettingKey::Resolution)
            .expect("the resolution is reported");

        assert_eq!(resolution.value(), "2560x1440");
        assert_eq!(
            resolution.source(),
            EffectiveSource::Configured(SettingSource::Game)
        );
    }
}
