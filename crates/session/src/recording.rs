//! One recording, from a window to a finished file.
//!
//! This is the wiring the rest of the workspace was built for: the capture
//! backend `clipped-capture` selects, the encoder `clipped-encoder` opens, and
//! the Matroska writer `clipped-muxer` owns, joined into a loop that runs until
//! it is asked to stop.
//!
//! # The order things happen in, and why
//!
//! ```text
//! select a backend        pure, from declarations; no GPU work
//! initialise it           returns the frame format the encoder is configured from
//! acquire one frame       the only way to learn which device the textures are on
//! open the encoder        against that device, so no frame is ever copied across adapters
//! create the file         the encoder's parameter sets are the container's codec header
//! ── loop ──              acquire, admit, submit, drain, queue
//! flush and finalise      on every path out, including a panic
//! ```
//!
//! The one surprising step is the third. `CaptureBackend` hands out textures
//! and never its device, because the device is the backend's for its whole life
//! (`docs/capture-pipeline.md`), so the session asks a texture which device
//! created it (`crate::windows::device`). That costs the first frame of the
//! recording — it is released, not encoded, because holding a frame across a
//! file being created would keep a frame-pool slot for the length of an
//! `avio_open`.
//!
//! # Finalisation
//!
//! Every path out of [`record`] finalises the file: a stop request, the window
//! closing, an encoder failing, a full disk, and a panic. The last is why
//! [`crate::muxing::MuxingThread`] finalises from `Drop` as well as from
//! `finish` — AGENTS.md section 17 puts the recording above almost everything
//! else, and a bug in this file must still leave something that plays.

use core::time::Duration;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use clipped_capture::{
    registered_backend, registered_declarations, select, Acquisition, CaptureBackend, CaptureClock,
    CaptureConfig, CaptureError, CaptureMethod, CaptureMethodSetting, CapturedFrame, FrameFormat,
};
use clipped_encoder::{Codec, Resolution, SourceFrame, SourceTexture, SurfaceKind, VideoEncoder};
use clipped_logging::{RedactedPath, SessionContext, SessionId};
use clipped_muxer::{MkvWriter, RecordingLayout, VideoCodec, VideoTrack};

use crate::encoding;
use crate::error::SessionError;
use crate::muxing::MuxingThread;
use crate::pacing::FrameGate;
use crate::report::{EndReason, RecordingReport};
use crate::settings::RecordingSettings;
use crate::windows::device::FrameDevice;

/// How long one acquisition waits for a frame.
///
/// This is not a frame rate — it is how long the loop can be inside `acquire`
/// and therefore how long a stop request waits to be noticed. A tenth of a
/// second is imperceptible to somebody pressing Ctrl+C and is long enough that
/// an idle source does not spin the loop.
const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(100);

/// How long capture is given to produce its first frame before the recording is
/// abandoned.
///
/// A source only produces a frame when its content changes, so a window that is
/// not drawing produces none at all — and a recorder that waits for ever on one
/// looks identical to a recorder that has hung. Ten seconds is far beyond the
/// tens of milliseconds a drawing window takes and short enough to be a
/// diagnosis rather than a wait.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// Records `settings.target` until `stop` is raised.
pub(crate) fn record(
    settings: &RecordingSettings,
    stop: &dyn crate::StopSignal,
) -> Result<RecordingReport, SessionError> {
    if settings.audio_requested() {
        // Said once, plainly, rather than left to be discovered in a file with
        // no sound in it (AGENTS.md section 54).
        tracing::warn!(
            "audio was asked for and this build cannot record it: the recording will have a \
             video track and nothing else. Wiring clipped-audio into a session is issue #180"
        );
    }

    let method = choose_backend(settings)?;
    let mut backend = registered_backend(method)
        .ok_or_else(|| SessionError::BackendNotRegistered {
            method: method.to_string(),
        })?
        .create()?;

    let format = backend.initialise(
        &settings.target().target()?,
        &CaptureConfig::default().with_capture_cursor(settings.capture_cursor()),
    )?;
    tracing::info!(
        capture_backend = method.log_value(),
        width = format.size().width(),
        height = format.size().height(),
        pixel_format = %format.pixel_format(),
        "capture started"
    );

    let outcome = record_frames(settings, stop, backend.as_mut(), format, method);

    // The backend is shut down here rather than being left to `Drop` so that
    // the display's duplication, or the compositor's frame pool, is released
    // before this function returns to a caller that may go on to do something
    // slow with the report (AGENTS.md section 58).
    backend.shut_down();
    outcome
}

