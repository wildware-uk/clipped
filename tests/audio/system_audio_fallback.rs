//! End-to-end: record a machine that cannot scope audio capture to a process,
//! and measure what its one audio track holds.
//!
//! # The claim this makes checkable
//!
//! `crates/audio/src/error.rs` has told users this since process-scoped capture
//! was built:
//!
//! > this machine cannot record a game's audio separately from everything else,
//! > which needs Windows build 20348 or later. Clipped **records one System
//! > Audio track holding everything the machine plays** instead.
//!
//! Nothing implemented the second sentence. `crates/session/src/audio/mod.rs`
//! mapped every failure of `ProcessLoopbackCapture::open_pair` to
//! `SessionError::Audio` and propagated it, so a machine below build 20348 — the
//! whole population of shipping Windows 10 — got no recording at all while being
//! told it would get one
//! ([issue #604](https://github.com/wildware-uk/clipped/issues/604)).
//!
//! ```text
//!   the game's process tree            everything else on the machine
//!   ────────────────────────           ──────────────────────────────
//!   video-pattern --steady-tone 997    this test process, holding 1373 Hz
//!   a window, and a tone                on the same output endpoint
//!         │                                        │
//!         └──────────── record_into ───────────────┘
//!            with process scoping forced to fail
//!                            │
//!                ┌───────────┴───────────┐
//!           Compatibility            System Audio
//!             both tones              both tones
//! ```
//!
//! # What is asserted, and which assertion matters
//!
//! Three things, and the first two are what separate this from a unit test on a
//! match arm:
//!
//! 1. **The recording happens.** A build without the fallback does not get past
//!    `record_into`, which is the defect.
//! 2. **The one track holds everything the machine played** — both tones, at
//!    comparable level, measured by Goertzel filter through
//!    `clipped_media_validation::Tone` at 997 Hz and 1373 Hz. This is the
//!    assertion that cannot be satisfied by accident. A fallback that opened a
//!    *scoped* capture by mistake, or one that recorded silence, or one that
//!    recorded only the neighbour, fails here and passes everything else.
//!    The two frequencies are deliberately not harmonics of one another, for the
//!    reason `track_isolation.rs` gives: an absence measured at a harmonic is
//!    not an absence.
//! 3. **The track says what it is.** The file has a `System Audio` track and
//!    **no** `Game` or `Other System Audio` track. A recording whose undivided
//!    endpoint capture were filed under "Other System Audio" would pass every
//!    structural check and would be a track whose name is a lie — SPEC.md
//!    section 11 defines that track as all system audio *minus* the game's tree,
//!    and somebody muting it in an editor to hear the game alone would hear
//!    nothing (AGENTS.md sections 21 and 22).
//!
//! And the other direction, in the same run: a failure that is **not** an
//! inability to scope must still refuse the recording. A build that fell back on
//! anything at all would produce a `System Audio` track wherever audio was in
//! any way unwell, which is a silently different file rather than a message.
//!
//! # How the failure is forced
//!
//! `CLIPPED_FORCE_AUDIO_SCOPING_FAILURE`, read by `clipped_session::audio`. No
//! machine in this project and no hosted runner is below Windows build 20348, so
//! the path under test is one the hardware here cannot reach — the same problem
//! `crate::recording`'s `ScriptedFactory` solves for capture, and the same
//! answer: substitute the thing that fails and let everything downstream of it
//! be real. What the variable injects is the **error**, not the outcome. A
//! genuine `AudioError` comes back from the same call a Windows 10 machine's
//! failure comes back from, is classified by the same code, and everything after
//! it — the endpoint capture, the track declaration, the mixer, the encoder, the
//! Matroska writer — is the product's own.
//!
//! # It plays a sound, and puts a window on a display
//!
//! Two tones, quietly — about −28 dBFS each — and a borderless window, like
//! `track_isolation.rs` beside it, and `#[ignore]`d for the same reason: a test
//! that decides for itself that it could not run reads as a pass.
//! `CLIPPED_SKIP_AUDIO` asks a machine for quiet and skips it;
//! `CLIPPED_REQUIRE_AUDIO` turns any skip into a failure, which is what a run
//! whose numbers are being recorded should use.
//!
//! ```text
//! cargo test -p clipped-video-pattern --test system_audio_fallback -- --ignored --nocapture
//! ```

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::Write as _;

use clipped_media_validation::{require_media_tools, AudioStream, Media, TemporaryDirectory};
use clipped_session::{
    record_into, AudioSourceSetting, CaptureTargetSettings, RecordingOutputs, RecordingSettings,
    StopSignal, SystemAudioFallbackCause,
};
use clipped_video_pattern::harness::{SteadyTone, TestApp};
use clipped_video_pattern::steady_tone::{self, SteadyToneOutput};

