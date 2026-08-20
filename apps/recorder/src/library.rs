//! Answering the desktop application's questions about the recording library.
//!
//! The window cannot read `library.db`. It has no file-system permission for
//! the file and it may not link `clipped-library`
//! ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md),
//! `tests/integration/tests/workspace_layering.rs`), so the process that owns
//! the database answers for it — which is this one
//! ([issue #301](https://github.com/wildware-uk/clipped/issues/301)).
//!
//! Two halves, and they are deliberately separate:
//!
//! - [`LibraryReader`] answers questions. It is the join between two
//!   vocabularies and nothing else — `clipped_library::index` says what a row
//!   is, `clipped_ipc::library` says what goes on the wire, and neither knows
//!   about the other. Keeping the conversion here is what lets the protocol
//!   crate stay a leaf that the window can link (`crates/ipc/src/lib.rs`) and
//!   the library crate stay ignorant of the protocol.
//! - [`LibraryIndexer`] keeps there being anything to answer *with*. Before
//!   [issue #402](https://github.com/wildware-uk/clipped/issues/402) there was
//!   no such half at all: `reconcile` had no caller anywhere in the product, so
//!   every recording anybody made was on disk and in no index.
//!
//! They own a connection each, for the reason [`LibraryIndexer`] gives: a page
//! of the library must not wait behind a reconciliation.
//!
//! # Opening is lazy, and a failure is not permanent
//!
//! The database is opened on the first question and kept afterwards. It is not
//! opened during `serve` startup, for two reasons: a recorder that cannot open
//! its library must still record — that is the thing it exists to do — and the
//! most likely reason it cannot is a drive that is not plugged in yet, which
//! stops being true without restarting anything. So an attempt that fails is
//! reported and *retried* on the next question rather than remembered.
//!
//! # The failure is never an empty library
//!
//! A read that could not happen answers [`ErrorCode::LibraryUnavailable`] with
//! the reason. Answering an empty page would have a window drawing "nothing
//! recorded yet" over a database that is locked, corrupt, from a newer build or
//! on a disconnected drive (AGENTS.md sections 27 and 54).
//!
//! # Threading
//!
//! A [`CommandHandler`](clipped_ipc::CommandHandler) is called from several
//! connection threads at once, and an SQLite connection is `Send` and not
//! `Sync`, so the connection sits behind a mutex and library reads are
//! serialised against each other. They are not serialised against anything
//! else: a page is a bounded read of local data, and nothing on a capture,
//! encoder or recording thread ever waits on this lock.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::SystemTime;

use clipped_edit::{EditDocument, EditDocumentError, RecordingId, SourceSpan, SourceTime};
use clipped_ipc::{
    CategoryUsage, ClipDocument, ClipDocumentSaved, ErrorCode, FavouriteMark, LibraryClip,
    LibraryClipDocument, LibraryEventLane, LibraryEventMark, LibraryEvents, LibraryGame,
    LibraryRecording, LibrarySession, LibrarySessionPage, LibrarySessions, LockMark, Preview,
    ProtectedGroup, ProtocolError, RecordingList, RestoreFromTrash, RestoredItem, SaveClipDocument,
    SetFavourite, SetLock, StorageLimits, StorageRecording, StorageReport, TrashEmptied,
    TrashListing, TrashedItem, MAX_FRAME_BYTES, MOST_LISTED,
};
use clipped_library::favourites::Favourite;
use clipped_library::index::{
    cursor_of, game_summaries, list_sessions, reconcile, GameSummary, IndexControl, IndexReport,
    IndexSettings, IndexedClip, IndexedRecording, IndexedSession, SessionListing,
};
use clipped_library::locks::Lockable;
use clipped_library::search::Query;
use clipped_library::thumbnail::{ServiceOptions, ThumbnailCache, ThumbnailService};
use clipped_storage::Database;
use clipped_waveform::{ServiceOptions as WaveformOptions, WaveformCache, WaveformService};

/// The file the library index lives in, under Clipped's per-user directory.
///
/// `pub(crate)` rather than private: `crate::recover`'s `--discard` indexes a
/// recovered fragment before it sends it to the trash (issue #451), and it
/// opens the same database this module does. Naming a second `"library.db"`
/// literal there would be the two answers to one question AGENTS.md
/// section 55 warns about.
pub(crate) const LIBRARY_FILE: &str = "library.db";

/// The recording library, as this process reads it for the window.
#[derive(Debug)]
pub struct LibraryReader {
    /// Where the index is, or [`None`] when this machine describes no per-user
    /// directory at all — which on Windows means `%LOCALAPPDATA%` is unset.
    path: Option<PathBuf>,
    database: Mutex<Option<Database>>,
    /// Where deleted media waits, so that a trash listing can say where the
    /// files went rather than only that they went.
    ///
    /// Listing the trash reads the *index* — the directory is not consulted —
    /// so this is here to be reported rather than to be walked.
    trash_directory: Option<PathBuf>,
}

/// The item a request names, or a refusal saying what a kind may be.
fn trash_item(kind: &str, id: i64) -> Result<clipped_library::trash::TrashItem, ProtocolError> {
    match kind {
        "recording" => Ok(clipped_library::trash::TrashItem::recording(id)),
        "clip" => Ok(clipped_library::trash::TrashItem::clip(id)),
        other => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!("`{other}` is not something the trash holds; it holds `recording` and `clip`"),
        )),
    }
}

/// What a `set_favourite` request names, or why it names nothing.
///
/// The target takes two fields because the schema does — a sitting is keyed by
/// text and a recording or clip by an integer — and exactly one of them is read.
/// The half that is not read is *not* checked: a window echoing a whole row back
/// may well fill in both, and refusing that would be refusing a request whose
/// meaning is unambiguous.
///
/// What is refused is a target that names nothing. A `session` with no
/// identifier and a `recording` with an identifier of zero are both requests to
/// favourite something that does not exist, and answering "done" to one would be
/// a window drawing a star it will lose on the next read.
fn favourite_target(request: &SetFavourite) -> Result<Favourite, ProtocolError> {
    let missing = |what: &str| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "a `{}` is favourited by its {what}, and this request carries none",
                request.kind
            ),
        )
    };

    match request.kind.as_str() {
        "session" if request.session_id.is_empty() => Err(missing("session_id")),
        "session" => Ok(Favourite::Session(request.session_id.clone())),
        "recording" | "clip" if request.id == 0 => Err(missing("id")),
        "recording" => Ok(Favourite::Recording(request.id)),
        "clip" => Ok(Favourite::Clip(request.id)),
        other => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "`{other}` is not something that can be favourited; Clipped favourites \
                 `session`, `recording` and `clip`"
            ),
        )),
    }
}

/// What a `set_lock` request names, or why it names nothing.
///
/// The same rules as [`favourite_target`], over a shorter vocabulary: a clip
/// cannot be locked. Asking to lock one is refused rather than ignored, because
/// a window told "done" would draw a padlock against a clip that automatic
/// cleanup does not consult (`clipped_library::locks`).
fn lock_target(request: &SetLock) -> Result<Lockable, ProtocolError> {
    let missing = |what: &str| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "a `{}` is locked by its {what}, and this request carries none",
                request.kind
            ),
        )
    };

    match request.kind.as_str() {
        "session" if request.session_id.is_empty() => Err(missing("session_id")),
        "session" => Ok(Lockable::Session(request.session_id.clone())),
        "recording" if request.id == 0 => Err(missing("id")),
        "recording" => Ok(Lockable::Recording(request.id)),
        "clip" => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            "a clip cannot be locked: automatic cleanup deletes recordings, and a clip already \
             keeps the recording it was cut from. Lock that recording instead",
        )),
        other => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "`{other}` is not something that can be locked; Clipped locks `session` and \
                 `recording`"
            ),
        )),
    }
}

/// Why a restore did not happen.
fn restore_refused(error: clipped_library::trash::TrashError) -> ProtocolError {
    // A named item the trash does not hold is the caller's mistake and a
    // different thing from a library that could not be read, which is a
    // machine's problem. Saying so is what lets a window offer the right next
    // step (AGENTS.md section 15).
    let code = match &error {
        clipped_library::trash::TrashError::NoSuchItem { .. }
        | clipped_library::trash::TrashError::NotInTrash { .. } => ErrorCode::InvalidParameters,
        _ => ErrorCode::LibraryUnavailable,
    };
    ProtocolError::new(code, error.to_string())
}

/// Why the trash was not emptied.
fn empty_refused(error: clipped_library::trash::TrashError) -> ProtocolError {
    let code = match &error {
        // The confirmation did not match what is there. The message names both
        // counts, so a window can say what changed rather than only that
        // something did.
        clipped_library::trash::TrashError::Changed { .. } => ErrorCode::InvalidParameters,
        _ => ErrorCode::LibraryUnavailable,
    };
    ProtocolError::new(code, error.to_string())
}

/// One recording a sweep considered, as the window is told about it.
fn storage_recording(
    candidate: &clipped_library::accounting::cleanup::Candidate,
) -> StorageRecording {
    StorageRecording {
        recording_id: candidate.item.id,
        path: candidate.path.to_string_lossy().into_owned(),
        size_bytes: candidate.size_bytes,
        started_at: candidate.started_at.clone(),
        // The recorder's own sentence, which already reads as a reason: "it is
        // a favourite", "the sitting it belongs to is locked". A window keeping
        // its own table of these would say nothing at all for a rule a newer
        // recorder had added (`clipped_library::accounting::cleanup`).
        protected_because: candidate.protection.map(|why| why.to_string()),
    }
}

/// Some recordings and the whole set they came from, bounded to what a frame
/// and a screen can hold.
///
/// The totals are of everything and the rows are the first [`MOST_LISTED`], so a
/// window can say how many more there are. A truncated list carrying no total
/// would read as the whole answer, which for "what a limit would delete" is the
/// worst thing on this screen to be wrong about.
fn recording_list(candidates: &[clipped_library::accounting::cleanup::Candidate]) -> RecordingList {
    RecordingList {
        total: candidates.len() as u64,
        total_bytes: candidates
            .iter()
            .map(|candidate| candidate.size_bytes)
            .sum(),
        recordings: candidates
            .iter()
            .take(MOST_LISTED)
            .map(storage_recording)
            .collect(),
    }
}

/// The name a person reads for one protection rule.
///
/// The recorder's vocabulary rather than the window's, for the reason
/// `HotkeyBinding::label` is the recorder's: the rules live in
/// `clipped_library::accounting::cleanup`, and a window with its own table of
/// them would draw nothing for a rule added after it was built.
///
/// `Display` is not reused here. It writes the *reason one row is kept* - "it is
/// a favourite" - which reads correctly beside a single recording and not at all
/// as the heading of a group of them.
fn protection_label(protection: clipped_library::accounting::cleanup::Protection) -> &'static str {
    use clipped_library::accounting::cleanup::Protection;

    match protection {
        Protection::Locked => "Locked",
        Protection::LockedSession => "In a locked sitting",
        Protection::Favourite => "Favourites",
        Protection::FavouriteSession => "In a favourited sitting",
        Protection::SourceOfClips { .. } => "Clips were cut from them",
        Protection::AlreadyDeleted => "Already in the trash",
        Protection::Missing => "Their file is not on disk",
    }
}

/// What each protection rule is holding, in the order the rules were met.
///
/// Grouped rather than listed. Nobody wants to scroll ten thousand protected
/// recordings; what SPEC.md section 27's "never automatically delete" list needs
/// in order to be believable is a count and a size against each rule, so that
/// the promise is measured state rather than a sentence on a screen (AGENTS.md
/// section 27).
///
/// A rule holding nothing is left out. "Favourites: 0 recordings" on a machine
/// where nothing is favourited is a row about a feature rather than about the
/// disk, and a window says the rules are protecting nothing yet in one sentence
/// instead.
fn protected_groups(
    protected: &[clipped_library::accounting::cleanup::Candidate],
) -> Vec<ProtectedGroup> {
    let mut groups: Vec<ProtectedGroup> = Vec::new();

    for candidate in protected {
        let Some(protection) = candidate.protection else {
            continue;
        };
        let label = protection_label(protection);
        if let Some(group) = groups.iter_mut().find(|group| group.label == label) {
            group.recordings += 1;
            group.bytes += candidate.size_bytes;
        } else {
            groups.push(ProtectedGroup {
                label: label.to_owned(),
                recordings: 1,
                bytes: candidate.size_bytes,
            });
        }
    }

    groups
}

/// How the settings file spells a maximum age, which is whole days.
const SECONDS_A_DAY: u64 = 86_400;

/// What the library occupies, as the window is told about it.
///
/// `proposed` says whether the limits came from the request rather than from the
/// settings file, and it travels because the two readings must not be drawn the
/// same way: one is what is happening, the other is what *would* happen if
/// somebody saved a limit they have not saved yet (AGENTS.md section 56).
fn report_of(
    measured: &clipped_session::cleanup::Measurement,
    recordings: &std::path::Path,
    proposed: bool,
) -> StorageReport {
    let mut by_category: Vec<CategoryUsage> = measured
        .by_category
        .iter()
        // A category with nothing in it is left out rather than sent as a zero:
        // `screenshots: 0` on a machine that has never taken one is a row about
        // a feature rather than about the disk.
        .filter(|(_, bytes)| **bytes > 0)
        .map(|(category, bytes)| CategoryUsage {
            category: category.as_str().to_owned(),
            bytes: *bytes,
        })
        .collect();
    by_category.sort_by_key(|usage| core::cmp::Reverse(usage.bytes));

    StorageReport {
        recordings_directory: recordings.to_string_lossy().into_owned(),
        trash_directory: measured.trash.to_string_lossy().into_owned(),
        usage_bytes: measured.usage_bytes,
        by_category,
        free_bytes: measured.free_bytes,
        // Read from the same volume the free space was, and falling back to the
        // free figure rather than to zero: a capacity of nought beside 162 GB
        // free is a drive that is more than full, which is a worse thing to draw
        // than a bar that reads as entirely free.
        capacity_bytes: clipped_library::accounting::capacity_of(recordings)
            .map_or(measured.free_bytes, |volume| volume.total_bytes()),
        limits: StorageLimits {
            maximum_usage_bytes: measured.limits.maximum_usage(),
            minimum_free_space_bytes: measured.limits.minimum_free_space(),
            maximum_age_days: measured
                .limits
                .maximum_age()
                .map(|age| age.as_secs() / SECONDS_A_DAY),
        },
        proposed,
        would_delete: recording_list(&measured.plan.deletions),
        still_over_limit: measured.plan.still_over_limit,
        protected: protected_groups(&measured.plan.protected),
        largest: recording_list(&measured.largest),
    }
}

/// A path as the window is told about it, or nothing where there is none.
fn text(path: Option<&std::path::Path>) -> Option<String> {
    path.map(|path| path.to_string_lossy().into_owned())
}

/// One trash entry, as the window is told about it.
///
/// `original_path` is the one a person recognises: a file inside the trash is
/// named for the trash, and a screen that showed only that would be asking
/// somebody to identify their own recording by a name they have never seen.
fn trashed_item(entry: clipped_library::trash::TrashEntry) -> TrashedItem {
    TrashedItem {
        kind: match entry.item.kind {
            clipped_library::trash::ItemKind::Recording => "recording".to_owned(),
            clipped_library::trash::ItemKind::Clip => "clip".to_owned(),
        },
        id: entry.item.id,
        // Absent rather than blank where there is no file. A clip nothing has
        // exported has never been anywhere, and `""` is a file name a window
        // would try to open (issue #593).
        path: text(entry.path.as_deref()),
        original_path: text(entry.original_path.as_deref()),
        deleted_at: entry.deleted_at,
        // See `LibraryReader::trash`: nothing configures the retention period,
        // so there is no date to give that would not be invented.
        expires_at: None,
        size_bytes: entry.size_bytes,
        dependent_clips: entry.dependent_clips,
    }
}

impl LibraryReader {
    /// A reader for the library at Clipped's usual place:
    /// `%LOCALAPPDATA%\Clipped\library.db`, beside the logs (`docs/storage.md`).
    ///
    /// The trash comes from `configuration`, through the same
    /// `clipped_session::config::trash_directory` that a deletion uses, so that
    /// `library_trash` reports **the directory actually in use**.
    ///
    /// It used to be derived from `default_output_directory` alone, which
    /// ignored both a configured recording directory and a configured
    /// `trash_directory` — so a window could be shown a path nothing deletes
    /// into ([issue #646](https://github.com/wildware-uk/clipped/issues/646)).
    /// One question, one answer, which is what AGENTS.md section 55 asks for and
    /// what `cleanup::trash_directory`'s own documentation says it exists to
    /// keep.
    #[must_use]
    pub fn for_this_user(configuration: &clipped_session::config::Configuration) -> Self {
        let mut reader = Self::at(
            clipped_logging::application_directory().map(|directory| directory.join(LIBRARY_FILE)),
        );
        reader.trash_directory = crate::config::default_output_directory().map(|default| {
            let recordings = configuration
                .storage()
                .recording_directory()
                .map_or(default, std::path::Path::to_path_buf);
            clipped_session::cleanup::trash_directory(configuration, &recordings)
        });
        reader
    }

