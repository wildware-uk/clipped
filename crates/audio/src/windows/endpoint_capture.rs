//! One WASAPI capture client, turned into a continuous timestamped stream.
//!
//! # What it does
//!
//! Opens a WASAPI capture — the endpoint Windows plays through, in loopback
//! mode, a microphone, or one process tree's audio — and turns it into a
//! continuous, gapless run of timestamped `f32` buffers. "Continuous" is the
//! word doing the work: what WASAPI delivers is not continuous, and most of
//! this file exists to make it so.
//!
//! # Why one engine for all three
//!
//! The captures Clipped runs differ in very little: how the client is
//! activated, whether the stream is a loopback, what makes an open stream
//! stale, and the words their log lines use. Everything else — the format being
//! read and converted, the device clock, silence being synthesised for periods
//! nothing was said about, the device being unplugged mid-recording, a stream
//! that fails the instant it opens — is identical, and was solved once for
//! system audio (issue #19). So [`EndpointCapture`] is that engine,
//! parameterised by a [`CaptureSource`], and `loopback.rs`, `microphone.rs` and
//! `process_loopback.rs` are the thin public captures over it (AGENTS.md
//! section 55).
//!
//! # Threading
//!
//! One capture, one thread, no thread created here.
//!
//! [`EndpointCapture::read`] takes `&mut self` and blocks until it has
//! something to report, so the caller supplies the thread — as
//! `clipped-capture` does for video, and for the same reason: the session owns
//! its threads and can give this one the priority and the lifetime it wants
//! rather than discovering a thread it did not ask for. The capture is [`Send`]
//! so it can be built anywhere and moved onto that thread, and is not `Sync`,
//! because two threads reading one endpoint would interleave packets and
//! nothing here would notice. A recording that captures system audio and a
//! microphone runs two of these, on two threads, with nothing shared between
//! them.
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
//! # Privacy
//!
//! A microphone's samples are the most private thing this program ever holds.
//! Nothing here writes them anywhere, and nothing here logs anything derived
//! from their values: the log lines below count frames, name devices and
//! measure durations (AGENTS.md section 13).
//!
//! # Ownership
//!
//! Every native resource has one owner and one release point. [`Stream`] holds
//! the audio client, the capture client, the endpoint's mute switch and the
//! event handle, and its [`Drop`] stops the stream and closes the handle.
//! [`EndpointCapture`] holds an `Option<Stream>`, the device enumerator and the
//! notification registration, whose own [`Drop`] unsubscribes. Closing a capture
//! is `self.stream = None`, and dropping it does the same thing, so a thread
//! that unwinds releases exactly what a clean stop would (AGENTS.md
//! section 58). The COM apartment is the one exception, and `apartment.rs` says
//! why.
//!
//! # What is not here
//!
//! The complement of a process tree — everything the machine plays *except* one
//! game — is [issue #27](https://github.com/wildware-uk/clipped/issues/27), and
//! mixing several captures into one track is
//! [issue #29](https://github.com/wildware-uk/clipped/issues/29). This is the
//! single stream everything else is built beside.

use core::num::NonZeroU64;
use core::time::Duration;
use std::sync::Arc;
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use clipped_logging::AudioSource;

use crate::buffer::{CapturedAudio, SampleOrigin};
use crate::error::{AudioError, Capture};
use crate::format::{append_as_f32, AudioFormat};
use crate::time::AudioTimestamp;
use crate::timeline::{Continuity, Timeline};
use crate::windows::apartment::ensure_multi_threaded_apartment;
use crate::windows::endpoint::{
    create_enumerator, platform_error, EndpointIdentity, EndpointMute, EndpointSource, MixFormat,
    SourceKind,
};
use crate::windows::notifications::{EndpointChange, EndpointNotifications, EndpointWatch};
use crate::windows::process_loopback::ProcessLoopbackSource;

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
pub(super) const BUFFER_DURATION: i64 = 2_000_000;

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
/// where the audio engine refuses an event-driven stream.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// How long to wait before looking for the endpoint again when there is none.
///
/// Two seconds: often enough that plugging a headset back in resumes recording
/// while the user is still holding the plug, rare enough that a machine with no
/// sound card at all is not asking Windows the same question hundreds of times
/// a minute for hours.
const ENDPOINT_RETRY: Duration = Duration::from_secs(2);

/// How long a stream has to survive before a failed call on it is treated as
/// the device having gone rather than as the device being broken.
///
/// A stream that opens and then fails at once is a broken endpoint, and
/// reopening it immediately produces an open/fail loop that never returns to
/// the caller. One that has been running longer than this and then fails is the
/// ordinary case — a headset unplugged — and reopens with no delay, because
/// that delay would be silence in somebody's recording.
///
/// Half a second: longer than the worst reopen observed here by an order of
/// magnitude, and short enough that the delay only ever applies to a device
/// that is genuinely failing.
const SETTLED: Duration = Duration::from_millis(500);

/// How the capture thread waits for the next packet.
#[derive(Debug)]
pub(super) enum Wake {
    /// WASAPI signals this handle when a packet is ready. Owned by the
    /// [`Stream`], and closed when the [`WakeEvent`] drops — after the audio
    /// client that Windows signals it through has been released, which is the
    /// order Microsoft's own event-driven capture sample uses.
    Event(WakeEvent),
    /// The audio engine refused an event-driven stream, so the packet queue is
    /// looked at on a timer instead.
    Poll,
}

/// The event handle WASAPI signals, closed when it is dropped.
///
/// A wrapper rather than a bare [`HANDLE`] closed in [`Stream::drop`] so that
/// the close is ordered by the struct's field order: `wake` is declared after
/// `client` and `capture`, so the audio client is released first and there is
/// no instant at which Windows holds a handle this process has closed.
pub(super) struct WakeEvent(HANDLE);

impl core::fmt::Debug for WakeEvent {
    /// Forwards to the handle, so that the log line naming it reads as the
    /// handle it is.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Drop for WakeEvent {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateEventW` in `create_wake_event`,
        // this value has owned it since, and nothing refers to it afterwards.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            tracing::warn!(%error, "closing the capture stream's event handle failed");
        }
    }
}

/// Where the timestamp on a packet comes from.
///
/// An endpoint reports a performance-counter position with every packet, and
/// that position is the whole basis of the timeline (`docs/audio-routing.md`).
/// A process-scoped client is a different activation path, its packets come
/// from the audio engine's mix of one process tree rather than from a device,
/// and whether it fills that field is not documented. A stream that quietly
/// reported zero for every packet would look to the timeline like audio
/// arriving hours early, which it would answer by discarding the lot: the
/// track would be silence and nothing would say why.
///
/// So a stream that is not known to report positions checks its first packet
/// against the performance counter and decides once (AGENTS.md section 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PositionTrust {
    /// The stream reports a performance-counter position and it is used.
    Reported,
    /// Nothing is known yet: the first packet decides.
    Unverified,
    /// The stream's reported positions are not performance-counter readings,
    /// so each packet is stamped with the counter as it is read instead.
    Counter,
}

