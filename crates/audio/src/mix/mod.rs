//! The compatibility mix: the one track a naive player can be trusted with.
//!
//! # Why it exists
//!
//! A Clipped recording is several audio tracks — the game, the rest of the
//! system, the microphone, an application or two — and that is the product
//! (SPEC.md section 11). It is also a shape that some players handle badly:
//! handed a file with four audio tracks, a player that takes one arbitrarily
//! can land on the microphone, and the recording sounds broken to somebody who
//! only double-clicked it. SPEC.md section 13 is the answer — track 1 is a mix
//! of everything, flagged as the default, so casual playback sounds right while
//! an editor still sees every source on its own.
//!
//! `clipped-muxer` already orders the mix first and gives it Matroska's default
//! flag. This module is what fills it.
//!
//! # The rule this module is written under
//!
//! **The mix is the only place sources are deliberately combined**, and it
//! combines *copies*. AGENTS.md section 21 forbids silently merging sources the
//! user expects to stay isolated, so every contribution here is taken as a
//! shared borrow of somebody else's samples and read: there is no path through
//! this module that can alter a source's own track, and no level or limiter
//! applied here is visible anywhere but in the mix. That is not a convention —
//! [`Mixer::contribute`] takes `&[f32]`, and the borrow checker is what enforces
//! it.
//!
//! # The problems it actually has to solve
//!
//! **Sources arrive on different clocks and at different times.** A microphone
//! opened half a second after the game, or a source that produces its first
//! packet late, must land where it happened rather than at the start of the
//! mix. So a contribution is *placed* — its timestamp decides which frames of
//! the mix it is added to — and never appended. Concatenating instead would
//! make the mix a recording of the same session with every source sliding
//! against every other.
//!
//! **Sources arrive at different sample rates.** A 44.1 kHz headset microphone
//! beside a 48 kHz render endpoint is ordinary hardware, and a mix that refused
//! one of them left the microphone out of the one track a player that takes a
//! track arbitrarily is going to take — which is the failure this whole module
//! exists to prevent, arriving by another route. So a source at another rate is
//! converted to the mix's ([`rate`]), on the copy this module holds. What that
//! costs and what it does to the samples is that module; what it does *not*
//! touch is the source's own track, which still gets the capture's own samples
//! at the capture's own rate.
//!
//! **Sources clip when they sum.** Two sources at −6 dBFS are exactly full
//! scale and three are past it. [`limiter`] holds the result under the ceiling
//! by turning the mix down rather than by squaring off its peaks; see that
//! module for why that is the trade.
//!
//! **A source that produces nothing must not silence the rest.** The mix cannot
//! emit a frame until every source has had its chance to contribute to it, and
//! a source that has stopped — a microphone Windows muted, a capture whose
//! thread died — would otherwise hold the mix at that frame for ever. So the
//! mix waits for the slowest source only up to [`MAX_SOURCE_LAG`], and then
//! carries on without it. What is *not* done is dividing by the number of
//! sources: a mix that gets 12 dB quieter because four tracks were declared and
//! three are silent is a recording somebody turns up and then finds is noisy.
//!
//! # Threading and ownership
//!
//! A [`Mixer`] is owned by one thread and holds no lock. It is `Send` and not
//! `Sync`, deliberately: the alternative is a lock every capture thread takes on
//! every packet, which is exactly what AGENTS.md section 20 rules out on a
//! capture path. The intended shape is that capture threads copy their samples
//! into the queue they already write to, and the thread that drains that queue
//! owns the mixer — so mixing costs the capture threads nothing at all.
//!
//! Its memory is bounded and its per-buffer work is a multiply-add per sample
//! with no allocation once the accumulator has reached its steady size
//! (AGENTS.md section 18).
//!
//! # What it does not do
//!
//! It does not remix channel layouts beyond the two cases a recording actually
//! produces — a mono microphone into a stereo mix, and a mix folded to mono. A
//! source this module cannot place is refused when it is added rather than
//! silently left out of the mix later (AGENTS.md section 27).
//!
//! # Example
//!
//! ```
//! use core::num::{NonZeroU16, NonZeroU32};
//!
//! use clipped_audio::{AudioFormat, AudioTimestamp, ChannelMask, Level, Mixer, SampleFormat};
//! use clipped_logging::AudioSource;
//!
//! let format = AudioFormat::new(
//!     NonZeroU32::new(48_000).expect("48 kHz is not zero"),
//!     NonZeroU16::new(2).expect("stereo is not zero channels"),
//!     ChannelMask::from_bits(0x3),
//!     SampleFormat::Float32,
//! );
//!
//! let mut mixer = Mixer::new(format);
//! let game = mixer.add_source(AudioSource::Game, format, Level::UNITY)?;
//!
//! mixer.contribute(game, AudioTimestamp::from_nanos(0), &[0.25; 960])?;
//! while let Some(block) = mixer.drain() {
//!     // block.samples() goes to the compatibility mix track.
//!     assert_eq!(block.frames(), 480);
//! }
//! # Ok::<(), clipped_audio::MixError>(())
//! ```

