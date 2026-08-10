//! The recording clock: capture timestamps, the epoch they are measured from,
//! and how far the tracks timed against it have drifted apart.
//!
//! [`CaptureTimestamp`] and [`SourceClock`] are the raw readings a frame source
//! supplies. [`CaptureClock`] turns them into [`MediaTime`] — the one timeline
//! every track in a recording is placed on — and [`DriftEstimator`],
//! [`SyncTolerance`] and [`SyncState`] are how a session finds out that a track
//! is no longer where the reference clock says it should be.
//!
//! The model in prose, including what is *not* solved here, is
//! `docs/av-sync.md`. The short version: the reference clock is the one the
//! video source stamps frames with, every other source is expressed as an
//! offset from it, the conversion happens once per packet at the boundary of
//! this crate, and a source that disagrees with the reference is measured and
//! reported rather than quietly resampled — correcting it is
//! [issue #30](https://github.com/wildware-uk/clipped/issues/30).

use core::fmt;
use core::num::NonZeroU64;
use core::time::Duration;

/// Which clock a [`CaptureTimestamp`] counts on.
///
/// A backend has to name one, and the only names available are monotonic
/// clocks that a *frame source* stamps frames with. There is deliberately no
/// variant for wall-clock time, so a backend that reaches for
/// `SystemTime::now()` has nowhere to declare the result and the mistake stops
/// at the type rather than at a desynchronised recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceClock {
    /// The Windows high-resolution performance counter.
    ///
    /// This is what both Windows capture APIs stamp frames with — Windows
    /// Graphics Capture through `Direct3D11CaptureFrame::SystemRelativeTime`,
    /// Desktop Duplication through `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime`
    /// — and what Windows audio capture reports positions against, so video
    /// and audio timestamps can be compared without a conversion nobody can
    /// check.
    PerformanceCounter,
    /// A POSIX `CLOCK_MONOTONIC`-equivalent counter, for a future non-Windows
    /// backend. Nothing produces this today.
    Monotonic,
}

impl fmt::Display for SourceClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PerformanceCounter => "performance counter",
            Self::Monotonic => "monotonic",
        })
    }
}

/// When a frame was produced, according to whatever produced it.
///
/// # Why there is no `now()`
///
/// The obvious implementation of a capture timestamp — read the clock when the
/// frame arrives — is wrong, and wrong in a way that is invisible until someone
/// watches the recording. The moment a frame reaches this process is the moment
/// a compositor, a driver, a thread scheduler and an encoder queue have all
/// finished with it, and the delay each adds varies frame to frame and grows
/// under load. Timestamps taken at receipt therefore encode this recorder's
/// jitter rather than the game's frame pacing: the video drifts against audio
/// captured with its own device-clock positions, and the drift is worst exactly
/// when the machine is busiest, which is during a game.
///
/// So this type cannot be built from a clock. [`from_source`](Self::from_source)
/// and [`from_performance_counter`](Self::from_performance_counter) are the only
/// constructors, both take a value the frame arrived with, and both make the
/// backend name the [`SourceClock`] it came from. Nothing here can stop a
/// determined backend from calling `QueryPerformanceCounter` itself and passing
/// the result — the type system cannot know where a number came from — but the
/// accidental path is closed, and a review has one line to look at.
///
/// # Comparing
///
/// Two timestamps are comparable only if they name the same [`SourceClock`];
/// [`duration_since`](Self::duration_since) returns [`None`] otherwise rather
/// than subtracting two unrelated counters. Timestamps are stored in
/// nanoseconds so that a comparison never depends on a counter frequency the
/// caller does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptureTimestamp {
    clock: SourceClock,
    nanos: u64,
}

impl CaptureTimestamp {
    /// Takes the timestamp a frame arrived with, in nanoseconds on `clock`.
    ///
    /// `nanos` must be the value the capture API attached to this frame. It is
    /// not "close enough" to read a clock here: see the type documentation.
    #[must_use]
    pub const fn from_source(clock: SourceClock, nanos: u64) -> Self {
        Self { clock, nanos }
    }

    /// Converts a raw performance-counter reading into a timestamp.
    ///
    /// `ticks` is the counter value the frame arrived with and `frequency` is
    /// the counter's ticks per second, from `QueryPerformanceFrequency` — fixed
    /// for the lifetime of the system, so a backend reads it once at
    /// initialisation.
    ///
    /// The multiplication is done in 128-bit arithmetic because
    /// `ticks * 1_000_000_000` overflows a `u64` after about six seconds at a
    /// 10 MHz counter, and a performance counter counts from boot, not from
    /// when the recording started.
    #[must_use]
    pub const fn from_performance_counter(ticks: u64, frequency: NonZeroU64) -> Self {
        let nanos = (ticks as u128 * 1_000_000_000) / frequency.get() as u128;
        Self {
            clock: SourceClock::PerformanceCounter,
            // A u128 nanosecond count only exceeds u64 after ~584 years of
            // uptime, so this cannot truncate in practice; saturating rather
            // than wrapping keeps ordering sane if it somehow did.
            nanos: if nanos > u64::MAX as u128 {
                u64::MAX
            } else {
                nanos as u64
            },
        }
    }

