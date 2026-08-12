//! Manual bookmarks: marking a moment while it is being recorded.
//!
//! A bookmark is SPEC.md section 25's four fields — a timestamp, a label, a
//! colour and a duration — placed at an offset **into a recording**, not at a
//! wall-clock time. That is the same shape `clipped-storage`'s `bookmarks` table
//! holds ([issue #55](https://github.com/wildware-uk/clipped/issues/55)) and it
//! is chosen for the same reason: a bookmark is a place in a file, so it has to
//! survive the file being moved, copied, or opened on another machine.
//!
//! The prose version — the reaction-time decision, the tolerance, what happens
//! when the recorder is killed — is `docs/bookmarks.md`.
//!
//! # The decision that shapes everything else: when is a bookmark?
//!
//! **Before the key press, by [`DEFAULT_LEAD`].** A person presses the key
//! *after* the thing they wanted to mark, because they had to see it happen and
//! decide it was worth keeping. A bookmark stamped at the press is therefore
//! reliably late, and a marker that is reliably late is a marker you have to
//! scrub backwards from every single time.
//!
//! So the moment recorded is the recording's position at the press *minus* the
//! lead, clamped at the start of the recording. The lead is carried on every
//! bookmark ([`Bookmark::lead`]) and written into the file, so a timeline can
//! show where the key was actually pressed and nothing has to guess what
//! settings were in force when the bookmark was taken.
//!
//! # Responsibilities
//!
//! - What a bookmark is, and what a caller may put in one ([`Bookmark`],
//!   [`BookmarkRequest`]).
//! - Turning a recording position and a request into a bookmark, and getting it
//!   onto disk before anything else can go wrong ([`BookmarkLog`]).
//! - The documented sidecar format, and reading it back ([`BookmarkFile`]).
//!
//! # Not responsible for
//!
//! Noticing the key press. A global hotkey is `clipped-hotkeys`, the command
//! that carries the press to the recorder is `clipped-ipc`, and the recorder is
//! what joins them to this. Nor for indexing: when M6's library index catches up
//! with bookmarks it reads these files, exactly as it reads session sidecars.
//!
//! # Threading, and why capture is never involved
//!
//! Taking a bookmark touches three things: an atomic load
//! ([`crate::RecordingProgress`]), a `Vec` behind this log's own mutex, and one
//! file write. None of them is on the capture, encode or mux path — the capture
//! thread's only involvement with bookmarks in the whole application is a
//! relaxed atomic store per encoded frame, which is what publishes the position
//! this reads. `crate::record` is not given the log and could not reach it
//! (AGENTS.md section 20).
//!
//! # Surviving a recorder that is killed
//!
//! The file is rewritten, in full, after every bookmark: temporary file, then
//! rename over the real one, which is what makes a half-written file impossible
//! (`std::fs::rename` replaces the destination on Windows). Bookmarks taken
//! before a crash are on disk before the reply that says so is sent, so a
//! recorder that dies has already written down every bookmark it acknowledged
//! (AGENTS.md section 17).

use core::time::Duration;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::automatic::clock;

/// The version of the bookmark sidecar format.
///
/// The file's own, as `crate::automatic::SESSION_SCHEMA_VERSION` is the session
/// sidecar's: it changes when the shape changes and at no other time.
pub const SCHEMA_VERSION: u32 = 1;

/// How far before the key press a bookmark is stamped when nobody has said.
///
/// Five seconds. It is a reaction-time allowance rather than a clip length:
/// seeing something happen, deciding it was worth marking and reaching for a
/// function key is a second or two on its own, and the interesting part almost
/// always started before the moment that made it obvious. Landing a little early
/// costs a few seconds of scrubbing forwards; landing late costs the thing that
/// was being marked.
///
/// A caller that knows better says so per bookmark
/// ([`BookmarkRequest::with_lead`]), which is how a plugin marking an event it
/// detected itself — where there is no human reaction to allow for — asks for
/// [`Duration::ZERO`]. Promoting it to a stored preference waits on the recorder
/// reading the configuration API at all
/// ([issue #61](https://github.com/wildware-uk/clipped/issues/61)); see
/// `docs/bookmarks.md`.
pub const DEFAULT_LEAD: Duration = Duration::from_secs(5);

