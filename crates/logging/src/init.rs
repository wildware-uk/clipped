//! Installing the process-wide subscriber and its file output.

use std::fmt;
use std::fs;
use std::io::{self, IsTerminal};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt as fmt_layer, EnvFilter};

use crate::error::LoggingError;
use crate::filter::{candidate_directives, directive_from_level_file, FilterSource};

/// The directory Clipped keeps per-user data in, relative to the platform's
/// local application data directory.
const APPLICATION_DIRECTORY: &str = "Clipped";

/// The log directory, relative to [`APPLICATION_DIRECTORY`].
const LOG_DIRECTORY: &str = "logs";

/// The level configuration file, relative to [`APPLICATION_DIRECTORY`].
const LEVEL_FILE: &str = "log-level.txt";

/// How many rotated log files are kept by default.
///
/// Files rotate hourly, so this is two days of history. Rotation is hourly
/// rather than daily specifically because the recorder is expected to stay
/// running for days (AGENTS.md section 59): with daily rotation a single file
/// would keep growing for the whole of that run.
pub const DEFAULT_RETAINED_FILES: NonZeroUsize = NonZeroUsize::new(48).unwrap();

/// Where the level configuration file is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LevelFile {
    /// [`default_level_file`].
    Default,
    /// An explicit path, used by tests and by portable installations.
    At(PathBuf),
    /// Do not consult a file at all.
    Disabled,
}

/// How diagnostics should be set up.
///
/// The defaults are what the recorder uses: log to the per-user log directory
/// and to the console, keep two days of hourly files, and take the level from
/// the environment or the level file.
#[derive(Debug, Clone)]
pub struct LogSettings {
    directory: Option<PathBuf>,
    level_file: LevelFile,
    default_level: Option<String>,
    console: bool,
    retained_files: NonZeroUsize,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            directory: None,
            level_file: LevelFile::Default,
            default_level: None,
            console: true,
            retained_files: DEFAULT_RETAINED_FILES,
        }
    }
}

impl LogSettings {
    /// Writes log files somewhere other than [`default_log_directory`].
    #[must_use]
    pub fn with_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.directory = Some(directory.into());
        self
    }

    /// Reads the level from a specific file rather than [`default_level_file`].
    #[must_use]
    pub fn with_level_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.level_file = LevelFile::At(path.into());
        self
    }

    /// Ignores the level file entirely.
    ///
    /// Useful for tests, which must not depend on whatever the developer left
    /// in their own configuration.
    #[must_use]
    pub fn without_level_file(mut self) -> Self {
        self.level_file = LevelFile::Disabled;
        self
    }

    /// Supplies the level to use when neither the environment nor the level
    /// file sets one — normally the value held in the application's settings.
    #[must_use]
    pub fn with_default_level(mut self, level: impl Into<String>) -> Self {
        self.default_level = Some(level.into());
        self
    }

    /// Turns the console output on or off. On by default.
    #[must_use]
    pub fn with_console(mut self, console: bool) -> Self {
        self.console = console;
        self
    }

    /// Changes how many rotated files are kept.
    ///
    /// See [`DEFAULT_RETAINED_FILES`]. Older files are deleted as new ones are
    /// created, which is what bounds the space logs can occupy.
    #[must_use]
    pub fn with_retained_files(mut self, retained_files: NonZeroUsize) -> Self {
        self.retained_files = retained_files;
        self
    }
}

/// Keeps the background log writer alive.
///
/// Hold it for as long as the process should be able to log — normally by
/// binding it in `main`. Dropping it flushes anything the writer still holds
/// and stops the writer thread, after which further events are discarded.
pub struct LoggingGuard {
    directory: PathBuf,
    _worker: WorkerGuard,
}

impl LoggingGuard {
    /// The directory log files are being written to.
    ///
    /// The desktop application uses this for "open log folder"; nothing logs
    /// it, because it contains the account name.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl fmt::Debug for LoggingGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // WorkerGuard has no useful representation and the directory contains
        // the account name, so neither is printed.
        f.write_str("LoggingGuard")
    }
}

