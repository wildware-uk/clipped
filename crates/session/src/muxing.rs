//! The thread that writes the file, and the bounded queue feeding it.
//!
//! AGENTS.md section 20 forbids a capture thread from waiting on the
//! filesystem, and writing a packet is exactly that: `av_interleaved_write_frame`
//! with `AVFMT_FLAG_FLUSH_PACKETS` set hands every packet to the operating
//! system as it arrives (`docs/muxing.md`), which is what makes an interrupted
//! recording playable and also what makes it a syscall the capture loop must
//! not be inside. So the writer gets a thread, and packets — bytes, not
//! textures — cross to it through a bounded queue.
//!
//! # What happens when the queue fills
//!
//! Stated plainly, because the alternative is a recorder that silently drops
//! half a recording. The queue holds [`QUEUE_CAPACITY`] packets. While more
//! than [`HIGH_WATER`] of them are outstanding the capture loop stops
//! *submitting frames* and counts each one it skipped
//! ([`crate::RecordingReport::frames_dropped_writer_behind`]).
//!
//! Frames are skipped before they are encoded and never after. An encoded
//! packet thrown away would break every later frame that referenced it, so a
//! recording missing one of those is not a recording with a gap in it — it is a
//! recording that stops decoding. Dropping the *input* costs one frame and
//! nothing else.
//!
//! The alternative — blocking the capture thread until the disk catches up — is
//! worse in the way that matters: the capture backend keeps producing frames
//! while it is blocked, the game shares the GPU with a stalled encoder, and the
//! user sees the recorder as a stutter in the game.
//!
//! # What that guarantee rests on
//!
//! [`is_behind`](MuxingThread::is_behind) is read once per frame, before the
//! frame is submitted, and [`write`](MuxingThread::write) sends on a bounded
//! queue — so a submission made at [`HIGH_WATER`] whose packets do not fit in
//! [`HEADROOM`] would block the capture thread inside `send`. Nothing in the
//! type system stops an encoder from emitting nine packets from one frame; what
//! stands behind the guarantee is that no encoder in this workspace emits more
//! than one, in the low-latency configuration a recording opens them with.
//! `crate::recording::report_submission_over_headroom` says so, once, if that
//! ever stops being true, rather than leaving a stall with no explanation.
//!
//! # Watching the drive
//!
//! This thread is also the one that asks how much room is left, for the same
//! reason it exists at all: reading a volume's free space is a filesystem call
//! and the capture thread may not make one (AGENTS.md section 20). At most once
//! every [`crate::disk::PROBE_INTERVAL`] it asks, judges the answer against the
//! floor the recording was given, and publishes it as one atomic
//! ([`SpaceWatch`]) that the capture loop reads between frames. A recording
//! that reaches the floor is stopped by that loop while there is still room to
//! write the trailer — see `crate::disk` for why that matters more than it
//! sounds.
//!
//! # Ownership
//!
//! The writer thread owns the [`MkvWriter`], which owns the file. Nothing else
//! has a handle to either, so there is no path on which two threads write to
//! one container context — which is precisely what libavformat does not support
//! (`crates/muxer/src/writer.rs`).

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use clipped_muxer::{
    EncodedPacket, MkvWriter, MuxError, PacketTimestamp, RecordingSummary, TrackId,
};

use crate::disk::{self, SpaceVerdict};
use crate::error::SessionError;

/// How many encoded packets may be waiting to be written.
///
/// Two seconds of a 60 fps recording. Long enough to absorb the pauses a
/// desktop filesystem takes — a flush behind a virus scanner, a drive spinning
/// up — and short enough that the memory behind it is bounded at something
/// small: at 33 Mbit/s, two seconds of packets is about eight megabytes.
const QUEUE_CAPACITY: usize = 128;

