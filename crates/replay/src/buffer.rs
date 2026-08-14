//! The buffer itself: what it keeps, what it throws away, and when.
//!
//! # Retention
//!
//! Segments are kept in a queue, newest at the back, and the oldest is dropped
//! **only if the ones behind it still reach back over the configured window**
//! from the newest picture in the buffer. So a buffer that has been running
//! longer than its window holds between `window` and
//! `window + segment_duration` of history: the extra is however much of the
//! oldest segment is not yet needed, and it cannot be trimmed off because a
//! segment that does not begin on a keyframe cannot be decoded. Erring on the
//! side of extra is deliberate — a buffer holding slightly less than its window
//! would fail the request it was configured for, and the slack costs two
//! seconds of video.
//!
//! The rule is applied on every packet rather than at every segment boundary,
//! because the newest picture is in the segment currently being written: a
//! buffer that only evicted at a boundary would hold a whole segment more than
//! it was asked for and would grow past its ceiling until the next keyframe.
//!
//! A second rule sits under the first. If the segments the buffer owns exceed
//! [`ReplayConfig::memory_ceiling`], further segments are evicted regardless of
//! the window, counted ([`ReplayStats::segments_evicted_over_ceiling`]) and
//! reported once at `warn`. This is the answer to "what happens when the
//! machine cannot provide the memory the configuration asks for": the window
//! shortens, visibly, rather than the process growing until Windows starts
//! paging a game's memory to disk (AGENTS.md section 16). One sealed segment is
//! always kept, so a save is never impossible.
//!
//! # The ceiling binds the segment being written too
//!
//! Evicting sealed segments cannot bound a buffer on its own, because the
//! segment currently being written is not one of them. An encoder whose
//! keyframe interval is longer than the buffer's window produces a segment that
//! never seals: one keyframe followed by five minutes of predicted pictures is a
//! single segment, and a ceiling enforced only against the sealed queue lets it
//! grow to a gigabyte inside a 107 MiB configuration. No encoder in this
//! workspace is configured that way today — `KeyframeInterval::DEFAULT` is two
//! seconds — but the keyframe interval belongs to `clipped-encoder` and not
//! here, so "it cannot happen" would be a property of another crate's settings
//! rather than of this design.
//!
//! So the ceiling is checked **before** each packet is copied in, against what
//! the append would cost (`OpenSegment::resident_bytes_after`), and when
//! evicting sealed segments cannot free enough for it the buffer:
//!
//! 1. **Seals the open segment where it stands.** A segment is cut at its end,
//!    not at its front, so what is sealed still begins on a keyframe and is
//!    still decodable on its own. Nothing already buffered is thrown away, and a
//!    save during what follows gets real video.
//! 2. **Discards packets until the encoder's next keyframe**, counting them
//!    ([`ReplayStats::packets_discarded_over_ceiling`]) and returning
//!    [`PushOutcome::DiscardedOverCeiling`] for each. There is nowhere else for
//!    them to go: a segment that does not begin on a keyframe cannot be decoded,
//!    so admitting them would mean holding pictures no save could use.
//! 3. **Drops everything held from before the gap** when that keyframe arrives.
//!    Packets were lost in between, and `lease_last` resolves "the last thirty
//!    seconds" against the newest picture in the buffer — so material from
//!    either side of a gap would be selected together and written into one clip
//!    that silently jumps (AGENTS.md section 22). Material from before a gap
//!    cannot serve the request this buffer exists for, so it goes rather than
//!    misleading a save.
//!
//! The alternatives were weighed and rejected. *Sealing early and carrying on
//! into a segment that does not begin on a keyframe* keeps the window full, but
//! such a segment can only be decoded behind the keyframe segment it continues:
//! a thirty-second clip would have to drag in the five minutes back to that
//! keyframe, so it does not deliver the feature it costs the crate's central
//! invariant. *Refusing the packet and leaving the segment open* bounds nothing,
//! since the open segment is what is over. *Dropping the open segment outright*
//! bounds memory equally well but throws away decodable video that sealing keeps
//! for free.
//!
//! None of this makes such a configuration work — a buffer cannot cut a
//! thirty-second clip out of a stream with a keyframe every five minutes. What
//! it does is keep the memory where the documentation says it is, keep every
//! byte handed to a save decodable, and put the loss in the statistics where
//! somebody can see it.
//!
//! The ceiling governs what the buffer *owns*, and deliberately not what a save
//! is holding open. Counting a lease against it would mean a save of ten
//! seconds evicting the buffer's history to pay for itself — the buffer would
//! collapse to a single segment for as long as the clip took to write, and the
//! next hotkey press would find nothing there. So the memory a save keeps alive
//! is reported ([`ReplayStats::bytes_retained_for_a_save`]) and added to the
//! total ([`ReplayStats::bytes_held`]) rather than subtracted from the window,
//! and the honest ceiling for a configuration is
//! `ReplayConfig::memory_ceiling` plus the clip a save is reading. That is one
//! of the numbers `docs/replay-buffer.md` states.
//!
//! # Threading
//!
//! One writer, any number of readers, and the buffer takes the lock itself so
//! that neither side has to remember to.
//!
//! The writer is the capture and encoding thread, which pushes each packet as
//! it drains the encoder. Readers are save requests on other threads. AGENTS.md
//! section 18 says not to put locks on a capture thread, and this is a
//! deliberate, bounded exception, of exactly the shape the capture thread
//! already pays: `crate::muxing`'s bounded queue in `clipped-session` takes a
//! lock inside `SyncSender::send` for every packet, and this takes one for the
//! `memcpy` that follows it. What the lock is *never* held across is a
//! filesystem call, an allocation of unbounded size, or a wait on another
//! thread.
//!
//! The one moment a reader holds it for longer is [`ReplayBuffer::lease`],
//! which copies the open segment so that a save can include the newest material
//! (`crate::segment`, `OpenSegment::snapshot`). That is a copy of at most one
//! segment — about 4.5 MiB at 1080p60 — which is well under a frame interval and
//! happens once per save rather than once per frame.
//!
//! # Poisoning
//!
//! A reader that panics while holding the lock must not end the recording, so
//! the lock is taken through [`PoisonError::into_inner`]. The state behind it
//! is a queue of immutable segments and a byte count; a panic mid-push can
//! leave the count and the queue disagreeing about one segment, which is a
//! wrong number in a report, not a corrupt buffer. Stopping a recording over it
//! would be the worse failure (AGENTS.md section 17).

use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use clipped_encoder::EncodedPacket;
use clipped_muxer::TrackId;

use crate::config::ReplayConfig;
use crate::error::LeaseError;
use crate::lease::SegmentLease;
use crate::range::TimeRange;
use crate::segment::{OpenSegment, Segment, SegmentId, SegmentIds};

/// What one [`ReplayBuffer::push`] did with a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// The packet was discarded because no segment has begun yet.
    ///
    /// A segment begins on a keyframe so that it can be decoded on its own, so
    /// everything an encoder emits before its first keyframe has nowhere to go.
    /// In practice that is nothing at all: every encoder here emits a keyframe
    /// first.
    AwaitingKeyframe,
    /// The packet was added to the segment currently being written.
    Appended,
    /// The packet was a keyframe and began a new segment.
    OpenedSegment(SegmentId),
    /// The packet was discarded because the buffer is at its memory ceiling and
    /// the segment being written had to be sealed before a keyframe.
    ///
    /// Every packet up to the encoder's next keyframe is discarded this way, and
    /// what the buffer held from before the gap goes when that keyframe arrives.
    /// The module documentation says why, and what it means about the encoder's
    /// keyframe interval. Reaching this at all is a misconfiguration rather than
    /// an operating condition, and it is reported once at `warn`.
    DiscardedOverCeiling,
}