mod level;
mod limiter;
mod rate;

#[cfg(test)]
mod tests;

use core::fmt;

use clipped_logging::AudioSource;

use crate::format::AudioFormat;
use crate::time::AudioTimestamp;

pub use level::Level;

use limiter::Limiter;
use rate::RateConverter;

/// How long the mix waits for a source that has stopped contributing before it
/// carries on without it.
///
/// The mix cannot emit a frame until every source has had the chance to add to
/// it, so the slowest source sets the latency — and a source that has stopped
/// altogether would set it to infinity. Half a second is two orders of
/// magnitude above the packet-to-packet skew between two WASAPI captures on one
/// machine, and short enough that a stalled source does not stall the whole
/// recording's audio.
///
/// Audio that arrives after the mix has passed it is counted as
/// [`MixReport::late_frames`] and discarded rather than placed somewhere it does
/// not belong. It is still on that source's own track, at full quality, which is
/// the point of having isolated tracks at all.
const MAX_SOURCE_LAG: u64 = 500_000_000;

/// The most the mix hands over in one block.
///
/// The same reasoning as the capture timeline's silence instalments: a consumer
/// that stopped collecting for a minute must not cause a minute of samples to be
/// handed back in one allocation. 100 ms at 48 kHz stereo is 38 KB.
const MAX_BLOCK: u64 = 100_000_000;

/// The furthest ahead of the mix's current position a contribution may be
/// placed.
///
/// This is what bounds the accumulator: two seconds of stereo 48 kHz `f32` is
/// 1.5 MB, and nothing a source does can make it larger. A contribution
/// timestamped beyond it is a source whose clock has jumped rather than a source
/// that is early — no capture in this crate produces packets two seconds ahead of
/// the ones around them — so it is counted as [`MixReport::discarded_frames`] and
/// dropped, and the source's own reading of where it is is left alone so that one
/// bad packet cannot drag the whole mix into the future.
const MAX_PENDING: u64 = 2_000_000_000;

/// Which source of a mix a contribution belongs to.
///
/// Issued by [`Mixer::add_source`] and meaningful only to the mixer that issued
/// it. Opaque so that a caller addresses its samples by the handle it was given
/// rather than by counting the order it registered sources in, which is the
/// arithmetic that puts the microphone's audio where the game's should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MixSourceId(usize);

/// Why a mix would not accept something.
///
/// Every one of these is a caller mistake rather than a condition of the
/// machine, which is why they are separate from
/// [`AudioError`](crate::AudioError): that one is about audio devices, and a
/// device cannot cause any of these.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MixError {
    /// The handle does not belong to this mixer.
    UnknownSource,
    /// The source's channel layout is not one that can be placed into the mix's.
    ///
    /// A mono source goes into any mix, a source with the mix's own channel
    /// count goes in channel for channel, and any source folds into a mono mix.
    /// Anything else — 5.1 into stereo, say — is a downmix with a coefficient
    /// table behind it, which is a decision about what the user hears and not
    /// one this crate takes on its own (AGENTS.md section 21).
    UnmixableLayout {
        /// Channels per frame the source produces.
        source: u16,
        /// Channels per frame the mix is being written with.
        mix: u16,
    },
    /// The samples are not a whole number of frames for the source's channel
    /// count.
    ///
    /// The same refusal `clipped-muxer` makes, for the same reason: mixing them
    /// anyway swaps the channels of every frame after the short one, and nothing
    /// about the result looks wrong.
    PartialFrame {
        /// How many samples were offered.
        samples: usize,
        /// How many the source has per frame.
        channels: u16,
    },
}

