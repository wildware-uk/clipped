//! Where a buffer puts the segments it is not keeping in memory.
//!
//! A thirty-minute window of 1080p60 weighs about 4.2 GB
//! (`ReplayConfig::expected_bytes`), and the ceiling derived from it is 6.3 GB.
//! Holding that in memory is what
//! [issue #36](https://github.com/wildware-uk/clipped/issues/36) exists to stop,
//! and this is where the bytes go instead.
//!
//! # Where
//!
//! `%LOCALAPPDATA%\Clipped\replay\<owner>\`, one directory per buffer, beside
//! the rest of Clipped's per-user state
//! (`clipped_logging::application_directory`).
//!
//! The system drive rather than the recording's own is deliberate. A recording
//! directory is chosen for capacity — an external drive, a spinning disk kept
//! for the size of it — and a replay buffer spills continuously while the game
//! runs, so putting it there would put the slowest storage in the machine on the
//! path of something that has to keep up with an encoder.
//!
//! # Cleanup, and why a process id is not enough
//!
//! A crash leaves the files behind, so they are swept at start-up. The rule has
//! to survive **two Clipped processes running at once**, which this workspace
//! already does — `serve` alongside a `replay` subcommand — so "delete every
//! directory that is not mine" would have one recording delete another's buffer.
//!
//! Naming the directory after the process id is not enough either, because
//! Windows reuses them: a directory left by a dead process 4812 is
//! indistinguishable from a live process 4812's.
//!
//! So each directory holds a **lock file the owner keeps open exclusively** for
//! as long as its buffer lives. The sweep tries to open each one: it fails while
//! the owner is alive and succeeds once the owner has gone, whether it exited or
//! died, because that is what the operating system does with a handle when a
//! process ends. That is a rule that cannot delete a live buffer's files.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::segment::{Segment, SegmentId};

/// The directory, below the application's own, that every buffer spills into.
const SPILL_ROOT: &str = "replay";

/// The file whose being open says a buffer still owns its directory.
const LOCK_FILE: &str = "owner.lock";

/// Opens a file such that nobody else may open it at all while it is held.
///
/// The whole cleanup rule rests on this: a handle opened with no sharing is
/// released by Windows when the process ends however it ended, so a sweep that
/// can open the file knows the owner has gone.
fn open_exclusive(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(path)
}

/// One buffer's spill directory, owned for as long as the buffer is.
#[derive(Debug)]
pub struct SpillArea {
    directory: PathBuf,
    /// Held open, never read. Holding it is what stops another process's sweep
    /// taking this directory; releasing it is what lets one take it after a
    /// crash.
    ///
    /// An [`Option`] so that [`Drop`] can release it *before* removing the
    /// directory: Windows will not remove a directory that still holds an open
    /// file, so the obvious order leaves everything behind.
    lock: Option<File>,
}

impl SpillArea {
    /// Creates a directory of this buffer's own below `root`.
    ///
    /// The name carries the process id so that somebody looking at the
    /// directory can tell whose it is; the lock file is what makes the
    /// ownership real.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem reports. A buffer that cannot make one simply
    /// does not spill (`crate::buffer`), because a replay buffer may never cost
    /// a recording anything (AGENTS.md section 17).
    pub fn create(root: &Path, owner: u32, ordinal: u64) -> io::Result<Self> {
        let directory = root.join(format!("{owner}-{ordinal}"));
        fs::create_dir_all(&directory)?;
        let lock = open_exclusive(&directory.join(LOCK_FILE), true)?;
        Ok(Self {
            directory,
            lock: Some(lock),
        })
    }

    /// The default root: `%LOCALAPPDATA%\Clipped\replay`.
    ///
    /// [`None`] when the environment describes no per-user directory, which is
    /// also a machine where nothing should be written.
    #[must_use]
    pub fn default_root() -> Option<PathBuf> {
        clipped_logging::application_directory().map(|directory| directory.join(SPILL_ROOT))
    }

