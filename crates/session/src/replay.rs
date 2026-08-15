//! A replay buffer attached to a live recording, and saving a clip out of it.
//!
//! `clipped-replay` is the buffer: it holds encoded segments, evicts what falls
//! out of its window, leases what a save needs and writes the file
//! (`docs/replay-buffer.md`). What it deliberately does not know is **what the
//! video in it is** — a clip's container needs the codec, the picture size and
//! the out-of-band parameter sets the encoder produced, and those exist only
//! inside a recording that has already opened an encoder.
//!
//! This module is that join, and it is why a caller does not hold a
//! [`ReplayBuffer`] directly:
//!
//! ```text
//!  apps/recorder                clipped-session                 clipped-replay
//!  ─────────────                ───────────────                 ──────────────
//!  ReplayRecording::new(60s) ─▶ record_into(outputs.replay)
//!                               │
//!                               ├─ encoder opens ─▶ begin(video track, bitrate)
//!                               │                      └─▶ ReplayBuffer::new
//!                               └─ every packet ────────▶ ReplayBuffer::push
//!
//!  save_last(30s, path) ──────────────────────────────▶ lease + save_clip
//! ```
//!
//! # Why the buffer is built when the encoder opens rather than by the caller
//!
//! Two things a buffer needs are only known then, and both would otherwise have
//! to be guessed:
//!
//! - **The bitrate**, which is what `ReplayConfig` sizes its memory ceiling
//!   from. A recording's bitrate is chosen from the size capture is actually
//!   producing (`crate::encoding`), and a window's captured size is not the
//!   size that was asked for — so a ceiling computed from the command line
//!   would be wrong for every window with a border.
//! - **The video track**, including the sequence and picture parameter sets.
//!   Without them a saved clip is a file with a video stream nothing can
//!   decode, which passes every check short of decoding it.
//!
//! Until then the handle holds a window and nothing else, and [`Self::save_last`]
//! says so rather than writing an empty file.
//!
//! # Threading
//!
//! One writer, any number of readers, and the save is on neither's thread.
//! [`Self::save_last`] takes the lease under the buffer's own lock — measured at
//! 0.77 ms for a five-minute window (`docs/replay-buffer.md`) — and then writes
//! the file holding no lock at all, from the segments the lease keeps alive. It
//! **must not be called on a capture thread** (AGENTS.md section 20); in
//! `apps/recorder` it is called from a hotkey handler's thread and from the IPC
//! connection thread, neither of which is capturing.

use core::fmt;
use core::time::Duration;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use clipped_encoder::BitRate;
use clipped_muxer::RecordingLayout;
use clipped_replay::{
    save_clip, ConfigError, LeaseError, ReplayBuffer, ReplayConfig, ReplayStats, SaveError,
    SavedClip, SpillArea, MAXIMUM_WINDOW, MINIMUM_WINDOW,
};

/// Attaches `replay` to a recording whose encoder has just opened, and hands
/// back the buffer its packets are to be copied into.
///
/// The join in one place, because it is two steps that are only correct
/// together: the buffer is built from the track this recording will declare and
/// the bitrate its encoder was given ([`ReplayRecording::begin`]), and the thing
/// the packet loop pushes into is the buffer *that* built
/// ([`ReplayRecording::buffer`]). Doing the first and not the second is a
/// recording that keeps an empty buffer; doing the second against a different
/// handle is a clip declaring a track it does not contain. Both are silent, and
/// neither shows up until somebody presses the key.
///
/// [`None`] means no clip can be saved from this recording — either it was not
/// asked for a buffer, or one could not be configured for it, which
/// [`ReplayRecording::begin`] reports and does not fail the recording over.
///
/// Called once per recording, from `crate::recording`, after the encoder is open
/// and before the first packet.
#[must_use]
pub fn start_buffer<'replay>(
    layout: &RecordingLayout,
    bitrate: BitRate,
    replay: Option<&'replay ReplayRecording>,
) -> Option<&'replay ReplayBuffer> {
    let replay = replay?;
    replay.begin(layout, bitrate);
    replay.buffer()
}

