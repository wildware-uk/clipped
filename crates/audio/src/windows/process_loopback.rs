//! The game's own audio: one process tree, captured on its own.
//!
//! # What it is
//!
//! Everything one process tree plays, and nothing else — the track SPEC.md
//! section 11 calls "Game", and the reason Clipped exists rather than a
//! recorder that writes one mixed stream
//! ([ADR 0003](../../../../docs/adr/0003-process-specific-audio-capture.md)).
//! Windows scopes a capture client to a process and the processes it started
//! through `ActivateAudioInterfaceAsync` with
//! `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`; `activation.rs` is that
//! call, and this is what a recording does with it.
//!
//! Everything after the activation is `endpoint_capture.rs`: the same packet
//! loop, the same timeline that keeps a track as long as its recording, the
//! same conversion to `f32`, the same handling of a stream that fails
//! (AGENTS.md section 55). Three things are genuinely different, and they are
//! what this file is.
//!
//! # 1. Nobody says what shape the audio is
//!
//! A process-scoped client has no endpoint, so it has no mix format:
//! `GetMixFormat` is not available on it and the format is the *caller's*
//! choice, which the audio engine then converts into. The choice made here is
//! the default output endpoint's rate and channel count, as 32-bit float —
//! because the game track has to sit in the same file as the system-audio and
//! microphone tracks, and this crate has no resampler
//! ([issue #30](https://github.com/wildware-uk/clipped/issues/30)). A capture
//! that quietly asked for 44.1 kHz would produce a track nothing could mux
//! beside the others.
//!
//! If the audio engine refuses that shape — or the machine has no output device
//! to take it from — [`FALLBACK_RATE`] stereo is asked for instead, and the
//! accepted format is fixed for the life of the capture so that a stream
//! reopened halfway through a recording cannot change it.
//!
//! # 2. The target is a tree, and Windows only takes one root
//!
//! `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS` names **one** process, and Windows
//! includes what that process started. `clipped_windows::ProcessTree`
//! (issue #25) tracks the same membership from this side, with handles rather
//! than remembered identifiers, and is used here for the two questions the
//! activation cannot answer:
//!
//! - **Has the game gone?** An empty tree is the game and everything it started
//!   having exited, which is what [`ProcessLoopbackCapture::target_is_running`]
//!   reports. The capture does not end itself over it — a recording is worth
//!   more than the audio it is missing (AGENTS.md section 17) — the track
//!   simply becomes silence.
//! - **Has the process the activation names gone, while the game lives on?**
//!   Some titles re-execute themselves and exit the process that was launched.
//!   The tree survives that (its members are pinned by handles, and a dead
//!   member is kept as a ghost while anything descends from it), so the capture
//!   re-scopes onto a surviving member rather than recording silence for the
//!   rest of the session.
//!
//! What that re-scoping cannot do is cover *several* surviving members at once:
//! one client, one root. When more than one member survives the process the
//! capture was scoped to, the members outside the new root's own subtree are
//! not captured, and a `warn` says so by name.
//! [Issue #311](https://github.com/wildware-uk/clipped/issues/311) is the way
//! out — several clients mixed into one track — and it needs the mixing stage
//! from [issue #29](https://github.com/wildware-uk/clipped/issues/29).
//!
//! # 3. It may not be available at all
//!
//! Process loopback is documented from Windows build 20348, which no shipping
//! Windows 10 release reaches. [`AudioError::ProcessLoopbackUnavailable`] is
//! what a machine below that floor produces, and the documented answer is a
//! single system-audio track with the separation explicitly stated as
//! unavailable rather than a track labelled "Game" that is really everything
//! (ADR 0003's second consequence, `docs/audio-routing.md`).

use core::num::{NonZeroU16, NonZeroU32};
use core::time::Duration;

