//! System audio capture through WASAPI loopback.
//!
//! # What it does
//!
//! Opens the endpoint Windows currently plays through — the default render
//! device for the console role — in loopback mode, and turns it into a
//! continuous, gapless run of timestamped `f32` buffers. "Continuous" is the
//! word doing the work: what WASAPI delivers is not continuous, and most of
//! this file exists to make it so.
//!
//! # Threading
//!
//! One capture, one thread, no thread created here.
//!
//! [`SystemAudioCapture::read`] takes `&mut self` and blocks until it has
//! something to report, so the caller supplies the thread — as
//! `clipped-capture` does for video, and for the same reason: the session owns
//! its threads and can give this one the priority and the lifetime it wants
//! rather than discovering a thread it did not ask for. The capture is [`Send`]
//! so it can be built anywhere and moved onto that thread, and is not `Sync`,
//! because two threads reading one endpoint would interleave packets and
//! nothing here would notice.
//!
//! What that thread is allowed to wait on is a shorter list than it looks.
//! Inside `read` there is exactly one blocking call: a wait on the event handle
//! WASAPI signals when a packet is ready, bounded by the caller's timeout and
//! by [`WAIT_SLICE`]. There is no lock shared with anything that does work — the
//! only mutex in the crate is [`EndpointWatch`]'s, held for two field writes on
//! either side — no allocation once the buffers have reached their steady size,
//! no file, no logging in the per-packet path except at `debug`, and nothing
//! that talks to the rest of the recorder (AGENTS.md section 20). Device
//! changes arrive on a Windows audio-service thread and are reduced to a flag
//! this thread reads; reopening an endpoint happens on this thread, between
//! reads, and is the one operation that can take more than a moment.
//!
//! # Ownership
//!
//! Every native resource has one owner and one release point. [`Stream`] holds
//! the audio client, the capture client and the event handle, and its [`Drop`]
//! stops the stream and closes the handle. [`SystemAudioCapture`] holds an
//! `Option<Stream>`, the device enumerator and the notification registration,
//! whose own [`Drop`] unsubscribes. Closing a capture is
//! `self.stream = None`, and dropping it does the same thing, so a thread that
//! unwinds releases exactly what a clean stop would (AGENTS.md section 58). The
//! COM apartment is the one exception, and `apartment.rs` says why.
//!
//! # What is not here
//!
//! Process-scoped capture — the game's tree and its complement — is M2 and
//! [ADR 0003](../../../../docs/adr/0003-process-specific-audio-capture.md).
//! Microphone capture is
//! [issue #20](https://github.com/wildware-uk/clipped/issues/20). Mixing is
//! [issue #29](https://github.com/wildware-uk/clipped/issues/29). This is the
//! single system-audio stream everything else is built beside.

use core::num::NonZeroU64;
use core::time::Duration;
use std::sync::Arc;
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::buffer::{CapturedAudio, SampleOrigin};
use crate::error::{AudioError, Capture};
use crate::format::{append_as_f32, AudioFormat};
use crate::time::AudioTimestamp;
use crate::timeline::{Continuity, Timeline};
use crate::windows::apartment::ensure_multi_threaded_apartment;
use crate::windows::endpoint::{
    create_enumerator, default_render_endpoint, platform_error, EndpointIdentity, MixFormat,
};
use crate::windows::notifications::{EndpointChange, EndpointNotifications, EndpointWatch};

/// How much audio the endpoint holds for this capture, in 100-nanosecond units.
///
/// 200 ms. This is the entire buffering between the audio engine and the
/// caller, and it is fixed for the life of the stream: a consumer that stalls
/// does not cause anything in this process to grow, and after 200 ms the audio
/// engine discards the oldest data and flags the discontinuity, which the
/// timeline turns into silence of the right length. The alternative — an
/// unbounded queue that keeps everything a stalled consumer has not read — is
/// what AGENTS.md section 18 means by preferring bounded queues, and what
/// issue #19's third acceptance criterion asks about.
///
/// 200 ms rather than the 10 ms minimum because the cost is 77 KB for a stereo
/// 48 kHz endpoint and the benefit is surviving a scheduling hiccup on a
/// machine that is also running a game.
const BUFFER_DURATION: i64 = 2_000_000;

/// The longest one wait inside a read blocks for.
///
/// While the endpoint is producing audio the event fires every device period
/// and this is never reached. While it is silent, nothing fires at all, and
/// this is what brings the thread back often enough to keep synthesised silence
/// flowing at a steady rate rather than arriving in one lump when the caller's
/// timeout finally expires.
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// How long the polling fallback sleeps between looks.
///
/// Half the 10 ms device period observed on Windows 11 build 26200, which is
/// the interval Microsoft's own loopback samples use. Only reached on a system
/// where the audio engine refuses an event-driven loopback stream.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// How long to wait before looking for a default endpoint again when there is
/// none.
///
/// Two seconds: often enough that plugging a headset back in resumes recording
/// while the user is still holding the plug, rare enough that a machine with no
/// sound card at all is not asking Windows the same question hundreds of times
/// a minute for hours.
const ENDPOINT_RETRY: Duration = Duration::from_secs(2);

