//! What a session *is*: the thing recordings, clips and events hang off.
//!
//! A session is one sitting with one game. It is not the same thing as a
//! recording: a player who alt-F4s and comes straight back has had one sitting
//! and produced two files, and a game whose window is destroyed and recreated
//! on a resolution change has had one sitting and produced two files as well.
//! The session is what says those files belong together, which is the whole
//! reason the model exists rather than the library simply holding a list of
//! `.mkv` files.
//!
//! # What this build models, and what it does not
//!
//! Recordings and events, because those are the two that exist. Clips wait on
//! [issue #38](https://github.com/wildware-uk/clipped/issues/38) — writing one
//! exists ([issue #37](https://github.com/wildware-uk/clipped/issues/37)) but
//! no build runs a recording with a buffer to write from. Bookmarks exist
//! ([issue #64](https://github.com/wildware-uk/clipped/issues/64)) and are not
//! modelled here: a bookmark is an offset into one recording, so it belongs to
//! that recording's own sidecar ([`crate::bookmarks`]) rather than to the
//! session that groups the files. Both are reserved in the session schema — see
//! [`super::sidecar`] — so that filling either is not a format change.
//!
//! # Where it lives
//!
//! In memory while the recorder runs, and in a JSON sidecar beside the
//! recordings ([`super::sidecar`]). **M6's
//! [issue #55](https://github.com/wildware-uk/clipped/issues/55) owns the real
//! store**: the SQLite library index that makes sessions searchable, joins them
//! to clips and survives being asked about a thousand of them. This is the
//! minimum the recorder needs today so that "which game was this, and which
//! files belong together" is not held only in the memory of a process that can
//! be killed (AGENTS.md sections 17 and 55).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipped_events::GameEvent;
use clipped_library::virtual_clip::VirtualClip;

use crate::config::ResolvedSettings;
use crate::report::{EndReason, RecordingReport};

/// The `game_id` a session is filed under when the catalogue would not choose.
///
/// A tie between catalogue entries is a real answer and not a failure — see
/// [`GameIdentity::Ambiguous`] — but a session still needs something to be
/// named after on disk, and it must not be one of the candidates.
pub const UNATTRIBUTED: &str = "unattributed";

/// Which game a session is of.
///
/// The catalogue is asked the same question both ways a session starts
/// (`super::identify_process`), and what differs is what the answer decides. A
/// launch the catalogue does not claim never becomes a session at all, so a
/// session [`SessionManager`](super::SessionManager) opened is never
/// [`Self::Unidentified`]. A session somebody asked for by pointing at a window
/// can be: the user chose the window, so there is a session and a recording
/// whatever the catalogue said about it.
///
/// The three are three different statements and none of them is a guess —
/// "this game", "several tied", and "the catalogue claims nothing here".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameIdentity {
    /// Exactly one catalogue entry claimed the process.
    Known {
        /// The entry's `game_id`.
        game_id: String,
        /// What a person calls the game.
        name: String,
    },

    /// Several entries claimed it equally well, and the catalogue does not
    /// guess between them (`clipped_game_detection::catalogue::Match`).
    ///
    /// The session is recorded and filed under [`UNATTRIBUTED`] with the
    /// candidates written down, rather than being filed under a coin toss or
    /// not recorded at all. The footage is what cannot be made again; which of
    /// three names it goes under can be decided afterwards by a person, and the
    /// candidates are the whole of what they need to decide it.
    Ambiguous {
        /// Every entry that tied, by `game_id`, in catalogue order.
        candidates: Vec<String>,
    },

    /// No catalogue entry claims the process, so there is no game to name.
    ///
    /// A recording started over the protocol names a *window*, and the person
    /// who pressed the button is the authority on it being worth recording
    /// (`clipped_session::automatic::ManualSession`) — but nobody is an
    /// authority on what a window *is*. A browser, an editor, a game nobody has
    /// catalogued and a game the user excluded all reach here, and the session
    /// is filed under [`UNATTRIBUTED`] with no candidates, because there are
    /// none: this is an answer of "nothing" rather than an unresolved answer
    /// between several (AGENTS.md section 27).
    Unidentified,
}

