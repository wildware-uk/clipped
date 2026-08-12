//! Keeping a recording alive when the backend capturing it stops working.
//!
//! [`select`] chooses a backend before a recording starts. This is what happens
//! when that backend fails *during* one — a driver reset, a display change, a
//! window that turns out to be uncapturable, or a capture that keeps handing
//! over frames with nothing in them
//! ([issue #97](https://github.com/wildware-uk/clipped/issues/97), SPEC.md
//! section 8: "if one mechanism fails, automatically fall back where safe").
//!
//! # The shape
//!
//! [`CaptureFallback`] is a policy object, not a wrapper. The caller goes on
//! owning the [`CaptureBackend`] and goes on calling
//! [`acquire`](CaptureBackend::acquire) on it directly. That is deliberate:
//! wrapping the acquisition would mean handing out a frame borrowed from the
//! very value that has to replace the backend it came from, and the hot path of
//! a recorder is not where that complexity belongs. Instead the caller hands the
//! failed backend over, by value, and gets a running replacement back.
//!
//! ```text
//! let (mut fallback, mut backend, format) =
//!     CaptureFallback::start(candidates, &target, &config, setting)?.into_parts();
//!
//! loop {
//!     match backend.acquire(timeout) {
//!         Ok(Acquisition::Frame(frame)) => {
//!             match fallback.inspect(&frame) {           // is it all black?
//!                 None => encode(&frame),
//!                 Some(run) => {
//!                     drop(frame);
//!                     backend = fallback
//!                         .recover_from_black_frames(backend, run)?
//!                         .into_parts()
//!                         .0;
//!                 }
//!             }
//!         }
//!         Ok(Acquisition::Timeout) => fallback.note_silence(timeout),
//!         Ok(Acquisition::SizeChanged(size)) => { /* issue #184 decides */ }
//!         Err(error) => backend = fallback.recover(backend, error)?.into_parts().0,
//!     }
//! }
//! ```
//!
//! Passing the failed backend by value is enforcement rather than politeness: a
//! caller cannot go on using a backend it has given up, and the fallback can
//! shut it down before asking the platform for another — which matters, because
//! DXGI gives a process one duplication per display and a replacement would be
//! refused while the corpse of the old one still held it.
//!
//! # What it will not do
//!
//! **It will not change the size of the frames.** Matroska fixes a track's
//! dimensions in the header (ADR 0001) and the encoder is configured for one
//! resolution, so a replacement that initialises to a different [`FrameFormat`]
//! than the recording committed to cannot be used: it is shut down again, the
//! mismatch is recorded, and the next candidate is tried. When nothing matches,
//! the recording ends where it is rather than continuing into a file whose track
//! declares a size the frames do not have. That is the same answer the pipeline
//! already gives to a window resized mid-recording, and when
//! [issue #184](https://github.com/wildware-uk/clipped/issues/184) decides how a
//! session follows a size change, this rule relaxes in that same change.
//!
//! **It will not treat silence as failure.** A capture that produces no frames
//! is indistinguishable from a source that is producing none, and the commonest
//! reason for the second is a minimised window — which both Windows backends
//! deliberately wait out (`docs/capture-pipeline.md`). Falling back on silence
//! would swap a user's preferred backend for a worse one every time they
//! alt-tabbed. So silence is *reported* —
//! [`note_silence`](CaptureFallback::note_silence) logs it and
//! [`silent_for`](CaptureFallback::silent_for) can be shown — and nothing else.
//!
//! **It will not override a pinned method.** With
//! [`CaptureMethodSetting::Forced`], a failed backend is restarted but never
//! replaced by a different one, for the same reason
//! [`SelectionError::ForcedMethodRejected`] exists: an explicit setting is
//! obeyed or reported, never quietly swapped.

use core::fmt;
use core::time::Duration;
use std::error::Error;

use crate::{
    select, BackendDeclaration, BlackFrameWatch, BlackRun, CaptureBackend, CaptureBackendFactory,
    CaptureConfig, CaptureError, CaptureMethod, CaptureMethodSetting, CaptureTarget, CapturedFrame,
    FrameFormat, FrameSampler, FrameSize, Outcome, Selection, SelectionError,
};

