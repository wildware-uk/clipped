//! The commands the desktop application sends, and the replies it gets back.
//!
//! # Why a command is a name and a bag of parameters on the wire
//!
//! A [`Request`](crate::Request) carries `command` as a string and `params` as
//! an untyped object, and [`Command::from_request`] is what turns that into
//! something typed. The obvious alternative — one `serde` enum tagged by the
//! command name — was rejected because of what it does when it fails: a command
//! name the recorder has never heard of becomes a deserialisation error, which
//! is indistinguishable from a corrupt frame, and the useful part (which name)
//! survives only inside `serde`'s English. Parsing the envelope first means an
//! unknown command is [`ErrorCode::UnknownCommand`] *naming the command*, and a
//! command whose parameters are wrong is [`ErrorCode::InvalidParameters`]
//! carrying what was wrong with them — which is the difference between a UI
//! that can say what happened and one that says "protocol error".
//!
//! # Commands whose subsystem does not exist yet
//!
//! There are none left. The protocol used to define a command it could not
//! perform — one that parsed, so that the refusal could name it, and was then
//! refused with [`ErrorCode::NotImplemented`], the milestone and the issue.
//! `save_replay` was one until
//! [issue #38](https://github.com/wildware-uk/clipped/issues/38) and
//! `apply_settings` was the last, until the settings reached the protocol
//! ([issue #51](https://github.com/wildware-uk/clipped/issues/51),
//! `crate::settings`). Every command this build defines, it performs.
//!
//! The shape is still in the protocol for the thing that still needs it:
//! [`EventStream::Metrics`](crate::EventStream) is a subscription this build
//! refuses the same way, because nothing measures those figures yet.
//!
//! # The command with no handler behind it
//!
//! [`Command::Shutdown`] is the one command a [`CommandHandler`](crate::CommandHandler)
//! never sees: not because nothing can perform it, but because the thing that
//! performs it is the accept loop rather than the application.
//! `crates/ipc/src/server.rs` answers it.

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, ProtocolError};
use crate::hotkeys::HotkeyBinding;
use crate::library::{
    EmptyTrash, FavouriteMark, LibraryEventLane, LibraryEvents, LibraryGame, LibrarySessionPage,
    LibrarySessions, LibraryTrash, LockMark, RestoreFromTrash, RestoredItem, SetFavourite, SetLock,
    TrashEmptied, TrashListing,
};
use crate::message::Request;
use crate::playback::{OpenPlayback, PlaybackStream};
use crate::plugins::{PluginDeclaration, RefusedPlugin};
use crate::preview::{OpenPreview, Preview};
use crate::settings::{
    ApplySettings, AudioDevices, MicrophoneLevel, MicrophoneLevelRequest, SettingsView,
};
use crate::startup::{SetStartAtLogin, StartAtLogin};
use crate::status::{
    BookmarkSummary, ExportSummary, RecorderStatus, RecordingSummary, ReplaySummary,
    ScreenshotSummary,
};

/// A command, parsed and known to be one this build understands.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Is the recorder alive, and how long does a round trip take.
    Ping,
    /// What is the recorder doing.
    GetStatus,
    /// Start recording something.
    StartRecording(StartRecording),
    /// Stop the recording that is running.
    StopRecording(StopRecording),
    /// Mark this moment in the recording that is running.
    AddBookmark(AddBookmark),
    /// Save a still image of what is being captured.
    TakeScreenshot(TakeScreenshot),
    /// Keep the last few seconds of what is being recorded, as a clip.
    SaveReplay(SaveReplay),
    /// Read a page of the recording library: the sittings and what they
    /// produced.
    LibrarySessions(LibrarySessions),
    /// Read what the library holds per game (SPEC.md section 17).
    LibraryGames,

    /// The game events of one recording, placed in its file.
    LibraryEvents(LibraryEvents),
    /// What is in the trash: everything deleted and not yet emptied.
    ///
    /// A read, like the three above it. Restoring and emptying are separate
    /// commands because they change something, and because emptying takes back
    /// the listing it was shown
    /// ([issue #450](https://github.com/wildware-uk/clipped/issues/450)).
    LibraryTrash(LibraryTrash),
    /// Put one thing back where it was.
    ///
    /// Separate from listing because it changes something, and named by kind
    /// and identifier because that is what a listing gave the window.
    RestoreFromTrash(RestoreFromTrash),
    /// Destroy everything in the trash, confirmed against the listing that was
    /// shown.
    ///
    /// Both numbers are checked. A trash that has gained an item since the
    /// listing is refused rather than emptied, because the alternative is
    /// deleting something the user never saw
    /// ([issue #450](https://github.com/wildware-uk/clipped/issues/450)).
    EmptyTrash(EmptyTrash),
    /// Mark one thing a favourite, or clear the mark.
    SetFavourite(SetFavourite),
    /// Lock one thing against automatic cleanup, or unlock it.
    SetLock(SetLock),

    /// What plugins are installed, what each declares, and what will start.
    Plugins,
    /// Copy a finished recording into MP4, without re-encoding it.
    ExportRecording(ExportRecording),
    /// Open a finished recording so that the desktop window can play it, with
    /// one of its sound tracks.
    ///
    /// Asked of the recorder rather than done in the window for two reasons
    /// that are both hard: the window links no demuxer, and a media element
    /// cannot choose an audio track (`crate::playback`, issue #304).
    OpenPlayback(OpenPlayback),
    /// The picture for a finished recording, or the peaks of its sound.
    ///
    /// Asked of the recorder for the reason the library commands are: both are
    /// generated beside the recording, in the recorder's process, and the
    /// window can neither read a file nor link the crates that made them
    /// (`crate::preview`, issue #448).
    OpenPreview(OpenPreview),
    /// What every global hotkey is bound to, and whether it works.
    ///
    /// Asked rather than pushed because registration happens when the recorder
    /// starts, which is usually before any window exists to be told
    /// (`crate::hotkeys`).
    GetHotkeys,
    /// Every setting, what it resolves to, and whether anything reads it.
    ///
    /// Asked of the recorder rather than read from the file, because the
    /// recorder is the process that owns `settings.json` and the only one that
    /// knows which settings this build acts on (`crate::settings`).
    GetSettings,
    /// Change settings, and save them.
    ///
    /// Answered with the settings as they now stand, so a window draws what was
    /// saved rather than what it hoped had been.
    ApplySettings(ApplySettings),
    /// Which audio endpoints this machine has.
    ///
    /// Step 3 of SPEC.md section 45's MVP is picking a microphone, and a name
    /// is matched against the endpoints present when a recording starts — so a
    /// window has to be able to show what is there rather than asking somebody
    /// to type a name they cannot see.
    GetAudioDevices,
    /// What one microphone is hearing at this moment.
    ///
    /// The other half of step 3 of SPEC.md section 45's MVP. A list of endpoint
    /// names says which microphones exist and nothing about which of them can
    /// hear the person choosing — and on a machine with a webcam, a headset and
    /// a monitor's array microphone, that is the whole of the question. Answered
    /// by opening the endpoint the setting names, listening briefly, and closing
    /// it again (`crate::settings::MicrophoneLevel`).
    GetMicrophoneLevel(MicrophoneLevelRequest),
    /// Whether the recorder starts when this user signs in.
    ///
    /// Not a setting: it is a `Run` value Windows reads at sign-in rather than
    /// a key `settings.json` carries, and it is read here rather than in the
    /// window because the window cannot see the registry
    /// (`crate::startup`, issue #308).
    GetStartAtLogin,
    /// Turn starting at login on or off.
    ///
    /// Answered with the arrangement as it now stands, for the reason
    /// [`Self::ApplySettings`] is: a switch drawn from what a window asked for
    /// rather than from what the registry did would show "on" for a write that
    /// was refused.
    SetStartAtLogin(SetStartAtLogin),
    /// Stop serving, finish anything still being recorded, and exit.
    ///
    /// The one command not performed by a [`CommandHandler`](crate::CommandHandler):
    /// it is answered by [`crate::server`], which owns the accept loop and is
    /// therefore the only thing that can end it. See
    /// [`ShutdownRequest`](crate::server::ShutdownRequest) for the contract that
    /// makes "and exit" true rather than aspirational.
    Shutdown(Shutdown),
}