    /// A reader for the library at a named path, or for no library at all.
    #[must_use]
    pub fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            database: Mutex::new(None),
            trash_directory: None,
        }
    }

    /// What is waiting in the trash, newest deletion first.
    ///
    /// Reads the index and nothing else: an entry is a row with `deleted_at`
    /// set, so a trash on a drive that is not present still *lists*, and each
    /// item says where its file is. That is the difference between a screen
    /// that says "your deleted recordings are on a drive that is not here" and
    /// one that appears empty
    /// ([issue #450](https://github.com/wildware-uk/clipped/issues/450)).
    ///
    /// `expires_at` is absent on every item, deliberately: nothing configures
    /// the retention period yet, and a date computed from a policy nobody set
    /// would be a screen promising a deletion date it cannot keep.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::LibraryUnavailable`] when the index cannot be read.
    pub fn trash(&self) -> Result<TrashListing, ProtocolError> {
        let directory = self
            .trash_directory
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));

        self.with_database(|database| {
            let entries = clipped_library::trash::Trash::new(&directory)
                .list(database)
                .map_err(|error| {
                    ProtocolError::new(
                        ErrorCode::LibraryUnavailable,
                        format!("the trash could not be read: {error}"),
                    )
                })?;

            let total_bytes = entries
                .iter()
                .map(clipped_library::trash::TrashEntry::bytes)
                .sum();

            Ok(TrashListing {
                total_items: entries.len() as u64,
                total_bytes,
                directory: directory.to_string_lossy().into_owned(),
                items: entries.into_iter().map(trashed_item).collect(),
            })
        })
    }

    /// What the library occupies, and what a storage limit would do about it.
    ///
    /// The projection of `clipped_session::cleanup::preview` a window reads
    /// (`clipped_ipc::storage`,
    /// [issue #95](https://github.com/wildware-uk/clipped/issues/95)).
    /// `configuration` carries the limits to judge against — the configured
    /// ones, or the ones a caller proposed — and `recordings` is the folder this
    /// recorder writes into, which is the root that is measured and the volume
    /// whose free space is read.
    ///
    /// **Nothing is deleted, trashed or moved.** `preview` is the sweep's own
    /// measurement without its last step, which is what makes a dry run
    /// impossible to disagree with what happens (AGENTS.md section 55).
    ///
    /// # What this costs, and why it is answered here
    ///
    /// It walks the recording and trash directories, reading no file's
    /// contents, and runs one statement over the index. That is more than the
    /// other reads on this connection thread, and it is still the right thread:
    /// it shares nothing with a recording — no capture, encoder or muxer thread
    /// waits on the filesystem or on this lock — and it is behind a screen
    /// somebody opened deliberately (AGENTS.md sections 17 and 20).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::LibraryUnavailable`] when the index could not be read, when
    /// the directories could not be declared, or when the volume could not be
    /// measured — each carrying `clipped_session`'s own account of which.
    /// Refused rather than answered with a guess: free space that is unknown is
    /// not free space of zero, and a screen told the disk was full would send
    /// somebody deleting recordings to fix a reading nobody took.
    pub fn measure(
        &self,
        configuration: &clipped_session::config::Configuration,
        recordings: &std::path::Path,
        proposed: bool,
    ) -> Result<StorageReport, ProtocolError> {
        self.with_database(|database| {
            let measured = clipped_session::cleanup::preview(
                configuration,
                recordings,
                database,
                SystemTime::now(),
            )
            .map_err(|reason| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    format!(
                        "the library could not be measured: {}",
                        crate::storage::describe(&reason)
                    ),
                )
            })?;

            Ok(report_of(&measured, recordings, proposed))
        })
    }

    /// One page of the library.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] if `request.query` is not a query the
    /// search language accepts, carrying the position and what was expected
    /// there; [`ErrorCode::LibraryUnavailable`] if the index could not be read,
    /// carrying why.
    pub fn sessions(&self, request: &LibrarySessions) -> Result<LibrarySessionPage, ProtocolError> {
        // Before the database is opened: a query that cannot be parsed is the
        // caller's mistake and should be reported as one even on a machine
        // whose library is unreadable.
        let query = request
            .query
            .as_deref()
            .filter(|query| !query.trim().is_empty())
            .map(|query| {
                query.parse::<Query>().map_err(|error| {
                    ProtocolError::new(
                        ErrorCode::InvalidParameters,
                        format!("`library_sessions` was not given a usable query: {error}"),
                    )
                })
            })
            .transpose()?;

        self.with_database(|database| {
            let page = list_sessions(
                database,
                &SessionListing {
                    limit: request
                        .limit
                        .map_or(0, |limit| usize::try_from(limit).unwrap_or(usize::MAX)),
                    after: request.after.clone(),
                    query: query.as_ref(),
                },
            )
            .map_err(unavailable)?;

            Ok(within_one_frame(&page.sessions, page.next))
        })
    }

    /// What the library holds per game.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::LibraryUnavailable`] if the index could not be read,
    /// carrying why.
    pub fn games(&self) -> Result<Vec<LibraryGame>, ProtocolError> {
        self.with_database(|database| {
            let summaries = game_summaries(database).map_err(unavailable)?;
            Ok(summaries.iter().map(game).collect())
        })
    }

    /// The game events of one recording, placed in that recording's file.
    ///
    /// The offset is a subtraction the index can do because both halves are
    /// columns: an event's moment on the session's timeline, and where that
    /// recording starts on the same timeline (`starts_at_nanos`, migration
    /// `0005`). Nothing opens a file, and nothing re-reads a sidecar.
    ///
    /// A recording with no events answers with an empty lane rather than an
    /// error. "None" and "the question was never asked" are different things to
    /// draw, and the reply arriving at all is what tells them apart
    /// (AGENTS.md section 27).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] if the recording is not named by an
    /// identifier the library uses, and [`ErrorCode::LibraryUnavailable`] if
    /// the index could not be read.
    pub fn events(&self, request: &LibraryEvents) -> Result<LibraryEventLane, ProtocolError> {
        // Before the database is opened, for the reason `sessions` parses its
        // query first: a malformed identifier is the caller's mistake and is
        // worth saying so even on a machine whose library cannot be read.
        let recording: i64 = request.recording.parse().map_err(|_| {
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                format!(
                    "`library_events` was given `{}`, which is not a recording identifier this                      library uses",
                    request.recording
                ),
            )
        })?;

        self.with_database(|database| {
            let mut statement = database
                .connection()
                .prepare(
                    "SELECT game_events.at_nanos - recordings.starts_at_nanos, \n                            game_events.kind, game_events.source \n                     FROM game_events \n                     JOIN recordings \n                         ON recordings.recording_id = game_events.recording_id \n                     WHERE game_events.recording_id = ?1 \n                       AND recordings.starts_at_nanos IS NOT NULL \n                     ORDER BY game_events.at_nanos",
                )
                .map_err(unreadable)?;

            let marks = statement
                .query_map([recording], |row| {
                    Ok(LibraryEventMark {
                        recording: request.recording.clone(),
                        at: row.get(0)?,
                        kind: row.get(1)?,
                        source: row.get(2)?,
                    })
                })
                .map_err(unreadable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(unreadable)?;

            Ok(LibraryEventLane { marks })
        })
    }

    /// One clip's edit document, as text the window's reader can parse.
    ///
    /// This is what opens a clip in the editor (issue #306). The document lives
    /// in `clips.edit` and the window can reach neither the column nor
    /// `clipped_edit`, so everything that has to be decided about it is decided
    /// here — in one place, which is the point.
    ///
    /// # Three things this does that the window must not
    ///
    /// **It converts.** A document written by an older build is brought forward
    /// through `crates/edit`'s migration chain and sent at the current version,
    /// with [`ClipDocument::converted_from`] saying what it was. In memory
    /// only: nothing is written, and the stored text is still the older one
    /// until somebody saves. That is what makes the window's own refusal of an
    /// older document correct rather than a gap — it never receives one.
    ///
    /// **It synthesises.** A clip with no document at all is a saved replay,
    /// made before there was an editor: a file, and a window of the recording
    /// it came from. The document that means "this recording, this span, no
    /// edits" is built here, from the columns migration 0004 keeps beside
    /// `edit` for exactly this kind of question, and
    /// [`ClipDocument::synthesised`] says so. It is built here and nowhere else
    /// on purpose: two builds inventing a starting document separately would
    /// disagree about what an unedited clip is, and the disagreement would only
    /// show up the first time somebody saved one.
    ///
    /// **It refuses.** A document this build cannot read — one from a newer
    /// Clipped above all — is [`ErrorCode::EditUnreadable`] with the sentence
    /// `crates/edit` writes for it, and nothing is changed. An editor drawn
    /// over an empty timeline instead would be indistinguishable from a broken
    /// one (AGENTS.md section 27).
    ///
    /// Nothing here opens, reads or writes a media file. A document is metadata
    /// over recordings that are never touched (AGENTS.md sections 56 and 57).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] if the clip is not named by an
    /// identifier this library uses, if there is no such clip, if it is in the
    /// trash, or if it has neither a document nor a recording to build one
    /// from; [`ErrorCode::EditUnreadable`] if the stored document will not
    /// open; and [`ErrorCode::LibraryUnavailable`] if the index could not be
    /// read.
    pub fn clip_document(
        &self,
        request: &LibraryClipDocument,
    ) -> Result<ClipDocument, ProtocolError> {
        // Before the database is opened, for the reason `events` parses its
        // identifier first: a malformed one is the caller's mistake and worth
        // saying so even on a machine whose library cannot be read.
        let clip = clip_identifier(&request.clip, "library_clip_document")?;

        let stored = self.with_database(|database| {
            database
                .connection()
                .query_row(
                    "SELECT clips.edit, \
                            clips.title, \
                            clips.deleted_at, \
                            clips.source_recording_id, \
                            clips.source_start_seconds, \
                            clips.source_end_seconds, \
                            recordings.duration_seconds \
                     FROM clips \
                     LEFT JOIN recordings \
                         ON recordings.recording_id = clips.source_recording_id \
                     WHERE clips.clip_id = ?1",
                    [clip],
                    |row| {
                        Ok(StoredClip {
                            edit: row.get(0)?,
                            title: row.get(1)?,
                            deleted_at: row.get(2)?,
                            source_recording_id: row.get(3)?,
                            source_start_seconds: row.get(4)?,
                            source_end_seconds: row.get(5)?,
                            recording_duration_seconds: row.get(6)?,
                        })
                    },
                )
                .map_err(|error| match error {
                    clipped_storage::rusqlite::Error::QueryReturnedNoRows => no_such_clip(clip),
                    other => unreadable(other),
                })
        })?;

        if let Some(deleted_at) = stored.deleted_at.as_deref() {
            // A clip in the trash is one somebody deleted. Opening it in an
            // editor would offer to edit something that is on its way out, and
            // a save against it would resurrect an edit into a row the next
            // empty destroys. Putting it back first is the honest order, and it
            // is a thing the user can actually do (issue #450).
            return Err(ProtocolError::new(
                ErrorCode::InvalidParameters,
                format!(
                    "clip {clip} was deleted on {deleted_at} and is in the trash. Restore it \
                     before editing it."
                ),
            ));
        }

        match stored.edit.as_deref() {
            Some(text) => {
                let loaded =
                    EditDocument::read(text).map_err(|error| unreadable_edit(clip, &error))?;
                Ok(ClipDocument {
                    clip: request.clip.clone(),
                    // Written out again rather than sent verbatim, so that what
                    // crosses is always the version this build writes: for a
                    // converted document the stored text is the *old* one, and
                    // sending it would hand the window exactly the thing it
                    // refuses.
                    document: loaded
                        .document
                        .write()
                        .map_err(|error| unreadable_edit(clip, &error))?,
                    converted_from: loaded.migrated.map(|migrated| migrated.from),
                    synthesised: false,
                })
            }
            None => Ok(ClipDocument {
                clip: request.clip.clone(),
                document: starting_document(clip, &stored)?,
                converted_from: None,
                synthesised: true,
            }),
        }
    }

    /// Stores an edited document against a clip.
    ///
    /// # What a save may not do
    ///
    /// It may not touch a recording, and cannot: this writes one `TEXT` column
    /// of one row and opens no file (AGENTS.md sections 56 and 57).
    ///
    /// It may not store text that will not open again. The document is read and
    /// validated first — `crates/edit` validates on every read *and* every
    /// write — and what is stored is what the writer produced, so a window
    /// sending nonsense fails to save rather than corrupting a clip. The stored
    /// document is left exactly as it was in that case, which is what the
    /// refusal says.
    ///
    /// And it may not lose the older text it replaced. When the document being
    /// overwritten was in an older format, a copy of it goes into
    /// `clips.edit_superseded` in the same transaction — `docs/editing.md`'s
    /// "the caller decides whether to store the result, and must keep the
    /// original when it does", paid. The copy is written once and never
    /// overwritten: it holds the only text this build could not have produced,
    /// and a second save replacing it with text this build wrote would destroy
    /// the thing it is for.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] if the clip is not named by an
    /// identifier this library uses, if there is no such clip, or if it is in
    /// the trash; [`ErrorCode::EditUnreadable`] if the document sent will not
    /// open, or if the one already stored will not, naming what to do about it;
    /// and [`ErrorCode::LibraryUnavailable`] if the index could not be written.
    pub fn save_clip_document(
        &self,
        request: &SaveClipDocument,
        now: SystemTime,
    ) -> Result<ClipDocumentSaved, ProtocolError> {
        let clip = clip_identifier(&request.clip, "save_clip_document")?;

        // Read, convert and validate before the database is opened at all. A
        // document that will not open is the caller's mistake, and refusing it
        // here means there is no path from a bad request to a write.
        let incoming = EditDocument::read(&request.document)
            .map_err(|error| refused_document(&error))?
            .document;
        let text = incoming.write().map_err(|error| refused_document(&error))?;

        let at = rfc3339(now);

        self.with_database_mut(|database| {
            let transaction = database.transaction().map_err(|error| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    format!("the recording library could not be written: {error}"),
                )
            })?;

            let (stored, deleted_at, already_kept): (Option<String>, Option<String>, bool) =
                transaction
                    .query_row(
                        "SELECT edit, deleted_at, edit_superseded IS NOT NULL \
                         FROM clips WHERE clip_id = ?1",
                        [clip],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .map_err(|error| match error {
                        clipped_storage::rusqlite::Error::QueryReturnedNoRows => no_such_clip(clip),
                        other => unreadable(other),
                    })?;

            if let Some(deleted_at) = deleted_at.as_deref() {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidParameters,
                    format!(
                        "clip {clip} was deleted on {deleted_at} and is in the trash. Restore it \
                         before saving an edit to it."
                    ),
                ));
            }

            // What version the text about to be replaced was in. A document
            // that will not read at all is refused rather than overwritten:
            // this build cannot tell whether it is older or newer, and
            // overwriting the one case it must not — a document from a build
            // ahead of this one — is how somebody loses the edit they made on
            // their other machine (AGENTS.md sections 43 and 56).
            let superseding = match stored.as_deref() {
                None => None,
                Some(previous) => EditDocument::read(previous)
                    .map_err(|error| unreadable_edit(clip, &error))?
                    .migrated
                    .map(|migrated| migrated.from),
            };

            let changed = match (superseding, already_kept) {
                // The case the column exists for, and the only one that writes
                // it: older text, and nothing kept for this clip yet.
                //
                // `AND edit_superseded IS NULL` in the statement is deliberate
                // belt-and-braces and not the guard: this arm is, and removing
                // the clause on its own breaks no test because the arm already
                // makes the case unreachable. It is kept so that the statement
                // is safe on its own terms — the next caller to reach for it
                // gets write-once behaviour without having to know about this
                // match — and it is documented as redundant so nobody mistakes
                // it for the protection and relaxes the arm.
                (Some(from), false) => transaction
                    .execute(
                        "UPDATE clips \
                         SET edit = ?2, \
                             edit_superseded = ?3, \
                             edit_superseded_at = ?4, \
                             edit_superseded_version = ?5 \
                         WHERE clip_id = ?1 AND edit_superseded IS NULL",
                        clipped_storage::rusqlite::params![
                            clip,
                            &text,
                            stored.as_deref(),
                            &at,
                            from
                        ],
                    )
                    .map_err(unreadable)?,
                // Nothing older to keep, or an original already kept by an
                // earlier save. Either way the copy is left alone.
                _ => transaction
                    .execute(
                        "UPDATE clips SET edit = ?2 WHERE clip_id = ?1",
                        clipped_storage::rusqlite::params![clip, &text],
                    )
                    .map_err(unreadable)?,
            };

            if changed == 0 {
                return Err(no_such_clip(clip));
            }

            transaction.commit().map_err(|error| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    format!("the edit could not be saved: {error}"),
                )
            })?;

            Ok(ClipDocumentSaved {
                clip: request.clip.clone(),
                // Only when the copy was actually written by *this* save. A
                // reply saying an original was kept when an earlier save kept
                // it would be a different claim about a different text.
                superseded: if already_kept { None } else { superseding },
            })
        })
    }

    /// Runs a read against the open database, opening it if this is the first
    /// question or if the last attempt failed.
    ///
    /// The lock is held for the length of the read, which is what serialises the
    /// connection between connection threads. A read is a bounded query over
    /// local data; nothing that must not stall waits on this.
    fn with_database<T>(
        &self,
        read: impl FnOnce(&Database) -> Result<T, ProtocolError>,
    ) -> Result<T, ProtocolError> {
        let mut held = self.database.lock().map_err(poisoned)?;

        if held.is_none() {
            let path = self.path.as_ref().ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    "this machine describes no per-user application directory, so Clipped has \
                     nowhere to keep its library index",
                )
            })?;

            // `Database::open` creates the file and runs the migrations. This
            // process owns the writing connection (`docs/storage.md`), so it is
            // the one that may do that; the window never opens the file at all.
            //
            // A failure leaves this `None`, so the next question tries again —
            // the usual cause is a drive that is not plugged in, and that stops
            // being true without restarting anything.
            let opened = Database::open(path).map_err(|error| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    format!("the recording library could not be opened: {error}"),
                )
            })?;
            *held = Some(opened);
        }

        read(held.as_ref().expect("the database was just opened"))
    }

    /// Puts one thing back where it was.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] for a kind this build does not know or
    /// an item the trash does not hold, and [`ErrorCode::LibraryUnavailable`]
    /// when the index or the file system refuses.
    pub fn restore(&self, request: &RestoreFromTrash) -> Result<RestoredItem, ProtocolError> {
        let item = trash_item(&request.kind, request.id)?;
        let directory = self.trash_path();

        self.with_database_mut(|database| {
            let outcome = clipped_library::trash::Trash::new(&directory)
                .restore(database, item)
                .map_err(restore_refused)?;

            Ok(RestoredItem {
                kind: request.kind.clone(),
                id: request.id,
                path: text(outcome.path.as_deref()),
                file_restored: outcome.file_restored,
                renamed: outcome.path != outcome.original_path,
            })
        })
    }

    /// Destroys everything in the trash, if it is still what the caller was
    /// shown.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] when the trash is not what was
    /// confirmed — naming both counts, so a window can say what changed and
    /// show the new listing — and [`ErrorCode::LibraryUnavailable`] when it
    /// could not be read.
    pub fn empty(&self, request: &clipped_ipc::EmptyTrash) -> Result<TrashEmptied, ProtocolError> {
        let directory = self.trash_path();
        let confirmation = clipped_library::trash::EmptyTrash::confirmed(
            usize::try_from(request.items).unwrap_or(usize::MAX),
            request.bytes,
        );

        self.with_database_mut(|database| {
            let report = clipped_library::trash::Trash::new(&directory)
                .empty(database, confirmation)
                .map_err(empty_refused)?;

            Ok(TrashEmptied {
                removed: report.removed.len() as u64,
                reclaimed_bytes: report.bytes_reclaimed(),
                // Each failure names the item it is about as well as the
                // reason, because "one file would not go" without saying which
                // is a sentence nobody can act on (AGENTS.md section 15).
                refused: report
                    .failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.item, failure.error))
                    .collect(),
            })
        })
    }

    /// Marks one thing a favourite, or clears the mark.
    ///
    /// [Issue #58](https://github.com/wildware-uk/clipped/issues/58). `at` is
    /// when, and it is passed in rather than read here for the reason
    /// `clipped_library::favourites` takes one: every write in that crate does,
    /// so a test does not have to wait for a clock.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] for a kind this build does not know or
    /// a target that names nothing, and [`ErrorCode::LibraryUnavailable`] when
    /// the index refuses.
    pub fn set_favourite(
        &self,
        request: &SetFavourite,
        at: SystemTime,
    ) -> Result<FavouriteMark, ProtocolError> {
        let what = favourite_target(request)?;

        self.with_database_mut(|database| {
            let changed = if request.favourite {
                clipped_library::favourites::mark(database, &what, at)
            } else {
                clipped_library::favourites::unmark(database, &what)
            }
            .map_err(unreadable)?;

            // Read back rather than assume. The two disagree for a target that
            // is not there: nothing was written, and a window told "favourited"
            // would draw a full star against a row that has none.
            let favourite =
                clipped_library::favourites::is_marked(database, &what).map_err(unreadable)?;

            Ok(FavouriteMark {
                kind: request.kind.clone(),
                session_id: request.session_id.clone(),
                id: request.id,
                favourite,
                changed,
            })
        })
    }

    /// Locks one thing against automatic cleanup, or unlocks it.
    ///
    /// [Issue #472](https://github.com/wildware-uk/clipped/issues/472).
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] for a kind that cannot be locked or a
    /// target that names nothing, and [`ErrorCode::LibraryUnavailable`] when the
    /// index refuses.
    pub fn set_lock(&self, request: &SetLock, at: SystemTime) -> Result<LockMark, ProtocolError> {
        let what = lock_target(request)?;

        self.with_database_mut(|database| {
            let changed = if request.locked {
                clipped_library::locks::lock(database, &what, at)
            } else {
                clipped_library::locks::unlock(database, &what)
            }
            .map_err(unreadable)?;

            // Read back, for the reason `set_favourite` does: a target that is
            // not there is written to by nothing.
            let locked = clipped_library::locks::is_locked(database, &what).map_err(unreadable)?;

            // And separately, whether the sweep will leave it alone — which is
            // a different question for a recording inside a locked sitting, and
            // is the one a padlock on a screen is drawn from.
            let protected = match &what {
                Lockable::Session(_) => locked,
                Lockable::Recording(id) => {
                    clipped_library::locks::protects(database, *id).map_err(unreadable)?
                }
            };

            Ok(LockMark {
                kind: request.kind.clone(),
                session_id: request.session_id.clone(),
                id: request.id,
                locked,
                protected,
                changed,
            })
        })
    }

    /// Where deleted media waits, as this reader was told.
    fn trash_path(&self) -> PathBuf {
        self.trash_directory
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// The same, for the two commands that change the trash.
    ///
    /// Restoring and emptying write, and the trash module's every statement is
    /// its own transaction (`clipped_library::trash`), so this holds the same
    /// connection the reads use rather than opening a second one. The indexer
    /// has a connection of its own and may be reconciling at the same moment;
    /// the database is in write-ahead logging mode, which is what makes that
    /// safe (`docs/storage.md`).
    fn with_database_mut<T>(
        &self,
        write: impl FnOnce(&mut Database) -> Result<T, ProtocolError>,
    ) -> Result<T, ProtocolError> {
        let mut held = self.database.lock().map_err(poisoned)?;

        if held.is_none() {
            let path = self.path.as_ref().ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    "this machine describes no per-user application directory, so Clipped has                      nowhere to keep its library index",
                )
            })?;
            let opened = Database::open(path).map_err(|error| {
                ProtocolError::new(
                    ErrorCode::LibraryUnavailable,
                    format!("the recording library could not be opened: {error}"),
                )
            })?;
            *held = Some(opened);
        }

        write(held.as_mut().expect("the database was just opened"))
    }
}

