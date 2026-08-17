//! End-to-end: record two process trees playing two tones, and measure what
//! landed on each of the recording's tracks.
//!
//! # The criterion this makes checkable
//!
//! [Issue #26](https://github.com/wildware-uk/clipped/issues/26) put a game's
//! own audio and everything else on tracks of their own, and
//! [issue #27](https://github.com/wildware-uk/clipped/issues/27) made the
//! second of those the *complement* of the first. Neither could measure itself.
//! #27's pull request says so in as many words — its first acceptance criterion
//! was left to this test, because nothing in the workspace looked at what
//! actually arrived on a track of a real recording.
//!
//! Everything that measures isolation today measures **synthesised** samples.
//! `crates/session/src/audio/tests.rs` scripts sources through the real muxing
//! path and `crates/muxer/tests/multi_track_audio.rs` does the same over five
//! tracks: both prove that the *routing* keeps sources apart, and both run on a
//! machine with no sound card, which is the point of them. What neither can
//! prove is the half that belongs to Windows — that
//! `ProcessLoopbackCapture`'s include mode really hands over one process tree's
//! audio and its exclude mode really hands over everything else. A build that
//! recorded the whole endpoint into the game's track passes every one of those
//! tests, and is wrong in the way SPEC.md section 11 exists to prevent: muting
//! the game would not silence it.
//!
//! So this test uses real endpoints, real processes and the recorder's own
//! entry point.
//!
//! ```text
//!   the game's process tree            everything else on the machine
//!   ────────────────────────           ──────────────────────────────
//!   video-pattern --steady-tone 997    this test process, holding 1373 Hz
//!   a window, and a tone                on the same output endpoint
//!         │                                        │
//!         └──────────── record_into ───────────────┘
//!                            │
//!            ┌───────────────┼────────────────┐
//!       Compatibility       Game        Other System Audio
//!         both tones      997 only          1373 only
//! ```
//!
//! The neighbouring tone is played by **this test process** rather than by a
//! third application, and that is not a shortcut: the test process is the
//! parent of the subject, not a member of its tree, so it is exactly what
//! "other system audio" is defined as — the complement of the game. Starting a
//! separate program to play it would add a process without adding a claim.
//!
//! # What is asserted, and which assertion matters
//!
//! Six measurements, all by Goertzel filter over the decoded tracks, through
//! `clipped_media_validation::Tone` — the harness the two synthetic tests above
//! already use, rather than a second filter written here (AGENTS.md
//! section 55):
//!
//! | Track | Must contain | Must not contain |
//! | --- | --- | --- |
//! | Game | 997 Hz | 1373 Hz |
//! | Other System Audio | 1373 Hz | 997 Hz |
//! | Compatibility Mix | both | — |
//!
//! **The "must not contain" column is the whole test.** Asserting that a track
//! is non-empty proves almost nothing — a recorder that copied the endpoint to
//! every track would satisfy it three times over. The rejection threshold is
//! `Tone::DEFAULT_RATIO`: a track's own tone must measure at least **eight
//! times** — about 18 dB — whatever a source that does not belong there
//! measures. Two sources mixed at anything like equal level are nowhere near
//! that far apart, and a track that picked up a little bleed through a shared
//! device is.
//!
//! A per-frequency measurement rather than a peak, deliberately. A peak level
//! can only distinguish loud from quiet; "the neighbour's tone is not on this
//! track" is a statement about one bin of the spectrum, and a quiet track that
//! still holds the neighbour's tone at −40 dBFS is a routing defect a peak
//! would call silence.
//!
//! # What it does not cover
//!
//! **The microphone.** AGENTS.md section 26's plan has a third source at
//! 1320 Hz, and a simulated microphone needs a capture endpoint that a test can
//! feed a known tone into — which is a virtual audio device, installed by
//! somebody, and not something this repository can assume of a contributor's
//! machine (AGENTS.md section 25). Opening whoever is running this test's real
//! microphone would record their room, which section 14 rules out. So the
//! recording made here has no microphone track, and the microphone leg stays
//! manual: `docs/testing.md` has the procedure, and `tests/audio/README.md`
//! says what it is for.
//!
//! # It plays a sound, and puts a window on a display
//!
//! Two tones, quietly — about −28 dBFS each — for the length of the recording,
//! and a borderless window on a display for a little longer. It needs a GPU, a
//! desktop session, an encoder and an output endpoint, so it is `#[ignore]`d
//! like every other test in `tests/capture/`: a test that decides for itself it
//! could not run reads as a pass, and `tests/capture/README.md` has the
//! reasoning. `CLIPPED_SKIP_AUDIO` asks a machine for quiet and skips it;
//! `CLIPPED_REQUIRE_AUDIO` turns any skip into a failure, which is what a run
//! whose numbers are being recorded should use.
//!
//! ```text
//! cargo test -p clipped-video-pattern --test track_isolation -- --ignored --nocapture
//! ```

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::Write as _;

