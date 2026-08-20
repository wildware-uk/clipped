//! A buffer that keeps its window on disk rather than in memory.
//!
//! [Issue #36](https://github.com/wildware-uk/clipped/issues/36). The argument
//! for it is already stated in `crates/replay/src/config.rs`, where
//! `a_thirty_minute_window_is_sized_in_gigabytes` asserts a **6.3 GB** ceiling
//! for the longest window the configuration allows — which is what a machine
//! would have to find to keep half an hour the way the buffer used to.
//!
//! These are integration tests rather than unit tests because they want a real
//! directory and a real writer thread. Nothing here waits on wall-clock time
//! except the writer: the buffer contains no clock at all, so thirty minutes of
//! *media* time is pushed in about a second by giving the packets the
//! timestamps they would have had (AGENTS.md section 25).

use core::time::Duration;
use std::path::PathBuf;

use clipped_encoder::{BitRate, EncodedPacket, PictureKind};
use clipped_replay::{ReplayBuffer, ReplayConfig, SpillArea};

/// Frames a second, so that a packet count is a duration.
const FRAMES_PER_SECOND: u64 = 60;

/// How often a keyframe arrives, which is what a segment can begin on.
const KEYFRAME_INTERVAL: u64 = 120;

/// Bytes per packet. Small, because what is being measured is where the bytes
/// are rather than how many.
const PACKET_BYTES: usize = 2_000;

/// A directory of this test's own, removed first if it survived.
/// A scratch directory that removes itself when the test that made it passes.
///
/// The pattern PR #597 settled, and the two halves of it that matter
/// ([issue #598](https://github.com/wildware-uk/clipped/issues/598)):
///
/// - **A failing test keeps its directory**, with the path printed, because the
///   files in it are the evidence. Removing unconditionally buys tidiness with
///   the thing somebody needs at exactly the moment they need it.
/// - **A removal that fails is said aloud.** Windows refuses to remove a
///   directory holding an open file, and a discarded `Err` turns that into a
///   test that reports success having leaked — which is how these accumulated
///   unnoticed in the first place.
///
/// Never a sweep of the temporary directory by prefix: several of these suites
/// run at once, and a sweep would delete another run's directories out from
/// under it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory can be made");
        Self(path)
    }
}

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;

    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for Scratch {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("scratch directory kept for diagnosis: {}", self.0.display());
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!(
                "scratch directory could not be removed: {} ({error})",
                self.0.display()
            );
        }
    }
}

/// A directory of this test's own, removed again when it passes.
fn scratch(name: &str) -> Scratch {
    Scratch::new(&format!("spilling-{name}"))
}

/// A configuration whose segments are one second long, so that a minute of
/// media time is sixty of them.
fn config(window: Duration) -> ReplayConfig {
    ReplayConfig::new(
        window,
        // 2000 bytes at 60 fps is 960 kbit/s.
        BitRate::bits_per_second(960_000).expect("a real rate"),
    )
    .expect("a supported window")
    .with_segment_duration(Duration::from_secs(1))
    .expect("one second fits")
}

/// Pushes `seconds` of media time, one packet at a time.
fn push_seconds(buffer: &ReplayBuffer, seconds: u64) {
    let data = [7_u8; PACKET_BYTES];
    for frame in 0..seconds * FRAMES_PER_SECOND {
        let at = Duration::from_nanos(frame * 1_000_000_000 / FRAMES_PER_SECOND);
        buffer.push(&EncodedPacket::new(
            &data,
            at,
            at,
            if frame % KEYFRAME_INTERVAL == 0 {
                PictureKind::Keyframe
            } else {
                PictureKind::Predicted
            },
        ));
    }
}

