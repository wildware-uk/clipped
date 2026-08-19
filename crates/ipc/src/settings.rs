//! The settings, as a window reads and changes them.
//!
//! # Why the settings are on the protocol at all
//!
//! They live in `clipped_session::config`, which owns `settings.json` — its
//! defaults, its validation, its layering and its migrations
//! (`docs/configuration.md`). The desktop application may link one crate of
//! this workspace, `clipped-ipc`, and
//! `tests/integration/tests/workspace_layering.rs` enforces it: naming
//! `clipped-session` from the window would put the recording engine in the
//! window's process, which is the separation ADR 0002 exists to make.
//!
//! The two ways out were reading the file from the window — a second
//! implementation of its versioning, migration and validation, against the file
//! somebody's settings live in — or asking the process that owns it. This is the
//! second ([issue #252](https://github.com/wildware-uk/clipped/issues/252)), and
//! it is the one that also answers the questions a settings screen has to ask
//! about *this machine*: which microphones exist, and whether a setting the file
//! can hold is one any recording actually reads.
//!
//! # Why a value is text
//!
//! Every setting crosses as the words the settings file spells it in — `120`,
//! `hevc`, `name:Shure MV7` — and goes back the same way. The alternative was a
//! variant per setting on the wire, which would have meant a second vocabulary
//! for settings beside the file's own, and a protocol change for every setting
//! added. The recorder parses what comes back with the file reader's own
//! parsers, so a value this window can save is exactly a value the file would
//! accept, refused with the same sentence when it is not (AGENTS.md section 55).
//!
//! # Why [`SettingEntry::applies`] exists
//!
//! A settings file can carry a key that nothing acts on, and a screen that drew
//! that key as a working control would be the lie AGENTS.md section 27 is about.
//! So the recorder says, per setting, whether it is in force, and — when it is
//! not — the sentence that says what would have to land. It is the same pair
//! [`HotkeyBinding`](crate::HotkeyBinding) carries for the same reason: a
//! registered key nothing performs is still a key that does nothing.
//!
//! # A setting the recorder keeps and the window reads
//!
//! The four notification switches — `recording_failed`,
//! `recording_interrupted`, `recorder_unavailable` and `hotkey_unavailable` —
//! are settings like any other here, and they are the one group whose reader is
//! the window rather than a recording. They cross as `true` or `false`, which
//! is what the settings file holds.
//!
//! They are on these two commands because of the rule at the top of this file
//! read the other way round. The window decides whether to show a toast, so the
//! window needs the switch; the window may not open the settings file, so the
//! recorder hands it over. Before
//! [issue #252](https://github.com/wildware-uk/clipped/issues/252) the window
//! kept them in a file of its own — a second store of user preferences, with a
//! second version field, a second missing-key policy and a second reader
//! (AGENTS.md section 55).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One setting, as a window draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingEntry {
    /// The key the settings file holds it under, such as `microphone`.
    ///
    /// The same string `apply_settings` takes back, and the same string
    /// somebody editing the file by hand would type.
    pub key: String,
    /// The setting's name in the words a person reads, such as `Microphone`.
    ///
    /// Sent rather than derived, for the reason
    /// [`HotkeyBinding::label`](crate::HotkeyBinding) is: a window keeping its
    /// own table of labels would show nothing at all for a setting a newer
    /// recorder had added.
    pub label: String,
    /// What the setting resolves to, spelled the way the file spells it.
    ///
    /// Always the effective value, never blank: a setting nobody has configured
    /// carries the value Clipped ships with, and [`overridden`](Self::overridden)
    /// is what tells the two apart.
    pub value: String,
    /// Whether this value was configured, rather than being the shipped
    /// default.
    ///
    /// What a Reset control is enabled by. Resetting is `apply_settings` with a
    /// `null`, which is a different thing from setting the default explicitly:
    /// one follows a later change to the default and the other does not
    /// (`docs/configuration.md`).
    pub overridden: bool,
    /// Every value this setting can take, where the set is closed.
    ///
    /// Empty for the settings whose values are open — a frame rate, a size, a
    /// device name — which is how a window tells a list of options from a
    /// field without keeping its own copy of either.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// What the setting would accept, in the words its refusal uses.
    ///
    /// The hint beside a field, and the same sentence a refused value comes
    /// back with, so the two cannot disagree.
    pub accepted: String,
    /// Whether anything in this build acts on this setting.
    ///
    /// For almost every setting that means "reads it when a recording starts",
    /// because the recorder is what acts on them. The exception is the
    /// notification switches, whose reader is the *window* — it asks for them
    /// and consults them when it decides whether to show a toast, and the
    /// recorder keeps them and never reads them at all
    /// ([issue #252](https://github.com/wildware-uk/clipped/issues/252)). The
    /// question a screen is asking is the same either way: would changing this
    /// change anything?
    ///
    /// `false` is a setting the file can carry and nothing acts on. The value is
    /// still real — it is in the file and a later build will read it — but a
    /// window must not draw it as a working control, which is what
    /// [`unavailable`](Self::unavailable) says instead.
    pub applies: bool,
    /// Why changing it would change nothing, when that is the case.
    ///
    /// Present exactly when [`applies`](Self::applies) is `false`, and it is
    /// the recorder's own sentence, naming the work that would make the setting
    /// count. Absent for a setting that is in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// What is still in force, for a saved value that is not yet the one being
    /// used.
    ///
    /// A different thing from [`unavailable`](Self::unavailable): that one is a
    /// setting nothing reads at all, and this is one that is read and has not
    /// got there yet. Absent for every setting whose new value is used by the
    /// next recording, which is all of them but one.
    ///
    /// The one is the recording directory. Where automatic recordings are
    /// written moves **between sittings and never during one**, because a
    /// sitting's session record is written next to the files it names and the
    /// two must not be separated (AGENTS.md section 56,
    /// `clipped_session::automatic::SessionManager::set_recording_directory`).
    /// A directory saved while a game is being recorded is therefore saved but
    /// not yet used, and a screen that drew it as the answer would be saying
    /// something untrue about where the next few minutes of footage is going
    /// (AGENTS.md section 27,
    /// [issue #609](https://github.com/wildware-uk/clipped/issues/609)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_yet_in_force: Option<String>,
}