use clipped_media_validation::{require_media_tools, AudioStream, Media, TemporaryDirectory, Tone};
use clipped_session::{
    record_into, AudioSourceSetting, CaptureTargetSettings, RecordingOutputs, RecordingSettings,
    StopSignal,
};
use clipped_video_pattern::harness::{SteadyTone, TestApp};
use clipped_video_pattern::steady_tone::{self, SteadyToneOutput};

/// The tone the game's process tree plays.
const GAME: f64 = steady_tone::FREQUENCY as f64;

/// The tone everything that is not the game plays.
const OTHER_SYSTEM_AUDIO: f64 = steady_tone::SECOND_FREQUENCY as f64;

/// Where each source's track sits in the file.
///
/// The order is `clipped-muxer`'s track model, not this test's: the
/// compatibility mix first, because it is the one a player that takes a single
/// track should take (SPEC.md section 13). The titles are asserted below rather
/// than assumed, so a change to the model fails here with a name rather than
/// with a frequency.
const MIX_TRACK: usize = 0;
const GAME_TRACK: usize = 1;
const OTHER_TRACK: usize = 2;

/// The rate the subject presents at, and the rate the recording asks for.
///
/// Only the picture depends on it. It is high enough that the recording reaches
/// its first video frame — and therefore its epoch, and therefore its audio
/// threads — promptly.
const FPS: u32 = 60;

/// How long the recording runs for.
///
/// The analysis window is a quarter of a second taken a quarter of the way into
/// each track, so this only has to be long enough that the quarter-way point is
/// well clear of the endpoint's start-up. Four seconds is about ten times that
/// and keeps the run short on a machine somebody is using.
const RECORD_FOR: Duration = Duration::from_secs(4);

/// How long the subject is given to appear and announce itself.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the subject is given to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// The environment variable that turns "this machine cannot do this" from a
/// skip into a failure.
const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

