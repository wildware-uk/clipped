//! The audio sources of a recording: which ones are opened, what tracks they
//! get, and the threads that carry their samples to the file.
//!
//! `clipped-audio` captures system audio and a microphone; `clipped-muxer`
//! writes named audio tracks. This module is the join between them, and the
//! only place in the workspace that holds both.
//!
//! # The shape
//!
//! ```text
//!  the recording's thread                    one thread per source
//!  ──────────────────────                    ─────────────────────
//!  open the endpoints        ─── format ──▶  (opened here, moved there)
//!  declare the tracks
//!  create the file
//!  wait for the first frame  ─── clock ───▶  read a buffer
//!                                            place it on the timeline
//!                            ◀── samples ─── queue it (never waits)
//!  stop                      ─── flag ────▶  close the endpoint, report
//! ```
//!
//! Two things cross in each direction and no more. Out: the endpoint's format,
//! because Matroska fixes a track's sampling rate and channel count in the
//! header and the file cannot be created until both are known; and the
//! [`CaptureClock`], because a packet cannot be placed until the recording has
//! an epoch, and the epoch is the first video frame the recording keeps
//! (`docs/av-sync.md`). Back: samples, through a bounded queue that drops rather
//! than blocks, and a report of what the source produced.
//!
//! # Threading
//!
//! One thread per source, and they wait on nothing but their own endpoint
//! (AGENTS.md section 20). [`CaptureClock`] is `Copy` and stateless, so each
//! thread times its own packets with no lock; the queue is
//! [`crate::muxing::AudioQueue`], which refuses at its share of the capacity
//! rather than blocking; and the conversion to the container's PCM happens on
//! the writer thread, not here.
//!
//! The threads are started **after** the first video frame and joined **before**
//! the container's trailer is written, which is what makes both of those safe:
//! there is no clock to place a packet on before the first, and the writer's
//! queue closes when the last handle to it is dropped after the second.
//!
//! # What this module does not decide
//!
//! What goes into a compatibility mix
//! ([issue #29](https://github.com/wildware-uk/clipped/issues/29)), or what to
//! do about a drift it has measured
//! ([issue #30](https://github.com/wildware-uk/clipped/issues/30)). It opens the
//! sources this build can capture, places what they produce where their own
//! hardware said it happened, and reports how far the result moved.
//!
//! # Which sources those are
//!
//! A recording of a window scopes its system audio to that window's process
//! tree, twice: once for the tree and once for everything except it, which is
//! [`AudioSource::Game`] and [`AudioSource::OtherSystemAudio`] (issues
//! [#26](https://github.com/wildware-uk/clipped/issues/26) and
//! [#27](https://github.com/wildware-uk/clipped/issues/27), SPEC.md section 11).
//! A recording with no process to scope to — a monitor, or a window whose
//! process has gone — records the whole endpoint on one track instead.
//! [`plan_system_audio`] is that decision, and it is separate from opening
//! anything so that it can be tested without a machine.
//!
//! The two scoped captures are opened by **one** call to
//! `ProcessLoopbackCapture::open_pair`, so that they share one cell naming the
//! process both are scoped to and cannot end up on different survivors of a
//! game whose launcher exited partway through. Opening them separately was a
//! defect that produced no error and no log line, only a track with the game
//! twice on it or not at all
//! ([issue #581](https://github.com/wildware-uk/clipped/issues/581)).
//!
//! Routing a *named* application to a track of its own is still
//! [issue #33](https://github.com/wildware-uk/clipped/issues/33).

mod placement;

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

#[cfg(test)]
use clipped_audio::windows::ScopeAgreement;
use clipped_audio::{AudioError, AudioFormat, Capture};
use clipped_capture::{
    CaptureClock, DriftEstimator, SyncState, SyncTolerance, DEFAULT_DISCONTINUITY_STEP,
};
use clipped_muxer::{AudioSource, AudioTrack, RecordingLayout, TrackId, VideoTrack};

use crate::error::SessionError;
use clipped_replay::ReplayBuffer;

use crate::muxing::{AudioQueue, AudioQueued, MuxingThread};
use crate::report::{AudioSyncReport, AudioTrackReport};
use crate::settings::{AudioSourceSetting, RecordingSettings};

use placement::{place, AUDIO_CLOCK};

/// How long one read waits for a buffer.
///
/// Not a period: it is how long an audio thread can be inside `read` and
/// therefore how long a stop request waits to be noticed. A tenth of a second
/// matches the capture loop's own acquisition timeout, is far longer than the
/// 10 ms Windows delivers loopback at — so an idle endpoint does not spin — and
/// is short enough that stopping a recording is not perceptibly delayed by it.
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// One audio capture, whichever kind of endpoint is behind it.
///
/// `clipped_audio::windows::SystemAudioCapture` and `MicrophoneCapture` present
/// the same four operations and differ only in what they are pointed at, so the
/// thread body below is written once against this rather than twice against
/// them (AGENTS.md section 55). It is also what lets that body — the placement,
/// the drift measurement, the queueing and the report — be tested on a machine
/// with no sound card, which is every machine this workspace's CI runs on.
pub(crate) trait AudioCapture: Send {
    /// Reads the next block of audio, waiting up to `timeout` for one.
    ///
    /// # Errors
    ///
    /// Whatever the capture reports. A device disappearing is not one of these:
    /// it is handled inside `clipped-audio` and arrives as synthesised silence.
    fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError>;

