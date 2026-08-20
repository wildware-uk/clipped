//! The machine resources a hardware test cannot share, and how to wait for one.
//!
//! [Issue #194](https://github.com/wildware-uk/clipped/issues/194). Several
//! things this repository's tests use are exclusive **per process** or
//! **machine-wide**, so a second `cargo test` binary is not an independent
//! observer — it is a competitor:
//!
//! | Resource | Scope | What the loser sees |
//! | --- | --- | --- |
//! | Desktop Duplication of an output | one per process | `DuplicateOutput` refused, `E_INVALIDARG` |
//! | Exclusive fullscreen on an output | one per machine | `SetFullscreenState` refused, DXGI `0x887A0022` |
//! | The foreground window | one per machine | `SetForegroundWindow` refused |
//! | The default audio endpoint | one per machine | another suite's tones in your capture |
//!
//! Every one of those failures reads as a limitation of the machine. That is
//! the damage: a contended run does not say it was contended, it says Desktop
//! Duplication does not work here, or reports a frame count that was really a
//! measurement of two suites sharing a GPU. #194 lists the reviews and the
//! measurements this has already cost.
//!
//! # Why a named mutex
//!
//! Issue #13 found the first of these and added a `Mutex`, which is the right
//! answer for two threads and no answer at all for two processes. A Windows
//! named mutex is visible to every process in the session, and — the part a
//! lock file cannot do — **Windows releases it when the holder dies**. A test
//! binary killed part way through does not leave the next run waiting for a
//! lock nobody holds; the next waiter is told the mutex was abandoned, which is
//! [`Exclusive::acquire`] returning the lock rather than an error.
//!
//! # One lock per resource, not one lock
//!
//! Duplicating an output, owning the foreground and owning the default audio
//! endpoint are three separate exclusions. A single global lock would serialise
//! a test that needs the foreground behind one that needs an audio endpoint,
//! which is a slower suite for no gain in correctness. [`Resource`] names them
//! apart.
//!
//! # What this does not do
//!
//! Make a contended run correct. It makes it **wait**, and if it cannot have
//! the resource in time it makes it say so, naming the resource and what holds
//! it. A test that then reports a hardware limitation is reporting one that is
//! real.

#![cfg(windows)]

use core::fmt;
use core::time::Duration;

use windows::core::HSTRING;
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

/// How long a test waits for a resource before giving up, when it does not say.
///
/// Longer than any single hardware test in this repository takes to release
/// what it holds — the longest is the A/V sync run, and it holds the audio
/// endpoint for minutes rather than the whole run — and short enough that a
/// deadlock is reported within a coffee break rather than hanging a suite until
/// somebody notices.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(300);

/// A machine resource that only one process may use at a time.
///
/// Adding one means adding a name, and the name is what makes the exclusion
/// work across processes — two spellings of the same resource are two locks
/// and no exclusion at all, which is why these are an enumeration rather than
/// a string a caller passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// Duplicating one display's output.
    ///
    /// Per *process* rather than per machine: one process may hold the
    /// duplication of a given output, and a second is refused with
    /// `E_INVALIDARG` — which reads exactly like a machine that cannot
    /// duplicate at all. Named by the output so that two tests on two displays
    /// do not wait for each other.
    DesktopDuplication(String),
    /// Holding a display exclusively, which no other process may do meanwhile.
    ExclusiveFullscreen,
    /// Being the foreground window, which Windows grants to one window at a
    /// time and refuses to processes that have not just produced input.
    Foreground,
    /// Rendering to or capturing from the endpoint Windows currently calls the
    /// default.
    ///
    /// Shareable in the sense that Windows mixes it — which is the problem. A
    /// test measuring a tone hears every other suite's tones too, and the
    /// answer it reports is about both.
    DefaultAudioEndpoint,
    /// Capturing a real subject and encoding it, while counting the frames.
    ///
    /// The one entry here that is not a Windows object, and the one measured
    /// most directly. Two suites capturing and encoding at once do not refuse
    /// each other — they *perturb* each other, and the perturbation arrives as
    /// a frame accounting that looks like a defect in the recorder.
    ///
    /// Measured while writing this. `tests/capture/recorded_frames.rs` reads
    /// the source's own counters back out of a finished recording and passes
    /// alone every time; run beside a second capture suite it reported
    ///
    /// ```text
    /// the recording holds the same source frame more than once, which is a
    /// frame written twice rather than a frame dropped
    /// ```
    ///
    /// That sentence is a bug report about the recorder, and it would have been
    /// wrong. The subject stalls under a loaded GPU, capture is handed the same
    /// composed frame twice, and both are encoded — which is the pipeline doing
    /// what it was told with what it was given.
    ///
    /// So this is the exclusion that makes a frame count mean something, and it
    /// is why #194's first acceptance criterion is about two `cargo test`
    /// processes rather than about any single API refusing.
    CaptureMeasurement,
}

