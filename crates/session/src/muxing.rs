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
//! # Ownership
//!
//! The writer thread owns the [`MkvWriter`], which owns the file. Nothing else
//! has a handle to either, so there is no path on which two threads write to
//! one container context — which is precisely what libavformat does not support
//! (`crates/muxer/src/writer.rs`).

use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use clipped_muxer::{
    EncodedPacket, MkvWriter, MuxError, PacketTimestamp, RecordingSummary, TrackId,
};

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

/// The writer thread, and the queue into it.
#[derive(Debug)]
pub(crate) struct MuxingThread {
    /// [`None`] once [`finish`](Self::finish) has taken it, which is what tells
    /// the writer thread that no more packets are coming.
    sender: Option<SyncSender<QueuedPacket>>,
    /// How many packets have been sent and not yet written. Read by the capture
    /// loop before every submission, so it is an atomic and not a lock.
    depth: Arc<AtomicUsize>,
    handle: Option<JoinHandle<Result<RecordingSummary, MuxError>>>,
}

impl MuxingThread {
    /// Starts a thread that writes into `writer` until the queue closes.
    pub(crate) fn start(writer: MkvWriter) -> Self {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let depth = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&depth);

        let handle = thread::Builder::new()
            .name("clipped-muxer".to_owned())
            .spawn(move || write_until_closed(writer, &receiver, &counted))
            // A machine that cannot start a thread cannot record, and there is
            // nothing to fall back to. `spawn` failing here means the process
            // is out of handles or memory, which is not a state to carry on in.
            .expect("a recording needs a thread to write its file");

        Self {
            sender: Some(sender),
            depth,
            handle: Some(handle),
        }
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
fn write_until_closed(
    mut writer: MkvWriter,
    receiver: &Receiver<QueuedPacket>,
    depth: &AtomicUsize,
) -> Result<RecordingSummary, MuxError> {
    let mut failure = None;

    while let Ok(packet) = receiver.recv() {
        depth.fetch_sub(1, Ordering::Relaxed);

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