/// How many packets of headroom are kept above the point at which the capture
/// loop stops submitting frames.
///
/// One submitted frame can produce more than one packet — an encoder flushing
/// reordered pictures produces several — so the loop stops short of the
/// capacity rather than at it. Without the headroom a submission made just
/// under the limit could produce a packet with nowhere to go, and the only
/// remaining choices would be to block the capture thread or to lose an encoded
/// packet, which are the two things this design exists to avoid.
///
/// Eight is a bound on one submission's output, not a proof: every encoder in
/// this workspace emits at most one packet per submitted frame in the
/// low-latency configuration a recording opens them with, so eight is seven
/// more than is needed. It is stated rather than assumed because nothing in the
/// type system holds an encoder to it —
/// `crate::recording::report_submission_over_headroom` is what notices if one
/// stops obeying it.
pub(crate) const HEADROOM: usize = 8;

/// The depth at which the capture loop stops submitting frames.
pub(crate) const HIGH_WATER: usize = QUEUE_CAPACITY - HEADROOM;

/// One encoded packet, copied out of the encoder's own buffer so that it can
/// cross to another thread.
///
/// The copy is the price of the thread. `clipped_encoder::EncodedPacket`
/// borrows the encoder's output buffer and is released by the next call to
/// `next_packet`, so the bytes cannot simply be sent — and a memcpy of a few
/// tens of kilobytes per frame is nothing beside the encode that produced it
/// (AGENTS.md section 18).
#[derive(Debug)]
struct QueuedPacket {
    data: Vec<u8>,
    presentation_nanos: i64,
    decode_nanos: i64,
    keyframe: bool,
}

/// What the writer thread has found out about the drive it is writing to.
///
/// One byte, written by the writer thread and read by the capture loop once per
/// acquisition. It is an atomic and not a lock for the same reason
/// [`MuxingThread::depth`](MuxingThread) is: the capture loop reads it between
/// frames and must never wait for it.
#[derive(Debug, Default)]
pub(crate) struct SpaceWatch {
    state: AtomicU8,
}

/// [`SpaceWatch`]'s states, as the byte holds them.
const SPACE_AMPLE: u8 = 0;
const SPACE_LOW: u8 = 1;
const SPACE_EXHAUSTED: u8 = 2;
const SPACE_UNREADABLE: u8 = 3;

/// What the capture loop should do about the drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpaceState {
    /// Carry on.
    Ample,
    /// Carry on, and say so once.
    Low,
    /// Finish the recording now, while there is still room to finish it
    /// properly.
    Exhausted,
    /// The drive stopped answering. Nothing more can be written to it.
    Unreadable,
}

impl SpaceWatch {
    /// What the last probe found.
    fn state(&self) -> SpaceState {
        match self.state.load(Ordering::Relaxed) {
            SPACE_LOW => SpaceState::Low,
            SPACE_EXHAUSTED => SpaceState::Exhausted,
            SPACE_UNREADABLE => SpaceState::Unreadable,
            _ => SpaceState::Ample,
        }
    }

    /// Publishes what a probe found.
    fn publish(&self, state: SpaceState) {
        self.state.store(
            match state {
                SpaceState::Ample => SPACE_AMPLE,
                SpaceState::Low => SPACE_LOW,
                SpaceState::Exhausted => SPACE_EXHAUSTED,
                SpaceState::Unreadable => SPACE_UNREADABLE,
            },
            Ordering::Relaxed,
        );
    }
}

/// Where the recording is going and how much of the drive it refuses to
/// consume.
///
/// A floor of zero turns the guard off, and the writer thread then makes no
/// filesystem call at all beyond the writes themselves.
#[derive(Debug, Clone)]
pub(crate) struct SpaceGuard {
    directory: PathBuf,
    minimum_free_bytes: u64,
}

impl SpaceGuard {
    /// A guard over the volume holding `output`.
    pub(crate) fn new(output: &Path, minimum_free_bytes: u64) -> Self {
        Self {
            // The parent, not the file: the file may not exist yet, and
            // `crate::disk::free_space` walks up from whatever it is given
            // anyway. Naming the directory keeps the probe off a path that is
            // being written to.
            directory: output
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map_or_else(|| output.to_path_buf(), Path::to_path_buf),
            minimum_free_bytes,
        }
    }