/// How the capture thread waits for the next packet.
#[derive(Debug)]
enum Wake {
    /// WASAPI signals this handle when a packet is ready. Owned by the
    /// [`Stream`] and closed by its [`Drop`].
    Event(HANDLE),
    /// The audio engine refused an event-driven loopback stream, so the packet
    /// queue is looked at on a timer instead.
    Poll,
}

/// One live loopback stream on one endpoint.
///
/// Everything in here is replaced together when the endpoint changes; there is
/// no way to keep half of it.
#[derive(Debug)]
struct Stream {
    identity: EndpointIdentity,
    format: AudioFormat,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    wake: Wake,
    /// Bytes one frame occupies in the endpoint's buffer, cached because it is
    /// multiplied by a frame count for every packet.
    bytes_per_frame: usize,
}

/// What one look at the endpoint produced.
enum Polled {
    /// A packet, already converted into the capture's buffer.
    Packet {
        /// The position WASAPI attached to it.
        arrived: AudioTimestamp,
        /// Frames in it.
        frames: u64,
        /// Whether the audio engine reported that data was lost before it.
        discontinuity: bool,
    },
    /// Nothing queued.
    Empty,
    /// This stream is finished and a new one has to be opened.
    Lost(EndpointChange),
}

impl Stream {
    /// Opens a loopback stream on the default render endpoint.
    ///
    /// [`None`] when the machine has no default render endpoint. That is a
    /// state to wait through rather than an error, so it is not one.
    fn open(enumerator: &IMMDeviceEnumerator) -> Result<Option<Self>, AudioError> {
        let Some(device) = default_render_endpoint(enumerator)? else {
            return Ok(None);
        };
        let identity = EndpointIdentity::of(&device)?;

        // SAFETY: `device` is a live `IMMDevice`; windows-rs infers the
        // interface identifier from the return type, so the activation cannot
        // ask for one interface and be typed as another.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|error| platform_error("activating the audio client", error))?;

        let mix = MixFormat::of(&client)?;
        let format = mix.audio();

        // Event-driven first. It is what lets the capture thread sleep until
        // there is something to do instead of waking every few milliseconds to
        // find nothing, and — measured on Windows 11 build 26200 — the audio
        // engine accepts it for a loopback stream. It is not documented to,
        // and older Windows builds and some drivers refuse, so a refusal falls
        // back to polling rather than failing: a recording with system audio
        // captured on a timer is better than one without system audio.
        //
        // SAFETY: `mix.as_ptr()` is the `WAVEFORMATEX` this endpoint's own
        // `GetMixFormat` returned and `mix` outlives the call; the remaining
        // arguments are constants of the API. `Initialize` is called at most
        // once per `IAudioClient`, which is why the fallback below activates a
        // fresh one.
        let event_driven = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                BUFFER_DURATION,
                0,
                mix.as_ptr(),
                None,
            )
        };

        let (client, wake) = match event_driven.and_then(|()| create_wake_event(&client)) {
            Ok(wake) => (client, wake),
            Err(error) => {
                tracing::info!(
                    %error,
                    "this audio device would not drive a loopback stream from an event, so \
                     system audio is read on a timer instead"
                );
                // SAFETY: as above. A second `IAudioClient` is activated
                // because `Initialize` may not be called twice on one.
                let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
                    .map_err(|error| platform_error("activating the audio client", error))?;
                // SAFETY: as above.
                unsafe {
                    client.Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_LOOPBACK,
                        BUFFER_DURATION,
                        0,
                        mix.as_ptr(),
                        None,
                    )
                }
                .map_err(|error| {
                    platform_error("opening a loopback stream on the audio device", error)
                })?;
                (client, Wake::Poll)
            }
        };

        // SAFETY: `client` is an initialised `IAudioClient`, which is when
        // `GetService` is valid; the interface identifier comes from the
        // return type.
        let capture: IAudioCaptureClient = unsafe { client.GetService() }
            .map_err(|error| platform_error("obtaining the loopback capture client", error))?;

        // SAFETY: `client` is initialised and not started.
        unsafe { client.Start() }
            .map_err(|error| platform_error("starting the loopback stream", error))?;

        Ok(Some(Self {
            identity,
            format,
            client,
            capture,
            wake,
            bytes_per_frame: usize::from(format.channels().get())
                * format.endpoint_samples().bytes_per_sample(),
        }))
    }

    /// Takes the next packet, converting it into `out`.
    ///
    /// `out` is cleared first and left holding the packet's samples as
    /// interleaved `f32`. It keeps its capacity between calls, so a steady
    /// stream allocates nothing after the first packet.
    fn next_packet(&mut self, out: &mut Vec<f32>) -> Polled {
        // SAFETY: `capture` is a live `IAudioCaptureClient` on a started
        // stream.
        let available = match unsafe { self.capture.GetNextPacketSize() } {
            Ok(available) => available,
            Err(error) => return self.lost(&error, "asking for the next packet size"),
        };
        if available == 0 {
            return Polled::Empty;
        }

        let mut data = core::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        let mut position = 0u64;
        // SAFETY: every out parameter is a live local of the type the
        // signature names. On success `data` points at `frames *
        // bytes_per_frame` readable bytes owned by the audio engine and valid
        // until `ReleaseBuffer`, which is called below before this function
        // returns on every path.
        if let Err(error) = unsafe {
            self.capture.GetBuffer(
                &mut data,
                &mut frames,
                &mut flags,
                None,
                Some(&mut position),
            )
        } {
            return self.lost(&error, "reading a packet from the audio device");
        }

        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
        let discontinuity = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
        let samples = frames as usize * usize::from(self.format.channels().get());

        out.clear();
        if silent {
            // The audio engine is entitled to hand over a buffer whose contents
            // are undefined and say "this is silence" instead of writing zeros
            // into it. Reading it anyway is how a recording ends up with a
            // burst of noise in a quiet passage.
            out.resize(samples, 0.0);
        } else {
            // SAFETY: `data` is the pointer `GetBuffer` just returned, valid
            // for `frames * bytes_per_frame` bytes until `ReleaseBuffer`, which
            // has not been called yet. A `u8` slice has no alignment
            // requirement, so the cast is sound whatever the sample type is,
            // and `append_as_f32` reads it as little-endian bytes rather than
            // as typed values for the same reason.
            let bytes = unsafe {
                core::slice::from_raw_parts(data, frames as usize * self.bytes_per_frame)
            };
            append_as_f32(bytes, self.format.endpoint_samples(), out);
        }

        // SAFETY: `frames` is the count `GetBuffer` reported, which is what
        // `ReleaseBuffer` requires, and the borrowed slice above has ended.
        if let Err(error) = unsafe { self.capture.ReleaseBuffer(frames) } {
            return self.lost(&error, "returning a packet to the audio device");
        }

        Polled::Packet {
            arrived: AudioTimestamp::from_hundred_nanos(position),
            frames: u64::from(frames),
            discontinuity,
        }
    }

    /// Classifies a failed WASAPI call as the end of this stream.
    ///
    /// Every failure ends the stream, because there is no failure of these
    /// calls that leaves it usable, and reopening is something this crate can
    /// do. `AUDCLNT_E_DEVICE_INVALIDATED` is the expected one — it is what
    /// unplugging the endpoint produces — and is reported as such so that the
    /// ordinary case does not appear in the log as an unexplained fault.
    fn lost(&self, error: &windows::core::Error, operation: &str) -> Polled {
        if error.code() == AUDCLNT_E_DEVICE_INVALIDATED {
            return Polled::Lost(EndpointChange::CaptureEndpointInvalidated);
        }
        tracing::warn!(
            %error,
            operation,
            endpoint = self.identity.name,
            "the loopback stream failed; reopening on the default output device"
        );
        Polled::Lost(EndpointChange::CaptureEndpointInvalidated)
    }

    /// Waits for a packet, or for `limit`, whichever is sooner.
    fn wait(&self, limit: Duration) {
        match self.wake {
            Wake::Event(handle) => {
                let milliseconds = u32::try_from(limit.as_millis()).unwrap_or(u32::MAX);
                // SAFETY: `handle` is the event this stream created and owns,
                // and it is not closed until this stream is dropped.
                let _ = unsafe { WaitForSingleObject(handle, milliseconds) };
            }
            Wake::Poll => std::thread::sleep(limit.min(POLL_INTERVAL)),
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: `client` is a started `IAudioClient`; stopping one twice or
        // stopping one that failed is harmless, and there is nowhere to report
        // an error to from a drop.
        if let Err(error) = unsafe { self.client.Stop() } {
            tracing::warn!(%error, "stopping the loopback stream failed");
        }
        if let Wake::Event(handle) = self.wake {
            // SAFETY: `handle` came from `CreateEventW` in `create_wake_event`,
            // this stream has owned it since, and nothing refers to it after
            // the drop. The audio client above has been stopped, so Windows is
            // no longer signalling it.
            if let Err(error) = unsafe { CloseHandle(handle) } {
                tracing::warn!(%error, "closing the loopback stream's event handle failed");
            }
        }
    }
}