/// How many times one method may be restarted before it is given up on.
///
/// A restart is cheap and keeps the preferred backend, which is worth doing for
/// a driver reset or a mode change; a backend that has failed three times in one
/// recording is not having a bad moment. There is deliberately no time-based
/// forgiveness — a budget that refills is a budget that hides a backend failing
/// every thirty seconds for an hour.
const RESTARTS_PER_METHOD: u32 = 2;

/// How often a capture that is producing nothing is mentioned in the log.
const SILENCE_REPORTING_INTERVAL: Duration = Duration::from_secs(30);

/// Why a backend was restarted or replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FallbackTrigger {
    /// A backend could not be started at all, before the first frame.
    InitialisationFailed,
    /// The running backend returned an error mid-recording.
    CaptureFailed,
    /// The running backend kept producing frames with nothing in them.
    BlackFrames,
}

impl FallbackTrigger {
    /// How this is written in a structured log field, beside `capture_backend`
    /// (docs/logging.md).
    #[must_use]
    pub const fn log_value(self) -> &'static str {
        match self {
            Self::InitialisationFailed => "initialisation_failed",
            Self::CaptureFailed => "capture_failed",
            Self::BlackFrames => "black_frames",
        }
    }
}

impl fmt::Display for FallbackTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InitialisationFailed => "it could not be started",
            Self::CaptureFailed => "it failed",
            Self::BlackFrames => "it was recording nothing but black",
        })
    }
}

/// What a caller can do about a failure of the running backend.
///
/// This is the reading of [`CaptureError`]'s variants that
/// [`CaptureFallback::recover`] acts on, exposed because a caller may want to
/// know what will happen before it gives its backend up, and because it puts
/// "why did that recording end?" in one place rather than spread across a
/// `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FailureResponse {
    /// Nothing another backend could do better: the recording is over.
    ///
    /// The window closed, the display went, or the caller made a programming
    /// error. Falling back here would produce a second recording of nothing.
    Fatal,
    /// Restart this same backend, and fall back only if it will not restart.
    ///
    /// [`CaptureError::Interrupted`] says so outright — a driver reset or a mode
    /// change leaves the target where it was. An unclassified
    /// [`CaptureError::Backend`] is treated the same way: a hiccup in the
    /// preferred backend is likelier than a broken one, and the cost of being
    /// wrong is one restart before the fall back happens anyway.
    Restart,
    /// Give this backend up and try the next one.
    FallBack,
}

/// What a failure of the running backend means for the recording.
///
/// A free function rather than a method on [`CaptureError`] because it is a
/// *policy*: which failures are worth another backend is this module's decision,
/// and the error type should not have to know it.
#[must_use]
pub const fn response_to(error: &CaptureError) -> FailureResponse {
    match error {
        CaptureError::TargetLost { .. }
        | CaptureError::NotInitialised { .. }
        | CaptureError::AlreadyInitialised { .. } => FailureResponse::Fatal,
        CaptureError::UnsupportedTarget { .. } => FailureResponse::FallBack,
        CaptureError::Interrupted { .. } | CaptureError::Backend { .. } => FailureResponse::Restart,
    }
}

/// One backend that was asked to take over, and why it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    method: CaptureMethod,
    reason: String,
}

impl Attempt {
    pub(crate) const fn new(method: CaptureMethod, reason: String) -> Self {
        Self { method, reason }
    }

    /// The method that was tried.
    #[must_use]
    pub const fn method(&self) -> CaptureMethod {
        self.method
    }

    /// Why it could not take over, phrased for a diagnostics screen.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for Attempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.method, self.reason)
    }
}

/// A backend was restarted, or replaced by a different one.
///
/// This is the record behind "the capture method changed", and it is what the
/// diagnostics screen and the session log show. It exists even when the method
/// did not change — a restart of the same backend has [`from`](Self::from) equal
/// to [`to`](Self::to) — because "it fell over and came back" is what explains a
/// gap in an otherwise unexplained recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodChange {
    from: CaptureMethod,
    to: CaptureMethod,
    trigger: FallbackTrigger,
    reason: String,
}

impl MethodChange {
    /// The method that was in use before.
    #[must_use]
    pub const fn from(&self) -> CaptureMethod {
        self.from
    }

    /// The method in use now.
    #[must_use]
    pub const fn to(&self) -> CaptureMethod {
        self.to
    }

    /// Whether this was the same backend starting again rather than a different
    /// one taking over.
    #[must_use]
    pub fn is_restart(&self) -> bool {
        self.from == self.to
    }

