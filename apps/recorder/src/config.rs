//! Turning parsed arguments into a validated recording configuration.
//!
//! [`RecordingConfig`] is the value the capture pipeline will be handed once it
//! exists. Everything that can be checked without a capture engine is checked
//! here, before any of it is used: the target selector, the output file and its
//! directory, and the video and audio settings.
//!
//! The split is deliberate. [`crate::cli`] describes the command line and knows
//! nothing about the filesystem; this module knows about the filesystem and
//! nothing about clap. That keeps the validation testable without spawning a
//! process, which is most of why the rules below have tests at all.

use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use time::macros::format_description;
use time::OffsetDateTime;

use crate::cli::RecordArgs;
use crate::options::{
    AudioDeviceSelection, EncoderSelection, Framerate, ReplayLength, ReplayWindow, Resolution,
    VideoCodec,
};

/// The container extension the recorder writes.
///
/// MKV is the archival format and the only one the muxer will produce; MP4 is
/// reached by remuxing afterwards, without re-encoding. See
/// [ADR 0001](../../../docs/adr/0001-mkv-archival-container.md).
pub const OUTPUT_EXTENSION: &str = "mkv";

/// The directory, under the user's videos folder, that recordings default to.
pub const DEFAULT_OUTPUT_SUBDIRECTORY: &str = "Clipped";

/// Which window or process to record.
///
/// Resolving one of these to an actual window handle is issue #10; this type is
/// only the user's expressed intent, checked for the mistakes that can be seen
/// without enumerating anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    /// A window whose title contains this text.
    WindowTitle(String),
    /// A process whose executable is named this, such as `cs2.exe`.
    ProcessName(String),
    /// A process with this identifier.
    ProcessId(u32),
}

impl fmt::Display for CaptureTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowTitle(title) => write!(formatter, "window title containing `{title}`"),
            Self::ProcessName(name) => write!(formatter, "process `{name}`"),
            Self::ProcessId(pid) => write!(formatter, "process {pid}"),
        }
    }
}

/// A `record` invocation, validated.
///
/// Holding one of these means the arguments were coherent: exactly one target
/// was named, the output file can be created where it was asked for, and every
/// video and audio setting is inside the range the encoder accepts. It does not
/// mean the machine can honour them — whether this GPU can encode AV1 at this
/// size is answered by capability detection (issue #14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingConfig {
    /// What to capture.
    pub target: CaptureTarget,
    /// Where the recording will be written. Always absolute.
    pub output: PathBuf,
    /// Whether an existing file at [`Self::output`] may be replaced.
    pub overwrite: bool,
    /// The size to encode at.
    pub resolution: Resolution,
    /// The framerate to encode at.
    pub framerate: Framerate,
    /// The codec to encode with.
    pub codec: VideoCodec,
    /// The encoder to encode with.
    pub encoder: EncoderSelection,
    /// Where the microphone track comes from.
    pub microphone: AudioDeviceSelection,
    /// Where the system audio track comes from.
    pub system_audio: AudioDeviceSelection,
}

