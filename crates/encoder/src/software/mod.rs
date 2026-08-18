//! The software fallback: encoding on the CPU, for a machine with no hardware
//! encoder this build can use.
//!
//! This is the encoder of last resort. SPEC.md section 9 asks for hardware
//! encoding to be preferred automatically, and [`recommend`](crate::recommend)
//! already ranks [`EncoderKind::Software`] behind every hardware encoder that is
//! available *and* has a backend proven to drive it — so this backend is what a
//! machine reaches when NVENC, AMF and Quick Sync are all absent, broken, or not
//! implemented yet.
//!
//! That last case is why "a working GPU encoder" is not the test. Quick Sync is
//! detected and reported wherever the silicon has it, but
//! [`EncoderKind::is_implemented`] rejects it until
//! [#160](https://github.com/wildware-uk/clipped/issues/160) verifies a backend
//! against Intel hardware, and `recommend` ranks a family it rejects below this
//! one. A machine whose only GPU is an Intel one is therefore pointed here with
//! its GPU encoder neither absent nor broken. See "Choosing an encoder to open"
//! in `docs/encoder-pipeline.md` for the rule and the reasoning behind it.
//!
//! # What it costs, and where
//!
//! A hardware encoder reads the capture backend's texture where it already
//! lives, and the picture never enters system memory. A CPU encoder cannot do
//! that, so every frame makes a round trip the hardware path does not:
//!
//! ```text
//! ID3D11Texture2D (BGRA, video memory)
//!         │  CopyResource + Map          ../readback.rs ← 14 MB per frame at 1440p
//!   BGRA bytes (system memory)                           and a CPU/GPU wait
//!         │  swscale                     convert.rs    ← reads 14 MB, writes 5.5 MB
//!   YUV 4:2:0 planes
//!         │  libopenh264                 avcodec.rs    ← the CPU cost proper
//!   coded packets
//! ```
//!
//! All three are measured rather than described. Each stage accumulates the
//! time spent in it, and closing the session logs the totals, so a support log
//! from a machine where recording made the game stutter says which stage the
//! time went into (AGENTS.md sections 18 and 19). The measurements taken on the
//! development machine are in `docs/encoder-pipeline.md`; the short version is
//! that at 2560x1440 this sustains a fraction of what NVENC does on the same
//! machine, and that the readback and the colour conversion together cost about
//! as much as the encoding.
//!
//! **This is expected to hurt.** A user on this path is recording with the CPU
//! that is also running their game, and no tuning here changes that. What this
//! backend can do is be conservative — at most four slices, no frame dropping,
//! no lookahead — and be honest, which is what the logging is for.
//!
//! # Which encoder, and the licence
//!
//! `libopenh264`, Cisco's BSD-2-Clause H.264 encoder, inside the pinned LGPL
//! FFmpeg build. **Not x264**: x264 is GPL, and linking it would relicense every
//! binary this project distributes
//! (`docs/adr/0004-ffmpeg-dependency-strategy.md`). See [`avcodec`] for the
//! whole of that argument.
//!
//! # Ownership
//!
//! [`SoftwareEncoder`] owns its codec session, its staging texture, its scaler
//! and its own reference to the graphics device, all released by `shut_down` or
//! by `Drop`, whichever comes first — both do the same work, so an unwind on the
//! encoding thread cannot leak any of it (AGENTS.md section 58).
//!
//! It owns no frame. The texture handed to [`submit`](VideoEncoder::submit) is
//! copied inside that call and nothing derived from it survives the call, so a
//! capture backend may recycle the surface the moment `submit` returns. That is
//! easier to keep true here than on the hardware path: a CPU encoder has the
//! pixels, not a reference to them.

mod avcodec;
mod convert;

#[cfg(test)]
mod tests;

use core::fmt;
use core::time::Duration;

use crate::backend::VideoEncoder;
use crate::codec::{EncoderKind, Resolution};
use crate::config::{EncoderConfig, SurfaceFormat};
use crate::error::{EncodeContext, EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SurfaceKind};
use crate::packet::EncodedPacket;

use self::avcodec::CodecSession;
use self::convert::Converter;
use crate::readback::{Readback, ReadbackError};

