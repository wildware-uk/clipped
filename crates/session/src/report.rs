//! What a recording turned out to be.
//!
//! Returned rather than only logged, because three different callers need it:
//! the recorder prints it to whoever ran the command, the library will store
//! some of it against the recording (M4), and the end-to-end tests assert on it
//! — a test that only checks a file exists cannot tell a recording of 181
//! frames from a recording of six.
//!
//! The counts are deliberately separate rather than one "dropped" number. A
//! frame skipped to hold the requested rate is the recorder doing what it was
//! asked; a frame skipped because the muxer was behind is the recorder failing
//! to keep up; a frame the *source* never handed over is neither. Adding them
//! together would produce a figure that means nothing (AGENTS.md section 19).

use core::fmt;
use core::time::Duration;
use std::path::{Path, PathBuf};

use clipped_capture::{CaptureMethod, MethodChange, SyncState};
use clipped_encoder::{Codec, EncoderKind};

/// Why a recording ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EndReason {
    /// The stop signal was raised — Ctrl+C, or the desktop application asking.
    Stopped,
    /// The window closed, or the display was disconnected. Everything up to
    /// that moment is in the file.
    TargetLost,
    /// The window changed size.
    ///
    /// Matroska fixes a track's dimensions in the header and the encoder is
    /// configured for one size, so there is no honest way to carry the new size
    /// into the same file. The recording is finished where it is rather than
    /// filled with frames the track does not describe.
    TargetResized,
    /// The drive being recorded to was nearly full, so the recording was
    /// finished while there was still room to finish it properly.
    ///
    /// Deliberate, and not a failure: the alternative is writes that fail one
    /// after another until the *trailer* write fails too, which costs the file
    /// its duration and its cue index (`crate::disk`, AGENTS.md section 17).
    DiskSpaceLow,
    /// The drive being recorded to stopped answering — unplugged, or offline.
    ///
    /// Nothing further could be written, so the recording was closed where it
    /// was. Whether the close itself reached the drive depends on when it went.
    OutputUnavailable,
}

impl fmt::Display for EndReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stopped => "Stopped by request.",
            Self::TargetLost => "Stopped because the recorded window closed.",
            Self::TargetResized => {
                "Stopped because the recorded window changed size, which one file cannot follow."
            }
            Self::DiskSpaceLow => {
                "Stopped because the drive was nearly full, while there was still room to \
                 finish the file properly."
            }
            Self::OutputUnavailable => {
                "Stopped because the drive being recorded to stopped answering."
            }
        })
    }
}

impl EndReason {
    /// The token this reason is written as, in a session's record and in logs.
    ///
    /// The same words `clipped-ipc` and `clipped-library` use for the same
    /// thing, so `end_reason=target-lost` in a support bundle means one thing
    /// whichever file it came from (`docs/sessions.md`).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::TargetLost => "target-lost",
            Self::TargetResized => "target-resized",
            Self::DiskSpaceLow => "disk-space-low",
            Self::OutputUnavailable => "output-unavailable",
        }
    }
}

/// How far one audio track moved against the recording's reference clock.
///
/// Measured rather than assumed, per `docs/av-sync.md`: every buffer an endpoint
/// delivers carries two accounts of the same moment — where the endpoint said it
/// belongs, and where the track built from counting samples puts it — and
/// `clipped_capture::DriftEstimator` turns the stream of pairs into these
/// figures. It is a measurement of a *change*, so a constant offset that was
/// already there when the capture started does not appear in it at any size;
/// that one needs a subject whose sound and picture are known to be
/// simultaneous, which is what `tests/capture/av_sync.rs` is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSyncReport {
    pub(crate) first_offset_nanos: i64,
    pub(crate) latest_offset_nanos: i64,
    pub(crate) peak_offset_nanos: i64,
    pub(crate) observations: u64,
    pub(crate) discontinuities: u64,
    pub(crate) drift_parts_per_billion: Option<i64>,
    pub(crate) state: SyncState,
}

