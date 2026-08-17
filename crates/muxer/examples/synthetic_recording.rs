//! Writes a synthetic multi-track recording, for testing the muxer.
//!
//! The muxer's job is to take encoded packets and produce a file. Proving it
//! does needs packets, and this example makes them the way a recording session
//! does: a moving test pattern uploaded to a Direct3D 11 texture and encoded to
//! H.264 with `clipped_encoder::SoftwareEncoder`
//! ([issue #18](https://github.com/wildware-uk/clipped/issues/18)), and a tone
//! per audio source as interleaved `f32` samples, written through the public
//! API of `clipped_muxer` exactly as a session would write them. Before issue
//! #159 this drove `libopenh264` itself, from before the workspace had any
//! encoder of its own to exercise instead
//! ([issue #15](https://github.com/wildware-uk/clipped/issues/15) onwards).
//!
//! The audio tracks are the product's own — the compatibility mix, game, other
//! system audio, microphone (`clipped_muxer::AudioSource`) — each carrying a
//! different frequency, with the mix carrying all of them at once. That is what
//! lets `tests/multi_track_audio.rs` prove the sources stayed apart by
//! listening, rather than by counting streams.
//!
//! It is an example rather than a test because two of the muxer's tests need it
//! as a *process*:
//!
//! - `tests/synthetic_recording.rs` runs it to completion and takes the file
//!   apart with the workspace's media harness (`tests/media`).
//! - `tests/abrupt_termination.rs` kills it in the middle and checks what
//!   survived, which is the claim ADR 0001 rests on and cannot be tested
//!   in-process: a Rust panic unwinds and runs destructors, and destructors are
//!   exactly what a killed recorder does not get.
//!
//! Progress is printed to standard output as `media_seconds=<n>` after every
//! packet, flushed, so that a test can kill the process at a known point in the
//! recording and compare what it asked for against what came back.
//!
//! ```text
//! cargo run -p clipped-muxer --example synthetic_recording -- --output demo.mkv --seconds 5
//! ```

use std::error::Error;
use std::ffi::c_void;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use clipped_encoder::{
    BitRate, Codec, DeviceKind, EncoderConfig, FrameRate as EncoderFrameRate, GraphicsDevice,
    KeyframeInterval, RateControl, Resolution, SoftwareEncoder, SourceFrame, SourceTexture,
    SurfaceFormat, SurfaceKind, VideoEncoder,
};
use clipped_muxer::{
    AudioSource, AudioTrack, AudioTrackWriter, EncodedPacket, FrameRate, Language, MkvWriter,
    PacketTimestamp, RecordingLayout, TrackId, VideoCodec, VideoTrack,
};
use windows::core::Interface as _;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

/// How much audio goes into one packet.
///
/// 20 milliseconds is the packet length Windows loopback capture delivers at,
/// and short enough that audio and video interleave the way they will in a real
/// recording rather than in one big lump per second.
const AUDIO_PACKET: Duration = Duration::from_millis(20);

/// The bit rate the software encoder is configured for.
///
/// A round number well inside what a 640x360 test pattern needs, chosen for
/// consistency with the picture this example draws rather than for realism —
/// nothing here is trying to say what a real recording is configured for.
const BIT_RATE_MEGABITS_PER_SECOND: u32 = 2;

#[derive(Debug, Parser)]
#[command(
    about = "Writes a synthetic multi-track MKV recording",
    long_about = "Encodes a moving test pattern to H.264 with clipped_encoder::SoftwareEncoder, \
                  generates tones as PCM, and writes both through clipped-muxer. Used by the \
                  muxer's own tests, including the one that kills this process mid-recording."
)]
struct Arguments {
    /// Where to write the recording. Must not already exist.
    #[arg(long)]
    output: PathBuf,

    /// How many seconds of media to write before finishing the file.
    #[arg(long, default_value_t = 3.0)]
    seconds: f64,

    /// Write in real time, rather than as fast as the encoder manages.
    ///
    /// What the abrupt-termination test uses, so that "killed after two
    /// seconds" means two seconds of recording.
    #[arg(long)]
    pace: bool,

