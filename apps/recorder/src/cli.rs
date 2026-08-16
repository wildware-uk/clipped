//! The recorder's command-line surface.
//!
//! This module describes the arguments and nothing else: it does not touch the
//! filesystem, does not enumerate windows and does not start anything.
//! Validation that needs the world is [`crate::config`]; validation that is a
//! property of the value itself lives in the types in [`crate::options`], so
//! that clap reports it in the usual place with the usual formatting.
//!
//! # Adding a subcommand
//!
//! Add a variant to [`Command`] and a match arm to [`crate::run`]. Three
//! subcommands that were previously specified here as absent are now all
//! declared below:
//!
//! - `list-windows` — enumerate capturable windows
//!   ([issue #10](https://github.com/wildware-uk/clipped/issues/10)).
//! - `capabilities` — report detected encoders and codecs
//!   ([issue #14](https://github.com/wildware-uk/clipped/issues/14)).
//! - `serve` — the recorder as the service the desktop application drives
//!   ([issue #49](https://github.com/wildware-uk/clipped/issues/49)).
//! - `start-at-login` — ask Windows to run `serve` when this user signs in
//!   ([issue #106](https://github.com/wildware-uk/clipped/issues/106)).
//! - `watch` — record games automatically as they start and stop
//!   ([issue #46](https://github.com/wildware-uk/clipped/issues/46)).
//! - `recover` — the footage a killed recorder left behind
//!   ([issue #103](https://github.com/wildware-uk/clipped/issues/103)).
//! - `replay` — record with a rolling buffer and save the last N seconds on a
//!   hotkey ([issue #38](https://github.com/wildware-uk/clipped/issues/38)).
//!
//! Nothing is currently specified without being declared here. A subcommand
//! that parses arguments and then does nothing is a control that silently
//! does nothing, which AGENTS.md section 27 rules out for a command line as
//! much as for a window — so a new one stays undeclared until it works.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use clipped_windows::{TargetSelector, WindowHandle};

use crate::options::{
    AudioDeviceSelection, EncoderSelection, Framerate, ReplayLength, ReplayWindow, Resolution,
    VideoCodec,
};

/// Text appended to the top-level `--help`.
const AFTER_HELP: &str = "\
Exit codes:
  0  the command succeeded
  1  the command failed while running
  2  the arguments were rejected
  3  the command is not implemented yet

Diagnostics are written to %LOCALAPPDATA%\\Clipped\\logs and to standard error.
Set CLIPPED_LOG to change the level for one run, for example CLIPPED_LOG=debug.";

/// The mutually exclusive ways of naming what to record.
///
/// Exactly one is required, and they are the only `record` arguments without a
/// default — which is what makes "every other option documents a default" a
/// property the tests can check rather than a list they have to be told.
pub const TARGET_ARGUMENTS: [&str; 3] = ["window", "process", "pid"];

/// The `clipped-recorder` command line.
#[derive(Debug, Parser)]
#[command(
    name = "clipped-recorder",
    version,
    about = "Records a Windows game to a Matroska file",
    long_about = "Records a Windows game to a Matroska file.\n\n\
                  The recorder is a process of its own, independent of the Clipped desktop \
                  application (ADR 0002), and this is how it is driven without one.",
    after_help = AFTER_HELP,
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The recorder's subcommands.
///
/// See the module documentation for how the next two slot in.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Record a window or process to a file.
    Record(RecordArgs),

    /// Record with a replay buffer, and keep the last N seconds on a hotkey.
    ///
    /// What the category of application exists for (SPEC.md sections 15 and
    /// 16): the recording runs, the last few minutes are kept in memory, and
    /// Ctrl+F10 turns that into a clip of the thing that just happened. Ctrl+C
    /// stops, finishing the recording first.
    Replay(ReplayArgs),

    /// Watch for games and record them without being asked.
    ///
    /// The mode the product exists for (SPEC.md section 2): a game launching
    /// starts a session recording and quitting it finalises one, with nothing
    /// to press. Ctrl+C stops watching, finishing any recording first.
    /// `docs/sessions.md` describes what a session is and how it decides.
    Watch(WatchArgs),

    /// List the windows that can be captured.
    ListWindows(ListWindowsArgs),

    /// See what a plugin declares, and allow or stop one.
    ///
    /// A plugin is a program somebody else wrote, and every bundled one opens a
    /// loopback socket. Enabling one **is** the consent to what it declares, so
    /// the declaration is printed before it is taken and again whenever it
    /// changes (`docs/privacy.md`). The screen that will do this in the window
    /// is issue #281.
    Plugins(PluginsArgs),

    /// Report the encoders and codecs detected on this machine.
    ///
    /// Says which answers were measured here and which were inferred from
    /// published limits, because acting on the second kind is how a recording
    /// fails at the encoder rather than in the settings screen.
    Capabilities(CapabilitiesArgs),

    /// Listen for the desktop application and do what it asks.
    ///
    /// This is the shape the recorder runs in outside a terminal: a process
    /// with its own lifetime that the user interface connects to, sends
    /// commands to and receives status from (ADR 0002, docs/ipc.md). Ctrl+C
    /// stops it, finishing any recording first.
    Serve(ServeArgs),

    /// Turn starting at login on or off for this account, or report it.
    ///
    /// Nothing enables this on its own: the only thing that writes the registry
    /// value is `start-at-login enable`, and `disable` removes it again
    /// (ADR 0006, docs/privacy.md).
    StartAtLogin(StartAtLoginArgs),

    /// List the recordings an interrupted recorder left behind, and keep or
    /// discard them.
    ///
    /// A recorder that was killed leaves a file that plays as far as it got and
    /// a session record that never says the recording finished. With no
    /// arguments this lists them and changes nothing (docs/sessions.md).
    Recover(RecoverArgs),

    /// Say what the library occupies, and what automatic cleanup would do.
    ///
    /// Reads and prints; it never deletes. The dry run issue #111 asks for —
    /// exactly what a sweep would take, before anybody trusts a storage limit
    /// with their recordings — and the list of the largest recordings, so that
    /// somebody can act before automatic deletion does.
    Storage(StorageArgs),
}

