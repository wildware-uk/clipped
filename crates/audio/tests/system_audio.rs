//! Records a tone this test generates, and asserts the tone is in the result.
//!
//! # Why it plays its own tone
//!
//! AGENTS.md section 26 asks for controlled generators rather than "start
//! Spotify and listen", and AGENTS.md section 25 asks that tests not depend on
//! installed applications or on a particular machine's devices. So the test
//! synthesises a [`TONE`] Hz sine, renders it through WASAPI itself, captures
//! it back through [`SystemAudioCapture`], and looks for that frequency in the
//! samples with a Goertzel filter. Nothing about that needs a person, a speaker
//! or an application, and — the point of the exercise — it fails if the capture
//! path is wrong, which "the API returned S_OK" does not.
//!
//! # What it asserts, and why in that order
//!
//! Three measurements of the same frequency, taken in sequence from one
//! capture: before the tone, during it, and after it. The tone has to appear
//! and then disappear. Measuring only during it would pass on a machine that
//! happened to have that frequency in whatever else was playing; measuring
//! only the ratio would pass on a capture that latched its last buffer and
//! repeated it for ever. The frequency is also *found* rather than assumed: the
//! spectrum is swept from 200 Hz to 2 kHz and the peak has to land on the tone.
//!
//! # It plays a sound
//!
//! Quietly — the amplitude is [`AMPLITUDE`], about −28 dBFS — and for well
//! under two seconds, because a test suite that makes noise on somebody's
//! machine should make as little of it as it can get away with. A Goertzel
//! filter over 20,000 samples finds a tone far below that, so the volume is set
//! by politeness rather than by what the measurement needs.
//!
//! # Where it runs
//!
//! Anywhere with a default audio output device, which a GitHub Windows runner
//! does not have. There it reports a skip on stderr and passes, and
//! `CLIPPED_REQUIRE_AUDIO` turns that skip into a failure on a machine that is
//! supposed to have one.

#![cfg(windows)]

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clipped_audio::windows::SystemAudioCapture;
use clipped_audio::{Capture, SampleOrigin};

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CoCreateInstance, CoIncrementMTAUsage, CLSCTX_ALL};

/// The tone, in hertz. Low enough to be nowhere near any endpoint's Nyquist
/// frequency, and deliberately not a musical note.
///
/// It was 440 Hz, which is A above middle C exactly, and that is the problem:
/// the machine this runs on is a desktop somebody is using, and music playing
/// on it in standard tuning puts real energy at exactly 440 Hz. Measured here
/// while something was playing, the background at 440 Hz was 0.013 against the
/// tone's 0.037 — a ratio of under three, where the test asks for six — and the
/// test failed. 997 Hz is the frequency digital audio has used for this for
/// decades for the same reason: it sits between B5 and C6 and no instrument
/// plays it, so background music contributes almost nothing to that bin.
const TONE: f32 = 997.0;

/// Peak amplitude of the rendered tone, as a fraction of full scale.
///
/// About −28 dBFS. See the module documentation on why it is this quiet, and
/// [`MINIMUM_RATIO`] on why it is not quieter still.
const AMPLITUDE: f32 = 0.04;

/// How much louder the tone has to be while it plays than it was before it.
///
/// Six, not sixty, because the machine a developer runs this on is not silent:
/// whatever is already playing has some energy at the tone's frequency. At
/// 997 Hz that is small — the five runs measured for the pull request put the
/// background between 0.000002 and 0.0019 against a tone of about 0.040 — but
/// it is not zero, and a threshold tight enough to depend on it being zero
/// would fail on somebody else's desk. The assertions below are backed up by
/// two that background audio cannot satisfy at all: the tone has to *stop*
/// being there, and it has to be the strongest thing in the spectrum while it
/// plays.
const MINIMUM_RATIO: f64 = 6.0;

/// How finely [`strongest_frequency`] looks at the spectrum, in hertz.
const SWEEP_STEP: f32 = 1.0;