    /// Picture width in pixels.
    #[arg(long, default_value_t = 640)]
    width: u32,

    /// Picture height in pixels.
    #[arg(long, default_value_t = 360)]
    height: u32,

    /// Frames per second.
    #[arg(long, default_value_t = 30)]
    frame_rate: u32,

    /// How many audio tracks to write, each carrying a different tone.
    ///
    /// They are the product's own tracks, in the product's own order: the
    /// compatibility mix, then game, other system audio, microphone, and
    /// application tracks after that (SPEC.md sections 11 and 13).
    #[arg(long, default_value_t = 2)]
    audio_tracks: u16,

    /// Which audio track a player should choose on its own.
    ///
    /// Zero — the compatibility mix — is what a real recording marks (SPEC.md
    /// section 13) and what the track model produces on its own. Any other value
    /// is worth being able to write because a container that simply enables the
    /// *first* track of each kind produces the same file as one that carried the
    /// flag, so a test of the flag needs a recording whose default track is not
    /// the first one (`crates/muxer/tests/mp4_remux.rs`).
    #[arg(long, default_value_t = 0)]
    default_audio_track: u16,

    /// A language tag to put on every audio track, as `eng`.
    ///
    /// Left off by default, because the track model does not guess one: game
    /// audio has no language and a microphone's is a fact about the person
    /// speaking (`clipped_muxer::AudioTrack::for_source`). It can be set because
    /// a language only survives a change of container *visibly* when there is
    /// one — Matroska omits the element for an unknown language and MP4 writes
    /// `und`, so a recording that stated nothing proves nothing about whether
    /// the tag was carried (`crates/muxer/tests/mp4_remux.rs`).
    #[arg(long)]
    audio_language: Option<String>,

    /// How far into the recording the audio starts, in milliseconds.
    ///
    /// Zero writes a recording whose tracks all begin together, which is what a
    /// real one does. A non-zero value is the case a remux has to preserve: the
    /// audio track's own timeline starts later than the video's, and a copy that
    /// rebased each track onto its own first packet would silently pull the
    /// sound forward by this much (`crates/muxer/tests/mp4_remux.rs`).
    #[arg(long, default_value_t = 0)]
    audio_offset_ms: u64,

    /// How many seconds apart the keyframes are.
    ///
    /// Worth being able to change: a keyframe is one of the three things that
    /// close a Matroska cluster — the others are a size limit and a time limit
    /// — so with FFmpeg's defaults it is part of what decides how much an
    /// abrupt termination costs. A recorder using a long keyframe interval is
    /// the case the muxer's cluster time limit exists for.
    #[arg(long, default_value_t = 1.0)]
    keyframe_seconds: f64,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();

    match record(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let mut cause: Option<&dyn Error> = Some(error.as_ref());
            while let Some(error) = cause {
                eprintln!("error: {error}");
                cause = error.source();
            }
            ExitCode::FAILURE
        }
    }
}

