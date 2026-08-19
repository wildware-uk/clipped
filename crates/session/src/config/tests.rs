//! Inheritance, validation and migration, exercised end to end.
//!
//! The tests that matter most here are the ones about *unset*: a per-game
//! setting that is not set has to keep following the global one, and a per-game
//! setting that happens to equal the global one has to stop. Every other
//! property of this module is downstream of that distinction.

use core::time::Duration;
use std::fs;
use std::path::{Path, PathBuf};

use clipped_encoder::{Codec, EncoderKind};
use clipped_hotkeys::HotkeyAction;

use super::*;
use crate::settings::{
    AudioSourceSetting, CaptureTargetSettings, RecordingSettings, UnavailableChoice,
};

/// A directory of this test's own, removed when it is dropped.
///
/// The workspace has no `tempfile` dependency and this is not enough reason to
/// add one; `crates/session/src/automatic/tests.rs` and `crates/logging` build
/// the same thing from `std::env::temp_dir` and the process id (AGENTS.md
/// sections 10 and 55).
#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-config-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory can be created");
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join(FILE_NAME)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn game(name: &str) -> GameKey {
    GameKey::parse(name).expect("a game identifier this test writes down")
}

fn framerate(value: u32) -> Preferences {
    let mut preferences = Preferences::none();
    preferences
        .set_framerate(Some(value))
        .expect("a frame rate in range");
    preferences
}

/// AGENTS.md section 30's worked example, as a configuration.
fn worked_example() -> Configuration {
    let mut configuration = Configuration::defaults();
    configuration.set_global(framerate(60));
    configuration.set_game(game("counter-strike-2"), framerate(120));
    configuration
}

#[test]
fn a_game_that_overrides_gets_its_own_value_and_one_that_does_not_inherits() {
    // "Global: 60 FPS. Counter-Strike 2: 120 FPS. Minecraft: inherits 60 FPS."
    let configuration = worked_example();

    let counter_strike = configuration.resolve_for(&game("counter-strike-2"));
    assert_eq!(counter_strike.framerate().get(), 120);
    assert_eq!(counter_strike.framerate().source(), SettingSource::Game);
    assert!(counter_strike.framerate().is_overridden());

    let minecraft = configuration.resolve_for(&game("minecraft"));
    assert_eq!(minecraft.framerate().get(), 60);
    assert_eq!(minecraft.framerate().source(), SettingSource::Global);
    assert!(minecraft.framerate().is_inherited());
    assert!(!minecraft.framerate().is_overridden());
}

#[test]
fn a_setting_nobody_configured_comes_from_the_default() {
    let configuration = Configuration::defaults();
    let resolved = configuration.resolve_for(&game("minecraft"));

    assert_eq!(resolved.framerate().get(), crate::DEFAULT_FRAMERATE);
    assert_eq!(resolved.framerate().source(), SettingSource::Default);
    assert!(!resolved.framerate().is_overridden());
    assert_eq!(*resolved.codec().value(), CodecPreference::Automatic);
    assert_eq!(*resolved.encoder().value(), EncoderPreference::Automatic);
    assert_eq!(*resolved.resolution().value(), ResolutionSetting::Source);
    assert_eq!(
        *resolved.capture_target().value(),
        CaptureTargetSetting::GameWindow
    );
    assert_eq!(*resolved.microphone().value(), AudioDeviceSetting::Default);
    assert_eq!(*resolved.replay_window().value(), DEFAULT_REPLAY_WINDOW);
}

#[test]
fn setting_a_game_to_the_global_value_is_not_the_same_as_leaving_it_unset() {
    // The distinction the whole ticket turns on. Both games read 60 today;
    // only the one that never set it follows the global to 90.
    let mut configuration = Configuration::defaults();
    configuration.set_global(framerate(60));
    configuration.set_game(game("counter-strike-2"), framerate(60));
    configuration.set_game(game("minecraft"), Preferences::none());

    assert_eq!(
        configuration
            .resolve_for(&game("counter-strike-2"))
            .framerate()
            .get(),
        60
    );
    assert_eq!(
        configuration
            .resolve_for(&game("minecraft"))
            .framerate()
            .get(),
        60
    );
    assert!(configuration
        .resolve_for(&game("counter-strike-2"))
        .framerate()
        .is_overridden());
    assert!(!configuration
        .resolve_for(&game("minecraft"))
        .framerate()
        .is_overridden());

    configuration.set_global(framerate(90));

    assert_eq!(
        configuration
            .resolve_for(&game("counter-strike-2"))
            .framerate()
            .get(),
        60,
        "a game that pinned 60 keeps 60 when the global moves"
    );
    assert_eq!(
        configuration
            .resolve_for(&game("minecraft"))
            .framerate()
            .get(),
        90,
        "a game that never set a frame rate follows the global one"
    );
}

#[test]
fn clearing_an_override_returns_the_game_to_inheriting() {
    // What the settings screen's Reset does.
    let mut configuration = worked_example();
    let counter_strike = game("counter-strike-2");

    let mut preferences = configuration
        .game(&counter_strike)
        .expect("the game has settings")
        .clone();
    assert!(preferences.is_set(SettingKey::Framerate));
    preferences.clear(SettingKey::Framerate);
    assert!(!preferences.is_set(SettingKey::Framerate));
    assert!(
        preferences.is_empty(),
        "the frame rate was the only thing this game said, so it now says nothing"
    );
    configuration.set_game(counter_strike.clone(), preferences);

    let resolved = configuration.resolve_for(&counter_strike);
    assert_eq!(resolved.framerate().get(), 60);
    assert_eq!(resolved.framerate().source(), SettingSource::Global);
}

#[test]
fn the_global_page_never_shows_a_value_a_game_set() {
    // Resolving the global scope with a game's overrides in the configuration
    // must ignore them, or editing the global settings would show one game's
    // number.
    let configuration = worked_example();
    let global = configuration.resolve_global();

    assert_eq!(global.framerate().get(), 60);
    assert_eq!(global.framerate().source(), SettingSource::Global);
    assert!(
        global.framerate().is_overridden(),
        "on the global page, a value the global settings hold is what Reset clears"
    );
    assert_eq!(global.scope(), &Scope::Global);
}

#[test]
fn a_global_value_is_not_an_override_when_the_default_supplied_it() {
    let global = Configuration::defaults().resolve_global();
    assert_eq!(global.framerate().source(), SettingSource::Default);
    assert!(!global.framerate().is_overridden());
}

#[test]
fn the_source_of_every_setting_can_be_read_without_naming_its_type() {
    // What a settings screen loops over to draw the "inherited" badges.
    let mut counter_strike = Preferences::none();
    counter_strike.set_framerate(Some(120)).expect("in range");
    let mut configuration = Configuration::defaults();
    configuration.set_global(framerate(60));
    configuration.set_game(game("counter-strike-2"), counter_strike);

    let resolved = configuration.resolve_for(&game("counter-strike-2"));
    assert_eq!(
        resolved.source_of(SettingKey::Framerate),
        SettingSource::Game
    );
    assert!(resolved.is_overridden(SettingKey::Framerate));
    assert_eq!(
        resolved.source_of(SettingKey::Codec),
        SettingSource::Default
    );
    assert!(!resolved.is_overridden(SettingKey::Codec));
    for key in SettingKey::ALL {
        // Every setting answers, so a new one cannot be added to the model and
        // forgotten by the screen.
        let _ = resolved.source_of(key);
    }
}

