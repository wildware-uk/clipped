//! The sound half of the subject: a short tone placed at the moment a known
//! frame is presented.
//!
//! # Why the application makes a noise at all
//!
//! Without one, a capture of this application has nothing in it whose sound and
//! picture are known to be simultaneous *at the source*, and an A/V measurement
//! taken from it can only be relative — it can see the two clocks moving apart
//! and cannot see any constant offset they already had (`docs/av-sync.md`, and
//! [issue #173](https://github.com/wildware-uk/clipped/issues/173)). A tone
//! whose moment is tied to a decodable frame counter gives a recording both
//! halves of one event, so the offset between them becomes a number.
//!
//! It is off unless `--tone` is passed. Every other test that starts this
//! application is unaffected and the window stays silent.
//!
//! # How a sample is placed at a moment
//!
//! The unit of scheduling is a *sample*, not a write. Handing the audio engine a
//! buffer does not play it: the engine plays what it has queued, so a tone
//! written "now" is heard however much audio was queued in front of it — tens of
//! milliseconds, varying with how the render thread was scheduled, which is
//! exactly the size of the quantity being measured. So nothing here is timed by
//! when it is written.
//!
//! Instead every frame this thread writes has a known index in the stream, and
//! `IAudioClock` maps an index to a performance-counter moment: it reports the
//! position the endpoint is playing *and* the counter reading that position was
//! taken at, so
//!
//! ```text
//! moment(index) = counter_reading + (index / rate − position / frequency)
//! ```
//!
//! The tone's samples are written at the indices whose moments fall inside the
//! window asked for, and the moment reported back is computed from the index its
//! first sample actually landed on rather than from the one that was asked for.
//! What the schedule cannot deliver — a tone whose moment has already been
//! queued past — is reported as a miss rather than played late.
//!
//! # What "the moment of the tone" means
//!
//! The attack is a one-millisecond raised cosine rather than a step, because a
//! step is a click: broadband, louder than the tone and unpleasant on somebody's
//! desk. A ramp has no single instant, so the moment reported is the
//! **half-amplitude point of the attack**, which is the ramp's midpoint by
//! construction. A detector looking for where a smoothed envelope crosses half
//! its plateau finds that same point, so the definition is one both ends can
//! name (`tests/capture/av_sync.rs`).
//!
//! # It is quiet
//!
//! [`AMPLITUDE`] is about −28 dBFS and each tone is [`LENGTH`] long — the same
//! settings `crates/audio/tests/system_audio.rs` chose, for the same reason:
//! this runs on a machine somebody is using. [`FREQUENCY`] is 997 Hz for that
//! test's reason too. It is the frequency digital audio has used for decades
//! because no instrument plays it, so music playing on the machine contributes
//! almost nothing to that bin.
//!
//! # Ownership and threading
//!
//! One thread owns the `IAudioClient`, the `IAudioRenderClient` and the
//! `IAudioClock` for its whole life and stops the client before it returns.
//! [`ToneOutput::drop`] clears the flag and joins, so no path — panic included —
//! leaves a render stream open (AGENTS.md section 58). Nothing is shared with
//! the render loop except an [`AtomicBool`] and two channels (AGENTS.md
//! section 20).

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioClock, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoIncrementMTAUsage, CoTaskMemFree, CLSCTX_ALL,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// The tone, in hertz.
pub(crate) const FREQUENCY: f32 = 997.0;

/// Peak amplitude of the tone, as a fraction of full scale: about −28 dBFS.
const AMPLITUDE: f32 = 0.04;

/// How long each tone lasts, attack and decay included.
pub(crate) const LENGTH: Duration = Duration::from_millis(30);

/// How long the attack and the decay are, in nanoseconds.
///
/// The moment reported for a tone is the midpoint of the attack, so this is
/// also the width of the interval a detector has to resolve. One millisecond is
/// a fortieth of the smallest offset anybody cares about, and still slow enough
/// not to click.
const FADE_NANOS: u64 = 1_000_000;

/// [`FADE_NANOS`] as a duration.
const FADE: Duration = Duration::from_nanos(FADE_NANOS);

