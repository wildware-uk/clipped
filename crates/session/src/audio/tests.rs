//! What the wiring in this module does, checked against a real file.
//!
//! Nothing here needs an audio device, a GPU or a desktop session. The captures
//! are scripted through [`AudioCapture`], which is the reason that trait exists;
//! the writer is a real [`MkvWriter`] over a temporary file, driven by the real
//! [`MuxingThread`] and the real [`AudioThreads`]; and what comes out is
//! inspected with `ffprobe` rather than taken on trust (AGENTS.md section 22).
//!
//! The tones are the ones AGENTS.md section 26 names — 880 Hz for other system
//! audio, 1320 Hz for the microphone — so that "the sound on this track is the
//! sound that went into it, and none of the other track's" is a measurement
//! rather than an assumption. A session that routed both sources to one track
//! passes every structural assertion in this file.
//!
//! # Where the video comes from
//!
//! A recording needs a video track for its audio to be *synchronised against* —
//! a declared but empty track has no start time, so there would be nothing to
//! compare it with — and it has to be real coded video rather than identifiable
//! rubbish, because `ffprobe` runs the H.264 parser over what it reads and bytes
//! that are not access units are merged and discarded before any measurement is
//! made of them. That is not a guess: a first version of this file wrote 120
//! synthetic packets and `ffprobe` reported the track as holding one.
//!
//! Encoding real pictures with `clipped-encoder` would open a graphics device,
//! which the shared-machine test discipline forbids a test that runs by default
//! from doing. So the fixture is made the way `crates/replay/tests/support`
//! makes its own: `ffmpeg` encodes a moving test pattern to an Annex B
//! elementary stream and `ffprobe` reports where each access unit begins and
//! ends, which is a demuxer deciding the packet boundaries rather than this file
//! guessing at NAL parsing. It is encoded once per test binary and shared.

use core::num::{NonZeroU16, NonZeroU32};
use core::sync::atomic::AtomicBool;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use clipped_audio::{AudioTimestamp, CapturedAudio, ChannelMask, SampleFormat, SampleOrigin};
use clipped_capture::{CaptureTimestamp, MediaTime, SourceClock, SyncState};
use clipped_media_validation::{
    require_media_tools, AudioContent, AudioStream, Media, TemporaryDirectory, Tone, VideoStream,
};
use clipped_muxer::{MkvWriter, VideoCodec, VideoTrack};

use super::*;
use crate::muxing::SpaceGuard;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const SAMPLE_RATE: u32 = 48_000;
/// Frames in one buffer: the 10 ms period Windows delivers loopback at.
const BUFFER_FRAMES: usize = SAMPLE_RATE as usize / 100;
/// The counter reading a real recording's epoch is: a machine up for a year.
/// Nothing here may assume the epoch is small.
const EPOCH: u64 = 31_107_000 * 1_000_000_000;

/// The game's own audio, in AGENTS.md section 26's vocabulary.
const GAME_TONE: f64 = 440.0;
const SYSTEM_TONE: f64 = 880.0;
const MICROPHONE_TONE: f64 = 1320.0;

fn format(channels: u16) -> AudioFormat {
    AudioFormat::new(
        NonZeroU32::new(SAMPLE_RATE).expect("48 kHz is not zero"),
        NonZeroU16::new(channels).expect("a track has channels"),
        ChannelMask::from_bits(if channels == 1 { 0x4 } else { 0x3 }),
        SampleFormat::Float32,
    )
}

fn clock() -> CaptureClock {
    CaptureClock::start_at(CaptureTimestamp::from_source(
        SourceClock::PerformanceCounter,
        EPOCH,
    ))
}

/// One block a scripted capture will hand over.
#[derive(Debug, Clone)]
struct ScriptedBuffer {
    /// Where the track puts this buffer, in nanoseconds on the endpoint's clock.
    timestamp: u64,
    /// Where the endpoint itself said it belongs, when it said anything.
    device: Option<u64>,
    samples: Vec<f32>,
    origin: SampleOrigin,
}

/// A capture that replays a script and touches no hardware.
///
/// A real one needs a sound card, an audio service and a machine nobody else is
/// using. What is under test here is not WASAPI — `crates/audio` asserts that
/// against a real endpoint — but where the buffers it produces end up and what
/// timestamps they are given, and a script drives exactly that.
#[derive(Debug)]
struct ScriptedCapture {
    format: AudioFormat,
    ready: VecDeque<ScriptedBuffer>,
    /// What the audio engine is still holding: handed over only once
    /// [`AudioCapture::finish`] has been called, and lost entirely by a thread
    /// that closes without draining.
    ///
    /// This is the whole of what a real engine's backlog does to a caller
    /// (`EndpointCapture::begin_drain`, issue #320), with none of the timing:
    /// the buffers are already converted and already on the timeline, so what a
    /// test built on this measures is whether the recording *asks* for them.
    held: VecDeque<ScriptedBuffer>,
    /// The buffer last handed out, which owns the samples it lends — the same
    /// lifetime a real capture's converted packet has.
    current: Option<ScriptedBuffer>,
    /// Raised when the script has run out, so a test can wait for the thread to
    /// have consumed everything rather than sleeping for a guess.
    ///
    /// The *script*, not [`Self::held`]: a source whose engine is holding
    /// something is exhausted in exactly the sense this flag is used for — it
    /// has nothing more to give until it is asked to finish — and that is the
    /// moment a recording is stopped in.
    exhausted: Arc<AtomicBool>,
    /// Whether [`AudioCapture::finish`] has been called.
    draining: bool,
    /// Whether the capture has let go, which a real one does by itself once a
    /// drain has handed over everything.
    closed: bool,
}

