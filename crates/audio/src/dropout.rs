//! Counting audio a process-scoped tap loses without saying it lost anything.
//!
//! # The loss this counts
//!
//! When a **process-scoped** loopback tap's set of contributing streams changes
//! — any process starting or stopping playback on that side of the tree — the
//! Windows audio engine rebuilds that tap's mix, and the track loses **1,504
//! frames, 31.33 ms, of exact digital zeros**. The zeros arrive inside ordinary
//! packets whose flags are `0`: no `AUDCLNT_BUFFERFLAGS_SILENT`, no
//! `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`, so
//! [`CaptureStats::discontinuities`](crate::windows::CaptureStats::discontinuities)
//! stays at zero through every one of them and nothing in the recorder notices.
//! [Issue #626](https://github.com/wildware-uk/clipped/issues/626) is the
//! defect, `docs/audio-routing.md` has the measurements, and
//! `test-apps/process-tree-audio/tests/mid_recording_joiner.rs` produces one on
//! demand.
//!
//! Nothing on the client side of WASAPI avoids it: eight initialisation
//! variants were tried and two `IAudioClient`s activated separately against the
//! same tree produce sample-identical tracks with holes at the same sample. The
//! whole-endpoint tap is immune. So this module does not fix the hole. It makes
//! the hole **countable**, which is the difference between a support
//! conversation that can say "31 ms lost, fourteen times" and one that has
//! nothing at all to say.
//!
//! # How a hole is told from silence that is not a fault
//!
//! A tap whose processes are all quiet produces exact zeros legitimately, for
//! as long as they stay quiet, and counting that as loss would make the count
//! worthless — the commonest recording on this machine is one where the game's
//! tap is quiet for minutes at a time. Two properties separate the two, and
//! both are cheap to read off the samples as they go past:
//!
//! 1. **The run has delivered audio in front of it.** The engine's rebuild is a
//!    step straight from signal to zero, in the middle of a tap that is
//!    *audible* — which is exactly the moment a stream-set change lands, because
//!    a tap gains or loses a stream while its other streams carry on playing. A
//!    run with no delivered audio in front of it is a tap that was quiet, not a
//!    tap that lost something. Zeros at the very front of a track are therefore
//!    never counted, even though an activation costs the same 1,504 frames,
//!    because from inside the capture the two are indistinguishable.
//! 2. **The run is the length of a rebuild, not the length of a pause.** 1,504
//!    frames is 31.33 ms and did not vary over more than thirty runs, on both
//!    taps, in debug and release, across every initialisation tried.
//!    [`SHORTEST`] and [`LONGEST`] bracket that generously enough for an
//!    endpoint whose device period is not this machine's, and tightly enough
//!    that a source which genuinely stopped playing for a fifth of a second is
//!    not read as loss.
//!
//! This is a heuristic, and it is stated as one. It can over-count — a game
//! that writes 20 ms of exact zeros between two sound effects, with its stream
//! open throughout, is counted as a hole — and it can under-count, since a run
//! whose length falls outside the window is not counted at all. That is why
//! **both** the occurrences and the frames are reported rather than either
//! alone: fourteen runs totalling 438 ms is a mean of 31.3 ms and is
//! recognisably this defect, where fourteen runs totalling 90 ms is something
//! else and says so. A reader who can see the shape can tell which they have.
//!
//! # What is deliberately not counted
//!
//! **Silence this crate synthesised** because a reader fell behind, or because
//! the endpoint produced nothing for a period. That is zeros too, and it is a
//! different failure with a different cause and its own counter
//! (`CaptureStats::synthesised_silence_frames`); conflating the two would make
//! this number mean "something somewhere was quiet". Only delivered samples ever
//! reach [`DropoutWatch::examine`], so no synthesised frame is ever any part of
//! either figure.
//!
//! What synthesised silence does do is **interrupt**: the delivered samples
//! either side of it are not adjacent in time, so gluing them into one run would
//! report a hole longer than the engine ever made. [`DropoutWatch::interrupt`]
//! judges the run so far and starts a new one, which is exactly what
//! `mid_recording_joiner.rs`'s own `Track::holes` does with a synthesised block,
//! and this crate has to agree with the measurement that characterised the
//! defect. Disarming instead was the first attempt and it missed the hole on
//! five real joins out of five: the engine stalls as it rebuilds the mix, so a
//! stall is not merely *near* the loss — it is part of the same event, and a
//! rule that threw the run away whenever one happened threw away the thing being
//! counted. [`DropoutWatch::restarted`] is the stronger form, for a stream that
//! is genuinely a different stream.
//!
//! # What it costs
//!
//! One `f32` comparison per sample, on the capture thread, with no allocation,
//! no lock and no branch that can block (AGENTS.md sections 17 and 20). The
//! whole of the state is three `usize`-sized fields. `the_cost_of_examining_a_
//! packet` in this module measures it rather than asserting it; `docs/testing.md`
//! has the command and `docs/audio-routing.md` the reading.

