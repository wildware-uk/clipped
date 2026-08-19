/**
 * The recorder control protocol, in TypeScript.
 *
 * `docs/ipc.md` is the specification and `crates/ipc` is the implementation;
 * this file is the desktop application's view of the same messages. It is
 * written by hand rather than generated, and it is checked against the Rust
 * types on every run of the test suite — `conformance.test.ts` compares it with
 * `protocol-schema.json`, which `crates/ipc/src/schema.rs` derives from the
 * types themselves. Issue #209 and `docs/ipc.md` record why that way round.
 *
 * # The names are the wire names
 *
 * Fields are `snake_case` here, which no TypeScript style guide would ask for.
 * The alternative is a camelCase mirror plus a renaming layer, and then three
 * things to keep in step instead of two: a protocol trace in a bug report would
 * no longer read like the code that handles it, and the conformance check would
 * be comparing names through a translation table nobody could see through.
 *
 * # Values a build does not recognise
 *
 * `docs/ipc.md` promises that an error code, an end reason, an error detail or
 * an event invented after this build was compiled leaves the frame carrying it
 * readable. Two shapes express that here:
 *
 * - {@link Extensible} for the open sets of wire strings. The known values are
 *   listed and offered by autocompletion; anything else is still a valid value
 *   of the type, because it is still a valid thing to arrive.
 * - A variant holding the raw JSON — {@link UnrecognisedEvent},
 *   {@link UnrecognisedErrorDetail} — for the two tagged unions that can grow.
 *   Their `event` and `detail` tags are typed as absent, so narrowing on a
 *   known tag never lands in the catch-all and a `switch` over the union is
 *   still exhaustive.
 *
 * {@link RecorderStatus} deliberately has neither, because a window showing
 * "idle" for a recorder doing something else is a lie, and a message the
 * interface cannot read is not (`docs/ipc.md`, "Compatibility policy"). A state
 * this build does not know therefore fails whatever carried it: a reply, and
 * with it the frame; or a `status_changed` event, which is then kept as an
 * {@link UnrecognisedEvent} like any other event this build cannot read. Never
 * a state rendered as something it is not.
 */

/** Any value that can arrive as JSON. */
export type JsonValue = string | number | boolean | null | readonly JsonValue[] | JsonObject;

/** A JSON object, as it arrives. */
export interface JsonObject {
  readonly [key: string]: JsonValue | undefined;
}

/**
 * A set of wire strings this build knows, plus the ones it does not.
 *
 * `string & Record<never, never>` is the idiom that keeps the known values in
 * autocompletion while still admitting a value invented later: a bare `string`
 * in the union would swallow the literals and offer no suggestions at all.
 */
export type Extensible<Known extends string> = Known | (string & Record<never, never>);

/**
 * The protocol version this build of the interface speaks.
 *
 * Version 2 added the `watching` recorder state (issue #241). A state is the one
 * addition that cannot be additive, because {@link RecorderStatus} has no
 * catch-all: a build compiled against version 1 would fail to read
 * `{"state":"watching"}` rather than guess at it, which is the wanted behaviour
 * and is why the version moved instead.
 */
export const PROTOCOL_VERSION = 2;

/**
 * Every protocol version this build can hold a conversation in.
 *
 * A recorder may speak more than one at once; the interface speaks exactly one
 * and is told, in the refusal, what the recorder speaks instead.
 */
export const SUPPORTED_PROTOCOL_VERSIONS = [2] as const;

/** How many connections one recorder serves before refusing more. */
export const MAX_CONCURRENT_CONNECTIONS = 8;

/** What a connection is for. */
export const CONNECTION_ROLES = ['control', 'events'] as const;

/** A role this build knows. */
export type KnownConnectionRole = (typeof CONNECTION_ROLES)[number];

/**
 * What a connection is for, as it appears on the wire.
 *
 * Extensible because the recorder reads any string here — one it does not have
 * is refused with a sentence rather than as a broken frame, which is what lets
 * it say "a connection is either `control` or `events`".
 */
export type ConnectionRole = Extensible<KnownConnectionRole>;

/** The role a handshake that does not name one is asking for. */
export const DEFAULT_CONNECTION_ROLE = 'control';

/** The streams of events the protocol defines. */
export const EVENT_STREAMS = ['status', 'errors', 'metrics', 'exports'] as const;

/** A stream this build knows. `metrics` is defined and refused; see below. */
export type KnownEventStream = (typeof EVENT_STREAMS)[number];

/**
 * A stream of events a connection can ask for.
 *
 * A name the recorder does not have is refused at subscription time rather than
 * accepted and left silent, so a client that asked for one is told. `metrics`
 * is refused by this recorder with `not_implemented`: nothing measures those
 * figures during a recording yet (issue #100).
 *
 * **The refusal takes the whole events connection with it.** So a stream added
 * later is asked for only when the recorder's welcome advertises it: `exports`
 * is paired with the `export_progress` feature for exactly that reason, and a
 * client that asked an older recorder for it would lose its `status`
 * subscription too (issue #446).
 */
export type EventStream = Extensible<KnownEventStream>;

/** The capabilities a recorder can advertise in its welcome. */
export const FEATURES = [
  'recording',
  'status_events',
  'bookmarks',
  'screenshots',
  'shutdown',
  'library',
  'export',
  'playback',
  'hotkeys',
  'replay',
  'export_progress',
  'settings',
  'microphone_level',
  'diagnostics',
  'startup',
  'automatic',
  'previews',
  'storage',
  'editing',
] as const;

/** A capability this build knows how to make use of. */
export type KnownFeature = (typeof FEATURES)[number];

/**
 * Something a recorder can do.
 *
 * A version says what a build can express; a feature says what it can do, and
 * the two are not the same. The interface checks here before offering a
 * control, so that it never presents a button whose command will be refused
 * (AGENTS.md section 27).
 */
export type Feature = Extensible<KnownFeature>;

/** Every command the protocol defines. There is no longer one no build performs. */
export const COMMANDS = [
  'ping',
  'get_status',
  'start_recording',
  'stop_recording',
  'add_bookmark',
  'take_screenshot',
  'save_replay',
  'library_sessions',
  'library_games',
  'library_events',
  'library_clip_document',
  'save_clip_document',
  'library_trash',
  'restore_from_trash',
  'empty_trash',
  'set_favourite',
  'set_lock',
  'plugins',
  'export_recording',
  'open_playback',
  'open_preview',
  'get_hotkeys',
  'get_diagnostics',
  'get_storage',
  'get_settings',
  'apply_settings',
  'get_audio_devices',
  'get_microphone_level',
  'get_start_at_login',
  'set_start_at_login',
  'shutdown',
] as const;

/** A command the protocol defines. */
export type KnownCommandName = (typeof COMMANDS)[number];

/**
 * The name of a command, as it appears on a request.
 *
 * Extensible because a command name is carried as a string and refused by name
 * after the frame has been read: version skew has to be reportable as version
 * skew rather than as a malformed frame.
 */
export type CommandName = Extensible<KnownCommandName>;

/** The refusals this build knows by name. */
export const ERROR_CODES = [
  'unsupported_protocol_version',
  'handshake_required',
  'malformed_frame',
  'unknown_command',
  'invalid_parameters',
  'not_implemented',
  'already_recording',
  'not_recording',
  'target_not_found',
  'target_not_capturable',
  'recording_failed',
  'too_many_connections',
  'shutting_down',
  'destination_exists',
  'export_failed',
  'playback_failed',
  'library_unavailable',
  'edit_unreadable',
  'internal',
] as const;

/** A refusal this build can branch on. */
export type KnownErrorCode = (typeof ERROR_CODES)[number];

/**
 * What kind of refusal this is.
 *
 * Extensible, and that is the point: a code invented after this build was
 * compiled is kept verbatim and its message shown, rather than costing the
 * interface the whole refusal.
 */
export type ErrorCode = Extensible<KnownErrorCode>;

/** Why a recording ended. */
export const END_REASONS = ['stopped', 'target_lost', 'target_resized'] as const;

/** A reason this build knows. */
export type KnownEndReason = (typeof END_REASONS)[number];

/** Why a recording ended, keeping a reason invented later verbatim. */
export type EndReason = Extensible<KnownEndReason>;

/** The states a recorder reports. Closed, deliberately: see the file header. */
export const RECORDER_STATES = ['idle', 'watching', 'recording'] as const;

/** What the recorder is doing. There is no unrecognised state. */
export type RecorderState = (typeof RECORDER_STATES)[number];

/** The tags a response's outcome is carried under. */
export const OUTCOMES = ['ok', 'error'] as const;

/** The replies this build knows. */
export const REPLIES = [
  'pong',
  'status',
  'recording_started',
  'recording_stopped',
  'bookmark_added',
  'screenshot_taken',
  'replay_saved',
  'library_sessions',
  'library_games',
  'library_events',
  'library_clip_document',
  'clip_document_saved',
  'library_trash',
  'restored',
  'trash_emptied',
  'favourited',
  'locked',
  'plugins',
  'recording_exported',
  'playback_opened',
  'preview_opened',
  'hotkeys',
  'settings',
  'audio_devices',
  'microphone_level',
  'start_at_login',
  'shutting_down',
  'diagnostics',
  'storage',
] as const;

/** The name of a reply. Closed: a reply nobody can read is a failed command. */
export type ReplyName = (typeof REPLIES)[number];

/** The events this build knows. */
export const EVENTS = [
  'status_changed',
  'session_ended',
  'recording_failed',
  'export_progress',
] as const;

/** The name of an event this build knows. */
export type KnownEventName = (typeof EVENTS)[number];

/** The machine-readable details this build knows. */
export const ERROR_DETAILS = ['unsupported_protocol_version', 'not_implemented'] as const;

/** The tag of a detail this build knows. */
export type KnownErrorDetailName = (typeof ERROR_DETAILS)[number];

/** The `type` of everything the desktop application sends. */
export const CLIENT_MESSAGE_TYPES = ['hello', 'request'] as const;

/** The `type` of everything the recorder sends. */
export const SERVER_MESSAGE_TYPES = ['welcome', 'refused', 'response', 'event'] as const;

/** Who is at the other end of a connection. */
export interface PeerIdentity {
  /** The program's name, such as `clipped-recorder`. */
  readonly name: string;
  /**
   * Its build version.
   *
   * For diagnosis and for telling the user which side to update. Nothing
   * branches on it: what a peer can do is {@link Welcome.features}.
   */
  readonly version: string;
}

/**
 * The first frame on every connection.
 *
 * **Frozen.** This shape and the refusal it can produce are how two builds that
 * agree on nothing else still manage to say so.
 */
export interface Hello {
  /** The version this client speaks. One version, not a range. */
  readonly protocol_version: number;
  /** Who is connecting. */
  readonly client: PeerIdentity;
  /** What the connection is for. Absent means {@link DEFAULT_CONNECTION_ROLE}. */
  readonly role?: ConnectionRole;
  /** Which event streams to deliver. Only read for an `events` connection. */
  readonly streams?: readonly EventStream[];
}

/** The recorder accepting a connection. */
export interface Welcome {
  /** The version now in force, which is the one the client asked for. */
  readonly protocol_version: number;
  /** Which recorder answered. */
  readonly recorder: PeerIdentity;
  /** What the connection was accepted as. */
  readonly role: ConnectionRole;
  /** What this recorder can actually do. */
  readonly features: readonly Feature[];
  /** The streams this connection will receive. */
  readonly streams?: readonly EventStream[];
}

/**
 * What to record, and how. The `clipped-recorder record` options by name.
 *
 * A type alias rather than an interface so that it is assignable to
 * {@link JsonValue}, which is what {@link RecorderRequest.params} takes: an
 * interface without an index signature is not, and the request that carries
 * these would not type-check.
 */
