//! The AMF backend: encoding a Direct3D 11 texture on AMD hardware.
//!
//! # How this reaches AMF, and why
//!
//! Through AMD's own interface, the Advanced Media Framework, loaded from the
//! driver at run time. The same two alternatives the NVENC backend rejected
//! (`super::nvenc`) were rejected again for the same reasons: Media
//! Foundation's AMD transforms take NV12, so every captured BGRA frame would
//! need a colour conversion pass before it reached the encoder, and the binding
//! crates on crates.io do not cover AMF's DirectX path.
//!
//! There is no SDK to install and nothing to link: the interface is a set of
//! headers, and the implementation is `amfrt64.dll`, which the display driver
//! installs into System32. `sys.rs` holds bindings generated from those
//! headers — AMD publishes them under the MIT licence, which is what makes them
//! redistributable at all (AGENTS.md sections 11 and 12).
//!
//! # How it differs from NVENC, and where it does not
//!
//! The shape is NVENC's, because [`VideoEncoder`] decides the shape: a session
//! is opened against the caller's Direct3D 11 device, one `submit` does the
//! whole of a frame, packets borrow the encoder's own memory, and everything is
//! released in `Drop` as well as in `shut_down`. Three things genuinely differ,
//! and each is a consequence of AMF's design rather than a preference:
//!
//! - **There is no configuration structure.** An AMF encoder is configured by
//!   setting named properties one at a time, so `settings.rs` produces a list of
//!   them rather than filling in a structure.
//! - **The encoder is asynchronous by design.** `SubmitInput` queues a frame and
//!   `QueryOutput` collects the result, where NVENC's lock is what waits. This
//!   backend still waits inside `submit`, for the reason below.
//! - **The input surface is reference counted.** AMF wraps the caller's texture
//!   in an `AMFSurface` and takes its own reference to that wrapper while the
//!   frame is in the encoder, which — unlike NVENC's registration — gives an
//!   observable answer to "has the hardware finished with the caller's texture?"
//!
//! # Ownership
//!
//! [`AmfEncoder`] owns the loaded runtime, the AMF context, the encoder
//! component and every output buffer it is still holding. All of it is released
//! by [`Session::drop`], which runs from `shut_down` and from `Drop`, so an
//! unwind on the encoding thread cannot leak a hardware encoder session
//! (AGENTS.md section 58).
//!
//! It owns no texture. The `AMFSurface` wrapping a submitted texture lives
//! inside a single [`Session::submit`] call: wrap, submit, wait for the coded
//! picture, then wait for AMF to drop its own reference to the wrapper and
//! release ours. Only when the wrapper is gone can nothing in AMF still read
//! the pixels, so a capture backend may recycle the frame the moment `submit`
//! returns, which is what [`crate::frame::SourceTexture`] promises it may.

#[allow(
    dead_code,
    unused_imports,
    missing_docs,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unreachable_pub,
    missing_debug_implementations,
    clippy::all,
    // The generated file is not code a contributor writes, which is what the
    // workspace lint is about (AGENTS.md section 42); every `unsafe` block in
    // `api.rs`, `mod.rs` and `settings.rs` still has to justify itself.
    clippy::undocumented_unsafe_blocks
)]
mod sys;

mod api;
mod settings;

// Visible to the rest of `windows` for one item: the mutex that serialises
// every test which takes an AMF encoder component (`tests::SESSIONS`).
#[cfg(test)]
pub(in crate::windows) mod tests;

use core::fmt;
use core::ptr;
use core::time::Duration;
use std::collections::VecDeque;
use std::time::Instant;

use crate::backend::VideoEncoder;
use crate::codec::{Codec, EncoderKind, Resolution, Vendor};
use crate::config::EncoderConfig;
use crate::error::{EncodeContext, EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SurfaceKind};
use crate::packet::{EncodedPacket, PictureKind};
use crate::probe::EncoderLimits;

/// The kinds of graphics resource this backend can bind.
const SUPPORTED_SURFACES: &[SurfaceKind] = &[SurfaceKind::D3d11Texture2D];

/// How many coded pictures the session will hold before the caller has to drain
/// them.
///
/// One is enough for the configuration this backend uses — no B-frames, so
/// every submission produces exactly one packet — and the surplus is there so
/// that a future configuration which reorders pictures does not have to change
/// the interface. The same number NVENC's pool holds, so that a caller sees one
/// answer to "how far behind may I fall" whichever encoder it opened.
const OUTPUT_CAPACITY: usize = 8;

/// How long a single frame may take before the hardware is treated as having
/// stopped answering.
///
/// Absurdly long for an encode that normally takes single-digit milliseconds,
/// and that is the point: this is not a performance bound, it is what stops a
/// wedged driver from hanging the encoding thread for ever (AGENTS.md section
/// 16).
const FRAME_DEADLINE: Duration = Duration::from_secs(5);

/// How long AMF may keep hold of a submitted texture after its coded picture
/// has been collected.
const RELEASE_DEADLINE: Duration = Duration::from_secs(2);

/// How long to wait between polls when AMF has nothing to say yet.
///
/// `QueryOutput` blocks inside the driver for the timeout `settings.rs`
/// configures, so this is only reached on a driver that does not honour it —
/// where it is the difference between a poll loop and a spin that takes CPU
/// from the game (AGENTS.md section 18).
const POLL_INTERVAL: Duration = Duration::from_micros(200);

/// One second in the hundred-nanosecond units AMF measures time in.
const AMF_SECOND: i64 = 10_000_000;

/// How many luma samples a macroblock is.
///
/// AMF quotes `MaxThroughput` in macroblocks a second — 16x16 samples — which
/// is the unit the codec levels are quoted in too (`crate::reference`), and
/// that is what makes a measured ceiling comparable with the published one.
const LUMA_SAMPLES_PER_MACROBLOCK: u64 = 16 * 16;

/// A hardware encoding session on an AMD GPU.
///
/// Opened with [`open`](Self::open) against the Direct3D 11 device whose
/// textures will be submitted, and used through the
/// [`VideoEncoder`](crate::VideoEncoder) trait — the same interface, and the
/// same lifecycle, as [`NvencEncoder`](crate::NvencEncoder).
///
/// ```no_run
/// use core::time::Duration;
/// use clipped_encoder::{
///     AmfEncoder, BitRate, Codec, EncoderConfig, FrameRate, GraphicsDevice, RateControl,
///     Resolution, SourceFrame, SourceTexture, SurfaceFormat, VideoEncoder,
/// };
/// # fn example(device: GraphicsDevice, texture: *mut core::ffi::c_void)
/// # -> Result<(), clipped_encoder::EncodeError> {
/// let config = EncoderConfig::new(
///     Codec::Hevc,
///     Resolution::new(2560, 1440),
///     FrameRate::FPS_60,
///     RateControl::constant(BitRate::megabits_per_second(40)),
/// );
/// let mut encoder = AmfEncoder::open(&device, config)?;
///
/// // SAFETY: in a real pipeline this handle comes from a captured frame that
/// // is still alive; see `clipped_encoder::frame`.
/// let surface = unsafe {
///     SourceTexture::new(clipped_encoder::SurfaceKind::D3d11Texture2D, texture)
/// };
/// let frame = SourceFrame::new(
///     surface,
///     SurfaceFormat::Bgra8Unorm,
///     Resolution::new(2560, 1440),
///     Duration::ZERO,
/// );
///
/// encoder.submit(&frame)?;
/// while let Some(packet) = encoder.next_packet()? {
///     let _ = packet.data();
/// }
/// # Ok(())
/// # }
/// ```
pub struct AmfEncoder {
    /// `None` once [`shut_down`](VideoEncoder::shut_down) has run, which is
    /// what makes shutdown idempotent and every later call a clean
    /// [`EncodeErrorKind::NotRunning`] rather than a use-after-free.
    session: Option<Session>,
    config: EncoderConfig,
    context: EncodeContext,
    parameter_sets: Vec<u8>,
    last_presentation: Option<Duration>,
    finished: bool,
}

