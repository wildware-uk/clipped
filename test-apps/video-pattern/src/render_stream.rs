//! The default output endpoint, opened for shared-mode rendering.
//!
//! # Why this is a module rather than two loops
//!
//! Two things in this package feed the same endpoint and neither of them cares
//! how it is opened: [`crate::tone`], which writes the subject's tone at a
//! moment `IAudioClock` names, and the drift measurement in
//! `tests/capture/av_sync.rs`, which writes no sound at all and holds a stream
//! open only so that the endpoint's clock keeps running — WASAPI loopback
//! delivers nothing while an endpoint is idle, and a period the device never
//! described has no position to measure against. The enumeration, the mix
//! format, the shared-mode initialisation and the padding-driven feeding loop
//! are the same for both, so they are written once (AGENTS.md section 55).
//!
//! There is a third copy of the same machinery in
//! `crates/audio/tests/system_audio.rs`, and this module deliberately does not
//! try to be its home: `clipped-audio` is layer 1 of the dependency table in
//! `README.md` and this package is layer 5, so a dependency from that test onto
//! this one is the cycle `tests/integration/tests/workspace_layering.rs`
//! forbids. Folding those two together needs somewhere below both, which is
//! [issue #190](https://github.com/wildware-uk/clipped/issues/190).
//!
//! # Ownership and threading
//!
//! A [`RenderStream`] owns its `IAudioClient` and `IAudioRenderClient` and
//! stops the client when it is dropped. It is opened, fed and dropped on one
//! thread — the caller's — and nothing here is shared between threads.
//!
//! # What it will not do
//!
//! Play a format it cannot write. [`Samples::Float32`] refuses an endpoint
//! whose mix format is anything else, because writing the wrong format to an
//! endpoint is full-scale noise on somebody's speakers rather than a quiet
//! tone. A caller that only ever releases silent buffers has nothing to get
//! wrong and asks for [`Samples::Silence`].

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioClock, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoIncrementMTAUsage, CoTaskMemFree, CLSCTX_ALL,
};

/// The buffer asked of the audio engine, in 100-nanosecond units: 200 ms.
///
/// Large enough that a late wake-up on either caller's feeding loop has
/// somewhere to catch up into rather than an underrun, and small enough that
/// nothing waits on it: how much audio is actually kept queued is the caller's
/// decision, made against `IAudioClient::GetCurrentPadding` on every pass.
const BUFFER_HUNDRED_NANOS: i64 = 2_000_000;

/// What a caller intends to write to the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Samples {
    /// Real samples, as 32-bit floats. An endpoint that presents anything else
    /// is refused rather than written to.
    Float32,
    /// Nothing: every buffer is released as silence, so the format the endpoint
    /// presents never has to be understood.
    Silence,
}

/// A shared-mode render stream on the default output device.
#[derive(Debug)]
pub struct RenderStream {
    client: IAudioClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
    rate: u32,
    channels: u16,
    float32: bool,
}

impl RenderStream {
    /// Opens the default output device and starts it.
    ///
    /// The thread this is called on joins the multi-threaded apartment first,
    /// because the interfaces below are used from it.
    ///
    /// # Errors
    ///
    /// Why this machine cannot play through its default endpoint, as a sentence
    /// for a warning: no output device, an endpoint that refuses a shared-mode
    /// stream, or — for [`Samples::Float32`] — one whose mix format this cannot
    /// write. Every one of those is a legitimate outcome a caller reports and
    /// carries on from (AGENTS.md section 16).
    pub fn open(samples: Samples) -> Result<Self, String> {
        // SAFETY: `CoIncrementMTAUsage` takes a process-wide reference to the
        // multi-threaded apartment. The reference is deliberately never given
        // back, which is what makes it safe to take from a thread that will
        // exit — the same reasoning as `crates/audio/src/windows/apartment.rs`.
        unsafe { CoIncrementMTAUsage() }.map_err(|error| format!("COM is unavailable: {error}"))?;

        let opened = (|| -> windows::core::Result<Option<Self>> {
            // SAFETY: `MMDeviceEnumerator` is the class identifier for
            // `IMMDeviceEnumerator`, which is the interface the return type asks
            // for.
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
            // SAFETY: both arguments are values of the enumerations named, and
            // the enumerator is live.
            let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }?;
            // SAFETY: `device` is live; the interface is fixed by the return
            // type.
            let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
            // SAFETY: `client` is live and uninitialised, which is when
            // `GetMixFormat` is valid. The `WAVEFORMATEX` it returns is a
            // `CoTaskMemAlloc` allocation this function now owns, and every path
            // below gives it back before returning (AGENTS.md section 58).
            let mix = unsafe { client.GetMixFormat() }?;
            // SAFETY: `WAVEFORMATEX` is `#[repr(packed)]`, so its fields are
            // read by copying the whole structure rather than by reference.
            let header = unsafe { mix.read_unaligned() };
            let float32 = is_float32(mix);
            let usable = float32 || samples == Samples::Silence;

            let initialised = if usable {
                // SAFETY: `mix` is the format Windows just reported for this
                // endpoint, so it is a format the endpoint accepts, and shared
                // mode never takes the device away from anything else.
                // `Initialize` copies what it needs out of the format.
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

            if !usable {
                return Ok(None);
            }

            // SAFETY: `client` is initialised, which is when `GetService` is
            // valid.
            let render: IAudioRenderClient = unsafe { client.GetService() }?;
            // SAFETY: `client` is initialised.
            let buffer_frames = unsafe { client.GetBufferSize() }?;

            Ok(Some(Self {
                client,
                render,
                buffer_frames,
                rate: { header.nSamplesPerSec },
                channels: { header.nChannels },
                float32,
            }))
        })();

        let stream = match opened {
            Ok(Some(stream)) => stream,
            Ok(None) => {
                return Err(
                    "the default output device does not present 32-bit float samples, \
                            which is all this can play"
                        .to_owned(),
                )
            }
            Err(error) => {
                return Err(format!(
                    "the default output device would not accept a render stream: {error}"
                ))
            }
        };

        // SAFETY: `client` is initialised and has not been started.
        unsafe { stream.client.Start() }
            .map_err(|error| format!("the render stream would not start: {error}"))?;
        Ok(stream)
    }