    /// Whether this guard does anything at all.
    const fn is_armed(&self) -> bool {
        self.minimum_free_bytes > 0
    }

    /// Asks the volume, judges the answer, and reports it.
    fn measure(&self) -> SpaceState {
        match disk::free_space(&self.directory) {
            Ok(space) => match disk::judge(space.free_bytes(), self.minimum_free_bytes) {
                SpaceVerdict::Ample => SpaceState::Ample,
                SpaceVerdict::Low => SpaceState::Low,
                SpaceVerdict::Exhausted => SpaceState::Exhausted,
            },
            Err(error) => {
                // At `warn` rather than `error`: the recording is about to be
                // finished deliberately, which is the good outcome, and the
                // capture loop is what reports the end reason.
                tracing::warn!(
                    %error,
                    "the drive the recording is being written to stopped answering"
                );
                SpaceState::Unreadable
            }
        }
    }
}

/// The writer thread, and the queue into it.
#[derive(Debug)]
pub(crate) struct MuxingThread {
    /// [`None`] once [`finish`](Self::finish) has taken it, which is what tells
    /// the writer thread that no more packets are coming.
    sender: Option<SyncSender<QueuedPacket>>,
    /// How many packets have been sent and not yet written. Read by the capture
    /// loop before every submission, so it is an atomic and not a lock.
    depth: Arc<AtomicUsize>,
    /// What the writer thread last found out about the drive.
    space: Arc<SpaceWatch>,
    handle: Option<JoinHandle<Result<RecordingSummary, MuxError>>>,
}

impl MuxingThread {
    /// Starts a thread that writes into `writer` until the queue closes,
    /// watching `guard`'s volume as it goes.
    pub(crate) fn start(writer: MkvWriter, guard: SpaceGuard) -> Self {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let depth = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&depth);
        let space = Arc::new(SpaceWatch::default());
        let watched = Arc::clone(&space);

        let handle = thread::Builder::new()
            .name("clipped-muxer".to_owned())
            .spawn(move || write_until_closed(writer, &receiver, &counted, &guard, &watched))
            // A machine that cannot start a thread cannot record, and there is
            // nothing to fall back to. `spawn` failing here means the process
            // is out of handles or memory, which is not a state to carry on in.
            .expect("a recording needs a thread to write its file");

        Self {
            sender: Some(sender),
            depth,
            space,
            handle: Some(handle),
        }
    }

    /// What the writer thread last found out about the drive.
    ///
    /// Read once per acquisition by the capture loop. One relaxed load; the
    /// filesystem call behind the answer happened on the writer thread.
    pub(crate) fn space(&self) -> SpaceState {
        self.space.state()
    }

    /// Whether the writer is far enough behind that no more frames should be
    /// encoded for now.
    pub(crate) fn is_behind(&self) -> bool {
        self.depth.load(Ordering::Relaxed) >= HIGH_WATER
    }

    /// Queues one encoded packet.
    ///
    /// # Errors
    ///
    /// [`SessionError::WriterLost`] when the writer thread has stopped, which
    /// is how a failed write — a full disk, a disconnected drive — reaches the
    /// capture loop. The real reason is on the thread and comes back from
    /// [`finish`](Self::finish).
    pub(crate) fn write(
        &self,
        data: &[u8],
        presentation_nanos: i64,
        decode_nanos: i64,
        keyframe: bool,
    ) -> Result<(), SessionError> {
        let sender = self.sender.as_ref().ok_or(SessionError::WriterLost)?;

        // Counted before the send, not after: the capture loop reads this to
        // decide whether to submit the next frame, and a count that lags the
        // queue would let it submit into a queue that is already full.
        self.depth.fetch_add(1, Ordering::Relaxed);
        sender
            .send(QueuedPacket {
                data: data.to_vec(),
                presentation_nanos,
                decode_nanos,
                keyframe,
            })
            .map_err(|_| {
                self.depth.fetch_sub(1, Ordering::Relaxed);
                SessionError::WriterLost
            })
    }

    /// Closes the queue, waits for everything in it to be written, and
    /// finalises the file.
    ///
    /// This is the finalisation the whole design is arranged around: it writes
    /// the trailer, which is where Matroska's segment length, duration and cue
    /// index go. A recording that never reaches it still plays — that is why
    /// the container was chosen (ADR 0001) — but it is this call that makes it
    /// seekable.
    ///
    /// # Errors
    ///
    /// [`SessionError::Mux`] if a packet could not be written or the trailer
    /// could not be, and [`SessionError::WriterLost`] if the thread panicked.
    /// The file is closed on every one of those paths.
    pub(crate) fn finish(mut self) -> Result<RecordingSummary, SessionError> {
        // Dropping the sender is what ends the writer's loop. Without it the
        // join below would wait for ever.
        drop(self.sender.take());

        let handle = self.handle.take().ok_or(SessionError::WriterLost)?;
        match handle.join() {
            Ok(result) => result.map_err(SessionError::Mux),
            Err(_) => Err(SessionError::WriterLost),
        }
    }
}