/// The environment variable that turns "this machine has no sound card" from a
/// skip into a failure. Not set in the pull-request CI job.
const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

/// Reports that the test could not run here.
///
/// Written through `std::io::stderr()` rather than with `eprintln!` because
/// libtest captures the macros, and a skip nobody can see is how a test quietly
/// stops testing anything.
fn skipped(reason: &str) {
    if std::env::var_os(REQUIRE_AUDIO).is_some_and(|value| !value.is_empty()) {
        panic!("{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}");
    }
    let _ = writeln!(std::io::stderr(), "SKIPPED (audio): {reason}");
}

#[test]
fn a_generated_tone_is_captured_at_the_frequency_it_was_played() {
    let mut capture = match SystemAudioCapture::open() {
        Ok(capture) => capture,
        Err(error) => {
            skipped(&format!(
                "system audio capture could not be opened: {error}"
            ));
            return;
        }
    };
    let format = capture.format();

    let mut player = match TonePlayer::start(TONE, AMPLITUDE) {
        Ok(player) => player,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };

    // Before: whatever the machine happens to be playing, which on a developer's
    // desktop is not necessarily nothing. The tone is not sounding yet.
    let before = record(&mut capture, Duration::from_millis(400));

    player.play();
    // The endpoint holds a little audio, so the tone takes a moment to reach
    // the capture. Recording through that moment would put half a window of
    // pre-tone audio into the measurement.
    let _ = record(&mut capture, Duration::from_millis(250));
    let during = record(&mut capture, Duration::from_millis(700));

    player.hush();
    let _ = record(&mut capture, Duration::from_millis(250));
    let after = record(&mut capture, Duration::from_millis(400));

    player.stop();

    let rate = format.sample_rate().get() as f32;
    let channels = usize::from(format.channels().get());

    let quiet_before = goertzel(&before, channels, rate, TONE);
    let loud = goertzel(&during, channels, rate, TONE);
    let quiet_after = goertzel(&after, channels, rate, TONE);

    let _ = writeln!(
        std::io::stderr(),
        "{TONE} Hz magnitude: before {quiet_before:.6}, during {loud:.6}, after \
         {quiet_after:.6} ({} frames captured, endpoint {})",
        capture.stats().frames,
        capture.endpoint_name().unwrap_or("<none>")
    );

    // The tone this test played has to be in the recording, by a margin no
    // amount of background noise on a developer's machine would produce.
    assert!(
        loud > quiet_before * MINIMUM_RATIO + 1e-4,
        "the captured audio should contain the {TONE} Hz tone that was played: it measured \
         {loud:.6} while the tone sounded and {quiet_before:.6} before it"
    );

    // And silence has to be silence: once the tone stops, it stops. A capture
    // that repeated its last buffer, or that kept handing back a stale one,
    // would pass the assertion above and fail this one.
    assert!(
        quiet_after < loud / MINIMUM_RATIO,
        "the {TONE} Hz tone should be gone once it stops playing: it measured \
         {quiet_after:.6} after, against {loud:.6} while it sounded"
    );

    // The frequency is found rather than assumed. If the capture path resampled,
    // dropped a channel, or misread the endpoint's sample rate, the tone would
    // still be there — at the wrong frequency.
    let (peak, peak_magnitude) = strongest_frequency(&during, channels, rate, 200.0, 2_000.0);
    assert!(
        (peak - TONE).abs() <= 10.0,
        "the strongest frequency in the recording should be the {TONE} Hz that was played, \
         not {peak:.1} Hz (magnitude {peak_magnitude:.6})"
    );
}

