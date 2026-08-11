//! Writes a synthetic multi-track recording, for testing the muxer.
//!
//! The muxer's job is to take encoded packets and produce a file. Proving it
//! does needs packets, and the encoders that will produce them in the real
//! pipeline are separate pieces of work
//! ([issue #15](https://github.com/wildware-uk/clipped/issues/15) onwards). So
//! this example makes its own: a moving test pattern encoded to H.264 with the
//! pinned FFmpeg build's own software encoder, and two tones as uncompressed
//! PCM, written through the public API of `clipped_muxer` exactly as a session
//! would write them.
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
use std::ffi::c_int;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr;
use std::time::{Duration, Instant};

use clap::Parser;
use clipped_muxer::{
    AudioCodec, AudioTrack, EncodedPacket, FrameRate, Language, MkvWriter, PacketTimestamp,
    RecordingLayout, TrackId, VideoCodec, VideoTrack,
};
use rusty_ffmpeg::ffi;

/// FFmpeg's `AVERROR_EOF`, which is `FFERRTAG('E','O','F',' ')`.
///
/// The binding does not carry it: it is a macro over another macro, and
/// `bindgen` expands neither.
const AVERROR_EOF: c_int = -(0x20_46_4F_45);

/// FFmpeg's `AVERROR(EAGAIN)`: the encoder has taken the frame and has no
/// packet ready yet.
const AVERROR_EAGAIN: c_int = -(ffi::EAGAIN as c_int);

/// FFmpeg's `AV_NOPTS_VALUE`, the timestamp that means "there is none".
const AV_NOPTS_VALUE: i64 = i64::MIN;

/// The software H.264 encoder in the pinned LGPL build.
///
/// Not libx264, which is GPL and is not in the build Clipped links against
/// (`docs/adr/0004-ffmpeg-dependency-strategy.md`).
const SOFTWARE_ENCODER: &std::ffi::CStr = c"libopenh264";

/// How much audio goes into one packet.
///
/// 20 milliseconds is the packet length Windows loopback capture delivers at,
/// and short enough that audio and video interleave the way they will in a real
/// recording rather than in one big lump per second.
const AUDIO_PACKET: Duration = Duration::from_millis(20);