/// Which settings to read: the global page, or one game's.
///
/// # Why a game is named here rather than resolved in the window
///
/// Because inheritance is the recorder's answer and not an arithmetic a screen
/// can do. A window that read the global settings and a game's overrides and
/// folded them itself would be a second implementation of
/// `clipped_session::config`'s layering, and it would get
/// [`SettingEntry::overridden`] wrong in the one case that matters: a game that
/// set a value to the same text the global layer holds is *overridden* and
/// offers Reset, and a game that set nothing is *inherited* and does not
/// (`clipped_session::config::value`, AGENTS.md sections 27 and 30).
///
/// A game with no section of its own is not an error and not a special case:
/// every setting comes back inherited, which is what a page for a game nobody
/// has configured should show.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSettings {
    /// The game to resolve for, named as the settings file names it —
    /// `counter-strike-2`.
    ///
    /// Absent is the global settings, which is what every game inherits from
    /// and what an older window sends by sending no parameters at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
}

/// The settings, and the file they came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    /// The settings file these came from, so a window can say where they live
    /// rather than keeping its own copy of a path.
    pub file: String,
    /// The game these were resolved for, absent for the global settings.
    ///
    /// Echoed back rather than assumed, so that a window drawing a per-game page
    /// from this reply cannot draw the global settings under a game's name when
    /// a recorder too old to understand the scope answers the global page
    /// anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    /// Every game the settings file holds a section of its own for, in
    /// identifier order.
    ///
    /// The games somebody has already configured, which is the list a per-game
    /// page opens from. It is **not** the games this machine has: which
    /// processes are games is the catalogue's answer and no command reads it
    /// ([issue #245](https://github.com/wildware-uk/clipped/issues/245)), and a
    /// window that drew this as "your games" would be calling a settings file a
    /// catalogue.
    ///
    /// A game stays on it after its last override is cleared. `set_game` stores
    /// an empty section rather than dropping one, because a game somebody has
    /// opened the settings for and reset every value on is a game they may well
    /// come back to — so this is "games with a page", not "games with an
    /// override" (`clipped_session::config::Configuration::set_game`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub games: Vec<String>,
    /// Every setting the recorder will accept, in the order a screen lists
    /// them.
    ///
    /// Always the whole list, including the settings nothing reads yet: a
    /// window that was sent only the working ones could not say what the others
    /// are waiting for, and a setting missing from the list is
    /// indistinguishable from one this recorder has never heard of.
    pub settings: Vec<SettingEntry>,
}