/// Which backend will capture this target.
fn choose_backend(settings: &RecordingSettings) -> Result<CaptureMethod, SessionError> {
    let selection = select(
        &registered_declarations(),
        &settings.target().properties()?,
        CaptureMethodSetting::Automatic,
    )?;
    Ok(selection.method())
}

/// Everything from the first frame to the finished file.
fn record_frames(
    settings: &RecordingSettings,
    stop: &dyn crate::StopSignal,
    backend: &mut dyn CaptureBackend,
    mut format: FrameFormat,
    method: CaptureMethod,
) -> Result<RecordingReport, SessionError> {
    // Held for the whole of the recording: the encoder session is opened
    // against it and may not outlive it. `format` is passed by mutable
    // reference rather than by value because waiting for the first frame is
    // also where a target that resized between being measured and being
    // captured is followed, and everything below — the encoder, the size on
    // every submitted frame, the track in the header — is configured from it.
    let device = first_frame_device(backend, stop, &mut format)?;

    let encode_size = settings.encode_size(format.size())?;
    let opened = encoding::open(
        &device.as_graphics_device(),
        settings,
        encode_size,
        format.pixel_format(),
    )?;

    let span = SessionContext::new(session_id())
        .with_capture_backend(log_backend(method))
        .with_encoder(opened.kind.log_encoder_family())
        .span();
    let _entered = span.enter();

    let mut encoder = opened.encoder;
    let writer = open_output(settings, encoder.as_ref(), opened.codec, encode_size)?;
    let muxing = MuxingThread::start(writer);

    let mut counters = Counters::default();
    let mut gate = FrameGate::new(settings.framerate())?;
    let mut clock = None;
    let mut end_reason = EndReason::Stopped;
    let mut failure = None;

    while !stop.is_requested() {
        match backend.acquire(ACQUIRE_TIMEOUT) {
            Ok(Acquisition::Frame(frame)) => {
                counters.captured += 1;
                counters.missed_by_source += u64::from(frame.frames_missed().unwrap_or(0));

                let clock = *clock.get_or_insert_with(|| CaptureClock::start_at(frame.timestamp()));
                if let Err(error) = offer(
                    &frame,
                    clock,
                    &mut gate,
                    &muxing,
                    encoder.as_mut(),
                    encode_size,
                    &mut counters,
                ) {
                    failure = Some(error);
                    break;
                }
            }
            Ok(Acquisition::Timeout) => {}
            Ok(Acquisition::SizeChanged(size)) => {
                // Matroska fixes a track's dimensions in the header, and the
                // encoder is configured for one size, so there is no honest way
                // to carry on into the same file. The recording is finished
                // where it is rather than filled with frames of a size the
                // track does not declare.
                tracing::warn!(
                    width = size.width(),
                    height = size.height(),
                    "the recorded window changed size, which this build cannot follow within \
                     one file; the recording was finished at that point"
                );
                end_reason = EndReason::TargetResized;
                break;
            }
            Err(CaptureError::TargetLost { .. }) => {
                tracing::info!("the recorded window closed; finishing the recording");
                end_reason = EndReason::TargetLost;
                break;
            }
            Err(error) => {
                failure = Some(error.into());
                break;
            }
        }
    }

    // The finalisation, in order: the encoder is told the stream has ended and
    // drained of the pictures it was holding back, then the queue is closed and
    // the trailer written. A failure in the first does not skip the second.
    if let Err(error) = flush(encoder.as_mut(), &muxing, &mut counters) {
        failure.get_or_insert(error);
    }
    encoder.shut_down();

    let summary = match muxing.finish() {
        Ok(summary) => summary,
        Err(error) => return Err(reported_failure(failure, error)),
    };

    let report = RecordingReport {
        output: settings.output().to_path_buf(),
        capture_method: method,
        encoder: opened.kind,
        codec: opened.codec,
        width: encode_size.0,
        height: encode_size.1,
        requested_framerate: settings.framerate(),
        frames_captured: counters.captured,
        frames_encoded: counters.encoded,
        frames_skipped_for_rate: counters.skipped_for_rate,
        frames_dropped_writer_behind: counters.dropped_writer_behind,
        frames_missed_by_source: counters.missed_by_source,
        packets_written: summary.packets,
        timestamps_corrected: summary.timestamps_corrected(),
        duration: summary.duration,
        end_reason,
    };

    tracing::info!(
        output = %RedactedPath::new(report.output()),
        frames_encoded = report.frames_encoded(),
        frames_captured = report.frames_captured(),
        frames_skipped_for_rate = report.frames_skipped_for_rate(),
        frames_dropped_writer_behind = report.frames_dropped_writer_behind(),
        frames_missed_by_source = report.frames_missed_by_source(),
        packets = report.packets_written(),
        timestamps_corrected = report.timestamps_corrected(),
        duration_ms = report.duration().as_millis(),
        "recording finished"
    );

    match failure {
        Some(error) => Err(error),
        None => Ok(report),
    }
}