/// Counts the buffers this process has made, so that two recordings running at
/// once get spill directories of their own.
static BUFFERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A buffer for `config`, spilling to disk if this machine will let it.
///
/// A buffer that cannot make a spill directory — no per-user directory, a
/// read-only profile, a full disk — simply keeps everything in memory, which is
/// what every buffer did before
/// [issue #36](https://github.com/wildware-uk/clipped/issues/36) and is a
/// shorter window rather than a failed recording (AGENTS.md section 17).
fn buffer_for(config: ReplayConfig) -> Arc<ReplayBuffer> {
    let Some(root) = SpillArea::default_root() else {
        tracing::info!(
            "this machine describes no per-user directory, so the replay buffer will keep its              window in memory"
        );
        return Arc::new(ReplayBuffer::new(config));
    };

    let ordinal = BUFFERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match SpillArea::create(&root, std::process::id(), ordinal) {
        Ok(area) => {
            tracing::info!(
                directory = %clipped_logging::RedactedPath::new(area.directory()),
                "the replay buffer will keep its window on disk"
            );
            ReplayBuffer::spilling(config, area)
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "the replay buffer could not make a directory to spill into and will keep its                  window in memory, which for a long window may be less than was asked for"
            );
            Arc::new(ReplayBuffer::new(config))
        }
    }
}

/// What a recording's audio costs a replay buffer, in bytes a second.
///
/// Every declared track, at the four bytes a sample the buffer holds them in —
/// it keeps the interleaved `f32` the capture produced rather than the PCM the
/// file gets, so that a clip and the recording go through one conversion
/// (`clipped_replay::SegmentAudio`).
///
/// Told to [`ReplayConfig`] so that audio is budgeted rather than merely
/// stored. They share one ceiling, so audio nobody accounted for evicts video
/// and quietly shortens the window (`ReplayConfig::with_audio_bytes_per_second`).
fn audio_cost(layout: &RecordingLayout) -> u64 {
    layout
        .audio_tracks()
        .iter()
        .map(|track| {
            u64::from(track.sample_rate()) * u64::from(track.channels()) * size_of::<f32>() as u64
        })
        .sum()
}

/// A rolling window of the last few minutes of a recording, and what it takes
/// to save a clip from it.
///
/// Created before the recording starts and borrowed by it for its whole life
/// ([`crate::RecordingOutputs::with_replay`]). The caller keeps it because the
/// caller is what saves from it: a save runs on another thread while the
/// recording carries on.
#[derive(Debug)]
pub struct ReplayRecording {
    /// How much history the buffer will keep, checked before anything is
    /// recorded.
    window: Duration,
    /// The buffer and the track description, which arrive together when the
    /// encoder opens.
    ///
    /// A `OnceLock` rather than a mutex because it is written exactly once, by
    /// the recording, before the first packet is pushed — and read by every
    /// save afterwards without taking a lock the recording also takes.
    started: OnceLock<StartedReplay>,
}

/// The half of a [`ReplayRecording`] that exists once the encoder does.
#[derive(Debug)]
struct StartedReplay {
    /// Behind an [`Arc`] because the audio threads outlive the borrow the
    /// packet loop uses: they are spawned, not scoped, so they need an owned
    /// handle to push into (`crate::audio`).
    buffer: Arc<ReplayBuffer>,
    /// What a clip written from this buffer has to declare: the video track
    /// from the encoder that produced the packets, and every audio track the
    /// recording is writing, so a clip carries the same set the file does
    /// ([issue #40](https://github.com/wildware-uk/clipped/issues/40)).
    layout: RecordingLayout,
}