impl Drop for MuxingThread {
    /// Finalises the recording even when nobody called
    /// [`finish`](Self::finish).
    ///
    /// The path this exists for is a panic in the capture loop. AGENTS.md
    /// section 17 puts the user's recording above almost everything else, and a
    /// bug in the pipeline must still leave a file that plays — so the queue is
    /// closed and the thread joined here as well, and whatever the writer
    /// reported is logged, because a `Drop` has nowhere to return it.
    fn drop(&mut self) {
        drop(self.sender.take());
        let Some(handle) = self.handle.take() else {
            return;
        };
        match handle.join() {
            Ok(Ok(summary)) => tracing::info!(
                packets = summary.packets,
                "the recording was finalised while the session was being dropped"
            ),
            Ok(Err(error)) => tracing::error!(
                %error,
                "the recording could not be finalised; what reached the file before this remains"
            ),
            Err(_) => tracing::error!(
                "the thread writing the recording panicked; the file was closed by its own \
                 destructor and remains playable without an index"
            ),
        }
    }
}

/// The writer thread's body: write until the queue closes or a write fails,
/// then finalise whatever happened.
///
/// The volume is probed here, between packets, rather than on a timer thread of
/// its own: this thread is already awake for every packet, already owns the
/// path, and is the only thread in the recording that is allowed to touch the
/// filesystem at all.
fn write_until_closed(
    mut writer: MkvWriter,
    receiver: &Receiver<QueuedPacket>,
    depth: &AtomicUsize,
    guard: &SpaceGuard,
    space: &SpaceWatch,
) -> Result<RecordingSummary, MuxError> {
    let mut failure = None;
    // Measured before the first packet as well as between them, so that a
    // recording started on a drive that was full a moment ago does not have to
    // wait out an interval to find out (`crate::disk`).
    let mut next_probe = Instant::now();

    while let Ok(packet) = receiver.recv() {
        depth.fetch_sub(1, Ordering::Relaxed);

        if guard.is_armed() {
            let now = Instant::now();
            if now >= next_probe {
                next_probe = now + disk::PROBE_INTERVAL;
                space.publish(guard.measure());
            }
        }

        let muxed = EncodedPacket::new(
            TrackId::Video,
            PacketTimestamp::from_nanos(packet.presentation_nanos),
            &packet.data,
        )
        .with_decode_timestamp(PacketTimestamp::from_nanos(packet.decode_nanos))
        .with_keyframe(packet.keyframe);

        if let Err(error) = writer.write_packet(&muxed) {
            // Stop at the first failure rather than filling the log with one
            // line per frame for a disk that is not going to empty itself. The
            // trailer is still written below, so the recording up to here is a
            // finished file.
            failure = Some(error);
            break;
        }
    }

    // The trailer is written whether or not a packet failed: what was captured
    // before a disk filled is still the user's recording (AGENTS.md section
    // 17). A failure writing the trailer is only reported when there is not
    // already a more useful failure to report.
    let finished = writer.finish();
    match failure {
        Some(error) => Err(error),
        None => finished,
    }
}

