//! Noticing that a capture has gone black, without accusing a dark game of it.
//!
//! A capture that has silently stopped working does not report an error: it
//! keeps handing over frames, and every pixel in them is zero. That is the
//! failure [issue #97](https://github.com/wildware-uk/clipped/issues/97) calls
//! "never silently produce a black recording", and it is the only capture
//! failure that cannot be seen from the API's return values, so it is the only
//! one worth reading pixels for.
//!
//! # The rule, and why it is this one
//!
//! A sampled pixel counts as *lit* when any of its colour channels is non-zero.
//! A sample is black when **none** of the pixels it read is lit — not when they
//! are all below some brightness threshold. That distinction is the whole
//! defence against false positives: a legitimately dark scene is dark, not
//! empty. A night-time game frame, a dim menu or an unlit corridor has
//! dithering, noise and a heads-up display in it, so its pixels are 3, 8 or 20
//! rather than 0, and a threshold of "below 16 is black" would call it a broken
//! capture. Exactly zero, everywhere, is what a capture that has stopped
//! working produces, and it is what nothing else produces.
//!
//! # What is left, and it is not solvable here
//!
//! A source that is *deliberately* black — a loading screen, a fade between
//! cut-scenes, a game paused on a black screen — produces exactly the same
//! pixels as a broken capture, because they are the same pixels. Nothing in any
//! capture API distinguishes them. So [`BlackFrameWatch`] separates them the
//! only way that exists: by how long it lasts. Ten seconds of continuous black
//! is the default, which is far beyond a fade and long enough that a loading
//! screen usually beats it, and the consequence of being wrong is bounded — the
//! recording carries on, one method change is logged, and the frames that were
//! black were being recorded anyway.
//!
//! # Cost
//!
//! Sampling reads pixels back from the GPU, which is the one thing this pipeline
//! otherwise never does (`docs/capture-pipeline.md`). It is therefore rationed
//! rather than done per frame: [`BlackFrameWatch::is_due`] admits one sample
//! every half second, so a 60 fps capture reads back 2 frames in 120 and a
//! watched capture costs two small readbacks a second.

use core::fmt;
use core::time::Duration;

use crate::{CaptureTimestamp, CapturedFrame};

/// What a few pixels of one frame looked like.
///
/// Built one pixel at a time by a [`FrameSampler`], which is what makes the
/// counts impossible to disagree with each other: there is no constructor that
/// takes "twelve of nine pixels were lit".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameSample {
    sampled: u32,
    lit: u32,
    brightest: u8,
}

impl FrameSample {
    /// A sample that has read nothing yet.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            sampled: 0,
            lit: 0,
            brightest: 0,
        }
    }

    /// Adds one pixel, by its three colour channels.
    ///
    /// Alpha is deliberately not a parameter. Both Windows capture APIs hand
    /// over `B8G8R8A8` surfaces whose alpha is whatever the compositor left
    /// there — frequently zero for an opaque window — so a pixel judged on its
    /// alpha would be judged on noise.
    #[must_use]
    pub const fn with_pixel(mut self, red: u8, green: u8, blue: u8) -> Self {
        self.sampled += 1;
        if red > 0 || green > 0 || blue > 0 {
            self.lit += 1;
        }
        let mut brightest = red;
        if green > brightest {
            brightest = green;
        }
        if blue > brightest {
            brightest = blue;
        }
        if brightest > self.brightest {
            self.brightest = brightest;
        }
        self
    }

    /// How many pixels were read.
    #[must_use]
    pub const fn sampled(&self) -> u32 {
        self.sampled
    }

    /// How many of them had any colour in them at all.
    #[must_use]
    pub const fn lit(&self) -> u32 {
        self.lit
    }

    /// The brightest single colour channel seen, for diagnostics.
    ///
    /// This is what tells a reader of a log line whether a frame was *black* or
    /// merely dark: `brightest: 0` and `brightest: 6` are the same to
    /// [`is_black`](Self::is_black) only in the sense that the second is not
    /// black at all.
    #[must_use]
    pub const fn brightest(&self) -> u8 {
        self.brightest
    }

    /// Whether every pixel read was exactly black.
    ///
    /// False for a sample that read nothing: no evidence is not evidence.
    #[must_use]
    pub const fn is_black(&self) -> bool {
        self.sampled > 0 && self.lit == 0
    }
}