impl Resource {
    /// Text that can sit inside an object name.
    ///
    /// A backslash is a separator in the object namespace, so a name that
    /// contains one past the `Local\` prefix names a directory that does not
    /// exist and `CreateMutexW` fails with `ERROR_PATH_NOT_FOUND`.
    ///
    /// Every display name Windows hands out is of the form `\\.\DISPLAY1`, so
    /// this is not an edge case — it is every caller of
    /// [`Resource::DesktopDuplication`], and it is how the first version of
    /// this crate failed.
    fn safe_in_a_name(text: &str) -> String {
        text.replace('\\', "-")
    }

    /// The mutex name, which is what makes two processes agree.
    ///
    /// Session-local rather than `Global\`: these tests run in one interactive
    /// desktop session, `Global\` needs a privilege an ordinary account may not
    /// have, and a lock that fails to be created is worse than one scoped
    /// slightly narrower than the resource.
    fn mutex_name(&self) -> String {
        match self {
            Self::DesktopDuplication(output) => {
                format!(
                    "Local\\clipped-test-duplication-{}",
                    Self::safe_in_a_name(output)
                )
            }
            Self::ExclusiveFullscreen => "Local\\clipped-test-exclusive-fullscreen".to_owned(),
            Self::Foreground => "Local\\clipped-test-foreground".to_owned(),
            Self::DefaultAudioEndpoint => "Local\\clipped-test-default-audio".to_owned(),
            Self::CaptureMeasurement => "Local\\clipped-test-capture-measurement".to_owned(),
        }
    }
}

impl fmt::Display for Resource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DesktopDuplication(output) => {
                write!(formatter, "Desktop Duplication of {output}")
            }
            Self::ExclusiveFullscreen => formatter.write_str("exclusive fullscreen"),
            Self::Foreground => formatter.write_str("the foreground window"),
            Self::DefaultAudioEndpoint => formatter.write_str("the default audio endpoint"),
            Self::CaptureMeasurement => {
                formatter.write_str("capturing and encoding a subject while counting its frames")
            }
        }
    }
}

/// The resource could not be had.
#[derive(Debug, Clone)]
pub struct Contended {
    resource: Resource,
    waited: Duration,
}

impl Contended {
    /// Which resource.
    #[must_use]
    pub const fn resource(&self) -> &Resource {
        &self.resource
    }
}

impl fmt::Display for Contended {
    /// Says the machine is busy, not that it is incapable.
    ///
    /// The whole point of #194: without this, the caller goes on to fail with
    /// `E_INVALIDARG` or an empty capture and reports a hardware limitation
    /// that is not real.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "another process on this machine has held {} for longer than {:.0}s, so this test \
             could not take it. That is contention rather than a limitation of this machine: \
             something else is running these suites at the same time. Run them one at a time \
             (docs/testing.md), or wait for the other run to finish",
            self.resource,
            self.waited.as_secs_f64()
        )
    }
}

impl std::error::Error for Contended {}