use core::num::{NonZeroU16, NonZeroU32};

/// The shortest run of zeros that can be this defect, in microseconds.
///
/// 5 ms. Below this a run of zeros is a waveform crossing zero, a source fading
/// out onto the sample, or a packet boundary — none of which is 31 ms of
/// missing audio, and all of which are common enough that counting them would
/// bury the thing being counted.
const SHORTEST: u64 = 5_000;

/// The longest run of zeros that can be this defect, in microseconds.
///
/// 50 ms, against the 31.33 ms measured. The rebuild does not vary on this
/// machine, so the headroom is for an endpoint whose device period is not this
/// one's rather than for a defect that grew. Anything longer is a source that
/// stopped playing, which is not a loss and must not be counted as one — and a
/// hole that really had grown past 50 ms would stop being counted here and
/// start failing `mid_recording_joiner.rs`, which pins the size directly.
const LONGEST: u64 = 50_000;

/// Watches one capture's delivered samples for the loss issue #626 describes.
///
/// One of these belongs to each capture, on that capture's own thread, and is
/// fed every packet that capture hands to its caller, in the order it hands
/// them over. It holds no history but the run in progress, so nothing it does
/// grows with the length of a recording.
#[derive(Debug)]
pub(crate) struct DropoutWatch {
    /// Samples per frame, so a run is counted in frames rather than samples.
    channels: usize,
    /// The shortest run that counts, in frames.
    shortest: u64,
    /// The longest run that counts, in frames.
    longest: u64,
    /// Frames of exact zeros at the end of everything examined so far, or
    /// [`None`] when no delivered audio has been seen since the last
    /// interruption.
    ///
    /// [`None`] is what makes "bounded by audio on both sides" a property of
    /// three fields rather than of a buffer of history: a run can only be
    /// closed and counted while this is [`Some`], and it only becomes [`Some`]
    /// when a frame that is not silent goes past.
    run: Option<u64>,
}

/// What examining a packet found: runs closed, and the frames they held.
///
/// Both, because either alone answers half the question a support conversation
/// asks. See the module documentation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Dropouts {
    /// How many runs closed.
    pub(crate) count: u64,
    /// How many frames those runs held in total.
    pub(crate) frames: u64,
}

impl DropoutWatch {
    /// Starts a watch for a capture of `rate` and `channels`.
    pub(crate) const fn new(rate: NonZeroU32, channels: NonZeroU16) -> Self {
        let rate = rate.get() as u64;
        Self {
            channels: channels.get() as usize,
            // Truncating, and deliberately so on both bounds: it can only make
            // the window wider by a fraction of a frame at each end, which is
            // nothing beside a run of 1,504.
            shortest: rate * SHORTEST / 1_000_000,
            longest: rate * LONGEST / 1_000_000,
            run: None,
        }
    }

    /// Judges the run in progress and begins a new one.
    ///
    /// Call this wherever the delivered samples that come next are not adjacent
    /// in time to the ones before them: silence this crate synthesised for a
    /// period the engine described nothing of, a packet the timeline trimmed or
    /// discarded, a packet the engine flagged as following lost data. Gluing
    /// two delivered runs across one of those would report a hole longer than
    /// the engine made, and throwing the run away would miss the hole entirely —
    /// the engine stalls while it rebuilds a tap's mix, so the stall and the
    /// loss are one event.
    ///
    /// The watch stays armed, because what it is armed by is the tap having
    /// been audible, and an interruption says nothing about that.
    pub(crate) fn interrupt(&mut self) -> Dropouts {
        let mut found = Dropouts::default();
        if self.run.is_some() {
            self.close(&mut found);
        }
        found
    }

