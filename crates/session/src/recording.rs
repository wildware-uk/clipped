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
//! ── loop ──              acquire, inspect, admit, submit, drain, queue
//!    first frame          fixes the epoch, and starts the audio threads on it
//!    capture failed       hand the backend to the fallback, take a replacement,
//!                         and open the encoder again on *its* device
//! stop the audio          before the queue closes, so the last samples are written
//! flush and finalise      on every path out, including a panic
//! ```
//!
//! # Capture that stops working part way through
//!
//! The backend is not fixed for the length of a recording. `CaptureFallback`
//! owns the policy — which failures are worth another backend, which are not,
//! and what a replacement has to produce before it is allowed to take over — and
//! this loop owns the consequences: reading every frame for a capture that has
//! silently gone black, and reopening the encoder when a replacement puts a
//! different graphics device behind the frames
//! ([issues #285 and #97](https://github.com/wildware-uk/clipped/issues/285),
//! `docs/capture-pipeline.md`). None of it costs anything while capture is
//! working, which is the ordinary case and the one the loop is measured on.
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
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use clipped_capture::{
    registered_backends, Acquisition, CaptureBackend, CaptureClock, CaptureConfig, CaptureError,
    CaptureFallback, CaptureMethod, CapturedFrame, DisplayAwake, FrameFormat,
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

/// How long a source may produce nothing before the recording says so.
///
/// A source only produces a frame when its content changes, so a paused game, a
/// static menu or a desktop nobody is touching produces none, and none of that
/// is a fault — which is exactly why this cannot be an error and why the
/// threshold has to be long. Half a minute is far beyond a loading screen and
/// well past the ten seconds
/// [`BlackFrameWatch`](clipped_capture::BlackFrameWatch) tolerates a capture
/// being entirely black, and it is the same half-minute
/// `CaptureFallback::note_silence` already uses for the same judgement.
///
/// What makes it worth stating at all is that the recorder cannot tell "nothing
/// is happening on screen" from "this screen has stopped existing as far as I am
/// concerned" — a display Windows has powered down produces the identical
/// timeout, from a source that is not idle at all
/// ([issue #461](https://github.com/wildware-uk/clipped/issues/461)).
/// Shared with [`RecordingReport`]'s summary sentence rather than written twice:
/// a log line that fires at half a minute and a sentence that fires at a minute
/// would disagree about the same recording.
pub(crate) const SILENT_SOURCE_THRESHOLD: Duration = Duration::from_secs(30);

/// Records `settings.target` until `stop` is raised, feeding `outputs` as well
/// as the file.
pub(crate) fn record(
    settings: &RecordingSettings,
    stop: &dyn crate::StopSignal,
    outputs: &crate::RecordingOutputs<'_>,
) -> Result<RecordingReport, SessionError> {
    // `CaptureFallback` rather than one chosen backend: it tries the method
    // selection preferred, and moves on to the next candidate when that one
    // cannot be created or cannot initialise against this target. Before
    // [issue #285](https://github.com/wildware-uk/clipped/issues/285) that was
    // built and called by nothing, so a preferred backend that failed to start
    // ended the recording before it began, on a machine where another backend
    // would have worked.
    let target = settings.target().target()?;
    let config = CaptureConfig::default().with_capture_cursor(settings.capture_cursor());
    // `start_preferring` rather than `start`: a recording is told two separate
    // things about capture, and conflating them is how a remembered method
    // becomes a pin. `capture_method` is what the user asked for and is obeyed
    // or reported; `remembered_capture_method` is what a previous recording of
    // the same game was observed to end on, which only decides *which candidate
    // is asked first* and falls back like any other
    // ([issue #286](https://github.com/wildware-uk/clipped/issues/286)).
    let started = CaptureFallback::start_preferring(
        registered_backends(),
        &target,
        &config,
        settings.capture_method(),
        settings.remembered_capture_method(),
    )
    .map_err(SessionError::from)?;

    let (fallback, backend, format) = started.into_parts();
    let method = fallback.current_method();

    // The one moment this fact exists outside this thread's stack. `fallback` is
    // dropped a few lines below — the frame loop holds the backend, not the
    // thing that chose it — so a reading not taken here is a reading nobody can
    // take, and the capture backend would go on existing only in the log
    // (`crate::capture_account`, issue #302). It is one small copy into a mutex,
    // before the first frame is asked for, and nothing on the frame loop touches
    // it again (AGENTS.md sections 17 and 20).
    if let Some(accounting) = outputs.capture {
        accounting.publish(fallback.status().into());
    }

    // Held here, on this thread, for exactly as long as capture is open —
    // through the first frame, the whole loop and the finalisation. A display
    // the operating system has powered down is not a dark source, it is no
    // source: Desktop Duplication delivers nothing from it even while a window
    // on it repaints, and the compositor Windows Graphics Capture reads drops to
    // about 4 Hz. Neither reports anything wrong, because from the API's side
    // nothing is (ADR 0015, issue #461).
    //
    // `DisplayAwake` is deliberately not `Send`: the requirement belongs to the
    // thread that took it and Windows drops it when that thread ends, so binding
    // it to this scope is what makes "held for the length of the capture" true
    // rather than intended.
    let awake = DisplayAwake::hold();

    tracing::info!(
        capture_backend = method.log_value(),
        width = format.size().width(),
        height = format.size().height(),
        pixel_format = %format.pixel_format(),
        // Recorded beside the backend because it is the first thing to check
        // when a recording turns out to contain a long stretch of nothing:
        // `false` here and a `longest_source_silence` of minutes is a screen
        // that went to sleep, which looks identical to an idle source in every
        // other figure a recording produces (issue #461).
        display_held = awake.is_held(),
        "capture started"
    );

    // The fallback goes into the loop rather than being read once and dropped.
    // That is the whole of issue #285: before it, a `CaptureFallback` was built,
    // asked which method it had chosen, and let go of at the end of this
    // function — so a capture that failed, restarted or went black *during* a
    // recording was never noticed and never recovered from, and the black-frame
    // detection issue #97 built and measured had no caller at all.
    //
    // The backend goes with it, by value. `CaptureFallback::recover` takes a
    // failed backend that way on purpose: it shuts the old one down before
    // asking the platform for another, which DXGI requires, and the loop cannot
    // go on using a backend it has given up (`docs/capture-pipeline.md`).
    // Shutting the surviving backend down is therefore `record_frames`'s, and it
    // does it as soon as the loop ends rather than after the trailer is
    // written — earlier than this function used to, which releases the display's
    // duplication or the compositor's frame pool sooner (AGENTS.md section 58).
    record_frames(settings, stop, fallback, backend, format, outputs)
}

/// Everything from the first frame to the finished file.
fn record_frames(
    settings: &RecordingSettings,
    stop: &dyn crate::StopSignal,
    mut fallback: CaptureFallback<'_>,
    backend: Box<dyn CaptureBackend>,
    mut format: FrameFormat,
    outputs: &crate::RecordingOutputs<'_>,
) -> Result<RecordingReport, SessionError> {
    let replay = outputs.replay;
    // The running backend, and the one thing in this function that can stop
    // existing while the recording is still going. `CaptureFallback::recover`
    // takes a failed backend by value and shuts it down before asking the
    // platform for a replacement, so between handing one over and being given
    // another there is nothing here to hold; when nothing can take over there
    // never is one again, and the loop ends in the same step that discovers it.
    // An `Option` says that out loud, in the one place it is true, rather than a
    // second backend value that would have to be kept in step with this one.
    let mut capture = Some(backend);
    let method = fallback.current_method();

    // Held for the whole of the recording: the encoder session is opened
    // against it and may not outlive it. `format` is passed by mutable
    // reference rather than by value because waiting for the first frame is
    // also where a target that resized between being measured and being
    // captured is followed, and everything below — the encoder, the size on
    // every submitted frame, the track in the header — is configured from it.
    // The resize goes through the fallback rather than through the backend so
    // that the format a replacement is judged against is the one the recording
    // ended up committed to; a fallback left holding the pre-resize size would
    // refuse every replacement for producing exactly the right frames.
    let mut device = first_frame_device(
        capture
            .as_deref_mut()
            .expect("the backend is only given up inside the loop below"),
        &mut fallback,
        stop,
        &mut format,
    )?;

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
    let mut encoder_kind = opened.kind;
    let mut encoder = opened.encoder;
    // What the file's video track is about to declare, kept so that a
    // replacement encoder can be held to it. A track's codec and its
    // out-of-band parameter sets go in the container's header and cannot change
    // part way through a file (ADR 0001), so an encoder reopened against a
    // replacement backend's device is only usable if it produces exactly these.
    let declared = TrackDeclaration {
        codec: opened.codec,
        parameter_sets: encoder.parameter_sets().to_vec(),
    };
    // Before the file, because a track's sampling rate and channel count go in
    // the container's header and only the device knows them. A source that
    // cannot be opened fails the recording here, while nothing has been
    // created and while the user can still act on it.
    let audio::OpenAudio {
        sources,
        fallback: audio_fallback,
    } = audio::open(settings)?;
    let layout = audio::declare(
        video_track(encoder.as_ref(), opened.codec, encode_size),
        &sources,
        settings.compatibility_mix(),
    );

    // Before the first packet and after the encoder: this is the moment both
    // things a replay buffer needs exist — the bitrate its memory ceiling is
    // sized from, and the track description a clip saved from it has to declare
    // (`crate::replay`). A buffer that could not be configured leaves the
    // recording untouched.
    let replay_buffer = crate::replay::start_buffer(&layout, bitrate, replay);
    // The same buffer, owned, for the audio threads: they are spawned rather
    // than scoped, so the borrow the packet loop uses will not do
    // (`crate::replay::ReplayRecording::buffer_handle`).
    let buffered_audio = replay.and_then(crate::replay::ReplayRecording::buffer_handle);

    // The container, and the thread that owns it — **or neither**. A capture
    // that writes no continuous recording opens no file, starts no muxing
    // thread and arms no disk guard, because there is nothing growing on a
    // drive for a guard to watch (`crate::settings::RecordingSettings::buffered`,
    // ADR 0018). Everything above this line is identical either way: one
    // capture, one encoder, one set of packets.
    let muxing = match settings.output() {
        Some(output) => {
            let writer = open_output(settings, output, &layout)?;
            Some(MuxingThread::start(
                writer,
                SpaceGuard::new(output, settings.minimum_free_space()),
                &layout,
            )?)
        }
        None => {
            // Somewhere for the clips to go, and room for them: both are
            // settled here, before a game has been running for an hour and
            // while somebody is still looking at their terminal (AGENTS.md
            // section 45). It is the same directory a recording's own parent is
            // created as, by the same rule — Clipped's own recordings folder is
            // made by whatever goes in it, and a directory the *user* named is
            // never created here.
            make_directory(settings.directory())?;
            check_there_is_room(settings)?;
            None
        }
    };
    let sinks = PacketSinks {
        muxing: muxing.as_ref(),
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
    // When the source last handed over a frame, so that a replay buffer can be
    // told how long it has been since. Nothing in `clipped-replay` reads a
    // clock, deliberately, and its media time only advances when a packet
    // arrives — so a buffer whose source went quiet an hour ago would answer
    // "the last thirty seconds" with the thirty before it went quiet, marked
    // complete. This loop is the thing waiting for the frame that never came,
    // and it is the only place that knows (issue #574).
    let mut last_frame_at: Option<Instant> = None;

    while !stop.is_requested() {
        // The one thing the acquisition below can ask for that cannot be done
        // while a frame borrowed from the backend is alive: giving the backend
        // up. It is decided inside the match and acted on after it.
        let mut interruption = None;

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
        // the writer thread (`crate::muxing`). A capture with no file is always
        // `Ample`: nothing of its is growing on the drive, and the rolling
        // buffer's own spill area is bounded by its window and gives up
        // spilling rather than filling a disk (`docs/replay-buffer.md`).
        match muxing
            .as_ref()
            .map_or(SpaceState::Ample, MuxingThread::space)
        {
            SpaceState::Ample => {}
            SpaceState::Low => {
                if !low_space_reported {
                    low_space_reported = true;
                    // Once, at `warn`, while there is still time to act on it.
                    // A line per frame would be a log nobody reads and a
                    // recording that spends its last minutes writing about
                    // itself.
                    tracing::warn!(
                        output = %RedactedPath::new(settings.directory()),
                        "the drive this recording is being written to is filling up; the \
                         recording will be finished cleanly if it reaches the reserve"
                    );
                }
            }
            SpaceState::Exhausted => {
                tracing::warn!(
                    output = %RedactedPath::new(settings.directory()),
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

        let backend = capture
            .as_deref_mut()
            .expect("a recovery that finds no replacement leaves this loop in the same step");

        match backend.acquire(ACQUIRE_TIMEOUT) {
            Ok(Acquisition::Frame(frame)) => {
                counters.captured += 1;
                counters.missed_by_source += u64::from(frame.frames_missed().unwrap_or(0));
                counters.note_drawing();
                last_frame_at = Some(Instant::now());

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
                    audio_threads = start_audio(
                        sources,
                        &layout,
                        clock,
                        muxing.as_ref(),
                        buffered_audio.as_ref(),
                    );
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

                // Last of all, for the same rule. This is the only capture
                // failure that returns no error: a capture that has silently
                // stopped working goes on handing over frames and every pixel
                // in them is zero, and a recording that never looks writes a
                // black file and reports success (issue #97).
                //
                // Called with **every** frame rather than every Nth, and that
                // is a decision rather than the obvious thing. The rationing
                // belongs to the value that knows what a sample costs:
                // `BlackFrameWatch::is_due` admits one GPU readback every
                // 500 ms and answers every other frame from a timestamp
                // comparison. Rationing *here* instead — sampling one frame in
                // thirty — would ration by frame rate rather than by time, so
                // the same rule would be twice a second at 60 fps and four
                // times a second at 120, and it would put a second answer to
                // "how often can this be afforded?" in a second crate
                // (AGENTS.md section 55).
                //
                // Measured rather than assumed, at 1920x1080 BGRA8 on this
                // machine (AGENTS.md section 19), in a release build:
                //
                // ```text
                // a frame it does not sample          4 ns
                // a frame it does sample (hardware)  48 µs
                // a frame it does sample (WARP)      57 µs
                // averaged over a second at 60 fps  1.6 µs a frame
                // ```
                //
                // So a watched capture costs about 96 µs of every second, and
                // the two frames a second it does touch spend 0.3% of a 16.7 ms
                // frame budget. The cost does not grow with the picture: the
                // sampler reads sixteen single pixels whatever the resolution,
                // and the expense is the `Map` rather than the 64 bytes.
                if let Some(run) = fallback.inspect(&frame) {
                    interruption = Some(Interruption::Black(run));
                }
            }
            Ok(Acquisition::Timeout) => {
                // **Not an error, and not a reason to stop.** A source that has
                // nothing new is the ordinary case, and a recording of a still
                // screen is a recording of a still screen.
                //
                // What it must not be is *unrecorded*. Before issue #461 this
                // arm was empty, so a recording whose source went quiet for an
                // hour said nothing about the hour: the file's timestamps
                // simply jump, because a gap in a source is filled rather than
                // closed and the video timeline has no frame in it
                // (`docs/av-sync.md`). That is the right thing to write and the
                // wrong thing to keep quiet about, and the case that made it
                // matter is a display Windows powered down, which is not an
                // idle source at all.
                //
                // The wait is counted rather than clocked: `ACQUIRE_TIMEOUT` is
                // how long the acquisition just spent, and reading `Instant` a
                // few times a second on the capture thread to learn the same
                // thing would be a clock read that buys nothing (AGENTS.md
                // section 20). It also makes the figure exactly reproducible in
                // a test, which a wall clock would not.
                counters.note_idle(ACQUIRE_TIMEOUT);
                note_source_silence(replay_buffer, last_frame_at);
                // And the fallback's own account of the same silence, which is
                // what `silent_for` answers a diagnostics screen with. It is
                // deliberately not a failure — the module documentation of
                // `clipped_capture::fallback` says why, and this loop's
                // `SILENT_SOURCE_THRESHOLD` above uses that same judgement and
                // that same half-minute rather than a second rule.
                fallback.note_silence(ACQUIRE_TIMEOUT);
            }
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

                // The same silence as a timeout, with a name — and the common
                // one: this is how a source most often stops producing frames,
                // so a replay buffer told only about timeouts would be wrong in
                // exactly the case it is wrong in most (issue #574).
                note_source_silence(replay_buffer, last_frame_at);
                // Told to the fallback for the same reason, and with the same
                // consequence, which is none: a minimised window is the case
                // that would cost somebody their preferred backend every time
                // they alt-tabbed, and both Windows backends wait it out.
                fallback.note_silence(ACQUIRE_TIMEOUT);
            }
            Ok(Acquisition::SizeChanged(size)) => {
                // Matroska fixes a track's dimensions in the header, and the
                // encoder is configured for one size, so there is no honest way
                // to carry on into the same file. The recording is finished
                // where it is rather than filled with frames of a size the
                // track does not declare.
                //
                // **This is a seam, not the end of a sitting.** A session
                // follows a size change with the next file in the same session,
                // and does not scale to keep one file's dimensions constant:
                // ADR 0012 has the decision, the alternatives it closes and what
                // it costs. What this loop owes that decision is a file that is
                // complete up to the change and a reason on the report the
                // session can read, which is what the two lines below produce.
                // `clipped-recorder record` writes to the one path it was given
                // and therefore does stop here (`docs/recorder-cli.md`).
                tracing::warn!(
                    width = size.width(),
                    height = size.height(),
                    "the recorded window changed size, which one file cannot follow; this \
                     recording was finished at that point and an automatic session continues \
                     in the next file"
                );
                end_reason = EndReason::TargetResized;
                break;
            }
            Err(CaptureError::TargetLost { .. }) => {
                // Answered here rather than by the fallback, which agrees:
                // `FailureResponse::Fatal` is what `response_to` reads this as,
                // and no backend records a window that has closed. Keeping the
                // arm means the recording ends as `TargetLost` — a clean end a
                // session follows — rather than as a capture failure.
                tracing::info!("the recorded window closed; finishing the recording");
                end_reason = EndReason::TargetLost;
                break;
            }
            Err(error) => {
                // **Not the end of the recording any more.** Before issue #285
                // every other capture failure broke out of the loop here, so a
                // driver reset or a window that turned out to be uncapturable
                // finished the file where it stood even on a machine whose other
                // backend would have carried on.
                interruption = Some(Interruption::Failed(error));
            }
        }

        let Some(interruption) = interruption else {
            continue;
        };

        // The backend is given up here and only here, which is what the
        // `expect` at the top of the loop rests on.
        let failed = capture
            .take()
            .expect("the backend was taken from the same `Option` at the top of this turn");
        match resume(
            &mut fallback,
            failed,
            interruption,
            stop,
            settings,
            format,
            encode_size,
            &declared,
        ) {
            Resumed::Capture(resumed) => {
                // The old encoder's held pictures, into the file, before
                // anything the replacement produces goes in after them. A
                // failure here is the recording's, not the recovery's: the
                // capture is running again either way.
                //
                // Afterwards rather than before `resume`, which costs a second
                // encode session being open for the length of the recovery. The
                // alternative costs more: an encoder shut down before a recovery
                // that then fails is one the finalisation below would flush
                // again, and a `flush` of a session that has ended would turn a
                // clean stop into a reported failure. A session held for a few
                // milliseconds — or, at worst, for the ten seconds the
                // replacement is given to draw — is refused by `encoding::open`
                // with a message about session limits, which ends the recording
                // exactly where it would have ended without any of this.
                if let Err(error) = flush(encoder.as_mut(), &sinks, &mut counters) {
                    failure = Some(error);
                    capture = Some(resumed.backend);
                    break;
                }
                encoder.shut_down();

                capture = Some(resumed.backend);
                encoder = resumed.encoder;
                encoder_kind = resumed.encoder_kind;
                // Held rather than read: a `FrameDevice` is a reference to the
                // graphics device the encoder session was opened against, and
                // it is kept for as long as that session lives
                // (`crate::windows::device`). Swapped rather than assigned so
                // that the release of the old one is a statement rather than a
                // side effect of an assignment nothing reads — and it happens
                // *after* the session opened against it was shut down above.
                drop(core::mem::replace(&mut device, resumed.device));
                // The pacing gate is not reset. It holds the recording to the
                // rate it was asked for against the media timeline, which the
                // replacement carries on rather than restarts, and a gate that
                // forgot would admit a burst at the seam.
                tracing::warn!(
                    capture_backend = resumed.change.to().log_value(),
                    previous_capture_backend = resumed.change.from().log_value(),
                    trigger = resumed.change.trigger().log_value(),
                    encoder = %encoder_kind.log_encoder_family(),
                    frames_encoded = counters.encoded,
                    "the capture backend was replaced mid-recording and the encoder was reopened \
                     against the replacement's graphics device; the recording continues in the \
                     same file"
                );
                // The reading a window can ask for, published again (issues
                // #302 and #285). `CaptureAccounting` was built to be written
                // more than once and until now never was: the account was
                // published where the backend first opened, and the fallback
                // was consulted nowhere, so the two halves of "which backend is
                // this recording using" were each correct and never met.
                //
                // `fallback.status()` rather than `resumed.change` alone,
                // because a window wants the whole history — the method now
                // running *and* every replacement before it — and the status
                // already carries both.
                //
                // Here rather than inside `resume`: that function returns three
                // ways and only this one has a running capture to describe. It
                // is also off the frame path, on a branch a recording normally
                // never takes, which is what AGENTS.md sections 17 and 20
                // require of anything a diagnostics screen wants.
                if let Some(accounting) = outputs.capture {
                    accounting.publish(fallback.status().into());
                }
            }
            Resumed::Ends(reason) => {
                end_reason = reason;
                break;
            }
            Resumed::Fails(error) => {
                failure = Some(error);
                break;
            }
        }
    }

    // As soon as the loop is over, rather than after the trailer: a display's
    // duplication and a compositor's frame pool are the platform's, and nothing
    // below this line needs a frame (AGENTS.md section 58). `None` when a
    // recovery found nothing to take over, which is the one way out of the loop
    // with no backend to shut down — the fallback shut that one down itself,
    // before it went looking.
    if let Some(mut backend) = capture.take() {
        backend.shut_down();
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
    let audio_tracks = stop_audio(audio_threads, muxing.as_ref());

    // The muxer's account of the file when there was one, and nothing when
    // there was not. Everything the report takes from it below has a figure of
    // its own for the second case (`Produced`).
    let summary = match muxing.map(MuxingThread::finish).transpose() {
        Ok(summary) => summary,
        Err(error) => return Err(reported_failure(failure, error)),
    };

    let status = fallback.status();
    let report = RecordingReport {
        output: settings.output().map(Path::to_path_buf),
        // The method that was *running when the file was finished*, which after
        // a fallback is not the one selection chose. Reporting the chosen one
        // would name a backend that stopped working part way through, and
        // `capture_changes` below is what makes the answer explicable rather
        // than surprising (issue #285's third criterion).
        capture_method: status.current_method(),
        // What was *asked for*, beside what happened. Without it a caller
        // holding a report cannot tell a method that was chosen from a method
        // that was pinned, and "remember what worked" must not remember a
        // recording that was obeying an instruction (issue #286).
        capture_setting: status.setting(),
        capture_changes: status.changes().to_vec(),
        // Carried out of `audio::open` rather than re-derived here: by the time
        // the file is finished the only trace of a machine that could not scope
        // its audio would be the track names, and a caller comparing a
        // recording against the settings that produced it should not have to
        // infer it from those (issue #604).
        audio_fallback,
        // The same rule for the encoder: a replacement backend's device may be
        // one this machine's preferred encoder cannot open a session on, and
        // the family that actually encoded the last stretch is the one the
        // report names.
        encoder: encoder_kind,
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
        longest_source_silence: counters.longest_silence,
        packets_written: summary
            .as_ref()
            .map_or(counters.produced.packets, |summary| summary.packets),
        timestamps_corrected: summary
            .as_ref()
            .map_or(0, clipped_muxer::RecordingSummary::timestamps_corrected),
        duration: summary
            .as_ref()
            .map_or_else(|| counters.produced.duration(), |summary| summary.duration),
        end_reason,
        audio_tracks,
    };

    if let Some(empty) = summary
        .as_ref()
        .map(|summary| summary.audio_tracks_without_packets)
        .filter(|empty| *empty > 0)
    {
        // The muxer counted the tracks nothing was ever written to; each source
        // has already said what it produced, and this is the file's own account
        // of the same fact. Both are worth having: a track can be empty because
        // its device was silent, and it can be empty because everything it
        // produced preceded the recording.
        tracing::warn!(
            empty_audio_tracks = empty,
            "the recording has audio tracks with no audio in them"
        );
    }

    tracing::info!(
        // The file, or the directory the capture belonged to when there was no
        // file. `wrote_a_recording` is what tells the two apart, because a path
        // alone cannot: a support bundle showing a capture that produced 60,000
        // packets and no file has to be readable as the mode it was, and not as
        // a recording that went missing.
        output = %RedactedPath::new(report.output().unwrap_or_else(|| settings.directory())),
        wrote_a_recording = settings.writes_a_recording(),
        // The method in use at the end, and how many times it changed. Both,
        // rather than only the first: "Desktop Duplication" in a recording that
        // started on Windows Graphics Capture is a fact with no explanation
        // attached, and `capture_backend_changes` of 0 is what says the ordinary
        // recording was ordinary (SPEC.md section 36).
        capture_backend = report.capture_method().log_value(),
        capture_backend_changes = report.capture_changes().len(),
        frames_encoded = report.frames_encoded(),
        frames_captured = report.frames_captured(),
        frames_skipped_for_rate = report.frames_skipped_for_rate(),
        frames_dropped_writer_behind = report.frames_dropped_writer_behind(),
        frames_missed_by_source = report.frames_missed_by_source(),
        times_target_minimised = report.times_target_minimised(),
        longest_source_silence_seconds = report.longest_source_silence().as_secs_f64(),
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
            // A covered range that stops well short of the recording's length is
            // explained by these two and by nothing else in the line: the source
            // stopped producing pictures, and the buffer let go of everything
            // from before each stretch rather than let a save reach across it.
            source_gaps = stats.source_gaps(),
            segments_dropped_after_a_source_gap = stats.segments_dropped_after_a_source_gap(),
            segments_sealed_at_the_ceiling = stats.segments_sealed_at_the_ceiling(),
            packets_discarded_over_ceiling = stats.packets_discarded_over_ceiling(),
            "replay buffer at the end of the recording"
        );
    }

    conclude(report, failure)
}

/// Why the recording loop gave its capture backend up.
///
/// The two failures [`CaptureFallback`] recovers from, in the shape the loop
/// meets them. They are separate because they arrive differently: one is an
/// error the backend returned, and the other is a backend that returned no
/// error at all and has been handing over frames with nothing in them.
#[derive(Debug)]
enum Interruption {
    /// The backend reported a failure.
    Failed(CaptureError),
    /// The backend is running, and that is the problem: it has been producing
    /// nothing but black for longer than a fade or a loading screen lasts.
    Black(clipped_capture::BlackRun),
}

/// What the file's video track declares, and what a replacement encoder has to
/// match before a packet of its may go into it.
///
/// Matroska writes a track's codec and its out-of-band parameter sets — the
/// sequence and picture parameter sets for H.264 and HEVC, the sequence header
/// for AV1 — once, in the header, before the first frame (ADR 0001,
/// `crates/muxer/src/writer.rs`). A second encoder session that would produce a
/// different stream cannot be written into that track: a player decodes
/// everything after the change against a codec header describing something else,
/// in a file that looks finished. So it is checked rather than assumed.
///
/// In practice the two match. The replacement is configured from the same
/// [`crate::settings::RecordingSettings`], at the same size, for the same codec,
/// so an encoder of the same family produces the same parameter sets. The case
/// this exists for is the one where it does not: a driver reset that leaves
/// NVENC unopenable, where the reopened session is the software encoder and its
/// stream is its own.
struct TrackDeclaration {
    codec: Codec,
    parameter_sets: Vec<u8>,
}

impl TrackDeclaration {
    /// Why a reopened encoder cannot be carried into this file, or [`None`] when
    /// it can.
    ///
    /// A separate function from the caller because it is the one judgement in
    /// the recovery path that has to be right and cannot be exercised through
    /// it: forcing a real second encoder to produce different parameter sets
    /// needs a machine whose preferred encoder disappears between two calls
    /// (AGENTS.md section 23 on testing the rule where the rule is).
    fn refuses(
        &self,
        kind: clipped_encoder::EncoderKind,
        codec: Codec,
        parameter_sets: &[u8],
    ) -> Option<String> {
        // Named as a user reads them rather than as a log field: this goes
        // into `SessionError::EncoderCannotFollowCapture`, which is a sentence
        // somebody is shown (AGENTS.md section 15).
        if codec != self.codec {
            return Some(format!(
                "the {kind} encoder opened against its graphics device produces {codec} and \
                 this recording's video track is {}",
                self.codec
            ));
        }
        if parameter_sets != self.parameter_sets {
            return Some(format!(
                "the {kind} encoder opened against its graphics device produces a different \
                 {codec} stream from the one this recording's video track describes"
            ));
        }
        None
    }
}

/// A recording that has capture back under it.
struct Resumption {
    backend: Box<dyn CaptureBackend>,
    /// The replacement's graphics device, which must outlive the encoder below.
    device: FrameDevice,
    encoder: Box<dyn VideoEncoder>,
    encoder_kind: clipped_encoder::EncoderKind,
    change: clipped_capture::MethodChange,
}

/// What came of trying to put a working capture back under a recording.
///
/// Three answers, because there are three ways out of the loop that asks:
/// carry on, finish the file for a reason a session can follow, or fail.
enum Resumed {
    /// Capture is running again, on a backend the encoder is bound to.
    Capture(Box<Resumption>),
    /// The recording ends here with the file complete, for this reason.
    Ends(EndReason),
    /// The recording ends here, and this is what went wrong. The file is
    /// finalised on the way out either way.
    Fails(SessionError),
}

/// Puts a working capture back under a recording whose backend stopped, and
/// points the encoder at it.
///
/// This is the half of [issue #285](https://github.com/wildware-uk/clipped/issues/285)
/// that is not wiring. `CaptureFallback` will hand back a running replacement
/// for a backend that failed — but the replacement creates its own Direct3D
/// device, and an encoder session can only bind textures belonging to the device
/// it was opened against (`crate::windows::device`). So the encoder is opened
/// again, against a frame from the new backend, and the recording carries on
/// into the same file only if what the new session produces is what that file's
/// track already declares.
///
/// **The failed backend is gone by the time this returns, whatever it returns.**
/// `CaptureFallback::recover` takes it by value and shuts it down before asking
/// the platform for another, because DXGI gives a process one duplication per
/// display and a replacement would be refused while the old one still held it.
///
/// The costs are deliberate and are paid only here, on a path a recording
/// normally never takes: one frame of the replacement is acquired and released
/// to learn its device — the same price the first frame of a recording pays —
/// and one encoder session is opened. Neither happens while capture is working.
#[allow(clippy::too_many_arguments)]
fn resume(
    fallback: &mut CaptureFallback<'_>,
    failed: Box<dyn CaptureBackend>,
    interruption: Interruption,
    stop: &dyn crate::StopSignal,
    settings: &RecordingSettings,
    committed: FrameFormat,
    encode_size: (u32, u32),
    declared: &TrackDeclaration,
) -> Resumed {
    let recovery = match interruption {
        Interruption::Failed(cause) => fallback.recover(failed, cause),
        Interruption::Black(run) => fallback.recover_from_black_frames(failed, run),
    };
    let (mut backend, change) = match recovery {
        Ok(recovery) => recovery.into_parts(),
        // Every reason nothing could take over is already in this error, with
        // each candidate and what it said (`FallbackError::Exhausted`), and
        // `From<FallbackError>` keeps the two that had names of their own — a
        // window that closed is still reported as a lost target rather than as
        // a capture nothing could replace.
        Err(error) => return Resumed::Fails(error.into()),
    };

    // The replacement's own first frame, for the device its textures are on.
    // `CaptureFallback` has already refused any replacement whose format is not
    // the one this recording committed to, so this cannot be where the size
    // changes — except by the target itself resizing in the same moment, which
    // is what the check below is for.
    let mut resumed = committed;
    let device = match first_frame_device(backend.as_mut(), fallback, stop, &mut resumed) {
        Ok(device) => device,
        // A stop request while the replacement was starting is a stop request.
        // Reporting it as a capture that produced no frames would tell somebody
        // who pressed Ctrl+C that their recording failed.
        Err(_) if stop.is_requested() => {
            backend.shut_down();
            return Resumed::Ends(EndReason::Stopped);
        }
        Err(error) => {
            backend.shut_down();
            return Resumed::Fails(error);
        }
    };
    if resumed != committed {
        // The target changed size while the replacement was starting. One file
        // cannot follow that, for the reason the `SizeChanged` arm of the loop
        // gives: a session finishes this file and starts the next one (ADR
        // 0012). Reported as the resize it is rather than as the fallback that
        // happened to be in flight when it landed.
        tracing::warn!(
            width = resumed.size().width(),
            height = resumed.size().height(),
            "the recorded window changed size while a replacement capture backend was starting; \
             this recording was finished at that point"
        );
        backend.shut_down();
        return Resumed::Ends(EndReason::TargetResized);
    }

    let opened = match encoding::open(
        &device.as_graphics_device(),
        settings,
        encode_size,
        resumed.pixel_format(),
    ) {
        Ok(opened) => opened,
        Err(error) => {
            backend.shut_down();
            return Resumed::Fails(error);
        }
    };

    if let Some(reason) =
        declared.refuses(opened.kind, opened.codec, opened.encoder.parameter_sets())
    {
        // Shut down in this order for the reason `crate::windows::device`
        // gives: the session goes before the device it was opened against.
        let mut encoder = opened.encoder;
        encoder.shut_down();
        drop(encoder);
        drop(device);
        backend.shut_down();
        return Resumed::Fails(SessionError::EncoderCannotFollowCapture {
            method: change.to().to_string(),
            reason,
        });
    }

    Resumed::Capture(Box::new(Resumption {
        backend,
        device,
        encoder: opened.encoder,
        encoder_kind: opened.kind,
        change,
    }))
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
///
/// **A capture that wrote no file still fails on no video**, and has nothing to
/// remove. It is the same diagnosis for the same reason — a window that is not
/// drawing produced nothing, and a buffer nothing reached is a hotkey that will
/// never produce a clip — and saying "there is no file" would be describing the
/// mode rather than the fault (ADR 0018).
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

    let Some(output) = report.output() else {
        tracing::info!(
            end_reason = report.end_reason().token(),
            times_target_minimised = report.times_target_minimised(),
            longest_source_silence_seconds = report.longest_source_silence().as_secs_f64(),
            "no video reached this capture, and it wrote no file for there to be anything to \
             remove"
        );
        return Err(SessionError::NoFrames);
    };

    match std::fs::remove_file(output) {
        Ok(()) => tracing::info!(
            output = %RedactedPath::new(output),
            end_reason = report.end_reason().token(),
            times_target_minimised = report.times_target_minimised(),
            longest_source_silence_seconds = report.longest_source_silence().as_secs_f64(),
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
    muxing: Option<&MuxingThread>,
    replay: Option<&std::sync::Arc<clipped_replay::ReplayBuffer>>,
) -> Option<AudioThreads> {
    (!sources.is_empty()).then(|| AudioThreads::start(sources, layout, clock, muxing, replay))
}

/// Stops the audio threads and collects what each source produced.
///
/// Empty for a recording that had none. The count of buffers the writer had no
/// room for is read from the queue rather than summed from the reports, because
/// a thread that panicked has no report and the buffers it lost are still
/// missing from the file. A capture with no file has no writer to fall behind,
/// so the count is zero and nothing is said.
fn stop_audio(
    threads: Option<AudioThreads>,
    muxing: Option<&MuxingThread>,
) -> Vec<AudioTrackReport> {
    let tracks = threads.map_or_else(Vec::new, |mut threads| threads.finish());

    let dropped = muxing.map_or(0, MuxingThread::audio_buffers_dropped);
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

/// Tells the replay buffer how long the source has been producing nothing.
///
/// The join between an acquisition that found no frame and a buffer that cannot
/// tell that from no time having passed. `clipped-replay` measures everything in
/// media time, which only advances when a packet arrives, and reads no wall
/// clock on purpose (`docs/replay-buffer.md`, AGENTS.md section 25) — so the
/// stretch a minimised window or a sleeping display spends delivering nothing is
/// invisible from inside it. Without this, "save the last thirty seconds" during
/// such a stretch is answered with the thirty seconds before it began, marked
/// complete and hours old (issue #574).
///
/// Whole stretch rather than an increment, so calling it on every acquisition
/// that found nothing is correct. Nothing happens before the first frame: there
/// is no buffer to mislead and no stretch to measure from.
fn note_source_silence(replay: Option<&ReplayBuffer>, last_frame_at: Option<Instant>) {
    let (Some(replay), Some(last_frame_at)) = (replay, last_frame_at) else {
        return;
    };

    replay.note_source_silence(last_frame_at.elapsed());
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
    /// How long the source has produced nothing for, in the run happening now.
    ///
    /// The sum of the acquisition timeouts that have gone by without a frame,
    /// not a clock reading — see the `Acquisition::Timeout` arm for why.
    silence: Duration,
    /// The longest such run in the whole recording, which is the number worth
    /// reporting: a recording that lost four minutes in one stretch is a
    /// different thing from one that lost four minutes a tenth of a second at a
    /// time across an afternoon of a mostly-still screen.
    longest_silence: Duration,
    /// Whether the run happening now has already been mentioned, so that a
    /// source quiet for an hour produces one line rather than thirty-six
    /// thousand (AGENTS.md section 35).
    silence_reported: bool,
    /// Every packet the encoder produced, and the span they cover.
    ///
    /// The muxer counts both for a recording, out of the file it actually
    /// wrote, and its figures are the ones a recording reports. A capture that
    /// writes no file has no muxer to ask, and a report with no length and no
    /// packet count would say a buffered sitting did nothing — which is what
    /// `clipped-recorder replay --no-recording` prints when it ends, and what
    /// its session record keeps (ADR 0018).
    produced: Produced,
}

/// What the encoder produced, counted where the muxer cannot.
///
/// Deliberately *not* a [`clipped_muxer::RecordingSummary`] built by hand: that
/// type is the muxer's account of a file, and filling one in for a capture that
/// wrote no file would put "packets written" against packets nothing wrote. The
/// two figures here are the only ones a buffered capture can honestly claim,
/// and [`RecordingReport`] takes them from here or from the muxer accordingly.
#[derive(Debug, Default)]
struct Produced {
    /// How many encoded packets were drained.
    packets: u64,
    /// The presentation time of the first and last of them, in nanoseconds.
    first: Option<i64>,
    last: i64,
}

impl Produced {
    /// Records one packet's presentation time.
    fn note(&mut self, presentation_nanos: i64) {
        self.packets += 1;
        self.first.get_or_insert(presentation_nanos);
        self.last = self.last.max(presentation_nanos);
    }

    /// The span between the first and last packets, which is what
    /// [`clipped_muxer::RecordingSummary::duration`] measures for a file.
    fn duration(&self) -> Duration {
        let Some(first) = self.first else {
            return Duration::ZERO;
        };
        Duration::from_nanos(self.last.saturating_sub(first).unsigned_abs())
    }
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

    /// Records an acquisition that produced no frame.
    ///
    /// Kept apart from [`note_minimised`](Self::note_minimised) even though both
    /// mean "no frame arrived", for the reason `Acquisition` splits them: a
    /// timeout means the source had nothing new, and a minimised window means
    /// the source has stopped existing until somebody acts. Counting a minimised
    /// window's stretches as silence as well would say the same thing twice in
    /// two vocabularies and make both numbers harder to read.
    fn note_idle(&mut self, waited: Duration) {
        self.silence = self.silence.saturating_add(waited);
        self.longest_silence = self.longest_silence.max(self.silence);

        if self.silence_reported || self.silence < SILENT_SOURCE_THRESHOLD {
            return;
        }
        self.silence_reported = true;
        // At `warn` and once per stretch. This is a recording that is
        // accumulating nothing, and the sessions it happens to are the ones
        // nobody is watching a console for.
        tracing::warn!(
            frames_captured = self.captured,
            silent_seconds = self.silence.as_secs_f64(),
            "the capture source has produced no frames for a long stretch, so the recording \
             has nothing in it for that time; a still screen does this legitimately, and so \
             does a display the operating system has powered down"
        );
    }

    /// Records a frame arriving, which ends any stretch that was running.
    fn note_drawing(&mut self) {
        if self.silence_reported {
            tracing::info!(
                frames_captured = self.captured,
                silent_seconds = self.silence.as_secs_f64(),
                "the capture source is producing frames again"
            );
        }
        self.silence = Duration::ZERO;
        self.silence_reported = false;

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
/// The file when there is one, and the replay buffer as well when one was
/// given. The two are a pair rather than two arguments because they are always
/// passed together, and because the pairing is the point: **there is one
/// encoder**. A recording and a replay buffer running at the same time encode
/// once and copy the bytes twice, which is what SPEC.md section 16 asks for and
/// what makes a replay buffer nearly free while a recording is running.
///
/// Both are optional, and the four combinations are all reachable and all
/// meaningful:
///
/// | muxing | replay | what it is |
/// | --- | --- | --- |
/// | yes | no | `clipped-recorder record` |
/// | yes | yes | `clipped-recorder replay` |
/// | no | yes | `clipped-recorder replay --no-recording` (ADR 0018) |
/// | no | no | a capture that encodes and keeps nothing, which only a
/// measurement asks for |
#[derive(Debug, Clone, Copy)]
struct PacketSinks<'sinks> {
    muxing: Option<&'sinks MuxingThread>,
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
    // A capture with no file has no writer to fall behind, so no frame is ever
    // dropped for one. That is not a saving to advertise — it is the absence of
    // the thing that could not keep up.
    if sinks.muxing.is_some_and(MuxingThread::is_behind) {
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
    let packets = drain(encoder, sinks, counters)?;
    report_submission_over_headroom(packets);
    Ok(())
}

/// Moves every packet the encoder has ready into the writer's queue and, when
/// there is one, into the replay buffer, and reports how many that was.
///
/// **The muxer first, the buffer second.** The file is the recording and the
/// buffer is a copy of it, so a packet is never held back from the file for the
/// buffer's sake. A capture with no file skips the first half and is otherwise
/// identical: the same encoder, the same packets, the same push (ADR 0018).
///
/// The replay buffer's result is not a `Result`: it copies bytes into memory it
/// already owns and has no failure to report. A buffer that has reached its
/// ceiling drops its own oldest segments — or, for an encoder whose keyframes
/// are further apart than it can hold, the video it cannot buffer — and says so
/// in its statistics rather than refusing the packet, because a replay buffer
/// must never be able to end a recording (AGENTS.md section 17).
fn drain(
    encoder: &mut dyn VideoEncoder,
    sinks: &PacketSinks<'_>,
    counters: &mut Counters,
) -> Result<usize, SessionError> {
    let mut moved = 0;
    while let Some(packet) = encoder.next_packet()? {
        // Both timestamps are nanoseconds from the same zero as the frames that
        // went in, which is what the muxer's `PacketTimestamp` wants
        // (`crates/encoder/src/packet.rs`).
        let presentation = nanos_of(packet.presentation_time());
        if let Some(muxing) = sinks.muxing {
            muxing.write(
                packet.data(),
                presentation,
                nanos_of(packet.decode_time()),
                packet.is_keyframe(),
            )?;
        }
        if let Some(replay) = sinks.replay {
            replay.push(&packet);
        }
        // Counted here rather than only by the muxer, because a capture with no
        // muxer would otherwise report a sitting of no packets and no length.
        // For a recording the muxer's own figures win: they are what reached
        // the file, which is what a recording's report is about.
        counters.produced.note(presentation);
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
    let result = drain(encoder, sinks, counters).map(|_| ());
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
    fallback: &mut CaptureFallback<'_>,
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
                // Through the fallback rather than through the backend, which
                // is what its own documentation asks for: it judges every later
                // replacement against the format this recording committed to,
                // and a resize done behind its back would leave it refusing
                // replacements for producing exactly the right frames.
                let resized = fallback.resize(backend, size)?;
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
    output: &Path,
    layout: &RecordingLayout,
) -> Result<MkvWriter, SessionError> {
    // The recordings directory is Clipped's own and is created by the recording
    // that goes in it, which is what keeps a run that records nothing from
    // leaving an empty folder in somebody's videos (docs/recorder-cli.md). A
    // directory the *user* named is never created here: the caller has already
    // refused a `--output` inside one that does not exist.
    if let Some(directory) = output.parent() {
        make_directory(directory)?;
    }

    check_there_is_room(settings)?;

    // `MkvWriter::create` refuses to truncate anything that is already there
    // (AGENTS.md section 56), so replacing an existing recording is done here,
    // deliberately, and only when the caller asked for it.
    if settings.overwrite() && output.exists() {
        std::fs::remove_file(output).map_err(|source| SessionError::OutputDirectory { source })?;
    }

    Ok(MkvWriter::create(output, layout)?)
}

/// Creates a directory a capture is about to put files in, if it is not there.
///
/// One rule for both modes: a recording creates the folder its file goes in and
/// a buffered capture creates the folder its clips go in, and neither creates a
/// directory the *user* named — the caller refused an output inside one that
/// does not exist long before this (AGENTS.md section 55).
///
/// # Errors
///
/// [`SessionError::OutputDirectory`].
fn make_directory(directory: &Path) -> Result<(), SessionError> {
    if directory.as_os_str().is_empty() || directory.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(directory).map_err(|source| SessionError::OutputDirectory { source })
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
///
/// A capture that writes no file is asked the same question about the directory
/// its clips go in. It is the only disk check such a capture gets — there is no
/// growing file for [`SpaceGuard`] to watch — and it is worth having, because a
/// hotkey pressed an hour in on a drive that was already full is the one moment
/// somebody cannot be told anything useful (ADR 0018).
fn check_there_is_room(settings: &RecordingSettings) -> Result<(), SessionError> {
    let minimum = settings.minimum_free_space();
    if minimum == 0 {
        return Ok(());
    }

    match crate::disk::free_space(settings.directory()) {
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
    use std::time::SystemTime;

    use clipped_capture::{
        CaptureMethodSetting, CaptureTarget, CaptureTimestamp, FrameSize, FrameTexture,
        PixelFormat, SourceClock, TextureKind,
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
    use crate::config::{Configuration, GameKey};

    /// What a scripted backend hands back from one `acquire`.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        /// Nothing drew.
        Nothing,
        /// The window drew, so a frame is handed over.
        ///
        /// Only usable on a backend whose factory was given a texture by
        /// [`ScriptedFactory::painting`], because a frame is a texture and
        /// there is nothing honest to put there otherwise.
        Drew,
        /// The window is minimised, so nothing will draw until it is restored.
        Minimised,
        /// The target changed shape, which is what a window being dragged by
        /// its edge looks like from here.
        Resized(u32, u32),
        /// Capture failed, with the error this returns.
        ///
        /// The whole reason this fixture exists: no real backend can be made to
        /// have a driver reset on the fourth frame, and "what does a recording
        /// do when capture breaks half way through?" is not answerable without
        /// one that can (issues #285 and #97).
        Fails(fn() -> CaptureError),
    }

    /// A window that has opted out of being captured, which is the failure
    /// `CaptureFallback` answers by trying a different backend.
    fn opted_out() -> CaptureError {
        CaptureError::UnsupportedTarget {
            method: CaptureMethod::WindowsGraphicsCapture,
            target: clipped_capture::TargetKind::Window,
            reason: "the window has opted out of being captured",
        }
    }

    /// A driver reset, which is the failure answered by restarting the *same*
    /// backend.
    fn interrupted() -> CaptureError {
        CaptureError::Interrupted {
            method: CaptureMethod::WindowsGraphicsCapture,
            reason: "the display adapter was reset",
        }
    }

    /// A capture backend that replays a script.
    ///
    /// Without a texture it never produces a frame, which is all the tests of
    /// the wait for the first frame need: what they are about is what happens
    /// to the *format* on the way to one. Given one by its factory it hands over
    /// real frames on that texture, and the whole recording loop runs against it
    /// — an encoder opened on the texture's own device, a real file, and a
    /// script that can put a minimised stretch, or a driver reset, in the middle
    /// of a recording.
    #[derive(Debug)]
    struct ScriptedBackend {
        method: CaptureMethod,
        steps: VecDeque<Step>,
        format: FrameFormat,
        /// The texture every [`Step::Drew`] hands over, owned for the whole of
        /// this backend's life and never recycled — which is exactly the
        /// promise [`FrameTexture`] is documented to need.
        texture: Option<ID3D11Texture2D>,
        /// The clock every backend in one test stamps its frames from.
        ///
        /// Shared, and that is load-bearing: both Windows backends stamp from
        /// the machine's performance counter, so a replacement carries on from
        /// where the failed one stopped rather than starting again at zero. A
        /// fixture that restarted the clock would hand the recording's timeline
        /// frames it had already been given, which is a different bug from the
        /// one under test.
        clock: &'static AtomicU64,
        resizes: &'static AtomicU64,
        shut_downs: &'static AtomicU64,
    }

    /// How far apart the frames a scripted backend draws are, in nanoseconds.
    ///
    /// A tenth of a second: far enough inside the 60 frames a second the gate
    /// admits that every frame the script draws reaches the encoder, so a test
    /// counting frames is counting the script rather than the pacing rule.
    const SCRIPTED_FRAME_INTERVAL_NANOS: u64 = 100_000_000;

    impl CaptureBackend for ScriptedBackend {
        fn method(&self) -> CaptureMethod {
            self.method
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
                    let at = self
                        .clock
                        .fetch_add(SCRIPTED_FRAME_INTERVAL_NANOS, Ordering::Relaxed)
                        + SCRIPTED_FRAME_INTERVAL_NANOS;

                    // SAFETY: `handle` is an `ID3D11Texture2D` this backend owns
                    // for the whole of its life and never recycles, and the
                    // frame borrows the backend mutably, so nothing derived from
                    // it can outlive the texture.
                    let texture = unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, handle) };

                    Ok(Acquisition::Frame(CapturedFrame::new(
                        texture,
                        self.format,
                        CaptureTimestamp::from_source(SourceClock::PerformanceCounter, at),
                    )))
                }
                Some(Step::Resized(width, height)) => Ok(Acquisition::SizeChanged(
                    FrameSize::new(width, height).expect("a real size"),
                )),
                Some(Step::Minimised) => Ok(Acquisition::TargetMinimised),
                Some(Step::Fails(error)) => Err(error()),
                Some(Step::Nothing) | None => Ok(Acquisition::Timeout),
            }
        }

        fn resize(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError> {
            self.resizes.fetch_add(1, Ordering::Relaxed);
            // What a real backend does: rebuild the frame pool and report the
            // format it will now produce.
            self.format = FrameFormat::new(size, self.format.pixel_format());
            Ok(self.format)
        }

        fn shut_down(&mut self) {
            self.shut_downs.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A counter that outlives the test that made it.
    ///
    /// The backends a `CaptureFallback` creates outlive the borrow of the
    /// factory that made them — the fallback owns them — so a counter shared
    /// between the two has to be `'static`. Each call leaks its own, so no two
    /// tests can see each other's numbers. This is `crate::fallback`'s own
    /// arrangement in `clipped-capture`, for the same reason.
    fn counter() -> &'static AtomicU64 {
        Box::leak(Box::new(AtomicU64::new(0)))
    }

    /// A capture backend factory a test writes the script for.
    ///
    /// This is the seam issue #285's proof needs. Everything a recording does
    /// with capture goes through `CaptureFallback`, which builds backends from a
    /// candidate list, so a test that wants capture to break on cue substitutes
    /// the list — and then the *real* fallback policy, the real recovery, the
    /// real encoder and the real Matroska writer all run.
    ///
    /// Each factory paints its own texture on **its own Direct3D device**, and
    /// that is the point rather than tidiness: a replacement backend really does
    /// put a different graphics device behind the frames, which is the whole
    /// reason the encoder has to be reopened. A fixture that shared one device
    /// between the two would pass with the reopen deleted.
    #[derive(Debug)]
    struct ScriptedFactory {
        method: CaptureMethod,
        format: FrameFormat,
        /// Held so that the device outlives every texture and frame taken from
        /// it, and so that a test can assert the two factories are not the same
        /// device.
        device: Option<windows::Win32::Graphics::Direct3D11::ID3D11Device>,
        texture: Option<ID3D11Texture2D>,
        /// One script per `create`, in order. A factory asked more often than it
        /// was scripted hands over timeouts, which is what an idle source is.
        plans: std::sync::Mutex<VecDeque<Vec<Step>>>,
        clock: &'static AtomicU64,
        creations: &'static AtomicU64,
        resizes: &'static AtomicU64,
        shut_downs: &'static AtomicU64,
    }

    impl ScriptedFactory {
        fn new(method: CaptureMethod, clock: &'static AtomicU64) -> Self {
            Self {
                method,
                format: format_of(TEST_SIZE.0, TEST_SIZE.1),
                device: None,
                texture: None,
                plans: std::sync::Mutex::new(VecDeque::new()),
                clock,
                creations: counter(),
                resizes: counter(),
                shut_downs: counter(),
            }
        }

        /// The same factory, whose backends hand over a texture of `device`
        /// filled with `fill` in every byte.
        ///
        /// `0x40` is the flat grey the loop tests record; `0x00` is a capture
        /// that has silently stopped working, which is the only capture failure
        /// that returns no error and the one issue #97's detector exists for.
        fn painting(
            mut self,
            device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
            fill: u8,
        ) -> Self {
            self.texture = Some(test_texture(&device, TEST_SIZE.0, TEST_SIZE.1, fill));
            self.device = Some(device);
            self
        }

        /// The same factory, whose next `create` produces a backend running
        /// `steps`.
        fn planning(self, plans: impl IntoIterator<Item = Vec<Step>>) -> Self {
            *self.plans.lock().expect("no test panics holding this lock") =
                plans.into_iter().collect();
            self
        }

        fn creations(&self) -> u64 {
            self.creations.load(Ordering::Relaxed)
        }

        fn resizes(&self) -> u64 {
            self.resizes.load(Ordering::Relaxed)
        }

        fn shut_downs(&self) -> u64 {
            self.shut_downs.load(Ordering::Relaxed)
        }
    }

    impl clipped_capture::BackendDeclaration for ScriptedFactory {
        fn method(&self) -> CaptureMethod {
            self.method
        }

        fn capabilities(&self) -> clipped_capture::BackendCapabilities {
            clipped_capture::BackendCapabilities::new(true, true)
        }

        fn availability(
            &self,
            _target: &clipped_capture::TargetProperties,
        ) -> clipped_capture::Availability {
            clipped_capture::Availability::Available
        }
    }

    impl clipped_capture::CaptureBackendFactory for ScriptedFactory {
        fn create(&self) -> Result<Box<dyn CaptureBackend>, CaptureError> {
            self.creations.fetch_add(1, Ordering::Relaxed);
            let steps = self
                .plans
                .lock()
                .expect("no test panics holding this lock")
                .pop_front()
                .unwrap_or_default();

            Ok(Box::new(ScriptedBackend {
                method: self.method,
                steps: steps.into(),
                format: self.format,
                texture: self.texture.clone(),
                clock: self.clock,
                resizes: self.resizes,
                shut_downs: self.shut_downs,
            }))
        }
    }

    /// The window every scripted recording captures.
    fn scripted_target() -> CaptureTarget {
        CaptureTarget::new(
            clipped_capture::TargetHandle::from_raw(0x1234),
            clipped_capture::TargetProperties::new(
                clipped_capture::TargetKind::Window,
                FrameSize::new(TEST_SIZE.0, TEST_SIZE.1).expect("a real size"),
            ),
        )
    }

    /// Starts a capture over `candidates`, exactly as `record` does.
    ///
    /// Through `CaptureFallback::start` rather than by building a backend
    /// directly, because that is what the recording loop is handed and because
    /// the fallback's own bookkeeping — which method is current, which format
    /// the recording is committed to — has to be the real one for the recovery
    /// under test to mean anything.
    fn scripted_capture(
        candidates: &'static [&'static dyn clipped_capture::CaptureBackendFactory],
    ) -> (
        CaptureFallback<'static>,
        Box<dyn CaptureBackend>,
        FrameFormat,
    ) {
        CaptureFallback::start(
            candidates,
            &scripted_target(),
            &CaptureConfig::default(),
            CaptureMethodSetting::Automatic,
        )
        .expect("a scripted candidate list starts")
        .into_parts()
    }

    /// Gives one factory the lifetime a `CaptureFallback` needs of it.
    ///
    /// The fallback holds its candidates for the length of the recording and the
    /// backends it makes outlive the borrow of the factory that made them, so
    /// the factory has to outlive the test rather than the statement. The test
    /// keeps the same reference and asks it afterwards how many backends it was
    /// asked for. One leak per test of a handful of small values.
    fn leaked(factory: ScriptedFactory) -> &'static ScriptedFactory {
        Box::leak(Box::new(factory))
    }

    /// The candidate list `CaptureFallback::start` is handed, in preference
    /// order.
    fn candidates(
        factories: &[&'static ScriptedFactory],
    ) -> &'static [&'static dyn clipped_capture::CaptureBackendFactory] {
        let list: Vec<&'static dyn clipped_capture::CaptureBackendFactory> = factories
            .iter()
            .map(|factory| *factory as &'static dyn clipped_capture::CaptureBackendFactory)
            .collect();
        Box::leak(list.into_boxed_slice())
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

    /// The capture a test of the wait for the first frame runs against: one
    /// factory, no texture, so it never produces a frame.
    ///
    /// That is all these need — what they are about is what happens to the
    /// *format* on the way to a frame — and it keeps them runnable on a machine
    /// with no Direct3D at all, which is where they were before the factory
    /// existed.
    fn blind_capture(
        steps: Vec<Step>,
    ) -> (
        &'static ScriptedFactory,
        CaptureFallback<'static>,
        Box<dyn CaptureBackend>,
    ) {
        let factory = leaked(
            ScriptedFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
                .planning([steps]),
        );
        let (fallback, backend, _) = scripted_capture(candidates(&[factory]));
        (factory, fallback, backend)
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
        let (factory, mut fallback, mut backend) = blind_capture(vec![Step::Resized(1920, 1080)]);
        let mut format = format_of(TEST_SIZE.0, TEST_SIZE.1);
        let stop = StopAfter::polls(2);

        let error = first_frame_device(backend.as_mut(), &mut fallback, &stop, &mut format)
            .expect_err("a scripted backend with no texture never produces a frame");

        assert!(
            matches!(error, SessionError::NoFrames),
            "the wait should end with `no frames`, not {error}"
        );
        assert_eq!(factory.resizes(), 1, "the backend should have been rebuilt");
        assert_eq!(
            format,
            format_of(1920, 1080),
            "the recording must be configured from the format `resize` returned, not the one \
             `initialise` did"
        );
        assert_eq!(
            fallback.committed_format(),
            format_of(1920, 1080),
            "the resize must go through the fallback, or every later replacement is judged \
             against a size this recording no longer uses and is refused for producing exactly \
             the right frames (issue #285)"
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
        let (factory, mut fallback, mut backend) =
            blind_capture(vec![Step::Minimised, Step::Minimised, Step::Minimised]);
        let mut format = format_of(TEST_SIZE.0, TEST_SIZE.1);
        let stop = StopAfter::polls(3);

        let error = first_frame_device(backend.as_mut(), &mut fallback, &stop, &mut format)
            .expect_err("a minimised window produces no frame to take a device from");

        assert!(
            matches!(error, SessionError::NoFrames),
            "a minimised window is a window that produced no frames, not a failure of \
             capture: {error}"
        );
        assert_eq!(factory.resizes(), 0, "there was no size to adopt");
        assert_eq!(
            format,
            format_of(TEST_SIZE.0, TEST_SIZE.1),
            "the format must be the one capture reported, not one derived from the \
             minimised shape"
        );
    }

    #[test]
    fn a_target_that_never_changes_size_keeps_the_format_it_was_initialised_with() {
        let (factory, mut fallback, mut backend) = blind_capture(vec![Step::Nothing]);
        let mut format = format_of(TEST_SIZE.0, TEST_SIZE.1);
        let stop = StopAfter::polls(2);

        let _ = first_frame_device(backend.as_mut(), &mut fallback, &stop, &mut format);

        assert_eq!(factory.resizes(), 0);
        assert_eq!(format, format_of(TEST_SIZE.0, TEST_SIZE.1));
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
            output: Some(output),
            capture_method: CaptureMethod::WindowsGraphicsCapture,
            capture_setting: CaptureMethodSetting::Automatic,
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
            longest_source_silence: Duration::ZERO,
            packets_written: frames_encoded,
            timestamps_corrected: 0,
            duration: Duration::ZERO,
            end_reason: EndReason::Stopped,
            audio_tracks: Vec::new(),
            capture_changes: Vec::new(),
            audio_fallback: None,
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
        /// Removed when the test passed, kept when it did not.
        ///
        /// The pattern `crates/library/tests/support/mod.rs` uses, and it earns
        /// its keep here: what these tests produce is a Matroska file, and a
        /// failed assertion about one is not diagnosable without the file. The
        /// path is printed so it can be found.
        fn drop(&mut self) {
            let Some(directory) = self.0.parent() else {
                return;
            };
            if std::thread::panicking() {
                eprintln!(
                    "recording kept for diagnosis: {}",
                    RedactedPath::new(directory)
                );
                return;
            }
            if let Err(error) = std::fs::remove_dir_all(directory) {
                eprintln!(
                    "the temporary recording could not be removed: {} ({error})",
                    RedactedPath::new(directory)
                );
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
            muxing: Some(&muxing),
            replay: Some(&buffer),
        };
        let scripted = scripted_second();
        let mut encoder = ScriptedEncoder::new(scripted.clone());
        let mut counters = Counters::default();

        let moved = drain(&mut encoder, &sinks, &mut counters).expect("every packet is accepted");
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
            muxing: Some(&muxing),
            replay: None,
        };
        let mut encoder = ScriptedEncoder::new(scripted_second());
        let mut counters = Counters::default();

        let moved = drain(&mut encoder, &sinks, &mut counters).expect("every packet is accepted");
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
            muxing: Some(&muxing),
            replay: Some(&buffer),
        };
        let mut encoder = ScriptedEncoder::new(scripted_second());
        let mut counters = Counters::default();

        let error = drain(&mut encoder, &sinks, &mut counters).expect_err("the writer has stopped");

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

    /// A BGRA texture on `device`, with `fill` in every byte of it.
    ///
    /// The picture usually does not matter — what is under test is which
    /// acquisitions are counted as what, not what the encoder made of them — but
    /// the *format* does: `PixelFormat::Bgra8Unorm` is what the scripted format
    /// declares, and the software encoder checks the two agree before it copies
    /// anything (`crates/encoder/src/software/readback.rs`).
    ///
    /// It matters in exactly one place. `0x00` everywhere is a capture that has
    /// silently stopped working — every channel of every pixel zero — and it is
    /// what `clipped_capture::D3d11FrameSampler` reads back through the real
    /// Direct3D path in the black-frame test below. `0x40` is an ordinary grey.
    fn test_texture(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        width: u32,
        height: u32,
        fill: u8,
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
        let pixels = vec![fill; (width as usize) * (height as usize) * 4];
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
        recording_into(purpose, steps, &crate::RecordingOutputs::default())
    }

    /// The same settings for a capture that writes no continuous recording.
    ///
    /// The directory is the one a recording's file would have gone in, which is
    /// where its clips would go — so a test can assert what a run of this mode
    /// leaves behind by listing exactly the folder a recording would have
    /// filled.
    fn buffered_loop_settings(recording: &TemporaryRecording) -> RecordingSettings {
        RecordingSettings::buffered(
            crate::settings::CaptureTargetSettings::window(0x1234, TEST_SIZE.0, TEST_SIZE.1),
            directory_of(recording).to_path_buf(),
        )
        .with_minimum_free_space(0)
        .with_codec(crate::settings::CodecPreference::Fixed(Codec::H264))
        .with_encoder(crate::settings::EncoderPreference::Fixed(
            EncoderKind::Software,
        ))
    }

    /// The folder a temporary recording lives in.
    fn directory_of(recording: &TemporaryRecording) -> &std::path::Path {
        recording
            .path()
            .parent()
            .expect("the temporary recording is inside a directory")
    }

    /// Everything in `directory`, by file name, sorted.
    fn contents_of(directory: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .expect("the directory the capture ran in can be listed")
            .map(|entry| {
                entry
                    .expect("an entry can be read")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// Runs the whole loop with **no continuous recording**, and answers the
    /// report and the directory it ran in.
    ///
    /// The directory is handed back rather than listed here because what each
    /// test wants to say about it differs, and because the [`TemporaryRecording`]
    /// has to outlive the assertion — it removes the directory when it is
    /// dropped.
    fn buffered_capture_into(
        purpose: &str,
        steps: impl IntoIterator<Item = Step>,
        outputs: &crate::RecordingOutputs<'_>,
    ) -> (RecordingReport, TemporaryRecording) {
        let device =
            test_device().expect("no Direct3D 11 device could be created, on hardware or on WARP");
        let list = candidates(&[leaked(
            ScriptedFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
                .painting(device, 0x40)
                .planning([steps.into_iter().collect()]),
        )]);
        let (fallback, backend, format) = scripted_capture(list);

        let recording = TemporaryRecording::new(purpose);
        let settings = buffered_loop_settings(&recording);
        let stop = StopAfter::polls(40);

        let report = record_frames(&settings, &stop, fallback, backend, format, outputs)
            .expect("a capture with frames in it is a capture");

        (report, recording)
    }

    #[test]
    fn a_capture_with_no_recording_writes_no_file_and_still_fills_its_replay_buffer() {
        // Issue #423's whole claim, run through the real loop: a WARP Direct3D
        // device, the software H.264 encoder, real captured frames, real encoded
        // packets — and **no container**. What it must produce is a buffer full
        // of the capture's own video and an empty directory, and the two
        // together are the mode. Either alone is satisfiable by a bug: a loop
        // that still opened a writer would fill the buffer and leave a file, and
        // one that never reached the encoder would leave nothing and buffer
        // nothing.
        let replay = crate::replay::ReplayRecording::new(Duration::from_secs(30))
            .expect("thirty seconds is in range");

        let (report, recording) = buffered_capture_into(
            "buffered-capture",
            [Step::Drew, Step::Drew, Step::Drew, Step::Drew, Step::Drew],
            &crate::RecordingOutputs::default().with_replay(&replay),
        );

        assert_eq!(
            report.output(),
            None,
            "a capture that wrote no file must not name one"
        );
        assert!(
            report.frames_encoded() > 0,
            "the capture encoded nothing, so this says nothing about the rest"
        );
        assert_eq!(
            contents_of(directory_of(&recording)),
            Vec::<String>::new(),
            "a capture asked for no continuous recording left something in the output \
             directory anyway"
        );

        let stats = replay
            .stats()
            .expect("the capture never started the replay buffer it was given");
        assert_eq!(
            stats.packets_buffered(),
            report.frames_encoded(),
            "the buffer was not fed the capture's own packets, so there would be nothing to \
             save and nothing else would have noticed: {stats:?}"
        );
        assert!(
            stats
                .covered()
                .is_some_and(|covered| !covered.length().is_zero()),
            "the buffer holds no stretch of the capture's timeline: {stats:?}"
        );

        // The muxer is what counts these for a recording, and there is no muxer.
        // Without `Produced` the report would say a sitting that encoded half a
        // second of video produced no packets and lasted no time, which is what
        // the command prints when it ends.
        assert_eq!(
            report.packets_written(),
            stats.packets_buffered(),
            "the report did not count the packets the capture produced"
        );
        assert!(
            !report.duration().is_zero(),
            "the report gave a capture with video in it no length at all"
        );
        assert_eq!(
            report.timestamps_corrected(),
            0,
            "nothing corrected a timestamp, because nothing wrote a container"
        );
    }

    #[test]
    fn a_capture_with_no_recording_still_tells_its_replay_buffer_when_its_source_goes_quiet() {
        // ADR 0017 named this mode, by number, as the way to lose the silence
        // report: "a Manual/Replay-mode capture with no continuous file (#423)
        // that never calls `note_source_silence` reintroduces the
        // during-the-stall half of this defect and nothing in `clipped-replay`
        // will notice". It shares the loop today and therefore cannot — but "it
        // shares the loop today" is exactly the sort of fact that stops being
        // true in a later change, and the failure is silent: a save during the
        // stall is answered with the video from before it, marked complete.
        //
        // So it is asserted for this mode too, against a running capture rather
        // than against the shape of the code.
        let replay = crate::replay::ReplayRecording::new(Duration::from_secs(30))
            .expect("thirty seconds is in range");

        let (_report, _recording) = buffered_capture_into(
            "buffered-silence",
            [
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Minimised,
                Step::Minimised,
                Step::Nothing,
                Step::Nothing,
            ],
            &crate::RecordingOutputs::default().with_replay(&replay),
        );

        let stats = replay
            .stats()
            .expect("the capture never started the replay buffer it was given");
        assert!(
            stats.source_silence() > Duration::ZERO,
            "a capture with no recording waited out a minimised window and a stretch of \
             nothing without telling its buffer, so a save during either would be answered \
             with the video from before it: {stats:?}"
        );
    }

    #[test]
    fn a_capture_with_no_recording_and_no_video_fails_rather_than_reporting_a_sitting() {
        // The same diagnosis a recording gets, for the same reason: a window
        // that is not drawing produced nothing, and a buffer nothing reached is
        // a hotkey that would never produce a clip. `conclude` reaches it
        // through a different arm — there is no file to remove — and an arm that
        // returned the report instead would hand somebody a successful sitting
        // that could not produce a single clip.
        let device =
            test_device().expect("no Direct3D 11 device could be created, on hardware or on WARP");
        let list = candidates(&[leaked(
            ScriptedFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
                .painting(device, 0x40)
                .planning([vec![Step::Minimised]]),
        )]);
        let (fallback, backend, format) = scripted_capture(list);
        let recording = TemporaryRecording::new("buffered-no-video");
        let settings = buffered_loop_settings(&recording);

        let error = record_frames(
            &settings,
            &StopAfter::polls(4),
            fallback,
            backend,
            format,
            &crate::RecordingOutputs::default(),
        )
        .expect_err("no frame was ever drawn");

        assert!(matches!(error, SessionError::NoFrames), "{error}");
        assert_eq!(contents_of(directory_of(&recording)), Vec::<String>::new());
    }

    /// The same, writing into `outputs` as well as into the file.
    fn recording_into(
        purpose: &str,
        steps: impl IntoIterator<Item = Step>,
        outputs: &crate::RecordingOutputs<'_>,
    ) -> RecordingReport {
        // Not a skip. WARP ships with Windows, so a machine that cannot create
        // a device here is a broken one and this is a failure worth seeing —
        // a test that quietly does nothing is worse than no test (AGENTS.md
        // section 54).
        let device =
            test_device().expect("no Direct3D 11 device could be created, on hardware or on WARP");
        let list = candidates(&[leaked(
            ScriptedFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
                .painting(device, 0x40)
                .planning([steps.into_iter().collect()]),
        )]);
        let (fallback, backend, format) = scripted_capture(list);

        let recording = TemporaryRecording::new(purpose);
        let settings = loop_settings(&recording);
        // Far beyond the script, so the loop ends because it was asked to and
        // not because the script ran out: the steps after the last one are
        // ordinary timeouts, which is what an idle source looks like.
        let stop = StopAfter::polls(40);

        record_frames(&settings, &stop, fallback, backend, format, outputs)
            .expect("a recording with frames in it is a recording")
    }

    #[test]
    fn a_recording_given_a_replay_handle_starts_that_handle_and_fills_it_from_its_own_encoder() {
        // The one line that turns the replay feature on:
        // `record_frames` hands `outputs.replay` to `crate::replay::start_buffer`
        // once its encoder is open. Passing `None` there instead compiles, keeps
        // every other test in this crate green and is invisible in a running
        // recorder — the recording is written exactly as before, no buffer is
        // ever begun, and the first sign of it is a hotkey that answers "this
        // recording is not keeping a replay buffer" for every recording there
        // will ever be. Everything downstream of the hand-off is covered
        // elsewhere (`crate::replay`, `drain` above,
        // `apps/recorder/tests/replay_clip.rs`), and all of it stays green
        // without it, because none of it is reached from the recording loop.
        //
        // So this is the only test that runs the real loop with a handle in the
        // outputs and asks the handle what happened to it.
        let replay = crate::replay::ReplayRecording::new(Duration::from_secs(30))
            .expect("thirty seconds is in range");

        let report = recording_into(
            "replay-handed-off",
            [Step::Drew, Step::Drew, Step::Drew, Step::Drew, Step::Drew],
            &crate::RecordingOutputs::default().with_replay(&replay),
        );

        let stats = replay
            .stats()
            .expect("the recording never started the replay buffer it was given");

        assert!(
            report.frames_encoded() > 0,
            "the recording encoded nothing, so this says nothing about the buffer"
        );
        assert_eq!(
            stats.packets_buffered(),
            report.frames_encoded(),
            "the buffer was started but not fed the recording's own packets: {stats:?}"
        );
        assert!(
            stats
                .covered()
                .is_some_and(|covered| !covered.length().is_zero()),
            "the buffer holds no stretch of the recording's timeline: {stats:?}"
        );
        assert_eq!(
            stats.packets_discarded_before_first_keyframe(),
            0,
            "the buffer was attached after the recording's first keyframe: {stats:?}"
        );
    }

    #[test]
    fn a_recording_whose_window_stops_drawing_tells_its_replay_buffer_how_long_for() {
        // The other hand-off that is invisible when it goes missing, and the one
        // issue #574 is about. `clipped-replay` reads no clock and its media
        // time only advances when a packet arrives, so a buffer whose source
        // went quiet cannot tell an hour from an instant: "save the last thirty
        // seconds" is answered with the thirty before the window stopped
        // drawing, marked complete, however long ago that was.
        //
        // Deleting the `note_source_silence` calls from the loop compiles, keeps
        // every test in `clipped-replay` green — the buffer's own tests report
        // the silence themselves — and puts the defect straight back. So the
        // hand-off is asserted where it is made.
        //
        // A minimised window and a plain timeout are the same silence with
        // different names, and both are scripted here for that reason. The
        // assertion is that *something* was reported rather than a particular
        // number, because the number is however long this machine took to run
        // the loop.
        let replay = crate::replay::ReplayRecording::new(Duration::from_secs(30))
            .expect("thirty seconds is in range");

        recording_into(
            "replay-told-about-silence",
            [
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Minimised,
                Step::Minimised,
                Step::Nothing,
                Step::Nothing,
            ],
            &crate::RecordingOutputs::default().with_replay(&replay),
        );

        let stats = replay
            .stats()
            .expect("the recording never started the replay buffer it was given");

        assert!(
            stats.source_silence() > Duration::ZERO,
            "the loop waited out a minimised window and a stretch of nothing without telling the \
             buffer, so a save during either would be answered with the video from before it: \
             {stats:?}"
        );
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
    fn a_source_that_goes_quiet_mid_recording_has_the_length_of_the_stretch_on_its_report() {
        // Issue #461. This arm of the loop was empty, so a recording whose
        // source produced nothing for an hour said nothing about the hour: the
        // file's timestamps simply jump, which is the right thing to write
        // (`docs/av-sync.md`) and the wrong thing to keep quiet about.
        //
        // Thirty steps of silence is 3.0 s at `ACQUIRE_TIMEOUT`, which is
        // deliberately longer than the trailing stretch `StopAfter` leaves after
        // the script runs out — so the number asserted is the *scripted* stretch
        // and not an artefact of when the test stopped.
        const QUIET_ACQUISITIONS: u32 = 30;

        let mut steps = vec![Step::Drew];
        steps.extend(std::iter::repeat_n(
            Step::Nothing,
            QUIET_ACQUISITIONS as usize,
        ));
        steps.push(Step::Drew);

        let report = recording_of("source-went-quiet", steps);

        assert_eq!(
            report.longest_source_silence(),
            ACQUIRE_TIMEOUT * QUIET_ACQUISITIONS,
            "the stretch in which capture produced nothing reached nothing the user can see"
        );
        assert_eq!(
            report.times_target_minimised(),
            0,
            "a source with nothing new is not a minimised target, and the two must not be \
             reported as each other"
        );
        assert_eq!(
            report.end_reason(),
            EndReason::Stopped,
            "a quiet source must not end the recording; a still screen is a thing people record"
        );
    }

    #[test]
    fn quiet_is_measured_a_stretch_at_a_time_and_the_longest_one_is_kept() {
        // The policy, away from the loop: a frame ends a stretch rather than
        // pausing it, and what survives to the report is the longest single
        // stretch rather than the total. Four minutes lost in one go is a hole
        // somebody will notice; four minutes lost a tenth of a second at a time
        // is a screen that was not changing much, and reporting them as the same
        // number would make the report useless for both.
        let mut counters = Counters::default();

        for _ in 0..5 {
            counters.note_idle(Duration::from_secs(1));
        }
        assert_eq!(counters.longest_silence, Duration::from_secs(5));

        counters.note_drawing();
        assert_eq!(
            counters.silence,
            Duration::ZERO,
            "a frame ends the stretch rather than pausing it"
        );
        assert_eq!(
            counters.longest_silence,
            Duration::from_secs(5),
            "the stretch that ended is still the longest one this recording had"
        );

        for _ in 0..3 {
            counters.note_idle(Duration::from_secs(1));
        }
        assert_eq!(
            counters.longest_silence,
            Duration::from_secs(5),
            "a shorter second stretch must not overwrite the longest, and the two must not be \
             added together either"
        );

        for _ in 0..4 {
            counters.note_idle(Duration::from_secs(1));
        }
        assert_eq!(
            counters.longest_silence,
            Duration::from_secs(7),
            "the second stretch has now outrun the first and is the one worth reporting"
        );
    }

    #[test]
    fn a_minimised_window_is_not_also_counted_as_a_quiet_source() {
        // Both mean "no frame arrived", and the report says each of them in its
        // own words: `times_target_minimised` for a window that cannot draw
        // until somebody restores it, `longest_source_silence` for a source with
        // nothing new. Feeding one into the other would say the same thing twice
        // in two vocabularies and leave a reader unable to tell which happened.
        let mut counters = Counters::default();

        for _ in 0..100 {
            counters.note_minimised();
        }

        assert_eq!(
            counters.minimised_stretches, 1,
            "one stretch, not a hundred"
        );
        assert_eq!(
            counters.longest_silence,
            Duration::ZERO,
            "a minimised window has its own count and must not appear as a quiet source too"
        );
    }

    #[test]
    fn a_source_that_is_quiet_for_longer_than_the_threshold_is_only_said_once() {
        // AGENTS.md section 35: the sessions this happens to are the ones nobody
        // is watching a console for, so an hour of quiet has to be one line
        // rather than thirty-six thousand. `silence_reported` is what does it,
        // and it has to be cleared when frames come back or the *second* stretch
        // of a long recording is never mentioned.
        let mut counters = Counters::default();

        while counters.silence < SILENT_SOURCE_THRESHOLD {
            assert!(
                !counters.silence_reported,
                "nothing is worth saying before the threshold: a still menu does this constantly"
            );
            counters.note_idle(ACQUIRE_TIMEOUT);
        }
        assert!(
            counters.silence_reported,
            "reaching the threshold has to be said out loud; this is a recording that is \
             accumulating nothing"
        );

        counters.note_drawing();
        assert!(
            !counters.silence_reported,
            "the next quiet stretch must be reportable, or a recording says this once and then \
             never again however long it goes dark for"
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
    fn a_window_resized_mid_recording_keeps_everything_captured_before_the_resize() {
        // Issue #184's half of the bargain, and the half that must never be
        // given up. A session follows a resize by finishing this file and
        // starting the next one (ADR 0012), and that is only an honest
        // answer if the file it finishes holds the footage: everything up to the
        // resize, encoded, written and finalised, with a track that declares the
        // size the pictures in it actually are (AGENTS.md sections 22 and 56).
        //
        // Three things would each break it silently. Dropping the frames the
        // loop had already captured costs the user the run-up to whatever made
        // them resize the window. Carrying on past the size change writes
        // pictures of one size into a track that declares another, which no
        // player and nothing downstream can detect. Ending without the flush and
        // the trailer leaves a file that is not seekable and does not say how
        // long it is.
        //
        // The first frame of the script is the one the recording releases rather
        // than encodes: it exists to say which device the textures are on.
        let report = recording_of(
            "resized-mid-recording",
            [
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Resized(1920, 1080),
                // Never reached, and that is the assertion below: the loop ends
                // at the size change rather than encoding frames of a shape the
                // track does not declare.
                Step::Drew,
                Step::Drew,
            ],
        );

        assert_eq!(
            report.end_reason(),
            EndReason::TargetResized,
            "the recording must say why it ended, because the session reads that to decide \
             whether to follow it with another file"
        );
        assert_eq!(
            report.frames_captured(),
            3,
            "every frame captured before the resize belongs in the file, and none after it"
        );
        assert!(
            report.frames_encoded() > 0,
            "the frames before the resize were captured and never encoded"
        );
        assert_eq!(
            report.packets_written(),
            report.frames_encoded(),
            "the encoder's output did not reach the file before it was closed"
        );
        assert!(
            !report.duration().is_zero(),
            "the file was closed without a span in it, so nothing was finalised"
        );
        assert_eq!(
            report.size(),
            TEST_SIZE,
            "the track must declare the size of the pictures that are in it, not the size the \
             window became"
        );
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

    #[test]
    fn a_reopened_encoder_is_held_to_what_the_file_already_declares() {
        // The judgement that keeps a recovery from corrupting a file. Matroska
        // writes a track's codec and its out-of-band parameter sets once, in the
        // header, so an encoder reopened against a replacement backend's device
        // has to produce the same ones or its packets decode against a codec
        // header describing something else — in a file that looks finished.
        //
        // Tested here rather than through a recovery because forcing a real
        // second encoder to disagree needs a machine whose preferred encoder
        // stops existing between two calls, which is exactly the driver reset
        // this guards against and exactly what cannot be arranged.
        let declared = TrackDeclaration {
            codec: Codec::H264,
            parameter_sets: vec![0x00, 0x00, 0x01, 0x67, 0x42],
        };

        assert_eq!(
            declared.refuses(
                EncoderKind::Software,
                Codec::H264,
                &[0x00, 0x00, 0x01, 0x67, 0x42]
            ),
            None,
            "the ordinary case: the same encoder, at the same size, for the same codec"
        );

        let different_sets = declared
            .refuses(EncoderKind::Nvenc, Codec::H264, &[0x00, 0x00, 0x01, 0x67])
            .expect("a stream the track does not describe cannot go into it");
        assert!(
            different_sets.contains("NVENC") && different_sets.contains("H.264"),
            "the refusal must name the encoder and the codec so it can be acted on: \
             {different_sets}"
        );

        let different_codec = declared
            .refuses(
                EncoderKind::Amf,
                Codec::Hevc,
                &[0x00, 0x00, 0x01, 0x67, 0x42],
            )
            .expect("a track declaring H.264 cannot contain HEVC");
        assert!(
            different_codec.contains("HEVC") && different_codec.contains("H.264"),
            "the refusal must name both codecs, or it says nothing about which is wrong: \
             {different_codec}"
        );
    }

    // ---- capture that breaks mid-recording ---------------------------------
    //
    // Issues #285 and #97, and the one thing a unit test of `CaptureFallback`
    // cannot say: whether the recording loop consults it. Every test below
    // breaks capture *during* a recording and asks what the file and the report
    // turned out to be — the real fallback policy, a real replacement backend on
    // its own Direct3D device, the real encoder reopened against that device,
    // and a real Matroska file at the end of it.
    //
    // None of them needs a window, a compositor or a display, which is what lets
    // CI cover a path no hardware will reproduce on request: WARP is a Direct3D
    // device and a `ScriptedFactory` is a backend that fails when it is told to.

    /// Everything a fallback test wants to know afterwards.
    struct Recovered {
        report: RecordingReport,
        /// Kept so the file outlives the assertions; dropping it removes the
        /// directory, and keeps it when a test failed.
        recording: TemporaryRecording,
        preferred: &'static ScriptedFactory,
        replacement: &'static ScriptedFactory,
        /// The reading a connection thread would ask for, as the recording left
        /// it. Leaked for the same reason the factories are: `RecordingOutputs`
        /// borrows it for the length of `record_frames`.
        accounting: &'static crate::CaptureAccounting,
    }

    /// Records a script that breaks, with a second backend behind it.
    ///
    /// The two factories paint their textures on **two different Direct3D
    /// devices**, which is what makes this a test of the encoder being reopened
    /// rather than of the backend being swapped: an encoder session can only
    /// bind textures belonging to the device it was opened against, so a
    /// recording that carried on submitting frames to the first session would
    /// fail at the driver.
    ///
    /// Each factory is given one script per backend it will be asked to make,
    /// in order, because a backend that is *restarted* is a second backend from
    /// the same factory and needs its own.
    fn recording_that_recovers(
        purpose: &str,
        preferred_fill: u8,
        preferred: Vec<Vec<Step>>,
        replacement: Vec<Vec<Step>>,
        polls: u64,
    ) -> Result<Recovered, (SessionError, TemporaryRecording)> {
        let first = test_device().expect("no Direct3D 11 device could be created, on WARP either");
        let second = test_device().expect("no Direct3D 11 device could be created, on WARP either");
        assert_ne!(
            first.as_raw(),
            second.as_raw(),
            "the two backends must be on different graphics devices, or this says nothing about \
             the encoder being reopened"
        );

        // One clock between them, because both Windows backends stamp frames
        // from the machine's performance counter: a replacement carries on from
        // where the failed one stopped rather than handing the recording's
        // timeline moments it has already been given.
        let clock = counter();
        let preferred = leaked(
            ScriptedFactory::new(CaptureMethod::WindowsGraphicsCapture, clock)
                .painting(first, preferred_fill)
                .planning(preferred),
        );
        let replacement = leaked(
            ScriptedFactory::new(CaptureMethod::DesktopDuplication, clock)
                .painting(second, 0x40)
                .planning(replacement),
        );

        let (fallback, backend, format) = scripted_capture(candidates(&[preferred, replacement]));
        assert_eq!(
            fallback.current_method(),
            CaptureMethod::WindowsGraphicsCapture,
            "selection prefers Windows Graphics Capture, so that is what has to fail"
        );

        let recording = TemporaryRecording::new(purpose);
        let settings = loop_settings(&recording);
        let accounting: &'static crate::CaptureAccounting =
            Box::leak(Box::new(crate::CaptureAccounting::new()));
        // The publish `record` does before it hands over to `record_frames`
        // (`recording.rs`, where the backend first opens). Done here because
        // this harness calls `record_frames` directly, and without it removing
        // the republish would leave *nothing* published rather than something
        // stale — so the test would fail for the wrong reason and prove less
        // than it claims.
        accounting.publish(fallback.status().into());
        let outputs = crate::RecordingOutputs {
            capture: Some(accounting),
            ..crate::RecordingOutputs::default()
        };
        match record_frames(
            &settings,
            &StopAfter::polls(polls),
            fallback,
            backend,
            format,
            &outputs,
        ) {
            Ok(report) => Ok(Recovered {
                report,
                recording,
                preferred,
                replacement,
                accounting,
            }),
            Err(error) => Err((error, recording)),
        }
    }

    /// Records with two *healthy* backends, having been told which to try first.
    ///
    /// The counterpart to [`recording_that_recovers`]: nothing here fails, so
    /// what the recording ends on is entirely a question of which candidate was
    /// asked. `remembered` goes in the same two places `record` puts it — onto
    /// the `RecordingSettings` and from there into
    /// `CaptureFallback::start_preferring` — so the test exercises the wiring a
    /// real recording uses rather than calling the capture layer directly.
    fn recording_preferring(
        purpose: &str,
        remembered: Option<CaptureMethod>,
        polls: u64,
    ) -> (
        RecordingReport,
        TemporaryRecording,
        &'static ScriptedFactory,
        &'static ScriptedFactory,
    ) {
        let first = test_device().expect("no Direct3D 11 device could be created, on WARP either");
        let second = test_device().expect("no Direct3D 11 device could be created, on WARP either");
        let clock = counter();
        let wgc = leaked(
            ScriptedFactory::new(CaptureMethod::WindowsGraphicsCapture, clock)
                .painting(first, 0x80)
                .planning([vec![Step::Drew; 40]]),
        );
        let duplication = leaked(
            ScriptedFactory::new(CaptureMethod::DesktopDuplication, clock)
                .painting(second, 0x40)
                .planning([vec![Step::Drew; 40]]),
        );

        let recording = TemporaryRecording::new(purpose);
        let settings = loop_settings(&recording).with_remembered_capture_method(remembered);

        // The two lines `record` runs, with a scripted candidate list in place
        // of `registered_backends()`. Reading both values off the settings is
        // the point: a test that passed `remembered` to the capture layer by
        // hand would still pass with the plumbing through `RecordingSettings`
        // taken out.
        let (fallback, backend, format) = CaptureFallback::start_preferring(
            candidates(&[wgc, duplication]),
            &scripted_target(),
            &CaptureConfig::default(),
            settings.capture_method(),
            settings.remembered_capture_method(),
        )
        .expect("a scripted candidate list starts")
        .into_parts();

        let report = record_frames(
            &settings,
            &StopAfter::polls(polls),
            fallback,
            backend,
            format,
            &crate::RecordingOutputs::default(),
        )
        .expect("two healthy scripted backends record");
        (report, recording, wgc, duplication)
    }

    /// Asserts that `recording` is a Matroska file with a decodable H.264 track
    /// of at least `frames` pictures in it.
    ///
    /// Decoded rather than counted: the whole question a fallback raises about a
    /// file is whether the pictures *after* the change can be decoded against
    /// the codec header written *before* it, and only a decoder can answer that
    /// (AGENTS.md section 22).
    fn decodes_at_least(recording: &TemporaryRecording, frames: u64) {
        use clipped_media_validation::{require_media_tools, Media, VideoStream};

        if require_media_tools().is_none() {
            return;
        }
        Media::open(recording.path())
            .expect("a finished recording opens")
            .validate()
            .video_stream_count(1)
            .video(
                VideoStream::codec("h264")
                    .resolution(TEST_SIZE.0, TEST_SIZE.1)
                    .decoded_frames_at_least(frames),
            )
            .assert_valid();
    }

    #[test]
    fn the_reading_a_window_asks_for_follows_a_backend_replaced_mid_recording() {
        // The seam between #302 and #285, which neither closed on its own.
        //
        // #302 built `CaptureAccounting` — an owned reading of what a recording
        // is capturing with, publishable from the capture thread and readable
        // from a connection thread — and its own documentation says it is
        // "shaped for" being written again when a fallback happens. #285 built
        // the fallback that happens. Between them the account was published
        // once, where the backend first opened, and never again: two correct
        // halves that did not meet.
        //
        // The consequence was invisible in both pull requests and obvious to a
        // user: a backend replaced forty minutes into a recording, and a
        // Diagnostics screen still reporting whatever was true in the first
        // millisecond.
        //
        // This asserts the *live* reading rather than the report. The report is
        // read when the file closes; the reading is what a window can ask for
        // while the recording is still running, which is the whole point of it.
        let recovered = recording_that_recovers(
            "capture-account-follows",
            0x40,
            vec![vec![
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Fails(opted_out),
            ]],
            vec![vec![Step::Drew; 20]],
            60,
        )
        .unwrap_or_else(|(error, _recording)| {
            panic!("the recording should have survived its backend failing: {error}")
        });

        let account = recovered
            .accounting
            .account()
            .expect("a recording that opened a backend has published a reading");

        assert_eq!(
            account.current(),
            CaptureMethod::DesktopDuplication,
            "the live reading still names the backend the recording started with, so a window \
             asking mid-recording is told about a capture that is no longer running (issues #302 \
             and #285)",
        );
        // The pair is what makes the reading worth having: a window can say
        // "started on Windows Graphics Capture, now on Desktop Duplication"
        // rather than only naming one of them.
        assert_eq!(
            account.started_with(),
            CaptureMethod::WindowsGraphicsCapture,
            "republishing must not overwrite which backend the recording began with",
        );

        let changes = account.changes();
        assert!(
            changes.iter().any(|change| {
                change.from() == CaptureMethod::WindowsGraphicsCapture
                    && change.to() == CaptureMethod::DesktopDuplication
            }),
            "the replacement is missing from the live reading's history, so a window can see \
             which backend is running but not that anything went wrong: {changes:?}",
        );
    }
    #[test]
    fn a_backend_that_fails_mid_recording_is_replaced_and_the_recording_carries_on() {
        // Issue #285's first and second acceptance criteria, against a forced
        // failure rather than a unit test of the branch.
        //
        // Before this, `Err(error)` in the acquisition arm ended the recording:
        // a window that turned out to be uncapturable finished the file where it
        // stood, on a machine whose other backend would have gone on recording
        // for the rest of the session. The `CaptureFallback` that knows how to
        // replace it was built, asked once which method it had chosen, and
        // dropped.
        let recovered = recording_that_recovers(
            "capture-fallback",
            0x40,
            vec![vec![
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Fails(opted_out),
            ]],
            vec![vec![Step::Drew; 20]],
            60,
        )
        .unwrap_or_else(|(error, _recording)| {
            panic!("the recording should have survived its backend failing: {error}")
        });
        let report = &recovered.report;

        assert_eq!(
            report.capture_method(),
            CaptureMethod::DesktopDuplication,
            "the report must name the method actually in use at the end, not the one selection \
             chose at the start (issue #285)"
        );
        let changes = report.capture_changes();
        assert_eq!(changes.len(), 1, "one replacement happened: {changes:?}");
        assert_eq!(changes[0].from(), CaptureMethod::WindowsGraphicsCapture);
        assert_eq!(changes[0].to(), CaptureMethod::DesktopDuplication);
        assert_eq!(
            changes[0].trigger(),
            clipped_capture::FallbackTrigger::CaptureFailed
        );
        assert!(
            changes[0].reason().contains("opted out"),
            "the change must carry the failure in the words the failure used: {}",
            changes[0].reason()
        );

        assert_eq!(
            recovered.preferred.creations(),
            1,
            "a method that has failed is never asked again in the same recording"
        );
        assert_eq!(recovered.replacement.creations(), 1);
        assert!(
            recovered.preferred.shut_downs() >= 1,
            "the failed backend must be shut down before the platform is asked for another; \
             DXGI gives a process one duplication per display"
        );

        // Both halves are in the file. Three frames came from the preferred
        // backend and one of those opened the encoder, so anything past two is
        // video the replacement produced — and the count would be two exactly if
        // the recording had ended at the failure.
        assert!(
            report.frames_encoded() > 2,
            "the recording stopped at the failure instead of carrying on: {} frames",
            report.frames_encoded()
        );
        assert_eq!(
            report.packets_written(),
            report.frames_encoded(),
            "the replacement's packets did not reach the file"
        );
        assert!(matches!(report.end_reason(), EndReason::Stopped));

        // And it is one playable file rather than two halves that happen to be
        // adjacent: the pictures after the change decode against the codec
        // header written before it.
        decodes_at_least(&recovered.recording, report.frames_encoded());
    }

    #[test]
    fn a_second_recording_of_the_same_game_starts_on_the_method_the_first_one_fell_back_to() {
        // Issue #286, end to end and in the order it happens: a recording falls
        // back, what it ended on is remembered against the game, and the next
        // recording of that game *starts* there.
        //
        // The second half is the one worth stating carefully. Windows Graphics
        // Capture is healthy in the second recording and is the method selection
        // prefers, so nothing but the memory can keep it from being chosen —
        // and the recording is driven through `record_frames` with settings
        // built the way `record` builds them, rather than by handing a method to
        // the capture layer.
        let game = GameKey::parse("counter-strike-2").expect("a game key");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725);
        let mut configuration = Configuration::defaults();

        let first = recording_that_recovers(
            "remembered-capture-fell-back",
            0x40,
            vec![vec![
                Step::Drew,
                Step::Drew,
                Step::Drew,
                Step::Fails(opted_out),
            ]],
            vec![vec![Step::Drew; 20]],
            60,
        )
        .unwrap_or_else(|(error, _recording)| {
            panic!("the first recording should have survived its backend failing: {error}")
        });
        assert_eq!(
            first.report.capture_method(),
            CaptureMethod::DesktopDuplication,
            "the first recording has to end somewhere other than where it started, or the second \
             one proves nothing"
        );

        assert!(
            configuration.remember_capture_method(
                &game,
                first.report.capture_setting(),
                first.report.capture_method(),
                now,
            ),
            "a game nothing was known about, and a recording that fell back, is something to \
             learn from"
        );

        let (report, _recording, wgc, duplication) = recording_preferring(
            "remembered-capture-starts-there",
            configuration.remembered_capture_method(&game, now),
            60,
        );

        assert_eq!(
            report.capture_method(),
            CaptureMethod::DesktopDuplication,
            "the second recording of this game started on Windows Graphics Capture, which failed \
             last time, rather than on Desktop Duplication, which worked"
        );
        assert_eq!(
            report.capture_changes(),
            &[],
            "the second recording fell back as well, which is the second or two issue #286 \
             exists to stop losing"
        );
        assert_eq!(
            wgc.creations(),
            0,
            "the method that failed last time was opened again, so nothing was saved by \
             remembering"
        );
        assert_eq!(
            duplication.creations(),
            1,
            "the remembered method should have been the one and only backend opened"
        );
    }

    #[test]
    fn a_remembered_method_this_machine_no_longer_offers_falls_back_rather_than_failing() {
        // Issue #286's second acceptance criterion. The memory names a method
        // no candidate is registered for at all — a build that dropped a
        // backend, a machine that lost Desktop Duplication — and the recording
        // still happens, on whatever the published order can offer.
        let first = test_device().expect("no Direct3D 11 device could be created, on WARP either");
        let wgc = leaked(
            ScriptedFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
                .painting(first, 0x80)
                .planning([vec![Step::Drew; 40]]),
        );

        let recording = TemporaryRecording::new("remembered-capture-gone");
        let settings = loop_settings(&recording)
            .with_remembered_capture_method(Some(CaptureMethod::DesktopDuplication));
        let (fallback, backend, format) = CaptureFallback::start_preferring(
            candidates(&[wgc]),
            &scripted_target(),
            &CaptureConfig::default(),
            settings.capture_method(),
            settings.remembered_capture_method(),
        )
        .expect("a remembered method that is gone must not stop a capture starting")
        .into_parts();

        let report = record_frames(
            &settings,
            &StopAfter::polls(60),
            fallback,
            backend,
            format,
            &crate::RecordingOutputs::default(),
        )
        .expect("the recording should have been made on the backend that is here");

        assert_eq!(
            report.capture_method(),
            CaptureMethod::WindowsGraphicsCapture,
            "a memory of a method this machine no longer offers must fall back to the ordinary \
             preference order"
        );
        assert!(
            report.frames_encoded() > 0,
            "the recording produced no video at all"
        );
        // And it cost nothing to discover. Falling back *after* asking for a
        // method nothing is registered for records the fall-through as a
        // `MethodChange`, so an empty list is what says the memory was tested
        // against the candidate list before it was acted on rather than after —
        // which is the difference between a stale memory costing nothing and a
        // stale memory costing the very attempt issue #286 exists to save.
        assert_eq!(
            report.capture_changes(),
            &[],
            "the recording asked for the remembered method, was turned down and fell through, \
             instead of never asking for a method this machine cannot offer"
        );
        drop(recording);
    }

    #[test]
    fn a_method_a_pinned_recording_ended_on_is_not_remembered_over_the_pin() {
        // Issue #286's third acceptance criterion. A recording made under
        // `CaptureMethodSetting::Forced` ended on the method it was told to use;
        // that is the setting being obeyed and says nothing about the machine,
        // so there is nothing to learn and nothing to write.
        let game = GameKey::parse("counter-strike-2").expect("a game key");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725);
        let mut configuration = Configuration::defaults();

        assert!(
            !configuration.remember_capture_method(
                &game,
                CaptureMethodSetting::Forced(CaptureMethod::DesktopDuplication),
                CaptureMethod::DesktopDuplication,
                now,
            ),
            "what a pinned recording ended on is the pin, not an observation"
        );
        assert_eq!(configuration.remembered_capture_method(&game, now), None);
    }

    #[test]
    fn a_capture_that_has_gone_black_is_replaced_rather_than_recorded_to_the_end() {
        // Issue #97's unmet criterion, and the only capture failure that returns
        // no error at all: a capture that has silently stopped working goes on
        // handing over frames and every pixel in them is zero. The detector for
        // it was built and measured against real Direct3D textures, and until
        // this it had no caller — so no recording ever asked whether its frames
        // had gone black, and a black file was reported as a successful one.
        //
        // The texture here is genuinely black on a genuine Direct3D device and
        // the sampler reading it is the production one: nothing about this test
        // is arranged to make the judgement come out a particular way.
        //
        // `BlackFrameWatch` tolerates ten seconds and samples twice a second, so
        // the black stretch has to be at least that long on the source's own
        // clock. The first sample is the loop's first frame, at 200 ms — the
        // frame before it opened the encoder — and the twenty-first is at
        // 10,200 ms, which is the 102nd frame this backend produces. The script
        // has more than that so the detection rather than the script is what
        // ends the black stretch.
        let recovered = recording_that_recovers(
            "capture-gone-black",
            0x00,
            vec![vec![Step::Drew; 115]],
            vec![vec![Step::Drew; 10]],
            200,
        )
        .unwrap_or_else(|(error, _recording)| {
            panic!("a black capture should be replaced, not fatal: {error}")
        });
        let report = &recovered.report;

        let changes = report.capture_changes();
        assert_eq!(
            changes.len(),
            1,
            "a capture that produced nothing but black for over ten seconds was recorded to the \
             end and reported as a successful recording (issues #97 and #285): {changes:?}"
        );
        assert_eq!(
            changes[0].trigger(),
            clipped_capture::FallbackTrigger::BlackFrames,
            "the change must be attributed to the black frames rather than to an error, because \
             there was no error: {changes:?}"
        );
        assert_eq!(
            report.capture_method(),
            CaptureMethod::DesktopDuplication,
            "the black backend went on capturing"
        );
        assert!(
            changes[0].reason().contains("black"),
            "the reason must say what was seen: {}",
            changes[0].reason()
        );
        assert!(
            recovered.replacement.creations() == 1,
            "no replacement was ever created"
        );

        // The lit backend's frames are in the file after the black ones. 101 of
        // the black backend's 102 reached the loop — the first opened the
        // encoder — and nine of the replacement's ten did, for the same reason.
        // Counted as *captured* rather than as encoded because a writer that
        // falls behind drops encodes, which is a fact about the machine.
        assert_eq!(
            report.frames_captured(),
            110,
            "101 black frames and 9 lit ones is what this script produces; a different number \
             means the black run was measured somewhere else"
        );
        assert!(
            report.frames_encoded() > 101,
            "the recording did not carry on after the black capture was replaced: {} frames",
            report.frames_encoded()
        );
        decodes_at_least(&recovered.recording, report.frames_encoded());
    }

    #[test]
    fn an_interrupted_backend_is_restarted_rather_than_given_up_on() {
        // `CaptureError::Interrupted` is a driver reset or a mode change, and it
        // says outright that the target is still where it was — so the *same*
        // backend is started again rather than the preferred method being lost
        // for the rest of the recording. The encoder is still reopened, because
        // the restarted backend brings a new session with it.
        let recovered = recording_that_recovers(
            "capture-interrupted",
            0x40,
            // Two scripts for one factory: the second is the backend the
            // restart creates, which is a new backend from the same method.
            vec![
                vec![Step::Drew, Step::Drew, Step::Fails(interrupted)],
                vec![Step::Drew; 10],
            ],
            vec![Vec::new()],
            60,
        )
        .unwrap_or_else(|(error, _recording)| {
            panic!("an interrupted backend should be restarted: {error}")
        });
        let report = &recovered.report;

        let changes = report.capture_changes();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(
            changes[0].is_restart(),
            "a driver reset restarts the backend it interrupted; falling back would cost the \
             user their preferred method for a fault it did not have: {changes:?}"
        );
        assert_eq!(
            report.capture_method(),
            CaptureMethod::WindowsGraphicsCapture,
            "the method did not change, so the report must not say it did"
        );
        assert_eq!(
            recovered.preferred.creations(),
            2,
            "the same factory should have been asked for a second backend"
        );
        assert_eq!(
            recovered.replacement.creations(),
            0,
            "the other method was never needed and must not have been created"
        );
        assert!(report.frames_encoded() > 1);
        decodes_at_least(&recovered.recording, report.frames_encoded());
    }

    #[test]
    fn a_failure_nothing_can_take_over_from_still_leaves_the_recording_that_was_made() {
        // The half of this that must not regress. Recovery runs on every
        // recording and the failure it guards against is rare, so a bug in it
        // would cost footage that would otherwise have been fine (AGENTS.md
        // section 56). When there is no second backend the answer has to be
        // exactly what it was before any of this existed: the file is finalised
        // where it stands, everything up to the failure plays, and the error
        // says what was tried.
        let device = test_device().expect("no Direct3D 11 device could be created, on WARP either");
        let only = leaked(
            ScriptedFactory::new(CaptureMethod::WindowsGraphicsCapture, counter())
                .painting(device, 0x40)
                .planning([vec![
                    Step::Drew,
                    Step::Drew,
                    Step::Drew,
                    Step::Fails(opted_out),
                ]]),
        );
        let (fallback, backend, format) = scripted_capture(candidates(&[only]));

        let recording = TemporaryRecording::new("capture-exhausted");
        let settings = loop_settings(&recording);
        let error = record_frames(
            &settings,
            &StopAfter::polls(40),
            fallback,
            backend,
            format,
            &crate::RecordingOutputs::default(),
        )
        .expect_err("there was no other backend to take over");

        assert!(
            matches!(error, SessionError::CaptureExhausted(_)),
            "the user is owed every candidate that was tried and what each said: {error}"
        );
        assert!(
            recording.path().exists(),
            "the recording made before the failure is the thing that cannot be made again \
             (AGENTS.md section 17); it was deleted"
        );
        // Two frames reached the encoder — the first opened it — so the file has
        // video in it and is not the empty header `conclude` removes.
        decodes_at_least(&recording, 1);
    }
}
