//! Recording session coordination across capture, audio, encode and mux.
//!
//! This is the crate that knows the application's rules: which game is running,
//! which settings apply to it, when a recording starts and stops, and what
//! happens when part of the pipeline fails mid-session.
//!
//! # Responsibilities
//!
//! - Session lifecycle and capture-mode behaviour.
//! - Resolving configuration, including per-game overrides.
//! - Wiring capture, audio, encoding and muxing together and recovering from
//!   failures in any of them.
//!
//! # Not responsible for
//!
//! Any user interface. The desktop application talks to the recorder over an
//! explicit service boundary and the recorder must keep running without it
//! (AGENTS.md section 5).
//!
//! # Position in the architecture
//!
//! The top layer of the workspace, depended on by `apps/recorder`.
//!
//! # What exists today
//!
//! One recording: [`record`] captures a window, encodes its frames and writes
//! them into a Matroska file, stopping when the [`StopSignal`] it was given is
//! raised. It is the only caller in the workspace that holds `clipped-capture`,
//! `clipped-encoder` and `clipped-muxer` at once, which is the reason the
//! layering puts this crate above all three.
//!
//! And the policy that starts one without being asked: [`automatic`] joins
//! `clipped-game-detection`'s process watcher and game catalogue to [`record`],
//! so that launching a game produces a session recording and quitting it
//! finalises one ([issue #46](https://github.com/wildware-uk/clipped/issues/46),
//! `docs/sessions.md`). It is a state machine over watcher events and a
//! wall-clock reading — it opens no window and starts no thread — and
//! `apps/recorder`'s `watch` subcommand is the driver that carries out what it
//! decides.
//!
//! **There is no audio track yet.** A recording written here has one video
//! stream and nothing else; wiring `clipped-audio` in is
//! [issue #180](https://github.com/wildware-uk/clipped/issues/180). A caller
//! that asked for a microphone or system audio is told so once, at `warn`,
//! rather than being left to discover it in the file (AGENTS.md section 54).
//!
//! A recording can also fill a replay buffer: [`record_with_replay`] copies
//! every packet it writes to the file into a `clipped_replay::ReplayBuffer` as
//! well, so that a rolling window of the last few minutes is available to save
//! from. That is one encoder and two consumers, not two encodes
//! (`docs/replay-buffer.md`). Turning a buffered window into a clip is
//! `clipped_replay::save_clip`
//! ([issue #37](https://github.com/wildware-uk/clipped/issues/37)); the command
//! that would drive it is still
//! [issue #38](https://github.com/wildware-uk/clipped/issues/38), so nothing in
//! this workspace calls it outside its own tests.
//!
//! And the marks a person puts on a recording while it is being made:
//! [`bookmarks`] is the bookmark store
//! ([issue #64](https://github.com/wildware-uk/clipped/issues/64),
//! `docs/bookmarks.md`) — where a bookmark goes, what it may carry, and the
//! sidecar it is written to before the caller is told it was taken.
//! [`RecordingProgress`] is what it is placed against: the recording's own
//! position, published once per encoded frame, because timing a recording from
//! outside is ahead of the file by however long the encoder took to open.
//! `apps/recorder`'s `serve` subcommand answers `add_bookmark` with it.
//!
//! Per-game settings are modelled but not yet applied. [`config`] is the
//! configuration API — global settings, per-game overrides that inherit from
//! them, validation, and a versioned file that survives being opened by an
//! older build ([issue
//! #108](https://github.com/wildware-uk/clipped/issues/108), AGENTS.md section
//! 30). What it does *not* do is choose a recording's settings: [`record`] and
//! [`automatic`] still take what their caller hands them, and reading the
//! resolved settings at the moment a recording starts is
//! [issue #61](https://github.com/wildware-uk/clipped/issues/61).
//! A catalogue entry's own `default_settings` remain uninterpreted
//! (SPEC.md section 31).
//!
//! # Threading
//!
//! Two threads, and the split is forced by AGENTS.md section 20 — a capture
//! thread may not wait on the filesystem:
//!
//! ```text
//!  caller's thread                        muxing thread
//!  ───────────────                        ─────────────
//!  acquire a frame                          ┌──────────────┐
//!  submit it to the encoder                 │              │
//!  drain packets ──── bounded queue ──────▶ │ write_packet │──▶ recording.mkv
//!  poll the stop signal                     │              │
//!  repeat                                   └──────────────┘
//! ```
//!
//! Capture and encoding share one thread because a captured texture may not
//! outlive the acquisition that produced it (`docs/capture-pipeline.md`), so a
//! frame has to be encoded before the loop moves on. Only *packets* — bytes —
//! cross to the muxing thread, which owns the [`clipped_muxer::MkvWriter`] and
//! is the only thread that touches the file.
//!
//! The queue between them is bounded, and what happens when it fills is stated
//! rather than left to be discovered: the loop stops *submitting frames* while
//! the muxer is behind, and counts every frame it skipped
//! ([`RecordingReport::frames_dropped_writer_behind`]). Frames are dropped
//! before they are encoded and never after — an encoded packet thrown away
//! would corrupt every frame that referenced it — and the count is reported,
//! because a recorder that silently records at half the rate it says is worse
//! than one that admits it.
//!
//! # Timestamps
//!
//! `docs/av-sync.md`'s model, used exactly as written: the video source's clock
//! is the reference, the first frame kept fixes the epoch
//! ([`clipped_capture::CaptureClock`]), and every timestamp that reaches the
//! container has been through one conversion in one place. Nothing here reads a
//! clock to stamp a frame.
//!
//! # Example
//!
//! ```no_run
//! use std::path::PathBuf;
//! use std::sync::atomic::{AtomicBool, Ordering};
//!
//! use clipped_session::{record, CaptureTargetSettings, RecordingSettings, StopSignal};
//!
//! /// The simplest possible stop signal: something else sets the flag.
//! #[derive(Debug, Default)]
//! struct Flag(AtomicBool);
//!
//! impl StopSignal for Flag {
//!     fn is_requested(&self) -> bool {
//!         self.0.load(Ordering::SeqCst)
//!     }
//! }
//!
//! let settings = RecordingSettings::new(
//!     CaptureTargetSettings::window(0x0001_04ac, 2560, 1440),
//!     PathBuf::from(r"D:\clips\session.mkv"),
//! );
//!
//! let report = record(&settings, &Flag::default())?;
//! println!("{report}");
//! # Ok::<(), clipped_session::SessionError>(())
//! ```