/// Creates the event WASAPI signals, and hands it to the client.
///
/// The handle is returned rather than stored so that a failure to attach it
/// closes it here instead of leaking one handle per attempt on a machine where
/// event-driven loopback is refused.
fn create_wake_event(client: &IAudioClient) -> windows::core::Result<Wake> {
    // SAFETY: all four arguments are optional and null/false is valid for each:
    // default security, auto-reset, initially unsignalled, unnamed.
    let handle = unsafe { CreateEventW(None, false, false, None) }?;
    // SAFETY: `handle` is the event just created, and `client` is an
    // initialised audio client that has not been started.
    match unsafe { client.SetEventHandle(handle) } {
        Ok(()) => Ok(Wake::Event(handle)),
        Err(error) => {
            // SAFETY: `handle` is owned here and nothing else refers to it.
            let _ = unsafe { CloseHandle(handle) };
            Err(error)
        }
    }
}

/// How much of what a capture produced came from where.
///
/// Cheap counters rather than a measurement subsystem, and read by the probe
/// example, by the tests, and eventually by the diagnostics screen SPEC.md
/// section 36 describes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureStats {
    /// Frames handed to the caller, real and synthesised.
    pub frames: u64,
    /// Of those, frames of silence this crate synthesised because the endpoint
    /// produced nothing for that period.
    pub synthesised_silence_frames: u64,
    /// Times the endpoint was reopened, for any reason.
    pub endpoint_changes: u64,
    /// Packets the audio engine flagged as following lost data.
    pub discontinuities: u64,
}