/// The kinds of graphics resource this backend can read.
const SUPPORTED_SURFACES: &[SurfaceKind] = &[SurfaceKind::D3d11Texture2D];

/// The frame layouts this backend can read.
const SUPPORTED_FORMATS: &[SurfaceFormat] = &[SurfaceFormat::Bgra8Unorm];

/// A software encoding session, on the CPU.
///
/// Opened with [`open`](Self::open) against the Direct3D 11 device whose
/// textures will be submitted — the device is needed even though the encoding
/// is not on it, because reading a frame out of video memory is a Direct3D
/// operation — and used through the [`VideoEncoder`](crate::VideoEncoder)
/// trait.
///
/// ```no_run
/// use core::time::Duration;
/// use clipped_encoder::{
///     BitRate, Codec, EncoderConfig, FrameRate, GraphicsDevice, RateControl, Resolution,
///     SoftwareEncoder, SourceFrame, SourceTexture, SurfaceFormat, VideoEncoder,
/// };
/// # fn example(device: GraphicsDevice, texture: *mut core::ffi::c_void)
/// # -> Result<(), clipped_encoder::EncodeError> {
/// let config = EncoderConfig::new(
///     Codec::H264,
///     Resolution::HD_1080P,
///     FrameRate::FPS_60,
///     RateControl::constant(BitRate::megabits_per_second(12)),
/// );
/// let mut encoder = SoftwareEncoder::open(&device, config)?;
///
/// // SAFETY: in a real pipeline this handle comes from a captured frame that
/// // is still alive; see `clipped_encoder::frame`.
/// let surface = unsafe {
///     SourceTexture::new(clipped_encoder::SurfaceKind::D3d11Texture2D, texture)
/// };
/// let frame = SourceFrame::new(
///     surface,
///     SurfaceFormat::Bgra8Unorm,
///     Resolution::HD_1080P,
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
pub struct SoftwareEncoder {
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

impl SoftwareEncoder {
    /// Opens a session that reads frames from `device` and encodes them on the
    /// CPU.
    ///
    /// Everything that can be checked is checked here rather than on the first
    /// frame: that the linked FFmpeg has the encoder, that the codec is one
    /// this backend produces, that the picture size is encodable, and that a
    /// staging texture of that size can be created. A recording that cannot
    /// work should fail before the user believes it started (SPEC.md section 7).
    ///
    /// # Errors
    ///
    /// [`EncodeError`], naming the encoder, codec and resolution, for every one
    /// of those checks and for any failure to build the session.
    pub fn open(device: &GraphicsDevice, config: EncoderConfig) -> Result<Self, EncodeError> {
        let context =
            EncodeContext::new(EncoderKind::Software, config.codec(), config.resolution());
        let fail = |kind| EncodeError::new(context, kind);

        if device.kind() != DeviceKind::D3d11 {
            return Err(fail(EncodeErrorKind::Configuration {
                detail: format!(
                    "frames are read out of Direct3D 11 textures, and this is a {} device",
                    device.kind()
                ),
            }));
        }
        if device.as_raw().is_null() {
            return Err(fail(EncodeErrorKind::Configuration {
                detail: "the graphics device is null".to_owned(),
            }));
        }
        if !avcodec::produces(config.codec()) {
            return Err(fail(EncodeErrorKind::CodecUnsupported));
        }
        if config.source_format() != SurfaceFormat::Bgra8Unorm {
            return Err(fail(EncodeErrorKind::SurfaceFormatUnsupported {
                offered: config.source_format(),
                supported: SUPPORTED_FORMATS,
            }));
        }
        validate(&config).map_err(&fail)?;

        let codec = CodecSession::open(&config).map_err(&fail)?;
        // SAFETY: `device` is a live Direct3D 11 device — that is what
        // `GraphicsDevice::new` requires of the handle inside it — and it
        // outlives every encoder opened against it, which is the same
        // requirement. The readback takes its own reference to it.
        let readback = unsafe { Readback::open(device.as_raw(), config.resolution()) }
            .map_err(|error| fail(readback_failure(error)))?;
        let converter =
            Converter::open(config.resolution(), config.colour_space()).map_err(|error| {
                fail(EncodeErrorKind::Configuration {
                    detail: format!("{}: {}", error.operation, error.detail),
                })
            })?;

        let parameter_sets = codec.parameter_sets().to_vec();
        tracing::info!(
            encoder = %EncoderKind::Software.log_encoder_family(),
            codec = config.codec().log_value(),
            resolution = %config.resolution(),
            frame_rate = %config.frame_rate(),
            rate_control = %config.rate_control(),
            preset = %config.preset(),
            library = %avcodec::ENCODER.to_string_lossy(),
            parameter_set_bytes = parameter_sets.len(),
            "opened a software encoding session; this encodes on the CPU and will cost the \
             recorded application performance"
        );

        Ok(Self {
            session: Some(Session {
                codec,
                readback,
                converter,
            }),
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

impl fmt::Debug for SoftwareEncoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SoftwareEncoder")
            .field("config", &self.config)
            .field("running", &self.session.is_some())
            .field("finished", &self.finished)
            .finish()
    }
}

impl Drop for SoftwareEncoder {
    fn drop(&mut self) {
        self.shut_down();
    }
}

impl VideoEncoder for SoftwareEncoder {
    fn encoder(&self) -> EncoderKind {
        EncoderKind::Software
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
                supported: SUPPORTED_FORMATS,
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

        let context = self.context;
        let session = self.session()?;
        session.take_picture(frame, context)?;
        session
            .codec
            .send(frame.presentation_time(), frame.forces_keyframe())
            .map_err(|kind| EncodeError::new(context, kind))?;

        self.last_presentation = Some(frame.presentation_time());
        Ok(())
    }

    fn next_packet(&mut self) -> Result<Option<EncodedPacket<'_>>, EncodeError> {
        let context = self.context;
        let coded = self
            .session()?
            .codec
            .receive()
            .map_err(|kind| EncodeError::new(context, kind))?;

        Ok(coded.map(|coded| {
            EncodedPacket::new(coded.data, coded.presentation, coded.decode, coded.picture)
        }))
    }

    fn finish(&mut self) -> Result<(), EncodeError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;

        let context = self.context;
        self.session()?
            .codec
            .flush()
            .map_err(|kind| EncodeError::new(context, kind))
    }