impl fmt::Display for FrameSample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} of {} sampled pixels lit, brightest channel {}",
            self.lit, self.sampled, self.brightest
        )
    }
}

/// Reads a few pixels out of a captured frame.
///
/// # Why this is a trait
///
/// Sampling is the one part of black-frame detection that has to touch a
/// graphics API: the pixels are in a GPU texture, and getting at them means
/// Direct3D on Windows and something else anywhere else. The trait keeps that
/// in the platform module where AGENTS.md section 5 wants it, and it keeps the
/// policy in [`BlackFrameWatch`] — which is pure, and is therefore tested
/// exhaustively on a machine with no GPU.
///
/// The implementation a recording uses is
/// [`windows::D3d11FrameSampler`](crate::windows::D3d11FrameSampler).
///
/// # Threading
///
/// `Send`, and used only on the capture thread, like the backend whose frames
/// it reads.
pub trait FrameSampler: fmt::Debug + Send {
    /// Reads pixels from `frame`.
    ///
    /// Returns [`None`] when this frame cannot be sampled — a pixel format the
    /// sampler does not understand, or a graphics API that declined — which a
    /// watch treats as *no evidence* rather than as evidence of darkness. A
    /// capture that can never be sampled is therefore never accused of being
    /// black, which is the right way round: the alternative would end
    /// recordings over a readback failure.
    fn sample(&mut self, frame: &CapturedFrame<'_>) -> Option<FrameSample>;
}

/// An unbroken run of sampled frames with nothing in them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlackRun {
    samples: u32,
    length: Duration,
}

impl BlackRun {
    /// How many consecutive samples were black.
    #[must_use]
    pub const fn samples(&self) -> u32 {
        self.samples
    }

    /// How long the run has lasted, on the source's own clock.
    #[must_use]
    pub const fn length(&self) -> Duration {
        self.length
    }
}

impl fmt::Display for BlackRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "every pixel sampled was black for {:.1} s, across {} sampled frames",
            self.length.as_secs_f64(),
            self.samples
        )
    }
}

/// Watches sampled frames for a capture that has gone black.
///
/// Fed by [`CaptureFallback::inspect`](crate::CaptureFallback::inspect) in a
/// recording, and directly in tests. It reads no clock: every judgement is made
/// against the timestamps the frames arrived with, for the reason
/// [`CaptureTimestamp`] exists at all, and because it makes the policy
/// reproducible in a test that never sleeps.
#[derive(Debug, Clone)]
pub struct BlackFrameWatch {
    sample_interval: Duration,
    tolerated: Duration,
    last_sample: Option<CaptureTimestamp>,
    run_started: Option<CaptureTimestamp>,
    run_samples: u32,
    run_length: Duration,
}

impl BlackFrameWatch {
    /// How often a frame is sampled by default: twice a second.
    ///
    /// Two GPU readbacks a second is a cost nobody can measure against a
    /// recording; two hundred would be. The interval also sets how quickly a
    /// black capture is noticed, since
    /// [`DEFAULT_TOLERATED_BLACKNESS`](Self::DEFAULT_TOLERATED_BLACKNESS) is
    /// counted in samples as well as in seconds.
    pub const DEFAULT_SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

    /// How long a capture may be entirely black before it is called broken.
    ///
    /// Ten seconds. A cross-fade is under a second and a loading screen is
    /// usually under ten; a capture that has stopped working is black until the
    /// recording ends. The module documentation has the reasoning and what it
    /// costs when it is wrong.
    pub const DEFAULT_TOLERATED_BLACKNESS: Duration = Duration::from_secs(10);

    /// A watch that samples every `sample_interval` and reports a capture black
    /// once it has been so for `tolerated`.
    #[must_use]
    pub const fn new(sample_interval: Duration, tolerated: Duration) -> Self {
        Self {
            sample_interval,
            tolerated,
            last_sample: None,
            run_started: None,
            run_samples: 0,
            run_length: Duration::ZERO,
        }
    }

