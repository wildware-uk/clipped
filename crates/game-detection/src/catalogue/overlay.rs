//! The user's own catalogue file, and the one file here that must never be
//! lost.
//!
//! # Where it is, and why it is not in the repository
//!
//! `%LOCALAPPDATA%\Clipped\games.toml`. The seed data is Clipped's content and
//! is replaced wholesale by every update; this file is the user's, holds
//! entries they wrote for games nobody has contributed yet, and survives every
//! update untouched. Those are two different lifetimes, so they are two
//! different files (AGENTS.md section 56).
//!
//! # Migration
//!
//! The schema is versioned, and the seed data and the overlay share it. When
//! the shape changes, the seed data is simply rewritten in the new shape and
//! shipped — but the overlay is somebody's own file, and "delete it and start
//! again" is not an upgrade path (AGENTS.md section 31 says the same thing
//! about the database). So an overlay older than the current schema is
//! converted, and the conversion is careful in a specific order:
//!
//! 1. Read the file. Nothing is written yet.
//! 2. Convert it in memory, and validate the result. **If either fails, the
//!    file on disk has not been touched at all** and the error says so.
//! 3. Copy the original to `games.toml.v<old>.bak`. If that fails, stop —
//!    still without having touched the original.
//! 4. Write the converted document to a neighbouring temporary file and rename
//!    it over the original, so an interrupted write leaves the previous file
//!    rather than half of a new one.
//!
//! A migration therefore always leaves the user two readable files: the
//! converted one and the exact bytes they had before, comments and all. TOML
//! comments do not survive the conversion — the document is rewritten from its
//! parsed form — which is the other reason the backup is not optional.
//!
//! A file *newer* than this build understands is refused and left alone. An
//! older Clipped rewriting a file it cannot read is precisely how a user loses
//! the entries they added on the machine that was up to date.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clipped_logging::application_directory;

use super::entry::{Entry, EntrySource};
use super::error::CatalogueError;
use super::schema::{self, Migration};

/// The file name inside Clipped's per-user directory.
pub const OVERLAY_FILE_NAME: &str = "games.toml";

/// What happened to the overlay while loading a catalogue.
///
/// Returned rather than logged from here so that the caller decides how to
/// report it. A migration in particular is something a user should be told
/// about — their file was rewritten and a copy was kept.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OverlayStatus {
    /// The environment describes no per-user directory, so there is nowhere
    /// for an overlay to be. The catalogue is the seed data alone.
    NoUserDirectory,
    /// There is no overlay file, which is the state of every installation
    /// until a user adds a game of their own.
    Absent {
        /// Where one would go.
        path: PathBuf,
    },
    /// An overlay at the current schema version was read.
    Loaded {
        /// The file.
        path: PathBuf,
        /// How many entries it contributed.
        entries: usize,
    },
    /// An older overlay was converted, backed up and rewritten.
    Migrated {
        /// The file.
        path: PathBuf,
        /// The version it was.
        from: u32,
        /// The version it is now.
        to: u32,
        /// Where the original bytes were copied before it was rewritten.
        backup: PathBuf,
        /// How many entries it contributed.
        entries: usize,
    },
}

/// Where the overlay lives: `%LOCALAPPDATA%\Clipped\games.toml`.
///
/// `None` when the environment describes no per-user directory, which on
/// Windows means `%LOCALAPPDATA%` is unset. The catalogue is then the seed data
/// alone, which is a working catalogue and not an error.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    Some(application_directory()?.join(OVERLAY_FILE_NAME))
}

/// Reads the overlay at `path`, migrating and rewriting it if it is older.
///
/// `target` and `migrations` are parameters so that the migration machinery is
/// testable end to end against a conversion a test owns both sides of; the
/// public entry points pass [`super::SCHEMA_VERSION`] and
/// [`schema::MIGRATIONS`].
///
/// # Errors
///
/// Every variant of [`CatalogueError`] except those only the seed data can
/// produce. A file that exists and cannot be understood is an error rather
/// than an empty overlay: entries the user added going quietly missing is the
/// failure this whole module exists to avoid.
pub(crate) fn load(
    path: &Path,
    target: u32,
    migrations: &[Migration],
) -> Result<(Vec<Entry>, OverlayStatus), CatalogueError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((
                Vec::new(),
                OverlayStatus::Absent {
                    path: path.to_owned(),
                },
            ));
        }
        Err(error) => {
            return Err(CatalogueError::Read {
                path: path.to_owned(),
                source: error,
            });
        }
    };

    let file = EntrySource::Overlay {
        path: path.to_owned(),
    };
    let parsed = schema::parse(&text, &file, target, migrations)?;
    let entries = parsed.entries;

    let Some(migrated) = parsed.migrated else {
        let status = OverlayStatus::Loaded {
            path: path.to_owned(),
            entries: entries.len(),
        };
        return Ok((entries, status));
    };

    let backup = backup_path(path, migrated.from);
    fs::copy(path, &backup).map_err(|error| CatalogueError::BackupFailed {
        path: path.to_owned(),
        backup: backup.clone(),
        source: error,
    })?;
    write_atomically(path, &migrated.text).map_err(|error| CatalogueError::WriteFailed {
        path: path.to_owned(),
        source: error,
    })?;

    let status = OverlayStatus::Migrated {
        path: path.to_owned(),
        from: migrated.from,
        to: migrated.to,
        backup,
        entries: entries.len(),
    };
    Ok((entries, status))
}

