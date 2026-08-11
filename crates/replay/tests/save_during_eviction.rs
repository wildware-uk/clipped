//! Races a save against the eviction that is trying to delete what it is
//! reading.
//!
//! This is the acceptance criterion "eviction never drops a segment currently
//! being read for a save", and it is the one most easily satisfied by a comment
//! rather than by code. A single-threaded test that leases and then evicts
//! proves nothing about the case that happens in the field: the encoder keeps
//! producing, the buffer keeps evicting, and the save is reading throughout,
//! all on different threads.
//!
//! So these tests run a writer thread that fills and turns over the whole
//! buffer many times over while a reader thread walks every byte of a lease and
//! checks it against what was pushed. Each packet carries its own frame number
//! in its first eight bytes, so a segment that was recycled, truncated or
//! replaced is caught by the content rather than by a count.
//!
//! The first also checks it is not vacuous: it reads until the buffer has
//! thrown away every segment the lease holds *and* turned the whole window over
//! at least once, so a run where the reader simply finished first cannot pass.

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::sync::{Arc, Barrier};
use std::thread;

use clipped_encoder::{BitRate, EncodedPacket, PictureKind};
use clipped_replay::{ReplayBuffer, ReplayConfig, SegmentLease};

/// Frames a second of media time. Low, so that a second of video is ten
/// packets and a test can turn a thirty-second buffer over in milliseconds.
const RATE: u64 = 10;

/// Bytes in one packet.
const PACKET: usize = 512;

/// How long a segment is, in frames.
const SEGMENT_FRAMES: u64 = RATE;

/// A buffer of `window_seconds` in one-second segments, sized for the bitrate
/// the fixture actually produces so that the ceiling does not bite.
fn buffer(window_seconds: u64) -> Arc<ReplayBuffer> {
    let bits = RATE * PACKET as u64 * 8;
    let config = ReplayConfig::new(
        Duration::from_secs(window_seconds),
        BitRate::bits_per_second(u32::try_from(bits).expect("a small rate")).expect("a real rate"),
    )
    .expect("a supported window")
    .with_segment_duration(Duration::from_secs(1))
    .expect("one second fits in any supported window");

    Arc::new(ReplayBuffer::new(config))
}

/// The packet for `frame`: its number in the first eight bytes, then a fill
/// derived from it.
///
/// Both halves matter. The number identifies which frame a byte came from, so a
/// lease reading recycled memory is caught rather than merely producing the
/// wrong length; the fill means a partially overwritten packet is caught too.
fn payload(frame: u64) -> Vec<u8> {
    let mut bytes = vec![(frame % 251) as u8; PACKET];
    bytes[..8].copy_from_slice(&frame.to_le_bytes());
    bytes
}

fn presentation(frame: u64) -> Duration {
    Duration::from_millis(frame * 1000 / RATE)
}

/// Pushes `frame` into `buffer`, as the encoding thread would.
fn push(buffer: &ReplayBuffer, frame: u64) {
    let bytes = payload(frame);
    let at = presentation(frame);
    buffer.push(&EncodedPacket::new(
        &bytes,
        at,
        at,
        if frame % SEGMENT_FRAMES == 0 {
            PictureKind::Keyframe
        } else {
            PictureKind::Predicted
        },
    ));
}

/// Checks every packet of a lease against what was pushed, and returns how many
/// there were.
///
/// `pause` is a yield between segments, so that the writer thread genuinely
/// overtakes the reader rather than the reader finishing first on a fast
/// machine.
fn verify(lease: &SegmentLease, pause: bool) -> usize {
    let mut packets = 0;

    for segment in lease.segments() {
        assert!(
            segment
                .packets()
                .next()
                .expect("a segment always holds its keyframe")
                .is_keyframe(),
            "{} does not begin on a keyframe",
            segment.id()
        );

        for packet in segment.packets() {
            let data = packet.data();
            assert_eq!(data.len(), PACKET, "a packet changed length");

            let frame = u64::from_le_bytes(data[..8].try_into().expect("eight bytes"));
            assert_eq!(
                data,
                payload(frame).as_slice(),
                "frame {frame} came back with bytes that were not pushed for it"
            );
            assert_eq!(
                packet.presentation_time(),
                presentation(frame),
                "frame {frame} came back with another frame's timestamp"
            );
            packets += 1;
        }

        if pause {
            thread::yield_now();
        }
    }

    packets
}

/// Runs a writer thread pushing frames until it is told to stop, and returns
/// the handle and the flag that stops it.
fn writer(buffer: &Arc<ReplayBuffer>, from: u64, start: &Arc<Barrier>) -> Writer {
    let stop = Arc::new(AtomicBool::new(false));

    let handle = {
        let buffer = Arc::clone(buffer);
        let stop = Arc::clone(&stop);
        let start = Arc::clone(start);

        thread::Builder::new()
            .name("replay-writer".to_owned())
            .spawn(move || {
                start.wait();
                let mut frame = from;
                while !stop.load(Ordering::Relaxed) {
                    push(&buffer, frame);
                    frame += 1;
                }
            })
            .expect("a test can start a thread")
    };

    Writer { handle, stop }
}

/// The thread standing in for the encoding thread of a live recording.
struct Writer {
    handle: thread::JoinHandle<()>,
    stop: Arc<AtomicBool>,
}

impl Writer {
    fn finish(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().expect("the writer thread panicked");
    }
}