impl AudioCapture for ScriptedCapture {
    fn read(&mut self, _timeout: Duration) -> Result<Capture<'_>, AudioError> {
        if self.closed {
            return Err(AudioError::NotOpen);
        }
        self.current = if self.draining {
            self.held.pop_front()
        } else {
            self.ready.pop_front()
        };
        let Some(buffer) = self.current.as_ref() else {
            if self.draining {
                // A drain that has handed over everything closes the capture
                // itself, which is how a real one tells a caller there is no
                // more (`EndpointCapture::next_ready`).
                self.closed = true;
            } else {
                // A script that has run out stands for an endpoint with nothing
                // to report, which is what a real one returns between packets.
                self.exhausted.store(true, Ordering::Relaxed);
            }
            return Ok(Capture::Idle);
        };
        let mut audio = CapturedAudio::new(
            &buffer.samples,
            self.format,
            AudioTimestamp::from_nanos(buffer.timestamp),
            buffer.origin,
        );
        if let Some(device) = buffer.device {
            audio = audio.with_device_timestamp(AudioTimestamp::from_nanos(device));
        }
        Ok(Capture::Samples(audio))
    }

    fn finish(&mut self) {
        self.draining = true;
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

/// A source scripted to hand over `buffers`, and the flag raised when it has.
struct Scripted {
    source: OpenSource,
    exhausted: Arc<AtomicBool>,
}

fn scripted(source: AudioSource, channels: u16, buffers: Vec<ScriptedBuffer>) -> Scripted {
    scripted_holding(source, channels, buffers, Vec::new())
}

/// [`scripted`], with `held` standing for the audio the engine has captured and
/// nobody has collected when the recording is stopped.
fn scripted_holding(
    source: AudioSource,
    channels: u16,
    buffers: Vec<ScriptedBuffer>,
    held: Vec<ScriptedBuffer>,
) -> Scripted {
    let exhausted = Arc::new(AtomicBool::new(false));
    Scripted {
        source: OpenSource {
            source,
            device: Some("a scripted endpoint".to_owned()),
            format: format(channels),
            capture: Box::new(ScriptedCapture {
                format: format(channels),
                ready: buffers.into_iter().collect(),
                held: held.into_iter().collect(),
                current: None,
                exhausted: Arc::clone(&exhausted),
                draining: false,
                closed: false,
            }),
        },
        exhausted,
    }
}

/// `seconds` of a tone at `frequency`, cut into the 10 ms buffers Windows
/// delivers, with the first frame `starts_at` nanoseconds from the epoch.
///
/// The track's account of a buffer and the endpoint's are the same here, which
/// stands for a crystal with no rate error at all; the drift test below supplies
/// its own pair.
fn tone(frequency: f64, channels: u16, seconds: f64, starts_at: i64) -> Vec<ScriptedBuffer> {
    let buffers = (seconds * 100.0).round() as usize;
    (0..buffers)
        .map(|index| {
            let first_frame = index * BUFFER_FRAMES;
            let samples = (0..BUFFER_FRAMES)
                .flat_map(|frame| {
                    let at = (first_frame + frame) as f64 / f64::from(SAMPLE_RATE);
                    // Half scale, so that nothing clips and the detector has a
                    // clear peak to find.
                    let value = (0.5 * (core::f64::consts::TAU * frequency * at).sin()) as f32;
                    core::iter::repeat_n(value, usize::from(channels))
                })
                .collect();
            // The tone is generated from the absolute frame index rather than
            // per buffer, so the track is one continuous sine across every
            // boundary and a discontinuity in the file would be this crate's.
            let at = EPOCH
                .wrapping_add_signed(starts_at)
                .wrapping_add(first_frame as u64 * 1_000_000_000 / u64::from(SAMPLE_RATE));
            ScriptedBuffer {
                timestamp: at,
                device: Some(at),
                samples,
                origin: SampleOrigin::Endpoint,
            }
        })
        .collect()
}

/// Frames a second the fixture is encoded at, and therefore the spacing of the
/// timestamps the recordings below are given.
const FRAMES_PER_SECOND: u64 = 60;

/// How much video the fixture holds. Longer than any recording here needs.
const FIXTURE_SECONDS: u64 = 3;

/// One access unit: the bytes of one coded picture, and whether a decoder can
/// start at it.
#[derive(Debug)]
struct AccessUnit {
    offset: usize,
    length: usize,
    keyframe: bool,
}

/// A test pattern encoded to H.264, taken apart into access units.
#[derive(Debug)]
struct CodedVideo {
    stream: Vec<u8>,
    units: Vec<AccessUnit>,
    codec_private: Vec<u8>,
}

impl CodedVideo {
    /// The video track a recording of this fixture declares.
    ///
    /// The sequence and picture parameter sets are the container's mandatory
    /// out-of-band header; without them the file lists a video stream that
    /// nothing can decode.
    fn track(&self) -> VideoTrack {
        VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT)
            .with_codec_private(self.codec_private.clone())
    }

    /// The packets covering `seconds` of video, each with its media time and
    /// whether a decoder can start at it.
    fn packets(&self, seconds: f64) -> Vec<(&[u8], i64, bool)> {
        let frames = (seconds * FRAMES_PER_SECOND as f64).round() as usize;
        assert!(
            frames <= self.units.len(),
            "the fixture holds {} frames and {frames} were asked for",
            self.units.len()
        );
        self.units[..frames]
            .iter()
            .enumerate()
            .map(|(index, unit)| {
                (
                    &self.stream[unit.offset..unit.offset + unit.length],
                    (index as u64 * 1_000_000_000 / FRAMES_PER_SECOND) as i64,
                    unit.keyframe,
                )
            })
            .collect()
    }
}

/// The shared fixture, or [`None`] when the FFmpeg programs are not on this
/// machine — which `require_media_tools` has already reported as a skip.
fn coded_video() -> Option<&'static CodedVideo> {
    static FIXTURE: OnceLock<Option<CodedVideo>> = OnceLock::new();
    FIXTURE.get_or_init(encode_fixture).as_ref()
}

/// Encodes the pattern and takes it apart, once.
fn encode_fixture() -> Option<CodedVideo> {
    let tools = require_media_tools()?;
    let directory = TemporaryDirectory::new("session-audio-fixture");
    let path = directory.file("pattern.h264");

    // `testsrc2` moves, so consecutive frames really differ and the encoder
    // produces predicted pictures rather than a stream of near-empty ones.
    // `aud=insert` puts an access unit delimiter in front of every picture,
    // which is what lets ffprobe's raw H.264 demuxer report exact packet
    // boundaries below.
    let source = format!(
        "testsrc2=size={WIDTH}x{HEIGHT}:rate={FRAMES_PER_SECOND}:duration={FIXTURE_SECONDS}"
    );
    let encoded = Command::new(tools.ffmpeg())
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", &source])
        .args(["-c:v", "libopenh264", "-b:v", "2000000", "-g", "120"])
        .args(["-bsf:v", "h264_metadata=aud=insert", "-f", "h264"])
        .arg(&path)
        .output()
        .expect("ffmpeg can be started");
    assert!(
        encoded.status.success(),
        "the fixture could not be encoded: {}",
        String::from_utf8_lossy(&encoded.stderr)
    );

    let stream = std::fs::read(&path).expect("the encoded fixture can be read");
    let units = access_units(tools.ffprobe(), &path);
    assert_eq!(
        units.len() as u64,
        FIXTURE_SECONDS * FRAMES_PER_SECOND,
        "the fixture does not hold the frames it was asked for"
    );

    let opening = &units[0];
    let codec_private = parameter_sets(&stream[opening.offset..opening.offset + opening.length]);
    assert!(
        !codec_private.is_empty(),
        "no sequence or picture parameter set was found in the first access unit"
    );

    // Everything the tests need is in memory now, so the encoded file goes
    // here rather than being carried around by the fixture: a `OnceLock` is
    // never dropped, and a `TemporaryDirectory` inside one would leave itself
    // on the disk after every run.
    drop(directory);
    Some(CodedVideo {
        stream,
        units,
        codec_private,
    })
}

/// Where each access unit begins and ends, according to `ffprobe`.
///
/// Asked for as `key=value` pairs rather than positionally, so that this does
/// not depend on the order `ffprobe` happens to print its fields in.
fn access_units(ffprobe: &Path, path: &Path) -> Vec<AccessUnit> {
    let probed = Command::new(ffprobe)
        .args(["-hide_banner", "-loglevel", "error"])
        .args(["-select_streams", "v:0", "-show_packets"])
        .args(["-show_entries", "packet=size,pos,flags"])
        .args(["-of", "compact=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe can be started");
    assert!(
        probed.status.success(),
        "the fixture could not be read back: {}",
        String::from_utf8_lossy(&probed.stderr)
    );

    String::from_utf8_lossy(&probed.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let field = |name: &str| {
                line.split('|')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_owned())
                    .unwrap_or_else(|| panic!("ffprobe reported no {name} for a packet: {line}"))
            };
            let parse = |name: &str| {
                field(name)
                    .parse::<usize>()
                    .unwrap_or_else(|_| panic!("ffprobe reported a {name} that is not a number"))
            };
            AccessUnit {
                offset: parse("pos"),
                length: parse("size"),
                // `K` in the first position is `AV_PKT_FLAG_KEY`.
                keyframe: field("flags").starts_with('K'),
            }
        })
        .collect()
}

/// The sequence and picture parameter sets of an access unit, in the Annex B
/// form a Windows hardware encoder hands `clipped-session`.
fn parameter_sets(unit: &[u8]) -> Vec<u8> {
    const SEQUENCE_PARAMETER_SET: u8 = 7;
    const PICTURE_PARAMETER_SET: u8 = 8;

    let mut header = Vec::new();
    for (kind, payload) in nal_units(unit) {
        if kind == SEQUENCE_PARAMETER_SET || kind == PICTURE_PARAMETER_SET {
            header.extend_from_slice(&[0, 0, 0, 1]);
            header.extend_from_slice(payload);
        }
    }
    header
}