    /// Which clock this counts on.
    #[must_use]
    pub const fn clock(&self) -> SourceClock {
        self.clock
    }

    /// The reading, in nanoseconds on [`clock`](Self::clock).
    ///
    /// The zero point is the clock's own, which for a performance counter is
    /// system boot. Only differences mean anything.
    #[must_use]
    pub const fn as_nanos(&self) -> u64 {
        self.nanos
    }

    /// How much later this frame is than `earlier`.
    ///
    /// [`None`] when the two name different clocks, or when `earlier` is
    /// actually later — a source that reports timestamps going backwards is a
    /// fault to report, not a negative duration to average away.
    #[must_use]
    pub fn duration_since(&self, earlier: Self) -> Option<Duration> {
        if self.clock != earlier.clock {
            return None;
        }
        self.nanos
            .checked_sub(earlier.nanos)
            .map(Duration::from_nanos)
    }
}

impl fmt::Display for CaptureTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ns ({})", self.nanos, self.clock)
    }
}

/// A moment on a recording's own timeline, in nanoseconds from its start.
///
/// A [`CaptureTimestamp`] counts from whatever zero its clock has — system boot,
/// for a performance counter — which is a number no container and no player has
/// any use for. A `MediaTime` is the same moment rebased onto the recording's
/// [epoch](CaptureClock::epoch), so it is directly what
/// `clipped_muxer::PacketTimestamp` wants and what a viewer reads off a seek
/// bar.
///
/// # Why it is signed
///
/// Because a packet older than the epoch is normal, not a fault. The epoch is
/// fixed by whichever source produces the first thing worth timing, and the
/// other sources were already running: an audio packet describing the 5 ms
/// before the first video frame has a negative media time, and it is the
/// muxer's job to decide what to do about that (it clamps, and counts). Making
/// this unsigned would turn a 5 ms lead into 18 billion seconds of lag, silently
/// — which is the failure this type exists to make impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaTime(i64);

impl MediaTime {
    /// The start of the recording.
    pub const ZERO: Self = Self(0);

    /// A moment `nanos` nanoseconds after the recording's epoch.
    #[must_use]
    pub const fn from_nanos(nanos: i64) -> Self {
        Self(nanos)
    }

    /// The moment, in nanoseconds after the recording's epoch.
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }

    /// How far after `earlier` this is, in nanoseconds.
    ///
    /// Negative when `earlier` is in fact later. Saturating rather than
    /// wrapping: the inputs would have to be nearly three hundred years apart
    /// to reach the limit, and a difference pinned at the end of the range is
    /// at least visibly wrong.
    #[must_use]
    pub const fn nanos_since(self, earlier: Self) -> i64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Display for MediaTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ns", self.0)
    }
}

/// Two timestamps that cannot be compared, because they count on different
/// clocks.
///
/// Returned rather than papered over. Subtracting a performance-counter reading
/// from a monotonic-clock reading produces a number, and that number is the
/// difference between two machines' idea of when their epoch was — a recording
/// built on it is wrong by an amount nobody can predict, and it looks fine until
/// somebody watches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClockMismatch {
    /// The clock the recording is timed against.
    pub expected: SourceClock,
    /// The clock the timestamp offered counts on.
    pub found: SourceClock,
}

impl fmt::Display for ClockMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "this recording is timed against the {} clock, so a timestamp on the {} clock \
             cannot be placed on its timeline",
            self.expected, self.found
        )
    }
}

impl core::error::Error for ClockMismatch {}

/// The one clock a recording is timed against, and the moment it started.
///
/// # What it is for
///
/// Every source in a recording produces timestamps on some clock of its own.
/// The rule this type enforces is that exactly one of those clocks is the
/// *reference*, that every other source is expressed as a position on it, and
/// that the conversion happens in one place — here — rather than being spread
/// through the pipeline as subtractions a reviewer has to find and check.
///
/// The reference is chosen by the video source, because the video source is the
/// one that cannot be adjusted: frames exist when the compositor made them, and
/// a recorder that moved them would be inventing frame pacing. Audio is
/// expressed against it. `docs/av-sync.md` argues that at length.
///
/// # The epoch
///
/// [`start_at`](Self::start_at) fixes the epoch, and it should be given the
/// timestamp of the first frame the recording keeps. Everything before it is
/// negative, which is representable and which the muxer handles; nothing here
/// depends on the epoch being the earliest moment in the recording.
///
/// # Threading
///
/// [`Copy`] and stateless once built, so every thread in a recording can hold
/// its own copy rather than sharing one behind a lock. That matters: the
/// conversion happens on the capture and audio threads, which AGENTS.md
/// section 20 forbids from waiting on anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptureClock {
    clock: SourceClock,
    epoch: u64,
}

impl CaptureClock {
    /// Starts a recording's timeline at `epoch`.
    #[must_use]
    pub const fn start_at(epoch: CaptureTimestamp) -> Self {
        Self {
            clock: epoch.clock,
            epoch: epoch.nanos,
        }
    }