/// Why a `record` invocation was rejected.
///
/// Every variant carries what is needed to say which value was wrong and what
/// to do instead, because a user cannot act on "invalid configuration"
/// (AGENTS.md section 15).
///
/// # Two audiences, one set of rules
///
/// These messages are read in two places. On the command line they are printed
/// beside a pointer to `--help`; over IPC they become the `invalid_parameters`
/// refusal the desktop application renders (`docs/ipc.md`). One set of
/// validation rules serving both is deliberate (AGENTS.md section 55) — a second
/// copy reachable only over IPC would be a second set of answers to the same
/// question — so the messages name the *setting* that was wrong, under the name
/// it has in both interfaces, rather than the command-line spelling of it.
/// Telling somebody in a window to pass `--pid <PID>` is not an action they can
/// take (AGENTS.md section 45), and the flag spelling is what `--help` is for.
#[derive(Debug)]
pub enum ConfigError {
    /// No target was given.
    NoTarget,
    /// More than one target was given.
    ConflictingTargets,
    /// The `window` title match was empty.
    EmptyWindowTitle,
    /// The `process` name was empty.
    EmptyProcessName,
    /// The `process` value looked like a path rather than an executable name.
    ProcessNameIsAPath {
        /// What was supplied.
        value: String,
    },
    /// `pid` 0 is not a process; on Windows it is the idle process.
    ZeroProcessId,
    /// The output path had no file name.
    OutputHasNoFileName {
        /// What was supplied.
        path: PathBuf,
    },
    /// The output path did not end in [`OUTPUT_EXTENSION`].
    OutputIsNotMatroska {
        /// What was supplied.
        path: PathBuf,
    },
    /// The directory the output path names does not exist.
    OutputDirectoryMissing {
        /// The directory that is missing.
        directory: PathBuf,
    },
    /// The path the output path's directory names is not a directory.
    OutputDirectoryIsNotADirectory {
        /// The path that is not a directory.
        directory: PathBuf,
    },
    /// The directory could not be written to.
    OutputDirectoryNotWritable {
        /// The directory that was probed.
        directory: PathBuf,
        /// Why the probe failed.
        source: io::Error,
    },
    /// The output file already exists and `overwrite` was not set.
    OutputExists {
        /// The file that is in the way.
        path: PathBuf,
    },
    /// The output path is an existing directory.
    OutputIsADirectory {
        /// The directory that is in the way.
        path: PathBuf,
    },
    /// No default output directory could be worked out, and no output path was
    /// given.
    NoDefaultOutputDirectory,
    /// A relative output path could not be made absolute.
    WorkingDirectoryUnavailable {
        /// Why the current directory could not be read.
        source: io::Error,
    },
    /// A replay save was asked to keep more than the buffer holds.
    SaveLongerThanBuffer {
        /// The save duration that was asked for, in seconds.
        save: u32,
        /// The buffer's own duration, in seconds.
        window: u32,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTarget => formatter
                .write_str("no capture target: choose exactly one of `window`, `process` or `pid`"),
            Self::ConflictingTargets => formatter.write_str(
                "more than one capture target: choose exactly one of `window`, `process` \
                 or `pid`",
            ),
            Self::EmptyWindowTitle => formatter.write_str(
                "`window` needs some text to match a window title against; \
                 an empty value would match every window",
            ),
            Self::EmptyProcessName => {
                formatter.write_str("`process` needs an executable name, such as cs2.exe")
            }
            Self::ProcessNameIsAPath { value } => write!(
                formatter,
                "`process` takes an executable name such as cs2.exe, not a path; \
                 `{value}` contains a path separator"
            ),
            Self::ZeroProcessId => formatter.write_str(
                "`pid` 0 is not a process to record: on Windows 0 is the system idle process",
            ),
            Self::OutputHasNoFileName { path } => write!(
                formatter,
                "the output path {} does not name a file",
                path.display()
            ),
            Self::OutputIsNotMatroska { path } => write!(
                formatter,
                "the output path {} must end in .{OUTPUT_EXTENSION}: the recorder writes \
                 Matroska, which survives an interrupted recording (ADR 0001). Converting to \
                 MP4 is a remux afterwards, not a recording setting",
                path.display()
            ),
            Self::OutputDirectoryMissing { directory } => write!(
                formatter,
                "the directory {} does not exist; create it, or choose an output path \
                 inside a directory that does",
                directory.display()
            ),
            Self::OutputDirectoryIsNotADirectory { directory } => write!(
                formatter,
                "{} is not a directory, so nothing can be written inside it",
                directory.display()
            ),
            Self::OutputDirectoryNotWritable { directory, source } => write!(
                formatter,
                "the directory {} cannot be written to: {source}",
                directory.display()
            ),
            Self::OutputExists { path } => write!(
                formatter,
                "{} already exists. Recordings are not replaced silently; \
                 choose another output path, or set `overwrite` if you meant to replace it",
                path.display()
            ),
            Self::OutputIsADirectory { path } => write!(
                formatter,
                "the output path {} is an existing directory, not a file",
                path.display()
            ),
            Self::NoDefaultOutputDirectory => formatter.write_str(
                "there is no home directory to put recordings in, so an output path is required",
            ),
            Self::WorkingDirectoryUnavailable { source } => write!(
                formatter,
                "the output path is relative and the current directory could not be read: {source}"
            ),
            Self::SaveLongerThanBuffer { save, window } => write!(
                formatter,
                "a save of {save} seconds cannot be taken from a replay buffer that keeps \
                 {window}: ask for at most {window} seconds, or give the buffer a longer \
                 duration"
            ),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::OutputDirectoryNotWritable { source, .. }
            | Self::WorkingDirectoryUnavailable { source } => Some(source),
            _ => None,
        }
    }
}

impl RecordingConfig {
    /// Validates parsed arguments into a configuration.
    ///
    /// This reads the filesystem: it checks that the output directory exists
    /// and can be written to, by creating and removing a probe file inside it.
    /// It creates nothing that outlives the call — not the output file, and
    /// not the default recordings directory, which is created with the first
    /// recording rather than by validating a command that may never make one.
    ///
    /// `configured` is where recordings go when `--output` does not say: the
    /// user's configured recording directory, where they have set one
    /// (`clipped_session::config`'s `storage` section, issue #307). It is
    /// passed in rather than read here, so that validating a command never
    /// depends on the settings file of whoever is running the tests (AGENTS.md
    /// section 25) — and so that `record`, which takes everything from its
    /// command line, can pass nothing.
    pub fn resolve(args: &RecordArgs, configured: Option<&Path>) -> Result<Self, ConfigError> {
        let target = resolve_target(args)?;
        let output = resolve_output(
            args.output.as_deref(),
            configured,
            OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc()),
        )?;
        check_output_is_free(&output, args.overwrite)?;

