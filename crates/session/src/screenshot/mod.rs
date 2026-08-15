//! Screenshots: a still image of the game, taken on a hotkey.
//!
//! SPEC.md section 26 and
//! [issue #67](https://github.com/wildware-uk/clipped/issues/67). The prose
//! version — the decisions, what each path costs, and what is deliberately not
//! here — is `docs/screenshots.md`.
//!
//! # The decision everything else follows from
//!
//! **A screenshot is a frame the recording already had.**
//!
//! The capture backends already deliver every frame of the game to this crate,
//! several dozen times a second. Opening a second capture to take a picture of
//! the same window would mean a second frame pool, a second copy of every
//! frame the compositor produces, and — on Desktop Duplication — competing for
//! a resource Windows only hands to one client at a time. So when a recording
//! is running, a screenshot costs one texture copy on a frame that had already
//! been captured, and no capture at all (AGENTS.md sections 18 and 55).
//!
//! When nothing is being recorded there is no such frame, and rather than
//! refuse — a screenshot key that only works while recording is a key nobody
//! trusts — [`capture`](capture_still) opens a capture, takes one frame and
//! shuts it down. That is the expensive path and `docs/screenshots.md` says
//! what it costs.
//!
//! # Responsibilities
//!
//! - What a screenshot is saved as and where it goes: [`ScreenshotFormat`],
//!   [`ScreenshotSettings`], [`default_directory`], [`file_name`].
//! - Turning a captured frame into a file: [`write`], and [`Screenshot`], which
//!   is what was written.
//! - Asking a running recording for a frame without interrupting it:
//!   [`ScreenshotRequests`].
//! - Taking one when nothing is recording: [`capture_still`].
//!
//! # Not responsible for
//!
//! Noticing the key press — that is `clipped-hotkeys` and `clipped-ipc`, joined
//! by the recorder — and indexing the result. A screenshot is a file in the
//! user's own directory with a name that sorts beside the recordings of the
//! same game; putting it in the library index and on a timeline needs a
//! `screenshots` table that `clipped-storage` does not have yet
//! ([issue #334](https://github.com/wildware-uk/clipped/issues/334)).
//!
//! # Threading
//!
//! The split is the whole design, and it is AGENTS.md section 20's rule applied
//! literally: **the capture thread copies, and nothing else.**
//!
//! ```text
//! capture thread                     the thread that asked
//! ──────────────                     ─────────────────────
//! acquire a frame                    request()  ─┐
//! a request is waiting? ◀─────────────────────────┘
//! begin the GPU copy                 (waiting)
//! ... carry on recording ...
//! poll the copy                      (waiting)
//! hand over the pixels ──────────────▶ encode
//!                                      write the file
//! ```
//!
//! Encoding a 4K PNG is tens of milliseconds and writing it touches a disk.
//! Neither happens on the capture thread; both happen on whichever thread the
//! `take_screenshot` command arrived on, after the pixels have left the GPU.

use core::fmt;
use core::time::Duration;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clipped_capture::StillError;

use crate::automatic::UNATTRIBUTED;

mod encode;
mod request;

#[cfg(windows)]
mod capture;

#[cfg(test)]
mod tests;

pub use request::{ScreenshotRequests, ServedStill, DEFAULT_TIMEOUT};

/// One frame's pixels in system memory, re-exported from `clipped-capture`.
///
/// Everything a caller does with a screenshot goes through this module, and a
/// caller should not have to take a dependency on the capture crate to name the
/// thing it hands back (AGENTS.md section 44). It is the same type, not a
/// wrapper: `clipped_capture::StillFrame` is what the copier produces and what
/// [`write`] consumes.
pub use clipped_capture::StillFrame;

#[cfg(windows)]
pub use capture::{capture_still, capture_still_within, FIRST_FRAME_TIMEOUT};