/// How much audio is kept queued in front of the endpoint.
///
/// Small on purpose. Everything here is placed by extrapolating the endpoint's
/// reported position forward, and the extrapolation is only ever this long:
/// sixty milliseconds of a clock accurate to a few parts per million is a
/// placement error of nanoseconds. A comfortable half-second queue would be no
/// more robust — the buffer asked for is [`BUFFER_HUNDRED_NANOS`] either way —
/// and would put the arithmetic further out on a limb.
const QUEUE: Duration = Duration::from_millis(60);

/// The buffer asked of the audio engine, in 100-nanosecond units: 200 ms.
///
/// Larger than [`QUEUE`], so that a late wake-up has somewhere to catch up into
/// rather than an underrun.
const BUFFER_HUNDRED_NANOS: i64 = 2_000_000;

/// How long the render thread sleeps between top-ups.
const TOP_UP: Duration = Duration::from_millis(3);

/// The most tones one pass will declare missed.
///
/// A schedule handed over late has a past in it, and every tone in that past is
/// a miss. This bounds the reporting rather than the schedule: a run that trips
/// it has larger problems than its report.
const MISSES_PER_PASS: u32 = 64;

/// One tone, wanted at a moment on the performance counter.
///
/// Requested one at a time rather than as a schedule fixed at the start of the
/// run, and that is the whole point of it: the presentation loop asks for each
/// tone a fraction of a second before the frame it belongs to, from the moment
/// that frame is *actually* going to be presented. A schedule laid down in
/// advance would drift away from the frames as soon as the loop fell behind its
/// own pacing — which a loop presenting into a busy compositor does — and the
/// tone would end up a second away from the frame it was supposed to be
/// simultaneous with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Request {
    /// The moment the tone's attack midpoint is wanted, in nanoseconds on the
    /// performance counter.
    pub(crate) at_nanos: u64,
}

/// What the render thread did with one scheduled tone.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Emitted {
    /// Which tone of the schedule this is, counting from zero.
    pub(crate) index: u32,
    /// The moment the endpoint's own clock puts the attack's half-amplitude
    /// point at, in nanoseconds on the performance counter.
    ///
    /// [`None`] when the moment had already been queued past by the time the
    /// thread could place it: a tone that was not played is reported as one
    /// rather than played late (AGENTS.md section 54).
    pub(crate) midpoint_nanos: Option<u64>,
}

/// A render stream that plays silence, and tones at the moments it is given.
///
/// It starts rendering as soon as it is built, so that the endpoint is running —
/// and therefore delivering loopback packets to whatever is recording — before
/// the first tone rather than from the first tone.
#[derive(Debug)]
pub(crate) struct ToneOutput {
    running: Arc<AtomicBool>,
    requests: Sender<Request>,
    emitted: Receiver<Emitted>,
    thread: Option<JoinHandle<()>>,
}

impl ToneOutput {
    /// Opens the default output device and starts rendering.
    ///
    /// # Errors
    ///
    /// Why this machine cannot play a tone, as a sentence for a warning. A
    /// machine with no output device, or one that does not present 32-bit float
    /// samples, is a legitimate outcome the caller reports and carries on from
    /// (AGENTS.md section 16): the window still renders, and it is silent.
    pub(crate) fn start() -> Result<Self, String> {
        let running = Arc::new(AtomicBool::new(true));
        let (requests, wanted) = channel();
        let (emitted_tx, emitted) = channel();
        let (ready, started) = channel();

        let thread = std::thread::Builder::new()
            .name("video-pattern-tone".to_owned())
            .spawn({
                let running = Arc::clone(&running);
                move || render(&running, &wanted, &emitted_tx, &ready)
            })
            .map_err(|error| format!("the tone thread could not be started: {error}"))?;

        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                running,
                requests,
                emitted,
                thread: Some(thread),
            }),
            Ok(Err(reason)) => {
                running.store(false, Ordering::Relaxed);
                let _ = thread.join();
                Err(reason)
            }
            Err(_) => Err("the tone thread stopped before it reported anything".to_owned()),
        }
    }

    /// Asks for a tone at `at_nanos`.
    ///
    /// It has to be far enough ahead for the render thread to reach it before
    /// the audio already queued does — more than [`QUEUE`] — or the tone is
    /// reported missed rather than played late.
    pub(crate) fn request(&self, at_nanos: u64) {
        if self.requests.send(Request { at_nanos }).is_err() {
            eprintln!("[warn] the tone thread has stopped, so no tone will be played");
        }
    }

    /// Takes every tone the render thread has placed since this was last
    /// called.
    pub(crate) fn emitted(&self) -> Vec<Emitted> {
        self.emitted.try_iter().collect()
    }
}