impl AudioSyncReport {
    /// The first offset observed, in nanoseconds. Positive is sound behind
    /// picture.
    ///
    /// Worth reading beside [`latest`](Self::latest_offset_nanos): a track that
    /// starts 30 ms out and stays there has an alignment problem, and one that
    /// starts at zero and ends 30 ms out has a drift problem.
    #[must_use]
    pub const fn first_offset_nanos(&self) -> i64 {
        self.first_offset_nanos
    }

    /// The last offset observed, in nanoseconds.
    #[must_use]
    pub const fn latest_offset_nanos(&self) -> i64 {
        self.latest_offset_nanos
    }

    /// The largest offset seen in either direction, keeping its sign.
    #[must_use]
    pub const fn peak_offset_nanos(&self) -> i64 {
        self.peak_offset_nanos
    }

    /// How many buffers were measured.
    #[must_use]
    pub const fn observations(&self) -> u64 {
        self.observations
    }

    /// How many times the offset stepped rather than drifted — a timeline
    /// correcting itself, or an endpoint change.
    #[must_use]
    pub const fn discontinuities(&self) -> u64 {
        self.discontinuities
    }

    /// The fitted drift rate over the current correction-free segment, in parts
    /// per billion, or [`None`] when there was not enough of one to fit a line
    /// to.
    ///
    /// Parts per billion rather than per million because the rates worth
    /// reporting are single-digit parts per million, and an integer keeps two
    /// recordings comparable where a float would not.
    #[must_use]
    pub const fn drift_parts_per_billion(&self) -> Option<i64> {
        self.drift_parts_per_billion
    }

    /// Whether the track ended inside `SyncTolerance::default()`, and if not,
    /// which way.
    #[must_use]
    pub const fn state(&self) -> SyncState {
        self.state
    }
}

/// What one of a recording's audio tracks turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrackReport {
    pub(crate) track_name: String,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) device: Option<String>,
    pub(crate) buffers: u64,
    pub(crate) frames: u64,
    pub(crate) synthesised_silence_frames: u64,
    pub(crate) frames_before_the_recording: u64,
    pub(crate) buffers_dropped_writer_behind: u64,
    pub(crate) format_changes: u64,
    pub(crate) sync: Option<AudioSyncReport>,
}

impl AudioTrackReport {
    /// A report for a track that has recorded nothing yet.
    pub(crate) fn new(
        track_name: String,
        sample_rate: u32,
        channels: u16,
        device: Option<String>,
    ) -> Self {
        Self {
            track_name,
            sample_rate,
            channels,
            device,
            buffers: 0,
            frames: 0,
            synthesised_silence_frames: 0,
            frames_before_the_recording: 0,
            buffers_dropped_writer_behind: 0,
            format_changes: 0,
            sync: None,
        }
    }

    /// Records one buffer the capture handed over.
    pub(crate) fn note_buffer(&mut self, frames: u64, origin: clipped_audio::SampleOrigin) {
        self.buffers += 1;
        self.frames += frames;
        if origin == clipped_audio::SampleOrigin::SynthesisedSilence {
            self.synthesised_silence_frames += frames;
        }
    }

    /// Records frames dropped for describing a moment before the recording
    /// started.
    pub(crate) fn note_trimmed(&mut self, frames: u64) {
        self.frames_before_the_recording += frames;
    }

    /// Records a buffer the writer had no room for.
    pub(crate) fn note_dropped(&mut self) {
        self.buffers_dropped_writer_behind += 1;
    }

    /// Records the endpoint being replaced by one of a different shape.
    pub(crate) fn note_format_change(&mut self) {
        self.format_changes += 1;
    }

    /// Attaches the synchronisation measurement, once the source has stopped.
    pub(crate) fn with_sync(&mut self, sync: Option<AudioSyncReport>) {
        self.sync = sync;
    }

