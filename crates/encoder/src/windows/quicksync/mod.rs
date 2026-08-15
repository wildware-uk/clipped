//! The Quick Sync backend: encoding a Direct3D 11 texture on Intel hardware.
//!
//! # Nothing here has encoded a frame
//!
//! There is no Intel GPU on the machine this was written on (issue #17), so
//! every line below that needs one has never run. What *has* been exercised is
//! everything that does not: the configuration translation in `settings.rs`,
//! the runtime search in `api.rs` finding nothing and saying so, the refusal to
//! open on a non-Intel adapter, the Direct3D 11 half of the frame allocator in
//! [`allocator`] — which allocates and releases real textures on whatever GPU
//! the machine has — and detection reporting Quick Sync as unavailable.
//!
//! The rest is written from Intel's headers and documentation. Every statement
//! in this module about what a oneVPL runtime *does* is therefore read from a
//! document rather than measured, and is written that way: where a comment says
//! the header documents something, the header is quoted.
//! `docs/encoder-pipeline.md` lists which paths have never executed, and issue
//! [#160](https://github.com/wildware-uk/clipped/issues/160) is where they get
//! checked on real hardware.
//!
//! # How this reaches Quick Sync, and why
//!
//! Through oneVPL — Intel's current media interface, formerly the Media SDK —
//! loaded from the graphics driver at run time. Three routes exist and two were
//! rejected:
//!
//! | Route | Verdict |
//! | --- | --- |
//! | **oneVPL 2.x** (`mfxvideo.h`, `MFXInitialize`) | Chosen. It is the interface Intel currently develops, its headers are MIT-licensed and therefore redistributable as generated bindings, and it takes a Direct3D 11 texture the application owns without a copy. |
//! | **Media SDK 1.x** (`MFXInitEx`) | Rejected. Intel declared it legacy in 2023 and the oneVPL runtime supersedes it on every GPU that can encode AV1. Driving both would double the surface that cannot be tested here for hardware nobody is asked to buy. A machine with only a 1.x runtime is told to update its driver, by name. |
//! | **Media Foundation's Intel transform** | Rejected for the reason the NVENC backend rejected the NVIDIA one: the hardware transforms take NV12 and captured frames are BGRA, so every frame would need a colour conversion pass before it reached the encoder. oneVPL takes the BGRA surface and converts inside the encoder. |
//!
//! There is no SDK to install and nothing to link: the interface is a set of
//! headers, and the implementation is a DLL the Intel graphics driver puts in
//! System32. `sys.rs` holds bindings generated from those headers — Intel
//! publishes them under the MIT licence through
//! [libvpl](https://github.com/intel/libvpl), which is what makes them
//! redistributable at all (AGENTS.md sections 11 and 12).
//!
//! Clipped does not ship oneVPL's *dispatcher*, which is the piece that would
//! normally find the runtime and enumerate implementations. Doing the small
//! part of its job that is needed — load the driver's runtime, initialise a
//! session on the caller's Direct3D 11 device — costs a list of file names in
//! [`api::LIBRARIES`] and saves redistributing a binary.
//!
//! # Hybrid graphics
//!
//! A Quick Sync session encodes on the Intel adapter and on no other, and which
//! adapter it uses is decided entirely by the Direct3D 11 device the caller
//! hands over: `MFXVideoCORE_SetHandle` pins the session to it. There is no
//! adapter for this backend to choose.
//!
//! That makes the hybrid case — an Intel processor with a discrete GPU, where
//! the game renders on the discrete one — a decision for whoever creates the
//! capture device, not for the encoder. A texture captured from the discrete
//! GPU cannot be encoded by Quick Sync without a cross-adapter copy per frame,
//! which is the cost this whole interface exists to avoid (AGENTS.md section
//! 18). So [`QuickSyncEncoder::open`] refuses a device that is not on an Intel
//! adapter, by name, rather than failing somewhere inside the runtime; and
//! [`recommend`](crate::recommend) already prefers the discrete GPU's encoder on
//! exactly those machines. `docs/encoder-pipeline.md` describes the whole
//! behaviour.
//!
//! # The zero-copy path
//!
//! ```text
//! Windows Graphics Capture ─→ ID3D11Texture2D (BGRA, on the GPU)
//!                                   │
//!                     mfxFrameSurface1 { MemId → mfxHDLPair }
//!                                   │
//!                        MFXVideoENCODE_EncodeFrameAsync
//!                                   │
//!                         MFXVideoCORE_SyncOperation ─→ bytes for the muxer
//! ```
//!
//! The captured picture never enters system memory. The encoded bitstream does,
//! into a buffer this backend owns, and that is the point: at 2560x1440 a
//! captured frame is 14 MB and the encoded version of it is around 80 KB.
//!
//! What the diagram leaves out is the encoder's *own* surfaces — the
//! reconstructed pictures every inter-coded frame is predicted from — which
//! `mfxvideo.h` makes the application's allocator responsible for once one is
//! registered. [`allocator`] is where that is answered, and its documentation
//! is where the header is quoted.
//!
//! # Ownership
//!
//! [`QuickSyncEncoder`] owns the loaded runtime, the oneVPL session, the
//! encoder inside it, the frame allocator it registered — and therefore every
//! reconstruction surface allocated through it — and every output buffer. All
//! of it is released by [`Session::drop`], which runs from `shut_down` and from
//! an unwind, so a panic on the encoding thread cannot leak a session
//! (AGENTS.md section 58).
//!
//! It owns no *captured* texture. What oneVPL is given per frame is a surface
//! descriptor pointing at the caller's texture, and it lives inside a single
//! [`Session::submit`] call: describe, encode, wait for the coded picture, and
//! return — at which point the runtime holds nothing derived from the texture
//! and the capture backend may recycle it, which is what
//! [`crate::frame::SourceTexture`] promises it may. What makes that true on the
//! path where the runtime buffers a picture anyway is spelled out on
//! [`Session::submit`].

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
    clippy::pedantic,
    // The generated bitfield accessors are `unsafe` and carry no SAFETY
    // comments, because nothing writes them by hand. The workspace lint is
    // about code contributors write (AGENTS.md section 42); every `unsafe`
    // block in `api.rs`, `mod.rs` and `settings.rs` still has to justify
    // itself.
    clippy::undocumented_unsafe_blocks
)]
mod sys;

mod allocator;
mod api;
mod settings;

/// The libraries this backend loads oneVPL from, which is also the list
/// detection probes — see [`api::LIBRARIES`].
pub(super) use api::LIBRARIES;

#[cfg(test)]
mod tests;

use core::ffi::c_void;
use core::fmt;
use core::mem;
use core::ptr;
use core::time::Duration;
use std::collections::VecDeque;

use crate::backend::VideoEncoder;
use crate::codec::{Codec, EncoderKind, Resolution, Vendor};
use crate::config::{EncoderConfig, RateControl};
use crate::error::{EncodeContext, EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SurfaceKind};
use crate::packet::{EncodedPacket, PictureKind};

/// The kinds of graphics resource this backend can bind.
const SUPPORTED_SURFACES: &[SurfaceKind] = &[SurfaceKind::D3d11Texture2D];

