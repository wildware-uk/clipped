//! Saving a clip out of a live buffer, and taking the result apart again.
//!
//! Issue #37's three acceptance criteria, one test each, against **real coded
//! video**: the fixture in `support` is a test pattern encoded to H.264 by the
//! pinned FFmpeg build, so every clip written here is decoded rather than merely
//! described. That distinction is the reason `decoded_frames_at_least` appears
//! in each of them — a file whose packets all fail to decode has one video
//! stream, the right resolution, a plausible duration and monotonic timestamps,
//! and satisfies every other assertion in this file.
//!
//! The checking is `clipped-media-validation` (`tests/media`), the workspace's
//! one media harness, rather than assertions written here (AGENTS.md sections 22
//! and 55).
//!
//! What is deliberately absent is audio. A recording carries audio tracks
//! ([issue #180](https://github.com/wildware-uk/clipped/issues/180)), but the
//! replay buffer holds video packets only, so a clip cut from one is video
//! only and `audio_stream_count(0)` says so; carrying audio into a replay is
//! [issue #40](https://github.com/wildware-uk/clipped/issues/40).

mod support;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clipped_encoder::BitRate;
use clipped_media_validation::{Media, TemporaryDirectory, VideoStream};
use clipped_muxer::{MkvWriter, MuxError, RecordingLayout};
use clipped_replay::{
    save_clip, PushOutcome, ReplayBuffer, ReplayConfig, SaveError, DEFAULT_SEGMENT,
};
use support::{coded_video, frame_interval, presentation_time, CodedVideo, FRAMES_PER_SECOND};

/// The window every buffer here is configured with.
///
/// Sixty seconds, because "requesting the previous 60 seconds" is the first
/// acceptance criterion and a buffer cannot serve a request longer than its
/// window.
const WINDOW: Duration = Duration::from_secs(60);

/// The rate the buffer sizes its memory ceiling from.
///
/// Twice what the fixture is encoded at, so that the ceiling is nowhere near
/// what these tests are about: eviction under memory pressure is
/// `crates/replay/src/buffer.rs`'s subject, and a ceiling that bit here would
/// shorten the window and change what a clip contains for a reason that has
/// nothing to do with saving it.
fn generous_rate() -> BitRate {
    BitRate::bits_per_second(4_000_000).expect("a real rate")
}

/// How long the thread standing in for capture waits between frames.
///
/// Windows rounds a sleep up to something over half a millisecond, so this
/// paces at roughly two thousand frames a second: fast enough that filling a
/// minute of media takes a couple of seconds, fine enough to resolve a save
/// that takes a few milliseconds.
const PACE: Duration = Duration::from_micros(200);

fn buffer() -> ReplayBuffer {
    ReplayBuffer::new(
        ReplayConfig::new(WINDOW, generous_rate()).expect("sixty seconds is a supported window"),
    )
}

/// Which frame a presentation time belongs to.
///
/// The inverse of [`presentation_time`], which truncates to the nanosecond, so
/// this rounds up rather than dividing: at sixty frames a second the intervals
/// alternate between 16,666,666 and 16,666,667 nanoseconds, and a test that
/// compared differences instead of positions would be asserting on that
/// rounding rather than on whether a frame is missing.
fn frame_number(at: Duration) -> u64 {
    let ticks = (at.as_nanos() * u128::from(FRAMES_PER_SECOND)).div_ceil(1_000_000_000);
    u64::try_from(ticks).expect("a test never reaches 584 years of media time")
}

/// Pushes `frames` frames of the fixture, starting at frame `from`.
fn fill(buffer: &ReplayBuffer, video: &CodedVideo, from: u64, frames: u64) {
    for frame in from..from + frames {
        buffer.push(&video.packet(frame, presentation_time(frame)));
    }
}