/// Every NAL unit in `bytes`, as its type and its payload without the start
/// code.
fn nal_units(bytes: &[u8]) -> Vec<(u8, &[u8])> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == 0 && bytes[index + 1] == 0 && bytes[index + 2] == 1 {
            starts.push(index + 3);
            index += 3;
        } else {
            index += 1;
        }
    }

    starts
        .iter()
        .enumerate()
        .filter_map(|(position, start)| {
            let mut end = starts
                .get(position + 1)
                .map_or(bytes.len(), |next| next - 3);
            // A four-byte start code is the three-byte one with a zero in front
            // of it, and that zero belongs to neither unit. A NAL never ends in
            // a zero byte, so trimming them is unambiguous.
            while end > *start && bytes[end - 1] == 0 {
                end -= 1;
            }
            (end > *start).then(|| (bytes[*start] & 0x1f, &bytes[*start..end]))
        })
        .collect()
}

/// Records the scripted sources into a file at `path` and returns their reports.
///
/// This is the production path: the real layout, the real muxing thread, the
/// real audio threads, and the same placement and queueing a recording uses.
/// Only the capture and the encoder are scripted.
fn record(
    video: &CodedVideo,
    path: &Path,
    sources: Vec<Scripted>,
    seconds: f64,
) -> Vec<AudioTrackReport> {
    record_with(video, path, sources, seconds, false)
}

