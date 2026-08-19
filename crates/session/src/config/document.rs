//! The settings file: its shape, its version, and how an older one becomes a
//! current one.
//!
//! # The shape
//!
//! ```json
//! {
//!   "version": 1,
//!   "global": { "framerate": 60 },
//!   "games": { "counter-strike-2": { "framerate": 120 } },
//!   "hotkeys": { "save_replay": "Ctrl+F10" }
//! }
//! ```
//!
//! AGENTS.md section 30's worked example, exactly: the global layer says 60,
//! Counter-Strike 2 says 120, and Minecraft — which has no section at all, or
//! a section that does not mention the frame rate — inherits 60. A game
//! inherits by *saying nothing*, never by repeating the global value, which is
//! what keeps a later change to the global 60 reaching it.
//!
//! JSON rather than TOML because `clipped-session` already writes JSON for a
//! session's sidecar (`docs/sessions.md`) and already depends on `serde_json`;
//! a second serialisation format in one crate would be a dependency and a set
//! of quoting rules bought for nothing.
//!
//! # Versions
//!
//! [`SCHEMA_VERSION`] is what this build writes. A file declaring a *higher*
//! version is refused and left alone — the newer Clipped that wrote it still
//! has to be able to read it, and silently rewriting it at this version would
//! throw away whatever that build had stored (AGENTS.md section 56). A file
//! declaring a *lower* version is migrated in memory, and the file on disk is
//! not touched until the user next changes something; a migration that runs on
//! open would rewrite a file for somebody who only wanted to look at it.
//!
//! ## Version 0
//!
//! Version 0 is a document with no `version` key. Clipped has never shipped a
//! settings file, so no user has one; what version 0 describes is the
//! vocabulary that *did* exist before this module — the catalogue's
//! `default_settings` table, whose worked example spells the frame rate `fps`
//! (`crates/game-detection/src/catalogue/mod.rs`). Version 1 spells it
//! `framerate`, which is what `--framerate` and
//! [`RecordingSettings::framerate`](crate::RecordingSettings::framerate) call
//! it, and the migration renames it. It is a small migration on purpose: what
//! it is really for is that the mechanism — detect, migrate, refuse to go
//! backwards, preserve what it does not understand — is written and tested
//! before there is a user's file depending on it being right.
//!
//! # Keys this build has never heard of
//!
//! They are kept. Every section carries the keys it could not interpret and
//! writes them back out unchanged, so that a settings file written by a newer
//! Clipped survives being opened, read and saved by an older one. Losing a
//! user's settings because their other machine is a version ahead is exactly
//! the destruction AGENTS.md section 56 forbids.

use core::time::Duration;
use std::collections::BTreeMap;
use std::path::Path;

use clipped_encoder::{Codec, EncoderKind};
use clipped_hotkeys::HotkeyAction;
use serde_json::{Map, Value};

use crate::config::error::{ConfigurationError, Section, SettingError};
use crate::config::game::GameKey;
use crate::config::hotkeys::{HotkeyOverride, HotkeyOverrides};
use crate::config::preferences::{
    AudioDeviceSetting, CaptureTargetSetting, Preferences, ResolvedSettings, MAXIMUM_FRAMERATE,
    MINIMUM_FRAMERATE, REPLAY_WINDOW_OFF,
};
use crate::config::value::SettingKey;
use crate::config::Configuration;
use crate::settings::{CodecPreference, EncoderPreference, ResolutionSetting};

/// The settings format this build writes.
pub const SCHEMA_VERSION: u32 = 1;

/// The file name a settings file has under Clipped's per-user directory.
pub const FILE_NAME: &str = "settings.json";

/// The older spelling of [`SettingKey::Framerate`] that version 0 used.
const VERSION_0_FRAMERATE: &str = "fps";

/// What reading a settings file found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loaded {
    /// There is no settings file. The defaults apply, and this is not an error:
    /// a user who has never opened the settings screen has no file.
    Absent,
    /// The file was at the current version and was read as written.
    AsWritten,
    /// The file was at an older version and was migrated in memory. The file
    /// itself is untouched until something is saved.
    Migrated {
        /// The version it was written at.
        from: u32,
    },
}