// SAFETY: `VideoEncoder` is `Send` so that a session can be opened on one
// thread and moved to the encoding thread, and this type has to satisfy it.
//
// What crosses the boundary is a set of raw pointers: the AMF context, the
// encoder component, any output buffers still held, and the module handle. None
// of them is thread-bound. AMD's own API reference says so in as many words —
// "AMF components are thread-safe, submission of input samples and retrieval of
// output samples can be done either from a single thread, or multiple threads"
// — the Direct3D 11 device the context was initialised with is free-threaded,
// and a module handle belongs to the process.
//
// What is *not* claimed is that two threads may use one encoder at once. They
// may not, and nothing here makes that possible: `VideoEncoder` is not `Sync`,
// every method takes `&mut self`, and an `EncodedPacket` borrows the encoder it
// came from.
unsafe impl Send for AmfEncoder {}

impl AmfEncoder {
    /// Opens a session on `device`.
    ///
    /// Everything that can be checked is checked here rather than on the first
    /// frame: that the runtime loads, that it is new enough, that this build
    /// implements this codec, that the hardware has an encoder for it, and that
    /// the resolution is within what that encoder reports. A recording that
    /// cannot work should fail before the user believes it started (SPEC.md
    /// section 7).
    ///
    /// # Errors
    ///
    /// [`EncodeError`], naming the encoder, codec and resolution, for every one
    /// of those checks and for any failure to build the session.
    pub fn open(device: &GraphicsDevice, config: EncoderConfig) -> Result<Self, EncodeError> {
        let context = EncodeContext::new(EncoderKind::Amf, config.codec(), config.resolution());
        let fail = |kind| EncodeError::new(context, kind);

        if device.kind() != DeviceKind::D3d11 {
            return Err(fail(EncodeErrorKind::Configuration {
                detail: format!(
                    "AMF binds Direct3D 11 textures and was given a {} device",
                    device.kind()
                ),
            }));
        }
        if device.as_raw().is_null() {
            return Err(fail(EncodeErrorKind::Configuration {
                detail: "the graphics device is null".to_owned(),
            }));
        }
        validate(&config).map_err(&fail)?;

        if settings::surface_format(config.source_format()).is_none() {
            return Err(fail(EncodeErrorKind::SurfaceFormatUnsupported {
                offered: config.source_format(),
                supported: settings::SUPPORTED_FORMATS,
            }));
        }

        let Some(component_id) = settings::component_id(config.codec()) else {
            // Not `CodecUnsupported`: the hardware may well encode it, and
            // saying "this hardware does not encode that codec" about a missing
            // backend would send the reader to the wrong place (AGENTS.md
            // section 15).
            return Err(fail(EncodeErrorKind::Configuration {
                detail: format!(
                    "the AMF backend does not implement {} yet; see \
                     https://github.com/wildware-uk/clipped/issues/165",
                    config.codec()
                ),
            }));
        };

        // After every check that is about the *configuration* and before the
        // first that is about this machine, which is the order Quick Sync's
        // equivalent uses. A caller asking for something the backend cannot
        // encode gets that answer whatever GPU it holds, and this is the point
        // at which the device stops being a handle to carry and starts being
        // hardware to ask.
        //
        // An AMF context initialises against the caller's Direct3D 11 device,
        // and a device created on somebody else's adapter is refused by
        // `InitDX11` with `AMF_INVALID_ARG` — a status code that says nothing
        // about adapters and sends the reader looking for a driver fault
        // (issue #443). All three hardware backends refuse it by name through
        // this one function rather than three of their own (AGENTS.md section
        // 55).
        //
        // SAFETY: `open` has already checked the handle is non-null, and its
        // caller guarantees it is a live `ID3D11Device` it owns.
        unsafe { crate::windows::dxgi::require_adapter_vendor(device, Vendor::Amd, "AMF", &fail) }?;

        let runtime = api::AmfRuntime::load().map_err(|failure| {
            fail(match failure {
                api::LoadFailure::TooOld { installed } => EncodeErrorKind::DriverTooOld {
                    required: (api::VERSION.0, api::VERSION.1),
                    installed: (installed.0, installed.1),
                    minimum_driver: api::MINIMUM_DRIVER,
                },
                other => EncodeErrorKind::RuntimeUnavailable {
                    library: api::LIBRARY,
                    detail: other.to_string(),
                },
            })
        })?;

        let installed = runtime.installed_version();
        let mut session = Session::open(runtime, device.as_raw(), context, config.codec())?;
        session.create_component(component_id)?;
        session.check_size(config.resolution())?;
        session.configure(&config)?;
        session.initialise(&config)?;
        let parameter_sets = session.parameter_sets()?;

        tracing::info!(
            encoder = %EncoderKind::Amf.log_encoder_family(),
            codec = config.codec().log_value(),
            resolution = %config.resolution(),
            frame_rate = %config.frame_rate(),
            rate_control = %config.rate_control(),
            preset = %config.preset(),
            runtime_version = %format_args!("{}.{}.{}", installed.0, installed.1, installed.2),
            parameter_set_bytes = parameter_sets.len(),
            "opened an AMF encoding session"
        );

        Ok(Self {
            session: Some(session),
            config,
            context,
            parameter_sets,
            last_presentation: None,
            finished: false,
        })
    }

    /// The live session, or [`EncodeErrorKind::NotRunning`].
    fn session(&mut self) -> Result<&mut Session, EncodeError> {
        let context = self.context;
        self.session
            .as_mut()
            .ok_or_else(|| EncodeError::new(context, EncodeErrorKind::NotRunning))
    }

    /// Builds an error carrying this session's context.
    const fn fail(&self, kind: EncodeErrorKind) -> EncodeError {
        EncodeError::new(self.context, kind)
    }
}

impl fmt::Debug for AmfEncoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AmfEncoder")
            .field("config", &self.config)
            .field("running", &self.session.is_some())
            .field("finished", &self.finished)
            .finish()
    }
}

impl Drop for AmfEncoder {
    fn drop(&mut self) {
        self.shut_down();
    }
}

impl VideoEncoder for AmfEncoder {
    fn encoder(&self) -> EncoderKind {
        EncoderKind::Amf
    }

    fn configuration(&self) -> &EncoderConfig {
        &self.config
    }

    fn parameter_sets(&self) -> &[u8] {
        &self.parameter_sets
    }

    fn submit(&mut self, frame: &SourceFrame<'_>) -> Result<(), EncodeError> {
        if self.finished {
            return Err(self.fail(EncodeErrorKind::Configuration {
                detail: "the stream has been finished and cannot take more frames".to_owned(),
            }));
        }
        if frame.texture().kind() != SurfaceKind::D3d11Texture2D {
            return Err(self.fail(EncodeErrorKind::SurfaceKindUnsupported {
                offered: frame.texture().kind(),
                supported: SUPPORTED_SURFACES,
            }));
        }
        if frame.format() != self.config.source_format() {
            return Err(self.fail(EncodeErrorKind::SurfaceFormatUnsupported {
                offered: frame.format(),
                supported: settings::SUPPORTED_FORMATS,
            }));
        }
        if frame.resolution() != self.config.resolution() {
            return Err(self.fail(EncodeErrorKind::Configuration {
                detail: format!(
                    "a {} frame arrived for a session configured for {}; reconfigure the encoder \
                     when the capture changes size",
                    frame.resolution(),
                    self.config.resolution()
                ),
            }));
        }
        if let Some(previous) = self.last_presentation {
            if frame.presentation_time() < previous {
                return Err(self.fail(EncodeErrorKind::TimestampWentBackwards {
                    previous_nanos: previous.as_nanos(),
                    submitted_nanos: frame.presentation_time().as_nanos(),
                }));
            }
        }

        let frame_duration = frame_duration(&self.config);
        self.session()?.submit(frame, frame_duration)?;
        self.last_presentation = Some(frame.presentation_time());
        Ok(())
    }

    fn next_packet(&mut self) -> Result<Option<EncodedPacket<'_>>, EncodeError> {
        self.session()?.next_packet()
    }

    fn finish(&mut self) -> Result<(), EncodeError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.session()?.finish()
    }

    fn shut_down(&mut self) {
        if let Some(session) = self.session.take() {
            let frames = session.encoded_frames;
            drop(session);
            tracing::info!(
                encoder = %EncoderKind::Amf.log_encoder_family(),
                codec = self.config.codec().log_value(),
                frames,
                "closed the AMF encoding session"
            );
        }
    }
}

