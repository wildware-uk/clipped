//! Measures what accounting a library costs, on a library this harness builds.
//!
//! Issue #93 asks for accounting over a large library to complete "within a
//! measured, reasonable time". This is the measurement, and it exists so that
//! the figures in `docs/storage-management.md` are something somebody ran rather
//! than something somebody estimated (AGENTS.md section 19).
//!
//! It builds a synthetic library — games, sessions, recordings, thumbnails —
//! under a temporary directory, walks it with
//! [`clipped_library::accounting::scan`], and reports how long that took. The
//! files are small on purpose: a walk reads no file's contents, so the cost is
//! in the number of directory entries and not in the bytes behind them, and
//! writing a realistic 200 GB of video would measure the disk rather than the
//! code.
//!
//! ```text
//! cargo run -p clipped-library --example scan_cost -- --files 50000
//! ```
//!
//! It opens no window, needs no GPU and plays no audio.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;

use clipped_library::accounting::{scan, ScanOptions, StorageCategory, StorageRoots};

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(
    about = "Times a storage accounting scan over a synthetic library",
    long_about = None
)]
struct Arguments {
    /// How many recording files to create.
    #[arg(long, default_value_t = 10_000)]
    files: usize,

    /// How many recordings go in one session directory.
    #[arg(long, default_value_t = 8)]
    files_per_session: usize,

    /// How many sessions belong to one game.
    #[arg(long, default_value_t = 40)]
    sessions_per_game: usize,

    /// How many bytes each file holds.
    #[arg(long, default_value_t = 1024)]
    bytes_per_file: usize,

    /// Where to build the library. Defaults to a directory in the system
    /// temporary directory, which is removed afterwards.
    #[arg(long)]
    path: Option<PathBuf>,

    /// Leaves the library in place instead of removing it.
    #[arg(long)]
    keep: bool,
}

fn main() -> std::io::Result<()> {
    let arguments = Arguments::parse();

    let root = arguments.path.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("clipped-scan-cost-{}", std::process::id()))
    });

    println!(
        "Building {} files under {}",
        arguments.files,
        root.display()
    );
    let built = Instant::now();
    let written = build(&root, &arguments)?;
    println!(
        "  built in {:.1}s ({} files, {} directories, {:.1} MB)",
        built.elapsed().as_secs_f64(),
        written.files,
        written.directories,
        bytes_to_megabytes(written.bytes)
    );

    let roots = StorageRoots::new()
        .with(StorageCategory::Recordings, root.join("recordings"))
        .expect("an absolute path")
        .with(StorageCategory::Thumbnails, root.join("thumbnails"))
        .expect("an absolute path");

    // Twice: the first walk pays for whatever the filesystem cache has not got,
    // and the second is the steady state a user's second visit to the settings
    // screen sees. Both are reported, because quoting only the warm figure would
    // flatter it.
    for pass in 1..=2 {
        let report = scan(&roots, &ScanOptions::new());
        let seconds = report.elapsed().as_secs_f64();

        println!(
            "Pass {pass}: {} files, {} directories, {:.1} MB in {:.3}s ({:.0} files/second, complete: {})",
            report.files_seen(),
            report.directories_seen(),
            bytes_to_megabytes(report.inventory().total_bytes()),
            seconds,
            f64::from(u32::try_from(report.files_seen()).unwrap_or(u32::MAX)) / seconds.max(1e-9),
            report.inventory().is_complete()
        );

        assert_eq!(
            report.inventory().total_bytes(),
            written.bytes,
            "the scan must report exactly what this harness wrote"
        );
    }

    // What holding the inventory costs, as an estimate rather than a
    // measurement: one entry per file plus the bytes of its path.
    let report = scan(&roots, &ScanOptions::new());
    let paths: usize = report
        .inventory()
        .files()
        .map(|entry| entry.path().as_os_str().len())
        .sum();
    let entries =
        report.files_seen() * core::mem::size_of::<clipped_library::accounting::FileEntry>();
    println!(
        "Inventory footprint (estimate): {:.1} MB for {} entries",
        bytes_to_megabytes((entries + paths) as u64),
        report.files_seen()
    );

    if arguments.keep {
        println!("Left in place: {}", root.display());
    } else {
        fs::remove_dir_all(&root)?;
    }

    Ok(())
}

/// What building the library produced.
struct Written {
    files: usize,
    directories: usize,
    bytes: u64,
}

/// Builds a library of the requested shape.
fn build(root: &Path, arguments: &Arguments) -> std::io::Result<Written> {
    let contents = vec![b'x'; arguments.bytes_per_file];
    let mut written = Written {
        files: 0,
        directories: 0,
        bytes: 0,
    };

    let files_per_game = arguments.files_per_session.max(1) * arguments.sessions_per_game.max(1);
    let games = arguments.files.div_ceil(files_per_game.max(1)).max(1);

    let mut remaining = arguments.files;
    'games: for game in 0..games {
        for session in 0..arguments.sessions_per_game.max(1) {
            let directory = root
                .join("recordings")
                .join(format!("game-{game:03}"))
                .join(format!("session-{session:04}"));
            fs::create_dir_all(&directory)?;
            written.directories += 1;

            for file in 0..arguments.files_per_session.max(1) {
                if remaining == 0 {
                    break 'games;
                }

                write(
                    &directory.join(format!("recording-{file:03}.mkv")),
                    &contents,
                )?;
                written.files += 1;
                written.bytes += contents.len() as u64;
                remaining -= 1;
            }
        }
    }

    // One thumbnail per recording, in a flat directory, which is the shape that
    // makes a single enormous directory part of the measurement.
    let thumbnails = root.join("thumbnails");
    fs::create_dir_all(&thumbnails)?;
    written.directories += 1;
    let thumbnail = vec![b'x'; 64];
    for index in 0..written.files {
        write(
            &thumbnails.join(format!("thumb-{index:06}.jpg")),
            &thumbnail,
        )?;
        written.bytes += thumbnail.len() as u64;
    }
    written.files *= 2;

    Ok(written)
}

/// Writes one file of exactly `contents`.
fn write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(contents)
}

/// A byte count in decimal megabytes, which is how disk figures are quoted.
fn bytes_to_megabytes(bytes: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let megabytes = bytes as f64 / 1_000_000.0;
    megabytes
}
