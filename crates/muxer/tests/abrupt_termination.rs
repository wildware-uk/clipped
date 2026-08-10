//! Killing the writer mid-recording, and proving what survives is playable.
//!
//! This is the claim the whole container decision rests on. ADR 0001 chose
//! Matroska because a recording interrupted by a driver reset, a power cut or a
//! user holding the power button has to remain a recording of everything up to
//! that moment, and it says in as many words that this is a thing to test
//! rather than assume. AGENTS.md section 17 asks for the same.
//!
//! # Why this cannot be an in-process test
//!
//! A Rust panic unwinds and runs destructors, and destructors are exactly what
//! a killed process does not get: the container trailer is not written, the
//! file is not closed, and whatever libavformat was holding in memory is gone
//! with it. So the recorder here is a real child process
//! (`examples/synthetic_recording.rs`) and it is ended with `TerminateProcess`,
//! which is what `Child::kill` does on Windows — no notification, no
//! destructors, no flush.
//!
//! # What is asserted
//!
//! That the survivor opens, that it still declares every track, that its video
//! decodes frame by frame, and that it reaches to within a bounded distance of
//! the last moment the recorder reported writing. The bound is the muxer's
//! cluster time limit plus a margin, and the second test below is the one that
//! shows the bound is the muxer's and not the encoder's: it uses keyframes five
//! seconds apart, which without a cluster limit leaves a file `ffprobe` cannot
//! open at all.

mod support;

use support::{packets, Probe, RunningRecorder, TemporaryDirectory};

/// How far into the recording the process is killed.
const KILL_AFTER_SECONDS: f64 = 3.0;

/// How much of the recording an abrupt termination may cost.
///
/// FFmpeg accumulates a Matroska cluster before writing it, so what is lost is
/// whatever was in the open one: the muxer caps a cluster at a second of media,
/// and the margin covers the one packet the recorder may have written but not
/// yet reported when the kill landed.
///
/// This is the number `docs/muxing.md` publishes as the guarantee, so loosening
/// it is a change to what Clipped promises rather than a way to quieten a test.
const ACCEPTABLE_LOSS_SECONDS: f64 = 1.1;

#[test]
fn a_killed_recording_still_plays_up_to_nearly_the_moment_it_died() {
    let directory = TemporaryDirectory::new("killed");
    let path = directory.file("recording.mkv");

    let interrupted = record_and_kill(&path, 1.0);

    assert!(
        interrupted.lost() <= ACCEPTABLE_LOSS_SECONDS,
        "the recorder wrote {} seconds before it was killed and only {} survived",
        interrupted.reported,
        interrupted.recovered
    );
}

#[test]
fn what_is_lost_is_bounded_by_the_muxer_rather_than_by_the_keyframe_interval() {
    // A hardware encoder configured for bitrate rather than for seeking puts
    // keyframes seconds apart, and a Matroska cluster is otherwise closed when
    // one arrives. Left to FFmpeg's defaults this same run leaves an 823-byte
    // file that ffprobe reports `End of file` for; the muxer's cluster time
    // limit is what keeps the loss where the test above put it.
    let directory = TemporaryDirectory::new("killed-long-gop");
    let path = directory.file("recording.mkv");

    let interrupted = record_and_kill(&path, 5.0);

    assert!(
        interrupted.lost() <= ACCEPTABLE_LOSS_SECONDS,
        "with keyframes five seconds apart, {} of the {} seconds written were lost",
        interrupted.lost(),
        interrupted.reported
    );
}

/// How much of an interrupted recording came back.
#[derive(Debug, Clone, Copy)]
struct Interrupted {
    /// The last moment the recorder said it had written.
    reported: f64,
    /// The last moment the file it left behind actually holds.
    recovered: f64,
}

impl Interrupted {
    /// The seconds of recording the kill cost.
    fn lost(self) -> f64 {
        self.reported - self.recovered
    }
}

/// Records in real time until the writer reports [`KILL_AFTER_SECONDS`] of
/// media, kills it, and reports what was written against what survived.
///
/// Every assertion that the survivor is *usable* is made here, so that both
/// tests above make them: what differs between the two is only how much of the
/// recording is expected to come back.
fn record_and_kill(path: &std::path::Path, keyframe_seconds: f64) -> Interrupted {
    let mut recorder = RunningRecorder::start(&[
        "--output",
        &path.to_string_lossy(),
        // Longer than the test will ever let it run.
        "--seconds",
        "600",
        // In real time, so that "killed three seconds in" means three seconds
        // of recording rather than three seconds of whatever the encoder
        // managed.
        "--pace",
        "--keyframe-seconds",
        &keyframe_seconds.to_string(),
    ]);

    recorder.wait_for_media_seconds(KILL_AFTER_SECONDS);
    let reported = recorder.kill();
    assert!(
        reported >= KILL_AFTER_SECONDS,
        "the recorder was killed before it had written {KILL_AFTER_SECONDS} seconds"
    );

    // Nothing is asserted about what ffprobe says on standard error. A file
    // that ends in the middle of an element is what a killed writer leaves
    // behind, and FFmpeg mentions it — `File ended prematurely` — while reading
    // everything in front of it perfectly well. The question this test asks is
    // not whether the truncation is invisible; it is whether what precedes it
    // plays. So the assertions below are about content, and a file FFmpeg
    // genuinely cannot open fails them: it reports no streams at all.
    let probe = Probe::of(path);

    // Every track is still described: the header was written when the file was
    // created, not when it was finished.
    assert_eq!(
        probe.streams.len(),
        3,
        "the interrupted recording lost a track. ffprobe said: {}",
        probe.diagnostics
    );
    let video = probe.streams_of("video");
    assert_eq!(video.len(), 1);

    // And the video decodes, frame by frame, rather than merely being listed.
    let decoded = support::number(video[0], "nb_read_frames").unwrap_or_default();
    let parsed = support::number(video[0], "nb_read_packets").unwrap_or_default();
    assert!(
        decoded > 0.0,
        "no frame of the interrupted recording could be decoded. ffprobe said: {}",
        probe.diagnostics
    );
    assert!(
        decoded >= parsed - 1.0,
        "{parsed} packets survived but only {decoded} of them decoded; at most the final \
         one, which the kill may have cut in half, should be unusable"
    );

    // The timestamps that survived are still monotonic, per track.
    let written = packets(path);
    support::assert_decode_timestamps_increase(&written);

    let last_video_packet = written
        .iter()
        .filter(|packet| packet.stream_index == 0)
        .map(|packet| packet.decode_seconds)
        .fold(f64::MIN, f64::max);

    assert!(
        last_video_packet <= reported + 0.1,
        "the file claims to hold {last_video_packet} seconds of video, but the recorder \
         only reported writing {reported}"
    );

    Interrupted {
        reported,
        recovered: last_video_packet,
    }
}
