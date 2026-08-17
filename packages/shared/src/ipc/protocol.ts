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
export const EVENT_STREAMS = ['status', 'errors', 'metrics'] as const;

/** A stream this build knows. `metrics` is defined and refused; see below. */
export type KnownEventStream = (typeof EVENT_STREAMS)[number];

/**
 * A stream of events a connection can ask for.
 *
 * A name the recorder does not have is refused at subscription time rather than
 * accepted and left silent, so a client that asked for one is told. `metrics`
 * is refused by this recorder with `not_implemented`: nothing measures those
 * figures during a recording yet (issue #100).
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
  'automatic',
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

/** Every command the protocol defines, including the one no build performs yet. */
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
  'library_trash',
  'restore_from_trash',
  'empty_trash',
  'set_favourite',
  'set_lock',
  'plugins',
  'export_recording',
  'open_playback',
  'get_hotkeys',
  'shutdown',
  'apply_settings',
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
  'library_trash',
  'restored',
  'trash_emptied',
  'favourited',
  'locked',
  'plugins',
  'recording_exported',
  'playback_opened',
  'hotkeys',
  'shutting_down',
] as const;

/** The name of a reply. Closed: a reply nobody can read is a failed command. */
export type ReplyName = (typeof REPLIES)[number];

/** The events this build knows. */
export const EVENTS = ['status_changed', 'session_ended', 'recording_failed'] as const;

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
   * Keep the last this many seconds in memory, so that `save_replay` has
   * something to save.
   *
   * Absent means no buffer, which is what an ordinary recording is. It belongs
   * to the recording rather than to the save, because a buffer has to have been
   * filling since before the thing somebody wants to keep happened.
   */
  readonly replay_seconds?: number;
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
  /** The file. */
  readonly path: string;
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
  /** Where the file is now, inside the trash. */
  readonly path: string;
  /**
   * Where it was, and where restoring puts it back.
   *
   * The one a person recognises. A screen that showed only the trash's own copy
   * would be asking them to identify a recording by a name they have never
   * seen.
   */
  readonly original_path: string;
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
  /** Where the file is now, which is where the index now points. */
  readonly path: string;
  /**
   * Whether there was a file to move back.
   *
   * `false` for something whose media had already gone before it was deleted:
   * the row returns to the library and reports itself missing, which is the
   * truth rather than a row with no explanation.
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
  | LibraryTrashReply
  | RestoredReply
  | TrashEmptiedReply
  | FavouritedReply
  | LockedReply
  | PluginsReply
  | RecordingExportedReply
  | PlaybackOpenedReply
  | HotkeysReply
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
  StatusChangedEvent | SessionEndedEvent | RecordingFailedEvent | UnrecognisedEvent;

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
): event is StatusChangedEvent | SessionEndedEvent | RecordingFailedEvent {
  return event.event !== undefined;
}
