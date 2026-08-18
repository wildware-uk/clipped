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
//! # Who writes into it
//!
//! Two kinds of producer, and neither may wait on the other. The capture loop
//! sends encoded video packets; one thread per audio source
//! (`crate::audio`) sends the interleaved samples its endpoint produced. They
//! share one queue because they share one file — libavformat interleaves a
//! recording's tracks inside a single context, so exactly one thread may write
//! to it — and the samples are converted to the container's PCM on the writer
//! thread rather than on a capture thread, through
//! [`clipped_muxer::AudioTrackWriter`].
//!
//! # What happens when the queue fills
//!
//! Stated plainly, because the alternative is a recorder that silently drops
//! half a recording. The queue holds [`VIDEO_CAPACITY`] plus the audio share, and that
//! capacity is **divided** between the two producers: [`VIDEO_CAPACITY`] for
//! encoded packets and [`audio_capacity`] for captured buffers. Each producer is
//! held to its own share, so neither can fill the queue underneath the other and
//! turn its unblocked `send` into a blocking one.
//!
//! While more than [`HIGH_WATER`] video packets are outstanding the capture loop
//! stops *submitting frames* and counts each one it skipped
//! ([`crate::RecordingReport::frames_dropped_writer_behind`]).
//!
//! Frames are skipped before they are encoded and never after. An encoded
//! packet thrown away would break every later frame that referenced it, so a
//! recording missing one of those is not a recording with a gap in it — it is a
//! recording that stops decoding. Dropping the *input* costs one frame and
//! nothing else.
//!
//! Audio has no equivalent of "do not capture the next one": the endpoint
//! produces what it produces and a buffer that is not taken is gone. So a buffer
//! arriving when the audio share is full is **dropped and counted**
//! ([`crate::AudioTrackReport::buffers_dropped_writer_behind`]) rather than
//! waited on. What that costs is a hole in the track — every later packet still
//! carries the media time its own hardware gave it, so nothing after the hole
//! slides — and it is reported, because a recorder that silently loses audio is
//! the failure this whole arrangement exists to make visible.
//!
//! The alternative — blocking a capture thread until the disk catches up — is
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
//! The audio half rests on the same arithmetic and is checked the same way: an
//! audio producer counts its own outstanding buffers and refuses to send past
//! its own share, so `try_send` is never the thing that discovers the queue
//! is full. The constant assertions at the foot of this file are what hold the
//! two shares to the one capacity.
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

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use clipped_audio::{AudioFormat, ChannelMask, Level, MixSourceId, Mixer, SampleFormat};
use clipped_capture::MediaTime;
use clipped_muxer::{
    AudioTrackWriter, EncodedPacket, MkvWriter, MuxError, PacketTimestamp, RecordingLayout,
    RecordingSummary, TrackId,
};

use crate::disk::{self, SpaceVerdict};
use crate::error::SessionError;

/// How many encoded video packets may be waiting to be written.
///
/// Two seconds of a 60 fps recording. Long enough to absorb the pauses a
/// desktop filesystem takes — a flush behind a virus scanner, a drive spinning
/// up — and short enough that the memory behind it is bounded at something
/// small: at 33 Mbit/s, two seconds of packets is about eight megabytes.
const VIDEO_CAPACITY: usize = 128;