/// How many encoded frames the session can hold before the caller has to drain
/// them.
///
/// The configuration produces exactly one packet per submission — no B-frames,
/// one frame in flight — so one would do. Four is the same kind of margin the
/// NVENC backend keeps, and each buffer is a system-memory allocation of about
/// the size of one second of video.
const OUTPUT_BUFFERS: usize = 4;

/// The smallest output buffer this backend will use, whatever the runtime
/// suggests.
///
/// `BufferSizeInKB` is sized for the configured bit rate, and a keyframe can be
/// several times the average frame. Half a megabyte is cheap insurance against
/// `MFX_ERR_NOT_ENOUGH_BUFFER` on the first picture of a recording.
const MINIMUM_OUTPUT_BYTES: usize = 512 * 1024;

/// How long to wait for one coded picture before deciding the hardware is not
/// coming back.
///
/// A frame that takes five seconds means a hung GPU rather than a slow one; the
/// alternative is a recording thread that never returns.
const SYNC_TIMEOUT_MS: sys::mfxU32 = 5_000;

/// How many times to retry a submission the runtime says it is too busy for.
///
/// `MFX_WRN_DEVICE_BUSY` is oneVPL asking to be called again "in a few
/// milliseconds", and it is a normal condition when the GPU is loaded — which,
/// for a recorder running alongside a game, is always. A hundred attempts a
/// millisecond apart is a tenth of a second, well past the point where
/// something else is wrong.
const BUSY_RETRIES: u32 = 100;

/// How long to wait between those retries.
const BUSY_WAIT: Duration = Duration::from_millis(1);