/// Writes the recording described by `arguments`.
fn record(arguments: &Arguments) -> Result<(), Box<dyn Error>> {
    let frame_rate = FrameRate::per_second(arguments.frame_rate)
        .ok_or("a recording needs a frame rate of at least one frame per second")?;
    let mut encoder = SoftwareVideoEncoder::open(
        arguments.width,
        arguments.height,
        arguments.frame_rate,
        arguments.keyframe_seconds,
    )?;

    let video = VideoTrack::new(VideoCodec::H264, arguments.width, arguments.height)
        .with_frame_rate(frame_rate)
        .with_codec_private(encoder.codec_private())
        .with_name("Gameplay");

    let language = arguments
        .audio_language
        .as_deref()
        .map(Language::new)
        .transpose()?;

    let mut layout = RecordingLayout::new(video);
    for index in 0..arguments.audio_tracks {
        // Named, ordered and flagged by the track model rather than here
        // (`clipped_muxer::audio`), which is the point: this example writes the
        // tracks a real recording writes. The declarations go in in order only
        // because that is easiest to read — the layout would put them in the
        // same order whatever order they arrived in.
        let mut track = AudioTrack::for_source(source_for(index), SAMPLE_RATE, CHANNELS)
            .with_default_flag(index == arguments.default_audio_track);
        if let Some(language) = language {
            track = track.with_language(language);
        }
        layout = layout.with_audio_track(track);
    }

    let mut writer = MkvWriter::create(&arguments.output, &layout)?;
    let mut audio = ToneGenerator::new(&layout)?;

    let frame_interval = Duration::from_secs(1) / arguments.frame_rate;
    // Counted rather than compared against a running total, so that
    // `--seconds 4` is exactly four seconds of video and exactly four seconds
    // of audio. A frame interval is not a whole number of nanoseconds at most
    // frame rates, so a loop that ran until the clock passed four seconds would
    // write a frame more or less depending on the rate, and the tests would be
    // asserting on the rounding rather than on the recording.
    let frames = (arguments.seconds * f64::from(arguments.frame_rate)).round() as u64;
    let audio_packets =
        (arguments.seconds * 1000.0 / AUDIO_PACKET.as_millis() as f64).round() as u64;

    let started = Instant::now();
    let audio_offset = Duration::from_millis(arguments.audio_offset_ms);
    let mut next_frame = 0_u64;
    let mut next_audio_packet = 0_u64;
    let mut output = io::stdout().lock();

    // Video and audio are produced in lockstep, earliest first, which is what
    // the two threads of a real recorder approximate and what keeps the
    // interleaving queue inside libavformat shallow.
    loop {
        let video_at = frame_interval * u32::try_from(next_frame)?;
        let audio_at = audio_offset + AUDIO_PACKET * u32::try_from(next_audio_packet)?;
        let (video_left, audio_left) = (next_frame < frames, next_audio_packet < audio_packets);
        let next_at = match (video_left, audio_left) {
            (true, true) => video_at.min(audio_at),
            (true, false) => video_at,
            (false, true) => audio_at,
            (false, false) => break,
        };

        if arguments.pace {
            if let Some(wait) = next_at.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
        }

        if video_left && (video_at <= audio_at || !audio_left) {
            encoder.encode_frame(next_frame, frame_interval, &mut writer)?;
            next_frame += 1;
        } else {
            audio.write_packet(next_audio_packet, audio_offset, &mut writer)?;
            next_audio_packet += 1;
        }

        // Flushed every time: the test that kills this process reads the last
        // line it printed and compares it against what survived in the file,
        // which only means anything if the line reached the pipe before the
        // kill.
        writeln!(output, "media_seconds={:.3}", next_at.as_secs_f64())?;
        output.flush()?;
    }

    encoder.flush(frame_interval, &mut writer)?;
    let summary = writer.finish()?;
    writeln!(output, "finished packets={} {summary}", summary.packets)?;
    output.flush()?;

    Ok(())
}

/// The sampling rate the tones are generated at: what Windows shared-mode audio
/// runs at.
const SAMPLE_RATE: u32 = 48_000;

/// Stereo, as every Windows output endpoint mixes at.
const CHANNELS: u16 = 2;

/// How loud each source's tone is, well below full scale so that the
/// compatibility mix — which is the sum of them — does not clip.
const AMPLITUDE: f32 = 0.4;

/// Which source track `index` carries.
///
/// The first four are the model's own (SPEC.md section 11); anything beyond them
/// is an application track, which is what the model has any number of.
fn source_for(index: u16) -> AudioSource {
    match index {
        0 => AudioSource::CompatibilityMix,
        1 => AudioSource::Game,
        2 => AudioSource::OtherSystemAudio,
        3 => AudioSource::Microphone,
        other => AudioSource::application(format!("Application {other}")),
    }
}

/// The tone one track carries, in hertz, or [`None`] for the compatibility mix.
///
/// 440 Hz for the game, 880 Hz for other system audio and 1320 Hz for the
/// microphone, which are AGENTS.md section 26's own frequencies: a test that
/// proves the tracks stayed separate has to be able to tell them apart by
/// listening, and identical tracks would hide a writer that sent the same
/// packets to every stream.
fn tone_for(index: u16) -> Option<f64> {
    (index > 0).then(|| 440.0 * f64::from(index))
}