/// How many captured audio buffers may be waiting to be written, **per source**.
///
/// Sized in the same currency as [`VIDEO_CAPACITY`] — about two seconds of
/// production — but audio arrives an order of magnitude more often: Windows
/// delivers loopback in 10 ms packets, so two seconds is around two hundred
/// buffers from one source. Two hundred and fifty-six gives that some slack
/// while staying small in memory: a 10 ms stereo buffer at 48 kHz is 3,840
/// bytes of `f32`, so each source's share is under a megabyte.
///
/// **Per source, and not a fixed pool.** It was a fixed 512 while a recording
/// had at most two sources — the whole system mix and a microphone. Issues
/// [#26](https://github.com/wildware-uk/clipped/issues/26) and
/// [#27](https://github.com/wildware-uk/clipped/issues/27) made it three, and
/// three sources against a two-source pool is a queue that overflows in normal
/// use: the tracks then end early, by a different amount on every run, and the
/// A/V synchronisation check is what notices. Routing an application to a track
/// of its own ([#33](https://github.com/wildware-uk/clipped/issues/33)) will
/// make it more again, so the number scales rather than being raised.
///
/// It is deliberately a separate share rather than more room in one pool. The
/// point of the split is that a slow disk cannot make the audio threads consume
/// the video's headroom, which would turn the capture loop's unblocking `send`
/// into a blocking one.
const AUDIO_CAPACITY_PER_SOURCE: usize = 256;

/// The audio share of the queue for a recording with `sources` audio sources.
///
/// At least one source's worth even when a recording has no audio at all, so
/// the queue is never zero-sized and the arithmetic below has no special case.
const fn audio_capacity(sources: usize) -> usize {
    AUDIO_CAPACITY_PER_SOURCE * if sources == 0 { 1 } else { sources }
}

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
pub(crate) const HIGH_WATER: usize = VIDEO_CAPACITY - HEADROOM;

/// One thing waiting to be written into the recording.
#[derive(Debug)]
enum Queued {
    /// An encoded picture, for the video track.
    Video(QueuedPacket),
    /// A block of captured samples, for one audio track.
    Audio(QueuedSamples),
}

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

/// One capture buffer, copied out of the capture's own buffer so that it can
/// cross to the writer thread.
///
/// The same trade as [`QueuedPacket`], and for the same reason:
/// `clipped_audio::CapturedAudio` borrows the capture mutably and is released by
/// the next read. The samples cross as `f32` rather than as the container's PCM
/// because converting and cutting them into packets is
/// [`AudioTrackWriter`]'s job and it needs the writer to do it — which keeps
/// that arithmetic in one place (AGENTS.md section 55) and keeps the conversion
/// off a capture thread.
#[derive(Debug)]
struct QueuedSamples {
    track: TrackId,
    at_nanos: i64,
    samples: Vec<f32>,
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

/// What one attempt to queue a buffer of audio did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioQueued {
    /// It is on its way to the file.
    Written,
    /// The writer is far enough behind that the audio share of the queue is
    /// full, so the buffer was dropped. Counted, never waited on.
    DroppedWriterBehind,
    /// The writer thread has stopped, so nothing more will reach the file. The
    /// reason is on that thread and comes back from
    /// [`MuxingThread::finish`].
    WriterLost,
}

/// A handle an audio thread queues its samples through.
///
/// Cloned once per source and owned by that source's thread, so the threads
/// need no borrow of the [`MuxingThread`] and can be joined after the capture
/// loop has finished with it. It carries the audio share of the queue's depth
/// with it, which is what makes [`write`](Self::write) able to promise it never
/// blocks: it refuses at its own share rather than discovering a full queue
/// inside a send.
#[derive(Debug, Clone)]
pub(crate) struct AudioQueue {
    sender: SyncSender<Queued>,
    depth: Arc<AtomicUsize>,
    dropped: Arc<AtomicU64>,
    /// The audio share this recording was built with, which depends on how many
    /// sources it has ([`audio_capacity`]).
    capacity: usize,
}

