//! What the fallback policy does, against backends that fail on cue.
//!
//! The backends here are fakes, for the same reason the ones in
//! [`crate::selection`](crate::select)'s tests are: what is under test is the
//! *policy* — which failures are worth another backend, which are not, what a
//! replacement has to produce before it is allowed to take over, and what is
//! reported afterwards — and a backend that fails when told to is exactly the
//! input that policy consumes. A real backend cannot be made to have a driver
//! reset on the fourth frame.
//!
//! Real capture is exercised against real Windows APIs elsewhere in this crate,
//! and the one part of black-frame detection that reads real pixels — the
//! Direct3D sampler — is tested against a real GPU texture in
//! `crate::windows::pixel_sample`. Nothing here is production code or a
//! placeholder for any.

use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;
use std::collections::VecDeque;
use std::sync::Mutex;

use super::*;
use crate::{
    Acquisition, Availability, BackendCapabilities, CaptureTimestamp, FrameSample, PixelFormat,
    SourceClock, TargetHandle, TargetKind, TargetProperties, TextureKind, Unavailable,
};

const HD: (u32, u32) = (1920, 1080);
const SMALLER: (u32, u32) = (1280, 720);

fn size(dimensions: (u32, u32)) -> FrameSize {
    FrameSize::new(dimensions.0, dimensions.1).expect("a test size is not zero")
}

fn format(dimensions: (u32, u32)) -> FrameFormat {
    FrameFormat::new(size(dimensions), PixelFormat::Bgra8Unorm)
}

fn window() -> CaptureTarget {
    CaptureTarget::new(
        TargetHandle::from_raw(0x1234),
        TargetProperties::new(TargetKind::Window, size(HD)),
    )
}

/// A failure that means "try a different backend".
fn unsupported(method: CaptureMethod) -> CaptureError {
    CaptureError::UnsupportedTarget {
        method,
        target: TargetKind::Window,
        reason: "the window has opted out of being captured",
    }
}

/// A failure that means "restart this one".
fn interrupted(method: CaptureMethod) -> CaptureError {
    CaptureError::Interrupted {
        method,
        reason: "the display adapter was reset",
    }
}

fn wgc_unsupported() -> CaptureError {
    unsupported(CaptureMethod::WindowsGraphicsCapture)
}

fn wgc_interrupted() -> CaptureError {
    interrupted(CaptureMethod::WindowsGraphicsCapture)
}

fn wgc_target_lost() -> CaptureError {
    CaptureError::TargetLost {
        method: CaptureMethod::WindowsGraphicsCapture,
    }
}

/// What one created backend does when it is asked for frames.
#[derive(Debug, Clone)]
enum Step {
    /// Hand over a frame.
    Frame,
    /// Fail, with the error this returns.
    Fails(fn() -> CaptureError),
}

/// What the next `create` produces.
#[derive(Debug, Clone)]
enum Plan {
    /// A backend that initialises to `format` and then works through `steps`,
    /// handing over frames for ever once they run out.
    Runs {
        format: FrameFormat,
        steps: Vec<Step>,
    },
    /// A backend whose `initialise` fails.
    FailsToStart(fn() -> CaptureError),
}

/// A backend that does what a test told it to.
#[derive(Debug)]
struct ScriptedBackend {
    method: CaptureMethod,
    format: FrameFormat,
    steps: VecDeque<Step>,
    fails_to_start: Option<fn() -> CaptureError>,
    initialised: bool,
    next_timestamp: u64,
    frame_interval: Duration,
    shut_downs: &'static AtomicU32,
}

impl CaptureBackend for ScriptedBackend {
    fn method(&self) -> CaptureMethod {
        self.method
    }

    fn initialise(
        &mut self,
        _target: &CaptureTarget,
        _config: &CaptureConfig,
    ) -> Result<FrameFormat, CaptureError> {
        if let Some(failure) = self.fails_to_start {
            return Err(failure());
        }
        self.initialised = true;
        Ok(self.format)
    }