/// Turns the text of a settings file into a configuration.
///
/// # Errors
///
/// Every variant of [`ConfigurationError`] except `Read` and `Write`, each
/// naming `path` so that the message says which file to go and fix.
pub(crate) fn parse(
    path: &Path,
    text: &str,
) -> Result<(Configuration, Loaded), ConfigurationError> {
    let value: Value = serde_json::from_str(text).map_err(|error| ConfigurationError::Syntax {
        path: path.to_path_buf(),
        line: error.line(),
        column: error.column(),
        message: error.to_string(),
    })?;

    let Value::Object(mut document) = value else {
        return Err(ConfigurationError::Malformed {
            path: path.to_path_buf(),
            section: Section::Document,
            detail: format!("the file is {} and settings are an object", kind_of(&value)),
        });
    };

    let version = read_version(path, &document)?;
    if version > u64::from(SCHEMA_VERSION) {
        return Err(ConfigurationError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    // Narrowing is safe: anything above `SCHEMA_VERSION` was refused above.
    let version = u32::try_from(version).unwrap_or(SCHEMA_VERSION);
    migrate(path, &mut document, version)?;

    let global = read_preferences(
        path,
        Section::Global,
        take_object(path, &mut document, "global", Section::Document)?,
    )?;

    let mut games = BTreeMap::new();
    if let Some(table) = take_object(path, &mut document, "games", Section::Document)? {
        for (name, value) in table {
            let key = GameKey::parse(&name).map_err(|error| ConfigurationError::Malformed {
                path: path.to_path_buf(),
                section: Section::Document,
                detail: error.to_string(),
            })?;
            let section = Section::Game(name.clone());
            let Value::Object(object) = value else {
                return Err(ConfigurationError::Malformed {
                    path: path.to_path_buf(),
                    section: section.clone(),
                    detail: format!(
                        "a game's settings are an object and this is {}",
                        kind_of(&value)
                    ),
                });
            };
            games.insert(key, read_preferences(path, section, Some(object))?);
        }
    }

    let hotkeys = read_hotkeys(
        path,
        take_object(path, &mut document, "hotkeys", Section::Document)?,
    )?;

    // Never refused, only kept: an entry this build cannot read is a record of
    // what somebody consented to, and deleting one to tidy the file would be
    // the worst possible way to lose it (`super::plugins`).
    let plugins = super::plugins::read(take_object(
        path,
        &mut document,
        "plugins",
        Section::Document,
    )?);

    // Refused rather than kept, unlike the section above, and for the opposite
    // reason: a limit outside `clipped-library`'s bounds is a limit the user
    // meant and would not get, and a library they believe is capped and is not
    // is worse than a file that will not load (AGENTS.md section 27).
    let storage = super::storage::read(take_object(
        path,
        &mut document,
        "storage",
        Section::Document,
    )?)
    .map_err(|(key, source)| ConfigurationError::InvalidStorageLimit {
        path: path.to_path_buf(),
        key,
        source,
    })?;

    // Kept rather than refused, like `plugins` above and unlike `storage`: a
    // switch this build cannot read leaves that category interrupting, which is
    // a nuisance, and refusing would mean a typo in a notification switch
    // stopped the recording settings in the same file from loading
    // (`super::notifications`).
    let notifications = super::notifications::read(take_object(
        path,
        &mut document,
        "notifications",
        Section::Document,
    )?);

    // Dropped rather than kept or refused, unlike all three of the sections
    // above, because it is the one section the user did not write: an entry
    // this build cannot read is a note Clipped made about a machine and can
    // make again by recording the game once (`super::capture_memory`).
    let capture = super::capture_memory::read(take_object(
        path,
        &mut document,
        "capture",
        Section::Document,
    )?);

    // Whatever is left is a key from a newer build, or the version, which is
    // rewritten rather than kept.
    document.remove("version");
    let unknown = document.into_iter().collect();

    let configuration = Configuration::from_parts(crate::config::Parts {
        global,
        games,
        hotkeys,
        plugins,
        storage,
        notifications,
        capture,
        unknown,
    });
    let loaded = if version == SCHEMA_VERSION {
        Loaded::AsWritten
    } else {
        Loaded::Migrated { from: version }
    };
    Ok((configuration, loaded))
}

/// Turns a configuration into the text of a settings file, at
/// [`SCHEMA_VERSION`].
pub(crate) fn render(configuration: &Configuration) -> String {
    let mut document = Map::new();
    document.insert("version".to_owned(), Value::from(SCHEMA_VERSION));
    document.insert(
        "global".to_owned(),
        Value::Object(write_preferences(configuration.global())),
    );

    let mut games = Map::new();
    for (key, preferences) in configuration.games() {
        games.insert(
            key.as_str().to_owned(),
            Value::Object(write_preferences(preferences)),
        );
    }
    document.insert("games".to_owned(), Value::Object(games));
    document.insert(
        "hotkeys".to_owned(),
        Value::Object(write_hotkeys(configuration.hotkeys())),
    );
    document.insert(
        "plugins".to_owned(),
        Value::Object(super::plugins::write(configuration.plugins())),
    );
    // Written only when something is set, so that a file nobody has given a
    // storage limit does not grow an empty section explaining that it has none.
    let storage = super::storage::write(configuration.storage());
    if !storage.is_empty() {
        document.insert("storage".to_owned(), Value::Object(storage));
    }
    // Written only when something has been remembered, for the same reason and
    // one of its own: a settings file that grows an empty `capture` section on
    // the first save is a file that invites the question "what is this?" from
    // somebody who has nothing in it.
    let capture = super::capture_memory::write(configuration.capture_memories());
    if !capture.is_empty() {
        document.insert("capture".to_owned(), Value::Object(capture));
    }
    // Written only when something is set, for the same reason: a user who has
    // never switched a notification off should not find a section explaining
    // that everything is on.
    let notifications = super::notifications::write(configuration.notifications());
    if !notifications.is_empty() {
        document.insert("notifications".to_owned(), Value::Object(notifications));
    }

    for (key, value) in configuration.unrecognised() {
        document.insert(key.clone(), value.clone());
    }

    let mut text = serde_json::to_string_pretty(&Value::Object(document))
        .expect("a document of strings, numbers and objects always serialises");
    text.push('\n');
    text
}

fn read_version(path: &Path, document: &Map<String, Value>) -> Result<u64, ConfigurationError> {
    match document.get("version") {
        // No version key at all is version 0, the shape that predates this
        // module. See the module documentation.
        None => Ok(0),
        Some(Value::Number(number)) => {
            number
                .as_u64()
                .ok_or_else(|| ConfigurationError::Malformed {
                    path: path.to_path_buf(),
                    section: Section::Document,
                    detail: format!(
                        "\"version\" is {number} and a settings version is a whole number"
                    ),
                })
        }
        Some(other) => Err(ConfigurationError::Malformed {
            path: path.to_path_buf(),
            section: Section::Document,
            detail: format!(
                "\"version\" is {} and a settings version is a whole number",
                kind_of(other)
            ),
        }),
    }
}

/// Brings a document up to [`SCHEMA_VERSION`], one version at a time.
///
/// One step per version, applied in order, so that a file three versions old
/// arrives by the same path as one that is one version old — rather than by a
/// second, less-travelled branch that nobody has run.
fn migrate(
    path: &Path,
    document: &mut Map<String, Value>,
    from: u32,
) -> Result<(), ConfigurationError> {
    if from < 1 {
        migrate_0_to_1(path, document)?;
    }
    Ok(())
}

/// Version 0 to version 1: `fps` becomes `framerate`, in every section.
fn migrate_0_to_1(
    path: &Path,
    document: &mut Map<String, Value>,
) -> Result<(), ConfigurationError> {
    fn rename(
        path: &Path,
        section: Section,
        object: &mut Map<String, Value>,
    ) -> Result<(), ConfigurationError> {
        let Some(value) = object.remove(VERSION_0_FRAMERATE) else {
            return Ok(());
        };
        if object.contains_key(SettingKey::Framerate.name()) {
            // Both spellings. Guessing which the user meant risks recording at
            // the wrong rate for the rest of their sessions, so it is refused
            // and the file is left untouched.
            return Err(ConfigurationError::Invalid {
                path: path.to_path_buf(),
                section,
                source: SettingError::AmbiguousMigration {
                    from: VERSION_0_FRAMERATE,
                    to: SettingKey::Framerate,
                },
            });
        }
        object.insert(SettingKey::Framerate.name().to_owned(), value);
        Ok(())
    }

    if let Some(Value::Object(global)) = document.get_mut("global") {
        rename(path, Section::Global, global)?;
    }
    if let Some(Value::Object(games)) = document.get_mut("games") {
        for (name, value) in games.iter_mut() {
            if let Value::Object(game) = value {
                rename(path, Section::Game(name.clone()), game)?;
            }
        }
    }
    Ok(())
}

fn take_object(
    path: &Path,
    document: &mut Map<String, Value>,
    key: &str,
    section: Section,
) -> Result<Option<Map<String, Value>>, ConfigurationError> {
    match document.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object)),
        Some(other) => Err(ConfigurationError::Malformed {
            path: path.to_path_buf(),
            section,
            detail: format!("\"{key}\" is an object and this is {}", kind_of(&other)),
        }),
    }
}