impl Command {
    /// The command's name on the wire.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::GetStatus => "get_status",
            Self::StartRecording(_) => "start_recording",
            Self::StopRecording(_) => "stop_recording",
            Self::AddBookmark(_) => "add_bookmark",
            Self::TakeScreenshot(_) => "take_screenshot",
            Self::SaveReplay(_) => "save_replay",
            Self::LibrarySessions(_) => "library_sessions",
            Self::LibraryGames => "library_games",
            Self::LibraryEvents(_) => "library_events",
            Self::LibraryTrash(_) => "library_trash",
            Self::RestoreFromTrash(_) => "restore_from_trash",
            Self::EmptyTrash(_) => "empty_trash",
            Self::SetFavourite(_) => "set_favourite",
            Self::SetLock(_) => "set_lock",
            Self::Plugins => "plugins",
            Self::ExportRecording(_) => "export_recording",
            Self::OpenPlayback(_) => "open_playback",
            Self::OpenPreview(_) => "open_preview",
            Self::GetHotkeys => "get_hotkeys",
            Self::GetSettings => "get_settings",
            Self::ApplySettings(_) => "apply_settings",
            Self::GetAudioDevices => "get_audio_devices",
            Self::GetMicrophoneLevel(_) => "get_microphone_level",
            Self::GetStartAtLogin => "get_start_at_login",
            Self::SetStartAtLogin(_) => "set_start_at_login",
            Self::Shutdown(_) => "shutdown",
        }
    }

    /// Parses a request into a command this build knows.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::UnknownCommand`] if no command has that name, naming the
    /// one that was asked for; [`ErrorCode::InvalidParameters`] if the
    /// parameters are not the shape the command takes, naming the command and
    /// carrying `serde`'s account of what was wrong with the value.
    pub fn from_request(request: &Request) -> Result<Self, ProtocolError> {
        match request.command.as_str() {
            "ping" => Ok(Self::Ping),
            "get_status" => Ok(Self::GetStatus),
            "start_recording" => Ok(Self::StartRecording(parse_params(request)?)),
            "stop_recording" => Ok(Self::StopRecording(parse_params(request)?)),
            "add_bookmark" => Ok(Self::AddBookmark(parse_params(request)?)),
            "take_screenshot" => Ok(Self::TakeScreenshot(parse_params(request)?)),
            "save_replay" => Ok(Self::SaveReplay(parse_params(request)?)),
            "library_sessions" => Ok(Self::LibrarySessions(parse_params(request)?)),
            "library_games" => Ok(Self::LibraryGames),
            "library_events" => Ok(Self::LibraryEvents(parse_params(request)?)),
            "library_trash" => Ok(Self::LibraryTrash(parse_params(request)?)),
            "restore_from_trash" => Ok(Self::RestoreFromTrash(parse_params(request)?)),
            "empty_trash" => Ok(Self::EmptyTrash(parse_params(request)?)),
            "set_favourite" => Ok(Self::SetFavourite(parse_params(request)?)),
            "set_lock" => Ok(Self::SetLock(parse_params(request)?)),
            "plugins" => Ok(Self::Plugins),
            "export_recording" => Ok(Self::ExportRecording(parse_params(request)?)),
            "open_playback" => Ok(Self::OpenPlayback(parse_params(request)?)),
            "open_preview" => Ok(Self::OpenPreview(parse_params(request)?)),
            "get_hotkeys" => Ok(Self::GetHotkeys),
            "get_settings" => Ok(Self::GetSettings),
            "apply_settings" => Ok(Self::ApplySettings(parse_params(request)?)),
            "get_audio_devices" => Ok(Self::GetAudioDevices),
            "get_microphone_level" => Ok(Self::GetMicrophoneLevel(parse_params(request)?)),
            "get_start_at_login" => Ok(Self::GetStartAtLogin),
            "set_start_at_login" => Ok(Self::SetStartAtLogin(parse_params(request)?)),
            "shutdown" => Ok(Self::Shutdown(parse_params(request)?)),
            name => Err(ProtocolError::new(
                ErrorCode::UnknownCommand,
                format!("this recorder has no `{name}` command"),
            )),
        }
    }

    /// Builds the request that carries this command, with the identifier the
    /// reply will quote.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] if the parameters cannot be
    /// represented, which for the types here means a `f64` that is not a
    /// number. Kept as an error rather than a panic because a client builds
    /// these from user input.
    pub fn to_request(&self, id: u64) -> Result<Request, ProtocolError> {
        let params = match self {
            Self::Ping
            | Self::GetStatus
            | Self::LibraryGames
            | Self::Plugins
            | Self::GetHotkeys
            | Self::GetSettings
            | Self::GetAudioDevices
            | Self::GetStartAtLogin => Ok(serde_json::Value::Null),
            Self::LibrarySessions(listing) => serde_json::to_value(listing),
            Self::LibraryEvents(request) => serde_json::to_value(request),
            Self::LibraryTrash(request) => serde_json::to_value(request),
            Self::RestoreFromTrash(request) => serde_json::to_value(request),
            Self::EmptyTrash(request) => serde_json::to_value(request),
            Self::SetFavourite(request) => serde_json::to_value(request),
            Self::SetLock(request) => serde_json::to_value(request),
            Self::StartRecording(start) => serde_json::to_value(start),
            Self::GetMicrophoneLevel(request) => serde_json::to_value(request),
            Self::StopRecording(stop) => serde_json::to_value(stop),
            Self::AddBookmark(bookmark) => serde_json::to_value(bookmark),
            Self::TakeScreenshot(screenshot) => serde_json::to_value(screenshot),
            Self::SaveReplay(replay) => serde_json::to_value(replay),
            Self::ExportRecording(export) => serde_json::to_value(export),
            Self::OpenPlayback(playback) => serde_json::to_value(playback),
            Self::OpenPreview(preview) => serde_json::to_value(preview),
            Self::ApplySettings(settings) => serde_json::to_value(settings),
            Self::SetStartAtLogin(request) => serde_json::to_value(request),
            Self::Shutdown(shutdown) => serde_json::to_value(shutdown),
        }
        .map_err(|error| {
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                format!("`{}` could not be represented: {error}", self.name()),
            )
        })?;

        Ok(Request {
            id,
            command: self.name().to_owned(),
            params,
        })
    }
}