impl AudioQueue {
    /// Queues `samples` for `track`, starting at `at`.
    ///
    /// Never blocks and never fails a recording: a caller that is told
    /// [`AudioQueued::DroppedWriterBehind`] has lost that buffer and nothing
    /// else, and one told [`AudioQueued::WriterLost`] should stop reading its
    /// endpoint because the file is no longer being written.
    pub(crate) fn write(&self, track: TrackId, at: MediaTime, samples: &[f32]) -> AudioQueued {
        // Counted before the send for the same reason the video path counts
        // before its own: a depth that lagged the queue would let a producer
        // send into a queue that is already full, and a full `sync_channel` is
        // where a capture thread waits on the filesystem (AGENTS.md section 20).
        if self.depth.fetch_add(1, Ordering::Relaxed) >= self.capacity {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return AudioQueued::DroppedWriterBehind;
        }

        match self.sender.try_send(Queued::Audio(QueuedSamples {
            track,
            at_nanos: at.as_nanos(),
            samples: samples.to_vec(),
        })) {
            Ok(()) => AudioQueued::Written,
            Err(error) => {
                self.depth.fetch_sub(1, Ordering::Relaxed);
                match error {
                    // Unreachable while the share above is honoured — the two
                    // shares add up to the capacity — but a `try_send` that
                    // reported a full queue is still a lost buffer rather than
                    // a lost recording, so it is counted like one.
                    TrySendError::Full(_) => {
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                        AudioQueued::DroppedWriterBehind
                    }
                    TrySendError::Disconnected(_) => AudioQueued::WriterLost,
                }
            }
        }
    }
}

/// The writer thread, and the queue into it.
#[derive(Debug)]
pub(crate) struct MuxingThread {
    /// [`None`] once [`finish`](Self::finish) has taken it, which is what tells
    /// the writer thread that no more packets are coming — once every
    /// [`AudioQueue`] clone has been dropped too.
    sender: Option<SyncSender<Queued>>,
    /// How many video packets have been sent and not yet written. Read by the
    /// capture loop before every submission, so it is an atomic and not a lock.
    depth: Arc<AtomicUsize>,
    /// How many audio buffers have been sent and not yet written.
    audio_depth: Arc<AtomicUsize>,
    /// How many audio buffers were dropped because the writer was behind.
    audio_dropped: Arc<AtomicU64>,
    /// The audio share this recording was sized for, handed to every
    /// [`AudioQueue`] so each source refuses at the same number
    /// ([`audio_capacity`]).
    audio_capacity: usize,
    /// What the writer thread last found out about the drive.
    space: Arc<SpaceWatch>,
    handle: Option<JoinHandle<Result<RecordingSummary, MuxError>>>,
}

impl MuxingThread {
    /// Starts a thread that writes into `writer` until the queue closes,
    /// watching `guard`'s volume as it goes.
    ///
    /// `layout` is the one `writer` was created from. It is taken again here
    /// because the audio tracks it declares are what the writer thread needs an
    /// [`AudioTrackWriter`] for, and building those from the same value the
    /// container was described from is what stops a track's declared format
    /// from drifting from the samples written to it.
    ///
    /// # Errors
    ///
    /// [`SessionError::Mux`] when an audio track the layout declares cannot be
    /// written to — a codec this writer does not produce, or a track with no
    /// sampling rate. Refused here, before a frame is captured, rather than
    /// discovered by an audio thread part way through a recording.
    pub(crate) fn start(
        writer: MkvWriter,
        guard: SpaceGuard,
        layout: &RecordingLayout,
    ) -> Result<Self, SessionError> {
        let tracks = audio_track_writers(layout)?;
        // Built here, on the thread that starts the writer, and moved into it:
        // the mixer belongs to the one thread that writes, so nothing else can
        // contend for it (`CompatibilityMixer`).
        let mixer = CompatibilityMixer::new(layout);

        // Sized from the layout rather than from a constant: the audio share is
        // per source, so a three-track recording gets three sources' worth.
        let audio_share = audio_capacity(layout.audio_tracks().len());
        let (sender, receiver) = mpsc::sync_channel(VIDEO_CAPACITY + audio_share);
        let depth = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&depth);
        let audio_depth = Arc::new(AtomicUsize::new(0));
        let audio_counted = Arc::clone(&audio_depth);
        let space = Arc::new(SpaceWatch::default());
        let watched = Arc::clone(&space);

