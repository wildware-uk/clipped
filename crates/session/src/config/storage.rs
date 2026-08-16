//! What the library is allowed to occupy, as the settings file holds it.
//!
//! # Why this is a section of its own
//!
//! Every other setting is a [`SettingKey`](super::SettingKey), which is the
//! vocabulary of settings that resolve **per game**: a frame rate for
//! Counter-Strike, a microphone for Dota. A storage limit does not resolve that
//! way and cannot, because a library is one thing however many games are in it.
//! "What is this game's maximum library size" has no answer, and putting these
//! three keys in that vocabulary would have forced one
//! ([issue #111](https://github.com/wildware-uk/clipped/issues/111)).
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
/// Two shapes, because the two refusals come from different places: a limit is
/// `clipped-library`'s to judge, and a path is this crate's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageProblem {
    /// A limit outside the bounds `clipped-library` sets.
    Limit(LimitError),
    /// A trash directory that cannot be used.
    TrashPath(TrashPathError),
}

impl core::fmt::Display for StorageProblem {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Limit(error) => error.fmt(formatter),
            Self::TrashPath(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StorageProblem {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Limit(error) => Some(error),
            Self::TrashPath(error) => Some(error),
        }
    }
}

/// Why a trash directory was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrashPathError {
    /// The path is relative, so what it names depends on where Clipped was
    /// started from — which is not a thing a user's deleted footage should
    /// depend on.
    NotAbsolute {
        /// What was written.
        path: PathBuf,
    },
}

impl core::fmt::Display for TrashPathError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAbsolute { path } => write!(
                formatter,
                "`{}` is not an absolute path, and where deleted recordings go must not depend                  on where Clipped was started from",
                path.display()
            ),
        }
    }
}

impl std::error::Error for TrashPathError {}

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
    /// [`TrashPathError`] for a path that is not absolute. Everything else that
    /// can be wrong with it — a different volume from the media, an overlap
    /// with the recordings — is reported where it is discovered: `StorageRoots`
    /// refuses the overlap when the sweep builds them, and
    /// `Trash::send` reports a cross-volume rename against the item it could
    /// not move. Restating either here would be a second opinion that could
    /// disagree.
    pub fn set_trash_directory(&mut self, path: Option<PathBuf>) -> Result<(), TrashPathError> {
        if let Some(path) = &path {
            if !path.is_absolute() {
                return Err(TrashPathError::NotAbsolute { path: path.clone() });
            }
        }
        self.trash_directory = path;
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
                    .map_err(|error| (TRASH_DIRECTORY, StorageProblem::TrashPath(error)))?,
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
    fn a_null_says_the_same_thing_as_an_absent_key() {
        let settings =
            read(object(r#"{"maximum_usage_bytes": null}"#)).expect("null is not a value");

        assert!(settings.is_unlimited());
        assert!(write(&settings).is_empty());
    }
}