#[test]
fn a_save_reads_every_byte_it_leased_while_eviction_deletes_it() {
    // Thirty seconds in one-second segments, so the whole buffer turns over
    // every three hundred pushes and the writer below turns it over many times
    // while the reader is walking the lease.
    let buffer = buffer(30);
    for frame in 0..600 {
        push(&buffer, frame);
    }

    let lease = buffer
        .lease_last(Duration::from_secs(10))
        .expect("ten seconds are held");
    let leased: Vec<_> = lease.segments().map(clipped_replay::Segment::id).collect();
    let expected_packets = lease.packets().count();
    assert!(expected_packets >= 100, "leased only {expected_packets}");

    let evicted_before = buffer.stats().segments_evicted_for_window();

    let start = Arc::new(Barrier::new(2));
    let writer = writer(&buffer, 600, &start);
    start.wait();

    // Read the lease over and over while the writer evicts underneath it. Each
    // pass checks every byte against the frame it claims to be.
    //
    // Bounded rather than `loop`: a buffer that stopped evicting would satisfy
    // the exit condition never, and a test that hangs is a worse failure than a
    // test that fails — it is indistinguishable from a slow machine, and in CI
    // it costs the whole job's timeout rather than one red line. The bound is
    // enormous compared with the few passes this needs.
    let mut passes = 0;
    while !(buffer.held().expect("the writer is still pushing").start() > lease.covered().end()
        && buffer.stats().segments_evicted_for_window() - evicted_before > 30)
    {
        assert_eq!(verify(&lease, true), expected_packets);
        passes += 1;
        assert!(
            passes < 100_000,
            "the buffer never evicted past the lease, so the race this test exists for did not \
             happen: {:?}",
            buffer.stats()
        );
    }

    let during = buffer.stats();
    // Every segment in the lease but one: the newest is the copy the lease took
    // of the segment that was still being written, which the buffer never owned
    // and so never had to retain.
    assert_eq!(
        during.segments_retained_for_a_save(),
        leased.len() - 1,
        "the buffer should be holding the leased segments open, and is holding {}",
        during.segments_retained_for_a_save()
    );
    assert!(
        during.segments_evicted_for_window() - evicted_before > 30,
        "eviction never turned the whole window over while the reader was in \
         it, so this proved nothing: {during:?}"
    );

    // One last full read after every segment in it has been evicted.
    assert_eq!(verify(&lease, false), expected_packets);
    assert_eq!(
        lease
            .segments()
            .map(clipped_replay::Segment::id)
            .collect::<Vec<_>>(),
        leased,
        "the lease is holding different segments from the ones it was given"
    );

    writer.finish();

    // And the memory comes back once the reader has finished with it.
    drop(lease);
    for frame in 100_000..100_040 {
        push(&buffer, frame);
    }
    assert_eq!(buffer.stats().segments_retained_for_a_save(), 0);
}

#[test]
fn several_saves_reading_at_once_all_see_their_own_segments() {
    // Two saves in quick succession is an ordinary thing to do with a hotkey,
    // and the second must not disturb the first.
    let buffer = buffer(30);
    for frame in 0..600 {
        push(&buffer, frame);
    }

    let start = Arc::new(Barrier::new(5));
    let writer = writer(&buffer, 600, &start);

    let readers: Vec<_> = (0..3)
        .map(|reader| {
            let buffer = Arc::clone(&buffer);
            let start = Arc::clone(&start);
            thread::Builder::new()
                .name(format!("replay-reader-{reader}"))
                .spawn(move || {
                    start.wait();
                    let lease = buffer
                        .lease_last(Duration::from_secs(5))
                        .expect("five seconds are held");
                    let expected = lease.packets().count();
                    for _ in 0..20 {
                        assert_eq!(verify(&lease, true), expected);
                    }
                    expected
                })
                .expect("a test can start a thread")
        })
        .collect();

    start.wait();
    for reader in readers {
        let packets = reader.join().expect("a reader thread panicked");
        assert!(packets >= 50, "a reader leased only {packets} packets");
    }

    writer.finish();

    // Every lease has been dropped, so nothing should still be retained once
    // the buffer next evicts.
    for frame in 200_000..200_040 {
        push(&buffer, frame);
    }
    let stats = buffer.stats();
    assert_eq!(stats.segments_retained_for_a_save(), 0, "{stats:?}");
    assert!(stats.leases_taken() >= 3);
}

#[test]
fn a_long_session_stays_within_its_ceiling() {
    // AGENTS.md section 59: the recorder may be active for days. Thirty minutes
    // of media time at the fixture's rate is 18,000 packets, and the buffer has
    // to be the same size at the end of it as it was after the first minute.
    let buffer = buffer(30);
    let ceiling = buffer.config().memory_ceiling();

    for frame in 0..600 {
        push(&buffer, frame);
    }
    let settled = buffer.stats().bytes_held();

    for frame in 600..18_000 {
        push(&buffer, frame);
    }
    let after = buffer.stats();

    assert!(
        after.bytes_held() <= settled + u64::try_from(PACKET).expect("small") * RATE,
        "the buffer grew from {settled} to {} over thirty minutes",
        after.bytes_held()
    );
    assert!(
        after.peak_bytes_held() <= ceiling,
        "the buffer peaked at {} against a ceiling of {ceiling}",
        after.peak_bytes_held()
    );
    assert_eq!(after.segments_evicted_over_ceiling(), 0);
    assert_eq!(after.bytes_retained_for_a_save(), 0);
    assert!(after.segments_evicted_for_window() > 1_700);
}