#[test]
fn synthesised_silence_contains_no_samples_at_all() {
    // The other half of "silence is silence": what this crate invents to cover
    // a period the endpoint said nothing about has to be actual zeroes, not a
    // repeat of the last thing it heard and not uninitialised memory.
    let mut capture = match SystemAudioCapture::open() {
        Ok(capture) => capture,
        Err(error) => {
            skipped(&format!(
                "system audio capture could not be opened: {error}"
            ));
            return;
        }
    };

    // The condition has to be forced rather than waited for. A developer's
    // machine is rarely silent — something is usually playing — so a test that
    // reads for a while and asserts only on the silence it happens to see is a
    // test that asserts nothing on the machine it is run on. Stalling the
    // consumer produces the same state a silent endpoint does: the audio engine
    // holds 200 ms and discards the rest, and the discarded period is one the
    // device delivered no samples for, so it comes back as synthesised silence.
    let _ = capture.read(Duration::from_millis(200));
    let stall = Duration::from_millis(600);
    std::thread::sleep(stall);

    let until = Instant::now() + Duration::from_millis(500);
    let mut synthesised = 0usize;
    while Instant::now() < until {
        match capture
            .read(Duration::from_millis(200))
            .expect("a healthy capture does not fail")
        {
            Capture::Samples(samples) if samples.origin() == SampleOrigin::SynthesisedSilence => {
                assert!(
                    samples.samples().iter().all(|sample| *sample == 0.0),
                    "a buffer reported as synthesised silence contained a non-zero sample"
                );
                synthesised += samples.frames();
            }
            Capture::Samples(_) | Capture::Idle => {}
            Capture::FormatChanged(_) => {
                skipped("the default output device changed during the test");
                return;
            }
        }
    }

    // At least the part of the stall the audio engine could not hold: it keeps
    // 200 ms, so about 400 ms of a 600 ms stall is a period no samples exist
    // for, and 350 ms leaves room for the scheduler. A lower bound rather than
    // a range, because on a machine that was genuinely quiet the whole stall is
    // a period the endpoint said nothing about.
    let expected = capture.format().nanos_to_frames(350_000_000);
    assert!(
        synthesised as u64 >= expected,
        "a {stall:?} stall should have produced at least {expected} frames of synthesised \
         silence, and produced {synthesised}"
    );

    let _ = writeln!(
        std::io::stderr(),
        "{synthesised} frames of synthesised silence after a {stall:?} stall"
    );
}

#[test]
fn every_endpoint_buffer_carries_the_position_the_device_gave_it() {
    // The measurement A/V synchronisation is built on
    // (`docs/av-sync.md`, issue #22). A buffer's `timestamp` counts samples and
    // a buffer's `device_timestamp` is where the endpoint said those samples
    // belong; the gap between the two is how far the audio track has slid
    // against the performance counter, which is the clock video frames are
    // stamped on. If the second one were missing, or were the same number as
    // the first, that gap would be unobservable and drift could only be found
    // by watching a finished recording.
    let mut capture = match SystemAudioCapture::open() {
        Ok(capture) => capture,
        Err(error) => {
            skipped(&format!(
                "system audio capture could not be opened: {error}"
            ));
            return;
        }
    };

    // The player renders zeroes until it is asked for a tone, which is enough
    // to keep the audio engine running: loopback delivers nothing at all while
    // the endpoint is idle, and this test needs endpoint buffers to look at.
    let mut player = match TonePlayer::start(TONE, AMPLITUDE) {
        Ok(player) => player,
        Err(reason) => {
            skipped(&reason);
            return;
        }
    };

    let until = Instant::now() + Duration::from_secs(2);
    let mut from_endpoint = 0usize;
    let mut synthesised = 0usize;
    let mut widest_offset = 0i64;

    while Instant::now() < until {
        match capture
            .read(Duration::from_millis(200))
            .expect("a healthy capture does not fail")
        {
            Capture::Samples(samples) => match samples.origin() {
                SampleOrigin::Endpoint => {
                    let device = samples.device_timestamp().expect(
                        "a buffer the endpoint delivered carries the position the endpoint \
                         reported for it",
                    );
                    let offset = samples.timestamp().as_nanos() as i64 - device.as_nanos() as i64;
                    if offset.abs() > widest_offset.abs() {
                        widest_offset = offset;
                    }
                    from_endpoint += 1;
                }
                SampleOrigin::SynthesisedSilence => {
                    assert_eq!(
                        samples.device_timestamp(),
                        None,
                        "silence this crate invented covers a period the device never \
                         described, so it has no position of the device's to report"
                    );
                    synthesised += 1;
                }
            },
            Capture::Idle => {}
            Capture::FormatChanged(_) => {
                skipped("the default output device changed during the test");
                player.stop();
                return;
            }
        }
    }

    player.stop();

    assert!(
        from_endpoint > 0,
        "two seconds with a render stream open should produce endpoint buffers, and \
         produced none"
    );

    // The two accounts have to be genuinely independent. A `device_timestamp`
    // that was quietly the track's own timestamp would satisfy everything else
    // here and would make drift permanently unobservable, so the test insists
    // the two differ at least once: the track advances in whole frames while
    // the endpoint's reported position wanders by tens of microseconds, and
    // over hundreds of buffers they cannot agree exactly every time.
    assert_ne!(
        widest_offset, 0,
        "the endpoint's position and the track's never differed by a single \
         nanosecond over {from_endpoint} buffers, which means they are the same number \
         and no drift could ever be seen"
    );

    // The timeline holds the track to within its 20 ms deadband of the
    // device's own positions. Anything wider means the two accounts of the same
    // moment have come apart, which is the failure this pair exists to detect.
    assert!(
        widest_offset.abs() < 20_000_000,
        "the track drifted {:.3} ms from the endpoint's own positions in two seconds, \
         which is outside the timeline's deadband",
        widest_offset as f64 / 1e6
    );

    let _ = writeln!(
        std::io::stderr(),
        "{from_endpoint} endpoint buffers ({synthesised} synthesised); the widest gap \
         between the track and the device's own position was {:+.3} ms",
        widest_offset as f64 / 1e6
    );
}