impl Drop for ToneOutput {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The performance counter, in nanoseconds.
///
/// The same reading, in the same units, that both Windows capture APIs stamp
/// frames with and that WASAPI reports positions on — which is what makes the
/// moments this application announces comparable with a recording's
/// (`docs/av-sync.md`).
pub(crate) fn now_nanos() -> u64 {
    static TICKS_PER_SECOND: OnceLock<u64> = OnceLock::new();

    let frequency = *TICKS_PER_SECOND.get_or_init(|| {
        let mut frequency = 0i64;
        // SAFETY: the argument is the address of a live local the call writes
        // into. `QueryPerformanceFrequency` cannot fail on any Windows version
        // this project supports; a failure would leave the local zero, and the
        // fallback below is the counter's usual 10 MHz rather than a division
        // by zero.
        let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
        u64::try_from(frequency).unwrap_or(10_000_000).max(1)
    });

    let mut ticks = 0i64;
    // SAFETY: the argument is the address of a live local the call writes into.
    let _ = unsafe { QueryPerformanceCounter(&mut ticks) };
    let ticks = u64::try_from(ticks).unwrap_or(0);

    // In 128-bit arithmetic: a 10 MHz counter times a billion overflows a `u64`
    // after about six seconds, and this counts from boot.
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(frequency)).unwrap_or(u64::MAX)
}

/// The endpoint's own account of which moment a stream position falls at.
///
/// Built fresh from `IAudioClock` on every pass, so that nothing is
/// extrapolated further than [`QUEUE`].
#[derive(Debug, Clone, Copy)]
struct Timeline {
    /// The performance-counter reading the position below was taken at, in
    /// nanoseconds.
    anchor_nanos: u64,
    /// The stream position the endpoint was playing then, in frames.
    played_frames: f64,
    /// Frames a second.
    rate: f64,
}

impl Timeline {
    /// The stream index whose moment is `nanos`, rounded to the nearest frame.
    fn index_at(self, nanos: u64) -> i64 {
        let ahead = nanos as i64 - self.anchor_nanos as i64;
        (self.played_frames + ahead as f64 * self.rate / 1e9).round() as i64
    }

    /// The moment stream index `index` is played at, in nanoseconds.
    fn nanos_at(self, index: u64) -> u64 {
        let ahead = ((index as f64 - self.played_frames) / self.rate * 1e9) as i64;
        (self.anchor_nanos as i64 + ahead).max(0) as u64
    }
}

/// A tone that has been given its samples' stream indices and cannot move.
///
/// Frozen on purpose: a burst spanning two writes would otherwise be placed
/// twice, against two different readings of the endpoint's position, and the
/// join between them would be a step in the middle of the tone.
#[derive(Debug, Clone, Copy)]
struct Burst {
    /// The stream index of the first sample of the attack.
    start: u64,
    /// How many frames the whole tone occupies.
    frames: u64,
}

/// Turns requested moments into bursts at stream indices.
///
/// It holds the requests rather than being handed one each time because
/// `next_index` has to survive between passes: the tones are numbered in the
/// order they were asked for, missed ones included, which is what lets the
/// presentation loop pair an announcement with the frame it asked for.
#[derive(Debug)]
struct Placer {
    /// Moments asked for and not yet placed or missed, oldest first.
    wanted: Vec<u64>,
    next_index: u32,
    fade_frames: u64,
    tone_frames: u64,
}