use clipped_windows::{ProcessTree, WindowsError};
use windows::Win32::Media::Audio::{
    eRender, IAudioClient, IMMDeviceEnumerator, AUDCLNT_E_UNSUPPORTED_FORMAT,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::CLSCTX_ALL;

use crate::error::{AudioError, Capture};
use crate::format::{AudioFormat, ChannelMask, SampleFormat};
use crate::windows::activation::activate_process_loopback;
use crate::windows::endpoint::{
    default_endpoint, platform_error, EndpointIdentity, MixFormat, SourceKind,
};
use crate::windows::endpoint_capture::{
    create_wake_event, CaptureSource, CaptureStats, EndpointCapture, PositionTrust, Reopen,
    StreamParts, Wake, BUFFER_DURATION,
};

/// The sample rate asked for when the default output endpoint cannot supply
/// one.
///
/// 48 kHz is what the Windows audio engine mixes at on every machine this has
/// been seen on, so it is the shape least likely to be converted twice.
const FALLBACK_RATE: u32 = 48_000;

/// The channel count asked for beside [`FALLBACK_RATE`].
const FALLBACK_CHANNELS: u16 = 2;

/// The speaker positions of a stereo pair: front left and front right.
const STEREO_MASK: u32 = 0x3;

/// How the capture is described in a log line while it is being opened.
///
/// A process-scoped client is not on a device, so the `device` field of the
/// shared engine's log lines would otherwise be empty for it.
fn describe(root: u32) -> String {
    format!("the game's process tree, rooted at process {root}")
}

/// Which processes a capture is scoped to, and how that is kept current.
///
/// The [`CaptureSource`] arm of a process-scoped capture. It owns the tree, the
/// format that was accepted, and the decision to re-scope; the engine owns
/// everything that happens to the samples afterwards.
#[derive(Debug)]
pub(super) struct ProcessLoopbackSource {
    /// The game, as the session identified it. Never changes: it is what the
    /// tree is rooted at and what the track is named after.
    root: u32,
    /// The process the current activation names, which is [`Self::root`] until
    /// that process exits with descendants still running.
    scoped_to: u32,
    /// The membership this capture is really about, maintained live
    /// (issue #25).
    tree: ProcessTree,
    /// The format the audio engine accepted, fixed at the first successful
    /// open. A track's shape may not change underneath a muxer that has already
    /// written a stream header.
    format: Option<AudioFormat>,
    /// A pending reason for the engine to throw the stream away and activate
    /// again.
    change: Option<Reopen>,
    /// Whether the tree has been observed to be empty, so that the game ending
    /// is logged once rather than on every refresh.
    ended: bool,
    /// How this capture is named in a log line.
    description: String,
}

impl ProcessLoopbackSource {
    /// Builds a source scoped to `root` and everything it started.
    ///
    /// # Errors
    ///
    /// [`AudioError::ProcessUnavailable`] when the process cannot be followed:
    /// it has already exited, or it runs at a higher integrity level than
    /// Clipped. Either way there is no tree to scope a capture to, so it is a
    /// failure rather than an empty capture.
    pub(super) fn new(root: u32) -> Result<Self, AudioError> {
        let tree = ProcessTree::rooted_at(root).map_err(|error| match error {
            WindowsError::ProcessUnavailable { process_id } => {
                AudioError::ProcessUnavailable { process_id }
            }
            other => AudioError::Platform {
                operation: "reading the processes a game consists of",
                source: Box::new(other),
            },
        })?;

        Ok(Self {
            root,
            scoped_to: root,
            tree,
            format: None,
            change: None,
            ended: false,
            description: describe(root),
        })
    }

    /// How this capture is named in a log line.
    pub(super) fn description(&self) -> &str {
        &self.description
    }

    /// The process the current activation is scoped to.
    pub(super) fn scoped_to(&self) -> u32 {
        self.scoped_to
    }

    /// How many processes of the game are running.
    pub(super) fn members(&self) -> usize {
        self.tree.members().len()
    }

    /// Brings the tree up to date and decides whether the activation has to
    /// move.
    ///
    /// Called between reads, on the capture thread, from the shared engine.
    /// `ProcessTree::refresh` reads the process table at most once a second and
    /// costs about 25 ns inside that window, so calling it on every read is the
    /// intended use (`docs/audio-routing.md`).
    pub(super) fn take_change(&mut self) -> Option<Reopen> {
        match self.tree.refresh() {
            Ok(change) => {
                if !change.refused().is_empty() {
                    // Not an error and not membership: these are processes
                    // Windows will not let Clipped open, which in practice are
                    // a game's anti-cheat or crash-reporting services. Their
                    // audio — if they make any — is not in this track, and the
                    // only way anybody finds out is a log line.
                    tracing::debug!(
                        audio_source = %SourceKind::GameAudio.audio_source(),
                        refused = ?change.refused(),
                        "some of the game's processes cannot be opened, so they are not part \
                         of the tree this track is scoped to"
                    );
                }
            }
            Err(error) => {
                // The process table could not be read. Membership is left
                // exactly as it was and the next read tries again; a scan that
                // failed is not a game that has exited.
                tracing::warn!(
                    %error,
                    audio_source = %SourceKind::GameAudio.audio_source(),
                    "could not read which processes the game consists of; the capture stays \
                     scoped where it is"
                );
                return self.change.take();
            }
        }

        self.consider_rescoping();
        self.change.take()
    }

    /// Decides whether the process the activation names is still the right one.
    fn consider_rescoping(&mut self) {
        if self.tree.contains(self.scoped_to) {
            return;
        }

        let Some(&successor) = self.tree.members().first() else {
            // The game and everything it started have gone. The capture is left
            // exactly as it is: the track becomes silence of the right length,
            // because that is what a stream with nothing rendering into it
            // produces, and stopping is the caller's decision rather than this
            // crate's (AGENTS.md section 17).
            if !self.ended {
                self.ended = true;
                tracing::info!(
                    audio_source = %SourceKind::GameAudio.audio_source(),
                    root = self.root,
                    "the game and every process it started have exited; this track is silence \
                     from here"
                );
            }
            return;
        };

        let members = self.tree.members();
        if members.len() > 1 {
            // One activation, one root. A game that leaves two unrelated
            // processes behind cannot be captured by one client, and saying so
            // is better than a track that quietly lost half the game.
            tracing::warn!(
                audio_source = %SourceKind::GameAudio.audio_source(),
                scoping_to = successor,
                members = ?members,
                "the process this capture was scoped to has exited and more than one process \
                 of the game is still running. Windows scopes a capture to one process tree, \
                 so audio from a process that did not descend from the one named here is not \
                 in this track (issue #311)"
            );
        } else {
            tracing::info!(
                audio_source = %SourceKind::GameAudio.audio_source(),
                scoping_to = successor,
                "the process this capture was scoped to has exited and the game is still \
                 running, so the capture is re-scoping onto what is left of it"
            );
        }

        self.scoped_to = successor;
        self.description = format!(
            "the game's process tree, rooted at process {} and captured through process {}",
            self.root, successor
        );
        self.change = Some(Reopen {
            reason: "the process the game's audio was captured through exited",
            // Paced by processes exiting rather than by a call that fails on
            // every look, so this cannot become a loop.
            from_failed_call: false,
        });
    }

    /// Activates and initialises a client scoped to the tree.
    ///
    /// [`None`] when there is nothing to scope to: every process of the game
    /// has exited. The engine treats that as a state to wait through, exactly
    /// as it waits through an unplugged microphone.
    ///
    /// # Errors
    ///
    /// [`AudioError::ProcessLoopbackUnavailable`] when Windows will not give a
    /// process-scoped client, and [`AudioError::UnsupportedFormat`] when the
    /// audio engine refuses every shape this crate knows how to ask for.
    pub(super) fn open_stream(
        &mut self,
        enumerator: &IMMDeviceEnumerator,
    ) -> Result<Option<StreamParts>, AudioError> {
        if self.tree.members().is_empty() {
            return Ok(None);
        }

        let client = activate_process_loopback(
            self.scoped_to,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
        )?;

        let candidates = match self.format {
            // A reopen mid-recording: the shape is already decided, and asking
            // for anything else would change a track's format underneath
            // whatever is writing it.
            Some(format) => vec![format],
            None => candidate_formats(enumerator),
        };

        let (client, format, wake) = initialise(client, &candidates, self.scoped_to)?;
        self.format = Some(format);

        Ok(Some(StreamParts {
            kind: SourceKind::GameAudio,
            identity: EndpointIdentity {
                id: format!("process-loopback:{}", self.scoped_to),
                name: self.description.clone(),
            },
            format,
            client,
            wake,
            // There is no device and so no mute switch: what Windows mutes is
            // an endpoint, and this capture is not on one.
            mute: None,
            // Whether a process-scoped client fills the performance-counter
            // position of a packet is not documented, and a track timed from a
            // number that is not a counter reading would be discarded whole.
            // The first packet decides (`endpoint_capture.rs`).
            positions: PositionTrust::Unverified,
        }))
    }
}

/// The formats to ask the audio engine for, in the order they are tried.
///
/// The default output endpoint's rate and channel count first, as 32-bit float,
/// so that the game track can sit beside the system-audio track in one file
/// without resampling. Then a plain 48 kHz stereo, for a machine with no output
/// device — which is a machine that plays nothing, but whose games can still be
/// recorded — and for an engine that refuses the first.
fn candidate_formats(enumerator: &IMMDeviceEnumerator) -> Vec<AudioFormat> {
    let fallback = AudioFormat::new(
        NonZeroU32::new(FALLBACK_RATE).expect("48 kHz is not zero"),
        NonZeroU16::new(FALLBACK_CHANNELS).expect("stereo is not zero channels"),
        ChannelMask::from_bits(STEREO_MASK),
        SampleFormat::Float32,
    );

    let Some(preferred) = endpoint_shape(enumerator) else {
        return vec![fallback];
    };
    if preferred.is_interchangeable_with(&fallback) {
        return vec![fallback];
    }
    vec![preferred, fallback]
}

/// The rate and channel count the default output endpoint mixes at, as 32-bit
/// float.
///
/// [`None`] when there is no output device or Windows will not describe it,
/// both of which are ordinary on a machine with no sound card: there is then no
/// endpoint shape to match and the fallback is as good an answer as any.
fn endpoint_shape(enumerator: &IMMDeviceEnumerator) -> Option<AudioFormat> {
    let device = default_endpoint(enumerator, eRender).ok()??;
    // SAFETY: `device` is a live `IMMDevice`; windows-rs infers the interface
    // identifier from the return type, so the activation cannot ask for one
    // interface and be typed as another.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.ok()?;
    let mix = MixFormat::of(&client).ok()?.audio();

    Some(AudioFormat::new(
        mix.sample_rate(),
        mix.channels(),
        mix.channel_mask(),
        // Whatever the endpoint presents, this capture asks the engine for
        // floats: it is the format the engine mixes in, so it is the one
        // conversion that cannot lose anything.
        SampleFormat::Float32,
    ))
}

/// Initialises the client with the first format the audio engine accepts.
///
/// Answers the client, the format that was accepted and how the capture thread
/// will wait on it. The client is returned rather than mutated in place because
/// `IAudioClient::Initialize` may be called only once on one: every retry —
/// another format, or the fall back to polling — activates a fresh client.
fn initialise(
    client: IAudioClient,
    candidates: &[AudioFormat],
    target: u32,
) -> Result<(IAudioClient, AudioFormat, Wake), AudioError> {
    let mut client = client;
    let mut refused = Vec::new();

    for (attempt, format) in candidates.iter().enumerate() {
        if attempt > 0 {
            // A client whose `Initialize` failed is spent, whatever it failed
            // for.
            client = activate_process_loopback(
                target,
                PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            )?;
        }

        match initialise_with(&client, *format) {
            Ok(wake) => {
                tracing::debug!(
                    audio_source = %SourceKind::GameAudio.audio_source(),
                    format = %format,
                    wake = ?wake,
                    "the audio engine accepted this shape for a process-scoped capture"
                );
                return Ok((client, *format, wake));
            }
            Err(error) if error.code() == AUDCLNT_E_UNSUPPORTED_FORMAT => {
                refused.push(format.to_string());
            }
            Err(error) => {
                return Err(platform_error(
                    "opening a process-scoped audio capture stream",
                    error,
                ))
            }
        }
    }

    Err(AudioError::unsupported_format(format!(
        "the Windows audio engine would not capture the game's audio as {}",
        refused.join(" or ")
    )))
}

/// One attempt at initialising a client with one format.
///
/// Event-driven first, and polling if the audio engine refuses that, exactly as
/// an endpoint stream does (`endpoint_capture.rs`): a recording with the audio
/// read on a timer is better than one without the audio. The second attempt
/// needs a second client, which is the caller's business, so a refusal comes
/// back as the error it was.
fn initialise_with(client: &IAudioClient, format: AudioFormat) -> windows::core::Result<Wake> {
    let wave = wave_format(format);
    // SAFETY: `WAVEFORMATEXTENSIBLE` starts with the `WAVEFORMATEX` every
    // format begins with, at offset zero, so this is the pointer Windows
    // expects. It is taken from the whole structure rather than from the field,
    // because both are `#[repr(packed)]` and a reference to a field of a packed
    // structure is undefined behaviour.
    let raw = (&raw const wave).cast::<WAVEFORMATEX>();

    // SAFETY: `client` is a live, uninitialised `IAudioClient` from
    // `activate_process_loopback`, `raw` points at a live format that outlives
    // the call, and the flags are the ones Microsoft documents as required for
    // process loopback: shared mode, loopback, and a buffer duration.
    let event_driven = unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            SourceKind::GameAudio.stream_flags() | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            BUFFER_DURATION,
            0,
            raw,
            None,
        )
    };

    event_driven.and_then(|()| create_wake_event(client))
}