/// [`record`], saying whether the recording carries a compatibility mix.
fn record_with(
    video: &CodedVideo,
    path: &Path,
    sources: Vec<Scripted>,
    seconds: f64,
    compatibility_mix: bool,
) -> Vec<AudioTrackReport> {
    let exhausted: Vec<Arc<AtomicBool>> = sources
        .iter()
        .map(|scripted| Arc::clone(&scripted.exhausted))
        .collect();
    let sources: Vec<OpenSource> = sources
        .into_iter()
        .map(|scripted| scripted.source)
        .collect();

    let layout = declare(video.track(), &sources, compatibility_mix);
    let writer = MkvWriter::create(path, &layout).expect("the recording can be created");
    let muxing = MuxingThread::start(writer, SpaceGuard::new(path, 0), &layout)
        .expect("every declared track can be written to");

    let mut threads = AudioThreads::start(sources, &layout, clock(), Some(&muxing), None);

    for (data, nanos, keyframe) in video.packets(seconds) {
        muxing
            .write(data, nanos, nanos, keyframe)
            .expect("the writer accepts every packet");
    }

    // Bounded, and on a condition rather than on a duration: the threads are
    // stopped once every script has been read to the end, so this waits for the
    // work rather than for a guess at how long a machine takes to do it
    // (AGENTS.md section 25).
    let deadline = Instant::now() + Duration::from_secs(30);
    while exhausted.iter().any(|flag| !flag.load(Ordering::Relaxed)) {
        assert!(
            Instant::now() < deadline,
            "an audio thread did not read its script to the end"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let reports = threads.finish();
    muxing.finish().expect("the recording can be finalised");
    reports
}

#[test]
fn a_recording_with_both_sources_has_a_named_track_for_each_and_the_right_sound_on_it() {
    // Issue #180's first two acceptance criteria, end to end: the expected
    // streams, their codec, sampling rate and channel count, the names an editor
    // shows, and tracks that line up with the picture. The tone assertions are
    // what stop a session that routed both sources to one track from passing
    // everything above them (AGENTS.md section 21).
    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-tracks");
    let path = directory.file("recording.mkv");

    let reports = record(
        video,
        &path,
        vec![
            scripted(
                AudioSource::OtherSystemAudio,
                2,
                tone(SYSTEM_TONE, 2, 2.0, 0),
            ),
            scripted(AudioSource::Microphone, 1, tone(MICROPHONE_TONE, 1, 2.0, 0)),
        ],
        2.0,
    );

    Media::open(&path)
        .expect("a finished recording opens")
        .validate()
        .stream_count(3)
        .video(
            VideoStream::codec("h264")
                .resolution(WIDTH, HEIGHT)
                // Pictures out of a decoder, not packets in a container: a file
                // whose audio is perfect and whose video decodes to nothing is
                // not a recording, and every other assertion here passes on one.
                .decoded_frames(120),
        )
        .audio_stream_count(2)
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .sample_rate(SAMPLE_RATE)
                .channels(2)
                .title("Other System Audio")
                // The track a player that takes one audio track should take.
                // Nothing carries Matroska's default flag by accident: the model
                // gives it to the compatibility mix, which this build does not
                // make, so the session sets it on the first track the model
                // orders.
                .default_track(true),
        )
        .audio(
            1,
            AudioStream::codec("pcm_s16le")
                .sample_rate(SAMPLE_RATE)
                // A mono microphone stays mono. Resampling or upmixing it to
                // match the other track would be a decision nothing here is
                // entitled to take.
                .channels(1)
                .title("Microphone")
                .default_track(false),
        )
        // What is audible on each.
        .audio_tone(0, Tone::at(SYSTEM_TONE).isolated_from(MICROPHONE_TONE))
        .audio_tone(1, Tone::at(MICROPHONE_TONE).isolated_from(SYSTEM_TONE))
        .monotonic_timestamps()
        .synchronised_within(Duration::from_millis(40))
        .streams_start_at(0.0, 0.001)
        .assert_valid();

    let names: Vec<&str> = reports.iter().map(AudioTrackReport::track_name).collect();
    assert_eq!(
        names.len(),
        2,
        "both sources should have reported: {names:?}"
    );
    for report in &reports {
        assert!(
            report.frames() > 0,
            "{} recorded nothing at all",
            report.track_name()
        );
        assert_eq!(
            report.buffers_dropped_writer_behind(),
            0,
            "{} lost buffers to the writer",
            report.track_name()
        );
    }
}

/// The critical feature, measured rather than asserted: three sources, three
/// tracks, and each track carrying its own sound and none of the others.
///
/// This is the automated half of
/// [issue #34](https://github.com/wildware-uk/clipped/issues/34) and the proof
/// that the routing issues [#26](https://github.com/wildware-uk/clipped/issues/26)
/// and [#27](https://github.com/wildware-uk/clipped/issues/27) added does what
/// SPEC.md section 11 asks. The tones are section 26's: 440 Hz for the game,
/// 880 Hz for everything else the machine played, 1320 Hz for the microphone.
///
/// **What this does and does not prove.** The sources are scripted through
/// [`AudioCapture`], so what is measured is the *routing* — that the session
/// declares three tracks, puts each source on its own, and writes a file in
/// which they are separable. It is not measured against Windows: whether
/// `ProcessLoopbackCapture`'s include and exclude modes really partition the
/// machine's audio is the system half of #34, and it is
/// `tests/audio/track_isolation.rs` — a real recording of a real window, which
/// needs a GPU, a display and an output endpoint and therefore cannot live
/// here. The microphone leg of that is still the manual procedure in
/// `docs/testing.md`, because a simulated microphone needs a virtual capture
/// device rather than a program. A session that routed two
/// sources to one track passes every structural assertion above the tone ones
/// and fails here, which is the whole reason the tones exist.
#[test]
fn three_sources_produce_three_tracks_with_no_sound_shared_between_them() {
    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-isolation");
    let path = directory.file("recording.mkv");

    let reports = record(
        video,
        &path,
        vec![
            scripted(AudioSource::Game, 2, tone(GAME_TONE, 2, 2.0, 0)),
            scripted(
                AudioSource::OtherSystemAudio,
                2,
                tone(SYSTEM_TONE, 2, 2.0, 0),
            ),
            scripted(AudioSource::Microphone, 1, tone(MICROPHONE_TONE, 1, 2.0, 0)),
        ],
        2.0,
    );

    Media::open(&path)
        .expect("a finished recording opens")
        .validate()
        .audio_stream_count(3)
        // The order is the model's, not the order the sources were declared in:
        // `AudioSource::ordering_rank` puts the game before everything else and
        // the microphone last, so "track 2 is the microphone" keeps being true
        // across recordings.
        .audio(0, AudioStream::codec("pcm_s16le").title("Game"))
        .audio(
            1,
            AudioStream::codec("pcm_s16le").title("Other System Audio"),
        )
        .audio(2, AudioStream::codec("pcm_s16le").title("Microphone"))
        // The measurement. Each track's own tone must be at least eight times
        // the strength of either tone belonging to another source, which is the
        // documented rejection threshold #34 asks for
        // (`Tone::DEFAULT_RATIO`).
        .audio_tone(
            0,
            Tone::at(GAME_TONE)
                .isolated_from(SYSTEM_TONE)
                .isolated_from(MICROPHONE_TONE),
        )
        .audio_tone(
            1,
            Tone::at(SYSTEM_TONE)
                .isolated_from(GAME_TONE)
                .isolated_from(MICROPHONE_TONE),
        )
        .audio_tone(
            2,
            Tone::at(MICROPHONE_TONE)
                .isolated_from(GAME_TONE)
                .isolated_from(SYSTEM_TONE),
        )
        .monotonic_timestamps()
        .synchronised_within(Duration::from_millis(40))
        .assert_valid();

    let names: Vec<&str> = reports.iter().map(AudioTrackReport::track_name).collect();
    assert_eq!(
        names,
        vec!["Game", "Other System Audio", "Microphone"],
        "all three sources should have reported, in the model's order"
    );
}

/// The sound of the last fraction of a second, and of nothing before it.
///
/// A different frequency from the body of the track on purpose. What issue #320
/// is about is the audio the engine had captured and not handed over when
/// somebody pressed stop, and a tail carrying the same tone as everything before
/// it is a tail no measurement can tell from the track simply being shorter. Not
/// a harmonic of [`SYSTEM_TONE`] — 1760 Hz is its octave, and a quantised sine
/// puts a little of itself there — and far enough away that a Goertzel window
/// over one cannot see the other.
const LAST_MOMENT_TONE: f64 = 1500.0;

/// How long the engine is holding when the recording is stopped.
///
/// The 200 ms `EndpointCapture::begin_drain` exists for, which is what a real
/// engine holds at most.
const HELD_SECONDS: f64 = 0.2;

/// How long the recording runs, picture and all.
const RECORDING_SECONDS: f64 = 2.0;

/// How far apart the tracks may end.
///
/// One video frame of this fixture is 16.7 ms and one audio buffer is 10 ms, so
/// 40 ms is "within a packet" with room for the last of each landing on either
/// side of the other. It is also the bound `synchronised_within` is used with
/// everywhere else in this file, so a recording that lost its tail fails the
/// same assertion the rest of the suite already makes rather than a looser one
/// written for this test.
const ENDS_WITHIN: Duration = Duration::from_millis(40);

/// How much louder a tone has to be where it belongs than where it does not.
///
/// Eight times — about 18 dB — which is `Tone`'s own rejection threshold, so the
/// tail is held to the same standard the isolation assertions in this file are.
/// It is written out here because that constant is private to the harness; if
/// the two ever disagree it is this one that is wrong.
const REJECTION_RATIO: f64 = 8.0;

#[test]
fn a_recording_keeps_the_audio_its_engine_was_holding_when_it_was_stopped() {
    // [Issue #320](https://github.com/wildware-uk/clipped/issues/320). The audio
    // engine holds up to 200 ms that has been captured and not collected, and a
    // capture that is simply closed throws it away — the last fraction of a
    // second before somebody pressed stop, which is the part they were watching.
    //
    // The capture here holds back its last 200 ms until it is asked to finish,
    // which is exactly what a real one does, and the assertions are made against
    // the file rather than against the timestamps: the tail's own tone has to be
    // *in* the track, in the last 200 ms of it and not before, and the track has
    // to end with the picture.
    //
    // What makes every one of them fail is removing the `finish` and the reads
    // that follow it from `pump`. That was the state of this crate until #320:
    // `ProcessLoopbackCapture` was drained by a `close` that let go of the
    // client in the same breath, and the two endpoint captures had no drain at
    // all.
    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-drain");
    let path = directory.file("recording.mkv");

    let body_seconds = RECORDING_SECONDS - HELD_SECONDS;
    let held_from = (body_seconds * 1_000_000_000.0) as i64;
    let reports = record(
        video,
        &path,
        vec![scripted_holding(
            AudioSource::OtherSystemAudio,
            2,
            tone(SYSTEM_TONE, 2, body_seconds, 0),
            tone(LAST_MOMENT_TONE, 2, HELD_SECONDS, held_from),
        )],
        RECORDING_SECONDS,
    );

    let media = Media::open(&path).expect("a finished recording opens");
    let content = media.audio_content(0).expect("the track decodes");
    let rate = content.sample_rate();
    assert_eq!(
        rate, SAMPLE_RATE,
        "the track decodes at the rate it declares"
    );

    // The last 200 ms of the track, and the 200 ms before it, measured
    // separately. Two windows rather than one, because "the tail is present" and
    // "the tail is where it belongs" are different claims: a recording that
    // appended the drained audio somewhere else, or that let it overwrite the
    // end of the body, satisfies the first and not the second.
    let held_samples = (HELD_SECONDS * f64::from(rate)) as usize;
    let samples = content.samples();
    assert!(
        samples.len() > held_samples * 2,
        "the track is {} samples long, which is not enough to measure a {HELD_SECONDS}s tail in",
        samples.len()
    );
    let split = samples.len() - held_samples;
    let tail = AudioContent::from_samples(samples[split..].to_vec(), rate);
    let before_the_tail =
        AudioContent::from_samples(samples[split - held_samples..split].to_vec(), rate);

    let in_tail = tail.magnitude_at(LAST_MOMENT_TONE);
    let in_body = before_the_tail.magnitude_at(LAST_MOMENT_TONE);
    let body_tone_in_body = before_the_tail.magnitude_at(SYSTEM_TONE);
    let _ = writeln!(
        std::io::stderr(),
        "\n=== the drained tail ===\n\
         track            : {:.3}s, {} frames reported\n\
         last {HELD_SECONDS}s        : {LAST_MOMENT_TONE} Hz {in_tail:.5}\n\
         the {HELD_SECONDS}s before  : {LAST_MOMENT_TONE} Hz {in_body:.5}, \
         {SYSTEM_TONE} Hz {body_tone_in_body:.5}",
        content.duration().as_secs_f64(),
        reports.first().map_or(0, AudioTrackReport::frames),
    );

    media
        .validate()
        .audio_stream_count(1)
        // Issue #320's second acceptance criterion, against a produced file
        // rather than against the timestamps a capture reported: the audio ends
        // where the picture does. Asserted before the tones so that a failure
        // here is about the *shape* of the recording rather than about what is
        // on the track. A recording that dropped the tail ends 200 ms early and
        // fails this by five times the bound.
        .synchronised_within(ENDS_WITHIN)
        .monotonic_timestamps()
        .streams_start_at(0.0, 0.001)
        .assert_valid();

    // The measurement issue #320 asks for: the sound of the last fraction of a
    // second is in the file. Without the drain it is not in the file at all, and
    // this measures 0.
    assert!(
        in_tail > in_body * REJECTION_RATIO,
        "the last {HELD_SECONDS}s of the recording is the audio the engine was still holding \
         when it was stopped, and it has to be in the track: {LAST_MOMENT_TONE} Hz measures \
         {in_tail:.5} there against {in_body:.5} in the {HELD_SECONDS}s before it, which is \
         {:.1}x apart and not the {:.0}x that would mean the tail arrived",
        in_tail / in_body.max(f64::MIN_POSITIVE),
        REJECTION_RATIO
    );
    // And it did not arrive by overwriting what was already there. The body's
    // own tone is still in the body.
    assert!(
        body_tone_in_body > in_body * REJECTION_RATIO,
        "the audio before the tail should still be the body's own {SYSTEM_TONE} Hz \
         ({body_tone_in_body:.5}), not the tail's {LAST_MOMENT_TONE} Hz ({in_body:.5})"
    );

    // The same thing said in frames, so a failure distinguishes "the tail is
    // missing from the file" from "the tail never left the capture".
    let report = reports.first().expect("the source reported");
    let expected = (RECORDING_SECONDS * f64::from(SAMPLE_RATE)) as u64;
    assert_eq!(
        report.frames(),
        expected,
        "{}s of recording plus the {HELD_SECONDS}s the engine was holding is {expected} frames; \
         the source reported {}",
        body_seconds,
        report.frames()
    );
}

/// #34's third assertion: the compatibility track carries **all three** sources.
///
/// The mix is what a player that takes one audio track arbitrarily takes
/// (SPEC.md section 13), so a mix missing the game is a recording that sounds
/// empty to everybody who does not go looking for track 1. Measured the way the
/// two-source mix test measures its own: `Tone::at` asserts the *dominant*
/// frequency and a mix of equal tones has none, so each is checked against the
/// track's own peak instead.
#[test]
fn the_compatibility_mix_carries_the_game_the_rest_of_the_machine_and_the_microphone() {
    let Some(video) = coded_video() else {
        return;
    };
    // The same short script the two-source mix test uses, and for the same
    // reason: inside the mixer's `MAX_SOURCE_LAG`, so no scripted source can run
    // far enough ahead in media time to be left out of the mix.
    const SCRIPT_SECONDS: f64 = 0.4;

    let directory = TemporaryDirectory::new("session-mix-of-three");
    let path = directory.file("recording.mkv");

    record_with(
        video,
        &path,
        vec![
            scripted(AudioSource::Game, 2, tone(GAME_TONE, 2, SCRIPT_SECONDS, 0)),
            scripted(
                AudioSource::OtherSystemAudio,
                2,
                tone(SYSTEM_TONE, 2, SCRIPT_SECONDS, 0),
            ),
            scripted(
                AudioSource::Microphone,
                1,
                tone(MICROPHONE_TONE, 1, SCRIPT_SECONDS, 0),
            ),
        ],
        SCRIPT_SECONDS,
        true,
    );

    let media = Media::open(&path).expect("a finished recording opens");
    let mix = media
        .audio_content(0)
        .expect("the compatibility track can be decoded");
    let quiet = mix.peak_amplitude() as f64 / 8.0;
    let game = mix.magnitude_at(GAME_TONE);
    let system = mix.magnitude_at(SYSTEM_TONE);
    let microphone = mix.magnitude_at(MICROPHONE_TONE);

    media
        .validate()
        .that(game > quiet, || {
            format!(
                "the mix should carry the game at {GAME_TONE} Hz, and it measures {game:.4}                  against a peak of {:.4}",
                mix.peak_amplitude()
            )
        })
        .that(system > quiet, || {
            format!(
                "the mix should carry the rest of the machine at {SYSTEM_TONE} Hz, and it                  measures {system:.4} against a peak of {:.4}",
                mix.peak_amplitude()
            )
        })
        .that(microphone > quiet, || {
            format!(
                "the mix should carry the microphone at {MICROPHONE_TONE} Hz, and it measures                  {microphone:.4} against a peak of {:.4}",
                mix.peak_amplitude()
            )
        })
        // Four tracks: the mix, and the three it was made from.
        .audio_stream_count(4)
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .title("Compatibility Mix")
                .default_track(true),
        )
        .audio(1, AudioStream::codec("pcm_s16le").title("Game"))
        .audio(2, AudioStream::codec("pcm_s16le").title("Other System Audio"))
        .audio(3, AudioStream::codec("pcm_s16le").title("Microphone"))
        // And mixing changed none of the tracks it read.
        .audio_tone(
            1,
            Tone::at(GAME_TONE)
                .isolated_from(SYSTEM_TONE)
                .isolated_from(MICROPHONE_TONE),
        )
        .audio_tone(
            2,
            Tone::at(SYSTEM_TONE)
                .isolated_from(GAME_TONE)
                .isolated_from(MICROPHONE_TONE),
        )
        .audio_tone(
            3,
            Tone::at(MICROPHONE_TONE)
                .isolated_from(GAME_TONE)
                .isolated_from(SYSTEM_TONE),
        )
        .assert_valid();
}