pub mod automatic;
pub mod bookmarks;
pub mod config;
pub mod disk;
pub mod failure;

mod error;
mod pacing;
mod progress;
mod report;
mod settings;
mod stop;

#[cfg(windows)]
mod encoding;
#[cfg(windows)]
mod muxing;
#[cfg(windows)]
mod recording;
#[cfg(windows)]
mod windows;

pub use disk::{SpaceVerdict, VolumeSpace, VolumeUnreadable, DEFAULT_MINIMUM_FREE_SPACE};
pub use error::SessionError;
pub use failure::{FailureKind, FootageKept, RecordingFailure};
pub use pacing::FrameGate;
pub use progress::RecordingProgress;
pub use report::{EndReason, RecordingReport};
pub use settings::{
    CaptureTargetSettings, CodecPreference, EncoderPreference, RecordingSettings,
    ResolutionSetting, DEFAULT_FRAMERATE,
};
pub use stop::StopSignal;

/// Records `settings.target` until `stop` is raised, and finalises the file.
///
/// Blocks for the length of the recording, on the thread that called it, which
/// becomes the capture and encoding thread. A muxing thread is created for the
/// life of the recording and joined before this returns.
///
/// The file at [`RecordingSettings::output`] is finalised on **every** path out
/// of this function — a stop request, the target closing, an encoder failure, a
/// full disk — so that whatever was captured before the end is playable
/// (AGENTS.md section 17). Only a failure before the first packet leaves no
/// file at all.
///
/// # Errors
///
/// [`SessionError`], which names which stage refused and what it was asked
/// for. An error returned after recording started does not mean the recording
/// was lost: the file has been finalised by then, and the message says so.
/// What was written is in the log rather than in the error, because an error
/// carrying a success is a shape callers get wrong.
#[cfg(windows)]
pub fn record(
    settings: &RecordingSettings,
    stop: &dyn StopSignal,
) -> Result<RecordingReport, SessionError> {
    recording::record(settings, stop, &RecordingOutputs::default())
}

/// Records `settings.target`, filling `replay` from the same encoder.
///
/// Identical to [`record`] in every respect except that each encoded packet is
/// copied into `replay` as well as written to the file. **There is one
/// encoder**: a rolling replay buffer alongside a recording costs one memcpy
/// per packet and the memory the buffer's own configuration bounds, not a
/// second encode (SPEC.md section 16, `docs/replay-buffer.md`).
///
/// The buffer is owned by the caller rather than by the session, because the
/// caller is what saves from it: a save runs on another thread while this one
/// carries on recording, and
/// [`ReplayBuffer::lease`](clipped_replay::ReplayBuffer::lease) is what holds
/// the segments it reads against the eviction happening underneath it.
///
/// Building the clip is `clipped_replay::save_clip`
/// ([issue #37](https://github.com/wildware-uk/clipped/issues/37)), which a
/// caller can hand a lease taken from `replay`. Nothing in this workspace does
/// so outside that crate's own tests: the `recorder replay` command that would
/// drive it is
/// [issue #38](https://github.com/wildware-uk/clipped/issues/38), so today this
/// fills a buffer and reports what it holds at the end.
///
/// # Errors
///
/// Exactly [`record`]'s. A replay buffer cannot fail a recording: it copies
/// bytes into memory it already owns, and reaching its ceiling costs it its
/// oldest segments rather than costing the recording anything (AGENTS.md
/// section 17).
#[cfg(windows)]
pub fn record_with_replay(
    settings: &RecordingSettings,
    stop: &dyn StopSignal,
    replay: &clipped_replay::ReplayBuffer,
) -> Result<RecordingReport, SessionError> {
    recording::record(
        settings,
        stop,
        &RecordingOutputs::default().with_replay(replay),
    )
}