/// The environment variable that asks the tests which make a noise not to.
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
fn each_track_holds_the_tone_of_the_tree_it_belongs_to_and_not_the_other_ones() {
    if suppressed() {
        return;
    }
    let Some(_tools) = require_media_tools() else {
        // `require_media_tools` reports the reason itself, and fails rather than
        // reporting it when `CLIPPED_REQUIRE_MEDIA_TOOLS` is set.
        return;
    };

    // The game: one process tree that owns a window *and* makes a sound. Both
    // halves matter — the window is what the recording is pointed at, and it is
    // the window's process that the game's audio track is scoped to.
    let game = TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--mode",
            "borderless",
            "--fps",
            &FPS.to_string(),
            "--steady-tone",
            &GAME.to_string(),
            // A backstop well beyond the recording, so a panicking test still
            // leaves nothing on screen and nothing playing.
            "--seconds",
            "120",
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
    // parent, not a member of its tree, so what it plays is by definition the
    // complement the "Other System Audio" track is supposed to hold.
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

    let directory = TemporaryDirectory::new("audio-track-isolation");
    let output = directory.file("recording.mkv");
    let (width, height) = game.client_size();
    let settings = RecordingSettings::new(
        CaptureTargetSettings::window(game.window() as u64, width, height),
        output.clone(),
    )
    .with_framerate(FPS)
    // The game's tree and its complement. One setting opens both, because
    // opening one alone is what leaves the game's audio on two tracks or on
    // none (`crates/session/src/audio`, issue #27).
    .with_system_audio(AudioSourceSetting::SystemDefault)
    // Deliberately: see the module documentation. A simulated microphone needs
    // a virtual capture device, and a real one records the room of whoever ran
    // the test.
    .with_microphone(AudioSourceSetting::Off)
    .with_compatibility_mix(true);

    let stop = Flag::default();
    let report = std::thread::scope(|scope| {
        let recorder = scope.spawn(|| record_into(&settings, &stop, &RecordingOutputs::default()));
        std::thread::sleep(RECORD_FOR);
        stop.raise();
        recorder
            .join()
            .expect("the recording thread does not panic")
            .expect("a window that is drawing can be recorded on this machine")
    });

    // Nothing needs to be playing to read the file, and a test that panicked
    // below with two tones still sounding is somebody's afternoon.
    drop(neighbour);
    game.stop(STOP_TIMEOUT).expect("the application stops");

    let _ = writeln!(
        std::io::stderr(),
        "\n=== track_isolation ===\n\
         encoder        : {} {}\n\
         picture        : {}x{} at {} fps\n\
         ran for        : {:.2}s\n\
         frames encoded : {}",
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
            "track          : {:<20} {} Hz {}ch, {} frames ({} synthesised silence), \
             {} buffers dropped, device {:?}",
            track.track_name(),
            track.sample_rate(),
            track.channels(),
            track.frames(),
            track.synthesised_silence_frames(),
            track.buffers_dropped_writer_behind(),
            track.device().unwrap_or("none"),
        );
    }

    let media = Media::open(&output).expect("a finished recording opens");
    print_measurements(&media);

    media
        .validate()
        // The tracks the model declares, in the model's order and with the
        // names an editor shows. Asserted before the tones so that a failure
        // below is about what is *on* a track rather than about which track it
        // is.
        .audio_stream_count(3)
        .audio(
            MIX_TRACK,
            AudioStream::codec("pcm_s16le")
                .title("Compatibility Mix")
                .default_track(true),
        )
        .audio(
            GAME_TRACK,
            AudioStream::codec("pcm_s16le")
                .title("Game")
                .default_track(false),
        )
        .audio(
            OTHER_TRACK,
            AudioStream::codec("pcm_s16le")
                .title("Other System Audio")
                .default_track(false),
        )
        // And what Windows actually handed over. The game's track holds the tone
        // the game's tree played; the complement's track holds the one this
        // process played; and neither holds the other, which is the claim
        // `open_pair` exists to make and the one nothing has measured until now.
        .audio_tone(GAME_TRACK, Tone::at(GAME).isolated_from(OTHER_SYSTEM_AUDIO))
        .audio_tone(
            OTHER_TRACK,
            Tone::at(OTHER_SYSTEM_AUDIO).isolated_from(GAME),
        )
        .assert_valid();

    // The other half of the same claim, and the reason the two assertions above
    // are about routing rather than about a recording that only ever contained
    // one tone: every source in the recording is audible in the compatibility
    // mix, which is what makes it the track a naive player should take (SPEC.md
    // section 13).
    let mix = media
        .audio_content(MIX_TRACK)
        .expect("the compatibility mix decodes");
    let game_in_mix = mix.magnitude_at(GAME);
    let other_in_mix = mix.magnitude_at(OTHER_SYSTEM_AUDIO);
    let quieter = game_in_mix.min(other_in_mix);
    let louder = game_in_mix.max(other_in_mix);
    assert!(
        !mix.is_silent(),
        "the compatibility mix is silent while both sources were playing"
    );
    assert!(
        quieter * f64::from(MIX_IMBALANCE) > louder,
        "the compatibility mix has to carry every source the recording holds: {GAME} Hz measures \
         {game_in_mix:.5} and {OTHER_SYSTEM_AUDIO} Hz measures {other_in_mix:.5}, which is \
         {:.1}x apart. A mix missing one of them is a player that takes one track and hears half \
         the recording.",
        louder / quieter.max(f64::MIN_POSITIVE)
    );
}

/// How far apart the two tones in the compatibility mix may be.
///
/// Not one: the two sources are rendered by different processes onto one shared
/// endpoint, and their levels there are whatever the endpoint's mixer made of
/// them. Four is far tighter than the eight-times *rejection* threshold above —
/// so a mix that had dropped a source entirely, or was really a copy of one
/// track, cannot pass this — and loose enough that a couple of decibels between
/// the two players is not a failure.
const MIX_IMBALANCE: u8 = 4;

/// Prints what every track measured at both frequencies.
///
/// The evidence AGENTS.md section 53 asks to be recorded on the issue, and the
/// first thing anybody looks at when an assertion below fails: a full table
/// says immediately whether a track is empty, whether it holds the wrong tone,
/// or whether it holds both.
fn print_measurements(media: &Media) {
    for (index, label) in [
        (MIX_TRACK, "Compatibility Mix"),
        (GAME_TRACK, "Game"),
        (OTHER_TRACK, "Other System Audio"),
    ] {
        let Ok(content) = media.audio_content(index) else {
            let _ = writeln!(std::io::stderr(), "a:{index} {label}: does not decode");
            continue;
        };
        let (peak, magnitude) = content.dominant_frequency();
        let _ = writeln!(
            std::io::stderr(),
            "a:{index} {label:<20} {GAME} Hz {:.5}   {OTHER_SYSTEM_AUDIO} Hz {:.5}   strongest \
             {peak:.1} Hz ({magnitude:.5})   peak amplitude {:.2e}",
            content.magnitude_at(GAME),
            content.magnitude_at(OTHER_SYSTEM_AUDIO),
            content.peak_amplitude(),
        );
    }
}

fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Whether the caller should skip because the machine has been asked for quiet.
///
/// Consulted before anything is started, which is the difference between this
/// and [`skipped`]: by the time a test has discovered it cannot run, it has
/// already made whatever noise it was going to make.
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