/// Generates a different tone for each audio track, and their mix for the first.
///
/// Not a generator so much as a stand-in for a set of captures: it produces
/// interleaved `f32` samples exactly as `clipped-audio` does
/// (`docs/audio-routing.md`) and hands them to the muxer's own
/// [`AudioTrackWriter`], so the path this example exercises is the path a
/// recording session takes.
struct ToneGenerator {
    tracks: Vec<TrackTone>,
    /// Reused between packets, so the generator is not the thing that makes
    /// this example slow.
    samples: Vec<f32>,
}

/// One track, and what it is playing.
struct TrackTone {
    writer: AudioTrackWriter,
    /// The frequency this track carries, or [`None`] for the compatibility mix,
    /// which carries every other track's tone at once.
    frequency: Option<f64>,
}

impl ToneGenerator {
    /// Prepares a writer for every audio track `layout` declares.
    fn new(layout: &RecordingLayout) -> Result<Self, Box<dyn Error>> {
        let mut tracks = Vec::new();
        for (index, declared) in layout.audio_tracks().iter().enumerate() {
            let index = u16::try_from(index)?;
            tracks.push(TrackTone {
                writer: AudioTrackWriter::new(TrackId::Audio(index), declared)?,
                frequency: tone_for(index),
            });
        }

        Ok(Self {
            tracks,
            samples: Vec::new(),
        })
    }

    /// Writes packet `index` of every track, `offset` into the recording.
    fn write_packet(
        &mut self,
        index: u64,
        offset: Duration,
        writer: &mut MkvWriter,
    ) -> Result<(), Box<dyn Error>> {
        let frames_per_packet = (SAMPLE_RATE as u64 * AUDIO_PACKET.as_millis() as u64) / 1000;
        let first_frame = index * frames_per_packet;
        // The offset moves the packet on the recording's timeline and not the
        // tone inside it: the audio starts later, rather than starting on time
        // and being labelled wrongly.
        let timestamp = PacketTimestamp::from_nanos(
            i64::try_from(first_frame * 1_000_000_000 / u64::from(SAMPLE_RATE))?
                + i64::try_from(offset.as_nanos())?,
        );

        // The mix carries every other track's tone, averaged so that the sum
        // stays inside full scale. That is what makes a recording written here
        // worth asserting isolation against: the tones are all present in the
        // file, and every other track has to hold exactly one of them.
        let voices: Vec<f64> = self
            .tracks
            .iter()
            .filter_map(|track| track.frequency)
            .collect();

        for track in &mut self.tracks {
            self.samples.clear();
            for frame in 0..frames_per_packet {
                let seconds = (first_frame + frame) as f64 / f64::from(SAMPLE_RATE);
                let amplitude = match track.frequency {
                    Some(frequency) => sine(seconds, frequency),
                    None if voices.is_empty() => 0.0,
                    // A one-track recording has nothing to mix, so the mix is
                    // silent rather than invented.
                    None => {
                        voices
                            .iter()
                            .map(|voice| sine(seconds, *voice))
                            .sum::<f32>()
                            / voices.len() as f32
                    }
                };
                // Both channels the same: what is being tested here is which
                // track the sound is on, not where it sits in the stereo image.
                for _ in 0..CHANNELS {
                    self.samples.push(amplitude);
                }
            }

            track
                .writer
                .write_samples(writer, timestamp, &self.samples)?;
        }

        Ok(())
    }
}

/// One sample of a sine at `frequency`, `seconds` into the recording.
fn sine(seconds: f64, frequency: f64) -> f32 {
    (seconds * frequency * std::f64::consts::TAU).sin() as f32 * AMPLITUDE
}

/// Draws the test pattern into a Direct3D 11 texture and encodes it with
/// `clipped_encoder::SoftwareEncoder`.
///
/// This is not the encoder Clipped will record with — that is hardware, and it
/// is `clipped-encoder`'s work — but it is the same software fallback a machine
/// with no usable encoding hardware records with (issue #18), driven the way a
/// recording session drives it: frames arrive as Direct3D textures, not as
/// pixels this crate hands FFmpeg directly.
///
/// # Ownership
///
/// Owns the immediate context, one texture reused for every frame, and the
/// encoding session. `encoder` releases its own session on `Drop`; the
/// Direct3D types release their COM references the same way, so nothing here
/// needs a `Drop` implementation of its own.
struct SoftwareVideoEncoder {
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
    encoder: SoftwareEncoder,
    width: u32,
    height: u32,
    /// The picture being built for the frame about to be submitted, as BGRA8
    /// bytes: reused between frames so that drawing one is not an allocation.
    pixels: Vec<u8>,
}