/// Clipped's per-user data directory.
///
/// On Windows this is `%LOCALAPPDATA%\Clipped`. Elsewhere — Clipped only
/// targets Windows, but the crate must still build and test on a contributor's
/// other machine — it follows the XDG state directory, `$XDG_STATE_HOME/clipped`
/// or `$HOME/.local/state/clipped`.
fn application_directory() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(|local_app_data| PathBuf::from(local_app_data).join(APPLICATION_DIRECTORY))
    }

    #[cfg(not(windows))]
    {
        let state_home = std::env::var_os("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })?;
        Some(state_home.join(APPLICATION_DIRECTORY.to_lowercase()))
    }
}

/// Where log files are written unless [`LogSettings::with_directory`] says
/// otherwise: `%LOCALAPPDATA%\Clipped\logs` on Windows.
///
/// `None` when the environment does not describe a per-user directory, which
/// on Windows means `%LOCALAPPDATA%` is unset.
#[must_use]
pub fn default_log_directory() -> Option<PathBuf> {
    Some(application_directory()?.join(LOG_DIRECTORY))
}

/// Where the level configuration file is read from:
/// `%LOCALAPPDATA%\Clipped\log-level.txt` on Windows.
///
/// The file holds one `EnvFilter` directive on a line of its own; `#` starts a
/// comment. It does not have to exist.
#[must_use]
pub fn default_level_file() -> Option<PathBuf> {
    Some(application_directory()?.join(LEVEL_FILE))
}

/// A problem worth reporting that was found before a subscriber existed.
#[derive(Debug)]
enum StartupWarning {
    /// A configured filter directive did not parse and was skipped.
    ///
    /// Only the source is kept. The directive itself is an unvalidated external
    /// string, and this crate's rule everywhere else — see
    /// [`LoggingError::InvalidIdentifier`] — is that a rejected value is
    /// described, never echoed. Naming the source is enough to find it: there
    /// are four, and each of them is something the person reading the warning
    /// set themselves.
    UnparsableDirective { source: FilterSource },
    /// The level file exists but could not be read.
    UnreadableLevelFile { error: io::Error },
}

impl StartupWarning {
    fn emit(&self) {
        match self {
            Self::UnparsableDirective { source } => tracing::warn!(
                filter_source = %source,
                "ignoring a log filter directive that does not parse"
            ),
            Self::UnreadableLevelFile { error } => tracing::warn!(
                error = %error,
                "ignoring the log level file, which could not be read"
            ),
        }
    }
}

/// Reads the level directive out of the configured level file.
///
/// A missing file configures nothing. A file that exists but cannot be read
/// produces a warning rather than an error: an unreadable preference is not a
/// reason to refuse to record (AGENTS.md section 16).
fn read_level_file(level_file: &LevelFile, warnings: &mut Vec<StartupWarning>) -> Option<String> {
    let path = match level_file {
        LevelFile::Disabled => return None,
        LevelFile::At(path) => path.clone(),
        LevelFile::Default => default_level_file()?,
    };

    match fs::read_to_string(&path) {
        Ok(contents) => directive_from_level_file(&contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            warnings.push(StartupWarning::UnreadableLevelFile { error });
            None
        }
    }
}

/// The environment variables that can set the level.
///
/// Passed into [`resolve_filter`] rather than read inside it, so that the
/// precedence order and the skip-on-parse-failure path can be tested without
/// mutating the environment of a process that is running other tests in
/// parallel (AGENTS.md section 25).
#[derive(Debug, Default)]
struct EnvironmentLevels {
    clipped_log: Option<String>,
    rust_log: Option<String>,
}

impl EnvironmentLevels {
    /// Reads both variables from the process environment.
    fn from_process() -> Self {
        Self {
            clipped_log: std::env::var("CLIPPED_LOG").ok(),
            rust_log: std::env::var("RUST_LOG").ok(),
        }
    }
}

