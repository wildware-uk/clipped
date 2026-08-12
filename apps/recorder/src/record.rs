//! The `record` subcommand.
//!
//! Four steps, and only the first two belong to this file: validate the
//! arguments into a [`RecordingConfig`], resolve the capture target into one
//! window through `clipped-windows`, install the Ctrl+C handler, and hand the
//! whole thing to `clipped-session`, which owns the capture, encoding and
//! muxing between them.
//!
//! The split is the architecture rather than a preference. `clipped-session` is
//! the crate whose documented job is "wiring capture, audio, encoding and
//! muxing together and recovering from failures in any of them"; this module is
//! a command line over it, and a recording loop written here would be one the
//! desktop application could not reach when it drives the recorder over IPC in
//! M5 (ADR 0002).
//!
//! # Stopping
//!
//! [`crate::shutdown`] is the seam. The signal a Ctrl+C raises is what the
//! session's recording loop polls between frames, so the recording stops at a
//! frame boundary; the session then flushes the encoder and closes the
//! container, and the finalisation hook below runs afterwards to say so. What
//! the hook cannot do is the flushing itself — the encoder and the writer
//! belong to the session, and reaching back into them from here would be a
//! second owner for the same file handle.

use std::error::Error;
use std::fmt;
use std::path::Path;

use clipped_logging::RedactedPath;
use clipped_session::{
    record, AudioSourceSetting, CaptureTargetSettings, CodecPreference, EncoderPreference,
    RecordingFailure, RecordingReport, RecordingSettings, ResolutionSetting, SessionError,
};
use clipped_windows::{
    enumerate_windows, resolve, DpiAwareness, ResolveError, TargetSelector, WindowInfo,
    WindowsError,
};

use crate::cli::RecordArgs;
use crate::config::{CaptureTarget, ConfigError, RecordingConfig};
use crate::options::{AudioDeviceSelection, EncoderSelection, Resolution, VideoCodec};
use crate::shutdown::{install_ctrl_c_handler, run_until_shutdown, CtrlCError, ShutdownSignal};

/// Why `record` did not record, or did not finish.
#[derive(Debug)]
pub enum RecordError {
    /// The arguments did not describe a recording that could be made.
    Configuration(ConfigError),
    /// The Ctrl+C handler could not be installed, so a recording could not be
    /// stopped cleanly.
    Shutdown(CtrlCError),
    /// The desktop could not be enumerated, so the target could not be found.
    Enumeration(WindowsError),
    /// The target named no window, or more than one.
    Resolution(ResolveError),
    /// The recording itself failed.
    Session(SessionError),
}

impl fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => write!(formatter, "{error}"),
            Self::Shutdown(error) => write!(formatter, "{error}"),
            Self::Enumeration(error) => write!(formatter, "{error}"),
            Self::Resolution(error) => write!(formatter, "{error}"),
            Self::Session(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for RecordError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(error) => Some(error),
            Self::Shutdown(error) => Some(error),
            Self::Enumeration(error) => Some(error),
            Self::Resolution(error) => Some(error),
            Self::Session(error) => Some(error),
        }
    }
}

impl From<ConfigError> for RecordError {
    fn from(error: ConfigError) -> Self {
        Self::Configuration(error)
    }
}

impl From<CtrlCError> for RecordError {
    fn from(error: CtrlCError) -> Self {
        Self::Shutdown(error)
    }
}

impl From<WindowsError> for RecordError {
    fn from(error: WindowsError) -> Self {
        Self::Enumeration(error)
    }
}

impl From<ResolveError> for RecordError {
    fn from(error: ResolveError) -> Self {
        Self::Resolution(error)
    }
}

impl From<SessionError> for RecordError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