/// The longest lead a bookmark may be taken with.
///
/// Two minutes. Beyond this the mark has stopped being an allowance for
/// reaction and become a guess about a different part of the recording.
pub const MAXIMUM_LEAD: Duration = Duration::from_secs(120);

/// The longest span a bookmark may cover.
///
/// A bookmark is a moment; the duration is there for the cases where the thing
/// being marked has a length, which SPEC.md section 25 allows for. An hour is
/// past the point where the answer is a clip rather than a mark.
pub const MAXIMUM_DURATION: Duration = Duration::from_secs(60 * 60);

/// The longest a label may be.
///
/// Long enough for a sentence about what happened, short enough that a
/// corrupted or hostile file cannot put an unbounded string into a log line or
/// a menu label.
pub const MAXIMUM_LABEL: usize = 200;

/// The longest a colour may be.
///
/// Nothing here interprets a colour — it is stored as the interface wrote it,
/// exactly as `clipped-storage`'s column documents — so this is a bound rather
/// than a format.
pub const MAXIMUM_COLOUR: usize = 64;

/// Why a bookmark could not be taken.
#[derive(Debug)]
pub enum BookmarkError {
    /// A text field was longer than it may be.
    TooLong {
        /// Which field.
        field: &'static str,
        /// How many characters were given.
        characters: usize,
        /// How many are allowed.
        allowed: usize,
    },
    /// A text field contained a control character, which a file this build
    /// writes and a label a person reads both do without.
    ControlCharacter {
        /// Which field.
        field: &'static str,
    },
    /// A duration or a lead was outside the range that may be asked for.
    OutOfRange {
        /// Which field.
        field: &'static str,
        /// What was asked for.
        value: String,
        /// What is accepted.
        accepted: String,
    },
    /// The bookmark was taken and could not be written down.
    ///
    /// The bookmark is still in the log and will be written by the next one
    /// that succeeds. It is an error rather than a warning because a bookmark
    /// nobody can find after a restart has not really been taken, and the user
    /// is the only one who can do anything about a full or disconnected disk
    /// (AGENTS.md sections 15 and 45).
    NotWritten {
        /// The file it could not be written to.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// A bookmark file could not be read.
    NotReadable {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// A bookmark file was not the shape this build understands.
    Malformed {
        /// The file.
        path: PathBuf,
        /// What was wrong with it.
        detail: String,
    },
}

impl fmt::Display for BookmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong {
                field,
                characters,
                allowed,
            } => write!(
                formatter,
                "a bookmark's {field} is {characters} characters, and at most {allowed} are \
                 allowed"
            ),
            Self::ControlCharacter { field } => write!(
                formatter,
                "a bookmark's {field} contains a control character, which cannot be stored or \
                 displayed"
            ),
            Self::OutOfRange {
                field,
                value,
                accepted,
            } => write!(
                formatter,
                "a bookmark's {field} of {value} is outside the accepted range of {accepted}"
            ),
            Self::NotWritten { path, source } => write!(
                formatter,
                "the bookmark was taken but could not be saved to {}: {source}",
                path.display()
            ),
            Self::NotReadable { path, source } => {
                write!(formatter, "{} could not be read: {source}", path.display())
            }
            Self::Malformed { path, detail } => write!(
                formatter,
                "{} is not a bookmark file this build understands: {detail}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for BookmarkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotWritten { source, .. } | Self::NotReadable { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// What a caller wants marked, before it is placed on a recording.
///
/// Every field is optional except the lead, which always has an answer because
/// [`DEFAULT_LEAD`] is one. Validation happens here rather than at the moment of
/// writing, so a label the file could not hold is refused while the caller can
/// still be told which field was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkRequest {
    label: Option<String>,
    colour: Option<String>,
    duration: Option<Duration>,
    lead: Duration,
}

impl Default for BookmarkRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl BookmarkRequest {
    /// An unlabelled bookmark, taken with [`DEFAULT_LEAD`].
    ///
    /// This is what a bare hotkey press asks for: the whole point of the key is
    /// that pressing it costs nothing and interrupts nothing, so it carries no
    /// text (SPEC.md section 25).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            label: None,
            colour: None,
            duration: None,
            lead: DEFAULT_LEAD,
        }
    }