/// Asks AMF what it can do, on the device it would encode on.
///
/// One AMF context, and one encoder component per codec, created and destroyed
/// inside this call. No component is ever initialised: `AMFComponent::GetCaps`
/// answers before `Init`, so nothing is configured, no surface is allocated and
/// no picture is coded. Creating the component is still what reserves the
/// hardware encoder, which is why nothing calls this unless a user asked (see
/// [`crate::probe::Probing`]).
///
/// A failure at any point is a measurement that did not happen, not an error:
/// the published limits stand, and the reason goes to the diagnostics
/// (AGENTS.md sections 15 and 16).
pub(in crate::windows) fn measure_limits(device: &GraphicsDevice) -> Vec<EncoderLimits> {
    // No picture is being encoded, so there is no resolution to name. Nothing
    // built from this context reaches a user: the failures below are logged as
    // diagnostics about a query, by kind, never shown as "could not encode".
    let context = EncodeContext::new(EncoderKind::Amf, Codec::H264, Resolution::new(0, 0));

    let runtime = match api::AmfRuntime::load() {
        Ok(runtime) => runtime,
        Err(failure) => {
            tracing::debug!(
                library = api::LIBRARY,
                %failure,
                "AMF could not be loaded, so its limits were not measured"
            );
            return Vec::new();
        }
    };

    let mut session = match Session::open(runtime, device.as_raw(), context, Codec::H264) {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(
                reason = %error.kind(),
                "an AMF context could not be created, so no limits were measured and the \
                 published ones stand"
            );
            return Vec::new();
        }
    };

    let measured = Codec::EFFICIENCY_ORDER
        .into_iter()
        .filter_map(|codec| Some((codec, settings::component_id(codec)?)))
        .map(|(codec, component)| session.measure(codec, component))
        .collect();

    // The context and the last component are released here, by
    // `Session::drop`, which gives the hardware encoder back (AGENTS.md section
    // 58).
    drop(session);
    measured
}

/// The configuration checks that do not need a session.
fn validate(config: &EncoderConfig) -> Result<(), EncodeErrorKind> {
    let Resolution { width, height } = config.resolution();

    if width == 0 || height == 0 {
        return Err(EncodeErrorKind::Configuration {
            detail: "a picture with no width or no height cannot be encoded".to_owned(),
        });
    }
    // Every codec here samples chroma at half resolution in both directions, so
    // an odd dimension has no representation. AMF would refuse it later, with a
    // status that does not say which parameter was wrong.
    if width % 2 != 0 || height % 2 != 0 {
        return Err(EncodeErrorKind::Configuration {
            detail: format!(
                "{width}x{height} has an odd dimension, and 4:2:0 chroma needs both to be even"
            ),
        });
    }
    Ok(())
}

/// How long one frame lasts, in the units AMF measures time in.
///
/// Handed to the encoder with each surface. AMF's rate control reads the frame
/// rate from the configuration rather than from this, so it is a description of
/// the picture rather than an instruction — but a surface with no duration is
/// one whose duration AMF has to guess.
fn frame_duration(config: &EncoderConfig) -> i64 {
    let rate = config.frame_rate();
    AMF_SECOND * i64::from(rate.denominator()) / i64::from(rate.numerator())
}

/// Whether the picture about to be submitted has to be coded as an IDR.
///
/// Two reasons, and the second is the one that is not obvious.
///
/// The caller may ask, which is what saving a replay-buffer clip does.
///
/// And the *first* picture of a stream always has to be one, whatever the
/// keyframe interval says, because a recording whose own beginning is not a cut
/// point is one no clip can start at and no muxer can seek in (SPEC.md section
/// 7). AMF does not guarantee that by itself: with HEVC and
/// [`KeyframeInterval::Never`](crate::KeyframeInterval::Never) the backend sets
/// `HevcGOPSPerIDR = 0`, which means AMF inserts no IDR *at all* — the stream
/// opens with a clean random access picture (`CRA_NUT`, NAL type 21), reported
/// through `HevcOutputDataType` as intra rather than as an IDR, so the first
/// packet of the recording is not flagged as a keyframe. Forcing the first
/// picture is what makes
/// [`KeyframeInterval::Never`](crate::KeyframeInterval::Never)'s promise —
/// "only the first frame is a keyframe" — true here.
///
/// H.264 does not need it: `IDRPeriod = 0` still emits an IDR for the first
/// picture. Asking anyway costs nothing, because that picture is an IDR either
/// way, and it keeps one rule rather than a per-codec exception.
const fn force_idr(caller_asked: bool, pictures_coded: u64) -> bool {
    caller_asked || pictures_coded == 0
}

/// A presentation time in the hundred-nanosecond units AMF measures time in.
///
/// **This is where a timestamp loses precision**, and it is the one place in
/// this backend that a frame's position in the recording is not carried through
/// exactly. AMF's `amf_pts` is a count of hundred-nanosecond ticks, so a
/// capture timestamp is rounded down to the tick below it and the packet comes
/// back with that value: a frame submitted at 116.666666 ms is reported at
/// 116.6666 ms.
///
/// The error is bounded by 100 ns per frame and does not accumulate, because
/// every timestamp is converted from the original rather than from the previous
/// one. A hundred nanoseconds is a ten-thousandth of a frame at 60 frames a
/// second, so nothing downstream — a container's time base, audio
/// synchronisation, a clip boundary — can tell (AGENTS.md section 22). It is
/// recorded here because a reader comparing what went in with what came out
/// will otherwise think something is wrong.
///
/// Saturating at the top: a `Duration` beyond twenty-nine thousand years cannot
/// happen in a recording, and if it somehow did, clamping keeps timestamps
/// monotonic where wrapping would send them backwards.
fn presentation_units(time: Duration) -> i64 {
    i64::try_from(time.as_nanos() / 100).unwrap_or(i64::MAX)
}

/// A coded picture, in the AMF buffer that holds it.
///
/// The buffer is a reference this session owns, and the bytes stay readable
/// until [`Session::release_coded`] gives that reference back — which is the
/// next `next_packet`, or teardown.
///
/// Deliberately neither [`Clone`] nor [`Copy`]: it owns a reference to an
/// `AMFBuffer`, and a second value carrying the same pointer is a second thing
/// that looks releasable (AGENTS.md section 58). It is moved between the queues
/// instead, so exactly one value owns the reference at any moment.
#[derive(Debug)]
struct Coded {
    buffer: *mut sys::AMFBuffer,
    /// Where AMF put the bytes. Valid while the reference above is held.
    data: *const u8,
    length: usize,
    presentation: Duration,
    picture: PictureKind,
}

/// The live AMF session and everything it owns.
struct Session {
    runtime: api::AmfRuntime,
    context: *mut sys::AMFContext,
    component: *mut sys::AMFComponent,
    codec: Codec,
    error_context: EncodeContext,
    /// Coded and waiting to be handed over.
    ready: VecDeque<Coded>,
    /// The packet the caller is currently holding, which the next call
    /// releases.
    locked: Option<Coded>,
    encoded_frames: u64,
}