/// How far a reported position may be from the counter reading taken as the
/// packet was read before it is not a reading of the same clock.
///
/// A position is normally within a device period of *now* — one measurement on
/// Windows 11 build 26200 has a loopback packet's position about 10 ms ahead of
/// the moment it is read, because it is when the endpoint will play the audio.
/// Five seconds is hundreds of times that and still nowhere near the decades a
/// counter that counts from boot is worth, so the test separates "this clock"
/// from "not this clock" without adjudicating jitter, which the timeline's
/// deadband exists for.
const POSITION_SANITY: u64 = 5_000_000_000;

/// Whether `reported` is a reading of the same clock as `now`.
///
/// Both are nanoseconds on the performance counter, which counts from boot, so
/// a position from a stream that does not fill the field — zero, or a count of
/// frames since the stream started — is many orders of magnitude away from the
/// counter rather than slightly out.
fn position_is_on_the_counter(reported: AudioTimestamp, now: AudioTimestamp) -> bool {
    reported.as_nanos().abs_diff(now.as_nanos()) < POSITION_SANITY
}

/// Everything a [`Stream`] needs that is decided by how it was opened.
///
/// A structure rather than seven arguments because the two ways of opening a
/// stream — activating an endpoint, and activating a process-scoped client
/// (`process_loopback.rs`) — differ in exactly these fields and agree on
/// everything that happens afterwards.
pub(super) struct StreamParts {
    /// What is being recorded, for the log lines.
    pub(super) kind: SourceKind,
    /// How the thing being recorded is named in a log line.
    pub(super) identity: EndpointIdentity,
    /// The shape of the samples the stream will deliver.
    pub(super) format: AudioFormat,
    /// An initialised, unstarted audio client.
    pub(super) client: IAudioClient,
    /// How the capture thread waits for packets on it.
    pub(super) wake: Wake,
    /// The endpoint's mute switch, when one was wanted and Windows offered it.
    pub(super) mute: Option<EndpointMute>,
    /// Whether the positions this stream reports can be believed.
    pub(super) positions: PositionTrust,
}

/// One live stream: an endpoint, or one process tree's audio.
///
/// Everything in here is replaced together when the endpoint changes; there is
/// no way to keep half of it.
#[derive(Debug)]
pub(super) struct Stream {
    /// What is being recorded, so that a log line raised in here says which of
    /// a session's captures it is about.
    kind: SourceKind,
    identity: EndpointIdentity,
    format: AudioFormat,
    /// When this stream was started, which is how a device that fails the
    /// moment it opens is told apart from one that has been working and has
    /// now gone; see [`SETTLED`].
    opened: Instant,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    /// Declared after `client` and `capture` so that the event handle is closed
    /// after they are released.
    wake: Wake,
    /// Bytes one frame occupies in the endpoint's buffer, cached because it is
    /// multiplied by a frame count for every packet.
    bytes_per_frame: usize,
    /// Whether the positions this stream reports can be believed, and what to
    /// do about it if not.
    positions: PositionTrust,
    /// The performance counter's frequency, needed only when a packet has to be
    /// stamped with a reading taken here rather than with the one the stream
    /// reported. Fixed for the life of the system.
    counter_frequency: NonZeroU64,
    /// The endpoint's mute switch, when one was wanted and Windows offered it.
    /// Only microphones ask: a muted microphone is the commonest reason a
    /// microphone track is silent and the stream itself cannot tell.
    mute: Option<EndpointMute>,
    /// An `HRESULT` to return in place of the one the next WASAPI call on this
    /// stream would have returned. Set only by
    /// [`EndpointCapture::fail_every_endpoint_call_with`], and read in
    /// [`Stream::next_packet_size`], so that [`Stream::lost`] classifies an
    /// injected failure exactly as it classifies a real one.
    #[cfg(test)]
    injected_failure: Option<windows::core::HRESULT>,
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
    /// Opens a stream on whatever `source` describes.
    ///
    /// [`None`] when the thing to record is not there: the machine has no
    /// default output device, the chosen microphone is unplugged, or the game's
    /// process tree has no living member. That is a state to wait through
    /// rather than an error, so it is not one.
    fn open(
        source: &mut CaptureSource,
        enumerator: &IMMDeviceEnumerator,
        watch: &EndpointWatch,
        counter_frequency: NonZeroU64,
    ) -> Result<Option<Self>, AudioError> {
        let parts = match source {
            CaptureSource::Endpoint(endpoint) => Self::open_endpoint(endpoint, enumerator, watch)?,
            CaptureSource::ProcessTree(process) => process.open_stream(enumerator)?,
        };
        parts
            .map(|parts| Self::start(parts, counter_frequency))
            .transpose()
    }

    /// Starts a stream on an initialised client, whichever way it was
    /// activated.
    ///
    /// The half of opening that is the same for an endpoint and for a
    /// process-scoped client: obtain the capture client, start the stream, and
    /// work out the byte arithmetic every packet is read with.
    fn start(parts: StreamParts, counter_frequency: NonZeroU64) -> Result<Self, AudioError> {
        // SAFETY: `client` is an initialised `IAudioClient`, which is when
        // `GetService` is valid; the interface identifier comes from the
        // return type.
        let capture: IAudioCaptureClient = unsafe { parts.client.GetService() }
            .map_err(|error| platform_error("obtaining the capture client", error))?;

        // SAFETY: `client` is initialised and not started.
        unsafe { parts.client.Start() }
            .map_err(|error| platform_error("starting the capture stream", error))?;

        let format = parts.format;
        Ok(Self {
            kind: parts.kind,
            identity: parts.identity,
            format,
            opened: Instant::now(),
            client: parts.client,
            capture,
            wake: parts.wake,
            bytes_per_frame: usize::from(format.channels().get())
                * format.endpoint_samples().bytes_per_sample(),
            mute: parts.mute,
            positions: parts.positions,
            counter_frequency,
            #[cfg(test)]
            injected_failure: None,
        })
    }