#[test]
fn requesting_the_previous_sixty_seconds_yields_a_clip_of_that_length_that_plays() {
    let Some(video) = coded_video() else {
        return;
    };
    let buffer = buffer();
    fill(
        &buffer,
        video,
        0,
        u64::try_from(video.len()).expect("frames fit"),
    );

    let lease = buffer
        .lease_last(WINDOW)
        .expect("seventy seconds were pushed into a sixty second window");
    assert!(
        lease.is_complete(),
        "the buffer did not hold the whole minute: {lease:?}"
    );

    let directory = TemporaryDirectory::new("replay-save-sixty");
    let path = directory.file("clip.mkv");
    let clip = save_clip(&lease, &path, &RecordingLayout::new(video.track()))
        .expect("the clip is written");

    // The documented tolerance, and the whole of the keyframe-boundary
    // behaviour: never shorter than was asked for, never more than one segment
    // longer, and the extra is at the front because a clip can only begin on a
    // keyframe.
    assert!(
        clip.duration() >= WINDOW,
        "a clip of {:.3}s was written for a request of sixty seconds",
        clip.duration().as_secs_f64()
    );
    assert!(
        clip.duration() < WINDOW + DEFAULT_SEGMENT,
        "a clip of {:.3}s is more than one segment longer than the minute that was asked for",
        clip.duration().as_secs_f64()
    );
    assert_eq!(
        clip.duration(),
        WINDOW + clip.leading_slack(),
        "the extra is at the front and nowhere else"
    );
    assert_eq!(
        clip.trailing_slack(),
        Duration::ZERO,
        "the end of the clip is trimmed to the request"
    );
    assert!(clip.is_complete());
    assert!(
        clip.packets() > WINDOW.as_secs() * FRAMES_PER_SECOND,
        "a minute at sixty frames a second is more than {} pictures, and {} were written",
        WINDOW.as_secs() * FRAMES_PER_SECOND,
        clip.packets()
    );

    // The clip opens on a keyframe. Without this the file below would list a
    // video stream whose first pictures reference video that is not in it.
    assert!(lease
        .packets()
        .next()
        .expect("a lease holds packets")
        .is_keyframe());

    let media = Media::open(&path).expect("the clip opens");
    assert!(
        media.diagnostics().is_empty(),
        "ffprobe could not read the clip cleanly: {}",
        media.diagnostics()
    );

    media
        .validate()
        // One video track and no audio: a replay is video only until #180 gives
        // a recording an audio track at all.
        .stream_count(1)
        .audio_stream_count(0)
        .video(
            VideoStream::codec("h264")
                .resolution(support::WIDTH, support::HEIGHT)
                .pixel_format("yuv420p")
                .frame_rate("60/1")
                .title("Gameplay")
                // The assertion the rest of this file exists for, and it is
                // deliberately every packet rather than a round number: a clip
                // that did not open on a keyframe would still hold all its
                // packets and would still be a minute long, and the pictures
                // before the next keyframe would silently fail to decode.
                .decoded_frames_at_least(clip.packets())
                // And every packet handed to the writer reached the file.
                .packets(clip.packets()),
        )
        // What the API claimed, checked against the file: the span between the
        // first and last pictures, plus the last picture's own duration.
        .duration_seconds((clip.duration() + frame_interval()).as_secs_f64(), 0.01)
        .monotonic_timestamps()
        // The clip begins at zero however far into a session it was saved: the
        // writer rebases every timestamp onto the first packet of the file.
        .streams_start_at(0.0, 0.0)
        .assert_valid();
}