/// What to change, and to what.
///
/// A map rather than one key and one value, because a settings screen saves
/// what somebody edited: two changes made together are applied together, and a
/// value refused refuses the whole request rather than leaving half of it
/// written (`clipped_session::config` validates before anything is saved).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplySettings {
    /// The game to change them for, or absent for the global settings.
    ///
    /// The same identifier [`GetSettings::game`] takes, and a change is written
    /// into that game's own section — so it becomes an override, and every game
    /// without one goes on inheriting (AGENTS.md section 30).
    ///
    /// Only the settings a game may override are accepted here. The recording
    /// directory, the storage limits, the hotkeys and the notification switches
    /// are global by construction — a library is one thing however many games
    /// are in it, and Windows registers a combination once for a process — and
    /// naming one with a game is refused rather than quietly written to the
    /// global layer (`clipped_session::config`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    /// The settings to change, by the key each has in the file.
    ///
    /// `null` clears one, which is Reset: it returns the setting to the value
    /// Clipped ships with *and* keeps following it, which writing the current
    /// default in as a value would not.
    ///
    /// An empty map is accepted and changes nothing, so that a screen with
    /// nothing edited does not have to special-case Save.
    #[serde(default)]
    pub values: BTreeMap<String, Option<String>>,
}

/// One audio endpoint this machine has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    /// The name Windows gives it, which is also what a settings file names it
    /// by.
    pub name: String,
    /// Whether this is the endpoint Windows currently considers the default.
    ///
    /// The `default` setting follows whichever endpoint that is at the moment a
    /// recording starts, so this marks the device that choice resolves to
    /// today rather than a device the setting is pinned to.
    pub is_default: bool,
}

/// The audio endpoints a recording could be told to use.
///
/// Microphones only. The endpoint the machine plays through is not listed,
/// because a recording cannot be told to use one that is not the default:
/// `clipped-audio` opens loopback against whatever Windows is playing through
/// and offers no way to name another
/// ([issue #316](https://github.com/wildware-uk/clipped/issues/316)). Sending
/// an empty list would say "this machine has no playback devices", which is a
/// different and untrue thing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevices {
    /// Every capture endpoint that is present and active, in the order Windows
    /// lists them.
    pub microphones: Vec<AudioDevice>,
}

/// Which microphone to listen to, and for how long the recorder decides.
///
/// # Why a setting's value rather than a device name
///
/// Because the question is "what would *this choice* record", asked while
/// somebody is still choosing. The value is spelled exactly as the settings file
/// spells it — `default`, `name:Shure MV7` — so the level a window shows is
/// measured against the same words it is about to save, resolved by the same
/// code a recording resolves them with (`clipped_session::audio`). A window that
/// sent a bare device name would be asking about a device; this asks about a
/// setting, and those differ the moment `default` moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicrophoneLevelRequest {
    /// The microphone setting to listen to, in the settings file's own
    /// spelling.
    pub microphone: String,
}