    /// Which clock this recording is timed against.
    #[must_use]
    pub const fn clock(&self) -> SourceClock {
        self.clock
    }

    /// The reading the recording's timeline starts at.
    #[must_use]
    pub const fn epoch(&self) -> CaptureTimestamp {
        CaptureTimestamp::from_source(self.clock, self.epoch)
    }

    /// Places a source timestamp on the recording's timeline.
    ///
    /// # Errors
    ///
    /// [`ClockMismatch`] when `timestamp` counts on a different clock from the
    /// epoch. That is a wiring fault — two sources on two clocks in one
    /// recording — and the only honest thing to do with it is refuse.
    pub fn media_time(&self, timestamp: CaptureTimestamp) -> Result<MediaTime, ClockMismatch> {
        if timestamp.clock != self.clock {
            return Err(ClockMismatch {
                expected: self.clock,
                found: timestamp.clock,
            });
        }
        Ok(self.media_time_unchecked(timestamp.nanos))
    }

    /// Places a reading from a source that reports raw nanoseconds on
    /// `clock` — an audio device, whose timestamps are not [`CaptureTimestamp`]s
    /// — on the recording's timeline.
    ///
    /// This exists because `clipped-audio` cannot depend on this crate: both are
    /// layer 1 of the dependency table in README.md, so `AudioTimestamp` is a
    /// separate type counting the same nanoseconds on the same clock. This is
    /// the single, named bridge between them, and the caller has to say which
    /// clock the reading came from — which is exactly the claim a reviewer needs
    /// to check. `docs/av-sync.md` records why the duplication is tolerated
    /// rather than resolved.
    ///
    /// # Errors
    ///
    /// [`ClockMismatch`] when `clock` is not the recording's clock.
    pub fn media_time_on(
        &self,
        clock: SourceClock,
        nanos: u64,
    ) -> Result<MediaTime, ClockMismatch> {
        if clock != self.clock {
            return Err(ClockMismatch {
                expected: self.clock,
                found: clock,
            });
        }
        Ok(self.media_time_unchecked(nanos))
    }

    /// The conversion itself, once the clock has been agreed.
    ///
    /// Both readings are unsigned nanoseconds from the clock's own zero, and the
    /// difference between them is signed, so the subtraction is done in 128 bits
    /// and narrowed. A `u64` subtraction here would turn a packet 5 ms older
    /// than the epoch into 18 billion seconds of lag.
    const fn media_time_unchecked(&self, nanos: u64) -> MediaTime {
        let difference = nanos as i128 - self.epoch as i128;
        MediaTime(if difference > i64::MAX as i128 {
            i64::MAX
        } else if difference < i64::MIN as i128 {
            i64::MIN
        } else {
            difference as i64
        })
    }
}

/// Where a track is allowed to sit relative to the recording's reference clock.
///
/// # The numbers, and where they come from
///
/// Synchronisation error is perceived asymmetrically: sound arriving *after* the
/// picture is what a listener expects from any distance, and sound arriving
/// before it is not. ITU-R BT.1359-1 puts the detectability thresholds at
/// 45 ms of audio lead and 125 ms of audio lag, and EBU R37 asks a production
/// chain to stay inside 40 ms of lead and 60 ms of lag. [`Self::default`] is
/// EBU R37, because a recorder is the start of somebody's production chain and
/// should not spend the viewer's whole budget on its own.
///
/// That is the *reportable* limit, not the design target. `clipped-audio`'s
/// timeline holds its track to within 20 ms of the reference clock by
/// construction (`crates/audio/src/timeline.rs`), so a recording that reaches
/// these limits has something wrong with it rather than being merely
/// unfortunate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyncTolerance {
    ahead: Duration,
    behind: Duration,
}

impl SyncTolerance {
    /// A tolerance allowing a track to run `ahead` of the reference clock and
    /// `behind` it by the given amounts.
    #[must_use]
    pub const fn new(ahead: Duration, behind: Duration) -> Self {
        Self { ahead, behind }
    }

    /// How far a track may run ahead of the reference clock.
    #[must_use]
    pub const fn ahead(&self) -> Duration {
        self.ahead
    }

    /// How far a track may run behind the reference clock.
    #[must_use]
    pub const fn behind(&self) -> Duration {
        self.behind
    }

    /// Classifies an offset, in nanoseconds, of a track against the reference
    /// clock.
    ///
    /// Positive means the track is placed *later* on the recording's timeline
    /// than the moment it describes, which plays back as sound arriving after
    /// the picture.
    #[must_use]
    pub fn classify(&self, offset_nanos: i64) -> SyncState {
        let behind = i64::try_from(self.behind.as_nanos()).unwrap_or(i64::MAX);
        let ahead = i64::try_from(self.ahead.as_nanos()).unwrap_or(i64::MAX);

        if offset_nanos > behind {
            SyncState::Behind
        } else if offset_nanos < -ahead {
            SyncState::Ahead
        } else {
            SyncState::InTolerance
        }
    }
}