impl fmt::Display for MixError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSource => {
                formatter.write_str("that audio source does not belong to this mix")
            }
            Self::UnmixableLayout { source, mix } => write!(
                formatter,
                "a {source}-channel source cannot be placed into a {mix}-channel mix without a \
                 downmix Clipped does not define; the source is still recorded on its own track"
            ),
            Self::PartialFrame { samples, channels } => write!(
                formatter,
                "{samples} samples is not a whole number of {channels}-channel frames"
            ),
        }
    }
}

impl std::error::Error for MixError {}

/// How a source's channels are placed into the mix's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// Channel for channel: the source and the mix have the same layout.
    Direct,
    /// One channel to every channel of the mix: a mono microphone in a stereo
    /// recording, which is the common case.
    ///
    /// The same sample in both channels rather than half of it in each. A
    /// listener hears a centred mono source at the amplitude it was captured at,
    /// which is what a user who set its level expects; the mix's headroom is the
    /// limiter's problem, not the panner's.
    SpreadMono,
    /// Every channel of the source averaged into a mono mix.
    FoldToMono,
}

impl Placement {
    /// How `source` channels go into `mix` channels, if they can.
    fn of(source: u16, mix: u16) -> Result<Self, MixError> {
        match (source, mix) {
            (source_channels, mix_channels) if source_channels == mix_channels => Ok(Self::Direct),
            (1, _) => Ok(Self::SpreadMono),
            (_, 1) => Ok(Self::FoldToMono),
            _ => Err(MixError::UnmixableLayout { source, mix }),
        }
    }
}

/// One source of a mix: what it produces, how loud it is, and how far it has
/// got.
#[derive(Debug)]
struct MixSource {
    /// Which track this source feeds, for the `audio_source` field every
    /// diagnostic in this crate carries.
    ///
    /// `clipped-logging`'s closed enumeration rather than a name from the
    /// caller, for the reason docs/logging.md gives: a free-text label would put
    /// whatever the user called their application into a log file, and an
    /// application name is user content.
    source: AudioSource,
    format: AudioFormat,
    placement: Placement,
    level: Level,
    /// Converts this source's samples to the mix's rate, or [`None`] when it is
    /// already at the mix's rate — which is the ordinary case, and pays nothing.
    converter: Option<RateConverter>,
    /// How far behind its input [`converter`](Self::converter) runs, in
    /// nanoseconds, and zero without one. Subtracted from every contribution's
    /// timestamp so the conversion adds no offset of its own; see
    /// [`RateConverter::delay_frames`].
    conversion_delay: u64,
    /// The position on the shared clock this source has contributed up to, or
    /// [`None`] if it has contributed nothing at all yet.
    frontier: Option<u64>,
    /// Whether this source has already reported arriving late, so that a source
    /// which is late once does not write a log line per packet.
    reported_late: bool,
}

/// What a mix has done so far, in counts.
///
/// Deliberately counts and never levels. A mix contains a microphone, and
/// AGENTS.md section 13 puts a hard floor under what may be derived from
/// microphone samples: how many frames the limiter had to touch says whether the
/// user's levels are too hot without saying anything about what was said.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MixReport {
    /// Frames handed to the caller.
    pub frames: u64,
    /// Frames the limiter turned down, out of [`frames`](Self::frames).
    ///
    /// Zero for a mix with headroom to spare. Most of a recording means the
    /// levels are set high enough that the mix is being held down for most of
    /// its length, which is worth telling somebody.
    pub limited_frames: u64,
    /// Frames of source audio that arrived after the mix had already passed
    /// them, and were therefore not in it.
    ///
    /// Non-zero means a source fell more than half a second behind the others.
    /// The audio is not lost — it is on that source's own track — but the mix
    /// does not have it.
    pub late_frames: u64,
    /// Frames of source audio timestamped so far ahead of the mix that placing
    /// them would have meant buffering without limit.
    ///
    /// A source whose reported positions have jumped. Non-zero here is a fault
    /// to report rather than a level to adjust.
    pub discarded_frames: u64,
}

/// A block of mixed audio, borrowed from the mixer that produced it.
///
/// # Privacy
///
/// [`Debug`] describes the block and never its contents, for the reason
/// [`CapturedAudio`](crate::CapturedAudio)'s does: a mix contains the
/// microphone, so a consumer that writes `tracing::debug!(?block)` must not put
/// somebody's room in a log file (AGENTS.md section 13).
pub struct MixedAudio<'a> {
    samples: &'a [f32],
    format: AudioFormat,
    timestamp: AudioTimestamp,
}