/// Which of the loop's failure and the writer thread's failure a user is told
/// about.
///
/// [`SessionError::WriterLost`] is a placeholder and not a diagnosis.
/// [`MuxingThread::write`] returns it whenever the writer thread has already
/// stopped, which is how a failed write — a full disk, a disconnected drive —
/// reaches the capture loop at all; the real reason stays on that thread and
/// comes back from `finish`. Preferring the placeholder would give somebody
/// whose disk filled the message "the thread writing the recording stopped
/// unexpectedly", whose own documentation says it means a bug rather than an
/// operating condition — hiding the cause and misdescribing it at once
/// (AGENTS.md section 15).
///
/// Any other loop failure is kept: an encoder that died mid-recording is the
/// first cause, and the writer's own complaint afterwards is a consequence of
/// the recording ending.
fn reported_failure(loop_failure: Option<SessionError>, writer: SessionError) -> SessionError {
    match loop_failure {
        None | Some(SessionError::WriterLost) => writer,
        Some(error) => error,
    }
}

/// What one recording lost, gained and did.
#[derive(Debug, Default)]
struct Counters {
    captured: u64,
    encoded: u64,
    skipped_for_rate: u64,
    dropped_writer_behind: u64,
    missed_by_source: u64,
}

/// Offers one captured frame to the encoder, and queues whatever comes out.
///
/// Three reasons a frame is not encoded, and all three are counted rather than
/// silent: the recording is already at the frame rate it was asked for, the
/// writer is behind, or the frame's timestamp is on a clock this recording is
/// not timed against.
fn offer(
    frame: &CapturedFrame<'_>,
    clock: CaptureClock,
    gate: &mut FrameGate,
    muxing: &MuxingThread,
    encoder: &mut dyn VideoEncoder,
    size: (u32, u32),
    counters: &mut Counters,
) -> Result<(), SessionError> {
    let media = match clock.media_time(frame.timestamp()) {
        Ok(media) => media,
        Err(mismatch) => {
            // A backend that changed the clock it stamps frames with mid-run.
            // Nothing here can place such a frame on the recording's timeline,
            // and guessing would put the picture out of step with the audio
            // that is timed against the same reference (docs/av-sync.md).
            tracing::warn!(%mismatch, "a frame arrived on another clock and was not recorded");
            return Ok(());
        }
    };

    // The epoch is the first frame the recording saw, so this is only negative
    // if a backend hands over a frame older than the one that started it.
    let nanos = media.as_nanos().max(0);

    if !gate.admit(nanos) {
        counters.skipped_for_rate += 1;
        return Ok(());
    }
    if muxing.is_behind() {
        counters.dropped_writer_behind += 1;
        return Ok(());
    }

    // SAFETY: the handle comes from a live `CapturedFrame`, which the backend
    // guarantees is a valid `ID3D11Texture2D` owned by it and unrecycled until
    // the frame is dropped — and the frame is borrowed for the whole of this
    // function. `VideoEncoder::submit` is documented to read the texture during
    // the call and never afterwards, so nothing derived from it outlives the
    // borrow. The kinds match: `TextureKind::D3d11Texture2D` was checked when
    // the device was derived from a frame of this same backend.
    let texture =
        unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, frame.texture().as_raw()) };

    let source = SourceFrame::new(
        texture,
        encoder.configuration().source_format(),
        Resolution::new(size.0, size.1),
        // The frame's position in the recording, converted once, here, from the
        // timestamp its source stamped it with. Nothing in this pipeline reads
        // a clock to fill this in (docs/av-sync.md).
        Duration::from_nanos(nanos.unsigned_abs()),
    );

    encoder.submit(&source)?;
    counters.encoded += 1;
    let packets = drain(encoder, muxing)?;
    report_submission_over_headroom(packets);
    Ok(())
}