    /// Stops capturing and releases the device.
    ///
    /// Deliberately not a *drain*. `clipped-audio`'s engine holds up to 200 ms
    /// that closing throws away, and only `ProcessLoopbackCapture` exposes the
    /// call that hands it over first — so a recording's tracks end up to that
    /// much shorter than its picture.
    /// [Issue #320](https://github.com/wildware-uk/clipped/issues/320) is
    /// exposing it on the two captures a session opens; this trait grows a
    /// `finish` when it exists.
    fn close(&mut self);

    /// Which agreement this capture scopes itself through, for the
    /// process-scoped captures that have one.
    ///
    /// [`None`] for the endpoint and microphone captures, which are pointed at
    /// a device rather than at a process tree and have nothing to agree about.
    /// The two sides of one `ProcessLoopbackCapture::open_pair` return the same
    /// one, and that is the only thing that distinguishes a recording which
    /// opened the pair from one which opened `open` and `open_excluding`
    /// separately — the arrangement that made issue #27's fix ineffective
    /// everywhere a user could reach it
    /// ([issue #581](https://github.com/wildware-uk/clipped/issues/581)).
    ///
    /// Not shipped (AGENTS.md section 37). Nothing a recording *does* depends
    /// on knowing which agreement a capture follows — the captures themselves
    /// use it, on their own threads — but everything depends on there being one
    /// shared between the two of them, and this is the only way to ask from
    /// out here.
    #[cfg(test)]
    fn scope_agreement(&self) -> Option<ScopeAgreement> {
        None
    }
}

impl AudioCapture for clipped_audio::windows::SystemAudioCapture {
    fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
        Self::read(self, timeout)
    }

    fn close(&mut self) {
        Self::close(self);
    }
}

impl AudioCapture for clipped_audio::windows::MicrophoneCapture {
    fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
        Self::read(self, timeout)
    }

    fn close(&mut self) {
        Self::close(self);
    }
}

impl AudioCapture for clipped_audio::windows::ProcessLoopbackCapture {
    fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
        Self::read(self, timeout)
    }

    /// Hands over what the engine is still holding, then releases the device.
    ///
    /// The one capture that can. `finish` drains the buffered audio that a bare
    /// `close` throws away — up to 200 ms of it — so a game track and an
    /// other-system-audio track end where the recording ends rather than a
    /// fifth of a second early. The trait's contract is only that `close`
    /// releases the device, and this satisfies it by doing more; the two
    /// endpoint captures cannot yet, which is
    /// [issue #320](https://github.com/wildware-uk/clipped/issues/320).
    fn close(&mut self) {
        Self::finish(self);
        Self::close(self);
    }

    #[cfg(test)]
    fn scope_agreement(&self) -> Option<ScopeAgreement> {
        Some(Self::scope_agreement(self))
    }
}

/// One source a recording will record, opened and ready to be read.
pub(crate) struct OpenSource {
    /// Which of the recording's tracks this feeds.
    source: AudioSource,
    /// The device Windows is capturing, as it names it. [`None`] while there is
    /// no device, which is a state the capture survives rather than a failure.
    device: Option<String>,
    format: AudioFormat,
    capture: Box<dyn AudioCapture>,
}

impl core::fmt::Debug for OpenSource {
    /// Describes the source without reaching into the capture, which holds COM
    /// interfaces and, for a microphone, somebody's room.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OpenSource")
            .field("source", &self.source.track_name())
            .field("device", &self.device)
            .field("format", &self.format)
            .finish()
    }
}