    /// The name the track carries in the container, as an editor shows it.
    #[must_use]
    pub fn track_name(&self) -> &str {
        &self.track_name
    }

    /// The sampling rate the track was declared at.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// How many channels each frame has.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// The device Windows was capturing, if there was one.
    #[must_use]
    pub fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }

    /// How many buffers the capture handed over.
    #[must_use]
    pub const fn buffers(&self) -> u64 {
        self.buffers
    }

    /// How many frames of audio the capture handed over, real and synthesised.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Of those, frames of silence `clipped-audio` synthesised because the
    /// device produced nothing for that period.
    ///
    /// Not a fault: it is what keeps the track the same length as the recording
    /// (`docs/audio-routing.md`). A track that is *all* synthesised silence is
    /// a device that never produced anything.
    #[must_use]
    pub const fn synthesised_silence_frames(&self) -> u64 {
        self.synthesised_silence_frames
    }

    /// Frames dropped for describing a moment before the first video frame.
    ///
    /// Expected, and small: the audio endpoint is opened while the capture
    /// backend is still initialising, so the first buffer or two of every
    /// recording precede its epoch (`docs/av-sync.md`).
    #[must_use]
    pub const fn frames_before_the_recording(&self) -> u64 {
        self.frames_before_the_recording
    }

    /// Buffers lost because the thread writing the file had not caught up.
    ///
    /// **This one is a fault.** Anything above zero is a hole in the track.
    /// Nothing after the hole slides — every later packet keeps the media time
    /// its own hardware gave it — but the sound in it is gone.
    #[must_use]
    pub const fn buffers_dropped_writer_behind(&self) -> u64 {
        self.buffers_dropped_writer_behind
    }

    /// How many times the device was replaced by one this build cannot follow
    /// inside one file, after which the track is silence.
    #[must_use]
    pub const fn format_changes(&self) -> u64 {
        self.format_changes
    }

    /// How far this track moved against the recording's reference clock.
    #[must_use]
    pub const fn sync(&self) -> Option<AudioSyncReport> {
        self.sync
    }
}

impl fmt::Display for AudioTrackReport {
    /// The line the recorder prints for one audio track when a recording ends.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{name}: {seconds:.2}s at {rate} Hz, {channels} channel{plural}",
            name = self.track_name,
            seconds = if self.sample_rate == 0 {
                0.0
            } else {
                self.frames as f64 / f64::from(self.sample_rate)
            },
            rate = self.sample_rate,
            channels = self.channels,
            plural = if self.channels == 1 { "" } else { "s" },
        )?;
        if let Some(device) = &self.device {
            write!(formatter, " from {device}")?;
        }
        if self.frames == 0 {
            formatter.write_str(" — nothing was recorded on this track")?;
        } else if self.synthesised_silence_frames == self.frames {
            formatter.write_str(" — the device produced nothing, so the track is silence")?;
        }
        if self.buffers_dropped_writer_behind > 0 {
            write!(
                formatter,
                " — {} buffers were lost because the disk could not keep up",
                self.buffers_dropped_writer_behind
            )?;
        }
        Ok(())
    }
}

/// What one recording contained, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingReport {
    pub(crate) output: Option<PathBuf>,
    pub(crate) capture_method: CaptureMethod,
    pub(crate) encoder: EncoderKind,
    pub(crate) codec: Codec,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) requested_framerate: u32,
    pub(crate) frames_captured: u64,
    pub(crate) frames_encoded: u64,
    pub(crate) frames_skipped_for_rate: u64,
    pub(crate) frames_dropped_writer_behind: u64,
    pub(crate) frames_missed_by_source: u64,
    pub(crate) times_target_minimised: u64,
    pub(crate) longest_source_silence: Duration,
    pub(crate) packets_written: u64,
    pub(crate) timestamps_corrected: u64,
    pub(crate) duration: Duration,
    pub(crate) end_reason: EndReason,
    pub(crate) audio_tracks: Vec<AudioTrackReport>,
    pub(crate) capture_changes: Vec<MethodChange>,
}