impl Default for SyncTolerance {
    fn default() -> Self {
        Self::new(Duration::from_millis(40), Duration::from_millis(60))
    }
}

impl fmt::Display for SyncTolerance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "-{}ms to +{}ms",
            self.ahead.as_millis(),
            self.behind.as_millis()
        )
    }
}

/// Where a track sits relative to the recording's reference clock.
///
/// Three states rather than two, because "out of tolerance" on its own does not
/// say which way, and which way is most of the diagnosis: a track running ahead
/// of the clock has been given too many samples, and one running behind has
/// been given too few.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncState {
    /// Within the tolerance, in both directions.
    InTolerance,
    /// Earlier on the recording's timeline than the moment it describes: it
    /// plays before the picture.
    Ahead,
    /// Later on the recording's timeline than the moment it describes: it plays
    /// after the picture.
    Behind,
}

impl fmt::Display for SyncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InTolerance => "in tolerance",
            Self::Ahead => "ahead of the reference clock",
            Self::Behind => "behind the reference clock",
        })
    }
}

/// How fast a track is sliding against the reference clock.
///
/// Held as a dimensionless ratio — seconds of error per second of recording —
/// and read out in whichever of the two units the reader wants: parts per
/// million, which is how a crystal's accuracy is specified, and milliseconds per
/// minute, which is how the symptom is described.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct DriftRate(f64);

impl DriftRate {
    /// A rate of `ratio` seconds of error per second of recording.
    #[must_use]
    pub const fn from_ratio(ratio: f64) -> Self {
        Self(ratio)
    }

    /// The rate as seconds of error per second of recording.
    #[must_use]
    pub const fn as_ratio(self) -> f64 {
        self.0
    }

    /// The rate in parts per million, which is how a clock's accuracy is
    /// specified: a ±50 ppm crystal, the commodity part, is ±3 ms per minute.
    #[must_use]
    pub fn parts_per_million(self) -> f64 {
        self.0 * 1e6
    }

    /// The rate in milliseconds of error per minute of recording, which is how
    /// the symptom is described.
    #[must_use]
    pub fn millis_per_minute(self) -> f64 {
        self.0 * 60_000.0
    }

    /// How long a recording may run before this rate produces `budget` of
    /// error.
    ///
    /// [`None`] when it never will — a rate of exactly zero — and when the
    /// answer is longer than a [`Duration`] can hold, which a rate small enough
    /// to be indistinguishable from zero produces. Both mean "not in any
    /// recording anybody will make", and a caller reporting this says so.
    #[must_use]
    pub fn time_to(self, budget: Duration) -> Option<Duration> {
        if self.0 == 0.0 || !self.0.is_finite() {
            return None;
        }
        let seconds = budget.as_secs_f64() / self.0.abs();
        Duration::try_from_secs_f64(seconds).ok()
    }
}

impl fmt::Display for DriftRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:+.3} ppm ({:+.3} ms/min)",
            self.parts_per_million(),
            self.millis_per_minute()
        )
    }
}

/// How far apart two consecutive offsets have to be before the difference is a
/// step rather than drift.
///
/// A clock does not drift by a millisecond between two audio packets 10 ms
/// apart — that would be 100,000 ppm — so anything that large is a
/// *discontinuity*: the audio timeline correcting itself in one deadband-sized
/// move, a source clock stepping, or a stream restarting. It has to be excluded
/// from the fit or one 20 ms step in a thirty-minute run reads as 0.7 ms per
/// minute of drift that is not there.
///
/// One millisecond: two orders of magnitude above the packet-to-packet jitter
/// measured on Windows 11 build 26200, and well below `clipped-audio`'s 20 ms
/// deadband, which is the smallest correction that crate can make.
pub const DEFAULT_DISCONTINUITY_STEP: Duration = Duration::from_millis(1);

/// Measures how a track is moving against the recording's reference clock.
///
/// # What it is fed
///
/// Pairs of moments that describe *the same event*: where the reference clock
/// says it happened, and where the track being measured places it. For system
/// audio those are the performance-counter position the endpoint reported for a
/// packet, and the media time the audio timeline assigned to that packet's first
/// frame. Their difference is the A/V offset a viewer would see, in nanoseconds,
/// and its slope is the drift.
///
/// # What it does with a discontinuity
///
/// A jump larger than the step it was built with ends the current fit and starts
/// a new one, and is counted. That is not tidying the data: the underlying rate
/// is a property of a crystal and does not change when a correction is applied,
/// so a fit that spans a correction measures the correction instead of the
/// crystal. The peak and the latest offset still span the whole run — those are
/// what the recording actually contains — and only the *rate* is per-segment.
///
/// # Cost
///
/// Four integer accumulators and no allocation, so a capture thread may call
/// [`observe`](Self::observe) per packet (AGENTS.md sections 18 and 20).
#[derive(Debug, Clone)]
pub struct DriftEstimator {
    step_nanos: i64,
    observations: u64,
    discontinuities: u64,
    first: Option<i64>,
    latest: Option<i64>,
    peak: i64,
    /// The current segment's regression, of offset in nanoseconds against
    /// reference time in milliseconds. Milliseconds, not nanoseconds, so that
    /// the sum of squares of a multi-hour run stays far inside an `i128`.
    segment: Segment,
}