#[test]
fn capture_carries_on_uninterrupted_while_a_clip_is_saved() {
    let Some(video) = coded_video() else {
        return;
    };

    // The shape of `clipped_session::record_with_replay`: one thread drains the
    // encoder and puts every packet into both the recording and the buffer,
    // while a save reads the buffer from another thread.
    let directory = TemporaryDirectory::new("replay-save-during-capture");
    let recording_path = directory.file("recording.mkv");
    let clip_path = directory.file("clip.mkv");

    let buffer = Arc::new(buffer());
    let pushed = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let capturing = std::thread::spawn({
        let buffer = Arc::clone(&buffer);
        let pushed = Arc::clone(&pushed);
        let stop = Arc::clone(&stop);
        let layout = RecordingLayout::new(video.track());
        let path = recording_path.clone();
        move || {
            let mut recording =
                MkvWriter::create(&path, &layout).expect("the recording is created");
            let mut refused = 0_u64;
            let mut frame = 0_u64;

            // A hard ceiling as well as the flag, so that a failure on the other
            // thread cannot leave this one writing a file until the disk is
            // full.
            while !stop.load(Ordering::Acquire) && frame < 12_000 {
                let packet = video.packet(frame, presentation_time(frame));
                if !matches!(
                    buffer.push(&packet),
                    PushOutcome::Appended | PushOutcome::OpenedSegment(_)
                ) {
                    refused += 1;
                }
                recording
                    .write_packet(
                        &clipped_muxer::EncodedPacket::new(
                            clipped_muxer::TrackId::Video,
                            clipped_muxer::PacketTimestamp::from_nanos(
                                i64::try_from(presentation_time(frame).as_nanos())
                                    .expect("media time fits"),
                            ),
                            packet.data(),
                        )
                        .with_duration(frame_interval())
                        .with_keyframe(packet.is_keyframe()),
                    )
                    .expect("the recording takes the packet");

                frame += 1;
                pushed.store(frame, Ordering::Release);
                // Paced, so that capture is still running when the save starts
                // and its progress can be counted while the save is in flight.
                // Windows rounds a sleep up, and this one lands at about 540 µs
                // on the development machine — near enough two thousand frames a
                // second, which is fine detail beside a save measured in
                // milliseconds.
                std::thread::sleep(PACE);
            }

            (
                frame,
                refused,
                recording.finish().expect("the recording finishes"),
            )
        }
    });

    // Wait until the whole window is held, so that the save has a full minute of
    // video to write and therefore takes long enough to be observed.
    while pushed.load(Ordering::Acquire) < 61 * FRAMES_PER_SECOND {
        std::thread::yield_now();
    }

    let lease = buffer.lease_last(WINDOW).expect("a minute is held");
    let before = pushed.load(Ordering::Acquire);

    // The claim, and how it is made without a stopwatch: the save runs on its own
    // thread, and this watches whether capture advances **at all** while that
    // thread is still running. A `save_clip` holding the buffer's lock would
    // leave capture exactly where it was until the file was finished, so one
    // frame is the whole of the evidence — and it is evidence a busy machine
    // cannot destroy.
    //
    // It used to require ten, and that is what made it flaky (issue #490). The
    // loop has two exits, so a save that finished before a loaded runner had
    // scheduled the capture thread ten times left `advanced` short and failed
    // with "capture was waiting on the save" — accusing the code of the one
    // defect this test exists to catch, when what had actually happened was that
    // the machine was busy. A threshold scheduling luck can reach or miss is not
    // measuring the property; "did it move" is.
    let (advanced, clip) = std::thread::scope(|scope| {
        let saver =
            scope.spawn(|| save_clip(&lease, &clip_path, &RecordingLayout::new(video.track())));
        let mut advanced = 0;
        while !saver.is_finished() && advanced == 0 {
            advanced = pushed.load(Ordering::Acquire) - before;
            std::thread::yield_now();
        }
        (advanced, saver.join().expect("the save thread finishes"))
    });
    let clip = clip.expect("the clip is written");

    stop.store(true, Ordering::Release);
    let (frames, refused, recording) = capturing.join().expect("the capture thread finishes");

    assert!(
        advanced > 0,
        "capture did not advance by a single frame while the clip was being written, so the save \
         held something the capture thread needed"
    );
    assert_eq!(refused, 0, "the buffer refused a packet during the save");
    assert_eq!(
        buffer.stats().packets_buffered(),
        frames,
        "the buffer did not take every packet capture produced"
    );
    assert_eq!(recording.packets, frames);

    // No gap in what is still buffered, either: the pictures either side of the
    // save are one frame interval apart, so a save that had cost the buffer a
    // packet would show as a jump here (AGENTS.md section 22).
    let after_the_save = buffer
        .lease_last(Duration::from_secs(30))
        .expect("the buffer is still filling");
    let times: Vec<Duration> = after_the_save
        .packets()
        .map(|packet| packet.presentation_time())
        .collect();
    let opening = frame_number(times[0]);
    for (offset, at) in times.iter().enumerate() {
        let expected = presentation_time(opening + u64::try_from(offset).expect("frames fit"));
        assert_eq!(
            *at,
            expected,
            "frame {} of the buffered video is missing across the save",
            opening + offset as u64
        );
    }

    // And the continuing recording is a whole, playable recording of everything
    // that was captured — including the part written while the clip was.
    let media = Media::open(&recording_path).expect("the recording opens");
    media
        .validate()
        .stream_count(1)
        .video(
            VideoStream::codec("h264")
                .resolution(support::WIDTH, support::HEIGHT)
                .decoded_frames_at_least(frames)
                .packets(frames),
        )
        .monotonic_timestamps()
        .assert_valid();

    // The clip taken out of the middle of it plays too.
    let clip_media = Media::open(&clip_path).expect("the clip opens");
    clip_media
        .validate()
        .stream_count(1)
        .video(
            VideoStream::codec("h264")
                .resolution(support::WIDTH, support::HEIGHT)
                .decoded_frames_at_least(clip.packets())
                .packets(clip.packets()),
        )
        .monotonic_timestamps()
        .assert_valid();
}