export type StartRecordingParams = {
  /** Record the window whose title contains this text. */
  readonly window?: string;
  /** Record the window belonging to this executable, such as `cs2.exe`. */
  readonly process?: string;
  /** Record the window belonging to this process identifier. */
  readonly pid?: number;
  /** Where to write the recording. Absent means the recorder's own default. */
  readonly output?: string;
  /** Whether an existing file at `output` may be replaced. */
  readonly overwrite?: boolean;
  /** `source`, or `WIDTHxHEIGHT`. */
  readonly resolution?: string;
  /** Frames per second to encode at. */
  readonly framerate?: number;
  /** `auto`, `h264`, `hevc` or `av1`. */
  readonly codec?: string;
  /** `auto`, `nvenc`, `amf`, `quicksync` or `software`. */
  readonly encoder?: string;
  /** `default`, `none`, or part of a device name. */
  readonly microphone?: string;
  /** `default`, `none`, or part of a device name. */
  readonly system_audio?: string;
  /**
   * Keep the last this many seconds, so that `save_replay` has something to
   * save.
   *
   * Absent means no buffer unless {@link StartRecordingParams.replay} asks for
   * one, which is what an ordinary recording is. It belongs to the recording
   * rather than to the save, because a buffer has to have been filling since
   * before the thing somebody wants to keep happened.
   */
  readonly replay_seconds?: number;
  /**
   * Keep a replay buffer, at the length the recorder has configured.
   *
   * `replay_seconds` names a length; this asks for one without naming it, and
   * the recorder answers with `replay_window_seconds` resolved for the game it
   * turns out to be recording. A caller that resolved a length itself would be
   * a second place that setting is decided, and one that made a length up would
   * be recording to a duration nobody chose.
   *
   * Absent is `false`, and `false` with no `replay_seconds` is no buffer.
   */
  readonly replay?: boolean;
};

/**
 * How far a shutdown may go. A type alias for the reason above.
 *
 * `finalise_recording` left out means `false`, and `false` is refused with
 * `already_recording` while something is being recorded. That default is the
 * safety property, not a convenience: anything running as this user can reach
 * the recorder's pipe, and a bare `shutdown` must not be able to end somebody's
 * recording. An interface sends `true` only after the user has been told that is
 * what will happen.
 */
export type ShutdownParams = {
  /** Whether a recording in progress may be stopped and its file finished first. */
  readonly finalise_recording?: boolean;
};

/**
 * What to mark, and how far before this request to mark it.
 *
 * Every field is optional, because the request a hotkey or a tray item sends
 * carries none of them: pressing the key is the whole interaction and must not
 * stop to ask for a label while somebody is playing.
 */
export type AddBookmarkParams = {
  /**
   * Which recording to mark. Absent means "whatever is being recorded", which
   * is what a hotkey wants; naming one keeps a mark meant for a recording that
   * has since ended out of its successor.
   */
  readonly recording_id?: string;
  /** What to call the bookmark. */
  readonly label?: string;
  /** A colour, in whatever notation this interface writes. */
  readonly colour?: string;
  /** How long the marked moment lasts. Absent means it is a moment. */
  readonly duration_seconds?: number;
  /**
   * How far *before* this request the bookmark should be stamped.
   *
   * Absent means the recorder's own default, which is not zero: a person
   * presses the key after the thing they wanted to mark, so a bookmark stamped
   * at the press is reliably late (docs/bookmarks.md). Something with no human
   * reaction to allow for sends `0`.
   */
  readonly lead_seconds?: number;
};

/** Which recording to stop. A type alias for the reason above. */
export type StopRecordingParams = {
  /**
   * The recording to stop. Absent means "whatever is running", which is what a
   * tray menu wants; naming one is what a window with a recording on screen
   * does, so that a recording which ended by itself cannot have its successor
   * stopped instead.
   */
  readonly recording_id?: string;
};

/**
 * What to photograph, and how to save it.
 *
 * Every field is optional, because the request a hotkey or a tray item sends
 * carries none of them: photograph whatever is being recorded, in the
 * configured format, and put it where screenshots go.
 */
export type TakeScreenshotParams = {
  /**
   * Which recording to take the picture from. Absent means "whatever is being
   * recorded", exactly as it does for a bookmark.
   */
  readonly recording_id?: string;
  /**
   * Photograph the window whose title contains this text.
   *
   * This and the two below are only consulted when nothing is being recorded.
   * A screenshot taken during a recording comes from a frame that recording
   * already captured, which is far cheaper and is the only way to be sure the
   * picture is of what is being recorded.
   */
  readonly window?: string;
  /** Photograph the window belonging to this executable, such as `cs2.exe`. */
  readonly process?: string;
  /** Photograph the window belonging to this process identifier. */
  readonly pid?: number;
  /** `png`, `jpeg` or `webp`. Absent means the recorder's own default. */
  readonly format?: string;
};

/**
 * How much of the replay buffer to keep, and where to put it. A type alias for
 * the reason {@link StartRecordingParams} is one.
 *
 * Every field is optional, because the shape a hotkey sends is no fields at
 * all: keep the recorder's configured duration out of whatever is being
 * recorded, and put it where that recording's clips go.
 */
export type SaveReplayParams = {
  /**
   * Which recording to save out of. Absent means "whatever is being recorded",
   * exactly as it does for a bookmark and a screenshot.
   */
  readonly recording_id?: string;
  /**
   * How many seconds to keep. Absent means the duration the recording's buffer
   * was started with.
   *
   * More than the buffer's window is not refused: the clip is what there was,
   * and {@link ReplaySummary.complete} says it was short.
   */
  readonly duration_seconds?: number;
  /**
   * Where to write the clip. Absent means beside the recording, named after the
   * session it belongs to.
   */
  readonly output?: string;
};

/**
 * Which page of the recording library to read. A type alias for the reason
 * {@link StartRecordingParams} is one.
 *
 * Every field is optional: a window opening on the newest sessions sends none of
 * them.
 */
export type LibrarySessionsParams = {
  /**
   * How many sessions to answer with.
   *
   * The recorder clamps this to what one frame can carry, so asking for the
   * whole library returns a page and a cursor rather than a refusal. Absent
   * means the recorder's own page size.
   */
  readonly limit?: number;
  /**
   * Continue after the session this cursor names.
   *
   * It is {@link LibrarySessionPage.next_cursor} from the previous reply and is
   * opaque: the only thing to do with one is send it back. A cursor the
   * recorder cannot read starts at the newest session rather than refusing,
   * because a window may have kept one across a restart.
   */
  readonly after?: string;
  /**
   * Only the sessions this search query selects, in the language of
   * `docs/search.md`.
   *
   * A query that does not parse is refused with `invalid_parameters` carrying
   * the position and what was expected there, so a search box can say what is
   * wrong with what was typed rather than showing nothing and no reason.
   */
  readonly query?: string;
};

/**
 * Which recording to copy into MP4, and where to put the copy. A type alias for
 * the reason {@link StartRecordingParams} is one.
 *
 * Neither field has a default. The source is a file the caller already knows
 * about, because it read it out of the library; the destination is chosen by
 * whoever asked, and one that already exists is **refused** rather than
 * replaced (`destination_exists`).
 */
export type ExportRecordingParams = {
  /** The recording to copy, as {@link LibraryRecording.path} reported it. */
  readonly source: string;
  /** Where to write the MP4. */
  readonly destination: string;
};

/**
 * What to change in the settings, and to what. A type alias for the reason
 * {@link StartRecordingParams} is one.
 *
 * A map rather than one key and one value, because a settings screen saves what
 * somebody edited: two changes made together are applied together, and a value
 * the recorder refuses refuses the whole request rather than leaving half of it
 * written.
 *
 * `null` clears a setting, which is Reset: it returns the setting to the value
 * Clipped ships with *and* keeps following it, which writing today's default in
 * as a value would not.
 */
export type ApplySettingsParams = {
  /**
   * The game to change them for, or absent for the global settings.
   *
   * Naming one writes the change into that game's own section, which is what
   * makes the value an override; every game without one goes on inheriting
   * (AGENTS.md section 30). Only the settings a game may override are accepted
   * with a game — the recording directory, the storage limits, the hotkeys and
   * the notification switches are global by construction, and the recorder
   * refuses them by name rather than writing them globally.
   */
  readonly game?: string;
  /** The settings to change, by the key each has in the settings file. */
  readonly values?: Readonly<Record<string, string | null>>;
};

/**
 * Which settings to read: the global page, or one game's. A type alias for the
 * reason {@link StartRecordingParams} is one.
 *
 * A game with no section of its own is not an error: every setting comes back
 * inherited, which is what a page for a game nobody has configured should show.
 */
export type GetSettingsParams = {
  /**
   * The game to resolve for, named as the settings file names it —
   * `counter-strike-2`.
   *
   * Absent is the global settings, which is what every game inherits from.
   */
  readonly game?: string;
};

/**
 * Which microphone to listen to.
 *
 * A setting's value rather than a device name, spelled as the settings file
 * spells it — `default`, `name:Shure MV7` — because the question is what the
 * choice somebody is looking at would record. Resolved by the same code a
 * recording resolves it with, so the meter and the recording cannot end up
 * pointed at different endpoints.
 */
export type MicrophoneLevelParams = {
  /** The microphone setting to listen to, in the settings file's own spelling. */
  readonly microphone: string;
};

/**
 * A recording copied into another container, and what the copy turned out to
 * be.
 *
 * An export is a **stream copy**: the recording's own coded packets, in a
 * different box. Nothing is decoded and nothing is re-encoded, which is why
 * {@link ExportSummary.elapsed_ms} is worth showing.
 */
export interface ExportSummary {
  /** The recording that was copied, unchanged by the copy. */
  readonly source: string;
  /** The file that was written. */
  readonly destination: string;
  /** How much media the result holds. */
  readonly duration_ms: number;
  /** How many coded packets were copied, across every track. */
  readonly packets: number;
  /** How many bytes of coded media were copied, before container overhead. */
  readonly bytes: number;
  /** How long the copy took, measured. */
  readonly elapsed_ms: number;
  /**
   * Whether the destination holds everything the source did.
   *
   * `false` means something was left out and {@link ExportSummary.losses} says
   * what. It is never a picture or a sound track: a container that cannot carry
   * one of those is a refusal rather than a quiet loss.
   */
  readonly lossless: boolean;
  /**
   * Everything the destination does not contain, phrased for somebody to read.
   *
   * Absent when nothing was lost, rather than an empty array, because that is
   * what the recorder puts on the wire.
   */
  readonly losses?: readonly string[];
}

/**
 * How far a running export has got.
 *
 * The payload of {@link ExportProgressEvent}, and the answer to a copy of a
 * two-hour recording looking like a hang (issue #446). It has to be an event
 * and not a field on the reply, because the reply arrives when the MP4's index
 * has been written — the moment there is nothing left to report.
 *
 * {@link ExportProgress.destination} is what identifies the export: there is no
 * request identifier on the event path, and a destination that already exists
 * is refused, so two exports cannot be writing the same file at once.
 */
export interface ExportProgress {
  /** The recording being copied, unchanged by the copy. */
  readonly source: string;
  /** The file being written. This is what identifies the export. */
  readonly destination: string;
  /**
   * How much of the recording's own timeline has been copied so far.
   *
   * The same measurement {@link ExportSummary.duration_ms} carries, so the last
   * progress event of a copy and the reply that follows it agree.
   */
  readonly written_ms: number;
  /**
   * How long the recording says it is, where it says at all.
   *
   * **Absent, not zero**, when the container declares no duration — an
   * interrupted recording keeps every packet it wrote and no total. Draw an
   * unbounded indication from {@link ExportProgress.bytes} then rather than a
   * percentage: "nought per cent" and "no idea" are different things to show.
   */
  readonly total_ms?: number;
  /** How many coded packets have been copied, across every carried track. */
  readonly packets: number;
  /**
   * How many bytes of coded media have been copied, before container overhead.
   *
   * The one figure that still advances when {@link ExportProgress.total_ms} is
   * absent.
   */
  readonly bytes: number;
}

/**
 * How far through an export is, between 0 and 1, or `null` if the recording
 * never said how long it was.
 *
 * Clamped, because a source's declared duration and the end of its last packet
 * need not agree to the millisecond and a progress bar that reads 101 % is a
 * bug report. It mirrors `ExportProgress::fraction` in `crates/ipc` so that
 * both ends of the protocol agree about what "no total" means.
 */
export function exportFraction(progress: ExportProgress): number | null {
  const total = progress.total_ms;
  if (total === undefined || total === 0) {
    return null;
  }
  return Math.min(1, Math.max(0, progress.written_ms / total));
}

/**
 * Which recording to open for playback, and which of its tracks to hear.
 *
 * The track is a **stream index of the file**, as {@link PlaybackTrack.index}
 * carries it, rather than an ordinal among the sound tracks: the two differ by
 * however many picture tracks come first. Absent means the one a player should
 * choose on its own, which the recorder decides — for a Clipped recording, the
 * compatibility mix.
 */