/// Validates `args`, resolves the target, and records until Ctrl+C.
///
/// The summary of what was recorded goes to standard error when the recording
/// ends, alongside the same figures in the log. Standard output is left empty:
/// a recording's result is the file, and a `record` invocation in a pipeline
/// should carry nothing (docs/recorder-cli.md).
///
/// # Errors
///
/// [`RecordError::Configuration`] if the arguments cannot describe a recording,
/// [`RecordError::Resolution`] if the target named no window or several,
/// [`RecordError::Enumeration`] if the desktop could not be described,
/// [`RecordError::Shutdown`] if the process could not be made interruptible,
/// and [`RecordError::Session`] if the recording itself failed. A session
/// failure after recording started still leaves a finalised, playable file.
pub fn run(args: &RecordArgs) -> Result<(), RecordError> {
    let config = RecordingConfig::resolve(args)?;
    log_configuration(&config);

    // Before anything is measured. Without it every size below is the
    // compatibility fiction Windows tells a DPI-unaware process, and a
    // recording of a 4K window on a 150% display would be made at 2560x1440
    // — silently, and at the wrong size.
    enable_dpi_awareness();

    let window = resolve_window(&config.target)?;
    let settings = settings_for(&config, &window);

    let signal = ShutdownSignal::new();
    // The handler first, then the flag — in that order, deliberately. A
    // recorder started in a process group of its own, which is how a launcher
    // isolates a long-running child, inherits Ctrl+C *disabled*: safe, but
    // uninterruptible, and a recorder that can only be killed is the failure
    // the whole shutdown path exists to prevent. `allow_ctrl_c` turns it back
    // on, so doing that before the handler is installed would open a window in
    // which Ctrl+C terminates the process with default handling. Installing
    // first cannot: a handler on a process that ignores Ctrl+C simply never
    // runs.
    install_ctrl_c_handler(&signal)?;
    crate::shutdown::allow_ctrl_c();
    tracing::debug!("Ctrl+C will stop the recording at the next frame boundary");

    let outcome = run_until_shutdown(
        &signal,
        |signal| record(&settings, signal),
        |reason| {
            // The session has already flushed the encoder and written the
            // container's trailer by the time this runs — it owns both, and it
            // does that on every path out, including a panic. This is the
            // recorder's own end of the seam, and what it contributes is the
            // record that the run ended deliberately and why.
            tracing::info!(
                reason = %reason,
                "the recording was finalised and the finalisation hook ran"
            );
        },
    );

    let report = match outcome {
        Ok(report) => report,
        Err(error) => {
            report_failure(&error, &config.output);
            return Err(RecordError::Session(error));
        }
    };

    report_completion(&report);
    Ok(())
}

/// Says what went wrong in the terms of somebody who was recording.
///
/// The error itself still reaches `main`, which prints it the way clap prints
/// its own; this is the block above it, and it is where the recording's fate
/// and the thing to do next are said. AGENTS.md section 45: a failure with no
/// action attached is an error code with a sentence around it, and "the drive
/// filled up, everything before that plays, free up space on D:" is what
/// somebody actually needs.
fn report_failure(error: &SessionError, output: &Path) {
    let failure = RecordingFailure::of(error, output);

    tracing::error!(
        failure = failure.kind().token(),
        output = %RedactedPath::new(output),
        detail = failure.detail(),
        "the recording failed"
    );

    eprintln!("{}", failure.headline());
    eprintln!("{}", failure.footage_sentence(output));
    for action in failure.actions() {
        eprintln!("  - {action}");
    }
}

/// Enumerates the desktop and finds the one window the target names.
///
/// Shared with [`crate::serve`], so a recording started over IPC points at the
/// same window `record --window <TITLE>` would, resolved by the same rules and
/// reporting the same candidates for an ambiguous one (AGENTS.md section 55).
///
/// # Errors
///
/// [`RecordError::Enumeration`] if the desktop could not be described, and
/// [`RecordError::Resolution`] if the target named no window or more than one.
pub(crate) fn resolve_window(target: &CaptureTarget) -> Result<WindowInfo, RecordError> {
    let windows = enumerate_windows()?;
    let window = resolve(&windows, &selector(target))?;
    log_target(window);
    Ok(window.clone())
}