/// Reads a command's parameters out of the request carrying them.
///
/// A missing or null `params` is the same as an empty object, so a command
/// whose fields are all optional can be sent without one.
fn parse_params<T: serde::de::DeserializeOwned>(request: &Request) -> Result<T, ProtocolError> {
    let params = if request.params.is_null() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        request.params.clone()
    };

    serde_json::from_value(params).map_err(|error| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!(
                "`{}` was not given usable parameters: {error}",
                request.command
            ),
        )
    })
}

/// What to record, and how.
///
/// The fields are the `clipped-recorder record` options under the names they
/// have on the command line, and the recorder validates them through the same
/// code path — so a value the command line rejects is rejected here, with the
/// same message. One set of rules, in one place (AGENTS.md section 55).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StartRecording {
    /// Record the window whose title contains this text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// Record the window belonging to this executable, such as `cs2.exe`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Record the window belonging to this process identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Where to write the recording. Absent means the recorder's own
    /// timestamped default under the user's videos directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Whether an existing file at `output` may be replaced. A recording is
    /// never overwritten silently.
    pub overwrite: bool,
    /// `source`, or `WIDTHxHEIGHT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Frames per second to encode at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framerate: Option<u32>,
    /// `auto`, `h264`, `hevc` or `av1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// `auto`, `nvenc`, `amf`, `quicksync` or `software`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    /// `default`, `none`, or part of a device name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub microphone: Option<String>,
    /// `default`, `none`, or part of a device name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_audio: Option<String>,
    /// Keep the last this many seconds of the recording, so that `save_replay`
    /// has something to save.
    ///
    /// Absent means no buffer *unless* [`replay`](Self::replay) asks for one,
    /// which is what it has always meant: a buffer costs a recording a spill
    /// directory and the newest few seconds in memory
    /// (`docs/replay-buffer.md`), so it is kept only when somebody asked for
    /// one. The supported range is `clipped-replay`'s, 30 to 1800 seconds, and
    /// a value outside it is refused with that crate's own message rather than
    /// a second opinion about it.
    ///
    /// It is on `start_recording` rather than on `save_replay` because it is
    /// a property of the *recording*: a buffer has to have been filling since
    /// before the thing somebody wants to keep happened, and no request made
    /// afterwards can conjure the history it did not keep.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_seconds: Option<u32>,
    /// Keep a replay buffer, at the length this recorder has configured.
    ///
    /// [`replay_seconds`](Self::replay_seconds) *names* a length; this asks for
    /// a buffer without naming one, and the recorder answers with
    /// `replay_window_seconds` resolved for whatever game the target turns out
    /// to be. That is a question only the recorder can answer: the setting
    /// inherits per game (AGENTS.md section 30), and the game is what the
    /// catalogue makes of the window this request is still only a process
    /// identifier for. A caller that resolved a length itself would be a second
    /// place that setting is decided, and a caller that made one up would be
    /// recording to a duration nobody chose.
    ///
    /// `false` with no `replay_seconds` is no buffer, so a client written
    /// before this field keeps the recordings it always had. Both at once keeps
    /// the length that was named: a request that says a number has already
    /// answered the question this asks.
    pub replay: bool,
}

/// Which recording to stop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StopRecording {
    /// The recording to stop, as
    /// [`ActiveRecording::recording_id`](crate::ActiveRecording::recording_id)
    /// reported it.
    ///
    /// Absent means "whatever is running", which is what a tray menu wants.
    /// Naming it is what a UI does when the stop was aimed at a recording it
    /// had on screen, so that a recording which ended by itself in the meantime
    /// cannot have its successor stopped instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<String>,
}

/// What to mark, and how far before the press to mark it.
///
/// Every field is optional, because the command a hotkey sends carries none of
/// them: pressing the key is the whole interaction, and it must not stop to ask
/// for a label while somebody is playing (SPEC.md section 25).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AddBookmark {
    /// Which recording to mark, as
    /// [`ActiveRecording::recording_id`](crate::ActiveRecording::recording_id)
    /// reported it.
    ///
    /// Absent means "whatever is being recorded", which is what a hotkey and a
    /// tray menu want. Naming it is what a window that had a particular
    /// recording on screen does, so that a mark meant for a recording which
    /// ended in the meantime does not land in its successor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<String>,
    /// What to call the bookmark.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A colour, in whatever notation the interface writes; the recorder stores
    /// it and does not interpret it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    /// How long the marked moment lasts. Absent means it is a moment rather
    /// than a span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// How far *before* this request the bookmark should be stamped.
    ///
    /// Absent means the recorder's own default, which is not zero: a person
    /// presses the key after the thing they wanted to mark, so a bookmark
    /// stamped at the press is reliably late. `docs/bookmarks.md` gives the
    /// figure and the reasoning. A caller with no human reaction to allow for —
    /// a plugin marking an event it detected itself — sends `0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_seconds: Option<f64>,
}

