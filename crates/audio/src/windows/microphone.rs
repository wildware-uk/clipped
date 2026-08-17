//! Microphone capture: one input device, recorded as its own stream.
//!
//! # What it is
//!
//! The same capture as system audio, pointed at the other side of the audio
//! stack. A microphone is a WASAPI capture endpoint rather than a render
//! endpoint recorded in loopback mode, and that one difference is the whole
//! difference: the mix format, the device clock, the silence synthesised for
//! periods the device produced nothing for, the endpoint being unplugged
//! mid-recording and the endpoint that fails the instant it is opened are all
//! the engine in `endpoint_capture.rs`, which this file selects a device for
//! (AGENTS.md section 55).
//!
//! ```no_run
//! use core::time::Duration;
//!
//! use clipped_audio::windows::{MicrophoneCapture, MicrophoneSelection};
//! use clipped_audio::Capture;
//!
//! let mut microphone = MicrophoneCapture::open(&MicrophoneSelection::SystemDefault)?;
//! if microphone.is_muted() == Some(true) {
//!     println!("the microphone is muted in Windows");
//! }
//! match microphone.read(Duration::from_millis(100))? {
//!     Capture::Samples(audio) => println!("{} frames", audio.frames()),
//!     Capture::Idle => {}
//!     Capture::FormatChanged(format) => println!("the microphone is now {format}"),
//! }
//! # Ok::<(), clipped_audio::AudioError>(())
//! ```
//!
//! # Where a microphone genuinely differs
//!
//! **It may not exist.** Every machine that plays sound has a render endpoint;
//! plenty have no microphone at all. Opening therefore fails with
//! [`AudioError::NoMicrophone`], or with
//! [`AudioError::MicrophoneUnavailable`] naming the device the user chose, and
//! both messages say what to do about it (AGENTS.md section 45).
//!
//! **It goes away far more often.** A headset is unplugged when its owner
//! stands up. That must not end a recording (AGENTS.md sections 16 and 17), so
//! it does not: the track becomes silence of exactly the right length and the
//! capture keeps looking for the device until it comes back.
//!
//! **Windows can mute it.** A muted microphone still delivers packets — of
//! silence, flagged as such — so a recording of one looks perfectly healthy and
//! contains nothing. That is the commonest reason a microphone track is silent,
//! and [`MicrophoneCapture::is_muted`] is how the recorder can say so rather
//! than leave the user guessing.
//!
//! **A chosen device is not replaced by another one.** When the user picks a
//! microphone, this capture waits for that microphone. Unplugging a headset
//! makes Windows promote whatever is left — often a webcam on the other side of
//! the room — and a track that silently became that would be worse than a
//! silent one. Only [`MicrophoneSelection::SystemDefault`] follows the default.
//!
//! # Privacy
//!
//! A microphone hears the room, so its samples are the most private thing this
//! program handles. Nothing in this crate writes them anywhere or derives a log
//! line from their values: the diagnostics here count frames, name devices and
//! measure durations (AGENTS.md section 13). The tests in this file assert on
//! frame counts and timestamps for the same reason.
//!
//! # What is not here
//!
//! Keeping the chosen microphone across restarts is the configuration API,
//! [issue #108](https://github.com/wildware-uk/clipped/issues/108): what this
//! file offers it is [`Microphone::id`], which Windows keeps stable across
//! reboots and is therefore the value worth storing. Processing — gain, noise
//! suppression, a gate — is
//! [issue #31](https://github.com/wildware-uk/clipped/issues/31), the optional
//! raw pre-processing track is
//! [issue #32](https://github.com/wildware-uk/clipped/issues/32), and writing
//! several audio tracks into one file is
//! [issue #28](https://github.com/wildware-uk/clipped/issues/28).

use core::time::Duration;

use crate::error::{AudioError, Capture};
use crate::format::AudioFormat;
use crate::windows::apartment::ensure_multi_threaded_apartment;
use crate::windows::endpoint::{
    active_endpoints, create_enumerator, default_endpoint, platform_error, DeviceSelection,
    EndpointIdentity, EndpointSource, SourceKind,
};
use crate::windows::endpoint_capture::{CaptureSource, CaptureStats, EndpointCapture};

/// One microphone Windows currently has.
///
/// Produced by [`microphones`], and the thing a user is choosing between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Microphone {
    id: String,
    name: String,
    is_default: bool,
}