// ---------------------------------------------------------------- validation

#[test]
fn a_frame_rate_outside_the_range_is_refused_and_the_bound_is_in_the_message() {
    let mut preferences = framerate(60);
    for rejected in [0, MAXIMUM_FRAMERATE + 1] {
        let error = preferences
            .set_framerate(Some(rejected))
            .expect_err("outside the accepted range");
        let message = error.to_string();
        assert!(
            message.contains("framerate")
                && message.contains(&rejected.to_string())
                && message.contains(&MAXIMUM_FRAMERATE.to_string()),
            "the refusal must name the setting, the value and the range: {message}"
        );
    }
    assert_eq!(
        preferences.framerate(),
        Some(60),
        "a refused value must leave the previous one standing"
    );
}

#[test]
fn a_resolution_a_codec_cannot_encode_is_refused() {
    let mut preferences = Preferences::none();

    let odd = preferences
        .set_resolution(Some(ResolutionSetting::Fixed {
            width: 1921,
            height: 1080,
        }))
        .expect_err("4:2:0 chroma has no half samples");
    assert!(
        odd.to_string().contains("even"),
        "the refusal must say why: {odd}"
    );

    let tiny = preferences
        .set_resolution(Some(ResolutionSetting::Fixed {
            width: 64,
            height: 64,
        }))
        .expect_err("below the minimum dimension");
    let message = tiny.to_string();
    assert!(
        message.contains(&MINIMUM_DIMENSION.to_string())
            && message.contains(&MAXIMUM_DIMENSION.to_string()),
        "the refusal must name the range: {message}"
    );

    assert_eq!(preferences.resolution(), None);
}

#[test]
fn a_replay_window_the_buffer_would_refuse_is_refused_here_too() {
    // The buffer already rejects these (`clipped_replay::ReplayConfig::new`).
    // Catching it here is what puts the message in front of the user at the
    // moment they set it, rather than when a game launches.
    let mut preferences = Preferences::none();
    for rejected in [Duration::from_secs(5), Duration::from_secs(60 * 60)] {
        let error = preferences
            .set_replay_window(Some(rejected))
            .expect_err("outside the buffer's range");
        let message = error.to_string();
        assert!(
            message.contains(&clipped_replay::MINIMUM_WINDOW.as_secs().to_string())
                && message.contains(&clipped_replay::MAXIMUM_WINDOW.as_secs().to_string()),
            "the refusal must name the range the buffer accepts: {message}"
        );
    }
    preferences
        .set_replay_window(Some(clipped_replay::MINIMUM_WINDOW))
        .expect("the shortest window the buffer accepts");
}

/// Everything a setter accepts, saved and read back through the store.
///
/// The property the whole layer rests on: a `Preferences` this build will hold
/// is one this build can write and read again. Returning the configuration
/// rather than asserting inside lets each caller say what it expected to
/// survive.
fn round_trip(label: &str, configuration: &Configuration) -> Configuration {
    let directory = TestDirectory::new(label);
    let mut writer = ConfigurationStore::at(directory.file());
    writer
        .store(configuration.clone())
        .unwrap_or_else(|error| panic!("{label} could not be saved: {error}"));

    let mut reader = ConfigurationStore::at(directory.file());
    reader
        .load()
        .unwrap_or_else(|error| panic!("{label} could not be read back: {error}"));
    reader.current().clone()
}

#[test]
fn every_device_name_a_setter_accepts_survives_the_writer_and_the_reader() {
    // `AudioDeviceSetting::Named` is a public variant, so a settings screen can
    // build one without going through `named`. If the setter did not check it,
    // a name the writer renders as something the reader refuses would leave the
    // user with a settings file their own build cannot open — and, since a file
    // this build cannot read is never overwritten, unable to save again.
    let names = [
        "Shure MV7".to_owned(),
        // The two words that mean something else in the file, and the escape
        // that makes them literal.
        "none".to_owned(),
        "default".to_owned(),
        "name:none".to_owned(),
        // Not trimmed: what the user typed is what is matched against the
        // endpoint list.
        "  Shure  ".to_owned(),
        "Микрофон".to_owned(),
        "a".repeat(MAXIMUM_DEVICE_NAME),
    ];

    for name in names {
        let mut global = Preferences::none();
        global
            .set_microphone(Some(AudioDeviceSetting::Named(name.clone())))
            .unwrap_or_else(|error| panic!("{name:?} should be a device name: {error}"));
        global
            .set_system_audio(Some(AudioDeviceSetting::Named(name.clone())))
            .unwrap_or_else(|error| panic!("{name:?} should be a device name: {error}"));
        let mut configuration = Configuration::defaults();
        configuration.set_global(global);

        assert_eq!(
            round_trip("device-round-trip", &configuration),
            configuration,
            "a microphone called {name:?} did not survive being saved and read"
        );
    }
}

#[test]
fn a_device_name_the_file_could_not_hold_is_refused_by_the_setter() {
    // The same rule `AudioDeviceSetting::named` applies, reached the other way.
    let mut preferences = Preferences::none();
    preferences
        .set_microphone(Some(AudioDeviceSetting::Default))
        .expect("the default endpoint is always allowed");

    for rejected in ["", "   ", "head\nset", &"a".repeat(MAXIMUM_DEVICE_NAME + 1)] {
        let error = preferences
            .set_microphone(Some(AudioDeviceSetting::Named(rejected.to_owned())))
            .expect_err("the writer could not render that so the reader could read it");
        assert!(
            error.to_string().contains("microphone"),
            "the refusal must name which of the two audio settings it is about: {error}"
        );
        preferences
            .set_system_audio(Some(AudioDeviceSetting::Named(rejected.to_owned())))
            .expect_err("system audio takes the same names");
    }
    assert_eq!(
        preferences.microphone(),
        Some(&AudioDeviceSetting::Default),
        "a refused device must leave the previous selection standing"
    );
    assert_eq!(preferences.system_audio(), None);
}