/// Reads for `duration`, returning the interleaved samples.
fn record(capture: &mut SystemAudioCapture, duration: Duration) -> Vec<f32> {
    let until = Instant::now() + duration;
    let mut samples = Vec::new();
    while Instant::now() < until {
        match capture
            .read(Duration::from_millis(100))
            .expect("a healthy capture does not fail")
        {
            Capture::Samples(block) => samples.extend_from_slice(block.samples()),
            Capture::Idle => {}
            Capture::FormatChanged(_) => break,
        }
    }
    samples
}

/// The magnitude of `frequency` in the first channel of `samples`.
///
/// A Goertzel filter: one bin of a discrete Fourier transform, computed in one
/// pass with three multiplies per sample. Cheaper and clearer than a whole FFT
/// when the question is "how much of exactly this frequency is here", which is
/// the only question this test asks.
fn goertzel(samples: &[f32], channels: usize, rate: f32, frequency: f32) -> f64 {
    let count = samples.len() / channels;
    if count == 0 {
        return 0.0;
    }

    let coefficient = 2.0 * f64::cos(2.0 * std::f64::consts::PI * f64::from(frequency / rate));
    let mut previous = 0.0f64;
    let mut before_that = 0.0f64;
    for frame in 0..count {
        let sample = f64::from(samples[frame * channels]);
        let current = sample + coefficient * previous - before_that;
        before_that = previous;
        previous = current;
    }

    let power =
        previous * previous + before_that * before_that - coefficient * previous * before_that;
    // Normalised by the window length so that windows of different lengths are
    // comparable, which the before/during/after assertions rely on.
    power.max(0.0).sqrt() / (count as f64 / 2.0)
}