/// What one call to [`ReplayBuffer::push_audio`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPushOutcome {
    /// The block was copied into the segment being written.
    Appended,
    /// No keyframe has reached the buffer yet, so there is no segment to put it
    /// in and nothing it could be aligned against.
    DiscardedAwaitingKeyframe,
    /// The memory ceiling left no room, and the segment being written was
    /// sealed rather than grown.
    DiscardedOverCeiling,
}

/// A rolling window of encoded segments.
///
/// See the module documentation for retention, threading and what happens under
/// memory pressure.
#[derive(Debug)]
pub struct ReplayBuffer {
    config: ReplayConfig,
    inner: Mutex<Inner>,
}

impl ReplayBuffer {
    /// An empty buffer holding what `config` describes.
    #[must_use]
    pub fn new(config: ReplayConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// What this buffer was configured with.
    #[must_use]
    pub const fn config(&self) -> ReplayConfig {
        self.config
    }

    /// Adds one encoded packet.
    ///
    /// The bytes are copied out of the encoder's output buffer, which is what
    /// makes this safe to call from the encoding thread and then walk away:
    /// `clipped_encoder::EncodedPacket` is released by the next call to
    /// `next_packet`.
    pub fn push(&self, packet: &EncodedPacket<'_>) -> PushOutcome {
        self.locked().push(&self.config, packet)
    }

    /// Adds one block of captured audio.
    ///
    /// `at` is when the first frame of the block was captured, on the same
    /// media clock the encoder's packets carry. The samples are copied, as the
    /// coded bytes are, so the caller may reuse its buffer immediately.
    ///
    /// Audio arriving before the first keyframe is discarded and counted: a
    /// segment begins on a keyframe, and audio with no video to sit beside
    /// cannot be written into a clip.
    pub fn push_audio(&self, track: TrackId, at: Duration, samples: &[f32]) -> AudioPushOutcome {
        self.locked().push_audio(&self.config, track, at, samples)
    }

    /// The segments covering `range`, held against eviction until the lease is
    /// dropped.
    ///
    /// The range is expanded outwards to segment boundaries, so the lease
    /// covers at least what was asked for and at most one segment more at each
    /// end ([`SegmentLease::leading_slack`], [`SegmentLease::trailing_slack`]).
    ///
    /// # Errors
    ///
    /// [`LeaseError::Empty`] if no keyframe has reached the buffer yet, and
    /// [`LeaseError::OutsideBuffer`] if the range shares no instant with what
    /// is held.
    pub fn lease(&self, range: TimeRange) -> Result<SegmentLease, LeaseError> {
        self.locked().lease(Request::Range(range))
    }

    /// The segments covering the last `length` of buffered video.
    ///
    /// What a replay hotkey asks for. The range is resolved against the newest
    /// packet under the same lock the eviction takes, so a buffer that rolls
    /// over between deciding and leasing cannot produce a range that no longer
    /// exists.
    ///
    /// # Errors
    ///
    /// [`LeaseError::Empty`] if no keyframe has reached the buffer yet.
    pub fn lease_last(&self, length: Duration) -> Result<SegmentLease, LeaseError> {
        self.locked().lease(Request::Last(length))
    }

    /// The media time the buffer currently holds, or [`None`] if it holds
    /// nothing.
    #[must_use]
    pub fn held(&self) -> Option<TimeRange> {
        self.locked().held()
    }

    /// What the buffer holds and what it has done.
    #[must_use]
    pub fn stats(&self) -> ReplayStats {
        let mut inner = self.locked();
        // Releasing first so that the figures describe the memory that is
        // actually still held, rather than counting segments whose last reader
        // has already finished with them.
        inner.release_finished_leases();
        inner.stats()
    }

    /// The lock, taken through a poisoned mutex rather than around it.
    fn locked(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What a lease was asked for, resolved under the lock.
enum Request {
    Range(TimeRange),
    Last(Duration),
}

/// Everything behind the lock.
#[derive(Debug, Default)]
struct Inner {
    /// Sealed segments, oldest first. Each begins on a keyframe.
    sealed: VecDeque<Arc<Segment>>,
    /// The segment being written, which no reader shares.
    open: Option<OpenSegment>,
    /// Segments this buffer has evicted and a lease is still reading.
    ///
    /// Held so that the memory a save is keeping alive is counted against the
    /// ceiling rather than being invisible. They leave when their last reader
    /// does.
    leased: Vec<Arc<Segment>>,
    ids: SegmentIds,
    sealed_bytes: u64,
    leased_bytes: u64,
    peak_bytes: u64,
    counters: Counters,
    ceiling_reported: bool,
    /// How many packets have been dropped since the segment being written was
    /// sealed early to stay under the ceiling, or [`None`] when that has not
    /// happened and the buffer is simply waiting for its first keyframe.
    ///
    /// A non-zero count means there is a gap between what is held and what the
    /// next keyframe will begin, which is what makes the older material
    /// unusable (see the module documentation).
    ceiling_gap: Option<u64>,
}

/// Totals for the life of a buffer.
#[derive(Debug, Default, Clone, Copy)]
struct Counters {
    packets_buffered: u64,
    packets_discarded_before_first_keyframe: u64,
    segments_opened: u64,
    segments_evicted_for_window: u64,
    segments_evicted_over_ceiling: u64,
    segments_sealed_at_the_ceiling: u64,
    packets_discarded_over_ceiling: u64,
    audio_blocks_buffered: u64,
    audio_blocks_discarded_before_first_keyframe: u64,
    audio_blocks_discarded_over_ceiling: u64,
    leases_taken: u64,
}

impl Inner {
    fn push(&mut self, config: &ReplayConfig, packet: &EncodedPacket<'_>) -> PushOutcome {
        let keyframe = packet.is_keyframe();

        if self.open.is_none() && !keyframe {
            return self.discard_awaiting_keyframe();
        }

        // The open segment will cover from its own keyframe up to this one, so
        // it is this packet's timestamp — not the last one already in the
        // segment — that says whether the segment has reached its target
        // length. Measuring the other way would produce segments one frame
        // short of the target and so, at the two-second default, segments twice
        // as long as asked for.
        let full = self.open.as_ref().is_some_and(|open| {
            packet.presentation_time().saturating_sub(open.start()) >= config.segment_duration()
        });
        if keyframe && full {
            self.seal();
        }

        // Room is made *before* the bytes are copied in, which is the whole of
        // "the ceiling binds the segment being written too". It can seal that
        // segment, and then a packet that is not a keyframe has nowhere
        // decodable to go.
        self.make_room(config, |open| {
            open.resident_bytes_after(packet.data().len()) as u64
        });
        if self.open.is_none() && !keyframe {
            return self.discard_awaiting_keyframe();
        }

        self.counters.packets_buffered += 1;
        let outcome = match &mut self.open {
            Some(open) => {
                open.append(packet);
                PushOutcome::Appended
            }
            None => {
                self.resume_after_any_gap();
                let id = self.ids.next();
                let reserve =
                    usize::try_from(config.expected_segment_bytes()).unwrap_or(usize::MAX);
                self.open = Some(OpenSegment::open(id, packet, reserve));
                self.counters.segments_opened += 1;
                PushOutcome::OpenedSegment(id)
            }
        };

        // Every packet, rather than every seal. The retention rule is measured
        // against the newest picture in the buffer, and the newest picture is
        // in the segment being written, so a buffer that only evicted at a
        // segment boundary would hold a segment more than it was asked for and
        // would carry on growing past its ceiling until the next keyframe.
        self.evict(config);
        self.peak_bytes = self.peak_bytes.max(self.alive_bytes());
        outcome
    }

    /// Closes the open segment and adds it to the queue.
    fn seal(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };

        let segment = Arc::new(open.seal());
        self.sealed_bytes += segment.resident_bytes() as u64;
        self.sealed.push_back(segment);
    }

    /// Accounts for a packet that cannot be buffered because no segment is
    /// open.
    ///
    /// Two different things look the same from here and must not be reported as
    /// one: an encoder that has not yet produced its first keyframe, which is
    /// ordinary and lossless, and a buffer that sealed the segment it was
    /// writing to stay under its ceiling, which is losing video.
    fn discard_awaiting_keyframe(&mut self) -> PushOutcome {
        match &mut self.ceiling_gap {
            Some(lost) => {
                *lost += 1;
                self.counters.packets_discarded_over_ceiling += 1;
                PushOutcome::DiscardedOverCeiling
            }
            None => {
                self.counters.packets_discarded_before_first_keyframe += 1;
                PushOutcome::AwaitingKeyframe
            }
        }
    }

    /// Makes room for `incoming` bytes in the segment being written, sealing it
    /// early if nothing else can.
    ///
    /// The cost of the append is asked for rather than assumed, because a
    /// `Vec` that has to grow costs more than the packet going into it, and a
    /// ceiling checked after the memory has been committed is not a ceiling.
    fn make_room(&mut self, config: &ReplayConfig, cost_of: impl Fn(&OpenSegment) -> u64) {
        let Some(cost) = self.open.as_ref().map(cost_of) else {
            // No segment is open, so this packet is a keyframe about to start
            // one. Its reservation is smaller than any permitted ceiling
            // (`ReplayConfig::with_memory_ceiling` refuses a ceiling below the
            // window, which is longer than a segment), and `evict` brings the
            // sealed queue back under it after the segment opens.
            return;
        };

        // One sealed segment is kept, as everywhere else the ceiling is
        // enforced: cutting the segment being written short costs the next few
        // frames, and it is the lesser loss of the two.
        let ceiling = config.memory_ceiling();
        while self.sealed_bytes + cost > ceiling && self.sealed.len() >= 2 {
            self.drop_front();
            self.counters.segments_evicted_over_ceiling += 1;
            self.report_ceiling(config, ceiling);
        }

        if self.sealed_bytes + cost > ceiling {
            self.seal_at_the_ceiling(config);
        }
    }

    /// Adds one block of captured audio to the segment being written.
    fn push_audio(
        &mut self,
        config: &ReplayConfig,
        track: TrackId,
        at: Duration,
        samples: &[f32],
    ) -> AudioPushOutcome {
        // Nothing to attach it to. A segment begins on a keyframe, and audio
        // buffered before the first one could only be written into a clip that
        // has no video to align it against.
        if self.open.is_none() {
            self.counters.audio_blocks_discarded_before_first_keyframe += 1;
            return AudioPushOutcome::DiscardedAwaitingKeyframe;
        }

        // The same order the video half uses: room first, because a ceiling
        // checked after the samples are copied in is not a ceiling. This can
        // seal the open segment, and then there is nowhere to put the block.
        self.make_room(config, |open| {
            open.resident_bytes_after_audio(samples.len()) as u64
        });
        let Some(open) = &mut self.open else {
            self.counters.audio_blocks_discarded_over_ceiling += 1;
            return AudioPushOutcome::DiscardedOverCeiling;
        };

        open.append_audio(track, at, samples);
        self.counters.audio_blocks_buffered += 1;
        self.evict(config);
        self.peak_bytes = self.peak_bytes.max(self.alive_bytes());
        AudioPushOutcome::Appended
    }

    /// Cuts the segment being written short, because the ceiling leaves no room
    /// for another packet in it.
    ///
    /// What is sealed still begins on a keyframe, so it stays decodable and
    /// leasable; what follows is discarded until the encoder produces the next
    /// one. The module documentation covers the whole sequence and why the
    /// alternatives are worse.
    fn seal_at_the_ceiling(&mut self, config: &ReplayConfig) {
        self.seal();
        self.ceiling_gap = Some(0);
        self.counters.segments_sealed_at_the_ceiling += 1;

        tracing::warn!(
            ceiling_bytes = config.memory_ceiling(),
            held_bytes = self.owned_bytes(),
            segment_seconds = config.segment_duration().as_secs_f64(),
            "the replay buffer reached its memory ceiling inside a single segment and cut it \
             short; video is being dropped until the encoder's next keyframe, because the \
             encoder is producing keyframes far less often than this buffer can hold"
        );
    }

    /// Lets go of everything from before a gap, when video resumes after one.
    ///
    /// Only what was lost matters: a segment sealed at the ceiling and followed
    /// immediately by a keyframe leaves no gap at all, and nothing is dropped.
    fn resume_after_any_gap(&mut self) {
        let Some(lost) = self.ceiling_gap.take() else {
            return;
        };
        if lost == 0 {
            return;
        }

        // `lease_last` measures back from the newest picture, so leaving this
        // material in place would let a save select across the gap and write a
        // clip that jumps without saying so (AGENTS.md section 22).
        while !self.sealed.is_empty() {
            self.drop_front();
            self.counters.segments_evicted_over_ceiling += 1;
        }
    }

    /// Says once, at `warn`, that the ceiling is costing the buffer history.
    fn report_ceiling(&mut self, config: &ReplayConfig, ceiling: u64) {
        if self.ceiling_reported {
            return;
        }

        self.ceiling_reported = true;
        tracing::warn!(
            ceiling_bytes = ceiling,
            held_bytes = self.owned_bytes(),
            window_seconds = config.window().as_secs_f64(),
            "the replay buffer reached its memory ceiling and is keeping less than the window it \
             was configured for, because the encoder is producing more than the bitrate the \
             buffer was sized from"
        );
    }

    /// Drops segments the window no longer needs, then any the ceiling forbids.
    fn evict(&mut self, config: &ReplayConfig) {
        self.release_finished_leases();

        let Some(end) = self.latest_presentation() else {
            return;
        };

        // The window rule: the front goes only when the segment behind it still
        // reaches back far enough to cover the configured window.
        while self.sealed.len() >= 2 {
            let without_front = end.saturating_sub(self.sealed[1].start());
            if without_front < config.window() {
                break;
            }
            self.drop_front();
            self.counters.segments_evicted_for_window += 1;
        }

        // The ceiling rule, which can only bite when the encoder is producing
        // more than the bitrate the buffer was sized from.
        let ceiling = config.memory_ceiling();
        while self.owned_bytes() > ceiling && self.sealed.len() >= 2 {
            self.drop_front();
            self.counters.segments_evicted_over_ceiling += 1;
            self.report_ceiling(config, ceiling);
        }
    }

    /// Removes the oldest sealed segment, keeping it alive if a lease is
    /// reading it.
    ///
    /// This is the whole of "eviction never drops a segment currently being
    /// read". The reference count is checked under the same lock a lease is
    /// taken under, so a segment cannot acquire a reader between the check and
    /// the drop.
    fn drop_front(&mut self) {
        let Some(segment) = self.sealed.pop_front() else {
            return;
        };

        let bytes = segment.resident_bytes() as u64;
        self.sealed_bytes = self.sealed_bytes.saturating_sub(bytes);

        if Arc::strong_count(&segment) > 1 {
            self.leased_bytes += bytes;
            self.leased.push(segment);
        }
    }

    /// Lets go of evicted segments whose last reader has finished.
    fn release_finished_leases(&mut self) {
        let mut freed = 0;
        self.leased.retain(|segment| {
            if Arc::strong_count(segment) == 1 {
                freed += segment.resident_bytes() as u64;
                false
            } else {
                true
            }
        });
        self.leased_bytes = self.leased_bytes.saturating_sub(freed);
    }

    fn lease(&mut self, request: Request) -> Result<SegmentLease, LeaseError> {
        self.release_finished_leases();

        let held = self.held().ok_or(LeaseError::Empty)?;
        let (requested, requested_length) = match request {
            Request::Range(range) => (range, range.length()),
            Request::Last(length) => (TimeRange::ending_at(held.end(), length), length),
        };

        if !held.overlaps(requested) {
            return Err(LeaseError::OutsideBuffer { requested, held });
        }

        let starts: Vec<Duration> = self
            .sealed
            .iter()
            .map(|segment| segment.start())
            .chain(self.open.as_ref().map(OpenSegment::start))
            .collect();

        // The last segment beginning at or before the requested start, so that
        // the clip begins on a keyframe, and every segment up to the last one
        // beginning at or before the requested end.
        let first = starts
            .iter()
            .rposition(|start| *start <= requested.start())
            .unwrap_or(0);
        let last = starts
            .iter()
            .rposition(|start| *start <= requested.end())
            .unwrap_or(0)
            .max(first);

        let mut segments: Vec<Arc<Segment>> = self
            .sealed
            .iter()
            .skip(first)
            .take(last + 1 - first)
            .map(Arc::clone)
            .collect();

        // The open segment is the newest material there is — the seconds
        // somebody has just pressed a hotkey about — and it cannot be shared
        // while it is still being written, so the lease takes a copy of it.
        if last == starts.len() - 1 {
            if let Some(open) = &self.open {
                segments.push(Arc::new(open.snapshot()));
            }
        }

        self.counters.leases_taken += 1;
        Ok(SegmentLease::new(segments, requested, requested_length))
    }

    /// The newest presentation time in the buffer.
    fn latest_presentation(&self) -> Option<Duration> {
        self.open
            .as_ref()
            .map(OpenSegment::last_presentation)
            .or_else(|| {
                self.sealed
                    .back()
                    .map(|segment| segment.last_presentation())
            })
    }

    /// The media time held, from the oldest keyframe to the newest picture.
    fn held(&self) -> Option<TimeRange> {
        let start = self
            .sealed
            .front()
            .map(|segment| segment.start())
            .or_else(|| self.open.as_ref().map(OpenSegment::start))?;

        Some(TimeRange::new(start, self.latest_presentation()?))
    }

    /// The segments this buffer owns, in bytes. What the ceiling governs.
    fn owned_bytes(&self) -> u64 {
        self.sealed_bytes
            + self
                .open
                .as_ref()
                .map_or(0, |open| open.resident_bytes() as u64)
    }

    /// Everything this buffer is keeping alive, including what saves are
    /// reading.
    fn alive_bytes(&self) -> u64 {
        self.owned_bytes() + self.leased_bytes
    }

    fn stats(&self) -> ReplayStats {
        ReplayStats {
            segments_held: self.sealed.len() + usize::from(self.open.is_some()),
            packets_held: self
                .sealed
                .iter()
                .map(|segment| segment.len() as u64)
                .sum::<u64>()
                + self.open.as_ref().map_or(0, |open| open.len() as u64),
            bytes_held: self.alive_bytes(),
            bytes_retained_for_a_save: self.leased_bytes,
            peak_bytes_held: self.peak_bytes,
            segments_retained_for_a_save: self.leased.len(),
            covered: self.held(),
            packets_buffered: self.counters.packets_buffered,
            packets_discarded_before_first_keyframe: self
                .counters
                .packets_discarded_before_first_keyframe,
            segments_opened: self.counters.segments_opened,
            segments_evicted_for_window: self.counters.segments_evicted_for_window,
            segments_evicted_over_ceiling: self.counters.segments_evicted_over_ceiling,
            segments_sealed_at_the_ceiling: self.counters.segments_sealed_at_the_ceiling,
            packets_discarded_over_ceiling: self.counters.packets_discarded_over_ceiling,
            audio_blocks_buffered: self.counters.audio_blocks_buffered,
            audio_blocks_discarded_before_first_keyframe: self
                .counters
                .audio_blocks_discarded_before_first_keyframe,
            audio_blocks_discarded_over_ceiling: self.counters.audio_blocks_discarded_over_ceiling,
            leases_taken: self.counters.leases_taken,
        }
    }
}

/// What a buffer holds, and what it has done since it was created.
///
/// The counts are deliberately separate rather than one "evicted" number, for
/// the reason `clipped_session::RecordingReport` gives: a segment dropped
/// because the window moved past it is the buffer working, and one dropped
/// because the ceiling was reached is the buffer keeping less than it was asked
/// to (AGENTS.md section 19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayStats {
    segments_held: usize,
    packets_held: u64,
    bytes_held: u64,
    bytes_retained_for_a_save: u64,
    peak_bytes_held: u64,
    segments_retained_for_a_save: usize,
    covered: Option<TimeRange>,
    packets_buffered: u64,
    packets_discarded_before_first_keyframe: u64,
    segments_opened: u64,
    segments_evicted_for_window: u64,
    segments_evicted_over_ceiling: u64,
    segments_sealed_at_the_ceiling: u64,
    packets_discarded_over_ceiling: u64,
    audio_blocks_buffered: u64,
    audio_blocks_discarded_before_first_keyframe: u64,
    audio_blocks_discarded_over_ceiling: u64,
    leases_taken: u64,
}

impl ReplayStats {
    /// Segments held, including the one being written.
    #[must_use]
    pub const fn segments_held(&self) -> usize {
        self.segments_held
    }