impl SoftwareVideoEncoder {
    /// Opens a Direct3D 11 device and a software encoding session against it,
    /// for pictures of `width` by `height` at `frame_rate`, with a keyframe
    /// every `keyframe_seconds`.
    fn open(
        width: u32,
        height: u32,
        frame_rate: u32,
        keyframe_seconds: f64,
    ) -> Result<Self, Box<dyn Error>> {
        let device = open_device()?;
        // SAFETY: `device` is a live Direct3D 11 device. `GetImmediateContext`
        // hands back a reference the returned context owns, which is why
        // nothing here needs to keep `device` itself alive afterwards: the
        // context and the texture below each hold their own reference to the
        // device internally, as every Direct3D 11 child object does, and
        // `SoftwareEncoder::open` takes a reference of its own too.
        let context = unsafe { device.GetImmediateContext() }?;

        let description = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            #[allow(clippy::cast_sign_loss)]
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: the description is a live local; no initial data is given,
        // because the first call to `draw` fills the pixel buffer this uploads
        // before anything reads the texture; `texture` is a live out-parameter
        // that receives the one reference `CreateTexture2D` returns.
        unsafe { device.CreateTexture2D(&description, None, Some(&mut texture)) }?;
        let texture = texture.ok_or("CreateTexture2D reported success without a texture")?;

        let frame_rate = EncoderFrameRate::whole(frame_rate);
        let config = EncoderConfig::new(
            Codec::H264,
            Resolution::new(width, height),
            frame_rate,
            RateControl::constant(BitRate::megabits_per_second(BIT_RATE_MEGABITS_PER_SECOND)),
        )
        .with_keyframe_interval(KeyframeInterval::every(
            Duration::from_secs_f64(keyframe_seconds),
            frame_rate,
        ));

        // SAFETY: `device` is a live Direct3D 11 device, owned by this
        // function and kept alive by the local above for the whole of this
        // call, which is all `open` needs of it: it takes its own reference
        // before returning.
        let graphics_device = unsafe { GraphicsDevice::new(DeviceKind::D3d11, device.as_raw()) };
        let encoder = SoftwareEncoder::open(&graphics_device, config)?;

        Ok(Self {
            context,
            texture,
            encoder,
            width,
            height,
            pixels: vec![0_u8; width as usize * height as usize * 4],
        })
    }

    /// The sequence and picture parameter sets the encoder produced, for the
    /// container to store.
    fn codec_private(&self) -> Vec<u8> {
        self.encoder.parameter_sets().to_vec()
    }