/// What to photograph, and how to save it.
///
/// Every field is optional, because the shape a hotkey and a tray menu send is
/// no fields at all: photograph whatever is being recorded, in the configured
/// format, and put it where screenshots go (SPEC.md section 26).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TakeScreenshot {
    /// Which recording to take the picture from, as
    /// [`ActiveRecording::recording_id`](crate::ActiveRecording::recording_id)
    /// reported it.
    ///
    /// Absent means "whatever is being recorded", exactly as it does for
    /// [`AddBookmark`]. Naming it is what a window that had a particular
    /// recording on screen does, so that a screenshot meant for a recording
    /// which ended in the meantime is refused rather than taken of its
    /// successor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<String>,
    /// Photograph the window whose title contains this text.
    ///
    /// The three target fields — this, [`Self::process`] and [`Self::pid`] —
    /// are only consulted when **nothing is being recorded**. A screenshot
    /// taken during a recording comes from a frame that recording already
    /// captured, which is both far cheaper and the only way to be sure the
    /// picture is of what is being recorded.
    ///
    /// They are the same three [`StartRecording`] takes, under the same names,
    /// so a caller that can start a recording of something can photograph it
    /// (AGENTS.md section 43).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// Photograph the window belonging to this executable, such as `cs2.exe`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Photograph the window belonging to this process identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// `png`, `jpeg` or `webp`. Absent means the recorder's own default.
    ///
    /// A string rather than an enumeration for the reason
    /// [`StartRecording::codec`] is one: the recorder validates it through the
    /// same code path its own settings go through, so a value it would refuse
    /// on the command line is refused here with the same message (AGENTS.md
    /// section 55).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

/// How much of the replay buffer to keep, and where to put it.
///
/// **Every field is optional, and that is the whole design.** The shape a hotkey
/// sends is no fields at all: keep the recorder's configured duration out of
/// whatever is being recorded, and put it where that recording's clips go. A
/// person pressing Ctrl+F10 mid-fight has said everything they are going to say
/// (SPEC.md sections 25 and 34).
///
/// Each field exists because a *different* caller needs it, and each was weighed
/// against leaving it out:
///
/// - **`recording_id`** — the same field [`AddBookmark`] and [`TakeScreenshot`]
///   carry, meaning the same thing: absent is "whatever is being recorded",
///   which is what a hotkey and a tray item want; naming one is what a window
///   that had a particular recording on screen does, so that a save meant for a
///   recording which ended in the meantime is refused rather than taken out of
///   its successor.
/// - **`duration_seconds`** — SPEC.md section 15 asks for user-selectable
///   periods (15 s, 30 s, 1 min, 2 min, 5 min and a custom one), so this cannot
///   be fixed by the recorder; and it cannot be *required* either, because the
///   hotkey has nowhere to carry it. Absent means the duration the recording was
///   started with. It is a number of seconds rather than a name because
///   "custom" is one of the choices, and an enumeration of five values plus an
///   escape hatch is an enumeration with a hole in it.
/// - **`output`** — absent means beside the recording, named after the session,
///   which is what makes a hotkey press a complete interaction. A caller that
///   names one is doing what `export_recording` does: choosing where a file it
///   asked for goes. A destination that already exists is refused rather than
///   replaced, exactly as an export's is (AGENTS.md section 56).
///
/// What is deliberately **not** here is a range: no start, no end, no "from
/// this moment". A replay buffer holds the last N seconds of *now*, and a
/// request for an arbitrary window of a recording is a different feature over a
/// different source — the clip editor's, in M11 — that would need the file
/// rather than the buffer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveReplay {
    /// Which recording to save out of, as
    /// [`ActiveRecording::recording_id`](crate::ActiveRecording) reported it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_id: Option<String>,
    /// How many seconds to keep. Absent means the duration the recording's
    /// buffer was started with.
    ///
    /// A save longer than the buffer's window is not refused: the buffer cannot
    /// hold more than its window, so the clip is what there was and
    /// [`ReplaySummary::complete`](crate::ReplaySummary) says it was short.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// Where to write the clip. Absent means beside the recording, named after
    /// the session it belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Which recording to copy into MP4, and where to put the copy.
///
/// Both fields are required, and neither has a default. That is deliberate on
/// both counts:
///
/// - **The source** is a file the caller already knows about, because it read it
///   out of the library ([`LibraryRecording::path`](crate::LibraryRecording)).
///   There is no "the recording you just made" shorthand, because the thing
///   somebody exports is usually not the thing they just recorded, and a
///   command that guessed would export the wrong file in the one case where
///   nobody would check.
/// - **The destination** is chosen by whoever asked. The recorder has a default
///   place for a recording and for a screenshot, because it creates those
///   without being asked; an export is a thing a person asked for, at a moment
///   they were looking at a screen, and the place it goes is theirs to say. A
///   destination that already exists is **refused** rather than replaced
///   ([`ErrorCode::DestinationExists`]), which is AGENTS.md section 56: a file
///   nobody can get back must not be destroyed to save a dialog.
///
/// Neither carries a `serde` default either, which is the same decision on the
/// wire: a request that omits one is [`ErrorCode::InvalidParameters`] naming the
/// field it left out, rather than a request carrying an empty path that
/// something further down has to recognise as "not given". The recorder still
/// refuses a path that is only whitespace, because an empty string is a value
/// `serde` accepts and a file name it is not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRecording {
    /// The recording to copy, as the library reported its path.
    ///
    /// It is opened for reading and is not modified, whether the export
    /// succeeds or fails.
    pub source: String,
    /// Where to write the MP4.
    pub destination: String,
}

/// How far a shutdown may go.
///
/// The parameter exists because the two answers to "the user asked to exit and a
/// recording is running" are both wrong as a default. Exiting regardless would
/// let anything running as this user end somebody's recording by sending four
/// words down a pipe; refusing outright would leave a recorder that can never be
/// stopped while it is recording, which is
/// [issue #220](https://github.com/wildware-uk/clipped/issues/220) again in a
/// smaller shape.
///
/// So the recorder refuses by default and performs it when asked in as many
/// words. A caller that has not thought about the recording cannot end one by
/// accident, and one that has — a tray menu whose item reads "Stop recording and
/// exit" — says so in the request. Either way the file is finished and playable:
/// the recording is stopped through the recorder's ordinary shutdown path, not
/// abandoned (AGENTS.md section 17).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shutdown {
    /// Whether a recording in progress may be stopped and its file finished
    /// first.
    ///
    /// `false` — the default, and what a request that omits it means — refuses
    /// the shutdown with [`ErrorCode::AlreadyRecording`] while something is
    /// being recorded, naming the recording so the caller can put the question
    /// to the user.
    pub finalise_recording: bool,
}