    /// Packets held, including those in the segment being written.
    #[must_use]
    pub const fn packets_held(&self) -> u64 {
        self.packets_held
    }

    /// Memory the buffer is keeping alive, including segments it has evicted
    /// that a save is still reading.
    #[must_use]
    pub const fn bytes_held(&self) -> u64 {
        self.bytes_held
    }

    /// Blocks of captured audio copied into the buffer.
    #[must_use]
    pub const fn audio_blocks_buffered(&self) -> u64 {
        self.audio_blocks_buffered
    }

    /// Blocks that arrived before the first keyframe and had no segment to go
    /// in.
    ///
    /// A handful at the start of every recording is ordinary: the audio threads
    /// begin before the encoder has produced a keyframe. A number that keeps
    /// climbing means the buffer is not holding what the file is being written
    /// from.
    #[must_use]
    pub const fn audio_blocks_discarded_before_first_keyframe(&self) -> u64 {
        self.audio_blocks_discarded_before_first_keyframe
    }

    /// Blocks dropped because the memory ceiling left no segment open.
    ///
    /// Non-zero means clips from this buffer have gaps in their audio, which
    /// is worth saying out loud rather than leaving somebody to hear it.
    #[must_use]
    pub const fn audio_blocks_discarded_over_ceiling(&self) -> u64 {
        self.audio_blocks_discarded_over_ceiling
    }