/// Opens every audio source `settings` asked for.
///
/// Nothing is opened for a source set to [`AudioSourceSetting::Off`]: no device,
/// no thread and no track. That is what makes `--microphone none
/// --system-audio none` a video-only recording rather than a recording with two
/// empty tracks in it.
///
/// # Errors
///
/// [`SessionError::Audio`] when a source the caller asked for cannot be opened —
/// no output device, no microphone, the chosen microphone unplugged, a format
/// this build cannot convert. Refused rather than skipped: somebody who asked
/// for a microphone track and would get a file without one should be told before
/// the recording is made, not after (AGENTS.md section 54).
///
/// [`SessionError::AudioDeviceNotSelectable`] for a named system-audio device,
/// which this build cannot honour.
pub(crate) fn open(settings: &RecordingSettings) -> Result<Vec<OpenSource>, SessionError> {
    use clipped_audio::windows::{MicrophoneCapture, ProcessLoopbackCapture, SystemAudioCapture};

    let mut sources = Vec::new();

    for planned in plan_system_audio(settings.system_audio(), settings.target().game_process())? {
        match planned {
            PlannedSource::WholeEndpoint => {
                let capture = SystemAudioCapture::open().map_err(|source| SessionError::Audio {
                    track: AudioSource::OtherSystemAudio.track_name(),
                    source,
                })?;
                sources.push(OpenSource {
                    source: AudioSource::OtherSystemAudio,
                    device: capture.endpoint_name().map(str::to_owned),
                    format: capture.format(),
                    capture: Box::new(capture),
                });
            }
            PlannedSource::ScopedPair(root) => {
                // One call, and one agreement between the two captures it
                // returns. `open` and `open_excluding` are the same two
                // activations without it, and a recording that made them
                // separately would put a process that is in one side's tree and
                // not the other's onto both tracks or onto neither, whenever a
                // game's launcher exits partway through (issues #27 and #581).
                let (game, other) = ProcessLoopbackCapture::open_pair(root).map_err(|source| {
                    SessionError::Audio {
                        // Named for the track a user would notice missing.
                        // Either side failing means neither capture exists,
                        // so there is one failure here rather than two.
                        track: AudioSource::Game.track_name(),
                        source,
                    }
                })?;
                sources.push(OpenSource {
                    source: AudioSource::Game,
                    // A process-scoped capture is not opened against an endpoint
                    // the user chose, so there is no device name to report. The
                    // field says which device somebody would go and unplug, and
                    // for this source the answer is "none of them".
                    device: None,
                    format: game.format(),
                    capture: Box::new(game),
                });
                sources.push(OpenSource {
                    source: AudioSource::OtherSystemAudio,
                    device: None,
                    format: other.format(),
                    capture: Box::new(other),
                });
            }
        }
    }

    if let Some(selection) = selected_microphone(settings.microphone())? {
        let capture =
            MicrophoneCapture::open(&selection).map_err(|source| SessionError::Audio {
                track: AudioSource::Microphone.track_name(),
                source,
            })?;
        sources.push(OpenSource {
            source: AudioSource::Microphone,
            device: capture.device_name().map(str::to_owned),
            format: capture.format(),
            capture: Box::new(capture),
        });
    }

    Ok(sources)
}

/// One system-audio capture a recording will open.
///
/// The microphone is not here: it is one device whichever way the system side is
/// scoped, and issues #26 and #27 did not change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlannedSource {
    /// Loopback of the endpoint Windows is playing through — everything the
    /// machine played, filed as [`AudioSource::OtherSystemAudio`].
    ///
    /// Not [`AudioSource::Game`]: calling the whole machine's output the game's
    /// track would be a label a downstream editor acts on.
    WholeEndpoint,
    /// Both sides of one process tree: the tree itself as
    /// [`AudioSource::Game`], and everything the machine played except it as
    /// [`AudioSource::OtherSystemAudio`].
    ///
    /// **One variant rather than two**, because the two sides are not two
    /// decisions. They are opened by one call to
    /// `ProcessLoopbackCapture::open_pair` and they share one cell naming the
    /// process both are scoped to, which is what keeps them on the same tree
    /// after a game's launcher exits (issue #27). A plan that could name one
    /// side without the other is the shape that let a recording open them
    /// separately for a week while every document said it did not
    /// ([issue #581](https://github.com/wildware-uk/clipped/issues/581)), so
    /// the type no longer has that shape.
    ScopedPair(u32),
}

/// Which system-audio captures to open, decided before anything is opened.
///
/// The pair is the whole point. Windows' process loopback has an include mode
/// and an exclude mode against the same process tree
/// (`clipped_audio::windows::ProcessLoopbackCapture::open_pair`, issues
/// [#26](https://github.com/wildware-uk/clipped/issues/26) and
/// [#27](https://github.com/wildware-uk/clipped/issues/27)), and a recording has
/// to open **both or neither**: one alone leaves either the game or everything
/// else unrecorded, and opening a scoped capture beside a whole-endpoint one
/// would put the game's audio on two tracks — which is the failure SPEC.md
/// section 11 exists to prevent, and the one a user only discovers in an editor
/// when muting the game does not silence it.
///
/// It says which captures, not how many entries: [`PlannedSource::ScopedPair`]
/// is one plan item that opens two captures, because they are opened together
/// or not at all.
///
/// A recording with no process to scope to — a monitor capture, or a window
/// whose process has already gone — gets the unscoped endpoint instead. That is
/// a worse recording rather than a failed one: the tracks are still separate
/// from the microphone, and SPEC.md section 45's walkthrough is about a game
/// window.
///
/// # Errors
///
/// [`SessionError::AudioDeviceNotSelectable`] for a named system-audio device.
/// Refused rather than quietly recording the default endpoint, which is what a
/// control that silently does something else looks like (AGENTS.md section 27);
/// `clipped-audio` opens loopback against the endpoint Windows is *playing
/// through* and offers no way to name another, and issue #316 is adding one.
pub(crate) fn plan_system_audio(
    setting: &AudioSourceSetting,
    game_process: Option<u32>,
) -> Result<Vec<PlannedSource>, SessionError> {
    match (setting, game_process) {
        (AudioSourceSetting::Off, _) => Ok(Vec::new()),
        (AudioSourceSetting::Named(_), _) => Err(SessionError::AudioDeviceNotSelectable),
        (AudioSourceSetting::SystemDefault, Some(root)) => {
            Ok(vec![PlannedSource::ScopedPair(root)])
        }
        (AudioSourceSetting::SystemDefault, None) => Ok(vec![PlannedSource::WholeEndpoint]),
    }
}