impl GameIdentity {
    /// The identifier a session of this game is filed and named under.
    #[must_use]
    pub fn slug(&self) -> &str {
        match self {
            Self::Known { game_id, .. } => game_id,
            Self::Ambiguous { .. } | Self::Unidentified => UNATTRIBUTED,
        }
    }

    /// What to call the game in a log line or on a screen.
    #[must_use]
    pub fn display_name(&self) -> &str {
        match self {
            Self::Known { name, .. } => name,
            Self::Ambiguous { .. } => "an unattributed game",
            Self::Unidentified => "an unidentified window",
        }
    }

    /// Whether both identities name the same game.
    ///
    /// Two ambiguous matches are the same identity only when they tied between
    /// exactly the same candidates: a relaunch that ties differently is a
    /// different answer and does not rejoin the session.
    #[must_use]
    pub fn is_same_game_as(&self, other: &Self) -> bool {
        self == other
    }
}

/// A session's stable identity, and the stem every file of it is named from.
///
/// `<game_id>-<yyyymmdd>-<hhmmss>`, from the moment the session started. It is
/// sortable, it is legible in a directory listing, and it is a legal Windows
/// file name because a `game_id` is restricted to `[a-z0-9-]`
/// (`clipped_game_detection::catalogue::GameId`) and the rest is digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// The identifier for a session of `game` that started at `started`.
    #[must_use]
    pub fn new(game: &GameIdentity, started: SystemTime) -> Self {
        Self(format!("{}-{}", game.slug(), super::clock::stamp(started)))
    }

    /// The same, stamped against a stated offset from UTC.
    ///
    /// Tests use it: [`Self::new`] reads the machine's own offset, so an
    /// assertion about the text it produces would pass in one time zone and
    /// fail in the next (AGENTS.md section 25).
    #[cfg(test)]
    pub(crate) fn at(game: &GameIdentity, started: SystemTime, offset: time::UtcOffset) -> Self {
        Self(format!(
            "{}-{}",
            game.slug(),
            super::clock::stamp_at(started, offset)
        ))
    }

    /// The identifier as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One recording within a session, and what it turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecording {
    pub(crate) index: u32,
    /// Where this file starts on the **session's** timeline, in nanoseconds.
    ///
    /// The first recording of a session starts at zero by definition; a later
    /// one starts wherever it began relative to the session's first kept frame.
    /// With `duration_seconds` this is the span the file covers, which is what
    /// turns a moment into a position in one file
    /// (`clipped_library::events`, issue #71).
    ///
    /// [`None`] until something places it: a recording that produced no frame
    /// has no start, and neither has one written by a build before this
    /// existed.
    pub(crate) starts_at_nanos: Option<i64>,
    pub(crate) output: PathBuf,
    pub(crate) started_at: SystemTime,
    pub(crate) ended_at: Option<SystemTime>,
    pub(crate) outcome: Option<RecordingOutcomeSummary>,
    pub(crate) settings: ResolvedSettings,
}

impl SessionRecording {
    /// Which recording of the session this is, counting from one.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Where this file starts on the session's timeline, in nanoseconds.
    #[must_use]
    pub const fn starts_at_nanos(&self) -> Option<i64> {
        self.starts_at_nanos
    }

    /// The file it was written to.
    #[must_use]
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// When it started.
    #[must_use]
    pub const fn started_at(&self) -> SystemTime {
        self.started_at
    }

    /// When it ended, or [`None`] while it is still running.
    #[must_use]
    pub const fn ended_at(&self) -> Option<SystemTime> {
        self.ended_at
    }

    /// What it turned out to be, or [`None`] while it is still running.
    #[must_use]
    pub const fn outcome(&self) -> Option<&RecordingOutcomeSummary> {
        self.outcome.as_ref()
    }

    /// The settings that applied to it, as they were resolved when it started.
    ///
    /// Kept per recording rather than per session because that is where the
    /// answer can differ: a session that spans a settings change contains one
    /// recording made at the old settings and one at the new.
    #[must_use]
    pub const fn settings(&self) -> &ResolvedSettings {
        &self.settings
    }
}