    /// What made it necessary.
    #[must_use]
    pub const fn trigger(&self) -> FallbackTrigger {
        self.trigger
    }

    /// What the failure was, phrased for a diagnostics screen.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for MethodChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_restart() {
            write!(f, "{} was restarted: {}", self.from, self.reason)
        } else {
            write!(
                f,
                "{} took over from {}: {}",
                self.to, self.from, self.reason
            )
        }
    }
}

/// What the product shows about capture, and what diagnostics adds to it.
///
/// The first two lines are SPEC.md section 8's, unchanged by anything in this
/// module: a user sees what they asked for and what is running, and never has to
/// learn that a backend was swapped underneath them for the recording to keep
/// going.
///
/// ```text
/// Capture method: Automatic
/// Current method: Desktop Duplication
/// ```
///
/// [`changes`](Self::changes) is the third thing, and it is why this is a type
/// rather than two getters: "Desktop Duplication", in a recording that started
/// on Windows Graphics Capture, is a fact with no explanation attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureStatus<'changes> {
    setting: CaptureMethodSetting,
    initial: CaptureMethod,
    current: CaptureMethod,
    changes: &'changes [MethodChange],
}

impl CaptureStatus<'_> {
    /// What the user asked for: `Automatic`, or a method they pinned.
    #[must_use]
    pub const fn setting(&self) -> CaptureMethodSetting {
        self.setting
    }

    /// The method this recording started with.
    #[must_use]
    pub const fn initial_method(&self) -> CaptureMethod {
        self.initial
    }

    /// The method capturing now, to be reported as `Current method`.
    #[must_use]
    pub const fn current_method(&self) -> CaptureMethod {
        self.current
    }

    /// Whether the method capturing now is not the one this recording started
    /// with.
    #[must_use]
    pub fn has_changed(&self) -> bool {
        self.current != self.initial
    }

    /// Every restart and replacement, in the order they happened.
    #[must_use]
    pub const fn changes(&self) -> &[MethodChange] {
        self.changes
    }
}

impl fmt::Display for CaptureStatus<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Capture method: {}\nCurrent method: {}",
            self.setting, self.current
        )
    }
}

/// Nothing could keep the capture going.
#[derive(Debug)]
#[non_exhaustive]
pub enum FallbackError {
    /// No backend could be chosen for this target in the first place.
    Selection(SelectionError),
    /// The failure is not one any backend could have survived.
    ///
    /// The window closed, the display went, or a caller used the interface
    /// wrongly. The original error is kept, so a caller can go on telling those
    /// apart.
    Unrecoverable(CaptureError),
    /// Every backend that might have taken over has now been tried.
    Exhausted {
        /// What made the running backend unusable.
        trigger: FallbackTrigger,
        /// The failure behind it, when there was one. Black frames have none:
        /// nothing returned an error.
        cause: Option<CaptureError>,
        /// Every replacement considered, and why each could not take over.
        ///
        /// Empty when there was nothing left to consider, which the message
        /// says in those words rather than leaving a bug report to read as
        /// "capture failed" (AGENTS.md section 15).
        attempts: Vec<Attempt>,
    },
}

impl fmt::Display for FallbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Selection(error) => write!(f, "{error}"),
            Self::Unrecoverable(error) => write!(f, "{error}"),
            Self::Exhausted {
                trigger,
                cause,
                attempts,
            } => {
                match cause {
                    Some(cause) => write!(f, "{cause}")?,
                    None => write!(f, "the capture backend was replaced because {trigger}")?,
                }
                if attempts.is_empty() {
                    return f.write_str("; there was no other capture backend to try");
                }
                f.write_str("; no other capture backend could take over")?;
                for attempt in attempts {
                    write!(f, "; {attempt}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for FallbackError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Selection(error) => Some(error),
            Self::Unrecoverable(error) => Some(error),
            Self::Exhausted { cause, .. } => cause.as_ref().map(|cause| cause as &dyn Error),
        }
    }
}

impl From<SelectionError> for FallbackError {
    fn from(error: SelectionError) -> Self {
        Self::Selection(error)
    }
}

/// A capture that has started, and the fallback that will keep it going.
#[derive(Debug)]
pub struct StartedCapture<'candidates> {
    fallback: CaptureFallback<'candidates>,
    backend: Box<dyn CaptureBackend>,
    format: FrameFormat,
}