/// The columns a clip's document is read or built from.
///
/// `title` and the three `source_*` fields are what migration 0004 deliberately
/// keeps beside `edit` rather than folding into it, so that a question about a
/// clip does not mean parsing a document. Building a starting document is
/// exactly such a question.
struct StoredClip {
    /// The document, or `None` for a clip made before there was an editor.
    edit: Option<String>,
    /// What the clip is called, if anything.
    title: Option<String>,
    /// When it was deleted, for a clip that is in the trash.
    deleted_at: Option<String>,
    /// The recording it was cut from, when the library still knows.
    source_recording_id: Option<i64>,
    /// Where in that recording it starts.
    source_start_seconds: Option<f64>,
    /// And where it ends.
    source_end_seconds: Option<f64>,
    /// How long the whole recording is, for a clip that names no window.
    recording_duration_seconds: Option<f64>,
}

/// The clip a request names, or why it names none.
///
/// Shaped like `events`' parse of a recording identifier, and refusing for the
/// same reason: a window was handed this number by the index and sending
/// something else back is a fault worth naming rather than a lookup that
/// happens to find nothing.
fn clip_identifier(clip: &str, command: &str) -> Result<i64, ProtocolError> {
    clip.parse().map_err(|_| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "`{command}` was given `{clip}`, which is not a clip identifier this library uses"
            ),
        )
    })
}

/// A clip the index does not have.
fn no_such_clip(clip: i64) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::InvalidParameters,
        format!("this library has no clip {clip}"),
    )
}

/// A document that is stored and will not open.
///
/// The sentence comes from `crates/edit`, which is where the reason lives:
/// a version this build is too old for says so and says to update, and a
/// document that is not one at all says what the parser made of it. Restating
/// either here would be a second, worse copy of a message the model already
/// writes carefully.
fn unreadable_edit(clip: i64, error: &EditDocumentError) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::EditUnreadable,
        format!("clip {clip} could not be opened: {error}"),
    )
}

/// A document the window sent that will not open.
///
/// [`ErrorCode::EditUnreadable`] rather than `invalid_parameters`, even though
/// this one really is the caller's parameter, because the sentence the user
/// needs is the same one and the remedy is the same one. What matters more is
/// the second half of the message: **nothing was stored**. A save that refused
/// has to say the clip is untouched, or the only safe thing to do with an
/// editor that will not save is to close it and hope (AGENTS.md section 56).
fn refused_document(error: &EditDocumentError) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::EditUnreadable,
        format!("this edit was not saved: {error} The clip is exactly as it was."),
    )
}

/// The document of a clip nobody has ever edited.
///
/// "This recording, this window of it, no edits" — which is what a saved replay
/// is, and `EditDocument::from_recording` is the constructor written for
/// exactly that (SPEC.md section 20). It is not a second edit model built here;
/// it is the one the rest of Clipped uses, given the numbers the clip's row
/// already holds.
///
/// A clip that names no recording cannot have one built: there is nothing for
/// the document to play. That is refused rather than answered with an empty
/// document, because an empty document is a legitimate thing — a clip somebody
/// deleted everything from — and handing one back here would tell the user
/// their clip was empty when the truth is that the library lost track of what
/// it was cut from (issue #591).
fn starting_document(clip: i64, stored: &StoredClip) -> Result<String, ProtocolError> {
    let nothing_to_build = |why: &str| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!("clip {clip} has no edit document, and {why}, so there is nothing to open."),
        )
    };

    let recording = stored.source_recording_id.ok_or_else(|| {
        nothing_to_build("the library no longer knows which recording it came from")
    })?;

    // A clip that names no window of its recording is the whole of it. That is
    // the honest reading of two NULLs beside a `source_recording_id`, and it is
    // the state `save_replay` leaves a row in before anything trims it.
    let start = stored.source_start_seconds.unwrap_or(0.0);
    let end = match stored.source_end_seconds {
        Some(end) => end,
        None => stored
            .recording_duration_seconds
            .ok_or_else(|| nothing_to_build("neither it nor its recording says how long it is"))?,
    };

    let span = SourceSpan::new(seconds_to_source_time(start), seconds_to_source_time(end))
        .ok_or_else(|| {
            nothing_to_build(&format!(
                "the window it names — {start}s to {end}s — is empty or backwards"
            ))
        })?;

    EditDocument::from_recording(
        stored
            .title
            .clone()
            .unwrap_or_else(|| "Untitled".to_owned()),
        RecordingId::new(recording.to_string()),
        span,
    )
    .write()
    .map_err(|error| {
        ProtocolError::new(
            ErrorCode::EditUnreadable,
            format!("clip {clip} could not be opened: {error}"),
        )
    })
}

/// Seconds off a `REAL` column as a moment on a recording's own timeline.
///
/// Clamped at zero rather than wrapping, because the column is `REAL` and a
/// negative in it is a corrupt row rather than a time; the span check
/// downstream is what refuses the result. Rounded rather than truncated so that
/// a clip stored as 4.9999999 seconds does not open a nanosecond short of where
/// it was cut.
fn seconds_to_source_time(seconds: f64) -> SourceTime {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped into range on the line above, which is what makes the cast total"
    )]
    SourceTime::from_nanos(
        (seconds * 1_000_000_000.0)
            .round()
            .clamp(0.0, u64::MAX as f64) as u64,
    )
}

/// An instant as this database writes them.
///
/// The same shape `clipped_library`'s own writers use for `favourited_at` and
/// `locked_at`, because `edit_superseded_at` sits in the same table and a
/// second convention two columns apart would be read by the same query.
fn rfc3339(at: SystemTime) -> String {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// A lock a panic elsewhere left poisoned.
///
/// Unlike the recorder's status — which is read through a poisoned lock so that
/// a panic cannot turn "recording" into "unknown" — a poisoned database
/// connection may be mid-statement, so this refuses rather than reading through
/// it. The refusal says so, which is more use than a second panic.
fn poisoned<T>(_: std::sync::PoisonError<T>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::LibraryUnavailable,
        "the recording library was left in an unknown state by an earlier failure; restart \
         Clipped's recorder to read it again",
    )
}

/// A read the index refused.
fn unreadable(error: clipped_storage::rusqlite::Error) -> ProtocolError {
    // The same code a failed index read answers with, because it is the same
    // thing from the caller's side: the library is there and could not be
    // asked. A window shows one message for both and retries the same way.
    ProtocolError::new(
        ErrorCode::LibraryUnavailable,
        format!("the recording library could not be read: {error}"),
    )
}

fn unavailable(error: clipped_library::index::IndexError) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::LibraryUnavailable,
        format!("the recording library could not be read: {error}"),
    )
}

/// How many bytes of one frame a page of sessions may fill.
///
/// [`clipped_ipc::MAX_FRAME_BYTES`] is a bound on damage rather than a budget to
/// spend, and a frame over it is **not** a failed request: the reader closes the
/// connection rather than resynchronising (`crates/ipc/src/frame.rs`). So a page
/// that overran it would drop the window's control connection, and would do it
/// again on every retry, with nothing on screen to explain why.
///
/// Half a frame, because this measures the sessions alone and the response
/// envelope, the reply tag and the cursor are carried around them.
///
/// A count of sessions cannot be this bound, which is the thing worth knowing
/// here: a session holds any number of recordings and clips, so two hundred of
/// them — `clipped_library::index::MAX_PAGE_LIMIT` — is 135 KB with one
/// recording each and over 3 MB with thirty. Measured, not guessed:
/// `a_page_is_bounded_by_what_a_frame_can_carry_rather_than_by_a_count_of_sessions`
/// builds a real index of that shape and reads it back through
/// [`LibraryReader::sessions`].
const PAGE_BUDGET_BYTES: usize = MAX_FRAME_BYTES as usize / 2;

/// As many of these sittings as one frame can carry, and where to continue.
///
/// `beyond` is the cursor the index offered for the page it answered. It is kept
/// when every session fits; when the page is cut short, the cursor becomes that
/// of the last session actually carried, so the next page starts with the first
/// one left out rather than skipping it.
///
/// **At least one session is always carried, whatever it costs.** A page that
/// could come back empty because its first session was too large would page for
/// ever and never show it, which is worse than one oversized frame — and the
/// frame reader's ceiling is an order of magnitude above any single sitting.
fn within_one_frame(sessions: &[IndexedSession], beyond: Option<String>) -> LibrarySessionPage {
    let mut carried = Vec::with_capacity(sessions.len());
    let mut remaining = PAGE_BUDGET_BYTES;

    for indexed in sessions {
        let wire = session(indexed);
        // A session that cannot be measured is treated as the whole budget
        // rather than as free: the alternative is a page that grows without
        // bound because nothing could size it.
        let cost = serde_json::to_vec(&wire).map_or(PAGE_BUDGET_BYTES, |bytes| bytes.len());

        if !carried.is_empty() && cost > remaining {
            // `carried` is non-empty, so it has a last session, and it is the
            // one the cursor has to name.
            let last = &sessions[carried.len() - 1];
            return LibrarySessionPage {
                sessions: carried,
                next_cursor: Some(cursor_of(last)),
            };
        }

        remaining = remaining.saturating_sub(cost);
        carried.push(wire);
    }

    LibrarySessionPage {
        sessions: carried,
        next_cursor: beyond,
    }
}

/// One sitting, on the wire.
fn session(session: &IndexedSession) -> LibrarySession {
    LibrarySession {
        session_id: session.session_id.clone(),
        game_id: session.game_id.clone(),
        game_name: session.game_name.clone(),
        started_at: session.started_at.clone(),
        ended_at: session.ended_at.clone(),
        end_reason: session.end_reason.clone(),
        favourite: session.favourite,
        locked: session.locked,
        // The sitting's lock is passed down rather than looked up again: it is
        // the cascade, and working it out per recording would be a second
        // expression of a rule `clipped_library::locks` already owns.
        recordings: session
            .recordings
            .iter()
            .map(|held| recording(held, session.locked))
            .collect(),
        clips: session.clips.iter().map(clip).collect(),
    }
}

