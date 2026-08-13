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
//! open the audio          the endpoints' formats are what the audio tracks declare
//! create the file         the encoder's parameter sets are the container's codec header
//! ── loop ──              acquire, admit, submit, drain, queue
//!    first frame          fixes the epoch, and starts the audio threads on it
//! stop the audio          before the queue closes, so the last samples are written
//! flush and finalise      on every path out, including a panic
//! ```
//!
//! # Where audio joins
//!
//! Two moments, and they are apart for a reason. The endpoints are **opened**
//! before the file is created, because Matroska fixes a track's sampling rate
//! and channel count in the header and neither is knowable until a device has
//! been asked. The threads that read them are **started** at the first video
//! frame, because that frame is the recording's epoch and nothing can be placed
//! on the timeline until there is one (`docs/av-sync.md`). What happens in
//! between — the endpoint running while the encoder opens and the file is
//! created — is audio describing moments before the recording, and
//! `crate::audio::placement` trims it at the epoch rather than letting the
//! writer stack it on the first instant of the file.
//!
//! The threads are stopped and joined before [`crate::muxing::MuxingThread`] is
//! finished, because each holds a handle to its queue and the writer's loop ends
//! when the last handle is dropped.
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
use clipped_replay::ReplayBuffer;

use crate::audio::{self, AudioThreads, OpenSource};
use crate::encoding;
use crate::error::SessionError;
use crate::muxing::{MuxingThread, SpaceGuard, SpaceState};
use crate::pacing::FrameGate;
use crate::report::{AudioTrackReport, EndReason, RecordingReport};
use crate::screenshot::{ScreenshotRequests, ServedStill};
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

