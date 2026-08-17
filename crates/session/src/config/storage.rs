//! Where the library lives and what it is allowed to occupy, as the settings
//! file holds it.
//!
//! # Why this is a section of its own
//!
//! Every other setting is a [`SettingKey`](super::SettingKey), which is the
//! vocabulary of settings that resolve **per game**: a frame rate for
//! Counter-Strike, a microphone for Dota. A storage limit does not resolve that
//! way and cannot, because a library is one thing however many games are in it.
//! "What is this game's maximum library size" has no answer, and putting these
//! keys in that vocabulary would have forced one
//! ([issue #111](https://github.com/wildware-uk/clipped/issues/111)).
//!
//! [`RECORDING_DIRECTORY`] joined them for the same reason and a second one:
//! SPEC.md section 31 lists what a game may override, and where its recordings
//! are written is not on it
//! ([issue #307](https://github.com/wildware-uk/clipped/issues/307)). It is
//! also the key a settings screen reaches for first (SPEC.md section 45,
//! step 3), and a per-game recording directory would put one game's sittings
//! outside the root the library indexes, storage accounting measures and
//! cleanup sweeps.
//!
//! So they live beside `games`, `hotkeys` and `plugins` as a section of the
//! document, in the shape [`super::plugins`] established: read into a value,
//! written back from it, and keeping whatever a newer build wrote so that
//! reading a file and saving it does not delete what this build did not
//! understand (AGENTS.md section 56).
//!
//! # Nothing is limited unless somebody says so
//!
//! [`StorageSettings::none`] is what a file with no `storage` section means, and
//! it resolves to [`StorageLimits::unlimited`]. That is not a placeholder: it is
//! the shipped default, and it is why wiring automatic cleanup cannot delete
//! anybody's recordings on its own. A sweep asks these limits first and stops
//! before it reads a single directory when there is nothing to enforce.
//!
//! # The bounds are `clipped-library`'s
//!
//! `MINIMUM_QUOTA` and `MAXIMUM_AGE_FLOOR` are that crate's constants and the
//! validation here is its constructors, not a second opinion about what a valid
//! limit is. A quota under a gigabyte is breached from the first session and a
//! maximum age under a day deletes footage recorded this afternoon; both are
//! refused where they are defined, and this module reports the refusal rather
//! than restating the rule (AGENTS.md section 55).

use core::time::Duration;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clipped_library::accounting::{LimitError, StorageLimits};
use serde_json::{Map, Value};

/// The key holding how much the library may occupy, in bytes.
pub(crate) const MAXIMUM_USAGE: &str = "maximum_usage_bytes";

/// The key holding how much of the drive to leave free, in bytes.
pub(crate) const MINIMUM_FREE_SPACE: &str = "minimum_free_space_bytes";

/// The key holding how old a recording may get, in whole days.
///
/// Days rather than seconds because that is the unit a person sets a retention
/// policy in, and because a value the writer could not put back unchanged is a
/// setting that edits itself.
pub(crate) const MAXIMUM_AGE_DAYS: &str = "maximum_age_days";

/// The key naming where deleted media waits out its retention.
pub(crate) const TRASH_DIRECTORY: &str = "trash_directory";

/// The key naming where recordings are written.
///
/// **Step 3 of the MVP definition** (SPEC.md section 45): a fresh user picks a
/// microphone and a recording directory once, closes the window, and never
/// configures capture again. Before
/// [issue #307](https://github.com/wildware-uk/clipped/issues/307) there was
/// nowhere for that answer to be saved — the directory was a per-run command
/// line flag and otherwise the Clipped folder of the user's videos directory,
/// so a settings screen had a control with nothing behind it.
///
/// It is in this section rather than the per-game vocabulary for the reason the
/// limits are: SPEC.md section 31 lists what a game may override and where its
/// recordings are written is not on it. One library, one place it lives:
/// "where does Counter-Strike record to" has an answer only if the library is
/// allowed to be several libraries, which it is not.
pub(crate) const RECORDING_DIRECTORY: &str = "recording_directory";

/// Seconds in a day, for the one conversion this module makes.
const DAY: u64 = 86_400;