impl<'candidates> StartedCapture<'candidates> {
    /// The method that started, to be reported as `Current method`.
    #[must_use]
    pub const fn method(&self) -> CaptureMethod {
        self.fallback.current_method()
    }

    /// The shape of the frames it will produce, and the shape any replacement
    /// will have to produce too.
    #[must_use]
    pub const fn format(&self) -> FrameFormat {
        self.format
    }

    /// Splits it into the three things a recording loop needs.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        CaptureFallback<'candidates>,
        Box<dyn CaptureBackend>,
        FrameFormat,
    ) {
        (self.fallback, self.backend, self.format)
    }
}

/// A replacement backend, and the change that produced it.
#[derive(Debug)]
pub struct Recovery {
    backend: Box<dyn CaptureBackend>,
    change: MethodChange,
}

impl Recovery {
    /// What changed, for the log and the diagnostics screen.
    #[must_use]
    pub const fn change(&self) -> &MethodChange {
        &self.change
    }

    /// The method capturing now.
    #[must_use]
    pub const fn method(&self) -> CaptureMethod {
        self.change.to
    }

    /// The running backend, and the record of how it got there.
    #[must_use]
    pub fn into_parts(self) -> (Box<dyn CaptureBackend>, MethodChange) {
        (self.backend, self.change)
    }
}

/// Keeps one recording capturing when the backend under it fails.
///
/// # Ownership and threading
///
/// It owns no native resources. A failed backend is shut down and dropped inside
/// [`recover`](Self::recover), and its replacement is handed straight back to
/// the caller. What it holds is what it needs to start a backend without being
/// told any of it again — the candidate list, the target and the configuration —
/// plus a [`FrameSampler`], which is the one part of it that touches a graphics
/// API.
///
/// It belongs to the capture thread, like the backends it creates.
#[derive(Debug)]
pub struct CaptureFallback<'candidates> {
    candidates: Vec<&'candidates dyn CaptureBackendFactory>,
    target: CaptureTarget,
    config: CaptureConfig,
    setting: CaptureMethodSetting,
    /// The frame format this recording is committed to. A backend that cannot
    /// produce it is not a replacement — see the module documentation.
    committed: FrameFormat,
    initial: CaptureMethod,
    current: CaptureMethod,
    /// Methods that have failed and will not be asked again this recording.
    retired: Vec<CaptureMethod>,
    changes: Vec<MethodChange>,
    restarts: u32,
    watch: BlackFrameWatch,
    sampler: Option<Box<dyn FrameSampler>>,
    silence: Duration,
    silence_reported: Duration,
}

