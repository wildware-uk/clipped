//! Remuxes a recording into MP4 from the command line.
//!
//! The public API of `clipped_muxer::remux` with an argument parser in front of
//! it, so that the behaviour `docs/muxing.md` describes can be reproduced — and
//! measured — without writing a program. It is what the timings in that document
//! were taken with, against the pinned build's own `ffmpeg -c:v libopenh264` for
//! the re-encode they are compared with.
//!
//! ```text
//! cargo run -p clipped-muxer --example remux_recording -- \
//!     --source recording.mkv --destination recording.mp4
//! ```
//!
//! `--inspect` answers what the copy would cost without making it, which is what
//! a user interface would call before offering it.

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clipped_muxer::{remux_to_mp4, Mp4Plan};

#[derive(Debug, Parser)]
#[command(
    about = "Copies a recording into MP4 without re-encoding it",
    long_about = "Copies the coded packets of a recording into an MP4, which is what an upload \
                  target that will not take Matroska needs. No decoder and no encoder runs, so \
                  the picture and the sound are unchanged. Refuses rather than dropping a \
                  picture or sound track MP4 cannot store."
)]
struct Arguments {
    /// The recording to copy. It is opened for reading and never modified.
    #[arg(long)]
    source: PathBuf,

    /// Where to write the MP4. Must not already exist.
    ///
    /// Not required with `--inspect`, which writes nothing.
    #[arg(long, required_unless_present = "inspect")]
    destination: Option<PathBuf>,

    /// Report what an MP4 would contain, and what it would cost, without
    /// writing one.
    #[arg(long)]
    inspect: bool,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();

    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let mut cause: Option<&dyn Error> = Some(error.as_ref());
            while let Some(error) = cause {
                eprintln!("error: {error}");
                cause = error.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &Arguments) -> Result<(), Box<dyn Error>> {
    if arguments.inspect {
        let plan = Mp4Plan::inspect(&arguments.source)?;
        println!("{plan}");
        for track in plan.tracks() {
            println!("  {track}: {:?}", track.carriage());
        }
        return Ok(());
    }

    let Some(destination) = arguments.destination.as_deref() else {
        // Unreachable: clap requires it unless `--inspect` was given.
        return Err("a destination is needed to write an MP4".into());
    };

    let summary = remux_to_mp4(&arguments.source, destination)?;
    for loss in summary.plan().losses() {
        println!("lost: {loss}");
    }
    println!("{summary}");
    println!("elapsed_seconds={:.3}", summary.elapsed().as_secs_f64());

    Ok(())
}