    /// Sets, or with [`None`] clears, the label.
    ///
    /// # Errors
    ///
    /// [`BookmarkError::TooLong`] beyond [`MAXIMUM_LABEL`] characters, and
    /// [`BookmarkError::ControlCharacter`] for text a file could not carry
    /// back unchanged.
    pub fn with_label(mut self, label: Option<String>) -> Result<Self, BookmarkError> {
        if let Some(label) = &label {
            check_text("label", label, MAXIMUM_LABEL)?;
        }
        self.label = label;
        Ok(self)
    }

    /// Sets, or with [`None`] clears, the colour.
    ///
    /// The value is kept exactly as it was given: this crate does not interpret
    /// a colour, because the notation belongs to whatever draws it.
    ///
    /// # Errors
    ///
    /// As [`Self::with_label`], against [`MAXIMUM_COLOUR`].
    pub fn with_colour(mut self, colour: Option<String>) -> Result<Self, BookmarkError> {
        if let Some(colour) = &colour {
            check_text("colour", colour, MAXIMUM_COLOUR)?;
        }
        self.colour = colour;
        Ok(self)
    }

    /// Sets, or with [`None`] clears, how long the marked moment lasts.
    ///
    /// # Errors
    ///
    /// [`BookmarkError::OutOfRange`] beyond [`MAXIMUM_DURATION`].
    pub fn with_duration(mut self, duration: Option<Duration>) -> Result<Self, BookmarkError> {
        if let Some(duration) = duration {
            if duration > MAXIMUM_DURATION {
                return Err(BookmarkError::OutOfRange {
                    field: "duration",
                    value: format!("{:.3} seconds", duration.as_secs_f64()),
                    accepted: format!("0-{} seconds", MAXIMUM_DURATION.as_secs()),
                });
            }
        }
        self.duration = duration;
        Ok(self)
    }

    /// Sets how far before the press the bookmark is stamped.
    ///
    /// [`Duration::ZERO`] means "exactly where the recording is now", which is
    /// what something other than a human finger should ask for.
    ///
    /// # Errors
    ///
    /// [`BookmarkError::OutOfRange`] beyond [`MAXIMUM_LEAD`].
    pub fn with_lead(mut self, lead: Duration) -> Result<Self, BookmarkError> {
        if lead > MAXIMUM_LEAD {
            return Err(BookmarkError::OutOfRange {
                field: "lead",
                value: format!("{:.3} seconds", lead.as_secs_f64()),
                accepted: format!("0-{} seconds", MAXIMUM_LEAD.as_secs()),
            });
        }
        self.lead = lead;
        Ok(self)
    }

    /// The label, if one was given.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The colour, if one was given.
    #[must_use]
    pub fn colour(&self) -> Option<&str> {
        self.colour.as_deref()
    }

    /// How long the marked moment lasts, if that was said.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// How far before the press this bookmark will be stamped.
    #[must_use]
    pub const fn lead(&self) -> Duration {
        self.lead
    }
}

/// A marked moment in a recording.
///
/// [`Self::at`] is an offset from the start of the recording, in the same units
/// and from the same epoch as the timestamps in the container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    at: Duration,
    lead: Duration,
    label: Option<String>,
    colour: Option<String>,
    duration: Option<Duration>,
    created_at: String,
}

impl Bookmark {
    /// Places `request` against a recording that has reached `position`.
    ///
    /// The lead is subtracted and the result clamped at the start of the
    /// recording: a bookmark taken two seconds into a session lands at zero
    /// rather than being refused, because the moment being marked *is* the
    /// beginning of the recording.
    #[must_use]
    pub fn placed(request: &BookmarkRequest, position: Duration, now: SystemTime) -> Self {
        Self {
            at: position.saturating_sub(request.lead),
            lead: request.lead,
            label: request.label.clone(),
            colour: request.colour.clone(),
            duration: request.duration,
            created_at: clock::rfc3339(now),
        }
    }

    /// How far into the recording the marked moment is.
    #[must_use]
    pub const fn at(&self) -> Duration {
        self.at
    }

    /// How far before the key press this was stamped.
    #[must_use]
    pub const fn lead(&self) -> Duration {
        self.lead
    }

    /// Where the recording was when the key was pressed.
    ///
    /// [`Self::at`] plus the lead, which is the figure a timeline needs in
    /// order to show the press and the mark as two different things.
    #[must_use]
    pub fn pressed_at(&self) -> Duration {
        self.at + self.lead
    }