impl Session {
    /// Creates the AMF context and binds it to the caller's Direct3D 11 device.
    fn open(
        runtime: api::AmfRuntime,
        device: *mut core::ffi::c_void,
        error_context: EncodeContext,
        codec: Codec,
    ) -> Result<Self, EncodeError> {
        let factory = runtime.factory();
        // SAFETY: the factory came from a successful `AMFInit` and is owned by
        // the library, so its vtable is valid for as long as the module is
        // loaded — which is at least as long as `runtime`, moved into the
        // session below.
        let Some(create) = (unsafe { (*(*factory).pVtbl).CreateContext }) else {
            return Err(missing(error_context, "AMFFactory::CreateContext"));
        };

        let mut context: *mut sys::AMFContext = ptr::null_mut();
        // SAFETY: the factory is live and `context` is a live local
        // out-parameter.
        let status = unsafe { create(factory, &raw mut context) };
        if status != sys::AMF_OK || context.is_null() {
            return Err(EncodeError::new(
                error_context,
                classify(status, "AMFFactory::CreateContext"),
            ));
        }

        // From here on the context is owned, so every failure has to release it.
        let session = Self {
            runtime,
            context,
            component: ptr::null_mut(),
            codec,
            error_context,
            ready: VecDeque::new(),
            locked: None,
            encoded_frames: 0,
        };

        // SAFETY: the context was just created, and `device` is the caller's
        // Direct3D 11 device, which `AmfEncoder::open` has checked is non-null
        // and which the caller guarantees outlives the encoder. AMF takes its
        // own reference to it and gives it back in `Terminate`.
        let Some(init) = (unsafe { (*(*context).pVtbl).InitDX11 }) else {
            return Err(missing(error_context, "AMFContext::InitDX11"));
        };
        // SAFETY: as above.
        let status = unsafe { init(context, device, sys::AMF_DX11_0) };
        if status != sys::AMF_OK {
            return Err(EncodeError::new(
                error_context,
                classify(status, "AMFContext::InitDX11"),
            ));
        }

        Ok(session)
    }

    /// Creates the encoder component for this codec.
    fn create_component(&mut self, id: &[u16]) -> Result<(), EncodeError> {
        let factory = self.runtime.factory();
        // SAFETY: the factory is live for as long as the runtime is.
        let Some(create) = (unsafe { (*(*factory).pVtbl).CreateComponent }) else {
            return Err(missing(self.error_context, "AMFFactory::CreateComponent"));
        };

        let mut component: *mut sys::AMFComponent = ptr::null_mut();
        // SAFETY: the factory and the context are live, `id` is a
        // nul-terminated UTF-16 name built at compile time, and `component` is a
        // live local out-parameter.
        let status = unsafe { create(factory, self.context, id.as_ptr(), &raw mut component) };
        if status != sys::AMF_OK || component.is_null() {
            let kind = match status {
                // No encoder for this codec on this adapter. The report
                // `recorder capabilities` prints says the same thing, and this
                // is that answer arriving through the encoder.
                sys::AMF_NOT_FOUND | sys::AMF_NOT_SUPPORTED | sys::AMF_ENCODER_NOT_PRESENT => {
                    EncodeErrorKind::CodecUnsupported
                }
                other => classify(other, "AMFFactory::CreateComponent"),
            };
            return Err(EncodeError::new(self.error_context, kind));
        }

        self.component = component;
        Ok(())
    }

    /// Terminates and releases the encoder component, if there is one.
    ///
    /// Idempotent, and the only place a component is released: it runs from
    /// `Drop` and from the capability probe, which creates one component per
    /// codec and must not leave the previous one holding the hardware encoder
    /// while it asks about the next (AGENTS.md section 58).
    fn release_component(&mut self) {
        if self.component.is_null() {
            return;
        }

        // SAFETY: the component was created by `create_component` and is
        // terminated and released exactly once, because the pointer is nulled
        // below and this is the only path that releases it.
        if let Some(terminate) = unsafe { (*(*self.component).pVtbl).Terminate } {
            // SAFETY: as above.
            let status = unsafe { terminate(self.component) };
            log_release_failure(status, "AMFComponent::Terminate");
        }
        // SAFETY: as above — this session's only reference, given back once.
        let _ = unsafe { api::release(self.component.cast::<sys::AMFInterface>()) };
        self.component = ptr::null_mut();
    }

    /// Refuses a picture larger than the encoder says it can take.
    ///
    /// Best effort, deliberately. A driver that will not describe its own
    /// limits is not a reason to refuse a recording that would have worked, so
    /// a failure to read the capabilities is logged and the size is left to
    /// `Init` to accept or refuse.
    fn check_size(&self, wanted: Resolution) -> Result<(), EncodeError> {
        let Some(maximum) = self.maximum_size() else {
            return Ok(());
        };
        if wanted.width > maximum.width || wanted.height > maximum.height {
            return Err(EncodeError::new(
                self.error_context,
                EncodeErrorKind::ResolutionUnsupported { maximum },
            ));
        }
        Ok(())
    }

    /// The largest picture the encoder reports it can take, if it will say.
    fn maximum_size(&self) -> Option<Resolution> {
        self.with_capabilities(|session, capabilities| session.maximum_input_size(capabilities))
            .flatten()
    }

    /// Runs `read` against the encoder's capabilities, if it will describe
    /// them.
    ///
    /// # Ownership
    ///
    /// The reference `GetCaps` returns belongs to this function and is released
    /// before it returns, so `read` may use the pointer and must not store it.
    fn with_capabilities<T>(&self, read: impl FnOnce(&Self, *mut sys::AMFCaps) -> T) -> Option<T> {
        // SAFETY: the component was created by `create_component` and has not
        // been terminated.
        let caps = unsafe { (*(*self.component).pVtbl).GetCaps }?;

        let mut capabilities: *mut sys::AMFCaps = ptr::null_mut();
        // SAFETY: the component is live and `capabilities` is a live local
        // out-parameter. On success it holds a reference, released below.
        let status = unsafe { caps(self.component, &raw mut capabilities) };
        if status != sys::AMF_OK || capabilities.is_null() {
            tracing::debug!(
                status,
                status_name = api::status_name(status),
                "the AMF encoder would not describe its capabilities, so the published limits \
                 stand and the picture size is left to Init to accept or refuse"
            );
            return None;
        }

        let read = read(self, capabilities);

        // SAFETY: the reference came from the successful `GetCaps` above and is
        // given back exactly once, here.
        let _ = unsafe { api::release(capabilities.cast::<sys::AMFInterface>()) };
        Some(read)
    }

    /// Everything this encoder's capabilities say about one codec.
    ///
    /// Creates the component, reads it, and destroys it again, so that the
    /// three codecs are not three simultaneous encoders. A component that
    /// cannot be created because the hardware has no encoder for the codec is a
    /// measured `false` rather than an absence of an answer: AMF refusing to
    /// create its own encoder component is the encoder Clipped would record
    /// through saying it cannot produce this codec.
    fn measure(&mut self, codec: Codec, component: &[u16]) -> EncoderLimits {
        let measured = EncoderLimits::new(EncoderKind::Amf, codec);

        if let Err(error) = self.create_component(component) {
            let missing = matches!(error.kind(), EncodeErrorKind::CodecUnsupported);
            tracing::debug!(
                codec = codec.log_value(),
                reason = %error.kind(),
                "an AMF encoder component could not be created"
            );
            // Only "there is no such encoder" is an answer. Any other failure —
            // out of memory, a device that went away — says nothing about what
            // the hardware supports, and reporting `false` from one would be a
            // measurement of the wrong thing.
            return if missing {
                measured.with_supported(false)
            } else {
                measured
            };
        }

        let measured = measured.with_supported(true);
        let measured = self
            .with_capabilities(|session, capabilities| {
                session.read_capabilities(codec, capabilities, measured)
            })
            .unwrap_or(measured);

        self.release_component();
        measured
    }

    /// Reads one codec's capability properties out of a live `AMFCaps`.
    fn read_capabilities(
        &self,
        codec: Codec,
        capabilities: *mut sys::AMFCaps,
        measured: EncoderLimits,
    ) -> EncoderLimits {
        let measured = match self.maximum_input_size(capabilities) {
            Some(size) => measured.with_max_resolution(size),
            None => measured,
        };

        let Some(names) = settings::capability_names(codec) else {
            return measured;
        };
        let storage = capabilities.cast::<sys::AMFPropertyStorage>();

        // Zero is "did not say" rather than "cannot encode a single
        // macroblock": AMF reports it for a component that does not implement
        // the capability, and a ceiling of no frames a second would be a worse
        // answer than the published one.
        let measured = match self.capability_number(codec, storage, names.max_throughput) {
            Some(macroblocks) if macroblocks > 0 => measured.with_max_luma_samples_per_second(
                macroblocks.unsigned_abs() * LUMA_SAMPLES_PER_MACROBLOCK,
            ),
            _ => measured,
        };

        let measured = match names
            .b_frames
            .and_then(|name| self.capability_number(codec, storage, name))
        {
            // `AMF_VIDEO_ENCODER_CAP_BFRAMES` is declared a bool in
            // `VideoEncoderVCE.h` — "// bool  is B-Frames supported" — and
            // arrives in a variant this reads as a whole number, the way AMF
            // delivers every capability (see `api::as_whole_number`). So a
            // non-zero answer is "yes" and zero is a genuine "no", which is
            // what AMD's video engines without them report. It is not a count,
            // and nothing here may report one from it.
            Some(supported) => measured.with_b_frames(supported != 0),
            None => measured,
        };

        match self
            .capability_number(codec, storage, names.max_profile)
            .and_then(|profile| settings::ten_bit_from_profile(codec, profile))
        {
            Some(ten_bit) => measured.with_hdr(ten_bit),
            None => measured,
        }
    }