/// The directory Clipped writes screenshots into, under the user's own.
///
/// `Clipped`, under the pictures folder, and **not** under the recordings
/// directory. Two reasons, and the second is the one that would bite later:
///
/// - Windows files images in Pictures. Explorer, the Photos app and every
///   upload dialog start there, and a screenshot is something a person goes and
///   finds, unlike a recording, which the application shows them.
/// - Storage accounting requires that no root contains another
///   (`docs/storage-management.md`), and `Screenshots` is one of its categories.
///   A screenshots directory nested inside the recordings directory would have
///   its bytes counted twice or excluded, depending on which root won.
const DIRECTORY: &str = "Clipped";

/// How many names are tried before a screenshot gives up on a second.
///
/// Screenshots are named to the second, so two taken in the same second collide
/// and the later one takes `-2`, `-3` and so on. Nobody presses a key ninety-nine
/// times in a second; a loop with no bound, on the other hand, is what a
/// directory full of unwritable files turns into.
const MAXIMUM_NAMES: u32 = 99;

/// The format a screenshot is saved in.
///
/// The three SPEC.md section 26 asks for. All three are encoded by the FFmpeg
/// build Clipped already links (`docs/ffmpeg.md`), so none of them is a new
/// dependency (AGENTS.md section 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScreenshotFormat {
    /// PNG. Lossless, and what Clipped saves unless told otherwise.
    ///
    /// The default because a screenshot is kept, cropped, annotated and posted,
    /// and every one of those is done to the file that exists rather than to
    /// the frame it came from. A lossy default trades bytes — which nobody is
    /// short of for one image — against quality that cannot be recovered, and
    /// it is the trade a user only notices after the frame is gone. Thumbnails
    /// make the opposite choice for the opposite reason
    /// (`docs/thumbnails.md`): there are tens of thousands of them and nobody
    /// keeps one.
    #[default]
    Png,
    /// JPEG. Lossy, roughly a tenth of the bytes, and the format every
    /// application on Windows opens.
    Jpeg,
    /// WebP, encoded losslessly.
    ///
    /// Smaller than PNG for the same pixels. "Where practical" in SPEC.md
    /// section 26 is doing real work: it needs `libwebp` in the FFmpeg build,
    /// and a build without it reports
    /// [`ScreenshotError::FormatUnavailable`](ScreenshotError::FormatUnavailable)
    /// naming what is missing rather than silently saving a PNG with the wrong
    /// extension.
    WebP,
}

impl ScreenshotFormat {
    /// Every format, in the order a settings screen should list them.
    pub const ALL: [Self; 3] = [Self::Png, Self::Jpeg, Self::WebP];

    /// The format's stable name, for configuration files and logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::WebP => "webp",
        }
    }

    /// The extension a file of this format is given, without the dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
        }
    }

    /// The format with that [`name`](Self::name), if it is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.name() == name)
    }

    /// Whether this build can actually encode this format.
    ///
    /// The answer for [`Self::WebP`] depends on how FFmpeg was built, so it is
    /// a question asked of the linked library rather than a constant. A
    /// settings screen that offers a format this returns `false` for is
    /// offering a control that does nothing (AGENTS.md section 27).
    #[must_use]
    pub fn is_available(self) -> bool {
        encode::is_available(self)
    }
}

impl fmt::Display for ScreenshotFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::WebP => "lossless WebP",
        })
    }
}

/// The JPEG quantiser scale a screenshot is encoded at unless a caller says
/// otherwise.
///
/// MJPEG's scale runs from 1 (largest, best) to 31 (smallest, worst). Two,
/// where `docs/thumbnails.md` uses four, and the difference is deliberate: a
/// thumbnail is 640 pixels wide and disposable, a screenshot is full resolution
/// and kept. Two is visually indistinguishable from the frame at 100% zoom on
/// game content.
pub const DEFAULT_JPEG_QUALITY: u32 = 2;

/// The best JPEG quantiser scale MJPEG accepts.
pub const BEST_JPEG_QUALITY: u32 = 1;