impl Placer {
    /// Places the oldest outstanding request if its attack begins inside the
    /// `want` frames about to be written from stream index `written`.
    ///
    /// A moment already behind the write cursor is reported missed and dropped,
    /// because the alternative — playing it at the next moment available —
    /// would announce a moment the tone was not at.
    fn next(
        &mut self,
        timeline: Timeline,
        written: u64,
        want: u64,
        emitted: &Sender<Emitted>,
    ) -> Option<Burst> {
        for _ in 0..MISSES_PER_PASS {
            let midpoint = *self.wanted.first()?;
            let start = timeline.index_at(midpoint.saturating_sub(FADE_NANOS / 2));

            if start < 0 || (start as u64) < written {
                self.wanted.remove(0);
                let _ = emitted.send(Emitted {
                    index: self.next_index,
                    midpoint_nanos: None,
                });
                self.next_index += 1;
                continue;
            }

            let start = start as u64;
            if start >= written + want {
                return None;
            }

            self.wanted.remove(0);
            let _ = emitted.send(Emitted {
                index: self.next_index,
                midpoint_nanos: Some(timeline.nanos_at(start + self.fade_frames / 2)),
            });
            self.next_index += 1;
            return Some(Burst {
                start,
                frames: self.tone_frames,
            });
        }
        None
    }
}

/// The shape of the tone at `position` frames into a burst of `frames`.
///
/// A raised cosine over `fade` frames at each end, one in between. At half a
/// fade in it is exactly 0.5, which is the point the reported moment names.
fn envelope(position: u64, frames: u64, fade: u64) -> f32 {
    let ramp = |along: u64| -> f32 {
        let fraction = along as f64 / fade as f64;
        (0.5 - 0.5 * (core::f64::consts::PI * fraction).cos()) as f32
    };

    if fade == 0 || frames <= fade * 2 {
        return 1.0;
    }
    if position < fade {
        return ramp(position);
    }
    let from_end = frames.saturating_sub(position);
    if from_end <= fade {
        return ramp(from_end);
    }
    1.0
}

/// The render thread's body: open the endpoint, then keep it fed.
///
/// Reports through `ready` whether it got as far as playing, so that a machine
/// without a usable output device is told about rather than waited on.
fn render(
    running: &AtomicBool,
    wanted: &Receiver<Request>,
    emitted: &Sender<Emitted>,
    ready: &Sender<Result<(), String>>,
) {
    // SAFETY: `CoIncrementMTAUsage` takes a process-wide reference to the
    // multi-threaded apartment, and the reference is deliberately never given
    // back — which is what makes it safe to take from a thread that will exit.
    // The same reasoning as `crates/audio/src/windows/apartment.rs`.
    if let Err(error) = unsafe { CoIncrementMTAUsage() } {
        let _ = ready.send(Err(format!("COM is unavailable: {error}")));
        return;
    }

    let endpoint = match open_endpoint() {
        Ok(endpoint) => endpoint,
        Err(reason) => {
            let _ = ready.send(Err(reason));
            return;
        }
    };

    // SAFETY: `endpoint.client` is initialised and has not been started.
    if let Err(error) = unsafe { endpoint.client.Start() } {
        let _ = ready.send(Err(format!("the render stream would not start: {error}")));
        return;
    }
    let _ = ready.send(Ok(()));

    let rate = f64::from(endpoint.rate);
    let channels = usize::from(endpoint.channels);
    let queue_frames = (QUEUE.as_secs_f64() * rate) as u32;
    let step = 2.0 * core::f64::consts::PI * f64::from(FREQUENCY) / rate;

    let mut placer = Placer {
        wanted: Vec::new(),
        next_index: 0,
        fade_frames: (FADE.as_secs_f64() * rate) as u64,
        tone_frames: (LENGTH.as_secs_f64() * rate) as u64,
    };
    let mut burst: Option<Burst> = None;
    let mut written = 0u64;

    while running.load(Ordering::Relaxed) {
        placer
            .wanted
            .extend(wanted.try_iter().map(|request| request.at_nanos));

        // SAFETY: `endpoint.client` is a started `IAudioClient`.
        let Ok(padding) = (unsafe { endpoint.client.GetCurrentPadding() }) else {
            break;
        };
        let want = queue_frames
            .saturating_sub(padding)
            .min(endpoint.buffer_frames.saturating_sub(padding));
        if want == 0 {
            std::thread::sleep(TOP_UP);
            continue;
        }

        let Some(timeline) = read_timeline(&endpoint) else {
            break;
        };
        if burst.is_none() {
            burst = placer.next(timeline, written, u64::from(want), emitted);
        }

        // SAFETY: `want` is at most the free space `GetCurrentPadding` just
        // reported, which is what `GetBuffer` requires. The pointer it returns
        // is valid for `want * channels` floats until `ReleaseBuffer`, which is
        // called below before anything else happens.
        let Ok(buffer) = (unsafe { endpoint.render_client.GetBuffer(want) }) else {
            break;
        };
        // SAFETY: as above, and the endpoint was checked to present 32-bit
        // float samples before the stream was started, so the region is
        // `want * channels` `f32`s. It is written in full.
        let samples = unsafe {
            core::slice::from_raw_parts_mut(buffer.cast::<f32>(), want as usize * channels)
        };
        samples.fill(0.0);

        if let Some(sounding) = burst {
            let end = sounding.start + sounding.frames;
            for position in sounding.start.max(written)..end.min(written + u64::from(want)) {
                let along = position - sounding.start;
                let value = AMPLITUDE
                    * envelope(along, sounding.frames, placer.fade_frames)
                    * (step * along as f64).sin() as f32;
                let frame = (position - written) as usize * channels;
                samples[frame..frame + channels].fill(value);
            }
            if end <= written + u64::from(want) {
                burst = None;
            }
        }

        // SAFETY: `want` is the frame count `GetBuffer` was asked for and the
        // buffer has been written in full, so no silence flag is passed.
        if unsafe { endpoint.render_client.ReleaseBuffer(want, 0) }.is_err() {
            break;
        }
        written += u64::from(want);

        std::thread::sleep(TOP_UP);
    }

    // SAFETY: `endpoint.client` was started, and stopping it is what releases
    // the endpoint.
    let _ = unsafe { endpoint.client.Stop() };
}

