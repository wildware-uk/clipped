//! The `list-windows` subcommand.
//!
//! Prints what the recorder can be pointed at, and — given a selector — what
//! one of them resolves to. It is the answer to "why can't it find my game?",
//! which is otherwise unanswerable without a debugger, and it is how the
//! selection rules in `clipped-windows` are exercised by hand.
//!
//! Everything it knows comes from [`clipped_windows`]. There is no Windows API
//! call in this file and there should never be one: platform code lives at the
//! bottom of the stack (AGENTS.md section 5).
//!
//! The list goes to standard output, because it is the command's result and a
//! user may want to pipe it. Diagnostics go to the log and to standard error,
//! as they do everywhere else (docs/recorder-cli.md).

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use clipped_windows::{
    enumerate_monitors, enumerate_windows, monitor_info, resolve, scale_percentage, Exclusion,
    MonitorHandle, ResolveError, TargetSelector, WindowInfo, WindowsError,
};

use crate::cli::ListWindowsArgs;

/// Shown in the monitor column when a window's display cannot be named.
///
/// A display can be detached between a window being enumerated and its monitor
/// being looked up. That is a two-millisecond window that will never be seen,
/// but the alternative to a placeholder is failing the whole listing over one
/// row.
const UNKNOWN: &str = "unknown";

/// Why `list-windows` could not answer.
#[derive(Debug)]
pub enum ListWindowsError {
    /// The desktop could not be enumerated.
    Enumeration(WindowsError),
    /// The selector did not name exactly one capturable window.
    Resolution(ResolveError),
}

impl fmt::Display for ListWindowsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enumeration(error) => write!(formatter, "{error}"),
            Self::Resolution(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for ListWindowsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Enumeration(error) => Some(error),
            Self::Resolution(error) => Some(error),
        }
    }
}

impl From<WindowsError> for ListWindowsError {
    fn from(error: WindowsError) -> Self {
        Self::Enumeration(error)
    }
}

impl From<ResolveError> for ListWindowsError {
    fn from(error: ResolveError) -> Self {
        Self::Resolution(error)
    }
}

/// Lists the windows, or resolves the one the arguments named.
///
/// # Errors
///
/// [`ListWindowsError::Enumeration`] if Windows would not describe the desktop,
/// and [`ListWindowsError::Resolution`] if a selector matched no window, or
/// more than one.
pub fn run(args: &ListWindowsArgs) -> Result<(), ListWindowsError> {
    // Before anything is measured, and only once per process: without it every
    // size below is the compatibility fiction Windows tells a DPI-unaware
    // process, and a 4K window on a 150% display is reported as 2560x1440.
    if let Err(error) = clipped_windows::enable_per_monitor_dpi_awareness() {
        tracing::debug!(
            %error,
            "per-monitor DPI awareness was not set, which is expected if it was already set; \
             window sizes are reported as Windows gives them"
        );
    }

    let windows = enumerate_windows()?;
    let displays = display_names();
    tracing::debug!(
        windows = windows.len(),
        capturable = windows
            .iter()
            .filter(|window| window.is_capturable())
            .count(),
        displays = displays.len(),
        "enumerated the desktop"
    );

    match args.selector() {
        Some(selector) => {
            let resolved = resolve(&windows, &selector)?;
            print_resolution(&selector, resolved, &displays);
        }
        None => print_listing(&windows, &displays, args.all),
    }

    Ok(())
}

/// The device name of every attached display, by handle.
///
/// Enumerated once and looked up per window, rather than asking Windows for
/// each window's display in turn: a listing of a hundred windows would
/// otherwise make a hundred calls to describe the same two monitors.
fn display_names() -> HashMap<MonitorHandle, String> {
    match enumerate_monitors() {
        Ok(monitors) => monitors
            .into_iter()
            .map(|monitor| (monitor.handle(), monitor.device_name().to_owned()))
            .collect(),
        Err(error) => {
            // A listing without display names is still a useful listing, so
            // this is reported and stepped over rather than failing the
            // command (AGENTS.md section 15).
            tracing::warn!(%error, "displays could not be enumerated; monitor names are unknown");
            HashMap::new()
        }
    }
}