/// One microphone this machine has, as something to choose from.
///
/// The two things a settings screen needs about a device it is offering
/// ([issue #51](https://github.com/wildware-uk/clipped/issues/51)): what it is
/// called, and whether it is the one `default` currently resolves to. The name
/// is the one [`chosen_microphone`] matches a configured name against, so a
/// device picked from this list is a device that recording will find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrophoneChoice {
    /// The name Windows gives the endpoint.
    pub name: String,
    /// Whether Windows currently considers it the default capture endpoint.
    pub is_default: bool,
}

/// Every microphone this machine has, in the order Windows lists them.
///
/// The same enumeration a recording resolves a configured name against
/// ([`chosen_microphone`]), so that what a settings screen offers and what a
/// recording can open are one list rather than two (AGENTS.md section 55).
///
/// # Errors
///
/// [`SessionError::Audio`] when the endpoints cannot be enumerated at all —
/// which a caller shows as the reason there is no list, rather than as an empty
/// one (AGENTS.md section 27).
pub fn available_microphones() -> Result<Vec<MicrophoneChoice>, SessionError> {
    let devices = clipped_audio::windows::microphones().map_err(|source| SessionError::Audio {
        track: AudioSource::Microphone.track_name(),
        source,
    })?;

    Ok(devices
        .into_iter()
        .map(|device| MicrophoneChoice {
            name: device.name().to_owned(),
            is_default: device.is_default(),
        })
        .collect())
}

/// The endpoint a microphone setting names, resolved against this machine.
///
/// [`None`] for [`AudioSourceSetting::Off`], which is not a device that could
/// not be found: it is the absence of a microphone track, and the caller opens
/// nothing.
///
/// **This is the one place a microphone setting becomes a device**, and both
/// callers go through it: the recording that is about to be made
/// ([`open`]) and the level check a settings screen runs before saving one
/// ([`microphone_level`]). A second resolution would be a meter that listened to
/// one endpoint while the recording opened another, which is a worse lie than no
/// meter at all (AGENTS.md sections 27 and 55).
///
/// # Errors
///
/// [`SessionError::MicrophoneNotFound`] when a named device matches nothing on
/// this machine, or more than one, and [`SessionError::Audio`] when the
/// endpoints cannot be enumerated at all.
fn selected_microphone(
    setting: &AudioSourceSetting,
) -> Result<Option<clipped_audio::windows::MicrophoneSelection>, SessionError> {
    use clipped_audio::windows::MicrophoneSelection;

    match setting {
        AudioSourceSetting::Off => Ok(None),
        AudioSourceSetting::Named(name) => chosen_microphone(name).map(Some),
        AudioSourceSetting::SystemDefault => Ok(Some(MicrophoneSelection::SystemDefault)),
    }
}

/// What a microphone is hearing at this moment.
///
/// The answer to the one question a person choosing a microphone is actually
/// asking — "is this the one that can hear me?" — which no list of device names
/// can answer. A settings screen asks repeatedly and draws the result as a
/// meter.
#[derive(Debug, Clone, PartialEq)]
pub struct MicrophoneLevel {
    /// The endpoint that was listened to, as Windows names it.
    ///
    /// [`None`] while the device is unplugged or disabled, during which a
    /// capture produces silence rather than failing — so this is the difference
    /// between "nobody is speaking" and "there is nothing there".
    pub device: Option<String>,
    /// The loudest sample heard, from `0.0` to `1.0`.
    ///
    /// A linear amplitude rather than decibels, because it is a measurement and
    /// not a presentation: how a meter scales it is the drawing side's, and
    /// converting here would mean two places deciding what `-60 dB` looks like.
    /// Clamped at `1.0`: a float endpoint can deliver samples above full scale,
    /// and "louder than the loudest" is not a thing a meter can draw.
    pub peak: f32,
    /// Whether Windows reports the microphone muted.
    ///
    /// [`None`] when Windows will not report the switch for this device — some
    /// virtual devices have none. A muted microphone reads as silence, so this
    /// is what stops a flat meter being reported as "say something louder" when
    /// the answer is a switch.
    pub muted: Option<bool>,
}