/// The tone the game's process tree plays.
const GAME: f64 = steady_tone::FREQUENCY as f64;

/// The tone everything that is not the game plays.
const NEIGHBOUR: f64 = steady_tone::SECOND_FREQUENCY as f64;

/// Where each track sits in the file a fallback produces.
///
/// Two, not three. The whole point of the layout under test is that the pair is
/// not there.
const MIX_TRACK: usize = 0;
const SYSTEM_TRACK: usize = 1;

/// The variable that makes this build behave as a machine below Windows build
/// 20348, and the value that does it. Both are `clipped_session::audio`'s.
const FORCE: &str = "CLIPPED_FORCE_AUDIO_SCOPING_FAILURE";
const AS_OLD_WINDOWS: &str = "unavailable";
const AS_AN_UNCLASSIFIED_FAILURE: &str = "platform";

const FPS: u32 = 60;
const RECORD_FOR: Duration = Duration::from_secs(4);
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// How far apart the two tones on the one track may be.
///
/// The same bound and the same reasoning as `track_isolation.rs`'s compatibility
/// mix: the two sources are rendered by different processes onto one shared
/// endpoint and their levels there are whatever that endpoint's mixer made of
/// them. Four is far tighter than the eight-times *rejection* threshold the
/// isolation test holds a separated track to — so a "fallback" that had really
/// recorded one of the two, or a copy of a scoped capture, cannot pass this —
/// and loose enough that a couple of decibels between the two players is not a
/// failure.
const IMBALANCE: u8 = 4;

const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";
const SKIP_AUDIO: &str = "CLIPPED_SKIP_AUDIO";

/// A stop the test raises from another thread.
#[derive(Debug, Default)]
struct Flag(AtomicBool);

