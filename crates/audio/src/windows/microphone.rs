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
use crate::windows::endpoint_capture::{CaptureStats, EndpointCapture};

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
        let endpoint = EndpointCapture::open(source)?.ok_or_else(|| selection.unavailable())?;

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

    /// Stops capturing and releases the device.
    ///
    /// Idempotent, and does the same thing as dropping the capture. Releasing
    /// the microphone matters more than releasing a speaker: Windows shows a
    /// microphone-in-use indicator for as long as any application holds one.
    pub fn close(&mut self) {
        self.endpoint.close();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::buffer::SampleOrigin;
    use crate::windows::endpoint_capture::testing::{skipped, Contiguity};
    use crate::windows::notifications::EndpointChange;
    use crate::windows::SystemAudioCapture;

    /// Opens the default microphone, or reports why this machine cannot.
    ///
    /// Every test that uses this records the room the machine is in for a
    /// second or so. None of them keeps a sample: they count frames, compare
    /// timestamps, and check that buffers reported as silence are zero.
    fn open() -> Option<MicrophoneCapture> {
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
        assert_eq!(
            defaults, 1,
            "exactly one of the listed microphones should be the one Windows records from"
        );

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
        // The failure is injected where WASAPI reports it, so the whole
        // response — the stream torn down, the device tried again, the backing
        // off when it fails at once, the gap becoming silence — is the code
        // that runs when somebody stands up wearing a headset.
        //
        // The recording must carry on: a microphone track of the right length,
        // made of silence, rather than a read that fails or a capture that
        // stops (AGENTS.md sections 16 and 17).
        let Some(mut capture) = open() else { return };
        let format = capture.format();
        capture.endpoint.fail_the_endpoint_from_now_on();

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
        // device has to reopen that device rather than whatever Windows has
        // since made default. The device here is chosen by identifier, torn
        // down, and has to be the one that comes back.
        let Some(default) = microphones()
            .expect("Windows can list its input devices")
            .into_iter()
            .find(Microphone::is_default)
        else {
            skipped("this machine has no microphone");
            return;
        };

        let Ok(mut capture) = MicrophoneCapture::open(&default.select()) else {
            skipped("the chosen microphone could not be opened");
            return;
        };
        assert_eq!(capture.device_name(), Some(default.name()));

        let _ = capture.read(Duration::from_millis(200));
        capture
            .endpoint
            .simulate_endpoint_change(EndpointChange::CaptureEndpointRemoved);
        for _ in 0..10 {
            let _ = capture.read(Duration::from_millis(200));
        }

        assert_eq!(
            capture.device_name(),
            Some(default.name()),
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