/// One clip the session produced, and where in the session it came from.
///
/// A clip is not a recording and is deliberately not filed as one: it is a
/// shorter file somebody chose to keep out of a longer thing, it has its own
/// table in the library (`clipped-storage`'s `clips`), and the sidecar has
/// reserved the word since the schema shipped. What makes one today is a save
/// from the replay buffer ([issue
/// #38](https://github.com/wildware-uk/clipped/issues/38)).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionClip {
    pub(crate) path: PathBuf,
    pub(crate) created_at: SystemTime,
    pub(crate) source_index: Option<u32>,
    pub(crate) source_start: Duration,
    pub(crate) source_end: Duration,
    pub(crate) requested: Duration,
    pub(crate) complete: bool,
}

impl SessionClip {
    /// The file that was written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// When it was saved.
    #[must_use]
    pub const fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// Which recording of the session it was cut from, or [`None`] when there
    /// was none.
    ///
    /// [`None`] for a clip saved out of a capture that wrote no continuous
    /// recording, which is SPEC.md section 4's Manual/Replay mode: the sitting
    /// has no `recordings` entry for this to point at, and
    /// `clipped-storage`'s `clips.source_recording_id` is nullable in as many
    /// words for exactly this case
    /// (`crates/storage/migrations/0004_clips_without_a_file.sql`,
    /// `docs/adr/0018-a-capture-that-writes-no-recording.md`).
    ///
    /// [`source_start`](Self::source_start) and [`source_end`](Self::source_end)
    /// are still offsets into the capture's own timeline, which exists whether
    /// or not a file was written.
    #[must_use]
    pub const fn source_index(&self) -> Option<u32> {
        self.source_index
    }

    /// Where in that recording it begins.
    #[must_use]
    pub const fn source_start(&self) -> Duration {
        self.source_start
    }

    /// Where in that recording it ends.
    #[must_use]
    pub const fn source_end(&self) -> Duration {
        self.source_end
    }

    /// How long the clip is.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.source_end.saturating_sub(self.source_start)
    }

    /// How much was asked for.
    ///
    /// Not always [`duration`](Self::duration): a clip has to begin on a
    /// keyframe, so it is usually slightly longer at the front, and a buffer
    /// that had not filled yet makes it shorter (`docs/replay-buffer.md`).
    #[must_use]
    pub const fn requested(&self) -> Duration {
        self.requested
    }

    /// Whether the buffer held the whole of what was asked for.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }
}

/// How a recording ended, in the few fields a session needs to keep.
///
/// A summary rather than the whole [`RecordingReport`]: what belongs in a
/// session's metadata is what somebody looking at their library would ask —
/// how long, how many frames, what happened — and not the frame accounting a
/// recording produces for diagnostics, which is in the log.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordingOutcomeSummary {
    /// The recording ran and the file was finalised.
    Recorded {
        /// Frames submitted to the encoder.
        frames_encoded: u64,
        /// The span between the first and last timestamps written.
        duration: Duration,
        /// The size of the picture.
        size: (u32, u32),
        /// Why it ended.
        end_reason: EndReason,
    },

    /// No capturable window belonging to the game appeared, so nothing was
    /// recorded.
    NoWindow {
        /// What was tried and what the desktop said.
        detail: String,
    },

    /// The recording failed. Whatever had been written before the failure was
    /// still finalised, unless it failed before its first packet.
    Failed {
        /// What went wrong.
        detail: String,
    },
}

impl RecordingOutcomeSummary {
    /// The token this outcome is written as in the sidecar and in logs.
    #[must_use]
    pub const fn token(&self) -> &'static str {
        match self {
            Self::Recorded { .. } => "recorded",
            Self::NoWindow { .. } => "no-window",
            Self::Failed { .. } => "failed",
        }
    }

    /// Whether a file was produced.
    #[must_use]
    pub const fn produced_a_file(&self) -> bool {
        matches!(self, Self::Recorded { .. })
    }

    /// The summary of a finished recording.
    #[must_use]
    pub fn of(report: &RecordingReport) -> Self {
        Self::Recorded {
            frames_encoded: report.frames_encoded(),
            duration: report.duration(),
            size: report.size(),
            end_reason: report.end_reason(),
        }
    }
}

