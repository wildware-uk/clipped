//! End-to-end: capture video and system audio at the same time, and measure how
//! far apart they drift.
//!
//! This is the measurement behind `docs/av-sync.md`. Everything else about
//! synchronisation can be argued from first principles — both Windows capture
//! APIs stamp frames with the performance counter, WASAPI reports positions
//! against the same counter, so the two are comparable — but the argument stops
//! at the audio *device*, whose sample clock is a crystal on a sound card and
//! runs at its own rate. Whether that rate matters is an empirical question, and
//! this file is how it is answered.
//!
//! # What it measures, and why that is the right quantity
//!
//! Every buffer `clipped-audio` hands over carries two accounts of the same
//! moment:
//!
//! - `timestamp()` — where the *track* puts it, which is the capture's anchor
//!   plus every frame emitted since, so the track is contiguous and as long as
//!   the recording;
//! - `device_timestamp()` — where the *endpoint* said it belongs, a
//!   performance-counter position, which is the same clock the video frames
//!   arriving on the other thread are stamped with.
//!
//! Their difference is how far the track has moved against the performance
//! counter, in nanoseconds, at that moment. Video timestamps are readings of
//! that same counter, so the way that difference grows over the run is the way
//! the audio moves against the picture, and its slope is the drift rate — the
//! number that decides whether a two-hour recording *ends* as well aligned as it
//! started.
//!
//! # The second measurement: the absolute offset
//!
//! The drift measurement above cannot see a *constant* offset, for the reason
//! in the next section, so this file holds a second test that can:
//! [`the_absolute_av_offset_of_a_synchronised_subject_is_within_tolerance`]. It
//! runs the same subject with `--tone`, which places a short sound at the
//! moment a named frame is presented, and then finds both halves of that one
//! event in what was captured — the tone by its frequency in the samples, the
//! frame by its counter in the pixels.
//!
//! What it reports is
//!
//! ```text
//! offset = (audio in the recording − audio at the source)
//!        − (video in the recording − video at the source)
//! ```
//!
//! which is a number the drift measurement cannot produce at any size, because
//! it has no source to compare against. Both source moments come from the
//! application (`test-apps/video-pattern/src/tone.rs`) and neither is assumed to
//! be the other: the skew between them is announced per tone and subtracted.
//!
//! **What that number contains that is not the recorder's.** Two Windows
//! latencies dominate it, and they are the reason the tolerance is what it is
//! rather than zero:
//!
//! - the compositor's, between the application handing over a frame and the
//!   frame being composed and stamped, which is up to a display refresh;
//! - the audio engine's, between the moment `IAudioClock` says a sample is
//!   played at the endpoint and the moment the loopback tap reports for the
//!   same sample.
//!
//! Neither is Clipped's to remove. Neither is *separable* here either, and that
//! cuts both ways: each of the two paths this prints holds one of those
//! latencies **and** whatever this recorder does on that side — the timestamp a
//! captured frame is reported with, the anchor an audio track is built from — so
//! a path is not an attribution and is not printed as one. What is bounded is
//! the total. `docs/av-sync.md` records the measured values and what can and
//! cannot be concluded about whose they are.
//!
//! # What it cannot measure
//!
//! Three things, and the printed result of the drift measurement means much
//! less without them.
//!
//! **A constant offset.** That measurement is relative. `clipped-audio` anchors
//! its track on the first packet's own device position, so the first observation
//! of a run is zero by construction, and everything after it is measured from
//! there. An error that was already present when the capture started — the
//! endpoint's own reporting bias, or a session that starts the audio at a
//! different moment from the video — is invisible in it at any size. What it
//! measures is the change. The absolute test above is what sees the constant,
//! and it is a different run with a different subject.
//!
//! **What a file ends up containing.** Nothing here writes one. `clipped-muxer`
//! rescales media times to 1 ms container ticks and clamps any timestamp before
//! the file's origin to the start of it (`docs/muxing.md`), and both of those
//! alter the offset a finished recording has in it. This measures the
//! timestamps the pipeline produces, which is what a writer is given, not what
//! it wrote. It is not an oversight and it is not deferred taste: no build of
//! this workspace writes a recording with an audio track in it yet — issue #126
//! wires capture to encode to mux, issue #180 puts audio in the file — and
//! measuring this same offset from a produced recording is issue #151.
//!
//! **Physical synchronisation** — whether the sound leaving the speakers and the
//! light leaving the panel are simultaneous. That needs a microphone and a
//! photodiode, and it would be measuring Windows' output latency rather than
//! this recorder.
//!
//! What is in scope is the timestamp domain: the recorder is responsible for
//! placing each source at the moment its own hardware said it happened, and that
//! is what is asserted. The fitted rate is printed with its standard error, so
//! that the part of it which is scatter rather than crystal is on the same line.
//!
//! # Why the endpoint is kept awake
//!
//! WASAPI loopback delivers nothing at all while the endpoint is idle, and a
//! period the device never described has no device position to disagree with —
//! so a run on a quiet machine measures nothing. [`SilenceKeeper`] holds a
//! render stream open for the length of the run and hands the audio engine
//! buffers marked as silence. That makes no sound whatsoever: it is not a quiet
//! tone, it is `AUDCLNT_BUFFERFLAGS_SILENT`, which is the audio engine's own way
//! of saying "these frames are zero". The endpoint's clock runs, packets flow,
//! and nobody hears anything.
//!
//! # Why it is `#[ignore]`d
//!
//! It needs a GPU, a display, an audio endpoint and minutes of wall-clock time,
//! and it puts a window on a display. Being `#[ignore]`d rather than deciding at
//! run time whether it applies is deliberate: what runs it is a person or a job
//! that meant to, rather than a filter inside the test.
//!
//! It does report two conditions and carry on rather than failing — the default
//! output device refusing a render stream, and an endpoint that delivered no
//! packets at all — because both mean the machine could not present the
//! measurement with anything to measure, and neither is a fault in the code
//! under test. Both print `SKIPPED (av-sync): …`, and both fail the run outright
//! when `CLIPPED_REQUIRE_AUDIO` is set, which is how CI and anybody collecting
//! evidence should run it. A green run without that variable set is not on its
//! own evidence that an offset was measured; the printed lines are.
//!
//! The absolute test does make a sound, because a subject that makes none is
//! exactly what it exists to fix. It is a [`TONE_LENGTH`] tone at about
//! −28 dBFS, once every five seconds of the run — quiet and brief on purpose,
//! since this runs on a machine somebody is using.
//!
//! ```text
//! # The default: about ninety seconds.
//! cargo test -p clipped-video-pattern --test av_sync -- --ignored --nocapture --test-threads=1
//!
//! # The long run the acceptance criteria ask for. CLIPPED_AV_SYNC_SECONDS is
//! # read by the same test.
//! CLIPPED_AV_SYNC_SECONDS=1800 cargo test -p clipped-video-pattern --test av_sync \
//!     -- --ignored --nocapture --test-threads=1 av_offset
//!
//! # The absolute offset, which plays a tone. CLIPPED_AV_SYNC_TONE_SECONDS
//! # lengthens it.
//! cargo test -p clipped-video-pattern --test av_sync \
//!     -- --ignored --nocapture --test-threads=1 absolute
//! ```

#![cfg(windows)]

mod readback;

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::Write as _;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use clipped_audio::windows::SystemAudioCapture;
use clipped_audio::{Capture, SampleOrigin};
use clipped_capture::{
    registered_backend, registered_declarations, select, Acquisition, CaptureClock, CaptureConfig,
    CaptureError, CaptureTarget, CaptureTimestamp, DriftEstimator, DriftRate, FrameSize, MediaTime,
    SourceClock, SyncState, SyncTolerance, TargetHandle, TargetKind, TargetProperties,
    DEFAULT_DISCONTINUITY_STEP,
};
use clipped_media_validation::AudioContent;
use clipped_video_pattern::harness::{Onset, TestApp, Tone, ToneEvent, TonePlan};
use clipped_video_pattern::pattern::{self, Surface};
use clipped_video_pattern::render_stream::{RenderStream, Samples};

use readback::FrameReader;

/// How long a run lasts unless [`RUN_SECONDS`] says otherwise.
///
/// Ninety seconds is long enough for the fit to mean something — nine thousand
/// audio packets — and short enough to be worth running on a whim. The thirty
/// minutes the acceptance criteria ask for is a deliberate act, not a default
/// somebody trips over.
const DEFAULT_RUN: Duration = Duration::from_secs(90);

/// The environment variable that lengthens a run, in seconds.
const RUN_SECONDS: &str = "CLIPPED_AV_SYNC_SECONDS";

/// How long a slice of the run the drift report describes on its own line.
///
/// One fitted rate over a whole run cannot tell a clock that is steadily a few
/// parts per million wrong from one that was right for fifty minutes and then
/// jumped: both produce the same slope, and they have different causes and
/// different fixes. So a run is also reported minute by minute — where the
/// offset had got to at the end of each minute, and the rate fitted inside that
/// minute alone. A steady clock shows the same rate in every slice; a step
/// shows one slice whose rate the others do not share, with the offset flat
/// either side of it.
///
/// A minute rather than anything finer because a slice has to be long enough
/// for its own fit to mean something: at 10 ms endpoint buffers a minute is
/// six thousand observations, and `DriftEstimator`'s own standard error says on
/// each line whether that was enough.
const TRACE_INTERVAL: Duration = Duration::from_secs(60);

/// How long the absolute measurement runs for unless
/// [`TONE_RUN_SECONDS`] says otherwise.
///
/// Ninety seconds is seventeen tones at the subject's five-second spacing,
/// which is enough independent readings for the spread of them to mean
/// something. It is deliberately not the thirty-minute figure: this run keeps
/// every sample it captures so that a tone can be found in them, and half an
/// hour of that is nearly a gigabyte for no gain — a constant offset does not
/// take longer to see than a variable one.
const DEFAULT_TONE_RUN: Duration = Duration::from_secs(90);

/// The environment variable that lengthens the absolute measurement, in
/// seconds.
const TONE_RUN_SECONDS: &str = "CLIPPED_AV_SYNC_TONE_SECONDS";