impl fmt::Debug for MixedAudio<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixedAudio")
            .field("frames", &self.frames())
            .field("timestamp", &self.timestamp)
            .field("format", &self.format)
            .finish()
    }
}

impl<'a> MixedAudio<'a> {
    /// The interleaved samples, `channels()` of them per frame, all within
    /// `[-1.0, 1.0]`.
    #[must_use]
    pub const fn samples(&self) -> &'a [f32] {
        self.samples
    }

    /// The shape of the samples.
    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }

    /// When the first frame of this block was heard, on the same clock every
    /// capture stamps its buffers with.
    ///
    /// Consecutive blocks are exactly contiguous, whatever the sources did.
    #[must_use]
    pub const fn timestamp(&self) -> AudioTimestamp {
        self.timestamp
    }

    /// Frames in this block.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.format.channels().get())
    }

    /// How long this block lasts.
    #[must_use]
    pub fn duration(&self) -> core::time::Duration {
        core::time::Duration::from_nanos(self.format.frames_to_nanos(self.frames() as u64))
    }
}

/// Combines several captures into the one track a naive player will take.
///
/// [`docs/audio-routing.md`](../../../docs/audio-routing.md) is the model, and
/// `src/mix/mod.rs` is the reasoning behind it. In short: register every
/// source with [`add_source`](Self::add_source), hand each buffer to
/// [`contribute`](Self::contribute) with the timestamp its capture gave it, and
/// take finished blocks out with [`take`](Self::take) until it returns [`None`].
/// After the last source has stopped, [`drain`](Self::drain) empties what is
/// left.
pub struct Mixer {
    format: AudioFormat,
    sources: Vec<MixSource>,
    /// The position on the shared clock the mix's very first frame sits at, or
    /// [`None`] until something has anchored it.
    anchor: Option<u64>,
    /// Frames already handed to the caller. With [`anchor`](Self::anchor) this
    /// is where the accumulator starts, counted rather than accumulated so that
    /// the rounding of one conversion cannot grow over a session.
    emitted: u64,
    /// The frames being summed, interleaved in the mix's own layout. Its first
    /// frame is the one after everything emitted.
    pending: Vec<f32>,
    /// The block last handed over, reused so that steady-state mixing allocates
    /// nothing.
    block: Vec<f32>,
    /// Where a rate-converted contribution is put before it is placed. Owned by
    /// the mixer rather than by each source so that there is one of them
    /// however many sources need converting, and reused for the same reason
    /// [`block`](Self::block) is.
    converted: Vec<f32>,
    limiter: Limiter,
    report: MixReport,
}

impl fmt::Debug for Mixer {
    /// Describes the mix without reaching into the accumulator, which holds
    /// somebody's microphone.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Mixer")
            .field("format", &self.format)
            .field("sources", &self.sources.len())
            .field("emitted_frames", &self.emitted)
            .field("pending_frames", &(self.pending.len() / self.channels()))
            .field("report", &self.report)
            .finish()
    }
}

impl Mixer {
    /// The highest amplitude any sample this mixer produces can reach.
    ///
    /// A guarantee rather than a target: however many sources are summed and at
    /// whatever levels, no sample leaves here above this. `src/mix/limiter.rs`
    /// is what holds it, and why the answer is not "clip it".
    pub const CEILING: f32 = limiter::CEILING;

    /// Starts a mix in `format`, with no sources and no position yet.
    ///
    /// The format is the caller's decision because it is the container's: a
    /// Matroska track's sampling rate and channel count are written into the
    /// header before a recording starts, so the mix has to know its shape before
    /// it has seen a sample. Taking the primary system source's format is what a
    /// session should do — it is the one every other source has to fit into.
    #[must_use]
    pub fn new(format: AudioFormat) -> Self {
        Self {
            format,
            sources: Vec::new(),
            anchor: None,
            emitted: 0,
            pending: Vec::new(),
            block: Vec::new(),
            converted: Vec::new(),
            limiter: Limiter::new(format.sample_rate()),
            report: MixReport::default(),
        }
    }

    /// Fixes where the mix's first frame sits on the shared clock.
    ///
    /// Worth doing when the caller knows the recording's epoch, because the
    /// alternative — anchoring on whichever source speaks first — means audio
    /// another source captured *before* that moment is late by definition. A
    /// session has the epoch: it is the first video frame the recording keeps
    /// (`docs/av-sync.md`).
    #[must_use]
    pub const fn anchored_at(mut self, start: AudioTimestamp) -> Self {
        self.anchor = Some(start.as_nanos());
        self
    }