/// A hardware encoding session on an Intel GPU.
///
/// Opened with [`open`](Self::open) against the Direct3D 11 device whose
/// textures will be submitted, and used through the
/// [`VideoEncoder`](crate::VideoEncoder) trait.
///
/// ```no_run
/// use core::time::Duration;
/// use clipped_encoder::{
///     BitRate, Codec, EncoderConfig, FrameRate, GraphicsDevice, QuickSyncEncoder, RateControl,
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
/// let mut encoder = QuickSyncEncoder::open(&device, config)?;
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
pub struct QuickSyncEncoder {
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
// What crosses the boundary is a set of raw pointers and one COM reference: the
// oneVPL session, the module handle, the allocator this backend registered, and
// inside that allocator the caller's Direct3D 11 device and the textures it
// created. None of them is thread-bound. oneVPL sessions have no thread
// affinity — the library's own samples create a session on one thread and
// encode on another — a Direct3D 11 device and its resources are free-threaded,
// and a module handle belongs to the process.
//
// What is *not* claimed is that two threads may use one encoder at once. They
// may not, and nothing here makes that possible: `VideoEncoder` is not `Sync`,
// every method takes `&mut self`, and an `EncodedPacket` borrows the encoder it
// came from.
unsafe impl Send for QuickSyncEncoder {}

impl QuickSyncEncoder {
    /// Opens a session on `device`.
    ///
    /// Everything that can be checked is checked here rather than on the first
    /// frame: that the device is on an Intel adapter, that the runtime loads,
    /// that this GPU encodes this codec, and that it will take the surfaces on
    /// offer. A recording that cannot work should fail before the user believes
    /// it started (SPEC.md section 7).
    ///
    /// # Errors
    ///
    /// [`EncodeError`], naming the encoder, codec and resolution, for every one
    /// of those checks and for any failure to create the session.
    pub fn open(device: &GraphicsDevice, config: EncoderConfig) -> Result<Self, EncodeError> {
        let context =
            EncodeContext::new(EncoderKind::QuickSync, config.codec(), config.resolution());
        let fail = |kind| EncodeError::new(context, kind);

        // The configuration is checked before the device is touched, and
        // deliberately in that order: everything below this point dereferences
        // the handle the caller supplied, and a test for a bad configuration
        // should not need a graphics device to run.
        if device.kind() != DeviceKind::D3d11 {
            return Err(fail(EncodeErrorKind::Configuration {
                detail: format!(
                    "Quick Sync binds Direct3D 11 textures and was given a {} device",
                    device.kind()
                ),
            }));
        }
        validate(&config).map_err(&fail)?;

        if settings::fourcc(config.source_format()).is_none() {
            return Err(fail(EncodeErrorKind::SurfaceFormatUnsupported {
                offered: config.source_format(),
                supported: settings::SUPPORTED_FORMATS,
            }));
        }

        if device.as_raw().is_null() {
            return Err(fail(EncodeErrorKind::Configuration {
                detail: "the graphics device is null".to_owned(),
            }));
        }
        check_adapter(device, &fail)?;

        if let RateControl::Quality {
            ceiling: Some(ceiling),
            target,
        } = config.rate_control()
        {
            // Not silently dropped (AGENTS.md section 15): Quick Sync's
            // constant-quality mode spends what the quality needs and has no
            // field to put a ceiling in, so the recording will be larger than
            // the caller asked for and the log is where that is explained.
            tracing::warn!(
                encoder = %EncoderKind::QuickSync.log_encoder_family(),
                %ceiling,
                %target,
                "Quick Sync's constant-quality mode has no bit rate ceiling, so the configured \
                 ceiling is not applied"
            );
        }

        let runtime = api::QuickSyncRuntime::load().map_err(|failure| {
            fail(EncodeErrorKind::RuntimeUnavailable {
                library: api::LIBRARY_LIST,
                detail: failure.to_string(),
            })
        })?;

        let mut session = Session::open(runtime, device.as_raw(), context)?;
        session.check_support(&config)?;
        session.initialise(&config)?;
        let parameter_sets = session.parameter_sets(config.codec())?;
        session.create_output_buffers()?;

        tracing::info!(
            encoder = %EncoderKind::QuickSync.log_encoder_family(),
            codec = config.codec().log_value(),
            resolution = %config.resolution(),
            frame_rate = %config.frame_rate(),
            rate_control = %config.rate_control(),
            preset = %config.preset(),
            library = session.runtime.library(),
            parameter_set_bytes = parameter_sets.len(),
            "opened a Quick Sync session"
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

impl fmt::Debug for QuickSyncEncoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuickSyncEncoder")
            .field("config", &self.config)
            .field("running", &self.session.is_some())
            .field("finished", &self.finished)
            .finish()
    }
}

impl Drop for QuickSyncEncoder {
    fn drop(&mut self) {
        self.shut_down();
    }
}

impl VideoEncoder for QuickSyncEncoder {
    fn encoder(&self) -> EncoderKind {
        EncoderKind::QuickSync
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

        self.session()?.submit(frame)?;
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
                encoder = %EncoderKind::QuickSync.log_encoder_family(),
                codec = self.config.codec().log_value(),
                frames,
                "closed the Quick Sync session"
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
    // an odd dimension has no representation. oneVPL would refuse it with
    // MFX_ERR_INVALID_VIDEO_PARAM, several calls later and without saying which
    // parameter.
    if width % 2 != 0 || height % 2 != 0 {
        return Err(EncodeErrorKind::Configuration {
            detail: format!(
                "{width}x{height} has an odd dimension, and 4:2:0 chroma needs both to be even"
            ),
        });
    }
    // `mfxFrameInfo` counts pixels in 16 bits, and the surface it describes is
    // the picture rounded up to a multiple of 16.
    if width > u32::from(u16::MAX) - 16 || height > u32::from(u16::MAX) - 16 {
        return Err(EncodeErrorKind::Configuration {
            detail: format!("{width}x{height} is larger than oneVPL can describe"),
        });
    }
    Ok(())
}

/// Refuses a device that is not on an Intel adapter.
///
/// See the module documentation, "Hybrid graphics": this is the whole of the
/// backend's adapter selection, because the caller's device *is* the choice.
fn check_adapter(
    device: &GraphicsDevice,
    fail: &impl Fn(EncodeErrorKind) -> EncodeError,
) -> Result<(), EncodeError> {
    // SAFETY: the caller of `open` guarantees the handle is a live
    // `ID3D11Device` it owns, and this function borrows it for the length of
    // the call without releasing it.
    let vendor = unsafe { crate::windows::dxgi::device_vendor(device.as_raw()) };

    match vendor {
        Some(Vendor::Intel) => Ok(()),
        Some(other) => Err(fail(EncodeErrorKind::Configuration {
            detail: format!(
                "Quick Sync encodes on Intel graphics and this device was created on a {other} \
                 adapter; capture on the Intel adapter to encode with Quick Sync, or use that \
                 adapter's own encoder"
            ),
        })),
        None => Err(fail(EncodeErrorKind::Configuration {
            detail: "the graphics device did not answer as a DXGI device, so the adapter it was \
                     created on could not be identified"
                .to_owned(),
        })),
    }
}

/// One coded picture, waiting in the output buffer that holds it.
#[derive(Debug, Clone, Copy)]
struct Coded {
    /// Which of [`Session::outputs`] the bytes are in.
    output: usize,
    /// Where the bytes start in that buffer. oneVPL is free to write at an
    /// offset.
    offset: usize,
    /// How many bytes there are.
    length: usize,
    presentation: Duration,
    picture: PictureKind,
}

/// The live oneVPL session and everything it owns.
struct Session {
    runtime: api::QuickSyncRuntime,
    session: sys::mfxSession,
    context: EncodeContext,
    /// Whether `MFXVideoENCODE_Init` has run, which is what decides whether
    /// teardown has an encoder to close.
    encoder_open: bool,
    /// The allocator oneVPL was given, which it may keep a pointer to for the
    /// life of the session, and which owns every surface it allocated through
    /// it.
    allocator: allocator::FrameAllocator,
    /// What each submitted surface says about itself, built once from the
    /// configuration.
    frame_info: sys::mfxFrameInfo,
    /// Every output buffer, whether free or holding a picture.
    outputs: Vec<Vec<u8>>,
    /// The output buffers not currently holding a coded picture.
    free: Vec<usize>,
    /// Coded and waiting to be handed over.
    ready: VecDeque<Coded>,
    /// The packet the caller is currently holding, which the next call
    /// releases.
    locked: Option<Coded>,
    encoded_frames: u64,
}

impl Session {
    /// Initialises a session against a Direct3D 11 device.
    fn open(
        runtime: api::QuickSyncRuntime,
        device: *mut c_void,
        context: EncodeContext,
    ) -> Result<Self, EncodeError> {
        // SAFETY: plain data, and oneVPL requires everything it is not told
        // about to be zero.
        let mut params: sys::mfxInitializationParam = unsafe { mem::zeroed() };
        params.AccelerationMode = sys::MFX_ACCEL_MODE_VIA_D3D11;

        let mut session: sys::mfxSession = ptr::null_mut();
        // SAFETY: `params` is passed by value, as the header declares, and
        // `session` is a live local out-parameter.
        let status = unsafe { (runtime.functions().MFXInitialize)(params, &raw mut session) };
        if status != sys::MFX_ERR_NONE {
            return Err(EncodeError::new(
                context,
                classify(status, "MFXInitialize", String::new()),
            ));
        }
        if session.is_null() {
            return Err(EncodeError::new(
                context,
                EncodeErrorKind::Api {
                    operation: "MFXInitialize",
                    status,
                    status_name: api::status_name(status),
                    detail: "the runtime reported success and produced no session".to_owned(),
                },
            ));
        }

        // SAFETY: plain data; filled in by `initialise` before any frame is
        // submitted.
        let frame_info: sys::mfxFrameInfo = unsafe { mem::zeroed() };

        // SAFETY: `device` is the caller's `ID3D11Device`, which
        // `QuickSyncEncoder::open` has checked is non-null and answers as a
        // DXGI device on an Intel adapter, and which the caller guarantees
        // outlives the encoder. The allocator takes a counted reference of its
        // own, so the textures it creates cannot outlive the device even if the
        // caller breaks that promise.
        let allocator = match unsafe { allocator::FrameAllocator::new(device) } {
            Ok(allocator) => allocator,
            Err(detail) => {
                // The session is open and nothing owns it yet, so it is closed
                // here rather than leaked.
                // SAFETY: the session was created above and is closed exactly
                // once, here.
                let status = unsafe { (runtime.functions().MFXClose)(session) };
                log_release_failure(status, "MFXClose");
                return Err(EncodeError::new(
                    context,
                    EncodeErrorKind::Configuration { detail },
                ));
            }
        };

        // From here the session exists, so every failure has to close it, which
        // is what building `Self` before the remaining calls arranges: the
        // early return of a `?` below drops this local, and `Session::drop`
        // closes the session.
        let mut opened = Self {
            runtime,
            session,
            context,
            encoder_open: false,
            allocator,
            frame_info,
            outputs: Vec::new(),
            free: Vec::new(),
            ready: VecDeque::new(),
            locked: None,
            encoded_frames: 0,
        };

        opened.log_version();

        // SAFETY: the session is live and `device` is the caller's
        // `ID3D11Device`, which `QuickSyncEncoder::open` has checked is
        // non-null and on an Intel adapter, and which the caller guarantees
        // outlives the encoder.
        let status = unsafe {
            (opened.runtime.functions().MFXVideoCORE_SetHandle)(
                opened.session,
                sys::MFX_HANDLE_D3D11_DEVICE,
                device,
            )
        };
        opened.check(status, "MFXVideoCORE_SetHandle")?;

        let allocator = opened.allocator.interface();
        // SAFETY: the session is live and the allocator's callbacks are boxed,
        // so their address is stable for as long as this session is; the
        // allocator is dropped after `MFXClose` in `Drop`.
        let status = unsafe {
            (opened.runtime.functions().MFXVideoCORE_SetFrameAllocator)(opened.session, allocator)
        };
        opened.check(status, "MFXVideoCORE_SetFrameAllocator")?;

        Ok(opened)
    }

    /// Records which implementation answered, which is the first thing a bug
    /// report about Quick Sync needs.
    fn log_version(&self) {
        // SAFETY: plain data, and a live local out-parameter.
        let mut version: sys::mfxVersion = unsafe { mem::zeroed() };
        // SAFETY: the session is live.
        let status =
            unsafe { (self.runtime.functions().MFXQueryVersion)(self.session, &raw mut version) };

        if status == sys::MFX_ERR_NONE {
            // SAFETY: the union's two members are the packed 32-bit version and
            // the major/minor pair that make it up, so either is valid to read
            // whichever was written.
            let (major, minor) = unsafe {
                (
                    version.__bindgen_anon_1.Major,
                    version.__bindgen_anon_1.Minor,
                )
            };
            tracing::debug!(
                library = self.runtime.library(),
                runtime_api = %format_args!("{major}.{minor}"),
                headers = %format_args!("{}.{}", api::API_MAJOR, api::API_MINOR),
                "opened an Intel media runtime"
            );
        } else {
            tracing::debug!(
                library = self.runtime.library(),
                status_name = api::status_name(status),
                "the Intel media runtime would not say which API version it implements"
            );
        }
    }