/// Listens to the microphone `setting` names for `listen`, and reports the
/// loudest thing it heard.
///
/// # Why it opens and closes the device every time
///
/// Because the alternative is a capture nobody owns. A monitor held open
/// between calls has to be closed by something, and the only thing that knows
/// the user has finished choosing is a window that may have been killed — which
/// would leave Windows showing the microphone-in-use indicator for the life of
/// the recorder (AGENTS.md section 58). Opening per call makes the cleanup
/// deterministic, and a settings screen polling a few times a second is not a
/// load worth carrying that risk for.
///
/// The consequence is stated rather than hidden: [`MicrophoneLevel::peak`] is
/// the loudest sample in the slice that was listened to, not since the last
/// call. Between two calls there is a gap that nothing measured.
///
/// # Privacy
///
/// One number leaves this function. The samples are read, reduced to a maximum
/// and dropped, and nothing here logs a buffer — the guarantee
/// `docs/audio-routing.md` makes about microphone audio never reaching a log
/// (AGENTS.md section 13) applies with more force to a function whose whole
/// purpose is to listen to somebody's room.
///
/// # Errors
///
/// [`SessionError::AudioDeviceNotSelectable`] is *not* one of them; a setting of
/// [`AudioSourceSetting::Off`] is refused by the caller rather than here, since
/// "record no microphone" is a valid setting and simply has no level.
///
/// [`SessionError::MicrophoneNotFound`] when the named device is not on this
/// machine, and [`SessionError::Audio`] when the device cannot be opened —
/// unplugged, in use, or presenting a format this build will not convert. A
/// caller shows the reason rather than a meter reading zero, because those are
/// opposite answers (AGENTS.md section 27).
///
/// # Panics
///
/// Never. `listen` of zero opens the device and reports what one read of it
/// gives, which is the honest answer to being asked for no time at all.
pub fn microphone_level(
    setting: &AudioSourceSetting,
    listen: Duration,
) -> Result<Option<MicrophoneLevel>, SessionError> {
    use clipped_audio::windows::MicrophoneCapture;

    let Some(selection) = selected_microphone(setting)? else {
        return Ok(None);
    };

    let mut capture =
        MicrophoneCapture::open(&selection).map_err(|source| SessionError::Audio {
            track: AudioSource::Microphone.track_name(),
            source,
        })?;

    let deadline = Instant::now() + listen;
    let mut peak = 0.0_f32;
    loop {
        // The first read gets the whole window: a capture that has just started
        // has nothing buffered, and a timeout of what is left after the first
        // read would shrink to nothing before the endpoint's first packet
        // arrived.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match capture.read(remaining) {
            // Silence the crate synthesised for a device that produced nothing
            // is a peak of zero, which is what it sounds like. `device` below
            // is what says the device was not there.
            Ok(Capture::Samples(buffer)) => peak = peak.max(loudest(buffer.samples())),
            Ok(Capture::Idle | Capture::FormatChanged(_)) => {}
            Err(source) => {
                return Err(SessionError::Audio {
                    track: AudioSource::Microphone.track_name(),
                    source,
                })
            }
        }
    }

    let level = MicrophoneLevel {
        device: capture.device_name().map(str::to_owned),
        peak,
        muted: capture.is_muted(),
    };

    // Before the borrow ends rather than at drop, so that the microphone-in-use
    // indicator goes away when this returns and not whenever the value happens
    // to be dropped.
    capture.close();
    Ok(Some(level))
}

/// The loudest of `samples`, as a positive amplitude no greater than `1.0`.
///
/// Non-finite samples are ignored rather than propagated: a `NaN` from a driver
/// would otherwise become the peak and stay it, since `NaN` compares greater
/// than nothing and `f32::max` prefers the number — a meter pinned at the top
/// for the rest of the session with no way back down.
fn loudest(samples: &[f32]) -> f32 {
    samples
        .iter()
        .filter(|sample| sample.is_finite())
        .fold(0.0_f32, |loudest, sample| loudest.max(sample.abs()))
        .min(1.0)
}

/// The microphone whose name contains `wanted`.
///
/// # Errors
///
/// [`SessionError::MicrophoneNotFound`] when nothing matches and when more than
/// one does. An ambiguous name is refused rather than resolved to the first
/// match, for the reason `clipped-windows` refuses an ambiguous window title:
/// picking one silently is how a recording ends up made of the wrong thing.
fn chosen_microphone(
    wanted: &str,
) -> Result<clipped_audio::windows::MicrophoneSelection, SessionError> {
    let devices = clipped_audio::windows::microphones().map_err(|source| SessionError::Audio {
        track: AudioSource::Microphone.track_name(),
        source,
    })?;

    let wanted_folded = wanted.to_lowercase();
    let matches: Vec<_> = devices
        .iter()
        .filter(|device| device.name().to_lowercase().contains(&wanted_folded))
        .collect();

    match matches.as_slice() {
        [device] => Ok(device.select()),
        _ => Err(SessionError::MicrophoneNotFound {
            matched: matches.len(),
            available: devices.len(),
        }),
    }
}