/// One recording, on the wire.
///
/// `locked_session` is whether the sitting it belongs to is locked, which is
/// what makes a recording with no lock of its own protected.
fn recording(recording: &IndexedRecording, locked_session: bool) -> LibraryRecording {
    LibraryRecording {
        recording_id: recording.recording_id,
        session_index: u32::try_from(recording.session_index).unwrap_or(u32::MAX),
        path: recording.path.clone(),
        started_at: recording.started_at.clone(),
        ended_at: recording.ended_at.clone(),
        outcome: recording.outcome.clone(),
        end_reason: recording.end_reason.clone(),
        duration_seconds: recording.duration_seconds,
        width: recording.width.and_then(|width| u32::try_from(width).ok()),
        height: recording
            .height
            .and_then(|height| u32::try_from(height).ok()),
        size_bytes: recording.size_bytes.map(count),
        // The field the whole read exists for. A screen that cannot see this
        // draws a broken tile instead of saying the file has gone (AGENTS.md
        // section 27).
        missing_since: recording.missing_since.clone(),
        favourite: recording.favourite,
        locked: recording.locked,
        // The cascade, expressed once and here rather than in every window:
        // a recording inside a locked sitting is protected without carrying
        // a lock of its own (`clipped_library::locks`).
        protected: recording.locked || locked_session,
        tags: recording.tags.clone(),
    }
}

/// One clip, on the wire.
fn clip(clip: &IndexedClip) -> LibraryClip {
    LibraryClip {
        clip_id: clip.clip_id,
        path: clip.path.clone(),
        title: clip.title.clone(),
        created_at: clip.created_at.clone(),
        duration_seconds: clip.duration_seconds,
        size_bytes: clip.size_bytes.map(count),
        missing_since: clip.missing_since.clone(),
        favourite: clip.favourite,
        tags: clip.tags.clone(),
    }
}

/// What the library holds for one game, on the wire.
fn game(summary: &GameSummary) -> LibraryGame {
    LibraryGame {
        game_id: summary.game_id.clone(),
        name: summary.name.clone(),
        first_seen_at: summary.first_seen_at.clone(),
        last_played_at: summary.last_played_at.clone(),
        sessions: summary.sessions,
        recordings: summary.recordings,
        clips: summary.clips,
        favourites: summary.favourites,
        bytes: summary.bytes,
        missing: summary.missing,
    }
}

/// A count the schema stores as a signed integer, as an unsigned one.
///
/// The columns carry `CHECK (… >= 0)`, so a negative value means somebody wrote
/// to the database with something other than Clipped. Zero is the honest answer
/// to "how large is this file" in that case; a wrapped enormous number is not.
fn count(value: i64) -> u64 {
    value.max(0).unsigned_abs()
}

/// Keeps the index in step with what is on disk.
///
/// The counterpart to [`LibraryReader`], and the answer to the second half of
/// [issue #402](https://github.com/wildware-uk/clipped/issues/402):
/// `clipped_library::index::reconcile` was a well-tested subsystem with nothing
/// in the product calling it, so a recording that had just been made never
/// reached the library at all. This is the caller.
///
/// # When it runs, and why those moments
///
/// **At start-up**, once, after the endpoint is listening and the ready line has
/// been printed — so it never delays a window being able to connect. This is
/// what picks up everything produced while nothing was indexing: sessions
/// `watch` recorded in a process of its own, recordings copied onto the machine,
/// a library file the user deleted. It is cheap when there is nothing new,
/// because reconciliation is an upsert against what is already there.
///
/// **After a recording finishes**, from `serve`'s recording state. That is the
/// moment the recording's session record is final, and it is what makes the
/// window's Library screen show a recording made from the window *in the same
/// session, without a restart* — the acceptance criterion the ticket is about.
///
/// Deliberately **not** on a command from the window. Nothing in the protocol
/// asks for a rescan, adding one would be a protocol change, and the two moments
/// above already make the library correct for everything this build can produce.
/// A rescan a user can ask for belongs with recovering sittings the library has
/// no record of ([issue #272](https://github.com/wildware-uk/clipped/issues/272)).
///
/// # Where it runs
///
/// On a thread of its own, and never on the recording thread, a capture thread
/// or a connection thread (AGENTS.md section 20). `reconcile` walks directories
/// and opens transactions; a recording thread that did that between finishing
/// its file and reporting its outcome would hold up the reply to
/// `stop_recording` for as long as the walk took.
///
/// Requests **coalesce**: asking while a run is in flight schedules exactly one
/// more, so a burst of recordings cannot queue a run each. The run itself is
/// paced by `IndexPace::background`, which assumes a game may be recording.
///
/// # Its own connection
///
/// The indexer opens the database itself rather than borrowing
/// [`LibraryReader`]'s. A reconciliation holds its connection for the length of
/// the run, and a library screen that had to wait behind it would be the stall
/// this design exists to avoid. SQLite in write-ahead logging mode is built for
/// exactly this: readers are never blocked by a writer, and both connections are
/// in this process, which is the one that owns the writing one
/// (`docs/storage.md`).
///
/// # What happens to a recording no session claims
///
/// Nothing, and it is reported. `reconcile` never invents a row for a media file
/// no sidecar names, and never deletes or moves one — the files stay exactly
/// where they are, and the run says how many it found and names the first few.
/// That is the state a user upgrading from a build whose `serve` wrote no
/// session record is in: their recordings are on disk, playable, and reported at
/// every run until [issue #272](https://github.com/wildware-uk/clipped/issues/272)
/// offers to recover them.
#[derive(Debug)]
pub struct LibraryIndexer {
    shared: Arc<Indexer>,
    /// Joined by [`Self::shut_down`], so that a run in progress is given the
    /// chance to stop before the process exits.
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// What the indexer thread and everything that pokes it share.
#[derive(Debug)]
struct Indexer {
    /// Where the index is, or [`None`] when this machine describes no per-user
    /// directory at all.
    path: Option<PathBuf>,
    settings: IndexSettings,
    /// The recording folders, kept because the storage sweep needs one and
    /// `IndexSettings` does not hand them back.
    roots: Vec<PathBuf>,
    /// What the library is allowed to occupy, as the user configured it.
    ///
    /// Handed in rather than read here, and empty unless a caller says
    /// otherwise, so that a test's answer does not depend on the settings file
    /// of whoever is running it (AGENTS.md section 25).
    ///
    /// Behind a lock because it changes while the process runs. It was a plain
    /// field, read once when `serve` started, which made a limit saved from the
    /// Settings screen a control whose effect waited for a restart - the
    /// recorder would keep sweeping to yesterday's figure and nothing on screen
    /// would say so (AGENTS.md section 27, issue #95).
    /// [`LibraryIndexer::set_storage`] is what a save calls, and the sweep reads
    /// it at the top of every run.
    storage: Mutex<clipped_session::config::StorageSettings>,
    control: IndexControl,
    state: Mutex<IndexerState>,
    woken: Condvar,
    /// Makes the pictures the library screens draw, or [`None`] on a machine
    /// with nowhere to keep them.
    ///
    /// Started with the indexer and asked for a picture per recording after
    /// every successful reconciliation. Before this, `ThumbnailService` was
    /// referenced by nothing but its own tests: the service, the cache and the
    /// renderer were all finished and unreachable, so a shipped build made no
    /// thumbnails at all
    /// ([issue #57](https://github.com/wildware-uk/clipped/issues/57)).
    thumbnails: Option<ThumbnailService>,
    /// Makes the peaks a timeline draws, on the same terms.
    ///
    /// `clipped-waveform` was not a dependency of anything before this: it
    /// appeared in the workspace root manifest and in comments and in nobody's
    /// `[dependencies]`, so no waveform was ever generated
    /// ([issue #66](https://github.com/wildware-uk/clipped/issues/66)).
    waveforms: Option<WaveformService>,
}

/// The indexer's own state, which is all that a request touches.
#[derive(Debug, Default)]
struct IndexerState {
    /// A run has been asked for and has not started yet.
    requested: bool,
    /// A run is in flight.
    running: bool,
    /// No further run will be started.
    stopping: bool,
    /// Runs that have finished, for a test to wait on.
    completed: u64,
}

impl LibraryIndexer {
    /// An indexer for this user's library and this user's recordings folder.
    ///
    /// The recordings folder is the one `record` and `watch` write into by
    /// default (`crate::config::default_output_directory`). A recording written
    /// somewhere else — an `output` the request named — has its session record
    /// beside it and is *not* under this root, so it is not indexed; that is the
    /// same answer `watch --output-directory` gets, and which folders make up a
    /// library is issue #272's question rather than this one's.
    #[must_use]
    pub fn for_this_user() -> Self {
        let mut indexer = Self::at(
            clipped_logging::application_directory().map(|directory| directory.join(LIBRARY_FILE)),
            crate::config::default_output_directory()
                .into_iter()
                .collect(),
        );
        // Only here, and deliberately not in `at`: a test must not write
        // pictures into the cache of whoever is running it (AGENTS.md section
        // 25), and `at` is what every test uses.
        if let Some(shared) = Arc::get_mut(&mut indexer.shared) {
            shared.thumbnails = ThumbnailCache::in_default_directory()
                .map(|cache| ThumbnailService::start(cache, ServiceOptions::new()));
            shared.waveforms = WaveformCache::in_default_directory()
                .map(|cache| WaveformService::start(cache, WaveformOptions::new()));
        }
        indexer
    }

    /// An indexer for a named library and named recording folders.
    ///
    /// For tests, which must not read or write the library of whoever is
    /// running them (AGENTS.md section 25).
    #[must_use]
    pub fn at(path: Option<PathBuf>, roots: Vec<PathBuf>) -> Self {
        Self {
            shared: Arc::new(Indexer {
                path,
                settings: IndexSettings::new(roots.clone()),
                roots,
                storage: Mutex::new(clipped_session::config::StorageSettings::none()),
                control: IndexControl::new(),
                state: Mutex::new(IndexerState::default()),
                woken: Condvar::new(),
                thumbnails: None,
                waveforms: None,
            }),
            thread: Mutex::new(None),
        }
    }

    /// The same indexer, enforcing these storage limits after each run.
    ///
    /// Without it nothing is ever deleted, which is what Clipped ships with:
    /// [`StorageSettings::none`](clipped_session::config::StorageSettings::none)
    /// is unlimited and `sweep` stops before it reads a directory
    /// ([issue #111](https://github.com/wildware-uk/clipped/issues/111)).
    #[must_use]
    pub fn with_storage(self, storage: clipped_session::config::StorageSettings) -> Self {
        self.set_storage(storage);
        self
    }

    /// What the sweep is enforcing, as it now stands.
    ///
    /// Here so that "a limit saved from the window reaches the sweep" is a claim
    /// something can check. It was the assertion that could not be written when
    /// the settings were a plain field read once at start-up, and a claim
    /// nothing checks is how that arrangement survived
    /// (`serve.rs`, issue #95).
    #[must_use]
    pub fn storage(&self) -> clipped_session::config::StorageSettings {
        match self.shared.storage.lock() {
            Ok(held) => held.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Tells the sweep what the library is allowed to occupy, from now on.
    ///
    /// Called when `apply_settings` saves, so that a limit set in the window is
    /// the limit the next sweep enforces. Before this the settings were read
    /// once when `serve` started, which made the storage limits the one group of
    /// settings a save did not reach until the recorder was restarted - and
    /// nothing on screen said so, which is a control that appears to do nothing
    /// (AGENTS.md section 27).
    ///
    /// It cannot disturb a sweep in progress. The lock is taken to clone the
    /// settings at the top of a run and released before anything is measured, so
    /// a save either lands before that clone and is honoured this run, or after
    /// it and is honoured the next one. Nothing waits on this but the indexer
    /// thread, and never for longer than a `clone` (AGENTS.md section 20).
    pub fn set_storage(&self, storage: clipped_session::config::StorageSettings) {
        match self.shared.storage.lock() {
            Ok(mut held) => *held = storage,
            // A poisoned lock means a previous holder panicked while replacing
            // the settings. Nothing is deleted on the strength of a limit that
            // could not be read: the sweep keeps whatever it last had, which is
            // at worst a stale limit and never an invented one.
            Err(_) => tracing::warn!(
                "the storage limits could not be updated, so the sweep keeps the ones it has"
            ),
        }
    }

    /// The same indexer, working at a pace the caller chose.
    ///
    /// For the one test that has to have a run still going while it looks at
    /// what asking for another costs
    /// (`asking_for_a_run_does_not_wait_for_the_run_that_is_writing`). Nothing
    /// in the product sets it: the pace a recorder indexes at is
    /// [`IndexPace::background`](clipped_library::index::IndexPace::background)
    /// because a run cannot know that a game is not about to start.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_pace(mut self, pace: clipped_library::index::IndexPace) -> Self {
        if let Some(shared) = Arc::get_mut(&mut self.shared) {
            shared.settings.pace = pace;
        }
        self
    }

    /// The same indexer, keeping its pictures and its peaks in named
    /// directories.
    ///
    /// For tests, which must not read or write the thumbnail and waveform
    /// caches of whoever is running them (AGENTS.md section 25) — the reason
    /// [`Self::at`] leaves both services unstarted and only
    /// [`Self::for_this_user`] installs them.
    #[must_use]
    pub fn with_preview_caches(mut self, thumbnails: PathBuf, waveforms: PathBuf) -> Self {
        if let Some(shared) = Arc::get_mut(&mut self.shared) {
            shared.thumbnails = Some(ThumbnailService::start(
                ThumbnailCache::at(thumbnails),
                ServiceOptions::new(),
            ));
            shared.waveforms = Some(WaveformService::start(
                WaveformCache::at(waveforms),
                WaveformOptions::new(),
            ));
        }
        self
    }

    /// One recording's thumbnail, or the peaks of its sound.
    ///
    /// Answered here rather than in `crate::serve` because this is what holds
    /// the two services: they are started beside the indexer, because indexing
    /// is what asks for a picture per recording after a reconciliation
    /// (`Indexer::picture_what_was_indexed`). A window asking for one goes to
    /// the same place, which is what makes "the window asked for it" and "the
    /// sweep asked for it" the same queue rather than two
    /// ([issue #448](https://github.com/wildware-uk/clipped/issues/448)).
    ///
    /// # Errors
    ///
    /// What `crate::preview::open` refuses: a request naming no recording, and
    /// a machine with nowhere to keep either cache.
    pub fn preview(&self, request: &clipped_ipc::OpenPreview) -> Result<Preview, ProtocolError> {
        crate::preview::open(
            request,
            self.shared.thumbnails.as_ref(),
            self.shared.waveforms.as_ref(),
        )
    }

    /// Starts the thread, and asks it for the run that catches up on everything
    /// produced while nothing was indexing.
    ///
    /// Called once, by `serve`. Calling it again is a no-op rather than a second
    /// thread.
    pub fn start(&self) {
        let mut thread = match self.thread.lock() {
            Ok(thread) => thread,
            Err(poisoned) => poisoned.into_inner(),
        };
        if thread.is_some() {
            return;
        }

        let shared = Arc::clone(&self.shared);
        *thread = Some(
            std::thread::Builder::new()
                .name("clipped-library-index".to_owned())
                .spawn(move || shared.run())
                .expect("a thread can be started to index on"),
        );
        drop(thread);
        self.request();
    }

    /// Asks for the index to be brought up to date.
    ///
    /// Returns immediately. A request made while a run is in flight schedules
    /// one more run rather than a run per request, so a caller may ask as often
    /// as it likes.
    pub fn request(&self) {
        let Ok(mut state) = self.shared.state.lock() else {
            tracing::error!(
                "the library indexer was left in an unknown state by an earlier failure, so the \
                 index was not brought up to date"
            );
            return;
        };
        if state.stopping {
            return;
        }
        state.requested = true;
        drop(state);
        self.shared.woken.notify_all();
    }

    /// Stops indexing and waits for a run in progress to give up.
    ///
    /// Cancellation is cooperative and is checked between files and between
    /// transactions, so what had been written stays written and the next run
    /// carries on from there (`clipped_library::index`).
    pub fn shut_down(&self) {
        self.shared.control.cancel();
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopping = true;
            state.requested = false;
        }
        self.shared.woken.notify_all();

        let taken = match self.thread.lock() {
            Ok(mut thread) => thread.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(thread) = taken {
            let _ = thread.join();
        }
    }

    /// Waits until nothing is queued and nothing is running, for a test that has
    /// to look at what a run wrote.
    ///
    /// Answers whether it settled within `patience`. Nothing in the product
    /// waits for the index: a run is background work by construction, and a
    /// caller that blocked on one would be the stall this whole design avoids.
    #[cfg(test)]
    pub(crate) fn settled_within(&self, patience: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + patience;
        let mut state = self
            .shared
            .state
            .lock()
            .expect("a lock that is not held long");
        loop {
            if !state.requested && !state.running {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return false;
            };
            let (held, _) = self
                .shared
                .woken
                .wait_timeout(state, left)
                .expect("a lock that is not held long");
            state = held;
        }
    }

    /// How many runs have finished.
    #[cfg(test)]
    pub(crate) fn runs(&self) -> u64 {
        self.shared
            .state
            .lock()
            .expect("a lock that is not held long")
            .completed
    }
}

impl Indexer {
    /// The indexer thread: wait to be asked, reconcile, repeat.
    fn run(self: Arc<Self>) {
        let mut database = None;

        loop {
            {
                let Ok(mut state) = self.state.lock() else {
                    tracing::error!("the library indexer stopped because its state was poisoned");
                    return;
                };
                while !state.requested && !state.stopping {
                    let Ok(held) = self.woken.wait(state) else {
                        tracing::error!(
                            "the library indexer stopped because its state was poisoned"
                        );
                        return;
                    };
                    state = held;
                }
                if state.stopping {
                    return;
                }
                state.requested = false;
                state.running = true;
            }

            self.reconcile_once(&mut database);

            if let Ok(mut state) = self.state.lock() {
                state.running = false;
                state.completed += 1;
            }
            self.woken.notify_all();
        }
    }