/// Arguments to `clipped-recorder storage`.
#[derive(Debug, Default, Args)]
pub struct StorageArgs {
    /// Directory to measure. [default: the Clipped folder of your videos
    /// directory]
    ///
    /// The same directory `watch` writes into.
    #[arg(long, value_name = "PATH")]
    pub directory: Option<PathBuf>,
}

/// Arguments to `clipped-recorder recover`.
///
/// The two actions are a group rather than a `--action` value because they are
/// not two settings of one thing: listing is the default and is safe, adopting
/// is safe and deliberate, and discarding deletes footage. clap refuses both at
/// once.
#[derive(Debug, Default, Args)]
#[command(group(ArgGroup::new("recovery").args(["adopt", "discard"])))]
pub struct RecoverArgs {
    /// Directory to look in. [default: the Clipped folder of your videos
    /// directory]
    ///
    /// The same directory `watch` writes into.
    //
    // The default is stated in the summary rather than through a
    // `default_value` because it is resolved from the environment, and `-h`
    // should still show what it will be.
    #[arg(long, value_name = "PATH")]
    pub directory: Option<PathBuf>,

    /// Only this session, by the identifier `recover` prints. [default: all of
    /// them]
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,

    /// Keep the recordings and stop listing them. [default: off]
    ///
    /// Nothing is written to the footage itself; what changes is the session
    /// record, so the recording is indexed like any other.
    #[arg(long)]
    pub adopt: bool,

    /// Move one recording's file to the trash and record that you did.
    /// [default: off]
    ///
    /// The file is moved rather than deleted, so it is recoverable — but not
    /// yet through the trash screen or its retention, because a recovered
    /// fragment has no library row for those to key off.
    /// `docs/recorder-cli.md` and `docs/storage-management.md` say exactly
    /// what that costs. Requires `--session`: even a recoverable action on
    /// footage is refused in bulk, so it always names the one recording it
    /// moves (AGENTS.md section 56).
    #[arg(long, requires = "session")]
    pub discard: bool,
}

/// What a `recover` invocation was asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverAction {
    /// Say what there is and change nothing.
    List,
    /// Keep the recordings.
    Adopt,
    /// Delete the named recording.
    Discard,
}

impl RecoverArgs {
    /// What the arguments asked for.
    ///
    /// Listing is what "nothing was asked for" means, and it is deliberately
    /// the default: somebody typing `recover` to find out where their recording
    /// went must not have anything happen to it.
    #[must_use]
    pub const fn action(&self) -> RecoverAction {
        if self.discard {
            RecoverAction::Discard
        } else if self.adopt {
            RecoverAction::Adopt
        } else {
            RecoverAction::List
        }
    }
}