/// The worst JPEG quantiser scale MJPEG accepts.
pub const WORST_JPEG_QUALITY: u32 = 31;

/// Where screenshots go and what they are saved as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotSettings {
    directory: PathBuf,
    format: ScreenshotFormat,
    jpeg_quality: u32,
}

impl ScreenshotSettings {
    /// Screenshots in `directory`, as PNG.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            format: ScreenshotFormat::Png,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
        }
    }

    /// The same, in `format`.
    #[must_use]
    pub fn with_format(mut self, format: ScreenshotFormat) -> Self {
        self.format = format;
        self
    }

    /// The same, at that JPEG quantiser scale.
    ///
    /// Clamped to [`BEST_JPEG_QUALITY`]..=[`WORST_JPEG_QUALITY`] rather than
    /// refused: a value outside the range is a setting somebody typed, and the
    /// nearest legal one is a better answer than a failed screenshot.
    #[must_use]
    pub fn with_jpeg_quality(mut self, quality: u32) -> Self {
        self.jpeg_quality = quality.clamp(BEST_JPEG_QUALITY, WORST_JPEG_QUALITY);
        self
    }

    /// Where the files go.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// What they are saved as.
    #[must_use]
    pub const fn format(&self) -> ScreenshotFormat {
        self.format
    }

    /// The JPEG quantiser scale, which is ignored by every other format.
    #[must_use]
    pub const fn jpeg_quality(&self) -> u32 {
        self.jpeg_quality
    }
}

/// The directory screenshots go in when nobody has configured one.
///
/// `%USERPROFILE%\Pictures\Clipped` on Windows, and the equivalent under
/// `$HOME` elsewhere so that the tests of everything above this line run on a
/// contributor's other machine. [`None`] only when the environment has no home
/// directory at all, which is a broken environment rather than something to
/// guess around — the same answer `clipped-recorder`'s
/// `default_output_directory` gives for recordings.
#[must_use]
pub fn default_directory() -> Option<PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    }?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join("Pictures").join(DIRECTORY))
}

/// The name a screenshot of `game` taken at `when` is saved under.
///
/// `clipped-<game>-<yyyymmdd>-<hhmmss>.<extension>`, which is deliberately the
/// same shape as a session's own files
/// (`clipped-<game>-<yyyymmdd>-<hhmmss>.mkv`, `crate::automatic::SessionId`):
/// sortable, legible in a directory listing, and free of every character
/// Windows forbids in a file name. A person looking at a screenshot and a
/// recording of the same evening can tell they belong together without opening
/// either.
///
/// `game` is the game's catalogue identifier. An empty one — a screenshot taken
/// with no session, or of a process the catalogue does not claim — becomes
/// `unattributed`, which is the word a session with no attributable game is
/// already filed under.
///
/// `index` above one appends `-<index>`, which is what a second screenshot in
/// the same second gets.
#[must_use]
pub fn file_name(game: &str, when: SystemTime, format: ScreenshotFormat, index: u32) -> String {
    let game = if game.is_empty() { UNATTRIBUTED } else { game };
    let stamp = crate::automatic::clock::stamp(when);
    let extension = format.extension();
    if index <= 1 {
        format!("clipped-{game}-{stamp}.{extension}")
    } else {
        format!("clipped-{game}-{stamp}-{index}.{extension}")
    }
}

/// A screenshot that has been written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    path: PathBuf,
    format: ScreenshotFormat,
    width: u32,
    height: u32,
    bytes: u64,
    taken_at: SystemTime,
    position: Option<Duration>,
}

impl Screenshot {
    /// The file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What it was saved as.
    #[must_use]
    pub const fn format(&self) -> ScreenshotFormat {
        self.format
    }

    /// Its width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Its height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// How large the file is.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// When it was taken, on the wall clock.
    #[must_use]
    pub const fn taken_at(&self) -> SystemTime {
        self.taken_at
    }