    fn acquire(&mut self, _timeout: Duration) -> Result<Acquisition<'_>, CaptureError> {
        if !self.initialised {
            return Err(CaptureError::NotInitialised {
                method: self.method,
            });
        }
        match self.steps.pop_front().unwrap_or(Step::Frame) {
            Step::Fails(error) => Err(error()),
            Step::Frame => {
                let timestamp = CaptureTimestamp::from_source(
                    SourceClock::PerformanceCounter,
                    self.next_timestamp,
                );
                self.next_timestamp += u64::try_from(self.frame_interval.as_nanos())
                    .expect("a test frame interval fits in 64 bits");

                // SAFETY: nothing in these tests calls `as_raw`, let alone
                // dereferences the handle, so the validity requirement is
                // vacuous — there is no Direct3D device in a unit test. The
                // same construction is used by `crate::frame`'s own tests.
                let texture = unsafe {
                    crate::FrameTexture::new(TextureKind::D3d11Texture2D, core::ptr::null_mut())
                };
                Ok(Acquisition::Frame(CapturedFrame::new(
                    texture,
                    self.format,
                    timestamp,
                )))
            }
        }
    }

    fn resize(&mut self, new_size: FrameSize) -> Result<FrameFormat, CaptureError> {
        self.format = FrameFormat::new(new_size, self.format.pixel_format());
        Ok(self.format)
    }

    fn shut_down(&mut self) {
        self.initialised = false;
        self.shut_downs.fetch_add(1, Ordering::Relaxed);
    }
}

/// A registered backend that creates [`ScriptedBackend`]s.
#[derive(Debug)]
struct FakeFactory {
    method: CaptureMethod,
    capabilities: BackendCapabilities,
    availability: Availability,
    plans: Mutex<VecDeque<Plan>>,
    default_format: FrameFormat,
    frame_interval: Duration,
    creations: AtomicU32,
    shut_downs: &'static AtomicU32,
}

impl FakeFactory {
    fn new(method: CaptureMethod, shut_downs: &'static AtomicU32) -> Self {
        Self {
            method,
            capabilities: BackendCapabilities::new(true, true),
            availability: Availability::Available,
            plans: Mutex::new(VecDeque::new()),
            default_format: format(HD),
            frame_interval: Duration::from_millis(16),
            creations: AtomicU32::new(0),
            shut_downs,
        }
    }

    fn planning(self, plans: impl IntoIterator<Item = Plan>) -> Self {
        *self.plans.lock().expect("no test panics holding this lock") = plans.into_iter().collect();
        self
    }

    fn unavailable(mut self) -> Self {
        self.availability = Availability::Unavailable(Unavailable::NotImplemented);
        self
    }

    fn producing(mut self, dimensions: (u32, u32)) -> Self {
        self.default_format = format(dimensions);
        self
    }

    fn every(mut self, frame_interval: Duration) -> Self {
        self.frame_interval = frame_interval;
        self
    }

    fn creations(&self) -> u32 {
        self.creations.load(Ordering::Relaxed)
    }
}

impl BackendDeclaration for FakeFactory {
    fn method(&self) -> CaptureMethod {
        self.method
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.capabilities
    }

    fn availability(&self, _target: &TargetProperties) -> Availability {
        self.availability
    }
}

impl CaptureBackendFactory for FakeFactory {
    fn create(&self) -> Result<Box<dyn CaptureBackend>, CaptureError> {
        self.creations.fetch_add(1, Ordering::Relaxed);
        let plan = self
            .plans
            .lock()
            .expect("no test panics holding this lock")
            .pop_front()
            .unwrap_or(Plan::Runs {
                format: self.default_format,
                steps: Vec::new(),
            });

        let (format, steps, fails_to_start) = match plan {
            Plan::Runs { format, steps } => (format, steps, None),
            Plan::FailsToStart(error) => (self.default_format, Vec::new(), Some(error)),
        };

        Ok(Box::new(ScriptedBackend {
            method: self.method,
            format,
            steps: steps.into(),
            fails_to_start,
            initialised: false,
            next_timestamp: 0,
            frame_interval: self.frame_interval,
            shut_downs: self.shut_downs,
        }))
    }
}

/// A sampler that reports the same thing about every frame.
#[derive(Debug)]
struct ScriptedSampler(FrameSample);

impl FrameSampler for ScriptedSampler {
    fn sample(&mut self, _frame: &CapturedFrame<'_>) -> Option<FrameSample> {
        Some(self.0)
    }
}

fn all_black() -> FrameSample {
    (0..16).fold(FrameSample::empty(), |sample, _| sample.with_pixel(0, 0, 0))
}

fn very_dark() -> FrameSample {
    (0..16).fold(FrameSample::empty(), |sample, _| sample.with_pixel(2, 0, 4))
}

