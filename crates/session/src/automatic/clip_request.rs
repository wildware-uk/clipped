//! The channel between whoever saves a clip and the thread that owns the
//! sitting it belongs to.
//!
//! [Issue #731](https://github.com/wildware-uk/clipped/issues/731). A recording
//! the *window* started is the whole of its own session, so the connection
//! thread answering `save_replay` can name the clip and record it there and
//! then. A recording the **watcher** started belongs to a sitting the
//! [`SessionManager`](super::SessionManager) on the driver's thread owns — it
//! may be the second file of one, and that manager is what writes the sidecar —
//! so the connection thread cannot touch it.
//!
//! Answering anyway is what it used to do, and it did not end well: the state a
//! connection thread holds for an automatic recording carries no session, so
//! reaching for one panicked and took the connection with it.
//!
//! # What crosses the channel, and what does not
//!
//! Only the two things saving a clip needs a sitting for — a name for it, and a
//! record of it once it exists. **Not the write itself**, which stays on the
//! caller's thread:
//!
//! ```text
//!  connection thread                       driver thread
//!  ─────────────────                       ─────────────
//!  next_clip_path() ──── ask ─────────────▶ names it from the sitting
//!                   ◀─── answer ───────────  (instant)
//!  save_last(keep, &path)
//!    …as long as the disk takes…            carries on watching for games
//!  clip_saved(…) ─────── ask ─────────────▶ enters it in the session record
//!                   ◀─── answer ───────────  (instant)
//! ```
//!
//! Because a driver that wrote the clip itself would stop watching for the
//! length of the write — up to a replay window of footage — and a game
//! launching or exiting in that gap would be seen late. Both asks it does
//! answer are a few field reads and one sidecar rewrite.
//!
//! It is also the lock discipline
//! [`ManualSession`](super::ManualSession) already keeps for the same reason:
//! take the session to name the file, **let go**, write, take it again to
//! record the result. Both callers reach it through the one
//! [`ClipDestination`](super::ClipDestination), so what a clip is called and
//! whether the library ever hears about it have one implementation between them
//! (AGENTS.md section 55).
//!
//! # Why a request rather than a lock on the session
//!
//! Because the sitting is not a value two threads may hold. The driver mutates
//! it as games launch and recordings start and end, and it writes the sidecar
//! from it; a second thread holding it is exactly the contention that design
//! avoids. So the connection thread asks, waits, and is answered.
//!
//! This is the same shape as
//! [`ScreenshotRequests`](crate::screenshot::ScreenshotRequests), which asks a
//! *capture* loop for a frame. Two stores rather than one because they ask
//! different questions of different threads; what they share is the discipline
//! of an identifier that is never reused, an answer matched to it, and a
//! withdrawal on the way out so nothing is served to a caller that has gone.

use core::time::Duration;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use super::{ClipDestination, ClipDestinationError};

/// How long an ask waits before giving up, when the caller does not say.
///
/// Generous against the driver's poll interval, which is about a second: an ask
/// made just after the driver went back to waiting for a game event waits out
/// that interval before it is even seen. A caller that gave up in a few hundred
/// milliseconds would report a failure for a save that was about to succeed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// What the caller needs from the sitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Where the next clip of this sitting goes.
    NameNextClip,
    /// A clip that has been written; enter it in the session record.
    ClipSaved(WrittenClip),
}

/// A clip that exists on disk, described for the sitting that will record it.
///
/// The fields
/// [`SessionManager::clip_saved`](super::SessionManager::clip_saved) takes, and
/// no others — this is the ask in transit, not a second description of a clip
/// to drift from `clipped_replay`'s (AGENTS.md section 55).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenClip {
    /// Where it was written.
    pub path: PathBuf,
    /// Where in the recording the clip starts.
    pub source_start: Duration,
    /// And where it ends.
    pub source_end: Duration,
    /// How much was asked for, which the clip may be short of.
    pub requested: Duration,
    /// Whether the buffer held the whole of what was asked for.
    pub complete: bool,
    /// The wall clock when it was saved.
    pub now: SystemTime,
}