#[test]
fn two_saves_in_quick_succession_both_produce_valid_clips() {
    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("replay-save-twice");

    let buffer = Arc::new(buffer());
    let pushed = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    // Capture keeps running for the whole of this test, so both saves read a
    // buffer that is still being written to. That it is also still *evicting* is
    // not assumed from the pacing below — it is waited for and then asserted.
    let capturing = std::thread::spawn({
        let buffer = Arc::clone(&buffer);
        let pushed = Arc::clone(&pushed);
        let stop = Arc::clone(&stop);
        move || {
            let mut frame = 0_u64;
            while !stop.load(Ordering::Acquire) && frame < 12_000 {
                buffer.push(&video.packet(frame, presentation_time(frame)));
                frame += 1;
                pushed.store(frame, Ordering::Release);
                if frame % 60 == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            frame
        }
    });

    // Wait until the window has begun to roll, so the saves below start against
    // a buffer at its steady state rather than one still filling up. Taken from
    // the buffer's own counter rather than inferred from a frame count, and
    // bounded by the capture thread's ceiling so that a buffer which never
    // evicts fails here instead of spinning.
    while buffer.stats().segments_evicted_for_window() == 0 {
        assert!(
            pushed.load(Ordering::Acquire) < 12_000,
            "the capture thread pushed everything it had and the window never moved, so nothing \
             in this test was read out of an evicting buffer"
        );
        std::thread::yield_now();
    }
    let before = buffer.stats();

    // Two leases, one after the other, as two hotkey presses a moment apart
    // would take them: the whole minute, then the last fifteen seconds. The
    // first is deliberately the whole window, so the oldest segment it holds is
    // the very one the buffer is about to evict — which is what makes the wait
    // inside the scope below reachable in the time a save takes.
    let first = buffer.lease_last(WINDOW).expect("the whole minute is held");
    let second = buffer
        .lease_last(Duration::from_secs(15))
        .expect("fifteen seconds are held");

    let first_path = directory.file("first.mkv");
    let second_path = directory.file("second.mkv");
    let track = video.track();

    // "Still evicting" made a precondition rather than hoped for, which is the
    // whole of [issue #430](https://github.com/wildware-uk/clipped/issues/430).
    //
    // This used to wait *inside* the scope below, with the saves running, and
    // its loop had two ways out: the contention appearing, or a save finishing.
    // On a runner where both saves completed before the capture thread evicted
    // anything it took the second and then asserted the first, so the test
    // reported a symptom of its own setup not having happened.
    //
    // It waits here instead, before the saves start, and it is sound because a
    // *lease* is what retains an evicted segment — `drop_front` moves a segment
    // to `leased` when anything else holds an `Arc` to it, which the two leases
    // above do for as long as they are alive. So neither counter needs a save to
    // be in flight, and the capture thread pushing underneath is enough to make
    // both move. `segments_evicted_for_window` says the window moved;
    // `segments_retained_for_a_save` says what it moved past was still held. The
    // eviction count alone would be satisfied by a window rolling past segments
    // no reader ever wanted.
    //
    // Bounded by the capture thread's own ceiling, so a buffer that never
    // reaches the state says so instead of spinning — and says it as a
    // *give-up*, distinct from the assertions below failing.
    loop {
        let now = buffer.stats();
        if now.segments_evicted_for_window() > before.segments_evicted_for_window()
            && now.segments_retained_for_a_save() > 0
        {
            break;
        }
        assert!(
            pushed.load(Ordering::Acquire) < 12_000,
            "gave up rather than failed: the capture thread pushed all 12,000 frames and the \
             window never moved past a leased segment, so this test never reached the state it \
             is about ({} evicted, {} retained, {} evicted before the leases were taken)",
            now.segments_evicted_for_window(),
            now.segments_retained_for_a_save(),
            before.segments_evicted_for_window()
        );
        std::thread::yield_now();
    }
    // The state the assertions at the bottom are about, taken at the moment it
    // was reached rather than after the saves — by then the saves have finished
    // and what is being asserted would be a fact about a different moment.
    let during = buffer.stats();

    let (first_clip, second_clip) = std::thread::scope(|scope| {
        let one =
            scope.spawn(|| save_clip(&first, &first_path, &RecordingLayout::new(track.clone())));
        let two =
            scope.spawn(|| save_clip(&second, &second_path, &RecordingLayout::new(track.clone())));

        (
            one.join().expect("the first save finishes"),
            two.join().expect("the second save finishes"),
        )
    });
    let first_clip = first_clip.expect("the first clip is written");
    let second_clip = second_clip.expect("the second clip is written");

    stop.store(true, Ordering::Release);
    let frames = capturing.join().expect("the capture thread finishes");

    // Both of these are now established by the wait above rather than raced
    // for, so neither can fail as the test stands — and they are kept anyway,
    // deliberately. They are what the wait is *for*, written where a reader
    // meets the clips, and they are the guard on the loop's exit condition: a
    // later change that weakens it back into "or a save finished" makes these
    // reachable again and they fail exactly as they used to.
    assert!(
        during.segments_evicted_for_window() > before.segments_evicted_for_window(),
        "the window did not move while a save held a lease, so neither save read an evicting \
         buffer: {} segments evicted before the leases were taken and {} once the wait ended",
        before.segments_evicted_for_window(),
        during.segments_evicted_for_window()
    );
    assert!(
        during.segments_retained_for_a_save() > 0,
        "the window moved past no segment a save was reading, so the saves and the eviction never \
         met on the same segment and the lease was never what kept one alive"
    );

    // The buffer is intact: it took every packet, and it will still serve a
    // clip afterwards.
    assert_eq!(buffer.stats().packets_buffered(), frames);
    assert!(buffer.lease_last(Duration::from_secs(30)).is_ok());

    for (clip, seconds) in [(&first_clip, 60), (&second_clip, 15)] {
        assert!(clip.is_complete(), "{clip}");
        assert!(clip.duration() >= Duration::from_secs(seconds));

        let media = Media::open(clip.path()).expect("the clip opens");
        media
            .validate()
            .stream_count(1)
            .audio_stream_count(0)
            .video(
                VideoStream::codec("h264")
                    .resolution(support::WIDTH, support::HEIGHT)
                    .decoded_frames_at_least(clip.packets())
                    .packets(clip.packets()),
            )
            .duration_seconds((clip.duration() + frame_interval()).as_secs_f64(), 0.01)
            .monotonic_timestamps()
            .streams_start_at(0.0, 0.0)
            .assert_valid();
    }

    // The two clips are different lengths, so this cannot have passed by
    // writing the same file twice.
    assert!(first_clip.duration() > second_clip.duration());
}

#[test]
fn a_clip_never_overwrites_something_already_there() {
    let Some(video) = coded_video() else {
        return;
    };
    let buffer = buffer();
    fill(&buffer, video, 0, 10 * FRAMES_PER_SECOND);

    let lease = buffer
        .lease_last(Duration::from_secs(5))
        .expect("five seconds are held");
    let directory = TemporaryDirectory::new("replay-save-existing");
    let path = directory.file("clip.mkv");

    save_clip(&lease, &path, &RecordingLayout::new(video.track()))
        .expect("the first clip is written");
    let refused = save_clip(&lease, &path, &RecordingLayout::new(video.track()))
        .expect_err("a clip must not be written over a clip");

    // Choosing another name belongs to the caller, which is the layer that knows
    // what the clip is of (issue #38). What this must never do is truncate one.
    assert!(
        matches!(
            refused,
            SaveError::Create {
                source: MuxError::OutputExists { .. },
                ..
            }
        ),
        "{refused}"
    );
    assert!(
        Media::open(&path)
            .expect("the first clip is still there")
            .video_streams()
            .len()
            == 1
    );
}