    /// Activates and initialises a client on the endpoint `source` describes.
    ///
    /// `watch` is told which endpoint this is as soon as the endpoint is known,
    /// which is before the stream exists rather than after. `Activate`,
    /// `Initialize` and `Start` take long enough for a device to be unplugged
    /// during them, and a notification that arrives while the watch says no
    /// endpoint is being captured is discarded as somebody else's business.
    fn open_endpoint(
        source: &EndpointSource,
        enumerator: &IMMDeviceEnumerator,
        watch: &EndpointWatch,
    ) -> Result<Option<StreamParts>, AudioError> {
        let Some(device) = source.resolve(enumerator)? else {
            watch.set_captured(None);
            return Ok(None);
        };
        let identity = EndpointIdentity::of(&device)?;
        watch.set_captured(Some(identity.id.clone()));

        // SAFETY: `device` is a live `IMMDevice`; windows-rs infers the
        // interface identifier from the return type, so the activation cannot
        // ask for one interface and be typed as another.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|error| platform_error("activating the audio client", error))?;

        let mix = MixFormat::of(&client)?;
        let format = mix.audio();
        let flags = source.kind.stream_flags();

        // Event-driven first. It is what lets the capture thread sleep until
        // there is something to do instead of waking every few milliseconds to
        // find nothing, and — measured on Windows 11 build 26200 — the audio
        // engine accepts it for both a loopback stream and a microphone. It is
        // not documented to for loopback, and older Windows builds and some
        // drivers refuse, so a refusal falls back to polling rather than
        // failing: a recording with the audio captured on a timer is better
        // than one without the audio.
        //
        // SAFETY: `mix.as_ptr()` is the `WAVEFORMATEX` this endpoint's own
        // `GetMixFormat` returned and `mix` outlives the call; the remaining
        // arguments are constants of the API. `Initialize` is called at most
        // once per `IAudioClient`, which is why the fallback below activates a
        // fresh one.
        let event_driven = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
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
                    audio_source = %source.kind.audio_source(),
                    "this audio device would not drive a stream from an event, so it is read \
                     on a timer instead"
                );
                // SAFETY: as above. A second `IAudioClient` is activated
                // because `Initialize` may not be called twice on one.
                let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
                    .map_err(|error| platform_error("activating the audio client", error))?;
                // SAFETY: as above.
                unsafe {
                    client.Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        flags,
                        BUFFER_DURATION,
                        0,
                        mix.as_ptr(),
                        None,
                    )
                }
                .map_err(|error| platform_error("opening a stream on the audio device", error))?;
                (client, Wake::Poll)
            }
        };

        Ok(Some(StreamParts {
            kind: source.kind,
            identity,
            format,
            client,
            wake,
            mute: match source.kind {
                // Only a microphone asks: a muted microphone is the commonest
                // reason a microphone track is silent and the stream itself
                // cannot tell.
                SourceKind::Microphone => EndpointMute::of(&device),
                SourceKind::SystemAudio | SourceKind::GameAudio => None,
            },
            // An endpoint reports the position of every packet it delivers, and
            // `tests/system_audio.rs` asserts that it does.
            positions: PositionTrust::Reported,
        }))
    }

    /// Takes the next packet, converting it into `out`.
    ///
    /// `out` is cleared first and left holding the packet's samples as
    /// interleaved `f32`. It keeps its capacity between calls, so a steady
    /// stream allocates nothing after the first packet.
    fn next_packet(&mut self, out: &mut Vec<f32>) -> Polled {
        let available = match self.next_packet_size() {
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
            // burst of noise in a quiet passage. A muted microphone produces
            // exactly this, packet after packet.
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

        let arrived = match self.arrival_of(position) {
            Ok(arrived) => arrived,
            Err(error) => return self.lost_reading_the_counter(&error),
        };

        Polled::Packet {
            arrived,
            frames: u64::from(frames),
            discontinuity,
        }
    }

    /// Where a packet belongs on the performance counter.
    ///
    /// The position the stream reported, unless this is a stream whose
    /// positions have not been shown to be counter readings — see
    /// [`PositionTrust`] — in which case the first packet decides for the rest
    /// of the stream's life.
    fn arrival_of(&mut self, position: u64) -> Result<AudioTimestamp, AudioError> {
        let reported = AudioTimestamp::from_hundred_nanos(position);
        match self.positions {
            PositionTrust::Reported => Ok(reported),
            PositionTrust::Counter => read_performance_counter(self.counter_frequency),
            PositionTrust::Unverified => {
                let now = read_performance_counter(self.counter_frequency)?;
                if position_is_on_the_counter(reported, now) {
                    self.positions = PositionTrust::Reported;
                    return Ok(reported);
                }
                tracing::warn!(
                    audio_source = %self.kind.audio_source(),
                    reported_nanos = reported.as_nanos(),
                    counter_nanos = now.as_nanos(),
                    "this capture's packets do not carry a performance-counter position, so \
                     each one is timed as it is read instead. The track stays the length of \
                     the recording; what is lost is the drift measurement against the \
                     endpoint's own clock"
                );
                self.positions = PositionTrust::Counter;
                Ok(now)
            }
        }
    }

    /// Ends the stream because the performance counter could not be read.
    ///
    /// Unreachable in practice — `QueryPerformanceCounter` cannot fail on any
    /// machine Windows runs on — and handled rather than unwrapped because this
    /// is a recorder that must not panic on a capture thread (AGENTS.md
    /// section 17).
    fn lost_reading_the_counter(&self, error: &AudioError) -> Polled {
        tracing::warn!(
            %error,
            audio_source = %self.kind.audio_source(),
            "the performance counter could not be read, so this capture is reopening"
        );
        Polled::Lost(EndpointChange::CaptureEndpointInvalidated)
    }

    /// How many frames are queued, or the failure a test asked for in place of
    /// the answer.
    ///
    /// The first call every look takes, and therefore the one place a stream
    /// that has gone is normally found out. A test injects here rather than at
    /// the caller so that what it replaces is the `HRESULT` WASAPI returned,
    /// not the decision made about it: [`Stream::lost`] runs on an injected
    /// failure exactly as it runs on a real one.
    fn next_packet_size(&self) -> windows::core::Result<u32> {
        #[cfg(test)]
        if let Some(code) = self.injected_failure {
            return Err(windows::core::Error::from_hresult(code));
        }

        // SAFETY: `capture` is a live `IAudioCaptureClient` on a started
        // stream.
        unsafe { self.capture.GetNextPacketSize() }
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
            audio_source = %self.kind.audio_source(),
            device = self.identity.name,
            "the capture stream failed; the endpoint will be opened again"
        );
        Polled::Lost(EndpointChange::CaptureEndpointInvalidated)
    }

    /// Waits for a packet, or for `limit`, whichever is sooner.
    fn wait(&self, limit: Duration) {
        match &self.wake {
            Wake::Event(event) => {
                let milliseconds = u32::try_from(limit.as_millis()).unwrap_or(u32::MAX);
                // SAFETY: `event.0` is the event this stream created and owns,
                // and it is not closed until this stream is dropped.
                let _ = unsafe { WaitForSingleObject(event.0, milliseconds) };
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
            tracing::warn!(%error, "stopping the capture stream failed");
        }
        // The event handle is closed by `WakeEvent`'s own drop, which runs
        // after the fields declared before it have been released.
    }
}