impl ReplayRecording {
    /// A handle that will keep the last `window` of the recording it is given
    /// to.
    ///
    /// The window is validated here, before a capture session, an encoder
    /// session or a file exists, so that `--duration 4h` is refused while
    /// somebody is still looking at their terminal rather than after their game
    /// has launched (AGENTS.md section 45).
    ///
    /// # Errors
    ///
    /// [`ConfigError::WindowOutOfRange`] for a window outside
    /// [`MINIMUM_WINDOW`]–[`MAXIMUM_WINDOW`].
    pub fn new(window: Duration) -> Result<Self, ConfigError> {
        if window < MINIMUM_WINDOW || window > MAXIMUM_WINDOW {
            return Err(ConfigError::WindowOutOfRange {
                requested: window,
                minimum: MINIMUM_WINDOW,
                maximum: MAXIMUM_WINDOW,
            });
        }

        Ok(Self {
            window,
            started: OnceLock::new(),
        })
    }

    /// How much history it keeps.
    #[must_use]
    pub const fn window(&self) -> Duration {
        self.window
    }

    /// Builds the buffer, now that there is an encoder to size it from.
    ///
    /// Called by the recording once, after the encoder is open and before any
    /// packet is pushed. A second call is ignored: a recording opens one
    /// encoder, and a handle reused for a second recording would be holding the
    /// first one's video.
    ///
    /// **It is public because a caller with coded video of its own has no other
    /// way in.** Every recording in this workspace opens a Direct3D device and
    /// an encoder session, so a test of what happens *after* the buffer — the
    /// naming, the session record, the file — could otherwise only be run on a
    /// machine with a GPU and a desktop session, which means in practice that it
    /// would not be run at all (`docs/testing.md`).
    /// `apps/recorder/tests/replay_clip.rs` pushes real H.264 in through here
    /// and decodes what comes out.
    ///
    /// A window this bitrate cannot be given a ceiling for is **not** a
    /// recording failure. The window is already known to be in range, so the
    /// only way `ReplayConfig` can refuse here is arithmetic nobody expected,
    /// and a replay buffer may never cost somebody their recording (AGENTS.md
    /// section 17): it is reported and the recording carries on with no buffer.
    pub fn begin(&self, layout: &RecordingLayout, bitrate: BitRate) {
        let config = match ReplayConfig::new(self.window, bitrate)
            .map(|config| config.with_audio_bytes_per_second(audio_cost(layout)))
        {
            Ok(config) => config,
            Err(error) => {
                tracing::error!(
                    %error,
                    window_seconds = self.window.as_secs_f64(),
                    "this recording will have no replay buffer, because one could not be \
                     configured for it; the recording itself is unaffected"
                );
                return;
            }
        };

        // `ReplayConfig` is `Copy`, so the configuration is still readable for
        // the line below after the buffer has taken it.
        let _ = self.started.set(StartedReplay {
            buffer: buffer_for(config),
            layout: layout.clone(),
        });

        tracing::info!(
            window_seconds = self.window.as_secs_f64(),
            ceiling_bytes = config.memory_ceiling(),
            "a replay buffer is running alongside this recording"
        );
    }

    /// The buffer the recording pushes into, or [`None`] before its encoder
    /// opened.
    ///
    /// For looking at what the buffer holds — `examples/replay_probe.rs` takes
    /// a lease through it to report what a save would have got. Saving is
    /// [`Self::save_last`], which is the path that knows what the video in the
    /// buffer is; a caller that leased and wrote a clip itself would have to
    /// describe the track a second time, and would get it wrong the moment the
    /// encoder changed.
    #[must_use]
    pub fn buffer(&self) -> Option<&ReplayBuffer> {
        self.started.get().map(|started| started.buffer.as_ref())
    }

    /// An owned handle to the buffer, for a thread that outlives this borrow.
    ///
    /// The packet loop pushes through [`Self::buffer`], because it runs inside
    /// the recording. The audio threads are spawned rather than scoped, so they
    /// need something `'static` to push into, and this is it.
    #[must_use]
    pub fn buffer_handle(&self) -> Option<Arc<ReplayBuffer>> {
        self.started
            .get()
            .map(|started| Arc::clone(&started.buffer))
    }

    /// What the buffer holds and what it has thrown away, or [`None`] before
    /// the recording opened its encoder.
    #[must_use]
    pub fn stats(&self) -> Option<ReplayStats> {
        self.started.get().map(|started| started.buffer.stats())
    }