export type OpenPlaybackParams = {
  /** The recording to play, as {@link LibraryRecording.path} reported it. */
  readonly source: string;
  /** Which sound track to hear. Absent means the recorder's own choice. */
  readonly audio_track?: number;
};

/**
 * A recording, ready to be played, and what could be heard instead.
 *
 * The window plays {@link PlaybackStream.path} and offers
 * {@link PlaybackStream.audio_tracks} beside it. There is deliberately **no
 * duration and no picture size** here: the media element measures both from the
 * file it is given, and a figure sent from the recorder would be a second
 * answer to the same question.
 */
export interface PlaybackStream {
  /** The file to play. */
  readonly path: string;
  /**
   * The source stream index whose sound this carries.
   *
   * Absent only for a recording with no sound at all, which a window has to be
   * able to tell from a track that would not play.
   */
  readonly audio_track?: number;
  /**
   * Every sound track of the **recording**, in the order the container declares
   * them — not of the file being played, which may hold one of them.
   *
   * Absent rather than an empty array for a recording with no sound, because
   * that is what the recorder puts on the wire.
   */
  readonly audio_tracks?: readonly PlaybackTrack[];
  /**
   * Whether {@link PlaybackStream.path} is a copy made for this choice rather
   * than the recording itself.
   *
   * A prepared copy is a cache entry: it is not in anybody's library and must
   * not be presented to a user as their recording.
   */
  readonly prepared?: boolean;
}

/** One sound track of a recording, as a window offers it. */
export interface PlaybackTrack {
  /** The stream index the container declares it at. */
  readonly index: number;
  /**
   * What the track is called, where the recording named it.
   *
   * Absent for a file that named none. A window shows the position rather than
   * inventing a name.
   */
  readonly name?: string;
  /** The track's language tag, where the recording carried one. */
  readonly language?: string;
  /**
   * Whether the container flags this as the track a player should choose on its
   * own.
   *
   * Not a promise about what a media element will play: Chromium ignores the
   * flag, which is why `open_playback` decides what is served.
   */
  readonly default?: boolean;
}

/**
 * The two derived pictures a recording has.
 *
 * Closed, like {@link RecorderState} and {@link HotkeyStateName}: the two are
 * drawn by entirely different code — a `data:` URI in an `<img>`, or peaks on a
 * canvas — so a window that met a third and guessed would draw peaks as a
 * picture, or a picture as peaks. `crates/ipc/src/preview.rs` refuses one it
 * does not know for the same reason, and the schema records both sides as
 * intolerant (issue #448).
 */
export const PREVIEW_KINDS = ['thumbnail', 'waveform'] as const;

/** Which picture a request asks for. There is no unrecognised kind. */
export type PreviewKind = (typeof PREVIEW_KINDS)[number];

/**
 * Where a preview stands.
 *
 * Closed for the reason above, and this one carries issue #448's second
 * criterion: `pending` is the ordinary state of a recording that has just been
 * written, `unavailable` means there will never be a picture, and a screen that
 * collapsed the two would put a broken tile over every new recording — or an
 * empty one over a file it can never draw. A state this build cannot name would
 * be drawn as one it can, which is the fabricated state AGENTS.md section 27
 * forbids.
 */
export const PREVIEW_STATES = ['pending', 'ready', 'unavailable'] as const;

/** How far a preview has got. There is no unrecognised state. */
export type PreviewState = (typeof PREVIEW_STATES)[number];

/**
 * Which recording to draw, and which of its two pictures.
 *
 * Asking is also what causes one to be made: the recorder answers `pending`
 * *and* queues the work, so a row being drawn is what puts that recording at
 * the front of the queue (`crates/ipc/src/preview.rs`).
 *
 * A type alias rather than an interface so that it is assignable to
 * {@link JsonValue}, which is what {@link RecorderRequest.params} takes.
 */
export type OpenPreviewParams = {
  /** The recording to draw, as {@link LibraryRecording.path} reported it. */
  readonly source: string;
  /**
   * Which picture is wanted.
   *
   * Required, with no default: the two answers are shaped differently, and
   * guessing would answer a screen asking for peaks with a picture it cannot
   * draw.
   */
  readonly kind: PreviewKind;
  /**
   * How many buckets of peaks the caller can draw, for a `waveform`.
   *
   * The width in pixels of the space the waveform goes in, in the ordinary
   * case. Asking at the width that will be drawn is not an approximation:
   * `crates/waveform` stores a pyramid of resolutions and merging buckets is
   * exact, so the answer arrives on the caller's own grid.
   *
   * Absent means an overview, the coarsest resolution the recorder keeps.
   * Clamped by the recorder rather than refused, and ignored for a thumbnail,
   * which has one stored size.
   */
  readonly buckets?: number;
};

/**
 * One recording's thumbnail or waveform, as far as it exists.
 *
 * Which of {@link Preview.picture} and {@link Preview.tracks} is filled in
 * follows from {@link Preview.kind}, and whether either is follows from
 * {@link Preview.state}. A window branches on those two rather than on which
 * fields happen to be present: the recorder leaves out what does not belong, so
 * a missing picture is never the difference between "not yet" and "never".
 */
export interface Preview {
  /**
   * Which picture this answers about.
   *
   * Echoed rather than assumed: a window draws several recordings at once and
   * an answer has to be matched to what it answers.
   */
  readonly kind: PreviewKind;
  /** Where it stands. */
  readonly state: PreviewState;
  /** The picture, for a `thumbnail` that is `ready`. */
  readonly picture?: PreviewPicture;
  /**
   * One entry per sound track, for a `waveform` that is `ready`.
   *
   * Empty for every other state, and legitimately empty for a recording with no
   * sound at all — which is what Clipped writes today, until multi-track audio
   * (issue #180). A window drawing a row per track therefore needs no branch.
   */
  readonly tracks?: readonly PreviewTrack[];
  /**
   * Why there will not be one, for a state of `unavailable`.
   *
   * A sentence naming what happened, shown as it arrives. It never carries a
   * directory: the generators format their errors through
   * `clipped_logging::RedactedPath`, so what crosses the boundary is a file
   * name rather than the folders somebody chose (AGENTS.md section 14).
   */
  readonly reason?: string;
}

/** A thumbnail, ready to be drawn. */
export interface PreviewPicture {
  /**
   * What {@link PreviewPicture.bytes} is, as a media type — `image/jpeg` for
   * what this build stores.
   *
   * Carried rather than assumed so that the window builds its `data:` URI
   * without knowing which format the generator chose; `docs/thumbnails.md`
   * argues JPEG against WebP and the argument is not settled.
   */
  readonly media_type: string;
  /**
   * The picture itself, base64 (RFC 4648, with padding).
   *
   * Straight into `src="data:{media_type};base64,{bytes}"`, which
   * `tauri.conf.json`'s `img-src` already permits — no asset scope, no new
   * origin, and the peaks travel by the same route (issue #448).
   */
  readonly bytes: string;
  /** How wide the picture is, in pixels. */
  readonly width: number;
  /** How tall it is. */
  readonly height: number;
  /**
   * How far into the recording the frame came from, in seconds.
   *
   * Not the first frame, and deliberately: `docs/thumbnails.md` explains which
   * frame is chosen and why.
   */
  readonly at_seconds: number;
  /**
   * Whether every candidate frame was a flat colour, so this one is too.
   *
   * True is honest rather than a failure — that is what the recording looks
   * like — and a screen may draw it or fall back to its no-picture tile.
   */
  readonly blank?: boolean;
}

/** One sound track of a recording, reduced to peaks. */
export interface PreviewTrack {
  /**
   * The stream index the container declares the track at.
   *
   * The same numbering {@link PlaybackTrack.index} uses, so a screen showing a
   * waveform beside a track selector can put the two together.
   */
  readonly index: number;
  /**
   * What the track is called — `Microphone`, `Game` — where the recording named
   * it.
   *
   * Absent for a file that named none. A window shows the position rather than
   * inventing a name.
   */
  readonly name?: string;
  /** The track's sample rate, in hertz. */
  readonly sample_rate: number;
  /**
   * How many channels it carries.
   *
   * The channels are merged into these peaks rather than averaged, so a sound
   * panned hard to one side is as visible as one in the middle. This is what
   * the track holds, not how many rows of peaks there are — there is always
   * one.
   */
  readonly channels: number;
  /**
   * How long the track is, in seconds.
   *
   * What {@link PreviewTrack.peaks} spans: the first bucket starts at zero and
   * the last one ends here.
   */
  readonly duration_seconds: number;
  /**
   * The peaks, two numbers per bucket: the lowest sample and then the highest,
   * each scaled to ±127.
   *
   * Interleaved rather than two arrays, so the two halves of a bucket cannot
   * arrive at different lengths, and rather than a list of objects, which costs
   * four times the bytes for the same numbers. The length is therefore always
   * even, there are `peaks.length / 2` buckets, and each covers
   * `duration_seconds / (peaks.length / 2)`.
   *
   * Minimum *and* maximum rather than one magnitude, because asymmetric audio
   * is a real thing and drawing it as a mirror image is a lie about the
   * recording.
   */
  readonly peaks?: readonly number[];
}

/** One media file a sitting produced. */
export interface LibraryRecording {
  /** The index's own identifier for it. */
  readonly recording_id: number;
  /** Its ordinal within the sitting, as the sidecar recorded it. */
  readonly session_index: number;
  /**
   * The file.
   *
   * This window cannot open it. It is what "reveal in Explorer" and a support
   * request name, and it is a path inside the user's own profile — so it is
   * redacted before it reaches a log (`redactPath.ts`).
   */
  readonly path: string;
  /** When it started, RFC 3339 with an offset. */
  readonly started_at: string;
  /** When it ended. Absent for one still running when the library was read. */
  readonly ended_at?: string;
  /** What became of it: `recorded`, `no-window` or `failed`. */
  readonly outcome?: string;
  /** Why it ended, and only meaningful for `recorded`. */
  readonly end_reason?: string;
  /** How long it runs for. Absent for a recording that produced no file. */
  readonly duration_seconds?: number;
  /** The encoded picture width. */
  readonly width?: number;
  /** The encoded picture height. */
  readonly height?: number;
  /**
   * What the file occupied when it was last seen.
   *
   * Kept while {@link missing_since} is set, so a drive coming back needs no
   * re-measurement — but it must not be added into a total meanwhile, because
   * that space is not being used.
   */
  readonly size_bytes?: number;
  /**
   * When the library first found the file gone.
   *
   * Absent while the file is there. Present is a state the screen has to *say*
   * rather than draw a broken tile around (AGENTS.md section 27).
   */
  readonly missing_since?: string;
  /** Whether the user favourited it. */
  readonly favourite: boolean;
  /**
   * Whether the user locked this recording itself.
   *
   * Its own lock only, which is what a *control* is drawn from: a recording
   * inside a locked sitting has nothing of its own to release.
   */
  readonly locked?: boolean;
  /**
   * Whether automatic cleanup will leave it alone.
   *
   * `locked`, or its sitting's lock, worked out by the recorder so the
   * cascade has one expression rather than one per window. This is what a
   * padlock is drawn from.
   */
  readonly protected?: boolean;
  /** The tags on it, alphabetically. */
  readonly tags: readonly string[];
}

/** One clip cut from a sitting. */
export interface LibraryClip {
  /** The index's own identifier for it. */
  readonly clip_id: number;
  /**
   * The file, when there is one.
   *
   * **Absent for a clip nothing has exported yet**, which is the normal state
   * of a generated highlight: it is a range of a recording until somebody asks
   * for a file, and asking is what makes one. It is still a clip the user made,
   * so a screen draws it — with whatever it offers in place of "reveal in
   * Explorer".
   *
   * Absent is not {@link LibraryClip.missing_since}. No path is "there is no
   * file yet"; `missing_since` is "there was one and it has gone".
   */
  readonly path?: string;
  /** What it is called, if anything. */
  readonly title?: string;
  /** When it was made, RFC 3339 with an offset. */
  readonly created_at: string;
  /** How long it runs for. */
  readonly duration_seconds?: number;
  /** What the file occupies, with the same caveat as a recording's. */
  readonly size_bytes?: number;
  /** When the library first found the file gone. */
  readonly missing_since?: string;
  /** Whether the user favourited it. */
  readonly favourite: boolean;
  /** The tags on it, alphabetically. */
  readonly tags: readonly string[];
}