/// One correction-free run of observations, and the sums a least-squares slope
/// needs.
#[derive(Debug, Clone, Default)]
struct Segment {
    count: i128,
    origin: i64,
    latest: i64,
    sum_x: i128,
    sum_y: i128,
    sum_xy: i128,
    sum_xx: i128,
}

impl Segment {
    fn add(&mut self, reference_nanos: i64, offset_nanos: i64) {
        if self.count == 0 {
            self.origin = reference_nanos;
        }
        let x = i128::from((reference_nanos.saturating_sub(self.origin)) / 1_000_000);
        let y = i128::from(offset_nanos);

        self.latest = reference_nanos;
        self.count += 1;
        self.sum_x += x;
        self.sum_y += y;
        self.sum_xy += x * y;
        self.sum_xx += x * x;
    }

    /// The least-squares slope, in nanoseconds of offset per millisecond of
    /// reference time — which is parts per million, exactly.
    fn slope_ppm(&self) -> Option<f64> {
        let numerator = self.count * self.sum_xy - self.sum_x * self.sum_y;
        let denominator = self.count * self.sum_xx - self.sum_x * self.sum_x;
        if denominator == 0 {
            // Fewer than two observations, or every observation at the same
            // millisecond: there is no slope, and inventing one from a single
            // point is how a measurement becomes a guess.
            return None;
        }
        Some(numerator as f64 / denominator as f64)
    }
}

impl DriftEstimator {
    /// An estimator that treats a jump of more than `step` as a discontinuity.
    ///
    /// [`DEFAULT_DISCONTINUITY_STEP`] is the value to pass unless there is a
    /// reason not to.
    #[must_use]
    pub fn new(step: Duration) -> Self {
        Self {
            step_nanos: i64::try_from(step.as_nanos()).unwrap_or(i64::MAX),
            observations: 0,
            discontinuities: 0,
            first: None,
            latest: None,
            peak: 0,
            segment: Segment::default(),
        }
    }

    /// Records that the track placed at `observed` an event the reference clock
    /// puts at `reference`.
    ///
    /// Returns the offset between them in nanoseconds — positive when the track
    /// is behind the clock — so a caller that wants to log or classify one
    /// observation does not have to subtract it again.
    pub fn observe(&mut self, reference: MediaTime, observed: MediaTime) -> i64 {
        let offset = observed.nanos_since(reference);

        if let Some(previous) = self.latest {
            // `unsigned_abs` rather than `abs`, here and below: a saturated
            // difference can be `i64::MIN`, whose absolute value is not an
            // `i64` and whose `abs()` panics in a debug build. The inputs would
            // have to be nonsense to get there, and a measurement panicking on
            // nonsense input is not a measurement.
            if offset.saturating_sub(previous).unsigned_abs() > self.step_nanos.unsigned_abs() {
                self.discontinuities += 1;
                self.segment = Segment::default();
            }
        }

        self.segment.add(reference.as_nanos(), offset);
        self.observations += 1;
        self.first.get_or_insert(offset);
        self.latest = Some(offset);
        if offset.unsigned_abs() > self.peak.unsigned_abs() {
            self.peak = offset;
        }
        offset
    }

    /// How many observations have been made.
    #[must_use]
    pub const fn observations(&self) -> u64 {
        self.observations
    }

    /// How many discontinuities have been seen.
    #[must_use]
    pub const fn discontinuities(&self) -> u64 {
        self.discontinuities
    }

    /// The first offset observed, in nanoseconds.
    ///
    /// Worth reading beside [`latest`](Self::latest): a recording that starts
    /// 30 ms out and stays there has an *alignment* problem, and one that starts
    /// at zero and ends 30 ms out has a *drift* problem. They have different
    /// causes and different fixes.
    #[must_use]
    pub const fn first(&self) -> Option<i64> {
        self.first
    }

    /// The most recent offset, in nanoseconds.
    #[must_use]
    pub const fn latest(&self) -> Option<i64> {
        self.latest
    }

    /// The largest offset seen in either direction, in nanoseconds, keeping its
    /// sign.
    #[must_use]
    pub const fn peak(&self) -> i64 {
        self.peak
    }

    /// The drift rate over the current correction-free segment, or [`None`]
    /// before there is enough of one to fit a line to.
    #[must_use]
    pub fn rate(&self) -> Option<DriftRate> {
        // The slope comes out in nanoseconds per millisecond, which is parts
        // per million; the ratio this type holds is a millionth of that.
        self.segment
            .slope_ppm()
            .map(|ppm| DriftRate::from_ratio(ppm / 1e6))
    }