    /// Writes the last `keep` of buffered video to `destination`.
    ///
    /// The clip begins at the keyframe at or before `keep` ago and ends at the
    /// newest picture the buffer holds, so it is never shorter than was asked
    /// for and at most one segment longer at the front
    /// ([`SavedClip::leading_slack`], `docs/replay-buffer.md`). A buffer that
    /// does not hold the whole of `keep` — a hotkey pressed ten seconds into a
    /// recording asking for thirty — still produces the clip there is, and
    /// [`SavedClip::is_complete`] says it was short.
    ///
    /// Blocks for as long as the write takes and must not be called on a
    /// capture thread.
    ///
    /// # Errors
    ///
    /// [`ReplaySaveError::NotBuffering`] before the recording's encoder has
    /// opened, [`ReplaySaveError::NothingBuffered`] when no keyframe has
    /// reached the buffer yet, and [`ReplaySaveError::NotWritten`] when the
    /// file could not be created or the write failed part way — which names the
    /// path, because a full drive or a name already taken is something only the
    /// user can act on.
    pub fn save_last(
        &self,
        keep: Duration,
        destination: &Path,
    ) -> Result<SavedClip, ReplaySaveError> {
        let started = self.started.get().ok_or(ReplaySaveError::NotBuffering)?;

        let lease = started
            .buffer
            .lease_last(keep)
            .map_err(ReplaySaveError::NothingBuffered)?;

        save_clip(&lease, destination, &started.layout).map_err(|source| {
            ReplaySaveError::NotWritten {
                destination: destination.to_path_buf(),
                source,
            }
        })
    }
}

/// Why a replay could not be saved.
///
/// Three cases, kept apart because a caller has to say something different
/// about each (AGENTS.md section 45): the recording has not reached the point
/// where there could be a buffer, the buffer has nothing in it yet, or the file
/// itself could not be written.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReplaySaveError {
    /// The recording is not running a replay buffer, or has not opened its
    /// encoder yet.
    NotBuffering,
    /// The buffer holds nothing that covers the request.
    NothingBuffered(LeaseError),
    /// The clip could not be written.
    NotWritten {
        /// Where it was going.
        destination: PathBuf,
        /// What the writer said.
        source: SaveError,
    },
}

impl fmt::Display for ReplaySaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBuffering => formatter.write_str(
                "this recording is not keeping a replay buffer, so there is nothing to save from",
            ),
            Self::NothingBuffered(error) => write!(formatter, "{error}"),
            Self::NotWritten { source, .. } => write!(formatter, "{source}"),
        }
    }
}

impl Error for ReplaySaveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotBuffering => None,
            Self::NothingBuffered(error) => Some(error),
            Self::NotWritten { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_outside_the_supported_range_is_refused_before_anything_is_recorded() {
        // The whole reason the window is checked here rather than when the
        // buffer is built: the buffer is built when the encoder opens, which is
        // after the capture session has started and the game is running.
        let error = ReplayRecording::new(Duration::from_secs(4 * 3600))
            .expect_err("four hours is not a supported window");

        let message = error.to_string();
        assert!(
            message.contains("30.0s") && message.contains("1800.0s"),
            "the refusal has to name the bounds, or there is nothing to type instead: {message}"
        );

        assert!(ReplayRecording::new(Duration::from_secs(29)).is_err());
        assert!(ReplayRecording::new(MINIMUM_WINDOW).is_ok());
        assert!(ReplayRecording::new(MAXIMUM_WINDOW).is_ok());
    }

    #[test]
    fn saving_before_the_encoder_opened_is_refused_rather_than_writing_an_empty_file() {
        let replay = ReplayRecording::new(Duration::from_secs(60)).expect("a supported window");

        let error = replay
            .save_last(Duration::from_secs(30), Path::new("clip.mkv"))
            .expect_err("nothing has been encoded");

        assert!(matches!(error, ReplaySaveError::NotBuffering));
        assert!(replay.stats().is_none());
        assert!(
            !Path::new("clip.mkv").exists(),
            "a refused save must not leave a file behind"
        );
    }
}