#[test]
fn a_recording_with_no_audio_sources_is_a_video_only_file() {
    // `--microphone none --system-audio none`. No device is opened, no thread is
    // started and — the part a container makes visible — no audio track is
    // declared. A recording with two empty tracks in it would pass a test that
    // only counted packets.
    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-none");
    let path = directory.file("recording.mkv");

    let reports = record(video, &path, Vec::new(), 1.0);
    assert!(reports.is_empty());

    Media::open(&path)
        .expect("a finished recording opens")
        .validate()
        .stream_count(1)
        .video(VideoStream::codec("h264").resolution(WIDTH, HEIGHT))
        .audio_stream_count(0)
        .assert_valid();
}

#[test]
fn audio_that_precedes_the_first_video_frame_does_not_stack_on_the_start_of_the_file() {
    // The endpoint is opened while the encoder is still being created, so the
    // first buffers of a recording describe moments before it — 293 ms of them
    // on the machine docs/av-sync.md was measured on. Left to the writer they
    // are clamped onto the file's first instant, which puts a quarter of a
    // second of audio in one place and leaves the track ending that much later
    // than the picture.
    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-early");
    let path = directory.file("recording.mkv");

    let reports = record(
        video,
        &path,
        vec![scripted(
            AudioSource::OtherSystemAudio,
            2,
            tone(SYSTEM_TONE, 2, 2.0, -250_000_000),
        )],
        1.75,
    );

    assert_eq!(
        reports[0].frames_before_the_recording(),
        u64::from(SAMPLE_RATE) / 4,
        "exactly the quarter-second before the epoch should have been trimmed, and counted"
    );

    Media::open(&path)
        .expect("a finished recording opens")
        .validate()
        .audio_stream_count(1)
        // Both halves of the failure this prevents: the audio must not begin
        // before the picture, and — because trimming is not shifting — it must
        // not end a quarter of a second after it either.
        .streams_start_at(0.0, 0.001)
        .synchronised_within(Duration::from_millis(40))
        .monotonic_timestamps()
        .assert_valid();
}

#[test]
fn a_source_that_produced_only_silence_leaves_a_declared_track_and_says_so() {
    // A microphone Windows had muted. `clipped-audio` keeps such a track the
    // length of its recording with synthesised silence, so the file is
    // structurally perfect and contains nothing — which is exactly the state a
    // recorder has to be able to describe (AGENTS.md section 45).
    let silence: Vec<ScriptedBuffer> = (0..100)
        .map(|index| ScriptedBuffer {
            timestamp: EPOCH + index * 10_000_000,
            // Synthesised silence covers a period the device never described,
            // so it has no position of its own to disagree with.
            device: None,
            samples: vec![0.0; BUFFER_FRAMES],
            origin: SampleOrigin::SynthesisedSilence,
        })
        .collect();

    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-silence");
    let path = directory.file("recording.mkv");
    let reports = record(
        video,
        &path,
        vec![scripted(AudioSource::Microphone, 1, silence)],
        1.0,
    );

    let report = &reports[0];
    assert_eq!(
        report.frames(),
        report.synthesised_silence_frames(),
        "every frame on this track was synthesised, and the report has to say so"
    );
    assert!(
        report.sync().is_none(),
        "silence has no device position to measure against, and quoting a zero offset would \
         be inventing a measurement"
    );
    assert!(
        report.to_string().contains("the device produced nothing"),
        "{report}"
    );
}