/// Turns off the compatibility scaling Windows applies to a DPI-unaware
/// process, or says why the sizes that follow cannot be trusted.
///
/// A property of the process, set once and never unset, so `serve` calls it
/// before it accepts anything rather than per recording.
pub(crate) fn enable_dpi_awareness() {
    match clipped_windows::enable_per_monitor_dpi_awareness() {
        Ok(DpiAwareness::Set) => {}
        Ok(DpiAwareness::AlreadySet) => {
            tracing::debug!("this process was already per-monitor DPI aware");
        }
        // Not a debug line: on a display scaled above 100% this is the
        // difference between recording a window and recording a smaller
        // rectangle Windows made up, and nothing else would say so.
        Err(error) => tracing::warn!(
            %error,
            "this process could not be made per-monitor DPI aware; on a display scaled above \
             100% the recording will be made at the smaller size Windows reports to a \
             DPI-unaware process rather than at the window's real pixel size"
        ),
    }
}

/// The selector `clipped-windows` resolves, from the one the arguments named.
///
/// `record` and `list-windows` go through the same function and the same rules,
/// so `clipped-recorder list-windows --window <TITLE>` shows exactly what
/// `record --window <TITLE>` will point at — including the candidates of an
/// ambiguous title, which is the answer to "why did it record the wrong
/// window?" before it has happened.
fn selector(target: &CaptureTarget) -> TargetSelector {
    match target {
        CaptureTarget::WindowTitle(title) => TargetSelector::WindowTitle(title.clone()),
        CaptureTarget::ProcessName(name) => TargetSelector::ProcessName(name.clone()),
        CaptureTarget::ProcessId(pid) => TargetSelector::ProcessId(*pid),
    }
}

/// Builds the session's settings from a validated command line and the window
/// it resolved to.
///
/// Shared with [`crate::serve`] for the reason [`resolve_window`] is: a
/// recording asked for over IPC and one asked for on the command line must
/// reach the session as the same settings.
pub(crate) fn settings_for(config: &RecordingConfig, window: &WindowInfo) -> RecordingSettings {
    let size = window.geometry().client_size();
    let target =
        CaptureTargetSettings::window(window.handle().as_u64(), size.width(), size.height())
            .content_protected(window.is_content_protected());

    RecordingSettings::new(target, config.output.clone())
        .with_overwrite(config.overwrite)
        .with_resolution(match config.resolution {
            Resolution::Source => ResolutionSetting::Source,
            Resolution::Fixed { width, height } => ResolutionSetting::Fixed { width, height },
        })
        .with_framerate(config.framerate.frames_per_second())
        .with_codec(match config.codec {
            VideoCodec::Auto => CodecPreference::Automatic,
            VideoCodec::H264 => CodecPreference::Fixed(clipped_encoder::Codec::H264),
            VideoCodec::Hevc => CodecPreference::Fixed(clipped_encoder::Codec::Hevc),
            VideoCodec::Av1 => CodecPreference::Fixed(clipped_encoder::Codec::Av1),
        })
        .with_encoder(match config.encoder {
            EncoderSelection::Auto => EncoderPreference::Automatic,
            EncoderSelection::Nvenc => {
                EncoderPreference::Fixed(clipped_encoder::EncoderKind::Nvenc)
            }
            EncoderSelection::Amf => EncoderPreference::Fixed(clipped_encoder::EncoderKind::Amf),
            EncoderSelection::QuickSync => {
                EncoderPreference::Fixed(clipped_encoder::EncoderKind::QuickSync)
            }
            EncoderSelection::Software => {
                EncoderPreference::Fixed(clipped_encoder::EncoderKind::Software)
            }
        })
        // Both default to `default`, so an invocation that says nothing about
        // audio records the output device and the microphone — which is what
        // the product does (SPEC.md section 11). `none` on either turns that
        // source off entirely: no device is opened and no track is declared.
        .with_system_audio(audio_source(&config.system_audio))
        .with_microphone(audio_source(&config.microphone))
}

/// The session's vocabulary for a command line's audio selection.
///
/// The two enumerations are separate for the reason `clipped_session::settings`
/// gives for existing at all: a command-line front end that named
/// `clipped-audio`'s types would be reaching two layers past the crate it calls.
fn audio_source(selection: &AudioDeviceSelection) -> AudioSourceSetting {
    match selection {
        AudioDeviceSelection::Default => AudioSourceSetting::SystemDefault,
        AudioDeviceSelection::Disabled => AudioSourceSetting::Off,
        AudioDeviceSelection::Named(name) => AudioSourceSetting::Named(name.clone()),
    }
}