    /// Adds a source to the mix, at `level`.
    ///
    /// `source` says which track it is, for the `audio_source` field this
    /// crate's diagnostics carry (docs/logging.md).
    ///
    /// A source at a rate the mix is not being written at is **converted to the
    /// mix's rate** rather than refused. That combination is ordinary hardware
    /// — a 44.1 kHz headset microphone beside a 48 kHz render endpoint — and the
    /// alternative left the microphone out of the one track a player that takes
    /// a track arbitrarily is going to take. What is converted is the copy this
    /// mix holds: `src/mix/rate.rs` says what the conversion costs and what it
    /// does to the samples, and the source's own isolated track is not touched
    /// by any of it.
    ///
    /// # Errors
    ///
    /// [`MixError::UnmixableLayout`] when the source's channel layout cannot be
    /// placed into this mix's. Refused here, before the recording starts,
    /// rather than dropped quietly during it: a caller that is told can record
    /// the source on its own track and say that the mix does not contain it,
    /// which is the honest outcome (AGENTS.md section 27).
    pub fn add_source(
        &mut self,
        source: AudioSource,
        format: AudioFormat,
        level: Level,
    ) -> Result<MixSourceId, MixError> {
        let placement = Placement::of(format.channels().get(), self.format.channels().get())?;
        let converter = (format.sample_rate() != self.format.sample_rate()).then(|| {
            RateConverter::new(
                format.sample_rate().get(),
                self.format.sample_rate().get(),
                format.channels().get(),
            )
        });
        let conversion_delay = converter.as_ref().map_or(0, |converter| {
            format.frames_to_nanos(converter.delay_frames())
        });

        self.sources.push(MixSource {
            source,
            format,
            placement,
            level,
            converter,
            conversion_delay,
            frontier: None,
            reported_late: false,
        });
        Ok(MixSourceId(self.sources.len() - 1))
    }

    /// Changes how loud a source is in the mix, from the next contribution on.
    ///
    /// **Only in the mix.** Nothing about the source's own track changes, now or
    /// retrospectively, which is why a level can be moved during a recording at
    /// all.
    ///
    /// # Errors
    ///
    /// [`MixError::UnknownSource`] for a handle this mixer did not issue.
    pub fn set_level(&mut self, source: MixSourceId, level: Level) -> Result<(), MixError> {
        self.source_mut(source)?.level = level;
        Ok(())
    }

    /// What a source is currently mixed at.
    ///
    /// # Errors
    ///
    /// [`MixError::UnknownSource`] for a handle this mixer did not issue.
    pub fn level(&self, source: MixSourceId) -> Result<Level, MixError> {
        Ok(self.source(source)?.level)
    }