/// What is appended to the output directory's name to make the default trash.
///
/// `D:\Clips` becomes `D:\Clips.trash`: beside the recordings rather than
/// inside them, because a trash inside the recordings root would be counted as
/// recordings by storage accounting — and `StorageRoots` refuses that overlap
/// outright. Beside rather than under a parent nobody chose, so that a user
/// looking for their deleted footage finds it next to the folder they picked.
const TRASH_SUFFIX: &str = ".trash";

/// Why a `storage` value was refused.
///
/// Three shapes, because the refusals come from different places: a limit is
/// `clipped-library`'s to judge, and a path is this crate's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageProblem {
    /// A limit outside the bounds `clipped-library` sets.
    Limit(LimitError),
    /// A directory setting that cannot be used.
    DirectoryPath(DirectoryPathError),
}

impl core::fmt::Display for StorageProblem {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Limit(error) => error.fmt(formatter),
            Self::DirectoryPath(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StorageProblem {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Limit(error) => Some(error),
            Self::DirectoryPath(error) => Some(error),
        }
    }
}

/// Why a directory setting was refused.
///
/// Every message names the value and what would have been accepted, because
/// this is what a settings screen shows somebody who has just typed a path
/// (AGENTS.md section 45).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryPathError {
    /// Nothing, or only whitespace. Distinct from [`Self::NotAbsolute`] because
    /// `""` is not a path a user can be told to make absolute, and because
    /// clearing a directory setting is [`None`] rather than a blank string.
    Blank {
        /// The key that carried it, so the message can name the setting.
        setting: &'static str,
    },
    /// The path is relative, so what it names depends on where Clipped was
    /// started from — which is not a thing anybody's footage should depend on.
    ///
    /// Both directory settings in this section have the same rule, and this
    /// error names which one was written rather than each having an error of
    /// its own that says the same thing (AGENTS.md section 55).
    NotAbsolute {
        /// The key that carried it, so the message can name the setting.
        setting: &'static str,
        /// What was written.
        path: PathBuf,
    },
}

impl core::fmt::Display for DirectoryPathError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Blank { setting } => write!(
                formatter,
                "`{setting}` cannot be blank; name a folder such as `D:\\Clips`, or clear the \
                 setting to fall back to the default",
            ),
            Self::NotAbsolute { setting, path } => write!(
                formatter,
                "`{}` is not an absolute path, and `{setting}` must not depend on where Clipped \
                 was started from; name a folder such as `D:\\Clips`",
                path.display()
            ),
        }
    }
}

impl std::error::Error for DirectoryPathError {}

/// Where deleted media goes when nothing says otherwise.
///
/// `D:\Clips` becomes `D:\Clips.trash`. Beside the recordings, on the same
/// volume — deletion is a rename — and outside the recordings root, which
/// storage accounting counts and `StorageRoots` refuses to overlap.
#[must_use]
pub fn trash_beside(recordings: &Path) -> PathBuf {
    let mut name = recordings.as_os_str().to_os_string();
    name.push(TRASH_SUFFIX);
    PathBuf::from(name)
}

/// What the user has said the library may occupy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageSettings {
    maximum_usage: Option<u64>,
    minimum_free_space: Option<u64>,
    maximum_age_days: Option<u64>,
    trash_directory: Option<PathBuf>,
    recording_directory: Option<PathBuf>,
    /// Keys this build could not read, kept so that writing the file back does
    /// not delete what a newer build stored.
    unknown: BTreeMap<String, Value>,
}

impl StorageSettings {
    /// No limit of any kind, which is what a file with no `storage` section
    /// means and what Clipped ships with.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The limits these settings describe.
    ///
    /// [`StorageLimits::unlimited`] when nothing is set, which is the value that
    /// makes a sweep do nothing at all.
    ///
    /// Every value here was validated when it was set or read, so this cannot
    /// fail: a limit that did not satisfy `clipped-library`'s bounds never
    /// reached this type.
    #[must_use]
    pub fn limits(&self) -> StorageLimits {
        let mut limits = StorageLimits::unlimited();
        if let Some(bytes) = self.maximum_usage {
            limits = limits.with_maximum_usage(bytes).unwrap_or(limits);
        }
        if let Some(bytes) = self.minimum_free_space {
            limits = limits.with_minimum_free_space(bytes);
        }
        if let Some(days) = self.maximum_age_days {
            limits = limits
                .with_maximum_age(Duration::from_secs(days.saturating_mul(DAY)))
                .unwrap_or(limits);
        }
        limits
    }

