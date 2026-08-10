//! The NVENC backend: encoding a Direct3D 11 texture on NVIDIA hardware.
//!
//! # How this reaches NVENC, and why
//!
//! Through NVIDIA's own interface, `nvEncodeAPI.h`, loaded from the driver at
//! run time. Two other routes exist and were rejected:
//!
//! - **Media Foundation's NVIDIA transform.** Reaches the same silicon through
//!   an operating system abstraction, which is attractive until the input
//!   format is considered: the hardware transforms take NV12, and captured
//!   frames are BGRA. Every frame would need a colour conversion pass — a
//!   shader or a video processor — before it reached the encoder. NVENC's own
//!   interface takes a BGRA surface directly and converts inside the encoder,
//!   which is the difference between one GPU pass per frame and none.
//! - **A binding crate from crates.io.** The maintained ones wrap NVENC's CUDA
//!   path rather than its DirectX path, so they would pull in a CUDA runtime to
//!   avoid a copy that the DirectX path does not make in the first place.
//!
//! There is no SDK to install and nothing to link: the interface is a header,
//! and the implementation is `nvEncodeAPI64.dll`, which the display driver
//! installs into System32. `sys.rs` holds bindings generated from that header —
//! NVIDIA publishes it under the MIT licence through
//! [nv-codec-headers](https://github.com/FFmpeg/nv-codec-headers), which is
//! what makes it redistributable at all (AGENTS.md sections 11 and 12).
//!
//! # The zero-copy path
//!
//! ```text
//! Windows Graphics Capture ─→ ID3D11Texture2D (BGRA, on the GPU)
//!                                   │
//!                    nvEncRegisterResource / nvEncMapInputResource
//!                                   │
//!                            nvEncEncodePicture
//!                                   │
//!                             nvEncLockBitstream ─→ bytes for the muxer
//! ```
//!
//! The picture never enters system memory. Only the encoded bitstream does, and
//! that is the point: at 2560x1440 and 60 frames a second a captured frame is
//! 14 MB, and the encoded version of it is around 80 KB (AGENTS.md section 18).
//!
//! # Ownership
//!
//! [`NvencEncoder`] owns the loaded runtime, the encoder session, every output
//! buffer, and every resource registration in flight. All of it is released by
//! [`Session::drop`], which runs from `shut_down` and from `Drop`, so an unwind
//! on the encoding thread cannot leak a session. That matters more here than in
//! most places: consumer NVIDIA cards permit a small number of concurrent
//! encoding sessions across the whole machine, and a leaked one is taken away
//! from every application until the process exits (AGENTS.md section 58).

#[allow(
    dead_code,
    unused_imports,
    unnecessary_transmutes,
    missing_docs,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unreachable_pub,
    missing_debug_implementations,
    clippy::all,
    clippy::pedantic,
    // The generated bitfield accessors are `unsafe` and carry no SAFETY
    // comments, because nothing writes them by hand. The workspace lint is
    // about code contributors write (AGENTS.md section 42); every `unsafe`
    // block in `api.rs`, `mod.rs` and `settings.rs` still has to justify
    // itself.
    clippy::undocumented_unsafe_blocks
)]
mod sys;

mod api;
mod settings;

#[cfg(test)]
mod tests;

use core::ffi::c_void;
use core::fmt;
use core::mem;
use core::ptr;
use core::time::Duration;
use std::collections::VecDeque;

use crate::backend::VideoEncoder;
use crate::codec::{Codec, EncoderKind, Resolution};
use crate::config::EncoderConfig;
use crate::error::{EncodeContext, EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SurfaceKind};
use crate::packet::{EncodedPacket, PictureKind};

/// The kinds of graphics resource this backend can bind.
const SUPPORTED_SURFACES: &[SurfaceKind] = &[SurfaceKind::D3d11Texture2D];

/// How many encoded frames the session can hold before the caller has to drain
/// them.
///
/// One is enough for the configuration this backend uses — no B-frames and no
/// lookahead, so every submission produces exactly one packet — and the surplus
/// is there so that a future configuration which reorders pictures does not
/// have to change the interface. Each buffer is a driver allocation of a few
/// hundred kilobytes.
const OUTPUT_BUFFERS: usize = 8;

