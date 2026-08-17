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
//! **A recording has sound in it.** [`RecordingSettings::with_system_audio`] and
//! [`RecordingSettings::with_microphone`] each add a named, independent audio
//! track fed by its own `clipped-audio` capture on its own thread
//! ([issue #180](https://github.com/wildware-uk/clipped/issues/180)); [`audio`]
//! is the wiring and `docs/av-sync.md` is the timing model it implements. Both
//! default to [`AudioSourceSetting::Off`], so a library caller that says nothing
//! gets a video-only file and no microphone is opened behind its back.
//!
//! What is *not* here is routing: a compatibility mix
//! ([issue #29](https://github.com/wildware-uk/clipped/issues/29)), the game's
//! own process tree ([issue #26](https://github.com/wildware-uk/clipped/issues/26))
//! and per-application tracks
//! ([issue #27](https://github.com/wildware-uk/clipped/issues/27)) are M2. A
//! recording made today has at most one system-audio track — the whole output
//! endpoint — and one microphone track.
//!
//! **A recording can keep the last few minutes to save from.** [`replay`] is
//! the join between a live recording and `clipped-replay`'s rolling buffer:
//! [`record_with_replay`] copies every packet it writes to the file into the
//! buffer as well — one encoder and two consumers, not two encodes
//! (`docs/replay-buffer.md`) — and [`ReplayRecording::save_last`] turns the
//! last N seconds of it into a playable clip on whichever thread asked for one
//! ([issues #37 and #38](https://github.com/wildware-uk/clipped/issues/38)).
//! The handle exists because a clip's container needs the codec, the picture
//! size and the parameter sets the encoder produced, and those are known only
//! once a recording has opened one. `apps/recorder`'s `replay` subcommand is
//! the driver, and its `serve` subcommand answers `save_replay` with the same
//! call.
//!
//! **And it can keep the buffer and write no recording at all.**
//! [`RecordingSettings::buffered`] names a directory instead of a file, and
//! [`record_into`] then opens no container, starts no muxing thread and arms no
//! disk guard — SPEC.md section 4's Manual/Replay capture mode
//! ([issue #423](https://github.com/wildware-uk/clipped/issues/423),
//! `docs/adr/0018-a-capture-that-writes-no-recording.md`). It is one branch
//! rather than a second capture loop: everything from the backend to the frame
//! gate to the silence reporting is the same code in the same order, and
//! [`RecordingReport::output`] answering [`None`] is what says which mode ran.
//! `clipped-recorder replay --no-recording` is the driver.
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
//! **A game is recorded at its own settings.** [`config`] is the configuration
//! API — global settings, per-game overrides that inherit from them,
//! validation, and a versioned file that survives being opened by an older
//! build ([issue #108](https://github.com/wildware-uk/clipped/issues/108),
//! AGENTS.md section 30), and [`automatic`] reads it: each recording it asks
//! for carries the settings resolved for that game, once, at the moment it
//! starts ([issue #61](https://github.com/wildware-uk/clipped/issues/61),
//! `docs/configuration.md`). [`record`] itself still takes what its caller
//! hands it — a library call has no game — and
//! [`config::ResolvedSettings::apply_to`] is the conversion between the two.
//! What is not done yet is the last link in `apps/recorder`'s `watch`, which
//! neither hands over a configuration nor applies what it is given, so a
//! settings file changes nothing about a shipped build's recordings. A
//! catalogue entry's own `default_settings` remain uninterpreted (SPEC.md
//! section 31).
//!
//! And which moments in a recording would be worth a clip: [`highlights`] is
//! the rule between `clipped-events`' vocabulary and `clipped-library`'s
//! virtual clip ([issue
//! #75](https://github.com/wildware-uk/clipped/issues/75), `docs/highlights.md`)
//! — what each kind of event keeps either side of itself, how sure its source
//! has to be, and how a firefight's worth of events becomes one highlight
//! rather than twenty overlapping ones. It resolves through the same
//! three-layer fold [`config`] uses, so a game's rules inherit the global ones.
//! The settings file does not carry those rules yet
//! ([issue #290](https://github.com/wildware-uk/clipped/issues/290)), so today
//! every caller resolves the shipped defaults.
//!
//! And the clips themselves: [`highlights::HighlightGeneration`] turns each of
//! those moments into a `clipped_library::VirtualClip` of the recording that
//! holds it, titled after what happened and tagged by kind ([issue
//! #76](https://github.com/wildware-uk/clipped/issues/76)). **It writes no
//! file** — a highlight costs metadata rather than disk, and rendering one is an
//! export ([issue #89](https://github.com/wildware-uk/clipped/issues/89)) — and
//! it works from the recordings a session has *finished*, so it is never
//! something a capture thread waits for (AGENTS.md section 20). Nothing in this
//! workspace calls it yet: no game event reaches a session, and a clip with no
//! file cannot be stored until the `clips` table has its migration ([issue
//! #269](https://github.com/wildware-uk/clipped/issues/269)), so a generated
//! highlight does not reach a user's library today.
//!
//! And the game's own account of what happened while it was being recorded:
//! [`plugins`] is what starts a highlight plugin during a recording
//! ([issue #338](https://github.com/wildware-uk/clipped/issues/338)).
//! `clipped-plugins` is the contract and deliberately starts nothing itself —
//! its supervisor is a state machine over a clock reading its owner supplies —
//! and this crate is that owner, because a plugin's event is a *position in a
//! recording* and this is where a recording's timeline exists.
//! [`plugins::SessionPlugins`] creates the supervisor, attaches the plugins
//! whose manifest claims the game that launched, polls it once a second on a
//! thread of its own and stops everything when the recording ends. Nothing in
//! the capture path waits on any of it: the only thing a recording gives a
//! plugin is [`RecordingProgress::timeline_began`], one `OnceLock` store on the
//! first frame.
//!
//! It starts only what the user enabled. `config::plugins` records that, and
//! what they agreed to when they did
//! ([issue #282](https://github.com/wildware-uk/clipped/issues/282)); a plugin
//! the settings file does not mention is disabled, and one whose network
//! declaration no longer matches the token beside it is refused and reported
//! rather than started. What a session cannot start it names, rather than
//! leaving somebody to wonder.
//!
//! What it drains now reaches the open session
//! ([`SessionManager::record_game_events`](automatic::SessionManager)), which
//! writes it to the sidecar the library indexes
//! ([issue #71](https://github.com/wildware-uk/clipped/issues/71)). Which file
//! each moment landed in is still undecided -- see
//! [`plugins`] for the one number that is missing.
//!
//! # Threading
//!
//! Two threads plus one per audio source, and the split is forced by AGENTS.md
//! section 20 — a capture thread may not wait on the filesystem:
//!
//! ```text
//!  caller's thread                        muxing thread
//!  ───────────────                        ─────────────
//!  acquire a frame                          ┌──────────────┐
//!  submit it to the encoder                 │              │
//!  drain packets ──── bounded queue ──────▶ │ write_packet │──▶ recording.mkv
//!  poll the stop signal                     │              │
//!  repeat                                   │              │
//!                                           │              │
//!  audio thread (one per source)            │              │
//!  ─────────────────────────────            │              │
//!  read a buffer from the endpoint          │              │
//!  place it on the timeline                 │              │
//!  queue it ────────── same queue ────────▶ │ write_samples│
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
//! clock to stamp a frame or a sample.
//!
//! That includes the audio: each buffer is placed through
//! `CaptureClock::media_time_on`, naming the clock WASAPI reports on, and the
//! offset between the endpoint's own account of a moment and the track's is
//! *measured* with `clipped_capture::DriftEstimator` rather than assumed —
//! [`AudioSyncReport`] is what it found. The rule about audio that precedes the
//! epoch is applied here too: it is trimmed at the epoch, to the sample, rather
//! than left for the writer to clamp onto the first instant of the file.
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
pub mod cleanup;
pub mod config;
pub mod disk;
pub mod failure;
pub mod highlights;
pub mod plugins;
pub mod replay;
pub mod screenshot;