/// Counters live for the process because a `ScriptedBackend` outlives the
/// borrow of the factory that made it — the fallback owns it. Each test leaks
/// its own, so tests never see each other's numbers.
fn counter() -> &'static AtomicU32 {
    Box::leak(Box::new(AtomicU32::new(0)))
}

/// Drives one acquisition and asserts a frame came out of it.
fn expect_frame(backend: &mut dyn CaptureBackend) {
    match backend.acquire(Duration::from_millis(16)) {
        Ok(Acquisition::Frame(_)) => {}
        other => panic!("expected a frame, got {other:?}"),
    }
}

#[test]
fn a_backend_that_fails_mid_recording_is_replaced_and_the_recording_continues() {
    // The acceptance criterion: a forced backend failure results in a working
    // recording via fallback.
    let wgc_shut_downs = counter();
    let wgc = FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, wgc_shut_downs).planning([
        Plan::Runs {
            format: format(HD),
            steps: vec![Step::Frame, Step::Frame, Step::Fails(wgc_unsupported)],
        },
    ]);
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter());
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, frames) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture is available")
    .into_parts();

    assert_eq!(
        fallback.current_method(),
        CaptureMethod::WindowsGraphicsCapture
    );
    assert_eq!(frames, format(HD));
    expect_frame(backend.as_mut());
    expect_frame(backend.as_mut());

    let failure = backend
        .acquire(Duration::from_millis(16))
        .expect_err("the third acquisition was scripted to fail");

    let recovery = fallback
        .recover(backend, failure)
        .expect("Desktop Duplication can take over");
    let (mut backend, change) = recovery.into_parts();

    assert_eq!(change.from(), CaptureMethod::WindowsGraphicsCapture);
    assert_eq!(change.to(), CaptureMethod::DesktopDuplication);
    assert_eq!(change.trigger(), FallbackTrigger::CaptureFailed);
    assert!(!change.is_restart());
    assert_eq!(
        wgc_shut_downs.load(Ordering::Relaxed),
        1,
        "the failed backend must be shut down before another is asked for: DXGI gives a \
         process one duplication per display"
    );

    // The recording continues, which is the whole point.
    expect_frame(backend.as_mut());
    expect_frame(backend.as_mut());

    let status = fallback.status();
    assert_eq!(status.current_method(), CaptureMethod::DesktopDuplication);
    assert_eq!(
        status.initial_method(),
        CaptureMethod::WindowsGraphicsCapture
    );
    assert!(status.has_changed());
    assert_eq!(status.changes().len(), 1);
    assert_eq!(
        status.changes()[0].to_string(),
        "Desktop Duplication took over from Windows Graphics Capture: Windows Graphics Capture \
         cannot capture a window: the window has opted out of being captured"
    );
}

#[test]
fn a_backend_that_cannot_be_started_is_fallen_past_before_the_first_frame() {
    let wgc = FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
        .planning([Plan::FailsToStart(wgc_unsupported)]);
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter());
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let started = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Desktop Duplication starts");

    assert_eq!(started.method(), CaptureMethod::DesktopDuplication);
    let (fallback, _backend, _format) = started.into_parts();

    let status = fallback.status();
    assert_eq!(
        status.initial_method(),
        CaptureMethod::DesktopDuplication,
        "the recording started on Desktop Duplication; the preferred method never ran"
    );
    assert!(
        !status.has_changed(),
        "nothing changed during the recording — the report explains the start instead"
    );
    assert_eq!(status.changes().len(), 1);
    assert_eq!(
        status.changes()[0].trigger(),
        FallbackTrigger::InitialisationFailed
    );
    assert_eq!(
        status.changes()[0].from(),
        CaptureMethod::WindowsGraphicsCapture
    );
}