    /// What the bookmark is called, if anything.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The colour it was given, as the interface wrote it.
    #[must_use]
    pub fn colour(&self) -> Option<&str> {
        self.colour.as_deref()
    }

    /// How long the marked moment lasts, if that was said.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// When it was taken, as an RFC 3339 timestamp carrying its offset.
    #[must_use]
    pub fn created_at(&self) -> &str {
        &self.created_at
    }
}

/// The bookmarks of one recording, and the file they are kept in.
///
/// # Threading
///
/// `Sync`, and meant to be: the recorder answers commands on whichever
/// connection thread they arrived on. Two bookmarks taken at once are appended
/// in the order the mutex grants, and each is on disk before its own call
/// returns.
#[derive(Debug)]
pub struct BookmarkLog {
    path: PathBuf,
    recording: String,
    taken: Mutex<Vec<Bookmark>>,
}

impl BookmarkLog {
    /// The log for the recording being written to `output`.
    ///
    /// The file goes beside the recording and is named after it —
    /// `clipped-….mkv` gets `clipped-….bookmarks.json` — so that the pair
    /// travels together when somebody moves their clips to another drive, and
    /// so that a recording deleted outside Clipped does not leave a bookmark
    /// file nobody can attribute (AGENTS.md section 32).
    ///
    /// It starts empty. A bookmark file belongs to the recording beside it, so
    /// a recording that replaced an existing file replaces its bookmarks with
    /// it rather than inheriting a stranger's.
    #[must_use]
    pub fn for_recording(output: &Path) -> Self {
        Self {
            path: sidecar_path(output),
            recording: output
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            taken: Mutex::new(Vec::new()),
        }
    }

