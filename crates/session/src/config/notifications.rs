//! Which failures are worth interrupting somebody for, as the settings file
//! holds them.
//!
//! # Why this is a section of its own
//!
//! For the reason [`super::storage`] is one, arrived at from the other
//! direction. Every setting in [`SettingKey`](super::SettingKey) resolves **per
//! game** — a frame rate for Counter-Strike, a microphone for Dota — and a
//! notification category does not: "should Counter-Strike's failures interrupt
//! me" has no answer that differs from "should failures interrupt me", because
//! the thing being interrupted is a person and not a recording. SPEC.md section
//! 31's list of what a game may override does not mention notifications either.
//!
//! So these live beside `games`, `hotkeys`, `plugins` and `storage` as a section
//! of the document, in the shape those established: read into a value, written
//! back from it, and keeping whatever a newer build wrote
//! ([issue #252](https://github.com/wildware-uk/clipped/issues/252)).
//!
//! # Why the recorder keeps a setting it never reads
//!
//! Because the process that *does* read it may not read a file. The desktop
//! application decides whether a toast is shown, and it may link one crate of
//! this workspace — `clipped-ipc` — so it asks the recorder for these the way it
//! asks for everything else (`get_settings`, `apply_settings`, `docs/ipc.md`).
//! Before this section existed the switches were a second store of user
//! preferences in a second file with a second versioning policy, which is the
//! duplication AGENTS.md section 55 forbids; the alternative — the window
//! reading `settings.json` itself — would have been a second implementation of
//! this module's versioning, migration and validation, against the file
//! somebody's recording settings live in.
//!
//! # Everything is on until somebody says otherwise
//!
//! [`NotificationSettings::none`] is what a file with no `notifications` section
//! means, and every category resolves to on. That is the shipped default rather
//! than a placeholder: every category is a *failure* — nothing is being
//! recorded, or a recording ended, or a key does nothing — and a user who has
//! not said otherwise wants to be told.
//!
//! A value this build cannot read is therefore **kept and ignored** rather than
//! refused: a `notifications` key holding `"no"` leaves that category on, and
//! writing the file back does not lose it. That is the opposite of `storage`,
//! deliberately. A limit that is quietly ignored leaves somebody believing their
//! library is capped when it is not, so it is refused; a switch that is quietly
//! ignored leaves somebody being told about a failure they had asked not to hear
//! about, which is a nuisance rather than a loss — and refusing would mean a
//! typo here stopped the *recording* settings in the same file from loading.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// What a notification is about: the unit the user switches off.
///
/// There are four because there are four things worth interrupting anybody for.
/// A fifth category means a fifth real event, not a fifth wording — the desktop
/// application's `notification_policy` is where that argument is made in full,
/// and this is the same list as a settings key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotificationCategory {
    /// A recording ended because something went wrong. The recorder is still
    /// running.
    RecordingFailed,
    /// A recorder stopped while it was recording, without being asked to.
    RecordingInterrupted,
    /// Nothing is being recorded and nothing further will be tried on its own.
    RecorderUnavailable,
    /// Windows refused a global hotkey, so pressing it does nothing.
    HotkeyUnavailable,
}

/// What every notification switch accepts, in the words its refusal uses.
///
/// The file holds a JSON boolean, so these are the two words JSON spells one
/// with; a value crosses the protocol as the text the file spells it in
/// (`crates/ipc/src/settings.rs`), which makes `true` and `false` the two words
/// a window sends as well.
const ACCEPTED: &str = "true or false";

impl NotificationCategory {
    /// Every category, in the order a settings screen lists them.
    ///
    /// The single list, so that a category added without somewhere to switch it
    /// off is a compile error rather than a notification nothing can silence.
    pub const ALL: [Self; 4] = [
        Self::RecordingFailed,
        Self::RecordingInterrupted,
        Self::RecorderUnavailable,
        Self::HotkeyUnavailable,
    ];

