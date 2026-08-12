//! Measures the Windows Graphics Capture backend against a controlled test
//! window.
//!
//! The acceptance criteria on
//! [issue #12](https://github.com/wildware-uk/clipped/issues/12) are
//! measurements, not assertions: frame pacing at 60 fps with dropped frames
//! reported, capture of a windowed, borderless and fullscreen application, and
//! a long run that does not leak GPU resources. None of those can be checked by
//! a unit test, and AGENTS.md sections 25 and 26 rule out measuring them against
//! an installed game, so this program brings its own subject: it renders a
//! Direct3D 11 window at a fixed rate, captures it through the real backend, and
//! reports what it saw.
//!
//! It is an example rather than a test because it needs a desktop, a GPU and
//! minutes of wall-clock time — none of which the pull-request CI job has — and
//! because its output is a measurement somebody reads, not a pass or a fail.
//! `tests/capture/` is where the automated version belongs once the shared test
//! applications exist
//! ([issue #23](https://github.com/wildware-uk/clipped/issues/23)).
//!
//! # Running it
//!
//! ```text
//! cargo run --release -p clipped-capture --example wgc_probe -- --help
//! cargo run --release -p clipped-capture --example wgc_probe -- --mode windowed --seconds 60
//! cargo run --release -p clipped-capture --example wgc_probe -- --mode borderless --seconds 60
//! cargo run --release -p clipped-capture --example wgc_probe -- --mode fullscreen --seconds 60
//! cargo run --release -p clipped-capture --example wgc_probe -- --mode monitor --seconds 60
//! cargo run --release -p clipped-capture --example wgc_probe -- --mode lifecycle --seconds 25
//! cargo run --release -p clipped-capture --example wgc_probe -- --mode windowed --seconds 1800 --sample-seconds 60
//! cargo run --release -p clipped-capture --example wgc_probe -- --mode windowed --seconds 10 --stall-ms 200
//! ```
//!
//! # What it measures
//!
//! - **Pacing.** Every frame's `SystemRelativeTime` — the source's own clock,
//!   never the moment this process noticed it — is differenced against the
//!   previous one. The report gives the mean rate, the median and 99th
//!   percentile interval, and how many intervals were more than 1.5 target
//!   intervals long, which is what "a stutter" means when a person says it.
//! - **Dropped frames.** `CapturedFrame::frames_missed`, as the backend derives
//!   it from the source's timestamps. `--stall-ms` holds each frame for a while
//!   before releasing it, which is what a machine that cannot encode fast enough
//!   does to the frame pool, and is how the count is shown to move rather than
//!   assumed to.
//! - **Leaks.** Process working set, private bytes, handle count and the
//!   adapter's *local* video memory usage for this process, sampled on a fixed
//!   interval. A capture that leaks a texture, a frame or a pool per frame shows
//!   up in the last of those within a minute; a run whose samples are flat for
//!   half an hour is the evidence the third acceptance criterion asks for.
//!
//! # Minimising the window invalidates a run
//!
//! Windows Graphics Capture stops delivering frames for a **minimised** window.
//! It keeps delivering for an occluded one — that is the whole reason SPEC.md
//! section 8 prefers this method — but minimise is different, and any pacing or
//! dropped-frame figure covering a minimised stretch is measuring Alt-Tab rather
//! than the backend.
//!
//! So the probe watches its own window's state throughout the run and says so:
//! a run in which the window was minimised prints a `CONTAMINATED` banner and
//! exits non-zero. A contaminated run is discarded and repeated, not reported.
//! The exception is `--mode lifecycle`, which minimises on purpose and says so.

use std::env;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use clipped_capture::{
    registered_backend, registered_declarations, select, Acquisition, CaptureConfig,
    CaptureMethodSetting, CaptureTarget, CaptureTimestamp, FrameSize, TargetHandle, TargetKind,
    TargetProperties,
};
use clipped_windows::enumerate_monitors;

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_DESC, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter3, IDXGIDevice, IDXGIFactory2, IDXGISwapChain, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    DXGI_QUERY_VIDEO_MEMORY_INFO, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH,
    DXGI_SWAP_CHAIN_FULLSCREEN_DESC, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetSystemMetrics, IsIconic, IsWindowVisible, PeekMessageW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, MSG, PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SW_MINIMIZE, SW_RESTORE, SW_SHOW, WM_DESTROY, WNDCLASSW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW,
    WS_POPUP, WS_VISIBLE,
};

/// How the test window is presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// An ordinary window with a title bar and a border.
    Windowed,
    /// A `WS_POPUP` window with no chrome, smaller than the screen.
    Borderless,
    /// A `WS_POPUP` window covering the whole primary display, then asked to
    /// take the output exclusively through `IDXGISwapChain::SetFullscreenState`.
    Fullscreen,
    /// The primary display itself, rather than a window. The test window is
    /// still rendered, so there is something moving to capture.
    Monitor,
    /// A windowed capture whose subject is deliberately abused: it is resized,
    /// minimised, restored and finally closed, on a fixed schedule, so that the
    /// backend's answers to each can be observed rather than assumed.
    Lifecycle,
}

impl Mode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "windowed" => Some(Self::Windowed),
            "borderless" => Some(Self::Borderless),
            "fullscreen" => Some(Self::Fullscreen),
            "monitor" => Some(Self::Monitor),
            "lifecycle" => Some(Self::Lifecycle),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::Borderless => "borderless",
            Self::Fullscreen => "fullscreen (exclusive)",
            Self::Monitor => "monitor",
            Self::Lifecycle => "lifecycle (resize, minimise, restore, close)",
        }
    }
}

/// When each stage of the lifecycle run happens, in seconds from the start of
/// capture. Generous gaps so that a slow machine still shows each stage as its
/// own stretch of the timeline rather than as one blur.
const LIFECYCLE_RESIZE: Duration = Duration::from_secs(3);
const LIFECYCLE_MINIMISE: Duration = Duration::from_secs(7);
const LIFECYCLE_RESTORE: Duration = Duration::from_secs(11);
const LIFECYCLE_CLOSE: Duration = Duration::from_secs(15);