/// Sweeps the spectrum and returns the strongest frequency and its magnitude.
///
/// In steps of [`SWEEP_STEP`], which is 1 Hz rather than something coarser for
/// a reason worth stating: a Goertzel bin over this window is about 1.4 Hz
/// wide, so a sweep that steps further than that walks straight past peaks —
/// including the tone's, if the tone does not happen to land on the grid. A
/// 5 Hz step here found background at 490 Hz and reported it as the strongest
/// frequency in a recording containing a 997 Hz tone twenty-five times its
/// size, because it looked at 995 Hz and 1000 Hz and never at 997.
fn strongest_frequency(
    samples: &[f32],
    channels: usize,
    rate: f32,
    from: f32,
    to: f32,
) -> (f32, f64) {
    let mut best = (from, 0.0);
    let mut frequency = from;
    while frequency <= to {
        let magnitude = goertzel(samples, channels, rate, frequency);
        if magnitude > best.1 {
            best = (frequency, magnitude);
        }
        frequency += SWEEP_STEP;
    }
    best
}

/// Renders a sine wave to the default output device, on its own thread.
///
/// It runs for the whole test and is silent until [`play`](Self::play), so that
/// the endpoint is active — and therefore delivering loopback packets — for all
/// three measurements. Otherwise the "before" window would be measuring a
/// period during which WASAPI delivered nothing at all, which is a different
/// thing from a period in which it delivered no tone, and the test would be
/// weaker for it.
struct TonePlayer {
    sounding: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TonePlayer {
    /// Starts the render thread, or explains why this machine cannot.
    fn start(frequency: f32, amplitude: f32) -> Result<Self, String> {
        let sounding = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));
        let (ready, started) = std::sync::mpsc::channel();

        let thread = std::thread::spawn({
            let sounding = Arc::clone(&sounding);
            let running = Arc::clone(&running);
            move || render(frequency, amplitude, &sounding, &running, &ready)
        });

        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                sounding,
                running,
                thread: Some(thread),
            }),
            Ok(Err(reason)) => {
                running.store(false, Ordering::Relaxed);
                let _ = thread.join();
                Err(reason)
            }
            Err(_) => Err("the tone render thread stopped before it reported anything".to_owned()),
        }
    }

    fn play(&self) {
        self.sounding.store(true, Ordering::Relaxed);
    }

    fn hush(&self) {
        self.sounding.store(false, Ordering::Relaxed);
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TonePlayer {
    fn drop(&mut self) {
        // A test that panics must not leave a thread rendering audio to
        // somebody's speakers.
        self.stop();
    }
}

/// The render thread's body.
///
/// Reports through `ready` whether it got as far as playing, so that a machine
/// without a usable output device skips the test rather than hanging in it.
fn render(
    frequency: f32,
    amplitude: f32,
    sounding: &AtomicBool,
    running: &AtomicBool,
    ready: &std::sync::mpsc::Sender<Result<(), String>>,
) {
    // SAFETY: as `crates/audio/src/windows/apartment.rs` explains at length —
    // the reference is never given back, which is what makes it safe to take
    // from a thread that will exit.
    if let Err(error) = unsafe { CoIncrementMTAUsage() } {
        let _ = ready.send(Err(format!("COM is unavailable: {error}")));
        return;
    }

    let prepared =
        (|| -> windows::core::Result<(IAudioClient, IAudioRenderClient, u32, u32, u16)> {
            // SAFETY: `MMDeviceEnumerator` is the class identifier for
            // `IMMDeviceEnumerator`, which is the return type.
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
            // SAFETY: both arguments are values of the enumerations named.
            let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }?;
            // SAFETY: `device` is live; the interface comes from the return type.
            let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
            // SAFETY: `client` is live and uninitialised, which is when
            // `GetMixFormat` is valid. The pointer is owned here until the
            // `Initialize` below has copied what it needs; it is deliberately
            // leaked rather than freed, because this is a test process that is
            // about to exit and the alternative is a second unsafe block for no
            // benefit.
            let mix = unsafe { client.GetMixFormat() }?;
            // SAFETY: `WAVEFORMATEX` is `#[repr(packed)]`, so its fields are read
            // by copying the whole structure rather than by reference.
            let header = unsafe { mix.read_unaligned() };

            // SAFETY: `mix` is the format Windows returned for this endpoint.
            unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, mix, None) }?;
            // SAFETY: `client` is initialised, which is when `GetService` is valid.
            let render: IAudioRenderClient = unsafe { client.GetService() }?;
            // SAFETY: `client` is initialised.
            let buffer_frames = unsafe { client.GetBufferSize() }?;

            Ok((client, render, buffer_frames, { header.nSamplesPerSec }, {
                header.nChannels
            }))
        })();

    let (client, render_client, buffer_frames, rate, channels) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = ready.send(Err(format!(
                "the default output device would not accept a render stream: {error}"
            )));
            return;
        }
    };

    if !endpoint_is_float32() {
        let _ = ready.send(Err(
            "this endpoint does not present 32-bit float samples, which is all this test \
             knows how to render"
                .to_owned(),
        ));
        return;
    }

    // SAFETY: `client` is initialised and not started.
    if let Err(error) = unsafe { client.Start() } {
        let _ = ready.send(Err(format!("the render stream would not start: {error}")));
        return;
    }
    let _ = ready.send(Ok(()));

    let step = 2.0 * std::f32::consts::PI * frequency / rate as f32;
    let mut phase = 0.0f32;

    while running.load(Ordering::Relaxed) {
        // SAFETY: `client` is a started `IAudioClient`.
        let padding = match unsafe { client.GetCurrentPadding() } {
            Ok(padding) => padding,
            Err(_) => break,
        };
        let free = buffer_frames.saturating_sub(padding);
        if free == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // SAFETY: `free` is at most the free space `GetCurrentPadding` just
        // reported, which is what `GetBuffer` requires. The pointer it returns
        // is valid for `free * channels` floats until `ReleaseBuffer`, which is
        // called below before anything else happens.
        let buffer = match unsafe { render_client.GetBuffer(free) } {
            Ok(buffer) => buffer,
            Err(_) => break,
        };
        // SAFETY: as above. The endpoint was checked to be 32-bit float, so the
        // buffer is `free * channels` `f32`s, and it is written in full.
        let samples = unsafe {
            std::slice::from_raw_parts_mut(buffer.cast::<f32>(), free as usize * channels as usize)
        };

        let audible = sounding.load(Ordering::Relaxed);
        for frame in samples.chunks_exact_mut(channels as usize) {
            let value = if audible {
                phase.sin() * amplitude
            } else {
                0.0
            };
            phase = (phase + step) % (2.0 * std::f32::consts::PI);
            frame.fill(value);
        }

        // SAFETY: `free` is the frame count `GetBuffer` was asked for and the
        // buffer has been written in full, so no silence flag is needed.
        if unsafe { render_client.ReleaseBuffer(free, 0) }.is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    // SAFETY: `client` was started; stopping it is what releases the endpoint.
    let _ = unsafe { client.Stop() };
}

