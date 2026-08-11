//! Where Clipped keeps the files that belong to one user.
//!
//! Logs, the encoder's capability cache and the user's own game catalogue all
//! live in the same per-user directory, and three crates were resolving it with
//! three verbatim copies of the same function (issue #228). Three copies means
//! three places to change when the directory moves and three chances for one of
//! them to disagree about what happens when the environment does not describe
//! one — which is a supported state, not an error, in all three.
//!
//! It lives in `clipped-logging` because that is the lowest crate any of the
//! three can see: layer 0, no workspace dependencies of its own, and already a
//! dependency of the encoder. Only the directory is shared. Each caller keeps
//! its own file name, so nothing here knows that logs rotate or that the
//! catalogue is TOML, and no crate has to depend on the shape of another
//! crate's files to find its own.

use std::path::PathBuf;

/// The directory name Clipped uses under the platform's per-user root.
const APPLICATION_DIRECTORY: &str = "Clipped";

/// Clipped's per-user data directory.
///
/// On Windows this is `%LOCALAPPDATA%\Clipped`. Elsewhere — Clipped only
/// targets Windows, but every crate must still build and test on a
/// contributor's other machine — it follows the XDG state directory,
/// `$XDG_STATE_HOME/clipped` or `$HOME/.local/state/clipped`.
///
/// `None` when the environment describes no per-user directory at all, which on
/// Windows means `%LOCALAPPDATA%` is unset. That is not an error and callers do
/// not treat it as one: logging falls back to the console, the encoder detects
/// capabilities every run, and the game catalogue is the seed data alone.
#[must_use]
pub fn application_directory() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        windows_directory(std::env::var_os("LOCALAPPDATA").as_deref())
    }

    #[cfg(not(windows))]
    {
        xdg_directory(
            std::env::var_os("XDG_STATE_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }
}

/// The Windows answer, given what `%LOCALAPPDATA%` held.
///
/// Taking the value rather than reading it is what makes the unset and empty
/// cases testable: setting a process-wide environment variable from a test
/// would be visible to every other test running beside it.
#[cfg(windows)]
fn windows_directory(local_app_data: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    local_app_data
        .filter(|value| !value.is_empty())
        .map(|local_app_data| PathBuf::from(local_app_data).join(APPLICATION_DIRECTORY))
}

/// The XDG answer, given what `$XDG_STATE_HOME` and `$HOME` held.
///
/// Lower-cased, because that is the convention everything else under
/// `~/.local/state` follows.
#[cfg(not(windows))]
fn xdg_directory(
    state_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let state_home = state_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local").join("state"))
        })?;
    Some(state_home.join(APPLICATION_DIRECTORY.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_directory_is_clipped_s_own_under_the_per_user_root() {
        // Spelled out rather than built from `APPLICATION_DIRECTORY`, which
        // would assert only that a constant equals itself.
        #[cfg(windows)]
        assert_eq!(
            windows_directory(Some(std::ffi::OsStr::new(
                r"C:\Users\somebody\AppData\Local"
            ))),
            Some(PathBuf::from(r"C:\Users\somebody\AppData\Local\Clipped"))
        );

        #[cfg(not(windows))]
        assert_eq!(
            xdg_directory(
                Some(std::ffi::OsStr::new("/home/somebody/.local/state")),
                None
            ),
            Some(PathBuf::from("/home/somebody/.local/state/clipped"))
        );
    }

    #[test]
    fn an_environment_that_describes_no_per_user_directory_is_answered_with_none() {
        // The one place this case is decided, rather than the three it used to
        // be decided in (issue #228). An unset variable and an empty one mean
        // the same thing: nothing said where the directory is.
        #[cfg(windows)]
        {
            assert_eq!(windows_directory(None), None);
            assert_eq!(windows_directory(Some(std::ffi::OsStr::new(""))), None);
        }

        #[cfg(not(windows))]
        {
            assert_eq!(xdg_directory(None, None), None);
            assert_eq!(
                xdg_directory(
                    Some(std::ffi::OsStr::new("")),
                    Some(std::ffi::OsStr::new(""))
                ),
                None
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn the_home_fallback_is_used_only_when_the_state_directory_is_not_named() {
        assert_eq!(
            xdg_directory(None, Some(std::ffi::OsStr::new("/home/somebody"))),
            Some(PathBuf::from("/home/somebody/.local/state/clipped"))
        );
        assert_eq!(
            xdg_directory(
                Some(std::ffi::OsStr::new("/somewhere/state")),
                Some(std::ffi::OsStr::new("/home/somebody"))
            ),
            Some(PathBuf::from("/somewhere/state/clipped"))
        );
    }

    #[test]
    fn the_live_environment_answers_the_same_way() {
        // The wiring between the pure functions above and the process's own
        // environment, which the rest of these tests deliberately do not touch.
        let Some(directory) = application_directory() else {
            // A machine that describes no per-user directory, which is a
            // supported state rather than a failure.
            return;
        };
        assert!(
            directory.ends_with(Path::new("Clipped")) || directory.ends_with(Path::new("clipped")),
            "{}",
            directory.display()
        );
    }
}