#[test]
fn a_replacement_that_would_change_the_frame_size_is_refused() {
    // ADR 0001: Matroska fixes a track's dimensions in the header, so a
    // replacement producing a different size cannot continue this file. The
    // recording ends where it is, and the report says exactly why.
    let wgc =
        FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter()).planning([Plan::Runs {
            format: format(HD),
            steps: vec![Step::Fails(wgc_unsupported)],
        }]);
    let duplication_shut_downs = counter();
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, duplication_shut_downs)
        .producing(SMALLER);
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    let failure = backend
        .acquire(Duration::from_millis(16))
        .expect_err("scripted to fail");
    let error = fallback
        .recover(backend, failure)
        .expect_err("the only replacement produces a different size");

    let FallbackError::Exhausted { attempts, .. } = &error else {
        panic!("expected Exhausted, got {error:?}");
    };
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].method(), CaptureMethod::DesktopDuplication);
    assert_eq!(
        attempts[0].reason(),
        "it would produce 1280x720 BGRA8 unorm frames, and this recording's video track is \
         fixed at 1920x1080 BGRA8 unorm"
    );
    assert_eq!(
        duplication_shut_downs.load(Ordering::Relaxed),
        1,
        "a replacement that is not used must still be shut down, or it holds a duplication \
         nothing will ever release"
    );
    assert_eq!(
        fallback.current_method(),
        CaptureMethod::WindowsGraphicsCapture,
        "nothing took over, so nothing is capturing: the report must not claim otherwise"
    );
}

#[test]
fn a_resize_the_caller_followed_becomes_the_size_a_replacement_must_produce() {
    // The other half of the rule above: once a caller has followed a resize,
    // the committed format is the new one, and a replacement is judged against
    // that rather than against the size the recording started at.
    let wgc =
        FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter()).planning([Plan::Runs {
            format: format(HD),
            steps: vec![Step::Fails(wgc_unsupported)],
        }]);
    let duplication =
        FakeFactory::new(CaptureMethod::DesktopDuplication, counter()).producing(SMALLER);
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    let resized = fallback
        .resize(backend.as_mut(), size(SMALLER))
        .expect("the fake backend resizes");
    assert_eq!(resized, format(SMALLER));
    assert_eq!(fallback.committed_format(), format(SMALLER));

    let failure = backend
        .acquire(Duration::from_millis(16))
        .expect_err("scripted to fail");
    let recovery = fallback
        .recover(backend, failure)
        .expect("Desktop Duplication now produces exactly the committed size");
    assert_eq!(recovery.method(), CaptureMethod::DesktopDuplication);
}

#[test]
fn a_lost_target_is_not_fallen_back_from() {
    let wgc =
        FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter()).planning([Plan::Runs {
            format: format(HD),
            steps: vec![Step::Fails(wgc_target_lost)],
        }]);
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter());
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    let failure = backend
        .acquire(Duration::from_millis(16))
        .expect_err("scripted to fail");
    let error = fallback
        .recover(backend, failure)
        .expect_err("the window closed; no backend can record a window that is gone");

    assert!(
        matches!(
            error,
            FallbackError::Unrecoverable(CaptureError::TargetLost { .. })
        ),
        "a closed window must stay a closed window in the report, not become \
         'no capture backend was available': {error:?}"
    );
    assert_eq!(
        duplication.creations(),
        0,
        "nothing should have been asked to record a window that no longer exists"
    );
}

#[test]
fn an_interrupted_backend_is_restarted_before_another_one_is_tried() {
    let wgc =
        FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter()).planning([Plan::Runs {
            format: format(HD),
            steps: vec![Step::Fails(wgc_interrupted)],
        }]);
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter());
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    let failure = backend
        .acquire(Duration::from_millis(16))
        .expect_err("scripted to fail");
    let recovery = fallback
        .recover(backend, failure)
        .expect("a driver reset is what restarting is for");

    assert!(recovery.change().is_restart());
    assert_eq!(recovery.method(), CaptureMethod::WindowsGraphicsCapture);
    assert_eq!(
        duplication.creations(),
        0,
        "an interruption means reinitialise this backend, not abandon the preferred one"
    );
    assert_eq!(
        fallback.status().changes()[0].to_string(),
        "Windows Graphics Capture was restarted: Windows Graphics Capture was interrupted: \
         the display adapter was reset"
    );
}

#[test]
fn a_backend_that_keeps_being_interrupted_is_eventually_given_up_on() {
    // Three failures: two restarts, then the method is retired and the next
    // candidate takes over. Without a budget this loops for the length of the
    // recording, restarting a backend that is never going to work.
    let wgc = FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter()).planning(
        core::iter::repeat_n(
            Plan::Runs {
                format: format(HD),
                steps: vec![Step::Fails(wgc_interrupted)],
            },
            3,
        ),
    );
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter());
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    for expected in [
        CaptureMethod::WindowsGraphicsCapture,
        CaptureMethod::WindowsGraphicsCapture,
        CaptureMethod::DesktopDuplication,
    ] {
        let failure = backend
            .acquire(Duration::from_millis(16))
            .expect_err("scripted to fail");
        let recovery = fallback
            .recover(backend, failure)
            .expect("something can carry on");
        assert_eq!(recovery.method(), expected);
        backend = recovery.into_parts().0;
    }

    assert_eq!(fallback.current_method(), CaptureMethod::DesktopDuplication);
    assert_eq!(fallback.changes().len(), 3);
    expect_frame(backend.as_mut());
}