impl Flag {
    fn raise(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl StopSignal for Flag {
    fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
#[ignore = "records a real window through a real encoder and plays two tones; see the module \
            documentation"]
fn a_machine_that_cannot_scope_audio_records_one_track_of_everything_it_played() {
    if suppressed() {
        return;
    }
    let Some(_tools) = require_media_tools() else {
        // `require_media_tools` reports the reason itself, and fails rather than
        // reporting it when `CLIPPED_REQUIRE_MEDIA_TOOLS` is set.
        return;
    };

    // The game: one process tree that owns a window *and* makes a sound. It is
    // the stand-in for the thing whose audio a working machine would separate
    // out, and the point of this test is that its tone ends up on the same track
    // as everything else rather than on one of its own.
    let game = TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--mode",
            "borderless",
            "--fps",
            &FPS.to_string(),
            "--steady-tone",
            &GAME.to_string(),
            // A backstop well beyond the two recordings, so a panicking test
            // still leaves nothing on screen and nothing playing.
            "--seconds",
            "180",
        ],
        READY_TIMEOUT,
    )
    .expect("the video pattern application should start and announce itself");

    match game.steady_tone() {
        SteadyTone::Playing(frequency) => assert!(
            (f64::from(frequency) - GAME).abs() < 0.5,
            "the subject reports playing {frequency} Hz, and this test measures {GAME} Hz"
        ),
        SteadyTone::Unavailable => {
            skipped(
                "the subject could not play a tone; this machine has no usable output endpoint",
            );
            return;
        }
        SteadyTone::Off => panic!(
            "the subject was started with --steady-tone and reported no tone at all, which means \
             this test and the application disagree about the protocol"
        ),
    }

    // Everything else the machine is playing. This process is the subject's
    // parent and not a member of its tree, so on a machine that *could* scope
    // its captures this tone and the subject's would land on different tracks —
    // which is exactly what `track_isolation.rs` measures beside this file, and
    // exactly what must not happen here.
    let neighbour =
        match SteadyToneOutput::start(steady_tone::SECOND_FREQUENCY, steady_tone::AMPLITUDE) {
            Ok(playing) => playing,
            Err(reason) => {
                skipped(&format!(
                    "the neighbouring tone could not be played: {reason}"
                ));
                return;
            }
        };

    let directory = TemporaryDirectory::new("system-audio-fallback");
    let (width, height) = game.client_size();

    // ---------------------------------------------------------------- refusal
    //
    // First, because it costs a second and it is the half of the guard a
    // fallback is most likely to swallow: a failure that is not an inability to
    // scope must still end the recording rather than quietly produce a different
    // file. Run before the measurement so that a build which fell back on
    // everything fails here, on the sentence that says what it did, rather than
    // passing the whole test with a file nobody asked for.
    let refused = directory.file("refused.mkv");
    let refusal = {
        // Built out here, not in the closure: `TestApp` holds the pipe it reads
        // the subject's announcements from and is not `Sync`, so a closure that
        // asked it for the window handle could not be spawned.
        let settings = recording(game.window(), width, height, refused.clone());
        let _forced = Forced::to(AS_AN_UNCLASSIFIED_FAILURE);
        let stop = Flag::default();
        std::thread::scope(|scope| {
            let recorder =
                scope.spawn(|| record_into(&settings, &stop, &RecordingOutputs::default()));
            std::thread::sleep(Duration::from_secs(1));
            stop.raise();
            recorder
                .join()
                .expect("the recording thread does not panic")
        })
    };
    let refusal = refusal.expect_err(
        "an audio failure this build cannot classify must refuse the recording. A build that \
         falls back on anything at all writes a different file from the one the settings \
         describe whenever audio is in any way unwell, and says so only in a log line \
         (AGENTS.md section 27, issue #604)",
    );
    let _ = writeln!(
        std::io::stderr(),
        "\n=== the refusal ===\n{refusal}\n(and no file at {})",
        refused.display()
    );
    assert!(
        !refused.exists(),
        "the recording was refused before its first packet, so there is no file to leave behind"
    );

    // ------------------------------------------------------------ the fallback
    let output = directory.file("recording.mkv");
    let settings = recording(game.window(), width, height, output.clone());
    let stop = Flag::default();
    let report = {
        let _forced = Forced::to(AS_OLD_WINDOWS);
        std::thread::scope(|scope| {
            let recorder =
                scope.spawn(|| record_into(&settings, &stop, &RecordingOutputs::default()));
            std::thread::sleep(RECORD_FOR);
            stop.raise();
            recorder
                .join()
                .expect("the recording thread does not panic")
                .expect(
                    "a machine that cannot scope an audio capture to a process must still record. \
                     This is issue #604 itself: before it, every failure of \
                     `ProcessLoopbackCapture::open_pair` ended the recording, so a Windows 10 \
                     machine got nothing at all while the error message promised it one system \
                     audio track",
                )
        })
    };

    // Nothing needs to be playing to read the file, and a test that panicked
    // below with two tones still sounding is somebody's afternoon.
    drop(neighbour);
    game.stop(STOP_TIMEOUT).expect("the application stops");

    let _ = writeln!(
        std::io::stderr(),
        "\n=== system_audio_fallback ===\n\
         encoder        : {} {}\n\
         picture        : {}x{} at {} fps\n\
         ran for        : {:.2}s\n\
         frames encoded : {}\n\
         summary        : {report}",
        report.encoder(),
        report.codec(),
        report.size().0,
        report.size().1,
        report.requested_framerate(),
        report.duration().as_secs_f64(),
        report.frames_encoded(),
    );
    for track in report.audio_tracks() {
        let _ = writeln!(
            std::io::stderr(),
            "track          : {:<20} {} Hz {}ch, {} frames ({} synthesised silence), device {:?}",
            track.track_name(),
            track.sample_rate(),
            track.channels(),
            track.frames(),
            track.synthesised_silence_frames(),
            track.device().unwrap_or("none"),
        );
    }

    // The recording knows what it did, which is what puts the fact into the
    // session's record beside the file rather than only into a log that rotates
    // (issue #61's gap, deliberately not repeated).
    let fallback = report
        .audio_fallback()
        .expect("a recording that could not scope its audio says so in its own report");
    assert_eq!(
        fallback.cause(),
        SystemAudioFallbackCause::ProcessScopingUnavailable,
        "the reason recorded has to be the one that happened: {fallback}"
    );

    let media = Media::open(&output).expect("a finished recording opens");
    print_measurements(&media);

    media
        .validate()
        // Two tracks, and the names are the assertion. A file with a `Game`
        // track here would be one whose track holds the whole machine under the
        // name of one process tree; a file with an `Other System Audio` track
        // would be one whose track holds the game under the name of
        // everything-but-the-game. Both are labels a downstream editor acts on.
        .audio_stream_count(2)
        .audio(
            MIX_TRACK,
            AudioStream::codec("pcm_s16le")
                .title("Compatibility Mix")
                .default_track(true),
        )
        .audio(
            SYSTEM_TRACK,
            AudioStream::codec("pcm_s16le")
                .title("System Audio")
                .default_track(false),
        )
        .assert_valid();

    // And what the track actually holds, which is the measurement this test
    // exists for. "Everything the machine plays" is a claim about *both* tones
    // being present at comparable level, and it is the only claim a scoped
    // capture, a silent track or a copy of one source cannot also satisfy.
    let system = media
        .audio_content(SYSTEM_TRACK)
        .expect("the system audio track decodes");
    assert!(
        !system.is_silent(),
        "the one audio track is silent while two sources were playing, so the fallback opened \
         something that captured nothing"
    );

    let game_level = system.magnitude_at(GAME);
    let neighbour_level = system.magnitude_at(NEIGHBOUR);
    let quieter = game_level.min(neighbour_level);
    let louder = game_level.max(neighbour_level);
    assert!(
        quieter * f64::from(IMBALANCE) > louder,
        "the track named \"System Audio\" has to hold everything the machine played, and this \
         one does not: {GAME} Hz — the game's own tone — measures {game_level:.5} and \
         {NEIGHBOUR} Hz — everything that is not the game — measures {neighbour_level:.5}, which \
         is {:.1}x apart. A track missing the game is a scoped capture wearing the wrong name; a \
         track missing the neighbour is not the endpoint at all.",
        louder / quieter.max(f64::MIN_POSITIVE)
    );
}

/// The recording under test: a window, its own tone, and no microphone.
///
/// `with_system_audio(SystemDefault)` is what asks for the separated pair — the
/// same setting `track_isolation.rs` uses to get one — so what this test changes
/// is the machine, not the request. A recording that asked for one track and got
/// one would prove nothing.
fn recording(
    window: usize,
    width: u32,
    height: u32,
    output: std::path::PathBuf,
) -> RecordingSettings {
    RecordingSettings::new(
        CaptureTargetSettings::window(window as u64, width, height),
        output,
    )
    .with_framerate(FPS)
    .with_system_audio(AudioSourceSetting::SystemDefault)
    // Deliberately: a simulated microphone needs a virtual capture device, and a
    // real one records the room of whoever ran the test (AGENTS.md section 14).
    .with_microphone(AudioSourceSetting::Off)
    .with_compatibility_mix(true)
}

/// `CLIPPED_FORCE_AUDIO_SCOPING_FAILURE`, set for as long as this value lives.
///
/// A guard rather than two bare calls because the variable is process-wide: a
/// recording made after one that forgot to unset it would be a measurement of
/// the wrong build, and it would pass. Nothing else in this binary runs while
/// one is held — the two recordings are sequential statements of one test, which
/// is also why this file has a single `#[test]` in it rather than one per
/// recording.
struct Forced;

impl Forced {
    fn to(failure: &str) -> Self {
        std::env::set_var(FORCE, failure);
        Self
    }
}

impl Drop for Forced {
    fn drop(&mut self) {
        std::env::remove_var(FORCE);
    }
}

/// Prints what every track measured at both frequencies.
///
/// The evidence AGENTS.md section 53 asks to be recorded on the issue, and the
/// first thing anybody looks at when the assertion above fails: the table says
/// immediately whether the track is empty, holds one tone or holds both.
fn print_measurements(media: &Media) {
    let _ = writeln!(std::io::stderr(), "\n=== what each track holds ===");
    for (index, label) in [
        (MIX_TRACK, "Compatibility Mix"),
        (SYSTEM_TRACK, "System Audio"),
    ] {
        let Ok(content) = media.audio_content(index) else {
            let _ = writeln!(std::io::stderr(), "a:{index} {label}: does not decode");
            continue;
        };
        let (peak, magnitude) = content.dominant_frequency();
        let _ = writeln!(
            std::io::stderr(),
            "a:{index} {label:<20} {GAME} Hz {:.5}   {NEIGHBOUR} Hz {:.5}   strongest \
             {peak:.1} Hz ({magnitude:.5})   peak amplitude {:.2e}",
            content.magnitude_at(GAME),
            content.magnitude_at(NEIGHBOUR),
            content.peak_amplitude(),
        );
    }
}

fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Whether the caller should skip because the machine has been asked for quiet.
fn suppressed() -> bool {
    if !is_set(SKIP_AUDIO) {
        return false;
    }
    assert!(
        !is_set(REQUIRE_AUDIO),
        "{SKIP_AUDIO} and {REQUIRE_AUDIO} are both set. One says these tests must not run and the \
         other says they must not be skipped; there is no behaviour that satisfies both, so \
         neither is being guessed at."
    );
    skipped(&format!("{SKIP_AUDIO} is set"));
    true
}

/// Reports that the test could not run here.
///
/// Written through `std::io::stderr()` rather than with `eprintln!` because
/// libtest captures the macros, and a skip nobody can see is how a test quietly
/// stops testing anything (AGENTS.md section 54).
fn skipped(reason: &str) {
    if is_set(REQUIRE_AUDIO) {
        panic!("{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}");
    }
    let _ = writeln!(std::io::stderr(), "SKIPPED (audio): {reason}");
}