/// How long a tone lasts, as the subject renders it
/// (`test-apps/video-pattern/src/tone.rs`).
///
/// Named here as well because the detector checks the burst it found is this
/// long: a burst of a wildly different length is something else that happened
/// to be playing, not the subject's tone.
const TONE_LENGTH: Duration = Duration::from_millis(30);

/// How far either side of a tone's announced moment the detector looks.
///
/// It has to be wider than any offset that could plausibly be there — a quarter
/// of a second is four times the tolerance's own lag limit — and narrower than
/// half the spacing between tones, so that a search for one cannot find its
/// neighbour.
const SEARCH: Duration = Duration::from_millis(250);

/// The window each point of the envelope is measured over.
///
/// Two milliseconds is two cycles of the tone, which is enough for a Goertzel
/// filter to separate it from anything else in the room, and short enough that
/// the envelope follows a one-millisecond attack rather than smearing it. The
/// window is centred on the point it reports, so a symmetric attack's
/// half-amplitude point lands where the smoothed envelope crosses half its
/// plateau — which is the moment the subject announces (AGENTS.md section 26).
const ENVELOPE_WINDOW: Duration = Duration::from_micros(2_000);

/// How far apart the points of the envelope are.
///
/// A quarter of a millisecond, which is the detector's own resolution before
/// the interpolation between two points, and a fortieth of the smallest offset
/// worth reporting.
const ENVELOPE_HOP: Duration = Duration::from_micros(250);

/// How much louder than the noise floor a burst has to be to be the tone.
///
/// Six, the same margin `crates/audio/tests/system_audio.rs` settled on for the
/// same reason: the machine this runs on is a desktop somebody is using, and
/// whatever is already playing has some energy at 997 Hz. The burst's length is
/// checked as well, which background audio does not reproduce.
const MINIMUM_RATIO: f64 = 6.0;

/// How strong the tone's frequency has to be for a burst to be the tone.
///
/// The subject plays at about −28 dBFS, which measures 0.04 on the harness's
/// normalised scale where a full-scale sine is 1.0. A quarter of that is well
/// below anything the endpoint's own mixing could take off it and far above the
/// numerical noise a window of digital silence produces.
const MINIMUM_MAGNITUDE: f64 = 0.01;

/// How many tones have to be found for the absolute measurement to mean
/// anything.
///
/// The number quoted is a mean over the tones of a run, and the run's own
/// spread is what says whether the mean is a measurement or a coincidence. Five
/// is the fewest that gives a spread worth printing.
const MINIMUM_TONES: usize = 5;

/// How far a run's tones may scatter about their mean before the mean stops
/// being a measurement, in nanoseconds.
///
/// The offset each tone gives is a constant plus the two latencies the module
/// documentation names, and one of those — the compositor's present-to-compose —
/// varies by a display refresh or so from frame to frame: over the two runs in
/// `docs/av-sync.md` the video path of individual tones spread 6.7 to 28.5 ms,
/// for standard deviations of 5.7 and 4.3 ms in the offsets themselves.
/// Fifteen milliseconds is more than twice that and still a quarter of the
/// tolerance the mean is judged against, so a run whose readings scatter more
/// than this is measuring something other than a constant and says so rather
/// than averaging it away.
///
/// A standard deviation rather than the full range, because the range of a
/// dozen readings is decided by the two most extreme of them: one late compose
/// in a run should widen the error bar, not fail the test.
const MAXIMUM_DEVIATION: f64 = 15e6;

/// The most seconds of audio the absolute run keeps for analysis.
///
/// It keeps one channel of every frame so that a tone can be found in it, which
/// is 192 kB a second. Ten minutes of that is 115 MB and is far more than the
/// measurement needs; a longer run than this is a mistake at the command line
/// rather than a request, and the cap says so rather than filling memory.
const MAXIMUM_KEPT_SECONDS: u64 = 600;

/// The environment variable that turns "this machine has no audio endpoint"
/// from a skip into a failure.
const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

/// The frames per second the test pattern is presented at.
///
/// Thirty rather than sixty: the point of the subject is that the compositor
/// always has new content for the whole run, and thirty is enough for that at
/// half the cost on a machine somebody is using.
const PATTERN_FPS: u32 = 30;

/// One frame in this many is copied back and decoded.
///
/// Decoding every frame of a thirty-minute run would be fifty thousand
/// full-frame GPU readbacks, which measures the readback rather than the
/// capture. One a second is enough to prove the frames arriving are the
/// subject's, and enough to fit the source's presentation interval against the
/// reference clock.
const DECODE_EVERY: u64 = PATTERN_FPS as u64;

/// The largest drift rate that is a measurement rather than a fault, in parts
/// per million.
///
/// A commodity audio crystal is specified at ±50 ppm and the worst consumer
/// parts are a few hundred. A thousand — 60 ms per minute — is not a clock, it
/// is a broken conversion somewhere in this pipeline, and the test should say so
/// rather than dutifully reporting it.
const IMPLAUSIBLE_PPM: f64 = 1_000.0;

/// How many times the subject is started again after its window goes away.
///
/// It goes away for reasons this test does not control: the machine it runs on
/// belongs to somebody, the subject is a topmost window on one of their
/// displays, and half an hour is long enough for them to close it or for the
/// session to lock. Losing the whole run to that would mean the long
/// measurement could only be taken on an idle machine, which is not where
/// recording software has to work.
///
/// Three, not unlimited: if somebody is closing the window, putting it back for
/// half an hour is worse than stopping. After the third loss the video side
/// gives up and says so, and the audio side — which is where the offset is
/// actually measured — carries on.
const MAX_SUBJECT_RESTARTS: u32 = 3;

/// How much of a run the video capture has to cover for the run to count.
///
/// The A/V offset is measured from the audio endpoint's positions against the
/// reference clock, and that continues whatever happens to the window, so a
/// subject that dies late does not invalidate the measurement. What it does
/// invalidate is the claim that video and audio were captured *together*, so
/// there is a floor: half the run.
///
/// Coverage, not the interval from the first frame to the last. Those are the
/// same number until a subject has to be restarted, and after that the interval
/// includes the time nothing was being captured — which is exactly the case the
/// floor exists to catch.
const MINIMUM_VIDEO_COVERAGE: f64 = 0.5;

/// Reports that the test could not run here.
///
/// Written through `std::io::stderr()` rather than with `eprintln!` because
/// libtest captures the macros.
fn skipped(reason: &str) {
    if is_set(REQUIRE_AUDIO) {
        panic!("{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}");
    }
    let _ = writeln!(std::io::stderr(), "SKIPPED (av-sync): {reason}");
}

/// The environment variable that asks the tests which make a noise not to.
const SKIP_AUDIO: &str = "CLIPPED_SKIP_AUDIO";

/// Whether an environment variable is set to anything but the empty string.
fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Whether the caller should skip because the machine has been asked for quiet.
///
/// Consulted before a subject is started, because the subject is what plays the
/// tone. See `docs/testing.md`.
fn suppressed() -> bool {
    if !is_set(SKIP_AUDIO) {
        return false;
    }
    assert!(
        !is_set(REQUIRE_AUDIO),
        "{SKIP_AUDIO} and {REQUIRE_AUDIO} are both set. One says these tests          must not run and the other says they must not be skipped; there is no          behaviour that satisfies both, so neither is being guessed at."
    );
    skipped(&format!("{SKIP_AUDIO} is set"));
    true
}

fn note(message: &str) {
    let _ = writeln!(std::io::stderr(), "[av-sync] {message}");
}

fn run_length(variable: &str, default: Duration) -> Duration {
    match std::env::var(variable).ok().and_then(|value| {
        let seconds: u64 = value.trim().parse().ok()?;
        (seconds > 0).then(|| Duration::from_secs(seconds))
    }) {
        Some(length) => length,
        None => default,
    }
}

#[test]
#[ignore = "needs a GPU, a display, an audio endpoint and minutes of wall-clock time"]
fn av_offset_stays_within_tolerance_while_video_and_audio_are_captured_together() {
    if suppressed() {
        return;
    }

    let run = run_length(RUN_SECONDS, DEFAULT_RUN);

    let keeper = match SilenceKeeper::start() {
        Ok(keeper) => Some(keeper),
        Err(reason) => {
            // Not fatal on its own: the run still measures the video side and
            // whatever the machine happens to be playing. It is reported
            // loudly, because a run with no endpoint packets measures no drift.
            skipped(&format!("the endpoint could not be kept awake: {reason}"));
            None
        }
    };

    let mut audio = AudioRun::start(Keep::Nothing);

    let video = capture_video(run, Subject::Silent);

    let audio = audio.finish();
    drop(keeper);

    let report = Report::build(&video, &audio);
    report.print();
    report.assert_healthy(run);
}

/// The absolute offset: how far a recording puts a sound from the picture it
/// was simultaneous with at the source.
///
/// This is what the drift measurement above cannot do, and the difference is
/// entirely in the subject. It is started with `--tone`, so it places a short
/// sound at the moment it presents a named frame and announces both moments;
/// the recording is then searched for the tone by its frequency and for the
/// frame by its counter, and the two are compared against the two the
/// application announced.
///
/// It makes a sound — quietly, briefly, and every five seconds rather than
/// continuously. A measurement of where a recording puts a sound needs a sound.
#[test]
#[ignore = "needs a GPU, a display and an audio endpoint, and plays a quiet tone"]
fn the_absolute_av_offset_of_a_synchronised_subject_is_within_tolerance() {
    if suppressed() {
        return;
    }

    let run = run_length(TONE_RUN_SECONDS, DEFAULT_TONE_RUN);

    // No `SilenceKeeper` here: the subject holds a render stream open for its
    // whole run, which is what keeps the endpoint's clock going and loopback
    // delivering, and a second stream feeding silence would add nothing.
    let mut audio = AudioRun::start(Keep::Samples);
    let video = capture_video(run, Subject::Sounded);
    let audio = audio.finish();

    if let Some(reason) = &video.silent_subject {
        skipped(reason);
        return;
    }

    // The same health checks the drift measurement makes — the capture found
    // its subject, covered the run, and the two accounts of the audio's
    // position are independent — because an absolute offset measured over a
    // capture that was not working is not a measurement.
    let report = Report::build(&video, &audio);
    report.print();
    report.assert_healthy(run);

    let absolute = Absolute::measure(&video, &audio);
    absolute.print();
    absolute.assert_within(&SyncTolerance::default());
}