/// Creates the event WASAPI signals, and hands it to the client.
///
/// The handle is returned rather than stored so that a failure to attach it
/// closes it here instead of leaking one handle per attempt on a machine where
/// event-driven capture is refused.
pub(super) fn create_wake_event(client: &IAudioClient) -> windows::core::Result<Wake> {
    // SAFETY: all four arguments are optional and null/false is valid for each:
    // default security, auto-reset, initially unsignalled, unnamed.
    let handle = unsafe { CreateEventW(None, false, false, None) }?;
    // SAFETY: `handle` is the event just created, and `client` is an
    // initialised audio client that has not been started.
    match unsafe { client.SetEventHandle(handle) } {
        Ok(()) => Ok(Wake::Event(WakeEvent(handle))),
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
/// examples, by the tests, and eventually by the diagnostics screen SPEC.md
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

/// What [`EndpointCapture::read`] has decided to return, without borrowing
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
        /// Where the endpoint said the first frame handed over belongs, which
        /// is what [`CapturedAudio::device_timestamp`] reports and what makes
        /// the track's drift against the reference clock measurable.
        device: AudioTimestamp,
    },
}

/// What a capture is a capture *of*.
///
/// The engine below is the same whichever this is — the mix format, the device
/// clock, the silence synthesised for periods nothing was said about, the
/// stream that fails and has to be opened again — so the difference is one
/// enumeration rather than a second implementation of all of it (AGENTS.md
/// section 55). Exactly three things vary: how a stream is activated, what
/// makes an open stream stale, and the words the log lines use.
#[derive(Debug)]
pub(super) enum CaptureSource {
    /// One audio endpoint: the one Windows plays through, or a microphone.
    Endpoint(EndpointSource),
    /// Everything one process tree plays, through process loopback
    /// (`process_loopback.rs`).
    ProcessTree(ProcessLoopbackSource),
}

impl CaptureSource {
    /// What is being recorded.
    fn kind(&self) -> SourceKind {
        match self {
            Self::Endpoint(endpoint) => endpoint.kind,
            Self::ProcessTree(_) => SourceKind::GameAudio,
        }
    }

    /// What the `audio_source` field on this capture's log lines says.
    fn audio_source(&self) -> AudioSource {
        self.kind().audio_source()
    }

    /// How the thing being recorded is named in a log line, including while it
    /// is not there to be asked.
    fn description(&self) -> &str {
        match self {
            Self::Endpoint(endpoint) => endpoint.device_description(),
            Self::ProcessTree(process) => process.description(),
        }
    }

    /// Whether Windows moving the default endpoint concerns this capture.
    fn follows_default(&self) -> bool {
        match self {
            Self::Endpoint(endpoint) => endpoint.follows_default(),
            // A process-scoped client is not on an endpoint, so no endpoint
            // notification is about it.
            Self::ProcessTree(_) => false,
        }
    }

    /// Whether this capture has any reason to subscribe to device changes.
    fn watches_devices(&self) -> bool {
        matches!(self, Self::Endpoint(_))
    }

    /// Anything that has happened to what is being recorded that means the
    /// current stream has to be replaced.
    fn take_change(&mut self) -> Option<Reopen> {
        match self {
            Self::Endpoint(_) => None,
            Self::ProcessTree(process) => process.take_change(),
        }
    }
}

/// Why a capture is about to throw its stream away and open another.
///
/// Carried rather than acted on immediately so that the decision and the log
/// line are made in one place, and so that the two reasons which can repeat
/// without limit are told apart from the ones a person's hand paces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Reopen {
    /// What to put in the log line, phrased as the thing that happened.
    pub(super) reason: &'static str,
    /// Whether this was raised by a failed call on the stream rather than by
    /// something outside it. Those are the ones that can loop: a stream that
    /// fails on every look raises one every time it is read, so a stream that
    /// has only just been opened and has already failed is left alone for a
    /// moment rather than reopened at once.
    pub(super) from_failed_call: bool,
}

/// What a process-scoped capture's tree looks like at one moment.
///
/// Two facts a caller acts on rather than the tree itself: the tree is owned by
/// the capture thread and lending it out would let something else scan the
/// process table from wherever it liked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TreeState {
    /// The process the current activation names.
    pub(super) scoped_to: u32,
    /// How many processes of the game are running.
    pub(super) members: usize,
}

/// A capture of one endpoint or one process tree.
///
/// See the module documentation for the threading and ownership rules, and
/// `docs/audio-routing.md` for what happens when the endpoint changes. The
/// public captures built on this are `SystemAudioCapture`, `MicrophoneCapture`
/// and `ProcessLoopbackCapture`.
#[derive(Debug)]
pub(super) struct EndpointCapture {
    source: CaptureSource,
    enumerator: IMMDeviceEnumerator,
    watch: Arc<EndpointWatch>,
    /// Held for its [`Drop`], which unsubscribes from device notifications.
    /// [`None`] for a capture no device notification can concern.
    _notifications: Option<EndpointNotifications>,
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
    /// The position the endpoint reported for the first frame of `packet` that
    /// survives `packet_offset`. Meaningful only while `packet_pending`.
    packet_device: AudioTimestamp,
    /// Zeroes, long enough for one instalment of silence.
    silence: Vec<f32>,
    /// When to look for the endpoint again, when there is no stream.
    retry_at: Option<Instant>,
    /// Whether the last endpoint found was unusable, in which case there is no
    /// point looking again until something changes.
    awaiting_change: bool,
    /// A format change to report to the caller, once.
    format_change: Option<AudioFormat>,
    /// Whether the capture is handing over what the audio engine still holds
    /// and then closing; see [`begin_drain`](Self::begin_drain).
    draining: bool,
    closed: bool,
    stats: CaptureStats,
    /// The `HRESULT` every WASAPI call on this capture's stream returns instead
    /// of doing anything, including on a stream opened after the current one is
    /// torn down. Set only by
    /// [`fail_every_endpoint_call_with`](Self::fail_every_endpoint_call_with).
    #[cfg(test)]
    injected_failure: Option<windows::core::HRESULT>,
}