/// Arguments to `clipped-recorder watch`.
///
/// The video and audio options are `record`'s, and mean the same things. What
/// is deliberately absent is a capture-mode option: Full Session is the only
/// mode this build can run, and an option offering four values three of which
/// would do nothing is exactly the control AGENTS.md section 27 rules out. Per
/// game overrides are M7 (SPEC.md section 31) and are also absent for that
/// reason rather than by oversight.
#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Directory to write recordings and session records into. [default: the
    /// Clipped folder of your videos directory]
    ///
    /// Created if it is not there, at start-up rather than when a game
    /// launches, so that a drive that is not connected is reported before it
    /// costs somebody a session.
    //
    // The default is stated in the summary rather than through a
    // `default_value` because it is resolved from the environment, and `-h`
    // should still show what it will be.
    #[arg(long, value_name = "PATH")]
    pub output_directory: Option<PathBuf>,

    /// Seconds to wait for a game to put a window on screen before giving up on
    /// recording it.
    ///
    /// A launch is reported a few seconds after the process starts and a game
    /// can take much longer than that to reach a window while it compiles
    /// shaders, so this is not a timeout on anything going wrong; it is how
    /// long a game is allowed to take to appear.
    #[arg(long, value_name = "SECONDS", default_value_t = 120)]
    pub window_timeout: u32,

    /// Size to encode at, as WIDTHxHEIGHT, or `source` for the game's own size.
    #[arg(short, long, value_name = "WIDTHxHEIGHT", default_value_t = Resolution::Source)]
    pub resolution: Resolution,

    /// Frames per second to encode at.
    #[arg(short, long, value_name = "FPS", default_value_t = Framerate::DEFAULT)]
    pub framerate: Framerate,

    /// Video codec to encode with.
    #[arg(long, value_name = "CODEC", default_value_t = VideoCodec::Auto)]
    pub codec: VideoCodec,

    /// Encoder to encode with.
    #[arg(long, value_name = "ENCODER", default_value_t = EncoderSelection::Auto)]
    pub encoder: EncoderSelection,

    /// Microphone to record, as `default`, `none`, or part of a device name.
    #[arg(long, value_name = "DEVICE", default_value_t = AudioDeviceSelection::Default)]
    pub microphone: AudioDeviceSelection,

    /// System audio output to record, as `default`, `none`, or part of a
    /// device name.
    #[arg(long, value_name = "DEVICE", default_value_t = AudioDeviceSelection::Default)]
    pub system_audio: AudioDeviceSelection,
}

/// Arguments to `clipped-recorder plugins`.
#[derive(Debug, Args)]
pub struct PluginsArgs {
    /// What to do.
    #[command(subcommand)]
    pub action: PluginsAction,
}

/// What `plugins` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum PluginsAction {
    /// Show what is installed, what each asks for, and what you have agreed to.
    ///
    /// The reading action, and the one to run before any other: enabling a
    /// plugin is agreeing to the network access it declares, and this is where
    /// that declaration is printed.
    List,

    /// Allow a plugin to run, agreeing to what it declares now.
    ///
    /// The declaration is printed first, every time, including when you have
    /// enabled this plugin before — consent to something you were not shown is
    /// not consent (`docs/privacy.md`).
    Enable {
        /// The plugin's identifier, as `plugins list` prints it.
        plugin: String,
    },

    /// Stop a plugin running, keeping what you agreed to.
    ///
    /// Turning it back on will not ask again unless its declaration has
    /// changed in the meantime.
    Disable {
        /// The plugin's identifier, as `plugins list` prints it.
        plugin: String,
    },
}

/// Arguments to `clipped-recorder start-at-login`.
#[derive(Debug, Args)]
pub struct StartAtLoginArgs {
    /// What to do.
    #[arg(value_enum)]
    pub action: StartAtLoginAction,
}

/// What `start-at-login` was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StartAtLoginAction {
    /// Write the value, so the recorder starts when this user signs in.
    Enable,
    /// Remove the value.
    Disable,
    /// Report what is configured, without changing it.
    ///
    /// The default action would be a poor choice for a subcommand that can
    /// change a machine's startup, so there is none: the action is required and
    /// this is the one that reads.
    Status,
}

/// Arguments to `clipped-recorder serve`.
#[derive(Debug, Default, Args)]
pub struct ServeArgs {
    /// Listen on a named pipe of this name rather than the default.
    ///
    /// The default is `clipped-recorder.<session>`, where the session is the
    /// Windows sign-in session this process belongs to — which is what keeps
    /// two signed-in users' recorders apart. Name one yourself to run a second
    /// recorder beside it, for development or for a test that must not reach
    /// the recorder somebody is using. [default: clipped-recorder.<session>]
    ///
    /// A name, not a path: letters, digits, `-`, `_` and `.`. The `\.\pipe\`
    /// prefix is added for you, so an endpoint can never be pointed at another
    /// machine.
    //
    // The default is stated in the summary rather than through a `default_value`
    // because it is resolved from the operating system, and `-h` should still
    // show what it will be.
    #[arg(long, value_name = "NAME")]
    pub endpoint: Option<String>,
}

/// The mutually exclusive ways of naming one window to `list-windows`.
///
/// The same three `record` accepts, plus `--handle`, which is the answer to an
/// ambiguous match: every candidate is reported with its handle, and a handle
/// is the one selector that cannot be ambiguous.
pub const SELECTOR_ARGUMENTS: [&str; 4] = ["window", "process", "pid", "handle"];