/** One sitting, and what it produced. */
export interface LibrarySession {
  /** The identifier the recorder generated, shared by the sidecar and the files. */
  readonly session_id: string;
  /**
   * The catalogue's identifier for the game.
   *
   * Absent for a sitting the catalogue would not attribute: it reported a tie
   * and the recording was filed under no game rather than under a guess.
   */
  readonly game_id?: string;
  /**
   * The game's name as the catalogue knew it when it was played.
   *
   * Absent for the same reason. What to call that group on screen is this
   * interface's decision, which is why the protocol does not make one.
   */
  readonly game_name?: string;
  /** When the sitting started, RFC 3339 with the offset it was recorded in. */
  readonly started_at: string;
  /** When it ended. Absent for a sitting that has not. */
  readonly ended_at?: string;
  /** Why it ended: `game-exited`, `system-resumed` or `recorder-stopping`. */
  readonly end_reason?: string;
  /** Whether the user favourited the sitting itself. */
  readonly favourite: boolean;
  /**
   * Whether the user locked the sitting against automatic cleanup.
   *
   * A locked sitting protects every recording in it, so a padlock against a
   * *recording* is drawn from that recording's `protected` rather than from
   * this. Absent from a recorder older than locks — that build has none.
   */
  readonly locked?: boolean;
  /** The files it recorded, in the order they were recorded. */
  readonly recordings: readonly LibraryRecording[];
  /** The clips cut from it. */
  readonly clips: readonly LibraryClip[];
}

/** One page of the recording library, newest session first. */
export interface LibrarySessionPage {
  /**
   * The sessions on this page.
   *
   * Empty means the library holds no more sessions matching the request. That
   * is a different thing from a library that could not be read, which arrives
   * as a `library_unavailable` refusal saying why — and the two must never be
   * drawn the same way.
   */
  readonly sessions: readonly LibrarySession[];
  /**
   * The cursor for the page after this one.
   *
   * Absent at the end of the library, and present only when a further session
   * was actually found — so paging stops on this rather than on an empty page.
   */
  readonly next_cursor?: string;
}

/**
 * What the library holds for one game (SPEC.md section 17).
 *
 * `game_id` and `name` are both absent on the row for sittings the catalogue
 * would not attribute. There is at most one such row and it is last.
 */
export interface LibraryGame {
  /** The catalogue's identifier. */
  readonly game_id?: string;
  /** The name as the catalogue knew it when the game was last played. */
  readonly name?: string;
  /** When the first sitting of this game was recorded. */
  readonly first_seen_at?: string;
  /** When the most recent one was. */
  readonly last_played_at?: string;
  /** Sittings recorded. */
  readonly sessions: number;
  /** Recordings that are not in the trash. */
  readonly recordings: number;
  /** Clips that are not in the trash. */
  readonly clips: number;
  /** Sessions, recordings and clips the user has favourited. */
  readonly favourites: number;
  /**
   * What the files that are still there occupy.
   *
   * A missing file contributes nothing: the space it is not occupying is not
   * being used, and a library reporting 83 GB of files nobody can find would be
   * a lie.
   */
  readonly bytes: number;
  /** Recordings and clips whose file could not be found when the library was read. */
  readonly missing: number;
}

/** One command, and the identifier its reply will quote. */
export interface RecorderRequest {
  /** Chosen by the client, and quoted back in the response. */
  readonly id: number;
  /** The command's name. */
  readonly command: CommandName;
  /** The command's parameters. May be left out when they are all optional. */
  readonly params?: JsonValue;
}

/** One reply, quoting the request it answers. */
export interface RecorderResponse {
  /** The request this answers. */
  readonly id: number;
  /** What happened. */
  readonly outcome: Outcome;
}

/** The command worked. */
export interface OkOutcome {
  /** What it produced. */
  readonly ok: Reply;
}

/** It did not, and this is what to tell the user. */
export interface ErrorOutcome {
  /** Why not. */
  readonly error: ProtocolError;
}

/** What a command produced, or why it did not. */
export type Outcome = OkOutcome | ErrorOutcome;

/** The recorder is alive. */
export interface PongReply {
  /** The tag. */
  readonly reply: 'pong';
}

/** What the recorder is doing. */
export interface StatusReply {
  /** The tag. */
  readonly reply: 'status';
  /** The state, as of the moment the recorder answered. */
  readonly status: RecorderStatus;
}

/** A recording started. */
export interface RecordingStartedReply {
  /** The tag. */
  readonly reply: 'recording_started';
  /** Identifies it to `stop_recording`. */
  readonly recording_id: string;
  /** The file it is writing. */
  readonly output: string;
}

/** A recording stopped, and its file is finished. */
export interface RecordingStoppedReply {
  /** The tag. */
  readonly reply: 'recording_stopped';
  /** What it turned out to be. */
  readonly summary: RecordingSummary;
}

/** A moment was marked, and the mark is on disk. */
export interface BookmarkAddedReply {
  /** The tag. */
  readonly reply: 'bookmark_added';
  /** Where it landed, which is not where the request was made. */
  readonly bookmark: BookmarkSummary;
}

/** A screenshot was taken, and the file is on disk. */
export interface ScreenshotTakenReply {
  /** The tag. */
  readonly reply: 'screenshot_taken';
  /** The file, and what is in it. */
  readonly screenshot: ScreenshotSummary;
}

/** A replay was saved, and the clip is finished and playable. */
export interface ReplaySavedReply {
  /** The tag. */
  readonly reply: 'replay_saved';
  /** The clip, and how it compares with what was asked for. */
  readonly clip: ReplaySummary;
}

/** One page of the recording library. */
export interface LibrarySessionsReply {
  /** The tag. */
  readonly reply: 'library_sessions';
  /** The sittings, newest first, and where the next page starts. */
  readonly page: LibrarySessionPage;
}

/** What the library holds per game. */
export interface LibraryGamesReply {
  /** The tag. */
  readonly reply: 'library_games';
  /** One row per game, and one for the sittings nothing was attributed to. */
  readonly games: readonly LibraryGame[];
}

/** One mark on a recording's timeline. */
export interface LibraryEventMark {
  /** The recording this mark is on, as the library identifies it. */
  readonly recording: string;
  /** How far into that recording's file the event is, in nanoseconds. */
  readonly at: number;
  /**
   * What happened.
   *
   * **Not a closed set.** A kind added after this build shipped, and a
   * plugin's namespaced custom name, both arrive here and both must be drawn:
   * validating against a list would delete exactly the marks that have to
   * survive.
   */
  readonly kind: string;
  /** Who reported it: a plugin's identifier, or `clipped`. */
  readonly source: string;
}

/** The marks of one recording. */
export interface LibraryEventLane {
  /**
   * The marks, earliest first.
   *
   * Always present. An empty array means the recording has no events, which is
   * a different thing from the question not having been asked — and the two
   * are drawn differently.
   */
  readonly marks: readonly LibraryEventMark[];
}

/**
 * Asking for one clip's edit document.
 *
 * The clip is named by the index's own identifier, carried as a string exactly
 * as `LibraryEvents` carries a recording's.
 */
export interface LibraryClipDocument {
  /** The clip to open, as the library identifies it. */
  readonly clip: string;
}

/**
 * One clip's edit document, as text.
 *
 * # Why text
 *
 * `docs/editing.md` settles it: a document crosses this boundary as the same
 * JSON `clipped_edit` writes, rather than being converted into a second
 * representation for the window. `src/editor/document.ts` is the reader for it,
 * and it is the only thing in this window that knows the shape of a document.
 *
 * # What the text is guaranteed to be
 *
 * The version this window's reader understands. A stored document older than
 * the recorder's build is converted before it is sent — {@link convertedFrom}
 * says so — and one newer than the recorder's build is refused rather than
 * sent. That is what makes the reader's own refusal of an older document
 * correct rather than a gap: it never receives one.
 */
export interface ClipDocument {
  /** The clip this is, echoed from the request. */
  readonly clip: string;
  /** The document itself, as `clipped_edit` writes one. */
  readonly document: string;
  /**
   * The format the stored text was in, when it was older than the text above.
   *
   * Absent in the ordinary case. **Present does not mean anything was written**
   * — reading converts in memory only, and the stored text is still the older
   * one until somebody saves.
   */
  readonly converted_from?: number;
  /**
   * Whether the library held no document and this is a starting one.
   *
   * True for a saved replay: a clip made before there was an editor. The
   * recorder builds "this recording, this span, no edits" rather than leaving
   * the window to invent one, so that two builds cannot disagree about what an
   * unedited clip is. Nothing has been stored.
   */
  readonly synthesised: boolean;
}

/** Storing an edited document against a clip. */
export interface SaveClipDocument {
  /** The clip to store it against. */
  readonly clip: string;
  /** The document, as this window has it. */
  readonly document: string;
}

/** What a save did. */
export interface ClipDocumentSaved {
  /** The clip, echoed from the request. */
  readonly clip: string;
  /**
   * The format the text this save replaced was in, when it was older and was
   * therefore kept.
   *
   * Present means a copy of the older text is in the index beside the new one
   * and will not be overwritten by a later save. Absent means there was nothing
   * older to keep.
   */
  readonly superseded?: number;
}

/**
 * One thing waiting in the trash.
 *
 * The window may not link `clipped-library`, so this is the projection a screen
 * needs: what it was, when it went, when it expires, and what keeping or
 * freeing it costs ([issue #450](https://github.com/wildware-uk/clipped/issues/450)).
 */
export interface TrashedItem {
  /** `recording` or `clip`. */
  readonly kind: string;
  /** The library's own identifier for it. */
  readonly id: number;
  /**
   * Where the file is now, inside the trash.
   *
   * Absent for an item that has no file: a clip nothing has exported is a range
   * of a recording, and deleting it deletes the clip rather than a file
   * ([issue #593](https://github.com/wildware-uk/clipped/issues/593)). Absent
   * rather than `''`, which is a file name a screen would try to open.
   */
  readonly path?: string;
  /**
   * Where it was, and where restoring puts it back.
   *
   * The one a person recognises. A screen that showed only the trash's own copy
   * would be asking them to identify a recording by a name they have never
   * seen.
   *
   * Absent for the same reason `path` is: something that never had a file was
   * never anywhere, and a screen names it by what it is instead.
   */
  readonly original_path?: string;
  /** When it was deleted, RFC 3339 with an offset. */
  readonly deleted_at: string;
  /** When it will be removed for good, where the recorder knows. */
  readonly expires_at?: string;
  /** What it occupied when the index last saw it. */
  readonly size_bytes?: number;
  /** How many clips were cut from this recording and now point at it. */
  readonly dependent_clips: number;
}

/** What is in the trash, and what emptying it would take. */
export interface TrashListing {
  /** Everything in it, newest deletion first. */
  readonly items: readonly TrashedItem[];
  /** How many there are, which is half of what emptying confirms. */
  readonly total_items: number;
  /** What they occupy, which is the other half. */
  readonly total_bytes: number;
  /** Where the trash directory is. */
  readonly directory: string;
}

/** What is waiting in the trash. */
export interface LibraryTrashReply {
  /** The tag. */
  readonly reply: 'library_trash';
  /** Everything in it, what it occupies, and where it is. */
  readonly trash: TrashListing;
}

/** What came back out of the trash. */
export interface RestoredItem {
  /** Which one it was. */
  readonly kind: string;
  /** Its identifier. */
  readonly id: number;
  /**
   * Where the file is now, which is where the index now points.
   *
   * Absent for an item that has no file, which comes back with none.
   */
  readonly path?: string;
  /**
   * Whether there was a file to move back.
   *
   * `false` for something whose media had already gone before it was deleted,
   * and for something that never had any: the row returns to the library and
   * reports itself missing or fileless, which is the truth rather than a row
   * with no explanation.
   */
  readonly file_restored: boolean;
  /** Whether it had to go somewhere else, because something was in the way. */
  readonly renamed: boolean;
}

/** One thing was put back where it was. */
export interface RestoredReply {
  /** The tag. */
  readonly reply: 'restored';
  /** What came back. */
  readonly restored: RestoredItem;
}

/** What emptying the trash destroyed, and what it could not. */
export interface TrashEmptied {
  /** How many things were destroyed. */
  readonly removed: number;
  /** What the volume got back. */
  readonly reclaimed_bytes: number;
  /**
   * What would not go, each saying why.
   *
   * Always present. A file another program had open is a real outcome and the
   * next sweep tries it again; a window that showed only the count would say
   * the trash is empty when it is not.
   */
  readonly refused: readonly string[];
}