    /// Forgets everything, and waits for delivered audio before looking for
    /// another run.
    ///
    /// For a stream that is a genuinely different stream — an endpoint that was
    /// replaced or reopened. Nothing the last one delivered says anything about
    /// what this one delivers, including whether it is audible at all, so the
    /// zeros at the front of it are treated the way the zeros at the front of a
    /// track are: not counted.
    pub(crate) const fn restarted(&mut self) {
        self.run = None;
    }

    /// Examines one packet of delivered, interleaved samples.
    ///
    /// The samples have to be the ones the caller is given, in the order they
    /// are given them, or a run that spans a packet boundary — which this one
    /// does, every time, since it starts on a boundary and ends inside the next
    /// packet — is counted as two short ones and neither reaches [`SHORTEST`].
    pub(crate) fn examine(&mut self, samples: &[f32]) -> Dropouts {
        let mut found = Dropouts::default();
        for frame in samples.chunks_exact(self.channels) {
            // `-0.0 == 0.0`, so a negative zero counts as silence, which is
            // what it sounds like.
            if frame.iter().all(|sample| *sample == 0.0) {
                if let Some(run) = self.run.as_mut() {
                    *run += 1;
                }
            } else {
                // Audio, so whatever run was in progress has ended and can be
                // judged; and the watch is armed either way.
                self.close(&mut found);
            }
        }
        found
    }