/// Adds an audio track for every open source to `video`'s layout.
///
/// The order and the names come from `clipped-muxer`'s track model rather than
/// from here, so two recordings made with the same sources have their tracks in
/// the same places whatever order they were opened in.
///
/// The **default-track flag** is this crate's decision and is set on the first
/// track the model orders, because it is the one a player that takes a single
/// audio track should take. `AudioTrack::for_source` gives the flag to the
/// compatibility mix and to nothing else (SPEC.md section 13), and this build
/// makes no compatibility mix
/// ([issue #29](https://github.com/wildware-uk/clipped/issues/29)) — so without
/// this a recording with a system track and a microphone track would leave a
/// player to guess, and a player that guessed the microphone is a recording
/// somebody concludes is broken.
pub(crate) fn declare(
    video: VideoTrack,
    sources: &[OpenSource],
    compatibility_mix: bool,
) -> RecordingLayout {
    let mut ordered: Vec<&OpenSource> = sources.iter().collect();
    ordered.sort_by_key(|open| open.source.ordering_rank());

    let mut layout = RecordingLayout::new(video);

    // First, and carrying the default flag, because that is the whole point of
    // it: a player that takes one track arbitrarily has to get the one that
    // sounds like the recording (SPEC.md section 13,
    // [issue #29](https://github.com/wildware-uk/clipped/issues/29)).
    //
    // Its shape is the first ordered source's. The mixer refuses a source whose
    // sampling rate differs from the mix's and channel counts are placed rather
    // than resampled, so taking the rate from the highest-ranked source is the
    // choice that includes the game's audio — which is the one somebody would
    // most notice missing. `crate::muxing` says what happens to a source whose
    // rate does not match.
    let mix_format = compatibility_mix
        .then(|| ordered.first().map(|open| open.format))
        .flatten();
    if let Some(format) = mix_format {
        layout = layout.with_audio_track(AudioTrack::for_source(
            AudioSource::CompatibilityMix,
            format.sample_rate().get(),
            format.channels().get(),
        ));
    }

    for (position, open) in ordered.into_iter().enumerate() {
        layout = layout.with_audio_track(
            AudioTrack::for_source(
                open.source.clone(),
                open.format.sample_rate().get(),
                open.format.channels().get(),
            )
            // `for_source` gives the flag to the compatibility mix and to
            // nothing else, so this only has to stand in when there is no mix.
            .with_default_flag(mix_format.is_none() && position == 0),
        );
    }
    layout
}

/// The threads reading a recording's audio sources.
///
/// Started once the recording has an epoch and stopped by
/// [`finish`](Self::finish), which is also what [`Drop`] does — so a panic in
/// the capture loop still releases the microphone and still lets the writer's
/// queue close (AGENTS.md sections 17 and 58).
#[derive(Debug)]
pub(crate) struct AudioThreads {
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<AudioTrackReport>>,
}

impl AudioThreads {
    /// Starts one thread per source, timing every packet against `clock`.
    ///
    /// A source the layout has no track for is dropped with a message rather
    /// than started, which cannot happen — the layout was built from these same
    /// sources — and is handled because the alternative is a silent track.
    pub(crate) fn start(
        sources: Vec<OpenSource>,
        layout: &RecordingLayout,
        clock: CaptureClock,
        muxing: &MuxingThread,
        replay: Option<&Arc<ReplayBuffer>>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::with_capacity(sources.len());

        for open in sources {
            let Some(track) = layout.audio_track_for(&open.source) else {
                tracing::error!(
                    audio_track = open.source.track_name(),
                    "an opened audio source has no track in the recording and was not \
                     recorded; please report this"
                );
                continue;
            };

            let queue = muxing.audio_queue();
            let buffered = replay.map(Arc::clone);
            let stopping = Arc::clone(&stop);
            let name = format!("clipped-audio-{}", track_thread_name(&open.source));
            match thread::Builder::new()
                .name(name)
                .spawn(move || pump(open, track, clock, &queue, buffered.as_deref(), &stopping))
            {
                Ok(handle) => workers.push(handle),
                // A recording without one of its audio tracks is worth far more
                // than no recording (AGENTS.md section 17), so a machine that
                // cannot start this thread loses the track and says so.
                Err(error) => tracing::error!(
                    %error,
                    "a thread could not be started to record one of the audio sources; the \
                     recording will have that track declared and empty"
                ),
            }
        }

        Self { stop, workers }
    }