    /// Whether nothing is limited.
    #[must_use]
    pub fn is_unlimited(&self) -> bool {
        self.limits().is_unlimited()
    }

    /// Sets how much the library may occupy, in bytes.
    ///
    /// # Errors
    ///
    /// [`LimitError`] from `clipped-library`, whose floor this is.
    pub fn set_maximum_usage(&mut self, bytes: Option<u64>) -> Result<(), LimitError> {
        if let Some(bytes) = bytes {
            StorageLimits::unlimited().with_maximum_usage(bytes)?;
        }
        self.maximum_usage = bytes;
        Ok(())
    }

    /// Sets how much of the output drive to leave free, in bytes.
    pub const fn set_minimum_free_space(&mut self, bytes: Option<u64>) {
        self.minimum_free_space = bytes;
    }

    /// Sets how old a recording may get, in whole days.
    ///
    /// # Errors
    ///
    /// [`LimitError`] from `clipped-library`, whose floor this is.
    pub fn set_maximum_age_days(&mut self, days: Option<u64>) -> Result<(), LimitError> {
        if let Some(days) = days {
            StorageLimits::unlimited()
                .with_maximum_age(Duration::from_secs(days.saturating_mul(DAY)))?;
        }
        self.maximum_age_days = days;
        Ok(())
    }

    /// How much the library may occupy, where that is set.
    #[must_use]
    pub const fn maximum_usage(&self) -> Option<u64> {
        self.maximum_usage
    }

    /// How much of the drive to leave free, where that is set.
    #[must_use]
    pub const fn minimum_free_space(&self) -> Option<u64> {
        self.minimum_free_space
    }

    /// How old a recording may get, in whole days, where that is set.
    #[must_use]
    pub const fn maximum_age_days(&self) -> Option<u64> {
        self.maximum_age_days
    }

    /// Where deleted media waits out its retention, where that is set.
    ///
    /// [`None`] means [`trash_beside`], which is what a file that says nothing
    /// resolves to.
    #[must_use]
    pub fn trash_directory(&self) -> Option<&Path> {
        self.trash_directory.as_deref()
    }

    /// Sets where deleted media waits out its retention.
    ///
    /// # Errors
    ///
    /// [`DirectoryPathError`] for a path that is not absolute. Everything else that
    /// can be wrong with it — a different volume from the media, an overlap
    /// with the recordings — is reported where it is discovered: `StorageRoots`
    /// refuses the overlap when the sweep builds them, and
    /// `Trash::send` reports a cross-volume rename against the item it could
    /// not move. Restating either here would be a second opinion that could
    /// disagree.
    pub fn set_trash_directory(&mut self, path: Option<PathBuf>) -> Result<(), DirectoryPathError> {
        if let Some(path) = &path {
            if !path.is_absolute() {
                return Err(DirectoryPathError::NotAbsolute {
                    setting: TRASH_DIRECTORY,
                    path: path.clone(),
                });
            }
        }
        self.trash_directory = path;
        Ok(())
    }

    /// Where recordings are written, where the user has chosen.
    ///
    /// [`None`] means the caller's own default — the Clipped folder of the
    /// user's videos directory, which is what `apps/recorder` resolves. A
    /// setting that is not set does not become a path here, because this crate
    /// does not know a user's videos directory and inventing one would put two
    /// answers to the same question in the workspace (AGENTS.md section 55).
    #[must_use]
    pub fn recording_directory(&self) -> Option<&Path> {
        self.recording_directory.as_deref()
    }

    /// Sets where recordings are written.
    ///
    /// # Errors
    ///
    /// [`DirectoryPathError`] for a blank path or one that is not absolute, for
    /// the reason a trash directory must be absolute: a recording written
    /// relative to wherever the recorder happened to be started from is a
    /// recording nobody can find twice, and the recorder is started by the
    /// shell's `Run` key with a working directory the user never chose. Blank
    /// is refused separately from relative because `""` is not a path anybody
    /// can be told to make absolute, and because clearing the setting is
    /// [`None`] rather than an empty string.
    ///
    /// Whether the directory exists, is writable, or is on a volume with room
    /// is deliberately **not** checked here. A settings file is read at start-up
    /// and a drive can be unplugged after it; a removable drive that is not
    /// plugged in when somebody opens the settings screen is not a reason to
    /// refuse the setting. The answer that matters is the one at the moment a
    /// recording starts, and that is where it is reported (`crate::disk`,
    /// `SessionError::OutputDirectory`, and `check_directory_usable` in
    /// `apps/recorder`).
    pub fn set_recording_directory(
        &mut self,
        path: Option<PathBuf>,
    ) -> Result<(), DirectoryPathError> {
        if let Some(path) = &path {
            if path.as_os_str().is_empty() || path.to_string_lossy().trim().is_empty() {
                return Err(DirectoryPathError::Blank {
                    setting: RECORDING_DIRECTORY,
                });
            }
            if !path.is_absolute() {
                return Err(DirectoryPathError::NotAbsolute {
                    setting: RECORDING_DIRECTORY,
                    path: path.clone(),
                });
            }
        }
        self.recording_directory = path;
        Ok(())
    }