    /// One reconciliation, opening the database if this is the first run or if
    /// the last attempt failed.
    ///
    /// A failure to open is reported and *retried* on the next run, for the
    /// reason [`LibraryReader`] retries: the usual cause is a drive that is not
    /// plugged in, and that stops being true without restarting anything.
    fn reconcile_once(&self, database: &mut Option<Database>) {
        let Some(path) = self.path.as_ref() else {
            tracing::warn!(
                "this machine describes no per-user application directory, so there is nowhere \
                 to keep a library index and nothing was indexed"
            );
            return;
        };
        if self.settings.roots.is_empty() {
            tracing::warn!("no recording folder could be worked out, so there is nothing to index");
            return;
        }

        if database.is_none() {
            match Database::open(path) {
                Ok(opened) => *database = Some(opened),
                Err(error) => {
                    tracing::warn!(
                        library = %clipped_logging::RedactedPath::new(path),
                        %error,
                        "the recording library could not be opened, so the index was not brought \
                         up to date; the next recording will try again"
                    );
                    return;
                }
            }
        }

        let open = database.as_mut().expect("the database was just opened");
        match reconcile(open, &self.settings, &self.control, SystemTime::now()) {
            Ok(report) => {
                report_unindexed(&report);
                self.picture_what_was_indexed(open);
                self.enforce_storage_limits(open);
            }
            Err(error) => {
                // The connection is dropped rather than kept: a database that
                // refused may have gone with its drive, and the next run should
                // open it again rather than reuse a handle to something that is
                // no longer there.
                *database = None;
                tracing::warn!(
                    %error,
                    "the library index could not be brought up to date; the next recording will \
                     try again"
                );
            }
        }
    }
}

impl Indexer {
    /// Enforces the configured storage limits, if there are any.
    ///
    /// After the reconciliation and on the same thread, which is the whole
    /// reason it is here rather than at the end of a recording: the sweep walks
    /// directories, and a recording that had to wait for one before it could
    /// report itself finished would be a recording that looks like it hung. The
    /// indexer already runs off the recording path and is already asked for a
    /// run when a recording ends.
    ///
    /// Nothing here fails a reconciliation. A library that could not be swept is
    /// a library that is over its limit, which is a smaller problem than an
    /// index that did not update — and every reason it could not is logged by
    /// `sweep` against the thing that caused it (AGENTS.md section 17).
    fn enforce_storage_limits(&self, database: &mut Database) {
        // The first root is where this recorder writes. A library spread over
        // several folders is issue #272's question, and so is which of them a
        // sweep should measure; this measures the one Clipped records into,
        // which is the one that grows.
        let Some(recordings) = self.roots.first() else {
            return;
        };

        // Cloned out from under the lock and the lock released, so that a save
        // arriving mid-sweep waits on nothing (`LibraryIndexer::set_storage`).
        let storage = match self.storage.lock() {
            Ok(held) => held.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };

        let mut configuration = clipped_session::config::Configuration::defaults();
        configuration.set_storage(storage);

        let report = clipped_session::cleanup::sweep(
            &configuration,
            recordings,
            database,
            SystemTime::now(),
        );

        match &report.skipped {
            // The ordinary case on a machine nobody has set a limit on, and it
            // is `debug` rather than `info` because it happens after every run
            // and says that nothing happened.
            Some(clipped_session::cleanup::Skipped::NoLimit) => {}
            Some(reason) => tracing::warn!(
                ?reason,
                "the configured storage limits could not be enforced this run"
            ),
            None if report.deleted > 0 => tracing::info!(
                deleted = report.deleted,
                reclaimed_bytes = report.reclaimed_bytes,
                refused = report.refused,
                "automatic cleanup moved recordings to the trash to stay inside the storage limits"
            ),
            None => {}
        }
    }

    /// Asks for a picture and a waveform of every recording the index holds.
    ///
    /// After the reconciliation rather than during it, and by
    /// [`ThumbnailService::request`] rather than
    /// [`ThumbnailService::thumbnail`], because a scan has thousands of files
    /// and wants none of the pictures yet — the service reads its own cache and
    /// does the work on a background thread at the lowest priority it can get
    /// (`clipped_library::thumbnail`).
    ///
    /// Nothing here waits, and nothing here fails a reconciliation. A recording
    /// with no picture is a tile with no picture, which is a smaller problem
    /// than an index that did not update (AGENTS.md section 17).
    fn picture_what_was_indexed(&self, database: &Database) {
        if self.thumbnails.is_none() && self.waveforms.is_none() {
            return;
        }
        let recordings = match clipped_library::thumbnail::recordings_worth_reading(database) {
            Ok(recordings) => recordings,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the library index could not be asked which recordings need a picture, so \
                     none were made this time"
                );
                return;
            }
        };

        let asked = recordings.len();
        for recording in recordings {
            if let Some(service) = self.thumbnails.as_ref() {
                let _ = service.request(&recording);
            }
            if let Some(service) = self.waveforms.as_ref() {
                let _ = service.request(&recording);
            }
        }
        if asked > 0 {
            tracing::debug!(
                recordings = asked,
                "asked for a picture and a waveform of every recording the index holds"
            );
        }
    }
}