/// Arguments to `clipped-recorder list-windows`.
///
/// With no selector it lists; with one it resolves, and reports the candidates
/// if more than one window answers to it.
///
/// `record` goes through the same [`clipped_windows::resolve`] with the same
/// three selectors, so what this subcommand reports for a selector is what
/// `record` will point at — including the candidates of an ambiguous one. That
/// is deliberate: "why did it record the wrong window?" should be answerable
/// before the recording, not after it.
#[derive(Debug, Default, Args)]
#[command(group(ArgGroup::new("selector").args(SELECTOR_ARGUMENTS)))]
pub struct ListWindowsArgs {
    /// Also list the windows that cannot be captured, and why. [default: off]
    #[arg(long)]
    pub all: bool,

    /// Resolve the window whose title contains this text.
    #[arg(long, value_name = "TITLE", value_parser = parse_selector_text)]
    pub window: Option<String>,

    /// Resolve the window belonging to this executable, such as `cs2.exe`.
    #[arg(long, value_name = "NAME", value_parser = parse_selector_text)]
    pub process: Option<String>,

    /// Resolve the window belonging to this process identifier.
    #[arg(long, value_name = "PID")]
    pub pid: Option<u32>,

    /// Resolve this exact window, as printed in the HANDLE column.
    ///
    /// Hexadecimal with `0x`, or decimal.
    #[arg(long, value_name = "HANDLE", value_parser = parse_window_handle)]
    pub handle: Option<WindowHandle>,
}

impl ListWindowsArgs {
    /// The selector the arguments named, if any.
    ///
    /// [`None`] means "list everything" rather than "nothing was asked for":
    /// the group is optional, and clap has already rejected any invocation
    /// naming two selectors.
    #[must_use]
    pub fn selector(&self) -> Option<TargetSelector> {
        if let Some(title) = &self.window {
            return Some(TargetSelector::WindowTitle(title.clone()));
        }
        if let Some(name) = &self.process {
            return Some(TargetSelector::ProcessName(name.clone()));
        }
        if let Some(process_id) = self.pid {
            return Some(TargetSelector::ProcessId(process_id));
        }
        self.handle.map(TargetSelector::WindowHandle)
    }
}

/// Rejects an empty window title or process name.
///
/// An empty substring is inside every title, so `--window=` asks for the whole
/// desktop. [`clipped_windows::resolve`] reads it as matching nothing, which is
/// the only reading available to a pure function that has no way to report a
/// usage error — but that surfaces as ``no window matches the window title
/// containing `` ``, which looks like a formatting bug rather than an answer.
/// A command line *can* report a usage error, so it does, here, before
/// matching ever runs.
///
/// Only wholly blank values are rejected. A title is matched as typed,
/// including leading and trailing spaces, because a window really can be called
/// `Untitled - Notepad ` and the user is the one who knows.
fn parse_selector_text(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err(
            "a window title or process name cannot be empty; run `clipped-recorder \
             list-windows` to see what there is to choose from"
                .to_owned(),
        );
    }

    Ok(value.to_owned())
}

/// Parses a window handle as this program prints one, or as a person would
/// type it.
///
/// `0x000104ac` is what the HANDLE column shows and what most people will
/// paste; a bare decimal is what a script that has the number as an integer
/// will produce. Both are accepted, and nothing else is: a handle typed without
/// its `0x` is read as decimal, because silently treating an ambiguous string
/// as hexadecimal would resolve to a different window than the user believed.
fn parse_window_handle(value: &str) -> Result<WindowHandle, String> {
    let trimmed = value.trim();
    let handle = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(hexadecimal) => isize::from_str_radix(hexadecimal, 16)
            .map_err(|error| format!("`{value}` is not a hexadecimal window handle: {error}"))?,
        None => trimmed
            .parse::<isize>()
            .map_err(|error| format!("`{value}` is not a window handle: {error}"))?,
    };

    if handle == 0 {
        return Err(
            "0 is not a window handle; run `clipped-recorder list-windows` to see the \
             handles that exist"
                .to_owned(),
        );
    }

    Ok(WindowHandle::from_raw(handle))
}

/// Arguments to `clipped-recorder capabilities`.
#[derive(Debug, Default, Args)]
pub struct CapabilitiesArgs {
    /// Ignore the cached report and ask the machine again. [default: off]
    ///
    /// The report is cached in `%LOCALAPPDATA%\Clipped` and invalidated
    /// automatically when an adapter or a driver version changes, so this is
    /// for the case where that has gone wrong — or where you want to see what
    /// detection costs. The fresh answer replaces the cached one.
    #[arg(long)]
    pub refresh: bool,
}

/// Arguments to `clipped-recorder record`.
///
/// Every option has a default except the capture target, so that
/// `clipped-recorder record --window <TITLE>` is a complete invocation.
#[derive(Debug, Default, Args)]
#[command(group(
    ArgGroup::new("target")
        .required(true)
        .args(TARGET_ARGUMENTS)
))]
pub struct RecordArgs {
    /// Record the window whose title contains this text.
    ///
    /// Matching is by substring, so `--window "Counter-Strike"` finds
    /// "Counter-Strike 2". A title matching more than one window is an error
    /// rather than a guess; run `clipped-recorder list-windows --window
    /// <TITLE>` to see the candidates.
    #[arg(long, value_name = "TITLE", value_parser = parse_selector_text)]
    pub window: Option<String>,