/// The `WAVEFORMATEXTENSIBLE` describing `format` as 32-bit float.
///
/// Extensible rather than a plain `WAVEFORMATEX` because it is the only form
/// that states which speaker each channel is, and a surround game whose
/// channels were unlabelled would be muxed by guesswork.
fn wave_format(format: AudioFormat) -> WAVEFORMATEXTENSIBLE {
    let channels = format.channels().get();
    let bits = u16::try_from(SampleFormat::Float32.bytes_per_sample() * 8)
        .expect("a sample is far narrower than u16::MAX bits");
    let block = channels * bits / 8;
    let rate = format.sample_rate().get();

    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: u16::try_from(size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>())
                .expect("the extensible tail is 22 bytes"),
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: bits,
        },
        dwChannelMask: mask_for(format),
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// The speaker positions to ask for.
///
/// An endpoint whose mix format had no extensible part says nothing about
/// speaker positions, and asking the audio engine for a stereo stream with no
/// positions at all is asking it to guess. Stereo is filled in because it is
/// the only layout that has one obvious answer; anything else is passed through
/// as it was, including zero.
fn mask_for(format: AudioFormat) -> u32 {
    match (format.channel_mask().bits(), format.channels().get()) {
        (0, FALLBACK_CHANNELS) => STEREO_MASK,
        (mask, _) => mask,
    }
}