        Ok(Self {
            target,
            output,
            overwrite: args.overwrite,
            resolution: args.resolution,
            framerate: args.framerate,
            codec: args.codec,
            encoder: args.encoder,
            microphone: args.microphone.clone(),
            system_audio: args.system_audio.clone(),
        })
    }
}

/// A validated `replay` invocation: a recording, and the buffer beside it.
///
/// The recording half is [`RecordingConfig`] and is resolved by exactly the same
/// rules, so `replay --window X` points at the window `record --window X` would
/// and writes the same kind of file to the same place. What is added is the two
/// numbers the buffer needs, and the one rule that relates them.
#[derive(Debug)]
pub struct ReplayConfig {
    /// The recording the buffer is filled from.
    pub recording: RecordingConfig,
    /// How much history the buffer keeps.
    pub window: std::time::Duration,
    /// How much of it one save keeps.
    pub save: std::time::Duration,
}

impl ReplayConfig {
    /// Validates parsed arguments into a configuration.
    ///
    /// Reads the filesystem exactly as [`RecordingConfig::resolve`] does, and
    /// adds the one check neither value's own parser can make: a save cannot be
    /// longer than the buffer it is taken from. That is refused here rather than
    /// silently shortened, because "I asked for the last five minutes and got
    /// thirty seconds" is the kind of quiet disappointment AGENTS.md section 54
    /// is about — and unlike a buffer that has not filled yet, this one is
    /// knowable before anything is recorded.
    ///
    /// `configured` is the window the settings file resolved to, which is what
    /// `--duration` overrides for one run. It is read from the same fold every
    /// other setting comes through, so "how much does Clipped keep" has one
    /// answer whether it is asked here or in the settings screen (AGENTS.md
    /// section 30, `docs/configuration.md`).
    ///
    /// # Errors
    ///
    /// [`RecordingConfig::resolve`]'s, and
    /// [`ConfigError::SaveLongerThanBuffer`].
    pub fn resolve(
        args: &crate::cli::ReplayArgs,
        configured: std::time::Duration,
        configured_directory: Option<&Path>,
    ) -> Result<Self, ConfigError> {
        let recording = RecordingConfig::resolve(&args.record, configured_directory)?;
        let window = args.duration.map_or(configured, ReplayWindow::duration);
        // Nothing said means the whole window: SPEC.md section 42's example is
        // `replay --duration 60` and a hotkey that saves the previous sixty
        // seconds, so the two are one number until somebody says otherwise.
        let save = args.save_duration.map_or(window, ReplayLength::duration);

        if save > window {
            return Err(ConfigError::SaveLongerThanBuffer {
                save: seconds_of(save),
                window: seconds_of(window),
            });
        }

        Ok(Self {
            recording,
            window,
            save,
        })
    }
}

/// A whole number of seconds, for a message somebody reads.
///
/// Saturating rather than wrapping: both durations are already inside a replay
/// buffer's bounds, so this is unreachable, and a silent wrap would put an
/// absurd number in a refusal.
fn seconds_of(duration: std::time::Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}

/// Picks the single target out of the three mutually exclusive selectors.
///
/// clap rejects two of them at parse time, but this function is also reachable
/// from tests and from any future caller that builds [`RecordArgs`] itself, so
/// it enforces the rule rather than assuming it.
fn resolve_target(args: &RecordArgs) -> Result<CaptureTarget, ConfigError> {
    target_from(args.window.as_deref(), args.process.as_deref(), args.pid)
}