    /// The key this category has in the `notifications` section.
    ///
    /// Stable, and the same spelling `notifications.json` used before this
    /// section existed: renaming one would silently switch a category back on
    /// for somebody who had switched it off, and would leave the migration with
    /// nothing to match (AGENTS.md section 43).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::RecordingFailed => "recording_failed",
            Self::RecordingInterrupted => "recording_interrupted",
            Self::RecorderUnavailable => "recorder_unavailable",
            Self::HotkeyUnavailable => "hotkey_unavailable",
        }
    }

    /// The category's name in the words a person reads (AGENTS.md section 28).
    ///
    /// A sentence about what happened rather than a noun, because that is what
    /// the switch beside it turns off: "A recording failed", not "Failures".
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::RecordingFailed => "A recording failed",
            Self::RecordingInterrupted => "A recording was interrupted",
            Self::RecorderUnavailable => "The recorder cannot be reached",
            Self::HotkeyUnavailable => "A hotkey is unavailable",
        }
    }

    /// The category a key names, if it names one.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|category| category.key() == key)
    }

    /// The two values a switch takes, for a screen to draw a control from.
    ///
    /// A free function rather than one taking a category, because every switch
    /// offers the same two: a second list per category would be four copies of
    /// one answer (AGENTS.md section 55).
    #[must_use]
    pub fn choices() -> Vec<String> {
        vec!["true".to_owned(), "false".to_owned()]
    }

    /// What a switch accepts, in the words its refusal uses.
    ///
    /// The shape [`SettingKey::accepted`](super::SettingKey::accepted) has, so
    /// that the hint beside a control and the sentence a refused value comes
    /// back with cannot disagree.
    #[must_use]
    pub fn accepted() -> String {
        ACCEPTED.to_owned()
    }

    /// Where this category sits in [`Self::ALL`], which is how one is stored.
    const fn index(self) -> usize {
        self as usize
    }
}

impl core::fmt::Display for NotificationCategory {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.key())
    }
}

/// A value a notification switch cannot take.
///
/// Named the way [`SettingError`](super::SettingError) names one — the key, the
/// value that was offered, and what would have been accepted — because it is
/// shown in the same place: beside the control somebody has just changed
/// (AGENTS.md section 45).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotTrueOrFalse {
    /// Which switch.
    pub category: NotificationCategory,
    /// What was offered.
    pub value: String,
}

impl core::fmt::Display for NotTrueOrFalse {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "{} is {:?}; it accepts {ACCEPTED}",
            self.category, self.value
        )
    }
}

impl std::error::Error for NotTrueOrFalse {}

/// Which notifications the user wants.
///
/// One [`Option<bool>`] per category and nothing else: `None` is "this file says
/// nothing", which is what makes clearing a switch different from setting it to
/// the value that happens to be the default today — the same three-state model
/// every other layer of this configuration uses (`docs/configuration.md`).
///
/// Stored as an array indexed by [`NotificationCategory::index`] rather than as
/// four named fields, so that a category added to
/// [`NotificationCategory::ALL`] has somewhere to be stored without anybody
/// remembering to add it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NotificationSettings {
    configured: [Option<bool>; NotificationCategory::ALL.len()],
    /// Keys this build could not read, kept so that writing the file back does
    /// not delete what a newer build stored.
    unknown: BTreeMap<String, Value>,
}

impl NotificationSettings {
    /// Nothing configured, which is what a file with no `notifications` section
    /// means: every category interrupts.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether this category may interrupt the user.
    ///
    /// The resolved answer, which is the shipped default where the file says
    /// nothing.
    #[must_use]
    pub fn is_enabled(&self, category: NotificationCategory) -> bool {
        self.configured[category.index()].unwrap_or(true)
    }

    /// What the file says about this category, where it says anything.
    ///
    /// [`None`] is what a Reset control is disabled by: the value is the one
    /// Clipped ships with and follows.
    #[must_use]
    pub fn configured(&self, category: NotificationCategory) -> Option<bool> {
        self.configured[category.index()]
    }

    /// Switches a category on or off, or clears it.
    pub fn set(&mut self, category: NotificationCategory, enabled: Option<bool>) {
        self.configured[category.index()] = enabled;
    }

    /// Sets a switch from the text the settings file spells it with.
    ///
    /// The one parser, for the reason
    /// [`Preferences::set_written`](super::Preferences::set_written) is the one
    /// parser for the settings it covers: a value a settings screen can save is
    /// exactly a value the file would accept, refused with the same sentence
    /// when it is not (AGENTS.md section 55). [`None`] clears the switch, which
    /// is Reset.
    ///
    /// # Errors
    ///
    /// [`NotTrueOrFalse`] naming the category, the value and what would have
    /// been accepted.
    pub fn set_written(
        &mut self,
        category: NotificationCategory,
        token: Option<&str>,
    ) -> Result<(), NotTrueOrFalse> {
        let enabled = match token {
            None => None,
            Some(text) => Some(parse(category, text)?),
        };
        self.set(category, enabled);
        Ok(())
    }

