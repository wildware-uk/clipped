//! Hardware and software video encoder backends.
//!
//! Hardware encoding is the default because the recorder runs alongside a game
//! and CPU time is the scarcest resource on the machine (AGENTS.md section 18).
//! A software encoder exists only as a fallback for hardware that cannot encode.
//!
//! This crate does two things.
//!
//! **Detection** answers what a machine could encode: which adapters are in it,
//! which encoders are on them, which codecs those encoders produce, and which
//! one "Automatic" should choose (SPEC.md section 9).
//!
//! **Encoding** turns captured GPU frames into coded packets. The interface is
//! [`VideoEncoder`], and two backends implement it on Windows: NVENC
//! ([#15](https://github.com/wildware-uk/clipped/issues/15)), through
//! [`NvencEncoder`], for H.264, HEVC and AV1; AMF
//! ([#16](https://github.com/wildware-uk/clipped/issues/16)), through
//! [`AmfEncoder`], for H.264 and HEVC; Quick Sync
//! ([#17](https://github.com/wildware-uk/clipped/issues/17)), through
//! [`QuickSyncEncoder`] — which is written and, for want of an Intel GPU to run
//! it on, unverified against hardware; `docs/encoder-pipeline.md` says exactly
//! which of its paths have never executed; and the software fallback
//! ([#18](https://github.com/wildware-uk/clipped/issues/18)), through
//! [`SoftwareEncoder`], which encodes H.264 on the CPU for a machine with no
//! usable encoding hardware.
//!
//! `docs/encoder-pipeline.md` describes the encoding half; the detection half
//! is `docs/encoder-capabilities.md`.
//!
//! # Responsibilities
//!
//! - Detecting adapters and encoders, and reporting what was found:
//!   [`detect`], [`CapabilityReport`].
//! - Saying which answers were measured and which were inferred: [`Claim`].
//! - Ranking encoders for "Automatic": [`recommend`].
//! - Remembering the answer between runs: [`CapabilityCache`].
//! - Describing a stream to be produced: [`EncoderConfig`].
//! - Producing it: [`VideoEncoder`], [`NvencEncoder`], [`AmfEncoder`],
//!   [`QuickSyncEncoder`], [`SoftwareEncoder`].
//!
//! # Not responsible for
//!
//! Frame acquisition (see `clipped-capture`) or container writing (see
//! `clipped-muxer`). This crate takes a texture somebody else captured and
//! produces packets somebody else writes.
//!
//! # Position in the architecture
//!
//! Sits above `clipped-windows` and below `clipped-muxer` and
//! `clipped-session`.
//!
//! # Measured, inferred, unknown
//!
//! Every capability this crate reports carries how it was arrived at, because
//! the two ways of answering "does this GPU encode AV1?" fail differently. A
//! table keyed on a GPU model is instant and lies the moment a driver changes;
//! opening an encoder session is the truth and costs GPU memory and a session
//! slot on a machine that may be in the middle of a match.
//!
//! This crate takes neither on its own:
//!
//! - **Measured**, in this run, without opening a session: the adapters and
//!   their driver versions, from DXGI; whether each vendor's encoder runtime
//!   loads; and which codecs the display driver registers a hardware encoder
//!   for, from Media Foundation.
//! - **Measured by asking an encoder**, which costs a session and therefore
//!   happens only when a user asks for it ([`Probing::WithSessions`], reached
//!   through `capabilities --refresh`): the maximum resolution, the encoder's
//!   own throughput, B-frames and 10-bit, for the vendors whose runtime answers
//!   ([`EncoderLimits`]).
//! - **Inferred**, from published limits, wherever nothing measured: maximum
//!   resolution, the codec level's framerate ceiling, B-frame and 10-bit
//!   support. Labelled as inferred everywhere they appear.
//! - **Unknown** wherever neither applies. In particular, no table entry in
//!   this crate claims HEVC or AV1 support: those become `true` only when the
//!   operating system reports an encoder for them. A user cannot be talked into
//!   AV1 by a table and then lose a recording to a driver that disagrees.
//!
//! The full trade-off, including what this deliberately cannot tell you, is in
//! `docs/encoder-capabilities.md`.
//!
//! # Example
//!
//! Reporting what a machine with no display adapter at all can do:
//!
//! ```
//! use clipped_encoder::{detect, recommend, EncoderKind, EncoderObservations, SystemFacts};
//!
//! let facts = SystemFacts::new(Vec::new(), EncoderObservations::none());
//! let report = detect(&facts);
//!
//! assert!(!report.has_hardware_encoder());
//! let automatic = recommend(&report);
//! assert_eq!(automatic[0].encoder(), EncoderKind::Software);
//! ```

mod adapter;
mod backend;
mod cache;
mod claim;
mod codec;
mod config;
mod detection;
mod error;
mod frame;
mod packet;
mod probe;
mod recommendation;
mod reference;

#[cfg(windows)]
mod software;

#[cfg(windows)]
mod windows;

pub use adapter::{Adapter, AdapterId, AdapterKind, DriverVersion};
pub use backend::VideoEncoder;
pub use cache::{
    CacheError, CacheState, CapabilityCache, HardwareSignature, StaleReason, CACHE_FILE_NAME,
    CACHE_FORMAT, DETECTION_REVISION,
};
pub use claim::{Claim, Evidence};
pub use codec::{Codec, EncoderKind, Resolution, Vendor};
pub use config::{
    BitRate, ColourMatrix, ColourPrimaries, ColourRange, ColourSpace, EncodePreset, EncoderConfig,
    FrameRate, KeyframeInterval, QualityTarget, RateControl, SurfaceFormat, TransferFunction,
};
pub use detection::{
    detect, detect_cached, Availability, CapabilityReport, CodecSupport, Detection,
    DetectionSource, EncoderReport, Signal, Unavailable,
};
pub use error::{EncodeContext, EncodeError, EncodeErrorKind};
pub use frame::{DeviceKind, GraphicsDevice, SourceFrame, SourceTexture, SurfaceKind};
pub use packet::{EncodedPacket, PictureKind};
pub use probe::{
    probe, EncoderLimits, EncoderObservations, HardwareEncoder, ProbeError, Probing,
    RuntimeObservation, RuntimeOutcome, SystemFacts, SystemProbe,
};
pub use recommendation::{measured_codecs, recommend, ChoiceReason, Recommendation};

#[cfg(windows)]
pub use software::SoftwareEncoder;
#[cfg(windows)]
pub use windows::{AmfEncoder, NvencEncoder, QuickSyncEncoder, WindowsProbe};