/// What the sitting said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The path the clip should take.
    Named(PathBuf),
    /// The clip is in the session record.
    Recorded,
}

/// The channel itself, cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct ClipRequests {
    shared: Arc<Shared>,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    /// Signalled when a request is answered.
    answered: Condvar,
}

#[derive(Debug, Default)]
struct State {
    /// The identifier the next request takes. Never reused, which is what
    /// matches an answer to the request that asked for it.
    next_id: u64,
    /// Requests made and not yet claimed, oldest first.
    waiting: Vec<(u64, Ask)>,
    /// Answers to claimed requests.
    ///
    /// A `Vec` rather than a map because there is never more than a handful:
    /// one per press nobody has collected yet.
    answers: Vec<(u64, Result<Answer, String>)>,
}

impl ClipRequests {
    /// A channel nobody has asked anything of.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts `ask` to whoever owns the sitting, and waits.
    ///
    /// # Errors
    ///
    /// The sitting's own sentence when it refused, or a sentence of this
    /// channel's when nothing answered in time — which includes the case where
    /// no driver is listening at all, because a channel nobody reads looks
    /// exactly like one whose reader is busy.
    pub fn ask(&self, ask: Ask) -> Result<Answer, ClipDestinationError> {
        self.ask_within(ask, DEFAULT_TIMEOUT)
    }

    /// The same, waiting no longer than `timeout`.
    ///
    /// # Errors
    ///
    /// As [`ask`](Self::ask).
    pub fn ask_within(&self, ask: Ask, timeout: Duration) -> Result<Answer, ClipDestinationError> {
        let id = {
            let mut state = self.lock();
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            state.waiting.push((id, ask));
            id
        };

        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if let Some(position) = state.answers.iter().position(|(held, _)| *held == id) {
                let (_, answer) = state.answers.remove(position);
                // Whatever the driver said is a refusal from the sitting
                // itself, which is a different thing from not having reached it
                // — and the two get different codes on the wire.
                return answer.map_err(ClipDestinationError::Refused);
            }

            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                // Withdrawn on the way out, so the driver does not later act on
                // a request nobody is waiting on. An answer that arrives after
                // this is dropped by `serve`.
                state.waiting.retain(|(held, _)| *held != id);
                state.answers.retain(|(held, _)| *held != id);
                return Err(ClipDestinationError::Unreachable(format!(
                    "the session that recording belongs to did not answer within {:.0}s",
                    timeout.as_secs_f64()
                )));
            }

            let (guarded, _) = self
                .shared
                .answered
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = guarded;
        }
    }

    /// The oldest request nobody has started serving, if there is one.
    ///
    /// Called by the driver between events. The identifier it returns must
    /// eventually reach [`serve`](Self::serve), with an answer or with a reason
    /// — a claimed request that is never answered is a caller waiting out its
    /// whole timeout for nothing.
    #[must_use]
    pub fn claim(&self) -> Option<(u64, Ask)> {
        let mut state = self.lock();
        if state.waiting.is_empty() {
            return None;
        }
        Some(state.waiting.remove(0))
    }

    /// Answers a claimed request.
    pub fn serve(&self, id: u64, answer: Result<Answer, String>) {
        {
            let mut state = self.lock();
            state.answers.push((id, answer));
        }
        // Every waiter is woken, because a condition variable cannot wake one
        // particular waiter and each checks for its own identifier.
        self.shared.answered.notify_all();
    }

    /// How many requests are waiting to be claimed.
    ///
    /// For a driver deciding whether it has work, and for a test waiting until
    /// a request it made on another thread has actually been queued — which is
    /// what makes "oldest first" an assertion about order rather than about
    /// which thread happened to win.
    #[must_use]
    pub fn claimable(&self) -> usize {
        self.lock().waiting.len()
    }

    /// The state, recovering from a panic in another holder.
    ///
    /// A poisoned lock here costs at most one save. Refusing every save
    /// afterwards would turn a moment's failure into a recording-long one, and
    /// this data has no invariant a panic could have broken halfway (AGENTS.md
    /// section 16).
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A sitting that is on another thread is a destination like any other.
///
/// Which is the point of the trait: `save` names the clip and records it the
/// one way, and does not know whether the sitting is a mutex it can take or a
/// driver it has to ask.
impl ClipDestination for ClipRequests {
    fn next_clip_path(&self) -> Result<PathBuf, ClipDestinationError> {
        match self.ask(Ask::NameNextClip)? {
            Answer::Named(path) => Ok(path),
            // Only reachable by serving the wrong answer to an identifier, which
            // is a bug in the driver rather than something a user can cause. Said
            // rather than panicked: it costs one clip.
            Answer::Recorded => Err(ClipDestinationError::Refused(
                "the session answered a request it was not asked".to_owned(),
            )),
        }
    }