#[test]
fn a_replay_buffer_can_be_declined_and_the_file_gives_that_answer_back() {
    // Issue #539's whole point. `replay_window_seconds` accepted 30 to 1800 and
    // nothing else, so the nearest thing to "no buffer" was a thirty-second one
    // that still spills at the recording's own bitrate — and every recording
    // the desktop window starts asks for a buffer. This is the value that says
    // no, and it goes through the file because a settings screen is not the
    // only way somebody sets one.
    let mut preferences = Preferences::none();
    preferences
        .set_replay_window(Some(REPLAY_WINDOW_OFF))
        .expect("declining the replay buffer is a thing a user may do");

    let mut configuration = Configuration::defaults();
    configuration.set_global(preferences);
    assert_eq!(round_trip("replay-off", &configuration), configuration);

    // And it is a *number* in the file, not a word: the key holds seconds, so
    // the off value is spelled the way the rest of its range is. Writing
    // "none" here would give one key two types.
    let directory = TestDirectory::new("replay-off-text");
    ConfigurationStore::at(directory.file())
        .store(configuration.clone())
        .expect("the settings file can be written");
    let written = fs::read_to_string(directory.file()).expect("the settings file can be read");
    assert!(
        written.contains(r#""replay_window_seconds": 0"#),
        "the off value is the number zero in the file: {written}"
    );

    // Zero is an answer and *absent* is the inherit. They resolve differently
    // and report different sources, which is what stops "off" being mistaken
    // for "unset" (AGENTS.md section 30).
    let resolved = configuration.resolve_global();
    assert_eq!(*resolved.replay_window().value(), REPLAY_WINDOW_OFF);
    assert_eq!(
        resolved.source_of(SettingKey::ReplayWindow),
        SettingSource::Global
    );
    assert_eq!(
        resolved.replay_buffer_window(),
        None,
        "a recording made with these settings keeps no buffer at all"
    );

    let untouched = Configuration::defaults().resolve_global();
    assert_eq!(
        untouched.source_of(SettingKey::ReplayWindow),
        SettingSource::Default,
        "a file that says nothing about the window is not a file that turned it off"
    );
    assert_eq!(
        untouched.replay_buffer_window(),
        Some(DEFAULT_REPLAY_WINDOW),
        "and it still keeps the five minutes Clipped ships with"
    );
}

#[test]
fn a_settings_file_from_a_build_that_had_no_off_value_still_reads() {
    // AGENTS.md section 56. Widening a range must not make an existing file
    // unreadable, and the two shapes a build without the off value could have
    // left behind are a window inside the old range and no key at all.
    let directory = TestDirectory::new("replay-older-build");
    fs::write(
        directory.file(),
        r#"{
  "version": 1,
  "global": { "replay_window_seconds": 300, "framerate": 144 },
  "games": { "counter-strike-2": { "resolution": "1920x1080" } }
}"#,
    )
    .expect("a settings file this test wrote");

    let mut store = ConfigurationStore::at(directory.file());
    store
        .load()
        .expect("a file written before the off value existed is still one this build reads");
    let resolved = store.current().resolve_for(&game("counter-strike-2"));
    assert_eq!(
        resolved.replay_buffer_window(),
        Some(Duration::from_secs(300)),
        "the window an older build wrote is still the window this one keeps"
    );
    assert_eq!(*resolved.framerate().value(), 144);
    assert_eq!(
        store
            .current()
            .resolve_for(&game("minecraft"))
            .replay_buffer_window(),
        Some(Duration::from_secs(300)),
        "and a game the file never mentions still inherits it"
    );
}

#[test]
fn declining_the_replay_buffer_inherits_per_game_in_both_directions() {
    // AGENTS.md section 30's worked example, for the setting that has just
    // gained an off value: a game may keep a buffer on a machine that globally
    // declines one, and may decline one on a machine that globally keeps one.
    // Neither is special-cased — both fall out of the same fold every other
    // setting uses, which is the point of not inventing machinery for this key.
    let mut off = Preferences::none();
    off.set_replay_window(Some(REPLAY_WINDOW_OFF))
        .expect("the off value");
    let mut two_minutes = Preferences::none();
    two_minutes
        .set_replay_window(Some(Duration::from_secs(120)))
        .expect("two minutes is in range");

    let mut globally_off = Configuration::defaults();
    globally_off.set_global(off.clone());
    globally_off.set_game(game("counter-strike-2"), two_minutes.clone());
    assert_eq!(
        globally_off
            .resolve_for(&game("counter-strike-2"))
            .replay_buffer_window(),
        Some(Duration::from_secs(120)),
        "a game that asked for a buffer gets one however the global layer reads"
    );
    assert_eq!(
        globally_off
            .resolve_for(&game("minecraft"))
            .replay_buffer_window(),
        None,
        "and a game with no section of its own inherits the refusal"
    );

    let mut globally_on = Configuration::defaults();
    globally_on.set_global(two_minutes);
    globally_on.set_game(game("counter-strike-2"), off);
    assert_eq!(
        globally_on
            .resolve_for(&game("counter-strike-2"))
            .replay_buffer_window(),
        None,
        "and one game may decline the buffer without the others losing theirs"
    );
    assert_eq!(
        globally_on
            .resolve_for(&game("minecraft"))
            .replay_buffer_window(),
        Some(Duration::from_secs(120)),
    );
}

#[test]
fn the_off_value_travels_as_text_the_settings_screen_can_send_and_read_back() {
    // The settings screen sets values as the text the file spells them with
    // (`Preferences::set_written`), so an off value the file understands and
    // the screen cannot send would be a control that exists in one place only.
    let mut preferences = Preferences::none();
    preferences
        .set_written(SettingKey::ReplayWindow, Some("0"))
        .expect("the screen can send the off value");
    assert_eq!(preferences.replay_window(), Some(REPLAY_WINDOW_OFF));

    let mut configuration = Configuration::defaults();
    configuration.set_global(preferences);
    assert_eq!(
        configuration
            .resolve_global()
            .written_value(SettingKey::ReplayWindow),
        "0",
        "and reads it back as the same text, so the field shows what was set"
    );

    // What somebody who typed a number in between is told: the refusal names
    // the off value as well as the range, because "as little as possible" is
    // usually what they were reaching for.
    let error = Preferences::none()
        .set_replay_window(Some(Duration::from_secs(5)))
        .expect_err("five seconds is neither off nor a window a buffer will take");
    let message = error.to_string();
    assert!(
        message.contains("0 to keep no replay buffer"),
        "the refusal has to offer the way out: {message}"
    );
    assert!(
        message.contains("30") && message.contains("1800"),
        "and still name the range a buffer accepts: {message}"
    );
}

#[test]
fn a_replay_window_the_file_could_not_hold_exactly_is_refused() {
    // `replay_window_seconds` is whole seconds. Accepting half of one would
    // mean a setting that came back from the file as something other than what
    // was set, which is a value silently changing itself.
    let mut preferences = Preferences::none();
    let error = preferences
        .set_replay_window(Some(Duration::from_millis(30_500)))
        .expect_err("the file holds whole seconds");
    assert!(
        error.to_string().contains("whole number of seconds"),
        "the refusal must say why a value inside the range was refused: {error}"
    );
    assert_eq!(preferences.replay_window(), None);

    preferences
        .set_replay_window(Some(Duration::from_secs(31)))
        .expect("a whole number of seconds inside the range");
    let mut configuration = Configuration::defaults();
    configuration.set_global(preferences);
    assert_eq!(
        round_trip("window-round-trip", &configuration),
        configuration
    );
}