    /// One numeric capability property, where the encoder answers with one.
    ///
    /// AMF stores these as whole numbers, and a name it does not recognise is
    /// answered with `AMF_NOT_FOUND` rather than an error — so a property this
    /// runtime is too old to have leaves the published limit in place instead
    /// of failing the query.
    fn capability_number(
        &self,
        codec: Codec,
        storage: *mut sys::AMFPropertyStorage,
        name: &[u16],
    ) -> Option<i64> {
        // SAFETY: `storage` is the live `AMFCaps` the caller holds a reference
        // to, seen through the property storage its interface derives from (see
        // `api`), and `name` is nul-terminated.
        let value = unsafe { api::get_property(storage, name) };

        match value {
            Ok(variant) => {
                let number = api::as_whole_number(&variant);
                if number.is_none() {
                    tracing::debug!(
                        codec = codec.log_value(),
                        capability = %api::name_to_string(name),
                        variant = variant.type_,
                        "an AMF capability came back as something other than a number"
                    );
                }
                number
            }
            Err(status) => {
                tracing::debug!(
                    codec = codec.log_value(),
                    capability = %api::name_to_string(name),
                    status,
                    status_name = api::status_name(status),
                    "AMF would not answer a capability query, so the published limit stands"
                );
                None
            }
        }
    }

    /// The width and height ranges of the encoder's input, from its
    /// capabilities.
    ///
    /// The two are separate ranges — `AMFIOCaps::GetWidthRange` and
    /// `GetHeightRange` — and the [`Resolution`] built from their maxima is the
    /// corner of a rectangle rather than a size AMF said it would accept. On
    /// the integrated Radeon this was written against, that corner is
    /// 4096x4096 for H.264 where the published table says 4096x2160, and it is
    /// worth being plain about what that does and does not contradict: the
    /// encoder answers a wider *height* range than the documentation's example
    /// resolution, and nothing here has opened a session at 4096x4096 to find
    /// out whether a picture of that shape is encodable. It is an upper bound,
    /// which is all a resolution ceiling ever is here
    /// (`docs/encoder-capabilities.md`).
    fn maximum_input_size(&self, capabilities: *mut sys::AMFCaps) -> Option<Resolution> {
        // SAFETY: `capabilities` is the live object `maximum_size` is holding a
        // reference to for the whole of this call.
        let inputs = unsafe { (*(*capabilities).pVtbl).GetInputCaps }?;

        let mut io: *mut sys::AMFIOCaps = ptr::null_mut();
        // SAFETY: the capabilities are live and `io` is a live local
        // out-parameter. On success it holds a reference, released below.
        let status = unsafe { inputs(capabilities, &raw mut io) };
        if status != sys::AMF_OK || io.is_null() {
            return None;
        }

        // SAFETY: `io` is the live object just returned.
        let width = unsafe { (*(*io).pVtbl).GetWidthRange };
        // SAFETY: as above.
        let height = unsafe { (*(*io).pVtbl).GetHeightRange };

        let size = match (width, height) {
            (Some(width), Some(height)) => {
                let (mut min_width, mut max_width) = (0i32, 0i32);
                let (mut min_height, mut max_height) = (0i32, 0i32);
                // SAFETY: `io` is live and all four are live local
                // out-parameters.
                unsafe {
                    width(io, &raw mut min_width, &raw mut max_width);
                    height(io, &raw mut min_height, &raw mut max_height);
                }
                (max_width > 0 && max_height > 0)
                    .then(|| Resolution::new(max_width.unsigned_abs(), max_height.unsigned_abs()))
            }
            _ => None,
        };

        // SAFETY: the reference came from the successful `GetInputCaps` above
        // and is given back exactly once, here.
        let _ = unsafe { api::release(io.cast::<sys::AMFInterface>()) };
        size
    }

    /// Sets every property the configuration asks for.
    ///
    /// A property AMF does not recognise is reported and does not stop the
    /// session: an older runtime that has never heard of one of these encodes
    /// perfectly well with its own default, and refusing to record at all
    /// because of a knob would be the wrong trade (AGENTS.md section 16). A
    /// property it recognises and refuses is a real misconfiguration and fails
    /// the open.
    fn configure(&self, config: &EncoderConfig) -> Result<(), EncodeError> {
        for property in settings::properties(config) {
            // SAFETY: the component is live, and its vtable begins with
            // `AMFPropertyStorage`'s entries because `AMFComponent` derives from
            // it (see `api`). The name is nul-terminated.
            let status = unsafe {
                api::set_property(
                    self.component.cast::<sys::AMFPropertyStorage>(),
                    property.name,
                    property.value,
                )
            };
            match status {
                sys::AMF_OK => {}
                sys::AMF_NOT_FOUND => tracing::debug!(
                    property = %api::name_to_string(property.name),
                    "this AMF runtime has no such encoder property, so its default stands"
                ),
                other => {
                    return Err(EncodeError::new(
                        self.error_context,
                        EncodeErrorKind::Configuration {
                            detail: format!(
                                "the encoder refused {} with {} ({other})",
                                api::name_to_string(property.name),
                                api::status_name(other)
                            ),
                        },
                    ))
                }
            }
        }
        Ok(())
    }

    /// Starts the encoder.
    fn initialise(&self, config: &EncoderConfig) -> Result<(), EncodeError> {
        let format = settings::surface_format(config.source_format()).ok_or_else(|| {
            EncodeError::new(
                self.error_context,
                EncodeErrorKind::SurfaceFormatUnsupported {
                    offered: config.source_format(),
                    supported: settings::SUPPORTED_FORMATS,
                },
            )
        })?;
        let resolution = config.resolution();

        // SAFETY: the component is live and has not been initialised.
        let Some(init) = (unsafe { (*(*self.component).pVtbl).Init }) else {
            return Err(missing(self.error_context, "AMFComponent::Init"));
        };
        // SAFETY: as above; the format is one of AMF's own enumerators and the
        // dimensions have been checked to be positive and even.
        let status = unsafe {
            init(
                self.component,
                format,
                i32::try_from(resolution.width).unwrap_or(i32::MAX),
                i32::try_from(resolution.height).unwrap_or(i32::MAX),
            )
        };
        if status != sys::AMF_OK {
            let kind = match status {
                sys::AMF_SURFACE_FORMAT_NOT_SUPPORTED => {
                    EncodeErrorKind::SurfaceFormatUnsupported {
                        offered: config.source_format(),
                        supported: settings::SUPPORTED_FORMATS,
                    }
                }
                sys::AMF_INVALID_RESOLUTION | sys::AMF_OUT_OF_RANGE => {
                    EncodeErrorKind::ResolutionUnsupported {
                        maximum: self.maximum_size().unwrap_or(resolution),
                    }
                }
                other => classify(other, "AMFComponent::Init"),
            };
            return Err(EncodeError::new(self.error_context, kind));
        }
        Ok(())
    }

