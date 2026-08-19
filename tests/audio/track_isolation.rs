//! End-to-end: record two process trees playing two tones and a simulated
//! microphone playing a third, and measure what landed on each of the
//! recording's tracks.
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
//! audio and its exclude mode really hands over everything else, and that a
//! capture endpoint hands over what is on *it* and nothing else. A build that
//! recorded the whole endpoint into the game's track passes every one of those
//! tests, and is wrong in the way SPEC.md section 11 exists to prevent: muting
//! the game would not silence it.
//!
//! So this test uses real endpoints, real processes and the recorder's own
//! entry point.
//!
//! ```text
//!   the game's process tree      everything else on the machine   a virtual audio device
//!   ────────────────────────     ──────────────────────────────   ──────────────────────
//!   video-pattern                this test process, holding       this test process, holding
//!   --steady-tone 997            1373 Hz on the same output       1699 Hz on the device's
//!   a window, and a tone         endpoint                         *render* end
//!         │                                    │                             │
//!         │                                    │                  it reappears on the
//!         │                                    │                  device's *capture* end
//!         │                                    │                             │
//!         └──────────────── record_into ───────┴─────────────────────────────┘
//!                                      │
//!            ┌─────────────────┬───────┴────────┬────────────────┐
//!       Compatibility        Game        Other System Audio   Microphone
//!        all three tones    997 only      1373 (and 1699)      1699 only
//! ```
//!
//! The neighbouring tone is played by **this test process** rather than by a
//! third application, and that is not a shortcut: the test process is the
//! parent of the subject, not a member of its tree, so it is exactly what
//! "other system audio" is defined as — the complement of the game. Starting a
//! separate program to play it would add a process without adding a claim.
//!
//! # The microphone leg, and why it took until now
//!
//! A microphone track can only be *measured* if a test decides what the
//! microphone hears, and two rules pull in opposite directions. AGENTS.md
//! section 14 forbids opening the real microphone of whoever is running the
//! tests, which would record their room. AGENTS.md section 25 forbids assuming
//! a machine has any particular device, so the name of a virtual audio cable
//! cannot be written down here. For a long time that was read as "the
//! microphone cannot be automated", and it is not: it is a **presence**
//! problem, which is what `#[ignore]` and `CLIPPED_REQUIRE_AUDIO` are for.
//!
//! What this test needs is a device whose render end reappears on its capture
//! end — VB-Audio's Virtual Cable, VoiceMeeter, Steam's streaming microphone.
//! [`clipped_video_pattern::virtual_audio`] finds one in two stages, and the
//! order matters:
//!
//! 1. **A gate that opens nothing.** `PKEY_Device_EnumeratorName` says which
//!    PnP enumerator produced the device an endpoint belongs to, and only a
//!    device with no hardware behind it is root-enumerated. A headset is `USB`,
//!    a webcam is `USB`, a display's HDMI audio is `HDAUDIO`; a virtual cable is
//!    `ROOT`. Nothing a person can speak into survives this, so no real capture
//!    endpoint is ever opened — not even to test it.
//! 2. **A tone that proves it.** [`find_simulated_microphone`] renders
//!    [`MICROPHONE`] into a candidate's render end and listens for it on the
//!    candidate's capture end. That is the only honest way to know two endpoints
//!    are two ends of one cable, and it works whatever the device is called,
//!    which a list of vendor names would not.
//!
//! The probe also **calibrates**. A virtual cable is free to apply gain — the
//! one on the machine this was written on adds exactly 30 dB — so the probe
//! measures what came back, and the run renders at whatever amplitude makes the
//! captured tone land at [`steady_tone::AMPLITUDE`], the same level as the other
//! two sources. Without that the microphone track arrives clipped and the
//! compatibility mix is a microphone with two tones faintly behind it.
//!
//! On a machine with no such device the test **skips, loudly**, naming what
//! would make it runnable — and `CLIPPED_REQUIRE_AUDIO` turns that skip into a
//! failure, which is what a run whose numbers are being recorded uses.
//!
//! # What is asserted, and which assertion matters
//!
//! Every track measured at all three frequencies, by Goertzel filter through
//! `clipped_media_validation::Tone` — the harness the two synthetic tests above
//! already use, rather than a second filter written here (AGENTS.md
//! section 55):
//!
//! | Track | Must contain | Must not contain |
//! | --- | --- | --- |
//! | Game | 997 Hz | 1373 Hz, 1699 Hz |
//! | Other System Audio | 1373 Hz | 997 Hz |
//! | Microphone | 1699 Hz | 997 Hz, 1373 Hz |
//! | Compatibility Mix | all three | — |
//!
//! **The "must not contain" column is the whole test.** Asserting that a track
//! is non-empty proves almost nothing — a recorder that copied the endpoint to
//! every track would satisfy it four times over. The rejection threshold is
//! `Tone`'s default: a track's own tone must measure at least [`SEPARATION`]
//! times — about 18 dB — whatever a source that does not belong there measures.
//! Two sources mixed at anything like equal level are nowhere near that far
//! apart, and a track that picked up a little bleed through a shared device is.
//!
//! **`Other System Audio` is not asserted to be free of 1699 Hz, and that is
//! not an omission.** The microphone's tone is rendered by this process, and
//! this process is not the game — so "everything on this machine that is not
//! the game" genuinely includes it, and a build that left it out would have a
//! hole in the complement rather than better isolation. The test asserts its
//! *presence* there instead, so the measurement is stated rather than quietly
//! skipped. Nothing is lost: the microphone's isolation is carried by the two
//! assertions that do belong to it — its own track holds neither of the other
//! tones, and the game's track does not hold its tone.
//!
//! A per-frequency measurement rather than a peak, deliberately. A peak level
//! can only distinguish loud from quiet; "the neighbour's tone is not on this
//! track" is a statement about one bin of the spectrum, and a quiet track that
//! still holds the neighbour's tone at −40 dBFS is a routing defect a peak
//! would call silence.
//!
//! # What it does not cover
//!
//! A **real** microphone, a **real** game and a person listening. The isolation
//! of a simulated microphone is a claim about routing, which is what this
//! measures; whether a headset in a room sounds right is a claim about the
//! world, and `docs/testing.md` keeps the manual procedure for it.
//!
//! # It plays a sound, and puts a window on a display
//!
//! Two tones on the machine's output endpoint, quietly — about −28 dBFS each —
//! for the length of the recording, a third into a virtual device that goes
//! nowhere near a speaker, and a borderless window on a display for a little
//! longer. It needs a GPU, a desktop session, an encoder and an output
//! endpoint, so it is `#[ignore]`d like every other test in `tests/capture/`: a
//! test that decides for itself it could not run reads as a pass, and
//! `tests/capture/README.md` has the reasoning. `CLIPPED_SKIP_AUDIO` asks a
//! machine for quiet and skips it; `CLIPPED_REQUIRE_AUDIO` turns any skip into
//! a failure.
//!
//! ```text
//! cargo test -p clipped-video-pattern --test track_isolation -- --ignored --nocapture
//! ```

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::Write as _;
use std::time::Instant;