    /// Asks the runtime whether it can encode what it is about to be asked for.
    ///
    /// `MFXVideoENCODE_Query` is oneVPL's "would this work". `mfxvideo.h`
    /// documents it as answering with a corrected copy of the parameters in
    /// which every field the implementation cannot support has been zeroed,
    /// which is what the checks below read — the documented contract, not a
    /// measured one, because no runtime has answered this call here (issue
    /// #17). It is the only way to find out, before spending a session on it,
    /// whether this particular Intel generation encodes this codec and takes
    /// BGRA surfaces.
    fn check_support(&self, config: &EncoderConfig) -> Result<(), EncodeError> {
        let mut wanted = settings::video_params(config);
        // SAFETY: plain data; the runtime fills it in.
        let mut supported: sys::mfxVideoParam = unsafe { mem::zeroed() };

        // SAFETY: the session is live and both parameters are live locals of
        // the type the entry point expects. Neither carries an extension
        // buffer, so there is no pointer inside them for the call to follow.
        let status = unsafe {
            (self.runtime.functions().MFXVideoENCODE_Query)(
                self.session,
                &raw mut wanted,
                &raw mut supported,
            )
        };

        match status {
            sys::MFX_ERR_UNSUPPORTED => {
                return Err(EncodeError::new(
                    self.context,
                    EncodeErrorKind::CodecUnsupported,
                ))
            }
            sys::MFX_WRN_PARTIAL_ACCELERATION => {
                // The runtime is offering to do this on the CPU. That is not
                // Quick Sync, it takes the cores the game is using, and a user
                // who chose a hardware encoder did not ask for it (AGENTS.md
                // section 18); the software backend is a deliberate choice
                // somebody else makes.
                return Err(EncodeError::new(
                    self.context,
                    EncodeErrorKind::Configuration {
                        detail: "this Intel GPU cannot encode that in hardware, and the runtime \
                                 offered to do it on the processor instead"
                            .to_owned(),
                    },
                ));
            }
            sys::MFX_ERR_NONE | sys::MFX_WRN_INCOMPATIBLE_VIDEO_PARAM => {}
            other => return Err(self.error(other, "MFXVideoENCODE_Query")),
        }

        // SAFETY: `settings::video_params` writes the `mfx` member of the
        // union, the runtime answers in the same member, and nothing in this
        // backend reads the other one.
        let (codec_id, frame_info) = unsafe {
            let mfx = &supported.__bindgen_anon_1.mfx;
            (mfx.CodecId, mfx.FrameInfo)
        };

        if codec_id == 0 {
            return Err(EncodeError::new(
                self.context,
                EncodeErrorKind::CodecUnsupported,
            ));
        }
        if frame_info.FourCC == 0 {
            return Err(EncodeError::new(
                self.context,
                EncodeErrorKind::Configuration {
                    detail: "this Intel GPU's encoder does not take BGRA surfaces, and converting \
                             them to NV12 first is not implemented \
                             (https://github.com/wildware-uk/clipped/issues/160)"
                        .to_owned(),
                },
            ));
        }

        // SAFETY: the size member is the one `settings::frame_info` wrote and
        // the one the runtime answers in.
        let size = unsafe { frame_info.__bindgen_anon_1.__bindgen_anon_1 };
        if size.Width == 0 || size.Height == 0 {
            return Err(EncodeError::new(
                self.context,
                EncodeErrorKind::Configuration {
                    detail: format!(
                        "this Intel GPU's encoder does not accept a {} picture",
                        config.resolution()
                    ),
                },
            ));
        }

        if status == sys::MFX_WRN_INCOMPATIBLE_VIDEO_PARAM {
            tracing::debug!(
                encoder = %EncoderKind::QuickSync.log_encoder_family(),
                codec = config.codec().log_value(),
                "the Intel runtime adjusted parameters it could not honour exactly"
            );
        }
        Ok(())
    }

    /// Configures and starts the encoder.
    fn initialise(&mut self, config: &EncoderConfig) -> Result<(), EncodeError> {
        self.frame_info = settings::frame_info(config);

        let mut params = InitParams::new(config);
        // SAFETY: the session is live, and `params` is boxed and linked, so
        // every pointer inside the `mfxVideoParam` addresses an extension
        // buffer that outlives this call. oneVPL copies both the parameters and
        // the buffers during initialisation.
        let status =
            unsafe { (self.runtime.functions().MFXVideoENCODE_Init)(self.session, params.video()) };

        match status {
            sys::MFX_ERR_NONE => {}
            sys::MFX_WRN_INCOMPATIBLE_VIDEO_PARAM | sys::MFX_WRN_VIDEO_PARAM_CHANGED => {
                // The encoder started, having changed something it could not do
                // exactly as asked. Worth a line, not worth refusing: the
                // alternative is no recording at all (AGENTS.md section 16).
                tracing::info!(
                    encoder = %EncoderKind::QuickSync.log_encoder_family(),
                    codec = config.codec().log_value(),
                    status_name = api::status_name(status),
                    "the Intel runtime adjusted the encoder configuration it was given"
                );
            }
            other => return Err(self.error(other, "MFXVideoENCODE_Init")),
        }

        self.encoder_open = true;
        Ok(())
    }

    /// The sequence and picture parameter sets, for a container that stores
    /// them out of band.
    ///
    /// Empty for AV1: the 2.15 headers define no extension buffer that hands
    /// back a sequence header, and AV1 carries one in the bitstream at every
    /// keyframe anyway — a property of the format rather than of Intel's
    /// encoder — which is what `settings::av1_bitstream_param` keeps true by
    /// switching the IVF wrapper off.
    fn parameter_sets(&self, codec: Codec) -> Result<Vec<u8>, EncodeError> {
        if codec == Codec::Av1 {
            return Ok(Vec::new());
        }

        let mut request = ParameterSetRequest::new(codec);
        // SAFETY: the encoder is initialised, and `request` is boxed and
        // linked: the `mfxVideoParam` handed over points at extension buffers
        // that outlive the call, and each of those points at a byte buffer of
        // the size declared beside it.
        let status = unsafe {
            (self.runtime.functions().MFXVideoENCODE_GetVideoParam)(self.session, request.video())
        };
        self.check(status, "MFXVideoENCODE_GetVideoParam")?;

        Ok(request.take_parameter_sets())
    }