    /// The part of [`bytes_held`](Self::bytes_held) that is only alive because
    /// a save is reading it.
    ///
    /// Subtract it to get what the buffer owns, which is what
    /// [`ReplayConfig::memory_ceiling`](crate::ReplayConfig::memory_ceiling)
    /// governs.
    #[must_use]
    pub const fn bytes_retained_for_a_save(&self) -> u64 {
        self.bytes_retained_for_a_save
    }

    /// The most memory it has ever held at once, saves included.
    #[must_use]
    pub const fn peak_bytes_held(&self) -> u64 {
        self.peak_bytes_held
    }

    /// Evicted segments a save is still reading.
    #[must_use]
    pub const fn segments_retained_for_a_save(&self) -> usize {
        self.segments_retained_for_a_save
    }

    /// The media time held, or [`None`] if nothing is.
    #[must_use]
    pub const fn covered(&self) -> Option<TimeRange> {
        self.covered
    }

    /// Packets accepted since the buffer was created.
    #[must_use]
    pub const fn packets_buffered(&self) -> u64 {
        self.packets_buffered
    }

    /// Packets discarded because no keyframe had arrived to begin a segment.
    #[must_use]
    pub const fn packets_discarded_before_first_keyframe(&self) -> u64 {
        self.packets_discarded_before_first_keyframe
    }