    /// The parameter sets a container needs before the first packet.
    ///
    /// AMF hands them over as a property holding a buffer, so this is two
    /// references to give back: the one the property's variant took, and the one
    /// `QueryInterface` took.
    fn parameter_sets(&self) -> Result<Vec<u8>, EncodeError> {
        let name = settings::extra_data(self.codec);
        // SAFETY: the component is live and initialised, and its vtable begins
        // with `AMFPropertyStorage`'s entries.
        let variant =
            unsafe { api::get_property(self.component.cast::<sys::AMFPropertyStorage>(), name) }
                .map_err(|status| {
                    EncodeError::new(
                        self.error_context,
                        classify(status, "AMFComponent::GetProperty (parameter sets)"),
                    )
                })?;

        let Some(interface) = api::as_interface(&variant) else {
            return Err(EncodeError::new(
                self.error_context,
                EncodeErrorKind::Api {
                    operation: "AMFComponent::GetProperty (parameter sets)",
                    status: sys::AMF_OK,
                    status_name: "AMF_OK",
                    detail: "the encoder reported no parameter sets, so a container that stores \
                             them out of band could not be configured"
                        .to_owned(),
                },
            ));
        };

        // SAFETY: the variant held a live interface, and the copy `get_property`
        // made took a reference to it which this function owns and releases
        // below.
        let buffer = unsafe { api::query_interface::<sys::AMFBuffer>(interface, &api::BUFFER_IID) };
        let bytes = match buffer {
            Ok(buffer) => {
                // SAFETY: `buffer` is the live `AMFBuffer` just obtained, and
                // this reads its contents before either reference is released.
                let bytes = unsafe { buffer_bytes(buffer) };
                // SAFETY: the reference came from the successful
                // `QueryInterface` above and is given back exactly once, here.
                let _ = unsafe { api::release(buffer.cast::<sys::AMFInterface>()) };
                Ok(bytes)
            }
            Err(status) => Err(EncodeError::new(
                self.error_context,
                classify(status, "AMFInterface::QueryInterface (AMFBuffer)"),
            )),
        };

        // SAFETY: the reference the variant carries belongs to this function —
        // `GetProperty` copies the property's variant, which acquires the
        // interface — and is given back exactly once, here.
        let _ = unsafe { api::release(interface) };
        bytes
    }

    /// Encodes one frame, and releases everything derived from its texture
    /// before returning.
    ///
    /// The whole of the encoder's business with the caller's texture happens
    /// here, which is what the interface promises
    /// ([`crate::frame::SourceTexture`]): wrap it in an `AMFSurface`, submit it,
    /// wait for the coded picture, wait for AMF to give the wrapper back, and
    /// release it. When this returns — by any path — nothing in AMF holds the
    /// wrapper, so the capture backend may recycle the texture immediately.
    fn submit(&mut self, frame: &SourceFrame<'_>, duration: i64) -> Result<(), EncodeError> {
        if self.ready.len() + usize::from(self.locked.is_some()) >= OUTPUT_CAPACITY {
            return Err(EncodeError::new(
                self.error_context,
                EncodeErrorKind::OutputBuffersExhausted {
                    capacity: OUTPUT_CAPACITY,
                },
            ));
        }

        let idr = force_idr(frame.forces_keyframe(), self.encoded_frames);
        let surface = self.wrap_texture(frame)?;
        // From here on the surface is derived from a texture this encoder does
        // not own, so every path has to release it before returning.
        let coded = self.encode(surface, frame, duration, idr);
        self.release_surface(surface);

        let coded = coded?;
        self.encoded_frames += 1;
        self.ready.push_back(coded);
        Ok(())
    }

    /// Wraps the caller's texture in an `AMFSurface`, without copying it.
    fn wrap_texture(&self, frame: &SourceFrame<'_>) -> Result<*mut sys::AMFSurface, EncodeError> {
        // SAFETY: the context is live for as long as the session is.
        let Some(wrap) = (unsafe { (*(*self.context).pVtbl).CreateSurfaceFromDX11Native }) else {
            return Err(missing(
                self.error_context,
                "AMFContext::CreateSurfaceFromDX11Native",
            ));
        };

        let mut surface: *mut sys::AMFSurface = ptr::null_mut();
        // SAFETY: the context is live; the texture is the one the caller
        // promised is a live `ID3D11Texture2D` on this context's device, and
        // that promise holds for the whole of this call because the frame is
        // borrowed for it. No observer is registered, because `submit` waits for
        // the wrapper's reference count to fall to its own before it returns —
        // see `release_surface` — rather than being told about it later.
        let status = unsafe {
            wrap(
                self.context,
                frame.texture().as_raw(),
                &raw mut surface,
                ptr::null_mut(),
            )
        };
        if status != sys::AMF_OK || surface.is_null() {
            return Err(EncodeError::new(
                self.error_context,
                classify(status, "AMFContext::CreateSurfaceFromDX11Native"),
            ));
        }
        Ok(surface)
    }

    /// Submits one wrapped frame and waits for its coded picture.
    ///
    /// `idr` is whether this picture has to be coded as an IDR — see
    /// [`force_idr`], which is where the decision is made and explained.
    fn encode(
        &self,
        surface: *mut sys::AMFSurface,
        frame: &SourceFrame<'_>,
        duration: i64,
        idr: bool,
    ) -> Result<Coded, EncodeError> {
        // SAFETY: `surface` is the live wrapper `wrap_texture` produced, held
        // for the whole of this call by its caller.
        if let Some(set_pts) = unsafe { (*(*surface).pVtbl).SetPts } {
            // SAFETY: as above. AMF carries the timestamp through the encoder
            // and hands it back on the coded picture, so the units are Clipped's
            // to choose within AMF's hundred-nanosecond tick.
            unsafe { set_pts(surface, presentation_units(frame.presentation_time())) };
        }
        // SAFETY: as above.
        if let Some(set_duration) = unsafe { (*(*surface).pVtbl).SetDuration } {
            // SAFETY: as above.
            unsafe { set_duration(surface, duration) };
        }

        if idr {
            for property in settings::forced_keyframe(self.codec) {
                // SAFETY: the surface is live and its vtable begins with
                // `AMFPropertyStorage`'s entries, because `AMFSurface` derives
                // from it through `AMFData`.
                let status = unsafe {
                    api::set_property(
                        surface.cast::<sys::AMFPropertyStorage>(),
                        property.name,
                        property.value,
                    )
                };
                if status != sys::AMF_OK {
                    // Not fatal: a frame that was meant to be a keyframe and is
                    // not costs a clip its exact start point, and refusing to
                    // encode it at all would cost the recording the frame
                    // (AGENTS.md section 17).
                    tracing::warn!(
                        property = %api::name_to_string(property.name),
                        status,
                        status_name = api::status_name(status),
                        "a frame asked to be a keyframe and the encoder would not be told"
                    );
                }
            }
        }

        self.submit_input(surface)?;
        self.wait_for_output()
    }