/// Moves every packet the encoder has ready into the writer's queue, and
/// reports how many that was.
fn drain(encoder: &mut dyn VideoEncoder, muxing: &MuxingThread) -> Result<usize, SessionError> {
    let mut moved = 0;
    while let Some(packet) = encoder.next_packet()? {
        // Both timestamps are nanoseconds from the same zero as the frames that
        // went in, which is what the muxer's `PacketTimestamp` wants
        // (`crates/encoder/src/packet.rs`).
        muxing.write(
            packet.data(),
            nanos_of(packet.presentation_time()),
            nanos_of(packet.decode_time()),
            packet.is_keyframe(),
        )?;
        moved += 1;
    }
    Ok(moved)
}

/// Says so, once, when one submission produced more packets than the queue
/// keeps in reserve.
///
/// `crate::muxing` states that the capture thread never waits on the
/// filesystem, and what makes that true is a pair of numbers: the loop stops
/// submitting at [`crate::muxing::HIGH_WATER`], leaving
/// [`crate::muxing::HEADROOM`] slots for the packets of a submission made just
/// under it. Above that the bounded queue's `send` blocks, and the capture
/// thread is inside a write after all. Every encoder in this workspace emits at
/// most one packet per submitted frame in the low-latency configuration a
/// recording opens, so this has never fired — but the invariant had nothing
/// watching it, and a stated guarantee that nothing checks is a guarantee only
/// until an encoder changes.
///
/// Once, not per frame: an encoder that does this does it on every submission,
/// and a warning a second is a log nobody reads. The flush at the end of a
/// recording is deliberately not checked — it drains everything the encoder
/// held back, so it is *expected* to exceed the headroom, and blocking there is
/// blocking at shutdown with nothing left to capture.
fn report_submission_over_headroom(packets: usize) {
    static REPORTED: AtomicBool = AtomicBool::new(false);

    if packets > crate::muxing::HEADROOM && !REPORTED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            packets,
            headroom = crate::muxing::HEADROOM,
            "one submitted frame produced more encoded packets than the writer's queue keeps \
             in reserve, so the capture thread can be made to wait on the filesystem; please \
             report this with the encoder and codec from the lines above"
        );
    }
}

/// Ends the stream and drains what the encoder was holding back.
///
/// An encoder that reorders pictures has frames in hand that only come out
/// after this, so a recording that skipped it would be missing its last
/// fraction of a second — which is precisely the part somebody who pressed
/// Ctrl+C was watching.
fn flush(
    encoder: &mut dyn VideoEncoder,
    muxing: &MuxingThread,
    counters: &mut Counters,
) -> Result<(), SessionError> {
    let before = counters.encoded;
    encoder.finish()?;
    let result = drain(encoder, muxing).map(|_| ());
    // At `info` rather than `debug`: it happens once per recording, and it is
    // the line that says the pictures the encoder was holding back reached the
    // file — which is the difference between a recording that ends where the
    // user stopped it and one that ends a fraction of a second earlier
    // (AGENTS.md section 53 asks for this to be evidence rather than an
    // assumption).
    tracing::info!(
        frames_encoded = before,
        "the encoder was flushed and the container is being closed"
    );
    result
}

/// A [`Duration`] as the nanoseconds a container timestamp is measured in.
fn nanos_of(time: Duration) -> i64 {
    i64::try_from(time.as_nanos()).unwrap_or(i64::MAX)
}