impl Microphone {
    /// The identifier Windows gives this device.
    ///
    /// Stable across reboots and across the device being unplugged and plugged
    /// in again, which is what makes it the value to store when the user
    /// chooses a microphone (issue #108).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name Windows shows for this device, such as
    /// `Microphone (Yeti Stereo Microphone)`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this is the microphone Windows records from by default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.is_default
    }

    /// The selection that records this device, and only this device.
    #[must_use]
    pub fn select(&self) -> MicrophoneSelection {
        MicrophoneSelection::device(self.id.clone(), self.name.clone())
    }
}

/// Which microphone a capture records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MicrophoneSelection {
    /// Whatever Windows makes the default input device, and follow it when it
    /// moves. This is what a user who has never opened the settings expects: a
    /// new headset is plugged in, and it is the microphone.
    #[default]
    SystemDefault,
    /// One particular microphone, whatever Windows thinks the default is.
    Device {
        /// [`Microphone::id`].
        id: String,
        /// [`Microphone::name`], remembered so that a message about the device
        /// being unplugged can name it even though it is not there to be asked
        /// (AGENTS.md section 45).
        name: String,
    },
}

impl MicrophoneSelection {
    /// Records one particular microphone.
    #[must_use]
    pub fn device(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::Device {
            id: id.into(),
            name: name.into(),
        }
    }

    /// The stored identifier of the chosen device, or [`None`] when the
    /// selection follows whatever Windows makes default.
    #[must_use]
    pub fn device_id(&self) -> Option<&str> {
        match self {
            Self::SystemDefault => None,
            Self::Device { id, .. } => Some(id),
        }
    }

    /// The name of the chosen device, or [`None`] when the selection follows
    /// whatever Windows makes default.
    #[must_use]
    pub fn device_name(&self) -> Option<&str> {
        match self {
            Self::SystemDefault => None,
            Self::Device { name, .. } => Some(name),
        }
    }

    /// How the capture engine describes this choice.
    fn as_device_selection(&self) -> DeviceSelection {
        match self {
            Self::SystemDefault => DeviceSelection::Default,
            Self::Device { id, name } => DeviceSelection::Device {
                id: id.clone(),
                name: name.clone(),
            },
        }
    }

    /// What to report when the device this selection names is not there.
    fn unavailable(&self) -> AudioError {
        match self {
            Self::SystemDefault => AudioError::NoMicrophone,
            Self::Device { name, .. } => AudioError::MicrophoneUnavailable { name: name.clone() },
        }
    }
}

/// Every microphone Windows currently has, in the order it lists them.
///
/// Only devices that are present and enabled: offering a microphone that is
/// unplugged would be offering a choice that fails.
///
/// # Errors
///
/// [`AudioError::Platform`] when Windows will not enumerate its devices, which
/// means the audio service is not answering and no capture is possible either.
pub fn microphones() -> Result<Vec<Microphone>, AudioError> {
    ensure_multi_threaded_apartment()
        .map_err(|error| platform_error("preparing the COM apartment", error))?;
    let enumerator = create_enumerator()?;

    let flow = SourceKind::Microphone.flow();
    let default = default_endpoint(&enumerator, flow)?
        .map(|device| EndpointIdentity::of(&device))
        .transpose()?;

    Ok(active_endpoints(&enumerator, flow)?
        .into_iter()
        .map(|endpoint| Microphone {
            is_default: default
                .as_ref()
                .is_some_and(|default| default.id == endpoint.id),
            id: endpoint.id,
            name: endpoint.name,
        })
        .collect())
}

/// A microphone, captured as its own independent stream.
///
/// See `endpoint_capture.rs` for the threading and ownership rules, and
/// `docs/audio-routing.md` for what happens when the device changes.
#[derive(Debug)]
pub struct MicrophoneCapture {
    endpoint: EndpointCapture,
}

