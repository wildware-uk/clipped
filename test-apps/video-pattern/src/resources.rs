//! What this process is holding, for a test that watches it over hours.
//!
//! [Issue #105](https://github.com/wildware-uk/clipped/issues/105). Many faults
//! only appear after hours of recording, and the recorder is expected to stay
//! resident for days — so the question a soak asks is not "did it work" but
//! "is it holding more than it was".
//!
//! Three counters, because they fail differently:
//!
//! - **Private bytes** is memory this process has committed and nobody else
//!   shares. It is the one that grows when something is leaked outright.
//! - **The working set** is what is resident now, which the operating system
//!   trims under pressure — so it can *fall* while a leak grows, and a soak
//!   watching only this would report a leak as an improvement.
//! - **Handles** are kernel objects. A capture that forgets a texture or a
//!   thread leaks these without leaking a byte of private memory, and #598's
//!   scratch directories were found the same way: by counting rather than by
//!   looking.
//!
//! # Why it samples this process
//!
//! Because the recording pipeline runs in it. `clipped_session::record_into`
//! captures, encodes and muxes on the caller's threads, so a soak that drives
//! it in-process is watching the thing it means to watch. A soak driving the
//! recorder as a child would measure the child, and would need the child to
//! report on itself.

#![cfg(windows)]

use core::fmt;

use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

/// What this process was holding at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    private_bytes: u64,
    working_set_bytes: u64,
    handles: u32,
}

impl Held {
    /// Reads the counters now.
    ///
    /// # Panics
    ///
    /// If Windows will not answer about the calling process, which is not a
    /// condition a soak should paper over: a run whose measurements silently
    /// became zero would report the flattest possible graph.
    #[must_use]
    pub fn now() -> Self {
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // closing and is valid for the life of the process.
        let process = unsafe { GetCurrentProcess() };

        let mut memory = PROCESS_MEMORY_COUNTERS_EX::default();
        let size = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>())
            .expect("the counters are far smaller than u32::MAX");
        // SAFETY: the pointer is to a live, correctly sized
        // `PROCESS_MEMORY_COUNTERS_EX`. The API is declared against the shorter
        // `PROCESS_MEMORY_COUNTERS` and reads the extended form when told its
        // size, which is what `cb` is for — this is the documented way to ask
        // for `PrivateUsage`.
        unsafe {
            GetProcessMemoryInfo(
                process,
                std::ptr::from_mut(&mut memory).cast::<PROCESS_MEMORY_COUNTERS>(),
                size,
            )
        }
        .expect("Windows answers about the calling process's memory");

        let mut handles = 0u32;
        // SAFETY: `handles` is a live `u32` the call writes into.
        unsafe { GetProcessHandleCount(process, &mut handles) }
            .expect("Windows answers about the calling process's handles");

        Self {
            private_bytes: memory.PrivateUsage as u64,
            working_set_bytes: memory.WorkingSetSize as u64,
            handles,
        }
    }

    /// Memory committed to this process alone.
    #[must_use]
    pub const fn private_bytes(&self) -> u64 {
        self.private_bytes
    }

    /// Memory resident now, which the operating system may trim.
    #[must_use]
    pub const fn working_set_bytes(&self) -> u64 {
        self.working_set_bytes
    }

    /// Kernel handles open.
    #[must_use]
    pub const fn handles(&self) -> u32 {
        self.handles
    }

    /// How much more this holds than `earlier`, in bytes and handles.
    ///
    /// Signed, because a process may hold *less* than it did — the allocator
    /// returns pages and the working set is trimmed — and a soak that could
    /// only report growth would have to call a shrink zero.
    #[must_use]
    pub const fn since(&self, earlier: &Self) -> Growth {
        Growth {
            private_bytes: self.private_bytes as i64 - earlier.private_bytes as i64,
            working_set_bytes: self.working_set_bytes as i64 - earlier.working_set_bytes as i64,
            handles: self.handles as i64 - earlier.handles as i64,
        }
    }
}

impl fmt::Display for Held {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "private {:.1} MB, working set {:.1} MB, {} handles",
            self.private_bytes as f64 / (1024.0 * 1024.0),
            self.working_set_bytes as f64 / (1024.0 * 1024.0),
            self.handles
        )
    }
}

/// The difference between two [`Held`] readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Growth {
    private_bytes: i64,
    working_set_bytes: i64,
    handles: i64,
}

impl Growth {
    /// Committed memory gained, or lost when negative.
    #[must_use]
    pub const fn private_bytes(&self) -> i64 {
        self.private_bytes
    }

    /// Resident memory gained, or lost when negative.
    #[must_use]
    pub const fn working_set_bytes(&self) -> i64 {
        self.working_set_bytes
    }

    /// Handles gained, or closed when negative.
    #[must_use]
    pub const fn handles(&self) -> i64 {
        self.handles
    }
}

impl fmt::Display for Growth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "private {:+.1} MB, working set {:+.1} MB, handles {:+}",
            self.private_bytes as f64 / (1024.0 * 1024.0),
            self.working_set_bytes as f64 / (1024.0 * 1024.0),
            self.handles
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Held;

    #[test]
    fn a_reading_is_not_empty() {
        let held = Held::now();

        assert!(
            held.private_bytes() > 0,
            "a running process has committed memory; a zero here is a reading that failed \
             quietly, which would make a soak report the flattest possible graph"
        );
        assert!(held.working_set_bytes() > 0, "and pages resident");
        assert!(held.handles() > 0, "and at least its own handles open");
    }

    /*
     * The direction matters more than the magnitude. A soak subtracts two
     * readings and decides whether the second is worse; a sign error there
     * turns a leak into a clean run.
     */
    #[test]
    fn growth_is_the_later_reading_minus_the_earlier_one() {
        let earlier = Held::now();
        // Sixty-four megabytes, written rather than merely reserved.
        //
        // A megabyte is not enough: the allocator serves it out of an arena it
        // already holds, private bytes do not move, and the case fails on a
        // healthy build — which is what it did the first time. This is far
        // larger than any arena a test process is sitting on, and `vec!` writes
        // every byte, so the pages are committed rather than promised.
        let ballast = vec![7u8; 64 * 1024 * 1024];
        let later = Held::now();

        let growth = later.since(&earlier);
        assert!(
            growth.private_bytes() > 0,
            "holding sixty-four megabytes should show as growth, and showed {growth}"
        );
        assert!(
            earlier.since(&later).private_bytes() < 0,
            "and the same pair the other way round should be negative"
        );

        drop(ballast);
    }

    #[test]
    fn a_reading_against_itself_has_not_grown() {
        let held = Held::now();
        let growth = held.since(&held);

        assert_eq!(growth.private_bytes(), 0);
        assert_eq!(growth.working_set_bytes(), 0);
        assert_eq!(growth.handles(), 0);
    }
}