#[test]
fn an_audio_device_name_that_cannot_be_shown_is_refused() {
    for rejected in ["", "   ", "head\nset"] {
        assert!(
            AudioDeviceSetting::named(SettingKey::Microphone, rejected).is_err(),
            "{rejected:?} should not be a device name"
        );
    }
    let long = "a".repeat(MAXIMUM_DEVICE_NAME + 1);
    let error =
        AudioDeviceSetting::named(SettingKey::Microphone, long).expect_err("longer than the limit");
    assert!(
        error.to_string().contains(&MAXIMUM_DEVICE_NAME.to_string()),
        "the refusal must name the limit: {error}"
    );
    assert_eq!(
        AudioDeviceSetting::named(SettingKey::Microphone, "Shure MV7"),
        Ok(AudioDeviceSetting::Named("Shure MV7".to_owned()))
    );
}

// --------------------------------------------------------------- the file

/// A configuration exercising every setting, so that a round trip proves the
/// reader and the writer agree about all of them.
fn fully_populated() -> Configuration {
    let mut global = Preferences::none();
    global.set_capture_target(Some(CaptureTargetSetting::Display));
    global
        .set_resolution(Some(ResolutionSetting::Fixed {
            width: 1920,
            height: 1080,
        }))
        .expect("a size in range");
    global.set_framerate(Some(60)).expect("in range");
    global.set_codec(Some(CodecPreference::Fixed(Codec::Av1)));
    global.set_encoder(Some(EncoderPreference::Fixed(EncoderKind::Nvenc)));
    global
        .set_microphone(Some(
            AudioDeviceSetting::named(SettingKey::Microphone, "Shure MV7").expect("a real name"),
        ))
        .expect("a real name");
    global
        .set_system_audio(Some(AudioDeviceSetting::Disabled))
        .expect("recording nothing is always allowed");
    global
        .set_replay_window(Some(Duration::from_secs(120)))
        .expect("in range");

    let mut configuration = Configuration::defaults();
    configuration.set_global(global);
    configuration.set_game(game("counter-strike-2"), framerate(120));

    let mut hotkeys = HotkeyOverrides::none();
    hotkeys
        .set(HotkeyAction::AddBookmark, Some(HotkeyOverride::Unbound))
        .expect("unbinding is allowed");
    hotkeys
        .set(
            HotkeyAction::TakeScreenshot,
            Some(HotkeyOverride::Bound(
                "Ctrl+F8".parse().expect("a combination"),
            )),
        )
        .expect("Ctrl+F8 is nobody's");
    configuration.set_hotkeys(hotkeys);
    configuration
}

/// One value per setting that is *not* the shipped default, written the way
/// the file writes it.
///
/// Not a list of expected outputs: each of these goes in through
/// `Preferences::set_written` and comes back out through `written_value`, and
/// the test asserts the two are the same text rather than that either equals
/// something computed by hand from the implementation.
fn a_written_value_for(key: SettingKey) -> String {
    match key {
        SettingKey::CaptureTarget => "display".to_owned(),
        SettingKey::Resolution => "1280x720".to_owned(),
        SettingKey::Framerate => "120".to_owned(),
        SettingKey::Codec => "hevc".to_owned(),
        SettingKey::Encoder => "nvenc".to_owned(),
        SettingKey::Microphone => "name:Shure MV7".to_owned(),
        SettingKey::SystemAudio => "none".to_owned(),
        SettingKey::ReplayWindow => "600".to_owned(),
    }
}

#[test]
fn every_setting_goes_in_and_comes_back_out_as_the_same_words() {
    // What the settings screen does: it is handed a setting's value as text
    // over the control protocol, sends the edited text back, and draws whatever
    // comes next. A setting whose value changed spelling in transit would show
    // the user something other than what they chose, and a setting that could
    // not be set from text at all would be a control that does nothing.
    for key in SettingKey::ALL {
        let written = a_written_value_for(key);

        let mut global = Preferences::none();
        global
            .set_written(key, Some(&written))
            .unwrap_or_else(|error| panic!("{key} accepts {written}: {error}"));
        let mut configuration = Configuration::defaults();
        configuration.set_global(global);

        let resolved = configuration.resolve_global();
        assert_eq!(
            resolved.written_value(key),
            written,
            "{key} came back spelled differently from the way it went in",
        );
        assert!(
            resolved.is_overridden(key),
            "{key} was set from text and does not count as set",
        );
    }
}

#[test]
fn every_choice_a_setting_offers_is_one_it_accepts() {
    // The list a screen draws as a set of options. An option that the setter
    // then refuses is a control that fails when it is used, which is worse than
    // one that is not offered.
    for key in SettingKey::ALL {
        for choice in key.choices() {
            let mut global = Preferences::none();
            global
                .set_written(key, Some(&choice))
                .unwrap_or_else(|error| panic!("{key} offers {choice} and refuses it: {error}"));

            let mut configuration = Configuration::defaults();
            configuration.set_global(global);
            assert_eq!(
                configuration.resolve_global().written_value(key),
                choice,
                "{key}'s choice {choice} is not the value it becomes",
            );
        }
        assert!(
            !key.accepted().is_empty(),
            "{key} says nothing about what it would accept",
        );
    }
}

#[test]
fn clearing_a_setting_from_text_returns_it_to_the_default() {
    // Reset. `None` is the absence of a value, not the default written out: a
    // setting reset here has to start following the layer below it again.
    let mut global = Preferences::none();
    global
        .set_written(SettingKey::Framerate, Some("120"))
        .expect("120 is in range");
    global
        .set_written(SettingKey::Framerate, None)
        .expect("clearing is always allowed");

    let mut configuration = Configuration::defaults();
    configuration.set_global(global);

    let resolved = configuration.resolve_global();
    assert_eq!(
        resolved.source_of(SettingKey::Framerate),
        SettingSource::Default
    );
    assert!(!resolved.is_overridden(SettingKey::Framerate));
}

#[test]
fn a_value_set_from_text_is_refused_exactly_as_the_file_would_refuse_it() {
    // One set of rules. The settings screen must not be able to save a value
    // that the same text typed into settings.json would be refused for, and the
    // refusal it shows must be the one the file's reader gives (AGENTS.md
    // section 55).
    let mut global = Preferences::none();
    let refused = global
        .set_written(SettingKey::Framerate, Some("900"))
        .expect_err("900 frames per second is outside the range");

    let directory = TestDirectory::new("same-refusal");
    fs::write(
        directory.file(),
        r#"{"version": 1, "global": {"framerate": 900}}"#,
    )
    .expect("the fixture can be written");
    let from_the_file = ConfigurationStore::at(directory.file())
        .load()
        .expect_err("the same value in the file is refused too");

    assert!(
        from_the_file.to_string().contains(&refused.to_string()),
        "the file said `{from_the_file}` and the setter said `{refused}`",
    );
    assert!(
        refused
            .to_string()
            .contains(&SettingKey::Framerate.accepted()),
        "the refusal should say what would have been accepted: {refused}",
    );
}