    /// Record the window belonging to this executable, such as `cs2.exe`.
    #[arg(long, value_name = "NAME", value_parser = parse_selector_text)]
    pub process: Option<String>,

    /// Record the window belonging to this process identifier.
    #[arg(long, value_name = "PID")]
    pub pid: Option<u32>,

    /// Where to write the recording, ending in `.mkv`. [default: a timestamped
    /// file in the Clipped folder of your videos directory]
    ///
    /// The generated name is, for example,
    /// `%USERPROFILE%\Videos\Clipped\clipped-20260810-143205.mkv`. That
    /// directory is created when there is a recording to put in it; a
    /// directory you name yourself must already exist.
    //
    // The default is stated in the summary rather than below it so that `-h`
    // and `--help` agree about whether this option has one. clap only appends
    // `[default: …]` for itself when a `default_value` is registered, and this
    // default is a path built from the clock and the environment.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Replace the output file if it already exists. [default: off]
    ///
    /// An existing recording is never overwritten silently, because it cannot
    /// be recovered afterwards.
    #[arg(long)]
    pub overwrite: bool,

    /// Size to encode at, as WIDTHxHEIGHT, or `source` for the capture
    /// target's own size.
    ///
    /// Both dimensions must be even and between 128 and 7680.
    #[arg(short, long, value_name = "WIDTHxHEIGHT", default_value_t = Resolution::Source)]
    pub resolution: Resolution,

    /// Frames per second to encode at.
    #[arg(short, long, value_name = "FPS", default_value_t = Framerate::DEFAULT)]
    pub framerate: Framerate,

    /// Video codec to encode with.
    ///
    /// `auto` picks the most efficient codec the selected encoder supports.
    #[arg(long, value_name = "CODEC", default_value_t = VideoCodec::Auto)]
    pub codec: VideoCodec,

    /// Encoder to encode with.
    ///
    /// `auto` prefers hardware encoding and falls back to the CPU.
    #[arg(long, value_name = "ENCODER", default_value_t = EncoderSelection::Auto)]
    pub encoder: EncoderSelection,

    /// Microphone to record, as `default`, `none`, or part of a device name.
    ///
    /// Prefix with `name:` to select a device actually called "default" or
    /// "none".
    #[arg(long, value_name = "DEVICE", default_value_t = AudioDeviceSelection::Default)]
    pub microphone: AudioDeviceSelection,

    /// System audio output to record, as `default`, `none`, or part of a
    /// device name.
    ///
    /// Splitting this into per-application tracks is milestone M2.
    #[arg(long, value_name = "DEVICE", default_value_t = AudioDeviceSelection::Default)]
    pub system_audio: AudioDeviceSelection,
}