/// The same rule, from the three selectors alone.
///
/// Separated from [`resolve_target`] because a screenshot names a target and
/// nothing else (`crate::serve`): it has no output path, no codec and no audio,
/// so building a whole [`RecordArgs`] to ask this one question would be
/// inventing values for fields nobody set. One rule, in one place (AGENTS.md
/// section 55).
///
/// # Errors
///
/// [`ConfigError::NoTarget`] or [`ConfigError::ConflictingTargets`] when the
/// count is not one, and the empty, path-like and zero cases below.
pub(crate) fn target_from(
    window: Option<&str>,
    process: Option<&str>,
    pid: Option<u32>,
) -> Result<CaptureTarget, ConfigError> {
    let selected =
        usize::from(window.is_some()) + usize::from(process.is_some()) + usize::from(pid.is_some());
    match selected {
        0 => return Err(ConfigError::NoTarget),
        1 => {}
        _ => return Err(ConfigError::ConflictingTargets),
    }

    if let Some(title) = window {
        let title = title.trim();
        if title.is_empty() {
            return Err(ConfigError::EmptyWindowTitle);
        }
        return Ok(CaptureTarget::WindowTitle(title.to_owned()));
    }
    if let Some(name) = process {
        let name = name.trim();
        if name.is_empty() {
            return Err(ConfigError::EmptyProcessName);
        }
        if name.contains(['/', '\\']) {
            return Err(ConfigError::ProcessNameIsAPath {
                value: name.to_owned(),
            });
        }
        return Ok(CaptureTarget::ProcessName(name.to_owned()));
    }

    let pid = pid.expect("one selector was counted and two were absent");
    if pid == 0 {
        return Err(ConfigError::ZeroProcessId);
    }
    Ok(CaptureTarget::ProcessId(pid))
}

/// Works out the absolute output path and checks the directory it goes in.
///
/// `now` is a parameter so that the generated default name is testable without
/// waiting for the clock to move.
fn resolve_output(
    requested: Option<&Path>,
    configured: Option<&Path>,
    now: OffsetDateTime,
) -> Result<PathBuf, ConfigError> {
    match requested {
        Some(path) => {
            let path = absolute(path)?;
            check_output_file_name(&path)?;
            let directory = path
                .parent()
                .ok_or_else(|| ConfigError::OutputHasNoFileName { path: path.clone() })?;
            check_directory_usable(directory)?;
            Ok(path)
        }
        None => {
            // Three layers, and the flag is the top one: `--output` beats the
            // settings file, which beats the videos folder this build would
            // pick on its own. That is the order `docs/configuration.md` gives
            // for every other setting, and step 3 of SPEC.md section 45 is the
            // reason the middle one exists at all — a directory chosen once in
            // the settings screen has to survive the window being closed
            // (issue #307).
            //
            // A configured directory that has gone — an unplugged drive — is
            // reported against this recording by the check below, rather than
            // being quietly replaced by the videos folder: a recording somebody
            // cannot find is worse than one that did not start (AGENTS.md
            // section 45).
            let directory = match configured {
                Some(chosen) => chosen.to_path_buf(),
                None => default_output_directory().ok_or(ConfigError::NoDefaultOutputDirectory)?,
            };
            // The recordings directory is Clipped's own, so it is created
            // rather than reported as missing — but not here. Validating a
            // command must leave nothing behind, and a run that ends without
            // recording anything (every run, until the capture pipeline
            // exists) would otherwise leave an empty directory in the user's
            // videos folder. Creation belongs with the first write, in the
            // muxer session that opens the file.
            //
            // A directory the user named themselves is a different case and is
            // never created: a path that is not there is far more often a typo
            // than an instruction to build a tree.
            if directory.exists() {
                check_directory_usable(&directory)?;
            }
            Ok(directory.join(default_output_file_name(now)))
        }
    }
}

/// Makes `path` absolute against the current directory.
fn absolute(path: &Path) -> Result<PathBuf, ConfigError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let current = std::env::current_dir()
        .map_err(|source| ConfigError::WorkingDirectoryUnavailable { source })?;
    Ok(current.join(path))
}

/// Checks that the path names a file, with the container extension.
fn check_output_file_name(path: &Path) -> Result<(), ConfigError> {
    // `file_name` is `None` for a path ending in `..` or naming only a root,
    // and never `Some("")`, so this is the whole of the check.
    path.file_name()
        .ok_or_else(|| ConfigError::OutputHasNoFileName {
            path: path.to_path_buf(),
        })?;

    let extension_matches = path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(OUTPUT_EXTENSION));
    if !extension_matches {
        return Err(ConfigError::OutputIsNotMatroska {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Checks that a directory exists and that a file can be created inside it.
fn check_directory_usable(directory: &Path) -> Result<(), ConfigError> {
    let metadata = match fs::metadata(directory) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::OutputDirectoryMissing {
                directory: directory.to_path_buf(),
            })
        }
        Err(source) => {
            return Err(ConfigError::OutputDirectoryNotWritable {
                directory: directory.to_path_buf(),
                source,
            })
        }
    };
    if !metadata.is_dir() {
        return Err(ConfigError::OutputDirectoryIsNotADirectory {
            directory: directory.to_path_buf(),
        });
    }

    probe_writable(directory).map_err(|source| ConfigError::OutputDirectoryNotWritable {
        directory: directory.to_path_buf(),
        source,
    })
}