    /// Stops every thread, waits for them, and reports what each source
    /// produced.
    ///
    /// Idempotent: a second call has no threads left to join and reports
    /// nothing. That is what makes [`Drop`] safe after an explicit call.
    pub(crate) fn finish(&mut self) -> Vec<AudioTrackReport> {
        self.stop.store(true, Ordering::Relaxed);
        self.workers
            .drain(..)
            .filter_map(|worker| match worker.join() {
                Ok(report) => Some(report),
                Err(_) => {
                    tracing::error!(
                        "a thread recording one of the audio sources panicked; that track ends \
                         where it stopped and the rest of the recording is unaffected"
                    );
                    None
                }
            })
            .collect()
    }
}

impl Drop for AudioThreads {
    fn drop(&mut self) {
        let reports = self.finish();
        for report in reports {
            // Nowhere to return them from here. The path this exists for is a
            // panic in the capture loop, where the alternative is a microphone
            // Windows still shows as in use and a writer queue that never
            // closes.
            tracing::info!(
                audio_track = report.track_name(),
                frames = report.frames(),
                "an audio source was stopped while the recording was being dropped"
            );
        }
    }
}

/// A short, stable word for a source, for the name of the thread reading it.
fn track_thread_name(source: &AudioSource) -> &'static str {
    match source {
        AudioSource::CompatibilityMix => "mix",
        AudioSource::Game => "game",
        AudioSource::OtherSystemAudio => "system",
        AudioSource::Microphone => "microphone",
        AudioSource::VoiceChat => "voice",
        _ => "application",
    }
}

/// One source's thread: read, place, queue, until it is asked to stop.
///
/// Everything this does per buffer is bounded and allocation-free apart from
/// the copy into the queue: the placement is integer arithmetic, the drift
/// measurement is five accumulators (`clipped_capture::DriftEstimator`), and the
/// queue refuses rather than waits. Nothing here touches the filesystem, a lock
/// or the muxer (AGENTS.md section 20).
fn pump(
    mut open: OpenSource,
    track: TrackId,
    clock: CaptureClock,
    queue: &AudioQueue,
    replay: Option<&ReplayBuffer>,
    stop: &AtomicBool,
) -> AudioTrackReport {
    let format = open.format;
    let channels = format.channels().get();
    let sample_rate = format.sample_rate().get();

    let mut report = AudioTrackReport::new(
        open.source.track_name().to_owned(),
        sample_rate,
        channels,
        open.device.clone(),
    );
    let mut drift = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);

    tracing::info!(
        audio_track = open.source.track_name(),
        %track,
        device = open.device.as_deref().unwrap_or("<none>"),
        sample_rate,
        channels,
        "recording an audio source"
    );

    while !stop.load(Ordering::Relaxed) {
        let audio = match open.capture.read(READ_TIMEOUT) {
            Ok(Capture::Samples(audio)) => audio,
            Ok(Capture::Idle) => continue,
            Ok(Capture::FormatChanged(format)) => {
                // Once. `clipped-audio` keeps the timeline running as silence
                // from here, so the track stays the length of the recording and
                // the container's declared format stays true — it is the sound
                // that is missing, not the structure.
                report.note_format_change();
                tracing::warn!(
                    audio_track = open.source.track_name(),
                    %format,
                    "the device behind this audio track was replaced by one of a different \
                     shape, which this build cannot follow inside one file; the rest of the \
                     track is silence"
                );
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    audio_track = open.source.track_name(),
                    %error,
                    "an audio source stopped; the rest of the recording has no sound on that \
                     track"
                );
                break;
            }
        };

        report.note_buffer(audio.frames() as u64, audio.origin());

        // The measurement, before the placement and independent of it: two
        // accounts of the same moment — where the endpoint said this buffer
        // belongs, and where the track this crate is building puts it. Their
        // difference is how far the audio has moved against the reference
        // clock, and its slope is the drift (`docs/av-sync.md`). Only endpoint
        // buffers have both; synthesised silence covers a period the device
        // never described.
        if let Some(device) = audio.device_timestamp() {
            if let (Ok(reference), Ok(observed)) = (
                clock.media_time_on(AUDIO_CLOCK, device.as_nanos()),
                clock.media_time_on(AUDIO_CLOCK, audio.timestamp().as_nanos()),
            ) {
                drift.observe(reference, observed);
            }
        }

        let samples = audio.samples();
        let placed = match place(
            clock,
            audio.timestamp().as_nanos(),
            samples.len(),
            channels,
            sample_rate,
        ) {
            Ok(Some(placed)) => placed,
            Ok(None) => {
                // Entirely before the first video frame. Ordinary for the first
                // buffer or two — the endpoint was opened while the capture
                // backend was still initialising — and counted rather than
                // silently discarded.
                report.note_trimmed(audio.frames() as u64);
                continue;
            }
            Err(mismatch) => {
                tracing::warn!(
                    audio_track = open.source.track_name(),
                    %mismatch,
                    "this recording is not timed against the clock the audio device reports \
                     on, so its samples cannot be placed; the track ends here"
                );
                break;
            }
        };
        report.note_trimmed((placed.samples_trimmed / usize::from(channels.max(1))) as u64);

        let placed_samples = &samples[placed.samples_trimmed..];

        // The buffer gets the same samples the file does, at the same instant,
        // taken from the same placement — so a clip's audio and the recording's
        // agree by construction rather than by two pieces of arithmetic that
        // have to be kept in step. It is copied into memory the buffer already
        // owns and cannot fail a recording (AGENTS.md section 17): audio that
        // arrives before the first keyframe has nothing to sit beside and is
        // counted there rather than reported here.
        if let Some(buffer) = replay {
            let at = Duration::from_nanos(placed.at.as_nanos().max(0).unsigned_abs());
            let _ = buffer.push_audio(track, at, placed_samples);
        }

        match queue.write(track, placed.at, placed_samples) {
            AudioQueued::Written => {}
            AudioQueued::DroppedWriterBehind => report.note_dropped(),
            AudioQueued::WriterLost => {
                // The file is no longer being written — a full disk, a drive
                // unplugged, or the recording ending. The capture loop is what
                // reports why; there is nothing for this thread to do but stop.
                break;
            }
        }
    }

    open.capture.close();
    report.with_sync(sync_report(&drift));
    report_what_the_source_did(&report, &open.source);
    report
}