    fn shut_down(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };

        let (readback, frames) = session.readback.elapsed();
        let converted = session.converter.elapsed();
        let (encoded, _) = session.codec.elapsed();
        drop(session);

        let per_frame = |total: Duration| -> f64 {
            if frames == 0 {
                0.0
            } else {
                #[allow(clippy::cast_precision_loss)]
                {
                    total.as_secs_f64() * 1000.0 / frames as f64
                }
            }
        };
        let total = readback + converted + encoded;

        // Logged at `info` rather than `debug` because it is the answer to "why
        // did my game stutter while recording?", and a user reporting that
        // should not have to reproduce it with logging turned up (AGENTS.md
        // section 35).
        tracing::info!(
            encoder = %EncoderKind::Software.log_encoder_family(),
            codec = self.config.codec().log_value(),
            resolution = %self.config.resolution(),
            frames,
            readback_ms = per_frame(readback),
            convert_ms = per_frame(converted),
            encode_ms = per_frame(encoded),
            total_ms = per_frame(total),
            "closed the software encoding session"
        );
    }
}

/// Everything a live session owns.
///
/// Kept in one structure so that [`SoftwareEncoder::shut_down`] releases all of
/// it by dropping one value, in one place, on both the ordinary and the
/// unwinding path.
#[derive(Debug)]
struct Session {
    codec: CodecSession,
    readback: Readback,
    converter: Converter,
}