/// What [`SystemAudioCapture::read`] has decided to return, without borrowing
/// anything yet.
///
/// The borrow is taken after the decision rather than during it, because a
/// buffer borrowed from `self` inside the loop would keep `self` borrowed for
/// the next iteration. `clipped-capture` splits `acquire` the same way.
enum Ready {
    Idle,
    FormatChanged(AudioFormat),
    Silence {
        samples: usize,
        timestamp: AudioTimestamp,
    },
    Packet {
        start: usize,
        timestamp: AudioTimestamp,
    },
}

/// System audio, captured from the endpoint Windows is playing through.
///
/// See the module documentation for the threading and ownership rules, and
/// `docs/audio-routing.md` for what happens when the endpoint changes.
#[derive(Debug)]
pub struct SystemAudioCapture {
    enumerator: IMMDeviceEnumerator,
    watch: Arc<EndpointWatch>,
    /// Held for its [`Drop`], which unsubscribes from device notifications.
    _notifications: EndpointNotifications,
    stream: Option<Stream>,
    /// The shape of the track, fixed when the capture was opened.
    format: AudioFormat,
    timeline: Timeline,
    counter_frequency: NonZeroU64,
    /// The converted samples of the packet not yet handed over.
    packet: Vec<f32>,
    /// Samples at the front of `packet` that the timeline said to discard.
    packet_offset: usize,
    /// Whether `packet` holds something to hand over.
    packet_pending: bool,
    /// Zeroes, long enough for one instalment of silence.
    silence: Vec<f32>,
    /// When to look for a default endpoint again, when there is no stream.
    retry_at: Option<Instant>,
    /// Whether the last endpoint found was unusable, in which case there is no
    /// point looking again until something changes.
    awaiting_change: bool,
    /// A format change to report to the caller, once.
    format_change: Option<AudioFormat>,
    closed: bool,
    stats: CaptureStats,
}

// SAFETY: `SystemAudioCapture` is `Send` so that a session can open it and move
// it onto the thread that will read it, which is how the module documentation
// says it is used.
//
// The COM interfaces it holds are all free-threaded: `IMMDeviceEnumerator` and
// `IMMNotificationClient` are documented as such, `IAudioClient` and
// `IAudioCaptureClient` are activated in the multi-threaded apartment, and that
// apartment belongs to the process rather than to any thread (`apartment.rs`),
// so there is no per-thread COM state for a move to invalidate. The event
// handle is a kernel object with no thread affinity.
//
// What is *not* claimed is that two threads may use one capture at once. They
// may not, and nothing here makes that possible: this type is not `Sync`, every
// method that does anything takes `&mut self`, and a `CapturedAudio` borrows
// the capture it came from.
unsafe impl Send for SystemAudioCapture {}

impl SystemAudioCapture {
    /// Opens a capture of the endpoint Windows is currently playing through.
    ///
    /// The endpoint's mix format becomes the format of the whole capture; see
    /// [`format`](Self::format).
    ///
    /// # Errors
    ///
    /// [`AudioError::NoEndpoint`] when the machine has no default output
    /// device, because there is then no system audio to record and no format to
    /// give a track. Once a capture is open the same situation is survivable and
    /// is not an error: see `docs/audio-routing.md`.
    ///
    /// [`AudioError::UnsupportedFormat`] when the endpoint presents samples in
    /// a shape this crate will not convert, and [`AudioError::Platform`] when
    /// Windows refuses something outright.
    pub fn open() -> Result<Self, AudioError> {
        ensure_multi_threaded_apartment()
            .map_err(|error| platform_error("preparing the COM apartment", error))?;

        let enumerator = create_enumerator()?;
        let watch = Arc::new(EndpointWatch::default());
        let notifications = EndpointNotifications::register(&enumerator, Arc::clone(&watch))?;

        let stream = Stream::open(&enumerator)?.ok_or(AudioError::NoEndpoint)?;
        let format = stream.format;
        watch.set_captured(Some(stream.identity.id.clone()));

        let counter_frequency = performance_counter_frequency()?;
        let opened = read_performance_counter(counter_frequency)?;

        tracing::info!(
            endpoint = stream.identity.name,
            endpoint_id = stream.identity.id,
            sample_rate = format.sample_rate().get(),
            channels = format.channels().get(),
            channel_layout = %format.channel_mask(),
            endpoint_samples = %format.endpoint_samples(),
            wake = ?stream.wake,
            "system audio capture started"
        );

        Ok(Self {
            enumerator,
            watch,
            _notifications: notifications,
            stream: Some(stream),
            format,
            timeline: Timeline::new(format, opened),
            counter_frequency,
            packet: Vec::new(),
            packet_offset: 0,
            packet_pending: false,
            silence: Vec::new(),
            retry_at: None,
            awaiting_change: false,
            format_change: None,
            closed: false,
            stats: CaptureStats::default(),
        })
    }

