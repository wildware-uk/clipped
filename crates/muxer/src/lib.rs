//! Container writing and remuxing built on the FFmpeg libraries.
//!
//! Recordings are written incrementally into a recoverable container so that an
//! abrupt termination costs at most the last few seconds rather than the whole
//! session (AGENTS.md section 17).
//!
//! # Responsibilities
//!
//! - Writing video and multiple audio tracks into a container.
//! - Remuxing without re-encoding where the codecs already match.
//! - Preserving track identity and metadata.
//!
//! # Not responsible for
//!
//! Encoding (see `clipped-encoder`) or edit decisions (see the export engine).
//!
//! # Position in the architecture
//!
//! Sits above `clipped-encoder` and below `clipped-session`.
//!
//! # FFmpeg
//!
//! This crate owns the workspace's link against FFmpeg. It is a dynamic link
//! against a prebuilt, LGPL-only build that `scripts/fetch-ffmpeg.ps1`
//! downloads and checksum-verifies; `docs/adr/0004-ffmpeg-dependency-strategy.md`
//! records why, and `docs/ffmpeg.md` covers building against it.
//!
//! The binding, `rusty_ffmpeg`, is a `-sys` crate and offers no safe API, so the
//! safe wrappers over FFmpeg are written here and every other crate reaches
//! FFmpeg through them rather than through the raw FFI.
//!
//! # What is here today
//!
//! [`MkvWriter`] writes a recording — one video track and any number of named
//! audio tracks — into Matroska as the packets arrive, and [`linkage`] reports
//! and probes the FFmpeg actually loaded. Remuxing to MP4
//! ([issue #92](https://github.com/wildware-uk/clipped/issues/92)) is not
//! written yet.
//!
//! A replay clip is written by this same writer rather than by anything of its
//! own: `clipped_replay::save_clip` leases the segments a clip needs and drives
//! [`MkvWriter`] over them
//! ([issue #37](https://github.com/wildware-uk/clipped/issues/37)). Nothing here
//! knows a replay buffer exists, and nothing here should — the dependency points
//! the other way, which is what keeps a recording and a clip written by one
//! implementation.
//!
//! `docs/muxing.md` is the subsystem document: what the container guarantees
//! when a recording is interrupted, how timestamps are converted, and how the
//! output is validated.
//!
//! ```no_run
//! use std::path::Path;
//! use clipped_muxer::{
//!     AudioCodec, AudioTrack, EncodedPacket, MkvWriter, PacketTimestamp, RecordingLayout,
//!     TrackId, VideoCodec, VideoTrack,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let sequence_header: Vec<u8> = Vec::new();
//! # let frame: Vec<u8> = Vec::new();
//! let layout = RecordingLayout::new(
//!     VideoTrack::new(VideoCodec::H264, 2560, 1440).with_codec_private(sequence_header),
//! )
//! .with_audio_track(
//!     AudioTrack::new(AudioCodec::PcmS16Le, 48_000, 2)
//!         .with_name("Compatibility Mix")
//!         .as_default(),
//! )
//! .with_audio_track(AudioTrack::new(AudioCodec::PcmS16Le, 48_000, 2).with_name("Game"));
//!
//! let mut writer = MkvWriter::create(Path::new("recording.mkv"), &layout)?;
//! writer.write_packet(
//!     &EncodedPacket::new(TrackId::Video, PacketTimestamp::from_nanos(0), &frame)
//!         .with_keyframe(true),
//! )?;
//! let summary = writer.finish()?;
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod linkage;
pub mod packet;
mod timeline;
pub mod track;
pub mod writer;

pub use crate::error::{AvError, MuxError};
pub use crate::packet::{EncodedPacket, PacketTimestamp};
pub use crate::track::{
    AudioCodec, AudioTrack, FrameRate, InvalidLanguage, Language, RecordingLayout, TrackId,
    VideoCodec, VideoTrack,
};
pub use crate::writer::{MkvWriter, RecordingSummary};