use clipped_audio::windows::{microphones, MicrophoneCapture, MicrophoneSelection};
use clipped_audio::Capture;
use clipped_media_validation::{
    require_media_tools, AudioContent, AudioStream, Media, TemporaryDirectory, Tone,
};
use clipped_session::{
    record_into, AudioSourceSetting, CaptureTargetSettings, RecordingOutputs, RecordingSettings,
    StopSignal,
};
use clipped_video_pattern::harness::{SteadyTone, TestApp};
use clipped_video_pattern::steady_tone::{self, SteadyToneOutput};
use clipped_video_pattern::virtual_audio;

/// The tone the game's process tree plays.
const GAME: f64 = steady_tone::FREQUENCY as f64;

/// The tone everything that is not the game plays.
const OTHER_SYSTEM_AUDIO: f64 = steady_tone::SECOND_FREQUENCY as f64;

/// The tone the simulated microphone plays.
///
/// 1699 Hz, and the reason it is that number rather than AGENTS.md section 26's
/// 1320 Hz is the reason the other two are not 440 and 880: 880 and 1320 are the
/// second and third harmonics of 440, and an *absence* assertion cannot survive
/// a frequency that another source puts energy into by arithmetic. All three of
/// these are prime and none is a harmonic or a subharmonic of another, so
/// "1699 Hz is not on this track" is a statement about where the microphone's
/// audio went and about nothing else.
const MICROPHONE: f64 = steady_tone::THIRD_FREQUENCY as f64;

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
const MICROPHONE_TRACK: usize = 3;

