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
//! Add a variant to [`Command`] and a match arm to [`crate::run`]. The next two
//! are already specified:
//!
//! - `list-windows` — enumerate capturable windows
//!   ([issue #10](https://github.com/wildware-uk/clipped/issues/10)).
//! - `capabilities` — report detected encoders and codecs
//!   ([issue #14](https://github.com/wildware-uk/clipped/issues/14)).
//!
//! Neither is declared here. A subcommand that parses arguments and then does
//! nothing is a control that silently does nothing, which AGENTS.md section 27
//! rules out for a command line as much as for a window.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand};

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
    /// "Counter-Strike 2". If more than one window matches, the candidates are
    /// reported rather than one being picked (issue #10).
    #[arg(long, value_name = "TITLE")]
    pub window: Option<String>,

    /// Record the window belonging to this executable, such as `cs2.exe`.
    #[arg(long, value_name = "NAME")]
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
        let Command::Record(args) = parse(arguments).expect("the arguments are valid").command;
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
        let error = parse(&["list-windows"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
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