    fn clip_saved(
        &self,
        path: PathBuf,
        source_start: Duration,
        source_end: Duration,
        requested: Duration,
        complete: bool,
        now: SystemTime,
    ) -> Result<(), ClipDestinationError> {
        self.ask(Ask::ClipSaved(WrittenClip {
            path,
            source_start,
            source_end,
            requested,
            complete,
            now,
        }))
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::{Answer, Ask, ClipDestination, ClipDestinationError, ClipRequests, WrittenClip};
    use core::time::Duration;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    /// A clip to record, with figures a wrong field order would show up in.
    fn written(path: &str) -> WrittenClip {
        WrittenClip {
            path: PathBuf::from(path),
            source_start: Duration::from_secs(11),
            source_end: Duration::from_secs(41),
            requested: Duration::from_secs(60),
            complete: false,
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        }
    }

    /// Claims the one request that is waiting, or fails rather than hanging.
    fn claim(requests: &ClipRequests) -> (u64, Ask) {
        for _ in 0..400 {
            if let Some(claimed) = requests.claim() {
                return claimed;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("nothing was ever queued for the driver to claim");
    }

    #[test]
    fn a_name_is_asked_of_the_sitting_and_comes_back() {
        let requests = ClipRequests::new();
        let serving = requests.clone();

        let waiter = std::thread::spawn(move || requests.next_clip_path());

        let (id, ask) = claim(&serving);
        assert_eq!(
            ask,
            Ask::NameNextClip,
            "naming is what the caller cannot do for itself"
        );
        serving.serve(
            id,
            Ok(Answer::Named(PathBuf::from("D:/clips/cs2-replay-3.mkv"))),
        );

        assert_eq!(
            waiter.join().expect("the waiting thread does not panic"),
            Ok(PathBuf::from("D:/clips/cs2-replay-3.mkv")),
            "the clip is called what the sitting called it"
        );
    }

    /*
     * The second ask carries the whole of what the session record needs. A field
     * dropped or transposed here is a library row with the wrong length on it,
     * which nothing downstream can detect.
     */
    #[test]
    fn a_written_clip_reaches_the_sitting_with_everything_it_needs_recording() {
        let requests = ClipRequests::new();
        let serving = requests.clone();
        let expected = written("D:/clips/cs2-replay-3.mkv");

        let sent = expected.clone();
        let waiter = std::thread::spawn(move || {
            requests.clip_saved(
                sent.path.clone(),
                sent.source_start,
                sent.source_end,
                sent.requested,
                sent.complete,
                sent.now,
            )
        });

        let (id, ask) = claim(&serving);
        assert_eq!(
            ask,
            Ask::ClipSaved(expected),
            "what was written, where it came from in the recording, and how much of the ask it \
             covers"
        );
        serving.serve(id, Ok(Answer::Recorded));

        assert_eq!(waiter.join().expect("no panic"), Ok(()));
    }

    #[test]
    fn a_refusal_from_the_sitting_reaches_whoever_asked() {
        let requests = ClipRequests::new();
        let serving = requests.clone();

        let waiter = std::thread::spawn(move || requests.next_clip_path());
        let (id, _) = claim(&serving);
        serving.serve(id, Err("nothing is being recorded".to_owned()));

        assert_eq!(
            waiter.join().expect("no panic"),
            Err(ClipDestinationError::Refused(
                "nothing is being recorded".to_owned()
            )),
            "the sitting's own sentence, and marked as the sitting's answer rather than as a \
             failure to reach it"
        );
    }

    /*
     * A channel nobody is reading is the case that matters most: it is what a
     * `save_replay` meets when the driver has stopped, and it must be a
     * sentence rather than a wait that never ends.
     */
    #[test]
    fn a_request_nothing_serves_gives_up_and_says_so() {
        let requests = ClipRequests::new();

        let error = requests
            .ask_within(Ask::NameNextClip, Duration::from_millis(150))
            .expect_err("nothing served it");

        assert!(
            matches!(&error, ClipDestinationError::Unreachable(message) if
                message.contains("did not answer")),
            "nothing answering is not the sitting refusing, and the two get different codes on \
             the wire: {error:?}"
        );
    }

    /*
     * And it withdraws on the way out, so a driver reaching the request after
     * the waiter has gone does not name a clip nobody is expecting.
     */
    #[test]
    fn a_request_that_timed_out_is_no_longer_waiting_to_be_claimed() {
        let requests = ClipRequests::new();

        let _ = requests.ask_within(Ask::NameNextClip, Duration::from_millis(50));

        assert!(
            requests.claim().is_none(),
            "a request whose waiter gave up must not still be claimable"
        );
    }

    #[test]
    fn nothing_is_claimable_when_nobody_has_asked() {
        assert!(ClipRequests::new().claim().is_none());
    }

    /*
     * Two presses are two saves, in the order they were made. A store that
     * served the newest first would answer the second and leave somebody
     * waiting out a timeout for the first.
     */
    #[test]
    fn requests_are_served_oldest_first() {
        let requests = ClipRequests::new();
        let first = requests.clone();
        let second = requests.clone();

        let one = std::thread::spawn(move || first.next_clip_path());
        while requests.claimable() == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        let two = std::thread::spawn(move || {
            let clip = written("D:/clips/second.mkv");
            second.clip_saved(
                clip.path,
                clip.source_start,
                clip.source_end,
                clip.requested,
                clip.complete,
                clip.now,
            )
        });
        while requests.claimable() < 2 {
            std::thread::sleep(Duration::from_millis(5));
        }

        let (first_id, first_ask) = requests.claim().expect("one is waiting");
        let (second_id, second_ask) = requests.claim().expect("and so is the other");
        assert_eq!(first_ask, Ask::NameNextClip);
        assert!(matches!(second_ask, Ask::ClipSaved(_)));

        requests.serve(
            first_id,
            Ok(Answer::Named(PathBuf::from("D:/clips/first.mkv"))),
        );
        requests.serve(second_id, Ok(Answer::Recorded));
        assert_eq!(
            one.join().expect("no panic"),
            Ok(PathBuf::from("D:/clips/first.mkv"))
        );
        assert_eq!(two.join().expect("no panic"), Ok(()));
    }

    /*
     * An answer to a request whose waiter has gone is dropped rather than kept:
     * nothing will ever collect it, and a store that accumulated them would
     * grow for the length of a recording.
     */
    #[test]
    fn an_answer_nobody_is_waiting_for_does_not_reach_the_next_caller() {
        let requests = ClipRequests::new();
        requests.serve(404, Ok(Answer::Named(PathBuf::from("D:/clips/stale.mkv"))));

        let serving = requests.clone();
        let waiter = std::thread::spawn(move || requests.next_clip_path());
        let (id, _) = claim(&serving);
        serving.serve(id, Ok(Answer::Named(PathBuf::from("D:/clips/mine.mkv"))));

        assert_eq!(
            waiter.join().expect("no panic"),
            Ok(PathBuf::from("D:/clips/mine.mkv")),
            "an answer is matched to the request that asked for it, not taken off a pile"
        );
        assert_ne!(
            Path::new("D:/clips/stale.mkv"),
            Path::new("D:/clips/mine.mkv")
        );
    }
}