fn read_preferences(
    path: &Path,
    section: Section,
    object: Option<Map<String, Value>>,
) -> Result<Preferences, ConfigurationError> {
    let mut preferences = Preferences::none();
    let Some(object) = object else {
        return Ok(preferences);
    };

    for (key, value) in object {
        let Some(setting) = SettingKey::from_name(&key) else {
            preferences.keep_unrecognised(key, value);
            continue;
        };
        // A key present with a `null` value means the same thing as an absent
        // key: this layer says nothing. That is what a settings screen writes
        // when the user presses Reset, and reading it as "set to nothing" would
        // make Reset unrepresentable.
        if value.is_null() {
            continue;
        }
        read_setting(&mut preferences, setting, &value).map_err(|source| {
            ConfigurationError::Invalid {
                path: path.to_path_buf(),
                section: section.clone(),
                source,
            }
        })?;
    }
    Ok(preferences)
}

fn read_setting(
    preferences: &mut Preferences,
    key: SettingKey,
    value: &Value,
) -> Result<(), SettingError> {
    match key {
        SettingKey::CaptureTarget => {
            let token = expect_string(key, value, "\"game-window\" or \"display\"")?;
            let target = CaptureTargetSetting::from_token(token).ok_or_else(|| {
                SettingError::Unrecognised {
                    key,
                    value: format!("{token:?}"),
                    accepted: "\"game-window\" or \"display\"",
                }
            })?;
            preferences.set_capture_target(Some(target));
        }
        SettingKey::Resolution => {
            let token = expect_string(key, value, "\"source\" or \"1920x1080\"")?;
            preferences.set_resolution(Some(parse_resolution(token)?))?;
        }
        SettingKey::Framerate => {
            let framerate = expect_u32(key, value, "a whole number of frames per second")?;
            preferences.set_framerate(Some(framerate))?;
        }
        SettingKey::Codec => {
            let token = expect_string(key, value, CODECS)?;
            let codec = parse_codec(token)?;
            preferences.set_codec(Some(codec));
        }
        SettingKey::Encoder => {
            let token = expect_string(key, value, ENCODERS)?;
            let encoder = parse_encoder(token)?;
            preferences.set_encoder(Some(encoder));
        }
        SettingKey::Microphone => {
            let token = expect_string(key, value, DEVICES)?;
            preferences.set_microphone(Some(parse_device(key, token)?))?;
        }
        SettingKey::SystemAudio => {
            let token = expect_string(key, value, DEVICES)?;
            preferences.set_system_audio(Some(parse_device(key, token)?))?;
        }
        SettingKey::ReplayWindow => {
            let seconds = expect_u32(key, value, "a whole number of seconds")?;
            preferences.set_replay_window(Some(Duration::from_secs(u64::from(seconds))))?;
        }
    }
    Ok(())
}