    /// Where the bookmarks are written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Marks the moment `position` refers to, and writes the file.
    ///
    /// `position` is the recording's own position, from
    /// [`crate::RecordingProgress::position`]. `now` is the wall clock, taken by
    /// the caller so that this is testable without one (AGENTS.md section 25).
    ///
    /// The bookmark is returned whether or not the write succeeded — it is in
    /// the log either way — and a write failure is reported as
    /// [`BookmarkError::NotWritten`] so the user is told rather than finding out
    /// when the bookmarks are not there.
    ///
    /// # Errors
    ///
    /// [`BookmarkError::NotWritten`], naming the file and what the filesystem
    /// said.
    pub fn add(
        &self,
        request: &BookmarkRequest,
        position: Duration,
        now: SystemTime,
    ) -> Result<Bookmark, BookmarkError> {
        let bookmark = Bookmark::placed(request, position, now);

        let written = {
            let mut taken = self
                .taken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            taken.push(bookmark.clone());
            write(&self.path, &self.recording, &taken)
        };

        match written {
            Ok(()) => Ok(bookmark),
            Err(source) => Err(BookmarkError::NotWritten {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Every bookmark taken so far, oldest first.
    #[must_use]
    pub fn bookmarks(&self) -> Vec<Bookmark> {
        self.taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many bookmarks have been taken.
    #[must_use]
    pub fn count(&self) -> usize {
        self.taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// A bookmark file, as it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkFile {
    /// The version the file declares.
    pub schema_version: u32,
    /// The name of the recording the bookmarks are in.
    pub recording: String,
    /// The bookmarks, in the order they were taken.
    pub bookmarks: Vec<Bookmark>,
}

impl BookmarkFile {
    /// Reads the bookmarks of the recording at `output`.
    ///
    /// # Errors
    ///
    /// [`BookmarkError::NotReadable`] if the file could not be opened —
    /// including when there is none, which is what a recording with no
    /// bookmarks looks like — and [`BookmarkError::Malformed`] if it is not
    /// this format.
    pub fn for_recording(output: &Path) -> Result<Self, BookmarkError> {
        Self::read(&sidecar_path(output))
    }

    /// Reads a bookmark file.
    ///
    /// Keys this build does not recognise are ignored rather than refused, so a
    /// file a later Clipped wrote is still readable by this one (AGENTS.md
    /// section 43). `docs/bookmarks.md` states that as the rule for every
    /// reader.
    ///
    /// # Errors
    ///
    /// As [`Self::for_recording`].
    pub fn read(path: &Path) -> Result<Self, BookmarkError> {
        let text = fs::read_to_string(path).map_err(|source| BookmarkError::NotReadable {
            path: path.to_path_buf(),
            source,
        })?;
        let file: StoredFile =
            serde_json::from_str(&text).map_err(|error| BookmarkError::Malformed {
                path: path.to_path_buf(),
                detail: error.to_string(),
            })?;

        Ok(Self {
            schema_version: file.schema_version,
            recording: file.recording,
            bookmarks: file
                .bookmarks
                .into_iter()
                .map(StoredBookmark::into)
                .collect(),
        })
    }
}

/// Where a recording's bookmarks go.
fn sidecar_path(output: &Path) -> PathBuf {
    let mut name = output
        .file_stem()
        .map_or_else(|| OsString::from("clipped"), std::ffi::OsStr::to_os_string);
    name.push(".bookmarks.json");
    output.with_file_name(name)
}

/// Writes the whole set, atomically.
///
/// Whole rather than appended: a rewritten file is one shape to read and one
/// shape to test, and the set is bounded by how fast a person can press a key.
/// Temporary file then rename, for the reason the session sidecar gives — a
/// recorder killed halfway through a write must not leave a truncated file where
/// the bookmarks used to be.
fn write(path: &Path, recording: &str, bookmarks: &[Bookmark]) -> io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }

    let file = StoredFile {
        schema_version: SCHEMA_VERSION,
        recording: recording.to_owned(),
        bookmarks: bookmarks.iter().map(StoredBookmark::of).collect(),
    };
    let json = serde_json::to_vec_pretty(&file)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let temporary = temporary_path(path);
    fs::write(&temporary, &json)?;
    fs::rename(&temporary, path)
}

/// The name the file is written under before it is renamed into place.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || OsString::from("bookmarks.json"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".tmp");
    path.with_file_name(name)
}

/// The file, as JSON.
///
/// A shape of its own rather than `Serialize` on [`Bookmark`], for the reason
/// the session sidecar gives: the format is visible in one place and a change to
/// a public type does not alter it by accident (AGENTS.md section 43).
#[derive(Debug, Serialize, Deserialize)]
struct StoredFile {
    schema_version: u32,
    recording: String,
    bookmarks: Vec<StoredBookmark>,
}

/// One bookmark, in the field names `clipped-storage`'s `bookmarks` table uses.
///
/// Seconds rather than nanoseconds, because that is what the table's
/// `at_seconds` and `duration_seconds` columns hold and a value that changed
/// units on its way into the index would be a bug nobody could see.
#[derive(Debug, Serialize, Deserialize)]
struct StoredBookmark {
    at_seconds: f64,
    lead_seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    colour: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<f64>,
    created_at: String,
}

impl StoredBookmark {
    fn of(bookmark: &Bookmark) -> Self {
        Self {
            at_seconds: bookmark.at.as_secs_f64(),
            lead_seconds: bookmark.lead.as_secs_f64(),
            label: bookmark.label.clone(),
            colour: bookmark.colour.clone(),
            duration_seconds: bookmark.duration.map(|span| span.as_secs_f64()),
            created_at: bookmark.created_at.clone(),
        }
    }
}

impl From<StoredBookmark> for Bookmark {
    fn from(stored: StoredBookmark) -> Self {
        Self {
            at: seconds(stored.at_seconds),
            lead: seconds(stored.lead_seconds),
            label: stored.label,
            colour: stored.colour,
            duration: stored.duration_seconds.map(seconds),
            created_at: stored.created_at,
        }
    }
}

/// A duration from a figure in a file, which may be anything at all.
///
/// A negative or non-finite value becomes zero rather than a panic:
/// `Duration::from_secs_f64` panics on both, and a file somebody edited by hand
/// must not be able to bring down a recorder (AGENTS.md section 16).
fn seconds(value: f64) -> Duration {
    if value.is_finite() && value > 0.0 {
        Duration::try_from_secs_f64(value).unwrap_or(Duration::ZERO)
    } else {
        Duration::ZERO
    }
}

/// Whether a text field is one the file can hold and give back unchanged.
fn check_text(field: &'static str, value: &str, allowed: usize) -> Result<(), BookmarkError> {
    let characters = value.chars().count();
    if characters > allowed {
        return Err(BookmarkError::TooLong {
            field,
            characters,
            allowed,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(BookmarkError::ControlCharacter { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