/// What a command produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// The recorder is alive.
    Pong,
    /// What the recorder is doing.
    Status {
        /// The state, as of the moment the recorder answered.
        status: RecorderStatus,
    },
    /// A recording started.
    RecordingStarted {
        /// Identifies it to `stop_recording`.
        recording_id: String,
        /// The file it is writing.
        output: String,
    },
    /// A recording stopped, and its file is finished.
    RecordingStopped {
        /// What it turned out to be.
        summary: RecordingSummary,
    },
    /// A moment was marked, and the mark is on disk.
    BookmarkAdded {
        /// Where it landed, which is not where the key was pressed.
        bookmark: BookmarkSummary,
    },
    /// A screenshot was taken, and the file is on disk.
    ScreenshotTaken {
        /// The file, and what is in it.
        screenshot: ScreenshotSummary,
    },
    /// A replay was saved, and the clip is finished and playable.
    ///
    /// Sent **after** the container's trailer has been written, for the reason
    /// [`Self::RecordingStopped`] is: a window that said "saved" before that
    /// would be pointing at a file that is not yet playable.
    ReplaySaved {
        /// The clip, and how it compares with what was asked for.
        clip: ReplaySummary,
    },
    /// One page of the recording library.
    LibrarySessions {
        /// The sittings, newest first, and where the next page starts.
        ///
        /// An empty page is this reply, not a refusal: a library that could not
        /// be read is
        /// [`ErrorCode::LibraryUnavailable`](crate::ErrorCode::LibraryUnavailable)
        /// and says why.
        page: LibrarySessionPage,
    },
    /// What plugins are installed, and what each of them asks for.
    Plugins {
        /// Every plugin discovery could read, in the order it found them.
        installed: Vec<PluginDeclaration>,
        /// Everything under the plugins directory that is not one, and why.
        ///
        /// Always sent, so that "nothing was refused" and "the question was not
        /// asked" are told apart by whether the reply arrived.
        refused: Vec<RefusedPlugin>,
    },
    /// The marks of one recording's timeline.
    LibraryEvents {
        /// The events, placed in that recording's file, earliest first.
        ///
        /// Always sent, so that "none" and "not asked" are told apart by
        /// whether the reply arrived rather than by guessing at an empty field.
        lane: LibraryEventLane,
    },
    /// What is waiting in the trash.
    LibraryTrash {
        /// Everything in it, what it occupies, and where it is.
        ///
        /// Always sent, so that an empty trash and a question nobody answered
        /// are told apart by whether the reply arrived.
        trash: TrashListing,
    },
    /// One thing was put back where it was.
    Restored {
        /// What came back, and whether its file did.
        restored: RestoredItem,
    },
    /// The trash was emptied.
    TrashEmptied {
        /// What was destroyed, and what would not go.
        emptied: TrashEmptied,
    },
    /// A favourite mark was set or cleared.
    Favourited {
        /// Which thing, and what its mark is now.
        mark: FavouriteMark,
    },
    /// A lock was set or cleared.
    Locked {
        /// Which thing, what its lock is now, and whether cleanup will leave
        /// it alone.
        lock: LockMark,
    },
    /// What the library holds per game.
    LibraryGames {
        /// One row per game, and one for the sittings nothing was attributed
        /// to.
        games: Vec<LibraryGame>,
    },
    /// A recording was copied into MP4, and the file is finished.
    ///
    /// Sent **after** the container's index has been written, for the reason
    /// [`Self::RecordingStopped`] is: a window that said "exported" before that
    /// would be pointing at a file that is not yet playable.
    RecordingExported {
        /// The copy, and what it turned out to hold.
        export: ExportSummary,
    },
    /// A recording is ready to be played, and here is what to play.
    ///
    /// Sent after any copy it needed has been finished, for the reason
    /// [`Self::RecordingExported`] is: a window pointed at a file that is still
    /// being written would draw a player over a truncated recording.
    PlaybackOpened {
        /// The file to play, the track it carries, and the tracks that could be
        /// chosen instead.
        playback: PlaybackStream,
    },
    /// A recording's thumbnail or the peaks of its sound, as far as either
    /// exists.
    ///
    /// One reply for both, because the transport is the point: a picture and a
    /// set of peaks are both derived from a finished recording, both cached
    /// outside the library index, and both unreachable from a window that has
    /// no file-system permission. Two mechanisms for that would be two things
    /// to keep in step (`crate::preview`, issue #448).
    PreviewOpened {
        /// Where it stands, and — when it is here — the picture or the peaks.
        preview: Preview,
    },
    /// Every action a global hotkey can perform, and where each one stands.
    Hotkeys {
        /// One row per action, in the order a configuration screen lists them.
        ///
        /// Always the whole list, including the actions bound to nothing: a
        /// screen that showed only the bound ones could not offer the rest, and
        /// an action missing from the list is indistinguishable from an action
        /// the recorder has never heard of.
        hotkeys: Vec<HotkeyBinding>,
    },
    /// Every setting, as it now stands.
    ///
    /// The answer to `get_settings` **and** to `apply_settings`, deliberately:
    /// a window that drew its own idea of what it had just saved would show a
    /// value the recorder had refused to write, or one another window had
    /// changed a moment earlier. What it draws is what came back
    /// (`crate::settings`).
    Settings {
        /// The settings, and the file they live in.
        settings: SettingsView,
    },
    /// The audio endpoints this machine has.
    AudioDevices {
        /// Every microphone, with the default one marked.
        devices: AudioDevices,
    },
    /// What the microphone that was asked about is hearing.
    MicrophoneLevel {
        /// The reading, and what was listened to.
        level: MicrophoneLevel,
    },
    /// Whether the recorder starts at sign-in, as it now stands.
    ///
    /// The answer to `get_start_at_login` **and** to `set_start_at_login`, for
    /// the reason [`Self::Settings`] answers both settings commands: what a
    /// window draws is what the registry says, never what the window asked for
    /// (`crate::startup`).
    StartAtLogin {
        /// The arrangement: whether it is on, what would run, and whether that
        /// still exists.
        start_at_login: StartAtLogin,
    },
    /// The recorder has stopped listening and is winding up.
    ///
    /// Sent **before** the recorder exits, because a reply written after the
    /// process ended would never arrive. What it promises is that the shutdown
    /// was accepted and the endpoint is closing; the observable proof that it
    /// finished is the endpoint going away, which
    /// [`supervisor::wait_for_recorder_to_exit`](crate::supervisor::wait_for_recorder_to_exit)
    /// waits for.
    ShuttingDown {
        /// The recording that will be stopped and finished before the recorder
        /// exits, if there was one.
        ///
        /// Absent when nothing was being recorded. Present only for a shutdown
        /// that asked for [`Shutdown::finalise_recording`], because one that did
        /// not is refused rather than answered while a recording is running —
        /// so this names a file the caller can tell the user about rather than a
        /// file it has just silently ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finalising: Option<crate::status::ActiveRecording>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: &str, params: serde_json::Value) -> Request {
        Request {
            id: 1,
            command: command.to_owned(),
            params,
        }
    }

    #[test]
    fn a_command_with_no_parameters_parses_from_a_null_a_missing_field_and_an_empty_object() {
        for params in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"ignored": true}),
        ] {
            assert_eq!(
                Command::from_request(&request("ping", params.clone())).expect("ping parses"),
                Command::Ping,
                "ping should not care about {params}"
            );
        }
    }

    #[test]
    fn an_unknown_command_is_refused_by_name_rather_than_as_a_broken_frame() {
        // The distinction matters to the UI: a name it sent that this recorder
        // does not have is a version-skew problem it can report; a broken frame
        // is a bug. They must not look the same.
        let error = Command::from_request(&request("delete_everything", serde_json::json!({})))
            .expect_err("no such command");

        assert_eq!(error.code, ErrorCode::UnknownCommand);
        assert!(
            error.message.contains("delete_everything"),
            "the refusal should name the command: {}",
            error.message
        );
    }

    #[test]
    fn parameters_of_the_wrong_shape_are_refused_with_what_was_wrong_with_them() {
        let error = Command::from_request(&request(
            "start_recording",
            serde_json::json!({"pid": "not a number"}),
        ))
        .expect_err("a pid is a number");

        assert_eq!(error.code, ErrorCode::InvalidParameters);
        assert!(
            error.message.contains("start_recording") && error.message.contains("expected u32"),
            "the refusal should name the command and say what was wrong: {}",
            error.message
        );
    }

    #[test]
    fn every_command_this_protocol_defines_is_one_this_build_performs() {
        // There is no longer such a thing as a command that parses and is then
        // refused as unbuilt: `apply_settings` was the last, and the settings
        // reached the protocol with issue #51. The guard that replaces the list
        // is this one — every name the protocol answers to has to become a
        // command, and a name that quietly stopped being parsed would be
        // `unknown_command` here.
        for name in [
            "ping",
            "get_status",
            "start_recording",
            "stop_recording",
            "add_bookmark",
            "take_screenshot",
            "save_replay",
            "library_sessions",
            "library_games",
            "library_events",
            "library_trash",
            "restore_from_trash",
            "empty_trash",
            "set_favourite",
            "set_lock",
            "plugins",
            "export_recording",
            "open_playback",
            "open_preview",
            "get_hotkeys",
            "get_settings",
            "apply_settings",
            "get_audio_devices",
            "get_microphone_level",
            "get_start_at_login",
            "set_start_at_login",
            "shutdown",
        ] {
            // Parsed with no parameters, so a command with required ones is
            // refused for *that* rather than for its name. The claim here is
            // only that the name is one this build answers to, which is what
            // `unknown_command` would deny; whether each command's parameters
            // are right is every other test in this module.
            match Command::from_request(&request(name, serde_json::Value::Null)) {
                Ok(command) => assert_eq!(command.name(), name),
                Err(error) => assert_ne!(
                    error.code,
                    ErrorCode::UnknownCommand,
                    "`{name}` is in the protocol and this build does not answer to it: {}",
                    error.message
                ),
            }
        }
    }

    #[test]
    fn the_settings_commands_carry_what_they_change_and_nothing_else() {
        // `apply_settings` used to take an open object, because nothing knew
        // what it would need. It knows now: keys spelled as the settings file
        // spells them, and a `null` for the one thing a value cannot express,
        // which is Reset.
        let command = Command::from_request(&request(
            "apply_settings",
            serde_json::json!({"values": {"microphone": "name:Shure MV7", "framerate": null}}),
        ))
        .expect("a settings change parses");

        let Command::ApplySettings(applied) = command else {
            panic!("apply_settings parsed as something else");
        };
        assert_eq!(
            applied.values.get("microphone"),
            Some(&Some("name:Shure MV7".to_owned()))
        );
        assert_eq!(applied.values.get("framerate"), Some(&None));

        // And the two reads take nothing at all, so a window can ask without
        // knowing anything about the settings first.
        assert_eq!(
            Command::from_request(&request("get_settings", serde_json::Value::Null))
                .expect("it parses"),
            Command::GetSettings,
        );
        assert_eq!(
            Command::from_request(&request("get_audio_devices", serde_json::Value::Null))
                .expect("it parses"),
            Command::GetAudioDevices,
        );
    }

    #[test]
    fn starting_at_login_is_read_with_nothing_and_changed_with_one_switch() {
        // It is not a setting and it is not spelled like one: no key, no value
        // string, no map. A window sends where the user put the switch, and
        // reading takes nothing at all so that a screen can ask before it knows
        // anything (issue #308).
        assert_eq!(
            Command::from_request(&request("get_start_at_login", serde_json::Value::Null))
                .expect("it parses"),
            Command::GetStartAtLogin,
        );

        let command = Command::from_request(&request(
            "set_start_at_login",
            serde_json::json!({"enabled": true}),
        ))
        .expect("turning it on parses");
        assert_eq!(
            command,
            Command::SetStartAtLogin(SetStartAtLogin { enabled: true }),
        );

        // And it survives the round trip a client actually makes, which is
        // where a variant that serialised its parameters as `null` would be
        // read back as "turn it off".
        let request = command.to_request(3).expect("it can be represented");
        assert_eq!(request.command, "set_start_at_login");
        assert_eq!(
            Command::from_request(&request).expect("and parses back"),
            command,
        );
    }

    #[test]
    fn a_command_round_trips_through_the_request_it_is_carried_in() {
        let start = Command::StartRecording(StartRecording {
            process: Some("cs2.exe".to_owned()),
            framerate: Some(144),
            ..StartRecording::default()
        });

        let request = start.to_request(7).expect("it can be represented");
        assert_eq!(request.id, 7);
        assert_eq!(request.command, "start_recording");

        let json = serde_json::to_string(&request).expect("it serialises");
        assert!(
            !json.contains("\"window\""),
            "an absent option should not reach the wire as a null: {json}"
        );

        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(Command::from_request(&back).expect("it parses"), start);
    }

    #[test]
    fn a_start_that_says_nothing_about_a_replay_asks_for_no_buffer() {
        // The compatibility half of issue #427. `replay` was added so that a
        // caller with no way to read a setting can still ask for the buffer the
        // user configured; a client written before it sends neither field, and
        // must go on getting the ordinary recording it always got rather than
        // acquiring a spill directory and a buffer nobody asked for. A default
        // of `true`, or a `#[serde(default)]` lost from the struct, would give
        // every third-party client one silently.
        let request = request(
            "start_recording",
            serde_json::json!({ "pid": 4242, "framerate": 60 }),
        );
        let Ok(Command::StartRecording(start)) = Command::from_request(&request) else {
            panic!("a start with no replay field is still a start");
        };

        assert!(
            !start.replay,
            "a request that says nothing about a replay is asking for no buffer"
        );
        assert_eq!(start.replay_seconds, None);
        assert_eq!(
            start,
            StartRecording {
                pid: Some(4_242),
                framerate: Some(60),
                ..StartRecording::default()
            },
            "and the default has to be the same silence"
        );
    }

    #[test]
    fn a_bookmark_is_a_command_this_build_performs_rather_than_one_it_refuses() {
        // Issue #64 built the bookmark store, so `add_bookmark` stopped being
        // a command that was parsed only to be refused. Leaving it refused
        // would have meant the recorder answering every bookmark with
        // "bookmarks arrive in M8" while the subsystem sat there working, and
        // the refusal reads plausibly enough that nobody would question it.
        assert!(matches!(
            Command::from_request(&request("add_bookmark", serde_json::Value::Null)),
            Ok(Command::AddBookmark(_))
        ));
    }

    #[test]
    fn a_bare_bookmark_asks_for_nothing_and_gets_the_recorders_own_defaults() {
        // What the hotkey and the tray item send. Every field absent has to
        // mean "you decide", including the lead: a `lead_seconds` that
        // defaulted to zero here would silently undo the reaction-time
        // allowance for every caller that did not name one.
        for params in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"invented_later": true}),
        ] {
            assert_eq!(
                Command::from_request(&request("add_bookmark", params.clone())).expect("it parses"),
                Command::AddBookmark(AddBookmark::default()),
                "a bookmark carrying {params} must ask for nothing in particular"
            );
        }

        let bare = AddBookmark::default();
        assert_eq!(bare.lead_seconds, None);
        assert_eq!(bare.label, None);
        assert_eq!(bare.colour, None);
        assert_eq!(bare.duration_seconds, None);
        assert_eq!(bare.recording_id, None);
    }

    #[test]
    fn every_bookmark_parameter_survives_the_request_it_is_carried_in() {
        // Distinct values on purpose: a round trip whose fields could be
        // confused with one another passes while the code swaps two of them.
        let bookmark = Command::AddBookmark(AddBookmark {
            recording_id: Some("r-7".to_owned()),
            label: Some("triple kill".to_owned()),
            colour: Some("#ffcc00".to_owned()),
            duration_seconds: Some(12.5),
            lead_seconds: Some(3.25),
        });

        let request = bookmark.to_request(11).expect("it can be represented");
        assert_eq!(request.command, "add_bookmark");

        let json = serde_json::to_string(&request).expect("it serialises");
        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        let Command::AddBookmark(parsed) = Command::from_request(&back).expect("it parses") else {
            panic!("an add_bookmark request parsed as something else");
        };

        assert_eq!(parsed.recording_id.as_deref(), Some("r-7"));
        assert_eq!(parsed.label.as_deref(), Some("triple kill"));
        assert_eq!(parsed.colour.as_deref(), Some("#ffcc00"));
        assert_eq!(parsed.duration_seconds, Some(12.5));
        assert_eq!(parsed.lead_seconds, Some(3.25));
    }

    #[test]
    fn a_bookmark_reply_says_where_it_landed_and_not_only_that_it_was_taken() {
        // The lead means the mark is not where the key was pressed. A reply
        // that dropped either figure would leave an interface unable to say
        // what it had just done.
        let reply = Reply::BookmarkAdded {
            bookmark: BookmarkSummary {
                recording_id: "r-1".to_owned(),
                at_seconds: 115.0,
                pressed_at_seconds: 120.0,
                lead_seconds: 5.0,
                label: None,
                colour: None,
                duration_seconds: None,
                bookmarks_file: r"D:\clips\session.bookmarks.json".to_owned(),
                bookmarks_in_recording: 3,
            },
        };

        let json = serde_json::to_string(&reply).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<Reply>(&json).expect("and deserialises"),
            reply
        );
        assert!(
            json.contains("\"at_seconds\":115.0") && json.contains("\"pressed_at_seconds\":120.0"),
            "both the mark and the press have to reach the wire: {json}"
        );
        assert!(
            !json.contains("\"label\""),
            "an unlabelled bookmark should not reach the wire as a null: {json}"
        );
    }

    #[test]
    fn a_shutdown_that_says_nothing_is_the_one_that_will_not_end_a_recording() {
        // The default is the whole of the safety property: a caller that has
        // not thought about the recording cannot end one, because saying
        // nothing means "no". A `#[serde(default)]` that flipped to `true`
        // would turn every bare `{"command":"shutdown"}` on this machine into a
        // way to stop somebody's recording.
        for params in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"invented_later": true}),
        ] {
            assert_eq!(
                Command::from_request(&request("shutdown", params.clone())).expect("it parses"),
                Command::Shutdown(Shutdown {
                    finalise_recording: false
                }),
                "a shutdown carrying {params} must not be allowed to finalise a recording"
            );
        }

        assert_eq!(
            Command::from_request(&request(
                "shutdown",
                serde_json::json!({"finalise_recording": true})
            ))
            .expect("it parses"),
            Command::Shutdown(Shutdown {
                finalise_recording: true
            }),
        );
    }

    #[test]
    fn a_shutdown_round_trips_through_the_request_it_is_carried_in() {
        let shutdown = Command::Shutdown(Shutdown {
            finalise_recording: true,
        });
        let request = shutdown.to_request(3).expect("it can be represented");
        assert_eq!(request.command, "shutdown");

        let json = serde_json::to_string(&request).expect("it serialises");
        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(
            Command::from_request(&back).expect("it parses"),
            shutdown,
            "the parameter has to survive the wire, or every shutdown becomes the refusing one"
        );
    }

    #[test]
    fn a_library_listing_that_asks_for_nothing_gets_the_recorders_own_page() {
        // What a window sends when it opens. Every field absent has to mean
        // "you decide", including the limit: a page size defaulted here would
        // be a second place deciding what one frame can carry.
        for params in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"invented_later": true}),
        ] {
            assert_eq!(
                Command::from_request(&request("library_sessions", params.clone()))
                    .expect("it parses"),
                Command::LibrarySessions(LibrarySessions::default()),
                "a listing carrying {params} must ask for nothing in particular"
            );
        }
    }

    #[test]
    fn every_library_listing_parameter_survives_the_request_it_is_carried_in() {
        // Distinct values on purpose: a round trip whose fields could be
        // confused with one another passes while the code swaps two of them —
        // and a `query` that arrived as the cursor would silently search for
        // nothing.
        let listing = Command::LibrarySessions(LibrarySessions {
            limit: Some(25),
            after: Some("2026-08-11T20:14:00+01:00|cs2-20260811-201400".to_owned()),
            query: Some("game:cs2 tag:clutch".to_owned()),
        });

        let request = listing.to_request(4).expect("it can be represented");
        assert_eq!(request.command, "library_sessions");

        let json = serde_json::to_string(&request).expect("it serialises");
        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        let Command::LibrarySessions(parsed) = Command::from_request(&back).expect("it parses")
        else {
            panic!("a library_sessions request parsed as something else");
        };

        assert_eq!(parsed.limit, Some(25));
        assert_eq!(
            parsed.after.as_deref(),
            Some("2026-08-11T20:14:00+01:00|cs2-20260811-201400")
        );
        assert_eq!(parsed.query.as_deref(), Some("game:cs2 tag:clutch"));
    }

    #[test]
    fn reading_the_library_is_a_command_this_build_performs_rather_than_one_it_refuses() {
        // The mistake this guards against is the one `add_bookmark` records
        // above: a command refused as unbuilt after its subsystem landed
        // answers every request with a plausible sentence about a milestone,
        // and nobody questions it.
        for name in ["library_sessions", "library_games"] {
            assert!(
                Command::from_request(&request(name, serde_json::Value::Null)).is_ok(),
                "{name} should parse"
            );
        }
    }

    #[test]
    fn both_halves_of_an_export_survive_the_request_they_are_carried_in() {
        // Distinct values on purpose, and distinguishable ones: a round trip
        // whose two paths could be confused passes while the code swaps them —
        // and an export that swapped source for destination would read the MP4
        // that is not there and refuse, or, far worse, be handed a source that
        // already exists as a destination and refuse a copy nobody could then
        // explain.
        let export = Command::ExportRecording(ExportRecording {
            source: r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
            destination: r"E:\share\ace on mirage.mp4".to_owned(),
        });

        let request = export.to_request(11).expect("it can be represented");
        assert_eq!(request.command, "export_recording");

        let json = serde_json::to_string(&request).expect("it serialises");
        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        let Command::ExportRecording(parsed) = Command::from_request(&back).expect("it parses")
        else {
            panic!("an export_recording request parsed as something else");
        };

        assert_eq!(parsed.source, r"D:\clips\cs2-20260811-201400-1.mkv");
        assert_eq!(parsed.destination, r"E:\share\ace on mirage.mp4");
    }

    #[test]
    fn a_playback_request_carries_the_track_it_asked_for_and_not_only_the_recording() {
        // The field that is easy to drop and impossible to notice: a window
        // that asked for the microphone and was answered with the compatibility
        // mix would play *something*, with sound, and look like it worked. So
        // the track is asserted on the far side of a real serialise-and-parse
        // rather than on the value that was constructed.
        let open = Command::OpenPlayback(crate::playback::OpenPlayback {
            source: r"D:\clips\cs2-20260811-201400-1.mkv".to_owned(),
            audio_track: Some(3),
        });

        let request = open.to_request(13).expect("it can be represented");
        assert_eq!(request.command, "open_playback");

        let json = serde_json::to_string(&request).expect("it serialises");
        let back: Request = serde_json::from_str(&json).expect("and deserialises");
        let Command::OpenPlayback(parsed) = Command::from_request(&back).expect("it parses") else {
            panic!("an open_playback request parsed as something else");
        };

        assert_eq!(parsed.source, r"D:\clips\cs2-20260811-201400-1.mkv");
        assert_eq!(parsed.audio_track, Some(3));
    }

    #[test]
    fn a_playback_request_that_names_no_track_asks_for_the_default_rather_than_track_zero() {
        // Absent means "whichever track a player should choose on its own",
        // which is the recorder's answer to make. A `serde` default of `0`
        // would silently make it stream zero — the picture, in a Clipped
        // recording — and the difference between the two is invisible in a
        // request that omits the field.
        let parsed = Command::from_request(&request(
            "open_playback",
            serde_json::json!({"source": r"D:\clips\match.mkv"}),
        ))
        .expect("a playback request may leave the track out");

        let Command::OpenPlayback(open) = parsed else {
            panic!("an open_playback request parsed as something else");
        };
        assert_eq!(open.audio_track, None);
    }

    #[test]
    fn a_playback_request_that_names_no_recording_is_refused_naming_the_field() {
        let error = Command::from_request(&request("open_playback", serde_json::Value::Null))
            .expect_err("a playback request has to say what to play");

        assert_eq!(error.code, ErrorCode::InvalidParameters);
        assert!(
            error.message.contains("open_playback") && error.message.contains("source"),
            "the refusal should name the command and the field: {}",
            error.message
        );
    }

    #[test]
    fn an_export_that_names_neither_file_is_refused_naming_the_one_it_was_not_given() {
        // Unlike every other command's parameters, these two have no `serde`
        // default: there is no sensible recording to export and nowhere
        // sensible to put it, so "you did not say" has to be a refusal rather
        // than a value. A default here would send the recorder an empty path,
        // and the failure would surface four calls further on as a file that
        // could not be read.
        let error = Command::from_request(&request(
            "export_recording",
            serde_json::json!({"source": r"D:\clips\match.mkv"}),
        ))
        .expect_err("an export has to say where the MP4 goes");

        assert_eq!(error.code, ErrorCode::InvalidParameters);
        assert!(
            error.message.contains("export_recording") && error.message.contains("destination"),
            "the refusal should name the command and the field: {}",
            error.message
        );
    }

    #[test]
    fn exporting_is_a_command_this_build_performs_rather_than_one_it_refuses() {
        // The mistake `add_bookmark` and the library commands above record: a
        // command refused as unbuilt after its subsystem landed answers every
        // request with a plausible sentence about a milestone, and nobody
        // questions it.
        assert!(
            matches!(
                Command::from_request(&request(
                    "export_recording",
                    serde_json::json!({"source": "a.mkv", "destination": "a.mp4"}),
                )),
                Ok(Command::ExportRecording(_)),
            ),
            "export_recording should parse into the command that performs it",
        );
    }

    #[test]
    fn an_export_reply_says_what_the_copy_holds_and_not_only_that_one_was_made() {
        // The figures are what let a window say "4.2 s, 9.8 GB copied, nothing
        // lost" instead of "done", and `losses` is the half that has to survive
        // an empty vector without becoming a null the window has to guard.
        let reply = Reply::RecordingExported {
            export: crate::status::ExportSummary {
                source: r"D:\clips\match.mkv".to_owned(),
                destination: r"D:\clips\match.mp4".to_owned(),
                duration_ms: 6_540_000,
                packets: 588_120,
                bytes: 9_811_204_112,
                elapsed_ms: 4_182,
                lossless: true,
                losses: Vec::new(),
            },
        };

        let json = serde_json::to_string(&reply).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<Reply>(&json).expect("and deserialises"),
            reply
        );
        assert!(
            !json.contains("\"losses\""),
            "an export that lost nothing should not carry an empty list: {json}"
        );
        assert!(
            json.contains("\"lossless\":true") && json.contains("\"elapsed_ms\":4182"),
            "both the verdict and the measurement have to reach the wire: {json}"
        );
    }

    #[test]
    fn a_reply_round_trips() {
        let reply = Reply::Status {
            status: RecorderStatus::Idle,
        };
        let json = serde_json::to_string(&reply).expect("it serialises");
        assert_eq!(json, r#"{"reply":"status","status":{"state":"idle"}}"#);
        assert_eq!(
            serde_json::from_str::<Reply>(&json).expect("and deserialises"),
            reply
        );
    }
}
