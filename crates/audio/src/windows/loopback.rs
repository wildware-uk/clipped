//! System audio capture through WASAPI loopback.
//!
//! Records the endpoint Windows currently plays through — the default render
//! device for the console role — in loopback mode, so that what a recording
//! contains is what the machine was heard to play.
//!
//! Everything about how that is done is in `endpoint_capture.rs`, which is the
//! same engine `microphone.rs` uses: the mix format, the device clock, the
//! silence synthesised for periods the endpoint produced nothing for, and what
//! happens when the device is unplugged mid-recording. This file is the choice
//! of endpoint and the public shape of the capture, and no more.

use core::time::Duration;

use crate::error::{AudioError, Capture};
use crate::format::AudioFormat;
use crate::windows::endpoint::EndpointSource;
use crate::windows::endpoint_capture::{CaptureSource, CaptureStats, EndpointCapture};

/// System audio, captured from the endpoint Windows is playing through.
///
/// See `endpoint_capture.rs` for the threading and ownership rules, and
/// `docs/audio-routing.md` for what happens when the endpoint changes.
#[derive(Debug)]
pub struct SystemAudioCapture {
    endpoint: EndpointCapture,
}

impl SystemAudioCapture {
    /// Opens a capture of the endpoint Windows is currently playing through.
    ///
    /// The endpoint's mix format becomes the format of the whole capture; see
    /// [`format`](Self::format).
    ///
    /// # Errors
    ///
    /// [`AudioError::NoEndpoint`] when the machine has no default output
    /// device, because there is then no system audio to record and no format to
    /// give a track. Once a capture is open the same situation is survivable and
    /// is not an error: see `docs/audio-routing.md`.
    ///
    /// [`AudioError::UnsupportedFormat`] when the endpoint presents samples in
    /// a shape this crate will not convert, and [`AudioError::Platform`] when
    /// Windows refuses something outright.
    pub fn open() -> Result<Self, AudioError> {
        let endpoint =
            EndpointCapture::open(CaptureSource::Endpoint(EndpointSource::system_audio()))?
                .ok_or(AudioError::NoEndpoint)?;
        Ok(Self { endpoint })
    }