/// Prints the capturable windows, and optionally the ones that are not.
fn print_listing(windows: &[WindowInfo], displays: &HashMap<MonitorHandle, String>, all: bool) {
    let (capturable, excluded): (Vec<&WindowInfo>, Vec<&WindowInfo>) =
        windows.iter().partition(|window| window.is_capturable());

    println!(
        "{} of {} top-level windows can be captured.",
        capturable.len(),
        windows.len()
    );
    println!();

    if capturable.is_empty() {
        println!("Nothing on this desktop can be captured. Pass --all to see why.");
    } else {
        print_table(
            &[
                "HANDLE", "PID", "PROCESS", "CLIENT", "DPI", "MONITOR", "TITLE",
            ],
            &capturable
                .iter()
                .map(|window| {
                    vec![
                        window.handle().to_string(),
                        window.process_id().to_string(),
                        window.process_name().unwrap_or(UNKNOWN).to_owned(),
                        client_size(window),
                        window.geometry().dpi().to_string(),
                        display_name(window, displays),
                        window.title().to_owned(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }

    if excluded.is_empty() {
        return;
    }

    if all {
        println!();
        println!("Windows that cannot be captured:");
        println!();
        print_table(
            &["HANDLE", "PID", "PROCESS", "REASON", "TITLE"],
            &excluded
                .iter()
                .map(|window| {
                    vec![
                        window.handle().to_string(),
                        window.process_id().to_string(),
                        window.process_name().unwrap_or(UNKNOWN).to_owned(),
                        window
                            .exclusion()
                            .map_or(UNKNOWN, Exclusion::as_str)
                            .to_owned(),
                        window.title().to_owned(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
        println!();
        print_legend(&excluded);
    } else {
        println!();
        println!(
            "{} more window{} cannot be captured. Pass --all to list them with the reason.",
            excluded.len(),
            if excluded.len() == 1 { "" } else { "s" }
        );
    }
}

/// Explains each reason that actually appeared, and only those.
fn print_legend(excluded: &[&WindowInfo]) {
    let mut seen: Vec<Exclusion> = Vec::new();
    for window in excluded {
        if let Some(reason) = window.exclusion() {
            if !seen.contains(&reason) {
                seen.push(reason);
            }
        }
    }

    for reason in seen {
        println!("  {:<18}{}", reason.as_str(), reason.explanation());
    }
}

/// Prints everything known about the one window a selector resolved to.
fn print_resolution(
    selector: &TargetSelector,
    window: &WindowInfo,
    displays: &HashMap<MonitorHandle, String>,
) {
    let geometry = window.geometry();
    println!("{selector} resolves to one window:");
    println!();
    println!("  Handle    {}", window.handle());
    println!("  Title     {}", window.title());
    println!(
        "  Process   {} (pid {})",
        window.process_name().unwrap_or(UNKNOWN),
        window.process_id()
    );
    println!(
        "  Client    {} pixels at {} DPI ({}% scale)",
        geometry.client_size(),
        geometry.dpi(),
        scale_percentage(geometry.dpi())
    );
    println!("  Monitor   {}", monitor_description(window, displays));

    if window.is_minimised() {
        println!();
        println!(
            "This window is minimised. Its size is not final and capture produces no frames \
             until it is restored."
        );
    }
}

/// The window's display, described as fully as it can still be read.
fn monitor_description(window: &WindowInfo, displays: &HashMap<MonitorHandle, String>) -> String {
    match monitor_info(window.geometry().monitor()) {
        Ok(monitor) => monitor.to_string(),
        // The handle was valid when the window was enumerated. If it is not now,
        // the display has just been detached or rearranged; the name that was
        // read a moment ago is still the most useful thing to print.
        Err(_) => display_name(window, displays),
    }
}

fn display_name(window: &WindowInfo, displays: &HashMap<MonitorHandle, String>) -> String {
    displays
        .get(&window.geometry().monitor())
        .cloned()
        .unwrap_or_else(|| UNKNOWN.to_owned())
}

/// A window's client size, or a note that a minimised window has not got one
/// worth printing.
fn client_size(window: &WindowInfo) -> String {
    if window.is_minimised() {
        "minimised".to_owned()
    } else {
        window.geometry().client_size().to_string()
    }
}

/// Prints rows under headers, with every column as wide as its widest value.
///
/// The last column is not padded, so a long window title cannot push a line
/// past the terminal width for the rows that follow it.
fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|header| header.len()).collect();
    for row in rows {
        for (column, value) in row.iter().enumerate() {
            let width = display_width(value);
            if width > widths[column] {
                widths[column] = width;
            }
        }
    }

    println!("{}", pad(headers.iter().map(|h| (*h).to_owned()), &widths));
    for row in rows {
        println!("{}", pad(row.iter().cloned(), &widths));
    }
}

fn pad(values: impl Iterator<Item = String>, widths: &[usize]) -> String {
    let values: Vec<String> = values.collect();
    let last = values.len().saturating_sub(1);
    let mut line = String::new();
    for (column, value) in values.into_iter().enumerate() {
        if column == last {
            line.push_str(&value);
        } else {
            let padding = widths[column].saturating_sub(display_width(&value)) + 2;
            line.push_str(&value);
            line.push_str(&" ".repeat(padding));
        }
    }
    line
}

/// How many columns a value occupies, near enough for aligning a table.
///
/// Counted in `char`s rather than bytes, so that a window title with an
/// accented or non-Latin character does not shift its row. This is not full
/// terminal width measurement — a CJK title still occupies two columns per
/// character and will misalign — and going further would mean a Unicode width
/// dependency for a diagnostic listing, which is not a trade worth making
/// (AGENTS.md section 10).
fn display_width(value: &str) -> usize {
    value.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_aligns_every_column_but_the_last() {
        let widths = [6, 3];
        let line = pad(
            ["0x01".to_owned(), "a title".to_owned()].into_iter(),
            &widths,
        );
        assert_eq!(
            line, "0x01    a title",
            "columns are padded to width plus two"
        );
    }

    #[test]
    fn a_column_wider_than_its_header_is_not_truncated() {
        let widths = [2, 2];
        let line = pad(
            ["0x000104ac".to_owned(), "x".to_owned()].into_iter(),
            &widths,
        );
        assert!(
            line.starts_with("0x000104ac  "),
            "a value wider than the column keeps its two spaces: {line:?}"
        );
    }

    #[test]
    fn alignment_counts_characters_rather_than_bytes() {
        // "Savaş" is five characters and six bytes. Padding by bytes would
        // shift the following column left by one.
        assert_eq!(display_width("Savaş"), 5);
    }
}