/// Records `settings.target` until `stop` is raised, feeding `outputs` as well
/// as the file.
pub(crate) fn record(
    settings: &RecordingSettings,
    stop: &dyn crate::StopSignal,
    outputs: &crate::RecordingOutputs<'_>,
) -> Result<RecordingReport, SessionError> {
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

    let outcome = record_frames(settings, stop, backend.as_mut(), format, method, outputs);

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
    outputs: &crate::RecordingOutputs<'_>,
) -> Result<RecordingReport, SessionError> {
    let replay = outputs.replay;
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

    let bitrate = opened.bitrate;
    let mut encoder = opened.encoder;
    // Before the file, because a track's sampling rate and channel count go in
    // the container's header and only the device knows them. A source that
    // cannot be opened fails the recording here, while nothing has been
    // created and while the user can still act on it.
    let sources = audio::open(settings)?;
    let layout = audio::declare(
        video_track(encoder.as_ref(), opened.codec, encode_size),
        &sources,
    );

    // Before the first packet and after the encoder: this is the moment both
    // things a replay buffer needs exist — the bitrate its memory ceiling is
    // sized from, and the track description a clip saved from it has to declare
    // (`crate::replay`). A buffer that could not be configured leaves the
    // recording untouched.
    let replay_buffer = crate::replay::start_buffer(&layout, bitrate, replay);

    let writer = open_output(settings, &layout)?;
    let muxing = MuxingThread::start(
        writer,
        SpaceGuard::new(settings.output(), settings.minimum_free_space()),
        &layout,
    )?;
    let sinks = PacketSinks {
        muxing: &muxing,
        replay: replay_buffer,
    };

    let mut counters = Counters::default();
    let mut gate = FrameGate::new(settings.framerate())?;
    let mut clock = None;
    let mut sources = Some(sources);
    let mut audio_threads: Option<AudioThreads> = None;
    let mut end_reason = EndReason::Stopped;
    let mut failure = None;
    let mut low_space_reported = false;
    let mut screenshots = outputs.screenshots.map(Screenshots::new);

    while !stop.is_requested() {
        // Before the space check and before the acquisition: a copy issued on
        // an earlier frame may be ready now, and reading it back is what lets
        // whoever pressed the key stop waiting. It touches the GPU and nothing
        // else — no disk, no encoder, no lock held across either.
        if let Some(screenshots) = screenshots.as_mut() {
            screenshots.collect();
        }

        // Before the acquisition, not after: the point of the guard is to stop
        // the recording while there is still room to finish the file, so a
        // frame that has already been captured is worth less than the trailer.
        // One relaxed atomic load — the filesystem call behind it happened on
        // the writer thread (`crate::muxing`).
        match muxing.space() {
            SpaceState::Ample => {}
            SpaceState::Low => {
                if !low_space_reported {
                    low_space_reported = true;
                    // Once, at `warn`, while there is still time to act on it.
                    // A line per frame would be a log nobody reads and a
                    // recording that spends its last minutes writing about
                    // itself.
                    tracing::warn!(
                        output = %RedactedPath::new(settings.output()),
                        "the drive this recording is being written to is filling up; the \
                         recording will be finished cleanly if it reaches the reserve"
                    );
                }
            }
            SpaceState::Exhausted => {
                tracing::warn!(
                    output = %RedactedPath::new(settings.output()),
                    "the drive this recording is being written to reached its reserve; \
                     finishing the recording now so that the file is complete"
                );
                end_reason = EndReason::DiskSpaceLow;
                break;
            }
            SpaceState::Unreadable => {
                end_reason = EndReason::OutputUnavailable;
                break;
            }
        }

        match backend.acquire(ACQUIRE_TIMEOUT) {
            Ok(Acquisition::Frame(frame)) => {
                counters.captured += 1;
                counters.missed_by_source += u64::from(frame.frames_missed().unwrap_or(0));
                counters.note_drawing();

                let clock = *clock.get_or_insert_with(|| {
                    // Beside the epoch, and nowhere else. This reading of *this
                    // process's* monotonic clock is what places a moment timed
                    // outside the recording — a plugin's event — inside the
                    // file, and the two readings are only interchangeable
                    // because nothing happens between them (`crate::progress`,
                    // `crate::plugins`, docs/plugin-api.md). It is one
                    // `OnceLock` store on the first frame and cannot block.
                    if let Some(progress) = outputs.progress {
                        progress.timeline_began(Instant::now());
                    }
                    CaptureClock::start_at(frame.timestamp())
                });
                // The epoch exists from here, so the audio sources can be given
                // one. Started once, on the first frame the recording keeps, and
                // not before: a packet has nowhere to go on a timeline that has
                // not begun (`docs/av-sync.md`).
                if let Some(sources) = sources.take() {
                    audio_threads = start_audio(sources, &layout, clock, &muxing);
                }

                if let Err(error) = offer(
                    &frame,
                    clock,
                    &mut gate,
                    &sinks,
                    encoder.as_mut(),
                    encode_size,
                    &mut counters,
                    outputs.progress,
                ) {
                    failure = Some(error);
                    break;
                }

                // After the encoder, deliberately. The recording is what must
                // not be delayed; a screenshot waits one more frame rather than
                // being the reason a frame was late (AGENTS.md section 17).
                if let Some(screenshots) = screenshots.as_mut() {
                    screenshots.consider(&frame, clock);
                }
            }
            Ok(Acquisition::Timeout) => {}
            Ok(Acquisition::TargetMinimised) => {
                // **The recording continues.** Alt-tabbing out of an exclusive
                // fullscreen game minimises it, so ending the session here would
                // cost somebody the rest of their game for pressing Alt+Tab —
                // far more than the silent stretch costs. The file keeps
                // everything before the minimise and everything after the
                // restore, on one timeline, and the frozen stretch between them
                // is what the source actually did.
                //
                // What it must not do is pass in silence. `note_minimised` says
                // so once per stretch, the count reaches the report and the
                // summary the user reads, and a recording that turns out to have
                // spent its whole life here leaves no file at all (see below).
                // That is issue #383: the pipeline was right that there was
                // nothing to record and wrong to keep it to itself.
                counters.note_minimised();
            }
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

    // Anything still waiting for a frame is told there will not be one, before
    // the finalisation below — which takes as long as it takes to write a
    // trailer — rather than being left to time out. A screenshot key pressed as
    // a game exits is the case: the request is real and the answer is no.
    if let Some(mut screenshots) = screenshots.take() {
        screenshots.abandon();
    }

    // The finalisation, in order: the encoder is told the stream has ended and
    // drained of the pictures it was holding back, the audio threads are stopped
    // and joined, then the queue is closed and the trailer written. A failure in
    // any of them does not skip the ones after it.
    if let Err(error) = flush(encoder.as_mut(), &sinks, &mut counters) {
        failure.get_or_insert(error);
    }
    encoder.shut_down();

    // Before `muxing.finish`, and this order is load-bearing: each audio thread
    // holds a clone of the writer's queue, and the writer's loop only ends once
    // the last of them has been dropped. Joining them here is also what puts
    // their final buffers into the file rather than into a closed channel.
    let audio_tracks = stop_audio(audio_threads, &muxing);

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
        times_target_minimised: counters.minimised_stretches,
        packets_written: summary.packets,
        timestamps_corrected: summary.timestamps_corrected(),
        duration: summary.duration,
        end_reason,
        audio_tracks,
    };

    if summary.audio_tracks_without_packets > 0 {
        // The muxer counted the tracks nothing was ever written to; each source
        // has already said what it produced, and this is the file's own account
        // of the same fact. Both are worth having: a track can be empty because
        // its device was silent, and it can be empty because everything it
        // produced preceded the recording.
        tracing::warn!(
            empty_audio_tracks = summary.audio_tracks_without_packets,
            "the recording has audio tracks with no audio in them"
        );
    }

    tracing::info!(
        output = %RedactedPath::new(report.output()),
        frames_encoded = report.frames_encoded(),
        frames_captured = report.frames_captured(),
        frames_skipped_for_rate = report.frames_skipped_for_rate(),
        frames_dropped_writer_behind = report.frames_dropped_writer_behind(),
        frames_missed_by_source = report.frames_missed_by_source(),
        times_target_minimised = report.times_target_minimised(),
        packets = report.packets_written(),
        timestamps_corrected = report.timestamps_corrected(),
        duration_ms = report.duration().as_millis(),
        "recording finished"
    );

    if let Some(stats) = replay.and_then(crate::replay::ReplayRecording::stats) {
        // Said once, at the end, because it is the only account of what the
        // buffer actually did: a window that came out shorter than it was
        // configured for, or a segment count that never grew, is the difference
        // between a replay that can be saved and one that cannot.
        tracing::info!(
            segments_held = stats.segments_held(),
            bytes_held = stats.bytes_held(),
            peak_bytes_held = stats.peak_bytes_held(),
            covered_seconds = stats
                .covered()
                .map_or(0.0, |covered| covered.length().as_secs_f64()),
            segments_evicted_for_window = stats.segments_evicted_for_window(),
            segments_evicted_over_ceiling = stats.segments_evicted_over_ceiling(),
            segments_sealed_at_the_ceiling = stats.segments_sealed_at_the_ceiling(),
            packets_discarded_over_ceiling = stats.packets_discarded_over_ceiling(),
            "replay buffer at the end of the recording"
        );
    }

    conclude(report, failure)
}

/// What a finished recording turns out to be: the report, or the failure that
/// takes its place — and whether the file it wrote is a recording at all.
///
/// Everything a recording's end depends on is decided here, and there are two
/// decisions.
///
/// **A failure wins.** A recording that stopped because its encoder died or its
/// drive filled has a diagnosis, and the diagnosis is what the user acts on.
///
/// **A recording no video reached is not a recording, and does not stay on
/// disk.** What it is instead is a container header and however many empty audio
/// tracks were declared — 791 bytes, in the case that raised
/// [issue #383](https://github.com/wildware-uk/clipped/issues/383) — which the
/// library indexes like any other file and draws as a tile that cannot be played
/// (#56). So it is removed, and the recording is reported as
/// [`SessionError::NoFrames`], whose message already says that capture produced
/// no frame and that a window which is not drawing is the usual reason, and
/// whose `FootageKept::Nothing` sentence already promises that no file was left.
/// Before this, that promise was broken by exactly one path: capture that
/// produced the single frame the encoder is opened against — enough to create
/// the file — and then stopped, which is what a window minimised at the moment
/// the recording started does.
///
/// The removal is deliberately **not** done when there is a failure to report.
/// The file a failure left is evidence for that failure and its message is about
/// the failure rather than about a missing recording; deleting it would also
/// make `FootageKept::UpToTheFailure`'s "everything recorded before this plays"
/// name a file that is no longer there.
///
/// A removal that itself fails is logged and otherwise ignored: there is nothing
/// a caller could do about it, and the error a user is owed is the one about
/// their recording rather than one about tidying up after it (AGENTS.md section
/// 15).
fn conclude(
    report: RecordingReport,
    failure: Option<SessionError>,
) -> Result<RecordingReport, SessionError> {
    if let Some(error) = failure {
        return Err(error);
    }
    if report.frames_encoded() > 0 {
        return Ok(report);
    }

    let output = report.output();
    match std::fs::remove_file(output) {
        Ok(()) => tracing::info!(
            output = %RedactedPath::new(output),
            end_reason = report.end_reason().token(),
            times_target_minimised = report.times_target_minimised(),
            "no video reached the recording, so the empty file was removed rather than left \
             to be indexed as a recording"
        ),
        Err(error) => tracing::warn!(
            %error,
            output = %RedactedPath::new(output),
            "no video reached the recording and the empty file could not be removed; it will \
             not play"
        ),
    }

    Err(SessionError::NoFrames)
}

/// How long a screenshot copy is polled before the loop waits for it.
///
/// A quarter of a second. Polling is what keeps the capture thread off the GPU's
/// critical path (`clipped_capture::windows::D3d11StillCopier`), and in practice
/// the copy is ready on the next frame — but a source that stops producing
/// frames stops the polling too, and somebody who pressed a key is owed a
/// picture rather than a timeout. So after this long the loop maps the staging
/// texture the blocking way, once, and takes whatever stall that costs.
const SCREENSHOT_POLL_LIMIT: Duration = Duration::from_millis(250);

/// The screenshot half of the capture loop.
///
/// Owned by [`record_frames`] and touched only from its thread. The division of
/// labour is `crate::screenshot::request`'s: this copies pixels, and the thread
/// that asked encodes and writes them.
struct Screenshots<'a> {
    requests: &'a ScreenshotRequests,
    copier: clipped_capture::windows::D3d11StillCopier,
    /// The request a copy is in flight for, and when it was issued.
    serving: Option<(u64, Instant)>,
    /// Where the frame being copied sits on the recording's timeline.
    position: Option<Duration>,
}

impl<'a> Screenshots<'a> {
    fn new(requests: &'a ScreenshotRequests) -> Self {
        Self {
            requests,
            copier: clipped_capture::windows::D3d11StillCopier::new(),
            serving: None,
            position: None,
        }
    }