    /// Hands the surface to the encoder, waiting out a full input queue.
    ///
    /// The queue cannot be full in this backend's own loop — a frame is drained
    /// before the next is submitted — so this only matters if AMF decides to
    /// hold more than it was given.
    fn submit_input(&self, surface: *mut sys::AMFSurface) -> Result<(), EncodeError> {
        // SAFETY: the component is live and initialised.
        let Some(submit) = (unsafe { (*(*self.component).pVtbl).SubmitInput }) else {
            return Err(missing(self.error_context, "AMFComponent::SubmitInput"));
        };

        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            // SAFETY: the component is live and `surface` is the live wrapper
            // its caller holds a reference to for the whole of this call. AMF
            // takes its own reference to it while the frame is in the encoder.
            let status = unsafe { submit(self.component, surface.cast::<sys::AMFData>()) };
            match status {
                sys::AMF_OK => return Ok(()),
                sys::AMF_INPUT_FULL if Instant::now() < deadline => {
                    std::thread::sleep(POLL_INTERVAL);
                }
                sys::AMF_NEED_MORE_INPUT => {
                    // AMF is holding the picture back until it has more of them,
                    // which is what B-frames do — and this backend configures
                    // none, precisely so that a borrowed texture never has to
                    // stay available past `submit` (see `settings`). Rather than
                    // quietly keep the caller's texture, the encoder is drained
                    // so that the picture completes and the wrapper is released,
                    // and the failure is reported.
                    self.discard_queued();
                    return Err(EncodeError::new(
                        self.error_context,
                        EncodeErrorKind::Api {
                            operation: "AMFComponent::SubmitInput",
                            status,
                            status_name: api::status_name(status),
                            detail: "the encoder buffered the picture, which it must not do with \
                                     B-frames disabled, because the input texture belongs to the \
                                     caller and is released when this call returns"
                                .to_owned(),
                        },
                    ));
                }
                other => {
                    return Err(EncodeError::new(
                        self.error_context,
                        classify(other, "AMFComponent::SubmitInput"),
                    ))
                }
            }
        }
    }

    /// Waits for the coded picture belonging to the submission just made.
    ///
    /// This is what makes an asynchronous encoder synchronous, and it is not a
    /// concession: until the picture is coded, AMF is still reading the caller's
    /// texture, and `submit` may not return while that is true.
    fn wait_for_output(&self) -> Result<Coded, EncodeError> {
        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            match self.query_output("AMFComponent::QueryOutput")? {
                Output::Data(coded) => return Ok(coded),
                Output::Empty | Output::EndOfStream if Instant::now() < deadline => {
                    std::thread::sleep(POLL_INTERVAL);
                }
                _ => {
                    return Err(EncodeError::new(
                        self.error_context,
                        EncodeErrorKind::Api {
                            operation: "AMFComponent::QueryOutput",
                            status: sys::AMF_REPEAT,
                            status_name: api::status_name(sys::AMF_REPEAT),
                            detail: format!(
                                "the encoder produced no picture for a submitted frame within \
                                 {} seconds",
                                FRAME_DEADLINE.as_secs()
                            ),
                        },
                    ))
                }
            }
        }
    }

    /// One call to `QueryOutput`, with the coded picture read out of whatever
    /// it produced.
    fn query_output(&self, operation: &'static str) -> Result<Output, EncodeError> {
        // SAFETY: the component is live and initialised.
        let Some(query) = (unsafe { (*(*self.component).pVtbl).QueryOutput }) else {
            return Err(missing(self.error_context, "AMFComponent::QueryOutput"));
        };

        let mut data: *mut sys::AMFData = ptr::null_mut();
        // SAFETY: the component is live and `data` is a live local
        // out-parameter. On success it holds a reference, which becomes the one
        // `Coded` owns.
        let status = unsafe { query(self.component, &raw mut data) };
        match status {
            // AMD's own documentation warns that some components answer AMF_OK
            // with nothing attached when there is nothing ready yet, which is
            // the same condition as AMF_REPEAT.
            sys::AMF_OK if data.is_null() => Ok(Output::Empty),
            sys::AMF_OK => self.read_coded(data).map(Output::Data),
            sys::AMF_REPEAT => Ok(Output::Empty),
            sys::AMF_EOF => Ok(Output::EndOfStream),
            other => Err(EncodeError::new(
                self.error_context,
                classify(other, operation),
            )),
        }
    }

    /// Turns the data AMF produced into a coded picture, taking ownership of the
    /// buffer behind it.
    fn read_coded(&self, data: *mut sys::AMFData) -> Result<Coded, EncodeError> {
        // The picture type is a property of the data, so it is read before the
        // interface is narrowed.
        // SAFETY: `data` is the live object `QueryOutput` just produced, and
        // this session holds the reference to it.
        let picture = unsafe {
            api::get_property(
                data.cast::<sys::AMFPropertyStorage>(),
                settings::output_data_type(self.codec),
            )
        }
        .ok()
        .and_then(|variant| api::as_int64(&variant))
        .map_or(PictureKind::Unknown, |value| {
            settings::picture_kind(self.codec, value)
        });

        // SAFETY: `data` is live and its vtable begins with `AMFData`'s entries.
        let presentation = unsafe { (*(*data).pVtbl).GetPts }
            // SAFETY: as above.
            .map_or(0, |get| unsafe { get(data) });

        // SAFETY: `data` is the live object this session holds a reference to,
        // and `BUFFER_IID` is the identifier of `AMFBuffer`, so the pointer that
        // comes back is used with the right vtable.
        let buffer = unsafe {
            api::query_interface::<sys::AMFBuffer>(
                data.cast::<sys::AMFInterface>(),
                &api::BUFFER_IID,
            )
        };

        // The reference `QueryOutput` handed over is this session's, and once
        // the buffer interface has been taken from it there is a second one. The
        // first is given back here so that `Coded` owns exactly one.
        // SAFETY: the reference came from `QueryOutput` and is given back
        // exactly once, here. When the narrowing above succeeded, the buffer
        // reference keeps the object alive.
        let release_data = || unsafe { api::release(data.cast::<sys::AMFInterface>()) };

        match buffer {
            Ok(buffer) => {
                let _ = release_data();
                // SAFETY: `buffer` is a live `AMFBuffer` this session holds the
                // only remaining reference to, and the pointer it reports stays
                // valid until that reference is released in `release_coded`.
                let (data_ptr, length) = unsafe { buffer_range(buffer) };
                Ok(Coded {
                    buffer,
                    data: data_ptr,
                    length,
                    presentation: Duration::from_nanos(presentation.unsigned_abs() * 100),
                    picture,
                })
            }
            Err(status) => {
                let _ = release_data();
                Err(EncodeError::new(
                    self.error_context,
                    classify(status, "AMFInterface::QueryInterface (AMFBuffer)"),
                ))
            }
        }
    }

    /// Takes the next coded packet, releasing whatever the caller was holding.
    fn next_packet(&mut self) -> Result<Option<EncodedPacket<'_>>, EncodeError> {
        if let Some(previous) = self.locked.take() {
            self.release_coded(&previous);
        }

        let Some(coded) = self.ready.pop_front() else {
            return Ok(None);
        };

        // Copied out before the picture is moved into `locked`, so that the
        // owning value can be moved rather than duplicated. Raw pointers and
        // lengths are not a reference to anything; the reference stays in the
        // single `Coded` this session now holds.
        let (data, length) = (coded.data, coded.length);
        let presentation = coded.presentation;
        let picture = coded.picture;
        self.locked = Some(coded);

        let data = if data.is_null() || length == 0 {
            &[][..]
        } else {
            // SAFETY: AMF's buffer holds `length` readable bytes at `data` for
            // as long as the reference `Coded` owns is held, which is until the
            // next call to this method or `Drop`. The returned slice borrows
            // `*self` for exactly that long, so the compiler will not let a
            // caller hold it across either.
            unsafe { core::slice::from_raw_parts(data, length) }
        };

        Ok(Some(EncodedPacket::new(
            data,
            presentation,
            // Equal to the presentation time by construction: the encoder is
            // configured without B-frames, so pictures come out in the order
            // they went in (see `settings`).
            presentation,
            picture,
        )))
    }

    /// Flushes the encoder at the end of the stream.
    ///
    /// Every picture has already been collected by the `submit` that produced
    /// it, so this declares the end of the stream and collects nothing — but it
    /// collects anyway, because "nothing is outstanding" is this backend's
    /// belief about AMF rather than a promise AMF made.
    fn finish(&mut self) -> Result<(), EncodeError> {
        // SAFETY: the component is live and initialised.
        let Some(drain) = (unsafe { (*(*self.component).pVtbl).Drain }) else {
            return Err(missing(self.error_context, "AMFComponent::Drain"));
        };
        // SAFETY: as above.
        let status = unsafe { drain(self.component) };
        if status != sys::AMF_OK {
            return Err(EncodeError::new(
                self.error_context,
                classify(status, "AMFComponent::Drain"),
            ));
        }

        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            match self.query_output("AMFComponent::QueryOutput (draining)")? {
                Output::EndOfStream => return Ok(()),
                Output::Data(coded) => {
                    self.encoded_frames += 1;
                    self.ready.push_back(coded);
                }
                Output::Empty if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
                Output::Empty => {
                    return Err(EncodeError::new(
                        self.error_context,
                        EncodeErrorKind::Api {
                            operation: "AMFComponent::QueryOutput (draining)",
                            status: sys::AMF_REPEAT,
                            status_name: api::status_name(sys::AMF_REPEAT),
                            detail: format!(
                                "the encoder did not finish draining within {} seconds",
                                FRAME_DEADLINE.as_secs()
                            ),
                        },
                    ))
                }
            }
        }
    }

    /// Throws away everything the encoder is holding, input and output alike.
    ///
    /// The only thing that releases an input surface AMF has decided to keep,
    /// which is why it exists (see `submit_input` and `release_surface`).
    fn discard_queued(&self) {
        // SAFETY: the component is live and initialised.
        if let Some(flush) = unsafe { (*(*self.component).pVtbl).Flush } {
            // SAFETY: as above.
            let status = unsafe { flush(self.component) };
            log_release_failure(status, "AMFComponent::Flush");
        }
    }

    /// Gives back this session's reference to a submitted texture's wrapper,
    /// once AMF has given back its own.
    ///
    /// The reference count is the whole argument. AMF takes a reference to the
    /// wrapper while the frame is in the encoder and gives it back when it has
    /// finished with the pixels, so a count of one means the only reference left
    /// is this session's and nothing in AMF can still read the texture. AMD's
    /// API reference describes the same thing from the other side: input samples
    /// are tracked after submission through the surface's observer, which is
    /// called when the last reference goes.
    fn release_surface(&self, surface: *mut sys::AMFSurface) {
        let interface = surface.cast::<sys::AMFInterface>();

        if !wait_until_unused(interface, RELEASE_DEADLINE) {
            // AMF is still holding a texture the caller is about to be told it
            // may recycle. Throwing away what the encoder has queued is what
            // gets it back; losing those pictures is a far smaller failure than
            // encoding whatever the capture backend writes over the surface next
            // (AGENTS.md section 22).
            tracing::warn!(
                "AMF still held a submitted frame after its picture was coded; discarding what \
                 the encoder had queued so that the capture backend can reuse the texture"
            );
            self.discard_queued();
            wait_until_unused(interface, RELEASE_DEADLINE);
        }

        // SAFETY: this session created the wrapper in `wrap_texture` and holds
        // exactly one reference to it, given back exactly once, here.
        let remaining = unsafe { api::release(interface) };
        if remaining != 0 {
            // Not silent, and not something a caller could act on: the encoder
            // has been flushed and AMF has still not let go, which means the
            // texture may be read after this returns (AGENTS.md section 15).
            tracing::error!(
                remaining,
                "AMF would not release a submitted frame, so the texture it wraps may still be \
                 read after submit returned"
            );
        }
    }

    /// Gives back the reference a coded picture owns.
    fn release_coded(&self, coded: &Coded) {
        // SAFETY: the reference belongs to `coded`, which is moved between the
        // queues rather than copied out of them, so it is given back exactly
        // once — either here or in `Drop`, never both.
        let _ = unsafe { api::release(coded.buffer.cast::<sys::AMFInterface>()) };
    }
}

