//! Windows platform primitives shared by the capture, audio and encoder crates.
//!
//! Every direct call into a Windows API lives either here or in a `windows/`
//! submodule of the crate that owns the behaviour, so that the rest of the
//! workspace stays free of platform conditionals and a future Linux port has a
//! single well-marked surface to reimplement (AGENTS.md section 5).
//!
//! # Responsibilities
//!
//! - COM/WinRT apartment initialisation and lifetime.
//! - Safe wrappers over raw Windows handles and interfaces.
//! - Process, window and monitor queries used by higher layers.
//!
//! # Not responsible for
//!
//! Capture, encoding or audio policy. This crate exposes capability, it does
//! not decide how that capability is used.
//!
//! # Position in the architecture
//!
//! The lowest layer of the workspace. It depends on no other `clipped-*` crate.
//!
//! # What exists today
//!
//! Windows, monitors and target selection
//! ([issue #10](https://github.com/wildware-uk/clipped/issues/10)), and the
//! process tree a game's audio is scoped to
//! ([issue #25](https://github.com/wildware-uk/clipped/issues/25)) — see
//! [`ProcessTree`], and `docs/audio-routing.md` for what it is for and what it
//! costs. [`process_table`] is the recorder's one read of the process table:
//! `clipped_game_detection`'s process watcher folded its own copy into it
//! ([issue #289](https://github.com/wildware-uk/clipped/issues/289)). The
//! desktop application keeps a second, in its own Cargo workspace and for a
//! question about its own descendants that it cannot ask this crate — see
//! [`process_table`] for why, and
//! `tests/integration/tests/process_table_reads.rs` for the list a third would
//! have to be added to.
//! What the user is looking at, and whether it is something to record
//! ([issue #416](https://github.com/wildware-uk/clipped/issues/416)) — see
//! [`foreground_target`], which is what a press of the "Start or stop
//! recording" hotkey turns into a target, in a process that may have no window
//! open at all.
//! COM and
//! WinRT apartment handling arrives with the first capture backend
//! ([issue #12](https://github.com/wildware-uk/clipped/issues/12)), which is
//! what will need one; there is none here yet, because none of the calls below
//! requires an initialised apartment.
//!
//! # The shape of it: enumerate, then choose
//!
//! ```text
//! enumerate_windows()  ── needs Windows ──▶  Vec<WindowInfo>
//!                                                  │
//!                                    resolve(&windows, &selector)
//!                                    ── pure, and where the rules live ──▶
//!                                                  │
//!                        Ok(&WindowInfo) │ Err(NoMatch │ Ambiguous │ NotCapturable)
//! ```
//!
//! The split is the point. "Which windows are there?" can only be answered by
//! Windows and has almost no logic in it; "which of these did the user mean?"
//! is all logic and needs no desktop at all, so it is a pure function over a
//! list and is unit tested against constructed desktops rather than against
//! whatever happened to be open. Ambiguity is reported rather than guessed at:
//! see [`resolve`].
//!
//! # Handles and ownership
//!
//! [`WindowHandle`] and [`MonitorHandle`] are weak references. Nothing here
//! allocates them, nothing releases them, and there is no close call for
//! either — so both are `Copy`, neither has a destructor, and both can stop
//! naming anything at any moment. Every query that takes one is fallible for
//! that reason: [`WindowsError::WindowGone`] is how a game closing reaches the
//! caller, and [`is_window`] is the cheap check a capture loop makes.
//!
//! The owned handles in the crate are process handles. The one behind
//! [`process_image_path`] is opened, read and closed inside that call and never
//! escapes it; a [`ProcessTree`] holds one per member for as long as it tracks
//! that member, which is not tidiness but the mechanism — an open handle is
//! what stops Windows reusing a process identifier underneath a recording
//! (AGENTS.md section 58).
//!
//! # Threading
//!
//! Everything here is a free function that borrows nothing and shares nothing,
//! so any of it can be called from any thread. Two callers enumerating at once
//! get two independent snapshots. [`ProcessTree`] is the one thing with state:
//! it has a single owner, does its work on the calling thread, and is [`Send`]
//! so that it can be moved to whichever thread that is.
//!
//! What must not be done from a capture thread is any of it. Enumeration walks
//! every top-level window and opens a handle to every process behind them,
//! which takes low milliseconds and blocks on other applications' window
//! procedures; a capture loop should resolve its target before it starts and
//! then use [`window_geometry`] and [`is_window`], both of which are a handful
//! of syscalls (AGENTS.md section 20).
//!
//! # DPI
//!
//! Call [`enable_per_monitor_dpi_awareness`] once at process start. Without it
//! Windows reports scaled sizes to this process for compatibility, and every
//! measurement here is silently wrong on a high-DPI display — the failure mode
//! is a recording at the wrong resolution rather than an error, so it is worth
//! the one line.
//!
//! # Example
//!
//! ```no_run
//! use clipped_windows::{enumerate_windows, resolve, TargetSelector};
//!
//! clipped_windows::enable_per_monitor_dpi_awareness()?;
//!
//! let windows = enumerate_windows()?;
//! let selector = TargetSelector::WindowTitle("Counter-Strike".to_owned());
//!
//! match resolve(&windows, &selector) {
//!     Ok(window) => println!(
//!         "{} ({}x{})",
//!         window.title(),
//!         window.geometry().client_size().width(),
//!         window.geometry().client_size().height(),
//!     ),
//!     // Every candidate, rather than a guess at which one was meant.
//!     Err(error) => eprintln!("{error}"),
//! }
//! # Ok::<(), clipped_windows::WindowsError>(())
//! ```

mod error;
mod foreground;
mod geometry;
mod monitor;
mod process;
mod process_table;
mod process_tree;
mod selection;
mod window;

pub use error::WindowsError;
pub use foreground::{
    foreground_target, ForegroundTarget, NotRecordable, CLIPPED_EXECUTABLES, SHELL_SURFACE_CLASSES,
};
pub use geometry::{scale_percentage, PixelRect, PixelSize, DEFAULT_DPI};
pub use monitor::{
    enumerate_monitors, monitor_for_window, monitor_info, MonitorHandle, MonitorInfo,
};
pub use process::{process_image_name, process_image_path};
pub use process_table::{process_table, ProcessTableEntry};
pub use process_tree::{ProcessTree, TreeChange};
pub use selection::{resolve, ResolveError, TargetSelector};
pub use window::{
    enable_per_monitor_dpi_awareness, enumerate_windows, is_window, window_geometry,
    window_process, DpiAwareness, Exclusion, WindowGeometry, WindowHandle, WindowInfo,
};