    /// Segments started since the buffer was created.
    #[must_use]
    pub const fn segments_opened(&self) -> u64 {
        self.segments_opened
    }

    /// Segments dropped because the window had moved past them, which is the
    /// buffer doing what it exists to do.
    #[must_use]
    pub const fn segments_evicted_for_window(&self) -> u64 {
        self.segments_evicted_for_window
    }

    /// Segments dropped because the memory ceiling was reached, which is the
    /// buffer keeping less history than it was configured for.
    ///
    /// Includes the segments dropped on the far side of a gap left by
    /// [`packets_discarded_over_ceiling`](Self::packets_discarded_over_ceiling),
    /// since the ceiling is what caused the gap.
    #[must_use]
    pub const fn segments_evicted_over_ceiling(&self) -> u64 {
        self.segments_evicted_over_ceiling
    }

    /// Times the segment being written was cut short because the ceiling left
    /// no room for another packet in it.
    ///
    /// Zero for every encoder in this workspace. A non-zero count means the
    /// encoder's keyframe interval is too long for this buffer to hold a whole
    /// segment of, so the buffer cannot serve a clip of the window it was
    /// configured for; `crate::buffer` describes what it does instead.
    #[must_use]
    pub const fn segments_sealed_at_the_ceiling(&self) -> u64 {
        self.segments_sealed_at_the_ceiling
    }

    /// Packets dropped while waiting for a keyframe to resume on after a
    /// segment was cut short at the ceiling.
    ///
    /// This is video the buffer did not keep, as distinct from video it evicted
    /// after keeping.
    #[must_use]
    pub const fn packets_discarded_over_ceiling(&self) -> u64 {
        self.packets_discarded_over_ceiling
    }

    /// Leases taken since the buffer was created.
    #[must_use]
    pub const fn leases_taken(&self) -> u64 {
        self.leases_taken
    }
}

#[cfg(test)]
mod tests {
    use clipped_encoder::{BitRate, PictureKind};

    use super::*;

    /// A packet of `size` bytes at `at`, whose bytes identify it.
    fn packet(at: Duration, size: usize, keyframe: bool) -> (Vec<u8>, Duration, bool) {
        #[allow(clippy::cast_possible_truncation)]
        let fill = (at.as_millis() % 251) as u8;
        (vec![fill; size], at, keyframe)
    }

    fn push(
        buffer: &ReplayBuffer,
        (data, at, keyframe): &(Vec<u8>, Duration, bool),
    ) -> PushOutcome {
        buffer.push(&EncodedPacket::new(
            data,
            *at,
            *at,
            if *keyframe {
                PictureKind::Keyframe
            } else {
                PictureKind::Predicted
            },
        ))
    }

    /// A buffer with a 30 second window and one second segments, fed 10 packets
    /// a second of 1000 bytes each: 10 kbit/s of arithmetic that is easy to
    /// check by hand.
    fn buffer(window_seconds: u64) -> ReplayBuffer {
        let config = ReplayConfig::new(
            Duration::from_secs(window_seconds),
            BitRate::bits_per_second(80_000).expect("a real rate"),
        )
        .expect("a supported window")
        .with_segment_duration(Duration::from_secs(1))
        .expect("one second fits");

        ReplayBuffer::new(config)
    }

    /// Feeds `seconds` of 10 fps video, a keyframe every second.
    fn fill(buffer: &ReplayBuffer, seconds: u64) {
        feed(buffer, 0, seconds * 10);
    }

    /// Feeds `frames` frames starting at frame `from`, at 10 fps.
    fn feed(buffer: &ReplayBuffer, from: u64, frames: u64) {
        for frame in from..from + frames {
            let at = Duration::from_millis(frame * 100);
            push(buffer, &packet(at, 1000, frame % 10 == 0));
        }
    }

    #[test]
    fn nothing_is_buffered_until_the_first_keyframe() {
        // A segment that does not begin on a keyframe cannot be decoded on its
        // own, so there is nowhere to put a predicted picture that arrives
        // first.
        let buffer = buffer(30);

        for frame in 0..5 {
            let at = Duration::from_millis(frame * 100);
            assert_eq!(
                push(&buffer, &packet(at, 1000, false)),
                PushOutcome::AwaitingKeyframe
            );
        }

        assert!(buffer.held().is_none());
        assert_eq!(buffer.stats().packets_discarded_before_first_keyframe(), 5);
        assert_eq!(buffer.stats().packets_buffered(), 0);
    }

    #[test]
    fn a_segment_begins_only_on_a_keyframe() {
        let buffer = buffer(30);
        fill(&buffer, 5);

        let stats = buffer.stats();
        assert_eq!(stats.segments_opened(), 5, "one per keyframe");
        assert_eq!(stats.packets_buffered(), 50);
    }

    #[test]
    fn a_keyframe_before_the_target_length_does_not_start_a_segment() {
        // An encoder configured with a shorter keyframe interval than the
        // buffer's segment target must not produce a segment per keyframe: the
        // target is what decides, and every extra segment is an extra eviction
        // boundary and an extra index.
        let buffer = buffer(30);
        for frame in 0..50u64 {
            let at = Duration::from_millis(frame * 100);
            // A keyframe every 200 ms, five times as often as the one second
            // segment target.
            push(&buffer, &packet(at, 1000, frame % 2 == 0));
        }

        assert_eq!(buffer.stats().segments_opened(), 5);
    }