#[test]
fn the_offset_between_the_track_and_the_endpoint_is_measured_rather_than_assumed() {
    // docs/av-sync.md's drift measurement, made by the session rather than by a
    // test harness. The script is an endpoint whose sample clock runs slow: its
    // own reported positions advance faster than the track built by counting
    // samples, so each sound is stamped progressively earlier than the moment it
    // happened — sound ahead of picture, and a negative rate.
    //
    // A hundred parts per million, which is far larger than any real crystal
    // (four was measured) so that two seconds of script produces a slope worth
    // fitting rather than noise.
    const PPM: i64 = 100;
    let buffers: Vec<ScriptedBuffer> = (0..200_i64)
        .map(|index| {
            let track = EPOCH as i64 + index * 10_000_000;
            ScriptedBuffer {
                timestamp: track as u64,
                // The endpoint's own account, ahead of the track's by the rate
                // error accumulated so far.
                device: Some((track + index * 10_000_000 * PPM / 1_000_000) as u64),
                samples: vec![0.0; BUFFER_FRAMES * 2],
                origin: SampleOrigin::Endpoint,
            }
        })
        .collect();

    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-drift");
    let path = directory.file("recording.mkv");
    let reports = record(
        video,
        &path,
        vec![scripted(AudioSource::OtherSystemAudio, 2, buffers)],
        2.0,
    );

    let sync = reports[0]
        .sync()
        .expect("an endpoint that reported positions has a measurement");
    assert_eq!(sync.observations(), 200);
    assert_eq!(
        sync.first_offset_nanos(),
        0,
        "the first observation is zero by construction: what is measured is a change"
    );
    assert!(
        sync.latest_offset_nanos() < 0,
        "a slow endpoint puts the track ahead of the picture, not behind it: {} ns",
        sync.latest_offset_nanos()
    );

    let rate = sync
        .drift_parts_per_billion()
        .expect("two hundred observations fit a line");
    assert!(
        (rate + PPM * 1_000).abs() < 1_000,
        "the fitted rate should be about {} ppb, and was {rate}",
        -PPM * 1_000
    );
    assert_eq!(
        sync.discontinuities(),
        0,
        "a steady rate is drift, not a series of steps"
    );
    // Two seconds at a hundred parts per million is 200 microseconds, which is
    // nowhere near the tolerance — so the state is the useful half of this
    // assertion rather than the magnitude.
    assert_eq!(sync.state(), SyncState::InTolerance);
}

#[test]
fn a_layout_orders_and_names_its_tracks_from_the_model_and_flags_only_the_first() {
    // Declared in the order somebody's settings screen might produce, which is
    // not the order the file has to be in — and the flag has to land on the
    // track a player that takes one should take, whichever that turns out to be.
    let layout = declare(
        VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT),
        &[
            scripted(AudioSource::Microphone, 1, Vec::new()).source,
            scripted(AudioSource::OtherSystemAudio, 2, Vec::new()).source,
        ],
        false,
    );

    let tracks = layout.audio_tracks();
    assert_eq!(
        tracks
            .iter()
            .map(clipped_muxer::AudioTrack::name)
            .collect::<Vec<_>>(),
        [Some("Other System Audio"), Some("Microphone")]
    );
    assert!(tracks[0].is_default(), "the first track carries the flag");
    assert!(
        !tracks[1].is_default(),
        "two default tracks is a file whose player chooses between them"
    );
    assert_eq!(
        layout.audio_track_for(&AudioSource::Microphone),
        Some(TrackId::Audio(1)),
        "a source addresses its packets by asking the layout, and the answer has to be right"
    );
}

#[test]
fn a_microphone_only_recording_flags_the_microphone_track() {
    // The other end of the same rule. With no system audio there is nothing
    // ahead of the microphone, so it is the track a naive player should take —
    // and a recording where nothing carries the flag leaves the player to guess.
    let layout = declare(
        VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT),
        &[scripted(AudioSource::Microphone, 1, Vec::new()).source],
        false,
    );
    assert!(layout.audio_tracks()[0].is_default());
}

#[test]
fn a_video_only_recording_declares_no_audio_track_at_all() {
    let layout = declare(VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT), &[], false);
    assert!(
        layout.audio_tracks().is_empty(),
        "a source that was turned off must not leave an empty track behind it"
    );
}

#[test]
fn a_buffer_the_writer_has_no_room_for_is_dropped_and_counted_rather_than_waited_on() {
    // AGENTS.md section 20: a capture thread may not wait on the filesystem, and
    // `sync_channel::send` on a full queue is exactly that. Twenty thousand
    // buffers is far more than the audio share holds, so the writer has to lose
    // this race — and the assertion is that nothing here ever blocks, whatever
    // it does.
    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-full-queue");
    let path = directory.file("recording.mkv");
    let layout = declare(
        video.track(),
        &[scripted(AudioSource::Microphone, 1, Vec::new()).source],
        false,
    );
    let writer = MkvWriter::create(&path, &layout).expect("the recording can be created");
    let muxing = MuxingThread::start(writer, SpaceGuard::new(&path, 0), &layout)
        .expect("every declared track can be written to");
    let queue = muxing.audio_queue();

    let samples = vec![0.0_f32; BUFFER_FRAMES];
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut dropped = 0_u64;
    for index in 0..20_000_i64 {
        assert!(
            Instant::now() < deadline,
            "queueing audio blocked instead of dropping; {index} buffers in"
        );
        match queue.write(
            TrackId::Audio(0),
            MediaTime::from_nanos(index * 10_000_000),
            &samples,
        ) {
            AudioQueued::Written => {}
            AudioQueued::DroppedWriterBehind => dropped += 1,
            AudioQueued::WriterLost => panic!("the writer stopped: {index} buffers in"),
        }
    }

    drop(queue);
    let reported = muxing.audio_buffers_dropped();
    muxing.finish().expect("the recording can be finalised");

    assert!(
        dropped > 0,
        "twenty thousand buffers should have outrun a queue that holds hundreds; if this \
         machine is fast enough to write them all, the drop path has stopped being exercised"
    );
    assert_eq!(
        dropped, reported,
        "every dropped buffer has to be countable from the recording as well as by the \
         thread that lost it"
    );
}