/** The trash was emptied. */
export interface TrashEmptiedReply {
  /** The tag. */
  readonly reply: 'trash_emptied';
  /** What was destroyed, and what would not go. */
  readonly emptied: TrashEmptied;
}

/**
 * How many recordings a list in a storage report names before it stops.
 *
 * Mirrors `MOST_LISTED` in `crates/ipc/src/storage.rs`. Enough to fill a panel
 * and to answer "what is filling my drive"; far short of a frame. What is left
 * out is not hidden — {@link RecordingList.total} and
 * {@link RecordingList.total_bytes} are of the whole set, and a screen says so.
 */
export const MOST_LISTED = 25;

/**
 * What a library may occupy, as a window reads and proposes it.
 *
 * Every field is optional and absent means **no limit of that kind**, which is
 * what Clipped ships with. That reading is the same in both directions: absent
 * in a {@link StorageReport} is a limit nobody has configured, and absent in a
 * {@link GetStorageParams.limits} is a limit the window is asking about the
 * removal of.
 *
 * Bytes rather than gigabytes, and days rather than a duration, because that is
 * how `settings.json` spells them — and a second unit on the wire is a second
 * place for a factor of 1024 to be wrong.
 */
export interface StorageLimits {
  /** What the library may occupy, in bytes. */
  readonly maximum_usage_bytes?: number;
  /** What must stay free on the volume, in bytes. */
  readonly minimum_free_space_bytes?: number;
  /** How old a recording may get, in days. */
  readonly maximum_age_days?: number;
}

/**
 * Ask what the library occupies and what a limit would do about it. A type
 * alias for the reason {@link StartRecordingParams} is one.
 *
 * All optional, and an omitted request measures against the limits that are
 * configured — which is what a screen asks when it opens.
 */
export type GetStorageParams = {
  /**
   * Limits to judge the measurement against **instead of** the configured ones.
   *
   * This is the dry run: a window about to save a maximum usage sends the value
   * somebody typed and is told what saving it would delete, before the setting
   * is written and before the sweep acts on it.
   *
   * The whole set is replaced rather than merged, so a field left out is a limit
   * the proposal does not have. Merging would make "clear this limit"
   * unexpressible, and a window could not preview a removal.
   *
   * Nothing is saved by asking. The limits are written through `apply_settings`
   * like every other setting.
   */
  readonly limits?: StorageLimits;
};

/** What one recording occupies, and whether a sweep may take it. */
export interface StorageRecording {
  /** The index's own identifier, as `library_sessions` reports it. */
  readonly recording_id: number;
  /**
   * The file.
   *
   * The same path `library_sessions` sends, so a window can match a row here
   * against the recording it already drew. A path inside the user's own profile,
   * so it is redacted before it reaches a log (`redactPath.ts`).
   */
  readonly path: string;
  /** What it occupies, or zero when nothing has measured it. */
  readonly size_bytes: number;
  /** When it started, RFC 3339. The order a sweep deletes in. */
  readonly started_at: string;
  /**
   * Why a sweep will not take it, in the recorder's own words.
   *
   * Absent for a recording nothing protects, which is one a sweep may take.
   * Present is drawn beside the row rather than instead of it: a protected
   * recording is still filling the drive.
   */
  readonly protected_because?: string;
}

/**
 * Some recordings, and the whole set they were taken from.
 *
 * The two totals are of everything, and {@link recordings} is the first
 * {@link MOST_LISTED} of them. A screen draws the rows and says how many more
 * there are — a truncated list that did not carry its own total would read as
 * the whole answer, which for "what a limit would delete" is the worst possible
 * thing to be wrong about.
 */
export interface RecordingList {
  /** How many recordings there are in all. */
  readonly total: number;
  /** What all of them occupy. */
  readonly total_bytes: number;
  /** The first {@link MOST_LISTED} of them, in the order the list is about. */
  readonly recordings: readonly StorageRecording[];
}

/**
 * One rule that keeps recordings out of a sweep, and what it is holding.
 *
 * SPEC.md section 27's "never automatically delete" list, as measured state
 * rather than as a sentence on a screen: this is how many recordings that rule
 * is protecting right now and what they occupy. A screen drawing "favourites are
 * protected" with no figure beside it is decorative copy, and a user cannot tell
 * it from a promise nothing keeps (AGENTS.md section 27).
 */
export interface ProtectedGroup {
  /**
   * The rule, in the words a person reads, such as `Favourites`.
   *
   * Sent rather than derived, for the reason {@link HotkeyBinding.label} is: the
   * vocabulary of protections lives in the recorder, and a window keeping its own
   * table of them would show nothing at all for a rule a newer recorder had
   * added.
   */
  readonly label: string;
  /** How many recordings it is protecting. */
  readonly recordings: number;
  /** What they occupy. */
  readonly bytes: number;
}

/** What one kind of file occupies. */
export interface CategoryUsage {
  /** The kind, as accounting names it: `recordings`, `trash`, `thumbnails`. */
  readonly category: string;
  /** What the files of that kind occupy. */
  readonly bytes: number;
}

/**
 * What the library occupies, what a limit would take, and what it would keep.
 *
 * The reply to `get_storage`. Everything in it is measured: the usage is a walk
 * of the recording and trash directories, the free space is what the volume
 * reports, and the plan is the one the sweep would carry out.
 */
export interface StorageReport {
  /**
   * Where this recorder writes, which is the directory that was measured.
   *
   * The directory **in force**, which for the length of a sitting can differ
   * from the one `settings.json` holds: where automatic recordings go moves
   * between sittings and never during one (issue #609). This is the folder the
   * figures are about — `get_settings` carries the other, and says so on the row
   * with {@link SettingEntry.not_yet_in_force}.
   */
  readonly recordings_directory: string;
  /** Where deleted media waits, and is measured as part of the usage. */
  readonly trash_directory: string;
  /** What the library occupies, across every category measured. */
  readonly usage_bytes: number;
  /**
   * What each category occupies, largest first.
   *
   * A category with nothing in it is left out rather than sent as zero.
   */
  readonly by_category: readonly CategoryUsage[];
  /**
   * What is free on the volume the recordings are on.
   *
   * Measured, not derived: the disk holds other applications' files too, so this
   * cannot be worked out from the usage above.
   */
  readonly free_bytes: number;
  /** The whole volume, which is what makes the free figure mean something. */
  readonly capacity_bytes: number;
  /** The limits the measurement was judged against. */
  readonly limits: StorageLimits;
  /**
   * Whether those limits came from {@link GetStorageParams.limits} rather than
   * from the settings file.
   *
   * `true` is a dry run: **nothing has been saved**, and a window has to say so
   * or it is showing somebody the consequences of a setting they will believe is
   * already in force.
   */
  readonly proposed: boolean;
  /**
   * What a sweep would send to the trash under those limits, oldest first.
   *
   * Empty for a library inside its limits, and for one with no limits at all.
   * Not empty is the confirmation a window owes somebody before it saves them:
   * these recordings, this much.
   */
  readonly would_delete: RecordingList;
  /**
   * What would still be over the limit once all of that had gone.
   *
   * Zero when the limits would be met. Non-zero means the sweep would run out of
   * things it is allowed to delete, which is a disk that stays full and is
   * something somebody has to be told rather than a cleanup that worked.
   */
  readonly still_over_limit: number;
  /**
   * What a sweep would keep, one row per rule.
   *
   * Empty on a library where nothing is favourited or locked. Never a reason to
   * draw nothing: a screen says the rules protect nothing yet, which is a
   * different thing from a screen that did not ask.
   */
  readonly protected: readonly ProtectedGroup[];
  /**
   * Every recording the index knows, largest first.
   *
   * The review path SPEC.md section 27 asks for: somebody who can see what is
   * filling their drive can act before automatic cleanup does.
   */
  readonly largest: RecordingList;
}

/** What the library occupies, and what a limit would do about it. */
export interface StorageReply {
  /** The tag. */
  readonly reply: 'storage';
  /** The measurement, the limits it was judged against, and the plan. */
  readonly storage: StorageReport;
}

/**
 * Marking one thing a favourite, or clearing the mark.
 *
 * The target takes two fields because the schema does: a sitting is addressed by
 * the identifier the recorder generated, which is text, and a recording or a
 * clip by the integer key the index gave it. `kind` says which of the two to
 * read, and the recorder refuses a request that filled in neither rather than
 * marking whatever row is at zero.
 */
export interface SetFavourite {
  /** `session`, `recording` or `clip`. */
  readonly kind: string;
  /** The sitting's own identifier, for `session`. */
  readonly session_id: string;
  /** The library's integer identifier, for `recording` and `clip`. */
  readonly id: number;
  /**
   * Whether it should be a favourite afterwards.
   *
   * The state to be in, not a toggle: two windows open on one library would
   * disagree about what a toggle means.
   */
  readonly favourite: boolean;
}

/** What the mark is now. */
export interface FavouriteMark {
  /** Which thing it was, echoed so a window can match the reply to the row. */
  readonly kind: string;
  /** The sitting's identifier, for `session`. */
  readonly session_id: string;
  /** The integer identifier, for `recording` and `clip`. */
  readonly id: number;
  /** Whether it is a favourite now, which is what a screen draws. */
  readonly favourite: boolean;
  /**
   * Whether this request is what changed it.
   *
   * `false` for a star that was already full: the difference between "you did
   * that" and "that was already so".
   */
  readonly changed: boolean;
}

/** A favourite mark was set or cleared. */
export interface FavouritedReply {
  /** The tag. */
  readonly reply: 'favourited';
  /** Which thing, and what its mark is now. */
  readonly mark: FavouriteMark;
}

/**
 * Locking one thing against automatic cleanup, or unlocking it.
 *
 * The same target shape as {@link SetFavourite} over a shorter vocabulary: a
 * clip cannot be locked, because automatic cleanup deletes recordings and a
 * mark nothing consults is worse than no mark at all.
 *
 * A lock protects against automatic cleanup and nothing else. A locked
 * recording is deleted by a manual delete exactly as an unlocked one is, and a
 * window must not imply otherwise.
 */
export interface SetLock {
  /** `session` or `recording`. */
  readonly kind: string;
  /** The sitting's own identifier, for `session`. */
  readonly session_id: string;
  /** The library's integer identifier, for `recording`. */
  readonly id: number;
  /** Whether it should be locked afterwards. */
  readonly locked: boolean;
}

/** What the lock is now. */
export interface LockMark {
  /** Which thing it was, echoed so a window can match the reply to the row. */
  readonly kind: string;
  /** The sitting's identifier, for `session`. */
  readonly session_id: string;
  /** The integer identifier, for `recording`. */
  readonly id: number;
  /** Whether it has a lock of its own now. */
  readonly locked: boolean;
  /**
   * Whether automatic cleanup will leave it alone.
   *
   * Not the same question as {@link locked}, and this is the one a padlock is
   * drawn from: a recording inside a locked sitting is protected without
   * having a lock of its own.
   */
  readonly protected: boolean;
  /** Whether this request is what changed it. */
  readonly changed: boolean;
}

/** A lock was set or cleared. */
export interface LockedReply {
  /** The tag. */
  readonly reply: 'locked';
  /** Which thing, what its lock is now, and whether cleanup will leave it alone. */
  readonly lock: LockMark;
}

/** The marks on one recording's timeline. */
export interface LibraryEventsReply {
  /** The tag. */
  readonly reply: 'library_events';
  /** The events, placed in that recording's file. */
  readonly lane: LibraryEventLane;
}

/** One clip's edit document. */
export interface LibraryClipDocumentReply {
  /** The tag. */
  readonly reply: 'library_clip_document';
  /** The document as text, and how it was arrived at. */
  readonly clip: ClipDocument;
}

/** An edited document was stored. */
export interface ClipDocumentSavedReply {
  /** The tag. */
  readonly reply: 'clip_document_saved';
  /** Which clip, and what was kept. */
  readonly saved: ClipDocumentSaved;
}

/**
 * Whether a plugin will start, and why not when it will not.
 *
 * Four states rather than a boolean: a plugin nobody has enabled needs an
 * invitation, one that was turned off needs nothing, and one whose consent has
 * lapsed needs somebody to look at what changed.
 */
export type PluginState =
  | { readonly state: 'enabled' }
  | { readonly state: 'not-enabled' }
  | { readonly state: 'turned-off' }
  | {
      readonly state: 'needs-consent-again';
      /** What the user agreed to. */
      readonly agreed_to: string;
      /** What it declares now. */
      readonly now_declares: string;
    };