        let handle = thread::Builder::new()
            .name("clipped-muxer".to_owned())
            .spawn(move || {
                write_until_closed(
                    writer,
                    tracks,
                    mixer,
                    &receiver,
                    &Depths {
                        video: &counted,
                        audio: &audio_counted,
                    },
                    &guard,
                    &watched,
                )
            })
            // A machine that cannot start a thread cannot record, and there is
            // nothing to fall back to. `spawn` failing here means the process
            // is out of handles or memory, which is not a state to carry on in.
            .expect("a recording needs a thread to write its file");

        Ok(Self {
            sender: Some(sender),
            depth,
            audio_depth,
            audio_dropped: Arc::new(AtomicU64::new(0)),
            audio_capacity: audio_share,
            space,
            handle: Some(handle),
        })
    }

    /// A handle for one audio source's thread to queue its samples through.
    ///
    /// # Panics
    ///
    /// When [`finish`](Self::finish) has already been called, which would be
    /// asking to write into a recording that is being closed.
    pub(crate) fn audio_queue(&self) -> AudioQueue {
        AudioQueue {
            sender: self
                .sender
                .clone()
                .expect("audio queues are taken before the recording is finished"),
            depth: Arc::clone(&self.audio_depth),
            dropped: Arc::clone(&self.audio_dropped),
            capacity: self.audio_capacity,
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
            .send(Queued::Video(QueuedPacket {
                data: data.to_vec(),
                presentation_nanos,
                decode_nanos,
                keyframe,
            }))
            .map_err(|_| {
                self.depth.fetch_sub(1, Ordering::Relaxed);
                SessionError::WriterLost
            })
    }

    /// How many audio buffers were dropped because the writer was behind.
    pub(crate) fn audio_buffers_dropped(&self) -> u64 {
        self.audio_dropped.load(Ordering::Relaxed)
    }

    /// Closes the queue, waits for everything in it to be written, and
    /// finalises the file.
    ///
    /// **Every [`AudioQueue`] taken from this thread has to have been dropped
    /// first**, because each one holds a clone of the sender and the writer's
    /// loop ends when the last of them goes. `crate::audio::AudioThreads` joins
    /// its threads — which is what drops their queues — before the capture loop
    /// reaches this call.
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

/// The two depth counters the writer thread decrements as it takes work off the
/// queue.
///
/// One structure rather than two arguments because they always travel together
/// and because which of them a queued item belongs to is the whole of the
/// bookkeeping: decrementing the wrong one would let a producer past its share.
struct Depths<'counters> {
    video: &'counters AtomicUsize,
    audio: &'counters AtomicUsize,
}

/// An [`AudioTrackWriter`] for every audio track the layout declared.
///
/// Built before the thread starts, from the same layout the container was
/// described from, so a track this writer cannot produce packets for is refused
/// while the caller still has somewhere to report it.
/// The mixer that fills the compatibility track, and which source feeds which
/// part of it.
///
/// [`None`] when the layout has no `CompatibilityMix` track, which is what
/// `--no-compatibility-mix` produces.
///
/// It lives on the writer thread rather than beside the captures for the reason
/// the issue gives: that thread already owns the queue every capture writes
/// into, so mixing there costs no capture thread a lock
/// ([issue #29](https://github.com/wildware-uk/clipped/issues/29)).
struct CompatibilityMixer {
    mixer: Mixer,
    /// Which mix source each declared audio track is, indexed the way
    /// `TrackId::Audio` indexes them. [`None`] for the mix track itself and for
    /// any source the mixer would not take.
    sources: Vec<Option<MixSourceId>>,
}

