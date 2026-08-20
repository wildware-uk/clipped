//! The two ways to read the process table, measured against each other.
//!
//! [Issue #288](https://github.com/wildware-uk/clipped/issues/288) asks one
//! question and this answers it: is `NtQuerySystemInformation` enough cheaper
//! than `CreateToolhelp32Snapshot` to be worth depending on an API Microsoft
//! documents as subject to change?
//!
//! [`clipped_windows::process_table`] — the shipped read, and the only one in
//! this workspace (AGENTS.md section 55) — takes a Toolhelp snapshot and walks
//! it. `ProcessTree` calls it every rescan interval for the whole length of a
//! recording, so its cost is paid continuously rather than once, which is what
//! made it worth measuring at all.
//!
//! # Why this is an example and not a benchmark
//!
//! The number that matters is a proportion of one core on a machine in
//! ordinary use, with a few hundred processes running and a game among them.
//! That is not a state a benchmark harness can construct — it is the machine
//! you are sitting at — so the measurement is a program you run on it, the way
//! `process_tree_probe` is.
//!
//! # Why the answer is not just the two timings
//!
//! Both APIs are asked the same question — identifier, parent identifier and
//! image name for every process — and this checks they give the same answer,
//! because a cheaper call that reads the parent identifier out of the wrong
//! offset is not cheaper, it is wrong. That check is the greater part of the
//! risk: the parent identifier is not in the struct Microsoft documents. It
//! lives where `windows-rs` declares `Reserved2`, immediately after
//! `UniqueProcessId`, and the whole of the saving below is bought by reading a
//! field named `Reserved2`.
//!
//! ```text
//! cargo run --release -p clipped-windows --example process_table_apis
//! cargo run --release -p clipped-windows --example process_table_apis -- 200
//! ```

use std::time::{Duration, Instant};

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Win32::Foundation::STATUS_INFO_LENGTH_MISMATCH;
use windows::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION;

/// One row, reduced to the three things both APIs are being asked for.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Row {
    pid: u32,
    parent_pid: u32,
    name: String,
}

/// What one `NtQuerySystemInformation` answer contained.
///
/// The rows are what the caller wanted. The other two are what it had to be
/// given to get them, and they are the finding: `SystemProcessInformation`
/// describes every *thread* of every process as well, and there is no way to
/// ask it not to.
struct Answer {
    rows: Vec<Row>,
    bytes: usize,
    threads: u64,
}

fn main() {
    let rounds: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "100".to_owned())
        .parse()
        .expect("the first argument is a number of rounds");

    // One of each first, thrown away. The first `NtQuerySystemInformation`
    // pays for resolving the import and for growing the buffer to a size that
    // fits, and neither is a cost the hundredth call pays.
    let _ = clipped_windows::process_table().expect("the process table can be read");
    let accounting = read_with_nt_query().expect("NtQuerySystemInformation answers");

    let mut toolhelp = Vec::with_capacity(rounds);
    let mut nt_query = Vec::with_capacity(rounds);
    let mut disagreements = Vec::new();

    for round in 0..rounds {
        // Interleaved rather than one API and then the other: the machine is in
        // ordinary use, so a burst of other work during the first half would
        // otherwise be read as one API being slower than the other.
        let started = Instant::now();
        let by_toolhelp = clipped_windows::process_table().expect("the process table can be read");
        toolhelp.push(started.elapsed());

        let started = Instant::now();
        let by_nt = read_with_nt_query().expect("NtQuerySystemInformation answers");
        nt_query.push(started.elapsed());

        // Only the first round's rows are compared in full. The table changes
        // between two calls a millisecond apart — that is the machine running,
        // not a disagreement — so comparing every round would report the churn.
        if round == 0 {
            disagreements = compare(&normalise_toolhelp(&by_toolhelp), &normalise(&by_nt.rows));
        }
    }

    let rows = clipped_windows::process_table()
        .expect("the process table can be read")
        .len();

    println!("{rows} processes, {rounds} rounds of each API");
    // Where the two timings come from, rather than a curiosity: the query was
    // asked for a few hundred processes and answered with every thread of every
    // one of them.
    println!(
        "the query returned {:.2} MB describing {} processes and {} threads\n",
        accounting.bytes as f64 / (1024.0 * 1024.0),
        accounting.rows.len(),
        accounting.threads,
    );
    report("CreateToolhelp32Snapshot", &mut toolhelp);
    report("NtQuerySystemInformation", &mut nt_query);

    let toolhelp_median = median(&mut toolhelp);
    let nt_median = median(&mut nt_query);
    println!(
        "\nNtQuerySystemInformation is {:.2}x the speed of the snapshot at the median",
        toolhelp_median.as_secs_f64() / nt_median.as_secs_f64()
    );

    // What one second's rescanning costs, which is the unit SPEC.md section 38's
    // budget is written in.
    for (label, middle) in [
        ("CreateToolhelp32Snapshot", toolhelp_median),
        ("NtQuerySystemInformation", nt_median),
    ] {
        println!(
            "  {label}: {:.4}% of one core, rescanning every second",
            middle.as_secs_f64() * 100.0
        );
    }

    println!();
    if disagreements.is_empty() {
        println!("The two APIs agree on every row: identifier, parent and name.");
    } else {
        println!(
            "{} row(s) differ between the two APIs:",
            disagreements.len()
        );
        for line in &disagreements {
            println!("  {line}");
        }
        std::process::exit(1);
    }
}