/// What the video side of a run produced.
#[derive(Debug, Default)]
struct VideoRun {
    /// Every delivered frame's source timestamp, in arrival order.
    timestamps: Vec<CaptureTimestamp>,
    /// How much of the run the capture actually had frames for: the sum of each
    /// subject's own first-to-last span, so that the downtime between a subject
    /// dying and the next one starting is not counted as covered.
    covered: Duration,
    /// How many subjects delivered at least one frame, which is how many pieces
    /// [`covered`](Self::covered) is the sum of.
    pieces: u32,
    /// `(pattern counter, source timestamp)` for the sampled frames of the
    /// longest single run of the subject. Only one run's worth, because the
    /// counter restarts at zero with the subject and a fit spanning a restart
    /// would read the reset as time going backwards.
    decoded: Vec<(u32, CaptureTimestamp)>,
    /// Sampled frames that decoded as the pattern, across every run.
    decoded_total: u64,
    timeouts: u64,
    undecodable: u64,
    backend_missed: u64,
    /// How many times the subject had to be started again because its window
    /// went away. See [`MAX_SUBJECT_RESTARTS`].
    restarts: u32,
    /// Which display the subject was last on.
    monitor: String,
    /// What ended the subject's own run, in its words.
    stopped: Option<String>,
    /// Every tone the subjects announced, paired with the capture timestamp of
    /// the frame it belonged to where that frame was captured and decoded.
    tones: Vec<ToneObservation>,
    /// Tones the subject announced whose frame never arrived in the capture, or
    /// arrived and did not decode.
    tones_without_a_frame: usize,
    /// Tones the subject announced and did not play, because it could not put
    /// them at the moment it wanted.
    tones_unplayed: usize,
    /// Tones the subject played without saying where it put them: its render
    /// thread had not reported the placement by the time it presented the
    /// frame. Counted apart from the unplayed ones, because a sound that was
    /// made and not reported is a different fault from one never made
    /// ([`Onset`]).
    tones_unreported: usize,
    /// Set when a sounded run was asked for and the machine could not play a
    /// tone. The run is then a skip rather than a failure: no endpoint is not a
    /// fault in the code under test.
    silent_subject: Option<String>,
}

/// One announced tone, with everything needed to place both halves of it.
#[derive(Debug, Clone, Copy)]
struct ToneObservation {
    /// The counter of the frame the tone belongs to.
    frame: u32,
    /// Where the endpoint's clock put the tone's half-amplitude point, at the
    /// source.
    onset_nanos: u64,
    /// The counter of the frame the video half is actually measured from.
    ///
    /// Usually the tone's own frame. It is allowed to be a near neighbour
    /// because the compositor does not compose every frame an application
    /// presents — on a display it is not showing it composes a small fraction
    /// of them — and the video path latency is a property of the pipeline
    /// rather than of one frame, so the frame nearest the tone measures it just
    /// as well. How far away it was is reported.
    matched_frame: u32,
    /// The moment the source presented [`matched_frame`](Self::matched_frame).
    ///
    /// The announced present of the tone's own frame, moved by the source's
    /// frame interval for each frame between the two. The subject paces
    /// presents against a fixed schedule, so that interval is exact to within
    /// the sub-millisecond jitter it announces per tone.
    matched_present_nanos: u64,
    /// What the capture backend stamped
    /// [`matched_frame`](Self::matched_frame) with.
    captured_nanos: u64,
    /// The frequency the subject said it was playing, which is the one the
    /// detector looks for. Taken from the announcement rather than from a
    /// constant here, so that the two cannot drift apart.
    hertz: f64,
}

/// Whether the subject is asked to make a sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subject {
    /// The window and nothing else, which is what the drift measurement wants.
    Silent,
    /// `--tone`: a short sound at the moment a named frame is presented.
    Sounded,
}

/// Whether a run keeps the samples it captures.
///
/// Only the absolute measurement needs them — it has to find a tone in what was
/// recorded — and keeping them costs 192 kB a second, which over the drift
/// measurement's half hour would be a third of a gigabyte nothing reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// Timestamps only.
    Nothing,
    /// One channel of every frame, and where each buffer starts in it.
    Samples,
}

/// What the audio side of a run produced.
#[derive(Debug, Default)]
struct AudioRun {
    /// `(endpoint position, track position)` in nanoseconds on the performance
    /// counter, one pair per buffer the endpoint delivered.
    observations: Vec<(u64, u64)>,
    /// The first channel of every frame handed over, in order, when the run was
    /// asked to keep them.
    mono: Vec<f32>,
    /// Where each buffer starts in [`mono`](Self::mono), and both accounts of
    /// when its first frame was heard.
    blocks: Vec<Block>,
    /// Set when [`MAXIMUM_KEPT_SECONDS`] was reached and the run stopped
    /// keeping samples, so that a measurement over the tail knows the samples
    /// are missing rather than the tone.
    truncated: bool,
    /// Frames of silence this crate synthesised because the endpoint said
    /// nothing.
    synthesised_frames: u64,
    /// Frames handed over in total, real and synthesised.
    frames: u64,
    sample_rate: u32,
    /// Every endpoint the capture was on, in the order it was on them. More
    /// than one means the default output device changed under the run.
    endpoints: Vec<String>,
    /// How many times the capture reported that it could not follow the new
    /// default endpoint and continued as silence.
    format_changes: u64,
    failed: Option<String>,
}

/// The audio side, running on its own thread for the length of the run.
///
/// One capture, one thread, as `clipped-audio`'s module documentation requires:
/// `SystemAudioCapture` is `Send` and not `Sync`, and reading it blocks, so it
/// is opened on the thread that will read it and never touched from anywhere
/// else. Nothing is shared with the video thread except an [`AtomicBool`].
#[derive(Debug)]
struct AudioThread {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<AudioRun>>,
}

/// One buffer's place in [`AudioRun::mono`], and both accounts of when its
/// first frame was heard.
///
/// Both, because they answer different questions. The track's timestamp is
/// where the *recorder* puts the samples, which is what a writer is handed and
/// therefore what a finished recording would contain; the endpoint's is where
/// the device said they belong. The absolute measurement reports the offset
/// from the first and prints the second beside it, and the difference between
/// the two is the drift the other test in this file measures.
#[derive(Debug, Clone, Copy)]
struct Block {
    /// Where this buffer's first frame is in [`AudioRun::mono`].
    start: usize,
    /// The track's timestamp for that frame, in nanoseconds.
    track_nanos: u64,
    /// The endpoint's own position for it, when it had one.
    device_nanos: Option<u64>,
}

impl AudioRun {
    fn start(keep: Keep) -> AudioThread {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || Self::run(&stop, keep)
        });
        AudioThread {
            stop,
            thread: Some(thread),
        }
    }

    /// The moment in the recording of the sample at `index` of
    /// [`mono`](Self::mono), by the track's account and by the endpoint's.
    ///
    /// The track is contiguous by construction, so a sample's moment is its
    /// buffer's timestamp plus its offset into that buffer — no interpolation
    /// between buffers and no assumption that the buffers are the same length.
    fn moment_of(&self, index: f64) -> Option<(f64, Option<f64>)> {
        let block = match self
            .blocks
            .binary_search_by_key(&(index as usize), |block| block.start)
        {
            Ok(exact) => self.blocks[exact],
            Err(0) => return None,
            Err(after) => self.blocks[after - 1],
        };
        let into = (index - block.start as f64) * 1e9 / f64::from(self.sample_rate.max(1));
        Some((
            block.track_nanos as f64 + into,
            block.device_nanos.map(|device| device as f64 + into),
        ))
    }

    /// The index into [`mono`](Self::mono) of the sample the track puts at
    /// `nanos`.
    fn index_of(&self, nanos: u64) -> Option<f64> {
        let block = match self
            .blocks
            .binary_search_by_key(&nanos, |block| block.track_nanos)
        {
            Ok(exact) => self.blocks[exact],
            Err(0) => return None,
            Err(after) => self.blocks[after - 1],
        };
        let into = (nanos - block.track_nanos) as f64 * f64::from(self.sample_rate.max(1)) / 1e9;
        let index = block.start as f64 + into;
        (index < self.mono.len() as f64).then_some(index)
    }

    fn run(stop: &AtomicBool, keep: Keep) -> Self {
        let mut run = Self::default();

        let mut capture = match SystemAudioCapture::open() {
            Ok(capture) => capture,
            Err(error) => {
                run.failed = Some(format!("system audio capture could not be opened: {error}"));
                return run;
            }
        };
        // The track's format, not the endpoint's. `clipped-audio` never changes
        // the shape of what it hands over mid-capture: an endpoint it cannot
        // follow becomes silence in this format and is reported as
        // `FormatChanged` (`crates/audio/src/error.rs`). So this rate stays the
        // right divisor for the frame count for the whole run.
        run.sample_rate = capture.format().sample_rate().get();
        let mut endpoint = capture.endpoint_name().unwrap_or("<none>").to_owned();
        run.endpoints.push(endpoint.clone());

        while !stop.load(Ordering::Relaxed) {
            match capture.read(Duration::from_millis(100)) {
                Ok(Capture::Samples(samples)) => {
                    run.frames += samples.frames() as u64;
                    if samples.origin() == SampleOrigin::SynthesisedSilence {
                        run.synthesised_frames += samples.frames() as u64;
                    }
                    if let Some(device) = samples.device_timestamp() {
                        run.observations
                            .push((device.as_nanos(), samples.timestamp().as_nanos()));
                    }
                    if keep == Keep::Samples {
                        let kept = u64::from(run.sample_rate) * MAXIMUM_KEPT_SECONDS;
                        if run.mono.len() as u64 >= kept {
                            run.truncated = true;
                        } else {
                            run.blocks.push(Block {
                                start: run.mono.len(),
                                track_nanos: samples.timestamp().as_nanos(),
                                device_nanos: samples.device_timestamp().map(|at| at.as_nanos()),
                            });
                            // One channel: the subject writes the same value to
                            // every channel of a frame, and a detector looking
                            // for one frequency has no use for the others.
                            let channels = usize::from(samples.format().channels().get());
                            run.mono
                                .extend(samples.samples().iter().step_by(channels.max(1)));
                        }
                    }
                }
                Ok(Capture::Idle) => {}
                Ok(Capture::FormatChanged(format)) => {
                    // The default endpoint moved to one this capture cannot
                    // follow, so the track continues as silence in the original
                    // format. There are no more device positions to compare
                    // against from here on, which is worth saying loudly: the
                    // rest of the run measures nothing.
                    run.format_changes += 1;
                    note(&format!(
                        "the default output device changed to {format}, which this capture \
                         cannot follow; the track continues as silence and no further A/V \
                         offset can be observed"
                    ));
                }
                Err(error) => {
                    run.failed = Some(format!("the audio capture failed mid-run: {error}"));
                    break;
                }
            }

            // Read after the match, because the borrow the samples held on the
            // capture has ended by here. An endpoint can also be replaced by an
            // *interchangeable* one — same rate, same channels — which the
            // capture follows silently and reports through no event at all. It
            // is still a different crystal, so the report has to name every
            // device it was on rather than the one it opened with.
            let current = capture.endpoint_name().unwrap_or("<none>");
            if current != endpoint {
                note(&format!(
                    "the endpoint changed from {endpoint:?} to {current:?} mid-run; the \
                     drift measured before the change is a different crystal's"
                ));
                endpoint = current.to_owned();
                run.endpoints.push(endpoint.clone());
            }
        }

        capture.close();
        run
    }
}