/// How much stronger a track's own tone must be than one that does not belong
/// on it.
///
/// The same eight times — about 18 dB — that `clipped_media_validation::Tone`
/// applies by default, written out here because the probe below makes the same
/// judgement before a recording exists to hand to `Tone`.
const SEPARATION: f64 = 8.0;

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

/// How far apart the tracks may start and end.
///
/// One video frame at [`FPS`] is 16.7 ms and one audio buffer is 10 ms, so 40 ms
/// is "within a packet" with room for the last of each landing on either side of
/// the other. It is the same bound `crates/session/src/audio/tests.rs` holds a
/// scripted recording to, so a real one is not held to a looser standard than a
/// synthetic one.
const ENDS_WITHIN: Duration = Duration::from_millis(40);

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
#[ignore = "records a real window through a real encoder and plays three tones; see the module \
            documentation"]
fn each_track_holds_the_tone_of_the_source_it_belongs_to_and_not_the_other_ones() {
    if suppressed() {
        return;
    }
    let Some(_tools) = require_media_tools() else {
        // `require_media_tools` reports the reason itself, and fails rather than
        // reporting it when `CLIPPED_REQUIRE_MEDIA_TOOLS` is set.
        return;
    };

    // Before anything is started, and before anything makes a noise: is there a
    // capture endpoint this test is allowed to feed? Nothing else on the
    // machine is playing yet, which is what makes the probe's own measurement
    // trustworthy.
    let simulated = match find_simulated_microphone() {
        Ok(found) => found,
        Err(reason) => {
            skipped(&reason);
            return;
        }
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

    // And the microphone: the same loop, pointed at the render end of the
    // virtual device whose capture end the recording is about to open. Nothing
    // reaches a speaker — the device is a cable between two endpoints — so this
    // is the one source of the three that is inaudible in the room.
    let microphone = match SteadyToneOutput::start_on(
        Some(&simulated.render),
        steady_tone::THIRD_FREQUENCY,
        simulated.amplitude,
    ) {
        Ok(playing) => playing,
        Err(reason) => {
            skipped(&format!(
                "the simulated microphone's tone could not be played into {}: {reason}",
                simulated.render_name
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
    // The capture end of the virtual device, by name, and never
    // `SystemDefault`: the default input device is whatever the person running
    // this has plugged in, and opening it would record their room (AGENTS.md
    // section 14). `find_simulated_microphone` has already checked that this
    // name picks out exactly one device.
    .with_microphone(AudioSourceSetting::Named(simulated.capture_name.clone()))
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
    // below with three tones still sounding is somebody's afternoon.
    drop(microphone);
    drop(neighbour);
    game.stop(STOP_TIMEOUT).expect("the application stops");

    let _ = writeln!(
        std::io::stderr(),
        "\n=== track_isolation ===\n\
         encoder            : {} {}\n\
         picture            : {}x{} at {} fps\n\
         ran for            : {:.2}s\n\
         frames encoded     : {}\n\
         simulated microphone: {}",
        report.encoder(),
        report.codec(),
        report.size().0,
        report.size().1,
        report.requested_framerate(),
        report.duration().as_secs_f64(),
        report.frames_encoded(),
        simulated,
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
    print_stream_ends(&media);

    media
        .validate()
        // The tracks the model declares, in the model's order and with the
        // names an editor shows. Asserted before the tones so that a failure
        // below is about what is *on* a track rather than about which track it
        // is.
        .audio_stream_count(4)
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
        .audio(
            MICROPHONE_TRACK,
            AudioStream::codec("pcm_s16le")
                .title("Microphone")
                .default_track(false),
        )
        // And what Windows actually handed over. The game's track holds the tone
        // the game's tree played and neither of the others; the complement's
        // track holds the one this process played on the shared output endpoint
        // and not the game's; and the microphone's track holds only what was
        // rendered into its own cable. The last of those is the claim nothing in
        // this repository had ever measured.
        //
        // `OTHER_TRACK` is deliberately not isolated from `MICROPHONE`: this
        // process renders that tone, and this process is not the game, so the
        // complement is supposed to contain it. Its presence there is asserted
        // below instead of being left unsaid.
        .audio_tone(
            GAME_TRACK,
            Tone::at(GAME)
                .isolated_from(OTHER_SYSTEM_AUDIO)
                .isolated_from(MICROPHONE),
        )
        .audio_tone(
            OTHER_TRACK,
            Tone::at(OTHER_SYSTEM_AUDIO).isolated_from(GAME),
        )
        .audio_tone(
            MICROPHONE_TRACK,
            Tone::at(MICROPHONE)
                .isolated_from(GAME)
                .isolated_from(OTHER_SYSTEM_AUDIO),
        )
        // Where the tracks *end*, which nothing had measured against a real
        // recording until
        // [issue #320](https://github.com/wildware-uk/clipped/issues/320) went
        // looking for it.
        //
        // **This passed before that issue's fix as well as after it, and saying
        // so is the point.** #320 expected a recording to end with its audio up
        // to 200 ms short of its picture, because a capture that is closed
        // rather than drained loses whatever the audio engine was holding. What
        // six runs of this test measured is that the engine is holding almost
        // nothing at that moment on a machine where the audio threads are
        // keeping up: three runs on the undrained build ended 0.009, 0.007 and
        // 0.008 s apart, and three on the drained build ended 0.008, 0.008 and
        // 0.005 s apart. The 200 ms is what a *stalled* reader leaves behind,
        // and `crates/audio` measures the drain recovering exactly that.
        //
        // It stays because it is the assertion whose absence let the question go
        // unanswered for as long as it did: a recording that started losing its
        // tail — to a drain that hung, a capture stopped early, a shutdown
        // reordered — is a file that looks perfect by every other measurement in
        // this test.
        .synchronised_within(ENDS_WITHIN)
        .assert_valid();

    assert_every_source_reaches_the_mix(&media);
    assert_the_complement_holds_what_the_game_did_not_play(&media, simulated.amplitude);
}

/// Every source is audible in the compatibility mix.
///
/// The other half of the claim the isolation assertions make, and the reason
/// they are about routing rather than about a recording that only ever
/// contained one tone: the mix is what makes a single-track player hear the
/// whole recording (SPEC.md section 13), so a mix that had dropped a source, or
/// was really a copy of one track, has to fail here.
fn assert_every_source_reaches_the_mix(media: &Media) {
    let mix = media
        .audio_content(MIX_TRACK)
        .expect("the compatibility mix decodes");
    assert!(
        !mix.is_silent(),
        "the compatibility mix is silent while three sources were playing"
    );

    let measured = [
        ("the game", GAME, mix.magnitude_at(GAME)),
        (
            "other system audio",
            OTHER_SYSTEM_AUDIO,
            mix.magnitude_at(OTHER_SYSTEM_AUDIO),
        ),
        ("the microphone", MICROPHONE, mix.magnitude_at(MICROPHONE)),
    ];
    let quieter = measured
        .iter()
        .map(|(_, _, level)| *level)
        .fold(f64::INFINITY, f64::min);
    let louder = measured
        .iter()
        .map(|(_, _, level)| *level)
        .fold(0.0f64, f64::max);
    assert!(
        quieter * f64::from(MIX_IMBALANCE) > louder,
        "the compatibility mix has to carry every source the recording holds, and these are \
         {:.1}x apart: {}. A mix missing one of them is a player that takes one track and hears \
         part of the recording.",
        louder / quieter.max(f64::MIN_POSITIVE),
        measured
            .iter()
            .map(|(what, frequency, level)| format!("{what} at {frequency} Hz measures {level:.5}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// The complement really is the complement, on every endpoint at once.
///
/// Process-scoped loopback capture is activated on `VAD\Process_Loopback`
/// rather than on a device, so it is supposed to hand over everything a process
/// tree renders wherever it renders it. Nothing had measured that. The
/// microphone's tone is rendered by this process into a *different* endpoint
/// from the other two, and this process is not the game, so `Other System
/// Audio` has to carry it — and a build whose exclude-mode capture had quietly
/// narrowed to the default endpoint would leave it out with every other
/// assertion in this test still passing.
///
/// It arrives at the amplitude the *stream* was written at rather than the one
/// the capture end delivers: the cable's gain belongs to the device, and the
/// loopback tap is upstream of it. On the machine this was written on that is
/// 0.00126 rendered against 0.00160 measured, which is why the claim is made
/// against the track's own noise floor rather than against a level.
fn assert_the_complement_holds_what_the_game_did_not_play(media: &Media, rendered: f32) {
    let other = media
        .audio_content(OTHER_TRACK)
        .expect("the complement's track decodes");
    let microphone_tone = other.magnitude_at(MICROPHONE);
    let game_tone = other.magnitude_at(GAME);
    assert!(
        microphone_tone > game_tone * SEPARATION,
        "everything this machine played that was not the game belongs on Other System Audio, and \
         this process rendered {MICROPHONE} Hz at {rendered:.5} into a virtual device — but that \
         track measures {microphone_tone:.5} there, against {game_tone:.5} at the game's own \
         {GAME} Hz, which is not the {SEPARATION}x that tells a source apart from a bin's noise \
         floor. A complement that had narrowed to one endpoint would look exactly like this."
    );
}

/// How far apart the tones in the compatibility mix may be.
///
/// Not one: the sources are rendered by different processes onto different
/// endpoints, and their levels are whatever each endpoint's mixer made of them.
/// Four is far tighter than the eight-times *rejection* threshold above — so a
/// mix that had dropped a source entirely, or was really a copy of one track,
/// cannot pass this — and loose enough that a couple of decibels between the
/// players is not a failure.
const MIX_IMBALANCE: u8 = 4;

/// A tone that reaches a capture endpoint without a microphone being opened.
#[derive(Debug)]
struct SimulatedMicrophone {
    /// The endpoint identifier the tone is rendered into.
    render: String,
    /// What Windows calls that endpoint, for a message.
    render_name: String,
    /// What Windows calls the capture endpoint it reappears on. This is the
    /// name the recording selects the device by.
    capture_name: String,
    /// What to render so that the captured tone lands at
    /// [`steady_tone::AMPLITUDE`].
    amplitude: f32,
    /// What the cable did to the probe, as a multiple. Printed as evidence:
    /// a cable that is not unity gain is the reason `amplitude` is not
    /// [`steady_tone::AMPLITUDE`].
    gain: f64,
}

impl core::fmt::Display for SimulatedMicrophone {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "{} -> {} (cable gain {:.1}x, rendering at {:.5} to arrive at {:.5})",
            self.render_name,
            self.capture_name,
            self.gain,
            self.amplitude,
            steady_tone::AMPLITUDE,
        )
    }
}

/// How long a candidate pair is listened to, after [`PROBE_SETTLE`].
///
/// Long enough for a Goertzel filter to settle on a tone — the analysis window
/// the media harness uses is a quarter of a second.
const PROBE_FOR: Duration = Duration::from_millis(500);

/// How much of the start of a probe is thrown away before anything is measured.
///
/// A capture endpoint does not reach its steady level the instant it opens, and
/// the probe's answer is not "is a tone there" but "how much gain does this
/// cable have" — an answer that is used to choose a render amplitude, so an
/// average taken across the ramp is a microphone track at the wrong level. It
/// was measured being wrong: a probe that started at zero read this machine's
/// cable as 22.3x on one run and 31.6x on another, and the second is the true
/// figure.
const PROBE_SETTLE: Duration = Duration::from_millis(300);

/// How weak the returned tone may be before a pair is not a working cable.
///
/// A tenth of what was rendered. A cable that attenuates by 20 dB is still a
/// cable and is calibrated for below; one that returns less than this is
/// something else that happened to be making a noise.
const PROBE_FLOOR: f64 = steady_tone::AMPLITUDE as f64 / 10.0;

/// Finds a virtual audio device whose render end reappears on its capture end.
///
/// # What it will not do
///
/// Open a real microphone. Candidates come from
/// [`virtual_audio::software_endpoints`], which keeps only endpoints Windows
/// root-enumerated — a device installed by software rather than plugged into a
/// bus — so a headset, a webcam and a line-in jack are excluded before anything
/// is activated. There is no fallback: a machine whose only capture endpoint is
/// a real one gets the skip below (AGENTS.md section 14).
///
/// # Errors
///
/// Why this machine cannot simulate a microphone, as a sentence naming what
/// would make it runnable.
fn find_simulated_microphone() -> Result<SimulatedMicrophone, String> {
    let endpoints = virtual_audio::software_endpoints()?;
    let known =
        microphones().map_err(|error| format!("the microphones could not be listed: {error}"))?;

    if endpoints.render().is_empty() || endpoints.capture().is_empty() {
        return Err(format!(
            "this machine has no virtual audio device: {} software render endpoints and {} \
             software capture endpoints, with {} capture endpoints left alone because Windows \
             says there is hardware behind them. Installing VB-Audio Virtual Cable, VoiceMeeter \
             or any other loopback device — one whose speaker end reappears on its microphone \
             end — makes this test runnable. Its real microphones are never opened.",
            endpoints.render().len(),
            endpoints.capture().len(),
            endpoints.hardware_capture(),
        ));
    }

    let mut tried = Vec::new();
    for (render, capture) in endpoints.pairs() {
        // The recording will select this device by name, so the probe has to
        // reach it the same way the recording does: a name that matches two
        // devices is a device the recording cannot ask for.
        let matched: Vec<_> = known
            .iter()
            .filter(|device| {
                device
                    .name()
                    .to_lowercase()
                    .contains(&capture.name().to_lowercase())
            })
            .collect();
        let [device] = matched.as_slice() else {
            tried.push(format!(
                "{}: {} devices answer to that name",
                capture.name(),
                matched.len()
            ));
            continue;
        };

        match probe(render, device) {
            Ok(measured) if measured > PROBE_FLOOR => {
                return Ok(SimulatedMicrophone {
                    render: render.id().to_owned(),
                    render_name: render.name().to_owned(),
                    capture_name: capture.name().to_owned(),
                    // The cable's gain, undone. Rendering at this amplitude puts
                    // the captured tone at the level the other two sources are
                    // played at, so the microphone track is neither clipped nor
                    // inaudible in the compatibility mix.
                    amplitude: (f64::from(steady_tone::AMPLITUDE)
                        * f64::from(steady_tone::AMPLITUDE)
                        / measured)
                        .clamp(f64::MIN_POSITIVE, 1.0) as f32,
                    gain: measured / f64::from(steady_tone::AMPLITUDE),
                });
            }
            Ok(measured) => tried.push(format!(
                "{} -> {}: {MICROPHONE} Hz measured {measured:.5}",
                render.name(),
                capture.name()
            )),
            Err(reason) => tried.push(format!("{} -> {}: {reason}", render.name(), capture.name())),
        }
    }

    Err(format!(
        "none of this machine's software audio endpoints is a loopback cable: a tone rendered \
         into each software output endpoint did not come back on any software input endpoint. \
         {} pairs were tried [{}], and {} capture endpoints were left alone because Windows says \
         there is hardware behind them. Installing VB-Audio Virtual Cable, VoiceMeeter or any \
         other loopback device makes this test runnable.",
        tried.len(),
        tried.join("; "),
        endpoints.hardware_capture(),
    ))
}

/// Plays [`MICROPHONE`] into `render` and reports how much of it came back on
/// `capture`.
///
/// # Errors
///
/// The endpoint would not play, would not capture, or delivered a tone that is
/// not the one that was played — the last of which means `capture` was picking
/// something else up rather than being the other end of a cable.
fn probe(
    render: &virtual_audio::Endpoint,
    capture: &clipped_audio::windows::Microphone,
) -> Result<f64, String> {
    let tone = SteadyToneOutput::start_on(
        Some(render.id()),
        steady_tone::THIRD_FREQUENCY,
        steady_tone::AMPLITUDE,
    )?;
    let mut client =
        MicrophoneCapture::open(&MicrophoneSelection::device(capture.id(), capture.name()))
            .map_err(|error| format!("it would not open: {error}"))?;

    let format = client.format();
    let mut samples = Vec::new();
    let settled = Instant::now() + PROBE_SETTLE;
    let deadline = settled + PROBE_FOR;
    while Instant::now() < deadline {
        match client.read(Duration::from_millis(50)) {
            Ok(Capture::Samples(block)) => {
                if Instant::now() >= settled {
                    samples.extend_from_slice(block.samples());
                }
            }
            Ok(Capture::Idle) => {}
            Ok(Capture::FormatChanged(_)) => break,
            Err(error) => return Err(format!("it stopped delivering: {error}")),
        }
    }
    client.close();
    drop(tone);

    let channels = usize::from(format.channels().get());
    let content = AudioContent::from_samples(
        samples
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect(),
        format.sample_rate().get(),
    );

    // Not merely "something arrived": a capture endpoint carrying an unrelated
    // signal would pass that, and the whole point of a probe is to establish
    // that what came back is what was sent.
    let measured = content.magnitude_at(MICROPHONE);
    for other in [GAME, OTHER_SYSTEM_AUDIO] {
        let noise = content.magnitude_at(other);
        if measured <= noise * SEPARATION {
            return Err(format!(
                "{MICROPHONE} Hz measured {measured:.5} against {noise:.5} at {other} Hz, which \
                 nothing was playing, so this is not a cable carrying what was put into it"
            ));
        }
    }
    Ok(measured)
}

/// Prints what every track measured at all three frequencies.
///
/// The evidence AGENTS.md section 53 asks to be recorded on the issue, and the
/// first thing anybody looks at when an assertion below fails: a full table
/// says immediately whether a track is empty, whether it holds the wrong tone,
/// or whether it holds more than one.
fn print_measurements(media: &Media) {
    let _ = writeln!(
        std::io::stderr(),
        "\n=== what each track holds ===\n{:<24} {:>12} {:>12} {:>12}   strongest",
        "track",
        format!("{GAME} Hz"),
        format!("{OTHER_SYSTEM_AUDIO} Hz"),
        format!("{MICROPHONE} Hz"),
    );
    for (index, label) in [
        (MIX_TRACK, "a:0 Compatibility Mix"),
        (GAME_TRACK, "a:1 Game"),
        (OTHER_TRACK, "a:2 Other System Audio"),
        (MICROPHONE_TRACK, "a:3 Microphone"),
    ] {
        let Ok(content) = media.audio_content(index) else {
            let _ = writeln!(std::io::stderr(), "{label}: does not decode");
            continue;
        };
        let (peak, magnitude) = content.dominant_frequency();
        let _ = writeln!(
            std::io::stderr(),
            "{label:<24} {:>12.5} {:>12.5} {:>12.5}   {peak:.1} Hz ({magnitude:.5}), peak \
             amplitude {:.2e}",
            content.magnitude_at(GAME),
            content.magnitude_at(OTHER_SYSTEM_AUDIO),
            content.magnitude_at(MICROPHONE),
            content.peak_amplitude(),
        );
    }
}

/// Prints where each stream of the file starts and ends.
///
/// The evidence behind the synchronisation assertion, and the first thing to
/// look at when it fails: a table of ends says immediately whether one track is
/// short or every one of them is, which is the difference between a capture that
/// stopped early and a recording that lost the audio its engines were holding
/// (issue #320).
fn print_stream_ends(media: &Media) {
    let mut ends: Vec<(String, f64, f64)> = Vec::new();
    for stream in media.streams() {
        let index = stream.number("index").map(|value| value as i64);
        let end = media
            .packets()
            .iter()
            .filter(|packet| Some(packet.stream_index) == index)
            .map(|packet| packet.presentation_seconds + packet.duration_seconds.unwrap_or_default())
            .fold(f64::NEG_INFINITY, f64::max);
        ends.push((
            stream.label(),
            stream.number("start_time").unwrap_or(f64::NAN),
            end,
        ));
    }

    let last = ends.iter().map(|(_, _, end)| *end).fold(f64::NAN, f64::max);
    let first = ends.iter().map(|(_, _, end)| *end).fold(f64::NAN, f64::min);
    let _ = writeln!(std::io::stderr(), "\n=== where the tracks end ===");
    for (label, start, end) in &ends {
        let _ = writeln!(
            std::io::stderr(),
            "{label:<40} starts {start:.3}s, ends {end:.3}s"
        );
    }
    let _ = writeln!(
        std::io::stderr(),
        "the tracks end {:.3}s apart",
        last - first
    );
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