#[test]
fn a_pinned_method_is_restarted_but_never_replaced() {
    // SPEC.md section 8 makes falling back the behaviour of Automatic. A user
    // who pinned a method asked a specific question and gets the answer.
    let wgc =
        FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter()).planning([Plan::Runs {
            format: format(HD),
            steps: vec![Step::Fails(wgc_unsupported)],
        }]);
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter());
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Forced(CaptureMethod::WindowsGraphicsCapture),
    )
    .expect("the pinned backend starts")
    .into_parts();

    let failure = backend
        .acquire(Duration::from_millis(16))
        .expect_err("scripted to fail");
    let error = fallback
        .recover(backend, failure)
        .expect_err("there is nothing else this setting allows");

    assert!(
        matches!(error, FallbackError::Exhausted { .. }),
        "{error:?}"
    );
    assert_eq!(
        duplication.creations(),
        0,
        "a pinned method must not be quietly swapped for a different one"
    );
    assert_eq!(
        fallback.status().to_string(),
        "Capture method: Windows Graphics Capture\nCurrent method: Windows Graphics Capture"
    );
}

#[test]
fn nothing_left_to_try_says_so_rather_than_naming_nothing() {
    let wgc =
        FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter()).planning([Plan::Runs {
            format: format(HD),
            steps: vec![Step::Fails(wgc_unsupported)],
        }]);
    let candidates: [&dyn CaptureBackendFactory; 1] = [&wgc];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    let failure = backend
        .acquire(Duration::from_millis(16))
        .expect_err("scripted to fail");
    let error = fallback
        .recover(backend, failure)
        .expect_err("this build has one backend and it has failed");

    assert_eq!(
        error.to_string(),
        "Windows Graphics Capture cannot capture a window: the window has opted out of being \
         captured; there was no other capture backend to try",
        "a bug report that says only 'capture failed' cannot be diagnosed"
    );
}

#[test]
fn a_candidate_that_declines_the_target_is_reported_with_its_reason() {
    let wgc =
        FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter()).planning([Plan::Runs {
            format: format(HD),
            steps: vec![Step::Fails(wgc_unsupported)],
        }]);
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter()).unavailable();
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    let failure = backend
        .acquire(Duration::from_millis(16))
        .expect_err("scripted to fail");
    let error = fallback
        .recover(backend, failure)
        .expect_err("the only other backend declines this target");

    let FallbackError::Exhausted { attempts, .. } = &error else {
        panic!("expected Exhausted, got {error:?}");
    };
    assert_eq!(
        attempts,
        &[Attempt::new(
            CaptureMethod::DesktopDuplication,
            "not implemented in this build".to_owned()
        )],
        "the reason a candidate declined is the useful half of the report"
    );
    assert_eq!(
        duplication.creations(),
        0,
        "a backend that declares itself unavailable must not be created"
    );
}

#[test]
fn a_capture_that_is_all_black_is_noticed_and_fallen_back_from() {
    let wgc = FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
        .every(Duration::from_millis(100));
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter());
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();
    fallback.set_frame_sampler(Some(Box::new(ScriptedSampler(all_black()))));
    fallback.set_black_frame_watch(BlackFrameWatch::new(
        Duration::from_millis(100),
        Duration::from_secs(1),
    ));

    let mut reported = None;
    for frame_number in 0..12 {
        let Ok(Acquisition::Frame(frame)) = backend.acquire(Duration::from_millis(100)) else {
            panic!("the fake backend produces frames");
        };
        if let Some(run) = fallback.inspect(&frame) {
            reported = Some((frame_number, run));
            break;
        }
    }

    let (frame_number, run) = reported.expect("a second of black must be noticed");
    assert_eq!(frame_number, 10, "a second at 100 ms a frame is ten frames");
    assert_eq!(run.length(), Duration::from_secs(1));

    let recovery = fallback
        .recover_from_black_frames(backend, run)
        .expect("Desktop Duplication can take over");
    assert_eq!(recovery.method(), CaptureMethod::DesktopDuplication);
    assert_eq!(
        recovery.change().trigger(),
        FallbackTrigger::BlackFrames,
        "the report has to say the recording was black, not that capture errored"
    );
    assert_eq!(
        recovery.change().reason(),
        "every pixel sampled was black for 1.0 s, across 11 sampled frames"
    );
}