/// Something that happened during a session.
///
/// The session's own history, which is what makes "why does this session have
/// three files in it?" answerable afterwards rather than only from a log that
/// has since rotated away (`docs/logging.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    pub(crate) at: SystemTime,
    pub(crate) kind: SessionEventKind,
}

impl SessionEvent {
    /// When it happened.
    #[must_use]
    pub const fn at(&self) -> SystemTime {
        self.at
    }

    /// What happened.
    #[must_use]
    pub const fn kind(&self) -> &SessionEventKind {
        &self.kind
    }
}

/// What a [`SessionEvent`] records.
///
/// Game events — a kill, a round starting — are a different vocabulary
/// entirely, they come from plugins, and they are M9's `clipped-events`. These
/// are events about the *session*: they explain its shape.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionEventKind {
    /// The session began, because this process was recognised as a game.
    Started {
        /// The process the game was recognised by.
        pid: u32,
        /// Its executable's file name.
        image_name: String,
    },

    /// A recording within the session began.
    RecordingStarted {
        /// Which recording of the session.
        index: u32,
        /// Where it is being written.
        output: PathBuf,
    },

    /// A recording within the session ended.
    RecordingEnded {
        /// Which recording of the session.
        index: u32,
        /// How it ended.
        outcome: String,
    },

    /// A clip was saved out of the replay buffer of a recording in this
    /// session.
    ///
    /// The event as well as the entry in `clips`, because the two answer
    /// different questions: the entry is what the library indexes, and the
    /// event is what puts the save in the session's own history beside the
    /// recording it came out of.
    ReplaySaved {
        /// Which recording of the session it was cut from, or [`None`] when the
        /// sitting wrote no recording for it to have come out of.
        ///
        /// A capture that keeps only a replay buffer produces clips and no
        /// continuous file, so there is nothing for this to name
        /// (`docs/adr/0018-a-capture-that-writes-no-recording.md`).
        index: Option<u32>,
        /// The file that was written.
        output: PathBuf,
    },

    /// The game's process exited.
    GameExited {
        /// The process that exited.
        pid: u32,
    },

    /// The same game launched again while the session was still open, so this
    /// session continued rather than a second one starting.
    GameRelaunched {
        /// The new process.
        pid: u32,
    },

    /// A different game started while this session was recording.
    ///
    /// There is one encoder and one capture target, so it was not recorded.
    /// See [`super::SessionManager`] for what happens to it afterwards.
    AnotherGameStarted {
        /// What the catalogue made of it.
        game_id: String,
        /// Its process.
        pid: u32,
    },

    /// The machine appears to have been suspended and resumed.
    SystemResumed {
        /// How long the clock jumped by.
        gap: Duration,
    },

    /// The session reached its cap on recordings and stopped starting them.
    RecordingLimitReached {
        /// The cap that was reached.
        limit: u32,
    },

    /// The session ended.
    Ended {
        /// Why.
        reason: SessionEndReason,
    },
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndReason {
    /// The game exited and did not come back within the restart grace period.
    GameExited,
    /// The machine was suspended, which ends the grace period rather than
    /// letting a relaunch hours later look like a fast restart.
    SystemResumed,
    /// The recorder was shutting down.
    RecorderStopping,
    /// The recording the session was opened for finished.
    ///
    /// Only a session somebody asked for ends this way
    /// ([`ManualSession`](super::ManualSession)). It has no game whose exit
    /// could end it and no grace period to wait through: the person who started
    /// the recording is the whole of the session, so the session is over when
    /// their recording is. *Why* that recording ended — stopped, the window
    /// went, the encoder failed — is on the recording itself, where the same
    /// answer belongs for an automatic session too.
    RecordingEnded,
}

impl SessionEndReason {
    /// The token this reason is written as.
    ///
    /// Also a value of `sessions.end_reason`, which is a `CHECK` constraint:
    /// adding a reason here means a migration in `clipped-storage`, and a
    /// reason the database has never heard of costs a session that column
    /// (`crates/library/src/index/ingest.rs`).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::GameExited => "game-exited",
            Self::SystemResumed => "system-resumed",
            Self::RecorderStopping => "recorder-stopping",
            Self::RecordingEnded => "recording-ended",
        }
    }
}

impl std::fmt::Display for SessionEndReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.token())
    }
}

