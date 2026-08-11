//! The file format, its version, and the checks an entry has to pass.
//!
//! # Two shapes, on purpose
//!
//! The `Raw*` structures here are what TOML deserialises into, and the types
//! in [`super::entry`] are what the rest of the crate sees. The conversion
//! between them is the validation, and it is where every error that names a
//! file and an entry comes from. Deriving `Deserialize` straight onto the
//! domain types would have been shorter and would have cost the thing that
//! matters: serde can say "invalid value at line 40", but only this pass can
//! say which `game_id` that entry has, which executable block inside it is
//! wrong, and what to write instead.
//!
//! `deny_unknown_fields` is on every one of them. A misspelled key is the most
//! likely mistake in a hand-written entry, and the alternative to failing is
//! accepting a file where `path_contian` silently does nothing.
//!
//! # The version, and what happens either side of it
//!
//! [`super::SCHEMA_VERSION`] is the version of the *format*, not of Clipped. A
//! file carrying it is read directly. An older one is migrated through
//! [`MIGRATIONS`] before it is read. A newer one is refused, and nothing is
//! written back — a build that cannot understand a file has no business
//! rewriting it (AGENTS.md sections 43 and 56).

use std::collections::btree_map::Entry as MapEntry;
use std::collections::BTreeMap;

use serde::Deserialize;

use super::entry::{
    CaptureCompatibility, CaptureSupport, Entry, EntrySource, ExecutableRule, GameId, Launcher,
    LauncherKind, SettingValue,
};
use super::error::{CatalogueError, EntryLocation, EntryProblem};
use super::matching::normalise_path;

/// One step from one schema version to the next.
///
/// A step is handed the whole document as TOML and may do anything to it; the
/// framework sets `schema_version` to [`Self::to`] afterwards, so a step never
/// has to remember to. Returning `Err` refuses the file rather than writing a
/// half-converted one.
#[derive(Clone, Copy)]
pub(crate) struct Migration {
    /// The version this step reads.
    pub(crate) from: u32,
    /// The version it produces, which must be greater than `from`.
    pub(crate) to: u32,
    /// The conversion.
    pub(crate) apply: fn(&mut toml::Table) -> Result<(), String>,
}

impl core::fmt::Debug for Migration {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Migration")
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

/// Every step this build knows, in no required order.
///
/// **Empty, and correctly so: version 1 is the first version there has ever
/// been, so no file older than the current schema exists anywhere.** Writing a
/// speculative migration for a version that never shipped would be inventing
/// history.
///
/// What is not speculative is the machinery that will run them, which is why
/// [`migrate`] exists now and is tested now, against migration lists the tests
/// supply themselves. When version 2 arrives, the change is an entry here and
/// a function — the loader, the backup, the atomic write and the refusal to
/// touch a file it cannot convert are already built and already covered.
pub(crate) const MIGRATIONS: &[Migration] = &[];

/// The result of getting a file's text to the current schema and validating it.
#[derive(Debug)]
pub(crate) struct Parsed {
    /// The entries, in file order.
    pub(crate) entries: Vec<Entry>,
    /// The document to write back, when migrating produced a different one.
    /// `None` when the file was already current.
    pub(crate) migrated: Option<MigratedDocument>,
}

/// A document that was migrated, and therefore should replace what is on disk.
#[derive(Debug)]
pub(crate) struct MigratedDocument {
    /// The version the file was found at, which names its backup.
    pub(crate) from: u32,
    /// The version it is now.
    pub(crate) to: u32,
    /// The TOML to write.
    pub(crate) text: String,
}

/// Parses catalogue text, migrating an older schema **in memory only**.
///
/// Writing anything back is [`super::overlay`]'s decision, not this function's:
/// the seed data is parsed by the same code and there is nothing to write it
/// to.
///
/// `target` and `migrations` are parameters rather than constants so that the
/// migration machinery can be exercised by tests that own both ends of a
/// conversion. Production passes [`super::SCHEMA_VERSION`] and [`MIGRATIONS`].
pub(crate) fn parse(
    text: &str,
    file: &EntrySource,
    target: u32,
    migrations: &[Migration],
) -> Result<Parsed, CatalogueError> {
    let document: toml::Table =
        text.parse()
            .map_err(|error: toml::de::Error| CatalogueError::Syntax {
                file: file.clone(),
                message: error.to_string(),
            })?;

    let found = schema_version(&document, file)?;
    if found > target {
        return Err(CatalogueError::SchemaTooNew {
            file: file.clone(),
            found,
            supported: target,
        });
    }

    let (text, migrated) = if found == target {
        (text.to_owned(), None)
    } else {
        let mut document = document;
        migrate(&mut document, found, target, migrations, file)?;
        let converted =
            toml::to_string_pretty(&document).map_err(|error| CatalogueError::MigrationFailed {
                file: file.clone(),
                from: found,
                to: target,
                reason: format!("the converted document could not be written back: {error}"),
            })?;
        (
            converted.clone(),
            Some(MigratedDocument {
                from: found,
                to: target,
                text: converted,
            }),
        )
    };

    let raw: RawFile =
        toml::from_str(&text).map_err(|error: toml::de::Error| CatalogueError::Syntax {
            file: file.clone(),
            message: error.to_string(),
        })?;

    Ok(Parsed {
        entries: validate(raw, file)?,
        migrated,
    })
}

/// Reads `schema_version` out of a document, before anything else is trusted.
///
/// Read from the raw document rather than from a deserialised structure
/// because that is the whole point of it: a version-2 file may have a shape
/// this build cannot deserialise at all, and it still has to be able to say
/// so as a version rather than as a parse failure.
fn schema_version(document: &toml::Table, file: &EntrySource) -> Result<u32, CatalogueError> {
    document
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| CatalogueError::SchemaVersionMissing { file: file.clone() })
}