// SAFETY: `EndpointCapture` is `Send` so that a session can open it and move it
// onto the thread that will read it, which is how the module documentation says
// it is used.
//
// The COM interfaces it holds are all free-threaded: `IMMDeviceEnumerator` and
// `IMMNotificationClient` are documented as such, `IAudioClient`,
// `IAudioCaptureClient` and `IAudioEndpointVolume` are activated in the
// multi-threaded apartment, and that apartment belongs to the process rather
// than to any thread (`apartment.rs`), so there is no per-thread COM state for a
// move to invalidate. The event handle is a kernel object with no thread
// affinity.
//
// What is *not* claimed is that two threads may use one capture at once. They
// may not, and nothing here makes that possible: this type is not `Sync`, every
// method that does anything takes `&mut self`, and a `CapturedAudio` borrows
// the capture it came from.
unsafe impl Send for EndpointCapture {}

impl EndpointCapture {
    /// Opens a capture of the endpoint `source` describes.
    ///
    /// [`None`] when that endpoint does not exist: the machine has no default
    /// output device, or the chosen microphone is not plugged in. There is then
    /// no format to give a track and no recording in progress to protect, so
    /// the two public captures turn it into the error that names what is
    /// missing. Once a capture is open the same situation is survivable and is
    /// not an error: see `docs/audio-routing.md`.
    ///
    /// # Errors
    ///
    /// [`AudioError::UnsupportedFormat`] when the endpoint presents samples in
    /// a shape this crate will not convert, and [`AudioError::Platform`] when
    /// Windows refuses something outright.
    pub(super) fn open(mut source: CaptureSource) -> Result<Option<Self>, AudioError> {
        ensure_multi_threaded_apartment()
            .map_err(|error| platform_error("preparing the COM apartment", error))?;

        let enumerator = create_enumerator()?;
        let watch = Arc::new(EndpointWatch::new(
            source.kind().flow(),
            source.follows_default(),
        ));
        let notifications = source
            .watches_devices()
            .then(|| EndpointNotifications::register(&enumerator, Arc::clone(&watch)))
            .transpose()?;

        let counter_frequency = performance_counter_frequency()?;

        let Some(stream) = Stream::open(&mut source, &enumerator, &watch, counter_frequency)?
        else {
            return Ok(None);
        };
        let format = stream.format;

        let opened = read_performance_counter(counter_frequency)?;

        tracing::info!(
            audio_source = %source.audio_source(),
            device = stream.identity.name,
            device_id = stream.identity.id,
            sample_rate = format.sample_rate().get(),
            channels = format.channels().get(),
            channel_layout = %format.channel_mask(),
            endpoint_samples = %format.endpoint_samples(),
            wake = ?stream.wake,
            "audio capture started"
        );

        Ok(Some(Self {
            source,
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
            packet_device: opened,
            silence: Vec::new(),
            retry_at: None,
            awaiting_change: false,
            format_change: None,
            draining: false,
            closed: false,
            stats: CaptureStats::default(),
            #[cfg(test)]
            injected_failure: None,
        }))
    }

    /// The shape of every buffer this capture produces.
    ///
    /// Fixed when the capture is opened, and it stays fixed across an endpoint
    /// change: a capture only follows its endpoint to a device whose sample rate
    /// and channel count match, so every buffer really does have this shape for
    /// the life of the capture.
    ///
    /// The two fields that describe the endpoint rather than the buffers —
    /// [`endpoint_samples`](AudioFormat::endpoint_samples) and
    /// [`channel_mask`](AudioFormat::channel_mask) — describe the endpoint the
    /// capture *opened* on, and are not refreshed when it moves to another one.
    /// They are diagnostics rather than a description of what is handed over:
    /// the samples are `f32` whatever the endpoint delivers, because this crate
    /// converts them.
    pub(super) fn format(&self) -> AudioFormat {
        self.format
    }

    /// The name Windows gives the device being recorded, if one is open.
    ///
    /// [`None`] while there is no usable device, during which the capture is
    /// producing silence.
    pub(super) fn device_name(&self) -> Option<&str> {
        self.stream
            .as_ref()
            .map(|stream| stream.identity.name.as_str())
    }

    /// Whether the device being recorded is muted in Windows.
    ///
    /// [`None`] when there is no device open, when the capture did not ask for
    /// the mute switch, or when Windows would not give one.
    pub(super) fn is_muted(&self) -> Option<bool> {
        self.stream.as_ref()?.mute.as_ref()?.is_muted()
    }

    /// What a process-scoped capture's process tree looks like now, or [`None`]
    /// for a capture that has no tree.
    pub(super) fn tree_state(&self) -> Option<TreeState> {
        match &self.source {
            CaptureSource::Endpoint(_) => None,
            CaptureSource::ProcessTree(process) => Some(TreeState {
                scoped_to: process.scoped_to(),
                members: process.members(),
            }),
        }
    }