    /// What this category resolves to, spelled the way the file spells it.
    #[must_use]
    pub fn written_value(&self, category: NotificationCategory) -> String {
        if self.is_enabled(category) {
            "true".to_owned()
        } else {
            "false".to_owned()
        }
    }

    /// Whether anything here has been configured at all.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.configured.iter().all(Option::is_none) && self.unknown.is_empty()
    }

    /// Keeps a key this build did not understand.
    fn keep_unrecognised(&mut self, key: String, value: Value) {
        self.unknown.insert(key, value);
    }
}

/// One switch, from the text the settings file spells it with.
fn parse(category: NotificationCategory, token: &str) -> Result<bool, NotTrueOrFalse> {
    match token.trim() {
        // Case-insensitively, for the reason every other token parser in this
        // module is: a file somebody edited by hand is a file somebody may have
        // typed `True` into, and JSON's own spelling is the one written back.
        text if text.eq_ignore_ascii_case("true") => Ok(true),
        text if text.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err(NotTrueOrFalse {
            category,
            value: token.to_owned(),
        }),
    }
}

/// Reads the `notifications` section.
///
/// Never fails. A key this build has never heard of, and a key it knows holding
/// something that is not a boolean, are both **kept** and both leave the
/// category on — see the module documentation for why that is the opposite
/// choice from `storage`.
pub(crate) fn read(object: Option<Map<String, Value>>) -> NotificationSettings {
    let mut settings = NotificationSettings::none();
    let Some(object) = object else {
        return settings;
    };

    for (key, value) in object {
        // A key present with `null` says the same thing as an absent key, which
        // is what a settings screen writes when somebody presses Reset.
        if value.is_null() {
            continue;
        }
        match (NotificationCategory::from_key(&key), value.as_bool()) {
            (Some(category), Some(enabled)) => settings.set(category, Some(enabled)),
            _ => settings.keep_unrecognised(key, value),
        }
    }

    settings
}