    /// The shape of every buffer this capture produces.
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// The name Windows gives the endpoint being captured, if one is open.
    ///
    /// [`None`] while the machine has no usable default output device, during
    /// which the capture is producing silence.
    #[must_use]
    pub fn endpoint_name(&self) -> Option<&str> {
        self.stream
            .as_ref()
            .map(|stream| stream.identity.name.as_str())
    }

    /// What this capture has produced so far.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        CaptureStats {
            // Taken from the timeline rather than counted again here: the
            // timeline is what decides how many frames exist, and a second
            // counter beside it would only ever be a way of disagreeing.
            frames: self.timeline.frames_emitted(),
            ..self.stats
        }
    }

    /// Reads the next block of audio, waiting up to `timeout` for one.
    ///
    /// Consecutive buffers are exactly contiguous: each one's timestamp is the
    /// previous one's plus the previous one's duration, whatever the endpoint
    /// did in between. Periods the endpoint produced nothing for come back as
    /// [`SampleOrigin::SynthesisedSilence`] of the right length rather than as
    /// nothing at all, which is the difference between an audio track that
    /// stays with its video and one that slides forwards all session.
    ///
    /// # Errors
    ///
    /// [`AudioError::NotOpen`] after [`close`](Self::close). Endpoint failures
    /// are not errors: they are handled, logged, and reported through
    /// [`Capture`].
    pub fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
        if self.closed {
            return Err(AudioError::NotOpen);
        }

        let deadline = Instant::now() + timeout;
        let ready = self.next_ready(deadline)?;

        Ok(match ready {
            Ready::Idle => Capture::Idle,
            Ready::FormatChanged(format) => Capture::FormatChanged(format),
            Ready::Silence { samples, timestamp } => Capture::Samples(CapturedAudio::new(
                &self.silence[..samples],
                self.format,
                timestamp,
                SampleOrigin::SynthesisedSilence,
            )),
            Ready::Packet { start, timestamp } => Capture::Samples(CapturedAudio::new(
                &self.packet[start..],
                self.format,
                timestamp,
                SampleOrigin::Endpoint,
            )),
        })
    }

    /// Stops capturing and releases the endpoint.
    ///
    /// Idempotent, and does the same thing as dropping the capture. A closed
    /// capture cannot be reopened; open a new one.
    pub fn close(&mut self) {
        if self.stream.take().is_some() || !self.closed {
            self.watch.set_captured(None);
            self.closed = true;
            tracing::info!(
                frames = self.timeline.frames_emitted(),
                synthesised_silence_frames = self.stats.synthesised_silence_frames,
                endpoint_changes = self.stats.endpoint_changes,
                discontinuities = self.stats.discontinuities,
                "system audio capture stopped"
            );
        }
    }

    /// Decides what the next read returns, without borrowing any buffer.
    fn next_ready(&mut self, deadline: Instant) -> Result<Ready, AudioError> {
        loop {
            if let Some(format) = self.format_change.take() {
                return Ok(Ready::FormatChanged(format));
            }

            let instalment = self.timeline.take_silence_instalment();
            if instalment > 0 {
                let samples = instalment as usize * usize::from(self.format.channels().get());
                if self.silence.len() < samples {
                    // Only ever grown, and only ever with zeroes, so the buffer
                    // stays silent without being rewritten each time. Bounded
                    // by one silence instalment however long the silence is.
                    self.silence.resize(samples, 0.0);
                }
                let timestamp = self.timeline.emit(instalment);
                self.stats.synthesised_silence_frames += instalment;
                return Ok(Ready::Silence { samples, timestamp });
            }

            if self.packet_pending {
                self.packet_pending = false;
                let channels = usize::from(self.format.channels().get());
                let start = self.packet_offset * channels;
                let frames = ((self.packet.len() - start) / channels) as u64;
                let timestamp = self.timeline.emit(frames);
                return Ok(Ready::Packet { start, timestamp });
            }

            self.service_endpoint();
            if self.format_change.is_some() {
                continue;
            }

            match self.poll_stream() {
                Polled::Packet {
                    arrived,
                    frames,
                    discontinuity,
                } => self.accept_packet(arrived, frames, discontinuity),
                Polled::Lost(change) => {
                    self.watch.request_reopen(change);
                }
                Polled::Empty => {
                    let now = Instant::now();
                    if now >= deadline {
                        self.timeline.owe_silence_until(self.counter_now()?);
                        if self.timeline.silence_owed() == 0 {
                            return Ok(Ready::Idle);
                        }
                        continue;
                    }
                    match self.stream.as_ref() {
                        Some(stream) => stream.wait((deadline - now).min(WAIT_SLICE)),
                        // No endpoint at all: there is nothing to wait on, and
                        // spinning would burn a core while the user looks for
                        // the cable.
                        None => std::thread::sleep((deadline - now).min(POLL_INTERVAL)),
                    }
                    self.timeline.owe_silence_until(self.counter_now()?);
                }
            }
        }
    }

    /// Places a freshly converted packet on the timeline.
    fn accept_packet(&mut self, arrived: AudioTimestamp, frames: u64, discontinuity: bool) {
        if discontinuity {
            self.stats.discontinuities += 1;
        }

        match self.timeline.plan(arrived, frames) {
            Continuity::Continue => {
                self.packet_offset = 0;
                self.packet_pending = true;
            }
            Continuity::SilenceFirst(silence) => {
                tracing::debug!(
                    silence_frames = silence,
                    discontinuity,
                    "filling a gap the audio device produced nothing for"
                );
                self.timeline.owe_silence(silence);
                self.packet_offset = 0;
                self.packet_pending = true;
            }
            Continuity::Trim(overlap) => {
                tracing::debug!(
                    trimmed_frames = overlap,
                    "trimming audio that overlaps silence already reported"
                );
                self.packet_offset = overlap as usize;
                self.packet_pending = true;
            }
            Continuity::Drop => {
                tracing::debug!(
                    frames,
                    "discarding audio that lies entirely inside a period already reported"
                );
            }
        }
    }

    /// Looks at the endpoint, if there is one.
    fn poll_stream(&mut self) -> Polled {
        let Self { stream, packet, .. } = self;
        match stream.as_mut() {
            Some(stream) => stream.next_packet(packet),
            None => Polled::Empty,
        }
    }

    /// Acts on anything that has happened to the default endpoint.
    ///
    /// Runs on the capture thread, between reads, which is the only place a
    /// stream may be torn down and rebuilt: the notification callbacks
    /// themselves only set a flag (`notifications.rs`).
    fn service_endpoint(&mut self) {
        if let Some(change) = self.watch.take_change() {
            if self.stream.is_some() {
                tracing::info!(
                    reason = change.as_str(),
                    endpoint = self.endpoint_name().unwrap_or("<none>"),
                    "the audio output device being recorded changed; moving system audio \
                     capture to the new default. The recording continues, and the gap is \
                     filled with silence"
                );
            }
            self.stream = None;
            self.watch.set_captured(None);
            self.awaiting_change = false;
            self.retry_at = None;
            self.stats.endpoint_changes += 1;
        }

        if self.stream.is_some() || self.awaiting_change {
            return;
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }

        match Stream::open(&self.enumerator) {
            Ok(Some(stream)) if self.format.is_interchangeable_with(&stream.format) => {
                tracing::info!(
                    endpoint = stream.identity.name,
                    endpoint_id = stream.identity.id,
                    "system audio capture resumed on the default output device"
                );
                self.watch.set_captured(Some(stream.identity.id.clone()));
                self.stream = Some(stream);
                self.retry_at = None;
            }
            Ok(Some(stream)) => {
                // The recording cannot follow this endpoint without resampling
                // or remixing, and neither exists yet (issue #30). Ending the
                // recording over a headset would be the worse answer, so the
                // track becomes silence, the caller is told once, and the
                // capture waits in case the user goes back to a device it can
                // use.
                tracing::warn!(
                    endpoint = stream.identity.name,
                    from = %self.format,
                    to = %stream.format,
                    "the new default output device presents audio in a different shape, \
                     which Clipped cannot yet convert mid-recording. System audio will be \
                     silent until the recording is restarted or the previous device is \
                     selected again"
                );
                self.format_change = Some(stream.format);
                self.awaiting_change = true;
            }
            Ok(None) => {
                tracing::warn!(
                    "there is no audio output device at the moment, so system audio is \
                     silence until one appears. The recording continues"
                );
                self.retry_at = Some(Instant::now() + ENDPOINT_RETRY);
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not open the default audio output device; system audio is \
                     silence until it can be opened. The recording continues"
                );
                self.retry_at = Some(Instant::now() + ENDPOINT_RETRY);
            }
        }
    }

    /// Reads the performance counter.
    ///
    /// The one place in this crate that reads a clock rather than being told a
    /// position. It is used only to measure how long the endpoint has been
    /// saying nothing, which is a period the device will never describe; see
    /// `Timeline::owe_silence_until`.
    fn counter_now(&self) -> Result<AudioTimestamp, AudioError> {
        read_performance_counter(self.counter_frequency)
    }

    /// Makes the capture behave as though Windows had reported `change`.
    ///
    /// The path a real notification takes is `IMMNotificationClient` →
    /// [`EndpointWatch::request_reopen`] → the capture noticing on its next
    /// read, and this enters it at the second step. Everything after that — the
    /// stream being torn down, the default endpoint being looked up again, the
    /// new one being opened, the silence covering the outage — is the same code
    /// that runs when a headset is plugged in, which is what makes it worth
    /// testing this way: the alternative is a test that can only be run by a
    /// person with a cable in their hand.
    #[cfg(test)]
    fn simulate_endpoint_change(&self, change: EndpointChange) {
        self.watch.request_reopen(change);
    }
}