/// A container track's source, in the vocabulary the mixer reports against.
///
/// Two enumerations, because they answer different questions: `clipped-muxer`'s
/// says what a *track* is, and `clipped-logging`'s is the field vocabulary every
/// diagnostic in the workspace shares. The mixer takes the second because what
/// it produces is a log line and a report rather than a track.
///
/// `VoiceChat` and a per-application track both map to `Application`, which is
/// the nearest thing the logging vocabulary has; neither is produced by this
/// build yet ([issue #33](https://github.com/wildware-uk/clipped/issues/33)).
fn mixed_as(source: &clipped_muxer::AudioSource) -> clipped_logging::AudioSource {
    use clipped_muxer::AudioSource as Track;
    match source {
        Track::CompatibilityMix => clipped_logging::AudioSource::CompatibilityMix,
        Track::Game => clipped_logging::AudioSource::Game,
        Track::OtherSystemAudio => clipped_logging::AudioSource::OtherSystem,
        Track::Microphone => clipped_logging::AudioSource::Microphone,
        // `Application` covers voice chat, a per-application track, and
        // anything the container model gains later: the mixer uses this only to
        // name a source in a report, so a new track kind must not stop a
        // recording compiling here.
        _ => clipped_logging::AudioSource::Application,
    }
}

/// The shape a declared track's samples arrive in.
///
/// Every capture in this crate hands over interleaved `f32`
/// (`clipped_audio::CapturedAudio`), so that is what the mixer is told, whatever
/// the container stores.
fn shape_of(track: &clipped_muxer::AudioTrack) -> Option<AudioFormat> {
    Some(AudioFormat::new(
        core::num::NonZeroU32::new(track.sample_rate())?,
        core::num::NonZeroU16::new(track.channels())?,
        ChannelMask::default(),
        SampleFormat::Float32,
    ))
}

impl CompatibilityMixer {
    /// Builds one from the layout, or [`None`] if there is no mix track.
    ///
    /// A source whose sampling rate differs from the mix's is **converted to
    /// the mix's rate and said so once**, rather than left out of the mix. It
    /// used to be left out, which meant that on a machine with a 44.1 kHz
    /// headset microphone and a 48 kHz render endpoint — ordinary hardware —
    /// the one track a player that takes a track arbitrarily takes had no
    /// microphone in it, and the only sign was a log line. The conversion is
    /// `clipped_audio`'s (`crates/audio/src/mix/rate.rs`), it happens on the
    /// mix's own copy, and the source's isolated track still carries the
    /// capture's own samples at the capture's own rate
    /// ([issue #30](https://github.com/wildware-uk/clipped/issues/30)).
    ///
    /// A source whose *channel layout* cannot be placed is still left out and
    /// said so, because a downmix is a decision about what the user hears
    /// rather than a conversion (AGENTS.md section 21).
    fn new(layout: &RecordingLayout) -> Option<Self> {
        let tracks = layout.audio_tracks();
        let mix = tracks.iter().position(|track| {
            track.source() == Some(&clipped_muxer::AudioSource::CompatibilityMix)
        })?;
        let declared = tracks.get(mix)?;
        let format = shape_of(declared)?;

        let mut mixer = Mixer::new(format);
        let mut sources = Vec::with_capacity(tracks.len());
        for (index, track) in tracks.iter().enumerate() {
            if index == mix {
                sources.push(None);
                continue;
            }
            let Some(source) = track.source().cloned() else {
                sources.push(None);
                continue;
            };
            let Some(shape) = shape_of(track) else {
                sources.push(None);
                continue;
            };
            match mixer.add_source(mixed_as(&source), shape, Level::UNITY) {
                Ok(id) => {
                    if shape.sample_rate() != format.sample_rate() {
                        // Worth one line at `info`, because it is the one thing
                        // in the mix that a user could act on if they wanted
                        // to: setting the two endpoints to the same rate in
                        // Windows removes the conversion altogether.
                        tracing::info!(
                            audio_track = %source,
                            source_rate = shape.sample_rate().get(),
                            mix_rate = format.sample_rate().get(),
                            "this source is captured at a different rate from the compatibility \
                             mix, so the mix's copy of it is converted; its own track is \
                             recorded at the rate it was captured at"
                        );
                    }
                    sources.push(Some(id));
                }
                Err(error) => {
                    tracing::warn!(
                        audio_track = %source,
                        %error,
                        "this source is not in the compatibility mix, because the mix cannot \
                         take it as it is; its own track is unaffected"
                    );
                    sources.push(None);
                }
            }
        }

        Some(Self { mixer, sources })
    }