/// Whether the default output device presents 32-bit float samples.
///
/// The rendering above writes `f32` directly, so an endpoint presenting
/// anything else would be written full-scale noise. The capture side handles
/// every format Windows can present (`crates/audio/src/format.rs`); this test
/// only knows how to *play* one, and says so rather than making a noise nobody
/// asked for.
fn endpoint_is_float32() -> bool {
    (|| -> windows::core::Result<bool> {
        // SAFETY: as in `render` above.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
        // SAFETY: as in `render` above.
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }?;
        // SAFETY: as in `render` above.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
        // SAFETY: as in `render` above.
        let mix = unsafe { client.GetMixFormat() }?;
        // SAFETY: `WAVEFORMATEX` is packed, so the structure is copied out
        // rather than borrowed.
        let header = unsafe { mix.read_unaligned() };
        if header.wBitsPerSample != 32 {
            return Ok(false);
        }
        if header.wFormatTag == 0xfffe {
            // SAFETY: the tag says the allocation is a `WAVEFORMATEXTENSIBLE`.
            let extensible = unsafe { mix.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
            return Ok({ extensible.SubFormat } == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        }
        // 3 is `WAVE_FORMAT_IEEE_FLOAT`.
        Ok(header.wFormatTag == 3)
    })()
    .unwrap_or(false)
}