    /// Reads back a copy issued on an earlier frame, if it is ready.
    ///
    /// Called once per turn of the loop, including turns where no frame
    /// arrived: a window that stopped drawing the instant after the key was
    /// pressed still has a copy in flight, and it is still readable.
    fn collect(&mut self) {
        let Some((id, began)) = self.serving else {
            return;
        };

        let waited = began.elapsed();
        let outcome = if waited >= SCREENSHOT_POLL_LIMIT {
            // Long enough. The blocking map costs this thread the GPU's
            // remaining work on one texture copy, which is the price of not
            // leaving a request unanswered.
            self.copier.finish().map(Some)
        } else {
            self.copier.poll()
        };

        match outcome {
            // Not ready. Nothing is dropped and nothing is retried; the next
            // turn of the loop asks again.
            Ok(None) => {}
            Ok(Some(still)) => {
                self.serving = None;
                self.requests.serve(
                    id,
                    Ok(ServedStill {
                        still,
                        position: self.position,
                    }),
                );
            }
            Err(error) => {
                self.serving = None;
                // Said at `warn` rather than swallowed: a screenshot that
                // cannot be copied is a feature that has stopped working, and
                // the waiter is told the same thing.
                tracing::warn!(%error, "a screenshot could not be copied out of the frame");
                self.requests.serve(id, Err(error.to_string()));
            }
        }
    }

    /// Starts a copy of `frame` if somebody is waiting for one.
    ///
    /// One request at a time: a copy already in flight is left alone, because
    /// replacing it would mean the earlier waiter never being answered. The
    /// next turn of the loop serves the next request.
    fn consider(&mut self, frame: &CapturedFrame<'_>, clock: CaptureClock) {
        if self.serving.is_some() || !self.requests.is_waiting() {
            return;
        }
        let Some(id) = self.requests.claim() else {
            return;
        };

        // The frame's own position on the recording's timeline, converted the
        // one way everything else in this pipeline converts a timestamp
        // (`docs/av-sync.md`). A clock mismatch leaves it unknown rather than
        // guessed: the picture is still worth having without a marker.
        self.position = clock
            .media_time(frame.timestamp())
            .ok()
            .map(|media| Duration::from_nanos(media.as_nanos().max(0).unsigned_abs()));

        match self.copier.begin(frame) {
            Ok(()) => self.serving = Some((id, Instant::now())),
            Err(error) => {
                tracing::warn!(%error, "a screenshot could not be taken from this frame");
                self.requests.serve(id, Err(error.to_string()));
            }
        }
    }

    /// Tells every waiter that this recording has no more frames.
    ///
    /// The claimed request first, then anything still queued. Without this a
    /// screenshot asked for as a game exits waits for its whole timeout to
    /// learn what the recording already knew.
    fn abandon(&mut self) {
        const GONE: &str = "the recording ended before a frame could be copied";

        if let Some((id, _)) = self.serving.take() {
            // One last attempt: the copy may already be finished, and a picture
            // is a better answer than an explanation.
            match self.copier.poll() {
                Ok(Some(still)) => self.requests.serve(
                    id,
                    Ok(ServedStill {
                        still,
                        position: self.position,
                    }),
                ),
                Ok(None) | Err(_) => self.requests.serve(id, Err(GONE.to_owned())),
            }
        }

        while let Some(id) = self.requests.claim() {
            self.requests.serve(id, Err(GONE.to_owned()));
        }

        // The staging texture is several megabytes of video memory and the
        // recording is over (AGENTS.md section 58).
        self.copier.release();
    }
}

/// Starts the threads reading a recording's audio sources, if it has any.
///
/// [`None`] for a recording with no audio at all, which is what
/// `--microphone none --system-audio none` produces: nothing is opened, nothing
/// is started, and nothing is said about it.
fn start_audio(
    sources: Vec<OpenSource>,
    layout: &RecordingLayout,
    clock: CaptureClock,
    muxing: &MuxingThread,
) -> Option<AudioThreads> {
    (!sources.is_empty()).then(|| AudioThreads::start(sources, layout, clock, muxing))
}

/// Stops the audio threads and collects what each source produced.
///
/// Empty for a recording that had none. The count of buffers the writer had no
/// room for is read from the queue rather than summed from the reports, because
/// a thread that panicked has no report and the buffers it lost are still
/// missing from the file.
fn stop_audio(threads: Option<AudioThreads>, muxing: &MuxingThread) -> Vec<AudioTrackReport> {
    let tracks = threads.map_or_else(Vec::new, |mut threads| threads.finish());

    let dropped = muxing.audio_buffers_dropped();
    if dropped > 0 {
        // A fault, and one nothing else would report: audio is gone from the
        // file, in holes wherever the disk could not keep up. Said once, at the
        // end, with the total.
        tracing::warn!(
            buffers_dropped_writer_behind = dropped,
            "audio was lost because the thread writing the recording could not keep up; the \
             tracks have holes in them where those buffers should have been"
        );
    }

    tracks
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
    /// How many separate stretches of the recording the window was minimised
    /// for.
    ///
    /// Stretches rather than acquisitions: a window minimised for a minute is
    /// one thing that happened, and counting the ten acquisitions a second it
    /// produces would report a number about the acquisition timeout.
    minimised_stretches: u64,
    /// Whether the current stretch is still running, so that one stretch is
    /// counted and logged once rather than ten times a second.
    minimised_now: bool,
}

impl Counters {
    /// Records an acquisition that found the window minimised.
    ///
    /// The log line is at `warn` and happens once per stretch: this is a
    /// recording that is accumulating nothing, and a `watch` session has nobody
    /// reading a console (AGENTS.md section 35 on not writing a line per frame).
    fn note_minimised(&mut self) {
        if self.minimised_now {
            return;
        }
        self.minimised_now = true;
        self.minimised_stretches += 1;
        tracing::warn!(
            frames_captured = self.captured,
            "the recorded window was minimised, so nothing is being recorded; the recording \
             is being kept open and resumes when the window is restored"
        );
    }

    /// Records a frame arriving, which ends any stretch that was running.
    fn note_drawing(&mut self) {
        if !self.minimised_now {
            return;
        }
        self.minimised_now = false;
        tracing::info!(
            frames_captured = self.captured,
            "the recorded window was restored; the recording is receiving frames again"
        );
    }
}

/// Where a drained packet goes.
///
/// The file always, and the replay buffer as well when one was given. The two
/// are a pair rather than two arguments because they are always passed
/// together, and because the pairing is the point: **there is one encoder**. A
/// recording and a replay buffer running at the same time encode once and copy
/// the bytes twice, which is what SPEC.md section 16 asks for and what makes a
/// replay buffer nearly free while a recording is running.
#[derive(Debug, Clone, Copy)]
struct PacketSinks<'sinks> {
    muxing: &'sinks MuxingThread,
    replay: Option<&'sinks ReplayBuffer>,
}