/** One installed plugin, and what it asks for. */
export interface PluginDeclaration {
  /** The plugin's identifier, as its manifest gives it. */
  readonly id: string;
  /** What to call it on a screen. */
  readonly name: string;
  /** Its own version. Free text: nothing compares two of them. */
  readonly version: string;
  /** What it says it does. */
  readonly description: string;
  /**
   * What it will do with the network, one plain sentence per grant.
   *
   * Empty means it declares none, which a screen must **say** rather than draw
   * as a blank row.
   */
  readonly network: readonly string[];
  /**
   * What Clipped can and cannot promise about the sentences above.
   *
   * Sent with every declaration rather than kept here, because it is part of
   * what somebody agrees to and a second copy could drift from what the
   * recorder enforces.
   */
  readonly enforcement: string;
  /** What this build will do about the plugin. */
  readonly state: PluginState;
}

/** Something under the plugins directory that is not a usable plugin. */
export interface RefusedPlugin {
  /** Where it is. */
  readonly directory: string;
  /** Why it was refused, in the words the recorder used. */
  readonly reason: string;
}

/** What plugins are installed, and what each of them asks for. */
export interface PluginsReply {
  /** The tag. */
  readonly reply: 'plugins';
  /** Every plugin discovery could read. */
  readonly installed: readonly PluginDeclaration[];
  /** Everything that is not one, and why. Always present. */
  readonly refused: readonly RefusedPlugin[];
}

/** A recording was copied into MP4, and the file is finished. */
export interface RecordingExportedReply {
  /** The tag. */
  readonly reply: 'recording_exported';
  /** The copy, and what it turned out to hold. */
  readonly export: ExportSummary;
}

/** A recording is ready to be played, and here is what to play. */
export interface PlaybackOpenedReply {
  /** The tag. */
  readonly reply: 'playback_opened';
  /** The file to play, the track it carries, and the tracks beside it. */
  readonly playback: PlaybackStream;
}

/**
 * A recording's thumbnail or waveform, or the reason there is not one yet.
 *
 * The picture itself, rather than a path to it: what the recorder holds is a
 * cache entry the window has no permission to read, and the peaks are a binary
 * sidecar only `crates/waveform` knows how to read. Both therefore travel as
 * this reply (issue #448).
 */
export interface PreviewOpenedReply {
  /** The tag. */
  readonly reply: 'preview_opened';
  /** What there is of the picture, and which of the three states it is in. */
  readonly preview: Preview;
}

/**
 * The states one hotkey binding can be in.
 *
 * Closed, like {@link RecorderState} and for the same reason: a state this
 * build cannot read would be drawn as one it can, and every one it can read
 * says the key works.
 */
export const HOTKEY_STATES = ['unbound', 'registered', 'conflict'] as const;

/** What Windows said about one binding. There is no unrecognised state. */
export type HotkeyStateName = (typeof HOTKEY_STATES)[number];

/** The action has no combination, so nothing was registered. */
export interface UnboundHotkey {
  /** The tag. */
  readonly state: 'unbound';
}

/** Windows accepted it, and presses are being delivered. */
export interface RegisteredHotkey {
  /** The tag. */
  readonly state: 'registered';
}

/** Windows refused it, most often because another application owns it. */
export interface ConflictingHotkey {
  /** The tag. */
  readonly state: 'conflict';
  /**
   * What to tell the user, in the recorder's own words.
   *
   * Shown as it arrives: only the recorder knows what failed and who is likely
   * to have the combination, so the window invents no wording of its own.
   */
  readonly reason: string;
}

/** What Windows said about one binding. */
export type HotkeyState = UnboundHotkey | RegisteredHotkey | ConflictingHotkey;

/** One action, what it is bound to, and whether pressing it would do anything. */
export interface HotkeyBinding {
  /** The action's stable name, such as `save_replay`. */
  readonly action: string;
  /** The action's name in the words a person reads, such as `Save replay`. */
  readonly label: string;
  /** The combination, written as `Ctrl+F10`. Absent when nothing is bound. */
  readonly hotkey?: string;
  /** What Windows said about it. */
  readonly state: HotkeyState;
  /**
   * Whether anything in the recorder performs the action.
   *
   * A registered combination with no handler is still a key that does nothing,
   * so this is read as well as the state before a row is drawn as working.
   */
  readonly handled: boolean;
  /** Why pressing it would do nothing, when that is the case. */
  readonly unavailable?: string;
}

/**
 * One setting, as a window draws it.
 *
 * The value crosses as the words the settings file spells it in — `120`,
 * `hevc`, `name:Shure MV7` — and goes back the same way, so the window keeps no
 * second vocabulary for settings and no second opinion about what is valid.
 */
export interface SettingEntry {
  /** The key the settings file holds it under, such as `microphone`. */
  readonly key: string;
  /** The setting's name in the words a person reads. */
  readonly label: string;
  /** What it resolves to, spelled the way the file spells it. Never blank. */
  readonly value: string;
  /** Whether this was configured, rather than being the value Clipped ships with. */
  readonly overridden: boolean;
  /**
   * Every value it can take, where the set is closed.
   *
   * Absent for the settings whose values are open — a frame rate, a size, a
   * device name — which is how a list of options is told from a field.
   */
  readonly choices?: readonly string[];
  /** What it would accept, in the words its refusal uses. */
  readonly accepted: string;
  /**
   * Whether anything reads it when a recording starts.
   *
   * `false` is a setting the file can carry and no recording acts on. Drawn as
   * a value with the sentence below rather than as a working control, for the
   * reason {@link HotkeyBinding.handled} exists (AGENTS.md section 27).
   */
  readonly applies: boolean;
  /** Why changing it would not change a recording, when that is the case. */
  readonly unavailable?: string;
  /**
   * What is still in force, for a value that is saved and not yet the one being
   * used.
   *
   * A different question from {@link unavailable}: that is a setting nothing
   * reads at all, this is one that is read and has not got there yet. Absent for
   * every setting the next recording uses, which is all of them but the
   * recording directory — where automatic recordings are written moves between
   * sittings and never during one, so that a sitting's session record is never
   * separated from the files it names (AGENTS.md section 56, issue #609).
   *
   * Drawn beside the control rather than in place of it: the value is what was
   * saved, and this says when it starts counting (AGENTS.md section 27).
   */
  readonly not_yet_in_force?: string;
}

/** The settings, and the file they came from. */
export interface SettingsView {
  /** The settings file they live in, as the recorder resolved it. */
  readonly file: string;
  /**
   * The game these were resolved for, absent for the global settings.
   *
   * Read rather than assumed. A recorder built before the per-game page answers
   * the global settings whatever is asked of it, and a window that drew that
   * under a game's name would show every value as inherited when the global
   * settings had set half of them — so a page compares this against the game it
   * asked about (AGENTS.md section 27).
   */
  readonly game?: string;
  /**
   * Every game the settings file holds a section of its own for, in identifier
   * order.
   *
   * The games somebody has already configured, and **not** the games this
   * machine has: which processes are games is the catalogue's answer, and no
   * command reads it (issue #245).
   *
   * A game stays on this list after its last override is cleared, because the
   * recorder keeps an empty section rather than dropping one — so it is "games
   * with a page", not "games with an override".
   */
  readonly games?: readonly string[];
  /** Every setting the recorder will accept, in the order a screen lists them. */
  readonly settings: readonly SettingEntry[];
}

/** Every setting, as it now stands. The answer to a read and to a change alike. */
export interface SettingsReply {
  /** The tag. */
  readonly reply: 'settings';
  /** The settings, and the file they live in. */
  readonly settings: SettingsView;
}

/** One audio endpoint this machine has. */
export interface AudioDevice {
  /** The name Windows gives it, which is what a settings file names it by. */
  readonly name: string;
  /** Whether this is the endpoint Windows currently considers the default. */
  readonly is_default: boolean;
}

/**
 * The audio endpoints a recording could be told to use.
 *
 * Microphones only: a recording cannot be told to use a playback endpoint that
 * is not the default one, so an empty list of them would say something untrue
 * about the machine (issue #316).
 */
export interface AudioDevices {
  /** Every capture endpoint present and active, in the order Windows lists them. */
  readonly microphones: readonly AudioDevice[];
}

/** The audio endpoints this machine has. */
export interface AudioDevicesReply {
  /** The tag. */
  readonly reply: 'audio_devices';
  /** Every microphone, with the default one marked. */
  readonly devices: AudioDevices;
}

/**
 * What a microphone is hearing.
 *
 * Asked repeatedly while a meter is on screen rather than streamed: the recorder
 * opens the endpoint, listens briefly and closes it inside the call, so a window
 * that is killed mid-choice leaves no capture running.
 */
export interface MicrophoneLevel {
  /**
   * The endpoint that was listened to, as Windows names it.
   *
   * Absent while the device is unplugged or disabled, during which a capture
   * produces silence rather than failing — which is what tells "nobody is
   * speaking" from "there is nothing there".
   */
  readonly device?: string;
  /**
   * The loudest sample heard, from `0` to `1`.
   *
   * The loudest in the moment that was listened to, not since the last question:
   * a screen that kept the highest reading it ever saw would draw a meter that
   * only ever went up.
   */
  readonly peak: number;
  /**
   * Whether Windows reports the microphone muted.
   *
   * Absent when Windows will not report the switch for this device. A muted
   * microphone reads as silence, so this is what stops a screen telling somebody
   * to speak up when what they need is to unmute.
   */
  readonly muted?: boolean;
}

/** What the microphone that was asked about is hearing. */
export interface MicrophoneLevelReply {
  /** The tag. */
  readonly reply: 'microphone_level';
  /** The reading, and what was listened to. */
  readonly level: MicrophoneLevel;
}

/**
 * Whether the recorder starts when this user signs in, and what is arranged.
 *
 * Not a setting: it is a `Run` value Windows reads at sign-in rather than a key
 * in `settings.json`, and the recorder is what reads and writes it because the
 * value names the executable to run and that executable is the recorder
 * (issue #308).
 */
export interface StartAtLogin {
  /**
   * Whether Windows has an entry for Clipped under this account.
   *
   * The switch's position, and nothing more: an entry that is there and names
   * an executable that is gone is still `true`, because that is what Windows
   * will try to run.
   */
  readonly enabled: boolean;
  /** Where the entry is, spelled the way a registry editor spells it. */
  readonly location: string;
  /** The command line Windows would run. Absent exactly when `enabled` is false. */
  readonly command?: string;
  /**
   * The executable the entry names, when it is no longer there.
   *
   * A Clipped that moved or was reinstalled. Absent when the entry is missing
   * and when it is fine, so its presence is exactly the case to act on — and
   * the action is turning the switch on again from this installation.
   */
  readonly missing_executable?: string;
}

/** Turn starting at login on, or off. */
export type SetStartAtLoginParams = {
  /** `true` writes the entry, `false` removes it. */
  readonly enabled: boolean;
};

/** Whether the recorder starts at sign-in, as it now stands. */
export interface StartAtLoginReply {
  /** The tag. */
  readonly reply: 'start_at_login';
  /** The arrangement: whether it is on, what would run, and whether that exists. */
  readonly start_at_login: StartAtLogin;
}

/**
 * One replacement or restart of the capture backend, and why.
 *
 * A restart of the same backend is one of these too, with {@link restart} set:
 * "it fell over and came back" is what explains a gap in an otherwise
 * unexplained recording.
 */
export interface CaptureMethodChange {
  /** The method that was in use before. */
  readonly from: string;
  /** The method in use after it, equal to {@link from} for a restart. */
  readonly to: string;
  /** Whether the same backend started again rather than a different one taking over. */
  readonly restart: boolean;
  /**
   * What made it necessary: `initialisation_failed`, `capture_failed` or
   * `black_frames`.
   *
   * Open, like an end reason: a trigger this build has never heard of is shown
   * rather than failing the frame that carried it, and nothing branches on it.
   */
  readonly trigger: string;
  /** The failure, in the recorder's own words and phrased for this screen. */
  readonly reason: string;
}

/**
 * How the recording in progress is capturing.
 *
 * The three things SPEC.md section 8 asks a recorder to be able to say: what
 * was asked for, what is running, and how it got there.
 */
