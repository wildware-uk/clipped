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
//! A settings file can carry a key that nothing reads when a recording starts,
//! and a screen that drew that key as a working control would be the lie
//! AGENTS.md section 27 is about. So the recorder says, per setting, whether it
//! is in force, and — when it is not — the sentence that says what would have to
//! land. It is the same pair
//! [`HotkeyBinding`](crate::HotkeyBinding) carries for the same reason: a
//! registered key nothing performs is still a key that does nothing.

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
    /// Whether anything reads this setting when a recording starts.
    ///
    /// `false` is a setting the file can carry and no recording acts on. The
    /// value is still real — it is in the file and a later build will read it —
    /// but a window must not draw it as a control that changes a recording,
    /// which is what [`unavailable`](Self::unavailable) says instead.
    pub applies: bool,
    /// Why changing it would not change a recording, when that is the case.
    ///
    /// Present exactly when [`applies`](Self::applies) is `false`, and it is
    /// the recorder's own sentence, naming the work that would make the setting
    /// count. Absent for a setting that is in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// The settings, and the file they came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    /// The settings file these came from, so a window can say where they live
    /// rather than keeping its own copy of a path.
    pub file: String,
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

#[cfg(test)]
mod tests {
    use super::{ApplySettings, AudioDevice, AudioDevices, SettingEntry, SettingsView};

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
        };

        let json = serde_json::to_string(&entry).expect("it serialises");
        assert!(
            !json.contains("unavailable") && !json.contains("choices"),
            "an absent reason and an open value set are absent fields: {json}",
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
    fn a_view_names_the_file_the_settings_came_from() {
        let view = SettingsView {
            file: r"C:\Users\alex\AppData\Local\Clipped\settings.json".to_owned(),
            settings: Vec::new(),
        };

        let json = serde_json::to_string(&view).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<SettingsView>(&json).expect("and deserialises"),
            view,
        );
    }
}