    /// How long the current correction-free segment has been running, in
    /// nanoseconds of reference time.
    ///
    /// The number that decides whether [`rate`](Self::rate) means anything: a
    /// slope fitted over two seconds of a clock that jitters by microseconds is
    /// noise, and the same fit over half an hour is a measurement.
    #[must_use]
    pub fn segment_span_nanos(&self) -> i64 {
        self.segment.latest.saturating_sub(self.segment.origin)
    }

    /// Classifies the most recent offset against `tolerance`.
    ///
    /// [`None`] before anything has been observed.
    #[must_use]
    pub fn state(&self, tolerance: &SyncTolerance) -> Option<SyncState> {
        self.latest.map(|offset| tolerance.classify(offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The performance-counter frequency Windows reports on every machine
    /// since Windows 10: 10 MHz, so one tick is 100 nanoseconds.
    fn ten_mhz() -> NonZeroU64 {
        NonZeroU64::new(10_000_000).expect("10 MHz is not zero")
    }

    #[test]
    fn performance_counter_ticks_convert_to_nanoseconds() {
        let timestamp = CaptureTimestamp::from_performance_counter(1, ten_mhz());
        assert_eq!(timestamp.as_nanos(), 100);
        assert_eq!(timestamp.clock(), SourceClock::PerformanceCounter);
    }

    #[test]
    fn conversion_does_not_overflow_at_realistic_uptimes() {
        // Ten days of uptime at 10 MHz. `ticks * 1_000_000_000` is about
        // 8.6e21, which does not fit in a u64: computing this in 64 bits
        // silently produces nonsense, and every frame timestamp in a session
        // started on a machine that has been up for a while would be wrong.
        let ticks = 10 * 24 * 60 * 60 * 10_000_000;
        let timestamp = CaptureTimestamp::from_performance_counter(ticks, ten_mhz());
        assert_eq!(timestamp.as_nanos(), 864_000_000_000_000);
    }

    #[test]
    fn frame_intervals_come_out_of_the_difference() {
        // Two frames one 60 Hz interval apart on a 10 MHz counter.
        let first = CaptureTimestamp::from_performance_counter(1_000_000, ten_mhz());
        let second = CaptureTimestamp::from_performance_counter(1_166_666, ten_mhz());
        let interval = second
            .duration_since(first)
            .expect("both readings are on the same clock");
        assert_eq!(interval, Duration::from_nanos(16_666_600));
    }

    #[test]
    fn timestamps_on_different_clocks_are_not_comparable() {
        let counter = CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 2_000);
        let monotonic = CaptureTimestamp::from_source(SourceClock::Monotonic, 1_000);
        assert_eq!(
            counter.duration_since(monotonic),
            None,
            "subtracting readings from two unrelated clocks must not produce a duration"
        );
    }

    #[test]
    fn a_timestamp_going_backwards_is_reported_rather_than_hidden() {
        let earlier = CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 1_000);
        let later = CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 2_000);
        assert_eq!(earlier.duration_since(later), None);
        assert_eq!(
            later.duration_since(earlier),
            Some(Duration::from_micros(1))
        );
    }

    const SECOND: u64 = 1_000_000_000;
    /// A counter reading of the kind a real recording starts at: the machine
    /// has been up for a year, and nothing may assume the epoch is small.
    const BOOTED_LONG_AGO: u64 = 31_107_000 * SECOND;

    fn counter(nanos: u64) -> CaptureTimestamp {
        CaptureTimestamp::from_source(SourceClock::PerformanceCounter, nanos)
    }

    fn clock() -> CaptureClock {
        CaptureClock::start_at(counter(BOOTED_LONG_AGO))
    }

    #[test]
    fn the_epoch_is_where_the_recordings_timeline_starts() {
        let clock = clock();
        assert_eq!(clock.epoch(), counter(BOOTED_LONG_AGO));
        assert_eq!(clock.clock(), SourceClock::PerformanceCounter);
        assert_eq!(
            clock.media_time(counter(BOOTED_LONG_AGO)),
            Ok(MediaTime::ZERO)
        );
        assert_eq!(
            clock.media_time(counter(BOOTED_LONG_AGO + 90 * SECOND)),
            Ok(MediaTime::from_nanos(90_000_000_000))
        );
    }

    #[test]
    fn a_packet_older_than_the_epoch_gets_a_negative_media_time() {
        // The case unsigned arithmetic turns into eighteen billion seconds: the
        // audio device was already running when the first video frame arrived,
        // so its first packet describes a moment 5 ms before the epoch.
        let clock = clock();
        let early = clock
            .media_time(counter(BOOTED_LONG_AGO - 5_000_000))
            .expect("the same clock");
        assert_eq!(early.as_nanos(), -5_000_000);
        assert!(early < MediaTime::ZERO);
    }

    #[test]
    fn rebasing_does_not_lose_precision_at_a_realistic_counter_reading() {
        // A year of uptime is 3.1e16 nanoseconds, comfortably inside a u64 but
        // far outside the range of an f64's exact integers. Converting through
        // a float — the obvious shortcut — would quantise this to 4 ns steps
        // and jitter every timestamp in the recording.
        let clock = clock();
        for offset in [1_i64, 7, 999, 16_666_667] {
            assert_eq!(
                clock
                    .media_time(counter(BOOTED_LONG_AGO + offset as u64))
                    .map(MediaTime::as_nanos),
                Ok(offset)
            );
        }
    }