    /// What this capture has produced so far.
    pub(super) fn stats(&self) -> CaptureStats {
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
    pub(super) fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
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
            Ready::Packet {
                start,
                timestamp,
                device,
            } => Capture::Samples(
                CapturedAudio::new(
                    &self.packet[start..],
                    self.format,
                    timestamp,
                    SampleOrigin::Endpoint,
                )
                .with_device_timestamp(device),
            ),
        })
    }

    /// Stops capturing and releases the endpoint.
    ///
    /// Idempotent, and does the same thing as dropping the capture. A closed
    /// capture cannot be reopened; open a new one.
    pub(super) fn close(&mut self) {
        if self.stream.take().is_some() || !self.closed {
            self.watch.set_captured(None);
            self.closed = true;
            self.draining = false;
            tracing::info!(
                audio_source = %self.source.audio_source(),
                frames = self.timeline.frames_emitted(),
                synthesised_silence_frames = self.stats.synthesised_silence_frames,
                endpoint_changes = self.stats.endpoint_changes,
                discontinuities = self.stats.discontinuities,
                "audio capture stopped"
            );
        }
    }

    /// Ends the capture by handing over whatever the audio engine still holds.
    ///
    /// The audio engine keeps up to [`BUFFER_DURATION`] of audio that has been
    /// captured and not yet collected. A capture that is simply closed throws
    /// that away, which is up to 200 ms missing from the end of the track — the
    /// last thing that happened before the user stopped recording, which is
    /// often exactly what they pressed the key for.
    ///
    /// So this leaves the capture readable and stops it looking forwards:
    /// subsequent reads hand over the packets that were queued, in order, on
    /// the same timeline as everything before them, and once the queue is empty
    /// the capture closes itself and the next read reports
    /// [`AudioError::NotOpen`]. Nothing is reopened during a drain and no
    /// further silence is synthesised, because there is no more recording for
    /// silence to keep in step with: a drain ends at the last sample that
    /// exists.
    ///
    /// **The client is deliberately not stopped first.** Stopping it and then
    /// reading is the obvious order and it loses the audio: measured on Windows
    /// 11 build 26200 against a process-scoped client, a stream stopped after a
    /// 150 ms stall reported no queued packets at all, where the same stream
    /// drained before stopping produced the 150 ms. `IAudioClient::Stop` is
    /// what ends the stream, and it happens where every other release does, in
    /// [`Stream::drop`].
    ///
    /// The cost of that order is a packet that the engine is producing at the
    /// moment the queue is first found empty, which is one device period —
    /// 10 ms — against the 200 ms a close would lose.
    ///
    /// Idempotent, and pointless after [`close`](Self::close): a closed capture
    /// has already let go of the stream, and this cannot get it back.
    pub(super) fn begin_drain(&mut self) {
        if self.closed || self.draining {
            return;
        }
        self.draining = true;
        tracing::debug!(
            audio_source = %self.source.audio_source(),
            "draining the audio the capture had not collected yet"
        );
    }

    /// Decides what the next read returns, without borrowing any buffer.
    fn next_ready(&mut self, deadline: Instant) -> Result<Ready, AudioError> {
        loop {
            if let Some(format) = self.format_change.take() {
                return Ok(Ready::FormatChanged(format));
            }

            if self.draining {
                if let Some(ready) = self.drain_one() {
                    return Ok(ready);
                }
                // Everything the audio engine held has been handed over.
                self.close();
                return Ok(Ready::Idle);
            }

            if let Some(silence) = self.silence_instalment() {
                return Ok(silence);
            }

            if let Some(packet) = self.pending_packet() {
                return Ok(packet);
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
                    // The same deadline the empty case observes, and for a
                    // sharper reason: an endpoint that opens and then fails at
                    // once produces `Lost` on every look, so a loop that only
                    // left on an empty queue would never return to the caller
                    // at all. `service_endpoint` decides how long to wait before
                    // trying that endpoint again; this decides that the caller
                    // hears about the silence meanwhile.
                    if Instant::now() >= deadline {
                        self.timeline.owe_silence_until(self.counter_now()?);
                        if self.timeline.silence_owed() == 0 {
                            return Ok(Ready::Idle);
                        }
                    }
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

    /// The next instalment of owed silence, if any is owed.
    fn silence_instalment(&mut self) -> Option<Ready> {
        let instalment = self.timeline.take_silence_instalment();
        if instalment == 0 {
            return None;
        }
        let samples = instalment as usize * usize::from(self.format.channels().get());
        if self.silence.len() < samples {
            // Only ever grown, and only ever with zeroes, so the buffer stays
            // silent without being rewritten each time. Bounded by one silence
            // instalment however long the silence is.
            self.silence.resize(samples, 0.0);
        }
        let timestamp = self.timeline.emit(instalment);
        self.stats.synthesised_silence_frames += instalment;
        Some(Ready::Silence { samples, timestamp })
    }

    /// The converted packet waiting to be handed over, if there is one.
    fn pending_packet(&mut self) -> Option<Ready> {
        if !self.packet_pending {
            return None;
        }
        self.packet_pending = false;
        let channels = usize::from(self.format.channels().get());
        let start = self.packet_offset * channels;
        let frames = ((self.packet.len() - start) / channels) as u64;
        let timestamp = self.timeline.emit(frames);
        Some(Ready::Packet {
            start,
            timestamp,
            device: self.packet_device,
        })
    }

    /// The next thing a draining capture has to hand over, or [`None`] once the
    /// audio engine has nothing left.
    ///
    /// The same two producers an ordinary read has, in the same order, so a
    /// drained tail is exactly as contiguous as everything before it: silence
    /// already owed is still paid, and a packet whose position leaves a gap
    /// still has that gap filled. What a drain does not do is invent silence
    /// for time passing, look at the process tree, or open anything.
    fn drain_one(&mut self) -> Option<Ready> {
        loop {
            if let Some(silence) = self.silence_instalment() {
                return Some(silence);
            }
            if let Some(packet) = self.pending_packet() {
                return Some(packet);
            }
            match self.poll_stream() {
                Polled::Packet {
                    arrived,
                    frames,
                    discontinuity,
                } => self.accept_packet(arrived, frames, discontinuity),
                // A stream that has failed has nothing left to give, and one
                // whose queue is empty has given everything: a stopped client
                // produces no more.
                Polled::Empty | Polled::Lost(_) => return None,
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
                self.packet_device = arrived;
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
                self.packet_device = arrived;
            }
            Continuity::Trim(overlap) => {
                tracing::debug!(
                    trimmed_frames = overlap,
                    "trimming audio that overlaps silence already reported"
                );
                self.packet_offset = overlap as usize;
                self.packet_pending = true;
                // The endpoint's position is for the frame at the front of the
                // packet, and that frame is being discarded, so the position
                // reported to the caller has to advance by the frames dropped.
                // Reporting the untrimmed one would show a fixed offset that is
                // an artefact of this crate rather than a property of the clock.
                self.packet_device = AudioTimestamp::from_nanos(
                    arrived
                        .as_nanos()
                        .saturating_add(self.format.frames_to_nanos(overlap)),
                );
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
        // Carried onto the stream on every look rather than only when it is
        // opened, so that a stream this capture opens *after* the injection —
        // which is what a device failing over and over produces — fails too.
        #[cfg(test)]
        let injected_failure = self.injected_failure;

        let Self { stream, packet, .. } = self;
        match stream.as_mut() {
            Some(stream) => {
                #[cfg(test)]
                {
                    stream.injected_failure = injected_failure;
                }
                stream.next_packet(packet)
            }
            None => Polled::Empty,
        }
    }

    /// Acts on anything that has happened to the endpoint.
    ///
    /// Runs on the capture thread, between reads, which is the only place a
    /// stream may be torn down and rebuilt: the notification callbacks
    /// themselves only set a flag (`notifications.rs`).
    fn service_endpoint(&mut self) {
        let audio_source = self.source.audio_source();

        if let Some(change) = self.take_reopen() {
            // A stream that has only just been opened and has already failed on
            // the endpoint itself is a broken device, and opening it again
            // immediately is how a recorder ends up doing nothing else for the
            // rest of the day: `Activate`, `Initialize`, `Start`, fail, repeat,
            // with two log lines each time round and no read ever returning.
            // Waiting is the only thing that helps.
            //
            // The reason is part of the test, not only the age. A notification
            // is paced by a person with a plug in their hand and cannot loop; a
            // failed call on the client is raised by this crate on every look,
            // and can. So an unplug still reopens the moment it is noticed,
            // however long the stream had been running, and only a device that
            // breaks the instant it is opened is left alone for a while.
            let failed_at_once = change.from_failed_call
                && self
                    .stream
                    .as_ref()
                    .is_some_and(|stream| stream.opened.elapsed() < SETTLED);

            if self.stream.is_some() {
                tracing::info!(
                    audio_source = %audio_source,
                    reason = change.reason,
                    device = self.device_name().unwrap_or("<none>"),
                    "what this capture is recording changed; the capture is opening what it \
                     should be on now. The recording continues, and the gap is filled with \
                     silence"
                );
            }
            self.stream = None;
            self.watch.set_captured(None);
            self.awaiting_change = false;
            self.retry_at = failed_at_once.then(|| Instant::now() + ENDPOINT_RETRY);
            self.stats.endpoint_changes += 1;

            if failed_at_once {
                tracing::warn!(
                    audio_source = %audio_source,
                    retry_in_seconds = ENDPOINT_RETRY.as_secs(),
                    "the audio device failed as soon as it was opened, so it is left alone \
                     for a moment rather than reopened at once. The recording continues, and \
                     this track is silence until it works"
                );
            }
        }

        if self.stream.is_some() || self.awaiting_change {
            return;
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }

        match Stream::open(
            &mut self.source,
            &self.enumerator,
            &self.watch,
            self.counter_frequency,
        ) {
            Ok(Some(stream)) if self.format.is_interchangeable_with(&stream.format) => {
                tracing::info!(
                    audio_source = %audio_source,
                    device = stream.identity.name,
                    device_id = stream.identity.id,
                    "audio capture resumed"
                );
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
                    audio_source = %audio_source,
                    device = stream.identity.name,
                    from = %self.format,
                    to = %stream.format,
                    "the audio device now presents audio in a different shape, which Clipped \
                     cannot yet convert mid-recording. This track will be silent until the \
                     recording is restarted or the previous device is selected again"
                );
                self.format_change = Some(stream.format);
                self.awaiting_change = true;
            }
            Ok(None) => {
                tracing::warn!(
                    audio_source = %audio_source,
                    device = self.source.description(),
                    "what this capture records is not available, so this track is silence \
                     until it comes back. The recording continues"
                );
                self.retry_at = Some(Instant::now() + ENDPOINT_RETRY);
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    audio_source = %audio_source,
                    device = self.source.description(),
                    "could not open what this capture records; this track is silence until \
                     it can be opened. The recording continues"
                );
                self.retry_at = Some(Instant::now() + ENDPOINT_RETRY);
            }
        }
    }

    /// The reason to replace the current stream, if there is one.
    ///
    /// Two places can raise one and they cannot both be right at once: a device
    /// notification — or a failed call on the stream, which arrives the same way
    /// (`notifications.rs`) — and the source itself, which for a process-scoped
    /// capture is the game's process tree having moved underneath it. The
    /// device is asked first because it is the one that can be urgent.
    fn take_reopen(&mut self) -> Option<Reopen> {
        if let Some(change) = self.watch.take_change() {
            return Some(Reopen {
                reason: change.as_str(),
                from_failed_call: change == EndpointChange::CaptureEndpointInvalidated,
            });
        }
        self.source.take_change()
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
    /// stream being torn down, the endpoint being looked up again, the new one
    /// being opened, the silence covering the outage — is the same code that
    /// runs when a headset is plugged in, which is what makes it worth testing
    /// this way: the alternative is a test that can only be run by a person
    /// with a cable in their hand.
    #[cfg(test)]
    pub(super) fn simulate_endpoint_change(&self, change: EndpointChange) {
        self.watch.request_reopen(change);
    }

    /// Makes every WASAPI call on this capture's stream fail with `code`.
    ///
    /// A device that answers and then stops — the client invalidated by an
    /// unplugged microphone, a dying USB sound card that fails on the first
    /// call after it opens — cannot be reached from a healthy machine and
    /// cannot be left untested. So the stream is opened on a real endpoint and
    /// `code` is returned in place of the `HRESULT`
    /// `IAudioCaptureClient::GetNextPacketSize` would have returned
    /// ([`Stream::next_packet_size`]). What runs on it from there is the whole
    /// of the real path: [`Stream::lost`] deciding what the `HRESULT` means,
    /// the stream being torn down, the endpoint being tried again, the backing
    /// off when it fails at once, and the gap becoming silence.
    ///
    /// What is *not* covered is Windows returning that `HRESULT` in the first
    /// place, which needs a hand on a cable; see
    /// [issue #141](https://github.com/wildware-uk/clipped/issues/141).
    #[cfg(test)]
    pub(super) fn fail_every_endpoint_call_with(&mut self, code: windows::core::HRESULT) {
        self.injected_failure = Some(code);
    }
}

impl Drop for EndpointCapture {
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
    use super::*;

    fn at(nanos: u64) -> AudioTimestamp {
        AudioTimestamp::from_nanos(nanos)
    }

    #[test]
    fn a_position_a_device_period_from_the_counter_is_a_counter_reading() {
        // The ordinary case, and the reason the test is not an equality: a
        // loopback packet's position is about one device period *ahead* of the
        // moment it is read, because it is when the endpoint will play the
        // audio rather than when it was captured.
        let now = 31_107_000_000_000_000;
        assert!(position_is_on_the_counter(at(now + 10_000_000), at(now)));
        assert!(position_is_on_the_counter(at(now - 10_000_000), at(now)));
        assert!(position_is_on_the_counter(at(now), at(now)));
    }

    #[test]
    fn a_stream_that_reports_no_position_at_all_is_not_believed() {
        // The failure this exists for. A client that leaves the field alone
        // reports zero, and a timeline anchored on zero treats every later
        // packet as audio from hours before the recording started and discards
        // the lot — a silent track, with nothing in the log to say why. The
        // same is true of a client that reports frames since the stream
        // started, which on a machine that has been up for a day is nine
        // orders of magnitude from the counter.
        let now = at(31_107_000_000_000_000);
        assert!(!position_is_on_the_counter(at(0), now));
        assert!(!position_is_on_the_counter(at(1_000_000_000), now));
        assert!(!position_is_on_the_counter(
            at(now.as_nanos() + POSITION_SANITY),
            now
        ));
        assert!(
            position_is_on_the_counter(at(now.as_nanos() + POSITION_SANITY - 1), now),
            "the boundary itself is inside, so a busy machine's late read is still believed"
        );
    }
}

/// What both public captures' tests need in order to say anything about a real
/// endpoint.
///
/// Shared for the same reason the capture itself is: the properties a
/// microphone stream has to have are the properties a loopback stream has to
/// have, and two copies of this would drift.
#[cfg(test)]
pub(super) mod testing {
    use std::io::Write as _;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::{AudioFormat, AudioTimestamp, CapturedAudio};

    /// The environment variable that turns "this machine has no audio device"
    /// from a skip into a failure.
    ///
    /// Set it on any machine that is supposed to be able to record audio. It is
    /// deliberately not set in the pull-request CI job, because a GitHub
    /// Windows runner has no audio endpoint at all and a test that cannot run
    /// there must say so rather than pretend.
    const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

    /// Reports that a test could not run here, and returns whether the caller
    /// should return early.
    ///
    /// Written through `std::io::stderr()` rather than with `eprintln!`
    /// because libtest captures the macros: a skip printed with `eprintln!` is
    /// invisible in a passing run, which is exactly the failure mode — a
    /// regression that turns this test into a no-op looks like a test that
    /// passed.
    pub(in crate::windows) fn skipped(reason: &str) -> bool {
        if is_set(REQUIRE_AUDIO) {
            panic!("{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}");
        }
        let _ = writeln!(std::io::stderr(), "SKIPPED (audio): {reason}");
        true
    }

    /// The environment variable that asks the tests which touch an audio device
    /// not to.
    ///
    /// Distinct from [`REQUIRE_AUDIO`], and not its opposite: that one is about
    /// whether a machine *can* run these tests, this one is about whether it
    /// should right now. `cargo test --workspace` is the command CONTRIBUTING.md
    /// asks every contributor to run before review, and on a machine with a
    /// sound card it plays audible tones — which is unwelcome on a call, in
    /// headphones, or simply on the tenth run of the afternoon
    /// ([issue #206](https://github.com/wildware-uk/clipped/issues/206)).
    const SKIP_AUDIO: &str = "CLIPPED_SKIP_AUDIO";

    /// Whether an environment variable is set to anything but the empty string.
    fn is_set(name: &str) -> bool {
        std::env::var_os(name).is_some_and(|value| !value.is_empty())
    }

    /// Whether the caller should skip because the machine has been asked for
    /// quiet.
    ///
    /// Consulted *before* a device is opened, which is the difference between
    /// this and [`skipped`]: by the time a test has discovered it cannot run, it
    /// has already done whatever it was going to do to the endpoint.
    ///
    /// # Panics
    ///
    /// When [`SKIP_AUDIO`] and [`REQUIRE_AUDIO`] are both set. One says these
    /// tests must not run and the other says they must not be skipped; letting
    /// either win silently would mean a machine configured to prove it can
    /// record audio quietly proving nothing.
    pub(in crate::windows) fn suppressed() -> bool {
        if !is_set(SKIP_AUDIO) {
            return false;
        }
        assert!(
            !is_set(REQUIRE_AUDIO),
            "{SKIP_AUDIO} and {REQUIRE_AUDIO} are both set. One says these \
             tests must not run and the other says they must not be skipped; \
             there is no behaviour that satisfies both, so neither is being \
             guessed at."
        );
        skipped(&format!("{SKIP_AUDIO} is set"));
        true
    }

    /// Collects what a subscriber writes, so a test can assert on it.
    #[derive(Clone, Default)]
    struct CapturedLog(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLog {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("the buffer is not poisoned")
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for CapturedLog {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    /// Runs `body` with a subscriber local to this thread, and returns
    /// everything this crate logged while it ran.
    ///
    /// The assertion is then on the log line that was rendered rather than on
    /// the intention behind it, which matters for the one decision this crate
    /// makes whose only outcome *is* a log line: whether a failed WASAPI call
    /// is the ordinary end of a stream or a fault nobody expected
    /// (`Stream::lost`).
    ///
    /// Thread-local rather than global, so tests running beside this one in the
    /// same process neither see this subscriber nor write into its buffer.
    pub(in crate::windows) fn logged(body: impl FnOnce()) -> String {
        let captured = CapturedLog::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();

        tracing::subscriber::with_default(subscriber, body);

        let written = captured
            .0
            .lock()
            .expect("the buffer is not poisoned")
            .clone();
        String::from_utf8(written).expect("the subscriber writes UTF-8")
    }

    /// Asserts that every buffer starts exactly where all the buffers before it
    /// ended.
    ///
    /// Measured from the first timestamp plus the running frame count, which is
    /// how `Timeline` computes them. Adding each buffer's own duration to the
    /// previous buffer's timestamp instead would floor the nanosecond
    /// conversion twice, and two floors disagree with one by a nanosecond
    /// whenever a buffer's length is not a whole number of nanoseconds — which
    /// a silence instalment reconciled against the device's position often is
    /// not. That is arithmetic in the test rather than drift in the capture,
    /// and this way of asking is both exact and stricter: it fails on
    /// cumulative drift as well as on a single bad buffer.
    #[derive(Debug)]
    pub(in crate::windows) struct Contiguity {
        format: AudioFormat,
        anchor: Option<AudioTimestamp>,
        pub(in crate::windows) frames: u64,
    }

    impl Contiguity {
        pub(in crate::windows) fn new(format: AudioFormat) -> Self {
            Self {
                format,
                anchor: None,
                frames: 0,
            }
        }

        pub(in crate::windows) fn accept(&mut self, samples: &CapturedAudio<'_>) {
            let anchor = *self.anchor.get_or_insert(samples.timestamp());
            let expected = anchor.as_nanos() + self.format.frames_to_nanos(self.frames);
            let actual = samples.timestamp().as_nanos();

            // One nanosecond of slack, and exactly one: it is the truncation
            // this arithmetic cannot avoid, not a tolerance for drift.
            //
            // The capture stamps every buffer `timeline_anchor +
            // frames_to_nanos(frames_since_the_timeline_started)`. This counts
            // from the first buffer *it* saw, which is not always the first the
            // timeline emitted -- so where the capture computes
            // `f2n(before + since)`, this computes `f2n(before) + f2n(since)`,
            // and integer division does not distribute over addition. At
            // 48 kHz, `f2n(1) + f2n(2)` is 62,499 where `f2n(3)` is 62,500.
            //
            // Each truncation loses less than a nanosecond and there are two of
            // them against one, so the difference is at most one -- which is why
            // this is `<= 1` rather than a figure somebody chose. A capture that
            // really lost or repeated audio is out by a frame, 20,833 ns at this
            // rate, and still fails
            // ([issue #424](https://github.com/wildware-uk/clipped/issues/424)).
            let out_by = expected.abs_diff(actual);
            assert!(
                out_by <= 1,
                "buffers must be contiguous: expected {expected} ns, got {actual} ns, which is \
                 {out_by} ns out — more than the one nanosecond this arithmetic truncates by, \
                 so audio was lost or repeated"
            );
            self.frames += samples.frames() as u64;
        }

        /// How many seconds of audio have been accepted.
        pub(in crate::windows) fn seconds(&self) -> f64 {
            self.frames as f64 / f64::from(self.format.sample_rate().get())
        }
    }
}