    /// Keeps a key this build did not understand.
    fn keep_unrecognised(&mut self, key: String, value: Value) {
        self.unknown.insert(key, value);
    }
}

/// Reads the `storage` section, refusing a value outside the library's bounds.
///
/// A key of the wrong JSON type, or one this build has never heard of, is
/// **kept** rather than refused: it is somebody's file and a newer build may
/// have written it. A key this build *does* know, holding a number outside the
/// bounds, is refused — because that is a limit the user meant and did not get,
/// and silently ignoring it would leave them believing their library is capped
/// when it is not (AGENTS.md section 27).
///
/// # Errors
///
/// [`StorageProblem`] with the key that carried it.
pub(crate) fn read(
    object: Option<Map<String, Value>>,
) -> Result<StorageSettings, (&'static str, StorageProblem)> {
    let mut settings = StorageSettings::none();
    let Some(object) = object else {
        return Ok(settings);
    };

    for (key, value) in object {
        // A key present with `null` says the same thing as an absent key, which
        // is what a settings screen writes when somebody presses Reset.
        if value.is_null() {
            continue;
        }
        match (key.as_str(), value.as_u64()) {
            (MAXIMUM_USAGE, Some(bytes)) => settings
                .set_maximum_usage(Some(bytes))
                .map_err(|error| (MAXIMUM_USAGE, StorageProblem::Limit(error)))?,
            (MINIMUM_FREE_SPACE, Some(bytes)) => settings.set_minimum_free_space(Some(bytes)),
            (MAXIMUM_AGE_DAYS, Some(days)) => settings
                .set_maximum_age_days(Some(days))
                .map_err(|error| (MAXIMUM_AGE_DAYS, StorageProblem::Limit(error)))?,
            (TRASH_DIRECTORY, _) => match value.as_str() {
                Some(path) => settings
                    .set_trash_directory(Some(PathBuf::from(path)))
                    .map_err(|error| (TRASH_DIRECTORY, StorageProblem::DirectoryPath(error)))?,
                None => settings.keep_unrecognised(key, value),
            },
            (RECORDING_DIRECTORY, _) => match value.as_str() {
                Some(path) => settings
                    .set_recording_directory(Some(PathBuf::from(path)))
                    .map_err(|error| (RECORDING_DIRECTORY, StorageProblem::DirectoryPath(error)))?,
                None => settings.keep_unrecognised(key, value),
            },
            _ => settings.keep_unrecognised(key, value),
        }
    }

    Ok(settings)
}