    /// The mix's own format.
    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }

    /// What this mix has done so far.
    #[must_use]
    pub const fn report(&self) -> MixReport {
        self.report
    }

    /// Adds one source's buffer to the mix, at the position `at` puts it.
    ///
    /// `samples` is read and never written: the buffer belongs to the source's
    /// own track and this cannot touch it.
    ///
    /// Placed rather than appended. `at` is the timestamp the capture gave the
    /// buffer, on the same clock every other source's timestamps are on, and it
    /// decides which frames of the mix these samples are added to — so a source
    /// that started late lands late, and two sources that overlap are summed
    /// over the frames they share.
    ///
    /// A buffer of
    /// [`SampleOrigin::SynthesisedSilence`](crate::SampleOrigin::SynthesisedSilence)
    /// should go to [`contribute_silence`](Self::contribute_silence) instead,
    /// which does the same bookkeeping without touching a sample.
    ///
    /// # Errors
    ///
    /// [`MixError::UnknownSource`] for a handle this mixer did not issue,
    /// [`MixError::PartialFrame`] for samples that are not a whole number of
    /// frames.
    pub fn contribute(
        &mut self,
        source: MixSourceId,
        at: AudioTimestamp,
        samples: &[f32],
    ) -> Result<(), MixError> {
        let format = self.source(source)?.format;
        let channels = usize::from(format.channels().get());
        if samples.len() % channels != 0 {
            return Err(MixError::PartialFrame {
                samples: samples.len(),
                channels: channels as u16,
            });
        }
        // The span these samples cover comes from the *source's* format, which
        // is the only thing that knows how long one of its frames lasts. It is
        // what the mix's own frame count is derived from below, so a source at
        // another rate occupies the time it really occupies rather than the
        // time the same number of the mix's frames would.
        let span = format.frames_to_nanos((samples.len() / channels) as u64);

        if self.sources[source.0].converter.is_some() {
            // Taken out and put back so that the conversion can borrow the
            // source and the buffer at once; the buffer's capacity survives the
            // round trip, so steady-state conversion still allocates nothing.
            let mut converted = core::mem::take(&mut self.converted);
            let delay = self.sources[source.0].conversion_delay;
            self.sources[source.0]
                .converter
                .as_mut()
                .expect("the converter was there a line ago")
                .process(samples, &mut converted);
            // Never before the mix's own first frame: at the start of a
            // recording the correction would otherwise put the filter's
            // fade-in ahead of the anchor, where it would be counted as a
            // source arriving late and reported as one. Clamping moves that
            // fade-in a fraction of a millisecond later, once, and leaves every
            // packet after the first exactly where the correction puts it.
            let shifted = at.as_nanos().saturating_sub(delay);
            let shifted = self.anchor.map_or(shifted, |anchor| shifted.max(anchor));
            let result = self.mix_in(
                source,
                AudioTimestamp::from_nanos(shifted),
                &converted,
                span,
            );
            self.converted = converted;
            return result;
        }

        self.mix_in(source, at, samples, span)
    }

    /// Adds one source's samples, already in the mix's sample rate, to the
    /// accumulator.
    ///
    /// `span` is how long the samples this came from really lasted, which is
    /// not the same as how long `samples` lasts once a rate conversion has
    /// changed the frame count by a fraction of a frame either way. The mix
    /// places by `span`, because that is what the source's clock said.
    fn mix_in(
        &mut self,
        source: MixSourceId,
        at: AudioTimestamp,
        samples: &[f32],
        span: u64,
    ) -> Result<(), MixError> {
        let channels = usize::from(self.source(source)?.format.channels().get());
        let frames = (samples.len() / channels) as u64;

        let Some(placed) = self.place(source, at, frames, span)? else {
            return Ok(());
        };
        let level = self.sources[source.0].level.as_linear();
        if level == 0.0 {
            // A muted source still occupies its span of the mix — the frames are
            // there, they are just not louder for it — so there is nothing to
            // add and no reason to walk the buffer.
            return Ok(());
        }

        let placement = self.sources[source.0].placement;
        let mix_channels = self.channels();
        let start = placed.start_frame * mix_channels;
        let end = start + placed.frames * mix_channels;
        let into = &mut self.pending[start..end];
        let from = &samples[placed.skipped_frames * channels..][..placed.frames * channels];

        match placement {
            Placement::Direct => {
                for (mixed, sample) in into.iter_mut().zip(from) {
                    *mixed += level * sample;
                }
            }
            Placement::SpreadMono => {
                for (frame, sample) in into.chunks_exact_mut(mix_channels).zip(from) {
                    let scaled = level * sample;
                    for mixed in frame.iter_mut() {
                        *mixed += scaled;
                    }
                }
            }
            Placement::FoldToMono => {
                let scaled = level / channels as f32;
                for (mixed, frame) in into.iter_mut().zip(from.chunks_exact(channels)) {
                    *mixed += scaled * frame.iter().sum::<f32>();
                }
            }
        }

        Ok(())
    }

    /// Tells the mix that a source covered `frames` frames from `at` and that
    /// all of them were silent.
    ///
    /// This is what a
    /// [`SampleOrigin::SynthesisedSilence`](crate::SampleOrigin::SynthesisedSilence)
    /// buffer is for. The frames still count — the mix cannot move past a period
    /// a source has not covered, and a source that is quiet has covered it — but
    /// adding zeros to an accumulator is work with no audible result, which is
    /// the case [`SampleOrigin`](crate::SampleOrigin) exists to let a mixer
    /// skip.
    ///
    /// # Errors
    ///
    /// [`MixError::UnknownSource`] for a handle this mixer did not issue.
    pub fn contribute_silence(
        &mut self,
        source: MixSourceId,
        at: AudioTimestamp,
        frames: u64,
    ) -> Result<(), MixError> {
        let format = self.source(source)?.format;
        let span = format.frames_to_nanos(frames);
        if let Some(converter) = self.sources[source.0].converter.as_mut() {
            // Silence is a stretch of the source the endpoint never described,
            // so the packet after it is not adjacent in time to the packet
            // before it. Interpolating across that join would blend two sounds
            // that were never next to each other, which is the case
            // `RateConverter::reset` exists for.
            converter.reset();
        }

        // Placed exactly like audio, so a silent stretch advances this source's
        // position in the mix and nothing else. `place` allocates the frames the
        // silence covers only when a later contribution needs them.
        self.advance(source, at, self.format.nanos_to_frames(span), span)?;
        Ok(())
    }

    /// Takes the next block of mixed audio, if every source has had its chance
    /// at it.
    ///
    /// Returns [`None`] when the mix is waiting: either nothing has been
    /// contributed at all, or a source has not yet covered the frames that would
    /// come next and has not been silent for long enough to be carried on
    /// without.
    ///
    /// A block is at most 100 ms long, so a caller that stopped
    /// collecting for a while gets its backlog in instalments rather than in one
    /// allocation. Call it until it returns [`None`].
    pub fn take(&mut self) -> Option<MixedAudio<'_>> {
        let boundary = self.boundary()?;
        let origin = self.origin()?;
        let available = boundary.checked_sub(origin)?;
        self.emit(self.format.nanos_to_frames(available.min(MAX_BLOCK)))
    }

    /// Takes everything the mix is holding, whether or not every source has
    /// covered it.
    ///
    /// For the end of a recording, when there is nothing more to wait for. Call
    /// it until it returns [`None`].
    pub fn drain(&mut self) -> Option<MixedAudio<'_>> {
        let held = (self.pending.len() / self.channels()) as u64;
        self.emit(held.min(self.format.nanos_to_frames(MAX_BLOCK)))
    }

    /// How far the mix may safely be emitted to, on the shared clock.
    ///
    /// The earliest position any source has reached — a frame nobody has been
    /// past yet may still gain audio — except that a source which has fallen
    /// more than [`MAX_SOURCE_LAG`] behind the furthest-on source stops holding
    /// the mix up. That exception is the whole answer to "a source that produces
    /// nothing must not silence the rest".
    fn boundary(&self) -> Option<u64> {
        let furthest = self
            .sources
            .iter()
            .filter_map(|source| source.frontier)
            .max()?;
        let agreed = self
            .sources
            .iter()
            .map(|source| source.frontier.unwrap_or(0))
            .min()
            .unwrap_or(furthest);
        Some(agreed.max(furthest.saturating_sub(MAX_SOURCE_LAG)))
    }

    /// Where the first frame of the accumulator sits on the shared clock.
    fn origin(&self) -> Option<u64> {
        self.anchor
            .map(|anchor| anchor + self.format.frames_to_nanos(self.emitted))
    }

    /// Hands over the first `frames` frames of the accumulator, limited.
    fn emit(&mut self, frames: u64) -> Option<MixedAudio<'_>> {
        let origin = self.origin()?;
        let frames = usize::try_from(frames).ok()?;
        if frames == 0 {
            return None;
        }
        let channels = self.channels();
        let samples = frames * channels;

        // A source's silence is a period nobody added to, so the accumulator can
        // be shorter than the mix has reached. Those frames are silence in the
        // mix too, and the mix has to be the length of the recording, so they are
        // filled rather than skipped — the same rule the capture timeline is
        // built on.
        if self.pending.len() < samples {
            self.pending.resize(samples, 0.0);
        }

        self.block.clear();
        self.block.extend_from_slice(&self.pending[..samples]);
        self.report.limited_frames += self.limiter.apply(&mut self.block, channels);

        self.pending.copy_within(samples.., 0);
        self.pending.truncate(self.pending.len() - samples);
        self.emitted += frames as u64;
        self.report.frames += frames as u64;

        Some(MixedAudio {
            samples: &self.block,
            format: self.format,
            timestamp: AudioTimestamp::from_nanos(origin),
        })
    }

    /// Where in the accumulator a contribution goes, growing it if it has to.
    ///
    /// [`None`] when none of it can be placed: it is entirely behind the mix, or
    /// entirely too far ahead of it. Both are counted in the report.
    fn place(
        &mut self,
        source: MixSourceId,
        at: AudioTimestamp,
        frames: u64,
        span: u64,
    ) -> Result<Option<Placed>, MixError> {
        let Some(placed) = self.advance(source, at, frames, span)? else {
            return Ok(None);
        };

        let channels = self.channels();
        let needed = (placed.start_frame + placed.frames) * channels;
        if self.pending.len() < needed {
            self.pending.resize(needed, 0.0);
        }
        Ok(Some(placed))
    }

    /// Works out where a contribution belongs and moves the source's position
    /// on, without touching the accumulator.
    ///
    /// `frames` is counted in the *mix's* frames, because that is what the
    /// accumulator is indexed by. `span` is how long the source said its own
    /// samples lasted, which is what its position is moved on by: the two are
    /// the same number of nanoseconds for a source at the mix's rate, and
    /// deliberately independent for one that is not.
    fn advance(
        &mut self,
        source: MixSourceId,
        at: AudioTimestamp,
        frames: u64,
        span: u64,
    ) -> Result<Option<Placed>, MixError> {
        self.source(source)?;
        if frames == 0 {
            return Ok(None);
        }
        let at = at.as_nanos();
        let anchor = *self.anchor.get_or_insert(at);
        let format = self.format;

        // Where this buffer starts, in frames from the mix's first frame. The
        // comparison is against the anchor rather than against the last buffer,
        // so a rounding error is never carried forward.
        let from_anchor = if at >= anchor {
            i128::from(format.nanos_to_frames(at - anchor))
        } else {
            -i128::from(format.nanos_to_frames(anchor - at))
        };
        let offset = from_anchor - i128::from(self.emitted);

        let (skipped, start) = if offset < 0 {
            let behind = u64::try_from(-offset).unwrap_or(u64::MAX);
            if behind >= frames {
                self.report.late_frames += frames;
                self.report_lateness(source, frames);
                return Ok(None);
            }
            self.report.late_frames += behind;
            self.report_lateness(source, behind);
            (behind, 0u64)
        } else {
            (0, u64::try_from(offset).unwrap_or(u64::MAX))
        };

        let capacity = format.nanos_to_frames(MAX_PENDING);
        if start >= capacity {
            // A source whose reported positions have jumped. Its own position is
            // deliberately left where it was: believing this one would drag the
            // mix's boundary into the future and throw away every other source's
            // audio between here and there.
            self.report.discarded_frames += frames;
            tracing::warn!(
                audio_source = %self.sources[source.0].source,
                frames,
                "an audio source reported a position far ahead of the rest of the recording, so \
                 those samples are not in the compatibility mix; the source's own track is \
                 unaffected. Please report this"
            );
            return Ok(None);
        }

        let placeable = (frames - skipped).min(capacity - start);
        if placeable < frames - skipped {
            self.report.discarded_frames += frames - skipped - placeable;
        }

        let ends_at = at + span;
        let frontier = &mut self.sources[source.0].frontier;
        *frontier = Some(frontier.map_or(ends_at, |reached| reached.max(ends_at)));

        Ok(Some(Placed {
            start_frame: usize::try_from(start).unwrap_or(usize::MAX),
            skipped_frames: usize::try_from(skipped).unwrap_or(usize::MAX),
            frames: usize::try_from(placeable).unwrap_or(usize::MAX),
        }))
    }

    /// Says, once per source, that its audio is arriving too late to be mixed.
    fn report_lateness(&mut self, source: MixSourceId, frames: u64) {
        let source = &mut self.sources[source.0];
        if source.reported_late {
            return;
        }
        source.reported_late = true;
        tracing::warn!(
            audio_source = %source.source,
            frames,
            "this audio source fell far enough behind the others that the compatibility mix had \
             already passed the moment its samples belong to, so they are not in the mix. The \
             source's own track still has them in full"
        );
    }

    /// Channels per frame of the mix.
    fn channels(&self) -> usize {
        usize::from(self.format.channels().get())
    }

    fn source(&self, source: MixSourceId) -> Result<&MixSource, MixError> {
        self.sources.get(source.0).ok_or(MixError::UnknownSource)
    }

    fn source_mut(&mut self, source: MixSourceId) -> Result<&mut MixSource, MixError> {
        self.sources
            .get_mut(source.0)
            .ok_or(MixError::UnknownSource)
    }
}

/// Where a contribution ended up.
#[derive(Debug, Clone, Copy)]
struct Placed {
    /// The frame of the accumulator its first placed sample goes to.
    start_frame: usize,
    /// How many frames were dropped off its front for being behind the mix.
    skipped_frames: usize,
    /// How many of its frames are being placed.
    frames: usize,
}