/// One sitting with one game.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub(crate) id: SessionId,
    pub(crate) game: GameIdentity,
    pub(crate) started_at: SystemTime,
    pub(crate) ended_at: Option<SystemTime>,
    pub(crate) recordings: Vec<SessionRecording>,
    pub(crate) clips: Vec<SessionClip>,
    /// The clips this session's events were worth, which have no file.
    ///
    /// A separate list from `clips` because they are separate things: a
    /// [`SessionClip`] is a file that was written because somebody pressed a
    /// key, and one of these is a *range* of a recording that costs nothing
    /// until an export is asked for (`docs/highlights.md`). Flattening them
    /// would give the first shape to the second and require a path that does
    /// not exist.
    pub(crate) generated: Vec<VirtualClip>,
    pub(crate) events: Vec<SessionEvent>,
    /// What plugins reported, ordered by the moment each event describes.
    pub(crate) game_events: Vec<GameEvent>,
}

impl Session {
    /// A session of `game` that started at `started`.
    pub(crate) fn new(game: GameIdentity, started: SystemTime) -> Self {
        Self {
            id: SessionId::new(&game, started),
            game,
            started_at: started,
            ended_at: None,
            recordings: Vec::new(),
            clips: Vec::new(),
            generated: Vec::new(),
            events: Vec::new(),
            game_events: Vec::new(),
        }
    }