impl AudioThread {
    fn finish(&mut self) -> AudioRun {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .expect("a run is finished once")
            .join()
            .expect("the audio thread should not panic")
    }
}

impl Drop for AudioThread {
    fn drop(&mut self) {
        // Only reached when the video side panicked, which is exactly when a
        // thread holding an open audio endpoint must not be left running
        // (AGENTS.md section 58).
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the subject, asking it to outlive the rest of the run by a margin so
/// that the measurement is never cut short by the application's own deadline.
///
/// It still stops when this test closes its standard input, and `TestApp::drop`
/// kills it if it does not, so the margin costs nothing.
fn start_subject(remaining: Duration, subject: Subject) -> Result<TestApp, String> {
    let mut arguments = vec![
        "--fps".to_owned(),
        PATTERN_FPS.to_string(),
        "--seconds".to_owned(),
        (remaining.as_secs() + 120).to_string(),
        "--mode".to_owned(),
        "borderless".to_owned(),
    ];
    if subject == Subject::Sounded {
        arguments.push("--tone".to_owned());
    }

    TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        arguments,
        Duration::from_secs(20),
    )
    .map_err(|error| error.to_string())
}

/// Captures the subject's window for `run`, keeping every frame's timestamp and
/// decoding one frame a second.
///
/// Owns the subject, because it may have to start it again: a window on a
/// display belonging to somebody who is using the machine can be closed, and a
/// half-hour run that ends the moment that happens measures nothing. A lost
/// target is reported, the subject is started again, and the run continues —
/// [`MAX_SUBJECT_RESTARTS`] times, after which the video side gives up and the
/// audio side, which is where the offset is actually measured, carries on to the
/// deadline.
fn capture_video(run: Duration, subject: Subject) -> VideoRun {
    let mut video = VideoRun::default();
    let deadline = Instant::now() + run;
    let mut method = None;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        let mut app = match start_subject(remaining, subject) {
            Ok(app) => app,
            Err(reason) => {
                note(&format!("the subject could not be started: {reason}"));
                break;
            }
        };
        video.monitor = app.monitor().to_owned();

        // The plan is per subject, because a restarted subject counts its
        // frames from zero again and announces a plan of its own.
        let plan = match (subject, app.tone()) {
            (Subject::Sounded, Tone::Playing(plan)) => Some(plan),
            (Subject::Sounded, unavailable) => {
                video.silent_subject = Some(format!(
                    "the subject could not play a tone on this machine ({unavailable:?}), so \
                     there is no event whose sound and picture are simultaneous to measure; \
                     it says why on its standard error"
                ));
                return video;
            }
            (Subject::Silent, _) => None,
        };
        if let Some(plan) = plan {
            note(&format!(
                "the subject is playing a {:.0} Hz tone of {} ms at frame {} and every {} \
                 frames after it",
                plan.frequency,
                plan.length.as_millis(),
                plan.first_frame,
                plan.frame_interval,
            ));
        }

        let (width, height) = app.client_size();
        let size = FrameSize::new(width, height).expect("the application announced a real size");
        let properties = TargetProperties::new(TargetKind::Window, size);

        let selection = select(
            &registered_declarations(),
            &properties,
            clipped_capture::CaptureMethodSetting::Automatic,
        )
        .expect("this machine should have a capture backend for a window");
        let factory = registered_backend(selection.method())
            .expect("selection only ever chooses a registered backend");
        let mut backend = factory.create().expect("the backend should be creatable");

        let target = CaptureTarget::new(TargetHandle::from_raw(app.window() as u64), properties);
        let format = backend
            .initialise(
                &target,
                &CaptureConfig::default().with_capture_cursor(false),
            )
            .expect("capturing the test application's window should start");
        if method.is_none() {
            note(&format!(
                "capturing {} through {} at {format} for {:.0} s",
                video.monitor,
                selection.method(),
                run.as_secs_f64()
            ));
            method = Some(selection.method());
        }

        // The counter is the subject's own and restarts at zero with it, so the
        // frames decoded from one subject are fitted separately from the next
        // one's — a fit spanning a restart would read the reset as a jump back
        // in time.
        let mut decoded_here = Vec::new();
        let mut region = None;
        // A new subject means a new backend and therefore a new Direct3D
        // device, and the reader caches the device and the staging texture it
        // copies into. Carrying one across a restart is reading a texture on a
        // device that no longer exists, which fails as
        // `DXGI_ERROR_DEVICE_REMOVED` several frames later — so the reader is
        // built beside the backend it reads from and dies with it.
        let mut reader = FrameReader::default();
        // Where this subject's timestamps start in the run's list, so that what
        // it covered can be added up separately from the gap before the next
        // one starts.
        let first_of_this_subject = video.timestamps.len();
        let mut hunt = ToneHunt::new(plan);
        let lost = loop {
            if Instant::now() >= deadline {
                break false;
            }
            match backend.acquire(Duration::from_millis(100)) {
                Ok(Acquisition::Frame(frame)) => {
                    let timestamp = frame.timestamp();
                    video.timestamps.push(timestamp);
                    let missed = frame.frames_missed().unwrap_or(0);
                    video.backend_missed += u64::from(missed);
                    hunt.arrived(missed);

                    // One frame a second is enough to prove the frames being
                    // timed are the subject's; a frame carrying a tone has to
                    // be decoded whenever it arrives, because it is half of the
                    // event being measured and there is no second chance at it.
                    if video.timestamps.len() as u64 % DECODE_EVERY != 0 && !hunt.closing_in() {
                        continue;
                    }

                    let image = reader.read(frame.texture()).unwrap_or_else(|error| {
                        panic!("a captured frame could not be read: {error}")
                    });
                    let surface =
                        Surface::new(&image.pixels, image.stride, image.width, image.height)
                            .expect("a mapped texture describes itself");

                    let found = match region {
                        Some(found) => found,
                        None => match pattern::locate(&surface, width, height) {
                            Some(found) => {
                                region = Some(found);
                                found
                            }
                            // The first frames of a capture can be the window
                            // before it had drawn anything.
                            None => continue,
                        },
                    };

                    match pattern::decode(&surface, found) {
                        Ok(decoded) => {
                            video.decoded_total += 1;
                            decoded_here.push((decoded.index(), timestamp));
                            hunt.saw(decoded.index());
                        }
                        Err(_) => video.undecodable += 1,
                    }
                }
                Ok(Acquisition::Timeout) => video.timeouts += 1,
                // Nothing here minimises the subject, and a run in which
                // something did is a run whose drift figures describe Alt-Tab
                // rather than audio and video sliding apart.
                Ok(Acquisition::TargetMinimised) => panic!(
                    "the subject window was minimised during the run, so no figure measured \
                     here means anything; run it again with the desktop left alone"
                ),
                Ok(Acquisition::SizeChanged(size)) => panic!(
                    "the captured window changed size to {size} during a run that never \
                     resized it"
                ),
                Err(CaptureError::TargetLost { .. }) => break true,
                Err(error) => panic!("the capture failed during the run: {error}"),
            }
        };

        // Read before the subject is stopped or dropped, because the
        // announcements are on the pipe this is about to close.
        if let Some(plan) = plan {
            let announced = app.tones().unwrap_or_else(|error| {
                panic!("the subject announced a tone this test could not read: {error}")
            });
            pair_tones(&mut video, plan, &announced, &decoded_here);
        }

        if decoded_here.len() > video.decoded.len() {
            video.decoded = decoded_here;
        }

        let this_subject = &video.timestamps[first_of_this_subject..];
        if let (Some(first), Some(last)) = (this_subject.first(), this_subject.last()) {
            video.covered += last.duration_since(*first).unwrap_or_default();
            video.pieces += 1;
        }

        if !lost {
            // The run reached its deadline with the subject still up, which is
            // the ordinary case: stop it cleanly and record what it said.
            match app.stop(Duration::from_secs(10)) {
                Ok(summary) => {
                    video.stopped = Some(format!(
                        "{} frames presented, stopped by {}",
                        summary.frames, summary.reason
                    ));
                }
                Err(error) => panic!("the test application did not stop cleanly: {error}"),
            }
            break;
        }

        // The window went away under the capture. `TestApp::drop` closes
        // standard input and kills whatever is left of the process, so the
        // restart below starts from a clean slate.
        video.restarts += 1;
        note(&format!(
            "the subject's window went away {:.0} s into the run; starting it again \
             (restart {} of {MAX_SUBJECT_RESTARTS})",
            run.as_secs_f64()
                - deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64(),
            video.restarts,
        ));
        drop(app);

        if video.restarts >= MAX_SUBJECT_RESTARTS {
            note(
                "the subject would not stay up, so the video side of this run stops here; \
                 the audio side continues to the deadline",
            );
            break;
        }
    }