    /// Draws frame `index` of the test pattern, uploads it and encodes it,
    /// writing whatever packets come out.
    fn encode_frame(
        &mut self,
        index: u64,
        frame_interval: Duration,
        writer: &mut MkvWriter,
    ) -> Result<(), Box<dyn Error>> {
        self.draw(index);

        // SAFETY: `self.texture` was created above for exactly this width,
        // height and format, so subresource 0 is the whole of it; the box is
        // null, meaning the whole resource; and `self.pixels` holds
        // `width * height * 4` bytes, which is what the row pitch below
        // declares.
        unsafe {
            self.context.UpdateSubresource(
                &self.texture,
                0,
                None,
                self.pixels.as_ptr().cast::<c_void>(),
                self.width * 4,
                0,
            );
            // Reaches the GPU now rather than whenever the driver next
            // flushes, so that the copy `submit` makes below reads this frame
            // and not the one before it.
            self.context.Flush();
        }

        let presentation_time = frame_interval * u32::try_from(index)?;
        // SAFETY: the texture is live for the whole of this call and was
        // created on the device the encoder was opened against; the frame's
        // lifetime ties the borrow to it, and nothing derived from it is kept
        // once `submit` returns.
        let surface =
            unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, self.texture.as_raw()) };
        let frame = SourceFrame::new(
            surface,
            SurfaceFormat::Bgra8Unorm,
            Resolution::new(self.width, self.height),
            presentation_time,
        );
        self.encoder.submit(&frame)?;

        self.drain(frame_interval, writer)
    }

    /// Ends the stream and writes the packets the encoder was still holding.
    fn flush(
        &mut self,
        frame_interval: Duration,
        writer: &mut MkvWriter,
    ) -> Result<(), Box<dyn Error>> {
        self.encoder.finish()?;
        self.drain(frame_interval, writer)
    }

    /// Writes every packet the encoder has ready, giving each the same
    /// duration since the software encoder does not report one of its own.
    fn drain(
        &mut self,
        frame_interval: Duration,
        writer: &mut MkvWriter,
    ) -> Result<(), Box<dyn Error>> {
        while let Some(packet) = self.encoder.next_packet()? {
            let nanos = |duration: Duration| -> Result<PacketTimestamp, Box<dyn Error>> {
                Ok(PacketTimestamp::from_nanos(i64::try_from(
                    duration.as_nanos(),
                )?))
            };

            writer.write_packet(
                &EncodedPacket::new(
                    TrackId::Video,
                    nanos(packet.presentation_time())?,
                    packet.data(),
                )
                .with_decode_timestamp(nanos(packet.decode_time())?)
                .with_duration(frame_interval)
                .with_keyframe(packet.is_keyframe()),
            )?;
        }

        Ok(())
    }

    /// Draws frame `index` into [`pixels`](Self::pixels): a bright block
    /// travelling left to right over a background that changes shade, so that
    /// a decoded frame can be told from its neighbours by eye.
    fn draw(&mut self, index: u64) {
        let (width, height) = (self.width as usize, self.height as usize);
        let block = (index as usize * 8) % width.max(1);

        for row in 0..height {
            for column in 0..width {
                let bright = column.abs_diff(block) < 24;
                let value = if bright {
                    235
                } else {
                    16 + ((row + index as usize) % 64) as u8
                };
                let at = (row * width + column) * 4;
                // BGRA, which is what `DXGI_FORMAT_B8G8R8A8_UNORM` stores and
                // what the encoder was configured to read
                // (`SurfaceFormat::Bgra8Unorm`). Grey rather than coloured, so
                // that the luma value written here is the value that lands in
                // the YUV picture the encoder codes.
                self.pixels[at] = value;
                self.pixels[at + 1] = value;
                self.pixels[at + 2] = value;
                self.pixels[at + 3] = 255;
            }
        }
    }
}

/// Opens a Direct3D 11 device: real graphics hardware if there is one, the WARP
/// software rasteriser otherwise.
///
/// WARP ships with Windows, so unlike opening a hardware *encoder* there is
/// nothing here worth doing without: if neither driver type produces a device,
/// Direct3D itself is broken on this machine, and every muxer test that runs
/// this example would be asserting on a recording that was never made. Tries
/// the same two driver types in the same order as
/// `crates/encoder/src/software/tests.rs`'s `TestGpu`, which is what lets that
/// crate's own encoding tests run on a hosted CI runner with no GPU.
fn open_device() -> Result<ID3D11Device, Box<dyn Error>> {
    for kind in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        if let Some(device) = try_open_device(kind) {
            return Ok(device);
        }
    }

    Err("no Direct3D 11 device could be created, on hardware or on WARP".into())
}

/// Tries to create a device of one driver type, answering [`None`] rather than
/// an error: the caller tries another type next, and only the failure of every
/// type is worth reporting.
fn try_open_device(kind: D3D_DRIVER_TYPE) -> Option<ID3D11Device> {
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: no adapter is named, which is what these driver types require;
    // the module handle is unused for them; the feature level list and the
    // out-parameter are live locals; `D3D11_SDK_VERSION` is the constant the
    // header requires. On success `device` holds one reference, released when
    // it is dropped.
    unsafe {
        D3D11CreateDevice(
            None,
            kind,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .ok()?;
    device
}