const CODECS: &str = "\"auto\", \"h264\", \"hevc\" or \"av1\"";
const ENCODERS: &str = "\"auto\", \"nvenc\", \"amf\", \"quicksync\" or \"software\"";
const DEVICES: &str = "\"default\", \"none\" or a device name";

/// The token every value a setting can be spelled as, where the set is closed.
///
/// Empty for the settings whose values are open — a frame rate, a size, a
/// device name — which a caller tells apart by the list being empty rather than
/// by knowing which settings those are. This is what lets a settings screen
/// draw a list of choices for one setting and a field for another without
/// keeping its own copy of either (issue #51).
///
/// Built from the same sources the parsers read, so a codec added to
/// [`Codec::EFFICIENCY_ORDER`] appears here without anybody remembering to add
/// it (AGENTS.md section 55).
pub(crate) fn choices_for(key: SettingKey) -> Vec<String> {
    match key {
        SettingKey::CaptureTarget => CaptureTargetSetting::ALL
            .iter()
            .map(|target| target.token().to_owned())
            .collect(),
        SettingKey::Codec => core::iter::once("auto".to_owned())
            .chain(
                Codec::EFFICIENCY_ORDER
                    .into_iter()
                    .map(|codec| codec.log_value().to_owned()),
            )
            .collect(),
        SettingKey::Encoder => core::iter::once("auto".to_owned())
            .chain(
                EncoderKind::ALL
                    .into_iter()
                    .map(|encoder| encoder_token(encoder).to_owned()),
            )
            .collect(),
        SettingKey::Resolution
        | SettingKey::Framerate
        | SettingKey::Microphone
        | SettingKey::SystemAudio
        | SettingKey::ReplayWindow => Vec::new(),
    }
}