impl MicrophoneCapture {
    /// Opens a capture of the microphone `selection` names.
    ///
    /// The device's mix format becomes the format of the whole capture; see
    /// [`format`](Self::format).
    ///
    /// # Errors
    ///
    /// [`AudioError::NoMicrophone`] when the machine has no input device at
    /// all, and [`AudioError::MicrophoneUnavailable`] when the chosen one is
    /// not plugged in — there is then no format to give a track and no
    /// recording in progress to protect. Once a capture is open the same
    /// situation is survivable and is not an error: the track becomes silence
    /// and the capture waits for the device.
    ///
    /// [`AudioError::UnsupportedFormat`] when the device presents samples in a
    /// shape this crate will not convert, and [`AudioError::Platform`] when
    /// Windows refuses something outright.
    pub fn open(selection: &MicrophoneSelection) -> Result<Self, AudioError> {
        let source = EndpointSource::microphone(selection.as_device_selection());
        let endpoint = EndpointCapture::open(CaptureSource::Endpoint(source))?
            .ok_or_else(|| selection.unavailable())?;

        // Said once, at `warn`, because it is the answer to "why is my
        // microphone track silent" and the user can act on it. It is not
        // rechecked on every read: Windows tells nobody when the switch moves,
        // and a recorder polling a COM call in the capture loop to find out
        // would be worse than a caller asking `is_muted` when it wants to show
        // the state.
        if endpoint.is_muted() == Some(true) {
            tracing::warn!(
                device = endpoint.device_name().unwrap_or("<none>"),
                "the microphone is muted in Windows, so the microphone track will be silent \
                 until it is unmuted"
            );
        }

        Ok(Self { endpoint })
    }