/// Walks a document from `found` up to `target`, one step at a time.
///
/// Each step's `to` becomes the next step's `from`, so the chain is followed
/// rather than assumed to be a sequence of single increments — a future
/// version 2 to 4 step is expressible. A version with no step out of it stops
/// the walk with [`CatalogueError::MigrationMissing`] and the caller writes
/// nothing.
fn migrate(
    document: &mut toml::Table,
    found: u32,
    target: u32,
    migrations: &[Migration],
    file: &EntrySource,
) -> Result<(), CatalogueError> {
    let mut current = found;
    while current < target {
        let step = migrations
            .iter()
            .find(|migration| migration.from == current && migration.to <= target)
            .ok_or_else(|| CatalogueError::MigrationMissing {
                file: file.clone(),
                from: current,
                to: target,
            })?;
        // A step that did not advance would spin here for ever, and a step
        // that overshot would leave the file claiming a version it is not.
        debug_assert!(step.to > step.from, "a migration must advance the version");
        (step.apply)(document).map_err(|reason| CatalogueError::MigrationFailed {
            file: file.clone(),
            from: step.from,
            to: step.to,
            reason,
        })?;
        document.insert(
            "schema_version".to_owned(),
            toml::Value::Integer(i64::from(step.to)),
        );
        current = step.to;
    }
    Ok(())
}

/// The whole file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    /// Read again here, having already been read out of the document, so that
    /// `deny_unknown_fields` does not reject the key it arrived on.
    #[allow(
        dead_code,
        reason = "consumed by schema_version before deserialisation"
    )]
    schema_version: u32,
    #[serde(default)]
    game: Vec<RawGame>,
}

/// One `[[game]]` block.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGame {
    game_id: String,
    name: String,
    icon: Option<String>,
    #[serde(default)]
    executables: Vec<RawExecutable>,
    launcher: Option<RawLauncher>,
    capture: Option<RawCapture>,
    #[serde(default)]
    child_processes: Vec<String>,
    #[serde(default)]
    default_settings: BTreeMap<String, toml::Value>,
    #[serde(default)]
    highlight_providers: Vec<String>,
}

/// One `[[game.executables]]` block.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExecutable {
    name: String,
    path_contains: Option<String>,
}

/// A `[game.launcher]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLauncher {
    kind: LauncherKind,
    app_id: Option<String>,
}

/// A `[game.capture]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapture {
    #[serde(default)]
    compatibility: CaptureSupport,
    note: Option<String>,
}

/// Turns a deserialised file into entries, or into the first problem found.
///
/// First problem rather than all of them: a file is loaded at start-up and the
/// contributor fixing it re-runs the check, so reporting one precise problem is
/// worth more than a list whose later halves are usually consequences of the
/// first.
fn validate(raw: RawFile, file: &EntrySource) -> Result<Vec<Entry>, CatalogueError> {
    let mut entries = Vec::with_capacity(raw.game.len());
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();

    for (index, game) in raw.game.into_iter().enumerate() {
        let position = index + 1;
        let entry = validate_game(game, position, &mut seen, file)?;
        entries.push(entry);
    }

    Ok(entries)
}