    // The audio side is still running, and it is where the offset is measured.
    // Leaving early because the video side gave up would shorten the very
    // measurement this run exists to take, so the deadline is waited out.
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        note(&format!(
            "the video side finished early; waiting out the remaining {:.0} s so the \
             audio side still covers the whole run",
            remaining.as_secs_f64()
        ));
        std::thread::sleep(remaining);
    }

    video
}

/// Everything measured, in one place, so that the printing and the assertions
/// read the same numbers.
#[derive(Debug)]
struct Report {
    clock: CaptureClock,
    video_frames: usize,
    /// First frame to last, downtime between subjects included.
    video_span: Duration,
    /// The sum of each subject's own first-to-last span: how much of the run
    /// the capture had frames for. Equal to [`Self::video_span`] unless the
    /// subject had to be restarted.
    video_covered: Duration,
    /// How many pieces [`Self::video_covered`] is the sum of.
    video_pieces: u32,
    video_timeouts: u64,
    video_missed: u64,
    video_undecodable: u64,
    video_restarts: u32,
    video_monitor: String,
    video_stopped: Option<String>,
    decoded: u64,
    source_interval_nanos: Option<f64>,
    audio: AudioSummary,
    estimator: DriftEstimator,
    /// The run cut into [`TRACE_INTERVAL`] slices, in order.
    trace: Vec<Slice>,
    tolerance: SyncTolerance,
}

/// One [`TRACE_INTERVAL`]'s worth of the run, fitted on its own.
///
/// The point of fitting a slice separately from the run is that a slice's rate
/// describes only what happened inside it, so comparing the slices to each
/// other is what says whether the drift was steady. The offset, by contrast, is
/// still measured from the run's first observation — a slice-relative offset
/// would hide exactly the accumulation the run exists to show.
#[derive(Debug)]
struct Slice {
    /// Seconds from the run's first observation to this slice's last one.
    until_seconds: f64,
    /// The offset at this slice's last observation, in nanoseconds, measured
    /// from the run's first observation.
    offset_nanos: i64,
    /// The rate fitted to this slice's observations alone, when it had a long
    /// enough uninterrupted segment for one.
    rate: Option<DriftRate>,
    /// The standard error of [`Self::rate`].
    rate_error: Option<DriftRate>,
    observations: u64,
    /// How many times the estimator decided this slice's observations had a
    /// discontinuity in them — a step it will not fit a rate across.
    discontinuities: u64,
}

impl Slice {
    /// Reads a finished slice out of the estimator that fitted it.
    fn of(slice: &DriftEstimator, until_seconds: f64, offset_nanos: i64) -> Self {
        Self {
            until_seconds,
            offset_nanos,
            rate: slice.rate(),
            rate_error: slice.rate_standard_error(),
            observations: slice.observations(),
            discontinuities: slice.discontinuities(),
        }
    }
}

#[derive(Debug)]
struct AudioSummary {
    endpoints: Vec<String>,
    format_changes: u64,
    sample_rate: u32,
    frames: u64,
    synthesised_frames: u64,
    endpoint_buffers: usize,
    failed: Option<String>,
    first_media: Option<MediaTime>,
    last_media: Option<MediaTime>,
}

impl Report {
    fn build(video: &VideoRun, audio: &AudioRun) -> Self {
        let first = *video
            .timestamps
            .first()
            .expect("a run with no video frames at all has nothing to measure");
        let last = *video
            .timestamps
            .last()
            .expect("a run with at least one frame has a last one");

        // The epoch is the first frame the recording keeps, which is what a
        // session will use. Audio that arrived before it gets a negative media
        // time, which is representable and is exactly the point.
        let clock = CaptureClock::start_at(first);

        let mut estimator = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
        let mut first_media = None;
        let mut last_media = None;

        // The slice being accumulated, and where the run had got to at its last
        // observation. A slice is fitted by its own estimator rather than by
        // arithmetic here, so that every rate this file prints — the run's and
        // each slice's — comes out of the same fit (AGENTS.md section 55).
        let mut trace: Vec<Slice> = Vec::new();
        let mut slice = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
        let mut slice_offset = 0_i64;
        let mut slice_until = 0.0_f64;
        let mut origin: Option<MediaTime> = None;
        let mut boundary = TRACE_INTERVAL.as_secs_f64();

        for (device, track) in &audio.observations {
            let reference = clock
                .media_time_on(SourceClock::PerformanceCounter, *device)
                .expect("the audio endpoint reports positions on the performance counter");
            let observed = clock
                .media_time_on(SourceClock::PerformanceCounter, *track)
                .expect("the audio track is timed on the performance counter");
            let offset = estimator.observe(reference, observed);
            first_media.get_or_insert(observed);
            last_media = Some(observed);

            let start = *origin.get_or_insert(reference);
            let elapsed = reference.nanos_since(start) as f64 / 1e9;
            if elapsed >= boundary {
                if slice.observations() > 0 {
                    trace.push(Slice::of(&slice, slice_until, slice_offset));
                    slice = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
                }
                // A run with a real gap in it can cross several boundaries at
                // once; the slices it produced nothing for are not invented.
                while boundary <= elapsed {
                    boundary += TRACE_INTERVAL.as_secs_f64();
                }
            }
            slice.observe(reference, observed);
            slice_offset = offset;
            slice_until = elapsed;
        }
        // The run ends where it ends, so the last slice is a partial one. A
        // fraction of a second of it fits a rate of hundreds of parts per
        // million with an error bar wider still, which is noise wearing the
        // same units as the answer; the run's own final offset is already
        // printed above, so a runt slice adds nothing worth the confusion.
        let covered = slice_until - trace.last().map_or(0.0, |last| last.until_seconds);
        if slice.observations() > 0 && covered >= TRACE_INTERVAL.as_secs_f64() / 10.0 {
            trace.push(Slice::of(&slice, slice_until, slice_offset));
        }

        Self {
            clock,
            video_frames: video.timestamps.len(),
            video_span: last.duration_since(first).unwrap_or_default(),
            video_covered: video.covered,
            video_pieces: video.pieces,
            video_timeouts: video.timeouts,
            video_missed: video.backend_missed,
            video_undecodable: video.undecodable,
            video_restarts: video.restarts,
            video_monitor: video.monitor.clone(),
            video_stopped: video.stopped.clone(),
            decoded: video.decoded_total,
            source_interval_nanos: source_interval(&video.decoded),
            audio: AudioSummary {
                endpoints: audio.endpoints.clone(),
                format_changes: audio.format_changes,
                sample_rate: audio.sample_rate,
                frames: audio.frames,
                synthesised_frames: audio.synthesised_frames,
                endpoint_buffers: audio.observations.len(),
                failed: audio.failed.clone(),
                first_media,
                last_media,
            },
            estimator,
            trace,
            tolerance: SyncTolerance::default(),
        }
    }

    fn print(&self) {
        let millis = |nanos: i64| nanos as f64 / 1e6;

        note(&format!("epoch {}", self.clock.epoch()));
        note(&format!(
            "subject: on {}, {} restart(s); {}",
            self.video_monitor,
            self.video_restarts,
            self.video_stopped
                .as_deref()
                .unwrap_or("it did not report a clean stop"),
        ));
        note(&format!(
            "video: {} frames covering {:.3} s of the run, {} acquisition timeouts, \
             {} frames missed, {} of {} sampled frames undecodable",
            self.video_frames,
            self.video_covered.as_secs_f64(),
            self.video_timeouts,
            self.video_missed,
            self.video_undecodable,
            self.decoded + self.video_undecodable,
        ));
        if self.video_restarts > 0 {
            note(&format!(
                "video: the first frame to the last spans {:.3} s, so {:.3} s of that was \
                 downtime between subjects and is not counted as covered",
                self.video_span.as_secs_f64(),
                self.video_span
                    .saturating_sub(self.video_covered)
                    .as_secs_f64(),
            ));
        }
        if let Some(interval) = self.source_interval_nanos {
            note(&format!(
                "video: the source presented a frame every {:.4} ms measured against the \
                 reference clock (nominal {:.4} ms at {PATTERN_FPS} fps)",
                interval / 1e6,
                1e3 / f64::from(PATTERN_FPS),
            ));
        }
        note(&format!(
            "audio: endpoint {} at {} Hz, {} frames ({:.3} s), {} synthesised, \
             {} endpoint buffers",
            if self.audio.endpoints.is_empty() {
                "<none>".to_owned()
            } else {
                self.audio
                    .endpoints
                    .iter()
                    .map(|name| format!("{name:?}"))
                    .collect::<Vec<_>>()
                    .join(", then ")
            },
            self.audio.sample_rate,
            self.audio.frames,
            self.audio.frames as f64 / f64::from(self.audio.sample_rate.max(1)),
            self.audio.synthesised_frames,
            self.audio.endpoint_buffers,
        ));
        if self.audio.format_changes > 0 {
            note(&format!(
                "audio: {} format change(s), after each of which the track is synthesised \
                 silence with no device position of its own and contributes no observations",
                self.audio.format_changes,
            ));
        }
        if let (Some(first), Some(last)) = (self.audio.first_media, self.audio.last_media) {
            note(&format!(
                "audio: track media time runs from {:.3} ms to {:.3} ms",
                millis(first.as_nanos()),
                millis(last.as_nanos()),
            ));
        }

        // Printed beside the numbers rather than left to the document, because
        // the numbers are what gets quoted.
        note(
            "A/V offset: the track's position minus the endpoint's, anchored on the first \
             buffer — so these are the change over the run, and any constant offset the \
             two already had is not in them",
        );
        match (self.estimator.first(), self.estimator.latest()) {
            (Some(first), Some(latest)) => note(&format!(
                "A/V offset: first {:+.3} ms, last {:+.3} ms, peak {:+.3} ms, {} \
                 ({} observations, {} discontinuities, tolerance {})",
                millis(first),
                millis(latest),
                millis(self.estimator.peak()),
                self.estimator
                    .state(&self.tolerance)
                    .map_or_else(|| "no observations".to_owned(), |state| state.to_string()),
                self.estimator.observations(),
                self.estimator.discontinuities(),
                self.tolerance,
            )),
            _ => note("A/V offset: the endpoint delivered nothing, so there is nothing to measure"),
        }

        match self.estimator.rate() {
            Some(rate) => {
                // The budget is the limit the offset is moving *towards*. A
                // negative rate is a track stamping events early, so it spends
                // the lead allowance; a positive one spends the lag allowance.
                // They differ by half, so reporting the wrong one overstates
                // the headroom by 50% (EBU R37: 40 ms of lead, 60 ms of lag).
                let (limit, direction) = if rate.as_ratio() < 0.0 {
                    (self.tolerance.ahead(), "lead")
                } else {
                    (self.tolerance.behind(), "lag")
                };
                note(&format!(
                    "drift: {rate}, standard error {} over {:.1} s of correction-free \
                     capture; at that rate the tolerance ({} ms of {direction}) is reached \
                     after {} of recording",
                    self.estimator.rate_standard_error().map_or_else(
                        || "unknown".to_owned(),
                        |error| format!("{:.4} ppm", error.parts_per_million())
                    ),
                    self.estimator.segment_span_nanos() as f64 / 1e9,
                    limit.as_millis(),
                    rate.time_to(limit).map_or_else(
                        || "never".to_owned(),
                        |budget| format!("{:.1} minutes", budget.as_secs_f64() / 60.0)
                    ),
                ));
            }
            None => note("drift: not enough of a correction-free segment to fit a rate to"),
        }

        self.print_trace();
    }