#[test]
fn a_very_dark_capture_is_left_alone() {
    // The false positive that would matter most: a night-time game, recorded
    // for two minutes, on a watch configured to be as impatient as the tests
    // above. Nothing about it is a failure.
    let wgc = FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
        .every(Duration::from_millis(100));
    let candidates: [&dyn CaptureBackendFactory; 1] = [&wgc];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();
    fallback.set_frame_sampler(Some(Box::new(ScriptedSampler(very_dark()))));
    fallback.set_black_frame_watch(BlackFrameWatch::new(
        Duration::from_millis(100),
        Duration::from_secs(1),
    ));

    for _ in 0..1_200 {
        let Ok(Acquisition::Frame(frame)) = backend.acquire(Duration::from_millis(100)) else {
            panic!("the fake backend produces frames");
        };
        assert_eq!(
            fallback.inspect(&frame),
            None,
            "a pixel of 2,0,4 is dark, not absent"
        );
    }
    assert_eq!(
        fallback.current_method(),
        CaptureMethod::WindowsGraphicsCapture
    );
}

#[test]
fn silence_is_recorded_and_not_acted_on() {
    // A minimised window produces no frames for as long as it stays minimised,
    // and both backends wait it out. Falling back here would cost a user their
    // preferred backend every time they alt-tabbed.
    let wgc = FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter());
    let duplication = FakeFactory::new(CaptureMethod::DesktopDuplication, counter());
    let candidates: [&dyn CaptureBackendFactory; 2] = [&wgc, &duplication];

    let (mut fallback, _backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    for _ in 0..600 {
        fallback.note_silence(Duration::from_millis(100));
    }
    assert_eq!(fallback.silent_for(), Duration::from_secs(60));
    assert_eq!(
        fallback.current_method(),
        CaptureMethod::WindowsGraphicsCapture
    );
    assert_eq!(duplication.creations(), 0);
    assert!(fallback.changes().is_empty());
}

#[test]
fn a_frame_ends_the_silence() {
    let wgc = FakeFactory::new(CaptureMethod::WindowsGraphicsCapture, counter());
    let candidates: [&dyn CaptureBackendFactory; 1] = [&wgc];

    let (mut fallback, mut backend, _) = CaptureFallback::start(
        &candidates,
        &window(),
        &CaptureConfig::default(),
        CaptureMethodSetting::Automatic,
    )
    .expect("Windows Graphics Capture starts")
    .into_parts();

    fallback.note_silence(Duration::from_secs(5));
    assert_eq!(fallback.silent_for(), Duration::from_secs(5));

    let Ok(Acquisition::Frame(frame)) = backend.acquire(Duration::from_millis(16)) else {
        panic!("the fake backend produces frames");
    };
    fallback.inspect(&frame);
    assert_eq!(fallback.silent_for(), Duration::ZERO);
}

#[test]
fn every_failure_has_a_documented_response() {
    let method = CaptureMethod::WindowsGraphicsCapture;
    assert_eq!(
        response_to(&CaptureError::TargetLost { method }),
        FailureResponse::Fatal
    );
    assert_eq!(
        response_to(&CaptureError::NotInitialised { method }),
        FailureResponse::Fatal
    );
    assert_eq!(
        response_to(&CaptureError::AlreadyInitialised { method }),
        FailureResponse::Fatal
    );
    assert_eq!(
        response_to(&unsupported(method)),
        FailureResponse::FallBack,
        "another backend may well capture a target this one cannot"
    );
    assert_eq!(
        response_to(&interrupted(method)),
        FailureResponse::Restart,
        "an interruption is documented as meaning reinitialise this backend"
    );
    assert_eq!(
        response_to(&CaptureError::Backend {
            method,
            operation: "creating the frame pool",
            source: Box::new(std::io::Error::other("DXGI_ERROR_DEVICE_REMOVED")),
        }),
        FailureResponse::Restart
    );
}