#[test]
fn every_setting_survives_being_written_and_read_back() {
    let directory = TestDirectory::new("round-trip");
    let mut store = ConfigurationStore::at(directory.file());
    let written = fully_populated();
    store
        .store(written.clone())
        .expect("the file can be written");

    let mut reader = ConfigurationStore::at(directory.file());
    assert_eq!(reader.load().expect("the file reads"), Loaded::AsWritten);
    assert_eq!(reader.current(), &written);

    // Named explicitly as well, so that a round trip which lost a setting by
    // dropping it from *both* halves would still fail.
    for key in SettingKey::ALL {
        assert!(
            reader.current().global().is_set(key),
            "{key} did not survive the round trip"
        );
    }
}

#[test]
fn a_device_called_none_survives_being_saved() {
    // The escape the `name:` prefix exists for. Without it a headset genuinely
    // called "none" would be read back as "record nothing".
    let directory = TestDirectory::new("literal-device");
    let mut global = Preferences::none();
    global
        .set_microphone(Some(
            AudioDeviceSetting::named(SettingKey::Microphone, "none").expect("a real name"),
        ))
        .expect("a real name");
    let mut configuration = Configuration::defaults();
    configuration.set_global(global);

    let mut store = ConfigurationStore::at(directory.file());
    store.store(configuration).expect("written");
    let mut reader = ConfigurationStore::at(directory.file());
    reader.load().expect("read");
    assert_eq!(
        reader.current().global().microphone(),
        Some(&AudioDeviceSetting::Named("none".to_owned()))
    );
}

#[test]
fn an_absent_file_is_the_defaults_and_not_a_failure() {
    let directory = TestDirectory::new("absent");
    let mut store = ConfigurationStore::at(directory.file());
    assert_eq!(
        store.load().expect("an absent file is fine"),
        Loaded::Absent
    );
    assert_eq!(store.current(), &Configuration::defaults());
    assert!(
        !directory.file().exists(),
        "reading must not create a settings file"
    );
}

#[test]
fn a_setting_written_as_null_means_unset_rather_than_empty() {
    // What a settings screen writes when the user presses Reset. Reading it as
    // "set to nothing" would make Reset unrepresentable in the file.
    let directory = TestDirectory::new("null-is-unset");
    fs::write(
        directory.file(),
        r#"{"version":1,"global":{"framerate":null}}"#,
    )
    .expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    store
        .load()
        .expect("null is a value the reader understands");
    assert!(!store.current().global().is_set(SettingKey::Framerate));
    assert_eq!(
        store.current().resolve_global().framerate().source(),
        SettingSource::Default
    );
}

#[test]
fn an_invalid_file_is_refused_with_a_message_and_the_previous_settings_stand() {
    let directory = TestDirectory::new("invalid");
    let mut store = ConfigurationStore::at(directory.file());
    store.store(worked_example()).expect("written");
    let before = store.current().clone();
    let good_text = fs::read_to_string(directory.file()).expect("the file reads");

    fs::write(
        directory.file(),
        r#"{"version":1,"global":{"framerate":0}}"#,
    )
    .expect("the file can be written");

    let error = store.load().expect_err("0 frames per second is not a rate");
    let message = error.to_string();
    assert!(
        message.contains("framerate")
            && message.contains("global")
            && message.contains(&MAXIMUM_FRAMERATE.to_string())
            && message.contains(FILE_NAME),
        "the refusal must name the file, the section, the setting and the range: {message}"
    );
    assert_eq!(
        store.current(),
        &before,
        "a rejected file must leave the configuration in force untouched"
    );
    assert_ne!(
        fs::read_to_string(directory.file()).expect("the file reads"),
        good_text,
        "nothing may rewrite the user's file behind their back"
    );
}

#[test]
fn a_file_that_is_not_json_is_refused_with_the_line_it_broke_on() {
    let directory = TestDirectory::new("syntax");
    let mut store = ConfigurationStore::at(directory.file());
    store.store(worked_example()).expect("written");
    let before = store.current().clone();

    fs::write(directory.file(), "{\n  \"version\": 1,\n  oops\n}")
        .expect("the file can be written");

    let error = store.load().expect_err("that is not JSON");
    let message = error.to_string();
    assert!(
        message.contains("line 3") && message.contains(FILE_NAME),
        "the refusal must say where to look: {message}"
    );
    assert_eq!(store.current(), &before);
}

#[test]
fn a_hotkey_conflict_in_a_file_is_refused() {
    let directory = TestDirectory::new("hotkey-conflict");
    fs::write(
        directory.file(),
        r#"{"version":1,"hotkeys":{"save_replay":"Ctrl+F10","take_screenshot":"Ctrl+F10"}}"#,
    )
    .expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    let error = store
        .load()
        .expect_err("one combination cannot serve two actions");
    let message = error.to_string();
    assert!(
        message.contains("Ctrl+F10") && message.contains("hotkeys"),
        "the refusal must name the combination: {message}"
    );
    assert_eq!(store.current(), &Configuration::defaults());
}

#[test]
fn a_file_from_a_newer_build_is_refused_rather_than_rewritten() {
    // The version that has to survive: a user with two machines, one ahead of
    // the other. Rewriting the file at this version would throw away whatever
    // the newer build had stored (AGENTS.md section 56).
    let directory = TestDirectory::new("newer");
    let text = format!(
        r#"{{"version":{},"global":{{"framerate":120}}}}"#,
        SCHEMA_VERSION + 1
    );
    fs::write(directory.file(), &text).expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    let error = store
        .load()
        .expect_err("this build is too old for that file");
    let message = error.to_string();
    assert!(
        message.contains(&(SCHEMA_VERSION + 1).to_string())
            && message.contains(&SCHEMA_VERSION.to_string())
            && message.to_lowercase().contains("update"),
        "the refusal must say what to do about it: {message}"
    );
    assert_eq!(
        fs::read_to_string(directory.file()).expect("the file reads"),
        text,
        "the newer build's file must be left exactly as it was"
    );
    assert_eq!(store.current(), &Configuration::defaults());
}

#[test]
fn saving_over_a_newer_builds_file_is_refused_and_the_file_survives() {
    // The whole point of refusing to *read* a newer file is that the settings
    // in it survive. They only survive if the next save refuses too: a user
    // whose other machine is a version ahead opens the settings here, changes
    // one thing, and the newer build's keys would otherwise be gone (AGENTS.md
    // section 56).
    let directory = TestDirectory::new("newer-save");
    let text = format!(
        r#"{{"version":{},"global":{{"framerate":120}},"vault":{{"remote":"nas"}}}}"#,
        SCHEMA_VERSION + 1
    );
    fs::write(directory.file(), &text).expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    store
        .load()
        .expect_err("this build is too old for that file");

    let error = store
        .store(worked_example())
        .expect_err("a file this build cannot read must not be overwritten");
    let message = error.to_string();
    assert!(
        message.contains(&(SCHEMA_VERSION + 1).to_string())
            && message.to_lowercase().contains("not saved"),
        "the refusal must say nothing was saved, and why: {message}"
    );
    assert_eq!(
        fs::read_to_string(directory.file()).expect("the file reads"),
        text,
        "the newer build's file must be exactly as it was"
    );
    assert_eq!(
        store.current(),
        &Configuration::defaults(),
        "a refused save must leave the configuration in force standing"
    );
}

