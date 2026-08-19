//! How much room is left where a recording is being written.
//!
//! A disk filling up is the most likely way a long recording ends badly, and it
//! is the one failure where doing nothing is actively destructive: the writes
//! start failing, and then the *trailer* write fails as well, so the recording
//! ends without the segment length, duration and cue index that make it
//! seekable. AGENTS.md section 17 puts the recording above almost everything
//! else, so this crate does not wait to be told the disk is full — it stops the
//! recording while there is still room to finish the file properly.
//!
//! # The two questions, and where each is asked
//!
//! ```text
//! before the file is created   is there enough room to start?   crate::recording::open_output
//! while it is being written    is there still enough room?      crate::muxing, on the writer thread
//! ```
//!
//! The second is deliberately not asked on the capture thread. Reading a
//! volume's free space is a filesystem call, and AGENTS.md section 20 forbids
//! the capture thread from making one; the thread that already owns the file
//! makes it instead, at most once every [`PROBE_INTERVAL`], and publishes the
//! answer as one relaxed atomic the capture loop reads between frames.
//!
//! # The policy
//!
//! [`judge`] is the whole of it, and it is a pure function of two numbers so
//! that every threshold below is tested without a disk, on any platform
//! (AGENTS.md section 25). Only [`free_space`] needs Windows, and it no longer
//! makes the call itself: `clipped_windows::volume_free_space` does, for this
//! crate and for `clipped_library::accounting`, which needs the same two
//! numbers to judge an entirely different thing (issue #277). The recorder can
//! therefore ask how full a disk is without the media library and the SQLite
//! index behind it (ADR 0002).

use core::fmt;
use core::time::Duration;
use std::path::{Path, PathBuf};

/// How much of the drive a recording refuses to consume.
///
/// A recording is stopped, cleanly, when the volume it is being written to has
/// this much left. It is not an estimate of what the file still needs: it is
/// the margin that keeps the *finalisation* — the trailer, the cue index, and
/// whatever the filesystem itself wants for its metadata — from being the write
/// that fails.
///
/// A gibibyte is about four minutes of 1080p60 at the bit rate a recording is
/// given, which is long enough for somebody who is told about it mid-game to
/// finish what they are doing, and small enough that it does not make a
/// half-full drive unusable. It is a default and not a law:
/// [`crate::RecordingSettings::with_minimum_free_space`] moves it, and zero
/// turns the guard off for a caller that would rather fill the disk.
pub const DEFAULT_MINIMUM_FREE_SPACE: u64 = 1 << 30;

/// How many times the floor still counts as "getting low".
///
/// Below four times the floor — a quarter of an hour at 1080p60, by the same
/// arithmetic — the recording says so once and carries on. A warning that
/// arrived at the same moment as the stop would be no warning at all.
const WARNING_MULTIPLE: u64 = 4;

/// How often the writer thread asks the volume how much is left.
///
/// `GetDiskFreeSpaceExW` is a cheap call, but it is still a syscall on the
/// thread that has to keep up with the encoder, and the answer cannot change
/// faster than the recording writes. Two seconds costs one call per hundred and
/// twenty frames at 60 fps and bounds how much is written past the floor before
/// the recording notices, which at 33 Mbit/s is about eight megabytes.
pub(crate) const PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// How large a volume is, and how much of it is free.
///
/// The platform layer's type rather than one of this crate's, because it is
/// the platform layer's answer: two numbers Windows reports, with no policy of
/// this crate's in them. A `VolumeSpace` of this crate's own would be a second
/// name for the same pair (AGENTS.md section 55), and the clamp its
/// constructor applies — free space above the total, which a per-user quota
/// can produce — is a fact about the API rather than about recording.
pub use clipped_windows::VolumeSpace;

/// What the free space on a volume means for a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceVerdict {
    /// There is plenty. Nothing to say.
    Ample,
    /// The drive is filling. The recording carries on and the user is told
    /// once, while there is still time to do something about it.
    Low,
    /// The recording must stop now, so that the file can be finished properly
    /// rather than truncated by a failing write.
    Exhausted,
}

/// What `free_bytes` means for a recording holding itself to `minimum`.
///
/// `minimum` of zero turns the guard off: the caller has said it would rather
/// fill the disk than lose the tail of the recording, and a guard that ignored
/// that would be a setting that silently does nothing (AGENTS.md section 27).
#[must_use]
pub fn judge(free_bytes: u64, minimum: u64) -> SpaceVerdict {
    if minimum == 0 {
        return SpaceVerdict::Ample;
    }
    if free_bytes <= minimum {
        return SpaceVerdict::Exhausted;
    }
    if free_bytes <= minimum.saturating_mul(WARNING_MULTIPLE) {
        return SpaceVerdict::Low;
    }
    SpaceVerdict::Ample
}

/// The volume holding `path` could not be asked how much room it has.
///
/// Mid-recording this is what an unplugged drive looks like: the path stops
/// resolving to anything and every ancestor of it fails too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeUnreadable {
    /// The path that was asked about.
    pub path: PathBuf,
    /// What the operating system said.
    pub reason: String,
}

impl fmt::Display for VolumeUnreadable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} could not be read: {}",
            clipped_logging::RedactedPath::new(&self.path),
            self.reason
        )
    }
}

impl std::error::Error for VolumeUnreadable {}