impl Drop for Session {
    /// Releases everything, in the order AMF requires.
    ///
    /// Runs from `AmfEncoder::shut_down` and from an unwind, and is the only
    /// place any of this is released — there is no path that frees a session
    /// without going through here (AGENTS.md section 58).
    fn drop(&mut self) {
        // Nothing derived from a caller's texture can be outstanding here:
        // `submit` releases every wrapper before it returns. What is left is
        // this session's own coded pictures.
        let outstanding: Vec<Coded> = self
            .locked
            .take()
            .into_iter()
            .chain(self.ready.drain(..))
            .collect();
        for coded in &outstanding {
            self.release_coded(coded);
        }

        self.release_component();

        if !self.context.is_null() {
            // The context is terminated after the component, because the
            // component was created from it and holds the device the context
            // owns a reference to.
            // SAFETY: the context was created by `Session::open` and is
            // terminated and released exactly once, here.
            if let Some(terminate) = unsafe { (*(*self.context).pVtbl).Terminate } {
                // SAFETY: as above.
                let status = unsafe { terminate(self.context) };
                log_release_failure(status, "AMFContext::Terminate");
            }
            // SAFETY: as above.
            let _ = unsafe { api::release(self.context.cast::<sys::AMFInterface>()) };
            self.context = ptr::null_mut();
        }
        // `self.runtime` is dropped after this, by field order, which frees the
        // module — so nothing above is calling into unmapped memory.
    }
}

/// What one call to `QueryOutput` produced.
#[derive(Debug)]
enum Output {
    /// A coded picture.
    Data(Coded),
    /// Nothing yet.
    Empty,
    /// Everything submitted has been collected.
    EndOfStream,
}

/// Waits until `interface` has no reference but the caller's.
///
/// Returns whether it got there. The probe is an `Acquire` followed by a
/// `Release`, which reports the count without giving anything up: the caller
/// holds a reference throughout, so the count can never reach zero in the
/// middle of it.
fn wait_until_unused(interface: *mut sys::AMFInterface, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        // SAFETY: the caller holds a live reference to `interface` for the whole
        // of this call, and the two calls below are balanced, so the count this
        // function observes is the count before it ran.
        let held = unsafe {
            let _ = api::acquire(interface);
            api::release(interface)
        };
        if held <= 1 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The bytes an AMF buffer holds, copied out.
///
/// # Safety
///
/// `buffer` must be a live `AMFBuffer` the caller holds a reference to.
unsafe fn buffer_bytes(buffer: *mut sys::AMFBuffer) -> Vec<u8> {
    // SAFETY: the caller guarantees a live buffer.
    let (data, length) = unsafe { buffer_range(buffer) };
    if data.is_null() || length == 0 {
        return Vec::new();
    }
    // SAFETY: AMF reports `length` readable bytes at `data`, valid while the
    // caller's reference is held, which it is for the whole of this call.
    unsafe { core::slice::from_raw_parts(data, length) }.to_vec()
}

/// Where an AMF buffer's bytes are, and how many there are.
///
/// # Safety
///
/// `buffer` must be a live `AMFBuffer` the caller holds a reference to. The
/// pointer returned is valid only while that reference is.
unsafe fn buffer_range(buffer: *mut sys::AMFBuffer) -> (*const u8, usize) {
    // SAFETY: the caller guarantees a live buffer, so its vtable is valid.
    let native = unsafe { (*(*buffer).pVtbl).GetNative };
    // SAFETY: as above.
    let size = unsafe { (*(*buffer).pVtbl).GetSize };

    let (Some(native), Some(size)) = (native, size) else {
        return (ptr::null(), 0);
    };

    // SAFETY: the buffer is live for the duration of both calls.
    let (data, length) = unsafe { (native(buffer), size(buffer)) };
    if data.is_null() || length == 0 {
        (ptr::null(), 0)
    } else {
        (data.cast::<u8>().cast_const(), length)
    }
}

/// Maps an AMF status onto the platform-neutral failure it represents.
fn classify(status: sys::AMF_RESULT, operation: &'static str) -> EncodeErrorKind {
    match status {
        sys::AMF_OUT_OF_MEMORY => EncodeErrorKind::OutOfMemory,
        // A driver reset, a GPU that vanished, or Direct3D refusing on the
        // encoder's behalf — all of which mean the device the session was built
        // on is gone (AGENTS.md section 16).
        sys::AMF_NO_DEVICE | sys::AMF_DIRECTX_FAILED => EncodeErrorKind::DeviceLost,
        sys::AMF_NOT_SUPPORTED
        | sys::AMF_NOT_IMPLEMENTED
        | sys::AMF_CODEC_NOT_SUPPORTED
        | sys::AMF_ENCODER_NOT_PRESENT => EncodeErrorKind::CodecUnsupported,
        _ => EncodeErrorKind::Api {
            operation,
            status,
            status_name: api::status_name(status),
            detail: String::new(),
        },
    }
}

/// The error for a vtable entry the runtime did not provide.
///
/// Only reachable on a runtime whose table is missing a method the version it
/// reports promises, which means the library is not what it claims to be.
fn missing(context: EncodeContext, operation: &'static str) -> EncodeError {
    EncodeError::new(
        context,
        EncodeErrorKind::RuntimeUnavailable {
            library: api::LIBRARY,
            detail: format!("the runtime does not provide {operation}"),
        },
    )
}

/// Logs a failure to release something, which is all a caller could do with it.
fn log_release_failure(status: sys::AMF_RESULT, operation: &'static str) {
    if status != sys::AMF_OK {
        // Not silent (AGENTS.md section 15) and not fatal: teardown carries on
        // releasing the rest, because stopping half way would leak the
        // remainder.
        tracing::warn!(
            operation,
            status,
            status_name = api::status_name(status),
            "an AMF resource could not be released"
        );
    }
}
