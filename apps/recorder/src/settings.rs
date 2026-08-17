//! The settings file, as the window reads and changes it.
//!
//! # What this module is, and is not
//!
//! It is **not** a second configuration system. `clipped_session::config` owns
//! `settings.json` — the defaults, the validation, the layering, the migrations
//! and the refusal to overwrite a file it could not read — and every value here
//! goes in and comes out through that API (`docs/configuration.md`). What this
//! module adds is the two things the settings screen needs and the file cannot
//! answer:
//!
//! - **which settings this build actually reads when a recording starts.** A
//!   settings file can carry a key nothing acts on, and a screen that drew that
//!   key as a working control would be the lie AGENTS.md section 27 is about.
//!   [`APPLIES`] is that answer, per setting, and it is the recorder's to give
//!   because the recorder is the process that starts recordings;
//! - **the machine.** Which microphones exist is not configuration and cannot be
//!   in the file at all.
//!
//! # Why the file is read again on every change
//!
//! [`ConfigurationStore::store`] already refuses to replace a file this build
//! could not read. Reading it again before applying an edit closes the other
//! half: a settings file that changed since this process started — the user
//! edited it by hand, or a second window saved — must not be overwritten with
//! what this process happened to have in memory (AGENTS.md section 56).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use clipped_ipc::settings::{
    ApplySettings, AudioDevice, AudioDevices, MicrophoneLevel, MicrophoneLevelRequest,
    SettingEntry, SettingsView,
};
use clipped_ipc::{ErrorCode, ProtocolError};
use clipped_session::config::{
    Configuration, ConfigurationStore, Preferences, SettingKey, StorageSettings,
};

/// The key the recording directory has in the settings file.
///
/// It is not a [`SettingKey`], because it is not a per-game setting: the
/// library is one thing however many games are in it
/// (`clipped_session::config::storage`). The screen still draws it beside the
/// others, so it is listed with them here.
pub const RECORDING_DIRECTORY: &str = "recording_directory";

/// Whether a setting is one this build reads when a recording starts, and what
/// to say when it is not.
///
/// The sentence is shown in place of a control (`crate::serve`), so it names
/// what would have to land rather than saying "not supported". A setting that
/// moves from `false` to `true` here is a line somebody deletes when they wire
/// it up, which is the point: the list is next to the answer rather than in a
/// document that goes stale.
fn applies(key: SettingKey) -> Result<(), &'static str> {
    match key {
        // Laid over what a recording was asked for by
        // `ResolvedSettings::apply_configured_to`, on the path both `watch` and
        // this recorder's `start_recording` take.
        SettingKey::Resolution
        | SettingKey::Framerate
        | SettingKey::Codec
        | SettingKey::Encoder
        | SettingKey::Microphone
        | SettingKey::SystemAudio => Ok(()),
        // Read when a recording that asked for a buffer without naming a length
        // starts: the window sends `replay` with no seconds and this recorder
        // resolves the length from here (`crate::serve`, `ReplayAsked::
        // Configured`, issue #427). `clipped-recorder replay --duration` is the
        // other reader. It was not in force when the settings screen was
        // written; it is now, so the screen draws it as a control rather than
        // as a sentence.
        SettingKey::ReplayWindow => Ok(()),
        // Not applied: a recording still captures the game's own window,
        // because the capture target decides which handle the caller resolves
        // before a recording exists to be configured
        // (`clipped_session::config`).
        SettingKey::CaptureTarget => Err(
            "every recording captures the game's own window. Reading this setting when a \
             recording starts is issue #61",
        ),
    }
}

/// The settings file this recorder owns, and the configuration in force.
///
/// One store behind one lock: two windows saving at once are serialised here,
/// and a recording that is starting reads the same configuration a save just
/// wrote. Before this existed the configuration was a snapshot taken when the
/// process started, so a setting saved from the window reached the next
/// recording only after a restart — which is not what "close the window and
/// recording works from then on" means (SPEC.md section 45).
#[derive(Debug)]
pub struct SettingsFile {
    /// [`None`] when the environment describes no per-user directory at all, in
    /// which case there is nowhere to read or write and every settings command
    /// says so rather than pretending to have saved
    /// ([`ConfigurationStore::default_path`]).
    store: Option<Mutex<ConfigurationStore>>,
}