    /// Prints where the offset had got to at the end of every
    /// [`TRACE_INTERVAL`], and the rate fitted inside that interval alone.
    ///
    /// This is the shape of the drift rather than its size, and the two answer
    /// different questions. The run's fitted rate says how far apart the track
    /// and the picture end up; only the slices say whether they got there
    /// steadily. A rate that is the same in every slice is a crystal running at
    /// its own speed, which is what resampling corrects. A rate that is near
    /// zero in every slice but one is a single event — a gap filled, a stream
    /// reopened, a device swapped — and resampling is the wrong answer to it.
    fn print_trace(&self) {
        if self.trace.len() < 2 {
            return;
        }
        note(&format!(
            "drift by {}s slice: elapsed, offset from the run's first observation, and the \
             rate fitted inside that slice alone",
            TRACE_INTERVAL.as_secs(),
        ));
        for slice in &self.trace {
            note(&format!(
                "  {:>8.1} s  {:+9.3} ms  {:>12}  se {:>10}  {} obs{}",
                slice.until_seconds,
                slice.offset_nanos as f64 / 1e6,
                slice.rate.map_or_else(
                    || "-".to_owned(),
                    |rate| format!("{:+.3} ppm", rate.parts_per_million())
                ),
                slice.rate_error.map_or_else(
                    || "-".to_owned(),
                    |error| format!("{:.3} ppm", error.parts_per_million())
                ),
                slice.observations,
                if slice.discontinuities > 0 {
                    format!(", {} discontinuity(ies)", slice.discontinuities)
                } else {
                    String::new()
                },
            ));
        }
    }

    fn assert_healthy(&self, run: Duration) {
        if let Some(failure) = &self.audio.failed {
            panic!("the audio side of the run did not complete: {failure}");
        }

        assert!(
            self.video_frames > 0,
            "the capture delivered no frames at all"
        );
        assert!(
            self.video_undecodable == 0,
            "{} of the sampled frames were not the test pattern, so the frames being \
             timed may not be the subject's",
            self.video_undecodable
        );
        assert!(
            self.decoded > 0,
            "no sampled frame decoded as the test pattern, so nothing proves the capture \
             found its subject"
        );

        // The video capture has to have covered the run. Summed per subject,
        // not first frame to last: a subject that dies at 5 s and comes back at
        // 60 s of a 70 s run spans 65 s while covering 15, and this assertion
        // is the only thing tying the offset measurement to the claim that
        // video and audio were captured together.
        assert!(
            self.video_covered.as_secs_f64() > run.as_secs_f64() * MINIMUM_VIDEO_COVERAGE,
            "the video capture covered {:.1} s of a {:.1} s run, in {} piece(s) spanning \
             {:.1} s, which is too little of it to claim video and audio were captured \
             together",
            self.video_covered.as_secs_f64(),
            run.as_secs_f64(),
            self.video_pieces,
            self.video_span.as_secs_f64(),
        );

        let Some(state) = self.estimator.state(&self.tolerance) else {
            skipped(
                "the endpoint delivered no packets during the run, so no A/V offset could be \
                 measured; something has to be rendering for the device clock to run",
            );
            return;
        };

        assert_eq!(
            state,
            SyncState::InTolerance,
            "the audio track ended {:+.3} ms from the reference clock, outside {}",
            self.estimator.latest().unwrap_or_default() as f64 / 1e6,
            self.tolerance,
        );
        assert_eq!(
            self.tolerance.classify(self.estimator.peak()),
            SyncState::InTolerance,
            "the audio track reached {:+.3} ms from the reference clock during the run, \
             outside {}",
            self.estimator.peak() as f64 / 1e6,
            self.tolerance,
        );

        // The two positions being compared have to be genuinely independent
        // accounts of the same moment. If `device_timestamp` were quietly the
        // track's own timestamp, every assertion above would pass for ever and
        // drift would be permanently invisible — the worst possible outcome for
        // this test, because it would read as proof of perfect synchronisation.
        // Over thousands of buffers the sample count and the counter cannot
        // agree to the nanosecond every single time.
        assert_ne!(
            self.estimator.peak(),
            0,
            "the endpoint's positions and the track's never differed by a nanosecond \
             across {} buffers, which means they are the same number rather than in \
             agreement",
            self.estimator.observations(),
        );

        if let Some(rate) = self.estimator.rate() {
            assert!(
                rate.parts_per_million().abs() < IMPLAUSIBLE_PPM,
                "a drift of {rate} is not a crystal, it is a broken conversion"
            );
        }
    }
}

/// How far from a tone's own frame the frame measuring the video half may be.
///
/// The same [`NEAR_TONE`] the run decodes around a tone, and deliberately not
/// wider. The video half of a match is measured on the frame that was found,
/// and the source moment for that frame is the announced present moved by
/// [`SOURCE_FRAME_NANOS`] per frame — an extrapolation that assumes the subject
/// kept its nominal pacing, which is exactly the assumption it abandons when it
/// falls behind. Six frames of it is bounded by a couple of milliseconds even
/// on a subject presenting at 28 rather than 30 frames a second; half a second
/// of it would put tens of milliseconds into a single tone's reading with
/// nothing but the standard-deviation check to catch it.
///
/// Frames further away than this are not decoded on purpose ([`NEAR_TONE`]), so
/// a match beyond it could only ever be one of the once-a-second samples —
/// which is the worst case for that extrapolation rather than a useful
/// fallback. A tone with no frame this close is counted as one, not measured.
const NEAREST_FRAME: u32 = NEAR_TONE;

/// The nanoseconds between the source's frames at [`PATTERN_FPS`].
///
/// Exactly what the subject computes — `Duration::from_secs(1) / fps`, which
/// truncates — because it is used to move an announced present from one frame
/// to its neighbour, and a different rounding would put the two apart by a
/// microsecond per frame.
const SOURCE_FRAME_NANOS: i64 = 1_000_000_000 / PATTERN_FPS as i64;

/// How close to a tone's frame the run decodes every frame it is given.
///
/// Six frames either side is two hundred milliseconds at [`PATTERN_FPS`], which
/// is enough to land on the tone's own frame when the compositor is composing
/// them all and to find a near neighbour when it is not — while costing about a
/// dozen readbacks a tone rather than the thirty a second decoding everything
/// would.
const NEAR_TONE: u32 = 6;

/// Decides which frames have to be decoded so that a tone's frame, or one of
/// its neighbours, is always among them.
///
/// Decoding every frame is thirty readbacks a second competing with the capture
/// and with the subject's own presenting, which measures the readback rather
/// than the capture ([`DECODE_EVERY`]) — a run that did it dropped the subject
/// from 30 to 28.6 frames a second. Decoding one a second misses the frame a
/// tone belongs to twenty-nine times in thirty.
///
/// So the counter is *followed* rather than read: a decode says exactly which
/// frame arrived, and every frame after it advances the count by one plus
/// however many the backend says went missing. That prediction decides when to
/// look, and the decode that follows confirms what actually arrived — nothing
/// here trusts the prediction for a measurement.
#[derive(Debug)]
struct ToneHunt {
    plan: Option<TonePlan>,
    /// The counter of the last frame decoded.
    last_seen: Option<u32>,
    /// How far the counter has moved since that decode: one per delivered
    /// frame, plus the frames the backend reported it never delivered.
    since: u32,
    /// The next frame carrying a tone that has not gone by yet.
    next: Option<u32>,
}

impl ToneHunt {
    fn new(plan: Option<TonePlan>) -> Self {
        Self {
            plan,
            last_seen: None,
            since: 0,
            next: plan.map(|plan| plan.first_frame),
        }
    }

    /// Records that a frame arrived, with however many the backend says were
    /// missed before it.
    fn arrived(&mut self, missed: u32) {
        self.since = self.since.saturating_add(1).saturating_add(missed);
    }

    /// Which counter the frame that has just arrived is expected to carry.
    fn expected(&self) -> Option<u32> {
        Some(self.last_seen?.saturating_add(self.since))
    }

    /// Whether the frame that has just arrived has to be decoded whatever the
    /// sampling says.
    fn closing_in(&self) -> bool {
        matches!(
            (self.expected(), self.next),
            (Some(here), Some(next)) if here.abs_diff(next) <= NEAR_TONE
        )
    }

    /// Records a decoded counter, so that the hunt knows where the subject's
    /// own count really is and which tone it is now waiting for.
    fn saw(&mut self, counter: u32) {
        let Some(plan) = self.plan else {
            return;
        };
        self.last_seen = Some(counter);
        self.since = 0;

        let Some(next) = self.next else {
            return;
        };
        // Only once the frame is far enough past that its neighbours are no
        // longer worth decoding: the video half may be measured on a frame
        // after the tone's as readily as on one before it.
        if counter < next + NEAR_TONE {
            return;
        }

        // The first frame still worth waiting for is the first whose
        // neighbourhood has not gone by, and the plan is what knows where that
        // is (AGENTS.md section 55).
        let from = counter.saturating_sub(NEAR_TONE).saturating_add(1);
        self.next = plan.frames_until(from).map(|until| from + until);
    }
}