/// A capture of everything one process tree plays.
///
/// See `endpoint_capture.rs` for the threading and ownership rules — a capture
/// is read on a thread the caller supplies, and reading is the only thing that
/// changes it — and `docs/audio-routing.md` for how the tree is resolved and
/// what happens when the game's processes come and go.
///
/// # Example
///
/// ```no_run
/// use core::time::Duration;
///
/// use clipped_audio::windows::ProcessLoopbackCapture;
/// use clipped_audio::Capture;
///
/// # let game_process = 0_u32;
/// let mut capture = ProcessLoopbackCapture::open(game_process)?;
/// while capture.target_is_running() {
///     match capture.read(Duration::from_millis(100))? {
///         Capture::Samples(audio) => { /* audio.samples() is the game, and only the game */ }
///         Capture::Idle | Capture::FormatChanged(_) => {}
///     }
/// }
///
/// // Hand over what the audio engine still holds rather than losing the last
/// // fraction of a second of the recording.
/// capture.finish();
/// while let Ok(Capture::Samples(audio)) = capture.read(Duration::from_millis(100)) {
///     let _ = audio;
/// }
/// # Ok::<(), clipped_audio::AudioError>(())
/// ```
#[derive(Debug)]
pub struct ProcessLoopbackCapture {
    capture: EndpointCapture,
    root: u32,
}

impl ProcessLoopbackCapture {
    /// Opens a capture of everything `root_process` and the processes it
    /// started are playing.
    ///
    /// `root_process` should be the game itself rather than the launcher that
    /// started it: a tree rooted at Steam would put its notification chime into
    /// the track named after the game (`docs/audio-routing.md`).
    ///
    /// # Errors
    ///
    /// [`AudioError::ProcessLoopbackUnavailable`] when this machine cannot
    /// capture a process tree at all, which is expected below Windows build
    /// 20348. The documented answer is to record one system-audio track with
    /// [`SystemAudioCapture`](super::SystemAudioCapture) and to say that
    /// per-source separation is unavailable, rather than to label everything
    /// the machine plays "Game" (ADR 0003).
    ///
    /// [`AudioError::ProcessUnavailable`] when the process cannot be followed —
    /// it has exited, or it runs at a higher integrity level than Clipped —
    /// [`AudioError::UnsupportedFormat`] when the audio engine refuses every
    /// shape this crate asks for, and [`AudioError::Platform`] when Windows
    /// refuses something outright.
    pub fn open(root_process: u32) -> Result<Self, AudioError> {
        let source = ProcessLoopbackSource::new(root_process)?;
        let capture = EndpointCapture::open(CaptureSource::ProcessTree(source))?
            // The tree was built a moment ago and had a member in it, so there
            // is only one way to be here: every process of the game exited
            // between the two calls.
            .ok_or(AudioError::ProcessUnavailable {
                process_id: root_process,
            })?;

        Ok(Self {
            capture,
            root: root_process,
        })
    }