#[test]
fn a_thread_stops_when_the_writer_has_gone_rather_than_reading_into_a_closed_queue() {
    // What a full disk looks like from an audio thread: the writer has stopped,
    // so the queue is disconnected. Reading on would hold an endpoint open —
    // and, for a microphone, leave Windows showing it as in use — for a
    // recording that is over.
    let Some(video) = coded_video() else {
        return;
    };
    let directory = TemporaryDirectory::new("session-audio-writer-gone");
    let path = directory.file("recording.mkv");
    let layout = declare(
        video.track(),
        &[scripted(AudioSource::Microphone, 1, Vec::new()).source],
        false,
    );
    let writer = MkvWriter::create(&path, &layout).expect("the recording can be created");
    let muxing = MuxingThread::start(writer, SpaceGuard::new(&path, 0), &layout)
        .expect("every declared track can be written to");
    let queue = muxing.audio_queue();

    // An empty packet is what the writer refuses (`MuxError::EmptyPacket`), and
    // the writer thread stops at its first failure — which is what a full disk
    // does to it as well.
    muxing
        .write(&[], 0, 0, true)
        .expect("the queue accepts it; the writer is what refuses it");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match queue.write(TrackId::Audio(0), MediaTime::ZERO, &[0.0; 480]) {
            AudioQueued::WriterLost => break,
            _ => assert!(
                Instant::now() < deadline,
                "the audio queue never noticed that the writer had stopped, so an audio \
                 thread would read its endpoint for the rest of the session"
            ),
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    drop(queue);
    assert!(
        muxing.finish().is_err(),
        "the refused packet has to come back from the writer thread"
    );
}

#[test]
fn the_compatibility_track_carries_every_source_and_the_isolated_tracks_stay_isolated() {
    // Issue #29's second acceptance criterion, end to end and in one file:
    // track 1 contains the summed tones, and the tracks after it each contain
    // only their own. Both halves matter — a session that wrote the mix
    // correctly and *also* mixed it into the isolated tracks would pass the
    // first assertion and ruin the recording.
    let Some(video) = coded_video() else {
        return;
    };
    // Shorter than the mixer's half-second `MAX_SOURCE_LAG`, and that is the
    // whole reason for the number. These sources are scripted and hand their
    // blocks over as fast as the threads will take them, so one can run ahead of
    // the other in *media* time — and the mixer deliberately carries on without
    // a source that has lagged too far, which is what stops one stalled capture
    // silencing a recording. Keeping the entire script inside that window means
    // no source can ever be far enough behind to be left out, whichever order
    // the threads happen to run in. A real capture produces at wall-clock rate
    // and the two stay milliseconds apart.
    const SCRIPT_SECONDS: f64 = 0.4;

    let directory = TemporaryDirectory::new("session-compatibility-mix");
    let path = directory.file("recording.mkv");

    record_with(
        video,
        &path,
        vec![
            scripted(
                AudioSource::OtherSystemAudio,
                2,
                tone(SYSTEM_TONE, 2, SCRIPT_SECONDS, 0),
            ),
            scripted(
                AudioSource::Microphone,
                1,
                tone(MICROPHONE_TONE, 1, SCRIPT_SECONDS, 0),
            ),
        ],
        SCRIPT_SECONDS,
        true,
    );

    let media = Media::open(&path).expect("a finished recording opens");

    // Both tones are on the mix, measured rather than inferred. `Tone::at`
    // cannot say this: it asserts the *dominant* frequency, and a mix of two
    // equal tones has no dominant one — which is the whole point of it.
    let opening = media
        .audio_content(0)
        .expect("the compatibility track can be decoded");
    let quiet = opening.peak_amplitude() as f64 / 8.0;
    let system = opening.magnitude_at(SYSTEM_TONE);
    let microphone = opening.magnitude_at(MICROPHONE_TONE);

    media
        .validate()
        .that(system > quiet, || {
            format!(
                "a:0 should carry the system source at {SYSTEM_TONE} Hz, and it measures                  {system:.4} against a peak of {:.4}",
                opening.peak_amplitude()
            )
        })
        .that(microphone > quiet, || {
            format!(
                "a:0 should carry the microphone at {MICROPHONE_TONE} Hz, and it measures                  {microphone:.4} against a peak of {:.4}",
                opening.peak_amplitude()
            )
        })
        // Three audio tracks now: the mix, and the two it was made from.
        .audio_stream_count(3)
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .sample_rate(SAMPLE_RATE)
                .title("Compatibility Mix")
                // The whole point of it: a player that takes one track
                // arbitrarily takes this one.
                .default_track(true),
        )
        .audio(1, AudioStream::codec("pcm_s16le").default_track(false))
        .audio(2, AudioStream::codec("pcm_s16le").default_track(false))
        // And mixing changed neither of the tracks it read.
        .audio_tone(1, Tone::at(SYSTEM_TONE).isolated_from(MICROPHONE_TONE))
        .audio_tone(2, Tone::at(MICROPHONE_TONE).isolated_from(SYSTEM_TONE))
        .monotonic_timestamps()
        .synchronised_within(Duration::from_millis(40))
        .assert_valid();
}

/// What [`plan_system_audio`] decides, which is the whole of issues #26 and #27
/// that can be decided without a machine.
///
/// Opening a capture needs Windows, an endpoint and a running process. Deciding
/// *which* captures to open needs none of those, and it is where the failure
/// this pair of issues exists to prevent actually lives: a plan that scopes the
/// game one way and everything-else another puts the game's audio on two tracks,
/// and nobody finds that until they mute the game track in an editor and the
/// game is still audible.
mod planning {
    use super::*;

    /// The pid is arbitrary; what matters is that the same one reaches both.
    const GAME: u32 = 4242;

    #[test]
    fn a_window_recording_scopes_the_game_and_everything_else_to_the_same_tree() {
        let planned =
            plan_system_audio(&AudioSourceSetting::SystemDefault, Some(GAME)).expect("a plan");

        assert_eq!(
            planned,
            vec![PlannedSource::ScopedPair(GAME)],
            "a window recording gets both halves of the split, against one tree, as one plan \
             item — two items would be two decisions, and a recording that acted on them \
             independently is issue #581"
        );
    }

    /// The invariant the pair exists for, stated as its own test because it is
    /// the one a future change is most likely to break: no plan may contain a
    /// whole-endpoint capture *and* a process-scoped one. The first records the
    /// game along with everything else, so together they write the game's audio
    /// to two tracks.
    #[test]
    fn no_plan_mixes_a_scoped_capture_with_the_whole_endpoint() {
        for game_process in [None, Some(GAME)] {
            for setting in [AudioSourceSetting::Off, AudioSourceSetting::SystemDefault] {
                let planned = plan_system_audio(&setting, game_process).expect("a plan");

                let whole = planned.contains(&PlannedSource::WholeEndpoint);
                let scoped = planned
                    .iter()
                    .any(|source| !matches!(source, PlannedSource::WholeEndpoint));

                assert!(
                    !(whole && scoped),
                    "{setting:?} with {game_process:?} planned {planned:?}, which records the \
                     game twice"
                );
            }
        }
    }

    /// A monitor recording, or a window whose process has already exited. One
    /// unscoped system track is a worse recording than the split, and a better
    /// one than none.
    #[test]
    fn a_recording_with_no_process_falls_back_to_the_whole_endpoint() {
        let planned = plan_system_audio(&AudioSourceSetting::SystemDefault, None).expect("a plan");

        assert_eq!(planned, vec![PlannedSource::WholeEndpoint]);
    }

    /// `--system-audio none` means none, and a resolvable game process is not a
    /// reason to open two captures the user turned off.
    #[test]
    fn system_audio_off_opens_nothing_even_when_a_game_is_known() {
        let planned = plan_system_audio(&AudioSourceSetting::Off, Some(GAME)).expect("a plan");

        assert!(
            planned.is_empty(),
            "off planned {planned:?}, so a recording somebody asked to be silent is not"
        );
    }

    /// Refused rather than silently recording the default endpoint (#316).
    #[test]
    fn a_named_system_audio_device_is_still_refused() {
        let named = AudioSourceSetting::Named("Speakers".to_owned());

        assert!(matches!(
            plan_system_audio(&named, Some(GAME)),
            Err(SessionError::AudioDeviceNotSelectable)
        ));
    }
}