    /// The shape of every buffer this capture produces.
    ///
    /// Fixed when the capture is opened, and it stays fixed across an endpoint
    /// change: a capture only follows the default endpoint to a device whose
    /// sample rate and channel count match.
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.endpoint.format()
    }

    /// The name Windows gives the endpoint being captured, if one is open.
    ///
    /// [`None`] while the machine has no usable default output device, during
    /// which the capture is producing silence.
    #[must_use]
    pub fn endpoint_name(&self) -> Option<&str> {
        self.endpoint.device_name()
    }

    /// What this capture has produced so far.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        self.endpoint.stats()
    }

    /// Reads the next block of audio, waiting up to `timeout` for one.
    ///
    /// Consecutive buffers are exactly contiguous: each one's timestamp is the
    /// previous one's plus the previous one's duration, whatever the endpoint
    /// did in between. Periods the endpoint produced nothing for come back as
    /// [`SampleOrigin::SynthesisedSilence`](crate::SampleOrigin::SynthesisedSilence)
    /// of the right length rather than as nothing at all, which is the
    /// difference between an audio track that stays with its video and one that
    /// slides forwards all session.
    ///
    /// # Errors
    ///
    /// [`AudioError::NotOpen`] after [`close`](Self::close). Endpoint failures
    /// are not errors: they are handled, logged, and reported through
    /// [`Capture`].
    pub fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
        self.endpoint.read(timeout)
    }

    /// Ends the capture by handing over what the audio engine still holds.
    ///
    /// The audio engine keeps up to 200 ms of captured audio nobody has asked
    /// for. A capture that is simply closed loses it, which is the last
    /// fraction of a second before the user stopped recording — often the part
    /// they pressed the key for
    /// ([issue #320](https://github.com/wildware-uk/clipped/issues/320)).
    ///
    /// **This does not close anything by itself.** It leaves the capture
    /// readable: [`read`](Self::read) then hands over the packets that were
    /// queued, in order and on the same timeline as everything before them,
    /// and once they run out the capture closes itself and the next read
    /// reports [`AudioError::NotOpen`]. A caller that calls this and then
    /// [`close`](Self::close) without reading in between has thrown the audio
    /// away exactly as before, so the loop that reads to `NotOpen` is the whole
    /// of the fix.
    ///
    /// It never waits for the device. Nothing is reopened during a drain and no
    /// silence is synthesised for time passing, so an endpoint that has been
    /// unplugged ends the drain immediately rather than holding up the end of a
    /// recording.
    ///
    /// Idempotent, and pointless after [`close`](Self::close): a closed capture
    /// has already let go of the endpoint and this cannot get it back.
    pub fn finish(&mut self) {
        self.endpoint.begin_drain();
    }

    /// Stops capturing and releases the endpoint, discarding anything not yet
    /// collected.
    ///
    /// [`finish`](Self::finish) is the ordinary way to end a recording; this is
    /// for a caller that wants the endpoint gone now. Idempotent, and does the
    /// same thing as dropping the capture. A closed capture cannot be reopened;
    /// open a new one.
    pub fn close(&mut self) {
        self.endpoint.close();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use windows::Win32::Media::Audio::AUDCLNT_E_DEVICE_INVALIDATED;

    use super::*;
    use crate::buffer::SampleOrigin;
    use crate::windows::endpoint_capture::testing::{skipped, suppressed, Contiguity};
    use crate::windows::notifications::EndpointChange;

    /// Opens a capture, or reports why this machine cannot.
    fn open() -> Option<SystemAudioCapture> {
        if suppressed() {
            return None;
        }
        match SystemAudioCapture::open() {
            Ok(capture) => Some(capture),
            Err(AudioError::NoEndpoint) => {
                skipped("this machine has no default audio output device");
                None
            }
            Err(error) => {
                skipped(&format!(
                    "system audio capture could not be opened: {error}"
                ));
                None
            }
        }
    }

    #[test]
    fn a_capture_produces_a_contiguous_timeline_for_as_long_as_it_is_read() {
        // The property everything downstream depends on, asserted against the
        // real endpoint rather than against the timeline in isolation: buffers
        // arrive with no gaps and no overlaps, and two seconds of reading
        // produces two seconds of audio whether or not anything is playing.
        let Some(mut capture) = open() else { return };
        let format = capture.format();

        let started = Instant::now();
        let mut timeline = Contiguity::new(format);

        while started.elapsed() < Duration::from_secs(2) {
            match capture
                .read(Duration::from_millis(200))
                .expect("a healthy capture does not fail")
            {
                Capture::Samples(samples) => timeline.accept(&samples),
                Capture::Idle => {}
                Capture::FormatChanged(_) => {
                    skipped("the default output device changed during the test");
                    return;
                }
            }
        }

        let seconds = timeline.seconds();
        assert!(
            (1.8..=2.3).contains(&seconds),
            "two seconds of reading should produce about two seconds of audio, got {seconds:.3}"
        );
    }

    #[test]
    fn a_consumer_that_stalls_does_not_make_this_process_buffer_without_limit() {
        // Issue #19's third acceptance criterion. The consumer disappears for
        // well over the endpoint's buffer duration; when it comes back, the
        // capture must not hand over the whole backlog, must not have grown its
        // own buffers to hold it, and must still produce a timeline that
        // matches real time rather than one that lags further behind with every
        // stall.
        let Some(mut capture) = open() else { return };
        let format = capture.format();

        // Prime the stream so the timeline is anchored on the device.
        let _ = capture.read(Duration::from_millis(200));
        let silence_before = capture.stats().synthesised_silence_frames;

        let stall = Duration::from_millis(600);
        std::thread::sleep(stall);

        let resumed = Instant::now();
        let mut frames_after_stall = 0u64;
        let mut largest_buffer = 0usize;
        let mut silent_samples_were_silent = true;
        while resumed.elapsed() < Duration::from_millis(500) {
            if let Capture::Samples(samples) = capture
                .read(Duration::from_millis(200))
                .expect("a healthy capture does not fail")
            {
                frames_after_stall += samples.frames() as u64;
                largest_buffer = largest_buffer.max(samples.samples().len());
                if samples.origin() == SampleOrigin::SynthesisedSilence {
                    silent_samples_were_silent &=
                        samples.samples().iter().all(|sample| *sample == 0.0);
                }
            }
        }

        // The debt is paid in instalments, so no single buffer is larger than
        // one instalment however long the stall was.
        let instalment_samples =
            format.nanos_to_frames(100_000_000) as usize * usize::from(format.channels().get());
        assert!(
            largest_buffer <= instalment_samples,
            "a buffer of {largest_buffer} samples is more than one instalment \
             ({instalment_samples}); a stalled consumer must not cause an unbounded buffer"
        );

        // The stalled period is accounted for exactly once: about 1.1 seconds
        // of audio for a 600 ms stall plus 500 ms of reading.
        let seconds = frames_after_stall as f64 / f64::from(format.sample_rate().get());
        assert!(
            (0.9..=1.4).contains(&seconds),
            "a 600 ms stall followed by 500 ms of reading should produce about 1.1 seconds \
             of audio, got {seconds:.3}"
        );

        // And it is accounted for the *right way*. The audio engine holds
        // `BUFFER_DURATION` and discards the rest, so about 400 ms of the
        // 600 ms stall is audio nobody will ever see — a period the device
        // produced no samples for, exactly like a silent endpoint, and reached
        // here through the device's own reported positions rather than through
        // a clock this test read. It has to come back as silence of at least
        // that length, and that silence has to be silent.
        //
        // A lower bound rather than a range, because on a machine where nothing
        // at all was playing the whole stall is a period the endpoint said
        // nothing about, not only the part the engine could not hold. Both are
        // correct, and the bound below still fails if nothing is synthesised.
        let synthesised = capture.stats().synthesised_silence_frames - silence_before;
        let synthesised_seconds = synthesised as f64 / f64::from(format.sample_rate().get());
        let unrecoverable = stall.as_secs_f64() - 0.2;
        assert!(
            synthesised_seconds >= unrecoverable - 0.15,
            "at least the part of the stall the audio engine could not hold \
             ({unrecoverable:.2} s) should have been filled with silence, but \
             {synthesised_seconds:.3} s was"
        );
        assert!(
            silent_samples_were_silent,
            "a buffer reported as synthesised silence contained a non-zero sample"
        );
    }

    #[test]
    fn the_endpoint_changing_does_not_end_the_recording() {
        // Issue #19's second acceptance criterion, entered at the point a
        // notification enters it. Everything after that is the real path: the
        // stream is dropped, the default endpoint is looked up again, a new
        // stream is opened on it, and the outage is covered by silence so that
        // the timeline stays contiguous across it.
        let Some(mut capture) = open() else { return };
        let before = capture
            .endpoint_name()
            .expect("a capture opens with an endpoint")
            .to_owned();

        let mut timeline = Contiguity::new(capture.format());
        let mut read_once = |capture: &mut SystemAudioCapture| {
            if let Capture::Samples(samples) = capture
                .read(Duration::from_millis(300))
                .expect("a healthy capture does not fail")
            {
                // The same contiguity the first test asserts, across the point
                // the endpoint moved.
                timeline.accept(&samples);
            }
        };

        read_once(&mut capture);
        capture
            .endpoint
            .simulate_endpoint_change(EndpointChange::CaptureEndpointRemoved);

        for _ in 0..10 {
            read_once(&mut capture);
        }

        assert_eq!(
            capture.stats().endpoint_changes,
            1,
            "the change should have been acted on exactly once"
        );
        assert_eq!(
            capture.endpoint_name(),
            Some(before.as_str()),
            "reopening should land on the default endpoint, which has not actually moved"
        );
        assert!(
            capture.stats().frames > 0,
            "the recording must still be producing audio after the endpoint changed"
        );
    }

    #[test]
    fn an_endpoint_that_fails_as_soon_as_it_opens_does_not_spin() {
        // A device that opens and then fails on the first call — a sound card
        // on its way out, or one being removed exactly as the capture reaches
        // it — must not become an open/fail loop. Reopening it with neither a
        // deadline nor a delay is a `read` that never returns, on a recorder
        // AGENTS.md section 59 expects to run for days: a burnt core, a log
        // that grows without limit, and no audio.
        let Some(mut capture) = open() else { return };
        let format = capture.format();
        // The `HRESULT` a device that has gone returns, put in place of the one
        // `GetNextPacketSize` would have returned, so the classification in
        // `Stream::lost` runs on it exactly as it would on the real thing.
        capture
            .endpoint
            .fail_every_endpoint_call_with(AUDCLNT_E_DEVICE_INVALIDATED);

        // Read on another thread, because the regression this guards against
        // is an infinite loop inside `read`, and a test that hangs reports
        // nothing at all. The channel gives it a bounded time to fail in.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let started = Instant::now();
            let mut longest_read = Duration::ZERO;
            let mut frames = 0u64;
            let mut failure = None;
            while started.elapsed() < Duration::from_millis(1_500) {
                let attempted = Instant::now();
                match capture.read(Duration::from_millis(200)) {
                    Ok(Capture::Samples(samples)) => frames += samples.frames() as u64,
                    Ok(Capture::Idle | Capture::FormatChanged(_)) => {}
                    Err(error) => {
                        failure = Some(error.to_string());
                        break;
                    }
                }
                longest_read = longest_read.max(attempted.elapsed());
            }
            let _ = sender.send((longest_read, frames, capture.stats(), failure));
        });

        let (longest_read, frames, stats, failure) = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("a capture whose endpoint fails immediately must still return from read");

        assert_eq!(
            failure, None,
            "an endpoint failing is handled, not an error"
        );
        assert!(
            longest_read < Duration::from_secs(1),
            "a read with a 200 ms timeout took {longest_read:?}"
        );

        // One teardown, then the endpoint is left alone for `ENDPOINT_RETRY`
        // rather than reopened at once, so a second and a half of failing
        // cannot be hundreds of `Activate`/`Initialize`/`Start` sequences.
        assert!(
            stats.endpoint_changes <= 2,
            "1.5 s of a failing endpoint reopened it {} times; it is meant to back off",
            stats.endpoint_changes
        );

        // And the recording carries on regardless, which is the whole point of
        // surviving the failure: the track is silence, of the right length.
        let seconds = frames as f64 / f64::from(format.sample_rate().get());
        assert!(
            (1.0..=2.0).contains(&seconds),
            "1.5 s of reading a failing endpoint should still produce about 1.5 s of \
             audio, got {seconds:.3}"
        );
        assert!(
            stats.synthesised_silence_frames > 0,
            "the gap a failing endpoint leaves has to be filled with silence"
        );
    }

    #[test]
    fn reading_a_closed_capture_is_reported_rather_than_attempted() {
        let Some(mut capture) = open() else { return };
        capture.close();
        capture.close();

        let error = capture
            .read(Duration::from_millis(1))
            .expect_err("a closed capture has nothing to read");
        assert!(matches!(error, AudioError::NotOpen));
    }
}
