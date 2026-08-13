//! Saving a replay, and taking apart everything the save was supposed to
//! produce.
//!
//! This is issue #38's central claim checked rather than described: **press the
//! key, get a clip of the last N seconds — one that decodes, is named after its
//! session, and is in the library.** Every one of those is a separate way for
//! the feature to be useless, so each is asserted separately and against the
//! real thing:
//!
//! | Claim | Checked by |
//! | --- | --- |
//! | The clip is playable media of the right length | `clipped-media-validation`, decoding every picture |
//! | It is what the buffer held rather than an empty file | `decoded_frames`, against the frames pushed |
//! | The session's record names it, and where it came from | reading the sidecar this save rewrote |
//!
//! The fourth — that the library indexes what the session recorded — is
//! `crates/library/tests/sidecars.rs`, which indexes a sidecar the real writer
//! produced and reads the `clips` row back. It lives there because
//! `clipped-library` sits four layers below this package and the file is the
//! contract between them.
//!
//! # What is real here, and what is not
//!
//! Real: the H.264 (encoded by the pinned FFmpeg build,
//! `clipped_media_validation::CodedVideo`), the replay buffer, the lease, the
//! Matroska writer, the session record, the SQLite library and its indexer, and
//! `clipped_recorder::replay::save` — which is the same function the hotkey
//! handler and `save_replay` over IPC both call.
//!
//! Not real: the capture and the encoder session. `ReplayRecording::begin` is
//! handed the fixture's track description and the buffer is filled by pushing
//! its pictures, because opening a real encoder needs a GPU and a desktop
//! session — which is what makes `tests/capture` and `record_end_to_end`
//! `#[ignore]`d, and would make this test one nobody runs. What that costs is
//! the pipeline *into* the buffer, and `crates/session/src/recording.rs`'s own
//! tests cover exactly that: every packet the file receives is also copied into
//! the buffer.

#![cfg(windows)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use clipped_hotkeys::HotkeyAction;
use clipped_media_validation::{
    require_media_tools, CodedVideo, Media, TemporaryDirectory, VideoStream,
};
use clipped_muxer::RecordingLayout;
use clipped_recorder::replay::{handlers_for, save};
use clipped_session::automatic::ManualSession;
use clipped_session::config::Configuration;
use clipped_session::ReplayRecording;

/// The picture the fixture is encoded at, and the rate it is timed at.
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const FRAMES_PER_SECOND: u64 = 60;

/// Two seconds, which is what the buffer's segments are and what the encoder's
/// default keyframe interval is.
const KEYFRAME_INTERVAL: u64 = 120;

/// How much video the fixture holds: longer than the window below, so the
/// buffer is filled while it is evicting rather than while it is still filling.
const FIXTURE_SECONDS: u32 = 40;

/// The window every buffer here keeps: the shortest one a buffer may have, so
/// the test pushes as little video as it can get away with.
const WINDOW: Duration = Duration::from_secs(30);

/// The bitrate the buffer is sized from — generously, because eviction under
/// memory pressure is `crates/replay`'s subject and a ceiling that bit here
/// would change what a clip contains for a reason that has nothing to do with
/// saving it.
fn generous_rate() -> clipped_encoder::BitRate {
    clipped_encoder::BitRate::bits_per_second(4_000_000).expect("a real rate")
}

/// When frame `index` is presented, from the frame number rather than a clock,
/// so the test behaves identically however fast the machine runs it.
fn presentation_time(index: u64) -> Duration {
    Duration::from_nanos(index * 1_000_000_000 / FRAMES_PER_SECOND)
}

/// The fixture's coded video, or [`None`] when the FFmpeg programs are not on
/// this machine — which `require_media_tools` turns into a failure where
/// `CLIPPED_REQUIRE_MEDIA` is set.
fn coded_video() -> Option<CodedVideo> {
    require_media_tools()?;
    CodedVideo::encode(
        WIDTH,
        HEIGHT,
        u32::try_from(FRAMES_PER_SECOND).expect("60 fits"),
        u32::try_from(KEYFRAME_INTERVAL).expect("120 fits"),
        FIXTURE_SECONDS,
    )
}

/// The layout a recording of that video would declare, as
/// `crates/session/src/audio.rs` builds one — video only, because nothing puts
/// audio in a replay buffer yet (issue #40).
fn layout(video: &CodedVideo) -> RecordingLayout {
    RecordingLayout::new(
        clipped_muxer::VideoTrack::new(clipped_muxer::VideoCodec::H264, WIDTH, HEIGHT)
            .with_frame_rate(
                clipped_muxer::FrameRate::per_second(
                    u32::try_from(FRAMES_PER_SECOND).expect("60 fits"),
                )
                .expect("a real rate"),
            )
            // Without these the file lists a video stream nothing can decode,
            // which is exactly the failure `decoded_frames` below exists to
            // catch.
            .with_codec_private(video.parameter_sets().to_vec())
            .with_name("Gameplay"),
    )
}