    /// Allocates the pool of output buffers, sized by what the encoder says one
    /// coded picture can be.
    fn create_output_buffers(&mut self) -> Result<(), EncodeError> {
        // SAFETY: plain data; the runtime fills it in.
        let mut params: sys::mfxVideoParam = unsafe { mem::zeroed() };
        // SAFETY: the encoder is initialised and `params` is a live local with
        // no extension buffers attached, so there is no pointer for the call to
        // follow.
        let status = unsafe {
            (self.runtime.functions().MFXVideoENCODE_GetVideoParam)(self.session, &raw mut params)
        };
        self.check(status, "MFXVideoENCODE_GetVideoParam")?;

        // SAFETY: the encoding member is the one this backend writes and the
        // one the runtime answers in.
        let (buffer_kb, multiplier) = unsafe {
            let mfx = &params.__bindgen_anon_1.mfx;
            (
                mfx.__bindgen_anon_1.__bindgen_anon_1.BufferSizeInKB,
                mfx.BRCParamMultiplier,
            )
        };

        let bytes = (usize::from(buffer_kb) * usize::from(multiplier.max(1)) * 1000)
            .max(MINIMUM_OUTPUT_BYTES);

        for index in 0..OUTPUT_BUFFERS {
            self.outputs.push(vec![0u8; bytes]);
            self.free.push(index);
        }

        tracing::debug!(
            buffers = OUTPUT_BUFFERS,
            bytes,
            "allocated the Quick Sync output buffers"
        );
        Ok(())
    }

    /// Encodes one frame and waits for it.
    ///
    /// The whole of the encoder's business with the caller's texture happens
    /// here, which is what the interface promises
    /// ([`crate::frame::SourceTexture`]): describe the surface, submit it, wait
    /// for the coded picture, and return. The capture backend may recycle the
    /// texture the moment this returns, and two different things make that safe
    /// on the two paths out:
    ///
    /// - On the ordinary path the picture is coded and synchronised before the
    ///   call returns, so there is nothing left to hold it.
    /// - On the path where the runtime buffered the picture instead —
    ///   [`deferred_picture`](Self::deferred_picture) — the surface's `Locked`
    ///   count is what decides. It is flushed, then the encoder is closed, then
    ///   the session is closed, and each step re-reads the count; whichever step
    ///   brings it to zero is where it stops. A session that is closed holds
    ///   nothing by construction, so the promise holds at the cost of the
    ///   recording rather than at the cost of the texture.
    ///
    /// `Locked` is oneVPL's own reference count on a surface, documented in
    /// `mfxstructures.h` as the number of operations that still hold it. No
    /// runtime has ever set it here (issue #17), so what is verified is that
    /// this code reads it and escalates on it, not that a real runtime clears
    /// it when the encoder closes.
    fn submit(&mut self, frame: &SourceFrame<'_>) -> Result<(), EncodeError> {
        if self.session.is_null() {
            // Only reachable after `deferred_picture` closed the session to
            // reclaim a texture: the encoder is finished, and every later call
            // says so rather than dereferencing a closed session.
            return Err(EncodeError::new(self.context, EncodeErrorKind::NotRunning));
        }

        let output = self.free.pop().ok_or_else(|| {
            EncodeError::new(
                self.context,
                EncodeErrorKind::OutputBuffersExhausted {
                    capacity: OUTPUT_BUFFERS,
                },
            )
        })?;

        // The descriptor, the handle pair it points at and the bitstream all
        // have to outlive the asynchronous call below, which is why they are
        // locals of this function and why nothing between here and the
        // synchronisation may return early.
        let mut handles = sys::mfxHDLPair {
            first: frame.texture().as_raw(),
            // The subresource index, which is zero for the single-slice
            // textures both capture backends produce.
            second: ptr::null_mut(),
        };
        let mut surface = self.surface_for(frame, &raw mut handles);
        let mut bitstream = bitstream_for(&mut self.outputs[output]);
        let mut control = control_for(frame);

        let coded = match self.encode(&mut surface, &mut bitstream, &mut control) {
            Ok(false) => self.collect(output, &bitstream, frame.presentation_time()),
            Ok(true) => Err(self.deferred_picture(&mut surface)),
            Err(error) => Err(error),
        };

        match coded {
            Ok(coded) => {
                self.encoded_frames += 1;
                self.ready.push_back(coded);
                Ok(())
            }
            Err(error) => {
                self.free.push(output);
                Err(error)
            }
        }
    }

    /// Describes one frame's texture to oneVPL.
    fn surface_for(
        &self,
        frame: &SourceFrame<'_>,
        handles: *mut sys::mfxHDLPair,
    ) -> sys::mfxFrameSurface1 {
        // SAFETY: plain data, and oneVPL requires everything it is not told
        // about to be zero. In particular `FrameInterface` — the first union
        // member — must be null on a surface the application owns, which is
        // what zeroing it makes it.
        let mut surface: sys::mfxFrameSurface1 = unsafe { mem::zeroed() };
        surface.Info = self.frame_info;
        surface.Data.MemId = handles.cast::<c_void>();
        surface.Data.TimeStamp = settings::timestamp_90khz(frame.presentation_time());
        surface.Data.FrameOrder = sys::MFX_FRAMEORDER_UNKNOWN as sys::mfxU32;
        surface
    }

    /// Hands one frame to the encoder, and says whether it was buffered rather
    /// than coded.
    ///
    /// `MFX_WRN_DEVICE_BUSY` is retried rather than reported: the header says
    /// to call again "in a few milliseconds", and a GPU busy with the game
    /// being recorded is the normal case rather than the exception.
    fn encode(
        &self,
        surface: &mut sys::mfxFrameSurface1,
        bitstream: &mut sys::mfxBitstream,
        control: &mut sys::mfxEncodeCtrl,
    ) -> Result<bool, EncodeError> {
        for attempt in 0..=BUSY_RETRIES {
            let mut sync: sys::mfxSyncPoint = ptr::null_mut();
            // SAFETY: the encoder is initialised, and every pointer is a live
            // local of the caller's that outlives the synchronisation below:
            // the surface descriptor, the output bitstream and the per-frame
            // control block. The surface's `MemId` addresses the handle pair in
            // `submit`, which outlives this call for the same reason.
            let status = unsafe {
                (self.runtime.functions().MFXVideoENCODE_EncodeFrameAsync)(
                    self.session,
                    &raw mut *control,
                    &raw mut *surface,
                    &raw mut *bitstream,
                    &raw mut sync,
                )
            };

            match status {
                sys::MFX_ERR_NONE if !sync.is_null() => {
                    self.synchronise(sync, "MFXVideoENCODE_EncodeFrameAsync")?;
                    return Ok(false);
                }
                // A success with no synchronisation point means the runtime
                // took the frame and produced nothing to wait for, which is the
                // same situation as `MFX_ERR_MORE_DATA` and is handled the same
                // way.
                sys::MFX_ERR_NONE | sys::MFX_ERR_MORE_DATA => return Ok(true),
                sys::MFX_WRN_DEVICE_BUSY => {
                    if attempt == BUSY_RETRIES {
                        return Err(self.error(status, "MFXVideoENCODE_EncodeFrameAsync"));
                    }
                    std::thread::sleep(BUSY_WAIT);
                }
                other => return Err(self.error(other, "MFXVideoENCODE_EncodeFrameAsync")),
            }
        }

        // The loop above returns on every path; this is unreachable and is
        // written as an error rather than a panic because a panic on the
        // encoding thread would take the recording with it.
        Err(self.error(sys::MFX_ERR_UNKNOWN, "MFXVideoENCODE_EncodeFrameAsync"))
    }