export interface CaptureAccount {
  /** What the user asked for: `Automatic`, or the method they pinned. */
  readonly setting: string;
  /** The method this recording started with. */
  readonly started_with: string;
  /** The method capturing now. */
  readonly current: string;
  /**
   * Every replacement and restart, in the order they happened.
   *
   * Empty is a measurement rather than an absence: it says the backend this
   * recording started with is still the one running.
   */
  readonly changes: readonly CaptureMethodChange[];
}

/** One graphics adapter. */
export interface AdapterSummary {
  /** The model name the driver publishes: `NVIDIA GeForce RTX 4090`. */
  readonly description: string;
  /** `nvidia`, `amd`, `intel`, `microsoft` or `other`. */
  readonly vendor: string;
  /** `own_video_memory`, `shared_video_memory` or `software`. */
  readonly kind: string;
  /** Bytes of video memory of its own; zero for one that shares the machine's. */
  readonly video_memory_bytes: number;
  /** The driver version, where the adapter reported one. */
  readonly driver_version?: string;
  /**
   * Whether this is the adapter a recording creates its graphics device on.
   *
   * The reason an encoder can be present, working and unusable.
   */
  readonly captures: boolean;
}

/** What is known about one codec on one encoder family. */
export interface CodecSummary {
  /** `h264`, `hevc` or `av1`. */
  readonly codec: string;
  /**
   * Whether the encoder can produce it.
   *
   * **Absent is not `false`.** Absent means nothing knows; `false` means
   * something was asked and said no.
   */
  readonly supported?: boolean;
  /** The widest picture, where a limit is known. */
  readonly max_width?: number;
  /** The tallest picture, where a limit is known. */
  readonly max_height?: number;
  /** Frames a second at 1080p, where a rate is known. */
  readonly max_framerate_1080p?: number;
  /**
   * Whether any value above came from the family's published limits rather
   * than from this machine.
   */
  readonly inferred: boolean;
}

/** One encoder family, and what is known about it. */
export interface EncoderSummary {
  /** `nvenc`, `amf`, `quick_sync` or `software`. */
  readonly encoder: string;
  /** The family in the words a person reads: `NVIDIA NVENC`. */
  readonly label: string;
  /** Whether a recording made on this machine could use it. */
  readonly available: boolean;
  /** Why it cannot be used, when it cannot. The recorder's own sentence. */
  readonly unavailable?: string;
  /**
   * Whether **this build** has a backend proven to encode with it.
   *
   * The distance between "your machine can do this" and "Clipped can do this".
   */
  readonly implemented: boolean;
  /** The adapter it runs on, by {@link AdapterSummary.description}. */
  readonly adapter?: string;
  /**
   * Whether an encoder session was ever opened and asked about this family.
   *
   * Not a claim about this call: answering `get_diagnostics` never opens one.
   * `true` means the stored answer was measured that way. Whether a particular
   * number came from the machine is {@link CodecSummary.inferred}.
   */
  readonly asked: boolean;
  /** What is known about each codec, most efficient first. Empty when absent. */
  readonly codecs: readonly CodecSummary[];
}

/** What this machine can encode, as `clipped-recorder capabilities` reports it. */
export interface EncoderAccount {
  /** Whether the machine was asked in this run rather than a stored answer used. */
  readonly probed: boolean;
  /** When a stored answer was measured, RFC 3339. Absent when {@link probed}. */
  readonly detected_at?: string;
  /** How long the answer took to obtain, cache lookup included. */
  readonly elapsed_ms: number;
  /** The graphics adapters, in the order the system enumerated them. */
  readonly adapters: readonly AdapterSummary[];
  /**
   * Every encoder family, including the ones that are not here.
   *
   * "Clipped did not find your NVIDIA card" is the report a user with a problem
   * needs, and a list that left the absent ones out would be indistinguishable
   * from a build that had never heard of them.
   */
  readonly encoders: readonly EncoderSummary[];
}

/**
 * One setting the recording in progress is running with.
 *
 * Not one setting of the settings file. A recording is built from what its
 * caller asked for - a `clipped-recorder watch` command line, or a
 * `start_recording` - with the settings configured for that game laid over it,
 * so a setting nobody configured keeps the caller's answer.
 */
export interface EffectiveSetting {
  /**
   * The key, as `settings.json` spells it: `resolution`, `framerate`, `codec`,
   * `encoder`, `microphone`, `system_audio`.
   *
   * Two of the settings a game may override are never here, and their absence
   * is the honest answer rather than a gap: `capture_target`, which nothing in
   * this build reads, and `replay_window_seconds`, which sizes a buffer rather
   * than being a property of the recording.
   */
  readonly setting: string;
  /** The value, spelled the way the settings file spells it. */
  readonly value: string;
  /**
   * Where this recording's answer came from.
   *
   * `default`, `global` or `game` for the three layers of the settings file,
   * and `request` for a setting the recording asked for itself.
   */
  readonly source: string;
}

/** What the recorder can say about capture and encoding, right now. */
export interface Diagnostics {
  /**
   * How the recording in progress is capturing.
   *
   * Absent when nothing is being recorded: there is no backend running between
   * recordings, and naming the last one used would be saying what *is*
   * happening from a reading of what happened.
   */
  readonly capture?: CaptureAccount;
  /** What this machine can encode. Never absent. */
  readonly encoders: EncoderAccount;
  /**
   * What the recording in progress is running with, setting by setting.
   *
   * Absent when nothing is being recorded, for the reason {@link capture} is:
   * these are one recording's answers and not a reading of the settings file,
   * which says what the *next* recording would be made with.
   */
  readonly settings?: readonly EffectiveSetting[];
}

/** How the recorder is capturing, and what this machine can encode. */
export interface DiagnosticsReply {
  /** The tag. */
  readonly reply: 'diagnostics';
  /** What the recorder can say about capture and encoding, right now. */
  readonly diagnostics: Diagnostics;
}

/** Every action a global hotkey can perform, and where each one stands. */
export interface HotkeysReply {
  /** The tag. */
  readonly reply: 'hotkeys';
  /** One row per action, in the order a configuration screen lists them. */
  readonly hotkeys: readonly HotkeyBinding[];
}

/**
 * The recorder has stopped listening and is winding up.
 *
 * Sent before it exits, because a reply written afterwards would never arrive.
 * The endpoint going away is the proof that it finished.
 */
export interface ShuttingDownReply {
  /** The tag. */
  readonly reply: 'shutting_down';
  /**
   * The recording that will be stopped and finished before the recorder exits.
   *
   * Absent when nothing was being recorded. Present only for a shutdown that
   * asked to finalise one, so this names a file worth telling the user about
   * rather than one that has just been ended behind their back.
   */
  readonly finalising?: ActiveRecording;
}

/**
 * What a command produced.
 *
 * No unrecognised variant: a reply the interface cannot read is a command whose
 * outcome it does not know, which it must report rather than absorb.
 */
export type Reply =
  | PongReply
  | StatusReply
  | RecordingStartedReply
  | RecordingStoppedReply
  | BookmarkAddedReply
  | ScreenshotTakenReply
  | ReplaySavedReply
  | LibrarySessionsReply
  | LibraryGamesReply
  | LibraryEventsReply
  | LibraryClipDocumentReply
  | ClipDocumentSavedReply
  | LibraryTrashReply
  | RestoredReply
  | TrashEmptiedReply
  | FavouritedReply
  | LockedReply
  | PluginsReply
  | RecordingExportedReply
  | PlaybackOpenedReply
  | PreviewOpenedReply
  | HotkeysReply
  | DiagnosticsReply
  | StorageReply
  | SettingsReply
  | AudioDevicesReply
  | MicrophoneLevelReply
  | StartAtLoginReply
  | ShuttingDownReply;

/** Nothing is being recorded, and nothing will be until something asks. */
export interface IdleStatus {
  /** The tag. */
  readonly state: 'idle';
}

/**
 * Nothing is being recorded, and the next game to start will be.
 *
 * A different answer from {@link IdleStatus}, which is the whole point of it: a
 * recorder watching for games with no game running used to be reported as idle,
 * which is what a recorder that will never record anything also reports.
 */
export interface WatchingStatus {
  /** The tag. */
  readonly state: 'watching';
  /**
   * The sitting that is still open, when one is.
   *
   * A game that exits keeps its sitting open for a grace period so that the same
   * game launching again rejoins it. During that period the recorder is watching
   * and in a sitting at once, and an interface that dropped the game's name for
   * those few seconds would flicker.
   */
  readonly session?: SessionSummary;
}

/**
 * One sitting, as the recorder currently holds it.
 *
 * The live counterpart of {@link LibrarySession}, carrying the same field names
 * for the same facts. What it leaves out is everything the library adds
 * afterwards — a row identifier, a favourite, a tag, a size on disk — none of
 * which is known while the file is still being written.
 *
 * Whether the sitting is over is {@link ended_at}. There is no separate finished
 * shape: the sitting on a status is one the recorder is still in, and the one on
 * a {@link SessionEndedEvent} is the same object with the two fields only an
 * ended sitting has.
 */
export interface SessionSummary {
  /** The recorder's identifier for it, shared with the library once indexed. */
  readonly session_id: string;
  /**
   * The catalogue's identifier for the game.
   *
   * Absent for a sitting the catalogue would not attribute: it reported a tie,
   * or claimed nothing, and the sitting is filed under no game rather than under
   * a guess.
   */
  readonly game_id?: string;
  /** The game's name as the catalogue knows it. Absent for the same reason. */
  readonly game_name?: string;
  /** When the sitting started, RFC 3339 with the offset it was recorded in. */
  readonly started_at: string;
  /** When it ended, RFC 3339. Absent while it is still open. */
  readonly ended_at?: string;
  /**
   * Why it ended: `game-exited`, `system-resumed`, `recorder-stopping` or
   * `recording-ended`. Absent while it is still open.
   *
   * The vocabulary of `LibrarySession.end_reason`, and open for the same reason:
   * a reason invented later is kept and shown rather than failing the frame.
   */
  readonly end_reason?: string;
  /**
   * The files it has produced, in the order they were recorded.
   *
   * Includes the one being written, which is what makes "the second file of this
   * sitting" sayable while it is still being recorded.
   */
  readonly recordings: readonly SessionRecording[];
}

/**
 * One recording within a sitting.
 *
 * Deliberately smaller than {@link LibraryRecording}: this is a file the
 * recorder has just written, with no row identifier, no tags and no measured
 * size, because nothing has indexed it yet.
 */
export interface SessionRecording {
  /** Which recording of the sitting this is, counting from one. */
  readonly session_index: number;
  /** The file that was written, or is being written. */
  readonly output: string;
  /**
   * What became of it: `recorded`, `no-window` or `failed`.
   *
   * Absent while it is still running. The two that produced no playable file are
   * listed anyway: a sitting whose recording failed is not a sitting with one
   * fewer recording.
   */
  readonly outcome?: string;
  /**
   * Why it ended: `stopped`, `target-lost`, `target-resized`, `disk-space-low`
   * or `output-unavailable`.
   *
   * The vocabulary of {@link LibraryRecording.end_reason} — the hyphenated one
   * the sidecar writes and the index stores — and open for the same reason that
   * one is: a reason invented after this build is kept and shown rather than
   * failing the frame that carried it. It is deliberately not
   * {@link RecordingSummary.end_reason}, which is the underscored
   * {@link EndReason} the reply to a `stop_recording` carries; the two spellings
   * are the two halves of the protocol they belong to, and collapsing them here
   * would make this field lie about what arrived.
   *
   * **Why a sitting that has just ended needs it at all.** A recording somebody
   * stopped answers "why did it end" in the reply to their stop. A recording
   * that ended *by itself* has no reply to answer in, and
   * {@link SessionEndedEvent} is the only thing the recorder sends about it — so
   * without this a window could name the file and could not say why it stopped,
   * and a sitting cut short by a window being dragged to a new size looked
   * exactly like one that ran to the end
   * ([#625](https://github.com/wildware-uk/clipped/issues/625),
   * [ADR 0012](../../../../docs/adr/0012-a-session-follows-a-resize-with-a-new-file.md)).
   * The library row for the same file has carried the word all along; this is
   * the announcement catching up with it, minutes earlier.
   *
   * Absent while the recording is still being written, and for an entry that
   * produced no file: a `no-window` or a `failed` never reached an ending to
   * have a reason for.
   */
  readonly end_reason?: string;
  /** How long it runs for. Absent while running, and for one with no file. */
  readonly duration_ms?: number;
}

