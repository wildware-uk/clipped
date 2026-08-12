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

use clipped_capture::CaptureMethod;
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

/// What one recording contained, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingReport {
    pub(crate) output: PathBuf,
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
    pub(crate) packets_written: u64,
    pub(crate) timestamps_corrected: u64,
    pub(crate) duration: Duration,
    pub(crate) end_reason: EndReason,
}

impl RecordingReport {
    /// The file that was written.
    #[must_use]
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// Which capture backend produced the frames.
    #[must_use]
    pub const fn capture_method(&self) -> CaptureMethod {
        self.capture_method
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

    /// Packets the muxer wrote.
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
            "Recorded {frames} frames of {width}x{height} {codec} in {seconds:.2}s to {path} \
             ({encoder}, {method}, {rate} sustained; {dropped} frames dropped). {reason}",
            frames = self.frames_encoded,
            width = self.width,
            height = self.height,
            codec = self.codec,
            seconds = self.duration.as_secs_f64(),
            path = self.output.display(),
            encoder = self.encoder,
            method = self.capture_method,
            rate = self
                .sustained_framerate()
                .map_or_else(|| "no rate".to_owned(), |rate| format!("{rate:.1} fps")),
            dropped = self.frames_dropped_writer_behind,
            reason = self.end_reason,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> RecordingReport {
        RecordingReport {
            output: PathBuf::from(r"D:\clips\session.mkv"),
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
            packets_written: 181,
            timestamps_corrected: 0,
            duration: Duration::from_millis(6_000),
            end_reason: EndReason::Stopped,
        }
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