/// What a setting would have accepted, in the words its refusal uses.
///
/// The same sentences [`SettingError`] carries, so that the hint beside a field
/// and the refusal that follows a bad value cannot disagree.
pub(crate) fn accepted_for(key: SettingKey) -> String {
    match key {
        SettingKey::CaptureTarget => "\"game-window\" or \"display\"".to_owned(),
        SettingKey::Resolution => "\"source\" or a size such as \"1920x1080\"".to_owned(),
        SettingKey::Framerate => {
            format!("{MINIMUM_FRAMERATE}-{MAXIMUM_FRAMERATE} frames per second")
        }
        SettingKey::Codec => CODECS.to_owned(),
        SettingKey::Encoder => ENCODERS.to_owned(),
        SettingKey::Microphone | SettingKey::SystemAudio => DEVICES.to_owned(),
        // The off value is named first because it is the answer somebody who
        // typed a too-small number is most likely to have been reaching for:
        // "5" is usually "as little as possible", and until issue #539 the
        // nearest thing this setting could offer was a buffer.
        SettingKey::ReplayWindow => format!(
            "{} to keep no replay buffer, or {}-{} seconds",
            REPLAY_WINDOW_OFF.as_secs(),
            clipped_replay::MINIMUM_WINDOW.as_secs(),
            clipped_replay::MAXIMUM_WINDOW.as_secs()
        ),
    }
}

/// Sets one setting from the text the file spells it with.
///
/// The inverse of [`written_setting`], and deliberately the *same* parsers the
/// file reader uses: a value the settings screen sends over the control
/// protocol is refused exactly when the same value written into
/// [`FILE_NAME`] by hand would be refused, with the same message
/// (AGENTS.md section 55). A screen that validated for itself would be a second
/// opinion about what a frame rate is.
///
/// # Errors
///
/// [`SettingError`] naming the setting, the value and what would have been
/// accepted.
pub(crate) fn set_written_setting(
    preferences: &mut Preferences,
    key: SettingKey,
    token: &str,
) -> Result<(), SettingError> {
    let whole_number = |accepted: &'static str| {
        token
            .trim()
            .parse::<u32>()
            .map_err(|_| SettingError::Unrecognised {
                key,
                value: format!("{token:?}"),
                accepted,
            })
    };

    match key {
        SettingKey::Framerate => {
            preferences.set_framerate(Some(whole_number("a whole number of frames per second")?))
        }
        SettingKey::ReplayWindow => preferences.set_replay_window(Some(Duration::from_secs(
            u64::from(whole_number("a whole number of seconds")?),
        ))),
        // Everything else is a string in the file, and `read_setting` is the
        // one reader of those strings.
        _ => read_setting(preferences, key, &Value::String(token.to_owned())),
    }
}

/// One *resolved* setting, spelled the way the settings file spells it.
///
/// The file's vocabulary is also the command line's (`docs/recorder-cli.md`),
/// so it is the vocabulary a log line and a session's record should use as
/// well: a user comparing what was recorded against what they set should not
/// have to translate between three spellings of the same encoder. It is written
/// here, beside the parser, because a second renderer is exactly how the two
/// would drift (AGENTS.md section 55).
pub(crate) fn written_setting(settings: &ResolvedSettings, key: SettingKey) -> String {
    match key {
        SettingKey::CaptureTarget => settings.capture_target().get().token().to_owned(),
        SettingKey::Resolution => write_resolution(settings.resolution().get()),
        SettingKey::Framerate => settings.framerate().get().to_string(),
        SettingKey::Codec => write_codec(settings.codec().get()),
        SettingKey::Encoder => write_encoder(settings.encoder().get()),
        SettingKey::Microphone => write_device(settings.microphone().value()),
        SettingKey::SystemAudio => write_device(settings.system_audio().value()),
        SettingKey::ReplayWindow => settings.replay_window().get().as_secs().to_string(),
    }
}