    /// The shape of every buffer this capture produces.
    ///
    /// Chosen when the capture is opened — see the module documentation — and
    /// fixed for its life, including across a re-scoping onto another process
    /// of the same game.
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.capture.format()
    }

    /// The process this capture was opened for.
    #[must_use]
    pub fn root_process(&self) -> u32 {
        self.root
    }

    /// The process the current activation is scoped to.
    ///
    /// [`Self::root_process`] unless that process has exited with descendants
    /// still running, in which case it is the surviving member the capture
    /// re-scoped onto.
    #[must_use]
    pub fn scoped_to(&self) -> u32 {
        self.capture
            .tree_state()
            .map_or(self.root, |state| state.scoped_to)
    }

    /// Whether any process of the game is still running.
    ///
    /// `false` once the game and everything it started have exited, which is
    /// how a caller knows there is nothing left to capture. The capture does not
    /// stop itself: the track keeps its place on the timeline as silence until
    /// the caller stops it, because a recording is worth more than the audio it
    /// is missing.
    #[must_use]
    pub fn target_is_running(&self) -> bool {
        self.capture
            .tree_state()
            .is_some_and(|state| state.members > 0)
    }

    /// What this capture has produced so far.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        self.capture.stats()
    }

    /// Reads the next block of the game's audio, waiting up to `timeout` for
    /// one.
    ///
    /// Consecutive buffers are exactly contiguous, whatever the game's
    /// processes did in between: periods it played nothing come back as
    /// [`SampleOrigin::SynthesisedSilence`](crate::SampleOrigin::SynthesisedSilence)
    /// of the right length rather than as nothing at all.
    ///
    /// # Errors
    ///
    /// [`AudioError::NotOpen`] after [`close`](Self::close), and after a drain
    /// started by [`finish`](Self::finish) has handed over everything it had.
    /// Failures of the capture itself are not errors: they are handled, logged,
    /// and reported through [`Capture`].
    pub fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
        self.capture.read(timeout)
    }

    /// Stops capturing and hands over what the audio engine still holds.
    ///
    /// The audio engine keeps up to 200 ms of captured audio for this stream. A
    /// capture that is simply closed loses it, which is the last fraction of a
    /// second before the user stopped recording — often the part they pressed
    /// the key for. After this, [`read`](Self::read) returns the packets that
    /// were queued and then reports [`AudioError::NotOpen`]; nothing is
    /// reopened and no further silence is synthesised.
    pub fn finish(&mut self) {
        self.capture.begin_drain();
    }

    /// Stops capturing and releases the client, discarding anything not yet
    /// collected.
    ///
    /// [`finish`](Self::finish) is the ordinary way to end a recording; this is
    /// for a caller that wants the capture gone now. Idempotent, and does the
    /// same thing as dropping the capture.
    pub fn close(&mut self) {
        self.capture.close();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::time::Instant;

    use super::*;
    use crate::buffer::SampleOrigin;
    use crate::windows::endpoint_capture::testing::{skipped, Contiguity};

    /// How long a test waits for Windows to start or end a process.
    ///
    /// Generous on purpose: it bounds a hang rather than asserting a latency.
    const PATIENCE: Duration = Duration::from_secs(20);

    /// How far a track's length may sit from the time that passed while it was
    /// read.
    ///
    /// A track is as long as the *device* says: what a read hands over is what
    /// the audio engine had captured by the moment it was asked for it, and the
    /// engine holds up to [`BUFFER_DURATION`] that nobody has asked for yet. So
    /// a length measured across an interval is the time that interval took, give
    /// or take how much the engine happened to be holding at each end of it.
    ///
    /// Every length below is asserted against a *measured* elapsed time plus
    /// this, and never against the duration a `sleep` or a read loop was asked
    /// for. `std::thread::sleep` is a floor rather than a duration, and a read
    /// loop leaves when its deadline has passed rather than when it arrives; a
    /// thread descheduled on a shared runner comes back long after either, and
    /// the audio engine goes on capturing the whole time it is away. A bound
    /// written against the nominal duration therefore fails on a busy machine
    /// with nothing having regressed — three of these tests did, on a commit
    /// that changed an icon
    /// ([issue #387](https://github.com/wildware-uk/clipped/issues/387),
    /// AGENTS.md section 25).
    const ENGINE_BACKLOG: Duration = Duration::from_nanos(BUFFER_DURATION as u64 * 100);

    /// How long the consumer stops reading for in the drain test.
    ///
    /// Long enough that what the drain has to produce is still bounded from
    /// below once [`ENGINE_BACKLOG`] is allowed for either side of it. The 150 ms
    /// this stalled for previously is shorter than the engine's own buffer, so
    /// the only honest lower bound on it would have been zero.
    const STALL: Duration = Duration::from_millis(500);

    /// What a stretch of reading produced, and how long it really took.
    #[derive(Debug)]
    struct Reading {
        /// Every buffer that arrived, checked contiguous with the one before it
        /// as it did.
        timeline: Contiguity,
        /// Of those frames, the ones the client delivered — as opposed to the
        /// ones this crate invented to cover a period the client said nothing
        /// about.
        from_the_client: u64,
        /// The wall-clock time this really occupied. See [`ENGINE_BACKLOG`] for
        /// why it is measured rather than assumed.
        elapsed: Duration,
    }

    impl Reading {
        /// Asserts that the audio is as long as the time it was read over.
        ///
        /// The property every downstream stage depends on: a second of
        /// recording is a second of audio, contiguous, whether or not the tree
        /// played anything in it. A capture that only produced samples while the
        /// game made a noise would slide against the video by exactly the amount
        /// of quiet in the recording.
        fn assert_as_long_as_it_took(&self, what: &str) {
            let took = self.elapsed.as_secs_f64();
            let slack = ENGINE_BACKLOG.as_secs_f64();
            let seconds = self.timeline.seconds();
            assert!(
                (took - slack..=took + slack).contains(&seconds),
                "{what}: {took:.3} s passed, so there should be about {took:.3} s of audio, \
                 give or take the {slack:.3} s the audio engine holds; got {seconds:.3} s"
            );
        }
    }

    /// Opens a capture of a process tree, or reports why this machine cannot.
    ///
    /// **These tests make no sound.** They capture a process tree that renders
    /// nothing — this test process, or a `cmd.exe` the test started — so what
    /// they read is silence, and they are exempt from the "does this machine
    /// want quiet" check every test that plays a tone begins with. What they
    /// do need is a machine whose Windows can scope a capture to a process at
    /// all, which a GitHub runner cannot, so they skip loudly there.
    fn open(root: u32) -> Option<ProcessLoopbackCapture> {
        match ProcessLoopbackCapture::open(root) {
            Ok(capture) => Some(capture),
            Err(error @ AudioError::ProcessLoopbackUnavailable { .. }) => {
                skipped(&format!("{error}"));
                None
            }
            Err(error) => {
                skipped(&format!(
                    "a process-scoped capture could not be opened here: {error}"
                ));
                None
            }
        }
    }

    /// Reads for at least `duration`, returning what arrived and how long it
    /// took.
    ///
    /// Answers the frames handed over — asserting on the way that every buffer
    /// was exactly contiguous with the one before it, which is the property
    /// everything downstream depends on — how many of those frames the client
    /// produced rather than this crate inventing them, and the elapsed time the
    /// length is judged against.
    ///
    /// The elapsed time is not `duration`. This leaves when the deadline has
    /// *passed*, which is one read later at best and however long the thread was
    /// descheduled for at worst (see [`ENGINE_BACKLOG`]).
    fn read_for(capture: &mut ProcessLoopbackCapture, duration: Duration) -> Reading {
        let mut timeline = Contiguity::new(capture.format());
        let mut from_the_client = 0u64;
        let started = Instant::now();
        let until = started + duration;
        while Instant::now() < until {
            match capture
                .read(Duration::from_millis(100))
                .expect("a healthy capture does not fail")
            {
                Capture::Samples(samples) => {
                    if samples.origin() == SampleOrigin::Endpoint {
                        from_the_client += samples.frames() as u64;
                    }
                    timeline.accept(&samples);
                }
                Capture::Idle | Capture::FormatChanged(_) => {}
            }
        }
        Reading {
            timeline,
            from_the_client,
            elapsed: started.elapsed(),
        }
    }

    /// A process that does nothing until its standard input closes, and the
    /// descendants it starts when told to.
    ///
    /// The same fixture `crates/windows/tests/process_tree.rs` uses, and for
    /// the same reasons: the descendants appear only when the test says so, and
    /// killing the first one leaves the others running with a parent identifier
    /// that names nothing — which is a game that re-executed itself and let go
    /// of the process it was launched as.
    struct Chain {
        root: Child,
        input: Option<ChildStdin>,
    }

    impl Chain {
        fn start() -> Self {
            let mut root = Command::new("cmd.exe")
                // `set /p` reads one line from standard input; what follows the
                // `&` runs once it has one, and `more` then holds that input
                // open until it is closed.
                .args(["/c", "set /p go= & cmd.exe /c more"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("cmd.exe is on every Windows installation");
            let input = root.stdin.take().expect("standard input was piped");
            Self {
                root,
                input: Some(input),
            }
        }

        fn root_pid(&self) -> u32 {
            self.root.id()
        }

        /// Makes the root start the two processes below it.
        fn start_descendants(&mut self) {
            let input = self.input.as_mut().expect("the chain is still open");
            input
                .write_all(b"go\r\n")
                .expect("the root is still reading");
            input.flush().expect("the root is still reading");
        }

        /// Ends the root, leaving its descendants running and orphaned.
        fn kill_root(&mut self) {
            self.root.kill().expect("the root is this test's own child");
            self.root.wait().expect("the root is this test's own child");
        }
    }

    impl Drop for Chain {
        fn drop(&mut self) {
            // Both halves, in this order. Killing the root ends only the root —
            // Windows does not end a process's children with it — and closing
            // the input is what ends the two below it. A test that failed early
            // must not leave processes behind on a machine that is shared.
            let _ = self.root.kill();
            let _ = self.root.wait();
            self.input = None;
        }
    }

    /// Waits until `condition` holds, reading the capture meanwhile.
    ///
    /// Reading is what makes the capture notice anything: the process tree is
    /// refreshed between reads, on the reading thread, and never behind one.
    fn read_until(
        capture: &mut ProcessLoopbackCapture,
        condition: impl Fn(&ProcessLoopbackCapture) -> bool,
    ) -> bool {
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            if condition(capture) {
                return true;
            }
            let _ = capture.read(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn a_process_tree_that_plays_nothing_still_produces_a_track_of_the_right_length() {
        // The property every downstream stage depends on, against the real
        // activation rather than against the timeline in isolation: a second of
        // reading is a second of audio, contiguous, whether or not the tree
        // played anything. A capture that only produced samples while the game
        // made a noise would slide against the video by exactly the amount of
        // quiet in the recording.
        let Some(mut capture) = open(std::process::id()) else {
            return;
        };

        let reading = read_for(&mut capture, Duration::from_millis(1_200));
        reading.assert_as_long_as_it_took("reading a tree that plays nothing");

        // And the length has to come from the client rather than from silence
        // invented to cover a client that produced nothing, or this would pass
        // just as well on a capture that never delivered a packet.
        //
        // That is a claim about the platform, and it is the one measured
        // difference between this and endpoint loopback: an endpoint delivers
        // nothing at all while it is quiet, whereas a process-scoped client on
        // Windows 11 build 26200 delivers silent packets continuously — 48,000
        // frames a second from a tree playing nothing, with no silence
        // synthesised at all (`docs/audio-routing.md`). A machine where that
        // stopped holding would fail here rather than quietly changing what
        // this crate is built on.
        assert!(
            reading.from_the_client > 0,
            "every one of the {} frames was silence this crate invented, and the client              delivered nothing",
            reading.timeline.frames
        );
        assert!(
            capture.target_is_running(),
            "this test process is a member of its own process tree"
        );
        assert_eq!(capture.scoped_to(), std::process::id());
    }

    #[test]
    fn stopping_a_capture_hands_over_the_audio_the_engine_was_still_holding() {
        // Issue #26's third scope item. The audio engine holds captured audio
        // nobody has asked for yet; a capture that is simply closed throws it
        // away, which is the last fraction of a second before somebody stopped
        // recording.
        //
        // The consumer stops reading for long enough to leave a real backlog,
        // and the drain then has to produce it — after the client has been
        // stopped, which is the part that makes this a drain rather than a
        // longer read.
        let Some(mut capture) = open(std::process::id()) else {
            return;
        };
        let format = capture.format();

        read_for(&mut capture, Duration::from_millis(300));

        // Measured, not assumed: `sleep` guarantees only that it does not
        // return early, and the engine goes on capturing for however long this
        // thread is really away (see `ENGINE_BACKLOG`).
        let stall_began = Instant::now();
        std::thread::sleep(STALL);
        let stalled = stall_began.elapsed();

        let before = capture.stats().frames;
        capture.finish();

        let mut timeline = Contiguity::new(format);
        let mut from_the_client = 0u64;
        loop {
            match capture.read(Duration::from_millis(100)) {
                Ok(Capture::Samples(samples)) => {
                    if samples.origin() == SampleOrigin::Endpoint {
                        from_the_client += samples.frames() as u64;
                    }
                    timeline.accept(&samples);
                }
                Ok(Capture::Idle | Capture::FormatChanged(_)) => break,
                // The drain has handed over everything and closed itself, which
                // is how a caller knows there is no more.
                Err(AudioError::NotOpen) => break,
                Err(error) => panic!("a drain does not fail: {error}"),
            }
        }
        let drain = Reading {
            timeline,
            from_the_client,
            elapsed: stalled,
        };

        assert!(
            drain.timeline.frames > 0,
            "a {stalled:.3?} stall leaves audio in the engine, and stopping the capture must \
             hand it over rather than lose it"
        );
        // Not just *a* length: the audio the engine was holding. The rest of
        // what a drain hands over is silence covering the gap between where the
        // recording had got to and where the surviving packets sit, and a drain
        // that had lost the packets and kept only the gap would still be the
        // right length — measured on Windows 11 build 26200, a stall of any
        // length from 50 ms to 1.5 s leaves the same 30 ms of real audio in the
        // engine and the rest of the drain is that silence.
        assert!(
            drain.from_the_client > 0,
            "every one of the {} frames the drain handed over was silence this crate invented; \
             what a drain is for is the audio the engine had captured and not been asked for",
            drain.timeline.frames
        );
        // And the recording does not lose the period the reader was away for:
        // the drain covers it, however long it really was.
        drain.assert_as_long_as_it_took("draining after a stall");
        assert_eq!(
            capture.stats().frames - before,
            drain.timeline.frames,
            "everything the drain handed over is on the same timeline as the recording"
        );
        assert!(
            matches!(
                capture.read(Duration::from_millis(10)),
                Err(AudioError::NotOpen)
            ),
            "a capture that has finished draining is closed"
        );
    }

    #[test]
    fn the_game_exiting_is_noticed_and_does_not_end_the_recording() {
        // Issue #26's second scope item, and AGENTS.md section 17: a recording
        // is worth more than the audio it is missing. When the last of the
        // game's processes goes, the caller is told — that is how a session
        // knows to stop — and the track carries on as silence of exactly the
        // right length rather than stopping mid-recording or failing a read.
        let mut chain = Chain::start();
        let Some(mut capture) = open(chain.root_pid()) else {
            return;
        };

        assert!(
            capture.target_is_running(),
            "the process the capture was opened for is running"
        );
        read_for(&mut capture, Duration::from_millis(200));

        chain.kill_root();
        drop(chain);

        assert!(
            read_until(&mut capture, |capture| !capture.target_is_running()),
            "the capture has to notice that every process of the game has exited"
        );

        let after = read_for(&mut capture, Duration::from_millis(600));
        after.assert_as_long_as_it_took(
            "the track has to keep its place on the timeline after the game exits",
        );
    }

    #[test]
    fn a_game_that_re_executes_itself_is_followed_onto_the_process_that_survived() {
        // The case a single-root API cannot express. Windows scopes a capture
        // to one process and its children; some titles start, spawn the process
        // that really is the game, and exit. Recording silence for the rest of
        // the session because the process named in the activation is gone would
        // be a bug nobody notices until they open the file.
        let mut chain = Chain::start();
        let root = chain.root_pid();
        let Some(mut capture) = open(root) else {
            return;
        };

        chain.start_descendants();
        // The tree reads the process table once a second, so the descendants
        // are members within an interval of appearing.
        assert!(
            read_until(&mut capture, |capture| capture.target_is_running()),
            "the chain is running"
        );
        read_for(&mut capture, Duration::from_millis(1_100));

        chain.kill_root();

        assert!(
            read_until(&mut capture, |capture| capture.scoped_to() != root),
            "the capture has to re-scope onto a process of the game that is still running"
        );
        assert!(
            capture.target_is_running(),
            "the game is still running: only the process it was launched as has gone"
        );
        assert_eq!(
            capture.root_process(),
            root,
            "the track is still the track of the game it was opened for"
        );
        assert!(
            capture.stats().endpoint_changes > 0,
            "re-scoping means activating a new client, which is a change worth counting"
        );

        // And the recording carries on across it, contiguously.
        let after = read_for(&mut capture, Duration::from_millis(600));
        after.assert_as_long_as_it_took("reading after a re-scoping");
        assert!(
            after.from_the_client > 0,
            "the client activated on the surviving process has to be delivering packets, and              everything after the re-scoping was invented silence"
        );
    }

    fn stereo(rate: u32, mask: u32) -> AudioFormat {
        AudioFormat::new(
            NonZeroU32::new(rate).expect("a rate is not zero"),
            NonZeroU16::new(2).expect("stereo is not zero channels"),
            ChannelMask::from_bits(mask),
            SampleFormat::Float32,
        )
    }

    #[test]
    fn the_format_asked_for_is_the_one_the_wave_format_describes() {
        // The structure Windows is handed *is* the request: a mismatch between
        // it and the `AudioFormat` this crate then converts by would be read as
        // noise rather than as audio, because every packet would be
        // deinterleaved with the wrong stride.
        let format = AudioFormat::new(
            NonZeroU32::new(44_100).expect("a rate is not zero"),
            NonZeroU16::new(6).expect("5.1 is not zero channels"),
            ChannelMask::from_bits(0x3f),
            SampleFormat::Float32,
        );
        let wave = wave_format(format);

        assert_eq!({ wave.Format.nSamplesPerSec }, 44_100);
        assert_eq!({ wave.Format.nChannels }, 6);
        assert_eq!({ wave.Format.wBitsPerSample }, 32);
        assert_eq!({ wave.Format.nBlockAlign }, 24, "six 32-bit samples");
        assert_eq!({ wave.Format.nAvgBytesPerSec }, 44_100 * 24);
        assert_eq!({ wave.dwChannelMask }, 0x3f);
        assert_eq!({ wave.SubFormat }, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        assert_eq!(
            { wave.Format.cbSize },
            22,
            "the tail Windows reads a subformat and a channel mask out of"
        );
    }

    #[test]
    fn a_stereo_endpoint_that_named_no_speakers_is_asked_for_the_obvious_pair() {
        // An endpoint with no extensible part reports a mask of zero, and
        // passing that on asks the audio engine to guess which speaker each
        // channel is.
        assert_eq!(mask_for(stereo(48_000, 0)), STEREO_MASK);
        assert_eq!(
            mask_for(stereo(48_000, 0x603)),
            0x603,
            "a mask the endpoint did state is passed through untouched"
        );

        let five_one = AudioFormat::new(
            NonZeroU32::new(48_000).expect("a rate is not zero"),
            NonZeroU16::new(6).expect("5.1 is not zero channels"),
            ChannelMask::from_bits(0),
            SampleFormat::Float32,
        );
        assert_eq!(
            mask_for(five_one),
            0,
            "there is no obvious layout for six unlabelled channels, so none is invented"
        );
    }

    #[test]
    fn a_capture_cannot_be_scoped_to_a_process_that_cannot_be_followed() {
        // 0 is the system idle process: the one identifier that cannot be
        // opened on any machine, which makes it the only deterministic negative
        // case available (AGENTS.md section 25). It needs no audio hardware, so
        // it runs in the pull-request CI job.
        let error = ProcessLoopbackSource::new(0).expect_err("the idle process cannot be followed");

        assert!(
            matches!(error, AudioError::ProcessUnavailable { process_id: 0 }),
            "unexpected error: {error}"
        );
        assert!(
            error.to_string().contains("no longer running"),
            "the message has to say what is wrong in words a user can act on: {error}"
        );
    }

    #[test]
    fn the_unavailable_error_says_what_the_machine_needs_and_what_happens_instead() {
        // ADR 0003's second consequence: below the Windows floor this feature
        // is unavailable and the fallback is a supported mode. A user whose
        // tracks all came out identical has to be able to find out why from the
        // message rather than from an HRESULT (AGENTS.md sections 15 and 45).
        let error = AudioError::process_loopback_unavailable("E_NOTIMPL".to_owned());
        let message = error.to_string();

        assert!(message.contains("20348"), "{message}");
        assert!(message.contains("E_NOTIMPL"), "{message}");
        assert!(
            message.contains("system audio"),
            "the message has to name the fallback: {message}"
        );
    }

    #[test]
    fn the_description_names_the_process_a_track_is_scoped_to() {
        // It is the `device` field of every log line the shared engine writes
        // about this capture, and "which game" is the only thing that makes
        // those lines worth reading when two captures are running.
        assert_eq!(
            describe(4_242),
            "the game's process tree, rooted at process 4242"
        );
    }
}