#[test]
fn saving_over_an_unreadable_file_is_refused_even_when_it_was_never_read() {
    // The store does not have to have been asked to `load` for the file to be
    // somebody's. A caller that constructs a store and saves immediately must
    // not be the way a newer build's settings are lost.
    let directory = TestDirectory::new("never-read");
    let text = format!(r#"{{"version":{}}}"#, SCHEMA_VERSION + 1);
    fs::write(directory.file(), &text).expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    store
        .store(worked_example())
        .expect_err("an unread file is still the user's");
    assert_eq!(
        fs::read_to_string(directory.file()).expect("the file reads"),
        text
    );
}

#[test]
fn saving_over_a_file_that_is_not_json_is_refused_and_says_what_to_do() {
    // Not JSON is the other way a file becomes unreadable, and the same rule
    // applies: what is in it may be a newer build's, or a user's own editing,
    // and this build cannot tell. The message has to leave them somewhere to
    // go (AGENTS.md section 45).
    let directory = TestDirectory::new("syntax-save");
    let text = "{\n  \"version\": 1,\n  oops\n}";
    fs::write(directory.file(), text).expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    let error = store
        .store(worked_example())
        .expect_err("this build cannot read what is already there");
    let message = error.to_string();
    assert!(
        message.contains("line 3") && message.contains(FILE_NAME),
        "the refusal must name the file and where it broke: {message}"
    );
    assert!(
        message.contains("move it aside"),
        "the refusal must leave the user an action: {message}"
    );
    assert_eq!(
        fs::read_to_string(directory.file()).expect("the file reads"),
        text
    );
    assert!(
        !directory.0.join("settings.json.tmp").exists(),
        "a refused save must not leave a temporary file behind"
    );
}

#[test]
fn moving_the_unreadable_file_aside_is_the_recovery_the_message_promises() {
    // The message tells the user to move the file aside. If that did not then
    // let them save, the refusal would be a dead end rather than a recovery
    // path (AGENTS.md sections 45 and 56).
    let directory = TestDirectory::new("recovery");
    fs::write(
        directory.file(),
        format!(r#"{{"version":{}}}"#, SCHEMA_VERSION + 1),
    )
    .expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    store.store(worked_example()).expect_err("refused");

    fs::rename(directory.file(), directory.0.join("settings.json.old"))
        .expect("the user moves it aside");
    store
        .store(worked_example())
        .expect("now there is nothing to destroy");
    assert_eq!(store.current(), &worked_example());
}

// ---------------------------------------------------------------- migration

#[test]
fn a_version_0_file_is_migrated_and_says_so() {
    // Version 0 is a document with no version key, spelling the frame rate the
    // way the game catalogue's `default_settings` example does.
    let directory = TestDirectory::new("migrate");
    let text = r#"{"global":{"fps":60},"games":{"counter-strike-2":{"fps":120}}}"#;
    fs::write(directory.file(), text).expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    assert_eq!(
        store.load().expect("an older file still reads"),
        Loaded::Migrated { from: 0 }
    );
    assert_eq!(store.current().global().framerate(), Some(60));
    assert_eq!(
        store
            .current()
            .resolve_for(&game("counter-strike-2"))
            .framerate()
            .get(),
        120
    );
    assert_eq!(
        store
            .current()
            .resolve_for(&game("minecraft"))
            .framerate()
            .get(),
        60
    );
    assert_eq!(
        fs::read_to_string(directory.file()).expect("the file reads"),
        text,
        "a migration on open must not rewrite a file the user only looked at"
    );
}

#[test]
fn a_migrated_configuration_is_written_back_at_the_current_version() {
    let directory = TestDirectory::new("migrate-save");
    fs::write(directory.file(), r#"{"global":{"fps":60}}"#).expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    store.load().expect("the older file reads");
    let migrated = store.current().clone();
    store.store(migrated).expect("saving writes the new shape");

    let text = fs::read_to_string(directory.file()).expect("the file reads");
    assert!(
        text.contains(&format!("\"version\": {SCHEMA_VERSION}")),
        "the saved file must declare its version: {text}"
    );
    assert!(
        text.contains("\"framerate\": 60") && !text.contains("\"fps\""),
        "the saved file must use the current spelling: {text}"
    );

    let mut reader = ConfigurationStore::at(directory.file());
    assert_eq!(reader.load().expect("it reads"), Loaded::AsWritten);
    assert_eq!(reader.current().global().framerate(), Some(60));
}

#[test]
fn a_version_0_file_that_says_the_frame_rate_twice_is_refused_rather_than_guessed() {
    let directory = TestDirectory::new("ambiguous");
    let text = r#"{"global":{"fps":60,"framerate":120}}"#;
    fs::write(directory.file(), text).expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    let error = store
        .load()
        .expect_err("guessing would record at the wrong rate for every session");
    let message = error.to_string();
    assert!(
        message.contains("fps") && message.contains("framerate"),
        "the refusal must name both spellings: {message}"
    );
    assert_eq!(
        fs::read_to_string(directory.file()).expect("the file reads"),
        text
    );
}

// ------------------------------------------------- keys from a newer build

#[test]
fn a_setting_this_build_has_never_heard_of_survives_being_read_and_saved() {
    // The failure this prevents: a user configures something on the machine
    // running the newer Clipped, opens the settings on the older one, changes
    // anything at all, and silently loses it (AGENTS.md section 56).
    let directory = TestDirectory::new("unknown-keys");
    fs::write(
        directory.file(),
        r#"{
  "version": 1,
  "global": { "framerate": 60, "hdr": true },
  "games": { "counter-strike-2": { "bitrate_kbps": 40000 } },
  "hotkeys": { "open_clip_editor": "Ctrl+F7" },
  "storage": { "maximum_usage_bytes": 500000000000, "keep_days": 30 },
  "telemetry": { "enabled": false }
}"#,
    )
    .expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    store.load().expect("unknown keys are not an error");
    assert_eq!(
        store
            .current()
            .global()
            .unrecognised_keys()
            .collect::<Vec<_>>(),
        vec!["hdr"]
    );
    assert_eq!(
        store.current().unrecognised_keys().collect::<Vec<_>>(),
        vec!["telemetry"],
        "`storage` is a section this build reads; `telemetry` is the one it has never heard of"
    );
    assert_eq!(
        store
            .current()
            .hotkeys()
            .unrecognised_keys()
            .collect::<Vec<_>>(),
        vec!["open_clip_editor"]
    );

    // Change something and save, the way an older build's settings screen
    // would: it builds fresh values from the controls it knows about, because
    // it cannot build a control for a setting it has never heard of.
    let mut configuration = store.current().clone();
    configuration.set_global(framerate(90));
    configuration.set_game(game("counter-strike-2"), framerate(144));
    configuration.set_hotkeys(HotkeyOverrides::none());
    store.store(configuration).expect("saving works");

    let saved = fs::read_to_string(directory.file()).expect("the file reads");
    for kept in ["hdr", "bitrate_kbps", "open_clip_editor", "keep_days"] {
        assert!(
            saved.contains(kept),
            "\"{kept}\" was dropped by an older build: {saved}"
        );
    }
    assert!(saved.contains("\"framerate\": 90"));
}

// ------------------------------------------------------- the notifications

#[test]
fn a_notification_switched_off_survives_the_file() {
    // Issue #252: the switches were a second store in a second directory with a
    // version of their own, and they are a section of this file now. What has to
    // hold is what holds for every other setting — it is saved, it is read back,
    // and it does not disturb the recording settings beside it.
    let directory = TestDirectory::new("notifications");
    let mut store = ConfigurationStore::at(directory.file());

    let mut configuration = store.current().clone();
    let mut notifications = configuration.notifications().clone();
    notifications.set(NotificationCategory::RecorderUnavailable, Some(false));
    configuration.set_notifications(notifications);
    configuration.set_global(framerate(90));
    store.store(configuration).expect("saving works");

    let saved = fs::read_to_string(directory.file()).expect("the file reads");
    assert!(
        saved.contains("\"notifications\"") && saved.contains("\"recorder_unavailable\": false"),
        "the switch is not in the settings file: {saved}",
    );

    let mut read = ConfigurationStore::at(directory.file());
    read.load().expect("what this build wrote, it reads");
    let notifications = read.current().notifications();
    assert!(!notifications.is_enabled(NotificationCategory::RecorderUnavailable));
    assert_eq!(
        notifications.configured(NotificationCategory::RecorderUnavailable),
        Some(false),
        "a switch somebody moved is one Reset is offered for",
    );
    for category in NotificationCategory::ALL {
        if category != NotificationCategory::RecorderUnavailable {
            assert!(
                notifications.is_enabled(category),
                "switching one category off silenced {category}",
            );
        }
    }
    assert_eq!(read.current().resolve_global().framerate().get(), 90);
}

#[test]
fn a_settings_file_with_no_notifications_section_interrupts_about_everything() {
    // The shipped default, and the shape of every file written before this
    // section existed. Silence would be the wrong way to read one: all four
    // categories are failures.
    let directory = TestDirectory::new("notifications-absent");
    fs::write(
        directory.file(),
        r#"{ "version": 1, "global": { "framerate": 120 } }"#,
    )
    .expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    store.load().expect("a file with no such section is a file");

    for category in NotificationCategory::ALL {
        assert!(store.current().notifications().is_enabled(category));
        assert_eq!(store.current().notifications().configured(category), None);
    }

    // And saving does not grow the section, so a user who has never switched
    // anything off does not find a paragraph saying so.
    let current = store.current().clone();
    store.store(current).expect("saving works");
    let saved = fs::read_to_string(directory.file()).expect("the file reads");
    assert!(!saved.contains("notifications"), "{saved}");
}

#[test]
fn a_notification_key_this_build_cannot_read_is_kept_and_leaves_the_category_on() {
    // Deliberately the opposite of what `storage` does with a value it cannot
    // read. A limit that is ignored leaves somebody believing their library is
    // capped; a switch that is ignored leaves them being told about a failure,
    // which is the direction to fail in — and refusing would mean a typo here
    // stopped the recording settings in the same file from loading.
    let directory = TestDirectory::new("notifications-unreadable");
    fs::write(
        directory.file(),
        r#"{
  "version": 1,
  "global": { "framerate": 120 },
  "notifications": { "recording_failed": "no", "replay_saved": true }
}"#,
    )
    .expect("the file can be written");

    let mut store = ConfigurationStore::at(directory.file());
    store
        .load()
        .expect("a switch this build cannot read must not cost the whole file");

    assert!(
        store
            .current()
            .notifications()
            .is_enabled(NotificationCategory::RecordingFailed),
        "a value nobody could read silenced a failure",
    );
    assert_eq!(store.current().resolve_global().framerate().get(), 120);

    let current = store.current().clone();
    store.store(current).expect("saving works");
    let saved = fs::read_to_string(directory.file()).expect("the file reads");
    for kept in ["\"no\"", "replay_saved"] {
        assert!(saved.contains(kept), "{kept} was dropped: {saved}");
    }
}

// -------------------------------------------------------------- the store

#[test]
fn saving_creates_the_directory_and_replaces_the_previous_file() {
    let directory = TestDirectory::new("atomic");
    let nested = directory.0.join("nested").join(FILE_NAME);
    let mut store = ConfigurationStore::at(&nested);

    store
        .store(worked_example())
        .expect("the directory is created");
    assert!(nested.exists());

    let mut second = Configuration::defaults();
    second.set_global(framerate(30));
    store.store(second.clone()).expect("the file is replaced");
    assert_eq!(store.current(), &second);

    let mut reader = ConfigurationStore::at(&nested);
    reader.load().expect("it reads");
    assert_eq!(reader.current(), &second);
    assert!(
        !Path::new(&nested.with_extension("json.tmp")).exists(),
        "the temporary file must not be left behind"
    );
}

#[test]
fn every_setting_a_game_was_given_reaches_the_recording_it_is_made_with() {
    // A setting that resolves and is then dropped on the way to the recording
    // is a control that silently does nothing (AGENTS.md section 27), and
    // `apply_to` is the only place one could be lost.
    let mut configuration = Configuration::defaults();
    let mut preferences = Preferences::none();
    preferences
        .set_resolution(Some(ResolutionSetting::Fixed {
            width: 2560,
            height: 1440,
        }))
        .expect("1440p is in range");
    preferences
        .set_framerate(Some(120))
        .expect("120 is in range");
    preferences.set_codec(Some(CodecPreference::Fixed(Codec::Av1)));
    preferences.set_encoder(Some(EncoderPreference::Fixed(EncoderKind::Nvenc)));
    preferences
        .set_microphone(Some(AudioDeviceSetting::Named("Yeti".to_owned())))
        .expect("a device name in range");
    preferences
        .set_system_audio(Some(AudioDeviceSetting::Disabled))
        .expect("nothing is a valid selection");
    configuration.set_game(game("counter-strike-2"), preferences);

    let recording = configuration
        .resolve_for(&game("counter-strike-2"))
        .apply_to(RecordingSettings::new(
            CaptureTargetSettings::window(0x1234, 2560, 1440),
            PathBuf::from("out.mkv"),
        ));

    assert_eq!(
        recording.resolution(),
        ResolutionSetting::Fixed {
            width: 2560,
            height: 1440
        }
    );
    assert_eq!(recording.framerate(), 120);
    assert_eq!(recording.codec(), CodecPreference::Fixed(Codec::Av1));
    assert_eq!(
        recording.encoder(),
        EncoderPreference::Fixed(EncoderKind::Nvenc)
    );
    assert_eq!(
        recording.microphone(),
        &AudioSourceSetting::Named("Yeti".to_owned())
    );
    assert_eq!(
        recording.system_audio(),
        &AudioSourceSetting::Off,
        "\"none\" must turn the source off rather than record the default endpoint"
    );
    assert_eq!(
        recording.unavailable_choice(),
        UnavailableChoice::Substitute
    );
}

#[test]
fn only_a_setting_somebody_configured_replaces_what_a_caller_already_asked_for() {
    // `apply_configured_to`, and the reason it exists: `clipped-recorder watch`
    // reaches a recording with a command line already answered — a resolution,
    // a frame rate, a codec, an encoder and two audio selections — and a
    // settings file that says nothing about them must leave every one of them
    // alone. `apply_to` would put the shipped default over all six, which is
    // `watch --framerate 144` recording at 60 and `--microphone none` opening
    // a microphone (AGENTS.md section 27).
    let base = RecordingSettings::new(
        CaptureTargetSettings::window(0x1234, 1920, 1080),
        PathBuf::from("out.mkv"),
    )
    .with_resolution(ResolutionSetting::Fixed {
        width: 1920,
        height: 1080,
    })
    .with_framerate(144)
    .with_codec(CodecPreference::Fixed(Codec::Av1))
    .with_encoder(EncoderPreference::Fixed(EncoderKind::Nvenc))
    .with_microphone(AudioSourceSetting::Off)
    .with_system_audio(AudioSourceSetting::Named("Speakers".to_owned()));

    assert_eq!(
        Configuration::defaults()
            .resolve_global()
            .apply_configured_to(base.clone()),
        base,
        "a user who has configured nothing must change nothing about what was asked for"
    );

    // And what they did configure — and only that — replaces it.
    let mut configuration = Configuration::defaults();
    let mut preferences = Preferences::none();
    preferences.set_framerate(Some(60)).expect("60 is in range");
    configuration.set_game(game("counter-strike-2"), preferences);

    let applied = configuration
        .resolve_for(&game("counter-strike-2"))
        .apply_configured_to(base.clone());

    assert_eq!(applied.framerate(), 60);
    assert_eq!(applied.resolution(), base.resolution());
    assert_eq!(applied.codec(), base.codec());
    assert_eq!(applied.encoder(), base.encoder());
    assert_eq!(applied.microphone(), base.microphone());
    assert_eq!(applied.system_audio(), base.system_audio());
    assert_eq!(
        applied.unavailable_choice(),
        UnavailableChoice::Refuse,
        "a configured frame rate is not one of the two settings the choice governs, so what the          caller named still refuses rather than substituting"
    );
}

#[test]
fn a_configured_encoder_substitutes_where_the_one_a_caller_named_would_refuse() {
    // The other half of the rule the table in `docs/configuration.md` states:
    // a value chosen once, possibly before this machine had the graphics card
    // it has now, must not fail a recording nobody is watching.
    let base = RecordingSettings::new(
        CaptureTargetSettings::window(0x1234, 1920, 1080),
        PathBuf::from("out.mkv"),
    )
    .with_encoder(EncoderPreference::Fixed(EncoderKind::Software));
    assert_eq!(base.unavailable_choice(), UnavailableChoice::Refuse);

    let mut configuration = Configuration::defaults();
    let mut preferences = Preferences::none();
    preferences.set_encoder(Some(EncoderPreference::Fixed(EncoderKind::Nvenc)));
    configuration.set_game(game("counter-strike-2"), preferences);

    let applied = configuration
        .resolve_for(&game("counter-strike-2"))
        .apply_configured_to(base);

    assert_eq!(
        applied.encoder(),
        EncoderPreference::Fixed(EncoderKind::Nvenc)
    );
    assert_eq!(applied.unavailable_choice(), UnavailableChoice::Substitute);
}

#[test]
fn what_a_recording_asks_for_when_nothing_was_configured_is_what_it_already_did() {
    // The promise that a user who has configured nothing sees no change. Every
    // video setting a resolved default asks for is the one a recording already
    // carried.
    let base = RecordingSettings::new(
        CaptureTargetSettings::window(0x1234, 1280, 720),
        PathBuf::from("out.mkv"),
    );
    let applied = Configuration::defaults()
        .resolve_global()
        .apply_to(base.clone());

    assert_eq!(applied.resolution(), base.resolution());
    assert_eq!(applied.framerate(), base.framerate());
    assert_eq!(applied.codec(), base.codec());
    assert_eq!(applied.encoder(), base.encoder());
    assert_eq!(applied.output(), base.output());
    assert_eq!(applied.target(), base.target());

    // The two that deliberately differ, and the difference is not a change to
    // any product behaviour: `RecordingSettings` defaults both audio sources
    // off so that a library caller which says nothing does not have somebody's
    // microphone opened behind its back, while the settings — like
    // `clipped-recorder record` and `watch`, whose `--microphone` and
    // `--system-audio` both default to `default` — record the default
    // endpoints unless told otherwise.
    assert_eq!(applied.microphone(), &AudioSourceSetting::SystemDefault);
    assert_eq!(applied.system_audio(), &AudioSourceSetting::SystemDefault);
}

#[test]
fn the_settings_a_recording_was_made_with_read_back_in_the_words_the_file_uses() {
    // What a log line and a session's record are written from. A second
    // spelling of "hevc" or of a device name would make a session's record
    // impossible to compare against the file that produced it.
    let mut configuration = Configuration::defaults();
    let mut preferences = Preferences::none();
    preferences.set_codec(Some(CodecPreference::Fixed(Codec::Hevc)));
    preferences
        .set_resolution(Some(ResolutionSetting::Fixed {
            width: 1920,
            height: 1080,
        }))
        .expect("1080p is in range");
    configuration.set_game(game("counter-strike-2"), preferences);

    let resolved = configuration.resolve_for(&game("counter-strike-2"));

    assert_eq!(resolved.written_value(SettingKey::Codec), "hevc");
    assert_eq!(resolved.written_value(SettingKey::Resolution), "1920x1080");
    assert_eq!(resolved.written_value(SettingKey::Framerate), "60");
    assert_eq!(resolved.written_value(SettingKey::Microphone), "default");
    assert_eq!(resolved.written_value(SettingKey::ReplayWindow), "300");

    // And the one line the recording writes into the log carries every setting
    // with the layer it came from, because "why was it recorded like that" is
    // not answerable from the values alone.
    let described = resolved.to_string();
    assert!(described.contains("codec=hevc (game)"), "{described}");
    assert!(described.contains("framerate=60 (default)"), "{described}");
}

#[test]
fn the_default_path_is_the_settings_file_under_clippeds_own_directory() {
    let Some(path) = ConfigurationStore::default_path() else {
        // A machine that describes no per-user directory, which
        // `clipped_logging::application_directory` documents as supported.
        return;
    };
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some(FILE_NAME)
    );
    let parent = path.parent().expect("the file is in a directory");
    assert!(
        parent.ends_with(Path::new("Clipped")) || parent.ends_with(Path::new("clipped")),
        "{}",
        parent.display()
    );
}