    /// Judges the run in progress, adds it to `found` if it is this defect, and
    /// leaves the watch armed with a fresh run.
    fn close(&mut self, found: &mut Dropouts) {
        if let Some(run) = self.run.replace(0) {
            if run >= self.shortest && run <= self.longest {
                found.count += 1;
                found.frames += run;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 48 kHz stereo, which is what every endpoint measured here reports.
    fn watch() -> DropoutWatch {
        DropoutWatch::new(
            NonZeroU32::new(48_000).expect("48000 is not zero"),
            NonZeroU16::new(2).expect("2 is not zero"),
        )
    }

    /// Interleaved stereo: `frames` frames of `sample` in both channels.
    fn frames(sample: f32, frames: usize) -> Vec<f32> {
        vec![sample; frames * 2]
    }

    /// The hole itself: audio, 1,504 frames of exact zeros, audio.
    fn a_join() -> Vec<f32> {
        let mut samples = frames(0.04, 4_800);
        samples.extend(frames(0.0, 1_504));
        samples.extend(frames(0.04, 4_800));
        samples
    }

    #[test]
    fn the_hole_a_stream_set_change_costs_is_counted_once_with_its_length() {
        let found = watch().examine(&a_join());

        assert_eq!(
            found,
            Dropouts {
                count: 1,
                frames: 1_504
            },
            "the 1,504 frames of exact zeros a tap loses when its stream set changes are the \
             whole of what issue #626 is, and they have to be counted as one occurrence of that \
             length rather than as an amount of quiet"
        );
    }

    #[test]
    fn a_hole_that_spans_packets_is_one_hole_rather_than_none() {
        // How it really arrives: the run begins on a packet boundary and ends
        // inside the packet after it, so a watch that judged each packet by
        // itself would see two runs, neither of them long enough to count.
        let mut watch = watch();
        // 480 + 480 + 480 + 64 is the 1,504, delivered as three whole packets
        // and the front of a fourth.
        let tail = {
            let mut samples = frames(0.0, 64);
            samples.extend(frames(0.04, 416));
            samples
        };
        let mut found = Dropouts::default();
        for packet in [
            frames(0.04, 480),
            frames(0.0, 480),
            frames(0.0, 480),
            frames(0.0, 480),
            tail,
        ] {
            let seen = watch.examine(&packet);
            found.count += seen.count;
            found.frames += seen.frames;
        }

        assert_eq!(
            found,
            Dropouts {
                count: 1,
                frames: 1_504
            },
            "a hole is delivered across four packets and is one hole; counting per packet would \
             find four runs of 480 or fewer frames and report nothing at all"
        );
    }

    #[test]
    fn a_tap_whose_processes_are_all_quiet_loses_nothing() {
        // The commonest recording there is: a game that is not making a sound.
        // Twenty seconds of legitimate digital silence, and not one frame of it
        // is a defect.
        let found = watch().examine(&frames(0.0, 48_000 * 20));

        assert_eq!(
            found,
            Dropouts::default(),
            "a tap whose sources are all silent produces exact zeros legitimately and \
             indefinitely. Counting that as lost audio would report a fault on every quiet \
             passage of every recording and make the number useless"
        );
    }

    #[test]
    fn a_source_that_stops_and_starts_again_is_not_a_hole() {
        let mut samples = frames(0.04, 4_800);
        samples.extend(frames(0.0, 48_000 / 2));
        samples.extend(frames(0.04, 4_800));
        let found = watch().examine(&samples);

        assert_eq!(
            found,
            Dropouts::default(),
            "half a second of quiet between two sounds is a source that stopped playing, which \
             is what a game does constantly. The rebuild this counts is 31.33 ms and does not \
             vary"
        );
    }

    #[test]
    fn zeros_before_any_audio_are_not_counted() {
        // An activation costs the same 1,504 frames at the front of a track —
        // but so does opening a capture on a tap that is simply quiet, and from
        // in here the two look identical. Not counting is the honest answer.
        let mut samples = frames(0.0, 1_504);
        samples.extend(frames(0.04, 4_800));
        let found = watch().examine(&samples);

        assert_eq!(
            found,
            Dropouts::default(),
            "zeros with no delivered audio in front of them are a tap that had not started \
             making a sound yet, and a count that guessed otherwise would report a fault on \
             every recording that began quietly"
        );
    }

    #[test]
    fn a_run_still_open_at_the_end_is_not_counted() {
        let mut samples = frames(0.04, 4_800);
        samples.extend(frames(0.0, 1_504));
        let found = watch().examine(&samples);

        assert_eq!(
            found,
            Dropouts::default(),
            "a run of zeros that has not ended yet is a source that may simply have stopped; it \
             is counted when audio returns and bounds it, and never before"
        );
    }

    #[test]
    fn a_reader_that_fell_behind_between_two_sounds_is_not_a_hole() {
        // A reader falling behind is a different failure with a different cause
        // and its own counter, and none of the silence synthesised to cover it
        // is ever handed to this. All that arrives is the interruption.
        let mut watch = watch();
        let opening = watch.examine(&frames(0.04, 4_800));
        let stall = watch.interrupt();
        let closing = watch.examine(&frames(0.04, 4_800));

        assert_eq!(
            (opening, stall, closing),
            (
                Dropouts::default(),
                Dropouts::default(),
                Dropouts::default()
            ),
            "audio, a stall the capture covered with synthesised silence, and audio again is the \
             recorder failing to keep up. Reporting it as audio Windows lost would put two \
             different faults with two different causes under one number"
        );
    }

    #[test]
    fn a_hole_the_capture_stalled_at_the_end_of_is_still_counted_once() {
        // What really happens on this machine: the engine stalls as it rebuilds
        // the tap's mix, so the delivered zeros are followed by a period it
        // described nothing of at all. Disarming here was the first attempt and
        // it missed the hole on five real joins out of five.
        let mut watch = watch();
        let before = watch.examine(&{
            let mut samples = frames(0.04, 4_800);
            samples.extend(frames(0.0, 1_504));
            samples
        });
        let stall = watch.interrupt();
        let after = watch.examine(&frames(0.04, 4_800));

        assert_eq!(
            (before, stall, after),
            (
                Dropouts::default(),
                Dropouts {
                    count: 1,
                    frames: 1_504
                },
                Dropouts::default()
            ),
            "the stall is part of the same event as the loss, not a reason to forget it — and it \
             is counted once, at the interruption, rather than again when audio returns"
        );
    }

    #[test]
    fn a_hole_that_arrives_after_an_interruption_is_still_counted() {
        // A capture that fell behind once must not stop counting for the rest
        // of the recording.
        let mut watch = watch();
        let armed = watch.examine(&frames(0.04, 480));
        let stall = watch.interrupt();
        let found = watch.examine(&a_join());

        assert_eq!(
            (armed, stall, found),
            (
                Dropouts::default(),
                Dropouts::default(),
                Dropouts {
                    count: 1,
                    frames: 1_504
                }
            ),
            "an interruption judges the run in progress; it does not switch the measurement off \
             for the rest of the recording"
        );
    }

    #[test]
    fn a_reopened_stream_starts_again_with_nothing_behind_it() {
        // A different device, or the same one opened again. Its leading zeros
        // are the zeros at the front of a track, and the audio the last stream
        // delivered says nothing about whether this one is audible at all.
        let mut watch = watch();
        let _ = watch.examine(&frames(0.04, 4_800));
        watch.restarted();
        let found = watch.examine(&{
            let mut samples = frames(0.0, 1_504);
            samples.extend(frames(0.04, 4_800));
            samples
        });

        assert_eq!(
            found,
            Dropouts::default(),
            "a reopened endpoint is a new track as far as this is concerned, and counting its \
             opening zeros would put a fault on every device change"
        );
    }

    #[test]
    fn a_run_shorter_than_the_defect_is_not_counted() {
        let mut samples = frames(0.04, 480);
        samples.extend(frames(0.0, 48));
        samples.extend(frames(0.04, 480));
        let found = watch().examine(&samples);

        assert_eq!(
            found,
            Dropouts::default(),
            "a millisecond of zeros is a waveform crossing zero, not 31 ms of missing audio"
        );
    }

    #[test]
    fn a_frame_is_silent_only_when_every_channel_is() {
        // One channel of a stereo pair going quiet is a mix doing its job, not
        // a hole: the engine's rebuild zeroes the whole frame.
        let mut samples = frames(0.04, 480);
        for _ in 0..1_504 {
            samples.extend([0.0, 0.04]);
        }
        samples.extend(frames(0.04, 480));
        let found = watch().examine(&samples);

        assert_eq!(
            found,
            Dropouts::default(),
            "audio is still being delivered while any channel carries it, and a tap that lost \
             its mix loses every channel of it at once"
        );
    }

    #[test]
    fn every_hole_in_a_run_of_them_is_counted() {
        // What a real recording holds: an application starting, and later one
        // stopping, each costing the other-system-audio track the same 31 ms.
        let mut samples = a_join();
        samples.extend(frames(0.0, 1_504));
        samples.extend(frames(0.04, 4_800));
        let found = watch().examine(&samples);

        assert_eq!(
            found,
            Dropouts {
                count: 2,
                frames: 3_008
            },
            "every application that starts playing costs one of these and every one that stops \
             costs another, so the count has to accumulate rather than latch"
        );
    }

    /// What examining a packet costs, on the thread that must never be delayed.
    ///
    /// A measurement rather than an assertion (AGENTS.md sections 17 and 19),
    /// which is why it prints. The bound it does assert is deliberately far
    /// above the reading so that it fails for a change of algorithm and not for
    /// a busy machine: one device period is 10 ms, so 50 µs of work per packet
    /// would still be half a percent of one core, and the reading is three
    /// orders of magnitude under that.
    ///
    /// ```text
    /// cargo test -p clipped-audio --lib dropout::tests::the_cost_of_examining_a_packet -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a timing measurement, which a loaded machine has no useful answer for"]
    fn the_cost_of_examining_a_packet() {
        use std::io::Write as _;

        // One device period of 48 kHz stereo, which is the packet the capture
        // thread really handles.
        let packet = frames(0.04, 480);
        let rounds = 100_000;

        let mut watch = watch();
        let started = std::time::Instant::now();
        let mut seen = 0u64;
        for _ in 0..rounds {
            seen += watch.examine(&packet).count;
        }
        let elapsed = started.elapsed();

        let per_packet = elapsed.as_secs_f64() / f64::from(rounds) * 1e9;
        let share = per_packet / 10_000_000.0 * 100.0;
        let mut out = std::io::stderr();
        let _ = writeln!(
            out,
            "examining a 480-frame stereo packet cost {per_packet:.0} ns, which is {share:.4}% of \
             the 10 ms of real time that packet represents ({rounds} packets in {:.3} s)",
            elapsed.as_secs_f64(),
        );

        assert_eq!(seen, 0, "a packet of pure tone holds no holes");
        assert!(
            per_packet < 50_000.0,
            "examining one packet cost {per_packet:.0} ns. Anything approaching a device period \
             is diagnostics taking priority over recording, which AGENTS.md section 17 forbids"
        );
    }
}