/// Records what the arguments resolved to, once, before anything uses them.
///
/// This is the line that answers "what was it actually asked to do?" in a bug
/// report, so it carries every resolved value. The output path goes through
/// [`RedactedPath`] because it normally contains the account name, which
/// AGENTS.md section 14 keeps out of logs; the digest still makes two lines
/// about the same file correlatable (docs/logging.md).
fn log_configuration(config: &RecordingConfig) {
    tracing::info!(
        target_selector = %config.target,
        output = %RedactedPath::new(&config.output),
        overwrite = config.overwrite,
        resolution = %config.resolution,
        framerate = config.framerate.frames_per_second(),
        codec = %config.codec,
        encoder = %config.encoder,
        microphone = %config.microphone,
        system_audio = %config.system_audio,
        "resolved recording configuration"
    );
}

/// Records which window the selector resolved to.
///
/// The title is deliberately absent: it is user content, and a window title is
/// the most reliable way to put somebody's document name or open tab into a log
/// file (AGENTS.md section 14). The handle, the process and the size identify
/// it well enough to diagnose a recording of the wrong thing.
fn log_target(window: &WindowInfo) {
    let size = window.geometry().client_size();
    tracing::info!(
        window = %window.handle(),
        process = window.process_name().unwrap_or("unknown"),
        pid = window.process_id(),
        width = size.width(),
        height = size.height(),
        dpi = window.geometry().dpi(),
        "resolved the capture target"
    );

    if window.is_minimised() {
        tracing::warn!(
            "the window is minimised, so it is not drawing and its size is not final; \
             restore it before recording"
        );
    }
}