impl RecordingReport {
    /// The file that was written, or [`None`] for a capture that wrote none.
    ///
    /// [`None`] is SPEC.md section 4's Manual/Replay mode: the capture ran, the
    /// encoder produced everything below, and the packets went to the replay
    /// buffer instead of into a container
    /// ([`RecordingSettings::buffered`](crate::RecordingSettings::buffered),
    /// ADR 0018). It is an [`Option`] rather than an empty path so that nothing
    /// downstream can hand a user a file name for a file that was never going
    /// to exist (AGENTS.md section 54).
    #[must_use]
    pub fn output(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    /// Which capture backend produced the frames **at the end**.
    ///
    /// Not necessarily the one selection chose at the start: a backend that
    /// fails or goes black mid-recording is restarted or replaced, and this is
    /// the method that was actually running when the file was finished
    /// ([issue #285](https://github.com/wildware-uk/clipped/issues/285)).
    /// [`capture_changes`](Self::capture_changes) is the account of how it got
    /// there, and is empty for the ordinary recording where nothing changed.
    #[must_use]
    pub const fn capture_method(&self) -> CaptureMethod {
        self.capture_method
    }

    /// Every capture backend restart and replacement, in the order they
    /// happened.
    ///
    /// Empty for a recording whose backend never faltered, which is nearly all
    /// of them. Each entry carries the method before, the method after, what
    /// triggered it and the failure in the words the failure used, because
    /// "Desktop Duplication" in a recording that started on Windows Graphics
    /// Capture is otherwise a fact with no explanation attached
    /// (`docs/capture-pipeline.md`, SPEC.md section 36).
    #[must_use]
    pub fn capture_changes(&self) -> &[MethodChange] {
        &self.capture_changes
    }

    /// Which encoder family encoded them.
    #[must_use]
    pub const fn encoder(&self) -> EncoderKind {
        self.encoder
    }

    /// Which codec is in the file.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// The size of the picture in the file.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The frame rate the recording was asked for, which is the ceiling the
    /// capture loop held it to.
    ///
    /// Worth reading beside [`sustained_framerate`](Self::sustained_framerate):
    /// a recording that asked for 60 and sustained 30 was made of a source
    /// producing 30, not of a recorder losing half of them — which is what
    /// [`frames_dropped_writer_behind`](Self::frames_dropped_writer_behind)
    /// being zero says.
    #[must_use]
    pub const fn requested_framerate(&self) -> u32 {
        self.requested_framerate
    }

    /// Frames the capture backend handed over.
    #[must_use]
    pub const fn frames_captured(&self) -> u64 {
        self.frames_captured
    }

    /// Frames submitted to the encoder, which is how many pictures the file
    /// should decode to.
    #[must_use]
    pub const fn frames_encoded(&self) -> u64 {
        self.frames_encoded
    }

    /// Frames deliberately not encoded, because the source was producing them
    /// faster than the requested frame rate.
    ///
    /// Expected, and not a fault: recording a 144 fps game at 60 skips 84 of
    /// every 144 by definition (`crate::FrameGate`).
    #[must_use]
    pub const fn frames_skipped_for_rate(&self) -> u64 {
        self.frames_skipped_for_rate
    }

    /// Frames dropped because the thread writing the file had not caught up.
    ///
    /// **This one is a fault.** Anything above zero means the disk, or the
    /// muxer, could not keep pace with the encoder and the recording is missing
    /// frames the user asked for. It is counted and reported rather than
    /// stalling capture, because a capture thread that blocks makes the game
    /// stutter (AGENTS.md section 20).
    #[must_use]
    pub const fn frames_dropped_writer_behind(&self) -> u64 {
        self.frames_dropped_writer_behind
    }

    /// Frames the *source* produced and the capture API never handed over, as
    /// far as the backend was able to say.
    #[must_use]
    pub const fn frames_missed_by_source(&self) -> u64 {
        self.frames_missed_by_source
    }

    /// How many separate stretches of this recording the window was minimised
    /// for.
    ///
    /// Zero for almost every recording. Anything above it is a stretch during
    /// which capture produced nothing at all and the file accumulated a frozen
    /// picture — ordinary when somebody alt-tabs out of an exclusive fullscreen
    /// game, which minimises it — and it is reported rather than left to be
    /// deduced from a duration that does not match the wall clock
    /// ([issue #383](https://github.com/wildware-uk/clipped/issues/383)).
    ///
    /// Stretches, not acquisitions: a window minimised for a minute is one
    /// thing that happened.
    #[must_use]
    pub const fn times_target_minimised(&self) -> u64 {
        self.times_target_minimised
    }

    /// The longest unbroken stretch in which capture produced no frame at all.
    ///
    /// Zero for the ordinary recording, and small for one of a mostly-still
    /// screen: a source produces a frame only when its content changes, so a
    /// paused game or a static menu legitimately goes quiet and this is not a
    /// fault. What it answers is the question a duration that does not match the
    /// wall clock provokes — where did the time go — and, in particular, the one
    /// case where the source was not idle at all: a display the operating system
    /// has powered down delivers nothing through Desktop Duplication while
    /// reporting itself attached and awake
    /// ([issue #461](https://github.com/wildware-uk/clipped/issues/461),
    /// ADR 0015).
    ///
    /// The longest stretch rather than the total, because the two say different
    /// things: four minutes lost in one go is a hole somebody will notice, and
    /// four minutes lost a tenth of a second at a time across an afternoon is a
    /// screen that was not changing much.
    ///
    /// A minimised window is *not* counted here. It has its own count in
    /// [`times_target_minimised`](Self::times_target_minimised), because
    /// "nothing new to show" and "nothing to show until somebody acts" are
    /// different facts about a recording.
    #[must_use]
    pub const fn longest_source_silence(&self) -> Duration {
        self.longest_source_silence
    }

    /// Packets the muxer wrote.
    ///
    /// For a capture with no [`output`](Self::output) there is no muxer, and
    /// this is what the encoder produced instead — the packets that went to the
    /// replay buffer. The same number for the same capture, counted one step
    /// earlier, which is as close to "written" as a sitting that wrote no file
    /// gets.
    #[must_use]
    pub const fn packets_written(&self) -> u64 {
        self.packets_written
    }

    /// Timestamps the muxer had to correct to keep the file valid.
    ///
    /// Anything above zero is worth investigating: it means something upstream
    /// produced a timestamp that went backwards or preceded the start of the
    /// file (`docs/muxing.md`).
    #[must_use]
    pub const fn timestamps_corrected(&self) -> u64 {
        self.timestamps_corrected
    }

    /// The span between the first and last timestamps written.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Why the recording ended.
    #[must_use]
    pub const fn end_reason(&self) -> EndReason {
        self.end_reason
    }

    /// What each of the recording's audio tracks turned out to be, in the order
    /// the container declares them.
    ///
    /// Empty for a recording made with both audio sources turned off, which is a
    /// file with one video stream and nothing else.
    #[must_use]
    pub fn audio_tracks(&self) -> &[AudioTrackReport] {
        &self.audio_tracks
    }

    /// Frames a second actually achieved, over the length of the recording.
    ///
    /// [`None`] for a recording too short to divide by — a single frame has no
    /// rate, and quoting one from a duration of zero would be arithmetic on
    /// nothing.
    #[must_use]
    pub fn sustained_framerate(&self) -> Option<f64> {
        let seconds = self.duration.as_secs_f64();
        (seconds > 0.0 && self.frames_encoded > 1).then(|| {
            // The span covers the gaps between frames, which is one fewer than
            // the number of frames: two frames a second apart are 2 fps over a
            // one-second span, not 2.
            (self.frames_encoded - 1) as f64 / seconds
        })
    }
}

impl fmt::Display for RecordingReport {
    /// The line the recorder prints when a recording ends.
    ///
    /// One line, with the numbers a person would ask for next: how much was
    /// recorded, where it went, what produced it, and whether anything was
    /// lost. The path is shown in full — this goes to the console of whoever
    /// asked for the recording and they need to be able to find the file;
    /// the *log* line carries it redacted (docs/logging.md).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Recorded {frames} frames of {width}x{height} {codec} in {seconds:.2}s {path}\
             ({encoder}, {method}, {rate} sustained; {dropped} frames dropped). {reason}",
            frames = self.frames_encoded,
            width = self.width,
            height = self.height,
            codec = self.codec,
            seconds = self.duration.as_secs_f64(),
            // "to <path>" when there is a file, and nothing at all when there is
            // not. A capture with no continuous recording says so in the line
            // the command that ran it prints beside this one; what this must not
            // do is name a path, because the only paths such a sitting produced
            // are its clips (ADR 0018).
            path = self
                .output
                .as_ref()
                .map_or_else(String::new, |output| format!("to {} ", output.display())),
            encoder = self.encoder,
            method = self.capture_method,
            rate = self
                .sustained_framerate()
                .map_or_else(|| "no rate".to_owned(), |rate| format!("{rate:.1} fps")),
            dropped = self.frames_dropped_writer_behind,
            reason = self.end_reason,
        )?;