/// Pushes `seconds` of `video` into `buffer`, as the recording's packet loop
/// does one packet at a time (`crates/session/src/recording.rs`).
fn push(video: &CodedVideo, buffer: &clipped_replay::ReplayBuffer, seconds: u64) {
    for frame in 0..seconds * FRAMES_PER_SECOND {
        buffer.push(&clipped_encoder::EncodedPacket::new(
            video.picture(frame),
            presentation_time(frame),
            presentation_time(frame),
            if video.is_keyframe(frame) {
                clipped_encoder::PictureKind::Keyframe
            } else {
                clipped_encoder::PictureKind::Predicted
            },
        ));
    }
}

/// A replay handle with `seconds` of real coded video already in it.
///
/// Attached through `clipped_session::start_buffer`, which is the same join a
/// real recording makes when its encoder opens, so every test below is saving
/// out of a buffer wired up the way the recorder wires one.
///
/// [`None`] when the FFmpeg programs are not on this machine.
fn buffered(seconds: u64) -> Option<ReplayRecording> {
    let video = coded_video()?;

    let replay = ReplayRecording::new(WINDOW).expect("thirty seconds is a supported window");
    let buffer = clipped_session::start_buffer(&layout(&video), generous_rate(), Some(&replay))
        .expect("a recording asked for a buffer has one once its encoder is open");
    push(&video, buffer, seconds);

    Some(replay)
}

/// A session for a recording in `directory`, as `replay` and `serve` open one.
fn session(directory: &Path) -> Mutex<ManualSession> {
    Mutex::new(ManualSession::start(
        directory,
        directory.join("clipped-recording.mkv"),
        &Configuration::defaults(),
        // Deliberately empty. What is under test is the clip and its entry in
        // the session record, and a catalogue is the one input here that would
        // otherwise come from the machine running the test rather than from the
        // test (AGENTS.md section 25).
        &clipped_game_detection::catalogue::Catalogue::default(),
        clipped_session::automatic::RecordedProcess::new(4_242, "cs2.exe"),
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725),
    ))
}

#[test]
fn a_saved_replay_is_the_last_n_seconds_and_every_picture_of_it_decodes() {
    // The acceptance criterion, end to end. A file that opens, has one video
    // stream of the right size, and whose every picture comes back out of a
    // decoder — the last of which is what separates "a clip was produced" from
    // "a clip was produced that anybody can watch" (AGENTS.md section 22).
    let Some(replay) = buffered(35) else {
        return;
    };
    let directory = TemporaryDirectory::new("replay-save");
    let session = session(directory.path());

    let saved = save(
        &replay,
        &session,
        Duration::from_secs(10),
        None,
        SystemTime::now(),
    )
    .expect("ten seconds of a thirty-second buffer can be saved");
    let clip = &saved.clip;

    assert!(clip.is_complete(), "the buffer held the whole request");
    assert!(
        clip.duration() >= Duration::from_secs(10),
        "a clip is never shorter than what was asked for: {clip}"
    );
    assert!(
        clip.duration() < Duration::from_secs(12),
        "and never more than one segment longer, at the front: {clip}"
    );

    // Every picture between the two ends, counted independently of the buffer's
    // own accounting: at 60 fps a 10-second request covering 10.0-11.99 s is
    // 600 pictures at the least.
    let media = Media::open(clip.path()).expect("the clip opens");
    media
        .validate()
        .stream_count(1)
        .video(
            VideoStream::codec("h264")
                .resolution(WIDTH, HEIGHT)
                .decoded_frames_at_least(600),
        )
        // Video only, deliberately and temporarily: nothing puts audio in a
        // replay buffer yet, and carrying every track into a clip is
        // [issue #40](https://github.com/wildware-uk/clipped/issues/40).
        // Asserting the count rather than leaving it unsaid is what makes that
        // a stated gap instead of an assumption nobody checked.
        .audio_stream_count(0)
        .monotonic_timestamps()
        .assert_valid();

    // And the session's own record names it, with where in the recording it came
    // from — which is what `clipped-library` indexes into the `clips` table
    // (`crates/library/tests/sidecars.rs`). A save that wrote a file and told
    // nobody would leave the user with a clip their library has never heard of,
    // and every assertion above would still pass.
    let sidecar = std::fs::read_to_string(session.lock().expect("a fresh lock").sidecar_path())
        .expect("the session's record is on disk");
    let written: serde_json::Value = serde_json::from_str(&sidecar).expect("the sidecar is JSON");
    let recorded = &written["clips"][0];
    assert_eq!(
        recorded["path"].as_str().expect("the clip names its file"),
        clip.path().display().to_string(),
        "the record has to name the file that was written: {sidecar}"
    );
    assert_eq!(recorded["source_recording"], serde_json::Value::from(1));
    assert!(
        (recorded["source_start_seconds"].as_f64().expect("a start")
            - clip.covered().start().as_secs_f64())
        .abs()
            < f64::EPSILON,
        "the clip's bounds in the recording have to be the ones it really covers: {sidecar}"
    );
    assert_eq!(recorded["complete"], serde_json::Value::from(true));
}

