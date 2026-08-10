//! Deciding how verbose logging should be, without a rebuild.
//!
//! Four places can set the level, and they are tried in this order:
//!
//! 1. `CLIPPED_LOG`
//! 2. `RUST_LOG`
//! 3. the log level file (see [`default_level_file`](crate::default_level_file))
//! 4. the default supplied by the application's own configuration
//!
//! falling back to `info` when none of them says anything. Environment
//! variables win because they are how someone reproduces a problem in a single
//! run without editing anything, and `CLIPPED_LOG` exists so that turning
//! Clipped up does not also turn up an unrelated Rust program that happens to
//! read `RUST_LOG` in the same shell.
//!
//! Every value is an `EnvFilter` directive, so it can be a bare level (`debug`)
//! or a per-target list (`info,clipped_capture=trace`).

use std::fmt;

/// The level used when nothing else is configured.
pub(crate) const BUILT_IN_DEFAULT: &str = "info";

/// Where a filter directive came from, reported in the first log line of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterSource {
    /// The `CLIPPED_LOG` environment variable.
    ClippedLogEnvironment,
    /// The `RUST_LOG` environment variable.
    RustLogEnvironment,
    /// The log level file.
    LevelFile,
    /// A default supplied by the caller, normally from application settings.
    ApplicationDefault,
    /// This crate's built-in default.
    BuiltInDefault,
}

impl fmt::Display for FilterSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClippedLogEnvironment => "CLIPPED_LOG",
            Self::RustLogEnvironment => "RUST_LOG",
            Self::LevelFile => "log level file",
            Self::ApplicationDefault => "application configuration",
            Self::BuiltInDefault => "built-in default",
        })
    }
}

/// The directives to try, most specific first.
///
/// A value that is empty or only whitespace counts as unset: an exported but
/// empty `RUST_LOG` is a common shell accident and should not silence the file
/// and application settings behind it.
///
/// The built-in default is always the last candidate, so the list is never
/// empty and a malformed directive earlier in the list can be skipped without
/// leaving the process without logging.
pub(crate) fn candidate_directives(
    clipped_log: Option<&str>,
    rust_log: Option<&str>,
    level_file: Option<&str>,
    application_default: Option<&str>,
) -> Vec<(FilterSource, String)> {
    let mut candidates: Vec<(FilterSource, String)> = [
        (FilterSource::ClippedLogEnvironment, clipped_log),
        (FilterSource::RustLogEnvironment, rust_log),
        (FilterSource::LevelFile, level_file),
        (FilterSource::ApplicationDefault, application_default),
    ]
    .into_iter()
    .filter_map(|(source, value)| {
        let value = value?.trim();
        (!value.is_empty()).then(|| (source, value.to_owned()))
    })
    .collect();

    candidates.push((FilterSource::BuiltInDefault, BUILT_IN_DEFAULT.to_owned()));
    candidates
}

/// Extracts a directive from the contents of a log level file.
///
/// The format is one directive on a line of its own. Blank lines and lines
/// starting with `#` are ignored so the file can explain itself to whoever
/// opens it next.
pub(crate) fn directive_from_level_file(contents: &str) -> Option<String> {
    contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(
        clipped_log: Option<&str>,
        rust_log: Option<&str>,
        level_file: Option<&str>,
        application_default: Option<&str>,
    ) -> (FilterSource, String) {
        candidate_directives(clipped_log, rust_log, level_file, application_default)
            .into_iter()
            .next()
            .expect("the built-in default guarantees at least one candidate")
    }

    #[test]
    fn nothing_configured_falls_back_to_info() {
        assert_eq!(
            first(None, None, None, None),
            (FilterSource::BuiltInDefault, "info".to_owned())
        );
    }

    #[test]
    fn each_source_beats_the_ones_below_it() {
        assert_eq!(
            first(Some("trace"), Some("debug"), Some("warn"), Some("error")).0,
            FilterSource::ClippedLogEnvironment
        );
        assert_eq!(
            first(None, Some("debug"), Some("warn"), Some("error")).0,
            FilterSource::RustLogEnvironment
        );
        assert_eq!(
            first(None, None, Some("warn"), Some("error")).0,
            FilterSource::LevelFile
        );
        assert_eq!(
            first(None, None, None, Some("error")).0,
            FilterSource::ApplicationDefault
        );
    }

    #[test]
    fn blank_values_count_as_unset() {
        assert_eq!(
            first(Some(""), Some("   "), Some("\n"), Some("error")).0,
            FilterSource::ApplicationDefault
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            first(Some(" clipped_capture=trace "), None, None, None).1,
            "clipped_capture=trace"
        );
    }

    #[test]
    fn the_built_in_default_is_always_the_last_candidate() {
        // Half of the guarantee that a typo cannot leave a run with no logging:
        // there is always something behind the directive that failed. The other
        // half — that a directive `EnvFilter` rejects is actually skipped — is
        // in `init::tests::a_malformed_directive_is_skipped_for_the_next_source`,
        // because it is `EnvFilter` that decides, not this function.
        let candidates = candidate_directives(Some("nonsense=="), None, None, None);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1].0, FilterSource::BuiltInDefault);

        let last = candidate_directives(Some("trace"), Some("debug"), Some("warn"), Some("error"));
        assert_eq!(
            last.last(),
            Some(&(FilterSource::BuiltInDefault, BUILT_IN_DEFAULT.to_owned()))
        );
    }

    #[test]
    fn the_level_file_ignores_comments_and_blank_lines() {
        let contents = "# Clipped log level.\n\n  debug  \nignored\n";
        assert_eq!(
            directive_from_level_file(contents).as_deref(),
            Some("debug")
        );
    }

    #[test]
    fn an_empty_level_file_configures_nothing() {
        assert_eq!(directive_from_level_file("# only a comment\n\n"), None);
    }
}