impl Session {
    /// Copies the frame out of video memory and converts it into the picture
    /// the encoder will code.
    ///
    /// The two halves are one function because they are one borrow: the pixels
    /// exist only while the staging texture is mapped, and the conversion is
    /// what reads them.
    fn take_picture(
        &mut self,
        frame: &SourceFrame<'_>,
        context: EncodeContext,
    ) -> Result<(), EncodeError> {
        let Self {
            codec,
            readback,
            converter,
        } = self;

        let picture = codec
            .writable_picture()
            .map_err(|kind| EncodeError::new(context, kind))?;

        let convert = |pixels: &[u8], stride: usize| {
            // SAFETY: `picture` is the session's frame, just made writable,
            // holding YUV 4:2:0 planes of the resolution the converter was
            // opened for; `pixels` and `stride` are what the mapping reported.
            unsafe { converter.convert(pixels, stride, picture) }
        };

        // SAFETY: the handle belongs to a live `SourceTexture`, whose lifetime
        // is tied to `frame`, so it is a live `ID3D11Texture2D` for the whole
        // of this call. Nothing derived from it is kept: `map` copies into the
        // staging texture and unmaps before it returns.
        let converted = unsafe { readback.map(frame.texture().as_raw(), convert) }
            .map_err(|error| EncodeError::new(context, readback_failure(error)))?;

        converted.map_err(|error| {
            EncodeError::new(
                context,
                EncodeErrorKind::Api {
                    operation: "sws_scale",
                    status: 0,
                    status_name: "a conversion failure",
                    detail: format!("{}: {}", error.operation, error.detail),
                },
            )
        })
    }
}

/// The configuration checks that need nothing but the configuration.
fn validate(config: &EncoderConfig) -> Result<(), EncodeErrorKind> {
    let Resolution { width, height } = config.resolution();

    if width == 0 || height == 0 {
        return Err(EncodeErrorKind::Configuration {
            detail: "a picture with no width or no height cannot be encoded".to_owned(),
        });
    }
    // H.264 samples chroma at half resolution in both directions, so an odd
    // dimension has no representation. `libopenh264` refuses it too, several
    // calls later and without saying which parameter.
    if width % 2 != 0 || height % 2 != 0 {
        return Err(EncodeErrorKind::Configuration {
            detail: format!(
                "{width}x{height} has an odd dimension, and 4:2:0 chroma needs both to be even"
            ),
        });
    }
    Ok(())
}

/// Turns a readback failure into something a session loop can act on.
pub(crate) fn readback_failure(error: ReadbackError) -> EncodeErrorKind {
    match error {
        ReadbackError::NotDirect3D { expected, detail } => EncodeErrorKind::Configuration {
            detail: format!("the handle given is not a live {expected}: {detail}"),
        },
        ReadbackError::Mismatched { detail } => EncodeErrorKind::Configuration { detail },
        ReadbackError::Api {
            operation,
            code,
            detail,
        } => {
            // A device that was removed or reset takes its staging texture with
            // it, and the recovery is a new device rather than a retry of the
            // frame (AGENTS.md section 16).
            if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
                EncodeErrorKind::DeviceLost
            } else {
                EncodeErrorKind::Api {
                    operation,
                    status: code,
                    status_name: "an HRESULT",
                    detail,
                }
            }
        }
    }
}

/// `DXGI_ERROR_DEVICE_REMOVED`, as an `HRESULT`.
const DXGI_ERROR_DEVICE_REMOVED: i32 = 0x887A_0005_u32 as i32;

/// `DXGI_ERROR_DEVICE_RESET`, as an `HRESULT`.
const DXGI_ERROR_DEVICE_RESET: i32 = 0x887A_0007_u32 as i32;

// SAFETY: `VideoEncoder` is `Send`, so that a session can be opened on one
// thread and moved to the encoding thread, and this type has to satisfy it.
//
// The three things it owns are each `Send` for their own reasons, argued where
// they are defined: the FFmpeg allocations in `avcodec`, the scaler in
// `convert`, and — the only one that is not obvious — the Direct3D objects in
// `readback`. Direct3D 11 has no thread affinity: a device is free-threaded,
// and its immediate context may be used from any thread as long as only one
// uses it at a time.
//
// That last condition is what `&mut self` provides. Two threads may not use one
// encoder at once, and nothing here makes it possible: this type is not `Sync`,
// every method takes `&mut self`, and a packet borrows the encoder it came
// from. What a caller *must* still arrange is that nothing else is using the
// same device's immediate context concurrently — a capture backend on another
// thread, for instance — which is why `docs/encoder-pipeline.md` says an
// encoder belongs on the thread that owns the device.
unsafe impl Send for SoftwareEncoder {}