    /// The directory this area owns.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Where a segment's file is.
    fn path_for(&self, id: SegmentId) -> PathBuf {
        self.directory.join(format!("{id}.segment"))
    }

    /// Writes `segment` out, returning how many bytes it took on disk.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem reports — for a spill file, the disk filling or
    /// the drive going away.
    pub(crate) fn write(&self, segment: &Segment) -> io::Result<u64> {
        let path = self.path_for(segment.id());
        let mut file = File::create(&path)?;
        // Buffered, because a segment is thousands of small writes otherwise:
        // one per packet index entry.
        let mut writer = io::BufWriter::new(&mut file);
        segment.write_to(&mut writer)?;
        io::Write::flush(&mut writer)?;
        drop(writer);
        file.metadata().map(|data| data.len())
    }

    /// Reads a segment back.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem reports, and
    /// [`io::ErrorKind::InvalidData`](std::io::ErrorKind::InvalidData) for a
    /// file that is not one of these.
    pub(crate) fn read(&self, id: SegmentId) -> io::Result<Segment> {
        let file = File::open(self.path_for(id))?;
        let mut reader = io::BufReader::new(file);
        Segment::read_from(id, &mut reader)
    }

    /// Removes a segment's file, if it is still there.
    pub(crate) fn remove(&self, id: SegmentId) {
        let _ = fs::remove_file(self.path_for(id));
    }
}