/// The invariants the two numbers above have to keep, checked while this crate
/// is compiled rather than while a test runs.
///
/// A run-time test would be the wrong tool: both are constants an author
/// changes, so the moment to refuse is the build. The headroom must leave the
/// capture loop somewhere to put the packets of a submission made at the limit,
/// and the queue must hold about two seconds of a 60 fps recording — long
/// enough to absorb a filesystem pause, short enough that the memory behind it
/// stays small.
const _: () = {
    assert!(
        HIGH_WATER < QUEUE_CAPACITY,
        "with no headroom, a submission made at the limit produces a packet with nowhere to \
         go, and the only remaining choices are blocking the capture thread or losing an \
         encoded packet"
    );
    assert!(
        QUEUE_CAPACITY >= 120,
        "the queue holds less than two seconds of a 60 fps recording"
    );
    assert!(
        QUEUE_CAPACITY <= 256,
        "the queue holds more memory than a queue should"
    );
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_watches_the_folder_the_recording_goes_in_rather_than_the_file() {
        // `GetDiskFreeSpaceExW` wants a directory, and the recording's own file
        // does not exist when the guard is built. Pointing at the file would
        // make the first probe fail and read as an unplugged drive.
        let guard = SpaceGuard::new(Path::new(r"D:\clips\session.mkv"), 1);
        assert_eq!(guard.directory, Path::new(r"D:\clips"));
    }

    #[test]
    fn a_recording_written_to_the_working_directory_still_has_something_to_probe() {
        // `Path::parent` of a bare file name is an empty path, which is not a
        // directory anything can be asked about.
        let guard = SpaceGuard::new(Path::new("session.mkv"), 1);
        assert_eq!(guard.directory, Path::new("session.mkv"));
    }

    #[test]
    fn a_floor_of_zero_leaves_the_writer_thread_making_no_extra_filesystem_call() {
        // The guard being off has to mean *off*: a probe every two seconds on
        // the thread that has to keep up with the encoder is not free, and a
        // caller that turned the guard off asked for it not to happen.
        assert!(!SpaceGuard::new(Path::new(r"D:\clips\a.mkv"), 0).is_armed());
        assert!(SpaceGuard::new(Path::new(r"D:\clips\a.mkv"), 1).is_armed());
    }

    #[test]
    fn every_state_survives_the_byte_it_is_published_through() {
        // The capture loop acts on this: a state that decoded to `Ample` when
        // it was published as `Exhausted` would let a recording run the drive
        // dry, which is precisely what the guard exists to prevent.
        let watch = SpaceWatch::default();
        assert_eq!(watch.state(), SpaceState::Ample, "the default is ample");

        for state in [
            SpaceState::Low,
            SpaceState::Exhausted,
            SpaceState::Unreadable,
            SpaceState::Ample,
        ] {
            watch.publish(state);
            assert_eq!(watch.state(), state);
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_guard_over_a_drive_that_is_not_there_reports_it_rather_than_reporting_room() {
        // The failure mode this rules out is the dangerous direction: a probe
        // that could not read the volume must never be read as "there is
        // plenty", because the recording would then carry on writing into
        // nothing.
        let guard = SpaceGuard::new(
            Path::new(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\clips\a.mkv"),
            1 << 30,
        );
        assert_eq!(guard.measure(), SpaceState::Unreadable);
    }

    #[cfg(windows)]
    #[test]
    fn a_guard_over_a_real_drive_with_an_impossible_floor_asks_for_the_recording_to_stop() {
        // The other end of the same call: a real volume, judged against a floor
        // no drive can be above, must come back `Exhausted`. Together with the
        // test above this proves the probe reads the volume rather than always
        // answering the same way.
        let recording = std::env::temp_dir().join("clipped-muxing-space-guard.mkv");
        let ample = SpaceGuard::new(&recording, 1);
        let impossible = SpaceGuard::new(&recording, u64::MAX);

        assert_eq!(ample.measure(), SpaceState::Ample);
        assert_eq!(impossible.measure(), SpaceState::Exhausted);
    }
}