    /// How far into the recording it was taken, when one was running.
    ///
    /// [`None`] for a screenshot taken with nothing recording, which is the
    /// honest answer: there is no timeline to be a position on. When it is
    /// [`Some`] it is the recording's own media clock — the same units the
    /// container's timestamps are in — so a timeline can put a marker exactly
    /// where the picture was taken rather than where a wall clock said it was
    /// (`crate::RecordingProgress` explains why those differ).
    #[must_use]
    pub const fn position(&self) -> Option<Duration> {
        self.position
    }
}

/// Encodes `still` and writes it into `settings.directory`.
///
/// The file is named [`file_name`] with `game` and `taken_at`, and an existing
/// file is never replaced: the index climbs until a free name is found, so two
/// screenshots in the same second are two files (AGENTS.md section 56).
///
/// The directory is created if it does not exist. The bytes are written to a
/// temporary file in the same directory and renamed into place, so a recorder
/// killed mid-write leaves no truncated image where a screenshot should be —
/// the same rule `crate::bookmarks` follows for the same reason (AGENTS.md
/// section 17).
///
/// `position` is where the frame sat on a running recording's timeline, and is
/// [`None`] when nothing was recording.
///
/// # Errors
///
/// [`ScreenshotError`], which names what failed and, where a person can act on
/// it, the file it failed on: an unreadable or full destination
/// ([`ScreenshotError::NotWritten`]), a format this FFmpeg build cannot encode
/// ([`ScreenshotError::FormatUnavailable`]), or a frame in a pixel layout this
/// build cannot read ([`ScreenshotError::Encode`]).
pub fn write(
    still: &StillFrame,
    settings: &ScreenshotSettings,
    game: &str,
    taken_at: SystemTime,
    position: Option<Duration>,
) -> Result<Screenshot, ScreenshotError> {
    let format = settings.format();
    let encoded = encode::encode(still, format, settings.jpeg_quality())?;

    fs::create_dir_all(settings.directory()).map_err(|source| {
        ScreenshotError::DirectoryNotCreated {
            directory: settings.directory().to_path_buf(),
            source,
        }
    })?;

    let path = free_path(settings.directory(), game, taken_at, format)?;
    write_atomically(&path, &encoded).map_err(|source| ScreenshotError::NotWritten {
        path: path.clone(),
        source,
    })?;

    let size = still.size();
    let screenshot = Screenshot {
        path,
        format,
        width: size.width(),
        height: size.height(),
        bytes: encoded.len() as u64,
        taken_at,
        position,
    };

    tracing::info!(
        screenshot = %clipped_logging::RedactedPath::new(screenshot.path()),
        format = format.name(),
        width = screenshot.width(),
        height = screenshot.height(),
        bytes = screenshot.bytes(),
        position_ms = position.map(|position| position.as_millis()),
        "screenshot saved"
    );

    Ok(screenshot)
}

/// The first name in `directory` that no file is using.
fn free_path(
    directory: &Path,
    game: &str,
    taken_at: SystemTime,
    format: ScreenshotFormat,
) -> Result<PathBuf, ScreenshotError> {
    for index in 1..=MAXIMUM_NAMES {
        let candidate = directory.join(file_name(game, taken_at, format, index));
        // `try_exists` rather than `exists`: a directory that cannot be read is
        // a different answer from a name that is free, and treating it as free
        // would mean the write below is the first thing to notice.
        match candidate.try_exists() {
            Ok(false) => return Ok(candidate),
            Ok(true) => {}
            Err(source) => {
                return Err(ScreenshotError::NotWritten {
                    path: candidate,
                    source,
                })
            }
        }
    }

    Err(ScreenshotError::NoFreeName {
        directory: directory.to_path_buf(),
        attempts: MAXIMUM_NAMES,
    })
}

/// Writes `bytes` to `path` through a temporary file in the same directory.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    clipped_logging::write_atomically(path, |temporary| io::Write::write_all(temporary, bytes))
}