    #[test]
    fn a_full_buffer_holds_the_window_and_no_more_than_one_segment_over() {
        // The retention rule, stated as an interval rather than an exact
        // number: dropping the oldest segment the moment the window is covered
        // would leave less than the window whenever a segment is only partly
        // needed.
        let buffer = buffer(30);
        fill(&buffer, 90);

        let held = buffer.held().expect("ninety seconds were pushed");
        assert!(
            held.length() >= Duration::from_secs(30),
            "the buffer kept {} of a thirty second window",
            held.length().as_secs_f64()
        );
        assert!(
            held.length() <= Duration::from_secs(31),
            "the buffer kept {} for a thirty second window, which is more than the window plus \
             the one segment the retention rule allows",
            held.length().as_secs_f64()
        );
        assert!(buffer.stats().segments_evicted_for_window() > 0);
        assert_eq!(buffer.stats().segments_evicted_over_ceiling(), 0);
    }

    #[test]
    fn the_window_is_covered_before_anything_is_evicted() {
        // Twenty seconds into a thirty second buffer nothing has left, because
        // nothing may: the buffer does not yet hold what it was configured to.
        let buffer = buffer(30);
        fill(&buffer, 20);

        assert_eq!(buffer.stats().segments_evicted_for_window(), 0);
        assert_eq!(buffer.stats().segments_held(), 20);
    }

    #[test]
    fn eviction_drops_whole_segments_and_never_a_partial_one() {
        // Every segment the buffer holds has to begin on a keyframe, so the
        // oldest one either goes or stays. A buffer that trimmed packets off
        // the front of a segment would be holding pictures that cannot be
        // decoded.
        let buffer = buffer(30);
        fill(&buffer, 90);

        let lease = buffer
            .lease_last(Duration::from_secs(30))
            .expect("thirty seconds are held");
        for segment in lease.segments() {
            assert!(
                segment
                    .packets()
                    .next()
                    .expect("a segment has packets")
                    .is_keyframe(),
                "{} does not begin on a keyframe",
                segment.id()
            );
        }
    }

    #[test]
    fn the_memory_ceiling_shortens_the_window_rather_than_growing_the_process() {
        // What happens when the machine cannot provide what the configuration
        // asks for. The bitrate here is a tenth of what is actually pushed, so
        // the ceiling derived from it bites almost immediately.
        let config = ReplayConfig::new(
            Duration::from_secs(30),
            BitRate::bits_per_second(8_000).expect("a real rate"),
        )
        .expect("a supported window")
        .with_segment_duration(Duration::from_secs(1))
        .expect("one second fits");
        let ceiling = config.memory_ceiling();
        let buffer = ReplayBuffer::new(config);

        fill(&buffer, 60);

        let stats = buffer.stats();
        assert!(
            stats.segments_evicted_over_ceiling() > 0,
            "the ceiling never bit: {stats:?}"
        );
        assert!(
            stats.peak_bytes_held() <= ceiling,
            "held {} against a ceiling of {ceiling}",
            stats.peak_bytes_held()
        );
        let held = buffer.held().expect("something is held");
        assert!(
            held.length() < Duration::from_secs(30),
            "the window should have been shortened, and covers {}",
            held.length().as_secs_f64()
        );
    }

    /// The 1080p60 rate `clipped-session` gives a recording, so that the
    /// ceiling under test is the one a real configuration produces.
    fn rate_1080p60() -> BitRate {
        BitRate::bits_per_second(18_662_400).expect("a real rate")
    }

    #[test]
    fn the_segment_being_written_is_held_to_the_ceiling_like_every_other() {
        // The case the ceiling used to be blind to: an encoder whose keyframe
        // interval is longer than the buffer's whole window. One keyframe and
        // five minutes of predicted pictures produce a single segment that no
        // amount of evicting *sealed* segments can shrink, and a buffer that
        // only weighed the sealed ones grew to 1,196,228,696 bytes against a
        // 111,974,400 byte ceiling — ten times over, in the subsystem whose
        // entire purpose is bounded memory.
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60())
            .expect("thirty seconds is in range");
        let ceiling = config.memory_ceiling();
        let buffer = ReplayBuffer::new(config);

        // 60 fps of 1080p60-sized packets for five minutes, keyframe first and
        // never again.
        for frame in 0..18_000u64 {
            let at = Duration::from_micros(frame * 1_000_000 / 60);
            push(&buffer, &packet(at, 38_880, frame == 0));
        }

        let stats = buffer.stats();
        assert!(
            stats.bytes_held() <= ceiling,
            "the buffer held {} against a ceiling of {ceiling}",
            stats.bytes_held()
        );
        assert!(
            stats.peak_bytes_held() <= ceiling,
            "the buffer peaked at {} against a ceiling of {ceiling}",
            stats.peak_bytes_held()
        );
        assert!(
            stats.packets_discarded_over_ceiling() > 0,
            "the loss should be counted rather than silent: {stats:?}"
        );
        assert_eq!(stats.segments_sealed_at_the_ceiling(), 1);
    }

    /// Feeds frames `from` to `until` of 1080p60-sized packets at 60 fps, with
    /// a keyframe wherever `keyframe` says so.
    fn feed_1080p60(buffer: &ReplayBuffer, from: u64, until: u64, keyframe: impl Fn(u64) -> bool) {
        for frame in from..until {
            let at = Duration::from_micros(frame * 1_000_000 / 60);
            // 18,662,400 bit/s at 60 fps is exactly this many bytes a frame.
            push(buffer, &packet(at, 38_880, keyframe(frame)));
        }
    }

    #[test]
    fn a_segment_cut_short_at_the_ceiling_is_still_a_segment_a_save_can_use() {
        // Sealing the open segment where it stands, rather than dropping it, is
        // what makes this true: a segment is cut at its *end*, so what is kept
        // still begins on a keyframe and is still decodable on its own. A
        // recording that stops during the gap still has a clip in it.
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60())
            .expect("thirty seconds is in range");
        let buffer = ReplayBuffer::new(config);

        feed_1080p60(&buffer, 0, 18_000, |frame| frame == 0);