    /// Which track the mix is written to.
    fn track(&self) -> Option<u16> {
        self.sources
            .iter()
            .position(Option::is_none)
            .and_then(|index| u16::try_from(index).ok())
    }
}

fn audio_track_writers(layout: &RecordingLayout) -> Result<Vec<AudioTrackWriter>, SessionError> {
    layout
        .audio_tracks()
        .iter()
        .enumerate()
        .map(|(index, declared)| {
            let track = u16::try_from(index).map_err(|_| MuxError::InvalidTrack {
                track: TrackId::Video,
                reason: "a recording cannot have more than 65,536 audio tracks",
            })?;
            Ok(AudioTrackWriter::new(TrackId::Audio(track), declared)?)
        })
        .collect()
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
    mut tracks: Vec<AudioTrackWriter>,
    mut mixer: Option<CompatibilityMixer>,
    receiver: &Receiver<Queued>,
    depths: &Depths<'_>,
    guard: &SpaceGuard,
    space: &SpaceWatch,
) -> Result<RecordingSummary, MuxError> {
    let mut failure = None;
    // Measured before the first packet as well as between them, so that a
    // recording started on a drive that was full a moment ago does not have to
    // wait out an interval to find out (`crate::disk`).
    let mut next_probe = Instant::now();

    while let Ok(item) = receiver.recv() {
        if guard.is_armed() {
            let now = Instant::now();
            if now >= next_probe {
                next_probe = now + disk::PROBE_INTERVAL;
                space.publish(guard.measure());
            }
        }

        let written = match item {
            Queued::Video(packet) => {
                depths.video.fetch_sub(1, Ordering::Relaxed);
                let muxed = EncodedPacket::new(
                    TrackId::Video,
                    PacketTimestamp::from_nanos(packet.presentation_nanos),
                    &packet.data,
                )
                .with_decode_timestamp(PacketTimestamp::from_nanos(packet.decode_nanos))
                .with_keyframe(packet.keyframe);
                writer.write_packet(&muxed)
            }
            Queued::Audio(audio) => {
                depths.audio.fetch_sub(1, Ordering::Relaxed);
                write_samples(&mut writer, &mut tracks, &audio)
                    .and_then(|()| mix_samples(&mut writer, &mut tracks, mixer.as_mut(), &audio))
            }
        };

        if let Err(error) = written {
            // Stop at the first failure rather than filling the log with one
            // line per frame for a disk that is not going to empty itself. The
            // trailer is still written below, so the recording up to here is a
            // finished file.
            failure = Some(error);
            break;
        }
    }

    // Whatever the mixer is still holding, before the trailer. A mix is
    // assembled a block behind its sources — it cannot emit a frame until every
    // source has either contributed to it or been left behind — so without this
    // the compatibility track is short by the tail of the recording.
    if failure.is_none() {
        if let Err(error) = drain_mix(&mut writer, &mut tracks, mixer.as_mut()) {
            failure = Some(error);
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

/// Sends one queued buffer of samples to the track it belongs to.
///
/// A buffer addressed to a track the layout never declared is a wiring fault
/// rather than an operating condition — track identifiers come from
/// `RecordingLayout::audio_track_for` and cannot name a track that is not there
/// — so it is reported once at `error` and the buffer is dropped. Failing the
/// write instead would end a recording over a bug in the code that placed it
/// (AGENTS.md section 17).
/// Adds one source's block to the compatibility mix and writes whatever that
/// completes.
///
/// Runs after the block has gone to its own track, and cannot change it: the
/// mix reads the samples and never writes them, so a level or a limiter in the
/// mix leaves the isolated tracks exactly as they were — which is the property
/// [issue #29](https://github.com/wildware-uk/clipped/issues/29) asks for and
/// the one a person would notice being wrong.
fn mix_samples(
    writer: &mut MkvWriter,
    tracks: &mut [AudioTrackWriter],
    mixer: Option<&mut CompatibilityMixer>,
    audio: &QueuedSamples,
) -> Result<(), MuxError> {
    let Some(mixer) = mixer else {
        return Ok(());
    };
    let TrackId::Audio(index) = audio.track else {
        return Ok(());
    };
    let Some(Some(source)) = mixer.sources.get(usize::from(index)).copied() else {
        // The mix track itself, or a source the mix could not take.
        return Ok(());
    };

    // A contribution that the mixer refuses is this block missing from a
    // convenience track, which is not worth failing a recording over
    // (AGENTS.md section 17). It is said once by `CompatibilityMixer::new` for
    // the cases that are knowable up front.
    // The queue carries a signed nanosecond count because a muxer timestamp can
    // legitimately be negative; a mix position cannot. A block placed before the
    // recording's own zero has nothing in the mix to be added to, so it is left
    // out rather than clamped to the start, where it would pile up on the first
    // frame.
    let Ok(nanos) = u64::try_from(audio.at_nanos) else {
        return Ok(());
    };
    let at = clipped_audio::AudioTimestamp::from_nanos(nanos);
    let _ = mixer.mixer.contribute(source, at, &audio.samples);
    emit_mix(writer, tracks, mixer, false)
}

/// Writes everything the mixer has finished with.
fn emit_mix(
    writer: &mut MkvWriter,
    tracks: &mut [AudioTrackWriter],
    mixer: &mut CompatibilityMixer,
    finishing: bool,
) -> Result<(), MuxError> {
    let Some(index) = mixer.track() else {
        return Ok(());
    };
    let Some(track) = tracks.get_mut(usize::from(index)) else {
        return Ok(());
    };

    loop {
        let taken = if finishing {
            mixer.mixer.drain()
        } else {
            mixer.mixer.take()
        };
        let Some(mixed) = taken else {
            return Ok(());
        };
        track.write_samples(
            writer,
            PacketTimestamp::from_nanos(
                i64::try_from(mixed.timestamp().as_nanos()).unwrap_or(i64::MAX),
            ),
            mixed.samples(),
        )?;
        if finishing {
            // `drain` empties what is left in one or more blocks; `take` is
            // called again above until it says there is nothing ready.
            continue;
        }
    }
}

/// Writes the tail of the mix when the recording ends.
fn drain_mix(
    writer: &mut MkvWriter,
    tracks: &mut [AudioTrackWriter],
    mixer: Option<&mut CompatibilityMixer>,
) -> Result<(), MuxError> {
    match mixer {
        Some(mixer) => emit_mix(writer, tracks, mixer, true),
        None => Ok(()),
    }
}

fn write_samples(
    writer: &mut MkvWriter,
    tracks: &mut [AudioTrackWriter],
    audio: &QueuedSamples,
) -> Result<(), MuxError> {
    let index = match audio.track {
        TrackId::Video => None,
        TrackId::Audio(index) => Some(usize::from(index)),
    };
    let Some(track) = index.and_then(|index| tracks.get_mut(index)) else {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        if !REPORTED.swap(true, Ordering::Relaxed) {
            tracing::error!(
                track = %audio.track,
                declared = tracks.len(),
                "audio was queued for a track this recording does not have, and was dropped; \
                 please report this"
            );
        }
        return Ok(());
    };

    track.write_samples(
        writer,
        PacketTimestamp::from_nanos(audio.at_nanos),
        &audio.samples,
    )
}

/// The invariants the numbers above have to keep, checked while this crate is
/// compiled rather than while a test runs.
///
/// A run-time test would be the wrong tool: all of them are constants an author
/// changes, so the moment to refuse is the build. The headroom must leave the
/// capture loop somewhere to put the packets of a submission made at the limit;
/// the video share must hold about two seconds of a 60 fps recording — long
/// enough to absorb a filesystem pause, short enough that the memory behind it
/// stays small — and the two shares together must be the whole capacity, which
/// is what makes neither producer able to block.
const _: () = {
    assert!(
        HIGH_WATER < VIDEO_CAPACITY,
        "with no headroom, a submission made at the limit produces a packet with nowhere to \
         go, and the only remaining choices are blocking the capture thread or losing an \
         encoded packet"
    );
    assert!(
        VIDEO_CAPACITY >= 120,
        "the queue holds less than two seconds of a 60 fps recording"
    );
    assert!(
        VIDEO_CAPACITY <= 256,
        "the queue holds more video memory than a queue should"
    );
    assert!(
        AUDIO_CAPACITY_PER_SOURCE >= 200,
        "one source's share holds less than two seconds at Windows' 10 ms loopback period"
    );
    assert!(
        audio_capacity(3) == AUDIO_CAPACITY_PER_SOURCE * 3,
        "the audio share has to grow with the number of sources: a recording with a game \
         track, an other-system-audio track and a microphone is three, and holding it to \
         two sources' worth drops buffers in ordinary use"
    );
    assert!(
        audio_capacity(0) > 0,
        "a recording with no audio at all must still have a queue with room in it"
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

    /// A queue with no writer behind it, which is what a stalled disk looks
    /// like from a producer.
    ///
    /// The receiver is returned rather than dropped: dropping it would
    /// disconnect the channel, and every write would come back
    /// [`AudioQueued::WriterLost`] instead of filling it.
    /// A queue nobody drains, sized for the one source that writes to it.
    fn stalled_queue() -> (AudioQueue, Receiver<Queued>) {
        let capacity = audio_capacity(1);
        let (sender, receiver) = mpsc::sync_channel(VIDEO_CAPACITY + capacity);
        (
            AudioQueue {
                sender,
                depth: Arc::new(AtomicUsize::new(0)),
                dropped: Arc::new(AtomicU64::new(0)),
                capacity,
            },
            receiver,
        )
    }

    #[test]
    fn an_audio_producer_stops_at_its_share_instead_of_waiting_for_the_writer() {
        // The guarantee AGENTS.md section 20 asks for, against the case that
        // tests it: a writer that has stopped taking anything off the queue.
        // The producer must come back with a dropped buffer rather than sitting
        // inside `send`, because a capture thread inside `send` is a capture
        // thread waiting on the filesystem.
        //
        // Run on a thread of its own and waited for with a timeout, so that a
        // producer which *does* block fails this test rather than hanging the
        // suite with no explanation.
        let (queue, receiver) = stalled_queue();
        let (done, finished) = mpsc::channel();

        std::thread::spawn(move || {
            let samples = vec![0.0_f32; 480];
            let mut written = 0_usize;
            let mut dropped = 0_usize;
            let capacity = audio_capacity(1);
            for _ in 0..(VIDEO_CAPACITY + capacity) * 2 {
                match queue.write(TrackId::Audio(0), MediaTime::ZERO, &samples) {
                    AudioQueued::Written => written += 1,
                    AudioQueued::DroppedWriterBehind => dropped += 1,
                    AudioQueued::WriterLost => break,
                }
            }
            let _ = done.send((written, dropped));
        });

        let (written, dropped) = finished
            .recv_timeout(core::time::Duration::from_secs(10))
            .expect(
                "queueing audio waited for a writer that had stopped, which is a capture thread \
                 blocked on the filesystem",
            );

        assert_eq!(
            written,
            audio_capacity(1),
            "an audio producer must stop at its own share of the queue and not one item further"
        );
        assert_eq!(
            dropped,
            (VIDEO_CAPACITY + audio_capacity(1)) * 2 - audio_capacity(1)
        );

        // And the point of the share: the video's slots are still there. A
        // producer allowed to fill the whole queue would leave the capture
        // loop's `send` — which *is* the blocking one — with nowhere to put an
        // encoded packet, and AGENTS.md section 20's guarantee would be gone
        // without anything failing.
        assert_eq!(
            receiver.try_iter().count(),
            audio_capacity(1),
            "the queue should hold exactly the audio share, leaving the video's {} free",
            VIDEO_CAPACITY
        );
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