impl Drop for SpillArea {
    fn drop(&mut self) {
        // The lock first, because Windows will not remove a directory holding an
        // open file — with the obvious order this leaves every byte behind, and
        // the only thing that notices is a disk filling up over weeks.
        //
        // Between the two there is a moment when another process's sweep could
        // take this directory instead. That is harmless: it removes exactly what
        // this was about to.
        drop(self.lock.take());
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// One segment's file, removed when the last thing holding it lets go.
///
/// The spilled counterpart of the [`Arc<Segment>`](std::sync::Arc) a resident
/// segment is held by, and it exists for exactly the same reason: a lease has to
/// be able to keep material alive after the buffer has evicted it. Cloning this
/// under the buffer's lock is what pins a spilled segment, and reading it
/// afterwards is what keeps disk out of the locked region
/// (`docs/replay-buffer.md` measures a lease at 0.77 ms, and that is why a save
/// does not disturb a recording).
#[derive(Debug)]
pub(crate) struct SpilledSegment {
    id: SegmentId,
    area: Arc<SpillArea>,
    /// What the file occupies, for reporting what the buffer is keeping where.
    disk_bytes: u64,
}

impl SpilledSegment {
    pub(crate) fn new(id: SegmentId, area: Arc<SpillArea>, disk_bytes: u64) -> Self {
        Self {
            id,
            area,
            disk_bytes,
        }
    }

    pub(crate) const fn id(&self) -> SegmentId {
        self.id
    }

    pub(crate) const fn disk_bytes(&self) -> u64 {
        self.disk_bytes
    }

    /// Reads it back into memory.
    ///
    /// # Errors
    ///
    /// Whatever the filesystem reports.
    pub(crate) fn load(&self) -> io::Result<Segment> {
        self.area.read(self.id)
    }
}

impl Drop for SpilledSegment {
    fn drop(&mut self) {
        // Failure is ignored on purpose: the file is this buffer's own, the
        // directory goes when the buffer does, and a recording must not be
        // disturbed because a temporary file would not delete.
        self.area.remove(self.id);
    }
}

/// Removes spill directories left behind by processes that are no longer
/// running.
///
/// Returns how many were removed. Safe to call while other Clipped processes are
/// recording: a directory whose owner is alive cannot be opened, so it is left
/// alone.
///
/// Called once at start-up. It never fails: a sweep that cannot read the root
/// has nothing to do, and one that cannot remove a directory will meet it again
/// next time.
pub fn sweep(root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let directory = entry.path();
        if !directory.is_dir() {
            continue;
        }
        let lock = directory.join(LOCK_FILE);
        // No lock file at all is a directory this build did not write, or one a
        // previous sweep half-removed. Either way nothing owns it.
        if lock.exists() && open_exclusive(&lock, false).is_err() {
            continue;
        }
        if fs::remove_dir_all(&directory).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    use clipped_encoder::{EncodedPacket, PictureKind};

    /// A directory of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "clipped-spill-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("a scratch directory can be made");
        directory
    }

    fn a_segment(id: u64) -> Segment {
        let data = [7_u8; 256];
        let packet = EncodedPacket::new(
            &data,
            Duration::from_millis(id * 100),
            Duration::from_millis(id * 100),
            PictureKind::Keyframe,
        );
        crate::segment::OpenSegment::open(SegmentId::for_test(id), &packet, 512).seal()
    }

    #[test]
    fn a_segment_written_to_the_area_reads_back_the_same() {
        let root = scratch("round-trip");
        let area = SpillArea::create(&root, std::process::id(), 0).expect("an area can be made");

        let written = a_segment(3);
        let bytes = area.write(&written).expect("a segment can be spilled");
        assert!(bytes > 256, "the file holds the payload and its index");

        let read = area.read(written.id()).expect("and read back");
        assert_eq!(read.byte_len(), written.byte_len());
        assert_eq!(read.start(), written.start());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dropping_an_area_takes_its_directory_with_it() {
        // What stops an ordinary exit leaving gigabytes behind, and what makes
        // the sweep below the exception rather than the rule.
        let root = scratch("drop");
        let area = SpillArea::create(&root, std::process::id(), 1).expect("an area can be made");
        let directory = area.directory().to_path_buf();
        area.write(&a_segment(1)).expect("a segment can be spilled");
        assert!(directory.is_dir());

        drop(area);
        assert!(!directory.exists(), "the directory goes with the buffer");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_last_holder_of_a_spilled_segment_removes_its_file() {
        // What lets a lease keep reading a segment the buffer has evicted, and
        // what stops the files outliving the need for them.
        let root = scratch("refcount");
        let area = Arc::new(SpillArea::create(&root, std::process::id(), 4).expect("an area"));
        let segment = a_segment(9);
        let bytes = area.write(&segment).expect("a segment can be spilled");
        let path = area.path_for(segment.id());

        let held = Arc::new(SpilledSegment::new(segment.id(), Arc::clone(&area), bytes));
        let also_held = Arc::clone(&held);
        assert!(path.is_file());

        drop(held);
        assert!(path.is_file(), "a second holder keeps the file alive");
        assert_eq!(
            also_held.load().expect("and can still read it").byte_len(),
            segment.byte_len()
        );

        drop(also_held);
        assert!(!path.exists(), "the last holder takes the file with it");

        drop(area);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_sweep_leaves_a_live_buffers_directory_alone_and_takes_a_dead_ones() {
        // The rule that has to hold while two Clipped processes are recording.
        // The live half is this test's own area, which is genuinely locked; the
        // dead half is a directory with a lock file nobody holds, which is what
        // a crash leaves behind.
        let root = scratch("sweep");
        let live = SpillArea::create(&root, std::process::id(), 2).expect("an area can be made");
        live.write(&a_segment(1)).expect("a segment can be spilled");

        let orphan = root.join("999999-0");
        fs::create_dir_all(&orphan).expect("an orphan can be made");
        fs::write(orphan.join(LOCK_FILE), b"").expect("with a lock nobody holds");
        fs::write(orphan.join("segment-0001.segment"), b"stale").expect("and a segment");

        let removed = sweep(&root);

        assert_eq!(removed, 1, "exactly the orphan");
        assert!(!orphan.exists(), "the crashed buffer's files are gone");
        assert!(
            live.directory().is_dir(),
            "a running buffer's files must survive a sweep by another process"
        );

        drop(live);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sweeping_somewhere_that_does_not_exist_is_not_a_failure() {
        // Start-up runs this before anything has ever spilled.
        assert_eq!(sweep(&scratch("empty").join("never-made")), 0);
    }
}