/// Arguments to `clipped-recorder replay`.
///
/// Every `record` option, and the same defaults, plus the two the buffer needs.
/// It is a flattened [`RecordArgs`] rather than a copy of its fields for the
/// reason `serve` routes `start_recording` through the same type: a resolution's
/// bounds, a frame rate's range and an output path that must end in `.mkv` are
/// one set of rules, and a second copy of them reachable only from this
/// subcommand would be a second set of answers (AGENTS.md section 55).
///
/// # What it does that `record` does not
///
/// It keeps the last [`Self::duration`] seconds of encoded video in memory and
/// saves them to a clip when Ctrl+F10 is pressed. **It also writes the ordinary
/// recording**, because the buffer is filled from the packets that recording
/// produces — there is one encoder and two consumers of it, not two encodes
/// (`docs/replay-buffer.md`). A capture that keeps *only* the buffer and writes
/// no continuous file is SPEC.md section 4's Manual/Replay capture mode and is
/// [issue #423](https://github.com/wildware-uk/clipped/issues/423).
#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// Everything `record` takes, meaning exactly what it means there.
    #[command(flatten)]
    pub record: RecordArgs,

    /// Seconds of video to keep in the buffer, from 30 to 1800. [default: the
    /// configured replay window, which is 300 unless it has been changed]
    ///
    /// This is what a save can reach back through, and what the buffer's memory
    /// is spent on: about 140 MiB a minute at 1080p60, and about 4 GiB at the
    /// half-hour maximum. `docs/replay-buffer.md` has the table.
    //
    // The default is stated in the summary rather than through a
    // `default_value` because it comes from the settings file, and `-h` should
    // still show what it will be.
    #[arg(short, long, value_name = "SECONDS")]
    pub duration: Option<ReplayWindow>,

    /// Seconds to keep when a replay is saved. [default: the whole of
    /// --duration]
    ///
    /// SPEC.md section 15's periods — 15, 30, 60, 120, 300 — are the ones a
    /// user interface will offer; any whole number of seconds up to the buffer's
    /// own duration is accepted here, which is what "and custom" means.
    //
    // The default is stated in the summary rather than through a `default_value`
    // because it is another argument's value, and `-h` should still show what it
    // will be.
    #[arg(short, long, value_name = "SECONDS")]
    pub save_duration: Option<ReplayLength>,
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;
    use clap::CommandFactory;

    use super::*;

    fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("clipped-recorder").chain(arguments.iter().copied()))
    }

    fn record_args(arguments: &[&str]) -> RecordArgs {
        let Command::Record(args) = parse(arguments).expect("the arguments are valid").command
        else {
            panic!("expected the record subcommand");
        };
        args
    }

    #[test]
    fn the_command_line_definition_is_internally_consistent() {
        // clap's own audit: duplicate argument names, unreachable groups,
        // defaults that its own value parsers would reject.
        Cli::command().debug_assert();
    }

    #[test]
    fn a_window_alone_is_a_complete_invocation() {
        let args = record_args(&["record", "--window", "Counter-Strike 2"]);

        assert_eq!(args.window.as_deref(), Some("Counter-Strike 2"));
        assert_eq!(args.output, None);
        assert!(!args.overwrite);
        assert_eq!(args.resolution, Resolution::Source);
        assert_eq!(args.framerate, Framerate::DEFAULT);
        assert_eq!(args.codec, VideoCodec::Auto);
        assert_eq!(args.encoder, EncoderSelection::Auto);
        assert_eq!(args.microphone, AudioDeviceSelection::Default);
        assert_eq!(args.system_audio, AudioDeviceSelection::Default);
    }

    #[test]
    fn every_option_can_be_set() {
        let args = record_args(&[
            "record",
            "--pid",
            "4242",
            "--output",
            "D:/clips/session.mkv",
            "--overwrite",
            "--resolution",
            "2560x1440",
            "--framerate",
            "144",
            "--codec",
            "av1",
            "--encoder",
            "nvenc",
            "--microphone",
            "none",
            "--system-audio",
            "Speakers",
        ]);

        assert_eq!(args.pid, Some(4242));
        assert_eq!(args.output, Some(PathBuf::from("D:/clips/session.mkv")));
        assert!(args.overwrite);
        assert_eq!(
            args.resolution,
            Resolution::Fixed {
                width: 2560,
                height: 1440
            }
        );
        assert_eq!(args.framerate.frames_per_second(), 144);
        assert_eq!(args.codec, VideoCodec::Av1);
        assert_eq!(args.encoder, EncoderSelection::Nvenc);
        assert_eq!(args.microphone, AudioDeviceSelection::Disabled);
        assert_eq!(
            args.system_audio,
            AudioDeviceSelection::Named("Speakers".to_owned())
        );
    }

    #[test]
    fn a_record_with_no_target_is_a_usage_error() {
        let error = parse(&["record"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        let rendered = error.to_string();
        for expected in ["--window <TITLE>", "--process <NAME>", "--pid <PID>"] {
            assert!(
                rendered.contains(expected),
                "the error should name {expected}: {rendered}"
            );
        }
    }

    #[test]
    fn two_targets_are_rejected_at_parse_time() {
        let error = parse(&["record", "--window", "cs2", "--pid", "12"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        let rendered = error.to_string();
        assert!(
            rendered.contains("--pid <PID>") && rendered.contains("--window <TITLE>"),
            "the error should name both selectors: {rendered}"
        );
    }

    #[test]
    fn an_invalid_resolution_is_reported_with_the_reason_from_the_parser() {
        let error = parse(&["record", "--window", "cs2", "--resolution", "1920x1081"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        assert!(
            error.to_string().contains("must be even"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn an_out_of_range_framerate_is_reported_with_the_range() {
        let error = parse(&["record", "--window", "cs2", "--framerate", "6000"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        assert!(
            error.to_string().contains("1-480"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn an_unknown_codec_lists_the_ones_that_exist() {
        let error = parse(&["record", "--window", "cs2", "--codec", "vp9"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
        let rendered = error.to_string();
        for expected in ["auto", "h264", "hevc", "av1"] {
            assert!(
                rendered.contains(expected),
                "the error should offer {expected}: {rendered}"
            );
        }
    }

    fn replay_args(arguments: &[&str]) -> ReplayArgs {
        let Command::Replay(args) = parse(arguments).expect("the arguments are valid").command
        else {
            panic!("expected the replay subcommand");
        };
        args
    }

    #[test]
    fn a_replay_needs_only_a_target_and_takes_its_durations_from_the_settings() {
        // What SPEC.md section 42 asks for, minus the duration: nothing about a
        // buffer has to be typed, because how much Clipped keeps is a setting.
        let args = replay_args(&["replay", "--window", "Counter-Strike 2"]);

        assert_eq!(args.record.window.as_deref(), Some("Counter-Strike 2"));
        assert_eq!(args.duration, None, "the configured window is the default");
        assert_eq!(args.save_duration, None, "and a save keeps the whole of it");
        // Every `record` option is here and means the same thing, because they
        // are the same arguments.
        assert_eq!(args.record.framerate, Framerate::DEFAULT);
        assert_eq!(args.record.microphone, AudioDeviceSelection::Default);
    }

    #[test]
    fn a_replay_takes_the_same_options_a_record_does() {
        let args = replay_args(&[
            "replay",
            "--process",
            "cs2.exe",
            "--duration",
            "120",
            "--save-duration",
            "15",
            "--framerate",
            "144",
            "--microphone",
            "none",
        ]);

        assert_eq!(args.duration.expect("a duration").seconds(), 120);
        assert_eq!(args.save_duration.expect("a save").seconds(), 15);
        assert_eq!(args.record.framerate.frames_per_second(), 144);
        assert_eq!(args.record.microphone, AudioDeviceSelection::Disabled);
    }

    #[test]
    fn a_replay_with_no_target_is_a_usage_error_naming_the_three_selectors() {
        // The flattened `record` arguments bring their own required group with
        // them, so `replay` cannot be started without saying what to record.
        let error = parse(&["replay"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        let rendered = error.to_string();
        for expected in ["--window <TITLE>", "--process <NAME>", "--pid <PID>"] {
            assert!(
                rendered.contains(expected),
                "the error should name {expected}: {rendered}"
            );
        }
    }

    #[test]
    fn a_replay_duration_no_buffer_can_hold_is_rejected_with_the_range() {
        // Issue #38's third acceptance criterion, at the value's own parser, so
        // that clap reports it where every other rejected value is reported —
        // and before a capture session exists.
        for duration in ["29", "1801", "0", "an hour"] {
            let error = parse(&["replay", "--window", "cs2", "--duration", duration]).unwrap_err();
            let rendered = error.to_string();
            assert!(
                rendered.contains("30") && rendered.contains("1800")
                    || rendered.contains("whole number"),
                "`--duration {duration}` should be refused with the range or the expected \
                 form: {rendered}"
            );
        }

        assert!(parse(&["replay", "--window", "cs2", "--duration", "30"]).is_ok());
        assert!(parse(&["replay", "--window", "cs2", "--duration", "1800"]).is_ok());
    }

    #[test]
    fn a_save_duration_of_zero_is_rejected_and_a_short_one_is_not() {
        // A save has no floor of thirty seconds — SPEC.md section 15 offers
        // fifteen — but zero seconds is not a clip.
        let error = parse(&["replay", "--window", "cs2", "--save-duration", "0"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        assert!(
            error.to_string().contains("1-1800"),
            "unexpected message: {error}"
        );

        assert!(parse(&["replay", "--window", "cs2", "--save-duration", "15"]).is_ok());
    }

    #[test]
    fn replay_help_states_a_default_for_every_optional_argument() {
        // The same property `record` is held to, for the same reason: an option
        // whose default nobody can see is an option nobody can reason about.
        // `--duration` and `--save-duration` have no `default_value` — one
        // comes from the settings file and the other from the first — so both
        // have to say so in the summary, which is the part `-h` prints.
        let command = Cli::command();
        let replay = command
            .find_subcommand("replay")
            .expect("replay is a subcommand");

        let mut checked = 0;
        for argument in replay.get_arguments() {
            let name = argument.get_id().as_str();
            if TARGET_ARGUMENTS.contains(&name) || matches!(name, "help" | "version") {
                continue;
            }

            let summary = argument
                .get_help()
                .map(ToString::to_string)
                .unwrap_or_default();
            assert!(
                !argument.get_default_values().is_empty() || summary.contains("[default:"),
                "`--{name}` documents no default, so `replay -h` cannot show one: {summary}"
            );
            checked += 1;
        }
        assert!(checked > 0, "no optional arguments were found to check");
    }

    #[test]
    fn an_unknown_subcommand_is_a_usage_error_rather_than_a_panic() {
        // `capabilities` used to be this test's example of an unknown
        // subcommand; it is a real one now (issue #14), so a name that is
        // still nobody's subcommand is used instead.
        let error = parse(&["frobnicate"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    fn list_windows_args(arguments: &[&str]) -> ListWindowsArgs {
        let Command::ListWindows(args) = parse(arguments).expect("the arguments are valid").command
        else {
            panic!("expected the list-windows subcommand");
        };
        args
    }

    #[test]
    fn list_windows_needs_no_arguments_and_lists_only_capturable_windows_by_default() {
        let args = list_windows_args(&["list-windows"]);
        assert!(!args.all);
        assert_eq!(args.selector(), None);
    }

    #[test]
    fn each_list_windows_selector_becomes_the_matching_target_selector() {
        assert_eq!(
            list_windows_args(&["list-windows", "--window", "Counter-Strike"]).selector(),
            Some(TargetSelector::WindowTitle("Counter-Strike".to_owned()))
        );
        assert_eq!(
            list_windows_args(&["list-windows", "--process", "cs2.exe"]).selector(),
            Some(TargetSelector::ProcessName("cs2.exe".to_owned()))
        );
        assert_eq!(
            list_windows_args(&["list-windows", "--pid", "4242"]).selector(),
            Some(TargetSelector::ProcessId(4242))
        );
        assert_eq!(
            list_windows_args(&["list-windows", "--handle", "0x000104ac"]).selector(),
            Some(TargetSelector::WindowHandle(WindowHandle::from_raw(
                0x0001_04ac
            )))
        );
    }

    #[test]
    fn an_empty_selector_is_a_usage_error_rather_than_a_match_against_everything() {
        for command in ["list-windows", "record"] {
            for argument in ["--window", "--process"] {
                let error = parse(&[command, argument, ""]).unwrap_err().to_string();
                assert!(
                    error.contains("cannot be empty"),
                    "`{command} {argument} \"\"` should be rejected with a reason: {error}"
                );
            }
        }

        // Whitespace only is the same request wearing a hat.
        let error = parse(&["list-windows", "--window", "   "])
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot be empty"), "{error}");
    }

    #[test]
    fn a_selector_keeps_the_spaces_it_was_typed_with() {
        // A window really can be titled with a trailing space, and a substring
        // match is the user's to specify: only wholly blank values are refused.
        assert_eq!(
            parse_selector_text(" Counter-Strike "),
            Ok(" Counter-Strike ".to_owned())
        );
    }

    #[test]
    fn a_handle_is_accepted_in_the_form_it_is_printed_in_and_in_decimal() {
        for (typed, expected) in [
            ("0x000104ac", 0x0001_04ac),
            ("0X104AC", 0x0001_04ac),
            ("66732", 66_732),
        ] {
            assert_eq!(
                parse_window_handle(typed),
                Ok(WindowHandle::from_raw(expected)),
                "`{typed}` should parse"
            );
        }
    }

    #[test]
    fn a_handle_that_is_not_one_is_rejected_with_the_reason() {
        let error = parse_window_handle("chrome").expect_err("that is not a number");
        assert!(error.contains("chrome"), "unexpected message: {error}");

        let error = parse_window_handle("0").expect_err("0 is not a window");
        assert!(
            error.contains("list-windows"),
            "the message should say how to find a real handle: {error}"
        );
    }

    #[test]
    fn two_list_windows_selectors_are_rejected_at_parse_time() {
        let error = parse(&["list-windows", "--window", "cs2", "--pid", "12"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn record_help_states_a_default_for_every_optional_argument() {
        // The criterion is a property of every option, not of the eleven that
        // happen to exist today, so this walks the arguments rather than
        // listing the strings a snapshot of them renders to. A twelfth option
        // with no default fails here.
        //
        // Whether the default reaches `-h` as well as `--help` is the point of
        // the check: clap renders `[default: …]` into both when a default value
        // is registered with it, and the two options whose default cannot be a
        // `default_value` — `--output` and `--overwrite` — have to say so in
        // the summary, which is the part `-h` prints.
        let command = Cli::command();
        let record = command
            .find_subcommand("record")
            .expect("record is a subcommand");

        let mut checked = 0;
        for argument in record.get_arguments() {
            let name = argument.get_id().as_str();
            if TARGET_ARGUMENTS.contains(&name) || matches!(name, "help" | "version") {
                continue;
            }

            let summary = argument
                .get_help()
                .map(ToString::to_string)
                .unwrap_or_default();
            assert!(
                !argument.get_default_values().is_empty() || summary.contains("[default:"),
                "`--{name}` documents no default, so `record -h` cannot show one: {summary}"
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "no optional arguments were found to check; the walk is looking in the wrong place"
        );
    }

    #[test]
    fn the_capture_target_is_the_only_argument_without_a_default() {
        // `TARGET_ARGUMENTS` is what the test above skips, so it has to stay
        // the set of arguments that genuinely have no default: the capture
        // target, exactly one of which is required.
        let command = Cli::command();
        let record = command
            .find_subcommand("record")
            .expect("record is a subcommand");
        let group = record
            .get_groups()
            .find(|group| group.get_id() == "target")
            .expect("the target group exists");

        let members: Vec<_> = group.get_args().map(|id| id.as_str().to_owned()).collect();
        assert_eq!(members, TARGET_ARGUMENTS, "the target group has changed");
        assert!(group.is_required_set(), "a capture target is required");
    }
}