/// Every process, read through `NtQuerySystemInformation`.
///
/// # Errors
///
/// The `NTSTATUS` as text when the call fails for a reason other than the
/// buffer being too small, which it grows past.
fn read_with_nt_query() -> Result<Answer, String> {
    // `u64` rather than `u8` so the allocation is aligned for the struct: the
    // rows contain pointers and `usize`s, and a `Vec<u8>` guarantees alignment
    // of one.
    let mut buffer = vec![0u64; 64 * 1024];

    let length = loop {
        let bytes = u32::try_from(buffer.len() * size_of::<u64>()).expect("the buffer fits a u32");
        let mut needed = 0u32;
        // SAFETY: the pointer is to `bytes` bytes this scope owns, the length
        // passed is that same size, and `needed` is a live `u32`. The call only
        // writes within the length it is given, and reports what it wanted when
        // that is too little.
        let status = unsafe {
            NtQuerySystemInformation(
                SystemProcessInformation,
                buffer.as_mut_ptr().cast(),
                bytes,
                &mut needed,
            )
        };

        if status.is_ok() {
            break needed as usize;
        }
        if status != STATUS_INFO_LENGTH_MISMATCH {
            return Err(format!("NtQuerySystemInformation failed: {status:?}"));
        }
        // Half again beyond what it asked for. Processes start between the two
        // calls, and a buffer sized to exactly the previous answer is a loop
        // that can run more than twice on a busy machine.
        let wanted = needed as usize / size_of::<u64>();
        buffer.resize(wanted + wanted / 2 + 1024, 0);
    };

    let mut rows = Vec::with_capacity(512);
    let mut threads = 0u64;
    let base = buffer.as_ptr().cast::<u8>();
    let mut offset = 0usize;
    loop {
        if offset + size_of::<SYSTEM_PROCESS_INFORMATION>() > length {
            return Err("a row began past the end of what was written".to_owned());
        }
        // SAFETY: `offset` is within the `length` bytes the call wrote, checked
        // above, and the buffer is aligned for the struct. The read is a copy:
        // nothing keeps a reference into the buffer.
        let entry = unsafe { base.add(offset).cast::<SYSTEM_PROCESS_INFORMATION>().read() };

        // `ImageName` is a `UNICODE_STRING` pointing into this same buffer, and
        // is null for the idle process, which has no image.
        let name = if entry.ImageName.Buffer.is_null() {
            String::new()
        } else {
            let units = entry.ImageName.Length as usize / size_of::<u16>();
            // SAFETY: the pointer and length are the ones the kernel wrote for
            // this row, and both address memory inside the buffer above, which
            // outlives this read.
            String::from_utf16_lossy(unsafe {
                std::slice::from_raw_parts(entry.ImageName.Buffer.as_ptr(), units)
            })
        };

        // Counted, not read. Every one of these has a
        // `SYSTEM_THREAD_INFORMATION` between this row and the next — that is
        // what the space between the size of a row and `NextEntryOffset` is —
        // and the kernel gathers and copies it whether or not anybody wanted it.
        threads += u64::from(entry.NumberOfThreads);

        rows.push(Row {
            pid: entry.UniqueProcessId.0 as usize as u32,
            // `Reserved2` in `windows-rs`, `InheritedFromUniqueProcessId` in
            // every description of the real layout. This one field is the whole
            // reason the issue asks whether the saving is worth the risk: it is
            // read by position, out of a struct member the crate declines to
            // name, and `main` checks it against Toolhelp for that reason.
            parent_pid: entry.Reserved2 as usize as u32,
            name,
        });

        if entry.NextEntryOffset == 0 {
            break;
        }
        offset += entry.NextEntryOffset as usize;
    }

    Ok(Answer {
        rows,
        bytes: length,
        threads,
    })
}