/// That a recording opens the two scoped captures **as a pair**.
///
/// # Why this is here and not in `clipped-audio`
///
/// `clipped-audio` already measures that `open_pair` works. What nothing
/// measured was whether anything *called* it: for a week
/// `crates/session/src/audio/mod.rs` opened `open` and `open_excluding`
/// separately, two lines apart, while that crate's own documentation and
/// `docs/audio-routing.md` both said a recording opened the pair
/// ([issue #581](https://github.com/wildware-uk/clipped/issues/581)). Every
/// test of the producer passed throughout. So this drives the real recording
/// path — [`open`], the function `crate::recording` calls — and asserts on what
/// comes back out of it (AGENTS.md section 55).
///
/// # What it can and cannot see
///
/// Two captures opened separately and two opened as a pair are identical in
/// every observable way *until* the process the activation names exits with
/// descendants still running, and which of them each side then picks depends on
/// when two independent process-table scans happened to run. A test that
/// arranged that and compared `scoped_to` would pass on a build with no
/// agreement at all, most of the time.
///
/// What is exact is the agreement itself:
/// [`ProcessLoopbackCapture::scope_agreement`] is the identity of the cell the
/// two sides re-scope through, and it is equal for the two halves of one
/// `open_pair` and for nothing else. So the assertion is that identity, which
/// fails if `open` reverts to two separate calls **and** fails if `open_pair`
/// stops handing its two captures one cell.
///
/// # What it needs from the machine
///
/// A real window of this process, so that `game_process` resolves the way it
/// does for a recording of a game, and a Windows that will scope a capture to a
/// process tree at all. It plays nothing and records nothing: the tree it opens
/// is this test binary, which makes no sound, and no packet is ever read. Where
/// the activation is refused it skips loudly, and `CLIPPED_REQUIRE_AUDIO` turns
/// that skip into a failure for whoever is supposed to have the hardware
/// (AGENTS.md section 54). A GitHub Windows runner has no audio endpoint and
/// still scopes a capture — issue #441 is the afternoon that assuming otherwise
/// cost — so this is not an `#[ignore]`d suite.
mod pairing {
    use std::io::Write as _;

    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
    };

    use super::*;
    use crate::settings::CaptureTargetSettings;

    /// The environment variable that turns "this machine will not scope a
    /// capture" from a skip into a failure. The same one `clipped-audio` uses,
    /// so one machine's configuration governs both.
    const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

    /// Reports that the test could not run here.
    ///
    /// Written through `std::io::stderr()` rather than with `eprintln!`, which
    /// libtest captures: a skip nobody can see in a passing run is exactly how
    /// a test becomes a no-op without anybody noticing.
    ///
    /// # Panics
    ///
    /// When [`REQUIRE_AUDIO`] is set, because a machine that is supposed to be
    /// able to do this saying it cannot is a failure and not a skip.
    fn skipped(reason: &str) {
        assert!(
            std::env::var_os(REQUIRE_AUDIO).is_none_or(|value| value.is_empty()),
            "{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}"
        );
        let _ = writeln!(std::io::stderr(), "SKIPPED (audio): {reason}");
    }

    /// A real window belonging to this process, for `game_process` to resolve.
    ///
    /// `CaptureTargetSettings::window` takes a handle and the audio sources are
    /// opened against whatever process owns it, so a fabricated handle would
    /// plan a whole-endpoint recording and measure nothing. The system `STATIC`
    /// class needs no class registration; the window is never shown and is far
    /// off-screen, because a test on a shared machine must not put anything in
    /// front of somebody.
    struct TestWindow(HWND);

    impl TestWindow {
        fn open() -> Option<Self> {
            // SAFETY: `STATIC` is a system window class and both strings are
            // static wide literals that outlive the call. No parent, menu,
            // instance or creation parameter is passed, which is what the
            // `None`s mean. `Drop` destroys what this returns.
            let window = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("clipped audio pairing test window"),
                    WS_POPUP,
                    -32_000,
                    -32_000,
                    16,
                    16,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .ok()?;
            Some(Self(window))
        }

        /// The handle in the form `CaptureTargetSettings::window` takes, which
        /// is what `WindowHandle::as_u64` produces and `game_process` casts
        /// back to an `isize` to undo.
        fn handle(&self) -> u64 {
            clipped_windows::WindowHandle::from_raw(self.0 .0 as isize).as_u64()
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateWindowExW` on this thread and
            // is destroyed once, here.
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn a_recording_opens_the_two_scoped_captures_as_one_pair() {
        let Some(window) = TestWindow::open() else {
            skipped("this machine would not create a window to record");
            return;
        };
        let settings = RecordingSettings::new(
            CaptureTargetSettings::window(window.handle(), 640, 360),
            std::path::PathBuf::from("pairing.mkv"),
        )
        .with_system_audio(AudioSourceSetting::SystemDefault);

        // The real thing: the same call `crate::recording` makes, against the
        // same settings a recording of a window is made with. Nothing below
        // here knows this is a test.
        let mut sources = match open(&settings) {
            Ok(sources) => sources,
            Err(error) => {
                skipped(&format!(
                    "a process-scoped capture could not be opened here: {error}"
                ));
                return;
            }
        };

        let tracks: Vec<_> = sources
            .iter()
            .map(|source| source.source.track_name())
            .collect();
        assert_eq!(
            tracks,
            vec![
                AudioSource::Game.track_name(),
                AudioSource::OtherSystemAudio.track_name(),
            ],
            "a window recording is the game's tree and everything except it, in track order"
        );

        let agreements: Vec<_> = sources
            .iter()
            .map(|source| source.capture.scope_agreement())
            .collect();

        // The two activations are released here rather than left to the end of
        // the test binary: this runs on somebody's machine, and the assertions
        // below need nothing from the captures that has not already been asked.
        for source in &mut sources {
            source.capture.close();
        }

        let [Some(game), Some(other)] = agreements.as_slice() else {
            panic!(
                "both scoped captures have an agreement to report; got {agreements:?}, which is \
                 a recording that opened something other than two process-scoped captures"
            );
        };

        assert_eq!(
            game, other,
            "the game's track and the other-system-audio track are scoped through two different \
             cells, so nothing keeps them on the same process once the game's launcher exits: a \
             process in one side's tree and not the other's is then on both tracks or on \
             neither. That is a recording opened through `open` and `open_excluding` separately \
             rather than through `open_pair` (issue #581), or an `open_pair` that has stopped \
             sharing its cell (issue #27)"
        );
    }
}

/// Reducing a buffer to the one number a meter draws.
///
/// Opening a microphone needs Windows and an endpoint somebody has plugged in.
/// Turning the samples it delivers into a level needs neither, and it is where
/// the two failures worth having a test for live: a meter that reads zero while
/// somebody is speaking, and a meter that sticks at the top and never comes
/// down again (issue #109).
mod level {
    use super::*;

    #[test]
    fn the_peak_is_the_loudest_sample_however_it_is_signed() {
        // The trough of a waveform is as loud as its crest. A meter that took
        // the maximum rather than the maximum magnitude would read zero for
        // every buffer whose loudest excursion happened to be downwards, which
        // on a symmetric signal is half of them.
        assert!((loudest(&[0.0, 0.25, -0.8, 0.3]) - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn silence_is_zero_and_not_something_a_meter_would_show() {
        assert_eq!(loudest(&[0.0; 480]), 0.0);
        assert_eq!(loudest(&[]), 0.0, "no samples is no signal, not a failure");
    }

    #[test]
    fn a_sample_above_full_scale_is_the_top_of_the_meter_rather_than_past_it() {
        // A float endpoint may deliver samples above 1.0, and the protocol
        // promises 0.0 to 1.0. Without the clamp a meter drawn as a percentage
        // of the width would run off the end of its own track.
        assert_eq!(loudest(&[1.9, -0.2]), 1.0);
    }

    #[test]
    fn a_sample_that_is_not_a_number_is_no_reading_rather_than_a_full_meter() {
        // `f32::max` already prefers the number over a `NaN`, so dropping the
        // filter would leave the two lines below passing. The infinity is the
        // one that needs it: without the filter it becomes the peak, the clamp
        // turns it into `1.0`, and the meter sits at the top for as long as the
        // driver keeps producing them — a reading that says the microphone is
        // clipping when what it means is that a sample was rubbish.
        assert!((loudest(&[f32::NAN, 0.4]) - 0.4).abs() < f32::EPSILON);
        assert!((loudest(&[0.4, f32::NAN]) - 0.4).abs() < f32::EPSILON);
        assert_eq!(
            loudest(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY]),
            0.0,
            "a buffer of nothing but rubbish is no reading, not a full meter"
        );
    }

    #[test]
    fn recording_no_microphone_has_no_level_rather_than_a_level_of_zero() {
        // `Off` is a setting somebody chose, not a device that could not be
        // found, and the two must not arrive at a screen looking the same: one
        // means "you asked for no microphone" and the other means "the one you
        // asked for is not there" (AGENTS.md section 27). Resolution is the
        // half of `microphone_level` that needs no endpoint, so it is the half
        // this can check.
        assert_eq!(
            selected_microphone(&AudioSourceSetting::Off).expect("Off resolves"),
            None,
        );
    }

    #[test]
    fn the_default_microphone_resolves_without_consulting_the_machine() {
        // `default` follows whichever endpoint Windows considers default at the
        // moment a capture opens, so resolving it must not enumerate: on a
        // machine with no capture endpoints at all, enumerating would fail and
        // a recording configured for `default` would be refused before the
        // capture had a chance to say anything.
        assert_eq!(
            selected_microphone(&AudioSourceSetting::SystemDefault).expect("the default resolves"),
            Some(clipped_audio::windows::MicrophoneSelection::SystemDefault),
        );
    }
}