/// Offers one captured frame to the encoder, and queues whatever comes out.
///
/// Three reasons a frame is not encoded, and all three are counted rather than
/// silent: the recording is already at the frame rate it was asked for, the
/// writer is behind, or the frame's timestamp is on a clock this recording is
/// not timed against.
///
/// `progress` is published *after* the frame has been submitted, and only then:
/// what it says is "the recording contains this moment", which is the only
/// answer a bookmark can be placed against (`crate::progress`).
#[allow(clippy::too_many_arguments)]
fn offer(
    frame: &CapturedFrame<'_>,
    clock: CaptureClock,
    gate: &mut FrameGate,
    sinks: &PacketSinks<'_>,
    encoder: &mut dyn VideoEncoder,
    size: (u32, u32),
    counters: &mut Counters,
    progress: Option<&crate::RecordingProgress>,
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
    if sinks.muxing.is_behind() {
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
    if let Some(progress) = progress {
        // One relaxed store. This is the whole of the capture thread's
        // involvement in bookmarks, and it is why a bookmark cannot delay a
        // frame (AGENTS.md section 20).
        progress.reached(Duration::from_nanos(nanos.unsigned_abs()));
    }
    let packets = drain(encoder, sinks)?;
    report_submission_over_headroom(packets);
    Ok(())
}

/// Moves every packet the encoder has ready into the writer's queue and, when
/// there is one, into the replay buffer, and reports how many that was.
///
/// **The muxer first, the buffer second.** The file is the recording and the
/// buffer is a copy of it, so a packet is never held back from the file for the
/// buffer's sake.
///
/// The replay buffer's result is not a `Result`: it copies bytes into memory it
/// already owns and has no failure to report. A buffer that has reached its
/// ceiling drops its own oldest segments — or, for an encoder whose keyframes
/// are further apart than it can hold, the video it cannot buffer — and says so
/// in its statistics rather than refusing the packet, because a replay buffer
/// must never be able to end a recording (AGENTS.md section 17).
fn drain(encoder: &mut dyn VideoEncoder, sinks: &PacketSinks<'_>) -> Result<usize, SessionError> {
    let mut moved = 0;
    while let Some(packet) = encoder.next_packet()? {
        // Both timestamps are nanoseconds from the same zero as the frames that
        // went in, which is what the muxer's `PacketTimestamp` wants
        // (`crates/encoder/src/packet.rs`).
        sinks.muxing.write(
            packet.data(),
            nanos_of(packet.presentation_time()),
            nanos_of(packet.decode_time()),
            packet.is_keyframe(),
        )?;
        if let Some(replay) = sinks.replay {
            replay.push(&packet);
        }
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
    sinks: &PacketSinks<'_>,
    counters: &mut Counters,
) -> Result<(), SessionError> {
    let before = counters.encoded;
    encoder.finish()?;
    let result = drain(encoder, sinks).map(|_| ());
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
    let mut minimised = false;

    while !stop.is_requested() && Instant::now() < deadline {
        match backend.acquire(ACQUIRE_TIMEOUT)? {
            Acquisition::Frame(frame) => {
                return FrameDevice::of(&frame).ok_or(SessionError::NoGraphicsDevice);
            }
            // A window that has not drawn since capture attached, and a window
            // that was resized between being measured and being captured. The
            // second needs the backend rebuilt before it will produce anything.
            Acquisition::Timeout => {}
            // Minimised between being measured — where it was not, or the
            // recording would have been refused — and capture attaching. Waited
            // out like any other silence, because the window may be restored
            // within the ten seconds; what it adds is the reason, so that the
            // `no frames` this ends in is explained rather than mysterious. Said
            // once, on the way into the stretch.
            Acquisition::TargetMinimised => {
                if !minimised {
                    minimised = true;
                    tracing::warn!(
                        "the window was minimised before capture produced its first frame; \
                         waiting for it to be restored"
                    );
                }
            }
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

/// The video track describing what the encoder produces.
///
/// Deliberately no `with_frame_rate`. `--framerate` is the ceiling the capture
/// loop holds a recording to, not the rate it achieved: a 30 fps source recorded
/// with the default `--framerate 60` produces a real 30 fps file.
/// `clipped-muxer` writes a declared rate into the stream's `avg_frame_rate`
/// (`crates/muxer/src/writer.rs`), which is by definition the *average* the file
/// carries, so declaring the ceiling there is incorrect codec metadata
/// (AGENTS.md section 22). Measured rather than argued: putting the rate back
/// and recording the 30 fps test pattern gave `ffprobe` `60/1 fps, 115 decoded
/// frames` over 3.806 s.
///
/// Leaving it out lets a player derive the rate from the timestamps, which are
/// the recording's own account of when its frames happened. The encoder is still
/// configured for the ceiling; what that costs is in `docs/recorder-cli.md` and
/// is [issue #191](https://github.com/wildware-uk/clipped/issues/191).
fn video_track(encoder: &dyn VideoEncoder, codec: Codec, size: (u32, u32)) -> VideoTrack {
    VideoTrack::new(container_codec(codec), size.0, size.1)
        // The encoder's out-of-band header: the sequence and picture parameter
        // sets for H.264 and HEVC, the sequence header for AV1. Matroska needs
        // it in the track entry, before the first frame, which is why
        // `VideoEncoder::parameter_sets` is available from the moment a session
        // opens (`crates/encoder/src/backend.rs`).
        .with_codec_private(encoder.parameter_sets().to_vec())
}

/// Creates the output file with every track the recording will contain.
fn open_output(
    settings: &RecordingSettings,
    layout: &RecordingLayout,
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

    check_there_is_room(settings)?;

    // `MkvWriter::create` refuses to truncate anything that is already there
    // (AGENTS.md section 56), so replacing an existing recording is done here,
    // deliberately, and only when the caller asked for it.
    if settings.overwrite() && settings.output().exists() {
        std::fs::remove_file(settings.output())
            .map_err(|source| SessionError::OutputDirectory { source })?;
    }

    Ok(MkvWriter::create(settings.output(), layout)?)
}

/// Refuses a recording the drive has no room to make.
///
/// Asked once, before the file is created, because the alternative is a
/// recording that opens, records for a few seconds and is stopped by the same
/// floor from the inside — which looks like a bug rather than a full disk. A
/// caller who has turned the guard off
/// ([`RecordingSettings::with_minimum_free_space`](crate::RecordingSettings::with_minimum_free_space))
/// is not stopped here either, because the two have to agree.
///
/// A volume that cannot be read is **not** a refusal. The recording is about to
/// try to create a file on it, and that will fail with something far more
/// specific than "the free space could not be read"; refusing here would
/// replace a good message with a worse one, and would stop recording altogether
/// on any volume Windows will not answer for.
fn check_there_is_room(settings: &RecordingSettings) -> Result<(), SessionError> {
    let minimum = settings.minimum_free_space();
    if minimum == 0 {
        return Ok(());
    }

    match crate::disk::free_space(settings.output()) {
        Ok(space) => {
            let free = space.free_bytes();
            if crate::disk::judge(free, minimum) == crate::disk::SpaceVerdict::Exhausted {
                return Err(SessionError::NotEnoughDiskSpace { free, minimum });
            }
            tracing::debug!(
                free_bytes = free,
                total_bytes = space.total_bytes(),
                minimum_free_bytes = minimum,
                "there is room for the recording"
            );
            Ok(())
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "how much room the recording has could not be read, so it is being started \
                 without the check"
            );
            Ok(())
        }
    }
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
    use std::path::PathBuf;

    use clipped_capture::{
        CaptureTarget, CaptureTimestamp, FrameSize, FrameTexture, PixelFormat, SourceClock,
        TextureKind,
    };
    use clipped_encoder::{
        BitRate, EncodeError, EncodedPacket, EncoderConfig, EncoderKind, FrameRate, PictureKind,
        RateControl,
    };
    use clipped_muxer::{MuxError, TrackId};
    use clipped_replay::ReplayConfig;
    use windows::core::Interface as _;
    use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

    use super::*;

    /// What a scripted backend hands back from one `acquire`.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        /// Nothing drew.
        Nothing,
        /// The window drew, so a frame is handed over.
        ///
        /// Only usable on a backend given a texture by
        /// [`ScriptedBackend::drawing`], because a frame is a texture and there
        /// is nothing honest to put there otherwise.
        Drew,
        /// The window is minimised, so nothing will draw until it is restored.
        Minimised,
        /// The target changed shape, which is what a window being dragged by
        /// its edge looks like from here.
        Resized(u32, u32),
    }

    /// A capture backend that replays a script.
    ///
    /// Without a texture it never produces a frame, which is all the tests of
    /// the wait for the first frame need: what they are about is what happens
    /// to the *format* on the way to one. Given a texture by
    /// [`drawing`](Self::drawing) it hands over real frames on that texture, and
    /// the whole recording loop runs against it — an encoder opened on the
    /// texture's own device, a real file, and a script that can put a minimised
    /// stretch in the middle of a recording, which no test can do to a real
    /// compositor without a window to minimise.
    #[derive(Debug)]
    struct ScriptedBackend {
        steps: VecDeque<Step>,
        format: FrameFormat,
        resizes: u32,
        /// The texture every [`Step::Drew`] hands over, owned for the whole of
        /// this backend's life and never recycled — which is exactly the
        /// promise [`FrameTexture`] is documented to need.
        texture: Option<ID3D11Texture2D>,
        /// How many frames have been handed over, which is what spaces their
        /// timestamps.
        drawn: u64,
    }

    /// How far apart the frames a scripted backend draws are, in nanoseconds.
    ///
    /// A tenth of a second: far enough inside the 60 frames a second the gate
    /// admits that every frame the script draws reaches the encoder, so a test
    /// counting frames is counting the script rather than the pacing rule.
    const SCRIPTED_FRAME_INTERVAL_NANOS: u64 = 100_000_000;

    impl ScriptedBackend {
        fn new(format: FrameFormat, steps: impl IntoIterator<Item = Step>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                format,
                resizes: 0,
                texture: None,
                drawn: 0,
            }
        }

        /// The same backend, handing over `texture` for every [`Step::Drew`].
        fn drawing(mut self, texture: ID3D11Texture2D) -> Self {
            self.texture = Some(texture);
            self
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
                Some(Step::Drew) => {
                    let handle = self
                        .texture
                        .as_ref()
                        .expect("a script that draws was given a texture to draw on")
                        .as_raw();
                    self.drawn += 1;

                    // SAFETY: `handle` is an `ID3D11Texture2D` this backend owns
                    // for the whole of its life and never recycles, and the
                    // frame borrows the backend mutably, so nothing derived from
                    // it can outlive the texture.
                    let texture = unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, handle) };

                    Ok(Acquisition::Frame(CapturedFrame::new(
                        texture,
                        self.format,
                        CaptureTimestamp::from_source(
                            SourceClock::PerformanceCounter,
                            self.drawn * SCRIPTED_FRAME_INTERVAL_NANOS,
                        ),
                    )))
                }
                Some(Step::Resized(width, height)) => Ok(Acquisition::SizeChanged(
                    FrameSize::new(width, height).expect("a real size"),
                )),
                Some(Step::Minimised) => Ok(Acquisition::TargetMinimised),
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
    fn a_target_minimised_before_the_first_frame_is_waited_out_rather_than_failed_on() {
        // A window minimised between being resolved — where it was not, or
        // `apps/recorder` and `select` would both have refused the recording —
        // and capture attaching. It is still a window and it is coming back, so
        // the wait for the first frame runs on exactly as it does for a window
        // that has not drawn yet; what must not happen is an error, a hang, or a
        // frame format built out of the minimised shape (issue #383).
        let mut format = format_of(1280, 720);
        let mut backend =
            ScriptedBackend::new(format, [Step::Minimised, Step::Minimised, Step::Minimised]);
        let stop = StopAfter::polls(3);

        let error = first_frame_device(&mut backend, &stop, &mut format)
            .expect_err("a minimised window produces no frame to take a device from");

        assert!(
            matches!(error, SessionError::NoFrames),
            "a minimised window is a window that produced no frames, not a failure of \
             capture: {error}"
        );
        assert_eq!(backend.resizes, 0, "there was no size to adopt");
        assert_eq!(
            format,
            format_of(1280, 720),
            "the format must be the one capture reported, not one derived from the \
             minimised shape"
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

    // ---- what a finished recording turns out to be --------------------------

    /// A report for a recording that wrote `frames_encoded` frames to `output`.
    fn finished(output: PathBuf, frames_encoded: u64) -> RecordingReport {
        RecordingReport {
            output,
            capture_method: CaptureMethod::WindowsGraphicsCapture,
            encoder: clipped_encoder::EncoderKind::Nvenc,
            codec: Codec::Av1,
            width: 1320,
            height: 900,
            requested_framerate: 60,
            frames_captured: frames_encoded,
            frames_encoded,
            frames_skipped_for_rate: 0,
            frames_dropped_writer_behind: 0,
            frames_missed_by_source: 0,
            times_target_minimised: u64::from(frames_encoded == 0),
            packets_written: frames_encoded,
            timestamps_corrected: 0,
            duration: Duration::ZERO,
            end_reason: EndReason::Stopped,
            audio_tracks: Vec::new(),
        }
    }

    /// A file with something in it, at a path of this test's own.
    fn a_written_file(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("clipped-{name}-{}.mkv", std::process::id()));
        std::fs::write(&path, b"a container header and nothing else").expect("a temporary file");
        path
    }

    #[test]
    fn a_recording_no_video_reached_leaves_no_file_behind() {
        // The file issue #383 was raised about: 791 bytes of Matroska header
        // with two empty audio tracks and no picture, produced by recording a
        // minimised window. Capture handed over the one frame the encoder is
        // opened against and then nothing, so the file was created and never
        // written to. Left there it is indexed by the library and drawn as a
        // tile that cannot be played, and the error the user is given already
        // promises — in `FootageKept::Nothing` — that no file was left.
        let output = a_written_file("empty-recording");

        let error = conclude(finished(output.clone(), 0), None)
            .expect_err("a recording with no video in it is not a recording");

        assert!(
            matches!(error, SessionError::NoFrames),
            "the user should be told that capture produced no frame: {error}"
        );
        assert!(
            !output.exists(),
            "a recording that captured nothing must not be left on disk as though it were \
             one: {}",
            output.display()
        );
    }

    #[test]
    fn a_recording_with_video_in_it_keeps_its_file_and_its_report() {
        // The other direction, and the one that makes the test above mean
        // something: without it, a `conclude` that deleted every recording would
        // pass just as well.
        let output = a_written_file("real-recording");

        let report = conclude(finished(output.clone(), 181), None)
            .expect("a recording with frames in it is a recording");

        assert_eq!(report.frames_encoded(), 181);
        assert!(
            output.exists(),
            "the recording is the thing that cannot be made again (AGENTS.md section 17)"
        );
        std::fs::remove_file(&output).expect("the test's own file");
    }

    #[test]
    fn a_recording_that_failed_keeps_its_file_and_reports_the_failure() {
        // A failure has a diagnosis and the file is evidence for it. Deleting it
        // would also make `FootageKept::UpToTheFailure` — "everything recorded
        // before this was finished and plays" — name a file that is not there.
        let output = a_written_file("failed-recording");

        let error = conclude(finished(output.clone(), 0), Some(SessionError::WriterLost))
            .expect_err("the recording failed");

        assert!(
            matches!(error, SessionError::WriterLost),
            "the diagnosis is what the user acts on, not `no frames`: {error}"
        );
        assert!(output.exists(), "the failure's own evidence was removed");
        std::fs::remove_file(&output).expect("the test's own file");
    }

    // ---- the disk guard, before anything is created ------------------------

    /// Settings for a recording that would go to `output`.
    fn settings_for(output: PathBuf, minimum_free_space: u64) -> RecordingSettings {
        RecordingSettings::new(
            crate::settings::CaptureTargetSettings::window(0x1234, 1280, 720),
            output,
        )
        .with_minimum_free_space(minimum_free_space)
    }

    #[test]
    fn a_recording_is_refused_before_it_starts_when_the_drive_is_below_the_reserve() {
        // A floor no real drive can be above, so the refusal is about the
        // check and not about the machine this runs on. Refusing here rather
        // than four seconds in is the point: a recording that opens and is
        // stopped immediately by the same floor from the inside looks like a
        // bug rather than a full disk.
        let error = check_there_is_room(&settings_for(
            std::env::temp_dir().join("clipped-preflight.mkv"),
            u64::MAX,
        ))
        .expect_err("no drive has more room than u64::MAX");

        match error {
            SessionError::NotEnoughDiskSpace { free, minimum } => {
                assert_eq!(minimum, u64::MAX);
                assert!(
                    free < u64::MAX,
                    "the refusal should carry what was actually free"
                );
            }
            other => panic!("the refusal should name the room, not {other}"),
        }
    }

    #[test]
    fn a_drive_with_room_lets_the_recording_start() {
        // The other direction. Without this the test above would pass just as
        // well against a check that refused everything.
        check_there_is_room(&settings_for(
            std::env::temp_dir().join("clipped-preflight.mkv"),
            1,
        ))
        .expect("the temporary drive has more than one byte free");
    }

    #[test]
    fn a_caller_that_turned_the_guard_off_is_not_refused_by_it() {
        // Zero has to mean the same thing at both ends: the pre-flight check
        // and the guard inside the recording must agree, or a caller who turned
        // the guard off would still be refused before the file was created.
        check_there_is_room(&settings_for(
            std::env::temp_dir().join("clipped-preflight.mkv"),
            0,
        ))
        .expect("a floor of zero turns the check off");
    }

    #[cfg(windows)]
    #[test]
    fn a_volume_that_cannot_be_read_does_not_refuse_the_recording_here() {
        // Deliberate. The recording is about to try to create a file on that
        // volume, and *that* failure says something specific — "the recording
        // could not be created where it was asked for: the system cannot find
        // the path specified". Refusing here would replace a good message with
        // "the free space could not be read", and would stop recording on any
        // volume Windows declines to answer for.
        let gone = PathBuf::from(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\clips\a.mkv");

        check_there_is_room(&settings_for(gone, 1 << 30))
            .expect("an unreadable volume is not this check's refusal to make");
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

    // ---- the replay tap ----------------------------------------------------
    //
    // `drain` is the only production wiring this crate adds between an encoder
    // and a replay buffer, and it is four lines that a refactor could silently
    // drop. Nothing below needs a GPU or a desktop session: the encoder is
    // scripted and the writer is a real `MkvWriter` over a temporary file, so
    // the packets are the ones the file actually received.

    /// A real H.264 sequence and picture parameter set, in the Annex B form
    /// every Windows hardware encoder emits.
    ///
    /// Matroska will not declare an H.264 track without one. Taken from
    /// `crates/muxer/tests/mkv_writing.rs`, which read it back out of a file
    /// the pinned `libopenh264` build produced for a 640x360 picture.
    const H264_PARAMETER_SETS: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1e, 0x8c, 0x68, 0x0a, 0x02, 0xff, 0x96, 0x01,
        0xe1, 0x10, 0x8d, 0x40, 0x00, 0x00, 0x00, 0x01, 0x68, 0xce, 0x3c, 0x80,
    ];

    /// The picture those parameter sets describe.
    const TEST_SIZE: (u32, u32) = (640, 360);

    /// One packet an encoder is scripted to produce.
    #[derive(Debug, Clone)]
    struct ScriptedPacket {
        data: Vec<u8>,
        presentation: Duration,
        decode: Duration,
        keyframe: bool,
    }

    /// An encoder that produces a fixed list of packets and touches no
    /// hardware.
    ///
    /// A real encoder would need a GPU and a Direct3D device, and what is under
    /// test here is not encoding: it is where the packets an encoder produced
    /// end up. Scripting them is also what makes "the buffer got *this* packet"
    /// checkable, since every packet's bytes identify it.
    #[derive(Debug)]
    struct ScriptedEncoder {
        config: EncoderConfig,
        ready: VecDeque<ScriptedPacket>,
        /// The packet `next_packet` last handed out, which owns the bytes it
        /// borrowed — the same lifetime a real encoder's output buffer has.
        current: Option<ScriptedPacket>,
    }

    impl ScriptedEncoder {
        fn new(packets: impl IntoIterator<Item = ScriptedPacket>) -> Self {
            Self {
                config: EncoderConfig::new(
                    Codec::H264,
                    Resolution::new(TEST_SIZE.0, TEST_SIZE.1),
                    FrameRate::whole(60),
                    RateControl::constant(BitRate::megabits_per_second(18)),
                ),
                ready: packets.into_iter().collect(),
                current: None,
            }
        }
    }

    impl VideoEncoder for ScriptedEncoder {
        fn encoder(&self) -> EncoderKind {
            EncoderKind::Software
        }

        fn configuration(&self) -> &EncoderConfig {
            &self.config
        }

        fn parameter_sets(&self) -> &[u8] {
            H264_PARAMETER_SETS
        }

        fn submit(&mut self, _frame: &SourceFrame<'_>) -> Result<(), EncodeError> {
            Ok(())
        }

        fn next_packet(&mut self) -> Result<Option<EncodedPacket<'_>>, EncodeError> {
            self.current = self.ready.pop_front();
            Ok(self.current.as_ref().map(|packet| {
                EncodedPacket::new(
                    &packet.data,
                    packet.presentation,
                    packet.decode,
                    if packet.keyframe {
                        PictureKind::Keyframe
                    } else {
                        PictureKind::Predicted
                    },
                )
            }))
        }

        fn finish(&mut self) -> Result<(), EncodeError> {
            Ok(())
        }

        fn shut_down(&mut self) {}
    }

    /// A second of 60 fps packets whose bytes identify the frame they came
    /// from, a keyframe every half second.
    fn scripted_second() -> Vec<ScriptedPacket> {
        (0..60u64)
            .map(|frame| {
                let at = Duration::from_micros(frame * 1_000_000 / 60);
                ScriptedPacket {
                    // Long enough that two frames cannot collide, and the frame
                    // number is in the bytes so a packet that went to the wrong
                    // place is identifiable rather than merely miscounted.
                    data: frame.to_le_bytes().repeat(8),
                    presentation: at,
                    decode: at,
                    keyframe: frame % 30 == 0,
                }
            })
            .collect()
    }

    /// A path under the system temporary directory, removed when dropped.
    #[derive(Debug)]
    struct TemporaryRecording(PathBuf);

    impl TemporaryRecording {
        fn new(purpose: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "clipped-session-{purpose}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&directory).expect("a temporary directory can be created");
            Self(directory.join("recording.mkv"))
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TemporaryRecording {
        fn drop(&mut self) {
            if let Some(directory) = self.0.parent() {
                let _ = std::fs::remove_dir_all(directory);
            }
        }
    }

    /// A muxing thread's disk guard, turned off.
    ///
    /// The tests below are about where packets go. A probe of the temporary
    /// drive every two seconds inside them would be an unrelated syscall and an
    /// unrelated failure mode; the guard itself is tested in `crate::muxing`.
    fn unguarded(recording: &TemporaryRecording) -> crate::muxing::SpaceGuard {
        crate::muxing::SpaceGuard::new(recording.path(), 0)
    }

    /// The layout the tests below record into: one video track and no audio.
    ///
    /// What is under test here is the replay tap, which is a video path; the
    /// audio side has its own file (`crate::audio`).
    fn video_only_layout() -> RecordingLayout {
        RecordingLayout::new(
            VideoTrack::new(VideoCodec::H264, TEST_SIZE.0, TEST_SIZE.1)
                .with_codec_private(H264_PARAMETER_SETS.to_vec()),
        )
    }

    /// A real Matroska writer over a temporary file.
    fn writer_for(recording: &TemporaryRecording) -> MkvWriter {
        MkvWriter::create(recording.path(), &video_only_layout())
            .expect("a recording can be created in the temporary directory")
    }

    /// A muxing thread over a temporary file, with the disk guard turned off.
    fn muxing_for(recording: &TemporaryRecording) -> MuxingThread {
        MuxingThread::start(
            writer_for(recording),
            unguarded(recording),
            &video_only_layout(),
        )
        .expect("a layout with no audio tracks has nothing to refuse")
    }

    /// A thirty-second buffer at the rate a 1080p60 recording is given.
    fn replay_buffer() -> ReplayBuffer {
        ReplayBuffer::new(
            ReplayConfig::new(
                Duration::from_secs(30),
                BitRate::bits_per_second(18_662_400).expect("a real rate"),
            )
            .expect("thirty seconds is in range"),
        )
    }

    #[test]
    fn every_packet_the_file_receives_is_also_copied_into_the_replay_buffer() {
        // The tap, end to end: one encoder, two consumers. If the copy into the
        // buffer is dropped the buffer holds nothing; if the wrong packet is
        // pushed the bytes below do not match the frame they claim to be.
        let recording = TemporaryRecording::new("replay-tap");
        let muxing = muxing_for(&recording);
        let buffer = replay_buffer();
        let sinks = PacketSinks {
            muxing: &muxing,
            replay: Some(&buffer),
        };
        let scripted = scripted_second();
        let mut encoder = ScriptedEncoder::new(scripted.clone());

        let moved = drain(&mut encoder, &sinks).expect("every packet is accepted");
        let summary = muxing.finish().expect("the recording can be finalised");

        assert_eq!(moved, scripted.len());
        assert_eq!(
            summary.packets,
            scripted.len() as u64,
            "the file did not receive every packet the encoder produced"
        );

        let stats = buffer.stats();
        assert_eq!(
            stats.packets_buffered(),
            scripted.len() as u64,
            "the replay buffer was not fed what the file was: {stats:?}"
        );
        assert_eq!(stats.packets_discarded_before_first_keyframe(), 0);

        // Not merely the count. Every packet the buffer holds must be the
        // packet the encoder produced for that instant, byte for byte.
        let lease = buffer
            .lease_last(Duration::from_secs(30))
            .expect("a second of video is held");
        let held: Vec<(Vec<u8>, Duration, bool)> = lease
            .packets()
            .map(|packet| {
                (
                    packet.data().to_vec(),
                    packet.presentation_time(),
                    packet.is_keyframe(),
                )
            })
            .collect();
        let expected: Vec<(Vec<u8>, Duration, bool)> = scripted
            .iter()
            .map(|packet| (packet.data.clone(), packet.presentation, packet.keyframe))
            .collect();

        assert_eq!(held, expected);
    }

    #[test]
    fn a_recording_with_no_replay_buffer_still_writes_every_packet() {
        // The other half of the option: attaching a buffer is what a caller
        // chooses, and a recording without one must be untouched by any of it.
        let recording = TemporaryRecording::new("no-replay");
        let muxing = muxing_for(&recording);
        let sinks = PacketSinks {
            muxing: &muxing,
            replay: None,
        };
        let mut encoder = ScriptedEncoder::new(scripted_second());

        let moved = drain(&mut encoder, &sinks).expect("every packet is accepted");
        let summary = muxing.finish().expect("the recording can be finalised");

        assert_eq!(moved, 60);
        assert_eq!(summary.packets, 60);
    }

    #[test]
    fn a_packet_the_file_refused_is_not_left_in_the_replay_buffer() {
        // Why the muxer is written first and the buffer second. The file is the
        // recording; the buffer is a copy of it. A `drain` that fed the buffer
        // first would leave it holding video that never reached the file and
        // then report the failure, which is a buffer describing a recording
        // that does not exist.
        let recording = TemporaryRecording::new("refused-packet");
        let muxing = muxing_for(&recording);

        // An empty packet is what the writer refuses (`MuxError::EmptyPacket`),
        // and the writer thread stops at its first failure — so after this the
        // queue is closed and every later write is refused.
        muxing
            .write(&[], 0, 0, true)
            .expect("the queue accepts it; the writer is what refuses it");
        wait_until_the_writer_stops(&muxing);

        let buffer = replay_buffer();
        let sinks = PacketSinks {
            muxing: &muxing,
            replay: Some(&buffer),
        };
        let mut encoder = ScriptedEncoder::new(scripted_second());

        let error = drain(&mut encoder, &sinks).expect_err("the writer has stopped");

        assert!(matches!(error, SessionError::WriterLost), "{error}");
        assert_eq!(
            buffer.stats().packets_buffered(),
            0,
            "a packet the file refused was copied into the replay buffer anyway"
        );

        assert!(matches!(
            muxing.finish(),
            Err(SessionError::Mux(MuxError::EmptyPacket { .. }))
        ));
    }

    /// Waits for the writer thread to notice a failed write and stop.
    ///
    /// Bounded twice over, deliberately. The deadline is what stops a writer
    /// that never stops from hanging the suite instead of failing it, and the
    /// interval is chosen so that the probes cannot fill the queue before the
    /// deadline arrives — a `send` into a full queue blocks, and a blocking
    /// wait inside a bounded loop is not a bounded loop.
    fn wait_until_the_writer_stops(muxing: &MuxingThread) {
        let deadline = Instant::now() + Duration::from_secs(2);

        while Instant::now() < deadline {
            // Never written: the writer stops at its first failure, which has
            // already happened, so these only ever queue or be refused.
            if muxing.write(b"probe", 0, 0, true).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        panic!("the writer thread did not stop after refusing a packet");
    }

    // ---- the capture loop, against real frames ------------------------------
    //
    // Everything above stops short of `record_frames`, because the loop cannot
    // be entered without a frame and a frame is a Direct3D texture. These tests
    // create one — on the graphics hardware, or on WARP, which is part of
    // Windows and is what makes this run on a hosted CI runner — and drive the
    // whole of `record_frames` with it: the encoder is opened against the
    // texture's own device, the file is a real Matroska file, and the report
    // that comes back is the one a recording produces.
    //
    // That is the only way to test what a *minimised window mid-recording* does,
    // short of a window to minimise. The behaviour is a rule about acquisitions
    // and it lives in the loop; a test of the counter or of the report's
    // sentence would leave the wiring between them free (issue #383).

    /// A Direct3D 11 device, on the graphics hardware or failing that on WARP.
    ///
    /// [`None`] only on a machine that has neither, which no supported Windows
    /// install is: WARP ships with the operating system. The fallback is what
    /// lets these tests cover the loop on a runner with no usable GPU, and it is
    /// the arrangement `crates/encoder`'s software encoder tests already use.
    fn test_device() -> Option<windows::Win32::Graphics::Direct3D11::ID3D11Device> {
        use windows::Win32::Graphics::Direct3D::{
            D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
        };
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        };

        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            let mut device: Option<ID3D11Device> = None;
            // SAFETY: no adapter is named, which is what these driver types
            // require; the module handle is unused for them; the feature level
            // list and the out-parameter are live locals; and
            // `D3D11_SDK_VERSION` is the constant the header requires. On
            // success `device` holds one reference, released on drop.
            let created = unsafe {
                D3D11CreateDevice(
                    None,
                    driver,
                    windows::Win32::Foundation::HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&[D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
            };
            if created.is_ok() {
                if let Some(device) = device {
                    return Some(device);
                }
            }
        }

        None
    }

    /// A BGRA texture on `device`, filled with a flat grey.
    ///
    /// The picture does not matter here — what is under test is which
    /// acquisitions are counted as what, not what the encoder made of them — but
    /// the *format* does: `PixelFormat::Bgra8Unorm` is what the scripted format
    /// declares, and the software encoder checks the two agree before it copies
    /// anything (`crates/encoder/src/software/readback.rs`).
    fn test_texture(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        width: u32,
        height: u32,
    ) -> ID3D11Texture2D {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
        };

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
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let pixels = vec![0x40u8; (width as usize) * (height as usize) * 4];
        let initial = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: the description and the initial data both describe the
        // `pixels` buffer, which is live for the whole call and is not retained
        // — Direct3D copies it into the texture — and the out-parameter is a
        // live local. On success it holds one reference, released on drop.
        unsafe { device.CreateTexture2D(&description, Some(&initial), Some(&mut texture)) }
            .expect("a texture can be created on a device that was just created");
        texture.expect("CreateTexture2D reported success without returning a texture")
    }

    /// Settings for a recording of the scripted backend into `recording`.
    ///
    /// The encoder is named rather than left automatic so that the recording is
    /// made the same way on every machine — the software encoder needs no
    /// encoding hardware, which is what lets this run where NVENC is not — and
    /// the disk guard is turned off because what is under test is not the disk.
    fn loop_settings(recording: &TemporaryRecording) -> RecordingSettings {
        RecordingSettings::new(
            crate::settings::CaptureTargetSettings::window(0x1234, TEST_SIZE.0, TEST_SIZE.1),
            recording.path().to_path_buf(),
        )
        .with_minimum_free_space(0)
        .with_codec(crate::settings::CodecPreference::Fixed(Codec::H264))
        .with_encoder(crate::settings::EncoderPreference::Fixed(
            EncoderKind::Software,
        ))
    }

    /// Runs the whole recording loop over `steps` and returns what it reported.
    fn recording_of(purpose: &str, steps: impl IntoIterator<Item = Step>) -> RecordingReport {
        // Not a skip. WARP ships with Windows, so a machine that cannot create
        // a device here is a broken one and this is a failure worth seeing —
        // a test that quietly does nothing is worse than no test (AGENTS.md
        // section 54).
        let device =
            test_device().expect("no Direct3D 11 device could be created, on hardware or on WARP");
        let texture = test_texture(&device, TEST_SIZE.0, TEST_SIZE.1);

        let format = format_of(TEST_SIZE.0, TEST_SIZE.1);
        let recording = TemporaryRecording::new(purpose);
        let settings = loop_settings(&recording);
        let mut backend = ScriptedBackend::new(format, steps).drawing(texture);
        // Far beyond the script, so the loop ends because it was asked to and
        // not because the script ran out: the steps after the last one are
        // ordinary timeouts, which is what an idle source looks like.
        let stop = StopAfter::polls(40);

        record_frames(
            &settings,
            &stop,
            &mut backend,
            format,
            CaptureMethod::WindowsGraphicsCapture,
            &crate::RecordingOutputs::default(),
        )
        .expect("a recording with frames in it is a recording")
    }

    #[test]
    fn a_window_minimised_during_a_recording_is_counted_in_the_report_the_recording_produces() {
        // Issue #383's second half, and the one that is invisible when it goes
        // wrong: the recording carries on — alt-tabbing out of a fullscreen game
        // minimises it, and ending the session there would cost somebody the
        // rest of their game — but it must not carry on *in silence*. The count
        // in the report is what the log line, the summary sentence and the
        // desktop all read, and if the loop stops producing it the recording
        // goes back to writing nothing and saying nothing about it.
        //
        // The first frame is the one the recording releases rather than encodes:
        // it exists to say which device the textures are on.
        let report = recording_of(
            "minimised-mid-recording",
            [
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Minimised,
                Step::Minimised,
                Step::Minimised,
                Step::Drew,
                Step::Drew,
            ],
        );

        assert_eq!(
            report.times_target_minimised(),
            1,
            "the stretch the window was minimised for reached nothing the user can see"
        );
        assert_eq!(
            report.frames_captured(),
            4,
            "the recording must keep the frames either side of the minimise, on one timeline"
        );
        assert_eq!(
            report.end_reason(),
            EndReason::Stopped,
            "a minimised window must not end the recording"
        );
        assert!(
            report.frames_encoded() > 0,
            "the frames either side of the minimise were captured and never encoded"
        );
    }

    #[test]
    fn a_recording_the_window_kept_drawing_through_reports_no_minimised_stretch() {
        // The other direction, and what makes the test above mean something:
        // without it a loop that counted every acquisition as a minimise, or
        // reported a fixed 1, would pass just as well — and every ordinary
        // recording would end with a sentence about a window nobody minimised.
        let report = recording_of(
            "never-minimised",
            [
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Nothing,
                Step::Drew,
            ],
        );

        assert_eq!(report.times_target_minimised(), 0);
        assert_eq!(report.frames_captured(), 3);
    }

    #[test]
    fn two_stretches_minimised_are_two_and_ten_acquisitions_of_one_are_still_one() {
        // What the count is *of*. A window minimised for a minute produces ten
        // acquisitions a second and is one thing that happened; a window
        // minimised, restored and minimised again is two. Counting acquisitions
        // would report a number about the acquisition timeout, and counting
        // without noticing the frames in between would report one stretch for a
        // whole session.
        let report = recording_of(
            "two-stretches",
            [
                Step::Drew,
                Step::Drew,
                Step::Minimised,
                Step::Minimised,
                Step::Minimised,
                Step::Drew,
                Step::Minimised,
                Step::Minimised,
                Step::Drew,
            ],
        );

        assert_eq!(
            report.times_target_minimised(),
            2,
            "five acquisitions in two stretches is two stretches"
        );
        assert_eq!(report.frames_captured(), 3);
    }
}
