//! Taking a screenshot when nothing is being recorded.
//!
//! # Why this path exists at all
//!
//! Because a screenshot key that only works while recording is a key nobody
//! trusts. Somebody in a menu, on a loading screen, or playing a game Clipped
//! is not set to record presses it and expects a picture. Refusing would be
//! defensible and it would be wrong.
//!
//! # What it costs, and why the other path is worth having
//!
//! Everything a recording does before its first frame, minus the encoder:
//!
//! ```text
//! select a backend        pure, from declarations; no GPU work
//! create it               a Direct3D 11 device — tens of milliseconds, more on a cold driver
//! initialise it           a frame pool, or a duplication of the output
//! wait for a frame        a source only produces one when its content changes
//! copy it                 one texture copy and one map
//! shut it down            the pool or the duplication is released
//! ```
//!
//! Measured on the machine in `docs/screenshots.md`, that is a couple of
//! hundred milliseconds for a window that is drawing, against about five for a
//! screenshot taken from a recording that is already running. The wait for the
//! frame is the part that is not under Clipped's control and the part that can
//! be unbounded: a window that is not redrawing produces nothing, so this gives
//! up ([`ScreenshotError::NoFrame`]) rather than waiting for ever.
//!
//! There is one thing it costs that is worth stating separately, because it is
//! not time. Desktop Duplication is exclusive per output: while this holds a
//! duplication, nothing else can. It is held for the length of one frame and
//! released, which is short — but it is a reason this path is the fallback and
//! not the design.
//!
//! # Threading
//!
//! The calling thread does all of it and blocks for the whole of it. That is
//! the right thread: it is the one answering the command, and it is emphatically
//! not a capture thread — a recording that is running has its own, and this path
//! only runs when none is.

use core::time::Duration;
use std::time::Instant;

use clipped_capture::windows::D3d11StillCopier;
use clipped_capture::{
    registered_backend, registered_declarations, select, Acquisition, CaptureConfig, CaptureError,
    CaptureMethodSetting, StillFrame,
};

use super::ScreenshotError;
use crate::settings::CaptureTargetSettings;
use crate::SessionError;

/// How long one acquisition waits for a frame.
///
/// The same tenth of a second `crate::recording` uses, and for the same reason:
/// it bounds how long this is inside `acquire` rather than setting a frame rate.
const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(100);

/// How long a screenshot waits for the window to draw something.
///
/// Two seconds, the same as [`super::DEFAULT_TIMEOUT`]. A recording waits ten
/// (`crate::recording::FIRST_FRAME_TIMEOUT`) because a recording that gives up
/// has lost the session; a screenshot that gives up has lost a picture the user
/// can take again immediately, and they are standing at the keyboard waiting for
/// it.
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

/// Opens a capture of `target`, takes one frame, and shuts it down.
///
/// The path for a screenshot with no recording running. Prefer the frame a
/// running recording already has: [`super::ScreenshotRequests`] costs a texture
/// copy where this costs a device, a frame pool and a wait.
///
/// # Errors
///
/// [`ScreenshotError::Capture`] if no backend in this build will capture the
/// target or the one chosen would not start, [`ScreenshotError::NoFrame`] if
/// the target produced nothing within [`FIRST_FRAME_TIMEOUT`] — a window that
/// is not redrawing does exactly that — and [`ScreenshotError::Copy`] if the
/// frame could not be read out of the GPU.
pub fn capture_still(target: &CaptureTargetSettings) -> Result<StillFrame, ScreenshotError> {
    capture_still_within(target, FIRST_FRAME_TIMEOUT)
}

/// The same, waiting `timeout` for the first frame.
///
/// # Errors
///
/// As [`capture_still`].
pub fn capture_still_within(
    target: &CaptureTargetSettings,
    timeout: Duration,
) -> Result<StillFrame, ScreenshotError> {
    let selection = select(
        &registered_declarations(),
        &target.properties()?,
        CaptureMethodSetting::Automatic,
    )
    .map_err(SessionError::from)?;
    let method = selection.method();

    let mut backend = registered_backend(method)
        .ok_or_else(|| SessionError::BackendNotRegistered {
            method: method.to_string(),
        })?
        .create()
        .map_err(SessionError::from)?;

    // Every path out of the loop below shuts the backend down, so it is written
    // once here around a closure rather than at each `return`: a frame pool or a
    // duplication left open by an error path is a resource nobody gets back
    // until the process exits (AGENTS.md section 58).
    let outcome = one_frame(backend.as_mut(), target, timeout);
    backend.shut_down();
    outcome
}

/// Initialises `backend`, waits for a frame, and copies it out.
fn one_frame(
    backend: &mut dyn clipped_capture::CaptureBackend,
    target: &CaptureTargetSettings,
    timeout: Duration,
) -> Result<StillFrame, ScreenshotError> {
    backend
        .initialise(
            &target.target()?,
            // No cursor. A screenshot with a mouse pointer sitting in it is a
            // screenshot somebody has to take again, and the pointer is not part
            // of the game. A recording captures it or not by the user's setting;
            // here there is no frame to lose by choosing.
            &CaptureConfig::default().with_capture_cursor(false),
        )
        .map_err(SessionError::from)?;

    let mut copier = D3d11StillCopier::new();
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        match backend.acquire(ACQUIRE_TIMEOUT) {
            Ok(Acquisition::Frame(frame)) => {
                copier.begin(&frame)?;
                // Blocking rather than polling: there is no recording to protect
                // here, the capture exists only for this picture, and waiting a
                // couple of milliseconds for the GPU is cheaper than acquiring
                // another frame in order to avoid it.
                return Ok(copier.finish()?);
            }
            // The window has not drawn since capture started. Ask again.
            //
            // A minimised window is the same answer here and not a separate
            // one: a screenshot has nothing to preserve, so there is no
            // recording to keep going and nothing a stretch of silence has to be
            // explained against. Waiting it out and reporting `NoFrame` — which
            // already says "a window that is not drawing produces none" — is the
            // whole of what this can usefully do (issue #383 changes what a
            // *recording* does about it, which has footage to lose).
            Ok(Acquisition::Timeout | Acquisition::TargetMinimised) => {}
            // The target changed size between being measured and being
            // captured. The next acquisition carries the new one; there is
            // nothing to reconfigure, because nothing has been encoded.
            Ok(Acquisition::SizeChanged(_)) => {}
            Err(CaptureError::TargetLost { .. }) => {
                return Err(ScreenshotError::NoFrame { waited: timeout })
            }
            Err(error) => return Err(ScreenshotError::Capture(error.into())),
        }
    }

    Err(ScreenshotError::NoFrame { waited: timeout })
}