/**
 * A recording that is running, without the tag that says a status carries it.
 *
 * The same four fields appear inside a `recording` status and inside a
 * `shutting_down` reply, so they are one interface rather than two: two copies
 * would be two things to keep in step with the Rust, and the conformance check
 * compares each against the same `active_recording` structure.
 */
export interface ActiveRecording {
  /** Identifies this recording for the length of the recorder's life. */
  readonly recording_id: string;
  /** The file being written. */
  readonly output: string;
  /**
   * What is being recorded, as the user asked for it: ``process `cs2.exe` ``.
   *
   * Never the window title: a title is user content, and the most reliable way
   * to put somebody's document name into a screenshot of a bug report.
   */
  readonly target: string;
  /** Milliseconds the recording has been running, as the recorder measures it. */
  readonly elapsed_ms: number;
  /**
   * How much history this recording's replay buffer keeps, when it has one.
   *
   * Absent for a recording with no buffer. The `replay` feature says the build
   * has the command; this says there is something for it to save from, and
   * bounds what may be asked for.
   */
  readonly replay_seconds?: number;
  /**
   * The sitting this recording belongs to, when it belongs to one.
   *
   * This is where the game is. {@link target} is a capture selector —
   * `process 4242` — and an interface cannot turn one into "Counter-Strike 2"
   * without the catalogue, which lives in the recorder. It is also how the
   * second file of one sitting stops looking like an unrelated recording.
   *
   * Absent for a recording that is not part of a sitting.
   */
  readonly session?: SessionSummary;
}

/** A recording is in progress. */
export interface RecordingStatus extends ActiveRecording {
  /** The tag. */
  readonly state: 'recording';
}

/**
 * What the recorder is doing right now.
 *
 * The one union here with no catch-all. See the file header.
 */
export type RecorderStatus = IdleStatus | WatchingStatus | RecordingStatus;

/** What a finished recording turned out to be. Every field is measured. */
export interface RecordingSummary {
  /** The file that was written, playable whatever ended the recording. */
  readonly output: string;
  /** How long the recording covers. */
  readonly duration_ms: number;
  /** Why it ended. */
  readonly end_reason: EndReason;
  /** Frames that reached the file. */
  readonly frames_encoded: number;
  /** Frames skipped to hold the requested frame rate. Expected, not a fault. */
  readonly frames_skipped_for_rate: number;
  /**
   * Frames dropped because the writer could not keep up.
   *
   * Kept separate from the count above rather than summed with it: one is the
   * recorder doing what it was asked and the other is the recorder failing, and
   * an interface cannot re-separate what the protocol has already mixed.
   */
  readonly frames_dropped_writer_behind: number;
  /** The frame rate actually sustained, where there was enough to measure one. */
  readonly sustained_framerate?: number;
  /** The encoder that produced the file. */
  readonly encoder: string;
  /** The codec in the file. */
  readonly codec: string;
  /** The encoded picture width. */
  readonly width: number;
  /** The encoded picture height. */
  readonly height: number;
}

/**
 * A bookmark that was taken, as the recorder placed it.
 *
 * It carries where the bookmark *landed* rather than only confirming it was
 * taken, because that is not where the key was pressed: the recorder stamps a
 * bookmark `lead_seconds` earlier, to allow for the fact that a person presses
 * the key after the thing they wanted to mark. An interface that showed the
 * press would be showing a moment that is not the one in the file.
 */
export interface BookmarkSummary {
  /** The recording it is in. */
  readonly recording_id: string;
  /** How far into that recording the marked moment is. */
  readonly at_seconds: number;
  /**
   * Where the recording was when the request was made.
   *
   * `at_seconds` plus `lead_seconds`, except at the very start of a recording,
   * where the offset is clamped at zero and this is the only record of where
   * the press actually was.
   */
  readonly pressed_at_seconds: number;
  /** How far before the request the bookmark was stamped. */
  readonly lead_seconds: number;
  /** What it is called, if anything. */
  readonly label?: string;
  /** The colour it was given, exactly as it was sent. */
  readonly colour?: string;
  /** How long the marked moment lasts, if that was said. */
  readonly duration_seconds?: number;
  /** The file the bookmarks of this recording are kept in. */
  readonly bookmarks_file: string;
  /** How many bookmarks this recording now has, including this one. */
  readonly bookmarks_in_recording: number;
}

/**
 * A screenshot that was taken and written to disk.
 *
 * It carries the file rather than only confirming the picture was taken,
 * because every useful next action - showing it, revealing it in Explorer,
 * attaching it to a message - needs the path.
 */
export interface ScreenshotSummary {
  /** The file that was written. */
  readonly path: string;
  /** What it was written as: `png`, `jpeg` or `webp`. */
  readonly format: string;
  /** The picture's width in pixels. */
  readonly width: number;
  /** The picture's height in pixels. */
  readonly height: number;
  /** How large the file is. */
  readonly bytes: number;
  /**
   * The recording it was taken during.
   *
   * Absent for a screenshot taken with nothing recording, which is a supported
   * thing to do rather than an error.
   */
  readonly recording_id?: string;
  /**
   * How far into that recording the picture was taken.
   *
   * Absent for the same reason `recording_id` is, and also when a recording had
   * not yet put a frame in its file. It is the recording's own media clock, so
   * a timeline can put a marker exactly where the picture came from.
   */
  readonly at_seconds?: number;
}

/**
 * A clip saved out of a recording's replay buffer.
 *
 * It carries what the clip turned out to be rather than only that one was
 * written, because what comes out is not exactly what was asked for: a clip can
 * only begin on a keyframe, so it is slightly longer at the front, and a buffer
 * that has not filled yet gives less than was asked for.
 */
export interface ReplaySummary {
  /** The file that was written. */
  readonly path: string;
  /** The recording it was saved out of. */
  readonly recording_id: string;
  /** How much video was asked for. */
  readonly requested_seconds: number;
  /** How long the clip is. */
  readonly duration_seconds: number;
  /** Where in the recording the clip begins, on the recording's own timeline. */
  readonly source_start_seconds: number;
  /** Where in the recording the clip ends. */
  readonly source_end_seconds: number;
  /** Video kept before the requested start, because a clip begins on a keyframe. */
  readonly leading_slack_seconds: number;
  /** Whether the buffer held the whole of what was asked for. */
  readonly complete: boolean;
  /** How much of the request the buffer did not hold. Zero when `complete`. */
  readonly shortfall_seconds: number;
  /** How many bytes of coded video were written. */
  readonly bytes: number;
}

/** Which versions this recorder actually speaks. */
export interface UnsupportedProtocolVersionDetail {
  /** The tag. */
  readonly detail: 'unsupported_protocol_version';
  /** What the client asked for. */
  readonly requested: number;
  /** Every version the recorder would have accepted. */
  readonly supported: readonly number[];
  /** The recorder's own build version, so the user can be told which to update. */
  readonly recorder_version: string;
}

/** Which subsystem is missing, and where it is being built. */
export interface NotImplementedDetail {
  /** The tag. */
  readonly detail: 'not_implemented';
  /** The subsystem in the user's words: "a recording with a replay buffer". */
  readonly subsystem: string;
  /** The milestone that builds it, such as `M3`. */
  readonly milestone: string;
  /** The issue that tracks it. */
  readonly tracking_issue: number;
}

/**
 * A detail this build has never heard of, kept exactly as it arrived.
 *
 * Not an error, and not something to construct: it is how the interface keeps a
 * newer recorder's refusal readable. There is nothing to render from it — the
 * message is what the user is shown — but a diagnostic can say what was not
 * understood.
 */
export interface UnrecognisedErrorDetail {
  /** Never present. The tag it arrived with is inside {@link unrecognised}. */
  readonly detail?: undefined;
  /** The detail exactly as it arrived. */
  readonly unrecognised: JsonValue;
}

/** The machine-readable particulars of the refusals that have any. */
export type ErrorDetail =
  UnsupportedProtocolVersionDetail | NotImplementedDetail | UnrecognisedErrorDetail;

/** A refusal, in the form the desktop application renders. */
export interface ProtocolError {
  /** What kind of refusal this is. Stable across versions. */
  readonly code: ErrorCode;
  /** One sentence for a person. This is the part that must always survive. */
  readonly message: string;
  /** The particulars, for the codes that have any. */
  readonly detail?: ErrorDetail;
}

/** The recorder started, stopped or changed what it is doing. */
export interface StatusChangedEvent {
  /** The tag. */
  readonly event: 'status_changed';
  /** The new state, whole rather than a delta, so a missed event costs nothing. */
  readonly status: RecorderStatus;
}

/**
 * A sitting ended, and this is what it produced.
 *
 * On the `status` stream rather than one of its own, because it is the end of
 * the thing {@link StatusChangedEvent} has been describing. It carries the
 * sitting rather than only its identifier because the files are the point: the
 * recorder is the only side that knows what it wrote, and the library has not
 * necessarily indexed any of it yet.
 */
export interface SessionEndedEvent {
  /** The tag. */
  readonly event: 'session_ended';
  /** The sitting, with `ended_at` and `end_reason` filled in. */
  readonly session: SessionSummary;
}

/** A recording ended because something failed, rather than because it was asked to. */
export interface RecordingFailedEvent {
  /** The tag. */
  readonly event: 'recording_failed';
  /** Which recording. */
  readonly recording_id: string;
  /** What failed. The file is still finished and playable. */
  readonly error: ProtocolError;
}

/**
 * A running export has got this far.
 *
 * On the `exports` stream, which a client asks for only when the recorder
 * advertises the `export_progress` feature. **Its absence means nothing**: a
 * recorder without it copies the file exactly as it always did and says nothing
 * while it does, so silence is neither failure nor completion. The reply to
 * `export_recording` remains the only thing that says an export finished
 * (issue #446).
 */
export interface ExportProgressEvent {
  /** The tag. */
  readonly event: 'export_progress';
  /** How far it has got, and which export it is. */
  readonly export: ExportProgress;
}

/**
 * An event this build has never heard of, kept exactly as it arrived.
 *
 * Adding an event costs no protocol version, so an older interface meeting a
 * newer recorder will see these. Losing one is losing information it does
 * without; failing the frame would lose the subscription.
 */
export interface UnrecognisedEvent {
  /** Never present. The name it arrived with is inside {@link unrecognised}. */
  readonly event?: undefined;
  /** The event exactly as it arrived, less the envelope's `type`. */
  readonly unrecognised: JsonValue;
}

/** Something the recorder decided to say without being asked. */
export type RecorderEvent =
  | StatusChangedEvent
  | SessionEndedEvent
  | RecordingFailedEvent
  | ExportProgressEvent
  | UnrecognisedEvent;

/** The handshake, on the wire. */
export type HelloMessage = { readonly type: 'hello' } & Hello;

/** A command, on the wire. */
export type RequestMessage = { readonly type: 'request' } & RecorderRequest;

/** A handshake accepted, on the wire. */
export type WelcomeMessage = { readonly type: 'welcome' } & Welcome;

/** A connection refused, on the wire. The connection closes after it. */
export type RefusedMessage = { readonly type: 'refused' } & ProtocolError;

/** A reply, on the wire. */
export type ResponseMessage = { readonly type: 'response' } & RecorderResponse;

/** An event, on the wire. */
export type EventMessage = { readonly type: 'event' } & RecorderEvent;

/** Anything the desktop application sends. */
export type ClientMessage = HelloMessage | RequestMessage;

/** Anything the recorder sends. */
export type ServerMessage = WelcomeMessage | RefusedMessage | ResponseMessage | EventMessage;

/** Whether a refusal is one this build knows how to act on. */
export function isKnownErrorCode(code: ErrorCode): code is KnownErrorCode {
  return (ERROR_CODES as readonly string[]).includes(code);
}

/** Whether a recorder can do the thing before the interface offers it. */
export function hasFeature(welcome: Welcome, feature: Feature): boolean {
  return welcome.features.includes(feature);
}

/** Whether a detail is one this build can render rather than only report. */
export function isRecognisedErrorDetail(
  detail: ErrorDetail,
): detail is UnsupportedProtocolVersionDetail | NotImplementedDetail {
  return detail.detail !== undefined;
}

/** Whether an event is one this build knows what to do with. */
export function isRecognisedEvent(
  event: RecorderEvent,
): event is StatusChangedEvent | SessionEndedEvent | RecordingFailedEvent | ExportProgressEvent {
  return event.event !== undefined;
}