fn write_preferences(preferences: &Preferences) -> Map<String, Value> {
    let mut object = Map::new();
    if let Some(target) = preferences.capture_target() {
        object.insert(
            SettingKey::CaptureTarget.name().to_owned(),
            Value::from(target.token()),
        );
    }
    if let Some(resolution) = preferences.resolution() {
        object.insert(
            SettingKey::Resolution.name().to_owned(),
            Value::from(write_resolution(resolution)),
        );
    }
    if let Some(framerate) = preferences.framerate() {
        object.insert(
            SettingKey::Framerate.name().to_owned(),
            Value::from(framerate),
        );
    }
    if let Some(codec) = preferences.codec() {
        object.insert(
            SettingKey::Codec.name().to_owned(),
            Value::from(write_codec(codec)),
        );
    }
    if let Some(encoder) = preferences.encoder() {
        object.insert(
            SettingKey::Encoder.name().to_owned(),
            Value::from(write_encoder(encoder)),
        );
    }
    if let Some(device) = preferences.microphone() {
        object.insert(
            SettingKey::Microphone.name().to_owned(),
            Value::from(write_device(device)),
        );
    }
    if let Some(device) = preferences.system_audio() {
        object.insert(
            SettingKey::SystemAudio.name().to_owned(),
            Value::from(write_device(device)),
        );
    }
    if let Some(window) = preferences.replay_window() {
        object.insert(
            SettingKey::ReplayWindow.name().to_owned(),
            Value::from(window.as_secs()),
        );
    }
    for (key, value) in preferences.unrecognised() {
        object.insert(key.clone(), value.clone());
    }
    object
}

fn read_hotkeys(
    path: &Path,
    object: Option<Map<String, Value>>,
) -> Result<HotkeyOverrides, ConfigurationError> {
    let mut overrides = HotkeyOverrides::none();
    let Some(object) = object else {
        return Ok(overrides);
    };

    let invalid = |detail: String| ConfigurationError::Malformed {
        path: path.to_path_buf(),
        section: Section::Hotkeys,
        detail,
    };

    for (key, value) in object {
        let Some(action) = HotkeyAction::from_name(&key) else {
            overrides.keep_unrecognised(key, value);
            continue;
        };
        let binding = match value {
            // `null` is a deliberate unbinding, which is not the same as the
            // key being absent. `crate::config::hotkeys` says why.
            Value::Null => HotkeyOverride::Unbound,
            Value::String(text) => {
                let hotkey = text.parse().map_err(|error| {
                    invalid(format!(
                        "\"{key}\" is {text:?}, which is not a combination: {error}"
                    ))
                })?;
                HotkeyOverride::Bound(hotkey)
            }
            other => {
                return Err(invalid(format!(
                    "\"{key}\" is {}, and a hotkey is a combination such as \"Ctrl+F10\" or null",
                    kind_of(&other)
                )))
            }
        };
        overrides.insert_unchecked(action, binding);
    }

    // Validated once, after every binding has been read: reading them one at a
    // time would report a conflict against a half-read file.
    overrides
        .resolve()
        .map_err(|source| ConfigurationError::Invalid {
            path: path.to_path_buf(),
            section: Section::Hotkeys,
            source,
        })?;
    Ok(overrides)
}

fn write_hotkeys(overrides: &HotkeyOverrides) -> Map<String, Value> {
    let mut object = Map::new();
    for (action, binding) in overrides.iter() {
        let value = match binding {
            HotkeyOverride::Bound(hotkey) => Value::from(hotkey.to_string()),
            HotkeyOverride::Unbound => Value::Null,
        };
        object.insert(action.name().to_owned(), value);
    }
    for (key, value) in overrides.unrecognised() {
        object.insert(key.clone(), value.clone());
    }
    object
}

fn expect_string<'a>(
    key: SettingKey,
    value: &'a Value,
    expected: &'static str,
) -> Result<&'a str, SettingError> {
    value.as_str().ok_or(SettingError::WrongType {
        key,
        expected,
        found: kind_of(value),
    })
}

fn expect_u32(key: SettingKey, value: &Value, expected: &'static str) -> Result<u32, SettingError> {
    let number = value.as_u64().ok_or(SettingError::WrongType {
        key,
        expected,
        found: kind_of(value),
    })?;
    u32::try_from(number).map_err(|_| SettingError::OutOfRange {
        key,
        value: number.to_string(),
        accepted: expected.to_owned(),
    })
}