#[test]
fn pressing_the_replay_key_saves_a_clip_and_no_other_key_does_anything() {
    // The wiring between "a key was pressed" and everything the test above
    // checks. `run` builds this map behind a window, an encoder and a capture
    // session, so the map is built here instead and its handler run directly —
    // no keyboard, no desktop session, and no dependence on `Ctrl`+`F10` being
    // free on the machine (`clipped_hotkeys::Handlers::press`).
    let Some(replay) = buffered(35) else {
        return;
    };
    let directory = TemporaryDirectory::new("replay-hotkey");
    let replay = Arc::new(replay);
    let session = Arc::new(session(directory.path()));

    let mut handlers = handlers_for(&replay, &session, Duration::from_secs(5));

    assert_eq!(
        handlers.handled().collect::<Vec<_>>(),
        vec![HotkeyAction::SaveReplay],
        "a `replay` invocation performs a save and nothing else, and an action \
         with a handler that does nothing is worse than one with none",
    );

    let hotkey = "Ctrl+F10".parse().expect("Ctrl+F10 is a hotkey");
    handlers
        .press(HotkeyAction::SaveReplay, hotkey)
        .expect("the replay key has a handler");
    assert!(
        handlers
            .press(HotkeyAction::TakeScreenshot, hotkey)
            .is_err(),
        "`replay` takes no screenshots, so that press must report itself \
         unhandled rather than look like it did something",
    );

    // What the press produced: a clip on disk, entered in the session's record
    // — which is the whole of what the handler is for.
    let recorded = session.lock().expect("a fresh lock");
    let clips = recorded.session().clips();
    assert_eq!(clips.len(), 1, "one press, one clip");
    let clip = clips[0].path().to_path_buf();
    assert_eq!(
        clips[0].requested(),
        Duration::from_secs(5),
        "a press saves the window `handlers_for` was given, not the thirty \
         seconds the buffer keeps",
    );
    assert!(clips[0].is_complete());
    drop(recorded);

    Media::open(&clip)
        .expect("the press wrote a clip that opens")
        .validate()
        .stream_count(1)
        .video(
            VideoStream::codec("h264")
                .resolution(WIDTH, HEIGHT)
                // Five seconds at 60 fps, and never the thirty-five pushed.
                .decoded_frames_at_least(300),
        )
        .monotonic_timestamps()
        .assert_valid();
}

#[test]
fn a_recording_pushes_into_the_buffer_it_started_and_a_clip_declares_that_recordings_video() {
    // The join between an encoder opening and a buffer existing: two steps that
    // are only correct together, four lines behind a GPU in
    // `crates/session/src/recording.rs`, and now one function that can be called
    // with coded video instead of a graphics device.
    let Some(video) = coded_video() else {
        return;
    };
    let replay = ReplayRecording::new(WINDOW).expect("thirty seconds is a supported window");

    assert!(
        clipped_session::start_buffer(&layout(&video), generous_rate(), None).is_none(),
        "a recording that was not asked for a replay buffer must not be given one",
    );
    assert!(
        replay.buffer().is_none(),
        "and nothing has a buffer before its encoder opened",
    );

    let buffer = clipped_session::start_buffer(&layout(&video), generous_rate(), Some(&replay))
        .expect("a recording asked for a buffer has one once its encoder is open");
    assert!(
        std::ptr::eq(
            buffer,
            replay.buffer().expect("the recording started a buffer"),
        ),
        "the packets have to go into the buffer this recording will save from, \
         not into one nothing can reach",
    );

    push(&video, buffer, 6);

    // And what a clip out of it declares is the recording's own video track. A
    // buffer begun without it holds the same pictures and produces a file with a
    // video stream nothing can decode, which passes every check short of
    // decoding it.
    let directory = TemporaryDirectory::new("replay-join");
    let session = session(directory.path());
    let saved = save(
        &replay,
        &session,
        Duration::from_secs(2),
        None,
        SystemTime::now(),
    )
    .expect("two of the six seconds pushed");

    Media::open(saved.clip.path())
        .expect("the clip opens")
        .validate()
        .stream_count(1)
        .video(
            VideoStream::codec("h264")
                .resolution(WIDTH, HEIGHT)
                .decoded_frames_at_least(120),
        )
        .assert_valid();
}