/// Creates and removes a uniquely named file to prove the directory is
/// writable.
///
/// Windows has no permission bit that can be read and believed — a directory's
/// read-only attribute means nothing, and an ACL has to be evaluated against
/// the process token — so the only honest test is to try. Failing here rather
/// than twenty minutes into a session is the point: by the time the encoder has
/// frames to write, the recording is what is lost (AGENTS.md section 17).
fn probe_writable(directory: &Path) -> io::Result<()> {
    // The name is unique per process *and* per call, so that two validations
    // running at once — two tests, or a recorder and a replay save — cannot
    // collide and report each other's probe as "the directory is not writable".
    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    let probe = directory.join(format!(
        ".clipped-write-probe-{}-{}",
        std::process::id(),
        NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
    ));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe);
    match file {
        Ok(file) => {
            drop(file);
            // Removing the probe is best-effort: the directory has already
            // proved it is writable, and failing the whole command because a
            // zero-byte file could not be deleted would be worse than leaving
            // it. It is logged so that it is not invisible.
            if let Err(error) = fs::remove_file(&probe) {
                tracing::warn!(
                    error = %error,
                    "could not remove the write probe file after checking the output directory"
                );
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Checks that nothing is already at the output path.
fn check_output_is_free(path: &Path, overwrite: bool) -> Result<(), ConfigError> {
    match fs::metadata(path) {
        Err(_) => Ok(()),
        Ok(metadata) if metadata.is_dir() => Err(ConfigError::OutputIsADirectory {
            path: path.to_path_buf(),
        }),
        Ok(_) if overwrite => Ok(()),
        Ok(_) => Err(ConfigError::OutputExists {
            path: path.to_path_buf(),
        }),
    }
}

/// Where recordings go when `--output` is not given.
///
/// The user's videos folder, which is where a video recorder's output belongs
/// and which is not a directory Clipped has to invent. Returns `None` only if
/// the environment has no home directory at all, which is a broken environment
/// rather than a configuration the recorder should guess around.
pub fn default_output_directory() -> Option<PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    }?;
    if home.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(home)
            .join("Videos")
            .join(DEFAULT_OUTPUT_SUBDIRECTORY),
    )
}

/// Where installed highlight plugins live.
///
/// `<Clipped's per-user directory>/plugins`, one directory per plugin, each
/// holding a `plugin.json` and the executable it names
/// ([docs/plugin-api.md](../../../docs/plugin-api.md)). The per-user directory
/// is `clipped-logging`'s answer, which is the same one the log, the encoder's
/// capability cache and the user's own game catalogue use — a second opinion
/// about where Clipped keeps its files is exactly what that crate exists to
/// prevent.
///
/// Returns `None` when the environment describes no per-user directory at all,
/// which on Windows means `%LOCALAPPDATA%` is unset. That is a machine with no
/// plugins rather than an error, and it is treated as one.
///
/// **Nothing puts a plugin there.** Installing one is a manual step today
/// ([issue #338](https://github.com/wildware-uk/clipped/issues/338) is what
/// starts one during a recording; packaging is not built), so this answers
/// where to look rather than where something was put.
pub fn plugins_directory() -> Option<PathBuf> {
    clipped_logging::application_directory().map(|directory| directory.join("plugins"))
}