/// The Toolhelp rows, in the form the comparison uses.
fn normalise_toolhelp(rows: &[clipped_windows::ProcessTableEntry]) -> Vec<Row> {
    normalise(
        &rows
            .iter()
            .map(|row| Row {
                pid: row.pid(),
                parent_pid: row.parent_pid(),
                name: row.name().to_owned(),
            })
            .collect::<Vec<_>>(),
    )
}

/// Rows sorted and case-folded, so the two answers are comparable.
fn normalise(rows: &[Row]) -> Vec<Row> {
    let mut rows: Vec<Row> = rows
        .iter()
        .map(|row| Row {
            pid: row.pid,
            parent_pid: row.parent_pid,
            name: row.name.to_ascii_lowercase(),
        })
        .collect();
    rows.sort();
    rows
}

/// How the two answers differ, described a row at a time.
///
/// The idle process is excluded. It is identifier zero, has no image, and the
/// two APIs name it differently — Toolhelp invents `[System Process]` where the
/// kernel reports no name at all. That is a naming convention rather than a
/// disagreement about what is running, and reporting it every run would bury a
/// real difference under a known one.
fn compare(toolhelp: &[Row], nt: &[Row]) -> Vec<String> {
    let mut differences = Vec::new();

    for row in toolhelp.iter().filter(|row| row.pid != 0) {
        match nt.iter().find(|other| other.pid == row.pid) {
            None => differences.push(format!(
                "{} ({}) is in the snapshot and not in the query",
                row.pid, row.name
            )),
            Some(other) if other.parent_pid != row.parent_pid => differences.push(format!(
                "{} ({}): snapshot says parent {}, query says {}",
                row.pid, row.name, row.parent_pid, other.parent_pid
            )),
            Some(other) if other.name != row.name => differences.push(format!(
                "{}: snapshot says {:?}, query says {:?}",
                row.pid, row.name, other.name
            )),
            Some(_) => {}
        }
    }

    for row in nt.iter().filter(|row| row.pid != 0) {
        if !toolhelp.iter().any(|other| other.pid == row.pid) {
            differences.push(format!(
                "{} ({}) is in the query and not in the snapshot",
                row.pid, row.name
            ));
        }
    }

    differences
}

/// Prints the spread of one API's timings.
fn report(label: &str, timings: &mut [Duration]) {
    timings.sort();
    println!(
        "{label:24}  min {:>7.3} ms   median {:>7.3} ms   max {:>7.3} ms",
        timings[0].as_secs_f64() * 1_000.0,
        median(timings).as_secs_f64() * 1_000.0,
        timings[timings.len() - 1].as_secs_f64() * 1_000.0,
    );
}

/// The middle timing, which is the one to compare: the maximum is whatever else
/// the machine was doing.
fn median(timings: &mut [Duration]) -> Duration {
    timings.sort();
    timings[timings.len() / 2]
}