/// Why a screenshot could not be taken or saved.
#[derive(Debug)]
#[non_exhaustive]
pub enum ScreenshotError {
    /// The frame could not be copied out of the GPU.
    Copy(StillError),
    /// This FFmpeg build has no encoder for the requested format.
    ///
    /// The real case is lossless WebP, which SPEC.md section 26 asks for "where
    /// practical" and which needs `libwebp` compiled into FFmpeg. Saying so is
    /// what stops a build quietly writing a PNG into a `.webp`.
    FormatUnavailable {
        /// The format that was asked for.
        format: ScreenshotFormat,
    },
    /// The picture could not be encoded.
    Encode {
        /// The format that was being written.
        format: ScreenshotFormat,
        /// What went wrong, in FFmpeg's words where it had any.
        detail: String,
    },
    /// The directory screenshots go in could not be created.
    DirectoryNotCreated {
        /// The directory.
        directory: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// The file could not be written.
    NotWritten {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// Every name for this second is already taken.
    NoFreeName {
        /// The directory that is full of them.
        directory: PathBuf,
        /// How many were tried.
        attempts: u32,
    },
    /// Nothing produced a frame in the time allowed.
    ///
    /// A window that is not drawing produces no frames at all, and a recording
    /// that ended between the request and the next frame has none left to give.
    NoFrame {
        /// How long was waited.
        waited: Duration,
    },
    /// The recording tried to hand a frame over and could not.
    ///
    /// Separate from [`Self::Copy`] because it crossed a thread boundary: the
    /// capture loop's error was rendered where it happened, so what survives is
    /// its words rather than its type.
    NotCaptured {
        /// What the recording said.
        detail: String,
    },
    /// A capture opened for a screenshot could not be started.
    Capture(crate::SessionError),
}

impl fmt::Display for ScreenshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Copy(error) => write!(formatter, "the frame could not be copied: {error}"),
            Self::FormatUnavailable { format } => write!(
                formatter,
                "this build of Clipped cannot write {format}; its FFmpeg was built without that \
                 encoder"
            ),
            Self::Encode { format, detail } => {
                write!(
                    formatter,
                    "the screenshot could not be encoded as {format}: {detail}"
                )
            }
            Self::DirectoryNotCreated { directory, source } => write!(
                formatter,
                "the screenshot folder {} could not be created: {source}",
                directory.display()
            ),
            Self::NotWritten { path, source } => write!(
                formatter,
                "the screenshot could not be saved to {}: {source}",
                path.display()
            ),
            Self::NoFreeName {
                directory,
                attempts,
            } => write!(
                formatter,
                "{attempts} screenshots in {} already have this second's name",
                directory.display()
            ),
            Self::NoFrame { waited } => write!(
                formatter,
                "nothing produced a frame to photograph within {} ms; a window that is not \
                 drawing produces none at all",
                waited.as_millis()
            ),
            Self::NotCaptured { detail } => {
                write!(
                    formatter,
                    "the recording could not hand over a frame: {detail}"
                )
            }
            Self::Capture(error) => {
                write!(
                    formatter,
                    "a capture could not be opened for a screenshot: {error}"
                )
            }
        }
    }
}

impl std::error::Error for ScreenshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Copy(error) => Some(error),
            Self::DirectoryNotCreated { source, .. } | Self::NotWritten { source, .. } => {
                Some(source)
            }
            Self::Capture(error) => Some(error),
            Self::FormatUnavailable { .. }
            | Self::Encode { .. }
            | Self::NoFreeName { .. }
            | Self::NoFrame { .. }
            | Self::NotCaptured { .. } => None,
        }
    }
}

impl From<StillError> for ScreenshotError {
    fn from(error: StillError) -> Self {
        Self::Copy(error)
    }
}

impl From<crate::SessionError> for ScreenshotError {
    fn from(error: crate::SessionError) -> Self {
        Self::Capture(error)
    }
}