/// Picks the first directive that `EnvFilter` accepts.
///
/// A typo in `CLIPPED_LOG` therefore falls through to the next source instead
/// of taking the process's logging with it. The built-in default is always the
/// last candidate and always parses.
fn resolve_filter(
    settings: &LogSettings,
    environment: &EnvironmentLevels,
    warnings: &mut Vec<StartupWarning>,
) -> (EnvFilter, FilterSource, String) {
    let level_file_directive = read_level_file(&settings.level_file, warnings);

    let candidates = candidate_directives(
        environment.clipped_log.as_deref(),
        environment.rust_log.as_deref(),
        level_file_directive.as_deref(),
        settings.default_level.as_deref(),
    );

    for (source, directive) in candidates {
        match EnvFilter::try_new(&directive) {
            Ok(filter) => return (filter, source, directive),
            Err(_) => warnings.push(StartupWarning::UnparsableDirective { source }),
        }
    }

    unreachable!("candidate_directives always ends with a directive that parses")
}

/// Installs the process-wide subscriber.
///
/// Call this once, as early in `main` as possible, and keep the returned
/// [`LoggingGuard`] alive for the life of the process. Events emitted before
/// this runs are discarded, and events emitted after the guard is dropped are
/// discarded too.
///
/// # Errors
///
/// Returns [`LoggingError::NoLogDirectory`] or
/// [`LoggingError::CreateLogDirectory`] if file output cannot be set up,
/// [`LoggingError::OpenLogFile`] if the appender cannot open the directory, and
/// [`LoggingError::AlreadyInitialised`] if a subscriber is already installed.
pub fn init(settings: &LogSettings) -> Result<LoggingGuard, LoggingError> {
    let directory = match &settings.directory {
        Some(directory) => directory.clone(),
        None => default_log_directory().ok_or(LoggingError::NoLogDirectory)?,
    };
    fs::create_dir_all(&directory).map_err(|source| LoggingError::CreateLogDirectory {
        directory: directory.clone(),
        source,
    })?;

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::HOURLY)
        .filename_prefix("clipped")
        .filename_suffix("log")
        .max_log_files(settings.retained_files.get())
        .build(&directory)
        .map_err(|source| LoggingError::OpenLogFile {
            directory: directory.clone(),
            source: Box::new(source),
        })?;

    // The appender writes on a background thread. A capture thread must never
    // block on disk (AGENTS.md section 20), and the writer's queue is what
    // guarantees that.
    let (file_writer, worker) = tracing_appender::non_blocking(appender);

    let mut warnings = Vec::new();
    let environment = EnvironmentLevels::from_process();
    let (filter, filter_source, directive) = resolve_filter(settings, &environment, &mut warnings);

    let file_output = fmt_layer::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_thread_ids(true);

    let console_output = settings.console.then(|| {
        fmt_layer::layer()
            // stderr, so that anything the recorder writes to stdout stays
            // machine-readable.
            .with_writer(io::stderr)
            .with_ansi(io::stderr().is_terminal())
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(file_output)
        .with(console_output)
        .try_init()
        .map_err(|_| LoggingError::AlreadyInitialised)?;

    for warning in &warnings {
        warning.emit();
    }
    tracing::info!(
        filter = %directive,
        filter_source = %filter_source,
        retained_files = settings.retained_files.get(),
        rotation = "hourly",
        "diagnostics initialised"
    );

    Ok(LoggingGuard {
        directory,
        _worker: worker,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    /// A directive `EnvFilter` refuses, which is what a typo looks like.
    const MALFORMED: &str = "nonsense==";

    /// Settings that consult nothing outside this test.
    fn test_settings() -> LogSettings {
        LogSettings::default().without_level_file()
    }

    fn environment(clipped_log: Option<&str>, rust_log: Option<&str>) -> EnvironmentLevels {
        EnvironmentLevels {
            clipped_log: clipped_log.map(str::to_owned),
            rust_log: rust_log.map(str::to_owned),
        }
    }

    /// Collects what a subscriber writes, so a test can assert on it.
    #[derive(Clone, Default)]
    struct CapturedLog(Arc<Mutex<Vec<u8>>>);

    impl CapturedLog {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().expect("the buffer is not poisoned").clone())
                .expect("the subscriber writes UTF-8")
        }
    }

    impl io::Write for CapturedLog {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("the buffer is not poisoned")
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for CapturedLog {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    /// Runs `body` against a subscriber local to this thread and returns what it
    /// wrote, so the assertion is on rendered output rather than on intent.
    fn captured(body: impl FnOnce()) -> String {
        let captured = CapturedLog::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();

        tracing::subscriber::with_default(subscriber, body);
        captured.contents()
    }

    #[test]
    fn the_log_directory_sits_under_the_application_directory() {
        let Some(application_directory) = application_directory() else {
            // The environment does not describe a per-user directory; the
            // `None` path is covered by `init` returning `NoLogDirectory`.
            return;
        };

        assert_eq!(
            default_log_directory(),
            Some(application_directory.join(LOG_DIRECTORY))
        );
        assert_eq!(
            default_level_file(),
            Some(application_directory.join(LEVEL_FILE))
        );
    }

    #[test]
    fn a_missing_level_file_configures_nothing() {
        let mut warnings = Vec::new();
        let missing = std::env::temp_dir().join("clipped-logging-no-such-level-file.txt");

        assert_eq!(
            read_level_file(&LevelFile::At(missing), &mut warnings),
            None
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_disabled_level_file_is_not_read() {
        let mut warnings = Vec::new();
        assert_eq!(read_level_file(&LevelFile::Disabled, &mut warnings), None);
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_directive_that_parses_is_the_one_that_is_used() {
        let mut warnings = Vec::new();
        let (_filter, source, directive) = resolve_filter(
            &test_settings().with_default_level("warn"),
            &environment(Some("info,clipped_capture=trace"), None),
            &mut warnings,
        );

        assert_eq!(source, FilterSource::ClippedLogEnvironment);
        assert_eq!(directive, "info,clipped_capture=trace");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_malformed_directive_is_skipped_for_the_next_source() {
        // The claim being tested is the one in docs/logging.md: a typo in
        // CLIPPED_LOG cannot leave a run with no logging. It is `EnvFilter`
        // that decides what a typo is, so the test has to go through it.
        let mut warnings = Vec::new();
        let (_filter, source, directive) = resolve_filter(
            &test_settings().with_default_level("warn"),
            &environment(Some(MALFORMED), None),
            &mut warnings,
        );

        assert_eq!(source, FilterSource::ApplicationDefault);
        assert_eq!(directive, "warn");
        assert!(
            matches!(
                warnings.as_slice(),
                [StartupWarning::UnparsableDirective {
                    source: FilterSource::ClippedLogEnvironment
                }]
            ),
            "{warnings:?}"
        );
    }

    #[test]
    fn every_source_being_malformed_still_leaves_the_built_in_default() {
        let mut warnings = Vec::new();
        let (_filter, source, directive) = resolve_filter(
            &test_settings().with_default_level(MALFORMED),
            &environment(Some(MALFORMED), Some(MALFORMED)),
            &mut warnings,
        );

        assert_eq!(source, FilterSource::BuiltInDefault);
        assert_eq!(directive, crate::filter::BUILT_IN_DEFAULT);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn a_skipped_directive_is_reported_by_source_and_nothing_else() {
        // The rejected directive is the one unvalidated external string this
        // crate holds at start-up, and the rule elsewhere in the crate is that
        // a rejected value is described rather than echoed. The warning does not
        // carry the directive at all, and this asserts the rendered line carries
        // no field beyond the source, so putting it back would fail here.
        const MESSAGE: &str = "ignoring a log filter directive that does not parse";

        let output = captured(|| {
            StartupWarning::UnparsableDirective {
                source: FilterSource::ClippedLogEnvironment,
            }
            .emit();
        });

        let fields = output
            .split_once(MESSAGE)
            .unwrap_or_else(|| panic!("the warning should report {MESSAGE}:\n{output}"))
            .1;
        assert_eq!(fields.trim(), "filter_source=CLIPPED_LOG", "{output}");
    }

    #[test]
    fn an_unreadable_level_file_is_reported_without_its_path() {
        // The path contains the account name, so the warning describes the
        // failure and leaves the location to `LoggingGuard::directory`.
        let output = captured(|| {
            StartupWarning::UnreadableLevelFile {
                error: io::Error::new(io::ErrorKind::PermissionDenied, "access is denied"),
            }
            .emit();
        });

        assert!(
            output.contains("ignoring the log level file, which could not be read"),
            "{output}"
        );
        assert!(output.contains("access is denied"), "{output}");
    }
}