#[test]
fn a_clip_is_named_after_its_session_and_numbered_within_it() {
    // What issue #37 deliberately left to this one: `save_clip` writes where it
    // is told, and deciding what a clip is called belongs to the layer that
    // knows what it is of. Two saves must not collide, and both must be
    // findable beside the recording they came from.
    let Some(replay) = buffered(35) else {
        return;
    };
    let directory = TemporaryDirectory::new("replay-names");
    let session = session(directory.path());
    let session_id = session
        .lock()
        .expect("a fresh lock")
        .id()
        .as_str()
        .to_owned();

    let first = save(
        &replay,
        &session,
        Duration::from_secs(5),
        None,
        SystemTime::now(),
    )
    .expect("the first save");
    let second = save(
        &replay,
        &session,
        Duration::from_secs(5),
        None,
        SystemTime::now(),
    )
    .expect("the second save");

    assert_eq!(
        first.clip.path().file_name().expect("a file name"),
        format!("clipped-{session_id}-replay-1.mkv").as_str()
    );
    assert_eq!(
        second.clip.path().file_name().expect("a file name"),
        format!("clipped-{session_id}-replay-2.mkv").as_str()
    );
    assert!(
        first.clip.path().exists() && second.clip.path().exists(),
        "a second save must not overwrite the first"
    );
}

#[test]
fn a_save_the_buffer_could_not_fill_still_produces_the_clip_there_is() {
    // The hotkey pressed ten seconds into a recording, asking for the last
    // thirty. Refusing would be worse: there is a clip to be had and it is the
    // one somebody asked for — but the caller has to be able to say it was
    // short, which is what `is_complete` and `shortfall` are for.
    let Some(replay) = buffered(10) else {
        return;
    };
    let directory = TemporaryDirectory::new("replay-short");
    let session = session(directory.path());

    let saved = save(
        &replay,
        &session,
        Duration::from_secs(30),
        None,
        SystemTime::now(),
    )
    .expect("a short buffer still saves what it has");
    let clip = &saved.clip;

    assert!(!clip.is_complete(), "twenty seconds were never recorded");
    assert!(
        clip.shortfall() >= Duration::from_secs(19),
        "the shortfall has to say how much is missing: {clip}"
    );
    assert!(
        clip.duration() >= Duration::from_secs(9),
        "and what there was still has to be written: {clip}"
    );

    Media::open(clip.path())
        .expect("a short clip is still a clip")
        .validate()
        .stream_count(1)
        .video(VideoStream::codec("h264").decoded_frames_at_least(540))
        .assert_valid();
}

#[test]
fn a_clip_is_written_to_a_path_the_caller_named_and_never_over_one_that_exists() {
    // What `save_replay`'s `output` reaches: a destination somebody chose. A
    // path already taken is refused rather than replaced, because a clip nobody
    // can get back must not be destroyed to save a dialog (AGENTS.md
    // section 56).
    let Some(replay) = buffered(35) else {
        return;
    };
    let directory = TemporaryDirectory::new("replay-destination");
    let session = session(directory.path());
    let chosen = directory.file("ace on mirage.mkv");

    let saved = save(
        &replay,
        &session,
        Duration::from_secs(5),
        Some(chosen.clone()),
        SystemTime::now(),
    )
    .expect("a named destination is written");
    assert_eq!(saved.clip.path(), chosen);

    let refused = save(
        &replay,
        &session,
        Duration::from_secs(5),
        Some(chosen.clone()),
        SystemTime::now(),
    )
    .expect_err("the second save cannot have the same file");
    assert!(
        refused.to_string().contains("could not be created"),
        "the refusal should say the file could not be created: {refused}"
    );

    // And the refused save left the first clip alone.
    Media::open(&chosen)
        .expect("the first clip is still there")
        .validate()
        .stream_count(1)
        .video(VideoStream::codec("h264").decoded_frames_at_least(300))
        .assert_valid();
}
