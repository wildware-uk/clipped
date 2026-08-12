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

use clipped_library::accounting::{
    scan, ScanOptions, StorageCategory, StorageInventory, StorageRoots,
};

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

    /// Where to build the library. Must not already exist: this harness creates
    /// the directory and removes it, and it will not write into or delete a
    /// directory it did not make. Defaults to a new directory in the system
    /// temporary directory.
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

    // The harness removes what it built, so it must only ever build somewhere
    // it created. Pointed at a real library — `--path D:\Clipped\Recordings` —
    // an unconditional `remove_dir_all` at the end would take the recordings
    // with it, and recordings are irreplaceable (AGENTS.md section 56). This is
    // the only guard that matters here, so it runs before anything is written:
    // `create_dir_all` succeeds on an existing directory, which is exactly the
    // case being refused.
    if root.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; this harness builds a library and then removes \
                 the directory it built, so it refuses to use one it did not create. \
                 Give --path a name that is not there yet.",
                root.display()
            ),
        ));
    }
    fs::create_dir_all(&root)?;

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

    // What holding the inventory costs. An estimate, and the arithmetic is
    // spelled out because the obvious version of it is wrong: a
    // `BTreeMap<PathBuf, FileEntry>` stores the path *twice* — once as the key
    // and once inside the entry — so each file owns two heap allocations of its
    // path as well as the two structs.
    //
    // Not included: the B-tree's node overhead (a node holds up to eleven
    // key/value pairs and a little bookkeeping, so it is a few per cent), the
    // allocator's rounding of each allocation up to a size class, and the
    // spare capacity `PathBuf` may hold. The figure is therefore a floor, and
    // is quoted as one in docs/storage-management.md.
    let report = scan(&roots, &ScanOptions::new());
    let paths: usize = report
        .inventory()
        .files()
        .map(|entry| entry.path().as_os_str().len())
        .sum();
    let structures = report.files_seen()
        * (core::mem::size_of::<PathBuf>()
            + core::mem::size_of::<clipped_library::accounting::FileEntry>());
    let footprint = structures + 2 * paths;
    println!(
        "Inventory footprint (estimate): {:.1} MB for {} entries \
         ({} bytes each: {} of structures, {} of path text, both copies)",
        bytes_to_megabytes(footprint as u64),
        report.files_seen(),
        footprint / report.files_seen().max(1),
        structures / report.files_seen().max(1),
        2 * paths / report.files_seen().max(1),
    );

    report_allocated_size(&root, report.inventory());

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

/// Measures what the volume actually spends on the library it was given, and
/// checks it against the documented tolerance.
///
/// `docs/storage-management.md` says the reported figure — the sum of logical
/// file lengths — is within *one cluster per file* of what the volume allocates.
/// That was arithmetic from a measured cluster size until this existed. Here it
/// is a measurement: `FILE_STANDARD_INFO.AllocationSize`, which is what NTFS
/// has actually reserved for each file, summed over the whole library and
/// compared against both the logical total and the bound.
///
/// It needs no elevation, and it is done after the timed passes so that the
/// per-file handle it opens cannot flatter or slow them.
#[cfg(windows)]
fn report_allocated_size(root: &Path, inventory: &StorageInventory) {
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandleEx, FILE_STANDARD_INFO,
    };

    let mut allocated = 0u64;
    let mut logical = 0u64;
    let mut measured = 0u64;

    for entry in inventory.files() {
        let Ok(file) = fs::File::open(entry.path()) else {
            continue;
        };

        let mut information = FILE_STANDARD_INFO::default();
        // SAFETY: `file` owns the handle and outlives the call; the buffer
        // pointer addresses a `FILE_STANDARD_INFO` local of exactly the size
        // passed, which is what `FileStandardInfo` writes; the call retains
        // nothing.
        let result = unsafe {
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileStandardInfo,
                std::ptr::from_mut(&mut information).cast(),
                u32::try_from(core::mem::size_of::<FILE_STANDARD_INFO>())
                    .expect("a fixed-size structure of a few dozen bytes"),
            )
        };

        if result.is_ok() {
            allocated += u64::try_from(information.AllocationSize).unwrap_or(0);
            logical += entry.bytes();
            measured += 1;
        }
    }

    if measured == 0 {
        println!("Allocated size: nothing could be opened to measure");
        return;
    }

    let cluster = cluster_size(root);
    let overhead = allocated.saturating_sub(logical);
    println!(
        "Allocated size: {:.1} MB against {:.1} MB logical over {measured} files \
         ({} bytes of rounding per file, cluster size {} bytes)",
        bytes_to_megabytes(allocated),
        bytes_to_megabytes(logical),
        overhead / measured,
        cluster
            .map(|size| size.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
    );

    if let Some(cluster) = cluster {
        println!(
            "  within one cluster per file: {} ({} <= {})",
            overhead <= cluster * measured,
            overhead,
            cluster * measured
        );
    }
}

#[cfg(not(windows))]
fn report_allocated_size(_root: &Path, _inventory: &StorageInventory) {
    // `FILE_STANDARD_INFO` is the Windows answer, and Windows is what Clipped
    // ships on. Elsewhere the harness still times a scan; it just cannot say
    // what the volume spent.
    println!("Allocated size: not measured (this measurement is Windows-only)");
}

/// The volume's cluster size in bytes, if Windows will say.
#[cfg(windows)]
fn cluster_size(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceW;

    // `GetDiskFreeSpaceW` takes the root of the volume — `C:\` — rather than a
    // directory on it.
    let volume = path.components().next()?;
    let volume = Path::new(volume.as_os_str()).join(std::path::MAIN_SEPARATOR_STR);
    let wide: Vec<u16> = volume
        .as_os_str()
        .encode_wide()
        .chain(core::iter::once(0))
        .collect();

    let mut sectors_per_cluster = 0u32;
    let mut bytes_per_sector = 0u32;

    // SAFETY: `wide` is null-terminated and outlives the call, and both output
    // pointers address `u32` locals that outlive it too.
    unsafe {
        GetDiskFreeSpaceW(
            PCWSTR(wide.as_ptr()),
            Some(&mut sectors_per_cluster),
            Some(&mut bytes_per_sector),
            None,
            None,
        )
    }
    .ok()?;

    Some(u64::from(sectors_per_cluster) * u64::from(bytes_per_sector))
}

/// A byte count in decimal megabytes, which is how disk figures are quoted.
fn bytes_to_megabytes(bytes: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let megabytes = bytes as f64 / 1_000_000.0;
    megabytes
}