    /// Frames a second, as the endpoint presents them.
    #[must_use]
    pub const fn rate(&self) -> u32 {
        self.rate
    }

    /// Channels a frame, as the endpoint presents them.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// How many frames the endpoint's buffer holds in total.
    #[must_use]
    pub const fn buffer_frames(&self) -> u32 {
        self.buffer_frames
    }

    /// The endpoint's clock, and the units of position it counts in a second.
    ///
    /// This is what ties a stream position to the performance counter, which is
    /// what placing a sample at a *moment* needs.
    ///
    /// # Errors
    ///
    /// The endpoint would not give up its clock, or reports a frequency of zero,
    /// which is a clock nothing can convert a position with.
    pub fn clock(&self) -> Result<(IAudioClock, u64), String> {
        // SAFETY: `client` is initialised, which is when `GetService` is valid.
        let clock: IAudioClock = unsafe { self.client.GetService() }
            .map_err(|error| format!("the endpoint has no readable clock: {error}"))?;
        // SAFETY: `clock` is live and `GetFrequency` takes no arguments.
        let frequency = unsafe { clock.GetFrequency() }
            .map_err(|error| format!("the endpoint's clock has no frequency: {error}"))?;
        if frequency == 0 {
            return Err("the endpoint's clock reports a frequency of zero".to_owned());
        }
        Ok((clock, frequency))
    }

    /// How many frames the endpoint has not played yet.
    ///
    /// # Errors
    ///
    /// The endpoint stopped answering, which is what a device disappearing
    /// mid-run looks like from here.
    pub fn queued_frames(&self) -> Result<u32, String> {
        // SAFETY: `client` is a started `IAudioClient`.
        unsafe { self.client.GetCurrentPadding() }
            .map_err(|error| format!("the endpoint stopped reporting its padding: {error}"))
    }

    /// Releases `frames` of silence.
    ///
    /// Nothing is written: the buffer is released with
    /// `AUDCLNT_BUFFERFLAGS_SILENT`, which tells the audio engine to treat the
    /// frames as zero without reading them. There is no attenuated signal and
    /// no chance of a noise escaping onto somebody's speakers.
    ///
    /// # Errors
    ///
    /// The endpoint refused the buffer, which ends a feeding loop.
    pub fn write_silence(&self, frames: u32) -> Result<(), String> {
        // SAFETY: `frames` is the caller's, and must be at most the free space
        // `queued_frames` reported, which is what `GetBuffer` requires. The
        // returned pointer is deliberately not written to: the silent flag
        // below is what makes leaving it unwritten correct.
        let _buffer = unsafe { self.render.GetBuffer(frames) }
            .map_err(|error| format!("the endpoint would not lend a buffer: {error}"))?;
        // SAFETY: `frames` is the count `GetBuffer` was asked for.
        unsafe {
            self.render
                .ReleaseBuffer(frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
        }
        .map_err(|error| format!("the endpoint would not take its buffer back: {error}"))
    }

    /// Writes `frames` of samples through `fill` and releases them.
    ///
    /// `fill` is handed exactly `frames * channels` floats, interleaved, and
    /// has to write all of them: nothing is zeroed first, because the buffer
    /// the engine lends holds whatever was in it last.
    ///
    /// # Errors
    ///
    /// The stream was opened as [`Samples::Silence`], so what the endpoint
    /// presents was never checked and writing floats to it could be noise; or
    /// the endpoint refused the buffer.
    pub fn write(&self, frames: u32, fill: impl FnOnce(&mut [f32])) -> Result<(), String> {
        if !self.float32 {
            return Err(
                "this stream was opened for silence, so its sample format is unknown".to_owned(),
            );
        }
        // SAFETY: `frames` is the caller's, and must be at most the free space
        // `queued_frames` reported, which is what `GetBuffer` requires. The
        // pointer it returns is valid for `frames * channels` floats until
        // `ReleaseBuffer`, which is called below before anything else happens.
        let buffer = unsafe { self.render.GetBuffer(frames) }
            .map_err(|error| format!("the endpoint would not lend a buffer: {error}"))?;
        // SAFETY: as above, and the endpoint was checked to present 32-bit
        // float samples before the stream was started, so the region is
        // `frames * channels` `f32`s.
        let samples = unsafe {
            core::slice::from_raw_parts_mut(
                buffer.cast::<f32>(),
                frames as usize * self.channels as usize,
            )
        };
        fill(samples);
        // SAFETY: `frames` is the count `GetBuffer` was asked for and the
        // buffer has been written in full, so no silence flag is passed.
        unsafe { self.render.ReleaseBuffer(frames, 0) }
            .map_err(|error| format!("the endpoint would not take its buffer back: {error}"))
    }
}

impl Drop for RenderStream {
    fn drop(&mut self) {
        // SAFETY: `client` was started by `open`, and stopping it is what
        // releases the endpoint. A failure here means the endpoint has already
        // gone, which is nothing this can act on.
        let _ = unsafe { self.client.Stop() };
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