#[derive(Debug, Parser)]
#[command(
    about = "Writes a synthetic multi-track MKV recording",
    long_about = "Encodes a moving test pattern to H.264 with the pinned FFmpeg build's \
                  software encoder, generates tones as PCM, and writes both through \
                  clipped-muxer. Used by the muxer's own tests, including the one that \
                  kills this process mid-recording."
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
    #[arg(long, default_value_t = 2)]
    audio_tracks: u16,

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
    let keyframe_interval = (arguments.keyframe_seconds * f64::from(arguments.frame_rate)) as u32;
    let mut encoder = SoftwareVideoEncoder::open(
        arguments.width,
        arguments.height,
        arguments.frame_rate,
        keyframe_interval.max(1),
    )?;

    let video = VideoTrack::new(VideoCodec::H264, arguments.width, arguments.height)
        .with_frame_rate(frame_rate)
        .with_codec_private(encoder.codec_private())
        .with_name("Gameplay");

    let mut layout = RecordingLayout::new(video);
    for index in 0..arguments.audio_tracks {
        // The names the product uses (SPEC.md sections 11 and 13). The first
        // track is the one a player should pick on its own.
        let name = match index {
            0 => "Compatibility Mix",
            1 => "Game",
            2 => "Other System Audio",
            _ => "Microphone",
        };
        let mut track = AudioTrack::new(AudioCodec::PcmS16Le, SAMPLE_RATE, 2)
            .with_name(name)
            .with_language(Language::ENGLISH);
        if index == 0 {
            track = track.as_default();
        }
        layout = layout.with_audio_track(track);
    }

    let mut writer = MkvWriter::create(&arguments.output, &layout)?;
    let mut audio = ToneGenerator::new(arguments.audio_tracks);

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
    let mut next_frame = 0_u64;
    let mut next_audio_packet = 0_u64;
    let mut output = io::stdout().lock();

    // Video and audio are produced in lockstep, earliest first, which is what
    // the two threads of a real recorder approximate and what keeps the
    // interleaving queue inside libavformat shallow.
    loop {
        let video_at = frame_interval * u32::try_from(next_frame)?;
        let audio_at = AUDIO_PACKET * u32::try_from(next_audio_packet)?;
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
            audio.write_packet(next_audio_packet, &mut writer)?;
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

/// Generates a different tone for each audio track.
///
/// Different frequencies rather than the same one on every track, because a
/// test that proves the tracks are separate has to be able to tell them apart
/// (AGENTS.md section 21), and because identical tracks would hide a muxer that
/// wrote the same packets to every stream.
struct ToneGenerator {
    tracks: u16,
    /// Reused between packets, so the generator is not the thing that makes
    /// this example slow.
    bytes: Vec<u8>,
}

impl ToneGenerator {
    fn new(tracks: u16) -> Self {
        Self {
            tracks,
            bytes: Vec::new(),
        }
    }

    /// Writes packet `index` of every track.
    fn write_packet(&mut self, index: u64, writer: &mut MkvWriter) -> Result<(), Box<dyn Error>> {
        let frames_per_packet = (SAMPLE_RATE as u64 * AUDIO_PACKET.as_millis() as u64) / 1000;
        let first_frame = index * frames_per_packet;
        let timestamp = PacketTimestamp::from_nanos(i64::try_from(
            first_frame * 1_000_000_000 / u64::from(SAMPLE_RATE),
        )?);

        for track in 0..self.tracks {
            // 440 Hz on the first track, 660 Hz on the second, and so on.
            let frequency = 440.0 * (1.0 + f64::from(track) * 0.5);
            self.bytes.clear();
            for frame in 0..frames_per_packet {
                let seconds = (first_frame + frame) as f64 / f64::from(SAMPLE_RATE);
                let amplitude =
                    (seconds * frequency * std::f64::consts::TAU).sin() * f64::from(i16::MAX) * 0.4;
                // Stereo, both channels the same.
                let sample = (amplitude as i16).to_le_bytes();
                self.bytes.extend_from_slice(&sample);
                self.bytes.extend_from_slice(&sample);
            }

            writer.write_packet(
                &EncodedPacket::new(TrackId::Audio(track), timestamp, &self.bytes)
                    .with_duration(AUDIO_PACKET)
                    // Every PCM packet is independently decodable.
                    .with_keyframe(true),
            )?;
        }

        Ok(())
    }
}

/// The pinned build's software H.264 encoder, wrapped just enough to produce
/// packets.
///
/// This is not the encoder Clipped will record with — that is hardware, and it
/// is `clipped-encoder`'s work — and nothing here should be mistaken for the
/// beginnings of one. It exists so the muxer has real bitstream to write.
///
/// # Ownership
///
/// The three pointers are null until they are allocated and are never anything
/// but null or live, which is what makes `Drop` correct on every path out of
/// [`open`](Self::open): FFmpeg's three free functions all accept a pointer to
/// a null pointer and do nothing. Once `open` has returned a value, all three
/// are live.
struct SoftwareVideoEncoder {
    context: *mut ffi::AVCodecContext,
    frame: *mut ffi::AVFrame,
    packet: *mut ffi::AVPacket,
    width: u32,
    height: u32,
}

impl SoftwareVideoEncoder {
    /// Opens the encoder for pictures of `width` by `height` at `frame_rate`,
    /// with a keyframe every `keyframe_interval` frames.
    fn open(
        width: u32,
        height: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Result<Self, Box<dyn Error>> {
        // SAFETY: `avcodec_find_encoder_by_name` reads the NUL-terminated name
        // and returns either null or a pointer to a static descriptor inside
        // libavcodec.
        let codec = unsafe { ffi::avcodec_find_encoder_by_name(SOFTWARE_ENCODER.as_ptr()) };
        if codec.is_null() {
            return Err(format!(
                "the linked FFmpeg has no {} encoder, so this example cannot produce video; \
                 see docs/ffmpeg.md",
                SOFTWARE_ENCODER.to_string_lossy()
            )
            .into());
        }

        // SAFETY: `codec` is a live descriptor, and this returns either null or
        // a context this value then owns and frees in `Drop`. Everything
        // allocated from here on is held by `encoder`, so every early return
        // below releases it.
        let context = unsafe { ffi::avcodec_alloc_context3(codec) };
        let mut encoder = Self {
            context,
            frame: ptr::null_mut(),
            packet: ptr::null_mut(),
            width,
            height,
        };
        if encoder.context.is_null() {
            return Err("there was not enough memory for an encoder context".into());
        }

        // SAFETY: `encoder.context` is live and exclusively owned, and every
        // field assigned is one the caller is expected to fill in before
        // `avcodec_open2`.
        unsafe {
            let context = encoder.context;
            (*context).width = c_int::try_from(width)?;
            (*context).height = c_int::try_from(height)?;
            (*context).pix_fmt = ffi::AV_PIX_FMT_YUV420P;
            (*context).time_base = ffi::AVRational {
                num: 1,
                den: c_int::try_from(frame_rate)?,
            };
            (*context).framerate = ffi::AVRational {
                num: c_int::try_from(frame_rate)?,
                den: 1,
            };
            (*context).bit_rate = 2_000_000;
            (*context).gop_size = c_int::try_from(keyframe_interval)?;
            // Puts the sequence header in `extradata` instead of in front of
            // every keyframe, which is where the container wants it.
            (*context).flags |= ffi::AV_CODEC_FLAG_GLOBAL_HEADER as c_int;
        }

        // SAFETY: the context was allocated for this codec and has not been
        // opened. Passing a null options dictionary uses the encoder's
        // defaults.
        let code = unsafe { ffi::avcodec_open2(encoder.context, codec, ptr::null_mut()) };
        if code < 0 {
            return Err(
                format!("the software encoder would not open (FFmpeg error {code})").into(),
            );
        }

        // SAFETY: takes no arguments and returns null on failure; ownership of
        // what it returns passes to `encoder`, which frees it.
        encoder.frame = unsafe { ffi::av_frame_alloc() };
        // SAFETY: as above.
        encoder.packet = unsafe { ffi::av_packet_alloc() };
        if encoder.frame.is_null() || encoder.packet.is_null() {
            return Err("there was not enough memory for a frame and a packet".into());
        }

        // SAFETY: the frame is live and empty. Its buffers are allocated for
        // the size and format set here and are freed with the frame.
        unsafe {
            let frame = encoder.frame;
            (*frame).width = c_int::try_from(width)?;
            (*frame).height = c_int::try_from(height)?;
            (*frame).format = ffi::AV_PIX_FMT_YUV420P;
            let code = ffi::av_frame_get_buffer(frame, 0);
            if code < 0 {
                return Err(format!("a picture buffer could not be allocated ({code})").into());
            }
        }

        Ok(encoder)
    }

    /// The sequence header the encoder produced, for the container to store.
    fn codec_private(&self) -> Vec<u8> {
        // SAFETY: the context is live and open. `extradata` is either null or
        // points at `extradata_size` bytes the context owns; the slice is
        // copied here and not kept.
        unsafe {
            let context = self.context;
            let size = usize::try_from((*context).extradata_size).unwrap_or(0);
            if (*context).extradata.is_null() || size == 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts((*context).extradata, size).to_vec()
        }
    }

    /// Draws frame `index` of the test pattern, encodes it and writes whatever
    /// comes out.
    fn encode_frame(
        &mut self,
        index: u64,
        frame_interval: Duration,
        writer: &mut MkvWriter,
    ) -> Result<(), Box<dyn Error>> {
        // SAFETY: the frame is live and its buffers were allocated in `open`.
        // `av_frame_make_writable` replaces them if anything else still holds a
        // reference, which the encoder does while a frame is queued.
        let code = unsafe { ffi::av_frame_make_writable(self.frame) };
        if code < 0 {
            return Err(format!("a picture buffer could not be made writable ({code})").into());
        }
        self.draw(index);

        // SAFETY: the frame is live, writable, and describes a picture the
        // encoder was opened for.
        unsafe {
            (*self.frame).pts = i64::try_from(index)?;
        }

        // SAFETY: both pointers are live and the context is open for encoding.
        let code = unsafe { ffi::avcodec_send_frame(self.context, self.frame) };
        if code < 0 {
            return Err(format!("the encoder refused a frame ({code})").into());
        }

        self.drain(frame_interval, writer)
    }

    /// Ends the stream and writes the packets the encoder was still holding.
    fn flush(
        &mut self,
        frame_interval: Duration,
        writer: &mut MkvWriter,
    ) -> Result<(), Box<dyn Error>> {
        // SAFETY: a null frame is the documented way to tell an encoder that no
        // more input is coming.
        let code = unsafe { ffi::avcodec_send_frame(self.context, ptr::null()) };
        if code < 0 {
            return Err(format!("the encoder refused the end of the stream ({code})").into());
        }
        self.drain(frame_interval, writer)
    }

    /// Writes every packet the encoder has ready.
    fn drain(
        &mut self,
        frame_interval: Duration,
        writer: &mut MkvWriter,
    ) -> Result<(), Box<dyn Error>> {
        loop {
            // SAFETY: both pointers are live; the packet is unreferenced at the
            // end of each iteration, so it is empty on entry.
            let code = unsafe { ffi::avcodec_receive_packet(self.context, self.packet) };
            if code == AVERROR_EAGAIN || code == AVERROR_EOF {
                return Ok(());
            }
            if code < 0 {
                return Err(format!("the encoder failed to produce a packet ({code})").into());
            }

            // SAFETY: a packet returned by `avcodec_receive_packet` holds
            // `size` bytes at `data`, owned by the packet, which is not touched
            // again until the borrow ends at `av_packet_unref` below.
            let (data, presentation, decode, keyframe) = unsafe {
                let packet = self.packet;
                let size = usize::try_from((*packet).size)?;
                (
                    std::slice::from_raw_parts((*packet).data, size),
                    (*packet).pts,
                    (*packet).dts,
                    ((*packet).flags & ffi::AV_PKT_FLAG_KEY as c_int) != 0,
                )
            };

            // The encoder counts in frames, because that is the time base it
            // was opened with. The muxer counts in nanoseconds on the
            // recording's clock, which here is a clock that starts at zero.
            let interval_nanos = i64::try_from(frame_interval.as_nanos())?;
            let nanos = |ticks: i64| PacketTimestamp::from_nanos(ticks * interval_nanos);
            // libopenh264 does not reorder, so this is always the presentation
            // timestamp; taken from the packet anyway, because an encoder that
            // did reorder would be silently mistimed otherwise.
            let decode = if decode == AV_NOPTS_VALUE {
                presentation
            } else {
                decode
            };

            writer.write_packet(
                &EncodedPacket::new(TrackId::Video, nanos(presentation), data)
                    .with_decode_timestamp(nanos(decode))
                    .with_duration(frame_interval)
                    .with_keyframe(keyframe),
            )?;

            // SAFETY: the packet is live and the borrow of its payload ended
            // with the call above.
            unsafe { ffi::av_packet_unref(self.packet) };
        }
    }

    /// Draws frame `index`: a bright block travelling left to right over a
    /// background that changes shade, so that a decoded frame can be told from
    /// its neighbours by eye.
    fn draw(&mut self, index: u64) {
        let (width, height) = (self.width as usize, self.height as usize);
        let block = (index as usize * 8) % width.max(1);

        // SAFETY: the frame's buffers were allocated by `av_frame_get_buffer`
        // for exactly this width, height and pixel format, and each plane has
        // at least `linesize` bytes per row. Every index below is bounded by
        // the plane's own dimensions.
        unsafe {
            let frame = self.frame;
            let luma = (*frame).data[0];
            let luma_stride = usize::try_from((*frame).linesize[0]).unwrap_or(0);
            for row in 0..height {
                for column in 0..width {
                    let bright = column.abs_diff(block) < 24;
                    let value = if bright {
                        235
                    } else {
                        16 + ((row + index as usize) % 64) as u8
                    };
                    *luma.add(row * luma_stride + column) = value;
                }
            }

            for plane in 1..3 {
                let chroma = (*frame).data[plane];
                let stride = usize::try_from((*frame).linesize[plane]).unwrap_or(0);
                for row in 0..height / 2 {
                    for column in 0..width / 2 {
                        *chroma.add(row * stride + column) = 128;
                    }
                }
            }
        }
    }
}

impl Drop for SoftwareVideoEncoder {
    fn drop(&mut self) {
        // SAFETY: this value owns all three, and each free takes a pointer to
        // the pointer and does nothing when it is null — which is the state
        // `open` leaves behind when it fails part-way through, so this is
        // correct on every path out of it.
        unsafe {
            ffi::av_packet_free(&mut self.packet);
            ffi::av_frame_free(&mut self.frame);
            ffi::avcodec_free_context(&mut self.context);
        }
    }
}