/// Says what happened to media files no session claims.
///
/// `reconcile` logs everything else about a run. This is said here because it is
/// the one part of the report that is about *the user's files* rather than about
/// the index: recordings made by a build whose `serve` wrote no session record
/// are exactly this case, and they are left alone rather than adopted, deleted or
/// renamed (AGENTS.md section 56). Recovering them is
/// [issue #272](https://github.com/wildware-uk/clipped/issues/272).
fn report_unindexed(report: &IndexReport) {
    if report.unindexed_media == 0 {
        return;
    }
    tracing::info!(
        files = report.unindexed_media,
        first = ?report
            .unindexed_sample
            .iter()
            .map(|path| clipped_logging::RedactedPath::new(path).to_string())
            .collect::<Vec<String>>(),
        "recordings under the recording folders belong to no session record, so the library \
         cannot say what they are; they have been left exactly where they are (issue #272)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;
    use clipped_storage::rusqlite::params;

    /// An empty directory of this test's own, removed again when the test that
    /// made it passes.
    ///
    /// This used to return a bare [`PathBuf`] and nothing ever removed it,
    /// which is where 3,312 of the `clipped-recorder-*` directories counted in
    /// [issue #598](https://github.com/wildware-uk/clipped/issues/598) came
    /// from — eleven a run, every run, since these tests were written.
    fn scratch_directory(name: &str) -> Scratch {
        Scratch::new(&format!("recorder-{name}"))
    }

    /// A [`LibraryReader`] and the scratch directory its database sits in.
    ///
    /// The helpers below used to return the reader alone, which dropped the
    /// directory at the end of the helper — so nothing removed it, and nothing
    /// could have, because the test had not finished with it. Returning both
    /// keeps the directory alive for as long as the test that reads it.
    ///
    /// The field order is load-bearing. [`LibraryReader`] caches the
    /// [`Database`] it opens, and Windows refuses to remove a file that is
    /// still open; a struct's fields are dropped in declaration order, so the
    /// reader has to be declared first. Get it the wrong way round and the
    /// removal fails — which [`Scratch`] now says out loud rather than
    /// swallowing, the defect PR #597 was written to expose.
    struct TestLibrary {
        reader: LibraryReader,
        _directory: Scratch,
    }

    impl std::ops::Deref for TestLibrary {
        type Target = LibraryReader;

        fn deref(&self) -> &LibraryReader {
            &self.reader
        }
    }

    /// A reader over a library of one sitting, whose file has gone.
    fn library_with_a_missing_recording(name: &str) -> TestLibrary {
        let directory = scratch_directory(name);
        let path = directory.join("library.db");
        {
            let database = Database::open(&path).expect("a database opens");
            let connection = database.connection();
            connection
                .execute(
                    "INSERT INTO games (game_id, name, first_seen_at) \
                     VALUES ('cs2', 'Counter-Strike 2', '2026-08-11T20:14:00+01:00')",
                    [],
                )
                .expect("a game inserts");
            connection
                .execute(
                    "INSERT INTO sessions (session_id, game_id, started_at) \
                     VALUES ('cs2-20260811-201400', 'cs2', '2026-08-11T20:14:00+01:00')",
                    [],
                )
                .expect("a session inserts");
            connection
                .execute(
                    "INSERT INTO recordings \
                        (session_id, session_index, path, started_at, duration_seconds, \
                         size_bytes, missing_since) \
                     VALUES ('cs2-20260811-201400', 1, ?1, '2026-08-11T20:14:00+01:00', 6540.0, \
                             9812009112, '2026-08-12T09:00:00+01:00')",
                    params![r"D:\clips\cs2-20260811-201400-1.mkv"],
                )
                .expect("a recording inserts");
        }
        TestLibrary {
            reader: LibraryReader::at(Some(path)),
            _directory: directory,
        }
    }

    #[test]
    fn a_recording_whose_file_has_gone_reaches_the_window_saying_so() {
        // Issue #305's second acceptance criterion, at the boundary rather than
        // in the index: the row has to be *on the wire*, carrying
        // `missing_since`, or the screen cannot say the file has gone.
        let library = library_with_a_missing_recording("library-missing");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");

        assert_eq!(page.sessions.len(), 1);
        let recording = &page.sessions[0].recordings[0];
        assert_eq!(
            recording.missing_since.as_deref(),
            Some("2026-08-12T09:00:00+01:00"),
            "a missing recording must be reported as missing rather than omitted"
        );
        assert_eq!(recording.size_bytes, Some(9_812_009_112));
        assert_eq!(
            page.sessions[0].game_name.as_deref(),
            Some("Counter-Strike 2")
        );
    }

    #[test]
    fn the_per_game_figures_cross_the_boundary_including_what_is_missing() {
        let library = library_with_a_missing_recording("library-games");

        let games = library.games().expect("the library reads");

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].game_id.as_deref(), Some("cs2"));
        assert_eq!(games[0].sessions, 1);
        assert_eq!(games[0].recordings, 1);
        assert_eq!(games[0].missing, 1);
        assert_eq!(
            games[0].bytes, 0,
            "a file nobody can find is not occupying the space it used to"
        );
    }

    /// A format 1 document, carrying the `soloed` version 2 drops.
    ///
    /// Taken from `crates/edit`'s own migration test rather than invented, so
    /// that "an older document" here means the same thing it means there.
    const A_FORMAT_ONE_DOCUMENT: &str = r#"{
      "schema_version": 1,
      "title": "Ace",
      "aspect_ratio": null,
      "sources": [{ "id": 0, "recording": "1" }],
      "segments": [
        {
          "source": 0,
          "span": { "start": 0, "end": 10000000000 },
          "speed": { "numerator": 1, "denominator": 1 },
          "crop": null,
          "rotation": "none"
        }
      ],
      "audio_tracks": [
        {
          "name": "Game",
          "inputs": [{ "source": 0, "stream": 0 }],
          "gain_db": -3.5,
          "muted": false,
          "soloed": false,
          "fade_in": 0,
          "fade_out": 1000000000
        }
      ],
      "overlays": []
    }"#;

    /// A document from a build ahead of this one.
    ///
    /// The version is deliberately far ahead rather than `SCHEMA_VERSION + 1`,
    /// so that the test keeps meaning "newer" after the next migration ships.
    const A_DOCUMENT_FROM_THE_FUTURE: &str = r#"{
      "schema_version": 9999,
      "title": "Made on the machine that was up to date",
      "sources": [],
      "segments": []
    }"#;

    /// A library holding one sitting, one recording and one clip.
    ///
    /// Written as SQL for the reason `library_with_an_unexported_highlight` is:
    /// what is under test is the read and the write, and the row is the row
    /// whichever writer made it. `edit` is the parameter because every case
    /// below differs only in what is in that column.
    fn library_with_a_clip(name: &str, edit: Option<&str>) -> TestLibrary {
        let directory = scratch_directory(name);
        let path = directory.join("library.db");
        {
            let database = Database::open(&path).expect("a database opens");
            let connection = database.connection();
            connection
                .execute(
                    "INSERT INTO sessions (session_id, started_at) VALUES ('sitting', ?1)",
                    params!["2026-08-11T20:14:00+01:00"],
                )
                .expect("the sitting is written");
            connection
                .execute(
                    "INSERT INTO recordings \
                         (recording_id, session_id, session_index, path, started_at, \
                          duration_seconds) \
                     VALUES (1, 'sitting', 1, ?1, ?2, 120.0)",
                    params![r"D:\clips\mirage.mkv", "2026-08-11T20:14:00+01:00"],
                )
                .expect("the recording is written");
            connection
                .execute(
                    "INSERT INTO clips \
                         (clip_id, session_id, source_recording_id, title, created_at, \
                          source_start_seconds, source_end_seconds, duration_seconds, edit) \
                     VALUES (3, 'sitting', 1, 'Ace on Mirage', ?1, 4.0, 34.0, 30.0, ?2)",
                    params!["2026-08-11T21:02:00+01:00", edit],
                )
                .expect("the clip is written");
        }

        TestLibrary {
            reader: LibraryReader::at(Some(path)),
            _directory: directory,
        }
    }

    /// What `clips.edit` holds now.
    fn stored_edit(library: &TestLibrary, clip: i64) -> Option<String> {
        library
            .with_database(|database| {
                database
                    .connection()
                    .query_row("SELECT edit FROM clips WHERE clip_id = ?1", [clip], |row| {
                        row.get(0)
                    })
                    .map_err(unreadable)
            })
            .expect("the clip is readable")
    }

    /// What was kept when a converted document replaced an older one.
    fn superseded_edit(library: &TestLibrary, clip: i64) -> Option<String> {
        library
            .with_database(|database| {
                database
                    .connection()
                    .query_row(
                        "SELECT edit_superseded FROM clips WHERE clip_id = ?1",
                        [clip],
                        |row| row.get(0),
                    )
                    .map_err(unreadable)
            })
            .expect("the clip is readable")
    }

    /// Asking for one clip's document.
    fn ask(library: &TestLibrary, clip: &str) -> Result<ClipDocument, ProtocolError> {
        library.clip_document(&LibraryClipDocument {
            clip: clip.to_owned(),
        })
    }

    /// Saving one.
    fn save(
        library: &TestLibrary,
        clip: &str,
        document: &str,
    ) -> Result<ClipDocumentSaved, ProtocolError> {
        library.save_clip_document(
            &SaveClipDocument {
                clip: clip.to_owned(),
                document: document.to_owned(),
            },
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_786_000_000),
        )
    }

    #[test]
    fn a_stored_document_reaches_the_window_with_what_is_in_it() {
        // The acceptance criterion this whole command exists for, and the
        // assertion is deliberately about the *contents*: a test that only
        // checked the call succeeded would pass against a build that answered
        // with an empty document, which is exactly the state issue #306 says
        // must not be drawn as a clip (AGENTS.md section 27).
        let stored = EditDocument::from_recording(
            "Ace on Mirage",
            RecordingId::new("1"),
            SourceSpan::new(
                SourceTime::from_nanos(4_000_000_000),
                SourceTime::from_nanos(34_000_000_000),
            )
            .expect("a span"),
        )
        .write()
        .expect("it writes");
        let library = library_with_a_clip("clip-document-stored", Some(&stored));

        let answer = ask(&library, "3").expect("a clip with a document opens");

        assert_eq!(answer.clip, "3");
        assert_eq!(answer.converted_from, None, "nothing needed converting");
        assert!(
            !answer.synthesised,
            "this document was stored, not built here"
        );

        let document = EditDocument::read(&answer.document)
            .expect("what crossed the boundary is a document")
            .document;
        assert_eq!(
            document.sources.len(),
            1,
            "the clip plays one recording and the answer has to carry it"
        );
        assert_eq!(
            document.sources[0].recording,
            RecordingId::new("1"),
            "the answer names the recording the clip was cut from"
        );
        assert_eq!(
            document.segments.len(),
            1,
            "the clip is one segment and the answer has to carry it"
        );
        assert_eq!(
            document.segments[0].span.start(),
            SourceTime::from_nanos(4_000_000_000),
            "the segment starts where the stored document said"
        );
        assert_eq!(
            document.segments[0].span.end(),
            SourceTime::from_nanos(34_000_000_000),
            "and ends where it said"
        );
    }

    #[test]
    fn a_clip_nobody_has_edited_is_given_a_starting_document_by_the_recorder() {
        // A saved replay: `clips.edit` is NULL. Somebody has to decide what
        // "unedited" means and it is this process, once — the window inventing
        // its own would mean two builds disagreeing the first time one was
        // saved.
        let library = library_with_a_clip("clip-document-synthesised", None);

        let answer = ask(&library, "3").expect("a replay opens in the editor");

        assert!(
            answer.synthesised,
            "the window has to be able to say this clip has never been edited"
        );
        assert_eq!(answer.converted_from, None);

        let document = EditDocument::read(&answer.document)
            .expect("a built document is a document")
            .document;
        assert_eq!(document.title, "Ace on Mirage", "the clip keeps its name");
        // Named before anything is indexed, and this is the assertion that
        // matters. A build that answered with an *empty* document — no sources,
        // no segments — is a build the editor draws as a clip with a dead
        // playhead, and it is indistinguishable from a broken one. Left to
        // `document.segments[0]` the failure would read "index out of bounds",
        // which names nothing.
        assert_eq!(
            document.sources.len(),
            1,
            "a starting document declares the recording the clip was cut from, and this one \
             declares none, so the editor would open a clip that plays nothing"
        );
        assert_eq!(
            document.sources[0].recording,
            RecordingId::new("1"),
            "and it has to be the recording the clip's row names"
        );
        assert_eq!(
            document.segments.len(),
            1,
            "a starting document has the one segment that is the clip, and this one has none, \
             so the editor would draw an empty timeline"
        );
        assert_eq!(
            document.segments[0].span.start(),
            SourceTime::from_nanos(4_000_000_000),
            "the starting document is the window the clip was cut from, not the whole recording"
        );
        assert_eq!(
            document.segments[0].span.end(),
            SourceTime::from_nanos(34_000_000_000)
        );

        assert_eq!(
            stored_edit(&library, 3),
            None,
            "opening a clip must not write one; the row is untouched until somebody saves"
        );
    }

    #[test]
    fn an_older_document_is_converted_before_it_crosses_and_says_it_was() {
        // The window refuses a document older than its own build, on purpose
        // (`apps/desktop/src/editor/document.ts`): it cannot store the
        // conversion, so converting there would show a document nothing agreed
        // to. That refusal is only correct because of this — the window never
        // receives one.
        let library = library_with_a_clip("clip-document-converted", Some(A_FORMAT_ONE_DOCUMENT));

        let answer = ask(&library, "3").expect("a format 1 clip still opens");

        assert_eq!(
            answer.converted_from,
            Some(1),
            "the window is told the stored text is older than what it was given"
        );
        assert!(
            !answer.document.contains("soloed"),
            "the converted document must not carry a solo anywhere: {}",
            answer.document
        );
        assert!(
            answer.document.contains(r#""schema_version": 2"#),
            "what crossed has to be the version this build writes: {}",
            answer.document
        );
        assert_eq!(
            stored_edit(&library, 3).as_deref(),
            Some(A_FORMAT_ONE_DOCUMENT),
            "reading converts in memory only; the stored text is still the original"
        );
    }

    #[test]
    fn a_document_from_a_newer_build_is_refused_and_says_to_update() {
        // The case that must never be smoothed over. Opening it as best it can
        // and saving it back is how somebody loses the edit they made on the
        // machine that was up to date (AGENTS.md sections 43 and 56).
        let library = library_with_a_clip("clip-document-newer", Some(A_DOCUMENT_FROM_THE_FUTURE));

        let refusal = ask(&library, "3").expect_err("a newer document is not readable");

        assert_eq!(refusal.code, ErrorCode::EditUnreadable);
        assert!(
            refusal.message.contains("Update Clipped"),
            "the refusal has to say what to do about it: {}",
            refusal.message
        );
        assert!(
            refusal.message.contains("Nothing has been changed"),
            "and that nothing was touched: {}",
            refusal.message
        );
        assert_eq!(
            stored_edit(&library, 3).as_deref(),
            Some(A_DOCUMENT_FROM_THE_FUTURE),
            "and nothing was"
        );
    }

    #[test]
    fn a_clip_this_library_does_not_have_is_refused_by_number() {
        let library = library_with_a_clip("clip-document-absent", None);

        let refusal = ask(&library, "404").expect_err("there is no clip 404");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("404"),
            "the refusal names what was asked for: {}",
            refusal.message
        );
    }

    #[test]
    fn an_edited_document_is_stored_and_comes_back() {
        // The other half of the round trip, end to end through the two methods
        // a window has: what was saved is what opens.
        let library = library_with_a_clip("clip-document-save", None);
        let opened = ask(&library, "3").expect("a replay opens");
        let mut document = EditDocument::read(&opened.document)
            .expect("it is a document")
            .document;
        document.title = "Ace on Mirage, trimmed".to_owned();
        let edited = document.write().expect("it writes");

        let saved = save(&library, "3", &edited).expect("an edit saves");

        assert_eq!(saved.clip, "3");
        assert_eq!(
            saved.superseded, None,
            "there was no stored document to keep"
        );

        let reopened = ask(&library, "3").expect("the saved clip opens");
        assert!(
            !reopened.synthesised,
            "the clip now has a document of its own"
        );
        assert_eq!(
            EditDocument::read(&reopened.document)
                .expect("it is a document")
                .document
                .title,
            "Ace on Mirage, trimmed",
            "the edit that was saved is the edit that comes back"
        );
    }

    #[test]
    fn saving_over_an_older_document_keeps_the_older_text() {
        // `docs/editing.md`: "the caller decides whether to store the result,
        // and must keep the original when it does". This is the caller, and
        // this is it doing so.
        let library = library_with_a_clip("clip-document-supersede", Some(A_FORMAT_ONE_DOCUMENT));
        let opened = ask(&library, "3").expect("a format 1 clip opens");

        let saved = save(&library, "3", &opened.document).expect("the converted document saves");

        assert_eq!(
            saved.superseded,
            Some(1),
            "the reply has to say the older text was kept"
        );
        assert_eq!(
            superseded_edit(&library, 3).as_deref(),
            Some(A_FORMAT_ONE_DOCUMENT),
            "and the kept text is the original, byte for byte"
        );
        assert!(
            stored_edit(&library, 3).is_some_and(|text| text.contains(r#""schema_version": 2"#)),
            "while the clip itself is now at the version this build writes"
        );
    }

    #[test]
    fn a_second_save_does_not_overwrite_the_original_that_was_kept() {
        // The kept text is the only copy of a document this build could not
        // have produced. Replacing it on the next save with text this build
        // wrote would destroy exactly the thing it exists for.
        let library =
            library_with_a_clip("clip-document-supersede-twice", Some(A_FORMAT_ONE_DOCUMENT));
        let opened = ask(&library, "3").expect("a format 1 clip opens");
        save(&library, "3", &opened.document).expect("the first save");

        let mut document = EditDocument::read(&opened.document)
            .expect("it is a document")
            .document;
        document.title = "Edited again".to_owned();
        let again = document.write().expect("it writes");

        let saved = save(&library, "3", &again).expect("the second save");

        assert_eq!(
            saved.superseded, None,
            "this save kept nothing; an earlier one did, and saying otherwise would be a claim \
             about a different text"
        );
        assert_eq!(
            superseded_edit(&library, 3).as_deref(),
            Some(A_FORMAT_ONE_DOCUMENT),
            "the format 1 original is still there, unchanged by the second save"
        );
    }

    #[test]
    fn a_document_the_recorder_cannot_parse_is_refused_and_nothing_is_stored() {
        // A window that sent nonsense must not be able to corrupt a clip. It
        // can only fail to save, and the refusal says the clip is untouched so
        // that the honest thing to do is retry rather than give up.
        let stored = EditDocument::from_recording(
            "Ace on Mirage",
            RecordingId::new("1"),
            SourceSpan::new(
                SourceTime::from_nanos(0),
                SourceTime::from_nanos(30_000_000_000),
            )
            .expect("a span"),
        )
        .write()
        .expect("it writes");
        let library = library_with_a_clip("clip-document-nonsense", Some(&stored));

        let refusal = save(&library, "3", "{ this is not JSON")
            .expect_err("text that is not a document does not save");

        assert_eq!(refusal.code, ErrorCode::EditUnreadable);
        assert!(
            refusal.message.contains("The clip is exactly as it was"),
            "the refusal has to say nothing was stored: {}",
            refusal.message
        );
        assert_eq!(
            stored_edit(&library, 3).as_deref(),
            Some(stored.as_str()),
            "and nothing was"
        );
    }

    #[test]
    fn a_document_at_a_version_the_recorder_does_not_know_is_refused_on_save() {
        let library = library_with_a_clip("clip-document-save-newer", None);

        let refusal = save(&library, "3", A_DOCUMENT_FROM_THE_FUTURE)
            .expect_err("a document from a newer build does not save");

        assert_eq!(refusal.code, ErrorCode::EditUnreadable);
        assert!(
            refusal.message.contains("Update Clipped"),
            "the refusal has to say what to do: {}",
            refusal.message
        );
        assert_eq!(
            stored_edit(&library, 3),
            None,
            "and the clip is still the one nobody has edited"
        );
    }

    /// The trash a reader reports is the one a deletion goes to.
    ///
    /// `for_this_user` derived it from `default_output_directory` alone, so a
    /// configured recording directory or a configured `trash_directory` was
    /// ignored and `library_trash` could name a path nothing deletes into
    /// ([issue #646](https://github.com/wildware-uk/clipped/issues/646)).
    ///
    /// Asserted against `clipped_session::cleanup::trash_directory` rather than
    /// against a path spelled out here: that function is the one a deletion
    /// uses, and a second spelling of the rule in this test is how the two
    /// would start disagreeing again (AGENTS.md section 55).
    #[test]
    fn the_trash_a_reader_reports_is_the_one_a_deletion_uses() {
        use clipped_session::config::{Configuration, StorageSettings};

        let recordings = std::path::Path::new(r"D:\Somewhere\Clips");
        let elsewhere = std::path::Path::new(r"E:\A trash of my own");

        // A configured recording directory, with no trash named: the trash sits
        // beside the recordings, wherever those are.
        let mut storage = StorageSettings::default();
        storage
            .set_recording_directory(Some(recordings.to_path_buf()))
            .expect("a plain directory is a valid recording directory");
        let mut configuration = Configuration::defaults();
        configuration.set_storage(storage.clone());

        assert_eq!(
            LibraryReader::for_this_user(&configuration).trash_directory,
            Some(clipped_session::cleanup::trash_directory(
                &configuration,
                recordings
            )),
            "a reader has to name the trash the configured recordings deletion goes to"
        );

        // And a trash named outright, which beats the recordings entirely.
        storage
            .set_trash_directory(Some(elsewhere.to_path_buf()))
            .expect("a plain directory is a valid trash directory");
        configuration.set_storage(storage);

        assert_eq!(
            LibraryReader::for_this_user(&configuration).trash_directory,
            Some(elsewhere.to_path_buf()),
            "a trash directory somebody chose is the one to report"
        );
    }

    #[test]
    fn a_clip_in_the_trash_is_not_opened_or_saved_to() {
        // Editing something on its way out would offer work the next empty
        // destroys. Putting it back first is the honest order and the user can
        // do it (issue #450).
        let library = library_with_a_clip("clip-document-trashed", None);
        library
            .with_database(|database| {
                database
                    .connection()
                    .execute(
                        "UPDATE clips SET deleted_at = ?1 WHERE clip_id = 3",
                        params!["2026-08-14T10:00:00+01:00"],
                    )
                    .map_err(unreadable)
            })
            .expect("the clip is deleted");

        let refused_open = ask(&library, "3").expect_err("a deleted clip does not open");
        assert_eq!(refused_open.code, ErrorCode::InvalidParameters);
        assert!(
            refused_open.message.contains("Restore it"),
            "the refusal names the way out: {}",
            refused_open.message
        );

        let refused_save = save(&library, "3", A_FORMAT_ONE_DOCUMENT)
            .expect_err("and does not take an edit either");
        assert_eq!(refused_save.code, ErrorCode::InvalidParameters);
    }

    #[test]
    fn an_empty_library_answers_an_empty_page_rather_than_a_refusal() {
        // The distinction issue #305's last acceptance criterion turns on. This
        // is the empty half; `a_library_that_cannot_be_opened_says_so` is the
        // other, and they must not be the same reply.
        // `directory` is bound rather than used in place: a `Scratch` used as a
        // temporary is dropped at the end of the statement, taking the
        // directory the reader is about to be pointed at with it.
        let directory = scratch_directory("library-empty");
        let library = LibraryReader::at(Some(directory.join("library.db")));

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("an empty library is not a failure");

        assert!(page.sessions.is_empty());
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn a_library_that_cannot_be_opened_says_so_rather_than_looking_empty() {
        // A file that is not a Clipped database stands in for every reason the
        // index cannot be read — a drive that is not plugged in, a database
        // from a newer build, a corrupt file. What matters is that none of them
        // reaches the window as "you have not recorded anything".
        let directory = scratch_directory("library-unopenable");
        let path = directory.join("library.db");
        std::fs::write(&path, b"this is not a database").expect("the file is written");
        let library = LibraryReader::at(Some(path));

        let refusal = library
            .sessions(&LibrarySessions::default())
            .expect_err("an unreadable library is not an empty one");

        assert_eq!(refusal.code, ErrorCode::LibraryUnavailable);
        assert!(
            refusal.message.contains("could not be opened"),
            "the refusal has to say what went wrong: {}",
            refusal.message
        );
    }

    /// A reader over one sitting with two clips: one exported, one not.
    ///
    /// Written as SQL because what is under test is the read, and the row a
    /// generated highlight leaves is a `path` of NULL whichever writer made it
    /// (`0004_clips_without_a_file.sql`).
    fn library_with_an_unexported_highlight(name: &str) -> TestLibrary {
        let directory = scratch_directory(name);
        let path = directory.join("library.db");
        {
            let database = Database::open(&path).expect("a database opens");
            let connection = database.connection();
            connection
                .execute(
                    "INSERT INTO sessions (session_id, started_at) \
                     VALUES ('cs2-20260811-201400', '2026-08-11T20:14:00+01:00')",
                    [],
                )
                .expect("a session inserts");
            connection
                .execute(
                    "INSERT INTO recordings (session_id, session_index, path, started_at) \
                     VALUES ('cs2-20260811-201400', 1, ?1, '2026-08-11T20:14:00+01:00')",
                    params![r"D:\clips\cs2-20260811-201400-1.mkv"],
                )
                .expect("a recording inserts");
            connection
                .execute(
                    "INSERT INTO clips (session_id, path, created_at, origin) \
                     VALUES ('cs2-20260811-201400', ?1, '2026-08-11T20:41:00+01:00', \
                             'replay-buffer')",
                    params![r"D:\clips\ace-on-mirage.mkv"],
                )
                .expect("the saved replay inserts");
            connection
                .execute(
                    "INSERT INTO clips (session_id, path, created_at, origin, origin_detail) \
                     VALUES ('cs2-20260811-201400', NULL, '2026-08-11T20:45:00+01:00', \
                             'highlight', '{\"kind\":\"kill\"}')",
                    [],
                )
                .expect("the generated highlight inserts");
        }
        TestLibrary {
            reader: LibraryReader::at(Some(path)),
            _directory: directory,
        }
    }

    #[test]
    fn a_clip_with_no_file_crosses_the_boundary_with_no_path_rather_than_failing_the_page() {
        // Issue #591, at the boundary. The index half is
        // `clipped-library`'s `a_clip_with_no_file_is_listed_rather_than_failing_the_listing`;
        // this is what the window actually receives, asserted **on the bytes of
        // the frame** rather than on a parsed reply. #576 and #586 were both
        // fields that a parsed reply could not tell from present, so a mirror
        // agreeing with a `LibraryClip` this process built proves nothing about
        // what went down the pipe.
        let library = library_with_an_unexported_highlight("library-unexported");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("a sitting with an unexported highlight lists");

        let frame = {
            let mut bytes = Vec::new();
            clipped_ipc::frame::write_message(
                &mut bytes,
                &clipped_ipc::ServerMessage::Response(clipped_ipc::Response {
                    id: 1,
                    outcome: clipped_ipc::Outcome::Ok(clipped_ipc::Reply::LibrarySessions { page }),
                }),
            )
            .expect("the page fits in a frame");
            bytes
        };

        // The frame as the peer reads it: a little-endian length and then the
        // JSON, parsed here with nothing of this build's own types involved.
        let declared = u32::from_le_bytes(
            frame[..clipped_ipc::LENGTH_PREFIX_BYTES]
                .try_into()
                .expect("a frame carries its length"),
        ) as usize;
        let body = &frame[clipped_ipc::LENGTH_PREFIX_BYTES..];
        assert_eq!(declared, body.len(), "the frame's length is its payload's");

        let sent: serde_json::Value = serde_json::from_slice(body).expect("the frame carries JSON");
        let clips = sent["outcome"]["ok"]["page"]["sessions"][0]["clips"]
            .as_array()
            .expect("the clips are on the wire");
        assert_eq!(
            clips.len(),
            2,
            "a clip with no file is a clip somebody made and must be sent, not dropped: {sent}"
        );

        assert_eq!(
            clips[0]["path"].as_str(),
            Some(r"D:\clips\ace-on-mirage.mkv"),
            "the exported clip lost its file on the wire: {}",
            clips[0]
        );

        let highlight = clips[1].as_object().expect("a clip is an object");
        assert!(
            !highlight.contains_key("path"),
            "a clip with no file must carry no `path` key at all — a null or an empty string \
             would be a file name a window would try to open: {highlight:?}"
        );
        assert!(
            !highlight.contains_key("missing_since"),
            "a clip nothing has exported has no file to have gone, and must not reach a window \
             looking like one whose file was lost: {highlight:?}"
        );
        assert_eq!(
            highlight["clip_id"].as_i64(),
            Some(2),
            "the highlight arrived without the identifier everything else names it by"
        );

        // Nothing else in the sitting may be lost by tolerating the pathless
        // clip (AGENTS.md section 56).
        assert_eq!(
            sent["outcome"]["ok"]["page"]["sessions"][0]["recordings"][0]["path"].as_str(),
            Some(r"D:\clips\cs2-20260811-201400-1.mkv"),
            "the recording went missing from the frame: {sent}"
        );
    }

    /// A reader over a trash holding a recording and a clip that has no file.
    ///
    /// Written as SQL because what is under test is the *read*: the row a
    /// generated highlight leaves has `path` NULL whichever writer made it
    /// (`0004_clips_without_a_file.sql`), and the deleted recording beside it
    /// is there so that the frame can be checked for losing anything else.
    fn trash_holding_a_clip_with_no_file(name: &str) -> TestLibrary {
        let directory = scratch_directory(name);
        let path = directory.join("library.db");
        {
            let database = Database::open(&path).expect("a database opens");
            let connection = database.connection();
            connection
                .execute(
                    "INSERT INTO sessions (session_id, started_at) \
                     VALUES ('cs2-20260811-201400', '2026-08-11T20:14:00+01:00')",
                    [],
                )
                .expect("a session inserts");
            connection
                .execute(
                    "INSERT INTO recordings \
                        (session_id, session_index, path, started_at, size_bytes, deleted_at, \
                         deleted_from) \
                     VALUES ('cs2-20260811-201400', 1, ?1, '2026-08-11T20:14:00+01:00', 4096, \
                             '2026-08-15T09:00:00+01:00', ?2)",
                    params![
                        r"D:\Clips.trash\cs2-20260811-201400-1.mkv",
                        r"D:\Clips\cs2-20260811-201400-1.mkv"
                    ],
                )
                .expect("a deleted recording inserts");
            connection
                .execute(
                    "INSERT INTO clips \
                        (session_id, path, created_at, origin, origin_detail, deleted_at) \
                     VALUES ('cs2-20260811-201400', NULL, '2026-08-11T20:45:00+01:00', \
                             'highlight', '{\"kind\":\"kill\"}', '2026-08-16T09:00:00+01:00')",
                    [],
                )
                .expect("a deleted highlight with no file inserts");
            // And one the user has not deleted, so that the listing can be
            // shown to carry the trashed rows and only those, and so that
            // something can be asked to restore that is not in the trash.
            connection
                .execute(
                    "INSERT INTO clips (session_id, path, created_at, origin, origin_detail) \
                     VALUES ('cs2-20260811-201400', NULL, '2026-08-11T20:47:00+01:00', \
                             'highlight', '{\"kind\":\"headshot\"}')",
                    [],
                )
                .expect("a live highlight with no file inserts");
        }
        TestLibrary {
            reader: LibraryReader::at(Some(path)),
            _directory: directory,
        }
    }

    /// The JSON one `ServerMessage` becomes on the wire, read back with none of
    /// this build's own types involved.
    ///
    /// The length prefix is checked against the payload, so what is asserted on
    /// is a frame a peer could actually read rather than a `serde_json::Value`
    /// this process happened to make.
    fn frame_of(message: &clipped_ipc::ServerMessage) -> serde_json::Value {
        let mut frame = Vec::new();
        clipped_ipc::frame::write_message(&mut frame, message).expect("the reply fits in a frame");
        let declared = u32::from_le_bytes(
            frame[..clipped_ipc::LENGTH_PREFIX_BYTES]
                .try_into()
                .expect("a frame carries its length"),
        ) as usize;
        let body = &frame[clipped_ipc::LENGTH_PREFIX_BYTES..];
        assert_eq!(declared, body.len(), "the frame's length is its payload's");
        serde_json::from_slice(body).expect("the frame carries JSON")
    }

    #[test]
    fn a_clip_with_no_file_in_the_trash_reaches_the_window_with_no_path_rather_than_no_trash() {
        // Issue #593 at the boundary. The index half is `clipped-library`'s
        // `a_clip_with_no_file_is_deleted_listed_and_restored_like_anything_else`;
        // this is what the window actually receives, asserted **on the bytes of
        // the frame** rather than on a parsed reply. #576 and #586 were both
        // fields whose absence a parsed reply could not tell from their
        // presence, so a mirror agreeing with a `TrashedItem` this process
        // built would prove nothing about what went down the pipe.
        let library = trash_holding_a_clip_with_no_file("trash-pathless");

        let listing = library
            .trash()
            .expect("a trash holding a clip with no file still lists");

        let sent = frame_of(&clipped_ipc::ServerMessage::Response(
            clipped_ipc::Response {
                id: 1,
                outcome: clipped_ipc::Outcome::Ok(clipped_ipc::Reply::LibraryTrash {
                    trash: listing,
                }),
            },
        ));
        let items = sent["outcome"]["ok"]["trash"]["items"]
            .as_array()
            .expect("the trash is on the wire");
        assert_eq!(
            items.len(),
            2,
            "an item with no file is an item somebody deleted and must be sent, not dropped: {sent}"
        );

        // Newest deletion first, so the clip leads.
        let clip = items[0].as_object().expect("a trashed item is an object");
        assert_eq!(clip["kind"].as_str(), Some("clip"), "{clip:?}");
        assert!(
            !clip.contains_key("path"),
            "an item with no file must carry no `path` key at all -- a null or an empty string \
             would be a file name a window would try to open: {clip:?}"
        );
        assert!(
            !clip.contains_key("original_path"),
            "an item that never had a file was never anywhere, and must not reach a window \
             claiming somewhere to be put back to: {clip:?}"
        );

        // Nothing else in the trash may be lost by tolerating it (AGENTS.md
        // section 56).
        let recording = items[1].as_object().expect("a trashed item is an object");
        assert_eq!(
            recording["original_path"].as_str(),
            Some(r"D:\Clips\cs2-20260811-201400-1.mkv"),
            "the deleted recording went missing from the frame: {sent}"
        );
        assert_eq!(
            sent["outcome"]["ok"]["trash"]["total_items"].as_u64(),
            Some(2)
        );
    }

    #[test]
    fn restoring_a_clip_that_has_no_file_is_the_callers_mistake_and_not_an_unreadable_library() {
        // The half of #593 that was reachable before anything ever put a
        // pathless clip in the trash. `Trash::read` is where every trash
        // operation starts, and reading `clips.path` into a `String` made a
        // perfectly readable library answer `library_unavailable` -- "try
        // again, or check the drive" -- for a caller that had simply named
        // something the trash does not hold.
        let library = trash_holding_a_clip_with_no_file("trash-pathless-restore");

        // Clip 2 is the highlight nobody has deleted: it has no file, and it is
        // not in the trash.
        let refusal = library
            .restore(&RestoreFromTrash {
                kind: "clip".to_owned(),
                id: 2,
            })
            .expect_err("a clip the trash does not hold cannot be restored");

        assert_eq!(
            refusal.code,
            ErrorCode::InvalidParameters,
            "restoring a clip with no file blamed the library rather than the request: {refusal:?}"
        );
    }

    #[test]
    fn a_search_that_does_not_parse_is_refused_with_what_was_wrong_with_it() {
        // Not an empty result set: a search box has to be able to say what is
        // wrong with what was typed (AGENTS.md section 45).
        let library = library_with_a_missing_recording("library-bad-query");

        let refusal = library
            .sessions(&LibrarySessions {
                query: Some("game:".to_owned()),
                ..LibrarySessions::default()
            })
            .expect_err("an unparseable query is refused");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("library_sessions"),
            "the refusal should name the command: {}",
            refusal.message
        );
    }

    #[test]
    fn a_query_that_parses_selects_the_sittings_it_names() {
        let library = library_with_a_missing_recording("library-query");

        let matched = library
            .sessions(&LibrarySessions {
                query: Some("game:counter".to_owned()),
                ..LibrarySessions::default()
            })
            .expect("the library reads");
        assert_eq!(matched.sessions.len(), 1);

        let unmatched = library
            .sessions(&LibrarySessions {
                query: Some("game:minecraft".to_owned()),
                ..LibrarySessions::default()
            })
            .expect("the library reads");
        assert!(
            unmatched.sessions.is_empty(),
            "a query that selects nothing is an empty page, not every session"
        );
    }

    #[test]
    fn a_blank_search_is_the_whole_library_rather_than_a_refusal() {
        // What a search box sends when the user clears it.
        let library = library_with_a_missing_recording("library-blank-query");

        let page = library
            .sessions(&LibrarySessions {
                query: Some("   ".to_owned()),
                ..LibrarySessions::default()
            })
            .expect("a blank query is not a bad one");

        assert_eq!(page.sessions.len(), 1);
    }

    /// A library of `sessions` sittings, each holding `recordings` recordings.
    ///
    /// Rows in a real index rather than [`IndexedSession`] values handed
    /// straight to a private function, because the properties the three tests
    /// below are named for belong to [`LibraryReader::sessions`] and not to
    /// anything it happens to call. A test that never opened a database could
    /// not tell "the page a window receives is bounded" from "a function exists
    /// which would bound one if it were called".
    ///
    /// The sittings are ordered by `started_at`, newest last, so the session at
    /// rank `r` of a page — newest first — is [`session_id_at_rank`].
    fn library_of(name: &str, sessions: usize, recordings_each: usize) -> TestLibrary {
        let directory = scratch_directory(name);
        let path = directory.join("library.db");
        {
            let database = Database::open(&path).expect("a database opens");
            let connection = database.connection();
            // One transaction for the lot. Several thousand rows at one commit
            // each would make these tests slow enough that somebody stops
            // running them.
            connection
                .execute_batch("BEGIN")
                .expect("a transaction begins");
            connection
                .execute(
                    "INSERT INTO games (game_id, name, first_seen_at) \
                     VALUES ('counter-strike-2', 'Counter-Strike 2', '2026-01-01T00:00:00+01:00')",
                    [],
                )
                .expect("a game inserts");

            for index in 0..sessions {
                let session_id = session_id_of(index);
                connection
                    .execute(
                        "INSERT INTO sessions \
                            (session_id, game_id, started_at, ended_at, end_reason, favourited_at) \
                         VALUES (?1, 'counter-strike-2', ?2, ?3, 'game-exited', ?3)",
                        params![
                            session_id,
                            started_at_of(index),
                            "2026-01-02T00:00:00+01:00"
                        ],
                    )
                    .expect("a session inserts");

                for ordinal in 0..recordings_each {
                    connection
                        .execute(
                            "INSERT INTO recordings \
                                (session_id, session_index, path, started_at, ended_at, outcome, \
                                 end_reason, duration_seconds, width, height, size_bytes, \
                                 favourited_at) \
                             VALUES (?1, ?2, ?3, ?4, '2026-01-02T00:00:00+01:00', 'recorded', \
                                     'target-lost', 6540.0, 2560, 1440, 9812009112, ?4)",
                            params![
                                session_id,
                                ordinal as i64 + 1,
                                format!(
                                    r"D:\Recordings\Counter-Strike 2\clipped-{session_id}-{ordinal}.mkv"
                                ),
                                started_at_of(index),
                            ],
                        )
                        .expect("a recording inserts");
                }
            }

            connection.execute_batch("COMMIT").expect("it commits");
        }
        TestLibrary {
            reader: LibraryReader::at(Some(path)),
            _directory: directory,
        }
    }

    /// The identifier of the sitting `library_of` inserted `index`th.
    fn session_id_of(index: usize) -> String {
        format!("counter-strike-2-{index:04}")
    }

    /// When it started. Distinct per sitting, so the newest-first order these
    /// tests reason about is total rather than a tie broken by the identifier.
    fn started_at_of(index: usize) -> String {
        format!("2026-01-01T{:02}:{:02}:00+01:00", index / 60, index % 60)
    }

    /// The identifier of the sitting at `rank` of a newest-first listing over a
    /// library of `sessions` sittings, counting from zero.
    fn session_id_at_rank(sessions: usize, rank: usize) -> String {
        session_id_of(sessions - 1 - rank)
    }

    #[test]
    fn a_page_is_bounded_by_what_a_frame_can_carry_rather_than_by_a_count_of_sessions() {
        // The defect this exists for is not hypothetical: the index will hand
        // over `MAX_PAGE_LIMIT` — two hundred — sessions, and two hundred
        // sessions is 135 KB with one recording each and over 3 MB with thirty.
        // A frame over `MAX_FRAME_BYTES` is not a failed request; the reader
        // closes the connection (`crates/ipc/src/frame.rs`), so a busy library
        // would drop the window's control connection on every attempt.
        //
        // Read through `sessions`, which is what the recorder's dispatch calls:
        // the bound is only worth anything if it is on the path a window's
        // question actually takes.
        const SESSIONS: usize = 120;
        let library = library_of("library-frame-bound", SESSIONS, 40);

        let page = library
            .sessions(&LibrarySessions {
                limit: Some(200),
                ..LibrarySessions::default()
            })
            .expect("the library reads");
        let bytes = serde_json::to_vec(&page).expect("a page serialises").len();

        assert!(
            bytes < MAX_FRAME_BYTES as usize,
            "a page has to fit in a frame, and this one is {bytes} bytes"
        );
        assert!(
            !page.sessions.is_empty() && page.sessions.len() < SESSIONS,
            "this page could only fit by being cut short, and it carried {} of {SESSIONS}",
            page.sessions.len()
        );

        // Cut short, so the cursor must continue at the first session left out
        // — not past every one of them, which is where the index's own cursor
        // points. Asked rather than compared to a string, because what the next
        // request does with it is the whole of what the cursor is for: getting
        // this wrong silently loses every session between the two.
        let carried = page.sessions.len();
        let next = library
            .sessions(&LibrarySessions {
                limit: Some(200),
                after: page.next_cursor.clone(),
                ..LibrarySessions::default()
            })
            .expect("the library reads");

        assert_eq!(
            next.sessions
                .first()
                .map(|session| session.session_id.as_str()),
            Some(session_id_at_rank(SESSIONS, carried).as_str()),
            "the page after a truncated one has to start with the first sitting left out"
        );
    }

    #[test]
    fn a_page_that_fits_keeps_the_cursor_the_index_gave_it() {
        // The other half: truncation must not fire when it is not needed, or
        // every page would end early and paging would take more round trips
        // than there are sessions.
        const SESSIONS: usize = 30;
        let library = library_of("library-page-fits", SESSIONS, 1);

        let page = library
            .sessions(&LibrarySessions {
                limit: Some(25),
                ..LibrarySessions::default()
            })
            .expect("the library reads");

        assert_eq!(page.sessions.len(), 25);

        let next = library
            .sessions(&LibrarySessions {
                limit: Some(25),
                after: page.next_cursor.clone(),
                ..LibrarySessions::default()
            })
            .expect("the library reads");

        assert_eq!(
            next.sessions
                .first()
                .map(|session| session.session_id.as_str()),
            Some(session_id_at_rank(SESSIONS, 25).as_str()),
            "a page that fitted has to continue where the index said, not earlier"
        );
    }

    #[test]
    fn a_single_session_too_large_for_the_budget_is_still_answered() {
        // Otherwise a page comes back empty, the caller reads "nothing here",
        // and the session can never be reached however many times it pages.
        // One oversized frame is the lesser evil, and the frame reader's own
        // ceiling is well above any one sitting.
        let library = library_of("library-one-enormous", 1, 2_500);

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");

        assert_eq!(
            page.sessions.len(),
            1,
            "a sitting that will not fit the budget still has to be answered"
        );
        assert_eq!(page.sessions[0].recordings.len(), 2_500);
        assert_eq!(page.next_cursor, None);

        // And it really is over the budget, or this test proves nothing: an
        // assertion that a page of one holds one is only interesting when that
        // one was too large to carry.
        let cost = serde_json::to_vec(&page.sessions[0])
            .expect("a session serialises")
            .len();
        assert!(
            cost > PAGE_BUDGET_BYTES,
            "this sitting was meant to overrun the {PAGE_BUDGET_BYTES}-byte budget and is {cost} \
             bytes"
        );
    }

    #[test]
    fn a_machine_with_no_per_user_directory_says_that_rather_than_answering_nothing() {
        let library = LibraryReader::at(None);

        let refusal = library
            .games()
            .expect_err("there is no library to read on such a machine");

        assert_eq!(refusal.code, ErrorCode::LibraryUnavailable);
    }

    /// A request to favourite something.
    fn asking(kind: &str, session_id: &str, id: i64, favourite: bool) -> SetFavourite {
        SetFavourite {
            kind: kind.to_owned(),
            session_id: session_id.to_owned(),
            id,
            favourite,
        }
    }

    /// When the tests below claim things happened.
    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + core::time::Duration::from_secs(1_786_000_000)
    }

    #[test]
    fn a_sitting_and_the_recording_in_it_can_both_be_favourited_and_unfavourited() {
        // Issue #58's first acceptance criterion at the boundary the window
        // actually reaches: the mark has to *persist*, and it is the reads on
        // this same reader that say whether it did.
        let library = library_with_a_missing_recording("favourite-round-trip");
        let recording_id = 1;

        for (kind, session_id, id) in [
            ("session", "cs2-20260811-201400", 0),
            ("recording", "", recording_id),
        ] {
            let marked = library
                .set_favourite(&asking(kind, session_id, id, true), at())
                .expect("a favourite is written");
            assert!(marked.favourite, "{kind} should be a favourite now");
            assert!(marked.changed, "{kind} was not one before");

            // Again, which is a second click on a full star. It must not look
            // like a failure and must not move the instant it was first marked.
            let again = library
                .set_favourite(&asking(kind, session_id, id, true), at())
                .expect("marking twice is not an error");
            assert!(again.favourite, "{kind}");
            assert!(!again.changed, "{kind} was already a favourite");

            let cleared = library
                .set_favourite(&asking(kind, session_id, id, false), at())
                .expect("a favourite is cleared");
            assert!(!cleared.favourite, "{kind}");
            assert!(cleared.changed, "{kind} was a favourite until now");
        }
    }

    #[test]
    fn the_mark_a_favourite_reports_is_the_one_the_library_read_answers_with() {
        // The reply is not what was asked for, it is what is true. A row the
        // index does not hold is written to by nothing, and a window told
        // "favourited" would draw a star it loses on the next read.
        let library = library_with_a_missing_recording("favourite-read-back");

        let answer = library
            .set_favourite(&asking("recording", "", 9_999, true), at())
            .expect("a write against a row that is not there is not an error");

        assert!(
            !answer.favourite,
            "nothing was written, so nothing is favourited"
        );
        assert!(!answer.changed);

        // And the session listing agrees, which is the read a screen makes.
        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");
        assert!(!page.sessions[0].recordings[0].favourite);
    }

    #[test]
    fn a_target_that_names_nothing_is_refused_rather_than_marking_row_zero() {
        // Both halves of the target are optional on the wire — one kind reads
        // each — so a request that filled in neither would otherwise favourite
        // whatever row happens to sit at identifier zero.
        for (kind, session_id, id, expected) in [
            ("session", "", 0, "session_id"),
            ("recording", "", 0, "id"),
            ("clip", "", 0, "id"),
        ] {
            let refusal = LibraryReader::at(None)
                .set_favourite(&asking(kind, session_id, id, true), at())
                .expect_err("a target that names nothing is refused");

            assert_eq!(refusal.code, ErrorCode::InvalidParameters, "{kind}");
            assert!(
                refusal.message.contains(expected) && refusal.message.contains(kind),
                "a refusal has to name the field that was missing: {}",
                refusal.message
            );
        }
    }

    #[test]
    fn a_kind_nothing_can_be_favourited_by_is_refused_and_says_what_can() {
        // A screenshot is in issue #58's scope and has no table in the schema,
        // so this is the message somebody gets when the two disagree.
        let refusal = LibraryReader::at(None)
            .set_favourite(&asking("screenshot", "", 1, true), at())
            .expect_err("screenshots have nowhere to keep a mark");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        for named in ["session", "recording", "clip"] {
            assert!(
                refusal.message.contains(named),
                "the refusal should say what can be favourited: {}",
                refusal.message
            );
        }
    }

    #[test]
    fn the_target_is_read_before_the_library_is_opened() {
        // A malformed request is the caller's mistake whatever the state of the
        // machine, and `LibraryReader::at(None)` is a machine with no library at
        // all: these refusals arrive as `InvalidParameters` rather than as
        // `LibraryUnavailable`, which is what the tests above rely on.
        let refusal = LibraryReader::at(None)
            .set_favourite(&asking("session", "cs2-20260811-201400", 0, true), at())
            .expect_err("there is no library on such a machine");

        assert_eq!(
            refusal.code,
            ErrorCode::LibraryUnavailable,
            "a well-formed request against no library is the machine's problem, not the caller's"
        );
    }
    /// A request to lock something.
    fn locking(kind: &str, session_id: &str, id: i64, locked: bool) -> SetLock {
        SetLock {
            kind: kind.to_owned(),
            session_id: session_id.to_owned(),
            id,
            locked,
        }
    }

    #[test]
    fn a_sitting_and_the_recording_in_it_can_both_be_locked_and_unlocked() {
        let library = library_with_a_missing_recording("lock-round-trip");

        for (kind, session_id, id) in [("session", "cs2-20260811-201400", 0), ("recording", "", 1)]
        {
            let set = library
                .set_lock(&locking(kind, session_id, id, true), at())
                .expect("a lock is written");
            assert!(set.locked, "{kind} should be locked now");
            assert!(set.protected, "{kind} should be protected now");
            assert!(set.changed, "{kind} was not locked before");

            let again = library
                .set_lock(&locking(kind, session_id, id, true), at())
                .expect("locking twice is not an error");
            assert!(again.locked, "{kind}");
            assert!(!again.changed, "{kind} was already locked");

            let cleared = library
                .set_lock(&locking(kind, session_id, id, false), at())
                .expect("a lock is cleared");
            assert!(!cleared.locked, "{kind}");
            assert!(cleared.changed, "{kind} was locked until now");
        }
    }

    #[test]
    fn a_recording_in_a_locked_sitting_is_protected_without_being_locked() {
        // The cascade, at the boundary. `locked` and `protected` are different
        // questions and this is the case that separates them: a screen drawing
        // a padlock from `locked` alone would show this recording as one
        // cleanup may take, and cleanup would not take it.
        let library = library_with_a_missing_recording("lock-cascade");

        library
            .set_lock(&locking("session", "cs2-20260811-201400", 0, true), at())
            .expect("the sitting locks");

        // Asked about the recording without changing it: setting the state it
        // is already in writes nothing, and the reply still reports both.
        let recording = library
            .set_lock(&locking("recording", "", 1, false), at())
            .expect("it can be asked");

        assert!(
            !recording.locked,
            "the recording has no lock of its own, and unlocking the sitting must not have to \
             find and clear one"
        );
        assert!(
            recording.protected,
            "it is inside a locked sitting, so automatic cleanup will not take it"
        );
        assert!(!recording.changed, "nothing was written");
    }

    #[test]
    fn a_recording_reaches_the_window_protected_by_its_sittings_lock() {
        // The cascade as it crosses the boundary, which is the one place it is
        // expressed for a window: `protected` is the recording's own lock *or*
        // its sitting's, worked out here so no window has to know the rule.
        //
        // A screen test cannot cover this — it stubs the page read — so without
        // this case the mapping could drop the sitting's half and nothing would
        // fail.
        let library = library_with_a_missing_recording("lock-on-the-wire");

        library
            .set_lock(&locking("session", "cs2-20260811-201400", 0, true), at())
            .expect("the sitting locks");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");
        let session = &page.sessions[0];
        let recording = &session.recordings[0];

        assert!(session.locked, "the sitting carries its own lock");
        assert!(
            !recording.locked,
            "the recording has none of its own, which is what a control is drawn from"
        );
        assert!(
            recording.protected,
            "and it is protected anyway, which is what a padlock is drawn from"
        );
    }

    #[test]
    fn a_recordings_own_lock_reaches_the_window_as_its_own_lock() {
        // The column beside it is `favourited_at`, and reading that one instead
        // would be invisible to every other test here: the cascade case asks
        // about a *session's* lock, and the negative case has both columns
        // empty. So this one locks the recording and favourites nothing, which
        // is the only arrangement that tells the two columns apart.
        let library = library_with_a_missing_recording("lock-own-on-the-wire");

        library
            .set_lock(&locking("recording", "", 1, true), at())
            .expect("the recording locks");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");
        let recording = &page.sessions[0].recordings[0];

        assert!(recording.locked, "its own lock is what it carries");
        assert!(recording.protected, "and its own lock protects it");
        assert!(
            !recording.favourite,
            "nothing favourited it, so a `locked` read off the favourite column \
             would be false here rather than true"
        );
        assert!(
            !page.sessions[0].locked,
            "the sitting has no lock of its own"
        );
    }

    #[test]
    fn a_favourite_is_not_mistaken_for_a_lock() {
        // The other direction of the same off-by-one: favouriting a recording
        // must not make it look locked.
        let library = library_with_a_missing_recording("favourite-is-not-a-lock");

        library
            .set_favourite(&asking("recording", "", 1, true), at())
            .expect("the recording is favourited");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");
        let recording = &page.sessions[0].recordings[0];

        assert!(recording.favourite);
        assert!(!recording.locked, "a favourite is not a lock");
        assert!(
            !recording.protected,
            "and it does not protect against cleanup through the lock path — \
             `Protection::Favourite` is a different rule, in `accounting::cleanup`"
        );
    }

    #[test]
    fn a_recording_with_no_lock_anywhere_reaches_the_window_unprotected() {
        // The other side of the same rule, or the assertion above would be
        // satisfied by a field that was always true.
        let library = library_with_a_missing_recording("lock-on-the-wire-absent");

        let page = library
            .sessions(&LibrarySessions::default())
            .expect("the library reads");

        assert!(!page.sessions[0].locked);
        assert!(!page.sessions[0].recordings[0].locked);
        assert!(!page.sessions[0].recordings[0].protected);
    }

    #[test]
    fn a_clip_cannot_be_locked_and_the_refusal_says_what_to_lock_instead() {
        // Not silently ignored: a window told "done" would draw a padlock
        // against a clip that automatic cleanup never consults.
        let refusal = LibraryReader::at(None)
            .set_lock(&locking("clip", "", 1, true), at())
            .expect_err("clips are not what cleanup deletes");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("recording"),
            "the refusal has to say what to lock instead: {}",
            refusal.message
        );
    }

    #[test]
    fn a_lock_target_that_names_nothing_is_refused_rather_than_locking_row_zero() {
        for (kind, session_id, id, expected) in
            [("session", "", 0, "session_id"), ("recording", "", 0, "id")]
        {
            let refusal = LibraryReader::at(None)
                .set_lock(&locking(kind, session_id, id, true), at())
                .expect_err("a target that names nothing is refused");

            assert_eq!(refusal.code, ErrorCode::InvalidParameters, "{kind}");
            assert!(
                refusal.message.contains(expected) && refusal.message.contains(kind),
                "a refusal has to name the field that was missing: {}",
                refusal.message
            );
        }
    }

    #[test]
    fn a_lock_stops_automatic_cleanup_taking_the_recording() {
        // The whole point of the column, through the boundary a window reaches:
        // lock it here, and the sweep's own candidate list says why it will not
        // be taken.
        let library = library_with_a_missing_recording("lock-protects");

        library
            .set_lock(&locking("recording", "", 1, true), at())
            .expect("it locks");

        let protection = library
            .with_database_mut(|database| {
                let candidates = clipped_library::accounting::cleanup::candidates(database)
                    .map_err(unreadable)?;
                Ok(candidates
                    .iter()
                    .find(|candidate| candidate.item.id == 1)
                    .and_then(|candidate| candidate.protection))
            })
            .expect("the candidates can be read");

        assert_eq!(
            protection,
            Some(clipped_library::accounting::cleanup::Protection::Locked),
            "the sweep has to say the lock is what saved it"
        );
    }

    /// A session record, as the recorder writes one beside its recordings.
    ///
    /// Written by hand rather than by running a recording: what the test below
    /// needs is a reconciliation with rows to write, and the file is the
    /// contract between the two halves (`crates/library/src/index/sidecar.rs`,
    /// `docs/sessions.md`).
    fn write_a_sitting(recordings: &std::path::Path, ordinal: usize) {
        let media = recordings.join(format!("clipped-2026081{ordinal:04}-1.mkv"));
        std::fs::write(&media, [0u8; 64]).expect("a recording can be written");
        let output = media.display().to_string().replace('\\', "\\\\");
        std::fs::write(
            recordings.join(format!("clipped-cs2-{ordinal:04}.session.json")),
            format!(
                r#"{{"schema_version":1,
                     "session_id":"cs2-{ordinal:04}",
                     "game":{{"kind":"known","game_id":"cs2","name":"Counter-Strike 2"}},
                     "started_at":"2026-08-11T20:14:00+01:00",
                     "ended_at":"2026-08-11T21:14:00+01:00",
                     "recordings":[{{"index":1,
                                     "output":"{output}",
                                     "started_at":"2026-08-11T20:14:00+01:00",
                                     "ended_at":"2026-08-11T21:14:00+01:00",
                                     "outcome":"recorded",
                                     "duration_seconds":3600.0}}],
                     "clips":[],"events":[]}}"#
            ),
        )
        .expect("a session record can be written");
    }

    /// How many sittings the run below has to get through.
    ///
    /// One session per transaction and a pause between them, so the run lasts
    /// seconds and spends most of them either inside a write or a few
    /// microseconds away from one.
    const SITTINGS: usize = 100;

    /// How long the asking is watched for, which has to be comfortably less
    /// than the run takes.
    const WATCHED_FOR: std::time::Duration = std::time::Duration::from_millis(600);

    /// The longest a caller may take to ask for a run before the answer is
    /// "it waited". The call costs tens of nanoseconds; this is six orders of
    /// magnitude of headroom, and still twenty times less than the run.
    const NOT_WAITING: std::time::Duration = std::time::Duration::from_millis(100);

    #[test]
    fn asking_for_a_run_does_not_wait_for_the_run_that_is_writing() {
        // AGENTS.md section 20 as the thing that actually happens, rather than
        // as the `WriteQueue` that was built for it and never used
        // ([issue #605](https://github.com/wildware-uk/clipped/issues/605)).
        //
        // Every row the recorder writes is written by the indexer thread, and
        // the recording thread's one contact with it is
        // `RecordingState::index_now`, which is `LibraryIndexer::request`. So
        // the property that keeps a recording off the database is not that
        // writes are queued — they are not — it is that asking for a run
        // returns whatever the run in flight is in the middle of.
        //
        // A hundred sittings at one per transaction is a run of several
        // seconds, holding a write transaction open for a hundred separate
        // stretches of it. Asking is timed repeatedly across the first six
        // hundred milliseconds of that, so the measurement is not one sample
        // that might have fallen in a gap.
        //
        // Break it by moving `self.reconcile_once(&mut database)` inside the
        // `state` lock in `Indexer::run`: `worst` becomes the rest of the run,
        // which is the recording thread waiting on the database.
        let directory = scratch_directory("index-request-never-waits");
        let path = directory.join(LIBRARY_FILE);
        let recordings = directory.join("recordings");
        std::fs::create_dir_all(&recordings).expect("a recordings folder can be made");
        for ordinal in 0..SITTINGS {
            write_a_sitting(&recordings, ordinal);
        }

        let indexer = LibraryIndexer::at(Some(path.clone()), vec![recordings.clone()]).with_pace(
            clipped_library::index::IndexPace {
                batch: 1,
                page: 512,
                rest: std::time::Duration::from_millis(20),
            },
        );
        indexer.start();

        let watching = std::time::Instant::now();
        let mut worst = std::time::Duration::ZERO;
        let mut asked = 0u32;
        while watching.elapsed() < WATCHED_FOR {
            let at = std::time::Instant::now();
            indexer.request();
            worst = worst.max(at.elapsed());
            asked += 1;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        // Read before the assertions, and through the same lock `request` takes:
        // a run that had already finished would make the measurement above a
        // measurement of an idle indexer, and reading it at all says the thread
        // doing the writing is holding nothing this needs.
        let finished = indexer.runs();

        assert!(
            worst < NOT_WAITING,
            "asking for a run waited {worst:?} for the run that was writing, over {asked} \
             attempts. That is the recording thread waiting on the database, which is what \
             AGENTS.md section 20 forbids"
        );
        assert_eq!(
            finished, 0,
            "the run was supposed to still be going after {WATCHED_FOR:?}; {asked} \
             measurements of an indexer that had already stopped prove nothing"
        );

        // And the run does write, which a test that only measured the asking
        // would be satisfied without.
        assert!(
            indexer.settled_within(std::time::Duration::from_secs(60)),
            "the indexer never finished the run"
        );
        indexer.shut_down();

        let reader = Database::open_read_only(&path).expect("the library can be read");
        let sessions: i64 = reader
            .connection()
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .expect("the sessions can be counted");
        assert_eq!(
            sessions,
            i64::try_from(SITTINGS).expect("a hundred fits"),
            "every sitting had to reach the index, or the run being measured was not writing"
        );
    }
}