    /// The shape of every buffer this capture produces.
    ///
    /// Fixed when the capture is opened, and it stays fixed across a device
    /// change: a capture only moves to a device whose sample rate and channel
    /// count match.
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.endpoint.format()
    }

    /// The name Windows gives the microphone being recorded, if one is open.
    ///
    /// [`None`] while the device is unplugged or disabled, during which the
    /// capture is producing silence rather than failing.
    #[must_use]
    pub fn device_name(&self) -> Option<&str> {
        self.endpoint.device_name()
    }

    /// Whether the microphone is muted in Windows.
    ///
    /// [`None`] when no device is open, or when Windows will not report the
    /// switch for it — some virtual devices do not have one. A muted microphone
    /// records as silence, so this is the difference between a recorder that
    /// can explain a silent track and one that cannot.
    #[must_use]
    pub fn is_muted(&self) -> Option<bool> {
        self.endpoint.is_muted()
    }

    /// What this capture has produced so far.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        self.endpoint.stats()
    }

    /// Reads the next block of audio, waiting up to `timeout` for one.
    ///
    /// Consecutive buffers are exactly contiguous, whatever the device did in
    /// between — including being unplugged. Periods it produced nothing for
    /// come back as
    /// [`SampleOrigin::SynthesisedSilence`](crate::SampleOrigin::SynthesisedSilence)
    /// of the right length, so the microphone track stays the same length as
    /// the recording it belongs to.
    ///
    /// # Errors
    ///
    /// [`AudioError::NotOpen`] after [`close`](Self::close). Device failures are
    /// not errors: they are handled, logged, and reported through [`Capture`].
    pub fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
        self.endpoint.read(timeout)
    }

    /// Ends the capture by handing over what the audio engine still holds.
    ///
    /// The audio engine keeps up to 200 ms of captured audio nobody has asked
    /// for. A capture that is simply closed loses it, which is the last
    /// fraction of a second of somebody speaking before they stopped recording
    /// ([issue #320](https://github.com/wildware-uk/clipped/issues/320)).
    ///
    /// **This does not close anything by itself.** It leaves the capture
    /// readable: [`read`](Self::read) then hands over the packets that were
    /// queued, in order and on the same timeline as everything before them, and
    /// once they run out the capture closes itself and the next read reports
    /// [`AudioError::NotOpen`]. A caller that calls this and then
    /// [`close`](Self::close) without reading in between has thrown the audio
    /// away exactly as before.
    ///
    /// **It never waits for the device**, which matters more here than
    /// anywhere: nothing is reopened during a drain and no silence is
    /// synthesised for time passing, so a microphone that has been unplugged
    /// ends the drain on the first look rather than holding the device — and
    /// the indicator Windows shows beside it — open while a recording tries to
    /// finish.
    ///
    /// Idempotent, and pointless after [`close`](Self::close): a closed capture
    /// has already let go of the device and this cannot get it back.
    pub fn finish(&mut self) {
        self.endpoint.begin_drain();
    }

    /// Stops capturing and releases the device, discarding anything not yet
    /// collected.
    ///
    /// [`finish`](Self::finish) is the ordinary way to end a recording; this is
    /// for a caller that wants the device gone now. Idempotent, and does the
    /// same thing as dropping the capture. Releasing the microphone matters
    /// more than releasing a speaker: Windows shows a microphone-in-use
    /// indicator for as long as any application holds one.
    pub fn close(&mut self) {
        self.endpoint.close();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::time::Instant;

    use windows::Win32::Foundation::E_FAIL;
    use windows::Win32::Media::Audio::AUDCLNT_E_DEVICE_INVALIDATED;

    use super::*;
    use crate::buffer::SampleOrigin;
    use crate::windows::endpoint_capture::testing::{logged, skipped, suppressed, Contiguity};
    use crate::windows::endpoint_capture::BUFFER_DURATION;
    use crate::windows::notifications::EndpointChange;
    use crate::windows::SystemAudioCapture;

    /// The message `Stream::lost` writes when a failed WASAPI call is *not* one
    /// it recognises.
    ///
    /// Matched as a substring of what the crate logged, so that the assertions
    /// below are about the line a person reading a user's log would see.
    const UNEXPLAINED_FAULT: &str = "the capture stream failed";

    /// Opens the default microphone, or reports why this machine cannot.
    ///
    /// Every test that uses this records the room the machine is in for a
    /// second or so. None of them keeps a sample: they count frames, compare
    /// timestamps, and check that buffers reported as silence are zero.
    fn open() -> Option<MicrophoneCapture> {
        if suppressed() {
            return None;
        }
        match MicrophoneCapture::open(&MicrophoneSelection::SystemDefault) {
            Ok(capture) => Some(capture),
            Err(AudioError::NoMicrophone) => {
                skipped("this machine has no microphone");
                None
            }
            Err(error) => {
                skipped(&format!("the microphone could not be opened: {error}"));
                None
            }
        }
    }

    #[test]
    fn a_microphone_that_is_not_connected_is_reported_by_name_with_something_to_do() {
        // The device identifier is not a real one, so this runs anywhere,
        // including on a machine with no audio hardware at all. What it asserts
        // is the difference AGENTS.md section 45 draws: a user who is told
        // "Shure MV7 is not connected" can act, and one who is told 0x88890004
        // cannot.
        let selection = MicrophoneSelection::device(
            "{0.0.1.00000000}.{00000000-0000-0000-0000-000000000000}",
            "Shure MV7",
        );
        let error = MicrophoneCapture::open(&selection)
            .expect_err("a microphone that is not there cannot be opened");

        assert!(
            matches!(error, AudioError::MicrophoneUnavailable { .. }),
            "expected the chosen microphone to be reported as unavailable, got {error:?}"
        );
        let message = error.to_string();
        assert!(
            message.contains("Shure MV7"),
            "the message must name the device the user chose: {message}"
        );
        assert!(
            message.contains("choose"),
            "the message must offer something to do about it: {message}"
        );
    }

    #[test]
    fn every_microphone_is_listed_once_and_at_most_one_is_the_default() {
        if suppressed() {
            return;
        }

        let microphones = microphones().expect("Windows can list its input devices");
        if microphones.is_empty() {
            skipped("this machine has no microphone");
            return;
        }

        let mut identifiers: Vec<&str> = microphones.iter().map(Microphone::id).collect();
        identifiers.sort_unstable();
        let listed = identifiers.len();
        identifiers.dedup();
        assert_eq!(
            identifiers.len(),
            listed,
            "a device listed twice would appear twice in the settings screen"
        );

        let defaults = microphones
            .iter()
            .filter(|entry| entry.is_default())
            .count();
        // At most one, which is what this test is named for. Two would be this
        // crate misreading the enumeration and would put a settings screen in a
        // state a user cannot resolve. None is unusual but legitimate — a
        // machine whose only input devices are disabled for the console role
        // has microphones and no default — and is a reason to say so rather
        // than to fail a build.
        assert!(
            defaults <= 1,
            "{defaults} of the listed microphones claim to be the one Windows records from"
        );
        if defaults == 0 {
            skipped("this machine has microphones but no default input device");
        }

        // The selection a chosen device produces has to be the device: this is
        // what a settings screen stores and hands back later.
        let first = &microphones[0];
        assert_eq!(first.select().device_id(), Some(first.id()));
        assert_eq!(first.select().device_name(), Some(first.name()));
        assert_eq!(MicrophoneSelection::SystemDefault.device_id(), None);
    }

    #[test]
    fn a_microphone_capture_produces_a_contiguous_timeline_for_as_long_as_it_is_read() {
        // The property the whole crate exists for, asserted on the input side:
        // a second and a half of reading produces a second and a half of audio,
        // with no gaps and no overlaps, whether anybody is talking or not.
        //
        // Nothing here looks at what was said. The counts and the timestamps
        // are the assertion.
        let Some(mut capture) = open() else { return };
        let mut timeline = Contiguity::new(capture.format());

        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(1_500) {
            match capture
                .read(Duration::from_millis(200))
                .expect("a healthy capture does not fail")
            {
                Capture::Samples(samples) => {
                    if samples.origin() == SampleOrigin::SynthesisedSilence {
                        assert!(
                            samples.samples().iter().all(|sample| *sample == 0.0),
                            "a buffer reported as synthesised silence contained a non-zero \
                             sample"
                        );
                    }
                    timeline.accept(&samples);
                }
                Capture::Idle => {}
                Capture::FormatChanged(_) => {
                    skipped("the microphone changed during the test");
                    return;
                }
            }
        }

        let seconds = timeline.seconds();
        assert!(
            (1.3..=1.8).contains(&seconds),
            "1.5 s of reading a microphone should produce about 1.5 s of audio, got \
             {seconds:.3}"
        );
    }

    #[test]
    fn a_microphone_that_stops_answering_leaves_a_silent_track_of_the_right_length() {
        // The other half of issue #20's second acceptance criterion, and the
        // closer of the two to what unplugging a USB microphone actually does:
        // the client is invalidated and every call on it fails from then on.
        //
        // `AUDCLNT_E_DEVICE_INVALIDATED` is returned in place of the `HRESULT`
        // `GetNextPacketSize` would have returned, so what runs on it is the
        // real path from the failed call onwards: `Stream::lost` deciding what
        // the code means, the stream torn down, the device tried again, the
        // backing off when it fails at once, the gap becoming silence. What is
        // *not* covered is Windows returning that code in the first place,
        // which needs a hand on a cable (issue #141).
        //
        // The recording must carry on: a microphone track of the right length,
        // made of silence, rather than a read that fails or a capture that
        // stops (AGENTS.md sections 16 and 17).
        let Some(mut capture) = open() else { return };
        let format = capture.format();
        capture
            .endpoint
            .fail_every_endpoint_call_with(AUDCLNT_E_DEVICE_INVALIDATED);

        let started = Instant::now();
        let mut frames = 0u64;
        while started.elapsed() < Duration::from_millis(1_200) {
            match capture
                .read(Duration::from_millis(200))
                .expect("a microphone going away is handled, not an error")
            {
                Capture::Samples(samples) => frames += samples.frames() as u64,
                Capture::Idle | Capture::FormatChanged(_) => {}
            }
        }

        let seconds = frames as f64 / f64::from(format.sample_rate().get());
        assert!(
            (0.8..=1.7).contains(&seconds),
            "1.2 s of reading a microphone that has gone should still produce about 1.2 s of \
             audio, got {seconds:.3}"
        );
        assert!(
            capture.stats().synthesised_silence_frames > 0,
            "the gap a microphone that has gone leaves has to be filled with silence"
        );
    }

    #[test]
    fn an_unplugged_microphone_is_recognised_rather_than_logged_as_an_unexplained_fault() {
        // `Stream::lost` is what a physically unplugged microphone goes
        // through: every failed WASAPI call ends the stream, and the only thing
        // it decides is whether the failure is explained.
        // `AUDCLNT_E_DEVICE_INVALIDATED` is the ordinary case — it is what
        // unplugging the device produces — and must not appear in the log as a
        // fault, because a `warn` for the commonest thing that happens to a
        // headset is a `warn` nobody reads. Anything else must appear, because
        // nothing else accounts for it.
        //
        // Asserted on what the crate actually logged, since that is the whole
        // of the difference the classification makes. Deleting the
        // `AUDCLNT_E_DEVICE_INVALIDATED` arm, or comparing against the wrong
        // code, fails the first half; treating every failure as an expected
        // unplug fails the second.
        let Some(mut expected) = open() else { return };
        let unplugged = logged(|| {
            expected
                .endpoint
                .fail_every_endpoint_call_with(AUDCLNT_E_DEVICE_INVALIDATED);
            read_for(&mut expected, Duration::from_millis(400));
        });
        assert!(
            !unplugged.contains(UNEXPLAINED_FAULT),
            "an unplugged microphone is expected, not a fault, but the log says: {unplugged}"
        );

        let Some(mut unexpected) = open() else { return };
        let broken = logged(|| {
            unexpected.endpoint.fail_every_endpoint_call_with(E_FAIL);
            read_for(&mut unexpected, Duration::from_millis(400));
        });
        assert!(
            broken.contains(UNEXPLAINED_FAULT),
            "a failure this crate cannot explain has to be logged, but the log says: {broken}"
        );
        assert!(
            broken.contains("asking for the next packet size"),
            "the log line has to name the call that failed: {broken}"
        );

        // And whichever it was, the recording carried on: both captures are
        // still producing a track rather than having failed a read.
        assert!(expected.stats().frames > 0 && unexpected.stats().frames > 0);
    }

    /// Reads `capture` for `duration`, discarding everything it produces.
    ///
    /// The samples are the room; what these tests are about is what the capture
    /// did, which is in its statistics and in what it logged.
    fn read_for(capture: &mut MicrophoneCapture, duration: Duration) {
        let started = Instant::now();
        while started.elapsed() < duration {
            capture
                .read(Duration::from_millis(100))
                .expect("a microphone going away is handled, not an error");
        }
    }

    #[test]
    fn the_microphone_being_unplugged_does_not_end_the_recording() {
        // Issue #20's second acceptance criterion, entered where a real
        // notification enters it. Everything after that is the path a headset
        // being unplugged takes: the stream is dropped, the device is looked up
        // again, a new stream is opened on it, and the outage is covered by
        // silence so that the track stays the same length as the recording.
        let Some(mut capture) = open() else { return };
        let before = capture
            .device_name()
            .expect("a capture opens with a device")
            .to_owned();

        let mut timeline = Contiguity::new(capture.format());
        let mut read_once = |capture: &mut MicrophoneCapture| {
            if let Capture::Samples(samples) = capture
                .read(Duration::from_millis(300))
                .expect("a healthy capture does not fail")
            {
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
            capture.device_name(),
            Some(before.as_str()),
            "the capture should have come back on the microphone it was recording, which has \
             not actually been unplugged"
        );
        assert!(
            capture.stats().frames > 0,
            "the recording must still be producing audio after the microphone changed"
        );
        assert!(
            timeline.frames > 0,
            "the timeline must stay contiguous across the device change"
        );
    }

    #[test]
    fn a_chosen_microphone_is_found_again_by_its_identifier() {
        // The other half of a device coming back: a capture on a *chosen*
        // device has to open that device and reopen that device, rather than
        // whatever Windows makes default. The device is chosen by identifier,
        // torn down, and has to be the one that comes back.
        //
        // A microphone that is *not* the default is preferred, because a
        // capture that ignored the identifier and fell back to the default
        // would pass this test on a machine with one microphone and lose
        // somebody's chosen device on a machine with several. The default is
        // the fallback, since a machine may genuinely have only one.
        if suppressed() {
            return;
        }

        let devices = microphones().expect("Windows can list its input devices");
        let Some((chosen, mut capture)) = devices
            .iter()
            .filter(|microphone| !microphone.is_default())
            .chain(devices.iter().filter(|microphone| microphone.is_default()))
            .find_map(|microphone| {
                MicrophoneCapture::open(&microphone.select())
                    .ok()
                    .map(|capture| (microphone, capture))
            })
        else {
            skipped("no microphone on this machine could be opened");
            return;
        };
        assert_eq!(
            capture.device_name(),
            Some(chosen.name()),
            "a capture must open the microphone whose identifier it was given"
        );

        let _ = capture.read(Duration::from_millis(200));
        capture
            .endpoint
            .simulate_endpoint_change(EndpointChange::CaptureEndpointRemoved);
        for _ in 0..10 {
            let _ = capture.read(Duration::from_millis(200));
        }

        assert_eq!(
            capture.device_name(),
            Some(chosen.name()),
            "a capture on a chosen microphone must reopen that microphone"
        );
        assert!(capture.stats().frames > 0);
    }

    #[test]
    fn a_microphone_and_system_audio_are_two_independent_streams() {
        // SPEC.md section 11 and AGENTS.md section 21: sources the user expects
        // to stay separate are never combined. The two captures are opened at
        // once, as a recording would open them, and each has to produce its own
        // audio from its own device with its own format — which is also what
        // proves neither reopens because the other's device changed.
        let Some(mut microphone) = open() else { return };
        let Ok(mut system) = SystemAudioCapture::open() else {
            skipped("this machine has no default audio output device");
            return;
        };

        let mut from_microphone = Contiguity::new(microphone.format());
        let mut from_system = Contiguity::new(system.format());

        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(1_000) {
            if let Capture::Samples(samples) = microphone
                .read(Duration::from_millis(100))
                .expect("a healthy capture does not fail")
            {
                from_microphone.accept(&samples);
            }
            if let Capture::Samples(samples) = system
                .read(Duration::from_millis(100))
                .expect("a healthy capture does not fail")
            {
                from_system.accept(&samples);
            }
        }

        assert_ne!(
            microphone.device_name(),
            system.endpoint_name(),
            "the two captures must be on different devices"
        );
        assert!(
            from_microphone.frames > 0 && from_system.frames > 0,
            "both captures must produce audio while the other is running: microphone {} \
             frames, system audio {} frames",
            from_microphone.frames,
            from_system.frames
        );
        assert_eq!(
            microphone.stats().endpoint_changes,
            0,
            "the microphone must not reopen because system audio is being captured"
        );
        assert_eq!(
            system.stats().endpoint_changes,
            0,
            "system audio must not reopen because a microphone is being captured"
        );
    }

    /// How long the consumer stops reading for before it finishes the capture.
    ///
    /// Longer than the [`BUFFER_DURATION`] the engine holds, so that what the
    /// drain has to produce is bounded from below by something the engine
    /// cannot have discarded.
    const STALL: Duration = Duration::from_millis(500);

    /// The most a drain can recover: what the audio engine holds.
    ///
    /// [`BUFFER_DURATION`] is asked for in hundreds of nanoseconds, which is the
    /// unit `IAudioClient::Initialize` takes. Everything a reader was away for
    /// beyond this the engine has already discarded, and no drain can get it
    /// back — the track covers that period as synthesised silence instead, which
    /// is what `a_consumer_that_stalls_does_not_make_this_process_buffer_without_limit`
    /// is about.
    const ENGINE_BACKLOG: Duration = Duration::from_nanos(BUFFER_DURATION as u64 * 100);

    /// How long a drain may take in wall-clock time.
    ///
    /// A drain reads what the engine has queued and stops; it waits for nothing.
    /// This is the assertion behind issue #320's third acceptance criterion —
    /// that a recording releases the microphone as promptly as it does today,
    /// because Windows shows an in-use indicator for as long as anything holds
    /// one. Generous by two orders of magnitude against the ~10 ms this takes,
    /// so what it fails on is a drain that has started *waiting* for something
    /// rather than one that had a busy afternoon.
    const PROMPT: Duration = Duration::from_millis(200);

    #[test]
    fn stopping_a_microphone_capture_hands_over_the_audio_the_engine_was_still_holding() {
        // [Issue #320](https://github.com/wildware-uk/clipped/issues/320). The
        // audio engine holds captured audio nobody has collected; a capture that
        // is simply closed throws it away, which is the last fraction of a
        // second of somebody speaking before they stopped recording.
        //
        // The consumer stops reading for long enough to leave a real backlog,
        // and the drain then has to produce it. Nothing here looks at what was
        // said: the counts, the timestamps and the clock are the assertion.
        let Some(mut capture) = open() else { return };
        let format = capture.format();

        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(300) {
            let _ = capture.read(Duration::from_millis(100));
        }

        // Measured, not assumed: `sleep` guarantees only that it does not return
        // early, and the engine goes on capturing for however long this thread
        // is really away.
        let stall_began = Instant::now();
        std::thread::sleep(STALL);
        let stalled = stall_began.elapsed();

        let before = capture.stats().frames;
        capture.finish();

        let drain_began = Instant::now();
        let mut timeline = Contiguity::new(format);
        let mut from_the_device = 0u64;
        // Bounded, so that a `finish` which did nothing at all fails the
        // assertions below rather than hanging the suite: a capture that is not
        // draining goes on reading its device for ever.
        let give_up_at = Instant::now() + Duration::from_secs(2);
        let mut ended_itself = false;
        while Instant::now() < give_up_at {
            match capture.read(Duration::from_millis(100)) {
                Ok(Capture::Samples(samples)) => {
                    if samples.origin() == SampleOrigin::Endpoint {
                        from_the_device += samples.frames() as u64;
                    }
                    timeline.accept(&samples);
                }
                // The drain has handed over everything and closed itself.
                Ok(Capture::Idle | Capture::FormatChanged(_)) | Err(AudioError::NotOpen) => {
                    ended_itself = true;
                    break;
                }
                Err(error) => panic!("a drain does not fail: {error}"),
            }
        }
        let drain_took = drain_began.elapsed();

        let seconds = timeline.seconds();
        let recovered = from_the_device as f64 / f64::from(format.sample_rate().get());
        // What the engine could still have been holding: the reader was away
        // for `stalled`, and the engine keeps at most `ENGINE_BACKLOG` of what
        // happened while it was. A drain that hands over less than this has lost
        // audio the engine still had.
        //
        // One-sided, and against the *smaller* of the two, because both ends are
        // properties of the machine rather than of this crate: an engine granted
        // a larger buffer than `BUFFER_DURATION` asked for hands over more, and
        // a stall shorter than the buffer leaves less than a buffer to hand
        // over. Measured on Windows 11 build 26200, five runs of this test each
        // recovered exactly 9,600 frames — 0.2000 s, the whole of
        // `BUFFER_DURATION` — from a 0.500 s stall, with no synthesised silence
        // at all.
        let recoverable = stalled.as_secs_f64().min(ENGINE_BACKLOG.as_secs_f64());
        // Two device periods. The engine hands over whole packets, so a drain
        // can end a packet either side of the figure above; it is not room for a
        // drain that lost a tenth of a second.
        let slack = 0.02;

        // First, because it is the difference between a drain and an ordinary
        // read, and every measurement below is meaningless without it: a drain
        // ends by itself, at the last sample that exists. A `finish` that did
        // nothing leaves a capture that goes on reading the live device, and
        // gets here having read two seconds of it.
        assert!(
            ended_itself,
            "a drain ends by handing over what it has and closing the capture; this one was \
             still reading the device {drain_took:.3?} later, so finishing it began no drain"
        );
        assert!(
            timeline.frames > 0,
            "a {stalled:.3?} stall leaves audio in the engine, and finishing the capture has to \
             hand it over rather than lose it"
        );
        // Not merely *a* length: the audio the engine was holding. A drain that
        // had lost the packets and produced silence covering the same period
        // would be exactly the right length and would contain nothing.
        assert!(
            recovered >= recoverable - slack,
            "the engine was holding about {recoverable:.3} s when the capture was finished, and \
             the drain has to hand that over as audio the device captured; it produced \
             {recovered:.3} s of device audio in {seconds:.3} s of track, so the rest is silence \
             this crate invented to cover a period the audio is missing from"
        );
        assert_eq!(
            capture.stats().frames - before,
            timeline.frames,
            "everything the drain handed over is on the same timeline as the recording"
        );
        assert!(
            matches!(
                capture.read(Duration::from_millis(10)),
                Err(AudioError::NotOpen)
            ),
            "a capture that has finished draining is closed"
        );
        // Issue #320's third acceptance criterion: the device is let go as
        // promptly as it is without a drain. See `PROMPT`.
        assert!(
            drain_took <= PROMPT,
            "draining took {drain_took:.3?}, which is longer than the {PROMPT:.3?} a drain that \
             waits for nothing should need; Windows shows a microphone as in use for as long as \
             this takes"
        );

        let _ = writeln!(
            std::io::stderr(),
            "drained {seconds:.3} s ({from_the_device} frames from the device) after a \
             {stalled:.3?} stall, in {drain_took:.3?}"
        );
    }

    #[test]
    fn reading_a_closed_microphone_capture_is_reported_rather_than_attempted() {
        let Some(mut capture) = open() else { return };
        capture.close();
        capture.close();

        let error = capture
            .read(Duration::from_millis(1))
            .expect_err("a closed capture has nothing to read");
        assert!(matches!(error, AudioError::NotOpen));
    }
}
