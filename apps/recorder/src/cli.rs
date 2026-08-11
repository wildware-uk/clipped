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
//! Add a variant to [`Command`] and a match arm to [`crate::run`]. Two
//! subcommands that were previously specified here as absent are now both
//! declared below:
//!
//! - `list-windows` — enumerate capturable windows
//!   ([issue #10](https://github.com/wildware-uk/clipped/issues/10)).
//! - `capabilities` — report detected encoders and codecs
//!   ([issue #14](https://github.com/wildware-uk/clipped/issues/14)).
//!
//! Nothing is currently specified without being declared here. A subcommand
//! that parses arguments and then does nothing is a control that silently
//! does nothing, which AGENTS.md section 27 rules out for a command line as
//! much as for a window — so a new one stays undeclared until it works.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand};
use clipped_windows::{TargetSelector, WindowHandle};

use crate::options::{AudioDeviceSelection, EncoderSelection, Framerate, Resolution, VideoCodec};

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

    /// List the windows that can be captured.
    ListWindows(ListWindowsArgs),

    /// Report the encoders and codecs detected on this machine.
    ///
    /// Says which answers were measured here and which were inferred from
    /// published limits, because acting on the second kind is how a recording
    /// fails at the encoder rather than in the settings screen.
    Capabilities(CapabilitiesArgs),
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