/// Pairs what the subject announced with what the capture caught.
///
/// A tone with no frame and a frame with no tone are both counted rather than
/// dropped: the number of tones a run measured has to be accountable against
/// the number it played.
fn pair_tones(
    video: &mut VideoRun,
    plan: TonePlan,
    announced: &[ToneEvent],
    decoded: &[(u32, CaptureTimestamp)],
) {
    for tone in announced {
        let onset_nanos = match tone.onset {
            Onset::At(nanos) => nanos,
            Onset::NotPlaced => {
                video.tones_unplayed += 1;
                continue;
            }
            Onset::Unreported => {
                video.tones_unreported += 1;
                continue;
            }
        };

        // The nearest decoded frame, which is the tone's own whenever the
        // compositor composed it.
        let nearest = decoded
            .iter()
            .filter(|(frame, _)| frame.abs_diff(tone.frame) <= NEAREST_FRAME)
            .min_by_key(|(frame, _)| frame.abs_diff(tone.frame));

        let Some((matched_frame, captured)) = nearest else {
            video.tones_without_a_frame += 1;
            continue;
        };

        let away = i64::from(*matched_frame) - i64::from(tone.frame);
        video.tones.push(ToneObservation {
            frame: tone.frame,
            onset_nanos,
            matched_frame: *matched_frame,
            matched_present_nanos: (tone.present_nanos as i64 + away * SOURCE_FRAME_NANOS) as u64,
            captured_nanos: captured.as_nanos(),
            hertz: f64::from(plan.frequency),
        });
    }
}

/// The absolute offset a run measured, tone by tone.
#[derive(Debug)]
struct Absolute {
    measured: Vec<Measured>,
    /// Tones the subject announced but did not play, because it could not put
    /// them at the moment it wanted.
    unplayed: usize,
    /// Tones the subject played and did not report the placement of in time,
    /// which leaves nothing to measure them from.
    unreported: usize,
    /// Tones whose frame never arrived in the capture.
    frameless: usize,
    /// Tones that were played and whose frame arrived, but which could not be
    /// found in the captured audio.
    unheard: Vec<u32>,
    /// Whether the run stopped keeping samples before the end.
    truncated: bool,
}

/// One tone's worth of the measurement.
#[derive(Debug, Clone, Copy)]
struct Measured {
    /// The frame the tone belongs to.
    frame: u32,
    /// How many frames from that one the video half was measured on: zero when
    /// the compositor composed the tone's own frame.
    frames_away: i64,
    /// How far apart the two halves of the event were at the source: the
    /// present minus the endpoint's moment.
    source_skew_nanos: i64,
    /// Where the recording puts the sound, minus where the endpoint's clock
    /// said it was played.
    audio_path_nanos: i64,
    /// Where the recording puts the picture, minus the moment the frame was
    /// handed to the compositor.
    video_path_nanos: i64,
    /// The absolute A/V offset this tone gives: the audio path minus the video
    /// path. Positive is sound behind picture.
    offset_nanos: i64,
    /// The same offset worked out from the endpoint's own reported positions
    /// rather than from the track the recorder built.
    device_offset_nanos: Option<i64>,
    /// How long the burst found in the audio was, which is a check that what
    /// was found is the subject's tone.
    burst: Duration,
    /// The strongest the tone's frequency was inside the search window. A
    /// full-scale sine measures about 1.0, so the subject's −28 dBFS tone
    /// measures about 0.04.
    peak: f64,
    /// The median of the same window, which is what the rest of the machine was
    /// playing at that frequency. Zero on a quiet endpoint, because loopback
    /// silence is exactly zero.
    floor: f64,
}

impl Absolute {
    fn measure(video: &VideoRun, audio: &AudioRun) -> Self {
        let mut measured = Vec::new();
        let mut unheard = Vec::new();

        for tone in &video.tones {
            let Some(heard) = hear(audio, tone.onset_nanos, tone.hertz) else {
                unheard.push(tone.frame);
                continue;
            };

            // The video path is worked out from the timestamps as integers;
            // the audio path from a sample position, which is fractional.
            let video_path_nanos = tone.captured_nanos as i64 - tone.matched_present_nanos as i64;
            let audio_path = |at: f64| (at - tone.onset_nanos as f64).round() as i64;

            measured.push(Measured {
                frame: tone.frame,
                frames_away: i64::from(tone.matched_frame) - i64::from(tone.frame),
                source_skew_nanos: tone.matched_present_nanos as i64 - tone.onset_nanos as i64,
                audio_path_nanos: audio_path(heard.track_nanos),
                video_path_nanos,
                offset_nanos: audio_path(heard.track_nanos) - video_path_nanos,
                device_offset_nanos: heard
                    .device_nanos
                    .map(|at| audio_path(at) - video_path_nanos),
                burst: heard.burst,
                peak: heard.peak,
                floor: heard.floor,
            });
        }

        Self {
            measured,
            unplayed: video.tones_unplayed,
            unreported: video.tones_unreported,
            frameless: video.tones_without_a_frame,
            unheard,
            truncated: audio.truncated,
        }
    }

    /// The mean offset over the run's tones, in nanoseconds.
    fn mean_nanos(&self) -> Option<i64> {
        (!self.measured.is_empty()).then(|| {
            self.measured
                .iter()
                .map(|tone| tone.offset_nanos)
                .sum::<i64>()
                / self.measured.len() as i64
        })
    }

    /// The smallest and largest offset any one tone gave.
    fn extremes_nanos(&self) -> Option<(i64, i64)> {
        let mut offsets = self.measured.iter().map(|tone| tone.offset_nanos);
        let first = offsets.next()?;
        Some(offsets.fold((first, first), |(low, high), offset| {
            (low.min(offset), high.max(offset))
        }))
    }

    /// The sample standard deviation of the offsets, in nanoseconds.
    fn deviation_nanos(&self) -> Option<f64> {
        if self.measured.len() < 2 {
            return None;
        }
        let mean = self.mean_nanos()? as f64;
        let sum: f64 = self
            .measured
            .iter()
            .map(|tone| (tone.offset_nanos as f64 - mean).powi(2))
            .sum();
        Some((sum / (self.measured.len() - 1) as f64).sqrt())
    }

    fn print(&self) {
        let millis = |nanos: i64| nanos as f64 / 1e6;

        note(&format!(
            "absolute: {} tone(s) measured, {} announced but not played, {} played whose \
             placement was not reported in time, {} whose frame the capture never delivered, \
             {} played but not found in the audio{}",
            self.measured.len(),
            self.unplayed,
            self.unreported,
            self.frameless,
            self.unheard.len(),
            if self.truncated {
                " (the run stopped keeping samples before the end)"
            } else {
                ""
            },
        ));

        for tone in &self.measured {
            note(&format!(
                "absolute: frame {:>6} (video {:+} frames away): audio path {:+.3} ms, video \
                 path {:+.3} ms, offset {:+.3} ms (endpoint's own positions {}), source skew \
                 {:+.3} ms, burst {:.1} ms at {:.4} against a floor of {:.6}",
                tone.frame,
                tone.frames_away,
                millis(tone.audio_path_nanos),
                millis(tone.video_path_nanos),
                millis(tone.offset_nanos),
                tone.device_offset_nanos.map_or_else(
                    || "none".to_owned(),
                    |offset| format!("{:+.3} ms", millis(offset))
                ),
                millis(tone.source_skew_nanos),
                tone.burst.as_secs_f64() * 1e3,
                tone.peak,
                tone.floor,
            ));
        }

        match (
            self.mean_nanos(),
            self.extremes_nanos(),
            self.deviation_nanos(),
        ) {
            (Some(mean), Some((low, high)), deviation) => {
                note(&format!(
                    "absolute: A/V offset {:+.3} ms (mean of {}), from {:+.3} to {:+.3} ms, \
                     standard deviation {}",
                    millis(mean),
                    self.measured.len(),
                    millis(low),
                    millis(high),
                    deviation.map_or_else(
                        || "unknown".to_owned(),
                        |deviation| format!("{:.3} ms", deviation / 1e6)
                    ),
                ));
                let mean_of = |path: fn(&Measured) -> i64| {
                    self.measured.iter().map(path).sum::<i64>() / self.measured.len() as i64
                };
                note(&format!(
                    "absolute: of which the video path averaged {:+.3} ms and the audio path \
                     {:+.3} ms",
                    millis(mean_of(|tone| tone.video_path_nanos)),
                    millis(mean_of(|tone| tone.audio_path_nanos)),
                ));
                note(
                    "absolute: positive is sound behind picture. The video path is mostly the \
                     compositor's present-to-compose latency and the audio path mostly the \
                     audio engine's render-to-loopback latency, but each also contains \
                     whatever this recorder does on that side — the frame's own timestamp, \
                     the audio track's anchor — and this measurement separates neither. What \
                     it bounds is the total (docs/av-sync.md)",
                );
            }
            _ => note("absolute: no tone was both played and found, so there is no offset"),
        }
    }

    /// What a run whose tones were mostly *not found* should be read as.
    ///
    /// The detector only looks [`SEARCH`] either side of where the recording
    /// puts a tone's announced moment, so an offset larger than that window
    /// makes every tone unfindable and the run fails for having too few
    /// measurements rather than for being out of synchronisation — the harder
    /// of the two diagnoses. The failure says so rather than leaving it to be
    /// worked out, because the numbers a run prints do not distinguish "there
    /// was no tone" from "the tone was not where anybody looked".
    fn what_unheard_tones_mean(&self) -> String {
        if self.unheard.len() <= self.unplayed + self.unreported + self.frameless {
            return String::new();
        }
        format!(
            " Most of them were played, and their frames arrived, but no burst of the \
             subject's tone was within ±{:.0} ms of where the recording puts the moment it \
             was announced at — so read this as an offset larger than that search window \
             before reading it as a detector that failed.",
            SEARCH.as_secs_f64() * 1e3,
        )
    }