/// Where the original bytes of a version `from` file are kept.
fn backup_path(path: &Path, from: u32) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".v{from}.bak"));
    path.with_file_name(name)
}

/// Writes `text` to `path` through a neighbouring temporary file.
///
/// The temporary name carries the process identifier for the same reason the
/// encoder's capability cache does: two processes writing the same file must
/// not share one temporary and rename half of it into place.
fn write_atomically(path: &Path, text: &str) -> io::Result<()> {
    if let Some(directory) = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
    {
        fs::create_dir_all(directory)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let temporary = path.with_file_name(name);
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::error::CatalogueError;
    use crate::catalogue::SCHEMA_VERSION;

    /// A directory of one test's own, removed when it is dropped.
    #[derive(Debug)]
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        /// Named for the test, the process and the thread, so that tests
        /// running in parallel cannot share one.
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "clipped-catalogue-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("the temporary directory can be created");
            Self(path)
        }

        /// Writes an overlay file into it and returns its path.
        fn with_overlay(&self, text: &str) -> PathBuf {
            let path = self.0.join(OVERLAY_FILE_NAME);
            fs::write(&path, text).expect("the overlay can be written");
            path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A version 1 file, as a user would have written it, comments and all.
    const CURRENT: &str = r#"schema_version = 1

# My own entry, which nobody has contributed upstream yet.
[[game]]
game_id = "my-game"
name = "My Game"

[[game.executables]]
name = "my-game.exe"
"#;

    /// The same file in a hypothetical version 1 that called `name` `title`.
    ///
    /// There has never been such a version — version 1 is the first — so this
    /// is a fixture rather than history. What it exercises is real: the
    /// loader, the chain, the backup and the atomic write are the code that
    /// will run when version 2 arrives.
    const OLDER: &str = r#"schema_version = 1

# My own entry, written before the schema changed.
[[game]]
game_id = "my-game"
title = "My Game"

[[game.executables]]
name = "my-game.exe"
"#;

    /// Renames every entry's `title` to `name`.
    fn rename_title_to_name(document: &mut toml::Table) -> Result<(), String> {
        let Some(games) = document.get_mut("game") else {
            return Ok(());
        };
        let games = games
            .as_array_mut()
            .ok_or_else(|| "`game` is not an array of entries".to_owned())?;
        for game in games {
            let table = game
                .as_table_mut()
                .ok_or_else(|| "an entry is not a table".to_owned())?;
            if let Some(title) = table.remove("title") {
                table.insert("name".to_owned(), title);
            }
        }
        Ok(())
    }

    /// Marks every entry's name, so a second step is visible in the result.
    fn mark_migrated(document: &mut toml::Table) -> Result<(), String> {
        let Some(games) = document.get_mut("game") else {
            return Ok(());
        };
        for game in games
            .as_array_mut()
            .ok_or_else(|| "`game` is not an array of entries".to_owned())?
        {
            let table = game
                .as_table_mut()
                .ok_or_else(|| "an entry is not a table".to_owned())?;
            let name = table
                .get("name")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| "an entry has no `name`".to_owned())?
                .to_owned();
            table.insert(
                "name".to_owned(),
                toml::Value::String(format!("{name} (v3)")),
            );
        }
        Ok(())
    }

    const RENAME: Migration = Migration {
        from: 1,
        to: 2,
        apply: rename_title_to_name,
    };
    const MARK: Migration = Migration {
        from: 2,
        to: 3,
        apply: mark_migrated,
    };
    const REFUSES: Migration = Migration {
        from: 1,
        to: 2,
        apply: |_| Err("this file is not something I can convert".to_owned()),
    };

    #[test]
    fn an_absent_overlay_is_not_an_error() {
        let directory = TestDirectory::new("absent");
        let path = directory.0.join(OVERLAY_FILE_NAME);

        let (entries, status) =
            load(&path, SCHEMA_VERSION, schema::MIGRATIONS).expect("no file is fine");

        assert!(entries.is_empty());
        assert_eq!(status, OverlayStatus::Absent { path });
    }

    #[test]
    fn a_current_overlay_is_read_and_the_file_is_left_byte_for_byte_alone() {
        let directory = TestDirectory::new("current");
        let path = directory.with_overlay(CURRENT);

        let (entries, status) =
            load(&path, SCHEMA_VERSION, schema::MIGRATIONS).expect("a current overlay loads");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name(), "My Game");
        assert_eq!(
            status,
            OverlayStatus::Loaded {
                path: path.clone(),
                entries: 1
            }
        );
        // Comments included: a file that did not need rewriting is not
        // rewritten, so nothing the user wrote is reformatted away.
        assert_eq!(
            fs::read_to_string(&path).expect("the file is still there"),
            CURRENT
        );
    }

    #[test]
    fn an_older_overlay_is_migrated_backed_up_and_rewritten() {
        let directory = TestDirectory::new("migrated");
        let path = directory.with_overlay(OLDER);

        let (entries, status) = load(&path, 2, &[RENAME]).expect("the migration runs");

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].name(),
            "My Game",
            "the migrated entry should carry the converted field"
        );

        let backup = path.with_file_name("games.toml.v1.bak");
        assert_eq!(
            status,
            OverlayStatus::Migrated {
                path: path.clone(),
                from: 1,
                to: 2,
                backup: backup.clone(),
                entries: 1,
            }
        );

        let rewritten = fs::read_to_string(&path).expect("the file was rewritten");
        assert!(
            rewritten.contains("schema_version = 2"),
            "the rewritten file should carry the new version: {rewritten}"
        );
        assert!(
            rewritten.contains(r#"name = "My Game""#) && !rewritten.contains("title"),
            "the rewritten file should carry the converted field: {rewritten}"
        );
        assert_eq!(
            fs::read_to_string(&backup).expect("the backup was taken"),
            OLDER,
            "the backup must hold the original bytes, comments and all"
        );

        // And it is now a current file, so loading it again converts nothing.
        let (_, status) = load(&path, 2, &[RENAME]).expect("the rewritten file loads");
        assert!(matches!(status, OverlayStatus::Loaded { .. }));
    }

    #[test]
    fn a_migration_chain_runs_every_step_in_order() {
        let directory = TestDirectory::new("chain");
        let path = directory.with_overlay(OLDER);

        // Deliberately out of order in the list: the chain is followed by
        // matching each step's `from`, not by the order somebody wrote them.
        let (entries, status) = load(&path, 3, &[MARK, RENAME]).expect("both steps run");

        assert_eq!(entries[0].name(), "My Game (v3)");
        assert!(
            matches!(
                status,
                OverlayStatus::Migrated {
                    from: 1,
                    to: 3,
                    ref backup,
                    ..
                } if backup.ends_with("games.toml.v1.bak")
            ),
            "the backup should be named for the version found on disk: {status:?}"
        );
    }

    #[test]
    fn a_step_beyond_the_target_is_not_taken() {
        let directory = TestDirectory::new("beyond");
        let path = directory.with_overlay(OLDER);

        // Target 2 with both steps available: the 2-to-3 step must not run, or
        // the file would end up claiming a version this build does not read.
        let (entries, _) = load(&path, 2, &[RENAME, MARK]).expect("only the first step runs");
        assert_eq!(entries[0].name(), "My Game");
        assert!(fs::read_to_string(&path)
            .expect("the file is there")
            .contains("schema_version = 2"));
    }

    #[test]
    fn a_step_that_overshoots_the_target_is_not_taken() {
        let directory = TestDirectory::new("overshoot");
        let path = directory.with_overlay(OLDER);

        // The only route out of version 1 lands on version 3, and this build
        // reads 2. Taking it would leave the user with a file their own
        // Clipped then refuses, so there is no route and the file is untouched.
        let overshoots = Migration {
            from: 1,
            to: 3,
            apply: rename_title_to_name,
        };
        let error = load(&path, 2, &[overshoots]).expect_err("there is no route to version 2");

        assert!(
            matches!(
                error,
                CatalogueError::MigrationMissing { from: 1, to: 2, .. }
            ),
            "expected a missing migration, got {error:?}"
        );
        assert_eq!(fs::read_to_string(&path).expect("the file is there"), OLDER);
    }

    #[test]
    fn an_older_overlay_with_no_migration_is_left_exactly_as_it_is() {
        let directory = TestDirectory::new("no-migration");
        let path = directory.with_overlay(OLDER);

        let error = load(&path, 2, &[]).expect_err("there is no route to version 2");

        assert!(
            matches!(
                error,
                CatalogueError::MigrationMissing { from: 1, to: 2, .. }
            ),
            "expected a missing migration, got {error:?}"
        );
        assert_eq!(fs::read_to_string(&path).expect("the file is there"), OLDER);
        assert!(!path.with_file_name("games.toml.v1.bak").exists());
    }

    #[test]
    fn a_migration_that_refuses_leaves_the_file_exactly_as_it_is() {
        let directory = TestDirectory::new("refused");
        let path = directory.with_overlay(OLDER);

        let error = load(&path, 2, &[REFUSES]).expect_err("the step refuses");

        assert!(
            matches!(error, CatalogueError::MigrationFailed { .. }),
            "expected a failed migration, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("this file is not something I can convert"),
            "the step's own reason should reach the user: {error}"
        );
        assert_eq!(fs::read_to_string(&path).expect("the file is there"), OLDER);
        assert!(!path.with_file_name("games.toml.v1.bak").exists());
    }

    #[test]
    fn a_migration_whose_result_does_not_validate_leaves_the_file_alone() {
        let directory = TestDirectory::new("invalid-result");
        // Converting this one produces an entry with no executables, which is
        // refused — and the file must survive being refused.
        let broken = "schema_version = 1\n\n[[game]]\ngame_id = \"my-game\"\ntitle = \"My Game\"\n";
        let path = directory.with_overlay(broken);

        let error = load(&path, 2, &[RENAME]).expect_err("the converted entry is not valid");

        assert!(
            matches!(error, CatalogueError::InvalidEntry { .. }),
            "expected an invalid entry, got {error:?}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the file is there"),
            broken,
            "validation happens before anything is written"
        );
        assert!(!path.with_file_name("games.toml.v1.bak").exists());
    }

    #[test]
    fn a_newer_overlay_is_left_exactly_as_it_is() {
        let directory = TestDirectory::new("newer");
        let newer = CURRENT.replace("schema_version = 1", "schema_version = 99");
        let path = directory.with_overlay(&newer);

        let error =
            load(&path, SCHEMA_VERSION, schema::MIGRATIONS).expect_err("a newer file is refused");

        assert!(
            matches!(
                error,
                CatalogueError::SchemaTooNew {
                    found: 99,
                    supported: 1,
                    ..
                }
            ),
            "expected a too-new schema, got {error:?}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the file is there"),
            newer,
            "an older Clipped rewriting a newer file is how a user loses entries"
        );
    }

    #[test]
    fn a_migrated_overlay_that_cannot_be_backed_up_is_not_rewritten() {
        let directory = TestDirectory::new("backup-blocked");
        let path = directory.with_overlay(OLDER);
        // Occupy the backup name with a directory, which no copy can overwrite.
        fs::create_dir(path.with_file_name("games.toml.v1.bak"))
            .expect("the blocking directory can be created");

        let error = load(&path, 2, &[RENAME]).expect_err("the backup cannot be taken");

        assert!(
            matches!(error, CatalogueError::BackupFailed { .. }),
            "expected a failed backup, got {error:?}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the file is there"),
            OLDER,
            "no backup means no rewrite"
        );
    }

    #[test]
    fn an_overlay_that_is_not_a_catalogue_names_the_users_own_file() {
        let directory = TestDirectory::new("not-a-catalogue");
        let path = directory.with_overlay("this is not TOML at all\n");

        let error = load(&path, SCHEMA_VERSION, schema::MIGRATIONS)
            .expect_err("the file is not a catalogue");

        assert!(
            error.to_string().contains(&path.display().to_string()),
            "the message should name the user's file: {error}"
        );
    }

    #[test]
    fn the_default_overlay_lives_beside_clipped_s_other_per_user_files() {
        let Some(path) = default_path() else {
            // No per-user directory in this environment, which is a supported
            // state rather than a failure: the catalogue is then the seed data.
            return;
        };
        // Spelled out rather than taken from `clipped-logging`, which would
        // assert only that a constant equals itself. `Clipped` is the directory
        // the logs and the encoder's capability cache already use, and the
        // overlay belongs beside them.
        #[cfg(windows)]
        {
            assert!(
                path.ends_with(Path::new("Clipped").join("games.toml")),
                "{}",
                path.display()
            );
            assert!(
                path.starts_with(std::env::var_os("LOCALAPPDATA").expect("it named a directory")),
                "{}",
                path.display()
            );
        }
        #[cfg(not(windows))]
        assert!(
            path.ends_with(Path::new("clipped").join("games.toml")),
            "{}",
            path.display()
        );
    }
}