        let lease = buffer
            .lease_last(Duration::from_secs(30))
            .expect("the segment cut short is still held");
        assert!(
            lease
                .packets()
                .next()
                .expect("a lease holds packets")
                .is_keyframe(),
            "what the ceiling left behind does not begin on a keyframe, so nothing can decode it"
        );
        assert!(lease.is_complete(), "{lease:?}");
    }

    #[test]
    fn video_from_before_a_gap_is_never_leased_alongside_video_from_after_it() {
        // The reason the older material goes when video resumes. `lease_last`
        // measures back from the newest picture, so a buffer holding both sides
        // of a gap would select across it and write one clip that jumps from
        // the first minute to the third without saying so (AGENTS.md section
        // 22).
        //
        // Quarter-second segments, which is a caller asking for finer
        // granularity than the encoder will give it — allowed, and documented
        // as changing nothing on its own. It matters here because it leaves the
        // ceiling with room to spare once the segment has been cut short, which
        // is what makes this test about the buffer letting go of the old
        // material *deliberately* rather than about the ceiling evicting it
        // anyway as the next segment fills. That precondition is asserted
        // below rather than assumed, so a change to the growth policy or the
        // ceiling arithmetic that quietly restored the masking would fail here
        // instead of leaving a test that cannot bite.
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60())
            .expect("thirty seconds is in range")
            .with_segment_duration(Duration::from_millis(250))
            .expect("a quarter of a second fits in thirty");
        let buffer = ReplayBuffer::new(config);

        // One keyframe and no more, until the segment has been cut short and
        // video is being dropped.
        let resumed = 3_000;
        feed_1080p60(&buffer, 0, resumed, |frame| frame == 0);

        let before = buffer.stats();
        assert_eq!(before.segments_sealed_at_the_ceiling(), 1, "{before:?}");
        assert!(before.packets_discarded_over_ceiling() > 0, "{before:?}");
        assert!(
            before.bytes_held() + config.expected_segment_bytes() < config.memory_ceiling(),
            "the ceiling has no room for another segment beside what was cut short, so it would \
             evict the older material on its own and this test could pass without the buffer \
             ever letting go of it: {before:?}"
        );

        // The keyframe video resumes on, and two frames after it. Stopping
        // there is the point: further on the ceiling does remove the older
        // material by itself.
        feed_1080p60(&buffer, resumed, resumed + 3, |frame| frame == resumed);

        let resumed_at = Duration::from_secs(50);
        let held = buffer.held().expect("the resumed video is held");
        assert!(
            held.start() >= resumed_at,
            "the buffer still offers material from before the gap: it holds {held}"
        );

        let lease = buffer
            .lease_last(Duration::from_secs(30))
            .expect("thirty seconds of resumed video are held");
        let times: Vec<Duration> = lease
            .packets()
            .map(|packet| packet.presentation_time())
            .collect();
        for pair in times.windows(2) {
            assert!(
                pair[1].saturating_sub(pair[0]) <= Duration::from_millis(17),
                "a save would write a clip that jumps from {:?} to {:?}",
                pair[0],
                pair[1]
            );
        }
        assert!(
            times.first().is_some_and(|first| *first >= resumed_at),
            "the clip reaches back over the gap"
        );
    }

    #[test]
    fn the_buffer_returns_to_its_configured_window_after_a_gap() {
        // A gap costs the history that was on the far side of it, and nothing
        // else: the buffer is not left crippled by a spell of misconfiguration.
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60())
            .expect("thirty seconds is in range");
        let buffer = ReplayBuffer::new(config);

        let resumed = 9_000;
        feed_1080p60(&buffer, 0, 15_000, |frame| {
            frame == 0 || (frame >= resumed && (frame - resumed) % 120 == 0)
        });

        let held = buffer.held().expect("the resumed video is held");
        assert!(
            held.length() >= Duration::from_secs(30) && held.length() < Duration::from_secs(32),
            "the buffer holds {} after recovering from a gap",
            held.length().as_secs_f64()
        );
        assert_eq!(buffer.stats().segments_sealed_at_the_ceiling(), 1);
    }

    #[test]
    fn a_lease_covers_at_least_what_was_asked_for() {
        let buffer = buffer(30);
        fill(&buffer, 60);

        let requested =
            TimeRange::new(Duration::from_millis(41_500), Duration::from_millis(43_500));
        let lease = buffer.lease(requested).expect("the range is held");

        assert!(lease.is_complete(), "{lease:?}");
        assert!(lease.covered().start() <= requested.start());
        assert!(lease.covered().end() >= requested.end());
        assert_eq!(lease.shortfall(), Duration::ZERO);
    }

    #[test]
    fn a_lease_begins_on_the_keyframe_before_the_requested_start() {
        // The granularity a save is bought at: asking for a range beginning at
        // 41.5 s yields a clip beginning at the 41 s keyframe, because a clip
        // beginning anywhere else could not be decoded.
        let buffer = buffer(30);
        fill(&buffer, 60);

        let lease = buffer
            .lease(TimeRange::new(
                Duration::from_millis(41_500),
                Duration::from_millis(43_500),
            ))
            .expect("the range is held");

        assert_eq!(lease.covered().start(), Duration::from_secs(41));
        assert_eq!(lease.leading_slack(), Duration::from_millis(500));
        assert!(
            lease.trailing_slack() <= Duration::from_secs(1),
            "{:?}",
            lease.trailing_slack()
        );
    }

    #[test]
    fn a_lease_selects_exactly_the_segments_that_overlap_the_range() {
        let buffer = buffer(30);
        fill(&buffer, 60);

        // 41.5 s to 43.5 s spans the segments beginning at 41, 42 and 43.
        let lease = buffer
            .lease(TimeRange::new(
                Duration::from_millis(41_500),
                Duration::from_millis(43_500),
            ))
            .expect("the range is held");

        assert_eq!(lease.len(), 3);
        let starts: Vec<u64> = lease
            .segments()
            .map(|segment| segment.start().as_secs())
            .collect();
        assert_eq!(starts, vec![41, 42, 43]);
    }

    #[test]
    fn the_newest_material_is_leasable_before_its_segment_is_sealed() {
        // The two seconds somebody just pressed a hotkey about are in the open
        // segment. A buffer that only offered sealed segments would lose them,
        // which is the part of the clip that mattered.
        let buffer = buffer(30);
        fill(&buffer, 10);
        // Half a segment more, sealed by nothing.
        feed(&buffer, 100, 5);

        let held = buffer.held().expect("ten seconds were pushed");
        assert_eq!(held.end(), Duration::from_millis(10_400));

        let lease = buffer
            .lease_last(Duration::from_secs(2))
            .expect("two seconds are held");
        assert_eq!(lease.covered().end(), Duration::from_millis(10_400));
    }

    #[test]
    fn leasing_more_than_the_buffer_holds_yields_what_there_is_and_says_so() {
        // The hotkey pressed four seconds into a session. Refusing would be
        // wrong — there is a clip to be had — and claiming thirty seconds
        // would be a lie.
        let buffer = buffer(30);
        fill(&buffer, 4);

        let lease = buffer
            .lease_last(Duration::from_secs(30))
            .expect("four seconds are held");

        assert!(!lease.is_complete());
        assert_eq!(lease.covered().start(), Duration::ZERO);
        assert!(lease.shortfall() >= Duration::from_secs(26));
    }

    #[test]
    fn a_range_older_than_anything_held_is_refused_with_what_is_held() {
        let buffer = buffer(30);
        fill(&buffer, 90);

        let error = buffer
            .lease(TimeRange::new(Duration::ZERO, Duration::from_secs(5)))
            .expect_err("the first five seconds were evicted an hour of frames ago");

        match error {
            LeaseError::OutsideBuffer { held, .. } => {
                assert!(held.start() > Duration::from_secs(5), "{held}");
            }
            other => panic!("expected an out-of-buffer refusal, got {other}"),
        }
    }

    #[test]
    fn a_range_in_the_future_is_refused() {
        let buffer = buffer(30);
        fill(&buffer, 30);

        assert!(matches!(
            buffer.lease(TimeRange::new(
                Duration::from_secs(60),
                Duration::from_secs(90)
            )),
            Err(LeaseError::OutsideBuffer { .. })
        ));
    }

    #[test]
    fn an_empty_buffer_refuses_a_lease() {
        let buffer = buffer(30);

        assert_eq!(
            buffer
                .lease_last(Duration::from_secs(30))
                .expect_err("nothing has been pushed"),
            LeaseError::Empty
        );
    }

    #[test]
    fn a_lease_reads_back_the_packets_that_were_pushed() {
        // Retention is worth nothing if what comes back is not what went in.
        let buffer = buffer(30);
        fill(&buffer, 20);

        let lease = buffer
            .lease(TimeRange::new(
                Duration::from_secs(10),
                Duration::from_secs(12),
            ))
            .expect("the range is held");

        for packet in lease.packets() {
            let (expected, _, _) = packet_at(packet.presentation_time());
            assert_eq!(packet.data(), expected.as_slice());
        }
        assert_eq!(lease.packets().count(), 30, "three one-second segments");
    }

    /// The packet the fixture would have produced at `at`.
    fn packet_at(at: Duration) -> (Vec<u8>, Duration, bool) {
        packet(at, 1000, at.as_millis() % 1000 == 0)
    }

    #[test]
    fn what_is_held_is_the_sum_of_what_the_segments_hold() {
        let buffer = buffer(30);
        fill(&buffer, 60);

        let stats = buffer.stats();
        assert_eq!(stats.segments_retained_for_a_save(), 0);
        assert!(stats.bytes_held() >= stats.packets_held() * 1000);
        assert!(
            stats.bytes_held() <= stats.peak_bytes_held(),
            "the peak cannot be below what is held now"
        );
    }

    #[test]
    fn an_evicted_segment_a_lease_is_reading_is_kept_and_counted() {
        // The single-threaded half of "eviction never drops a segment being
        // read": the buffer lets go, the lease does not, and the memory is
        // still counted against the ceiling rather than becoming invisible.
        // `tests/save_during_eviction.rs` is the half that races them.
        let buffer = buffer(30);
        fill(&buffer, 40);

        let lease = buffer
            .lease(TimeRange::new(
                Duration::from_secs(11),
                Duration::from_secs(13),
            ))
            .expect("the range is held");
        let ids: Vec<_> = lease.segments().map(Segment::id).collect();

        // Enough to move the window well past everything the lease holds.
        feed(&buffer, 400, 600);

        let stats = buffer.stats();
        assert_eq!(
            stats.segments_retained_for_a_save(),
            ids.len(),
            "the buffer should be holding the leased segments open"
        );
        for packet in lease.packets() {
            let (expected, _, _) = packet_at(packet.presentation_time());
            assert_eq!(packet.data(), expected.as_slice());
        }

        drop(lease);
        // Eviction is what releases them, and it runs on the next packet.
        feed(&buffer, 1000, 20);
        assert_eq!(buffer.stats().segments_retained_for_a_save(), 0);
    }

    #[test]
    fn a_save_in_progress_does_not_shorten_the_window_it_was_saved_from() {
        // Counting a lease against the ceiling looks tidy and is wrong: a save
        // would evict the buffer's history to pay for itself, collapse it to a
        // single segment for as long as the clip took to write, and leave the
        // next hotkey press with nothing. The memory is reported instead.
        let buffer = buffer(30);
        fill(&buffer, 60);
        let before = buffer.held().expect("sixty seconds were pushed").length();

        let lease = buffer
            .lease_last(Duration::from_secs(20))
            .expect("twenty seconds are held");
        feed(&buffer, 600, 300);

        let stats = buffer.stats();
        let after = buffer.held().expect("still recording").length();
        assert!(
            after >= before,
            "the window shrank from {} to {} while a save was reading it",
            before.as_secs_f64(),
            after.as_secs_f64()
        );
        assert_eq!(stats.segments_evicted_over_ceiling(), 0);
        assert!(
            stats.bytes_retained_for_a_save() > 0,
            "the save's memory should be reported: {stats:?}"
        );
        assert_eq!(
            stats.bytes_held() - stats.bytes_retained_for_a_save(),
            buffer.locked().owned_bytes(),
            "what the buffer owns is what the ceiling governs"
        );
        drop(lease);
    }

    #[test]
    fn audio_arriving_before_the_first_keyframe_has_nowhere_to_go_and_is_counted() {
        // A segment begins on a keyframe. Audio that arrives before one has no
        // segment to sit in and no picture to sit beside, so keeping it would
        // mean holding samples that could only ever be written into a clip
        // with no video at that instant.
        let buffer = buffer(30);

        assert_eq!(
            buffer.push_audio(TrackId::Audio(0), Duration::ZERO, &[0.5; 480]),
            AudioPushOutcome::DiscardedAwaitingKeyframe
        );
        assert_eq!(buffer.held(), None, "nothing is held yet");

        // And once a keyframe has opened one, the same block is taken.
        push(&buffer, &packet(Duration::ZERO, 1_000, true));
        assert_eq!(
            buffer.push_audio(TrackId::Audio(0), Duration::ZERO, &[0.5; 480]),
            AudioPushOutcome::Appended
        );
    }

    #[test]
    fn audio_is_leased_with_the_segment_it_arrived_in() {
        // What makes a clip able to contain it: the lease is the unit a save
        // reads, so audio that did not come back through one could not be
        // written however well it was stored.
        let buffer = buffer(30);
        for tenth in 0..20 {
            let at = Duration::from_millis(tenth * 100);
            push(&buffer, &packet(at, 1_000, tenth % 10 == 0));
            buffer.push_audio(TrackId::Audio(0), at, &[0.25; 480]);
            buffer.push_audio(TrackId::Audio(1), at, &[0.75; 480]);
        }

        let lease = buffer
            .lease_last(Duration::from_secs(1))
            .expect("a keyframe has been buffered");

        let blocks: Vec<_> = lease.audio().collect();
        assert!(
            !blocks.is_empty(),
            "the lease has to carry the audio of the segments it holds"
        );
        assert!(
            blocks
                .iter()
                .any(|block| block.track() == TrackId::Audio(0)),
            "both tracks were pushed and both have to come back"
        );
        assert!(
            blocks
                .iter()
                .any(|block| block.track() == TrackId::Audio(1)),
            "both tracks were pushed and both have to come back"
        );
        assert!(
            blocks.iter().all(|block| block.samples().len() == 480),
            "a block comes back the length it went in"
        );
    }

    #[test]
    fn audio_is_counted_against_the_ceiling_rather_than_held_beside_it() {
        // The failure this prevents is not an overrun but a quietly shorter
        // replay: audio and video share one ceiling, so audio nobody paid for
        // evicts video. `resident_bytes` is what the ceiling is enforced
        // against, so it is what has to grow.
        let buffer = buffer(30);
        push(&buffer, &packet(Duration::ZERO, 1_000, true));
        let before = buffer.stats().bytes_held();

        for block in 0..10 {
            buffer.push_audio(
                TrackId::Audio(0),
                Duration::from_millis(block * 10),
                &[0.5; 480],
            );
        }

        let after = buffer.stats().bytes_held();
        assert!(
            after >= before + 10 * 480 * size_of::<f32>() as u64,
            "the samples have to show up in what the buffer says it occupies:              {before} then {after}"
        );
    }
}