/// What a microphone is hearing.
///
/// # Why this is asked for repeatedly rather than streamed
///
/// A level is only wanted while somebody is looking at a meter, which is a few
/// seconds on a settings screen and never again. An event stream would have to
/// be started, stopped, and cleaned up after a window that was killed
/// mid-choice — and the thing left behind would be an open capture, which
/// Windows shows as a microphone-in-use indicator for the life of the recorder
/// (AGENTS.md section 58). Asking opens and closes the device inside the call,
/// so there is nothing to leak and nothing to tidy up.
///
/// The cost is stated in [`peak`](Self::peak) rather than hidden: between two
/// questions there is a gap nothing measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicrophoneLevel {
    /// The endpoint that was listened to, as Windows names it.
    ///
    /// Absent while the device is unplugged or disabled, during which a capture
    /// produces silence rather than failing — which is what tells "nobody is
    /// speaking" from "there is nothing there". A window that drew both as a
    /// flat meter would send somebody looking for the volume control when the
    /// answer is a cable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The loudest sample heard, from `0.0` to `1.0`.
    ///
    /// The loudest in the moment that was listened to, **not** since the last
    /// question: this is a sample of the signal, not a running maximum, and a
    /// window that held the highest reading it ever saw would show a meter that
    /// only ever went up.
    ///
    /// A linear amplitude rather than decibels, because how a meter scales it is
    /// the drawing side's decision and converting here would put that decision
    /// in two places.
    pub peak: f32,
    /// Whether Windows reports the microphone muted.
    ///
    /// Absent when Windows will not report the switch for this device — some
    /// virtual devices have none. A muted microphone reads as silence, so this
    /// is what stops a screen telling somebody to speak up when what they need
    /// is to unmute (AGENTS.md section 28).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::{
        ApplySettings, AudioDevice, AudioDevices, GetSettings, MicrophoneLevel,
        MicrophoneLevelRequest, SettingEntry, SettingsView,
    };

    #[test]
    fn a_setting_nothing_reads_carries_the_sentence_that_says_so() {
        // The field this type exists for: a window must be able to tell a
        // setting that changes a recording from one that does not, without
        // knowing anything about which is which.
        let entry = SettingEntry {
            key: "capture_target".to_owned(),
            label: "Capture target".to_owned(),
            value: "game-window".to_owned(),
            overridden: false,
            choices: vec!["game-window".to_owned(), "display".to_owned()],
            accepted: "\"game-window\" or \"display\"".to_owned(),
            applies: false,
            unavailable: Some(
                "a recording still captures the game's window (issue #61)".to_owned(),
            ),
            not_yet_in_force: None,
        };

        let json = serde_json::to_string(&entry).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<SettingEntry>(&json).expect("and deserialises"),
            entry,
        );
        assert!(
            json.contains("issue #61"),
            "a setting nothing reads must arrive with the reason: {json}",
        );
    }

    #[test]
    fn a_setting_that_is_in_force_leaves_the_reason_out_rather_than_sending_an_empty_one() {
        let entry = SettingEntry {
            key: "framerate".to_owned(),
            label: "Frame rate".to_owned(),
            value: "60".to_owned(),
            overridden: false,
            choices: Vec::new(),
            accepted: "1-480 frames per second".to_owned(),
            applies: true,
            unavailable: None,
            not_yet_in_force: None,
        };

        let json = serde_json::to_string(&entry).expect("it serialises");
        assert!(
            !json.contains("unavailable")
                && !json.contains("choices")
                && !json.contains("not_yet_in_force"),
            "an absent reason, an open value set and a setting that is already in force are \
             absent fields: {json}",
        );
        assert_eq!(
            serde_json::from_str::<SettingEntry>(&json).expect("and deserialises"),
            entry,
        );
    }

    #[test]
    fn a_directory_that_is_saved_and_not_yet_used_says_where_recordings_are_still_going() {
        // The distinction the field exists for. `applies` is true and
        // `unavailable` is absent — the recorder does read this setting — and
        // what a window must still be able to say is that the folder on screen
        // is not where the next few minutes of footage is going, because the
        // sitting that is open keeps its recordings and its session record
        // together (AGENTS.md sections 27 and 56, issue #609).
        let entry = SettingEntry {
            key: "recording_directory".to_owned(),
            label: "Recording directory".to_owned(),
            value: r"D:\Clips".to_owned(),
            overridden: true,
            choices: Vec::new(),
            accepted: r"a folder on this machine, such as D:\Clips".to_owned(),
            applies: true,
            unavailable: None,
            not_yet_in_force: Some(
                r"Automatic recordings still go to C:\Users\alex\Videos\Clipped. They go here \
                  from the next session."
                    .to_owned(),
            ),
        };

        let json = serde_json::to_string(&entry).expect("it serialises");
        assert!(
            !json.contains("unavailable"),
            "a setting the recorder does read must not arrive as one it does not: {json}",
        );
        assert!(
            json.contains("not_yet_in_force"),
            "and a window cannot say when a saved value starts counting unless the sentence \
             crosses: {json}",
        );
        assert_eq!(
            serde_json::from_str::<SettingEntry>(&json).expect("and deserialises"),
            entry,
        );
    }

    #[test]
    fn clearing_a_setting_crosses_the_wire_as_null_rather_than_as_an_empty_string() {
        // Reset. An empty string is a value the file would refuse; `null` is
        // the absence that makes a setting follow the default again.
        let request: ApplySettings = serde_json::from_str(r#"{"values": {"framerate": null}}"#)
            .expect("a null is a value this request can carry");

        assert_eq!(request.values.get("framerate"), Some(&None));
        assert_eq!(
            serde_json::to_string(&request).expect("it serialises"),
            r#"{"values":{"framerate":null}}"#,
        );
    }

    #[test]
    fn a_request_with_nothing_to_change_is_a_request_rather_than_a_broken_frame() {
        // What Save sends when nothing was edited.
        let request: ApplySettings = serde_json::from_str("{}").expect("an empty request parses");

        assert!(request.values.is_empty());
    }

    #[test]
    fn the_default_endpoint_is_marked_rather_than_being_the_first_in_the_list() {
        // Windows lists endpoints in its own order and the default is not
        // necessarily first; a window that drew the first as the default would
        // be wrong on most machines.
        let devices = AudioDevices {
            microphones: vec![
                AudioDevice {
                    name: "Line In (Realtek)".to_owned(),
                    is_default: false,
                },
                AudioDevice {
                    name: "Shure MV7".to_owned(),
                    is_default: true,
                },
            ],
        };

        let json = serde_json::to_string(&devices).expect("it serialises");
        let back: AudioDevices = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, devices);
        assert_eq!(
            back.microphones
                .iter()
                .filter(|device| device.is_default)
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Shure MV7"],
        );
    }

    #[test]
    fn a_level_asks_about_a_setting_in_the_words_the_file_spells_it() {
        // The mistake this guards against is sending a device name: the window
        // is asking what the value it is about to save would record, and
        // `default` is a value the file holds and not a device.
        let request: MicrophoneLevelRequest =
            serde_json::from_str(r#"{"microphone":"name:Shure MV7"}"#).expect("it parses");

        assert_eq!(request.microphone, "name:Shure MV7");
    }

    #[test]
    fn a_device_that_is_not_there_is_an_absent_name_rather_than_an_empty_one() {
        // The one distinction the meter exists to draw: a peak of zero from a
        // device that is present is somebody not speaking, and a peak of zero
        // with no device is a microphone that is not plugged in.
        let unplugged = MicrophoneLevel {
            device: None,
            peak: 0.0,
            muted: None,
        };

        let json = serde_json::to_string(&unplugged).expect("it serialises");
        assert_eq!(
            json, r#"{"peak":0.0}"#,
            "an absent device and an unknown mute switch are absent fields: {json}",
        );
        assert_eq!(
            serde_json::from_str::<MicrophoneLevel>(&json).expect("and deserialises"),
            unplugged,
        );
    }

    #[test]
    fn a_muted_microphone_says_so_beside_a_reading_of_silence() {
        let muted = MicrophoneLevel {
            device: Some("Shure MV7".to_owned()),
            peak: 0.0,
            muted: Some(true),
        };

        let json = serde_json::to_string(&muted).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<MicrophoneLevel>(&json).expect("and deserialises"),
            muted,
        );
        assert!(
            json.contains(r#""muted":true"#),
            "the switch is what explains the silence: {json}",
        );
    }

    #[test]
    fn a_view_names_the_file_the_settings_came_from() {
        let view = SettingsView {
            file: r"C:\Users\alex\AppData\Local\Clipped\settings.json".to_owned(),
            game: None,
            games: Vec::new(),
            settings: Vec::new(),
        };

        let json = serde_json::to_string(&view).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<SettingsView>(&json).expect("and deserialises"),
            view,
        );
        assert!(
            !json.contains("game"),
            "the global page is the absence of a game rather than a null: {json}",
        );
    }

    #[test]
    fn the_global_page_and_a_games_page_are_told_apart_by_the_request_and_by_the_answer() {
        // The two ends of the same fact. A window that sent a game and drew
        // whatever came back would draw the global settings under a game's name
        // against a recorder too old to know the scope, and every value on that
        // page would read as inherited when the global page had set half of
        // them.
        let global: GetSettings = serde_json::from_str("{}").expect("no game is the global page");
        assert_eq!(global.game, None);

        let for_game: GetSettings =
            serde_json::from_str(r#"{"game":"counter-strike-2"}"#).expect("a game parses");
        assert_eq!(for_game.game.as_deref(), Some("counter-strike-2"));
        assert_eq!(
            serde_json::to_string(&for_game).expect("it serialises"),
            r#"{"game":"counter-strike-2"}"#,
        );

        let view = SettingsView {
            file: r"C:\Users\alex\AppData\Local\Clipped\settings.json".to_owned(),
            game: Some("counter-strike-2".to_owned()),
            games: vec!["counter-strike-2".to_owned(), "minecraft".to_owned()],
            settings: Vec::new(),
        };
        let json = serde_json::to_string(&view).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<SettingsView>(&json).expect("and deserialises"),
            view,
        );
    }

    #[test]
    fn a_change_for_one_game_names_the_game_beside_the_values() {
        // What Save sends from a per-game page, and what Reset sends from one:
        // the same `null` that clears a value globally, against a game's own
        // section, which is what stops the value inheriting again.
        let request: ApplySettings =
            serde_json::from_str(r#"{"game":"counter-strike-2","values":{"framerate":"120"}}"#)
                .expect("a scoped change parses");
        assert_eq!(request.game.as_deref(), Some("counter-strike-2"));
        assert_eq!(
            request.values.get("framerate"),
            Some(&Some("120".to_owned()))
        );

        // And a request with no game is the global page, as every window built
        // before the per-game page sends it.
        let global: ApplySettings =
            serde_json::from_str(r#"{"values":{"framerate":"120"}}"#).expect("it parses");
        assert_eq!(global.game, None);
        assert_eq!(
            serde_json::to_string(&global).expect("it serialises"),
            r#"{"values":{"framerate":"120"}}"#,
            "a global change must not grow a null game that an older recorder would refuse",
        );
    }
}