    /// Deals with a picture oneVPL decided to buffer rather than code.
    ///
    /// The encoder is configured so that it cannot: no B-frames and one frame
    /// in flight, precisely so that a borrowed texture never has to stay
    /// available past `submit`. Reaching here means the runtime buffered a
    /// picture in a configuration that forbids it, and it is holding a
    /// reference to a texture the caller is about to reuse.
    ///
    /// So the stream is flushed until the surface is released, and the failure
    /// is reported; the flushed pictures are discarded, because `submit`
    /// promises a packet per frame through `next_packet` and half a frame of
    /// video is not worth an interface that can hand back pictures out of
    /// order.
    ///
    /// Each step re-reads `Data.Locked`, which is the runtime's own count of
    /// how many operations still hold the surface, and stops at the first step
    /// that brings it to zero:
    ///
    /// 1. flush the encoder;
    /// 2. close the encoder, which by `MFXVideoENCODE_Close`'s contract ends
    ///    every operation it started;
    /// 3. close the session, which destroys the runtime state entirely.
    ///
    /// The escalation exists because step 2's contract is read from the header
    /// rather than measured — no runtime has ever reached this branch here
    /// (issue #17) — and `submit` promises the caller a texture nothing is
    /// holding. A recording is worth less than the corruption or the device
    /// removal that recycling a texture the GPU is still reading would cause.
    fn deferred_picture(&mut self, surface: &mut sys::mfxFrameSurface1) -> EncodeError {
        let mut discarded = 0;
        while surface.Data.Locked != 0 && discarded < OUTPUT_BUFFERS {
            match self.flush_one() {
                Ok(Some(())) => discarded += 1,
                Ok(None) | Err(_) => break,
            }
        }

        if surface.Data.Locked != 0 {
            tracing::error!(
                encoder = %EncoderKind::QuickSync.log_encoder_family(),
                locked = surface.Data.Locked,
                "the Intel runtime is still holding a frame the caller owns, so the encoder is \
                 being closed to release it"
            );
            self.close_encoder();
        }

        if surface.Data.Locked != 0 {
            // Closing the encoder did not do it. Nothing weaker is left, and
            // reporting a failure while the runtime still holds the caller's
            // texture would hand back a texture the GPU may still be reading.
            tracing::error!(
                encoder = %EncoderKind::QuickSync.log_encoder_family(),
                locked = surface.Data.Locked,
                "the Intel runtime still holds the frame after MFXVideoENCODE_Close, so the whole \
                 session is being closed (https://github.com/wildware-uk/clipped/issues/160)"
            );
            self.close_session();
        }

        EncodeError::new(
            self.context,
            EncodeErrorKind::Api {
                operation: "MFXVideoENCODE_EncodeFrameAsync",
                status: sys::MFX_ERR_MORE_DATA,
                status_name: api::status_name(sys::MFX_ERR_MORE_DATA),
                detail: "the encoder buffered the picture, which it must not do with B-frames \
                         disabled and one frame in flight, because the input texture belongs to \
                         the caller and is released when this call returns"
                    .to_owned(),
            },
        )
    }

    /// Waits for one asynchronous operation.
    fn synchronise(
        &self,
        sync: sys::mfxSyncPoint,
        operation: &'static str,
    ) -> Result<(), EncodeError> {
        // SAFETY: the session is live and `sync` is a synchronisation point it
        // produced and which has not been waited on.
        let status = unsafe {
            (self.runtime.functions().MFXVideoCORE_SyncOperation)(
                self.session,
                sync,
                SYNC_TIMEOUT_MS,
            )
        };

        match status {
            sys::MFX_ERR_NONE => Ok(()),
            sys::MFX_WRN_IN_EXECUTION => Err(EncodeError::new(
                self.context,
                EncodeErrorKind::Api {
                    operation,
                    status,
                    status_name: api::status_name(status),
                    detail: format!(
                        "the picture was not coded within {SYNC_TIMEOUT_MS} ms, which means the \
                         graphics device has stopped responding"
                    ),
                },
            )),
            other => Err(self.error(other, "MFXVideoCORE_SyncOperation")),
        }
    }

    /// Records where a coded picture landed.
    fn collect(
        &self,
        output: usize,
        bitstream: &sys::mfxBitstream,
        presentation: Duration,
    ) -> Result<Coded, EncodeError> {
        let offset = bitstream.DataOffset as usize;
        let length = bitstream.DataLength as usize;

        if offset.saturating_add(length) > self.outputs[output].len() {
            return Err(EncodeError::new(
                self.context,
                EncodeErrorKind::Api {
                    operation: "MFXVideoENCODE_EncodeFrameAsync",
                    status: sys::MFX_ERR_NOT_ENOUGH_BUFFER,
                    status_name: api::status_name(sys::MFX_ERR_NOT_ENOUGH_BUFFER),
                    detail: format!(
                        "the runtime reported {length} bytes at offset {offset} in a buffer of {}",
                        self.outputs[output].len()
                    ),
                },
            ));
        }

        Ok(Coded {
            output,
            offset,
            length,
            // The presentation time the caller submitted, rather than the one
            // that came back: oneVPL counts in 90 kHz ticks and the recording
            // counts in nanoseconds, and with pictures coded in submission
            // order — no B-frames — the frame that went in is the packet that
            // came out.
            presentation,
            picture: settings::picture_kind(bitstream.FrameType),
        })
    }

    /// Takes the next coded packet, releasing whatever the caller was holding.
    fn next_packet(&mut self) -> Result<Option<EncodedPacket<'_>>, EncodeError> {
        if let Some(previous) = self.locked.take() {
            self.free.push(previous.output);
        }

        let Some(coded) = self.ready.pop_front() else {
            return Ok(None);
        };
        self.locked = Some(coded);

        let data = &self.outputs[coded.output][coded.offset..coded.offset + coded.length];

        Ok(Some(EncodedPacket::new(
            data,
            coded.presentation,
            // Equal to the presentation time by construction: the encoder is
            // configured without B-frames, so pictures come out in the order
            // they went in (see `settings::video_params`).
            coded.presentation,
            coded.picture,
        )))
    }

    /// Flushes the encoder at the end of the stream.
    ///
    /// Every picture has already been coded and waited for by the `submit` that
    /// produced it, so in practice this finds nothing. It is written as a
    /// proper drain anyway, because "in practice" is a property of the runtime
    /// rather than of this code.
    fn finish(&mut self) -> Result<(), EncodeError> {
        if self.session.is_null() {
            // As in `submit`: the session was closed to reclaim a texture, and
            // there is nothing left to flush.
            return Err(EncodeError::new(self.context, EncodeErrorKind::NotRunning));
        }

        loop {
            let Some(output) = self.free.pop() else {
                // Every buffer is holding a picture the caller has not drained,
                // so there is nowhere to put another one. Whatever the encoder
                // is still holding comes out of the next `next_packet` loop.
                return Ok(());
            };

            let mut bitstream = bitstream_for(&mut self.outputs[output]);
            match self.drain(&mut bitstream) {
                Ok(Some(())) => {
                    // A flushed picture has no submitted frame left to pair
                    // with — one packet per submission is the contract — so it
                    // carries the timestamp the runtime reported.
                    let presentation = settings::duration_from_90khz(bitstream.TimeStamp)
                        .unwrap_or(Duration::ZERO);
                    match self.collect(output, &bitstream, presentation) {
                        Ok(coded) => {
                            self.encoded_frames += 1;
                            self.ready.push_back(coded);
                        }
                        Err(error) => {
                            self.free.push(output);
                            return Err(error);
                        }
                    }
                }
                Ok(None) => {
                    self.free.push(output);
                    return Ok(());
                }
                Err(error) => {
                    self.free.push(output);
                    return Err(error);
                }
            }
        }
    }