    #[test]
    fn a_timestamp_from_another_clock_cannot_be_placed_on_the_timeline() {
        let clock = clock();
        let foreign = CaptureTimestamp::from_source(SourceClock::Monotonic, BOOTED_LONG_AGO);
        assert_eq!(
            clock.media_time(foreign),
            Err(ClockMismatch {
                expected: SourceClock::PerformanceCounter,
                found: SourceClock::Monotonic,
            })
        );
        assert_eq!(
            clock.media_time_on(SourceClock::Monotonic, BOOTED_LONG_AGO),
            Err(ClockMismatch {
                expected: SourceClock::PerformanceCounter,
                found: SourceClock::Monotonic,
            })
        );
    }

    #[test]
    fn an_audio_position_crosses_onto_the_timeline_by_naming_its_clock() {
        // The bridge `clipped-audio` uses, because it cannot depend on this
        // crate: the same nanoseconds on the same counter, converted in the one
        // place a reviewer has to check.
        let clock = clock();
        assert_eq!(
            clock.media_time_on(
                SourceClock::PerformanceCounter,
                BOOTED_LONG_AGO + 21_000_000
            ),
            Ok(MediaTime::from_nanos(21_000_000))
        );
    }

    #[test]
    fn a_tolerance_says_which_way_a_track_is_out() {
        // EBU R37: 40 ms of lead, 60 ms of lag. The asymmetry is the point —
        // sound arriving early is noticed at half the error that sound arriving
        // late is.
        let tolerance = SyncTolerance::default();
        assert_eq!(tolerance.ahead(), Duration::from_millis(40));
        assert_eq!(tolerance.behind(), Duration::from_millis(60));

        assert_eq!(tolerance.classify(0), SyncState::InTolerance);
        assert_eq!(tolerance.classify(60_000_000), SyncState::InTolerance);
        assert_eq!(tolerance.classify(60_000_001), SyncState::Behind);
        assert_eq!(tolerance.classify(-40_000_000), SyncState::InTolerance);
        assert_eq!(tolerance.classify(-40_000_001), SyncState::Ahead);
    }

    #[test]
    fn a_drift_rate_reads_out_in_both_units() {
        // 50 ppm is the commodity crystal specification, and the whole reason
        // this is measured: a track 50 ppm fast is 3 ms per minute out, which
        // is 360 ms by the end of a two-hour recording.
        let rate = DriftRate::from_ratio(50e-6);
        assert!((rate.parts_per_million() - 50.0).abs() < 1e-9);
        assert!((rate.millis_per_minute() - 3.0).abs() < 1e-9);

        // And the number a session actually wants: how long until it matters.
        let budget = rate
            .time_to(Duration::from_millis(60))
            .expect("a non-zero rate reaches any budget eventually");
        assert_eq!(budget.as_secs(), 1_200);

        assert_eq!(
            DriftRate::from_ratio(0.0).time_to(Duration::from_secs(1)),
            None
        );
    }

    /// Feeds `packets` observations of a track running `ppm` faster than the
    /// reference clock, one every 10 ms, and returns the estimator.
    fn run_at(ppm: f64, packets: i64) -> DriftEstimator {
        let mut estimator = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
        for packet in 0..packets {
            let reference = packet * 10_000_000;
            #[allow(clippy::cast_possible_truncation)]
            let offset = (reference as f64 * ppm / 1e6) as i64;
            estimator.observe(
                MediaTime::from_nanos(reference),
                MediaTime::from_nanos(reference + offset),
            );
        }
        estimator
    }

    #[test]
    fn a_track_that_keeps_step_with_the_clock_drifts_at_zero() {
        let estimator = run_at(0.0, 6_000);
        assert_eq!(estimator.observations(), 6_000);
        assert_eq!(estimator.discontinuities(), 0);
        assert_eq!(estimator.latest(), Some(0));
        assert_eq!(estimator.peak(), 0);
        let rate = estimator.rate().expect("a minute of observations fits");
        assert!(
            rate.parts_per_million().abs() < 1e-6,
            "a track in step should measure no drift, not {rate}"
        );
    }

    #[test]
    fn the_rate_of_a_slow_track_is_recovered_from_its_offsets() {
        // The measurement this whole module exists to make: ten minutes of a
        // track running 50 ppm behind the reference clock, which is 3 ms per
        // minute and 30 ms by the end.
        let estimator = run_at(50.0, 60_000);
        let rate = estimator.rate().expect("ten minutes of observations fit");

        assert!(
            (rate.parts_per_million() - 50.0).abs() < 0.1,
            "expected about 50 ppm, measured {rate}"
        );
        assert!(
            (rate.millis_per_minute() - 3.0).abs() < 0.01,
            "expected about 3 ms per minute, measured {rate}"
        );
        // Ten minutes less one packet, at 50 ppm: 599.99 s x 50e-6.
        assert_eq!(estimator.latest(), Some(29_999_500));
        assert_eq!(estimator.peak(), 29_999_500);
        assert_eq!(estimator.segment_span_nanos(), 599_990_000_000);
    }