impl<'candidates> CaptureFallback<'candidates> {
    /// Starts capturing `target`, falling past any candidate that cannot start.
    ///
    /// The first backend is chosen by [`select`], exactly as a recording without
    /// fallback would choose it. What differs is what happens when the chosen
    /// one cannot be created or initialised: under
    /// [`CaptureMethodSetting::Automatic`] that candidate is retired and the
    /// next is tried, so a machine where Windows Graphics Capture is present but
    /// broken records through Desktop Duplication rather than not recording. The
    /// fall-through is kept as a [`MethodChange`] with
    /// [`FallbackTrigger::InitialisationFailed`], so a report can say why the
    /// preferred method is not the one running.
    ///
    /// # Errors
    ///
    /// [`FallbackError::Selection`] when no candidate can capture the target,
    /// [`FallbackError::Unrecoverable`] when the target itself has gone, and
    /// [`FallbackError::Exhausted`] when every candidate was tried and none
    /// started.
    pub fn start(
        candidates: &[&'candidates dyn CaptureBackendFactory],
        target: &CaptureTarget,
        config: &CaptureConfig,
        setting: CaptureMethodSetting,
    ) -> Result<StartedCapture<'candidates>, FallbackError> {
        let candidates = candidates.to_vec();
        let mut retired: Vec<CaptureMethod> = Vec::new();
        let mut attempts: Vec<Attempt> = Vec::new();
        let mut preferred: Option<(CaptureMethod, String)> = None;

        loop {
            let method = choose(&candidates, &retired, target, setting)?.method();

            match open(&candidates, method, target, config, None) {
                Ok((backend, format)) => {
                    let mut fallback = Self {
                        candidates,
                        target: *target,
                        config: *config,
                        setting,
                        committed: format,
                        initial: method,
                        current: method,
                        retired,
                        changes: Vec::new(),
                        restarts: 0,
                        watch: BlackFrameWatch::default(),
                        sampler: default_frame_sampler(),
                        silence: Duration::ZERO,
                        silence_reported: Duration::ZERO,
                    };

                    if let Some((failed, reason)) = preferred {
                        tracing::warn!(
                            capture_backend = method.log_value(),
                            previous_capture_backend = failed.log_value(),
                            trigger = FallbackTrigger::InitialisationFailed.log_value(),
                            reason = %reason,
                            "the preferred capture backend could not be started, so another one \
                             is recording instead"
                        );
                        fallback.changes.push(MethodChange {
                            from: failed,
                            to: method,
                            trigger: FallbackTrigger::InitialisationFailed,
                            reason,
                        });
                    }

                    tracing::info!(
                        capture_backend = method.log_value(),
                        width = format.size().width(),
                        height = format.size().height(),
                        "capture started, and will fall back to another backend if this one fails"
                    );
                    return Ok(StartedCapture {
                        fallback,
                        backend,
                        format,
                    });
                }
                Err(OpenFailure::Fatal(error)) => return Err(FallbackError::Unrecoverable(error)),
                Err(failure) => {
                    let reason = failure.reason();
                    tracing::warn!(
                        capture_backend = method.log_value(),
                        trigger = FallbackTrigger::InitialisationFailed.log_value(),
                        reason = %reason,
                        "a capture backend could not be started"
                    );
                    attempts.push(Attempt::new(method, reason.clone()));
                    if preferred.is_none() {
                        preferred = Some((method, reason));
                    }
                    retired.push(method);

                    if setting != CaptureMethodSetting::Automatic {
                        // A pinned method is obeyed or reported. There is
                        // nothing else to try, by definition.
                        return Err(FallbackError::Exhausted {
                            trigger: FallbackTrigger::InitialisationFailed,
                            cause: failure.into_cause(),
                            attempts,
                        });
                    }
                }
            }
        }
    }

    /// Replaces the sampler black-frame detection reads pixels with.
    ///
    /// A recording gets the platform's own sampler and never calls this.
    /// Passing [`None`] turns black-frame detection off, which is the state a
    /// build with no sampler for its platform is in already.
    pub fn set_frame_sampler(&mut self, sampler: Option<Box<dyn FrameSampler>>) {
        self.sampler = sampler;
    }

    /// Replaces the black-frame policy with a differently configured one.
    pub fn set_black_frame_watch(&mut self, watch: BlackFrameWatch) {
        self.watch = watch;
    }