/// Writes the section back, including what this build did not understand.
pub(crate) fn write(settings: &NotificationSettings) -> Map<String, Value> {
    let mut object = Map::new();
    for category in NotificationCategory::ALL {
        if let Some(enabled) = settings.configured(category) {
            object.insert(category.key().to_owned(), Value::from(enabled));
        }
    }
    for (key, value) in &settings.unknown {
        object.insert(key.clone(), value.clone());
    }
    object
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(json: &str) -> Option<Map<String, Value>> {
        match serde_json::from_str::<Value>(json).expect("the fixture is valid JSON") {
            Value::Object(object) => Some(object),
            other => panic!("the fixture is not an object: {other}"),
        }
    }

    #[test]
    fn a_file_with_no_notifications_section_tells_the_user_everything() {
        // The shipped default. Every category is a failure, so silence is never
        // what an unconfigured machine gets.
        let settings = read(None);

        for category in NotificationCategory::ALL {
            assert!(
                settings.is_enabled(category),
                "{category} should be on by default"
            );
            assert_eq!(settings.configured(category), None);
        }
        assert!(write(&settings).is_empty(), "and it writes nothing back");
        assert!(settings.is_default());
    }

    #[test]
    fn a_category_switched_off_survives_being_written_and_read_back() {
        // The whole point of moving these into the settings file: a switch
        // somebody moved has to still be where they left it.
        let mut settings = NotificationSettings::none();
        settings.set(NotificationCategory::RecordingFailed, Some(false));

        let written = write(&settings);
        assert_eq!(
            written.get("recording_failed"),
            Some(&Value::Bool(false)),
            "the key is the one `notifications.json` used, so a migrated file reads back",
        );
        assert_eq!(read(Some(written)), settings);
    }

    #[test]
    fn switching_one_category_off_leaves_the_others_alone() {
        for category in NotificationCategory::ALL {
            let mut settings = NotificationSettings::none();
            settings.set(category, Some(false));

            assert!(!settings.is_enabled(category), "{category} is still on");
            for other in NotificationCategory::ALL {
                if other != category {
                    assert!(
                        settings.is_enabled(other),
                        "switching off {category} also silenced {other}",
                    );
                }
            }
        }
    }

    #[test]
    fn every_category_has_a_key_of_its_own_and_reads_back_as_itself() {
        // Two categories sharing a key would make one of them unswitchable, and
        // the array they are stored in is indexed by their order in `ALL`.
        let mut keys: Vec<&str> = NotificationCategory::ALL
            .iter()
            .map(|category| category.key())
            .collect();
        keys.sort_unstable();
        let unique = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), unique, "two categories share a key");

        for (index, category) in NotificationCategory::ALL.into_iter().enumerate() {
            assert_eq!(
                NotificationCategory::from_key(category.key()),
                Some(category)
            );
            assert_eq!(
                category.index(),
                index,
                "{category} is not stored where `ALL` puts it",
            );
            assert!(!category.label().is_empty(), "{category} has no label");
        }
    }

    #[test]
    fn a_switch_set_from_text_is_the_switch_the_file_would_have_carried() {
        // The protocol sends the words the file spells a value in, and this is
        // the one parser for them.
        let mut settings = NotificationSettings::none();
        settings
            .set_written(NotificationCategory::RecorderUnavailable, Some("false"))
            .expect("false is a value a switch takes");

        assert!(!settings.is_enabled(NotificationCategory::RecorderUnavailable));
        assert_eq!(
            settings.written_value(NotificationCategory::RecorderUnavailable),
            "false",
            "what a window is told is what it just sent",
        );
        assert_eq!(
            read(Some(write(&settings))),
            settings,
            "and it is what the file carries",
        );
    }

    #[test]
    fn every_value_a_switch_offers_is_one_the_setter_accepts() {
        // The list a screen draws its options from. An option the setter then
        // refuses is a control that fails when it is used.
        for category in NotificationCategory::ALL {
            for choice in NotificationCategory::choices() {
                NotificationSettings::none()
                    .set_written(category, Some(&choice))
                    .unwrap_or_else(|error| {
                        panic!("{category} offers {choice} and refuses it: {error}")
                    });
            }
        }
    }

    #[test]
    fn a_value_that_is_not_true_or_false_is_refused_and_says_what_would_have_been() {
        let refusal = NotificationSettings::none()
            .set_written(NotificationCategory::RecordingFailed, Some("maybe"))
            .expect_err("a switch has two positions");

        let message = refusal.to_string();
        assert!(
            message.contains("recording_failed") && message.contains("maybe"),
            "the refusal should name the switch and the value: {message}",
        );
        assert!(
            message.contains(ACCEPTED),
            "and what would have been accepted: {message}",
        );
    }

    #[test]
    fn clearing_a_switch_returns_it_to_the_default_and_stops_writing_it() {
        // Reset. Not "set it to true": one follows a later change to the
        // default and the other does not.
        let mut settings = NotificationSettings::none();
        settings.set(NotificationCategory::HotkeyUnavailable, Some(false));

        settings
            .set_written(NotificationCategory::HotkeyUnavailable, None)
            .expect("clearing is always allowed");

        assert!(settings.is_enabled(NotificationCategory::HotkeyUnavailable));
        assert_eq!(
            settings.configured(NotificationCategory::HotkeyUnavailable),
            None
        );
        assert!(!write(&settings).contains_key("hotkey_unavailable"));
    }

    #[test]
    fn a_null_says_the_same_thing_as_an_absent_key() {
        let settings = read(object(r#"{"recording_failed": null}"#));

        assert!(settings.is_enabled(NotificationCategory::RecordingFailed));
        assert!(write(&settings).is_empty());
    }

    #[test]
    fn a_key_this_build_does_not_understand_is_kept_rather_than_dropped() {
        // Somebody's file, written by a build that may know more than this one.
        let settings = read(object(
            r#"{"recording_failed": false, "replay_saved": true}"#,
        ));

        assert!(!settings.is_enabled(NotificationCategory::RecordingFailed));
        assert_eq!(
            write(&settings).get("replay_saved"),
            Some(&Value::Bool(true)),
            "a category a newer Clipped added must survive being read and saved here",
        );
    }

    #[test]
    fn a_known_key_that_is_not_a_boolean_leaves_the_category_on_and_is_kept() {
        // The opposite choice from `storage`, and the module documentation says
        // why: a switch nobody can read is a nuisance, and refusing would stop
        // the recording settings in the same file from loading over it.
        let settings = read(object(r#"{"recording_failed": "no"}"#));

        assert!(
            settings.is_enabled(NotificationCategory::RecordingFailed),
            "a value this build cannot read must not silence a failure",
        );
        assert_eq!(
            write(&settings).get("recording_failed"),
            Some(&Value::from("no")),
            "and it is written back rather than deleted",
        );
    }
}