/// Validates one entry, given where it is and what has been seen before it.
fn validate_game(
    game: RawGame,
    position: usize,
    seen: &mut BTreeMap<String, usize>,
    file: &EntrySource,
) -> Result<Entry, CatalogueError> {
    let anonymous = |problem| CatalogueError::InvalidEntry {
        file: file.clone(),
        entry: EntryLocation::at(position),
        problem,
    };

    let game_id = GameId::parse(&game.game_id).ok_or_else(|| {
        anonymous(if game.game_id.is_empty() {
            EntryProblem::GameIdEmpty
        } else {
            EntryProblem::GameIdCharacters {
                game_id: game.game_id.clone(),
            }
        })
    })?;

    let fail = |problem| CatalogueError::InvalidEntry {
        file: file.clone(),
        entry: EntryLocation::named(position, game_id.as_str()),
        problem,
    };

    match seen.entry(game_id.as_str().to_owned()) {
        MapEntry::Occupied(occupied) => {
            return Err(fail(EntryProblem::GameIdDuplicated {
                first_at: *occupied.get(),
            }));
        }
        MapEntry::Vacant(vacant) => {
            vacant.insert(position);
        }
    }

    if game.name.trim().is_empty() {
        return Err(fail(EntryProblem::NameEmpty));
    }
    if game.executables.is_empty() {
        return Err(fail(EntryProblem::NoExecutables));
    }

    let mut executables = Vec::with_capacity(game.executables.len());
    let mut seen_rules: Vec<String> = Vec::with_capacity(game.executables.len());
    for (index, executable) in game.executables.into_iter().enumerate() {
        let position = index + 1;
        if executable.name.trim().is_empty() {
            return Err(fail(EntryProblem::ExecutableNameEmpty { position }));
        }
        if is_path(&executable.name) {
            return Err(fail(EntryProblem::ExecutableNameIsPath {
                position,
                name: executable.name,
            }));
        }

        let rule = match executable.path_contains {
            None => ExecutableRule::new(executable.name),
            Some(fragment) if fragment.trim().is_empty() => {
                return Err(fail(EntryProblem::PathQualifierEmpty { position }));
            }
            Some(fragment) => ExecutableRule::new(executable.name).qualified_by(fragment),
        };

        let key = format!(
            "{}|{}",
            rule.name().to_lowercase(),
            rule.path_contains().map(normalise_path).unwrap_or_default()
        );
        if seen_rules.contains(&key) {
            return Err(fail(EntryProblem::ExecutableDuplicated {
                rule: describe_rule(&rule),
            }));
        }
        seen_rules.push(key);
        executables.push(rule);
    }

    let launcher = match game.launcher {
        None => None,
        Some(raw) => Some(match raw.app_id {
            None => Launcher::new(raw.kind),
            Some(app_id) if app_id.trim().is_empty() => {
                return Err(fail(EntryProblem::LauncherAppIdEmpty));
            }
            Some(app_id) => Launcher::new(raw.kind).with_app_id(app_id),
        }),
    };

    let icon = match game.icon {
        None => None,
        Some(icon) if icon.trim().is_empty() => return Err(fail(EntryProblem::IconEmpty)),
        Some(icon) => Some(icon),
    };

    let capture = match game.capture {
        None => CaptureCompatibility::default(),
        Some(raw) => match raw.note {
            None => CaptureCompatibility::new(raw.compatibility),
            Some(note) if note.trim().is_empty() => {
                return Err(fail(EntryProblem::CaptureNoteEmpty));
            }
            Some(note) => CaptureCompatibility::new(raw.compatibility).with_note(note),
        },
    };

    for child in &game.child_processes {
        if child.trim().is_empty() || is_path(child) {
            return Err(fail(EntryProblem::ChildProcessInvalid {
                name: child.clone(),
            }));
        }
    }

    let mut highlight_providers: Vec<String> = Vec::with_capacity(game.highlight_providers.len());
    for provider in game.highlight_providers {
        if provider.trim().is_empty() {
            return Err(fail(EntryProblem::HighlightProviderEmpty));
        }
        if highlight_providers.contains(&provider) {
            return Err(fail(EntryProblem::HighlightProviderDuplicated { provider }));
        }
        highlight_providers.push(provider);
    }

    let mut default_settings = BTreeMap::new();
    for (key, value) in game.default_settings {
        let value = match value {
            toml::Value::String(value) => SettingValue::Text(value),
            toml::Value::Integer(value) => SettingValue::Integer(value),
            toml::Value::Float(value) => SettingValue::Float(value),
            toml::Value::Boolean(value) => SettingValue::Boolean(value),
            toml::Value::Array(_) => {
                return Err(fail(EntryProblem::DefaultSettingNotScalar {
                    key,
                    found: "an array",
                }));
            }
            toml::Value::Table(_) => {
                return Err(fail(EntryProblem::DefaultSettingNotScalar {
                    key,
                    found: "a table",
                }));
            }
            toml::Value::Datetime(_) => {
                return Err(fail(EntryProblem::DefaultSettingNotScalar {
                    key,
                    found: "a date-time",
                }));
            }
        };
        default_settings.insert(key, value);
    }

    Ok(Entry {
        game_id,
        name: game.name,
        executables,
        launcher,
        icon,
        capture,
        child_processes: game.child_processes,
        default_settings,
        highlight_providers,
        source: file.clone(),
    })
}

/// Whether a name has a directory part, by either separator.
///
/// Both, on every platform: the catalogue is Windows data whatever machine
/// happens to be reading it, and a contributor on any operating system writing
/// `steamapps/common/x/game.exe` into a `name` has made the same mistake.
fn is_path(name: &str) -> bool {
    name.contains('\\') || name.contains('/') || name.contains(':')
}

/// A rule as it would be written in a message.
fn describe_rule(rule: &ExecutableRule) -> String {
    match rule.path_contains() {
        Some(fragment) => format!("`{}` under `{fragment}`", rule.name()),
        None => format!("`{}`", rule.name()),
    }
}
