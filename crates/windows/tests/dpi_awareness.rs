//! Per-monitor DPI awareness, which is process-wide and therefore needs a
//! process of its own.
//!
//! `SetProcessDpiAwarenessContext` changes how every later measurement in the
//! process is reported, so this cannot share a binary with `desktop.rs`: a test
//! that flipped the mode halfway through would change what `GetClientRect`
//! returns for windows created before it, on any display scaled above 100%.
//! Cargo builds each file under `tests/` as its own executable, which is
//! exactly the isolation this needs.

use clipped_windows::{enable_per_monitor_dpi_awareness, DpiAwareness};

/// The second attempt is reported as "already set", not as a failure.
///
/// This is the branch that matters. Windows rejects a second
/// `SetProcessDpiAwarenessContext` with `ERROR_ACCESS_DENIED`, which is
/// indistinguishable from a real failure to anyone reading a
/// `windows::core::Error` — and a caller that cannot tell the two apart logs
/// both at the level it would use for the harmless one, which is how "every
/// size is wrong on this high-DPI machine" ends up in nobody's log. Telling
/// them apart is this function's job, so it is asserted rather than assumed.
#[test]
fn asking_twice_reports_the_second_as_already_set_rather_than_as_a_failure() {
    // The first call may find the process already aware if a future application
    // manifest declares it, so both outcomes are accepted here. What is not
    // acceptable is an error.
    let first = enable_per_monitor_dpi_awareness()
        .expect("a process with no manifest can be made per-monitor DPI aware");
    assert!(
        matches!(first, DpiAwareness::Set | DpiAwareness::AlreadySet),
        "unexpected first outcome: {first:?}"
    );

    let second = enable_per_monitor_dpi_awareness()
        .expect("a second attempt is refused by Windows, and that refusal is not an error");
    assert_eq!(
        second,
        DpiAwareness::AlreadySet,
        "the second call must be reported as already-set, not as a failure"
    );
}