    fn assert_within(&self, tolerance: &SyncTolerance) {
        assert!(
            self.measured.len() >= MINIMUM_TONES,
            "only {} of the subject's tones could be measured ({} not played, {} not reported \
             in time, {} with no frame, {} not found in the audio), which is too few to call \
             the mean of them a measurement.{}",
            self.measured.len(),
            self.unplayed,
            self.unreported,
            self.frameless,
            self.unheard.len(),
            self.what_unheard_tones_mean(),
        );

        // Every burst found has to be the subject's tone rather than something
        // else that was playing. The length is the check background audio does
        // not reproduce: it is a 30 ms burst, and a run that found a
        // half-second of music at 997 Hz should say so rather than average it
        // in.
        for tone in &self.measured {
            let difference = tone.burst.as_secs_f64() - TONE_LENGTH.as_secs_f64();
            assert!(
                difference.abs() < TONE_LENGTH.as_secs_f64() / 2.0,
                "the burst found for frame {} lasted {:.1} ms and the subject plays {:.1} ms \
                 tones, so what was found is not the tone",
                tone.frame,
                tone.burst.as_secs_f64() * 1e3,
                TONE_LENGTH.as_secs_f64() * 1e3,
            );
        }

        let (low, high) = self
            .extremes_nanos()
            .expect("a run with tones measured has extremes");
        let deviation = self
            .deviation_nanos()
            .expect("a run with at least five tones has a deviation");
        assert!(
            deviation <= MAXIMUM_DEVIATION,
            "the tones of this run scatter by {:.3} ms about their mean, from {:+.3} to \
             {:+.3} ms, which is more than a constant offset plus a frame of compositor \
             latency can account for — so the mean of them is not a measurement of a \
             constant",
            deviation / 1e6,
            low as f64 / 1e6,
            high as f64 / 1e6,
        );

        let mean = self
            .mean_nanos()
            .expect("a run with tones measured has a mean");
        assert_eq!(
            tolerance.classify(mean),
            SyncState::InTolerance,
            "the recording puts the sound {:+.3} ms from the picture it was simultaneous \
             with at the source, which is outside {}",
            mean as f64 / 1e6,
            tolerance,
        );
    }
}

/// Where one tone turned up in the captured audio.
#[derive(Debug, Clone, Copy)]
struct Heard {
    /// The track's timestamp for the tone's half-amplitude point, in
    /// nanoseconds: where the *recorder* puts the sound.
    track_nanos: f64,
    /// The same point from the endpoint's own reported positions.
    device_nanos: Option<f64>,
    /// How long the burst stayed above half its own peak.
    burst: Duration,
    /// How strong the tone's frequency was at the peak of the burst.
    peak: f64,
    /// The median strength of the same frequency across the search window,
    /// which is what else was playing.
    floor: f64,
}

/// Finds the tone announced at `onset_nanos` in what was captured.
///
/// The envelope of the tone's own frequency is measured every [`ENVELOPE_HOP`]
/// over a window of [`ENVELOPE_WINDOW`] centred on the point it describes, and
/// the moment reported is where that envelope crosses **half its peak** on the
/// way up, interpolated between the two points either side of the crossing.
///
/// Half, because that is the moment the subject announces: it shapes the tone
/// with a symmetric one-millisecond attack and names its midpoint, which is
/// where the attack passes half amplitude
/// (`test-apps/video-pattern/src/tone.rs`). A detector that took the first
/// sample above some absolute threshold instead would report a moment that
/// moved with the volume.
///
/// [`None`] when nothing in the search window is [`MINIMUM_RATIO`] above the
/// noise floor around it, which is a tone that was not captured rather than one
/// that was captured late.
fn hear(audio: &AudioRun, onset_nanos: u64, hertz: f64) -> Option<Heard> {
    let rate = f64::from(audio.sample_rate.max(1));
    let window = (ENVELOPE_WINDOW.as_secs_f64() * rate) as usize;
    let hop = ((ENVELOPE_HOP.as_secs_f64() * rate) as usize).max(1);
    let search = (SEARCH.as_secs_f64() * rate) as usize;

    let centre = audio.index_of(onset_nanos)? as usize;
    let from = centre.saturating_sub(search).max(window / 2);
    let to = (centre + search).min(audio.mono.len().saturating_sub(window / 2 + 1));
    if to <= from || window == 0 {
        return None;
    }

    // The envelope: how much of the tone's frequency is in the window centred
    // on each point.
    let mut envelope = Vec::with_capacity((to - from) / hop + 1);
    let mut at = from;
    while at < to {
        let content = AudioContent::from_samples(
            audio.mono[at - window / 2..at + window / 2].to_vec(),
            audio.sample_rate,
        );
        envelope.push((at, content.magnitude_at(hertz)));
        at += hop;
    }

    let peak = envelope
        .iter()
        .fold(0.0f64, |peak, (_, magnitude)| peak.max(*magnitude));
    // The floor is the median of the whole span. The burst is thirty
    // milliseconds of a five-hundred-millisecond window, so the middle value is
    // a value from outside it whatever else the machine was playing.
    let mut sorted: Vec<f64> = envelope.iter().map(|(_, magnitude)| *magnitude).collect();
    sorted.sort_by(f64::total_cmp);
    let floor = sorted[sorted.len() / 2];

    // Two conditions, because either alone lets something through. A ratio
    // alone passes numerical noise on a silent endpoint, where the floor is
    // exactly zero because loopback silence is exactly zero samples. An
    // absolute level alone passes a machine that happens to be playing
    // something loud at the same frequency.
    if peak <= floor * MINIMUM_RATIO || peak < MINIMUM_MAGNITUDE {
        return None;
    }

    let half = peak / 2.0;
    let crossing = envelope
        .windows(2)
        .find(|pair| pair[0].1 < half && pair[1].1 >= half)?;
    let (before, after) = (crossing[0], crossing[1]);
    let fraction = (half - before.1) / (after.1 - before.1);
    let index = before.0 as f64 + fraction * (after.0 - before.0) as f64;

    let last_above = envelope
        .iter()
        .rev()
        .find(|(_, magnitude)| *magnitude >= half)?;
    let burst = Duration::from_secs_f64((last_above.0 as f64 - index).max(0.0) / rate);

    let (track_nanos, device_nanos) = audio.moment_of(index)?;
    Some(Heard {
        track_nanos,
        device_nanos,
        burst,
        peak,
        floor,
    })
}

/// The mean interval between the source's own frames, in nanoseconds, measured
/// against the reference clock.
///
/// A least-squares slope of capture timestamp against the counter the source
/// drew into the frame, so that a dropped sample or a missed frame changes the
/// fit rather than invalidating it.
fn source_interval(decoded: &[(u32, CaptureTimestamp)]) -> Option<f64> {
    if decoded.len() < 2 {
        return None;
    }
    let (first_index, first_timestamp) = decoded[0];
    let points: Vec<(f64, f64)> = decoded
        .iter()
        .map(|(index, timestamp)| {
            (
                f64::from(index.wrapping_sub(first_index)),
                timestamp
                    .duration_since(first_timestamp)
                    .unwrap_or_default()
                    .as_nanos() as f64,
            )
        })
        .collect();

    let n = points.len() as f64;
    let sum_x: f64 = points.iter().map(|(x, _)| x).sum();
    let sum_y: f64 = points.iter().map(|(_, y)| y).sum();
    let sum_xy: f64 = points.iter().map(|(x, y)| x * y).sum();
    let sum_xx: f64 = points.iter().map(|(x, _)| x * x).sum();

    let denominator = n * sum_xx - sum_x * sum_x;
    (denominator != 0.0).then(|| (n * sum_xy - sum_x * sum_y) / denominator)
}

/// Holds a render stream open so that the endpoint's clock keeps running.
///
/// # What it plays
///
/// Nothing. Every buffer is released with `AUDCLNT_BUFFERFLAGS_SILENT`, which
/// tells the audio engine the frames are zero without their contents ever being
/// read. There is no tone, no attenuated signal and no chance of a noise
/// escaping onto somebody's speakers, which matters because this runs for half
/// an hour on a machine somebody is using.
///
/// # Ownership
///
/// The thread owns the [`RenderStream`] for its whole life and drops it — which
/// stops the client — when it returns; [`Drop`] sets the flag and joins, so a
/// panicking test cannot leave a render stream open (AGENTS.md section 58).
/// Opening it is `test-apps/video-pattern/src/render_stream.rs`, which the
/// subject's own tone output opens through as well: one endpoint enumeration,
/// one mix-format check and one feeding loop between the two, rather than a
/// copy here (AGENTS.md section 55).
#[derive(Debug)]
struct SilenceKeeper {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SilenceKeeper {
    fn start() -> Result<Self, String> {
        let running = Arc::new(AtomicBool::new(true));
        let (ready, started) = std::sync::mpsc::channel();
        let thread = std::thread::spawn({
            let running = Arc::clone(&running);
            move || keep_awake(&running, &ready)
        });

        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                running,
                thread: Some(thread),
            }),
            Ok(Err(reason)) => {
                running.store(false, Ordering::Relaxed);
                let _ = thread.join();
                Err(reason)
            }
            Err(_) => Err("the render thread stopped before it reported anything".to_owned()),
        }
    }
}

impl Drop for SilenceKeeper {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The render thread's body: open the default endpoint and feed it silence.
fn keep_awake(running: &AtomicBool, ready: &std::sync::mpsc::Sender<Result<(), String>>) {
    // `Samples::Silence` rather than `Float32`: nothing here writes a sample,
    // so an endpoint presenting a format this could not play is still one whose
    // clock can be kept running, and refusing it would skip the measurement for
    // no reason.
    let stream = match RenderStream::open(Samples::Silence) {
        Ok(stream) => stream,
        Err(reason) => {
            let _ = ready.send(Err(reason));
            return;
        }
    };
    let _ = ready.send(Ok(()));

    while running.load(Ordering::Relaxed) {
        let Ok(queued) = stream.queued_frames() else {
            break;
        };
        let free = stream.buffer_frames().saturating_sub(queued);
        if free == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        if stream.write_silence(free).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