    /// What the product shows, and what diagnostics adds to it.
    #[must_use]
    pub fn status(&self) -> CaptureStatus<'_> {
        CaptureStatus {
            setting: self.setting,
            initial: self.initial,
            current: self.current,
            changes: &self.changes,
        }
    }

    /// The method capturing now.
    #[must_use]
    pub const fn current_method(&self) -> CaptureMethod {
        self.current
    }

    /// Every restart and replacement so far.
    #[must_use]
    pub fn changes(&self) -> &[MethodChange] {
        &self.changes
    }

    /// The frame format this recording is committed to.
    ///
    /// A replacement backend has to produce exactly this, or it cannot be used.
    #[must_use]
    pub const fn committed_format(&self) -> FrameFormat {
        self.committed
    }

    /// Looks at a frame, and says whether the capture has been black too long.
    ///
    /// Call it with every frame. It samples at most twice a second
    /// ([`BlackFrameWatch::DEFAULT_SAMPLE_INTERVAL`]), so calling it with the
    /// other fifty-eight costs one comparison.
    ///
    /// A [`BlackRun`] coming back means the recording has contained nothing but
    /// black for [`BlackFrameWatch::DEFAULT_TOLERATED_BLACKNESS`], and the
    /// caller hands the backend to
    /// [`recover_from_black_frames`](Self::recover_from_black_frames). Ignoring
    /// it is possible — it is reported again on the next sample — but a caller
    /// that never acts on it is writing a black file, which is the thing issue
    /// #97 exists to stop.
    pub fn inspect(&mut self, frame: &CapturedFrame<'_>) -> Option<BlackRun> {
        self.silence = Duration::ZERO;
        self.silence_reported = Duration::ZERO;

        if !self.watch.is_due(frame.timestamp()) {
            return None;
        }
        let sample = self.sampler.as_mut()?.sample(frame)?;
        let run = self.watch.observe(sample, frame.timestamp())?;

        tracing::warn!(
            capture_backend = self.current.log_value(),
            black_seconds = run.length().as_secs_f64(),
            samples = run.samples(),
            brightest_channel = sample.brightest(),
            "the capture is producing nothing but black frames"
        );
        Some(run)
    }

    /// Records that an acquisition produced no frame.
    ///
    /// Silence is not treated as a failure — the module documentation says why —
    /// and it is not swallowed either: a capture that has produced nothing for
    /// half a minute says so in the log, once every thirty seconds, and
    /// [`silent_for`](Self::silent_for) can be read by a diagnostics screen at
    /// any time. `waited` is the timeout the acquisition used, which is how long
    /// this particular silence lasted.
    pub fn note_silence(&mut self, waited: Duration) {
        self.silence = self.silence.saturating_add(waited);
        if self.silence
            >= self
                .silence_reported
                .saturating_add(SILENCE_REPORTING_INTERVAL)
        {
            self.silence_reported = self.silence;
            tracing::info!(
                capture_backend = self.current.log_value(),
                silent_seconds = self.silence.as_secs_f64(),
                "the capture source has produced no frames; a minimised or idle source is not a \
                 capture failure and is not fallen back from"
            );
        }
    }

    /// How long the source has gone without producing a frame.
    #[must_use]
    pub const fn silent_for(&self) -> Duration {
        self.silence
    }

    /// Follows a target that changed size, and commits the recording to the new
    /// format.
    ///
    /// Use this rather than [`CaptureBackend::resize`] directly. A caller that
    /// resizes the backend behind the fallback's back leaves it judging later
    /// replacements against a size the recording no longer uses, and the first
    /// symptom would be a replacement refused for matching the frames exactly.
    ///
    /// # Errors
    ///
    /// Whatever the backend says. A backend that cannot resize is not recovered
    /// from here: a resize is already a decision point for the caller
    /// ([issue #184](https://github.com/wildware-uk/clipped/issues/184)), and
    /// taking it away from them would be this module deciding what happens to
    /// the file.
    pub fn resize(
        &mut self,
        backend: &mut dyn CaptureBackend,
        size: FrameSize,
    ) -> Result<FrameFormat, CaptureError> {
        let format = backend.resize(size)?;
        self.committed = format;
        self.watch.reset();
        tracing::info!(
            capture_backend = self.current.log_value(),
            width = format.size().width(),
            height = format.size().height(),
            "the capture was resized; a replacement backend must now produce this size"
        );
        Ok(format)
    }

    /// Recovers from a failure of the running backend.
    ///
    /// `failed` is that backend, by value: it is shut down here, before anything
    /// asks the platform for a replacement, and the caller cannot go on using
    /// it. `cause` is the error it failed with, which decides what happens — see
    /// [`response_to`].
    ///
    /// # Errors
    ///
    /// [`FallbackError::Unrecoverable`] when no backend could do better, and
    /// [`FallbackError::Exhausted`] when they have all been tried. Either way
    /// the recording is over and the caller finalises the file.
    pub fn recover(
        &mut self,
        failed: Box<dyn CaptureBackend>,
        cause: CaptureError,
    ) -> Result<Recovery, FallbackError> {
        let response = response_to(&cause);
        if response == FailureResponse::Fatal {
            // Deliberately not reported as "no backend was available": the
            // target itself has gone, and saying anything else sends whoever
            // reads the log looking for a capture bug (AGENTS.md section 15).
            drop(failed);
            tracing::info!(
                capture_backend = self.current.log_value(),
                reason = %cause,
                "the capture cannot be recovered by any backend; the recording ends here"
            );
            return Err(FallbackError::Unrecoverable(cause));
        }

        let reason = cause.to_string();
        self.replace(
            failed,
            FallbackTrigger::CaptureFailed,
            Some(cause),
            reason,
            response == FailureResponse::Restart,
        )
    }

    /// Recovers from a capture that is producing nothing but black frames.
    ///
    /// The same as [`recover`](Self::recover), for the failure that reports no
    /// error: `run` is what [`inspect`](Self::inspect) returned. The failed
    /// backend is not restarted first — it is running, and running is the
    /// problem — so this always looks for a different method, and under
    /// [`CaptureMethodSetting::Forced`] there is none, which is reported rather
    /// than overridden.
    ///
    /// # Errors
    ///
    /// [`FallbackError::Exhausted`] when no other backend can take over.
    pub fn recover_from_black_frames(
        &mut self,
        failed: Box<dyn CaptureBackend>,
        run: BlackRun,
    ) -> Result<Recovery, FallbackError> {
        self.replace(
            failed,
            FallbackTrigger::BlackFrames,
            None,
            run.to_string(),
            false,
        )
    }

    /// Shuts the failed backend down and finds one that can carry on.
    fn replace(
        &mut self,
        mut failed: Box<dyn CaptureBackend>,
        trigger: FallbackTrigger,
        cause: Option<CaptureError>,
        reason: String,
        restart_first: bool,
    ) -> Result<Recovery, FallbackError> {
        let failing = self.current;

        // Released before anything else asks for a capture: DXGI gives a process
        // one duplication per display, so a replacement would be refused while
        // this one still held it (docs/capture-pipeline.md).
        failed.shut_down();
        drop(failed);

        tracing::warn!(
            capture_backend = failing.log_value(),
            trigger = trigger.log_value(),
            reason = %reason,
            restarts_used = self.restarts,
            "the capture backend stopped working; looking for one that can take over"
        );

        self.watch.reset();
        self.silence = Duration::ZERO;
        self.silence_reported = Duration::ZERO;
        let mut attempts: Vec<Attempt> = Vec::new();

        if restart_first && self.restarts < RESTARTS_PER_METHOD {
            self.restarts += 1;
            match self.open_committed(failing) {
                Ok(backend) => {
                    tracing::warn!(
                        capture_backend = failing.log_value(),
                        trigger = trigger.log_value(),
                        restarts_used = self.restarts,
                        "the capture backend was restarted and the recording continues"
                    );
                    return Ok(self.took_over(backend, failing, failing, trigger, reason));
                }
                Err(OpenFailure::Fatal(error)) => return Err(FallbackError::Unrecoverable(error)),
                Err(failure) => attempts.push(Attempt::new(failing, failure.reason())),
            }
        }

        self.retired.push(failing);

        loop {
            let method = match choose(&self.candidates, &self.retired, &self.target, self.setting) {
                Ok(selection) => selection.method(),
                Err(FallbackError::Selection(SelectionError::NoBackendAvailable {
                    considered,
                })) => {
                    // Why each remaining candidate declined is the useful half
                    // of that error, and reporting only "there was nothing left"
                    // would throw it away.
                    attempts.extend(considered.iter().filter_map(|candidate| {
                        match candidate.outcome() {
                            Outcome::Rejected(rejection) => {
                                Some(Attempt::new(candidate.method(), rejection.to_string()))
                            }
                            Outcome::Selected => None,
                        }
                    }));
                    return Err(self.exhausted(trigger, cause, attempts, failing));
                }
                Err(_) => return Err(self.exhausted(trigger, cause, attempts, failing)),
            };

            match self.open_committed(method) {
                Ok(backend) => {
                    tracing::warn!(
                        capture_backend = method.log_value(),
                        previous_capture_backend = failing.log_value(),
                        trigger = trigger.log_value(),
                        "another capture backend took over and the recording continues"
                    );
                    return Ok(self.took_over(backend, failing, method, trigger, reason));
                }
                Err(OpenFailure::Fatal(error)) => return Err(FallbackError::Unrecoverable(error)),
                Err(failure) => {
                    let why = failure.reason();
                    tracing::warn!(
                        capture_backend = method.log_value(),
                        reason = %why,
                        "a capture backend could not take over"
                    );
                    attempts.push(Attempt::new(method, why));
                    self.retired.push(method);
                }
            }
        }
    }

    /// Opens one backend, insisting on the format this recording committed to.
    fn open_committed(
        &self,
        method: CaptureMethod,
    ) -> Result<Box<dyn CaptureBackend>, OpenFailure> {
        open(
            &self.candidates,
            method,
            &self.target,
            &self.config,
            Some(self.committed),
        )
        .map(|(backend, _)| backend)
    }

    /// Records a successful restart or replacement.
    fn took_over(
        &mut self,
        backend: Box<dyn CaptureBackend>,
        from: CaptureMethod,
        to: CaptureMethod,
        trigger: FallbackTrigger,
        reason: String,
    ) -> Recovery {
        self.current = to;
        let change = MethodChange {
            from,
            to,
            trigger,
            reason,
        };
        self.changes.push(change.clone());
        Recovery { backend, change }
    }

    /// Reports that the recording is over, having said what was tried.
    fn exhausted(
        &self,
        trigger: FallbackTrigger,
        cause: Option<CaptureError>,
        attempts: Vec<Attempt>,
        failing: CaptureMethod,
    ) -> FallbackError {
        tracing::error!(
            capture_backend = failing.log_value(),
            trigger = trigger.log_value(),
            candidates_tried = attempts.len(),
            "no capture backend could take over; the recording ends here"
        );
        FallbackError::Exhausted {
            trigger,
            cause,
            attempts,
        }
    }
}