/// Says what one source produced, once, when its thread stops.
///
/// The empty-track line is at `warn` and is the point of the whole function: an
/// empty audio track is the visible symptom of a microphone Windows had muted or
/// a device that never opened, and the session is the only layer that knows
/// enough to say so (AGENTS.md section 45). The rest is the measurement
/// `docs/av-sync.md` asks to be reported rather than assumed.
fn report_what_the_source_did(report: &AudioTrackReport, source: &AudioSource) {
    if report.frames() == 0 {
        tracing::warn!(
            audio_track = source.track_name(),
            device = report.device().unwrap_or("<none>"),
            "this audio source produced nothing at all, so its track in the recording is \
             empty. A microphone muted in Windows is the commonest reason"
        );
    }

    let sync = report.sync();
    tracing::info!(
        audio_track = source.track_name(),
        device = report.device().unwrap_or("<none>"),
        buffers = report.buffers(),
        frames = report.frames(),
        synthesised_silence_frames = report.synthesised_silence_frames(),
        frames_before_the_recording = report.frames_before_the_recording(),
        buffers_dropped_writer_behind = report.buffers_dropped_writer_behind(),
        format_changes = report.format_changes(),
        sync_observations = sync.map_or(0, |sync| sync.observations()),
        sync_discontinuities = sync.map_or(0, |sync| sync.discontinuities()),
        sync_first_offset_us = sync.map_or(0, |sync| sync.first_offset_nanos() / 1_000),
        sync_latest_offset_us = sync.map_or(0, |sync| sync.latest_offset_nanos() / 1_000),
        sync_peak_offset_us = sync.map_or(0, |sync| sync.peak_offset_nanos() / 1_000),
        sync_drift_ppb = sync
            .and_then(|sync| sync.drift_parts_per_billion())
            .unwrap_or(0),
        sync_state = %sync.map_or(SyncState::InTolerance, |sync| sync.state()),
        "an audio source finished"
    );

    if let Some(sync) = sync {
        if sync.state() != SyncState::InTolerance {
            // The reportable limit of `docs/av-sync.md`, reached. A recording
            // that gets here has a fault in it rather than bad luck:
            // `clipped-audio` holds its own timeline to within 20 ms of the
            // reference clock by construction, which is two to three times the
            // headroom.
            tracing::warn!(
                audio_track = source.track_name(),
                state = %sync.state(),
                offset_ms = sync.latest_offset_nanos() / 1_000_000,
                tolerance = %SyncTolerance::default(),
                "this audio track ended outside the synchronisation tolerance a recording is \
                 held to; please report it with the log above"
            );
        }
    }
}

/// What the drift estimator measured, in the form a report carries.
///
/// [`None`] when nothing was observed at all, which is a recording whose
/// endpoint produced only synthesised silence — there is no measurement to
/// quote, and quoting a zero would be inventing one (AGENTS.md section 54).
fn sync_report(drift: &DriftEstimator) -> Option<AudioSyncReport> {
    let (first, latest) = (drift.first()?, drift.latest()?);
    Some(AudioSyncReport {
        first_offset_nanos: first,
        latest_offset_nanos: latest,
        peak_offset_nanos: drift.peak(),
        observations: drift.observations(),
        discontinuities: drift.discontinuities(),
        // Parts per *billion*, as an integer, so that the report stays
        // comparable: the rates worth reporting are single-digit parts per
        // million and this keeps three more digits of them without putting a
        // float in a value two recordings are compared by.
        drift_parts_per_billion: drift
            .rate()
            .map(|rate| (rate.as_ratio() * 1e9).round() as i64),
        state: drift
            .state(&SyncTolerance::default())
            .unwrap_or(SyncState::InTolerance),
    })
}

#[cfg(test)]
mod tests;