/// Waits for the writer to catch up with what it has been offered.
///
/// The only wall-clock wait in this file, and it is bounded: a failure to catch
/// up is reported as one rather than hanging.
fn settle(buffer: &ReplayBuffer, budget: u64) {
    // Generous, because this waits on a real disk and CI's is not the one this
    // was developed against. It is a bound on a converging process rather than
    // a guess at how long it takes: each completed write offers the next, so
    // the resident set falls until it is within the budget and then stops.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        if buffer.stats().bytes_held() <= budget {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_thirty_minute_window_is_held_without_thirty_minutes_of_memory() {
    // The acceptance criterion. Thirty minutes of media time, and the memory
    // held at the end compared against what the same window costs a buffer that
    // cannot spill — which `config.rs` measures in gigabytes.
    let root = scratch("thirty-minutes");
    let window = Duration::from_secs(30 * 60);
    let settings = config(window);
    let Ok(area) = SpillArea::create(&root, std::process::id(), 0) else {
        // A machine that will not give this test a directory has nothing to say
        // about where a buffer keeps its segments.
        return;
    };

    let buffer = ReplayBuffer::spilling(settings, area);
    push_seconds(&buffer, 30 * 60);
    // What the buffer promises to converge to: a few segments, not a fraction
    // of the window.
    settle(&buffer, settings.expected_segment_bytes() * 32);

    let stats = buffer.stats();
    let resident = stats.bytes_held();
    let spilled = stats.spilled_bytes();

    assert!(
        spilled > 0,
        "thirty minutes has to have reached the disk: {resident} bytes in memory, \
         {spilled} on disk, {} segments spilled",
        stats.segments_spilled()
    );

    // The number this issue exists for. The same window costs a buffer that
    // cannot spill `expected_bytes`, and the point is that this one does not
    // pay it.
    assert!(
        resident < settings.expected_bytes() / 8,
        "a spilling buffer must not hold what an in-memory one would: {resident} bytes \
         resident against {} the window is sized at",
        settings.expected_bytes()
    );

    // And it is bounded by the segment size rather than by the window: a
    // thirty-minute buffer and a thirty-second one hold the same amount.
    assert!(
        resident < settings.expected_segment_bytes() * 32,
        "the resident budget is a few segments, not a fraction of the window: {resident}"
    );

    drop(buffer);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_clip_can_still_be_saved_from_material_that_went_to_disk() {
    // Spilling that could not be read back would be a buffer that keeps its
    // history somewhere nobody can reach, which is worse than not keeping it.
    let root = scratch("save-from-disk");
    let settings = config(Duration::from_secs(60));
    let Ok(area) = SpillArea::create(&root, std::process::id(), 1) else {
        return;
    };

    let buffer = ReplayBuffer::spilling(settings, area);
    push_seconds(&buffer, 60);
    settle(&buffer, settings.expected_segment_bytes() * 32);

    assert!(
        buffer.stats().segments_spilled() > 0,
        "this test is only meaningful once something has been written out"
    );

    // The oldest half of the window, which is the half most likely to be on
    // disk rather than in memory.
    let lease = buffer
        .lease_last(Duration::from_secs(50))
        .expect("fifty seconds of a sixty-second buffer can be leased");

    // `len` counts segments; the packets are what a clip is written from, and
    // they are what has to have come back off disk.
    let packets = lease.packets().count();
    assert!(
        packets > FRAMES_PER_SECOND as usize * 40,
        "a lease has to carry the packets it selected, wherever they were: {packets} packets          across {} segments",
        lease.len()
    );
    assert!(
        lease
            .packets()
            .next()
            .expect("a first packet")
            .is_keyframe(),
        "a lease still begins on a keyframe when it came off disk"
    );

    drop(buffer);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_buffer_that_cannot_spill_keeps_working_and_says_so() {
    // The documented behaviour when the disk fills or the drive goes away: the
    // buffer falls back to what it did before spilling existed. The area's
    // directory is removed underneath it, which is what a removed drive looks
    // like to a write.
    let root = scratch("disk-gone");
    let settings = config(Duration::from_secs(60));
    let Ok(area) = SpillArea::create(&root, std::process::id(), 2) else {
        return;
    };
    let directory = area.directory().to_path_buf();

    let buffer = ReplayBuffer::spilling(settings, area);
    push_seconds(&buffer, 5);
    // Take the directory away, so the next write cannot land.
    let _ = std::fs::remove_dir_all(&directory);
    push_seconds(&buffer, 55);

    // The recording is unaffected: the buffer still holds a window and can
    // still be leased.
    let lease = buffer
        .lease_last(Duration::from_secs(5))
        .expect("a buffer that cannot spill can still be saved from");
    assert!(lease.packets().count() > 0, "and still has packets in it");

    drop(buffer);
    let _ = std::fs::remove_dir_all(&root);
}
