//! The checkpoint contract that lets a long read be paused and stopped from
//! outside itself.

/// Whether a paced read should carry on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    /// Carry on reading.
    Yes,
    /// Stop. The caller reports this as its own "cancelled" error, because
    /// this crate does not know what kind of work was interrupted.
    Stop,
}

/// What a long-running read asks, periodically, before doing more of it.
///
/// This is the hook that keeps a background read out of a game's way. An
/// implementation may block — that is the point: [`Worker`](crate::Worker)'s
/// blocks for as long as a recording is running, so a read that started
/// before a game launched stops inside a few checkpoints and resumes when the
/// recording ends, rather than being abandoned or running straight through
/// it. `crates/waveform/src/analyse.rs` and
/// `crates/library/src/thumbnail/render.rs` are the two readers that call
/// [`checkpoint`](Self::checkpoint), each between container packets, and
/// [`Worker`](crate::Worker) is the one implementation both of their services
/// hand them.
pub trait Pace: Send + Sync {
    /// Called every so often while reading. May block; may ask for a stop.
    fn checkpoint(&self) -> Continue;
}

/// The pace of a caller with nothing better to do, which never waits and
/// never stops.
///
/// For a caller that wants `analyse` or `render` outside a [`Worker`](crate::Worker)
/// — a one-off tool, or a test that wants the whole file read in one call
/// with nothing pausing it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unpaced;

impl Pace for Unpaced {
    fn checkpoint(&self) -> Continue {
        Continue::Yes
    }
}