        // Said here rather than only in the log because it is the answer to the
        // question this line otherwise provokes — "why is a ten-minute recording
        // ninety seconds long?" — and because a recording nobody was watching is
        // exactly the one this happens to (issue #383).
        if self.times_target_minimised > 0 {
            write!(
                formatter,
                " The window was minimised {times} during the recording, and nothing was \
                 recorded while it was.",
                times = match self.times_target_minimised {
                    1 => "once".to_owned(),
                    times => format!("{times} times"),
                },
            )?;
        }

        // Said for the same reason and under the same rule as the sentence
        // above, and with the same threshold the log line uses: a still screen
        // produces short stretches of this constantly and mentioning them would
        // make the sentence meaningless. Half a minute of nothing is worth a
        // word, because the recording genuinely has nothing in it for that time
        // and a powered-down display is one of the things that causes it
        // (issue #461).
        if self.longest_source_silence >= crate::recording::SILENT_SOURCE_THRESHOLD {
            write!(
                formatter,
                " The source produced no frames for {seconds} seconds at a stretch, and the \
                 recording has nothing in it for that time.",
                seconds = self.longest_source_silence.as_secs(),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> RecordingReport {
        RecordingReport {
            output: Some(PathBuf::from(r"D:\clips\session.mkv")),
            capture_method: CaptureMethod::WindowsGraphicsCapture,
            encoder: EncoderKind::Nvenc,
            codec: Codec::H264,
            width: 1280,
            height: 720,
            requested_framerate: 60,
            frames_captured: 200,
            frames_encoded: 181,
            frames_skipped_for_rate: 19,
            frames_dropped_writer_behind: 0,
            frames_missed_by_source: 2,
            times_target_minimised: 0,
            longest_source_silence: Duration::ZERO,
            packets_written: 181,
            timestamps_corrected: 0,
            duration: Duration::from_millis(6_000),
            end_reason: EndReason::Stopped,
            audio_tracks: Vec::new(),
            capture_changes: Vec::new(),
        }
    }

    fn audio_track(name: &str, frames: u64, channels: u16) -> AudioTrackReport {
        AudioTrackReport {
            track_name: name.to_owned(),
            sample_rate: 48_000,
            channels,
            device: Some("Speakers (Realtek(R) Audio)".to_owned()),
            buffers: frames / 480,
            frames,
            synthesised_silence_frames: 0,
            frames_before_the_recording: 0,
            buffers_dropped_writer_behind: 0,
            format_changes: 0,
            sync: None,
        }
    }

    #[test]
    fn an_audio_track_says_how_much_sound_it_holds_and_where_it_came_from() {
        let line = audio_track("Other System Audio", 288_000, 2).to_string();
        assert_eq!(
            line,
            "Other System Audio: 6.00s at 48000 Hz, 2 channels from Speakers (Realtek(R) Audio)"
        );
    }

    #[test]
    fn a_track_that_recorded_nothing_says_so_rather_than_reading_as_zero_seconds() {
        // The commonest support question about multi-track audio — "why is my
        // microphone track empty?" — starts with the recorder saying that it
        // is (AGENTS.md section 45). A line reading "0.00s" and stopping there
        // reads as a rounding error rather than as an answer.
        let mut track = audio_track("Microphone", 0, 1);
        track.buffers = 0;
        let line = track.to_string();
        assert!(
            line.contains("1 channel from"),
            "a mono track is not plural: {line}"
        );
        assert!(
            line.contains("nothing was recorded on this track"),
            "an empty track has to say so: {line}"
        );
    }

    #[test]
    fn a_track_that_only_ever_got_silence_is_distinguished_from_one_that_got_nothing() {
        // Two different faults. A device that produced nothing at all was never
        // opened or never delivered; a track that is entirely synthesised
        // silence had a device the whole time and it played nothing — a muted
        // microphone, or speakers nobody used.
        let mut track = audio_track("Microphone", 96_000, 1);
        track.synthesised_silence_frames = 96_000;
        let line = track.to_string();
        assert!(
            line.contains("the device produced nothing, so the track is silence"),
            "{line}"
        );
    }

    #[test]
    fn buffers_lost_to_a_slow_disk_are_named_in_the_line_rather_than_only_counted() {
        // The one figure on an audio track that means something went wrong.
        let mut track = audio_track("Other System Audio", 288_000, 2);
        track.buffers_dropped_writer_behind = 17;
        let line = track.to_string();
        assert!(line.contains("17 buffers were lost"), "{line}");
    }

    #[test]
    fn a_recording_carries_its_audio_tracks_in_the_order_the_container_declares_them() {
        // The report is what the recorder prints and what a library will store,
        // and "track 2 is the microphone" has to keep being true of it.
        let mut report = report();
        report.audio_tracks = vec![
            audio_track("Other System Audio", 288_000, 2),
            audio_track("Microphone", 288_000, 1),
        ];
        let names: Vec<&str> = report
            .audio_tracks()
            .iter()
            .map(AudioTrackReport::track_name)
            .collect();
        assert_eq!(names, ["Other System Audio", "Microphone"]);
    }

    #[test]
    fn a_video_only_recording_reports_no_audio_tracks_at_all() {
        // Not one empty track: a source that was turned off has no track in the
        // file, and the report has to say the same thing the container does.
        assert!(report().audio_tracks().is_empty());
    }

    #[test]
    fn the_summary_says_what_was_recorded_and_where() {
        let line = report().to_string();
        assert_eq!(
            line,
            "Recorded 181 frames of 1280x720 H.264 in 6.00s to D:\\clips\\session.mkv \
             (NVIDIA NVENC, Windows Graphics Capture, 30.0 fps sustained; 0 frames dropped). \
             Stopped by request."
        );
    }

    #[test]
    fn a_sustained_rate_counts_intervals_rather_than_frames() {
        // 181 frames over six seconds is 30 intervals a second, not 30.17.
        // Getting this wrong makes every short recording look faster than it
        // was, which is exactly the direction nobody would question.
        let rate = report()
            .sustained_framerate()
            .expect("six seconds of frames has a rate");
        assert!((rate - 30.0).abs() < 1e-9, "{rate}");
    }

    #[test]
    fn a_recording_the_window_was_minimised_during_says_so_in_the_line_the_user_reads() {
        // The stretch is invisible in every other figure: the frame counts are
        // simply lower and the duration still covers the silence, so a
        // ten-minute session that spent nine of them minimised reads as a
        // recording of a very still game. Alt-tabbing out of an exclusive
        // fullscreen game minimises it, so this is not a rare shape (issue
        // #383).
        let mut report = report();
        report.times_target_minimised = 1;
        let once = report.to_string();
        assert!(
            once.contains("The window was minimised once during the recording"),
            "{once}"
        );
        assert!(
            once.contains("nothing was recorded while it was"),
            "saying it happened without saying what it cost is half an answer: {once}"
        );

        report.times_target_minimised = 3;
        assert!(report.to_string().contains("minimised 3 times"), "{report}");
    }

    #[test]
    fn a_recording_whose_source_went_quiet_for_a_long_stretch_says_so_in_the_same_line() {
        // The other way a recording can turn out to have nothing in it, and the
        // one with no visible cause at all: no window was minimised, no error
        // was reported, and the frame counts are simply lower. A display the
        // computer powered down does this while telling every Windows API that
        // it is attached and active (issue #461).
        let mut report = report();

        report.longest_source_silence = crate::recording::SILENT_SOURCE_THRESHOLD
            .checked_sub(Duration::from_millis(100))
            .expect("the threshold is longer than one acquisition");
        let quiet = report.to_string();
        assert!(
            !quiet.contains("produced no frames"),
            "a still screen goes quiet for seconds constantly, and saying so every time would \
             make the sentence worthless: {quiet}"
        );

        report.longest_source_silence = Duration::from_secs(245);
        let dark = report.to_string();
        assert!(
            dark.contains("The source produced no frames for 245 seconds at a stretch"),
            "{dark}"
        );
        assert!(
            dark.contains("the recording has nothing in it for that time"),
            "saying it happened without saying what it cost is half an answer: {dark}"
        );
    }

    #[test]
    fn a_recording_nobody_minimised_says_nothing_about_minimising() {
        // The common case, and what keeps the sentence above worth reading.
        assert!(!report().to_string().contains("minimised"), "{}", report());
    }

    #[test]
    fn a_recording_of_one_frame_quotes_no_rate_rather_than_a_made_up_one() {
        let mut report = report();
        report.frames_encoded = 1;
        report.duration = Duration::ZERO;
        assert_eq!(report.sustained_framerate(), None);
        assert!(report.to_string().contains("no rate"), "{report}");
    }

    #[test]
    fn a_recording_stopped_by_the_disk_guard_says_the_file_was_finished_properly() {
        // The distinction the sentence has to carry: this is a recording that
        // ended deliberately and completely, not one that was truncated. A user
        // who reads it as a failure will go looking for a broken file.
        let mut report = report();
        report.end_reason = EndReason::DiskSpaceLow;
        let line = report.to_string();
        assert!(line.contains("nearly full"), "{line}");
        assert!(
            line.contains("finish the file properly"),
            "the message must say the file is whole: {line}"
        );
    }

    #[test]
    fn every_end_reason_has_a_token_and_no_two_share_one() {
        // The tokens are what a session's record, the IPC protocol and the
        // library index are joined by, so two reasons sharing a word would make
        // a support bundle unreadable and a reason with no word would index as
        // nothing.
        let reasons = [
            EndReason::Stopped,
            EndReason::TargetLost,
            EndReason::TargetResized,
            EndReason::DiskSpaceLow,
            EndReason::OutputUnavailable,
        ];
        let mut tokens: Vec<&str> = reasons.iter().map(|reason| reason.token()).collect();
        tokens.sort_unstable();
        let count = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), count, "two end reasons share a token");
        assert!(tokens.iter().all(|token| !token.is_empty()));
    }

    #[test]
    fn a_recording_that_ended_because_the_window_closed_says_so() {
        let mut report = report();
        report.end_reason = EndReason::TargetLost;
        assert!(
            report
                .to_string()
                .ends_with("Stopped because the recorded window closed."),
            "{report}"
        );
    }
}