/// Reads the endpoint's position and the counter reading it was taken at.
fn read_timeline(endpoint: &Endpoint) -> Option<Timeline> {
    let mut position = 0u64;
    let mut counter = 0u64;
    // SAFETY: `endpoint.clock` is live and both arguments are addresses of live
    // locals the call writes into. The second is optional in the interface and
    // is what ties the position to the performance counter, which is the whole
    // reason the clock is read here.
    unsafe {
        endpoint
            .clock
            .GetPosition(&mut position, Some(&mut counter))
    }
    .ok()?;

    Some(Timeline {
        // `IAudioClock` reports the counter in 100-nanosecond units, as WASAPI
        // does everywhere else.
        anchor_nanos: counter.saturating_mul(100),
        played_frames: position as f64 / endpoint.clock_frequency as f64 * f64::from(endpoint.rate),
        rate: f64::from(endpoint.rate),
    })
}

/// The endpoint, its render client and its clock, all owned by the render
/// thread for its whole life.
#[derive(Debug)]
struct Endpoint {
    client: IAudioClient,
    render_client: IAudioRenderClient,
    clock: IAudioClock,
    /// Units of `IAudioClock` position a second, which is what turns a position
    /// into seconds.
    clock_frequency: u64,
    buffer_frames: u32,
    rate: u32,
    channels: u16,
}