mod capture_account;
mod error;
mod pacing;
mod progress;
mod report;
mod settings;
mod stop;

#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod encoding;
#[cfg(windows)]
mod muxing;
#[cfg(windows)]
mod recording;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use audio::{available_microphones, microphone_level, MicrophoneChoice, MicrophoneLevel};
pub use capture_account::{CaptureAccount, CaptureAccounting};
pub use disk::{SpaceVerdict, VolumeSpace, VolumeUnreadable, DEFAULT_MINIMUM_FREE_SPACE};
pub use error::SessionError;
pub use failure::{FailureKind, FootageKept, RecordingFailure};
pub use pacing::FrameGate;
pub use progress::RecordingProgress;
pub use replay::{start_buffer, ReplayRecording, ReplaySaveError};
pub use report::{AudioSyncReport, AudioTrackReport, EndReason, RecordingReport};
pub use settings::{
    AudioSourceSetting, CaptureTargetSettings, CodecPreference, EncoderPreference,
    RecordingSettings, ResolutionSetting, UnavailableChoice, DEFAULT_FRAMERATE,
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
/// Settings built with [`RecordingSettings::buffered`] write no file, so there
/// is nothing to finalise and nothing keeps what was captured: this call is then
/// a capture that throws every packet away. [`record_with_replay`] is what that
/// mode is for.
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
/// Given settings built with [`RecordingSettings::buffered`] this is SPEC.md
/// section 4's Manual/Replay capture mode: the buffer is the only thing the
/// packets reach, and the sitting's only files are the clips
/// [`ReplayRecording::save_last`] writes
/// (`docs/adr/0018-a-capture-that-writes-no-recording.md`).
///
/// The handle is owned by the caller rather than by the session, because the
/// caller is what saves from it: [`ReplayRecording::save_last`] runs on another
/// thread while this one carries on recording, and the lease it takes is what
/// holds the segments it reads against the eviction happening underneath it.
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
    replay: &ReplayRecording,
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
/// **None of them can fail a recording.** A replay buffer copies bytes into
/// memory it already owns, progress is one relaxed atomic store per encoded
/// frame, and a screenshot is a texture copy taken after the frame has already
/// reached the encoder — a screenshot that cannot be copied is refused to
/// whoever asked for it and the recording carries on. That is the whole of what
/// a recording gives them, and it is what keeps AGENTS.md section 17's promise
/// that nothing optional can cost somebody their footage.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecordingOutputs<'a> {
    /// A rolling window of the last few minutes, filled from the same encoder.
    ///
    /// The handle rather than the buffer, because a clip's container needs the
    /// codec, the picture size and the parameter sets the encoder produced, and
    /// only a recording knows those: [`ReplayRecording`] is where the recording
    /// puts them so that a save can declare them (`crate::replay`).
    pub replay: Option<&'a ReplayRecording>,
    /// Where a screenshot asks the recording for one of the frames it already
    /// has ([issue #67](https://github.com/wildware-uk/clipped/issues/67)).
    ///
    /// The recording copies the frame and hands the pixels over; encoding and
    /// writing the file happen on the thread that asked. Without this a
    /// screenshot would have to open a second capture of a window that is
    /// already being captured — see [`screenshot::capture_still`] for what that
    /// costs.
    pub screenshots: Option<&'a screenshot::ScreenshotRequests>,
    /// Where the recording says which capture backend it is using.
    ///
    /// The one place `clipped_capture::CaptureStatus` escapes the capture
    /// thread. Without it the backend a recording chose, and every one it fell
    /// past to get there, exist only in the log — which is what stopped the
    /// desktop application reporting either
    /// ([issue #302](https://github.com/wildware-uk/clipped/issues/302),
    /// `crate::capture_account`).
    ///
    /// A recording given none captures identically and says nothing about it.
    pub capture: Option<&'a CaptureAccounting>,
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
    pub const fn with_replay(mut self, replay: &'a ReplayRecording) -> Self {
        self.replay = Some(replay);
        self
    }

    /// The same outputs, also publishing where the recording has reached.
    #[must_use]
    pub const fn with_progress(mut self, progress: &'a RecordingProgress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// The same outputs, also saying which capture backend it settled on.
    #[must_use]
    pub const fn with_capture_account(mut self, capture: &'a CaptureAccounting) -> Self {
        self.capture = Some(capture);
        self
    }

    /// The same outputs, also serving screenshots from the frames it captures.
    #[must_use]
    pub const fn with_screenshots(
        mut self,
        screenshots: &'a screenshot::ScreenshotRequests,
    ) -> Self {
        self.screenshots = Some(screenshots);
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
    replay: &ReplayRecording,
) -> Result<RecordingReport, SessionError> {
    let _ = (settings, stop, replay);
    Err(SessionError::UnsupportedPlatform)
}
