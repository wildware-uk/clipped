//! Finding the programs the harness inspects media with.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The environment variable that turns "this checkout has no FFmpeg" from a
/// skip into a failure.
///
/// The same lever `clipped-capture`, `clipped-encoder` and `clipped-audio`
/// already have, for the same reason: a test that decides for itself it could
/// not run reads as a pass, and the difference between "it ran and passed" and
/// "it did not run" is the whole point of validating media at all.
pub const REQUIRE_MEDIA: &str = "CLIPPED_REQUIRE_MEDIA";

/// Where the pinned FFmpeg build was installed, as of when this crate was
/// compiled.
///
/// `.cargo/config.toml` sets it for everything Cargo runs in this workspace and
/// resolves it against the checkout, so it names this repository's FFmpeg
/// rather than whichever one a machine happens to have. It is read at compile
/// time as well as at run time because a test binary executed directly — by a
/// debugger, or by an IDE — does not inherit Cargo's environment.
const PINNED_FFMPEG_DIR: Option<&str> = option_env!("FFMPEG_DIR");

/// The programs this harness runs.
///
/// Both come from the pinned FFmpeg build where there is one, and from `PATH`
/// otherwise, so that a contributor who has FFmpeg installed some other way can
/// still run the media tests.
#[derive(Debug, Clone)]
pub struct MediaTools {
    ffprobe: PathBuf,
    ffmpeg: PathBuf,
}

impl MediaTools {
    /// Looks for both programs.
    pub fn locate() -> Result<Self, ToolsUnavailable> {
        let ffprobe = tool("ffprobe");
        let ffmpeg = tool("ffmpeg");

        match (ffprobe, ffmpeg) {
            (Some(ffprobe), Some(ffmpeg)) => Ok(Self { ffprobe, ffmpeg }),
            (ffprobe, ffmpeg) => {
                let mut missing = Vec::new();
                if ffprobe.is_none() {
                    missing.push("ffprobe");
                }
                if ffmpeg.is_none() {
                    missing.push("ffmpeg");
                }
                Err(ToolsUnavailable {
                    missing: missing.join(" and "),
                })
            }
        }
    }

    /// The `ffprobe` every structural assertion is made against.
    pub fn ffprobe(&self) -> &Path {
        &self.ffprobe
    }

    /// The `ffmpeg` audio tracks are decoded with, for the tone assertions.
    pub fn ffmpeg(&self) -> &Path {
        &self.ffmpeg
    }
}

/// Neither program could be found, and which one is missing.
#[derive(Debug, Clone)]
pub struct ToolsUnavailable {
    missing: String,
}

impl fmt::Display for ToolsUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} could not be found, so nothing checked that the media is valid. \
             Looked in {} and then on PATH; run scripts/fetch-ffmpeg.ps1 to install the \
             pinned build",
            self.missing,
            PINNED_FFMPEG_DIR.map_or_else(
                || "no FFMPEG_DIR".to_owned(),
                |directory| format!("{directory}\\bin")
            ),
        )
    }
}

impl std::error::Error for ToolsUnavailable {}

/// The tools, or a clean skip when this machine has none.
///
/// Returns [`None`] after reporting the skip on standard error, so a test that
/// cannot validate anything says so instead of passing silently:
///
/// ```no_run
/// let Some(tools) = clipped_media_validation::require_media_tools() else {
///     return;
/// };
/// # let _ = tools;
/// ```
///
/// # Panics
///
/// When [`REQUIRE_MEDIA`] is set to a non-empty value, because a machine that
/// declared itself able to validate media must not skip.
pub fn require_media_tools() -> Option<MediaTools> {
    match MediaTools::locate() {
        Ok(tools) => Some(tools),
        Err(unavailable) => {
            assert!(
                !skipping_is_forbidden(std::env::var_os(REQUIRE_MEDIA)),
                "{REQUIRE_MEDIA} is set, so this must not be skipped: {unavailable}"
            );
            // Written through `std::io::stderr()` rather than with `eprintln!`
            // because libtest captures the macros, and a skip nobody can see is
            // how a suite quietly stops testing anything.
            let _ = writeln!(std::io::stderr(), "SKIPPED (media): {unavailable}");
            None
        }
    }
}

/// Whether [`REQUIRE_MEDIA`] has been set to a value that demands the tools be
/// there.
///
/// An empty value counts as unset, because that is how a workflow switches the
/// demand off without deleting the line.
fn skipping_is_forbidden(required: Option<std::ffi::OsString>) -> bool {
    required.is_some_and(|value| !value.is_empty())
}

/// The pinned build's copy of `name`, or the first one on `PATH`.
fn tool(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);

    let directories = PINNED_FFMPEG_DIR
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::var_os("FFMPEG_DIR").map(PathBuf::from))
        .map(|directory| directory.join("bin"));

    for directory in directories {
        let candidate = directory.join(&file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(&file))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The skip message is a deliverable of this crate — "skip cleanly with a
    /// clear message when the required tooling is unavailable" is a scope
    /// bullet of issue #24 — and it cannot be produced by running the harness
    /// on a machine that has the pinned build, which is every machine that runs
    /// these tests. So it is asserted directly instead of being read off a run
    /// nobody can reproduce.
    #[test]
    fn the_skip_message_names_what_is_missing_and_what_to_do_about_it() {
        let message = ToolsUnavailable {
            missing: "ffprobe and ffmpeg".to_owned(),
        }
        .to_string();

        assert!(
            message.starts_with(
                "ffprobe and ffmpeg could not be found, so nothing checked that \
                                 the media is valid."
            ),
            "the message does not open by naming what is missing and what went unchecked: \
             {message}"
        );
        assert!(
            message.contains("scripts/fetch-ffmpeg.ps1"),
            "the message does not say how to fix it: {message}"
        );
        assert!(
            message.contains("PATH"),
            "the message does not say where it looked: {message}"
        );
    }

    /// Where it looked is part of the message, and on a checkout with the
    /// pinned build that is a real directory rather than the "no FFMPEG_DIR"
    /// fallback.
    #[test]
    fn the_skip_message_says_which_ffmpeg_it_looked_for() {
        let message = ToolsUnavailable {
            missing: "ffprobe".to_owned(),
        }
        .to_string();

        let expected = PINNED_FFMPEG_DIR.map_or_else(
            || "no FFMPEG_DIR".to_owned(),
            |directory| format!("{directory}\\bin"),
        );
        assert!(
            message.contains(&format!("Looked in {expected} and then on PATH")),
            "the message does not name the directory it searched: {message}"
        );
    }

    /// The lever that turns a skip into a failure, driven through the same
    /// decision `require_media_tools` makes rather than read off the source.
    /// This is the check that stops a machine which declared itself able to
    /// validate media from quietly validating none.
    #[test]
    fn declaring_the_tools_required_forbids_a_skip() {
        assert!(
            !skipping_is_forbidden(None),
            "an unset variable leaves a machine without FFmpeg free to skip"
        );
        assert!(
            !skipping_is_forbidden(Some(std::ffi::OsString::from(""))),
            "an empty value is how a workflow switches the demand off, and must not force a run"
        );
        assert!(
            skipping_is_forbidden(Some(std::ffi::OsString::from("1"))),
            "a set variable must turn a skip into a failure"
        );
    }
}