/// Captures until a frame arrives, and takes the device it came from.
///
/// The frame itself is discarded. It could be encoded — nothing about it is
/// wrong — but the encoder does not exist yet, and holding a frame across the
/// encoder being opened and a file being created would keep a frame-pool slot
/// for as long as both take.
///
/// `format` is the format [`CaptureBackend::initialise`] reported, and it is
/// **updated in place** whenever the target resizes before the first frame
/// arrives. That is not bookkeeping: the wait here is up to
/// [`FIRST_FRAME_TIMEOUT`], which is long enough for somebody to drag a window
/// edge while `record` starts, and the format is what configures the encoder,
/// the resolution stamped on every submitted frame and the dimensions the
/// Matroska track declares. Handing the encoder a texture of one size while
/// telling it another is either a garbled picture in a track that declares the
/// wrong dimensions or a driver-level failure, and nothing downstream can
/// detect it (AGENTS.md section 22). It is passed by reference so that there is
/// only ever one `FrameFormat` for a recording and no stale copy to pick up.
fn first_frame_device(
    backend: &mut dyn CaptureBackend,
    stop: &dyn crate::StopSignal,
    format: &mut FrameFormat,
) -> Result<FrameDevice, SessionError> {
    let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;

    while !stop.is_requested() && Instant::now() < deadline {
        match backend.acquire(ACQUIRE_TIMEOUT)? {
            Acquisition::Frame(frame) => {
                return FrameDevice::of(&frame).ok_or(SessionError::NoGraphicsDevice);
            }
            // A window that has not drawn since capture attached, and a window
            // that was resized between being measured and being captured. The
            // second needs the backend rebuilt before it will produce anything.
            Acquisition::Timeout => {}
            Acquisition::SizeChanged(size) => {
                let resized = backend.resize(size)?;
                if resized != *format {
                    // At `info` because it changes what the file will contain:
                    // somebody reading the log to explain why a recording is
                    // 1284x741 rather than the size they measured needs this
                    // line to exist.
                    tracing::info!(
                        width = resized.size().width(),
                        height = resized.size().height(),
                        pixel_format = %resized.pixel_format(),
                        "the target changed size before the first frame; the recording will be \
                         made at the new size"
                    );
                }
                *format = resized;
            }
        }
    }

    Err(SessionError::NoFrames)
}

/// Creates the output file with a track describing what the encoder produces.
fn open_output(
    settings: &RecordingSettings,
    encoder: &dyn VideoEncoder,
    codec: Codec,
    size: (u32, u32),
) -> Result<MkvWriter, SessionError> {
    // The recordings directory is Clipped's own and is created by the recording
    // that goes in it, which is what keeps a run that records nothing from
    // leaving an empty folder in somebody's videos (docs/recorder-cli.md). A
    // directory the *user* named is never created here: the caller has already
    // refused a `--output` inside one that does not exist.
    if let Some(directory) = settings.output().parent() {
        if !directory.exists() {
            std::fs::create_dir_all(directory)
                .map_err(|source| SessionError::OutputDirectory { source })?;
        }
    }

    // `MkvWriter::create` refuses to truncate anything that is already there
    // (AGENTS.md section 56), so replacing an existing recording is done here,
    // deliberately, and only when the caller asked for it.
    if settings.overwrite() && settings.output().exists() {
        std::fs::remove_file(settings.output())
            .map_err(|source| SessionError::OutputDirectory { source })?;
    }

    // Deliberately no `with_frame_rate`. `--framerate` is the ceiling the
    // capture loop holds a recording to, not the rate it achieved: a 30 fps
    // source recorded with the default `--framerate 60` produces a real 30 fps
    // file. `clipped-muxer` writes a declared rate into the stream's
    // `avg_frame_rate` (`crates/muxer/src/writer.rs`), which is by definition
    // the *average* the file carries, so declaring the ceiling there is
    // incorrect codec metadata (AGENTS.md section 22). Measured rather than
    // argued: putting the rate back and recording the 30 fps test pattern gave
    // `ffprobe` `60/1 fps, 115 decoded frames` over 3.806 s.
    // Leaving it out lets a player derive the rate from the timestamps, which
    // are the recording's own account of when its frames happened. The
    // encoder is still configured for the ceiling; what that costs is in
    // `docs/recorder-cli.md` and is
    // [issue #191](https://github.com/wildware-uk/clipped/issues/191).
    let track = VideoTrack::new(container_codec(codec), size.0, size.1)
        // The encoder's out-of-band header: the sequence and picture parameter
        // sets for H.264 and HEVC, the sequence header for AV1. Matroska needs
        // it in the track entry, before the first frame, which is why
        // `VideoEncoder::parameter_sets` is available from the moment a session
        // opens (`crates/encoder/src/backend.rs`).
        .with_codec_private(encoder.parameter_sets().to_vec());

    Ok(MkvWriter::create(
        settings.output(),
        &RecordingLayout::new(track),
    )?)
}