    /// Whether a frame arriving at `at` should be sampled.
    ///
    /// Timestamps that cannot be compared — a different source clock, or a
    /// source that stepped backwards — answer yes and start the accounting
    /// again, because the alternative is a watch that quietly stops sampling
    /// after a backend change.
    #[must_use]
    pub fn is_due(&self, at: CaptureTimestamp) -> bool {
        self.last_sample.is_none_or(|last| {
            at.duration_since(last)
                .is_none_or(|since| since >= self.sample_interval)
        })
    }

    /// Records what one sampled frame looked like.
    ///
    /// Returns the run so far once it has gone past what the watch tolerates,
    /// and on every sample after that until the run is broken by a lit frame or
    /// by [`reset`](Self::reset). Acting on it is the caller's business:
    /// [`CaptureFallback`](crate::CaptureFallback) falls back to another backend
    /// and resets the watch, which is what stops it reporting the same run for
    /// ever.
    pub fn observe(&mut self, sample: FrameSample, at: CaptureTimestamp) -> Option<BlackRun> {
        self.last_sample = Some(at);

        if !sample.is_black() {
            self.reset_run();
            return None;
        }

        match self.run_started {
            // A run whose length cannot be measured — the clock changed under
            // it — is not a run. It starts again here rather than being given
            // the benefit of the doubt in either direction.
            Some(started) => match at.duration_since(started) {
                Some(length) => {
                    self.run_length = length;
                    self.run_samples += 1;
                }
                None => {
                    self.reset_run();
                    self.run_started = Some(at);
                    self.run_samples = 1;
                }
            },
            None => {
                self.run_started = Some(at);
                self.run_samples = 1;
                self.run_length = Duration::ZERO;
            }
        }

        // Two samples minimum, so that a run is a *duration* that was observed
        // rather than one frame either side of a gap the capture spent idle.
        (self.run_length >= self.tolerated && self.run_samples >= 2).then_some(BlackRun {
            samples: self.run_samples,
            length: self.run_length,
        })
    }

    /// How long the current run of black frames has lasted, if there is one.
    ///
    /// For the diagnostics screen: "black for 3.5 s" while a game sits on a
    /// loading screen is a fact worth showing, and it is not a failure.
    #[must_use]
    pub const fn black_for(&self) -> Option<Duration> {
        match self.run_started {
            Some(_) => Some(self.run_length),
            None => None,
        }
    }

    /// Forgets everything observed so far.
    ///
    /// Called when the backend under the watch has been replaced: the new one
    /// has not been black for anything.
    pub fn reset(&mut self) {
        self.reset_run();
        self.last_sample = None;
    }

    fn reset_run(&mut self) {
        self.run_started = None;
        self.run_samples = 0;
        self.run_length = Duration::ZERO;
    }
}

impl Default for BlackFrameWatch {
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_SAMPLE_INTERVAL,
            Self::DEFAULT_TOLERATED_BLACKNESS,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceClock;

    fn at(millis: u64) -> CaptureTimestamp {
        CaptureTimestamp::from_source(SourceClock::PerformanceCounter, millis * 1_000_000)
    }

    /// A sample of `count` pixels, all exactly black.
    fn black(count: u32) -> FrameSample {
        (0..count).fold(FrameSample::empty(), |sample, _| sample.with_pixel(0, 0, 0))
    }

    /// A sample of `count` pixels of a very dark but real colour: the night-time
    /// game frame the watch must not accuse.
    fn nearly_black(count: u32) -> FrameSample {
        (0..count).fold(FrameSample::empty(), |sample, _| sample.with_pixel(0, 0, 3))
    }

    #[test]
    fn a_pixel_with_any_colour_in_it_is_lit() {
        assert!(!FrameSample::empty().with_pixel(0, 0, 1).is_black());
        assert_eq!(FrameSample::empty().with_pixel(1, 0, 0).lit(), 1);
        assert!(black(4).is_black());
        assert_eq!(black(4).sampled(), 4);
        assert_eq!(black(4).brightest(), 0);
        assert_eq!(nearly_black(4).lit(), 4);
        assert_eq!(nearly_black(4).brightest(), 3);
    }

    #[test]
    fn nothing_sampled_is_not_a_black_frame() {
        // The sampler returns an empty sample for a frame it could not read.
        // Treating that as black would end recordings over a readback failure.
        assert!(!FrameSample::empty().is_black());
    }