/// The generated name for a recording started at `now`.
///
/// Sortable, unambiguous and free of characters Windows forbids in a file name,
/// so a directory of recordings reads in the order they were made.
pub fn default_output_file_name(now: OffsetDateTime) -> String {
    let format = format_description!("[year][month][day]-[hour][minute][second]");
    let stamp = now
        .format(format)
        .unwrap_or_else(|_| "unknown-time".to_owned());
    format!("clipped-{stamp}.{OUTPUT_EXTENSION}")
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use time::macros::datetime;

    use super::*;

    /// A directory of this test's own, removed by [`TestDirectory`]'s `Drop`.
    ///
    /// The workspace has no `tempfile` dependency and this is not enough reason
    /// to add one: `crates/logging`'s tests build the same thing out of
    /// `std::env::temp_dir` and the process id, and following that keeps the
    /// dependency count where it is (AGENTS.md sections 10 and 55).
    #[derive(Debug)]
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "clipped-recorder-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("the temporary directory can be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Step 3 of the MVP definition: a directory chosen once in the settings
    /// screen is where recordings land, without anybody passing a flag
    /// (issue #307).
    #[test]
    fn the_settings_file_decides_where_a_recording_goes_when_no_flag_does() {
        let configured = TestDirectory::new("configured-recordings");

        let resolved = resolve_output(
            None,
            Some(configured.path()),
            datetime!(2026-08-10 14:32:05 UTC),
        )
        .expect("the configured directory exists and is writable");

        assert_eq!(
            resolved.parent(),
            Some(configured.path()),
            "a recording went to {resolved:?} rather than to the configured directory"
        );
    }

    /// And the flag still wins, because a run somebody typed a path into is a
    /// run they meant to put somewhere else.
    #[test]
    fn an_output_flag_beats_the_settings_file() {
        let configured = TestDirectory::new("configured-loser");
        let asked_for = TestDirectory::new("asked-for");
        let file = asked_for.path().join("one.mkv");

        let resolved = resolve_output(
            Some(&file),
            Some(configured.path()),
            datetime!(2026-08-10 14:32:05 UTC),
        )
        .expect("the named file is in a usable directory");

        assert_eq!(resolved, file);
        assert_ne!(resolved.parent(), Some(configured.path()));
    }

    fn args_for(window: &str) -> RecordArgs {
        RecordArgs {
            window: Some(window.to_owned()),
            ..RecordArgs::default()
        }
    }

    /// A `replay` invocation of `window`, writing into `directory`.
    fn replay_args_in(directory: &Path, window: &str) -> crate::cli::ReplayArgs {
        crate::cli::ReplayArgs {
            record: RecordArgs {
                output: Some(directory.join("replay.mkv")),
                ..args_for(window)
            },
            duration: None,
            save_duration: None,
        }
    }

    /// The window the settings file resolves to when nobody has changed it.
    fn configured_window() -> std::time::Duration {
        clipped_session::config::DEFAULT_REPLAY_WINDOW
    }

    #[test]
    fn a_replay_keeps_the_configured_window_and_saves_the_whole_of_it() {
        // Nothing typed means the setting decides, and a save with no duration
        // of its own means the whole buffer — which is SPEC.md section 42's
        // "hotkey saves the previous N seconds" with N being the one number.
        let directory = TestDirectory::new("replay-defaults");
        let config = ReplayConfig::resolve(
            &replay_args_in(directory.path(), "Counter-Strike 2"),
            configured_window(),
            None,
        )
        .expect("a window and an output are enough");

        assert_eq!(config.window, configured_window());
        assert_eq!(config.save, configured_window());
    }

    #[test]
    fn a_duration_on_the_command_line_overrides_the_configured_one() {
        let directory = TestDirectory::new("replay-duration");
        let mut args = replay_args_in(directory.path(), "Counter-Strike 2");
        args.duration = Some("90".parse().expect("ninety seconds is in range"));

        let config =
            ReplayConfig::resolve(&args, configured_window(), None).expect("a valid replay");

        assert_eq!(config.window, std::time::Duration::from_secs(90));
        assert_eq!(
            config.save,
            std::time::Duration::from_secs(90),
            "a save with nothing said still means the whole buffer, and the buffer moved"
        );
    }

    #[test]
    fn a_save_longer_than_the_buffer_is_refused_with_both_numbers() {
        // The one rule neither value's own parser can enforce, because neither
        // can see the other. It is refused rather than silently shortened: "I
        // asked for five minutes and got thirty seconds" is the quiet
        // disappointment AGENTS.md section 54 is about, and unlike a buffer
        // that has not filled yet this is knowable before anything is recorded.
        let directory = TestDirectory::new("replay-too-long");
        let mut args = replay_args_in(directory.path(), "Counter-Strike 2");
        args.duration = Some("60".parse().expect("in range"));
        args.save_duration = Some("120".parse().expect("in range"));

        let error = ReplayConfig::resolve(&args, configured_window(), None)
            .expect_err("a minute of buffer cannot yield two minutes of clip");

        let message = error.to_string();
        assert!(
            message.contains("120") && message.contains("60"),
            "the refusal has to name both numbers, or there is nothing to change: {message}"
        );

        // And the same save against a buffer long enough for it is accepted,
        // so the refusal is a rule rather than a refusal of everything.
        args.duration = Some("300".parse().expect("in range"));
        assert!(ReplayConfig::resolve(&args, configured_window(), None).is_ok());
    }

    #[test]
    fn a_replay_resolves_its_recording_by_exactly_the_rules_record_uses() {
        // `replay` is `record` with a buffer beside it, so a target it cannot
        // resolve or an output it cannot write has to fail identically — the
        // same error type, from the same function (AGENTS.md section 55).
        let directory = TestDirectory::new("replay-recording");
        let args = replay_args_in(directory.path(), "Counter-Strike 2");

        let config = ReplayConfig::resolve(&args, configured_window(), None)
            .expect("a valid replay")
            .recording;
        assert_eq!(
            config.target,
            CaptureTarget::WindowTitle("Counter-Strike 2".to_owned())
        );
        assert_eq!(config.output, directory.path().join("replay.mkv"));

        let mut broken = replay_args_in(directory.path(), "Counter-Strike 2");
        broken.record.output = Some(directory.path().join("replay.mp4"));
        let error = ReplayConfig::resolve(&broken, configured_window(), None)
            .expect_err("the recorder writes Matroska");
        assert!(matches!(error, ConfigError::OutputIsNotMatroska { .. }));
    }

    #[test]
    fn a_window_title_resolves_to_a_window_target() {
        let args = args_for(" Counter-Strike 2 ");
        assert_eq!(
            resolve_target(&args).expect("a single selector"),
            CaptureTarget::WindowTitle("Counter-Strike 2".to_owned())
        );
    }

    #[test]
    fn no_selector_names_all_three() {
        let error = resolve_target(&RecordArgs::default()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "no capture target: choose exactly one of `window`, `process` or `pid`"
        );
    }

    #[test]
    fn two_selectors_are_rejected_rather_than_one_being_preferred() {
        let args = RecordArgs {
            window: Some("Counter-Strike 2".to_owned()),
            pid: Some(1234),
            ..RecordArgs::default()
        };
        let error = resolve_target(&args).unwrap_err();
        assert_eq!(
            error.to_string(),
            "more than one capture target: choose exactly one of `window`, `process` or `pid`"
        );
    }

    #[test]
    fn an_empty_window_title_is_refused_because_it_would_match_everything() {
        let error = resolve_target(&args_for("   ")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "`window` needs some text to match a window title against; \
             an empty value would match every window"
        );
    }

    #[test]
    fn a_process_path_is_refused_with_the_form_that_is_wanted() {
        let args = RecordArgs {
            process: Some(r"C:\Games\cs2.exe".to_owned()),
            ..RecordArgs::default()
        };
        let error = resolve_target(&args).unwrap_err();
        assert_eq!(
            error.to_string(),
            "`process` takes an executable name such as cs2.exe, not a path; \
             `C:\\Games\\cs2.exe` contains a path separator"
        );
    }

    #[test]
    fn process_id_zero_is_refused() {
        let args = RecordArgs {
            pid: Some(0),
            ..RecordArgs::default()
        };
        let error = resolve_target(&args).unwrap_err();
        assert_eq!(
            error.to_string(),
            "`pid` 0 is not a process to record: on Windows 0 is the system idle process"
        );
    }

    #[test]
    fn a_usable_output_path_is_returned_absolute() {
        let directory = TestDirectory::new("usable-output");
        let requested = directory.path().join("session.mkv");

        let resolved = resolve_output(Some(&requested), None, datetime!(2026-08-10 14:32:05 UTC))
            .expect("the directory exists and is writable");

        assert_eq!(resolved, requested);
        assert!(resolved.is_absolute());
    }

    #[test]
    fn the_write_probe_leaves_nothing_behind() {
        let directory = TestDirectory::new("probe-cleanup");
        let requested = directory.path().join("session.mkv");

        resolve_output(Some(&requested), None, datetime!(2026-08-10 14:32:05 UTC))
            .expect("the directory exists and is writable");

        let left_behind: Vec<_> = fs::read_dir(directory.path())
            .expect("the directory can be listed")
            .map(|entry| entry.expect("the entry can be read").file_name())
            .collect();
        assert!(
            left_behind.is_empty(),
            "validation left files behind: {left_behind:?}"
        );
    }

    #[test]
    fn a_relative_output_path_is_made_absolute() {
        // Against `absolute` rather than through `resolve_output`, because
        // `resolve_output` would then probe the current directory for
        // writability — which under `cargo test` is this crate's source
        // directory, and a killed run would leave a probe file in it.
        // `absolute` is where the whole of the rule lives.
        let resolved =
            absolute(Path::new("clips/session.mkv")).expect("the current directory can be read");

        assert!(resolved.is_absolute(), "{} is relative", resolved.display());
        assert_eq!(
            resolved,
            std::env::current_dir()
                .expect("the current directory can be read")
                .join("clips")
                .join("session.mkv")
        );
    }

    #[test]
    fn an_absolute_output_path_is_left_alone() {
        let already = TestDirectory::new("already-absolute");
        let requested = already.path().join("session.mkv");
        assert_eq!(
            absolute(&requested).expect("an absolute path needs no current directory"),
            requested
        );
    }

    #[test]
    fn a_missing_output_directory_is_reported_with_the_directory_in_it() {
        let directory = TestDirectory::new("missing-parent");
        let missing = directory.path().join("nope");
        let requested = missing.join("session.mkv");

        let error =
            resolve_output(Some(&requested), None, datetime!(2026-08-10 14:32:05 UTC)).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "the directory {} does not exist; create it, or choose an output path \
                 inside a directory that does",
                missing.display()
            )
        );
    }

    #[test]
    fn a_file_where_the_output_directory_should_be_is_reported_as_such() {
        let directory = TestDirectory::new("parent-is-a-file");
        let not_a_directory = directory.path().join("in-the-way");
        File::create(&not_a_directory).expect("the file can be created");
        let requested = not_a_directory.join("session.mkv");

        let error =
            resolve_output(Some(&requested), None, datetime!(2026-08-10 14:32:05 UTC)).unwrap_err();

        assert!(
            error
                .to_string()
                .ends_with("is not a directory, so nothing can be written inside it"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn an_output_path_that_is_not_matroska_explains_why() {
        let directory = TestDirectory::new("wrong-extension");
        let requested = directory.path().join("session.mp4");

        let error =
            resolve_output(Some(&requested), None, datetime!(2026-08-10 14:32:05 UTC)).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "the output path {} must end in .mkv: the recorder writes Matroska, which \
                 survives an interrupted recording (ADR 0001). Converting to MP4 is a remux \
                 afterwards, not a recording setting",
                requested.display()
            )
        );
    }

    #[test]
    fn a_matroska_extension_is_accepted_whatever_its_case() {
        let directory = TestDirectory::new("uppercase-extension");
        let requested = directory.path().join("session.MKV");
        assert!(check_output_file_name(&requested).is_ok());
    }

    #[test]
    fn an_existing_recording_is_not_replaced_silently() {
        let directory = TestDirectory::new("existing-output");
        let existing = directory.path().join("session.mkv");
        File::create(&existing).expect("the file can be created");

        let error = check_output_is_free(&existing, false).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "{} already exists. Recordings are not replaced silently; choose another \
                 output path, or set `overwrite` if you meant to replace it",
                existing.display()
            )
        );

        check_output_is_free(&existing, true).expect("overwrite permits replacing it");
    }

    #[test]
    fn a_directory_at_the_output_path_is_refused_even_with_overwrite() {
        let directory = TestDirectory::new("output-is-a-directory");
        let in_the_way = directory.path().join("session.mkv");
        fs::create_dir(&in_the_way).expect("the directory can be created");

        let error = check_output_is_free(&in_the_way, true).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "the output path {} is an existing directory, not a file",
                in_the_way.display()
            )
        );
    }

    #[test]
    fn the_default_file_name_is_sortable_and_names_its_container() {
        assert_eq!(
            default_output_file_name(datetime!(2026-08-10 14:32:05 UTC)),
            "clipped-20260810-143205.mkv"
        );
        assert_eq!(
            default_output_file_name(datetime!(2026-01-02 03:04:05 UTC)),
            "clipped-20260102-030405.mkv"
        );
    }

    #[test]
    fn an_unwritable_directory_is_reported_with_the_reason_from_the_operating_system() {
        let error = ConfigError::OutputDirectoryNotWritable {
            directory: PathBuf::from(r"D:\Recordings"),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert_eq!(
            error.to_string(),
            format!(
                "the directory D:\\Recordings cannot be written to: {}",
                io::Error::from(io::ErrorKind::PermissionDenied)
            )
        );
    }

    #[test]
    fn resolving_a_whole_invocation_keeps_every_setting() {
        let directory = TestDirectory::new("whole-invocation");
        let output = directory.path().join("session.mkv");
        let args = RecordArgs {
            window: Some("Counter-Strike 2".to_owned()),
            output: Some(output.clone()),
            resolution: Resolution::Fixed {
                width: 2560,
                height: 1440,
            },
            framerate: "144".parse().expect("a valid framerate"),
            codec: VideoCodec::Av1,
            encoder: EncoderSelection::Nvenc,
            microphone: AudioDeviceSelection::Disabled,
            system_audio: AudioDeviceSelection::Named("Speakers".to_owned()),
            ..RecordArgs::default()
        };

        let config = RecordingConfig::resolve(&args, None).expect("every value is valid");

        assert_eq!(
            config,
            RecordingConfig {
                target: CaptureTarget::WindowTitle("Counter-Strike 2".to_owned()),
                output,
                overwrite: false,
                resolution: Resolution::Fixed {
                    width: 2560,
                    height: 1440
                },
                framerate: "144".parse().expect("a valid framerate"),
                codec: VideoCodec::Av1,
                encoder: EncoderSelection::Nvenc,
                microphone: AudioDeviceSelection::Disabled,
                system_audio: AudioDeviceSelection::Named("Speakers".to_owned()),
            }
        );
    }
}