/// The container's name for a codec the encoder produces.
const fn container_codec(codec: Codec) -> VideoCodec {
    match codec {
        Codec::H264 => VideoCodec::H264,
        Codec::Hevc => VideoCodec::Hevc,
        Codec::Av1 => VideoCodec::Av1,
    }
}

/// The capture backend as the standard log vocabulary names it.
///
/// [`CaptureMethod::GameCapture`] has no counterpart: nothing implements it
/// (AGENTS.md section 34), so selection cannot choose it and this cannot be
/// reached. It maps to the backend that *is* preferred rather than inventing a
/// vocabulary word for something no line will ever be tagged with.
const fn log_backend(method: CaptureMethod) -> clipped_logging::CaptureBackend {
    match method {
        CaptureMethod::DesktopDuplication => clipped_logging::CaptureBackend::DesktopDuplication,
        _ => clipped_logging::CaptureBackend::WindowsGraphicsCapture,
    }
}

/// An identifier for this recording, unique within the process.
///
/// Digits and a dash, so it is always a valid [`SessionId`]; the fallback is
/// there because `SessionId::new` is fallible and a recording must not fail
/// over a log field.
fn session_id() -> SessionId {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let value = format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    SessionId::new(value).unwrap_or_else(|_| SessionId::new("recording").expect("an identifier"))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use clipped_capture::{CaptureTarget, FrameSize, PixelFormat};
    use clipped_muxer::{MuxError, TrackId};

    use super::*;

    /// What a scripted backend hands back from one `acquire`.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        /// Nothing drew.
        Nothing,
        /// The target changed shape, which is what a window being dragged by
        /// its edge looks like from here.
        Resized(u32, u32),
    }

    /// A capture backend that replays a script and never produces a frame.
    ///
    /// A real frame would need a real Direct3D texture and therefore a GPU, and
    /// the behaviour under test is what happens to the *format* on the way to
    /// the first frame — which the script can drive without one.
    #[derive(Debug)]
    struct ScriptedBackend {
        steps: VecDeque<Step>,
        format: FrameFormat,
        resizes: u32,
    }

    impl ScriptedBackend {
        fn new(format: FrameFormat, steps: impl IntoIterator<Item = Step>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                format,
                resizes: 0,
            }
        }
    }

    impl CaptureBackend for ScriptedBackend {
        fn method(&self) -> CaptureMethod {
            CaptureMethod::WindowsGraphicsCapture
        }

        fn initialise(
            &mut self,
            _target: &CaptureTarget,
            _config: &CaptureConfig,
        ) -> Result<FrameFormat, CaptureError> {
            Ok(self.format)
        }

        fn acquire(&mut self, _timeout: Duration) -> Result<Acquisition<'_>, CaptureError> {
            match self.steps.pop_front() {
                Some(Step::Resized(width, height)) => Ok(Acquisition::SizeChanged(
                    FrameSize::new(width, height).expect("a real size"),
                )),
                Some(Step::Nothing) | None => Ok(Acquisition::Timeout),
            }
        }

        fn resize(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError> {
            self.resizes += 1;
            // What a real backend does: rebuild the frame pool and report the
            // format it will now produce.
            self.format = FrameFormat::new(size, self.format.pixel_format());
            Ok(self.format)
        }

        fn shut_down(&mut self) {}
    }

    /// A stop signal that trips after a fixed number of polls.
    ///
    /// So that a test asking what happens when no frame arrives finishes in
    /// milliseconds instead of waiting out [`FIRST_FRAME_TIMEOUT`].
    #[derive(Debug)]
    struct StopAfter {
        polls: AtomicU64,
        limit: u64,
    }

    impl StopAfter {
        const fn polls(limit: u64) -> Self {
            Self {
                polls: AtomicU64::new(0),
                limit,
            }
        }
    }

    impl crate::StopSignal for StopAfter {
        fn is_requested(&self) -> bool {
            self.polls.fetch_add(1, Ordering::Relaxed) >= self.limit
        }
    }

    fn format_of(width: u32, height: u32) -> FrameFormat {
        FrameFormat::new(
            FrameSize::new(width, height).expect("a real size"),
            PixelFormat::Bgra8Unorm,
        )
    }

    #[test]
    fn a_target_resized_before_the_first_frame_carries_the_new_format_forward() {
        // The wait for the first frame is up to ten seconds, which is long
        // enough for somebody to drag a window edge while `record` starts.
        // Everything after this point — the encoder's configuration, the
        // resolution stamped on every submitted frame, the dimensions the
        // Matroska track declares — is read out of this one `FrameFormat`, so
        // dropping what `resize` returned records the new pictures under the
        // old size: a garbled picture in a track that declares the wrong
        // dimensions, and nothing downstream that can tell (AGENTS.md section
        // 22).
        let mut format = format_of(1280, 720);
        let mut backend = ScriptedBackend::new(format, [Step::Resized(1920, 1080)]);
        let stop = StopAfter::polls(2);

        let error = first_frame_device(&mut backend, &stop, &mut format)
            .expect_err("a scripted backend never produces a frame");

        assert!(
            matches!(error, SessionError::NoFrames),
            "the wait should end with `no frames`, not {error}"
        );
        assert_eq!(backend.resizes, 1, "the backend should have been rebuilt");
        assert_eq!(
            format,
            format_of(1920, 1080),
            "the recording must be configured from the format `resize` returned, not the one \
             `initialise` did"
        );
    }

    #[test]
    fn a_target_that_never_changes_size_keeps_the_format_it_was_initialised_with() {
        let mut format = format_of(1280, 720);
        let mut backend = ScriptedBackend::new(format, [Step::Nothing]);
        let stop = StopAfter::polls(2);

        let _ = first_frame_device(&mut backend, &stop, &mut format);

        assert_eq!(backend.resizes, 0);
        assert_eq!(format, format_of(1280, 720));
    }

    /// A muxer failure with a reason in it, which is what a full disk produces.
    fn writer_failure() -> SessionError {
        SessionError::Mux(MuxError::InvalidTrack {
            track: TrackId::Video,
            reason: "there is no space left on the device",
        })
    }

    #[test]
    fn a_failed_write_is_reported_with_the_writers_reason_and_not_the_placeholder() {
        // What a full disk does: the loop breaks with `WriterLost` because the
        // writer thread has already stopped, and the reason it stopped comes
        // back from `finish`. Reporting the placeholder would tell somebody
        // whose disk filled that a thread "stopped unexpectedly", which that
        // variant's own documentation says means a bug rather than an
        // operating condition.
        let reported = reported_failure(Some(SessionError::WriterLost), writer_failure());

        assert!(
            reported
                .to_string()
                .contains("there is no space left on the device"),
            "the user should be told why the write failed: {reported}"
        );
    }

    #[test]
    fn a_failure_the_capture_loop_diagnosed_itself_survives_the_writers_complaint() {
        // An encoder that died mid-recording is the first cause; the writer
        // failing afterwards is a consequence of the recording ending, and
        // replacing the diagnosis with it would lose the diagnosis.
        let reported = reported_failure(Some(SessionError::NoGraphicsDevice), writer_failure());

        assert!(
            matches!(reported, SessionError::NoGraphicsDevice),
            "the first cause should be kept, not {reported}"
        );
    }

    #[test]
    fn a_writer_failure_with_nothing_else_wrong_is_reported_as_itself() {
        let reported = reported_failure(None, writer_failure());
        assert!(matches!(reported, SessionError::Mux(_)), "{reported}");
    }

    #[test]
    fn every_codec_the_encoder_produces_has_a_container_name() {
        // A codec added to `clipped-encoder` without a container name would not
        // compile, which is the point of the exhaustive match; this pins the
        // pairs so that a mismatched one is a test failure rather than a file
        // that claims to hold something it does not.
        assert_eq!(container_codec(Codec::H264), VideoCodec::H264);
        assert_eq!(container_codec(Codec::Hevc), VideoCodec::Hevc);
        assert_eq!(container_codec(Codec::Av1), VideoCodec::Av1);
    }

    #[test]
    fn a_session_identifier_is_a_valid_log_field_and_changes_between_recordings() {
        let first = session_id();
        let second = session_id();
        assert_ne!(first.as_str(), second.as_str());
        assert!(first.as_str().starts_with(&std::process::id().to_string()));
    }

    #[test]
    fn a_duration_beyond_a_container_timestamp_saturates_rather_than_wrapping() {
        // 585 years of recording, which nobody will make; wrapping would put
        // the last packet before the first and break the file.
        assert_eq!(nanos_of(Duration::from_secs(1)), 1_000_000_000);
        assert_eq!(nanos_of(Duration::MAX), i64::MAX);
    }
}