    /// The session's identifier, which is also the stem its files are named
    /// from.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        &self.id
    }

    /// Which game it is of.
    #[must_use]
    pub const fn game(&self) -> &GameIdentity {
        &self.game
    }

    /// When it started.
    #[must_use]
    pub const fn started_at(&self) -> SystemTime {
        self.started_at
    }

    /// When it ended, or [`None`] while it is open.
    #[must_use]
    pub const fn ended_at(&self) -> Option<SystemTime> {
        self.ended_at
    }

    /// Why it ended, or [`None`] while it is open.
    ///
    /// Read back out of the events rather than kept in a field beside
    /// [`Self::ended_at`], so that there is one record of why a sitting ended.
    /// The event is what the sidecar carries and what the library reads out of
    /// it again (`clipped_library::index`), and a second copy could disagree
    /// with the first — including for a session rebuilt from a sidecar, which
    /// has the events and would have to remember to fill the field in as well
    /// (AGENTS.md section 55).
    ///
    /// The last one, for a session that somehow recorded two: what ended it is
    /// the last thing that happened to it.
    #[must_use]
    pub fn end_reason(&self) -> Option<SessionEndReason> {
        self.events
            .iter()
            .rev()
            .find_map(|event| match event.kind() {
                SessionEventKind::Ended { reason } => Some(*reason),
                _ => None,
            })
    }

    /// The recordings it contains, oldest first.
    #[must_use]
    pub fn recordings(&self) -> &[SessionRecording] {
        &self.recordings
    }

    /// The clips saved out of it, oldest first.
    #[must_use]
    pub fn clips(&self) -> &[SessionClip] {
        &self.clips
    }

    /// Its history.
    #[must_use]
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// The clips generated from this session's events, which have no file yet.
    #[must_use]
    pub fn generated(&self) -> &[VirtualClip] {
        &self.generated
    }

    /// Records what generation produced, replacing whatever it produced before.
    ///
    /// Replacing rather than appending, because generation is a pure function
    /// of the events and the rules: running it twice over the same session must
    /// answer the same clips, and appending would turn "the same answer" into a
    /// second copy of it. The generator is what avoids re-cutting a range it has
    /// already taken -- it is given the clips that exist and refuses to overlap
    /// them (`crate::highlights::generate`).
    pub fn record_generated(&mut self, clips: Vec<VirtualClip>) {
        self.generated = clips;
    }

    /// What happened *in the game*, oldest first.
    ///
    /// A different list from [`events`](Self::events) and deliberately so: those
    /// are the things the session manager did and this is what a plugin
    /// reported. See [`SessionEventKind`] for why the two vocabularies are not
    /// merged, and `docs/highlights.md` for why they are stored in two tables.
    #[must_use]
    pub fn game_events(&self) -> &[GameEvent] {
        &self.game_events
    }

    /// Where the sidecar for this session goes, given the recordings directory.
    #[must_use]
    pub fn sidecar_path(&self, directory: &Path) -> PathBuf {
        directory.join(format!("clipped-{}.session.json", self.id))
    }

    /// Where recording `index` of this session goes.
    ///
    /// The first recording of a session takes the plain name so that the
    /// overwhelmingly common case — one sitting, one file — produces a file
    /// named after the session and nothing else.
    #[must_use]
    pub fn recording_path(&self, directory: &Path, index: u32) -> PathBuf {
        let name = if index <= 1 {
            format!("clipped-{}.mkv", self.id)
        } else {
            format!("clipped-{}-{index}.mkv", self.id)
        };
        directory.join(name)
    }

    /// Where the `ordinal`th clip saved out of this session goes.
    ///
    /// Named from the session for the reason a recording is — everything one
    /// sitting produced sorts together in a directory listing — and numbered
    /// from one, because a session can produce many. The word `replay` is what
    /// keeps it out of the space recordings are named in: recording 2 of a
    /// session is `clipped-<id>-2.mkv`, and no recording is ever
    /// `clipped-<id>-replay-2.mkv`.
    #[must_use]
    pub fn clip_path(&self, directory: &Path, ordinal: u32) -> PathBuf {
        directory.join(format!("clipped-{}-replay-{ordinal}.mkv", self.id))
    }

    /// How many clips have been saved out of this session.
    ///
    /// What [`clip_path`](Self::clip_path)'s ordinal counts from: the next clip
    /// is this plus one.
    #[must_use]
    pub fn clips_saved(&self) -> u32 {
        u32::try_from(self.clips.len()).unwrap_or(u32::MAX)
    }

    /// Records a clip that has been written, and where it came from.
    pub(crate) fn add_clip(&mut self, clip: SessionClip) {
        self.record(
            clip.created_at,
            SessionEventKind::ReplaySaved {
                index: clip.source_index,
                output: clip.path.clone(),
            },
        );
        self.clips.push(clip);
    }

    /// Adds an event.
    pub(crate) fn record(&mut self, at: SystemTime, kind: SessionEventKind) {
        self.events.push(SessionEvent { at, kind });
    }

    /// Adds something a plugin reported.
    ///
    /// Kept in the order events happened rather than the order they arrived: a
    /// plugin observes a game telling it something, so two events can be heard
    /// out of order and the moment each *describes* is the one a timeline draws
    /// at. `EventTiming` keeps those apart precisely so that this can sort by
    /// the former.
    ///
    /// Nothing is dropped and nothing is deduplicated here. An event this build
    /// does not recognise is still the session's, and deciding what is worth a
    /// clip is the highlight rules' job rather than the session's
    /// (`crates/session/src/highlights/`).
    pub fn record_game_event(&mut self, event: GameEvent) {
        let at = event.timing().at();
        let position = self
            .game_events
            .partition_point(|existing| existing.timing().at() <= at);
        self.game_events.insert(position, event);
    }

    /// Adds a recording that has just started, with the settings it was
    /// resolved for.
    pub(crate) fn begin_recording(
        &mut self,
        index: u32,
        output: PathBuf,
        settings: ResolvedSettings,
        at: SystemTime,
    ) {
        self.recordings.push(SessionRecording {
            index,
            starts_at_nanos: None,
            output: output.clone(),
            started_at: at,
            ended_at: None,
            outcome: None,
            settings,
        });
        self.record(at, SessionEventKind::RecordingStarted { index, output });
    }

    /// Records what a recording turned out to be.
    pub(crate) fn end_recording(
        &mut self,
        index: u32,
        outcome: RecordingOutcomeSummary,
        at: SystemTime,
    ) {
        let token = outcome.token().to_owned();
        if let Some(recording) = self
            .recordings
            .iter_mut()
            .find(|recording| recording.index == index)
        {
            recording.ended_at = Some(at);
            recording.outcome = Some(outcome);
        }
        self.record(
            at,
            SessionEventKind::RecordingEnded {
                index,
                outcome: token,
            },
        );
    }

    /// Closes the session.
    pub(crate) fn end(&mut self, reason: SessionEndReason, at: SystemTime) {
        self.ended_at = Some(at);
        self.record(at, SessionEventKind::Ended { reason });

        // After the session is closed, not during it. Generation reads the
        // recordings' spans and the events, and both are only complete once
        // nothing more can be added -- a clip cut from a session still running
        // would be cut from half of it.
        //
        // It is arithmetic over events: no encoder, no filesystem, and nothing
        // a capture thread waits for. A session that heard nothing generates
        // nothing, which costs a walk of an empty list.
        let generated = crate::highlights::generate_for(self);
        if !generated.is_empty() {
            tracing::info!(
                session = %self.id(),
                clips = generated.len(),
                "this session's events were worth clips"
            );
        }
        self.record_generated(generated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> GameIdentity {
        GameIdentity::Known {
            game_id: "counter-strike-2".to_owned(),
            name: "Counter-Strike 2".to_owned(),
        }
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// The settings of a user who has configured nothing.
    fn defaults() -> ResolvedSettings {
        crate::config::Configuration::defaults().resolve_global()
    }

    #[test]
    fn an_identifier_is_the_game_and_the_moment_the_session_started() {
        // 2026-08-11T14:32:05Z, stamped in UTC so the assertion is about the
        // shape rather than about where the machine running it is.
        let id = SessionId::at(&known(), at(1_786_458_725), time::UtcOffset::UTC);
        assert_eq!(id.as_str(), "counter-strike-2-20260811-143205");
    }

    #[test]
    fn a_tie_is_filed_under_a_name_that_is_none_of_the_candidates() {
        // Filing an ambiguous session under one of the candidates is exactly
        // the guess the catalogue refuses to make, and it would be invisible
        // afterwards: the file would simply be named after the wrong game.
        let ambiguous = GameIdentity::Ambiguous {
            candidates: vec!["first-game".to_owned(), "second-game".to_owned()],
        };
        assert_eq!(ambiguous.slug(), UNATTRIBUTED);
        let id = SessionId::new(&ambiguous, at(1_786_458_725));
        assert!(
            !id.as_str().contains("first-game") && !id.as_str().contains("second-game"),
            "{id}"
        );
    }

    #[test]
    fn the_first_recording_of_a_session_is_named_after_the_session_alone() {
        let session = Session::new(known(), at(1_786_458_725));
        let directory = Path::new(r"D:\clips");
        let id = session.id();

        assert_eq!(
            session.recording_path(directory, 1),
            directory.join(format!("clipped-{id}.mkv"))
        );
        assert_eq!(
            session.recording_path(directory, 2),
            directory.join(format!("clipped-{id}-2.mkv"))
        );
        assert_eq!(
            session.sidecar_path(directory),
            directory.join(format!("clipped-{id}.session.json"))
        );
    }

    #[test]
    fn two_ambiguous_matches_are_the_same_game_only_when_they_tied_the_same_way() {
        let one = GameIdentity::Ambiguous {
            candidates: vec!["a".to_owned(), "b".to_owned()],
        };
        let same = GameIdentity::Ambiguous {
            candidates: vec!["a".to_owned(), "b".to_owned()],
        };
        let different = GameIdentity::Ambiguous {
            candidates: vec!["a".to_owned(), "c".to_owned()],
        };

        assert!(one.is_same_game_as(&same));
        assert!(!one.is_same_game_as(&different));
        assert!(!one.is_same_game_as(&known()));
    }

    #[test]
    fn a_recording_that_ended_is_stored_against_the_recording_it_ended() {
        let mut session = Session::new(known(), at(1_000));
        session.begin_recording(1, PathBuf::from("one.mkv"), defaults(), at(1_000));
        session.begin_recording(2, PathBuf::from("two.mkv"), defaults(), at(2_000));
        session.end_recording(
            1,
            RecordingOutcomeSummary::Failed {
                detail: "the encoder went".to_owned(),
            },
            at(1_500),
        );

        assert_eq!(session.recordings()[0].ended_at(), Some(at(1_500)));
        assert!(matches!(
            session.recordings()[0].outcome(),
            Some(RecordingOutcomeSummary::Failed { .. })
        ));
        assert_eq!(
            session.recordings()[1].outcome(),
            None,
            "the second recording is still running and must not have been ended"
        );
    }
}
