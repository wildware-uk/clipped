//! The Clipped recording process.
//!
//! This is a deliberately separate process from the desktop application: if the
//! UI crashes or is closed, recording must continue (SPEC.md section 5, and
//! [ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)).
//!
//! The entry point does four things and delegates the rest to the library
//! target in `src/lib.rs`: parse the command line, start diagnostics, run the
//! subcommand, and turn a failure into a message and an exit code.

use std::process::ExitCode;

use clap::Parser;
use clipped_logging::{LogSettings, LoggingGuard};
use clipped_recorder::cli::Cli;

fn main() -> ExitCode {
    // Parsed before anything else so that `--help` and `--version` cost
    // nothing, and so that a usage error is not preceded by a log banner.
    let cli = Cli::parse();

    // Held until `main` returns, which is what keeps the background log writer
    // alive (docs/logging.md).
    let _diagnostics = start_diagnostics();

    // The first thing in every log, because it is the variable most likely to
    // explain a bug report nobody can reproduce, and because the
    // corresponding-source obligation turns on which build was shipped
    // (`docs/licensing.md`, issue #123). Read out of the libraries this process
    // actually loaded rather than from a constant compiled in, so a machine
    // running an FFmpeg of its own — which `.cargo/config.toml` deliberately
    // allows — says so instead of repeating the pin
    // ([issue #256](https://github.com/wildware-uk/clipped/issues/256)).
    //
    // After logging is started and before anything else runs, so that a
    // recording which fails on its first frame still has this line above the
    // failure.
    let ffmpeg = clipped_muxer::linkage::linked_build();
    tracing::info!(
        build = %ffmpeg.identifier,
        licence = %ffmpeg.licence,
        avformat = %ffmpeg.avformat,
        avcodec = %ffmpeg.avcodec,
        avutil = %ffmpeg.avutil,
        "the FFmpeg this process loaded"
    );

    match clipped_recorder::run(&cli) {
        Ok(()) => ExitCode::from(clipped_recorder::EXIT_SUCCESS),
        Err(error) => {
            report(&error);
            ExitCode::from(error.exit_code())
        }
    }
}

/// Starts logging, or explains why it could not and carries on.
///
/// A recorder that refuses to run because it cannot write a log file would be
/// choosing its diagnostics over the user's recording, which is the wrong way
/// round (AGENTS.md section 17). The failure is printed rather than logged, for
/// the obvious reason.
fn start_diagnostics() -> Option<LoggingGuard> {
    match clipped_logging::init(&LogSettings::default()) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!(
                "clipped-recorder: warning: diagnostics could not be started, so this run \
                 will not be in the logs: {error}"
            );
            None
        }
    }
}

/// Prints a failure in the form clap prints its own, so that a rejected value
/// and a misspelled flag look the same to whoever is reading.
fn report(error: &clipped_recorder::RunError) {
    eprintln!("error: {error}");
    if error.is_usage_error() {
        eprintln!("\nFor more information, try 'clipped-recorder record --help'.");
    }
}