    #[test]
    fn a_capture_that_is_black_throughout_is_reported_once_it_passes_the_tolerance() {
        let mut watch = BlackFrameWatch::new(Duration::from_millis(500), Duration::from_secs(10));

        let mut reported = None;
        for step in 0..=24 {
            let moment = at(step * 500);
            assert!(
                watch.is_due(moment),
                "a sample every 500 ms is exactly the interval"
            );
            if let Some(run) = watch.observe(black(16), moment) {
                reported.get_or_insert((step, run));
            }
        }

        let (step, run) = reported.expect("ten seconds of black must be reported");
        assert_eq!(step, 20, "10 s at 500 ms a sample is the twentieth sample");
        assert_eq!(run.length(), Duration::from_secs(10));
        assert_eq!(run.samples(), 21);
        assert_eq!(
            run.to_string(),
            "every pixel sampled was black for 10.0 s, across 21 sampled frames"
        );
    }

    #[test]
    fn a_dark_scene_is_never_reported_however_long_it_lasts() {
        // The false positive that matters: ten minutes of a very dark game.
        let mut watch = BlackFrameWatch::default();
        for step in 0..1_200 {
            assert_eq!(
                watch.observe(nearly_black(16), at(step * 500)),
                None,
                "a pixel of 0,0,3 is dark, not absent"
            );
        }
        assert_eq!(watch.black_for(), None);
    }

    #[test]
    fn one_lit_frame_breaks_the_run() {
        // A loading screen that ends before the tolerance does, which is the
        // ordinary case this must not act on.
        let mut watch = BlackFrameWatch::new(Duration::from_millis(500), Duration::from_secs(10));
        for step in 0..18 {
            assert_eq!(watch.observe(black(16), at(step * 500)), None);
        }
        assert_eq!(watch.black_for(), Some(Duration::from_millis(8_500)));

        assert_eq!(watch.observe(nearly_black(16), at(9_000)), None);
        assert_eq!(watch.black_for(), None, "the run is over, not paused");

        // And the count starts again rather than resuming near the threshold.
        for step in 0..20 {
            assert_eq!(
                watch.observe(black(16), at(9_500 + step * 500)),
                None,
                "the second run has not lasted ten seconds yet"
            );
        }
        assert!(watch.observe(black(16), at(19_500)).is_some());
    }

    #[test]
    fn samples_are_rationed_to_the_interval() {
        let mut watch = BlackFrameWatch::new(Duration::from_millis(500), Duration::from_secs(10));
        assert!(watch.is_due(at(0)), "the first frame is always due");
        watch.observe(black(16), at(0));

        assert!(!watch.is_due(at(1)), "a 60 fps capture must not be sampled");
        assert!(!watch.is_due(at(499)));
        assert!(watch.is_due(at(500)));
    }

    #[test]
    fn a_run_measured_across_a_clock_change_is_started_again() {
        // What a backend change looks like to the watch when the replacement
        // stamps its frames from a different source clock: the elapsed time is
        // not a number, so it cannot be counted towards a ten-second run.
        let mut watch =
            BlackFrameWatch::new(Duration::from_millis(500), Duration::from_millis(500));
        assert_eq!(watch.observe(black(16), at(0)), None);
        assert_eq!(
            watch.observe(
                black(16),
                CaptureTimestamp::from_source(SourceClock::Monotonic, 60_000_000_000)
            ),
            None,
            "a run across two clocks is not a measured ten seconds"
        );

        // ... and the new clock's own run counts from there.
        let monotonic = |nanos: u64| {
            CaptureTimestamp::from_source(SourceClock::Monotonic, 60_000_000_000 + nanos)
        };
        assert_eq!(watch.observe(black(16), monotonic(400_000_000)), None);
        assert!(watch.observe(black(16), monotonic(600_000_000)).is_some());
    }

    #[test]
    fn a_reset_watch_has_seen_nothing() {
        let mut watch = BlackFrameWatch::new(Duration::from_millis(500), Duration::from_secs(1));
        for step in 0..4 {
            watch.observe(black(16), at(step * 500));
        }
        assert!(watch.black_for().is_some());

        watch.reset();
        assert_eq!(watch.black_for(), None);
        assert!(watch.is_due(at(1_500)), "the interval starts again too");
        assert_eq!(
            watch.observe(black(16), at(1_500)),
            None,
            "the replacement backend has not been black for a second yet"
        );
    }
}