impl Drop for SystemAudioCapture {
    fn drop(&mut self) {
        self.close();
    }
}

/// `QueryPerformanceFrequency`, which is fixed for the life of the system.
fn performance_counter_frequency() -> Result<NonZeroU64, AudioError> {
    let mut frequency = 0i64;
    // SAFETY: the out parameter is a live local of the type the signature
    // names.
    unsafe { QueryPerformanceFrequency(&mut frequency) }
        .map_err(|error| platform_error("reading the performance counter frequency", error))?;

    NonZeroU64::new(frequency.unsigned_abs()).ok_or_else(|| {
        platform_error(
            "reading the performance counter frequency",
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_UNEXPECTED),
        )
    })
}

/// `QueryPerformanceCounter`, as an [`AudioTimestamp`].
fn read_performance_counter(frequency: NonZeroU64) -> Result<AudioTimestamp, AudioError> {
    let mut ticks = 0i64;
    // SAFETY: the out parameter is a live local of the type the signature
    // names.
    unsafe { QueryPerformanceCounter(&mut ticks) }
        .map_err(|error| platform_error("reading the performance counter", error))?;
    Ok(AudioTimestamp::from_performance_counter(
        ticks.unsigned_abs(),
        frequency,
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// The environment variable that turns "this machine has no sound card"
    /// from a skip into a failure.
    ///
    /// Set it on any machine that is supposed to be able to record system
    /// audio. It is deliberately not set in the pull-request CI job, because a
    /// GitHub Windows runner has no audio endpoint at all and a test that
    /// cannot run there must say so rather than pretend.
    const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

    /// Reports that a test could not run here, and returns whether the caller
    /// should return early.
    ///
    /// Written through `std::io::stderr()` rather than with `eprintln!`
    /// because libtest captures the macros: a skip printed with `eprintln!` is
    /// invisible in a passing run, which is exactly the failure mode — a
    /// regression that turns this test into a no-op looks like a test that
    /// passed.
    pub(crate) fn skipped(reason: &str) -> bool {
        if std::env::var_os(REQUIRE_AUDIO).is_some_and(|value| !value.is_empty()) {
            panic!("{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}");
        }
        let _ = writeln!(std::io::stderr(), "SKIPPED (audio): {reason}");
        true
    }

    /// Opens a capture, or reports why this machine cannot.
    fn open() -> Option<SystemAudioCapture> {
        match SystemAudioCapture::open() {
            Ok(capture) => Some(capture),
            Err(AudioError::NoEndpoint) => {
                skipped("this machine has no default audio output device");
                None
            }
            Err(error) => {
                skipped(&format!(
                    "system audio capture could not be opened: {error}"
                ));
                None
            }
        }
    }

    #[test]
    fn a_capture_produces_a_contiguous_timeline_for_as_long_as_it_is_read() {
        // The property everything downstream depends on, asserted against the
        // real endpoint rather than against the timeline in isolation: buffers
        // arrive with no gaps and no overlaps, and two seconds of reading
        // produces two seconds of audio whether or not anything is playing.
        let Some(mut capture) = open() else { return };
        let format = capture.format();

        let started = Instant::now();
        let mut expected: Option<AudioTimestamp> = None;
        let mut frames = 0u64;

        while started.elapsed() < Duration::from_secs(2) {
            match capture
                .read(Duration::from_millis(200))
                .expect("a healthy capture does not fail")
            {
                Capture::Samples(samples) => {
                    if let Some(expected) = expected {
                        assert_eq!(
                            samples.timestamp(),
                            expected,
                            "buffers must be exactly contiguous"
                        );
                    }
                    frames += samples.frames() as u64;
                    expected = Some(AudioTimestamp::from_nanos(
                        samples.timestamp().as_nanos()
                            + format.frames_to_nanos(samples.frames() as u64),
                    ));
                }
                Capture::Idle => {}
                Capture::FormatChanged(_) => {
                    skipped("the default output device changed during the test");
                    return;
                }
            }
        }

        let seconds = frames as f64 / f64::from(format.sample_rate().get());
        assert!(
            (1.8..=2.3).contains(&seconds),
            "two seconds of reading should produce about two seconds of audio, got {seconds:.3}"
        );
    }

    #[test]
    fn a_consumer_that_stalls_does_not_make_this_process_buffer_without_limit() {
        // Issue #19's third acceptance criterion. The consumer disappears for
        // well over the endpoint's buffer duration; when it comes back, the
        // capture must not hand over the whole backlog, must not have grown its
        // own buffers to hold it, and must still produce a timeline that
        // matches real time rather than one that lags further behind with every
        // stall.
        let Some(mut capture) = open() else { return };
        let format = capture.format();

        // Prime the stream so the timeline is anchored on the device.
        let _ = capture.read(Duration::from_millis(200));
        let silence_before = capture.stats().synthesised_silence_frames;

        let stall = Duration::from_millis(600);
        std::thread::sleep(stall);

        let resumed = Instant::now();
        let mut frames_after_stall = 0u64;
        let mut largest_buffer = 0usize;
        let mut silent_samples_were_silent = true;
        while resumed.elapsed() < Duration::from_millis(500) {
            if let Capture::Samples(samples) = capture
                .read(Duration::from_millis(200))
                .expect("a healthy capture does not fail")
            {
                frames_after_stall += samples.frames() as u64;
                largest_buffer = largest_buffer.max(samples.samples().len());
                if samples.origin() == SampleOrigin::SynthesisedSilence {
                    silent_samples_were_silent &=
                        samples.samples().iter().all(|sample| *sample == 0.0);
                }
            }
        }

        // The debt is paid in instalments, so no single buffer is larger than
        // one instalment however long the stall was.
        let instalment_samples =
            format.nanos_to_frames(100_000_000) as usize * usize::from(format.channels().get());
        assert!(
            largest_buffer <= instalment_samples,
            "a buffer of {largest_buffer} samples is more than one instalment \
             ({instalment_samples}); a stalled consumer must not cause an unbounded buffer"
        );

        // The stalled period is accounted for exactly once: about 1.1 seconds
        // of audio for a 600 ms stall plus 500 ms of reading.
        let seconds = frames_after_stall as f64 / f64::from(format.sample_rate().get());
        assert!(
            (0.9..=1.4).contains(&seconds),
            "a 600 ms stall followed by 500 ms of reading should produce about 1.1 seconds \
             of audio, got {seconds:.3}"
        );

        // And it is accounted for the *right way*. The audio engine holds
        // `BUFFER_DURATION` and discards the rest, so about 400 ms of the
        // 600 ms stall is audio nobody will ever see — a period the device
        // produced no samples for, exactly like a silent endpoint, and reached
        // here through the device's own reported positions rather than through
        // a clock this test read. It has to come back as silence of at least
        // that length, and that silence has to be silent.
        //
        // A lower bound rather than a range, because on a machine where nothing
        // at all was playing the whole stall is a period the endpoint said
        // nothing about, not only the part the engine could not hold. Both are
        // correct, and the bound below still fails if nothing is synthesised.
        let synthesised = capture.stats().synthesised_silence_frames - silence_before;
        let synthesised_seconds = synthesised as f64 / f64::from(format.sample_rate().get());
        let unrecoverable = stall.as_secs_f64() - 0.2;
        assert!(
            synthesised_seconds >= unrecoverable - 0.15,
            "at least the part of the stall the audio engine could not hold \
             ({unrecoverable:.2} s) should have been filled with silence, but \
             {synthesised_seconds:.3} s was"
        );
        assert!(
            silent_samples_were_silent,
            "a buffer reported as synthesised silence contained a non-zero sample"
        );
    }

    #[test]
    fn the_endpoint_changing_does_not_end_the_recording() {
        // Issue #19's second acceptance criterion, entered at the point a
        // notification enters it. Everything after that is the real path: the
        // stream is dropped, the default endpoint is looked up again, a new
        // stream is opened on it, and the outage is covered by silence so that
        // the timeline stays contiguous across it.
        let Some(mut capture) = open() else { return };
        let before = capture
            .endpoint_name()
            .expect("a capture opens with an endpoint")
            .to_owned();

        let format = capture.format();
        let mut expected: Option<AudioTimestamp> = None;
        let mut read_once = |capture: &mut SystemAudioCapture| {
            if let Capture::Samples(samples) = capture
                .read(Duration::from_millis(300))
                .expect("a healthy capture does not fail")
            {
                if let Some(expected) = expected {
                    assert_eq!(
                        samples.timestamp(),
                        expected,
                        "the timeline must stay contiguous across an endpoint change"
                    );
                }
                expected = Some(AudioTimestamp::from_nanos(
                    samples.timestamp().as_nanos()
                        + format.frames_to_nanos(samples.frames() as u64),
                ));
            }
        };

        read_once(&mut capture);
        capture.simulate_endpoint_change(EndpointChange::CaptureEndpointRemoved);

        for _ in 0..10 {
            read_once(&mut capture);
        }

        assert_eq!(
            capture.stats().endpoint_changes,
            1,
            "the change should have been acted on exactly once"
        );
        assert_eq!(
            capture.endpoint_name(),
            Some(before.as_str()),
            "reopening should land on the default endpoint, which has not actually moved"
        );
        assert!(
            capture.stats().frames > 0,
            "the recording must still be producing audio after the endpoint changed"
        );
    }

    #[test]
    fn reading_a_closed_capture_is_reported_rather_than_attempted() {
        let Some(mut capture) = open() else { return };
        capture.close();
        capture.close();

        let error = capture
            .read(Duration::from_millis(1))
            .expect_err("a closed capture has nothing to read");
        assert!(matches!(error, AudioError::NotOpen));
    }
}
