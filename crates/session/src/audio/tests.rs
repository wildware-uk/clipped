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
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use clipped_audio::{AudioTimestamp, CapturedAudio, ChannelMask, SampleFormat, SampleOrigin};
use clipped_capture::{CaptureTimestamp, MediaTime, SourceClock, SyncState};
use clipped_media_validation::{
    require_media_tools, AudioStream, Media, TemporaryDirectory, Tone, VideoStream,
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
    /// The buffer last handed out, which owns the samples it lends — the same
    /// lifetime a real capture's converted packet has.
    current: Option<ScriptedBuffer>,
    /// Raised when the script has run out, so a test can wait for the thread to
    /// have consumed everything rather than sleeping for a guess.
    exhausted: Arc<AtomicBool>,
}

impl AudioCapture for ScriptedCapture {
    fn read(&mut self, _timeout: Duration) -> Result<Capture<'_>, AudioError> {
        self.current = self.ready.pop_front();
        let Some(buffer) = self.current.as_ref() else {
            // A script that has run out stands for an endpoint with nothing to
            // report, which is what a real one returns between packets.
            self.exhausted.store(true, Ordering::Relaxed);
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

    fn close(&mut self) {}
}

/// A source scripted to hand over `buffers`, and the flag raised when it has.
struct Scripted {
    source: OpenSource,
    exhausted: Arc<AtomicBool>,
}

fn scripted(source: AudioSource, channels: u16, buffers: Vec<ScriptedBuffer>) -> Scripted {
    let exhausted = Arc::new(AtomicBool::new(false));
    Scripted {
        source: OpenSource {
            source,
            device: Some("a scripted endpoint".to_owned()),
            format: format(channels),
            capture: Box::new(ScriptedCapture {
                format: format(channels),
                ready: buffers.into_iter().collect(),
                current: None,
                exhausted: Arc::clone(&exhausted),
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
    let exhausted: Vec<Arc<AtomicBool>> = sources
        .iter()
        .map(|scripted| Arc::clone(&scripted.exhausted))
        .collect();
    let sources: Vec<OpenSource> = sources
        .into_iter()
        .map(|scripted| scripted.source)
        .collect();

    let layout = declare(video.track(), &sources);
    let writer = MkvWriter::create(path, &layout).expect("the recording can be created");
    let muxing = MuxingThread::start(writer, SpaceGuard::new(path, 0), &layout)
        .expect("every declared track can be written to");

    let mut threads = AudioThreads::start(sources, &layout, clock(), &muxing, None);

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
    );
    assert!(layout.audio_tracks()[0].is_default());
}

#[test]
fn a_video_only_recording_declares_no_audio_track_at_all() {
    let layout = declare(VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT), &[]);
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
