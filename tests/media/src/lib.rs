//! Asserting that a produced recording is really valid media.
//!
//! AGENTS.md section 22 is the reason this crate exists:
//!
//! > Generated media must be validated. Do not assume successful
//! > encoder/muxer calls mean the recording is valid.
//!
//! Every crate that writes a file — the muxer today, the session recorder, the
//! replay buffer and the clip exporter later — needs the same seven answers:
//! the container opens, the video stream is there with the codec and size that
//! were asked for, the expected *number* of audio streams is there, the
//! duration is plausible, the timestamps increase, the tracks are in sync, and
//! the codec metadata is right. Before this crate each of them answered those
//! questions in its own way, which meant a third way arrived with every ticket
//! and none of them was ever tested against a file that was actually broken.
//!
//! **And the video to produce media *from*.** [`CodedVideo`] encodes a moving
//! test pattern to real H.264 with the same FFmpeg programs, and hands back the
//! coded pictures one at a time. Two crates need it — `clipped-replay` fills a
//! buffer with it and `apps/recorder` fills a replay recording — and neither
//! can encode without a GPU, so it lives here beside the checking rather than
//! twice beside the callers (AGENTS.md section 55). See `src/fixture.rs` for
//! why the programs rather than an encoder.
//!
//! An eighth answer arrived with the export command in
//! [issue #399](https://github.com/wildware-uk/clipped/issues/399), and for the
//! same reason: **were these the same coded bytes**. It is what tells a stream
//! copy from a re-encode, `crates/muxer` had it and
//! `apps/recorder/tests/ipc_protocol.rs` needed it, and two `ffprobe` wrappers
//! answering one question is what this crate exists to prevent. See
//! [`Media::packet_payloads_by_stream`].
//!
//! # How a test uses it
//!
//! ```no_run
//! use std::time::Duration;
//!
//! use clipped_media_validation::{AudioStream, Media, VideoStream};
//!
//! let media = Media::open("recording.mkv").expect("the recording opens");
//! media
//!     .validate()
//!     .stream_count(3)
//!     .video(
//!         VideoStream::codec("h264")
//!             .resolution(640, 360)
//!             .pixel_format("yuv420p")
//!             .decoded_frames(120),
//!     )
//!     .audio_stream_count(2)
//!     .audio(0, AudioStream::codec("pcm_s16le").sample_rate(48_000).channels(2))
//!     .audio(1, AudioStream::codec("pcm_s16le").sample_rate(48_000).channels(2))
//!     .duration_seconds(4.0, 0.1)
//!     .monotonic_timestamps()
//!     .synchronised_within(Duration::from_millis(50))
//!     .assert_valid();
//! ```
//!
//! Nothing is asserted until [`Validation::assert_valid`], so one run reports
//! *every* expectation that failed rather than the first, alongside an
//! inventory of what the file actually holds. A contributor reading the failure
//! at two in the morning should not have to run `ffprobe` by hand to find out
//! what they got.
//!
//! # Why `ffprobe` and `ffmpeg` rather than the linked libraries
//!
//! Deliberately, for three reasons.
//!
//! - AGENTS.md section 22 names `ffprobe` as the tool for exactly this, and the
//!   pinned FFmpeg build that `scripts/fetch-ffmpeg.ps1` installs on every
//!   machine that builds this workspace ships both programs. There is nothing
//!   extra to install.
//! - A validator that used the same code as the writer would be marking its own
//!   homework. `libavformat` reading a file through this crate's own wrapper
//!   would share every assumption `crates/muxer` makes; an outside program
//!   parsing the bytes on disk does not.
//! - The layering forbids the alternative anyway. `crates/muxer` is layer 2 and
//!   owns the only FFmpeg linkage in the workspace, so a harness the muxer's
//!   own tests can use has to sit *below* it — which rules out reaching FFmpeg
//!   through the muxer, and leaves linking a second, independent copy of
//!   `rusty_ffmpeg` into a test-only crate as the only other option.
//!
//! This is a test tool and nothing else. Nothing in the recorder shells out to
//! FFmpeg, and `docs/ffmpeg.md` says why.
//!
//! # When the tooling is missing
//!
//! [`MediaTools::locate`] says so with a message naming what was looked for and
//! how to get it, and [`require_media_tools`] turns that into a clean skip —
//! or, when `CLIPPED_REQUIRE_MEDIA` is set, into a failure, so a machine that
//! is supposed to validate media cannot quietly stop doing it.

mod audio;
mod expect;
mod fixture;
mod probe;
mod temporary;
mod tools;

pub use audio::{AudioContent, Tone};
pub use expect::{AudioStream, Validation, ValidationReport, VideoStream};
pub use fixture::{AccessUnit, CodedVideo};
pub use probe::{Media, MediaError, Packet, Stream};
pub use temporary::TemporaryDirectory;
pub use tools::{require_media_tools, MediaTools, ToolsUnavailable, REQUIRE_MEDIA};