/// A held resource, released when this value is dropped.
///
/// Deliberately **not** [`Send`]. A Windows mutex is owned by the *thread* that
/// waited on it, and `ReleaseMutex` from any other thread fails — so a guard
/// moved between threads would be a lock that is never released until the
/// process exits. The handle being a raw pointer means the compiler refuses it
/// for us, which is the right answer for the wrong reason and is worth saying
/// out loud in case somebody is tempted to `unsafe impl Send` past it.
#[derive(Debug)]
pub struct Exclusive {
    handle: HANDLE,
    resource: Resource,
}

impl Exclusive {
    /// Waits up to [`DEFAULT_WAIT`] for `resource`.
    ///
    /// # Errors
    ///
    /// [`Contended`] when another process held it for the whole wait.
    ///
    /// # Panics
    ///
    /// If Windows will not create the mutex at all, which is not a contended
    /// machine but a broken one, and is worth failing loudly rather than
    /// running the test unprotected.
    pub fn acquire(resource: Resource) -> Result<Self, Contended> {
        Self::acquire_within(resource, DEFAULT_WAIT)
    }

    /// The same, waiting no longer than `timeout`.
    ///
    /// # Errors
    ///
    /// [`Contended`] when another process held it for the whole of `timeout`.
    ///
    /// # Panics
    ///
    /// If Windows will not create the mutex.
    pub fn acquire_within(resource: Resource, timeout: Duration) -> Result<Self, Contended> {
        let name = HSTRING::from(resource.mutex_name());
        // SAFETY: the name is a live wide string for the length of the call.
        // `CreateMutexW` opens the existing mutex when one of that name exists,
        // which is how two processes come to share one, and `false` asks for it
        // unowned so that ownership is taken by the wait below and by nothing
        // else.
        let handle = unsafe { CreateMutexW(None, false, &name) }
            .expect("Windows can create a named mutex for a test to wait on");

        let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `handle` is the mutex just created and not yet closed.
        let waited = unsafe { WaitForSingleObject(handle, milliseconds) };

        match waited {
            // Held. `WAIT_ABANDONED` is *also* held: it means the previous
            // owner died without releasing, which is a killed test binary, and
            // the resource is ours. Treating it as a failure would make one
            // crashed run poison every later one.
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self { handle, resource }),
            WAIT_TIMEOUT => {
                // SAFETY: as above, and nothing else holds this handle.
                let _ = unsafe { CloseHandle(handle) };
                Err(Contended {
                    resource,
                    waited: timeout,
                })
            }
            other => {
                // SAFETY: as above — the handle this scope created, closed
                // once, on the path that is about to panic.
                let _ = unsafe { CloseHandle(handle) };
                panic!("waiting for {resource} failed: {other:?}");
            }
        }
    }

    /// Which resource this holds.
    #[must_use]
    pub const fn resource(&self) -> &Resource {
        &self.resource
    }
}

impl Drop for Exclusive {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is the mutex this value owns, it was acquired
        // by the wait in `acquire_within`, and `Drop` runs once.
        //
        // Both results are discarded deliberately and neither is a failure a
        // caller could act on: releasing fails only for a mutex this thread
        // does not own, and if that were so the abandonment path would hand the
        // next waiter the lock anyway (AGENTS.md section 15 allows an ignored
        // failure that is documented).
        let _ = unsafe { ReleaseMutex(self.handle) };
        // SAFETY: the same handle, closed exactly once — the field is private,
        // the type is not `Copy`, and `Drop` runs once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::{Contended, Exclusive, Resource, DEFAULT_WAIT};
    use core::time::Duration;

    /// Tries to take `resource` on another thread, and lets go of it there.
    ///
    /// On another thread because a Windows mutex is re-entrant *per thread*:
    /// asking for it again on the thread already holding it succeeds, and a
    /// case written that way would pass against a crate that excluded nothing.
    /// It is also the shape the real contention takes, where the competitor is
    /// another process.
    ///
    /// The guard is dropped inside the thread rather than returned, because
    /// [`Exclusive`] is not [`Send`] — see the type's own note.
    fn elsewhere(resource: Resource, timeout: Duration) -> Result<(), Contended> {
        std::thread::spawn(move || Exclusive::acquire_within(resource, timeout).map(drop))
            .join()
            .expect("the waiting thread does not panic")
    }