impl SettingsFile {
    /// The settings of whoever is running this recorder, read once.
    ///
    /// A file that cannot be read is reported and the defaults stand, exactly
    /// as `watch` treats one (`crate::watch::load_configuration`): a recorder
    /// that refused to start because of a settings file would be a recorder
    /// that stops recording over a typo.
    #[must_use]
    pub fn for_this_user() -> Self {
        match ConfigurationStore::default_path() {
            Some(path) => Self::at(path),
            None => Self { store: None },
        }
    }

    /// The settings kept at `path`, read once.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let mut store = ConfigurationStore::at(path);
        if let Err(error) = store.load() {
            // The same sentence `watch` says, through the same function, so
            // that an unreadable settings file reads the same way whichever
            // subcommand is running (AGENTS.md section 55). The store keeps the
            // defaults, and a later save is refused rather than performed
            // (`ConfigurationStore::store`).
            crate::watch::report_unreadable_settings(&error);
        }
        Self {
            store: Some(Mutex::new(store)),
        }
    }

    /// A settings file that is nowhere, for the environment that has no
    /// per-user directory.
    #[must_use]
    pub const fn nowhere() -> Self {
        Self { store: None }
    }

    /// The configuration a recording starting now is made with.
    ///
    /// A clone, taken under the lock and let go of immediately: what a
    /// recording is made with belongs to the moment it started, and nothing
    /// re-reads this while an encoder is running (issue #61).
    #[must_use]
    pub fn configuration(&self) -> Configuration {
        match &self.store {
            Some(store) => store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .current()
                .clone(),
            None => Configuration::defaults(),
        }
    }

    /// Where the settings are kept, for the window to show.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        self.store.as_ref().map(|store| {
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .path()
                .to_path_buf()
        })
    }

    /// Every setting, as it now stands.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Internal`] when there is nowhere to keep settings at all.
    pub fn view(&self) -> Result<SettingsView, ProtocolError> {
        let store = self.locked()?;
        Ok(view_of(store.current(), store.path()))
    }

    /// Applies changes and saves them, answering with the settings as they now
    /// stand.
    ///
    /// Nothing is written when anything is refused: the configuration is built
    /// in full and only then stored, so a request that names one good value and
    /// one bad one leaves the file exactly as it was.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] naming the setting, the value and what
    /// would have been accepted, and [`ErrorCode::Internal`] when the file
    /// itself cannot be read or written — carrying `clipped_session`'s own
    /// sentence, which names the file and what to do about it.
    pub fn apply(&self, request: &ApplySettings) -> Result<SettingsView, ProtocolError> {
        let mut store = self.locked()?;

        // Read first. What is on disk may be newer than what this process last
        // saw, and an edit is applied on top of it rather than instead of it.
        store.load().map_err(|error| {
            ProtocolError::new(
                ErrorCode::Internal,
                format!("the settings could not be read: {error}"),
            )
        })?;

        let mut configuration = store.current().clone();
        let mut global = configuration.global().clone();
        let mut storage = configuration.storage().clone();

        for (key, value) in &request.values {
            match key.as_str() {
                RECORDING_DIRECTORY => set_recording_directory(&mut storage, value.as_deref())?,
                name => {
                    let setting = SettingKey::from_name(name).ok_or_else(|| {
                        ProtocolError::new(
                            ErrorCode::InvalidParameters,
                            format!("this recorder has no `{name}` setting"),
                        )
                    })?;
                    set_setting(&mut global, setting, value.as_deref())?;
                }
            }
        }

        configuration.set_global(global);
        configuration.set_storage(storage);

        store.store(configuration).map_err(|error| {
            ProtocolError::new(
                ErrorCode::Internal,
                format!("the settings could not be saved: {error}"),
            )
        })?;

        Ok(view_of(store.current(), store.path()))
    }

    fn locked(&self) -> Result<std::sync::MutexGuard<'_, ConfigurationStore>, ProtocolError> {
        let store = self.store.as_ref().ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::Internal,
                "Clipped has nowhere to keep settings on this machine: no per-user application \
                 directory could be worked out, so nothing can be read or saved",
            )
        })?;
        Ok(store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

/// Sets one per-game-able setting, or clears it.
fn set_setting(
    global: &mut Preferences,
    key: SettingKey,
    value: Option<&str>,
) -> Result<(), ProtocolError> {
    global.set_written(key, value).map_err(|error| {
        // `clipped_session`'s own sentence, which already names the setting,
        // the value and what would have been accepted (AGENTS.md section 45).
        ProtocolError::new(ErrorCode::InvalidParameters, error.to_string())
    })?;

    // Refused here as well as when a recording starts, because "at the moment
    // you set it" is where a refusal is useful: a named playback endpoint is
    // something the file can carry and this build cannot open, and the session's
    // own sentence is the one shown so the two cannot disagree
    // (`clipped_session::audio`, issue #316).
    if key == SettingKey::SystemAudio {
        if let Some(text) = value {
            if !text.eq_ignore_ascii_case("default") && !text.eq_ignore_ascii_case("none") {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidParameters,
                    clipped_session::SessionError::AudioDeviceNotSelectable.to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Sets the recording directory, or clears it.
fn set_recording_directory(
    storage: &mut StorageSettings,
    value: Option<&str>,
) -> Result<(), ProtocolError> {
    storage
        .set_recording_directory(value.map(PathBuf::from))
        .map_err(|error| ProtocolError::new(ErrorCode::InvalidParameters, error.to_string()))
}

/// Every setting, resolved for the global scope, in the order a screen lists
/// them.
fn view_of(configuration: &Configuration, path: &Path) -> SettingsView {
    let resolved = configuration.resolve_global();
    let mut settings: Vec<SettingEntry> = SettingKey::ALL
        .into_iter()
        .map(|key| {
            let unavailable = applies(key).err();
            SettingEntry {
                key: key.name().to_owned(),
                label: key.label().to_owned(),
                value: resolved.written_value(key),
                overridden: resolved.is_overridden(key),
                choices: choices_for(key),
                accepted: accepted_for(key),
                applies: unavailable.is_none(),
                unavailable: unavailable.map(str::to_owned),
            }
        })
        .collect();

    settings.push(recording_directory_entry(configuration.storage()));

    SettingsView {
        file: path.to_string_lossy().into_owned(),
        settings,
    }
}

/// The recording directory's row.
///
/// Its value is a path or nothing, and "nothing" is not blank: it is the
/// recorder's own default, which a window has to be able to show as the answer
/// rather than as an empty field (`crate::config::default_output_directory`).
fn recording_directory_entry(storage: &StorageSettings) -> SettingEntry {
    let configured = storage.recording_directory();
    SettingEntry {
        key: RECORDING_DIRECTORY.to_owned(),
        label: "Recording directory".to_owned(),
        value: configured.map_or_else(
            || {
                crate::config::default_output_directory()
                    .map_or_else(String::new, |path| path.to_string_lossy().into_owned())
            },
            |path| path.to_string_lossy().into_owned(),
        ),
        overridden: configured.is_some(),
        choices: Vec::new(),
        accepted: "a folder on this machine, such as D:\\Clips".to_owned(),
        applies: true,
        unavailable: None,
    }
}

/// The values a setting offers, where the set is closed.
fn choices_for(key: SettingKey) -> Vec<String> {
    match key {
        // Narrower than what the file accepts, deliberately: the file can carry
        // the name of a playback endpoint and this build cannot open one, so
        // offering it would be offering a recording that fails when a game
        // launches (issue #316).
        SettingKey::SystemAudio => vec!["default".to_owned(), "none".to_owned()],
        other => other.choices(),
    }
}

/// What a setting would accept, in the words its refusal uses.
fn accepted_for(key: SettingKey) -> String {
    match key {
        SettingKey::SystemAudio => {
            "\"default\" to record what you hear, or \"none\" to record no system audio".to_owned()
        }
        other => other.accepted(),
    }
}

/// The microphones this machine has.
///
/// # Errors
///
/// [`ErrorCode::Internal`] carrying the reason the endpoints could not be
/// listed, so that a window can say why there is no list rather than drawing an
/// empty one as though it had looked (AGENTS.md section 27).
#[cfg(windows)]
pub fn audio_devices() -> Result<AudioDevices, ProtocolError> {
    let microphones = clipped_session::available_microphones().map_err(|error| {
        ProtocolError::new(
            ErrorCode::Internal,
            format!("this machine's microphones could not be listed: {error}"),
        )
    })?;

    Ok(AudioDevices {
        microphones: microphones
            .into_iter()
            .map(|device| AudioDevice {
                name: device.name,
                is_default: device.is_default,
            })
            .collect(),
    })
}

/// The same, on a build with no audio backend at all.
#[cfg(not(windows))]
pub fn audio_devices() -> Result<AudioDevices, ProtocolError> {
    Err(ProtocolError::new(
        ErrorCode::Internal,
        "this build has no audio capture and cannot list the machine's microphones",
    ))
}

/// How long one level check listens.
///
/// Long enough to be past the first packet of a capture that has just started —
/// WASAPI delivers at around 10 ms and the first one costs the activation — and
/// short enough that a window polling for a meter is not waiting on the recorder
/// (`clipped_session::microphone_level` explains why the device is opened per
/// question rather than held).
#[cfg(windows)]
const LISTEN_FOR: std::time::Duration = std::time::Duration::from_millis(120);

/// What the microphone a settings value names is hearing.
///
/// # Why the value is parsed rather than taken as a device name
///
/// Because the window is asking about a *setting* it has not saved yet, and the
/// settings file's spelling is the only vocabulary either side has for one
/// (`clipped_ipc::settings`). Parsing it through [`Preferences::set_written`] —
/// the same call [`SettingsFile::apply`] makes — means a value this can be asked
/// about is exactly a value that could be saved, refused with the same sentence
/// when it is not, and resolved to a device by the code a recording resolves it
/// with. Three separate ways for the meter to end up pointed at a different
/// endpoint from the recording, closed by using one path (AGENTS.md section 55).
///
/// # Errors
///
/// [`ErrorCode::InvalidParameters`] for a value the settings file would refuse,
/// carrying `clipped_session`'s own sentence, and for `none` — which is a
/// perfectly good setting with no level to report, and must not come back as a
/// reading of silence (AGENTS.md section 27).
///
/// [`ErrorCode::Internal`] when the device cannot be opened: unplugged, in use,
/// or a name that matches nothing on this machine. A window shows the reason
/// rather than a meter at zero, because those are opposite answers.
#[cfg(windows)]
pub fn microphone_level(
    request: &MicrophoneLevelRequest,
) -> Result<MicrophoneLevel, ProtocolError> {
    let mut scratch = Preferences::default();
    scratch
        .set_written(SettingKey::Microphone, Some(&request.microphone))
        .map_err(|error| ProtocolError::new(ErrorCode::InvalidParameters, error.to_string()))?;

    let setting = scratch
        .microphone()
        .cloned()
        .unwrap_or_default()
        .as_source();

    let level = clipped_session::microphone_level(&setting, LISTEN_FOR)
        .map_err(|error| {
            ProtocolError::new(
                ErrorCode::Internal,
                format!("this microphone could not be listened to: {error}"),
            )
        })?
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                "\"none\" records no microphone, so there is no level to report",
            )
        })?;

    Ok(MicrophoneLevel {
        device: level.device,
        peak: level.peak,
        muted: level.muted,
    })
}

/// The same, on a build with no audio backend at all.
#[cfg(not(windows))]
pub fn microphone_level(
    _request: &MicrophoneLevelRequest,
) -> Result<MicrophoneLevel, ProtocolError> {
    Err(ProtocolError::new(
        ErrorCode::Internal,
        "this build has no audio capture and cannot listen to a microphone",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;

    /// A directory of this test's own, removed when it is dropped.
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "clipped-recorder-settings-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("the temporary directory can be created");
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn change(key: &str, value: Option<&str>) -> ApplySettings {
        let mut values = BTreeMap::new();
        values.insert(key.to_owned(), value.map(str::to_owned));
        ApplySettings { values }
    }

    fn entry(view: &SettingsView, key: &str) -> SettingEntry {
        view.settings
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("the view carries {key}"))
            .clone()
    }

    #[test]
    fn a_microphone_picked_in_the_window_is_what_the_next_recording_is_made_with() {
        // The whole of step 3 of SPEC.md section 45, in one test: the window
        // saves a microphone, and the configuration a recording starting after
        // that is made with carries it. Before the settings reached the
        // protocol this could not be asked at all.
        let directory = TestDirectory::new("microphone");
        let settings = SettingsFile::at(directory.file());

        settings
            .apply(&change("microphone", Some("name:Shure MV7")))
            .expect("a device name is a value the file can hold");

        let resolved = settings.configuration().resolve_global();
        assert_eq!(
            resolved.written_value(SettingKey::Microphone),
            "name:Shure MV7",
            "the recording the recorder starts next is not made with what was saved",
        );
        assert!(
            directory.file().exists(),
            "the change has to reach the file, not only this process",
        );
    }

    #[test]
    fn the_recording_directory_is_saved_and_read_back_as_the_directory_that_was_picked() {
        let directory = TestDirectory::new("recordings");
        let settings = SettingsFile::at(directory.file());

        let view = settings
            .apply(&change(RECORDING_DIRECTORY, Some(r"D:\Clips")))
            .expect("an absolute path is accepted");

        assert_eq!(entry(&view, RECORDING_DIRECTORY).value, r"D:\Clips");
        assert!(entry(&view, RECORDING_DIRECTORY).overridden);
        assert_eq!(
            settings
                .configuration()
                .storage()
                .recording_directory()
                .map(Path::to_path_buf),
            Some(PathBuf::from(r"D:\Clips")),
        );
    }

    #[test]
    fn a_directory_that_is_not_absolute_is_refused_with_what_would_have_been_accepted() {
        let directory = TestDirectory::new("relative");
        let settings = SettingsFile::at(directory.file());

        let refusal = settings
            .apply(&change(RECORDING_DIRECTORY, Some("clips")))
            .expect_err("a relative path is refused");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("clips") && refusal.message.contains("absolute"),
            "the refusal should name the value and what would have been accepted: {}",
            refusal.message,
        );
        assert!(
            !directory.file().exists(),
            "a refused change must not have written anything",
        );
    }

    #[test]
    fn a_refused_value_leaves_the_settings_that_were_already_saved_alone() {
        // The half that matters: a screen saving two edits at once, one of
        // which is wrong, must not save half of them.
        let directory = TestDirectory::new("atomic");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change("framerate", Some("120")))
            .expect("120 is in range");

        let mut values = BTreeMap::new();
        values.insert("framerate".to_owned(), Some("60".to_owned()));
        values.insert("resolution".to_owned(), Some("wide".to_owned()));
        let refusal = settings
            .apply(&ApplySettings { values })
            .expect_err("`wide` is not a size");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert_eq!(
            settings
                .configuration()
                .resolve_global()
                .written_value(SettingKey::Framerate),
            "120",
            "the frame rate that was already saved was changed by a request that was refused",
        );
    }

    #[test]
    fn clearing_a_setting_returns_it_to_the_default_and_says_so() {
        let directory = TestDirectory::new("reset");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change("framerate", Some("120")))
            .expect("120 is in range");

        let view = settings
            .apply(&change("framerate", None))
            .expect("clearing is always allowed");

        let framerate = entry(&view, "framerate");
        assert!(
            !framerate.overridden,
            "a setting that was reset is still reported as configured",
        );
        assert_eq!(framerate.value, "60", "and it reads as the shipped default");
    }

    #[test]
    fn a_setting_nothing_reads_is_reported_as_one_rather_than_drawn_as_working() {
        // AGENTS.md section 27, on the wire. The capture target is a key the
        // file carries and no recording acts on, and a window has to be able to
        // tell it from one that works.
        let directory = TestDirectory::new("unavailable");
        let view = SettingsFile::at(directory.file())
            .view()
            .expect("a view of the defaults");

        let capture_target = entry(&view, "capture_target");
        assert!(!capture_target.applies);
        assert!(capture_target
            .unavailable
            .as_deref()
            .is_some_and(|reason| reason.contains("#61")));

        let microphone = entry(&view, "microphone");
        assert!(
            microphone.applies,
            "the microphone is read when a recording starts"
        );
        assert_eq!(microphone.unavailable, None);
    }

    #[test]
    fn a_named_playback_endpoint_is_refused_now_rather_than_when_a_game_launches() {
        // The file can carry it and this build cannot open it, so saving it
        // would be a control that produces a recording which fails later
        // (issue #316). The sentence is the session's own.
        let directory = TestDirectory::new("system-audio");
        let settings = SettingsFile::at(directory.file());

        let refusal = settings
            .apply(&change("system_audio", Some("name:Speakers")))
            .expect_err("a named playback endpoint is refused");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert_eq!(
            refusal.message,
            clipped_session::SessionError::AudioDeviceNotSelectable.to_string(),
        );

        settings
            .apply(&change("system_audio", Some("none")))
            .expect("recording no system audio is always allowed");
    }

    #[test]
    fn a_setting_this_recorder_does_not_have_is_refused_by_name() {
        let directory = TestDirectory::new("unknown");
        let refusal = SettingsFile::at(directory.file())
            .apply(&change("hdr", Some("on")))
            .expect_err("there is no `hdr` setting");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("hdr"),
            "the refusal should name the setting: {}",
            refusal.message,
        );
    }

    #[test]
    fn a_settings_file_a_hand_edit_changed_is_read_before_it_is_written_over() {
        // AGENTS.md section 56: what is on disk may be newer than what this
        // process last saw, and one saved setting must not cost the other.
        let directory = TestDirectory::new("hand-edited");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change("framerate", Some("120")))
            .expect("120 is in range");

        fs::write(
            directory.file(),
            r#"{"version": 1, "global": {"framerate": 120, "codec": "hevc"}}"#,
        )
        .expect("the file can be edited by hand");

        let view = settings
            .apply(&change("microphone", Some("none")))
            .expect("the edit applies on top of what is on disk");

        assert_eq!(
            entry(&view, "codec").value,
            "hevc",
            "the setting somebody wrote by hand was overwritten by this process's copy",
        );
        assert_eq!(entry(&view, "microphone").value, "none");
    }

    #[test]
    fn there_being_nowhere_to_keep_settings_is_said_rather_than_pretended_about() {
        let refusal = SettingsFile::nowhere()
            .view()
            .expect_err("there is nowhere to read from");

        assert_eq!(refusal.code, ErrorCode::Internal);
        assert!(
            refusal.message.contains("nowhere to keep settings"),
            "the refusal should say what is wrong: {}",
            refusal.message,
        );
        assert_eq!(
            SettingsFile::nowhere().configuration(),
            Configuration::defaults(),
            "and the recorder still records, at the settings Clipped ships with",
        );
    }

    #[test]
    fn every_setting_the_view_offers_is_one_the_same_view_will_accept() {
        // The list a screen draws its options from. An option the setter then
        // refuses is a control that fails when it is used.
        let directory = TestDirectory::new("choices");
        let settings = SettingsFile::at(directory.file());
        let view = settings.view().expect("a view of the defaults");

        for entry in &view.settings {
            for choice in &entry.choices {
                settings
                    .apply(&change(&entry.key, Some(choice)))
                    .unwrap_or_else(|error| {
                        panic!(
                            "{} offers {choice} and refuses it: {}",
                            entry.key, error.message
                        )
                    });
            }
            assert!(
                !entry.accepted.is_empty(),
                "{} says nothing about what it would accept",
                entry.key,
            );
        }
    }
}