    /// Asks the encoder for one buffered picture, discarding it.
    ///
    /// Used only by [`deferred_picture`](Self::deferred_picture), where the
    /// point is to get the caller's texture back rather than to keep the video.
    fn flush_one(&mut self) -> Result<Option<()>, EncodeError> {
        let Some(output) = self.free.pop() else {
            return Ok(None);
        };
        let mut bitstream = bitstream_for(&mut self.outputs[output]);
        let drained = self.drain(&mut bitstream);
        self.free.push(output);
        drained
    }

    /// One flush step: a submission with no input, which is how oneVPL is told
    /// to produce what it is holding.
    fn drain(&self, bitstream: &mut sys::mfxBitstream) -> Result<Option<()>, EncodeError> {
        for attempt in 0..=BUSY_RETRIES {
            let mut sync: sys::mfxSyncPoint = ptr::null_mut();
            // SAFETY: the encoder is initialised; a null control block and a
            // null surface are what the header documents as "no more input",
            // and the bitstream is a live local of the caller's that outlives
            // the synchronisation below.
            let status = unsafe {
                (self.runtime.functions().MFXVideoENCODE_EncodeFrameAsync)(
                    self.session,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &raw mut *bitstream,
                    &raw mut sync,
                )
            };

            match status {
                sys::MFX_ERR_NONE if !sync.is_null() => {
                    self.synchronise(sync, "MFXVideoENCODE_EncodeFrameAsync (flush)")?;
                    return Ok(Some(()));
                }
                sys::MFX_ERR_NONE | sys::MFX_ERR_MORE_DATA => return Ok(None),
                sys::MFX_WRN_DEVICE_BUSY => {
                    if attempt == BUSY_RETRIES {
                        return Err(self.error(status, "MFXVideoENCODE_EncodeFrameAsync (flush)"));
                    }
                    std::thread::sleep(BUSY_WAIT);
                }
                other => return Err(self.error(other, "MFXVideoENCODE_EncodeFrameAsync (flush)")),
            }
        }

        Err(self.error(
            sys::MFX_ERR_UNKNOWN,
            "MFXVideoENCODE_EncodeFrameAsync (flush)",
        ))
    }

    /// Closes the encoder, leaving the session open.
    fn close_encoder(&mut self) {
        if !self.encoder_open || self.session.is_null() {
            return;
        }
        self.encoder_open = false;

        // SAFETY: the encoder was initialised by `initialise` and is closed
        // exactly once, here.
        let status = unsafe { (self.runtime.functions().MFXVideoENCODE_Close)(self.session) };
        log_release_failure(status, "MFXVideoENCODE_Close");
    }

    /// Closes the encoder and the session behind it.
    ///
    /// Idempotent, and what [`Drop`] does;
    /// [`deferred_picture`](Self::deferred_picture) also reaches for it early,
    /// to reclaim a texture nothing weaker would release. Every later call
    /// finds a null session and reports [`EncodeErrorKind::NotRunning`].
    fn close_session(&mut self) {
        self.close_encoder();

        if !self.session.is_null() {
            // SAFETY: the session was created by `Session::open` and is closed
            // exactly once — the pointer is nulled below — after the encoder
            // inside it.
            let status = unsafe { (self.runtime.functions().MFXClose)(self.session) };
            log_release_failure(status, "MFXClose");
            self.session = ptr::null_mut();
        }
    }

    /// Turns a status into `Ok(())` or an error carrying this session's
    /// context.
    fn check(&self, status: sys::mfxStatus, operation: &'static str) -> Result<(), EncodeError> {
        if status == sys::MFX_ERR_NONE {
            Ok(())
        } else {
            Err(self.error(status, operation))
        }
    }

    /// Builds an error from a status.
    fn error(&self, status: sys::mfxStatus, operation: &'static str) -> EncodeError {
        EncodeError::new(self.context, classify(status, operation, String::new()))
    }
}

impl Drop for Session {
    /// Releases everything, in the order the runtime requires.
    ///
    /// Runs from `QuickSyncEncoder::shut_down` and from an unwind, and releases
    /// through [`close_session`](Session::close_session), which is idempotent —
    /// so a session `deferred_picture` already closed is not closed twice
    /// (AGENTS.md section 58).
    fn drop(&mut self) {
        // Nothing derived from a caller's texture can be outstanding here:
        // `submit` returns only once the picture is coded, or having closed
        // whatever was still holding the surface (see `deferred_picture`). What
        // is left is the encoder, the session, and the module behind them.
        self.close_session();

        // The allocator and the runtime are dropped after this, by field order:
        // the allocator's callbacks and the reconstruction surfaces it owns,
        // and the module's code, all have to outlive the session that could
        // still call into them.
    }
}

/// The parameters `MFXVideoENCODE_Init` is given, and the extension buffers
/// they point at.
///
/// Boxed and linked together in one place because the `mfxVideoParam` holds
/// pointers into the other fields: the box is what keeps those pointers valid,
/// and building the whole thing here is what keeps the unsafety in one
/// reviewable function rather than at every call site.
struct InitParams {
    inner: Box<InitParamsInner>,
}

/// The structures [`InitParams`] owns. Never moved once linked.
struct InitParamsInner {
    video: sys::mfxVideoParam,
    signal: sys::mfxExtVideoSignalInfo,
    options: sys::mfxExtCodingOption2,
    av1: sys::mfxExtAV1BitstreamParam,
    pointers: Vec<*mut sys::mfxExtBuffer>,
}

impl InitParams {
    /// Builds the parameters for `config` and links the extension buffers to
    /// them.
    fn new(config: &EncoderConfig) -> Self {
        let mut inner = Box::new(InitParamsInner {
            video: settings::video_params(config),
            signal: settings::video_signal_info(config.colour_space()),
            options: settings::coding_option2(),
            av1: settings::av1_bitstream_param(),
            pointers: Vec::new(),
        });

        // The pointers are taken after the box exists, so each addresses a
        // field at its final location.
        let mut pointers = vec![(&raw mut inner.signal).cast::<sys::mfxExtBuffer>()];
        match config.codec() {
            // `mfxExtCodingOption2` is attached for H.264 and for nothing else:
            // the only field this backend sets in it, `RepeatPPS`, is
            // documented as an AVC control, and attaching a buffer an encoder
            // has no use for is a plausible `MFX_ERR_UNSUPPORTED` from
            // `MFXVideoENCODE_Init` for no gain (see `settings::coding_option2`
            // for what covers the other two codecs).
            Codec::H264 => pointers.push((&raw mut inner.options).cast::<sys::mfxExtBuffer>()),
            Codec::Av1 => pointers.push((&raw mut inner.av1).cast::<sys::mfxExtBuffer>()),
            Codec::Hevc => {}
        }
        inner.pointers = pointers;

        inner.video.NumExtParam = inner.pointers.len() as sys::mfxU16;
        inner.video.ExtParam = inner.pointers.as_mut_ptr();

        Self { inner }
    }