/// Reads the size and free space of the volume holding `path`.
///
/// `path` does not have to exist. A recordings directory that has not been
/// created yet is an ordinary first-run state, so the nearest ancestor that
/// does exist is asked instead — the drive is what the question is really
/// about.
///
/// The walk itself is `clipped_windows::volume_free_space`'s, not this crate's:
/// storage accounting needs the same rule for the same reason, and issue #277
/// left one copy of it at the bottom of the stack. What stays here is what the
/// recording does about the answer.
///
/// # Errors
///
/// [`VolumeUnreadable`] when nothing along the path could be read, which is
/// what a disconnected drive looks like.
#[cfg(windows)]
pub fn free_space(path: &Path) -> Result<VolumeSpace, VolumeUnreadable> {
    clipped_windows::volume_free_space(path).map_err(|error| VolumeUnreadable {
        path: error.path,
        reason: error.reason,
    })
}

/// Recording is a Windows feature; this build has no way to ask.
///
/// # Errors
///
/// Always [`VolumeUnreadable`].
#[cfg(not(windows))]
pub fn free_space(path: &Path) -> Result<VolumeSpace, VolumeUnreadable> {
    Err(VolumeUnreadable {
        path: path.to_path_buf(),
        reason: "free space can only be read on Windows in this build".to_owned(),
    })
}

/// Bytes as a person reads them, for a message rather than for arithmetic.
///
/// Binary units, because that is what Windows shows for a drive, and one
/// decimal place, because "1.0 GiB free" is the whole of what somebody needs to
/// know and "1073741824 bytes free" is not (AGENTS.md section 45).
#[must_use]
pub fn describe_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ];

    for (unit, scale) in UNITS {
        if bytes >= scale {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a figure printed to one decimal place; the error is far below the \
                          precision shown"
            )]
            let value = bytes as f64 / scale as f64;
            return format!("{value:.1} {unit}");
        }
    }
    format!("{bytes} bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn a_drive_with_room_to_spare_is_not_worth_mentioning() {
        assert_eq!(
            judge(500 * GIB, DEFAULT_MINIMUM_FREE_SPACE),
            SpaceVerdict::Ample
        );
    }

    #[test]
    fn a_drive_filling_towards_the_floor_is_reported_before_it_is_reached() {
        // The point of the warning is that it arrives while somebody can still
        // act on it. Four gibibytes above a one-gibibyte floor is about a
        // quarter of an hour of 1080p60.
        assert_eq!(judge(3 * GIB, GIB), SpaceVerdict::Low);
        assert_eq!(judge(4 * GIB, GIB), SpaceVerdict::Low);
        assert_eq!(
            judge(4 * GIB + 1, GIB),
            SpaceVerdict::Ample,
            "just above the warning band is not a warning"
        );
    }

    #[test]
    fn reaching_the_floor_stops_the_recording_rather_than_waiting_for_a_failed_write() {
        // This is the whole reason the guard exists: the recording is finished
        // while there is still room for the trailer, instead of the trailer
        // being the write that fails.
        assert_eq!(judge(GIB, GIB), SpaceVerdict::Exhausted);
        assert_eq!(judge(GIB - 1, GIB), SpaceVerdict::Exhausted);
        assert_eq!(judge(0, GIB), SpaceVerdict::Exhausted);
    }

    #[test]
    fn a_floor_of_zero_turns_the_guard_off_rather_than_stopping_at_once() {
        // Zero is how a caller says "fill the disk". A guard that read it as
        // "stop immediately" would make the setting a trap, and one that read
        // it as the default would be a setting that silently does nothing.
        assert_eq!(judge(0, 0), SpaceVerdict::Ample);
        assert_eq!(judge(1, 0), SpaceVerdict::Ample);
    }

    #[test]
    fn an_enormous_floor_does_not_overflow_the_warning_band() {
        // `minimum * 4` is the warning band, and a caller that asked for a
        // floor near `u64::MAX` would wrap it into a small number — turning
        // "warn early" into "never warn".
        assert_eq!(judge(u64::MAX, u64::MAX), SpaceVerdict::Exhausted);
        assert_eq!(judge(u64::MAX, u64::MAX / 2), SpaceVerdict::Low);
    }

    #[test]
    fn a_size_is_described_in_the_units_a_drive_is_described_in() {
        assert_eq!(describe_bytes(0), "0 bytes");
        assert_eq!(describe_bytes(2048), "2.0 KiB");
        assert_eq!(describe_bytes(GIB + GIB / 2), "1.5 GiB");
        assert_eq!(describe_bytes(3 * (1 << 40)), "3.0 TiB");
    }

    #[cfg(windows)]
    #[test]
    fn the_volume_holding_a_real_directory_answers_with_its_size() {
        let space = free_space(&std::env::temp_dir()).expect("the temporary directory exists");
        assert!(space.total_bytes() > 0, "no volume is of size zero");
        assert!(space.free_bytes() <= space.total_bytes());
    }

    #[cfg(windows)]
    #[test]
    fn a_directory_that_does_not_exist_yet_is_answered_by_the_drive_above_it() {
        // The first-run state: the recordings directory has not been created.
        // Refusing to answer would make the pre-flight check useless on exactly
        // the run where a full disk is most surprising.
        let missing = std::env::temp_dir()
            .join("clipped-session-disk-103")
            .join("not-created-yet");
        assert!(
            !missing.exists(),
            "this test needs a path that is not there"
        );

        let space = free_space(&missing).expect("the drive above it exists");
        assert!(space.total_bytes() > 0);
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_that_is_not_there_is_refused_with_what_windows_said() {
        // What an unplugged output drive looks like: no ancestor of the path
        // resolves, so there is nothing to fall back to.
        let gone = Path::new(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\clips");

        let error = free_space(gone).expect_err("that volume does not exist");
        assert!(!error.reason.is_empty(), "Windows always says something");
    }
}