/// Writes the section back, including what this build did not understand.
pub(crate) fn write(settings: &StorageSettings) -> Map<String, Value> {
    let mut object = Map::new();
    if let Some(bytes) = settings.maximum_usage {
        object.insert(MAXIMUM_USAGE.to_owned(), Value::from(bytes));
    }
    if let Some(bytes) = settings.minimum_free_space {
        object.insert(MINIMUM_FREE_SPACE.to_owned(), Value::from(bytes));
    }
    if let Some(days) = settings.maximum_age_days {
        object.insert(MAXIMUM_AGE_DAYS.to_owned(), Value::from(days));
    }
    if let Some(path) = &settings.trash_directory {
        object.insert(
            TRASH_DIRECTORY.to_owned(),
            Value::from(path.to_string_lossy().into_owned()),
        );
    }
    if let Some(path) = &settings.recording_directory {
        object.insert(
            RECORDING_DIRECTORY.to_owned(),
            Value::from(path.to_string_lossy().into_owned()),
        );
    }
    for (key, value) in &settings.unknown {
        object.insert(key.clone(), value.clone());
    }
    object
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipped_library::accounting::{MAXIMUM_AGE_FLOOR, MINIMUM_QUOTA};

    fn object(json: &str) -> Option<Map<String, Value>> {
        match serde_json::from_str::<Value>(json).expect("the fixture is valid JSON") {
            Value::Object(object) => Some(object),
            other => panic!("the fixture is not an object: {other}"),
        }
    }

    /// Step 3 of the MVP definition needs somewhere to save the answer, and the
    /// answer has to survive being written and read again (issue #307).
    #[test]
    fn the_recording_directory_survives_a_round_trip() {
        let settings = read(object(r#"{"recording_directory": "D:\\Clips"}"#))
            .expect("an absolute path is accepted");

        assert_eq!(
            settings.recording_directory(),
            Some(Path::new(r"D:\Clips")),
            "the directory a settings screen saved is the directory that is read back"
        );
        assert_eq!(
            write(&settings).get(RECORDING_DIRECTORY),
            Some(&Value::from(r"D:\Clips")),
            "and writing the file back does not lose it"
        );
    }

    /// The recorder is started by the shell's `Run` key, with a working
    /// directory nobody chose, so a relative path names somewhere different
    /// every time.
    #[test]
    fn a_relative_recording_directory_is_refused_and_names_the_setting() {
        let error = read(object(r#"{"recording_directory": "Clips"}"#))
            .expect_err("a relative path is not a place");

        assert_eq!(error.0, RECORDING_DIRECTORY);
        assert!(
            matches!(
                &error.1,
                StorageProblem::DirectoryPath(DirectoryPathError::NotAbsolute { setting, .. })
                    if *setting == RECORDING_DIRECTORY
            ),
            "the refusal has to name which of the two directory settings was wrong, got {:?}",
            error.1
        );
    }

    /// The two directory settings are independent: setting one does not disturb
    /// the other, and the message names whichever was written.
    #[test]
    fn the_two_directory_settings_do_not_share_a_value() {
        let settings = read(object(
            r#"{"recording_directory": "D:\\Clips", "trash_directory": "D:\\Bin"}"#,
        ))
        .expect("both are absolute");

        assert_eq!(settings.recording_directory(), Some(Path::new(r"D:\Clips")));
        assert_eq!(settings.trash_directory(), Some(Path::new(r"D:\Bin")));
    }

    #[test]
    fn a_file_with_no_storage_section_limits_nothing() {
        // The shipped default, and the reason wiring automatic cleanup cannot
        // delete anybody's recordings on its own.
        let settings = read(None).expect("nothing to refuse");

        assert!(settings.is_unlimited());
        assert_eq!(settings.limits(), StorageLimits::unlimited());
        assert!(write(&settings).is_empty(), "and it writes nothing back");
    }

    #[test]
    fn the_three_limits_survive_a_round_trip() {
        let settings = read(object(
            r#"{"maximum_usage_bytes": 500000000000,
                "minimum_free_space_bytes": 21474836480,
                "maximum_age_days": 90}"#,
        ))
        .expect("every value is inside the bounds");

        let limits = settings.limits();
        assert_eq!(limits.maximum_usage(), Some(500_000_000_000));
        assert_eq!(limits.minimum_free_space(), Some(21_474_836_480));
        assert_eq!(
            limits.maximum_age(),
            Some(Duration::from_secs(90 * DAY)),
            "days are what the file holds and seconds are what the limit takes"
        );

        let written = write(&settings);
        assert_eq!(written.get(MAXIMUM_AGE_DAYS), Some(&Value::from(90_u64)));
        assert_eq!(
            read(Some(written)).expect("what was written reads back"),
            settings
        );
    }

    #[test]
    fn a_quota_below_the_librarys_floor_is_refused_with_the_key() {
        // Not clamped and not ignored: a quota under a gigabyte can only be
        // satisfied by a library with nothing in it, and honouring it would
        // delete everything the user has. The floor is `clipped-library`'s and
        // so is the refusal.
        let (key, error) = read(object(&format!(
            r#"{{"maximum_usage_bytes": {}}}"#,
            MINIMUM_QUOTA - 1
        )))
        .expect_err("a quota below the floor is refused");

        assert_eq!(key, MAXIMUM_USAGE);
        let message = error.to_string();
        assert!(
            message.contains("too small") && message.contains("no limit"),
            "the message should say what would have been accepted and that unset means no limit:              {message}"
        );
    }

    #[test]
    fn a_maximum_age_below_a_day_is_refused() {
        assert!(MAXIMUM_AGE_FLOOR.as_secs() == DAY, "the floor is a day");

        let (key, _) = read(object(r#"{"maximum_age_days": 0}"#))
            .expect_err("zero days deletes a recording as it is written");

        assert_eq!(key, MAXIMUM_AGE_DAYS);
    }

    #[test]
    fn a_key_this_build_does_not_understand_is_kept_rather_than_dropped() {
        // Somebody's file, written by a build that may know more than this one.
        // Reading it and saving it must not delete what it said.
        let settings = read(object(
            r#"{"maximum_usage_bytes": 500000000000, "delete_on_tuesdays": true}"#,
        ))
        .expect("an unknown key is not a refusal");

        let written = write(&settings);
        assert_eq!(written.get("delete_on_tuesdays"), Some(&Value::Bool(true)));
        assert_eq!(settings.limits().maximum_usage(), Some(500_000_000_000));
    }

    #[test]
    fn a_known_key_of_the_wrong_type_is_kept_rather_than_guessed_at() {
        // `"maximum_usage_bytes": "lots"` is not a number this build can act on.
        // Keeping it means the file survives; acting on it would mean inventing
        // a limit nobody set.
        let settings = read(object(r#"{"maximum_usage_bytes": "lots"}"#))
            .expect("a wrong type is not a refusal");

        assert!(settings.is_unlimited());
        assert_eq!(
            write(&settings).get(MAXIMUM_USAGE),
            Some(&Value::from("lots"))
        );
    }

    #[test]
    fn the_recording_directory_survives_a_round_trip_through_the_file() {
        // Step 3 of SPEC.md section 45's MVP: the directory somebody picked has
        // to come back out of the file as the directory they picked.
        let settings = read(object(r#"{"recording_directory": "D:\\Clips"}"#))
            .expect("an absolute path is accepted");

        assert_eq!(settings.recording_directory(), Some(Path::new(r"D:\Clips")));

        let written = write(&settings);
        assert_eq!(
            written.get(RECORDING_DIRECTORY),
            Some(&Value::from(r"D:\Clips")),
        );
        assert_eq!(
            read(Some(written)).expect("what was written reads back"),
            settings,
        );
    }

    #[test]
    fn a_relative_recording_directory_is_refused_and_says_what_would_be_accepted() {
        // Not silently resolved against the recorder's working directory: that
        // is a folder the user did not pick and cannot predict.
        let (key, problem) = read(object(r#"{"recording_directory": "clips"}"#))
            .expect_err("a relative path is refused");

        assert_eq!(key, RECORDING_DIRECTORY);
        let message = problem.to_string();
        assert!(
            message.contains("clips") && message.contains("absolute"),
            "the refusal should name the value and what would have been accepted: {message}",
        );
    }

    #[test]
    fn a_blank_recording_directory_is_refused_rather_than_written_as_an_empty_path() {
        let (key, problem) = read(object(r#"{"recording_directory": "   "}"#))
            .expect_err("whitespace is not a directory");

        assert_eq!(key, RECORDING_DIRECTORY);
        assert_eq!(
            problem,
            StorageProblem::DirectoryPath(DirectoryPathError::Blank {
                setting: RECORDING_DIRECTORY,
            }),
            "blank is its own refusal rather than a relative path nobody can be told to make \
             absolute, and it names which of the two directory settings was written"
        );
    }

    #[test]
    fn clearing_the_recording_directory_leaves_the_recorder_to_choose() {
        // What Reset writes. `None` is not a path of its own: it is the absence
        // that the recorder's own default answers.
        let mut settings = read(object(r#"{"recording_directory": "D:\\Clips"}"#))
            .expect("an absolute path is accepted");

        settings
            .set_recording_directory(None)
            .expect("clearing is always allowed");

        assert_eq!(settings.recording_directory(), None);
        assert!(!write(&settings).contains_key(RECORDING_DIRECTORY));
    }

    #[test]
    fn a_null_says_the_same_thing_as_an_absent_key() {
        let settings =
            read(object(r#"{"maximum_usage_bytes": null}"#)).expect("null is not a value");

        assert!(settings.is_unlimited());
        assert!(write(&settings).is_empty());
    }
}