/// Everything a recording writes into as it goes, besides its own file.
///
/// One argument rather than one function per combination. Both members are
/// optional and both are borrowed for the length of the recording, because the
/// caller is what outlives it: a replay save runs on another thread while this
/// one carries on recording, and a bookmark is taken by whichever thread the
/// command arrived on.
///
/// **Neither can fail a recording.** A replay buffer copies bytes into memory it
/// already owns, and progress is one relaxed atomic store per encoded frame.
/// That is the whole of what a recording gives them, and it is what keeps
/// AGENTS.md section 17's promise that nothing optional can cost somebody their
/// footage.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecordingOutputs<'a> {
    /// A rolling window of the last few minutes, filled from the same encoder.
    pub replay: Option<&'a clipped_replay::ReplayBuffer>,
    /// Where the recording has reached on its own timeline.
    ///
    /// What a manual bookmark is placed against
    /// ([issue #64](https://github.com/wildware-uk/clipped/issues/64)). Without
    /// it a caller can only time the recording from outside, which is ahead of
    /// the file by however long the encoder took to open — see
    /// [`RecordingProgress`].
    pub progress: Option<&'a RecordingProgress>,
}

impl<'a> RecordingOutputs<'a> {
    /// The same outputs, also filling `replay`.
    #[must_use]
    pub const fn with_replay(mut self, replay: &'a clipped_replay::ReplayBuffer) -> Self {
        self.replay = Some(replay);
        self
    }

    /// The same outputs, also publishing where the recording has reached.
    #[must_use]
    pub const fn with_progress(mut self, progress: &'a RecordingProgress) -> Self {
        self.progress = Some(progress);
        self
    }
}

/// Records `settings.target`, writing into `outputs` as well as into the file.
///
/// [`record`] and [`record_with_replay`] are this with one shape of `outputs`
/// each; this is the one to call when a recording needs both, or needs
/// [`RecordingProgress`] so that something can name a moment inside it while it
/// is still being written.
///
/// # Errors
///
/// Exactly [`record`]'s.
#[cfg(windows)]
pub fn record_into(
    settings: &RecordingSettings,
    stop: &dyn StopSignal,
    outputs: &RecordingOutputs<'_>,
) -> Result<RecordingReport, SessionError> {
    recording::record(settings, stop, outputs)
}

/// Recording is a Windows feature; this build has no capture backend.
///
/// # Errors
///
/// Always [`SessionError::UnsupportedPlatform`].
#[cfg(not(windows))]
pub fn record_into(
    settings: &RecordingSettings,
    stop: &dyn StopSignal,
    outputs: &RecordingOutputs<'_>,
) -> Result<RecordingReport, SessionError> {
    let _ = (settings, stop, outputs);
    Err(SessionError::UnsupportedPlatform)
}

/// Recording is a Windows feature; this build has no capture backend.
///
/// The workspace compiles and unit-tests on other platforms so that a
/// contributor's other machine is not useless to them, and saying so plainly is
/// better than a build that succeeds and a recorder that produces nothing
/// (AGENTS.md section 54).
///
/// # Errors
///
/// Always [`SessionError::UnsupportedPlatform`].
#[cfg(not(windows))]
pub fn record(
    settings: &RecordingSettings,
    stop: &dyn StopSignal,
) -> Result<RecordingReport, SessionError> {
    let _ = (settings, stop);
    Err(SessionError::UnsupportedPlatform)
}

/// Recording is a Windows feature; this build has no capture backend.
///
/// # Errors
///
/// Always [`SessionError::UnsupportedPlatform`].
#[cfg(not(windows))]
pub fn record_with_replay(
    settings: &RecordingSettings,
    stop: &dyn StopSignal,
    replay: &clipped_replay::ReplayBuffer,
) -> Result<RecordingReport, SessionError> {
    let _ = (settings, stop, replay);
    Err(SessionError::UnsupportedPlatform)
}