    #[test]
    fn a_free_resource_is_taken_at_once() {
        let held = Exclusive::acquire(Resource::Foreground).expect("nothing else holds it");
        assert_eq!(held.resource(), &Resource::Foreground);
    }

    /*
     * The property the whole crate exists for, as far as one process can show
     * it: a second acquisition of a held resource does not succeed. Windows
     * mutexes are re-entrant *per thread*, so this waits on another thread —
     * which is also the shape the real contention takes, since the competitor
     * is another process and not this call stack.
     */
    #[test]
    fn a_held_resource_is_not_handed_out_again() {
        let held = Exclusive::acquire(Resource::DefaultAudioEndpoint).expect("nothing holds it");

        let contended = elsewhere(Resource::DefaultAudioEndpoint, Duration::from_millis(200));

        assert!(
            contended.is_err(),
            "a resource this test is holding was handed to another waiter"
        );
        drop(held);
    }

    #[test]
    fn a_released_resource_is_available_again() {
        drop(Exclusive::acquire(Resource::ExclusiveFullscreen).expect("nothing holds it"));

        let again = elsewhere(Resource::ExclusiveFullscreen, Duration::from_millis(500));

        assert!(
            again.is_ok(),
            "a released resource should be available to the next waiter"
        );
    }

    /*
     * Two displays are two exclusions. Without the output in the name, a test
     * duplicating `\\.\DISPLAY2` would wait for one duplicating `\\.\DISPLAY1`,
     * which is a slower suite and no more correct.
     */
    #[test]
    fn two_outputs_do_not_wait_for_each_other() {
        let first = Exclusive::acquire(Resource::DesktopDuplication(r"\\.\DISPLAY1".to_owned()))
            .expect("nothing holds it");

        let second = elsewhere(
            Resource::DesktopDuplication(r"\\.\DISPLAY2".to_owned()),
            Duration::from_millis(500),
        );

        assert!(
            second.is_ok(),
            "duplicating one output must not wait for another output's duplication"
        );
        drop(first);
    }

    /*
     * The message is the deliverable. A contended run that said "this machine
     * cannot duplicate" is the failure #194 is about, so the sentence has to
     * name the resource and say the machine is busy rather than incapable.
     */
    #[test]
    fn the_refusal_names_the_resource_and_blames_contention() {
        let held = Exclusive::acquire(Resource::Foreground).expect("nothing holds it");

        let refusal =
            elsewhere(Resource::Foreground, Duration::from_millis(100)).expect_err("it is held");

        let said = refusal.to_string();
        assert!(
            said.contains("the foreground window"),
            "the refusal should name the resource: {said}"
        );
        assert!(
            said.contains("contention rather than a limitation"),
            "the refusal should say the machine is busy rather than incapable: {said}"
        );
        drop(held);
    }

    /*
     * Every display Windows names is `\.\DISPLAYn`, and a backslash past the
     * `Local\` prefix names a directory in the object namespace that does not
     * exist. Before this was handled, `CreateMutexW` failed with
     * `ERROR_PATH_NOT_FOUND` for every caller that named an output — which is
     * all of them.
     */
    #[test]
    fn an_output_name_full_of_backslashes_still_makes_a_mutex() {
        let held = Exclusive::acquire(Resource::DesktopDuplication(r"\.\DISPLAY7".to_owned()))
            .expect("a display name is not a path");

        assert!(
            elsewhere(
                Resource::DesktopDuplication(r"\.\DISPLAY7".to_owned()),
                Duration::from_millis(100)
            )
            .is_err(),
            "and the sanitised name still excludes the same output"
        );
        drop(held);
    }

    #[test]
    fn the_default_wait_is_long_enough_to_outlast_a_hardware_test() {
        assert!(
            DEFAULT_WAIT >= Duration::from_secs(60),
            "a wait shorter than the longest hardware test in this repository would report \
             contention for a suite that was going to finish"
        );
    }
}