/// A hardware encoding session on an NVIDIA GPU.
///
/// Opened with [`open`](Self::open) against the Direct3D 11 device whose
/// textures will be submitted, and used through the
/// [`VideoEncoder`](crate::VideoEncoder) trait.
///
/// ```no_run
/// use core::time::Duration;
/// use clipped_encoder::{
///     BitRate, Codec, EncoderConfig, FrameRate, GraphicsDevice, NvencEncoder, RateControl,
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
/// let mut encoder = NvencEncoder::open(&device, config)?;
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
pub struct NvencEncoder {
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
// What crosses the boundary is a set of raw pointers: the NVENC session, its
// output buffers, and the module handle. None of them is thread-bound. NVENC
// sessions have no thread affinity — the Video Codec SDK's own samples create a
// session on one thread and encode on another — the Direct3D 11 device the
// session was opened against is free-threaded, and a module handle belongs to
// the process.
//
// What is *not* claimed is that two threads may use one encoder at once. They
// may not, and nothing here makes that possible: `VideoEncoder` is not `Sync`,
// every method takes `&mut self`, and an `EncodedPacket` borrows the encoder it
// came from.
unsafe impl Send for NvencEncoder {}

impl NvencEncoder {
    /// Opens a session on `device`.
    ///
    /// Everything that can be checked is checked here rather than on the first
    /// frame: that the runtime loads, that the driver is new enough, that this
    /// GPU encodes this codec, and that the resolution is within what it
    /// reports. A recording that cannot work should fail before the user
    /// believes it started (SPEC.md section 7).
    ///
    /// # Errors
    ///
    /// [`EncodeError`], naming the encoder, codec and resolution, for every one
    /// of those checks and for any failure to create the session.
    pub fn open(device: &GraphicsDevice, config: EncoderConfig) -> Result<Self, EncodeError> {
        let context = EncodeContext::new(EncoderKind::Nvenc, config.codec(), config.resolution());
        let fail = |kind| EncodeError::new(context, kind);

        if device.kind() != DeviceKind::D3d11 {
            return Err(fail(EncodeErrorKind::Configuration {
                detail: format!(
                    "NVENC binds Direct3D 11 textures and was given a {} device",
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

        if settings::buffer_format(config.source_format()).is_none() {
            return Err(fail(EncodeErrorKind::SurfaceFormatUnsupported {
                offered: config.source_format(),
                supported: settings::SUPPORTED_FORMATS,
            }));
        }

        let runtime = api::NvencRuntime::load().map_err(|failure| {
            fail(match failure {
                api::LoadFailure::TooOld { installed } => EncodeErrorKind::DriverTooOld {
                    required: (api::API_MAJOR, api::API_MINOR),
                    installed,
                    minimum_driver: api::MINIMUM_DRIVER,
                },
                other => EncodeErrorKind::RuntimeUnavailable {
                    library: api::LIBRARY,
                    detail: other.to_string(),
                },
            })
        })?;

        let mut session = Session::open(runtime, device.as_raw(), context)?;
        session.check_codec_and_size(&config)?;
        session.initialise(&config)?;
        let parameter_sets = session.parameter_sets()?;
        session.create_output_buffers()?;

        tracing::info!(
            encoder = %EncoderKind::Nvenc.log_encoder_family(),
            codec = config.codec().log_value(),
            resolution = %config.resolution(),
            frame_rate = %config.frame_rate(),
            rate_control = %config.rate_control(),
            preset = %config.preset(),
            parameter_set_bytes = parameter_sets.len(),
            "opened an NVENC session"
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

impl fmt::Debug for NvencEncoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NvencEncoder")
            .field("config", &self.config)
            .field("running", &self.session.is_some())
            .field("finished", &self.finished)
            .finish()
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        self.shut_down();
    }
}

impl VideoEncoder for NvencEncoder {
    fn encoder(&self) -> EncoderKind {
        EncoderKind::Nvenc
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

        let format = settings::buffer_format(self.config.source_format()).ok_or_else(|| {
            self.fail(EncodeErrorKind::SurfaceFormatUnsupported {
                offered: self.config.source_format(),
                supported: settings::SUPPORTED_FORMATS,
            })
        })?;
        let resolution = self.config.resolution();

        self.session()?.submit(frame, format, resolution)?;
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
                encoder = %EncoderKind::Nvenc.log_encoder_family(),
                codec = self.config.codec().log_value(),
                frames,
                "closed the NVENC session"
            );
        }
    }
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
    // an odd dimension has no representation. NVENC would refuse it with
    // NV_ENC_ERR_INVALID_PARAM, several calls later and without saying which
    // parameter.
    if width % 2 != 0 || height % 2 != 0 {
        return Err(EncodeErrorKind::Configuration {
            detail: format!(
                "{width}x{height} has an odd dimension, and 4:2:0 chroma needs both to be even"
            ),
        });
    }
    Ok(())
}

/// One frame's worth of hardware state, from submission to the packet that
/// comes out of it.
///
/// # Ownership
///
/// Holds three things the session must release, in this order: the encoded
/// bitstream is unlocked, the input resource is unmapped, and the registration
/// is dropped. Doing it in any other order leaves the driver holding a
/// reference to a texture the capture backend has already recycled.
///
/// It carries no timestamp: the one on the packet comes back from NVENC, which
/// echoes the input timestamp it was given, so there is nothing here to keep in
/// step with it.
#[derive(Debug, Clone, Copy)]
struct Pending {
    output: *mut c_void,
    registered: sys::NV_ENC_REGISTERED_PTR,
    mapped: sys::NV_ENC_INPUT_PTR,
}

/// The live NVENC session and everything it owns.
struct Session {
    runtime: api::NvencRuntime,
    encoder: *mut c_void,
    context: EncodeContext,
    /// Every output buffer created, for teardown.
    outputs: Vec<*mut c_void>,
    /// The output buffers not currently holding a frame.
    free: Vec<*mut c_void>,
    /// Submitted, and not yet known to have been encoded.
    in_flight: VecDeque<Pending>,
    /// Encoded and waiting to be handed over.
    ready: VecDeque<Pending>,
    /// The packet the caller is currently holding, which the next call
    /// releases.
    locked: Option<Pending>,
    encoded_frames: u64,
}

impl Session {
    /// Opens the session against a Direct3D 11 device.
    fn open(
        runtime: api::NvencRuntime,
        device: *mut c_void,
        context: EncodeContext,
    ) -> Result<Self, EncodeError> {
        // SAFETY: plain data, and NVENC requires everything it is not told
        // about to be zero.
        let mut params: sys::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = unsafe { mem::zeroed() };
        params.version = api::OPEN_SESSION_EX_PARAMS_VER;
        params.deviceType = sys::NV_ENC_DEVICE_TYPE_DIRECTX;
        params.device = device;
        params.apiVersion = api::API_MAJOR | (api::API_MINOR << 24);

        let function = runtime
            .functions()
            .nvEncOpenEncodeSessionEx
            .ok_or_else(|| missing(context, "nvEncOpenEncodeSessionEx"))?;

        let mut encoder: *mut c_void = ptr::null_mut();
        // SAFETY: `params` is a live local of the type the entry point expects,
        // with its version set; `encoder` is a live local out-parameter; and
        // `device` is the caller's Direct3D 11 device, which
        // `NvencEncoder::open` has checked is non-null and which the caller
        // guarantees outlives the encoder.
        let status = unsafe { function(&raw mut params, &raw mut encoder) };

        if status != sys::NV_ENC_SUCCESS {
            // Consumer cards cap how many encoding sessions may exist at once,
            // across every application on the machine, and the two statuses
            // below are how that cap arrives. `INCOMPATIBLE_CLIENT_KEY` is the
            // one measured here: deliberately leaking sessions on a GeForce RTX
            // 4090 (driver 610.74) produced it on the thirteenth open, not the
            // out-of-memory status NVIDIA's documentation describes. Both are
            // mapped, because both are reported in the wild and neither means
            // the configuration is wrong.
            let kind = if status == sys::NV_ENC_ERR_OUT_OF_MEMORY
                || status == sys::NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY
            {
                tracing::debug!(
                    status,
                    status_name = api::status_name(status),
                    "NVENC refused to open a session, which is how a full session table arrives"
                );
                EncodeErrorKind::SessionLimitReached
            } else {
                // SAFETY: the session failed to open, so the only valid
                // argument is a null encoder, which the runtime accepts.
                let detail = unsafe { api::last_error(runtime.functions(), ptr::null_mut()) };
                classify(status, "nvEncOpenEncodeSessionEx", detail)
            };
            return Err(EncodeError::new(context, kind));
        }

        Ok(Self {
            runtime,
            encoder,
            context,
            outputs: Vec::new(),
            free: Vec::new(),
            in_flight: VecDeque::new(),
            ready: VecDeque::new(),
            locked: None,
            encoded_frames: 0,
        })
    }

    /// Asks the session whether it can do what it is about to be asked to do.
    fn check_codec_and_size(&self, config: &EncoderConfig) -> Result<(), EncodeError> {
        if !self.supports_codec(config.codec())? {
            return Err(EncodeError::new(
                self.context,
                EncodeErrorKind::CodecUnsupported,
            ));
        }

        let maximum = Resolution::new(
            self.capability(config.codec(), sys::NV_ENC_CAPS_WIDTH_MAX)?,
            self.capability(config.codec(), sys::NV_ENC_CAPS_HEIGHT_MAX)?,
        );
        let wanted = config.resolution();
        if wanted.width > maximum.width || wanted.height > maximum.height {
            return Err(EncodeError::new(
                self.context,
                EncodeErrorKind::ResolutionUnsupported { maximum },
            ));
        }
        Ok(())
    }

    /// Whether the hardware lists this codec among the ones it encodes.
    fn supports_codec(&self, codec: Codec) -> Result<bool, EncodeError> {
        let count_fn = self
            .runtime
            .functions()
            .nvEncGetEncodeGUIDCount
            .ok_or_else(|| missing(self.context, "nvEncGetEncodeGUIDCount"))?;
        let list_fn = self
            .runtime
            .functions()
            .nvEncGetEncodeGUIDs
            .ok_or_else(|| missing(self.context, "nvEncGetEncodeGUIDs"))?;

        let mut count: u32 = 0;
        // SAFETY: the session is live and `count` is a live local
        // out-parameter.
        let status = unsafe { count_fn(self.encoder, &raw mut count) };
        self.check(status, "nvEncGetEncodeGUIDCount")?;

        let mut guids: Vec<sys::GUID> = vec![
            sys::GUID {
                Data1: 0,
                Data2: 0,
                Data3: 0,
                Data4: [0; 8],
            };
            count as usize
        ];
        let mut written: u32 = 0;
        // SAFETY: `guids` has `count` elements and `count` is what the call
        // above reported, which is the buffer size the API is being told about;
        // `written` is a live local out-parameter.
        let status = unsafe { list_fn(self.encoder, guids.as_mut_ptr(), count, &raw mut written) };
        self.check(status, "nvEncGetEncodeGUIDs")?;
        guids.truncate(written as usize);

        let wanted = settings::codec_guid(codec);
        Ok(guids.iter().any(|guid| settings::same_guid(guid, &wanted)))
    }

    /// One numeric capability of this encoder for this codec.
    fn capability(&self, codec: Codec, capability: sys::NV_ENC_CAPS) -> Result<u32, EncodeError> {
        let function = self
            .runtime
            .functions()
            .nvEncGetEncodeCaps
            .ok_or_else(|| missing(self.context, "nvEncGetEncodeCaps"))?;

        // SAFETY: plain data.
        let mut param: sys::NV_ENC_CAPS_PARAM = unsafe { mem::zeroed() };
        param.version = api::CAPS_PARAM_VER;
        param.capsToQuery = capability;

        let mut value: i32 = 0;
        // SAFETY: the session is live, `param` and `value` are live locals, and
        // the codec GUID is one of the three constants in `settings`.
        let status = unsafe {
            function(
                self.encoder,
                settings::codec_guid(codec),
                &raw mut param,
                &raw mut value,
            )
        };
        self.check(status, "nvEncGetEncodeCaps")?;

        Ok(value.max(0).unsigned_abs())
    }

    /// Configures and starts the encoder.
    fn initialise(&mut self, config: &EncoderConfig) -> Result<(), EncodeError> {
        let mut preset = self.preset_config(config)?;
        settings::apply(config, &mut preset);

        let mut params = settings::initialise_params(config, &raw mut preset);
        let function = self
            .runtime
            .functions()
            .nvEncInitializeEncoder
            .ok_or_else(|| missing(self.context, "nvEncInitializeEncoder"))?;

        // SAFETY: the session is live, `params` is a live local of the expected
        // type, and the `encodeConfig` pointer inside it points at `preset`,
        // which outlives this call. NVENC copies both during initialisation.
        let status = unsafe { function(self.encoder, &raw mut params) };
        self.check(status, "nvEncInitializeEncoder")
    }

    /// NVIDIA's own configuration for the chosen preset and tuning, which
    /// [`settings::apply`] then overwrites in the few places Clipped has an
    /// opinion.
    fn preset_config(&self, config: &EncoderConfig) -> Result<sys::NV_ENC_CONFIG, EncodeError> {
        let function = self
            .runtime
            .functions()
            .nvEncGetEncodePresetConfigEx
            .ok_or_else(|| missing(self.context, "nvEncGetEncodePresetConfigEx"))?;

        // SAFETY: plain data; both version fields are set below, which is what
        // NVENC requires of it.
        let mut preset: sys::NV_ENC_PRESET_CONFIG = unsafe { mem::zeroed() };
        preset.version = api::PRESET_CONFIG_VER;
        preset.presetCfg.version = api::CONFIG_VER;

        // SAFETY: the session is live, the GUIDs are the constants in
        // `settings`, and `preset` is a live local out-parameter.
        let status = unsafe {
            function(
                self.encoder,
                settings::codec_guid(config.codec()),
                settings::preset_guid(config.preset()),
                settings::tuning_info(config.preset()),
                &raw mut preset,
            )
        };
        self.check(status, "nvEncGetEncodePresetConfigEx")?;

        Ok(preset.presetCfg)
    }

    /// The sequence and picture parameter sets, for a container that stores
    /// them out of band.
    fn parameter_sets(&self) -> Result<Vec<u8>, EncodeError> {
        let function = self
            .runtime
            .functions()
            .nvEncGetSequenceParams
            .ok_or_else(|| missing(self.context, "nvEncGetSequenceParams"))?;

        // 512 bytes is `NV_MAX_SEQ_HDR_LEN` from the header: the largest header
        // NVENC will produce.
        let mut buffer = vec![0u8; 512];
        let mut written: u32 = 0;

        // SAFETY: plain data.
        let mut payload: sys::NV_ENC_SEQUENCE_PARAM_PAYLOAD = unsafe { mem::zeroed() };
        payload.version = api::SEQUENCE_PARAM_PAYLOAD_VER;
        payload.inBufferSize = buffer.len() as u32;
        payload.spsppsBuffer = buffer.as_mut_ptr().cast::<c_void>();
        payload.outSPSPPSPayloadSize = &raw mut written;

        // SAFETY: the session is initialised, `payload` is a live local, and
        // the two pointers inside it address a buffer and a counter that
        // outlive the call. `inBufferSize` is the real size of that buffer, so
        // NVENC cannot write past it.
        let status = unsafe { function(self.encoder, &raw mut payload) };
        self.check(status, "nvEncGetSequenceParams")?;

        buffer.truncate(written as usize);
        Ok(buffer)
    }

    /// Allocates the pool of output buffers.
    fn create_output_buffers(&mut self) -> Result<(), EncodeError> {
        let function = self
            .runtime
            .functions()
            .nvEncCreateBitstreamBuffer
            .ok_or_else(|| missing(self.context, "nvEncCreateBitstreamBuffer"))?;

        for _ in 0..OUTPUT_BUFFERS {
            // SAFETY: plain data. `size` and `memoryHeap` are deprecated and
            // must be left at zero, which is what NVENC's own samples do.
            let mut create: sys::NV_ENC_CREATE_BITSTREAM_BUFFER = unsafe { mem::zeroed() };
            create.version = api::CREATE_BITSTREAM_BUFFER_VER;

            // SAFETY: the session is initialised and `create` is a live local
            // out-parameter.
            let status = unsafe { function(self.encoder, &raw mut create) };
            self.check(status, "nvEncCreateBitstreamBuffer")?;

            self.outputs.push(create.bitstreamBuffer);
            self.free.push(create.bitstreamBuffer);
        }
        Ok(())
    }

    /// Registers, maps and encodes one frame.
    fn submit(
        &mut self,
        frame: &SourceFrame<'_>,
        format: sys::NV_ENC_BUFFER_FORMAT,
        resolution: Resolution,
    ) -> Result<(), EncodeError> {
        let output = self.free.pop().ok_or_else(|| {
            EncodeError::new(
                self.context,
                EncodeErrorKind::OutputBuffersExhausted {
                    capacity: OUTPUT_BUFFERS,
                },
            )
        })?;

        // From here on the output buffer has been taken out of the free list,
        // so every failure path has to put it back.
        let mut pending = match self.register_and_map(frame, format, resolution) {
            Ok((registered, mapped)) => Pending {
                output,
                registered,
                mapped,
            },
            Err(error) => {
                self.free.push(output);
                return Err(error);
            }
        };

        match self.encode(&pending, frame, format, resolution) {
            Ok(complete) => {
                self.in_flight.push_back(pending);
                if complete {
                    // Every frame the encoder was holding has now been coded,
                    // and their bitstreams can be locked.
                    self.ready.append(&mut self.in_flight);
                }
                Ok(())
            }
            Err(error) => {
                self.release(&mut pending, false);
                self.free.push(output);
                Err(error)
            }
        }
    }

    /// Hands the texture to NVENC without copying it.
    fn register_and_map(
        &self,
        frame: &SourceFrame<'_>,
        format: sys::NV_ENC_BUFFER_FORMAT,
        resolution: Resolution,
    ) -> Result<(sys::NV_ENC_REGISTERED_PTR, sys::NV_ENC_INPUT_PTR), EncodeError> {
        let register_fn = self
            .runtime
            .functions()
            .nvEncRegisterResource
            .ok_or_else(|| missing(self.context, "nvEncRegisterResource"))?;
        let map_fn = self
            .runtime
            .functions()
            .nvEncMapInputResource
            .ok_or_else(|| missing(self.context, "nvEncMapInputResource"))?;

        // SAFETY: plain data.
        let mut register: sys::NV_ENC_REGISTER_RESOURCE = unsafe { mem::zeroed() };
        register.version = api::REGISTER_RESOURCE_VER;
        register.resourceType = sys::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
        register.width = resolution.width;
        register.height = resolution.height;
        // Direct3D resources have no pitch to declare: the driver reads it from
        // the texture.
        register.pitch = 0;
        register.subResourceIndex = 0;
        register.resourceToRegister = frame.texture().as_raw();
        register.bufferFormat = format;
        register.bufferUsage = sys::NV_ENC_INPUT_IMAGE;

        // SAFETY: the session is initialised and `register` is a live local.
        // `resourceToRegister` is the texture the caller promised is a live
        // `ID3D11Texture2D` on this session's device, and the promise holds for
        // the whole of this call because the frame is borrowed for it. The
        // registration it produces is released before `submit` returns to the
        // caller, or by `Session::drop`.
        let status = unsafe { register_fn(self.encoder, &raw mut register) };
        self.check(status, "nvEncRegisterResource")?;

        // SAFETY: plain data.
        let mut map: sys::NV_ENC_MAP_INPUT_RESOURCE = unsafe { mem::zeroed() };
        map.version = api::MAP_INPUT_RESOURCE_VER;
        map.registeredResource = register.registeredResource;

        // SAFETY: the session is initialised, `map` is a live local, and
        // `registeredResource` came from the successful registration above.
        let status = unsafe { map_fn(self.encoder, &raw mut map) };
        match self.check(status, "nvEncMapInputResource") {
            Ok(()) => Ok((register.registeredResource, map.mappedResource)),
            Err(error) => {
                // The registration succeeded and the mapping did not, so the
                // registration is this function's to release.
                if let Some(unregister) = self.runtime.functions().nvEncUnregisterResource {
                    // SAFETY: the pointer came from the successful registration
                    // above and has not been released.
                    let _ = unsafe { unregister(self.encoder, register.registeredResource) };
                }
                Err(error)
            }
        }
    }

    /// Encodes one mapped frame, and says whether the encoder produced output.
    fn encode(
        &self,
        pending: &Pending,
        frame: &SourceFrame<'_>,
        format: sys::NV_ENC_BUFFER_FORMAT,
        resolution: Resolution,
    ) -> Result<bool, EncodeError> {
        let function = self
            .runtime
            .functions()
            .nvEncEncodePicture
            .ok_or_else(|| missing(self.context, "nvEncEncodePicture"))?;

        // SAFETY: plain data.
        let mut params: sys::NV_ENC_PIC_PARAMS = unsafe { mem::zeroed() };
        params.version = api::PIC_PARAMS_VER;
        params.inputWidth = resolution.width;
        params.inputHeight = resolution.height;
        params.inputBuffer = pending.mapped;
        params.outputBitstream = pending.output;
        params.bufferFmt = format;
        params.pictureStruct = sys::NV_ENC_PIC_STRUCT_FRAME;
        // Nanoseconds, which is what comes back out on the packet. NVENC treats
        // this as an opaque number it echoes, so the units are Clipped's to
        // choose, and nanoseconds are what `Duration` already speaks.
        params.inputTimeStamp = presentation_nanos(frame.presentation_time());
        if frame.forces_keyframe() {
            params.encodePicFlags = sys::NV_ENC_PIC_FLAG_FORCEIDR as u32;
        }

        // SAFETY: the session is initialised and `params` is a live local
        // pointing at a mapped input and an output buffer this session owns,
        // both of which outlive the call.
        let status = unsafe { function(self.encoder, &raw mut params) };
        match status {
            sys::NV_ENC_SUCCESS => Ok(true),
            // Not a failure: the encoder is holding this frame back until it
            // has enough of them, which is ordinary whenever pictures are
            // reordered or a lookahead is configured.
            sys::NV_ENC_ERR_NEED_MORE_INPUT => Ok(false),
            _ => Err(self.error(status, "nvEncEncodePicture")),
        }
    }

    /// Takes the next encoded packet, releasing whatever the caller was holding.
    fn next_packet(&mut self) -> Result<Option<EncodedPacket<'_>>, EncodeError> {
        if let Some(mut previous) = self.locked.take() {
            self.release(&mut previous, true);
            self.free.push(previous.output);
        }

        let Some(pending) = self.ready.pop_front() else {
            return Ok(None);
        };

        let function = self
            .runtime
            .functions()
            .nvEncLockBitstream
            .ok_or_else(|| missing(self.context, "nvEncLockBitstream"))?;

        // SAFETY: plain data.
        let mut lock: sys::NV_ENC_LOCK_BITSTREAM = unsafe { mem::zeroed() };
        lock.version = api::LOCK_BITSTREAM_VER;
        lock.outputBitstream = pending.output;
        // Wait for the hardware rather than poll it: the encoding thread has
        // nothing else to do, and a spin would take CPU from the game.
        lock.set_doNotWait(0);

        // SAFETY: the session is initialised, `lock` is a live local, and
        // `outputBitstream` is a buffer this session created and has not
        // destroyed.
        let status = unsafe { function(self.encoder, &raw mut lock) };
        if let Err(error) = self.check(status, "nvEncLockBitstream") {
            let mut pending = pending;
            self.release(&mut pending, false);
            self.free.push(pending.output);
            return Err(error);
        }

        self.encoded_frames += 1;
        self.locked = Some(pending);

        let length = lock.bitstreamSizeInBytes as usize;
        let data = if length == 0 || lock.bitstreamBufferPtr.is_null() {
            &[][..]
        } else {
            // SAFETY: NVENC guarantees `bitstreamBufferPtr` addresses
            // `bitstreamSizeInBytes` readable bytes until the buffer is
            // unlocked, which happens in the next call to this method or in
            // `Drop`. The returned slice borrows `*self` for exactly that long,
            // so the compiler will not let a caller hold it across either.
            unsafe { core::slice::from_raw_parts(lock.bitstreamBufferPtr.cast::<u8>(), length) }
        };

        let presentation = Duration::from_nanos(lock.outputTimeStamp);
        Ok(Some(EncodedPacket::new(
            data,
            presentation,
            // Equal to the presentation time by construction: the encoder is
            // configured without B-frames, so pictures come out in the order
            // they went in (see `settings::apply`).
            presentation,
            picture_kind(lock.pictureType),
        )))
    }

    /// Flushes the encoder at the end of the stream.
    fn finish(&mut self) -> Result<(), EncodeError> {
        let function = self
            .runtime
            .functions()
            .nvEncEncodePicture
            .ok_or_else(|| missing(self.context, "nvEncEncodePicture"))?;

        // SAFETY: plain data. An end-of-stream submission carries no input and
        // no output buffer; everything but the version and the flag must be
        // zero.
        let mut params: sys::NV_ENC_PIC_PARAMS = unsafe { mem::zeroed() };
        params.version = api::PIC_PARAMS_VER;
        params.encodePicFlags = sys::NV_ENC_PIC_FLAG_EOS as u32;

        // SAFETY: the session is initialised and `params` is a live local.
        let status = unsafe { function(self.encoder, &raw mut params) };
        if status != sys::NV_ENC_SUCCESS {
            return Err(self.error(status, "nvEncEncodePicture (end of stream)"));
        }

        // Everything the encoder was holding has now been coded.
        self.ready.append(&mut self.in_flight);
        Ok(())
    }

    /// Releases one frame's hardware state.
    ///
    /// `unlock` says whether the bitstream is currently locked; unlocking one
    /// that is not produces an error the caller cannot act on.
    fn release(&self, pending: &mut Pending, unlock: bool) {
        let functions = self.runtime.functions();

        if unlock {
            if let Some(unlock_fn) = functions.nvEncUnlockBitstream {
                // SAFETY: the buffer belongs to this session and was locked by
                // `next_packet`.
                let status = unsafe { unlock_fn(self.encoder, pending.output) };
                log_release_failure(status, "nvEncUnlockBitstream");
            }
        }

        if !pending.mapped.is_null() {
            if let Some(unmap) = functions.nvEncUnmapInputResource {
                // SAFETY: the pointer came from a successful
                // `nvEncMapInputResource` on this session and has not been
                // unmapped.
                let status = unsafe { unmap(self.encoder, pending.mapped) };
                log_release_failure(status, "nvEncUnmapInputResource");
            }
            pending.mapped = ptr::null_mut();
        }

        if !pending.registered.is_null() {
            if let Some(unregister) = functions.nvEncUnregisterResource {
                // SAFETY: the pointer came from a successful
                // `nvEncRegisterResource` on this session and has not been
                // unregistered.
                let status = unsafe { unregister(self.encoder, pending.registered) };
                log_release_failure(status, "nvEncUnregisterResource");
            }
            pending.registered = ptr::null_mut();
        }
    }

    /// Turns a status into `Ok(())` or an error carrying this session's
    /// context.
    fn check(&self, status: sys::NVENCSTATUS, operation: &'static str) -> Result<(), EncodeError> {
        if status == sys::NV_ENC_SUCCESS {
            Ok(())
        } else {
            Err(self.error(status, operation))
        }
    }

    /// Builds an error from a status, including whatever the runtime says about
    /// it.
    fn error(&self, status: sys::NVENCSTATUS, operation: &'static str) -> EncodeError {
        // SAFETY: the session is live for as long as `self` is.
        let detail = unsafe { api::last_error(self.runtime.functions(), self.encoder) };
        EncodeError::new(self.context, classify(status, operation, detail))
    }
}

impl Drop for Session {
    /// Releases everything, in the order the driver requires.
    ///
    /// Runs from `NvencEncoder::shut_down` and from an unwind, and is the only
    /// place any of this is released — there is no path that frees a session
    /// without going through here (AGENTS.md section 58).
    fn drop(&mut self) {
        if let Some(mut locked) = self.locked.take() {
            self.release(&mut locked, true);
        }
        let mut outstanding: Vec<Pending> = self
            .ready
            .drain(..)
            .chain(self.in_flight.drain(..))
            .collect();
        for pending in &mut outstanding {
            self.release(pending, false);
        }

        if let Some(destroy) = self.runtime.functions().nvEncDestroyBitstreamBuffer {
            for output in self.outputs.drain(..) {
                // SAFETY: each buffer was created by this session and is
                // destroyed exactly once, here.
                let status = unsafe { destroy(self.encoder, output) };
                log_release_failure(status, "nvEncDestroyBitstreamBuffer");
            }
        }

        if let Some(destroy) = self.runtime.functions().nvEncDestroyEncoder {
            // SAFETY: the session was opened by `Session::open` and is
            // destroyed exactly once, here, after everything allocated from it
            // has been released.
            let status = unsafe { destroy(self.encoder) };
            log_release_failure(status, "nvEncDestroyEncoder");
        }
        self.encoder = ptr::null_mut();
    }
}

/// The presentation time as the nanoseconds NVENC carries through the encoder.
///
/// Saturating rather than truncating: a `Duration` beyond 584 years cannot
/// happen in a recording, and if it somehow did, clamping keeps timestamps
/// monotonic where wrapping would send them back to zero.
fn presentation_nanos(time: Duration) -> u64 {
    u64::try_from(time.as_nanos()).unwrap_or(u64::MAX)
}

/// Translates an NVENC picture type.
const fn picture_kind(picture: sys::NV_ENC_PIC_TYPE) -> PictureKind {
    match picture {
        sys::NV_ENC_PIC_TYPE_IDR => PictureKind::Keyframe,
        sys::NV_ENC_PIC_TYPE_I | sys::NV_ENC_PIC_TYPE_INTRA_REFRESH => PictureKind::Intra,
        sys::NV_ENC_PIC_TYPE_P | sys::NV_ENC_PIC_TYPE_NONREF_P | sys::NV_ENC_PIC_TYPE_SKIPPED => {
            PictureKind::Predicted
        }
        sys::NV_ENC_PIC_TYPE_B | sys::NV_ENC_PIC_TYPE_BI => PictureKind::Bidirectional,
        _ => PictureKind::Unknown,
    }
}

/// Maps an NVENC status onto the platform-neutral failure it represents.
fn classify(status: sys::NVENCSTATUS, operation: &'static str, detail: String) -> EncodeErrorKind {
    match status {
        sys::NV_ENC_ERR_OUT_OF_MEMORY => EncodeErrorKind::OutOfMemory,
        sys::NV_ENC_ERR_DEVICE_NOT_EXIST | sys::NV_ENC_ERR_INVALID_DEVICE => {
            EncodeErrorKind::DeviceLost
        }
        sys::NV_ENC_ERR_UNSUPPORTED_PARAM | sys::NV_ENC_ERR_UNIMPLEMENTED => {
            EncodeErrorKind::CodecUnsupported
        }
        _ => EncodeErrorKind::Api {
            operation,
            status,
            status_name: api::status_name(status),
            detail,
        },
    }
}

/// The error for a function pointer the runtime did not provide.
///
/// Only reachable on a driver whose table is missing an entry the interface
/// version promises, which means the library is not what it claims to be.
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
fn log_release_failure(status: sys::NVENCSTATUS, operation: &'static str) {
    if status != sys::NV_ENC_SUCCESS {
        // Not silent (AGENTS.md section 15) and not fatal: teardown carries on
        // releasing the rest, because stopping half way would leak the
        // remainder.
        tracing::warn!(
            operation,
            status,
            status_name = api::status_name(status),
            "an NVENC resource could not be released"
        );
    }
}