    /// The parameters, for handing to oneVPL.
    fn video(&mut self) -> *mut sys::mfxVideoParam {
        &raw mut self.inner.video
    }
}

/// The request `MFXVideoENCODE_GetVideoParam` is given when the parameter sets
/// are wanted, and the storage it writes them into.
struct ParameterSetRequest {
    inner: Box<ParameterSetRequestInner>,
    codec: Codec,
}

/// The structures [`ParameterSetRequest`] owns. Never moved once linked.
struct ParameterSetRequestInner {
    video: sys::mfxVideoParam,
    spspps: sys::mfxExtCodingOptionSPSPPS,
    vps: sys::mfxExtCodingOptionVPS,
    sps_bytes: Vec<u8>,
    pps_bytes: Vec<u8>,
    vps_bytes: Vec<u8>,
    pointers: Vec<*mut sys::mfxExtBuffer>,
}

/// How much room to give each parameter set.
///
/// A sequence parameter set is tens of bytes; 1024 is what Intel's own samples
/// reserve and is far more than any 8-bit 4:2:0 stream produces.
const PARAMETER_SET_CAPACITY: usize = 1024;

impl ParameterSetRequest {
    /// Builds an empty request with buffers for the runtime to fill in.
    fn new(codec: Codec) -> Self {
        // SAFETY: plain data; the runtime fills in everything but the extension
        // buffer pointers set below.
        let video: sys::mfxVideoParam = unsafe { mem::zeroed() };
        // SAFETY: plain data, as `settings::coding_option_vps` would argue.
        let mut vps: sys::mfxExtCodingOptionVPS = unsafe { mem::zeroed() };
        vps.Header = sys::mfxExtBuffer {
            BufferId: sys::MFX_EXTBUFF_CODING_OPTION_VPS as sys::mfxU32,
            BufferSz: mem::size_of::<sys::mfxExtCodingOptionVPS>() as sys::mfxU32,
        };

        let mut inner = Box::new(ParameterSetRequestInner {
            video,
            spspps: settings::parameter_set_request(),
            vps,
            sps_bytes: vec![0u8; PARAMETER_SET_CAPACITY],
            pps_bytes: vec![0u8; PARAMETER_SET_CAPACITY],
            vps_bytes: vec![0u8; PARAMETER_SET_CAPACITY],
            pointers: Vec::new(),
        });

        inner.spspps.SPSBuffer = inner.sps_bytes.as_mut_ptr();
        inner.spspps.SPSBufSize = PARAMETER_SET_CAPACITY as sys::mfxU16;
        inner.spspps.PPSBuffer = inner.pps_bytes.as_mut_ptr();
        inner.spspps.PPSBufSize = PARAMETER_SET_CAPACITY as sys::mfxU16;
        inner.vps.VPSBufSize = PARAMETER_SET_CAPACITY as sys::mfxU16;
        // The union's members are the buffer pointer and a reserved integer of
        // the same size. Writing one is safe — only reading the member that was
        // not written would not be — and the pointer is the member oneVPL reads
        // for this buffer identifier.
        inner.vps.__bindgen_anon_1.VPSBuffer = inner.vps_bytes.as_mut_ptr();

        let mut pointers = vec![(&raw mut inner.spspps).cast::<sys::mfxExtBuffer>()];
        if codec == Codec::Hevc {
            // HEVC has a video parameter set in front of the sequence one, and
            // a decoder handed the stream without it cannot start.
            pointers.push((&raw mut inner.vps).cast::<sys::mfxExtBuffer>());
        }
        inner.pointers = pointers;

        inner.video.NumExtParam = inner.pointers.len() as sys::mfxU16;
        inner.video.ExtParam = inner.pointers.as_mut_ptr();

        Self { inner, codec }
    }

    /// The request, for handing to oneVPL.
    fn video(&mut self) -> *mut sys::mfxVideoParam {
        &raw mut self.inner.video
    }

    /// The parameter sets the runtime wrote, in the order a decoder needs them.
    fn take_parameter_sets(self) -> Vec<u8> {
        let inner = self.inner;
        let mut sets = Vec::new();

        if self.codec == Codec::Hevc {
            let written = usize::from(inner.vps.VPSBufSize).min(inner.vps_bytes.len());
            sets.extend_from_slice(&inner.vps_bytes[..written]);
        }

        let sps = usize::from(inner.spspps.SPSBufSize).min(inner.sps_bytes.len());
        sets.extend_from_slice(&inner.sps_bytes[..sps]);
        let pps = usize::from(inner.spspps.PPSBufSize).min(inner.pps_bytes.len());
        sets.extend_from_slice(&inner.pps_bytes[..pps]);

        sets
    }
}

/// Points a bitstream at one of the session's output buffers.
fn bitstream_for(buffer: &mut [u8]) -> sys::mfxBitstream {
    // SAFETY: plain data — integers, a pointer and a union of reserved fields —
    // for which all-zeroes is a valid value, and oneVPL requires everything it
    // is not told about to be zero.
    let mut bitstream: sys::mfxBitstream = unsafe { mem::zeroed() };
    bitstream.Data = buffer.as_mut_ptr();
    bitstream.MaxLength = buffer.len() as sys::mfxU32;
    bitstream
}

/// The per-frame control block, which is where a forced keyframe is asked for.
fn control_for(frame: &SourceFrame<'_>) -> sys::mfxEncodeCtrl {
    // SAFETY: plain data, and zero means "no per-frame instructions".
    let mut control: sys::mfxEncodeCtrl = unsafe { mem::zeroed() };
    if frame.forces_keyframe() {
        // An IDR that is also a reference, which is what makes it a point a
        // clip can start from (SPEC.md section 7).
        control.FrameType =
            (sys::MFX_FRAMETYPE_I | sys::MFX_FRAMETYPE_IDR | sys::MFX_FRAMETYPE_REF) as sys::mfxU16;
    }
    control
}

/// Maps a oneVPL status onto the platform-neutral failure it represents.
fn classify(status: sys::mfxStatus, operation: &'static str, detail: String) -> EncodeErrorKind {
    match status {
        sys::MFX_ERR_MEMORY_ALLOC => EncodeErrorKind::OutOfMemory,
        sys::MFX_ERR_DEVICE_LOST | sys::MFX_ERR_DEVICE_FAILED | sys::MFX_ERR_GPU_HANG => {
            EncodeErrorKind::DeviceLost
        }
        sys::MFX_ERR_UNSUPPORTED | sys::MFX_ERR_NOT_IMPLEMENTED => {
            EncodeErrorKind::CodecUnsupported
        }
        sys::MFX_ERR_INVALID_VIDEO_PARAM | sys::MFX_ERR_INCOMPATIBLE_VIDEO_PARAM => {
            EncodeErrorKind::Configuration {
                detail: format!(
                    "{operation} refused the encoder configuration ({})",
                    api::status_name(status)
                ),
            }
        }
        _ => EncodeErrorKind::Api {
            operation,
            status,
            status_name: api::status_name(status),
            detail,
        },
    }
}

/// Logs a failure to release something, which is all a caller could do with it.
fn log_release_failure(status: sys::mfxStatus, operation: &'static str) {
    if status != sys::MFX_ERR_NONE {
        // Not silent (AGENTS.md section 15) and not fatal: teardown carries on
        // releasing the rest, because stopping half way would leak the
        // remainder.
        tracing::warn!(
            operation,
            status,
            status_name = api::status_name(status),
            "a Quick Sync resource could not be released"
        );
    }
}