/// Opens the default output device for shared-mode rendering.
///
/// Refuses anything that does not present 32-bit float samples. The capture
/// side handles every format Windows can present
/// (`crates/audio/src/format.rs`); this only knows how to *play* one, and an
/// endpoint written the wrong format is full-scale noise on somebody's speakers
/// rather than a quiet tone.
fn open_endpoint() -> Result<Endpoint, String> {
    let opened = (|| -> windows::core::Result<Option<Endpoint>> {
        // SAFETY: `MMDeviceEnumerator` is the class identifier for
        // `IMMDeviceEnumerator`, which is the interface the return type asks
        // for.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
        // SAFETY: both arguments are values of the enumerations named, and the
        // enumerator is live.
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }?;
        // SAFETY: `device` is live; the interface is fixed by the return type.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
        // SAFETY: `client` is live and uninitialised, which is when
        // `GetMixFormat` is valid. The `WAVEFORMATEX` it returns is a
        // `CoTaskMemAlloc` allocation this function now owns, and both paths
        // below give it back before returning (AGENTS.md section 58).
        let mix = unsafe { client.GetMixFormat() }?;
        // SAFETY: `WAVEFORMATEX` is `#[repr(packed)]`, so its fields are read
        // by copying the whole structure rather than by reference.
        let header = unsafe { mix.read_unaligned() };
        let float32 = is_float32(mix);

        let initialised = if float32 {
            // SAFETY: `mix` is the format Windows just reported for this
            // endpoint, so it is a format the endpoint accepts, and shared mode
            // never takes the device away from anything else. `Initialize`
            // copies what it needs out of the format.
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    0,
                    BUFFER_HUNDRED_NANOS,
                    0,
                    mix,
                    None,
                )
            }
        } else {
            Ok(())
        };
        // SAFETY: `mix` came from `GetMixFormat`, has not been freed, and is
        // not used again after this point.
        unsafe { CoTaskMemFree(Some(mix.cast())) };
        initialised?;

        if !float32 {
            return Ok(None);
        }

        // SAFETY: `client` is initialised, which is when `GetService` is valid.
        let render_client: IAudioRenderClient = unsafe { client.GetService() }?;
        // SAFETY: as above.
        let clock: IAudioClock = unsafe { client.GetService() }?;
        // SAFETY: `clock` is live and `GetFrequency` takes no arguments.
        let clock_frequency = unsafe { clock.GetFrequency() }?;
        // SAFETY: `client` is initialised.
        let buffer_frames = unsafe { client.GetBufferSize() }?;

        if clock_frequency == 0 {
            return Ok(None);
        }

        Ok(Some(Endpoint {
            client,
            render_client,
            clock,
            clock_frequency,
            buffer_frames,
            rate: { header.nSamplesPerSec },
            channels: { header.nChannels },
        }))
    })();

    match opened {
        Ok(Some(endpoint)) => Ok(endpoint),
        Ok(None) => Err(
            "the default output device does not present 32-bit float samples on a \
                         clock this application can read, which is all it knows how to play"
                .to_owned(),
        ),
        Err(error) => Err(format!(
            "the default output device would not accept a render stream: {error}"
        )),
    }
}