/// What the command line asked for.
#[derive(Debug, Clone, Copy)]
struct Options {
    mode: Mode,
    duration: Duration,
    target_fps: u32,
    sample_interval: Duration,
    width: u32,
    height: u32,
    stall: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Windowed,
            duration: Duration::from_secs(30),
            target_fps: 60,
            sample_interval: Duration::from_secs(30),
            width: 1280,
            height: 720,
            stall: Duration::ZERO,
        }
    }
}

const USAGE: &str = "\
wgc_probe — measures the Windows Graphics Capture backend against a test window

  --mode <windowed|borderless|fullscreen|monitor|lifecycle>
                                                    what to render and capture (default: windowed)
  --seconds <n>                                     how long to capture for (default: 30)
  --fps <n>                                         the rate the test window presents at (default: 60)
  --sample-seconds <n>                              resource sampling interval (default: 30)
  --size <width>x<height>                           test window client size (default: 1280x720)
  --stall-ms <n>                                    hold each frame this long before releasing it,
                                                    to starve the pool the way a slow encoder does
                                                    (default: 0)
  --help                                            this message
";

fn parse_options() -> Result<Option<Options>, String> {
    let mut options = Options::default();
    let mut arguments = env::args().skip(1);

    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| format!("{argument} needs a value"))
        };
        match argument.as_str() {
            "--help" | "-h" => return Ok(None),
            "--mode" => {
                let raw = value()?;
                options.mode = Mode::parse(&raw).ok_or_else(|| format!("unknown mode: {raw}"))?;
            }
            "--seconds" => {
                options.duration =
                    Duration::from_secs(value()?.parse().map_err(|_| "--seconds needs a number")?);
            }
            "--fps" => {
                options.target_fps = value()?.parse().map_err(|_| "--fps needs a number")?;
            }
            "--sample-seconds" => {
                options.sample_interval = Duration::from_secs(
                    value()?
                        .parse()
                        .map_err(|_| "--sample-seconds needs a number")?,
                );
            }
            "--stall-ms" => {
                options.stall = Duration::from_millis(
                    value()?.parse().map_err(|_| "--stall-ms needs a number")?,
                );
            }
            "--size" => {
                let raw = value()?;
                let (width, height) = raw
                    .split_once('x')
                    .ok_or_else(|| format!("--size wants WIDTHxHEIGHT, got {raw}"))?;
                options.width = width.parse().map_err(|_| "--size width is not a number")?;
                options.height = height
                    .parse()
                    .map_err(|_| "--size height is not a number")?;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if options.target_fps == 0 {
        return Err("--fps must be at least 1".to_owned());
    }
    Ok(Some(options))
}

fn main() -> ExitCode {
    tracing_subscriber_stub();

    let options = match parse_options() {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match run(options) {
        Ok(outcome) => {
            print!("{}", outcome.report);
            // A contaminated run must not be quietly readable as a result, so
            // it fails: whoever launched it — a person or a script — is told
            // that these numbers describe the window being minimised rather
            // than the backend, and that the run has to be repeated.
            if outcome.contaminated {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(message) => {
            eprintln!("wgc_probe failed: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Sends `tracing` output to stderr so that the backend's own log lines — which
/// build, adapter and border decisions land in — are visible beside the
/// measurements.
fn tracing_subscriber_stub() {
    // The example deliberately does not depend on `clipped-logging`: that crate
    // configures files, rotation and redaction for a real recorder, and none of
    // that belongs in a measurement run. `tracing` with no subscriber discards
    // everything, so the backend's warnings would vanish silently; printing them
    // here keeps "Windows refused to remove the capture border" visible, which
    // is one of the things being verified.
    struct Stderr;
    impl tracing::field::Visit for Stderr {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            eprint!(" {}={value:?}", field.name());
        }
    }
    impl tracing::Subscriber for Stderr {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::Id {
            tracing::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::Id, _: &tracing::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            eprint!("[{}]", event.metadata().level());
            event.record(&mut Stderr);
            eprintln!();
        }
        fn enter(&self, _: &tracing::Id) {}
        fn exit(&self, _: &tracing::Id) {}
    }

    let _ = tracing::subscriber::set_global_default(Stderr);
}

/// Watches the test window's state so that a run spoiled by minimising it says
/// so instead of reporting the numbers as though they meant something.
///
/// Windows Graphics Capture stops delivering frames for a minimised window, so
/// a minimised stretch shows up as a pacing collapse that looks exactly like a
/// backend problem and is not one. Occlusion is not the same thing and is not
/// watched for: the compositor is asked for the item's own content, so a window
/// drawn over the target changes nothing.
///
/// `IsWindowVisible` is checked alongside `IsIconic` because hiding a window
/// stops composition for the same reason and would spoil a run the same way.
#[derive(Debug)]
struct WindowWatch {
    window: HWND,
    started: Instant,
    /// When the current spoiled stretch began, if one is in progress.
    since: Option<Instant>,
    periods: u32,
    total: Duration,
}

impl WindowWatch {
    fn new(window: HWND, started: Instant) -> Self {
        Self {
            window,
            started,
            since: None,
            periods: 0,
            total: Duration::ZERO,
        }
    }

    /// Samples the window, and returns a timeline entry when the state changed.
    fn poll(&mut self) -> Option<(Duration, String)> {
        // SAFETY: `self.window` is the test window, live for the whole capture;
        // both calls only read the window table and take no pointers.
        let hidden = unsafe { IsIconic(self.window) }.as_bool()
            || !unsafe { IsWindowVisible(self.window) }.as_bool();

        match (hidden, self.since) {
            (true, None) => {
                self.since = Some(Instant::now());
                self.periods += 1;
                Some((
                    self.started.elapsed(),
                    "the test window was minimised or hidden; Windows Graphics Capture \
                     stops delivering frames for one, so this run no longer measures the \
                     backend"
                        .to_owned(),
                ))
            }
            (false, Some(since)) => {
                let spoiled = since.elapsed();
                self.total += spoiled;
                self.since = None;
                Some((
                    self.started.elapsed(),
                    format!(
                        "the test window came back after {:.1}s minimised or hidden",
                        spoiled.as_secs_f64()
                    ),
                ))
            }
            _ => None,
        }
    }

    /// Closes any stretch still open, and reports how much of the run was
    /// spoiled.
    fn finish(mut self) -> (u32, Duration) {
        if let Some(since) = self.since.take() {
            self.total += since.elapsed();
        }
        (self.periods, self.total)
    }
}

/// One resource sample, taken on a fixed interval so that a leak shows as a
/// trend rather than as a single suspicious number.
#[derive(Debug, Clone, Copy)]
struct ResourceSample {
    elapsed: Duration,
    frames: u64,
    working_set_mib: f64,
    private_mib: f64,
    handles: u32,
    gpu_local_mib: f64,
}

/// Everything the run observed.
#[derive(Debug, Default)]
struct Measurements {
    frames: u64,
    timeouts: u64,
    size_changes: u64,
    frames_missed: u64,
    /// Interval between consecutive frames, from the source's own clock.
    intervals: Vec<Duration>,
    samples: Vec<ResourceSample>,
    first_timestamp: Option<CaptureTimestamp>,
    last_timestamp: Option<CaptureTimestamp>,
    non_monotonic: u64,
    /// A timeline of the things that are not frames: size changes, gaps in
    /// delivery, and the target going away. This is what a lifecycle run is
    /// for, and it is the only part of the report that is a sequence rather
    /// than a total.
    events: Vec<(Duration, String)>,
    target_lost: bool,
    /// Stretches in which the test window was minimised or hidden, and how long
    /// they lasted in total. See [`WindowWatch`].
    spoiled_periods: u32,
    spoiled_for: Duration,
}

impl Measurements {
    fn record_frame(&mut self, timestamp: CaptureTimestamp, missed: Option<u32>) {
        self.frames += 1;
        self.frames_missed += u64::from(missed.unwrap_or(0));

        if let Some(previous) = self.last_timestamp {
            match timestamp.duration_since(previous) {
                Some(interval) => self.intervals.push(interval),
                None => self.non_monotonic += 1,
            }
        }
        self.first_timestamp.get_or_insert(timestamp);
        self.last_timestamp = Some(timestamp);
    }

    /// The mean rate over the whole run, from the first and last source
    /// timestamps rather than from wall clock at this end.
    fn measured_fps(&self) -> Option<f64> {
        let (first, last) = (self.first_timestamp?, self.last_timestamp?);
        let span = last.duration_since(first)?;
        (self.frames > 1 && !span.is_zero()).then(|| (self.frames - 1) as f64 / span.as_secs_f64())
    }

    fn percentile(&self, fraction: f64) -> Option<Duration> {
        if self.intervals.is_empty() {
            return None;
        }
        let mut sorted = self.intervals.clone();
        sorted.sort_unstable();
        let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
        sorted.get(index).copied()
    }

    /// Intervals longer than 1.5 target intervals: the frames a viewer would
    /// call a stutter.
    fn late_frames(&self, target_interval: Duration) -> usize {
        let threshold = target_interval.mul_f64(1.5);
        self.intervals
            .iter()
            .filter(|interval| **interval > threshold)
            .count()
    }
}

/// What a run produced: the report, and whether it is fit to be read.
struct Outcome {
    report: String,
    contaminated: bool,
}

fn run(options: Options) -> Result<Outcome, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let window_handle = Arc::new(AtomicIsize::new(0));
    let window_ready = Arc::new(AtomicBool::new(false));
    let window_failed = Arc::new(AtomicBool::new(false));

    let renderer = {
        let stop = Arc::clone(&stop);
        let window_handle = Arc::clone(&window_handle);
        let window_ready = Arc::clone(&window_ready);
        let window_failed = Arc::clone(&window_failed);
        thread::Builder::new()
            .name("test-window".to_owned())
            .spawn(move || {
                if let Err(error) =
                    render_test_window(options, &stop, &window_handle, &window_ready)
                {
                    window_failed.store(true, Ordering::SeqCst);
                    window_ready.store(true, Ordering::SeqCst);
                    eprintln!("test window failed: {error}");
                }
            })
            .map_err(|error| format!("could not start the test window thread: {error}"))?
    };

    let started_waiting = Instant::now();
    while !window_ready.load(Ordering::SeqCst) {
        if started_waiting.elapsed() > Duration::from_secs(10) {
            stop.store(true, Ordering::SeqCst);
            let _ = renderer.join();
            return Err("the test window did not appear within ten seconds".to_owned());
        }
        thread::sleep(Duration::from_millis(10));
    }
    if window_failed.load(Ordering::SeqCst) {
        let _ = renderer.join();
        return Err("the test window could not be created".to_owned());
    }

    // Give the compositor a moment with the window before capturing it, so that
    // the first measured interval is a frame interval rather than a window
    // opening.
    thread::sleep(Duration::from_millis(500));

    let result = capture(options, &window_handle);

    stop.store(true, Ordering::SeqCst);
    let _ = renderer.join();

    result
}

/// Runs the capture loop and produces the report.
fn capture(options: Options, window_handle: &AtomicIsize) -> Result<Outcome, String> {
    let raw = window_handle.load(Ordering::SeqCst);
    let window = HWND(raw as *mut core::ffi::c_void);

    let (kind, handle, size) = match options.mode {
        Mode::Monitor => {
            // SAFETY: `window` is a live window created by the renderer thread
            // and not destroyed until this function returns; `MonitorFromWindow`
            // only reads it.
            let monitor: HMONITOR = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTOPRIMARY) };
            // SAFETY: reading a system metric takes no pointers.
            let (width, height) =
                unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
            let size = FrameSize::new(width.unsigned_abs(), height.unsigned_abs())
                .ok_or("the primary display reported a zero size")?;
            (TargetKind::Monitor, monitor.0 as u64, size)
        }
        _ => {
            let mut client = RECT::default();
            // SAFETY: `window` is live and `client` is a live local the call
            // writes into.
            unsafe { GetClientRect(window, &mut client) }
                .map_err(|error| format!("could not measure the test window: {error}"))?;
            let size = FrameSize::new(
                (client.right - client.left).unsigned_abs(),
                (client.bottom - client.top).unsigned_abs(),
            )
            .ok_or("the test window reported a zero-sized client area")?;
            (TargetKind::Window, raw as u64, size)
        }
    };

    let properties = TargetProperties::new(kind, size);
    let selection = select(
        &registered_declarations(),
        &properties,
        CaptureMethodSetting::Automatic,
    )
    .map_err(|error| format!("no capture backend could be selected: {error}"))?;

    let factory = registered_backend(selection.method())
        .ok_or("selection chose a method with no registered backend")?;
    let mut backend = factory
        .create()
        .map_err(|error| format!("could not create the backend: {error}"))?;

    let target = CaptureTarget::new(TargetHandle::from_raw(handle), properties);
    let config = CaptureConfig::default().with_capture_cursor(false);
    let format = backend
        .initialise(&target, &config)
        .map_err(|error| format!("could not start capture: {error}"))?;

    let sampler = ResourceSampler::new();
    let target_interval = Duration::from_secs(1) / options.target_fps;
    let mut measurements = Measurements::default();
    // Reserved up front, not because the allocation is slow but because this
    // program is the instrument for a leak measurement: a `Vec` that doubles
    // eleven times over half an hour would show up in the working-set column as
    // a steady climb, and the reader would have no way to tell it from a leak in
    // the thing being measured.
    measurements.intervals.reserve(
        usize::try_from(options.duration.as_secs())
            .unwrap_or(0)
            .saturating_mul(usize::try_from(options.target_fps).unwrap_or(60))
            .saturating_add(1024),
    );
    let started = Instant::now();
    let mut watch = WindowWatch::new(window, started);
    let mut next_sample = Duration::ZERO;
    let mut last_frame_at = Instant::now();
    let mut in_a_gap = false;

    while started.elapsed() < options.duration {
        if let Some(event) = watch.poll() {
            measurements.events.push(event);
        }
        if started.elapsed() >= next_sample {
            measurements
                .samples
                .push(sampler.sample(started.elapsed(), measurements.frames));
            next_sample += options.sample_interval;
        }

        // The timeout bounds the wait so the loop notices the deadline; it is
        // not a frame-rate control.
        match backend.acquire(Duration::from_millis(100)) {
            Ok(Acquisition::Frame(frame)) => {
                // A real session hands `frame.texture()` to the encoder here.
                // This program only measures, so it reads the metadata and drops
                // the frame — which is what returns the buffer to the pool, and
                // therefore what makes the leak measurement meaningful.
                measurements.record_frame(frame.timestamp(), frame.frames_missed());
                if !options.stall.is_zero() {
                    // Deliberately inside the arm, so the frame — and the pool
                    // buffer behind it — is still held while this sleeps. That
                    // is what a slow encoder does to the frame pool, and it is
                    // the only way to make the dropped-frame count move without
                    // asking it to lie.
                    thread::sleep(options.stall);
                }
                if in_a_gap {
                    let gap = last_frame_at.elapsed();
                    measurements.events.push((
                        started.elapsed(),
                        format!("frames resumed after {:.1}s without one", gap.as_secs_f64()),
                    ));
                    in_a_gap = false;
                }
                last_frame_at = Instant::now();
            }
            Ok(Acquisition::Timeout) => {
                measurements.timeouts += 1;
                // A source only produces a frame when its content changes, so a
                // timeout on its own is ordinary. A whole second of them is the
                // observable shape of a minimised window, and worth a line.
                if !in_a_gap && last_frame_at.elapsed() > Duration::from_secs(1) {
                    measurements
                        .events
                        .push((started.elapsed(), "no frames for a second".to_owned()));
                    in_a_gap = true;
                }
            }
            // The same silence as a timeout, and named: the backend can now say
            // that the window is minimised rather than only that nothing
            // arrived, which is what `Contamination` below is watching for and
            // what `--mode lifecycle` provokes on purpose.
            Ok(Acquisition::TargetMinimised) => {
                measurements.timeouts += 1;
                if !in_a_gap {
                    measurements
                        .events
                        .push((started.elapsed(), "the window is minimised".to_owned()));
                    in_a_gap = true;
                }
            }
            Ok(Acquisition::SizeChanged(new_size)) => {
                measurements.size_changes += 1;
                let new_format = backend
                    .resize(new_size)
                    .map_err(|error| format!("could not resize the capture: {error}"))?;
                measurements
                    .events
                    .push((started.elapsed(), format!("size changed to {new_format}")));
            }
            Err(error) if matches!(error, clipped_capture::CaptureError::TargetLost { .. }) => {
                // In a lifecycle run this is the last scheduled stage and the
                // point of the exercise. Anywhere else it means the subject went
                // away early, which is still worth reporting rather than
                // treating as a crash.
                measurements.target_lost = true;
                measurements
                    .events
                    .push((started.elapsed(), format!("target lost: {error}")));
                break;
            }
            Err(error) => {
                backend.shut_down();
                return Err(format!(
                    "capture failed after {} frames: {error}",
                    measurements.frames
                ));
            }
        }
    }

    let (spoiled_periods, spoiled_for) = watch.finish();
    measurements.spoiled_periods = spoiled_periods;
    measurements.spoiled_for = spoiled_for;

    measurements
        .samples
        .push(sampler.sample(started.elapsed(), measurements.frames));

    backend.shut_down();
    // Shutting down twice is documented as harmless, and this is the cheapest
    // place to keep that honest.
    backend.shut_down();
    drop(backend);

    let after_shutdown = sampler.sample(started.elapsed(), measurements.frames);

    // A lifecycle run minimises on purpose and is about what the backend does
    // when it happens; every other mode measures the backend against a window
    // nobody touched, and a minimised stretch means it was not.
    let contaminated = measurements.spoiled_periods > 0 && options.mode != Mode::Lifecycle;

    Ok(Outcome {
        report: report(
            options,
            &selection,
            format,
            &measurements,
            target_interval,
            after_shutdown,
            contaminated,
        ),
        contaminated,
    })
}

fn report(
    options: Options,
    selection: &clipped_capture::Selection,
    format: clipped_capture::FrameFormat,
    measurements: &Measurements,
    target_interval: Duration,
    after_shutdown: ResourceSample,
    contaminated: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n=== wgc_probe ===");
    if contaminated {
        let _ = writeln!(
            out,
            "*** CONTAMINATED RUN - DISCARD IT ***\n\
             The test window was minimised or hidden for {:.1}s of this run, in {} \
             stretch(es).\n\
             Windows Graphics Capture stops delivering frames for a minimised window, so \
             every\npacing and dropped-frame figure below measures that rather than the \
             backend. Repeat\nthe run without touching the probe window.",
            measurements.spoiled_for.as_secs_f64(),
            measurements.spoiled_periods
        );
    }
    let _ = writeln!(out, "mode                 : {}", options.mode.label());
    let _ = writeln!(out, "capture method       : {}", selection.setting());
    let _ = writeln!(out, "current method       : {}", selection.method());
    let _ = writeln!(out, "frame format         : {format}");
    let _ = writeln!(
        out,
        "requested duration   : {:.0}s at {} fps",
        options.duration.as_secs_f64(),
        options.target_fps
    );
    let _ = writeln!(out, "frames delivered     : {}", measurements.frames);
    let _ = writeln!(out, "acquisition timeouts : {}", measurements.timeouts);
    let _ = writeln!(out, "size changes         : {}", measurements.size_changes);
    let _ = writeln!(
        out,
        "frames missed        : {} (reported by the backend)",
        measurements.frames_missed
    );
    let _ = writeln!(out, "non-monotonic stamps : {}", measurements.non_monotonic);
    let _ = writeln!(
        out,
        "window minimised     : {}",
        if measurements.spoiled_periods == 0 {
            "never - it was on screen for the whole run".to_owned()
        } else {
            format!(
                "{} stretch(es) totalling {:.1}s{}",
                measurements.spoiled_periods,
                measurements.spoiled_for.as_secs_f64(),
                if contaminated {
                    " <- CONTAMINATED"
                } else {
                    " (scripted by this mode)"
                }
            )
        }
    );

    match measurements.measured_fps() {
        Some(fps) => {
            let _ = writeln!(
                out,
                "measured rate        : {fps:.2} fps (from frame timestamps)"
            );
        }
        None => {
            let _ = writeln!(out, "measured rate        : not enough frames to measure");
        }
    }

    for (label, fraction) in [("median", 0.5), ("p99", 0.99), ("max", 1.0)] {
        if let Some(interval) = measurements.percentile(fraction) {
            let _ = writeln!(
                out,
                "interval {label:<12}: {:.3} ms",
                interval.as_secs_f64() * 1000.0
            );
        }
    }
    let _ = writeln!(
        out,
        "late frames          : {} of {} intervals longer than {:.2} ms (1.5x target)",
        measurements.late_frames(target_interval),
        measurements.intervals.len(),
        target_interval.mul_f64(1.5).as_secs_f64() * 1000.0
    );

    if !measurements.events.is_empty() {
        let _ = writeln!(out, "\ntimeline");
        for (elapsed, event) in &measurements.events {
            let _ = writeln!(out, "{:>8.1}s  {event}", elapsed.as_secs_f64());
        }
    }
    let _ = writeln!(
        out,
        "target lost          : {}",
        if measurements.target_lost {
            "yes"
        } else {
            "no"
        }
    );

    let _ = writeln!(out, "\nresource samples");
    let _ = writeln!(
        out,
        "{:>9}  {:>10}  {:>12}  {:>12}  {:>8}  {:>12}",
        "elapsed", "frames", "workingset", "private", "handles", "gpu local"
    );
    for sample in &measurements.samples {
        let _ = writeln!(
            out,
            "{:>8.0}s  {:>10}  {:>9.1} MiB  {:>9.1} MiB  {:>8}  {:>9.1} MiB",
            sample.elapsed.as_secs_f64(),
            sample.frames,
            sample.working_set_mib,
            sample.private_mib,
            sample.handles,
            sample.gpu_local_mib
        );
    }
    let _ = writeln!(
        out,
        "{:>8}   {:>10}  {:>9.1} MiB  {:>9.1} MiB  {:>8}  {:>9.1} MiB   <- after shut_down",
        "final",
        after_shutdown.frames,
        after_shutdown.working_set_mib,
        after_shutdown.private_mib,
        after_shutdown.handles,
        after_shutdown.gpu_local_mib
    );

    if let (Some(first), Some(last)) = (measurements.samples.first(), measurements.samples.last()) {
        let _ = writeln!(
            out,
            "\ndrift over the run   : working set {:+.1} MiB, private {:+.1} MiB, \
             handles {:+}, gpu local {:+.1} MiB",
            last.working_set_mib - first.working_set_mib,
            last.private_mib - first.private_mib,
            i64::from(last.handles) - i64::from(first.handles),
            last.gpu_local_mib - first.gpu_local_mib
        );
    }

    out
}

/// Reads this process's memory, handle and video-memory usage.
struct ResourceSampler {
    process: HANDLE,
    adapter: Option<IDXGIAdapter3>,
}

impl ResourceSampler {
    fn new() -> Self {
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // closing and is valid for the life of the process.
        let process = unsafe { GetCurrentProcess() };
        Self {
            process,
            adapter: local_adapter(),
        }
    }

    fn sample(&self, elapsed: Duration, frames: u64) -> ResourceSample {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        let size = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).unwrap_or(0);
        let mut handles = 0u32;

        // SAFETY: `self.process` is the current-process pseudo-handle;
        // `counters` and `handles` are live locals, and `size` is the size of
        // the structure being written, which is what the API contract requires.
        // The cast is the documented way to pass the extended structure to an
        // API declared over the base one.
        unsafe {
            let _ = GetProcessMemoryInfo(self.process, (&raw mut counters).cast(), size);
            let _ = GetProcessHandleCount(self.process, &mut handles);
        }

        let gpu_local_mib = self
            .adapter
            .as_ref()
            .and_then(|adapter| {
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                // SAFETY: `adapter` is a live `IDXGIAdapter3` and `info` is a
                // live local the call writes into.
                unsafe {
                    adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info)
                }
                .ok()
                .map(|()| info.CurrentUsage)
            })
            .map_or(f64::NAN, mebibytes);

        ResourceSample {
            elapsed,
            frames,
            working_set_mib: mebibytes(counters.WorkingSetSize as u64),
            private_mib: mebibytes(counters.PrivateUsage as u64),
            handles,
            gpu_local_mib,
        }
    }
}

fn mebibytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// The adapter this process renders on, for the video-memory reading.
fn local_adapter() -> Option<IDXGIAdapter3> {
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: every pointer argument is absent except `device`, which is the
    // address of a live local `Option<ID3D11Device>` — the representation
    // windows-rs uses for that out parameter.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .ok()?;
    }
    let dxgi: IDXGIDevice = device?.cast().ok()?;
    // SAFETY: `GetAdapter` is an ordinary query on a live DXGI device.
    let adapter = unsafe { dxgi.GetAdapter() }.ok()?;
    adapter.cast().ok()
}

/// Creates the test window, renders to it at the requested rate, and pumps its
/// messages until `stop` is set.
///
/// The window is deliberately dull: a full-screen clear whose colour advances
/// once per frame. That is enough for the compositor to have new content every
/// frame — which is what makes the pacing measurement mean anything — without
/// the renderer's own cost becoming what is being measured.
fn render_test_window(
    options: Options,
    stop: &AtomicBool,
    window_handle: &AtomicIsize,
    window_ready: &AtomicBool,
) -> Result<(), String> {
    // SAFETY: `GetModuleHandleW(None)` returns this executable's module handle
    // and takes no pointer arguments.
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map_err(|error| format!("GetModuleHandleW failed: {error}"))?;

    let class_name = w!("ClippedWgcProbeWindow");
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_procedure),
        hInstance: instance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    // SAFETY: `class` is a live, fully initialised `WNDCLASSW` whose string and
    // module fields outlive the call. Registering the same class twice returns
    // an error, which cannot happen here because this runs once per process.
    let atom = unsafe { RegisterClassW(&class) };
    if atom == 0 {
        return Err("RegisterClassW failed".to_owned());
    }

    let (style, x, y, width, height) = window_geometry(options);

    // Every mode except `Fullscreen` is created always-on-top, on a
    // non-primary monitor when one exists (see `probe_window_origin` and
    // `window_geometry`). Both exist for the same reason: "Minimising the
    // window invalidates a run", above, and `WindowWatch` mean this window
    // must never be minimised, and sitting on top of someone's work on the
    // primary monitor is exactly what tempts a person to minimise it.
    // `Fullscreen` already covers the whole display, so neither applies to it,
    // and it keeps `Default::default()` deliberately.
    let extended_style = if options.mode == Mode::Fullscreen {
        Default::default()
    } else {
        WS_EX_TOPMOST
    };

    // SAFETY: the class was registered immediately above, both strings are
    // static wide literals, and no parent, menu or creation parameter is
    // passed.
    let window = unsafe {
        CreateWindowExW(
            extended_style,
            class_name,
            w!("Clipped WGC probe"),
            style,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|error| format!("CreateWindowExW failed: {error}"))?;

    // SAFETY: `window` is the window just created.
    let _ = unsafe { ShowWindow(window, SW_SHOW) };

    let (device, context, swap_chain) = create_swap_chain(window, options)?;
    let mut render_target = create_render_target(&device, &swap_chain)?;

    if options.mode == Mode::Fullscreen {
        // The documented sequence, in order, because DXGI refuses the
        // transition if any part of it is missing: the window has to be in the
        // foreground, the swap chain has to have presented at least once so
        // that its output is known, and the target has to be resized to a mode
        // the output actually has before the exclusive transition is asked for.
        //
        // SAFETY: `window` and `swap_chain` are both live. `ResizeTarget` reads
        // the mode description through a live local pointer and retains
        // nothing; `ClearRenderTargetView` and `Present` are the ordinary render
        // path used by the loop below.
        unsafe {
            let foreground = SetForegroundWindow(window).as_bool();
            if !foreground {
                eprintln!(
                    "[WARN] SetForegroundWindow was refused; Windows only grants the \
                     foreground to a process that already has it or that the user \
                     interacted with, and DXGI will not go exclusive for a background window"
                );
            }
            for _ in 0..10 {
                context.ClearRenderTargetView(&render_target, &[0.0, 0.0, 0.0, 1.0]);
                let _ = swap_chain.Present(1, Default::default());
                pump_messages();
            }

            let mode = DXGI_MODE_DESC {
                Width: width.unsigned_abs(),
                Height: height.unsigned_abs(),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ..Default::default()
            };
            if let Err(error) = swap_chain.ResizeTarget(&mode) {
                eprintln!("[WARN] ResizeTarget failed before the fullscreen transition: {error}");
            }
        }

        // SAFETY: `swap_chain` is live and the output is left to DXGI to
        // choose. A refusal is reported rather than ignored: the mode a run
        // actually exercised has to be the mode it reports (AGENTS.md
        // section 54).
        // SAFETY: `swap_chain` is live; `GetContainingOutput` returns the output
        // the window is on, which is the one the exclusive transition targets.
        let output = unsafe { swap_chain.GetContainingOutput() }.ok();
        // SAFETY: `swap_chain` is live, and `output` is either absent or the
        // output DXGI itself just reported for this swap chain.
        match unsafe { swap_chain.SetFullscreenState(true, output.as_ref()) } {
            Ok(()) => eprintln!("[INFO] the test window took the display exclusively"),
            Err(error) => eprintln!(
                "[WARN] exclusive fullscreen was refused ({error}); the window is a \
                 borderless one covering the display instead"
            ),
        }
        // The buffers follow whatever mode the swap chain ended up in.
        drop(render_target);
        // SAFETY: no view of the buffers is outstanding — `render_target` was
        // dropped on the line above — which is `ResizeBuffers`' one
        // precondition.
        unsafe {
            swap_chain.ResizeBuffers(0, 0, 0, DXGI_FORMAT_B8G8R8A8_UNORM, Default::default())
        }
        .map_err(|error| format!("ResizeBuffers failed: {error}"))?;
        render_target = create_render_target(&device, &swap_chain)?;
    }

    window_handle.store(window.0 as isize, Ordering::SeqCst);
    window_ready.store(true, Ordering::SeqCst);

    let interval = Duration::from_secs(1) / options.target_fps;
    let mut next_present = Instant::now();
    let mut frame: u32 = 0;
    let scripted_from = Instant::now();
    let mut stage = 0usize;

    while !stop.load(Ordering::SeqCst) {
        pump_messages();

        if options.mode == Mode::Lifecycle
            && run_lifecycle_stage(window, scripted_from.elapsed(), &mut stage)
        {
            break;
        }

        // A clear colour that changes every frame, so the compositor has
        // genuinely new content each time rather than a static window it can
        // skip composing.
        let phase = f32::from(u16::try_from(frame % 360).unwrap_or(0)) / 360.0;
        let colour = [phase, 1.0 - phase, (phase * 2.0) % 1.0, 1.0];

        // SAFETY: `render_target` is a live view of the swap chain's current
        // back buffer, and `colour` is a live four-element array, which is what
        // `ClearRenderTargetView` reads.
        unsafe { context.ClearRenderTargetView(&render_target, &colour) };

        // SAFETY: `swap_chain` is live. A present interval of zero and no flags
        // is the unthrottled present this program paces itself.
        let presented = unsafe { swap_chain.Present(0, Default::default()) };
        if presented.is_err() {
            eprintln!("[WARN] Present returned {presented:?}");
        }

        frame = frame.wrapping_add(1);
        next_present += interval;
        let now = Instant::now();
        if next_present > now {
            thread::sleep(next_present - now);
        } else {
            // Behind schedule: give up the slice rather than spinning, and let
            // the deadline catch up on the next frame.
            next_present = now;
            thread::yield_now();
        }
    }

    if options.mode == Mode::Fullscreen {
        // Leaving exclusive fullscreen before the swap chain is released is
        // required by DXGI; skipping it leaves the display in the mode the test
        // chose.
        // SAFETY: `swap_chain` is still live at this point.
        let _ = unsafe { swap_chain.SetFullscreenState(false, None) };
    }

    // SAFETY: `window` was created on this thread and has not been destroyed.
    let _ = unsafe { DestroyWindow(window) };
    Ok(())
}

/// Advances the scripted lifecycle: resize, minimise, restore, close.
///
/// Returns true when the last stage has run and the window should be destroyed,
/// which is what the backend is expected to report as
/// `CaptureError::TargetLost`.
///
/// A schedule rather than an interactive test because AGENTS.md section 25 asks
/// for tests that do not depend on what a person happens to do: the same four
/// transitions happen at the same four moments on every machine, so two runs of
/// this produce timelines that can be compared.
fn run_lifecycle_stage(window: HWND, elapsed: Duration, stage: &mut usize) -> bool {
    // SAFETY: `window` is the live test window, owned by this thread.
    // `SetWindowPos` and `ShowWindow` only act on it, and neither takes a
    // pointer.
    unsafe {
        match *stage {
            0 if elapsed >= LIFECYCLE_RESIZE => {
                eprintln!("[INFO] lifecycle: resizing the window to 960x540");
                let _ = SetWindowPos(
                    window,
                    None,
                    0,
                    0,
                    960,
                    540,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                *stage = 1;
            }
            1 if elapsed >= LIFECYCLE_MINIMISE => {
                eprintln!("[INFO] lifecycle: minimising the window");
                let _ = ShowWindow(window, SW_MINIMIZE);
                *stage = 2;
            }
            2 if elapsed >= LIFECYCLE_RESTORE => {
                eprintln!("[INFO] lifecycle: restoring the window");
                let _ = ShowWindow(window, SW_RESTORE);
                *stage = 3;
            }
            3 if elapsed >= LIFECYCLE_CLOSE => {
                eprintln!("[INFO] lifecycle: closing the window");
                *stage = 4;
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Where the test window goes, for every mode except `Fullscreen`.
///
/// Always-on-top (set by the `CreateWindowExW` call in `render_test_window`)
/// and a non-primary monitor exist for the same reason: whoever is running
/// this probe has been interrupted by it before, because it used to appear
/// over their work on the primary monitor, and minimising it to get their
/// desktop back is exactly what contaminates the run — see "Minimising the
/// window invalidates a run" at the top of this file and `WindowWatch`, which
/// is what detects that after the fact. Occlusion does not have that problem
/// — Windows Graphics Capture keeps delivering frames for a covered window,
/// which is the whole reason SPEC.md section 8 prefers this method — so
/// always-on-top is enough to remove the temptation to move the window at
/// all, and a second monitor, when one exists, means there is nothing of the
/// maintainer's to be over in the first place.
///
/// The first non-primary monitor is chosen, falling back to the primary one
/// if that is all there is; the origin returned is a small inset inside its
/// bounds. If enumeration itself fails, this falls back to the historical
/// `(100, 100)` — a probe that refuses to start because it could not list
/// monitors is worse than one placed slightly less conveniently — and either
/// way the choice is printed so whoever reads the measurement knows where the
/// window was.
fn probe_window_origin() -> (i32, i32) {
    const FALLBACK: (i32, i32) = (100, 100);
    const INSET: i32 = 100;

    let monitors = match enumerate_monitors() {
        Ok(monitors) => monitors,
        Err(error) => {
            eprintln!(
                "[WARN] could not enumerate monitors ({error}); placing the probe window at \
                 (100, 100) instead"
            );
            return FALLBACK;
        }
    };

    let Some(chosen) = monitors
        .iter()
        .find(|monitor| !monitor.is_primary())
        .or_else(|| monitors.first())
    else {
        eprintln!(
            "[WARN] EnumDisplayMonitors reported no displays; placing the probe window at \
             (100, 100)"
        );
        return FALLBACK;
    };

    let bounds = chosen.bounds();
    eprintln!(
        "[INFO] placing the probe window on {} ({}){}",
        chosen.device_name(),
        bounds,
        if chosen.is_primary() {
            " - the only display found, so it is the primary one"
        } else {
            ""
        }
    );
    (bounds.left() + INSET, bounds.top() + INSET)
}

/// Where and how big the test window is, for each mode.
fn window_geometry(
    options: Options,
) -> (
    windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE,
    i32,
    i32,
    i32,
    i32,
) {
    let width = i32::try_from(options.width).unwrap_or(1280);
    let height = i32::try_from(options.height).unwrap_or(720);

    match options.mode {
        Mode::Windowed | Mode::Monitor | Mode::Lifecycle => {
            // The requested size is the *client* size, so that the capture is
            // of a known number of pixels rather than of a window whose chrome
            // varies with the Windows version.
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            // SAFETY: `rect` is a live local the call adjusts in place.
            let _ = unsafe { AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false) };
            let (x, y) = probe_window_origin();
            (
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                x,
                y,
                rect.right - rect.left,
                rect.bottom - rect.top,
            )
        }
        Mode::Borderless => {
            let (x, y) = probe_window_origin();
            (WS_POPUP | WS_VISIBLE, x, y, width, height)
        }
        Mode::Fullscreen => {
            // SAFETY: reading system metrics takes no pointers.
            let (screen_width, screen_height) =
                unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
            (WS_POPUP | WS_VISIBLE, 0, 0, screen_width, screen_height)
        }
    }
}

fn create_swap_chain(
    window: HWND,
    options: Options,
) -> Result<(ID3D11Device, ID3D11DeviceContext, IDXGISwapChain), String> {
    let mut client = RECT::default();
    // SAFETY: `window` is live and `client` is a live local.
    unsafe { GetClientRect(window, &mut client) }
        .map_err(|error| format!("GetClientRect failed: {error}"))?;

    let mut device = None;
    let mut context = None;
    let levels = [D3D_FEATURE_LEVEL_11_0];

    // SAFETY: `levels` is a live local read during the call, and the two out
    // parameters are the addresses of live `Option`s, which is how windows-rs
    // represents them. No adapter and no software module is requested, which
    // selects the default hardware adapter.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|error| format!("D3D11CreateDevice failed: {error}"))?;

    let device = device.ok_or("no device was returned")?;
    let context = context.ok_or("no device context was returned")?;

    let dxgi: IDXGIDevice = device
        .cast()
        .map_err(|error| format!("the device is not a DXGI device: {error}"))?;
    // SAFETY: `dxgi` is a live DXGI device; `GetAdapter` is an ordinary query.
    let adapter =
        unsafe { dxgi.GetAdapter() }.map_err(|error| format!("GetAdapter failed: {error}"))?;
    // SAFETY: `adapter` is live; `GetParent` returns the factory that created
    // it, checked against the requested interface's GUID by windows-rs.
    let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }
        .map_err(|error| format!("the adapter has no IDXGIFactory2 parent: {error}"))?;

    // The flip model, which is what a modern game's swap chain is and what
    // `SetFullscreenState` still works with — the older bit-block model is
    // refused for exclusive fullscreen on current Windows. `ALLOW_MODE_SWITCH`
    // is what makes the fullscreen transition legal at all.
    let description = DXGI_SWAP_CHAIN_DESC1 {
        Width: (client.right - client.left).unsigned_abs(),
        Height: (client.bottom - client.top).unsigned_abs(),
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0.unsigned_abs(),
        ..Default::default()
    };
    let fullscreen = DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
        RefreshRate: DXGI_RATIONAL {
            Numerator: options.target_fps,
            Denominator: 1,
        },
        Windowed: true.into(),
        ..Default::default()
    };

    // SAFETY: `device` is a live Direct3D 11 device, `window` is a live window
    // owned by this thread, and both descriptions are live locals read during
    // the call.
    let swap_chain = unsafe {
        factory.CreateSwapChainForHwnd(&device, window, &description, Some(&fullscreen), None)
    }
    .map_err(|error| format!("CreateSwapChainForHwnd failed: {error}"))?;

    Ok((
        device,
        context,
        swap_chain
            .cast()
            .map_err(|error| format!("the swap chain is not an IDXGISwapChain: {error}"))?,
    ))
}

fn create_render_target(
    device: &ID3D11Device,
    swap_chain: &IDXGISwapChain,
) -> Result<ID3D11RenderTargetView, String> {
    // SAFETY: `swap_chain` is live and buffer zero is its back buffer.
    let back_buffer: ID3D11Texture2D =
        unsafe { swap_chain.GetBuffer(0) }.map_err(|error| format!("GetBuffer failed: {error}"))?;

    let mut view = None;
    // SAFETY: `back_buffer` belongs to `device`, no view description is given
    // so the buffer's own format is used, and `view` is a live local.
    unsafe { device.CreateRenderTargetView(&back_buffer, None, Some(&mut view)) }
        .map_err(|error| format!("CreateRenderTargetView failed: {error}"))?;
    view.ok_or_else(|| "no render target view was returned".to_owned())
}

/// Drains the window's message queue without blocking.
fn pump_messages() {
    let mut message = MSG::default();
    // SAFETY: `message` is a live local; `PM_REMOVE` with no window filter
    // takes messages for any window on this thread, which is the test window.
    // `TranslateMessage` and `DispatchMessageW` read the message that
    // `PeekMessageW` just wrote.
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        // SAFETY: posting a quit message takes no pointers.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // SAFETY: forwarding the message Windows just delivered, unchanged, which
    // is what `DefWindowProcW` exists for.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