/// Chooses the most preferred candidate that has not been retired.
fn choose(
    candidates: &[&dyn CaptureBackendFactory],
    retired: &[CaptureMethod],
    target: &CaptureTarget,
    setting: CaptureMethodSetting,
) -> Result<Selection, FallbackError> {
    let declarations: Vec<&dyn BackendDeclaration> = candidates
        .iter()
        .filter(|candidate| !retired.contains(&candidate.method()))
        .map(|candidate| *candidate as &dyn BackendDeclaration)
        .collect();
    select(&declarations, target.properties(), setting).map_err(FallbackError::Selection)
}

/// Creates and initialises one backend, refusing one whose frames the recording
/// cannot use.
///
/// `committed` is the format the recording is already writing, or [`None`] for
/// the first backend of a recording — which defines the format rather than
/// having to match one.
fn open(
    candidates: &[&dyn CaptureBackendFactory],
    method: CaptureMethod,
    target: &CaptureTarget,
    config: &CaptureConfig,
    committed: Option<FrameFormat>,
) -> Result<(Box<dyn CaptureBackend>, FrameFormat), OpenFailure> {
    let factory = candidates
        .iter()
        .find(|candidate| candidate.method() == method)
        .ok_or(OpenFailure::NotRegistered(method))?;

    let mut backend = factory.create().map_err(OpenFailure::classify)?;
    let format = backend
        .initialise(target, config)
        .map_err(OpenFailure::classify)?;

    if let Some(committed) = committed {
        if format != committed {
            backend.shut_down();
            return Err(OpenFailure::WrongFormat {
                produced: format,
                committed,
            });
        }
    }
    Ok((backend, format))
}