/// Whether a mix format is 32-bit IEEE float.
fn is_float32(mix: *const WAVEFORMATEX) -> bool {
    // SAFETY: `mix` is the live `GetMixFormat` allocation, and `WAVEFORMATEX`
    // is packed, so the structure is copied out rather than borrowed.
    let header = unsafe { mix.read_unaligned() };
    if header.wBitsPerSample != 32 {
        return false;
    }
    if header.wFormatTag == 0xfffe {
        // SAFETY: that tag is `WAVE_FORMAT_EXTENSIBLE`, which says the
        // allocation is a `WAVEFORMATEXTENSIBLE`.
        let extensible = unsafe { mix.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
        return { extensible.SubFormat } == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
    }
    // 3 is `WAVE_FORMAT_IEEE_FLOAT`.
    header.wFormatTag == 3
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rate the arithmetic below is checked at.
    const RATE: f64 = 48_000.0;

    fn fade_frames() -> u64 {
        (FADE.as_secs_f64() * RATE) as u64
    }

    fn tone_frames() -> u64 {
        (LENGTH.as_secs_f64() * RATE) as u64
    }

    fn placer(wanted: &[u64]) -> Placer {
        Placer {
            wanted: wanted.to_vec(),
            next_index: 0,
            fade_frames: fade_frames(),
            tone_frames: tone_frames(),
        }
    }

    #[test]
    fn the_reported_moment_is_where_the_attack_is_half_way_up() {
        // The definition the detector on the other side rests on: the moment
        // announced for a tone is where its attack passes half amplitude, so a
        // detector finding where a smoothed envelope crosses half its plateau
        // is looking for the same instant.
        let (fade, frames) = (fade_frames(), tone_frames());

        let half = envelope(fade / 2, frames, fade);
        assert!(
            (half - 0.5).abs() < 0.01,
            "half a fade into the attack the envelope should be 0.5 and it is {half}"
        );
        assert!(
            envelope(0, frames, fade) < 0.01,
            "the attack starts at zero"
        );
        assert!(
            (envelope(frames / 2, frames, fade) - 1.0).abs() < f32::EPSILON,
            "the middle of the tone is at full amplitude"
        );
        assert!(
            envelope(frames, frames, fade) < 0.01,
            "the decay ends at zero"
        );
    }

    #[test]
    fn the_envelope_never_leaves_the_zero_to_one_range() {
        // It multiplies an amplitude already chosen to be quiet, so a value
        // above one is a louder noise than anybody consented to.
        let (fade, frames) = (fade_frames(), tone_frames());
        for position in 0..=frames {
            let value = envelope(position, frames, fade);
            assert!(
                (0.0..=1.0).contains(&value),
                "the envelope is {value} at {position} frames into a {frames}-frame tone"
            );
        }
    }

    #[test]
    fn a_stream_index_and_a_moment_convert_back_into_each_other() {
        // The arithmetic the placement rests on. An index converted to a moment
        // and back has to be the index it started at, or a tone would be
        // announced at a moment its samples are not at.
        let timeline = Timeline {
            anchor_nanos: 1_000_000_000_000,
            played_frames: 96_000.0,
            rate: RATE,
        };

        for index in [96_000u64, 96_048, 100_000, 1_000_000] {
            let nanos = timeline.nanos_at(index);
            assert_eq!(
                timeline.index_at(nanos),
                index as i64,
                "index {index} became {nanos} ns and did not come back"
            );
        }

        // And a moment one second after the anchor is a second's worth of
        // frames past the position the endpoint was playing.
        assert_eq!(
            timeline.index_at(timeline.anchor_nanos + 1_000_000_000),
            96_000 + RATE as i64
        );
    }

    #[test]
    fn a_tone_whose_moment_has_already_been_queued_past_is_reported_missed() {
        // The rule that keeps the announced moment honest: what cannot be
        // played at the moment asked for is not played at another one
        // (AGENTS.md section 54).
        let (sender, received) = channel();
        let timeline = Timeline {
            anchor_nanos: 1_000_000_000_000,
            played_frames: 0.0,
            rate: RATE,
        };
        // The write cursor is a second ahead of what the endpoint is playing,
        // so the two moments asked for at the anchor and 100 ms after it are
        // both behind it. The third, a second and a half on, is not in reach of
        // the 480 frames about to be written either.
        let written = RATE as u64;
        let mut placer = placer(&[
            timeline.anchor_nanos,
            timeline.anchor_nanos + 100_000_000,
            timeline.anchor_nanos + 1_500_000_000,
        ]);
        let burst = placer.next(timeline, written, 480, &sender);

        assert!(burst.is_none(), "no tone can be placed in that window");
        let reported: Vec<Emitted> = received.try_iter().collect();
        assert_eq!(
            reported.len(),
            2,
            "both moments behind the write cursor have to be reported: a caller counting \
             tones would otherwise wait for a sound that was never made"
        );
        assert!(
            reported.iter().all(|tone| tone.midpoint_nanos.is_none()),
            "a tone that was not played must not be reported with a moment"
        );
        assert_eq!(
            [reported[0].index, reported[1].index],
            [0, 1],
            "the tones are numbered in the order they were asked for, missed ones \
             included, so that a caller can pair them with the frames they belong to"
        );

        // And the one still ahead of the cursor is still outstanding rather
        // than thrown away with them.
        assert_eq!(
            placer.wanted,
            [timeline.anchor_nanos + 1_500_000_000],
            "a moment that has not arrived yet is not a missed tone"
        );
        assert_eq!(placer.next_index, 2);
    }

    #[test]
    fn a_tone_inside_the_window_is_placed_at_the_index_its_moment_falls_on() {
        let (sender, received) = channel();
        let timeline = Timeline {
            anchor_nanos: 1_000_000_000_000,
            played_frames: 0.0,
            rate: RATE,
        };
        // Wanted 10 ms after the anchor, which is frame 480, with the attack
        // starting half a fade before it.
        let mut placer = placer(&[timeline.anchor_nanos + 10_000_000]);

        let burst = placer
            .next(timeline, 0, RATE as u64, &sender)
            .expect("that moment is inside the frames about to be written");

        assert_eq!(burst.start, 480 - fade_frames() / 2);
        assert_eq!(burst.frames, tone_frames());
        assert_eq!(
            placer.next_index, 1,
            "the next tone placed is the next one of the schedule"
        );

        let reported: Vec<Emitted> = received.try_iter().collect();
        assert_eq!(reported.len(), 1);
        assert_eq!(
            reported[0]
                .midpoint_nanos
                .expect("a placed tone is announced with the moment it was placed at"),
            timeline.anchor_nanos + 10_000_000,
            "the moment announced is the moment the samples are at"
        );
    }
}