/// Says what was recorded, to whoever ran the command and to the log.
///
/// To standard error rather than standard output, for the same reason every
/// other diagnostic goes there: standard output is a command's result, and a
/// `record` invocation in a pipeline should carry nothing
/// (docs/recorder-cli.md).
fn report_completion(report: &RecordingReport) {
    eprintln!("{report}");

    // One line per audio track, because "the recording has sound in it" is not
    // something a user should have to open the file to find out — and because
    // an empty track has a cause the line names (AGENTS.md section 45).
    for track in report.audio_tracks() {
        eprintln!("  {track}");
    }

    if report.frames_dropped_writer_behind() > 0 {
        // Separated from the summary because it is the one figure that means
        // something went wrong rather than something happened.
        eprintln!(
            "warning: {} frames were not recorded because the disk could not keep up with \
             the encoder.",
            report.frames_dropped_writer_behind()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::options::Framerate;

    fn config() -> RecordingConfig {
        RecordingConfig {
            target: CaptureTarget::WindowTitle("Counter-Strike 2".to_owned()),
            output: PathBuf::from(r"D:\clips\session.mkv"),
            overwrite: false,
            resolution: Resolution::Source,
            framerate: Framerate::DEFAULT,
            codec: VideoCodec::Auto,
            encoder: EncoderSelection::Auto,
            microphone: AudioDeviceSelection::Default,
            system_audio: AudioDeviceSelection::Default,
        }
    }

    fn window() -> WindowInfo {
        WindowInfo::new(
            clipped_windows::WindowHandle::from_raw(0x0001_04ac),
            "Counter-Strike 2".to_owned(),
            4242,
            Some("cs2.exe".to_owned()),
            clipped_windows::WindowGeometry::new(
                clipped_windows::PixelSize::new(2560, 1440),
                96,
                clipped_windows::MonitorHandle::from_raw(1),
            ),
            false,
            None,
        )
    }

    #[test]
    fn a_configuration_error_is_reported_as_itself_rather_than_wrapped_in_jargon() {
        let error = RecordError::from(ConfigError::ZeroProcessId);
        assert_eq!(error.to_string(), ConfigError::ZeroProcessId.to_string());
        assert!(error.source().is_some(), "the cause should stay reachable");
    }

    #[test]
    fn every_selector_reaches_the_same_resolution_rules_list_windows_uses() {
        // If these drifted, `list-windows --window X` would show one window and
        // `record --window X` would record another, which is the failure the
        // shared function exists to prevent.
        assert_eq!(
            selector(&CaptureTarget::WindowTitle("cs2".to_owned())),
            TargetSelector::WindowTitle("cs2".to_owned())
        );
        assert_eq!(
            selector(&CaptureTarget::ProcessName("cs2.exe".to_owned())),
            TargetSelector::ProcessName("cs2.exe".to_owned())
        );
        assert_eq!(
            selector(&CaptureTarget::ProcessId(4242)),
            TargetSelector::ProcessId(4242)
        );
    }

    #[test]
    fn the_window_the_selector_resolved_to_is_what_the_session_is_pointed_at() {
        let settings = settings_for(&config(), &window());
        assert_eq!(settings.target().size(), (2560, 1440));
        assert_eq!(settings.output(), PathBuf::from(r"D:\clips\session.mkv"));
        assert_eq!(settings.framerate(), 60);
        assert_eq!(settings.resolution(), ResolutionSetting::Source);
        assert_eq!(settings.codec(), CodecPreference::Automatic);
        assert_eq!(settings.encoder(), EncoderPreference::Automatic);
    }

    #[test]
    fn every_command_line_choice_reaches_the_session() {
        // A setting that parses and is then dropped on the floor is a control
        // that silently does nothing (AGENTS.md section 27), and the mapping
        // below is the only place it could be lost.
        let config = RecordingConfig {
            overwrite: true,
            resolution: Resolution::Fixed {
                width: 1920,
                height: 1080,
            },
            framerate: "144".parse().expect("a valid framerate"),
            codec: VideoCodec::Av1,
            encoder: EncoderSelection::Nvenc,
            ..config()
        };

        let settings = settings_for(&config, &window());
        assert!(settings.overwrite());
        assert_eq!(
            settings.resolution(),
            ResolutionSetting::Fixed {
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(settings.framerate(), 144);
        assert_eq!(
            settings.codec(),
            CodecPreference::Fixed(clipped_encoder::Codec::Av1)
        );
        assert_eq!(
            settings.encoder(),
            EncoderPreference::Fixed(clipped_encoder::EncoderKind::Nvenc)
        );
    }

    #[test]
    fn each_audio_selection_reaches_the_session_as_the_source_it_named() {
        // The defaults ask for both, so an invocation that says nothing about
        // audio records the output device and the microphone. A selection that
        // parsed and was then flattened into "audio: yes" is a control that
        // silently does something else (AGENTS.md section 27), and this mapping
        // is the only place it could happen.
        let defaults = settings_for(&config(), &window());
        assert_eq!(defaults.system_audio(), &AudioSourceSetting::SystemDefault);
        assert_eq!(defaults.microphone(), &AudioSourceSetting::SystemDefault);

        let named = RecordingConfig {
            microphone: AudioDeviceSelection::Named("Yeti".to_owned()),
            system_audio: AudioDeviceSelection::Disabled,
            ..config()
        };
        let named = settings_for(&named, &window());
        assert_eq!(
            named.microphone(),
            &AudioSourceSetting::Named("Yeti".to_owned()),
            "the device somebody named has to reach the session, not merely `on`"
        );
        assert_eq!(named.system_audio(), &AudioSourceSetting::Off);
    }

    #[test]
    fn turning_both_sources_off_asks_the_session_for_a_video_only_recording() {
        // `--microphone none --system-audio none`: no device is opened, no
        // track is declared, and nothing is said about audio.
        let silent = RecordingConfig {
            microphone: AudioDeviceSelection::Disabled,
            system_audio: AudioDeviceSelection::Disabled,
            ..config()
        };
        let settings = settings_for(&silent, &window());

        assert!(settings.system_audio().is_off());
        assert!(settings.microphone().is_off());
        assert!(
            !settings.records_audio(),
            "somebody who asked for no audio should have no audio device opened"
        );
    }
}