/// Why one backend could not be opened.
#[derive(Debug)]
enum OpenFailure {
    /// The failure is not one another backend would survive either.
    Fatal(CaptureError),
    /// It could not be created or initialised.
    Refused(CaptureError),
    /// It started, but produces frames this recording cannot use.
    WrongFormat {
        produced: FrameFormat,
        committed: FrameFormat,
    },
    /// The candidate list has no factory for the method selection named. A
    /// registry bug rather than a capture failure: selection only ever names a
    /// method it was handed a candidate for.
    NotRegistered(CaptureMethod),
}

impl OpenFailure {
    fn classify(error: CaptureError) -> Self {
        match response_to(&error) {
            FailureResponse::Fatal => Self::Fatal(error),
            FailureResponse::Restart | FailureResponse::FallBack => Self::Refused(error),
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Fatal(error) | Self::Refused(error) => error.to_string(),
            Self::WrongFormat {
                produced,
                committed,
            } => format!(
                "it would produce {produced} frames, and this recording's video track is fixed \
                 at {committed}"
            ),
            Self::NotRegistered(method) => format!("this build has no {method} backend to create"),
        }
    }

    fn into_cause(self) -> Option<CaptureError> {
        match self {
            Self::Fatal(error) | Self::Refused(error) => Some(error),
            Self::WrongFormat { .. } | Self::NotRegistered(_) => None,
        }
    }
}

/// The sampler black-frame detection reads pixels with on this platform.
#[cfg(windows)]
fn default_frame_sampler() -> Option<Box<dyn FrameSampler>> {
    Some(Box::new(crate::windows::D3d11FrameSampler::new()))
}

/// No platform but Windows has a capture backend in this build, so none has a
/// sampler either. A fallback here still restarts and replaces backends; it just
/// has no pixels to look at, and says so by never reporting a black run rather
/// than by reporting one.
#[cfg(not(windows))]
fn default_frame_sampler() -> Option<Box<dyn FrameSampler>> {
    None
}

#[cfg(test)]
mod tests;