fn parse_resolution(token: &str) -> Result<ResolutionSetting, SettingError> {
    if token.eq_ignore_ascii_case("source") {
        return Ok(ResolutionSetting::Source);
    }
    let unrecognised = || SettingError::Unrecognised {
        key: SettingKey::Resolution,
        value: format!("{token:?}"),
        accepted: "\"source\" or a size such as \"1920x1080\"",
    };
    let (width, height) = token.split_once(['x', 'X']).ok_or_else(unrecognised)?;
    let width = width.trim().parse().map_err(|_| unrecognised())?;
    let height = height.trim().parse().map_err(|_| unrecognised())?;
    Ok(ResolutionSetting::Fixed { width, height })
}

fn write_resolution(resolution: ResolutionSetting) -> String {
    match resolution {
        ResolutionSetting::Source => "source".to_owned(),
        ResolutionSetting::Fixed { width, height } => format!("{width}x{height}"),
    }
}

fn parse_codec(token: &str) -> Result<CodecPreference, SettingError> {
    if token.eq_ignore_ascii_case("auto") {
        return Ok(CodecPreference::Automatic);
    }
    Codec::EFFICIENCY_ORDER
        .into_iter()
        .find(|codec| codec.log_value().eq_ignore_ascii_case(token))
        .map(CodecPreference::Fixed)
        .ok_or(SettingError::Unrecognised {
            key: SettingKey::Codec,
            value: format!("{token:?}"),
            accepted: CODECS,
        })
}

fn write_codec(codec: CodecPreference) -> String {
    match codec {
        CodecPreference::Automatic => "auto".to_owned(),
        CodecPreference::Fixed(codec) => codec.log_value().to_owned(),
    }
}

/// The token each encoder family is written as.
///
/// The same words `--encoder` takes (`docs/recorder-cli.md`), so that a
/// settings file and a command line do not disagree about what an encoder is
/// called. `EncoderKind`'s own `Display` is the label a person reads — "NVIDIA
/// NVENC" — which is not what belongs in a file.
const fn encoder_token(encoder: EncoderKind) -> &'static str {
    match encoder {
        EncoderKind::Nvenc => "nvenc",
        EncoderKind::Amf => "amf",
        EncoderKind::QuickSync => "quicksync",
        EncoderKind::Software => "software",
    }
}

fn parse_encoder(token: &str) -> Result<EncoderPreference, SettingError> {
    if token.eq_ignore_ascii_case("auto") {
        return Ok(EncoderPreference::Automatic);
    }
    EncoderKind::ALL
        .into_iter()
        .find(|encoder| encoder_token(*encoder).eq_ignore_ascii_case(token))
        .map(EncoderPreference::Fixed)
        .ok_or(SettingError::Unrecognised {
            key: SettingKey::Encoder,
            value: format!("{token:?}"),
            accepted: ENCODERS,
        })
}

fn write_encoder(encoder: EncoderPreference) -> String {
    match encoder {
        EncoderPreference::Automatic => "auto".to_owned(),
        EncoderPreference::Fixed(encoder) => encoder_token(encoder).to_owned(),
    }
}

/// The prefix that makes a device name literal.
///
/// Without it, a user whose headset is genuinely called "Default" could not
/// name it. The same escape `--microphone` uses.
const LITERAL_DEVICE_PREFIX: &str = "name:";

fn parse_device(key: SettingKey, token: &str) -> Result<AudioDeviceSetting, SettingError> {
    if let Some(name) = token.strip_prefix(LITERAL_DEVICE_PREFIX) {
        return AudioDeviceSetting::named(key, name);
    }
    if token.eq_ignore_ascii_case("default") {
        return Ok(AudioDeviceSetting::Default);
    }
    if token.eq_ignore_ascii_case("none") {
        return Ok(AudioDeviceSetting::Disabled);
    }
    AudioDeviceSetting::named(key, token)
}

fn write_device(device: &AudioDeviceSetting) -> String {
    match device {
        AudioDeviceSetting::Default => "default".to_owned(),
        AudioDeviceSetting::Disabled => "none".to_owned(),
        // Always written with the prefix, so that a device called "none"
        // survives being saved and read back.
        AudioDeviceSetting::Named(name) => format!("{LITERAL_DEVICE_PREFIX}{name}"),
    }
}

/// What a JSON value is, in the words an error message uses.
const fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}