    #[test]
    fn the_sign_of_the_rate_says_which_way_the_track_is_going() {
        let fast = run_at(-25.0, 60_000);
        let rate = fast.rate().expect("ten minutes of observations fit");
        assert!(
            (rate.parts_per_million() + 25.0).abs() < 0.1,
            "expected about -25 ppm, measured {rate}"
        );
        assert!(
            fast.peak() < 0,
            "a track running ahead has negative offsets"
        );
        assert_eq!(
            fast.state(&SyncTolerance::default()),
            Some(SyncState::InTolerance)
        );
    }

    #[test]
    fn a_correction_is_a_discontinuity_and_does_not_become_drift() {
        // What `clipped-audio` does when its 20 ms deadband is crossed: it
        // inserts silence or trims, and the offset steps back in one move.
        // Fitting a line across that step measures the correction rather than
        // the crystal — a single 20 ms step in half an hour reads as 0.7 ms per
        // minute of drift that does not exist.
        let mut estimator = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
        let mut step_applied = 0_i64;

        for packet in 0..180_000_i64 {
            let reference = packet * 10_000_000;
            // 100 ppm of real drift — a poor endpoint clock — corrected by
            // 20 ms whenever it gets there, which at that rate is every 200
            // seconds.
            let raw = reference / 10_000;
            if raw - step_applied >= 20_000_000 {
                step_applied += 20_000_000;
            }
            estimator.observe(
                MediaTime::from_nanos(reference),
                MediaTime::from_nanos(reference + raw - step_applied),
            );
        }

        assert_eq!(
            estimator.discontinuities(),
            8,
            "half an hour at 100 ppm crosses a 20 ms deadband every 200 seconds"
        );
        let rate = estimator.rate().expect("the last segment is long enough");
        assert!(
            (rate.parts_per_million() - 100.0).abs() < 0.5,
            "the crystal is 100 ppm out however often the correction fires, not {rate}"
        );
        // The offset itself never leaves the deadband, which is the property
        // the correction buys.
        assert!(
            estimator.peak().abs() <= 20_000_000,
            "the deadband bounds the offset at 20 ms, but the peak was {}",
            estimator.peak()
        );
        assert_eq!(
            estimator.state(&SyncTolerance::default()),
            Some(SyncState::InTolerance)
        );
    }

    #[test]
    fn a_clock_that_steps_starts_a_new_fit_without_losing_the_peak() {
        // The other discontinuity: a source clock that jumps. The fit restarts
        // — a step is not drift — but the recording still contains the error,
        // so the peak and the latest offset are not reset with it.
        let mut estimator = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
        for packet in 0..1_000_i64 {
            let reference = packet * 10_000_000;
            estimator.observe(
                MediaTime::from_nanos(reference),
                MediaTime::from_nanos(reference),
            );
        }
        assert_eq!(estimator.discontinuities(), 0);

        // A 150 ms step, which no tolerance accepts.
        estimator.observe(
            MediaTime::from_nanos(10_000_000_000),
            MediaTime::from_nanos(10_150_000_000),
        );
        assert_eq!(estimator.discontinuities(), 1);
        assert_eq!(estimator.peak(), 150_000_000);
        assert_eq!(
            estimator.state(&SyncTolerance::default()),
            Some(SyncState::Behind)
        );
        assert_eq!(
            estimator.rate(),
            None,
            "one observation is not a segment to fit a line to"
        );
        assert_eq!(estimator.first(), Some(0));
    }

    #[test]
    fn an_estimator_with_nothing_in_it_reports_nothing_rather_than_zero() {
        let estimator = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
        assert_eq!(estimator.observations(), 0);
        assert_eq!(estimator.latest(), None);
        assert_eq!(estimator.first(), None);
        assert_eq!(estimator.rate(), None);
        assert_eq!(estimator.state(&SyncTolerance::default()), None);
        assert_eq!(estimator.segment_span_nanos(), 0);
    }

    #[test]
    fn jitter_around_a_flat_offset_does_not_look_like_drift() {
        // Real positions wander. A fit over jitter with no trend has to come
        // out at zero, or every recording would report drift it does not have.
        let mut estimator = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
        let jitter = [37_000_i64, -52_000, 11_000, -80_000, 64_000, -9_000];
        for packet in 0..6_000_i64 {
            let reference = packet * 10_000_000;
            let offset = jitter[packet as usize % jitter.len()];
            estimator.observe(
                MediaTime::from_nanos(reference),
                MediaTime::from_nanos(reference + offset),
            );
        }

        assert_eq!(
            estimator.discontinuities(),
            0,
            "80 microseconds is not a step"
        );
        let rate = estimator.rate().expect("a minute of observations fits");
        assert!(
            rate.parts_per_million().abs() < 0.05,
            "jitter with no trend should measure no drift, not {rate}"
        );
    }
}
